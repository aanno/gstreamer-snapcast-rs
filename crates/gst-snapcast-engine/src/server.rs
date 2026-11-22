//! Embedded Snapcast server, shared by all senders in a process.
//!
//! [`acquire_stream`] returns a [`StreamHandle`] for one named stream of the
//! server listening on a given port. The first acquisition for a port creates
//! and starts the server; it is stopped when the last handle is dropped.
//!
//! # Several streams per server
//!
//! The first acquisition creates the server with its stream; acquiring another
//! stream name on the same port adds that stream to the running server
//! (`ServerCommand::AddAudioStream`). Each stream has its own codec and format.
//! A released stream stays on the server, unclaimed, until the server stops, so
//! clients keep their stream assignment when a sender restarts. Re-acquiring it
//! with another codec or format replaces it: the old one is removed (groups
//! playing it are left without a stream) and a new one is added.
//!
//! # Control API (feature `control`)
//!
//! With [`ServerOptions::control`], the server also serves the Snapcast JSON-RPC
//! control API (`snapcast-control`): over TCP, and over HTTP/WebSocket together with
//! Snapweb's files, so that Snapweb, Snapdroid or Home Assistant can manage clients,
//! groups and volumes. It belongs to the shared server: the first acquirer of a port
//! configures it, and it stops with the server.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, Weak};

use snapcast_server::{
    AudioData, AudioFrame, ServerCommand, ServerConfig, ServerEvent, SnapServer, StreamConfig,
};
use tokio::sync::{mpsc, oneshot};
use tracing::Instrument;

use crate::{EngineError, PcmFormat, runtime};

/// How long [`acquire_stream`] waits for a running server to add a stream.
const ADD_STREAM_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Codec used when none is configured. PCM has no encoder delay and is
/// understood by every Snapcast client.
pub const DEFAULT_CODEC: &str = "pcm";

/// Server-wide options. Only the first acquirer of a port decides them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerOptions {
    /// TCP port to listen on. `0` binds an ephemeral port and never shares the
    /// server with other acquirers (useful for tests).
    pub port: u16,
    /// End-to-end buffer in milliseconds announced to clients.
    pub buffer_ms: u32,
    /// Announce the server via mDNS (`_snapcast._tcp`) under this instance name, so
    /// clients find it without a host name (`None`: don't). Servers on an ephemeral
    /// port (`port == 0`) are never announced.
    pub mdns_name: Option<String>,
    /// The JSON-RPC control API (default: off). Served only with the feature
    /// `control`, or by the shared server daemon.
    pub control: ControlOptions,
    /// Where the server runs (default: [`Mode::Embedded`]).
    pub mode: Mode,
}

/// Where the server of an acquisition runs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Mode {
    /// In the shared server daemon of the user if one runs, otherwise embedded
    /// (with a log line saying why).
    Auto,
    /// In the shared server daemon of the user (`gst-snapcast-daemon`, feature
    /// `shared`, Unix); an error if there is none.
    Shared,
    /// In this process, shared by the acquisitions of this process.
    #[default]
    Embedded,
}

impl Default for ServerOptions {
    fn default() -> Self {
        Self {
            port: snapcast_server::DEFAULT_STREAM_PORT,
            buffer_ms: 1000,
            mdns_name: None,
            control: ControlOptions::default(),
            mode: Mode::default(),
        }
    }
}

/// Where the server offers the JSON-RPC control API. Ports are bound on all
/// interfaces and need no authentication: anyone who can reach them controls the
/// server. With [`ServerOptions::mdns_name`], they are announced too
/// (`_snapcast-ctrl._tcp`, `_snapcast-http._tcp`, as the C++ snapserver does).
/// An embedded server serves them only with the feature `control`; without it
/// they are ignored with a warning.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ControlOptions {
    /// JSON-RPC over TCP (Snapcast's default is 1705). `None`: off; `Some(0)`: an
    /// ephemeral port (see [`StreamHandle::control_tcp_port`]).
    pub tcp_port: Option<u16>,
    /// JSON-RPC over HTTP POST and WebSocket at `/jsonrpc`, plus `snapweb_dir`
    /// (Snapcast's default is 1780). `None`: off; `Some(0)`: an ephemeral port.
    pub http_port: Option<u16>,
    /// Static files served over HTTP for every other path, normally Snapweb's
    /// release files (Debian's `/usr/share/snapserver/snapweb` is only a placeholder).
    pub snapweb_dir: Option<std::path::PathBuf>,
}

/// Track information of a stream, shown by controllers such as Snapweb
/// (Snapcast stream property `metadata`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StreamMetadata {
    /// Track title.
    pub title: Option<String>,
    /// Artists.
    pub artist: Vec<String>,
    /// Album.
    pub album: Option<String>,
    /// Track length in seconds.
    pub duration_secs: Option<f64>,
}

impl StreamMetadata {
    fn to_json(&self) -> serde_json::Value {
        let mut metadata = serde_json::Map::new();
        if let Some(title) = &self.title {
            metadata.insert("title".into(), title.clone().into());
        }
        if !self.artist.is_empty() {
            metadata.insert("artist".into(), self.artist.clone().into());
        }
        if let Some(album) = &self.album {
            metadata.insert("album".into(), album.clone().into());
        }
        if let Some(duration) = self.duration_secs {
            metadata.insert("duration".into(), duration.into());
        }
        metadata.into()
    }
}

/// Player commands a stream accepts from controllers (Snapcast `canPlay`,
/// `canPause`, `canGoNext`, `canGoPrevious`, `canSeek`). Controllers such as
/// Snapweb show the matching buttons; the commands arrive through
/// [`StreamHandle::on_command`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StreamControls {
    /// `play`.
    pub play: bool,
    /// `pause` (and `playPause`).
    pub pause: bool,
    /// `next`.
    pub next: bool,
    /// `previous`.
    pub previous: bool,
    /// `seek` / `setPosition`.
    pub seek: bool,
}

impl StreamControls {
    /// Whether any command is accepted (Snapcast `canControl`).
    pub fn any(&self) -> bool {
        self.play || self.pause || self.next || self.previous || self.seek
    }
}

/// Playback state of a stream, as shown by controllers (`playbackStatus`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackStatus {
    /// Playing.
    Playing,
    /// Paused.
    Paused,
    /// Stopped.
    Stopped,
}

impl PlaybackStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Playing => "playing",
            Self::Paused => "paused",
            Self::Stopped => "stopped",
        }
    }
}

/// What controllers see of a stream (its Snapcast `properties`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StreamProperties {
    /// Track information.
    pub metadata: StreamMetadata,
    /// Accepted player commands.
    pub controls: StreamControls,
    /// Playback state (`None`: not reported).
    pub playback: Option<PlaybackStatus>,
}

impl StreamProperties {
    /// The stream `properties` object of the Snapcast control API.
    fn to_map(&self) -> HashMap<String, serde_json::Value> {
        let c = &self.controls;
        let mut map = HashMap::from([
            ("metadata".to_owned(), self.metadata.to_json()),
            ("canControl".to_owned(), c.any().into()),
            ("canPlay".to_owned(), c.play.into()),
            ("canPause".to_owned(), c.pause.into()),
            ("canGoNext".to_owned(), c.next.into()),
            ("canGoPrevious".to_owned(), c.previous.into()),
            ("canSeek".to_owned(), c.seek.into()),
        ]);
        if let Some(playback) = self.playback {
            map.insert("playbackStatus".to_owned(), playback.as_str().into());
        }
        map
    }
}

/// A player command for a stream from a controller (JSON-RPC `Stream.Control`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamCommand {
    /// The command: `play`, `pause`, `playPause`, `stop`, `next`, `previous`,
    /// `seek`, `setPosition`, … (as the controller sent it).
    pub command: String,
    /// Its parameters as JSON text (`null` if none), e.g. `{"offset":30}` for
    /// `seek` or `{"position":90}` for `setPosition`.
    pub params_json: String,
}

/// Receives the commands for one stream; runs on the engine runtime, so it must
/// not block.
type CommandHandler = Arc<dyn Fn(StreamCommand) + Send + Sync>;
/// Command handlers by stream name.
type CommandHandlers = Arc<Mutex<HashMap<String, CommandHandler>>>;

/// Per-stream options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamOptions {
    /// Stream name (Snapcast stream id).
    pub name: String,
    /// Codec: `"pcm"` or `"flac"`.
    pub codec: String,
    /// PCM format of the data passed to [`StreamHandle::send_pcm`].
    pub format: PcmFormat,
    /// Id for log lines of this acquisition (e.g. the element name), as the
    /// [`crate::LOG_ID_FIELD`] of its span. Empty: none. The server itself is
    /// shared and logs as `snapserver:<port>`.
    pub log_id: String,
}

struct StreamSlot {
    tx: mpsc::Sender<AudioFrame>,
    codec: String,
    format: PcmFormat,
    claimed: bool,
}

/// A running server. Dropped (and thereby stopped) with its last [`StreamHandle`].
struct ServerEntry {
    requested_port: u16,
    /// The options of the acquisition that started the server.
    options: ServerOptions,
    local_port: u16,
    cmd_tx: mpsc::Sender<ServerCommand>,
    streams: Mutex<HashMap<String, StreamSlot>>,
    /// The mDNS announcement, withdrawn together with the server.
    _announcement: Option<crate::mdns::Announcement>,
    /// Bound control ports (TCP, HTTP).
    control_ports: (Option<u16>, Option<u16>),
    /// Tasks to abort with the server (the control listeners).
    tasks: Vec<tokio::task::AbortHandle>,
    /// Where `Stream.Control` commands go.
    handlers: CommandHandlers,
}

impl Drop for ServerEntry {
    fn drop(&mut self) {
        let _ = self.cmd_tx.try_send(ServerCommand::Stop);
        for task in &self.tasks {
            task.abort();
        }
        let mut registry = REGISTRY.lock().unwrap();
        // Only remove our own (now dead) entry, not a newer replacement.
        if registry
            .get(&self.requested_port)
            .is_some_and(|w| w.strong_count() == 0)
        {
            registry.remove(&self.requested_port);
        }
    }
}

/// Running servers by requested port (never contains port 0).
static REGISTRY: LazyLock<Mutex<HashMap<u16, Weak<ServerEntry>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Exclusive handle to one stream of a server: embedded in this process or in
/// the shared server daemon (see [`Mode`]).
pub struct StreamHandle {
    name: String,
    backend: Backend,
}

enum Backend {
    Embedded(EmbeddedStream),
    #[cfg(all(feature = "shared", unix))]
    Shared(crate::shared::SharedStream),
}

/// A stream of a server in this process.
struct EmbeddedStream {
    server: Arc<ServerEntry>,
    name: String,
    tx: mpsc::Sender<AudioFrame>,
}

impl Drop for EmbeddedStream {
    fn drop(&mut self) {
        self.server.handlers.lock().unwrap().remove(&self.name);
        if let Some(slot) = self.server.streams.lock().unwrap().get_mut(&self.name) {
            slot.claimed = false;
        }
    }
}

impl std::fmt::Debug for StreamHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamHandle")
            .field("name", &self.name)
            .field("port", &self.port())
            .field("shared", &self.is_shared())
            .finish()
    }
}

impl StreamHandle {
    /// Port the server is actually listening on.
    pub fn port(&self) -> u16 {
        match &self.backend {
            Backend::Embedded(e) => e.server.local_port,
            #[cfg(all(feature = "shared", unix))]
            Backend::Shared(s) => s.port,
        }
    }

    /// Stream name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Whether the stream is fed through the shared server daemon.
    pub fn is_shared(&self) -> bool {
        match &self.backend {
            Backend::Embedded(_) => false,
            #[cfg(all(feature = "shared", unix))]
            Backend::Shared(_) => true,
        }
    }

    /// The server-wide options in effect: those of the acquisition that started
    /// the server, which may differ from the caller's.
    pub fn server_options(&self) -> &ServerOptions {
        match &self.backend {
            Backend::Embedded(e) => &e.server.options,
            #[cfg(all(feature = "shared", unix))]
            Backend::Shared(s) => &s.server_options,
        }
    }

    /// Port of the JSON-RPC control API over TCP, if the server offers it.
    pub fn control_tcp_port(&self) -> Option<u16> {
        match &self.backend {
            Backend::Embedded(e) => e.server.control_ports.0,
            #[cfg(all(feature = "shared", unix))]
            Backend::Shared(s) => s.control_tcp_port,
        }
    }

    /// Port of the JSON-RPC control API over HTTP/WebSocket (and Snapweb), if the
    /// server offers it.
    pub fn control_http_port(&self) -> Option<u16> {
        match &self.backend {
            Backend::Embedded(e) => e.server.control_ports.1,
            #[cfg(all(feature = "shared", unix))]
            Backend::Shared(s) => s.control_http_port,
        }
    }

    /// Publish what controllers see of this stream (replaces the previous
    /// properties as a whole).
    ///
    /// Does not block: fails with [`EngineError::Busy`] if the server's command
    /// queue is full. Not yet passed on by the shared backend (design step 4).
    pub fn set_properties(&self, properties: &StreamProperties) -> Result<(), EngineError> {
        match &self.backend {
            Backend::Embedded(e) => send_command(
                &e.server.cmd_tx,
                ServerCommand::SetStreamMeta {
                    stream_id: self.name.clone(),
                    metadata: properties.to_map(),
                },
            ),
            #[cfg(all(feature = "shared", unix))]
            Backend::Shared(_) => {
                tracing::debug!("Stream properties are not passed to the daemon yet");
                Ok(())
            }
        }
    }

    /// Call `handler` for each player command a controller sends to this stream
    /// (replaces a previous handler; removed when the handle is dropped). It runs
    /// on the engine runtime and must not block. Controllers only offer the
    /// commands announced in [`StreamProperties::controls`]. Not yet passed on by
    /// the shared backend (design step 4).
    pub fn on_command(&self, handler: impl Fn(StreamCommand) + Send + Sync + 'static) {
        match &self.backend {
            Backend::Embedded(e) => {
                e.server
                    .handlers
                    .lock()
                    .unwrap()
                    .insert(self.name.clone(), Arc::new(handler));
            }
            #[cfg(all(feature = "shared", unix))]
            Backend::Shared(_) => {
                drop(handler);
                tracing::debug!("Stream commands are not passed from the daemon yet");
            }
        }
    }

    /// Submit interleaved little-endian PCM in the stream's format.
    ///
    /// `timestamp_usec` is the server time (see [`crate::now_usec`]) of the
    /// first frame in `data`. Blocks while the stream's queue is full (shared: at
    /// most 2 s); must not be called from within the async runtime.
    pub fn send_pcm(&self, data: Vec<u8>, timestamp_usec: i64) -> Result<(), EngineError> {
        match &self.backend {
            Backend::Embedded(e) => {
                e.tx.blocking_send(AudioFrame {
                    data: AudioData::Pcm(data),
                    timestamp_usec,
                })
                .map_err(|_| EngineError::ServerGone)
            }
            #[cfg(all(feature = "shared", unix))]
            Backend::Shared(s) => s.send_pcm(data, timestamp_usec),
        }
    }

    /// The embedded server entry (tests).
    #[cfg(test)]
    fn entry(&self) -> &Arc<ServerEntry> {
        match &self.backend {
            Backend::Embedded(e) => &e.server,
            #[cfg(all(feature = "shared", unix))]
            Backend::Shared(_) => panic!("not embedded"),
        }
    }
}

/// Get exclusive access to stream `stream.name` of the server on `server.port`,
/// starting that server if necessary, and adding the stream to a running server
/// that does not have it yet (blocks until the server answers, at most 5 s).
///
/// [`ServerOptions::mode`] says where the server runs. A port of `0` (ephemeral)
/// is never shared, so it always means embedded.
pub fn acquire_stream(
    server: &ServerOptions,
    stream: &StreamOptions,
) -> Result<StreamHandle, EngineError> {
    let span =
        tracing::info_span!("snapcast_stream", log_id = %stream.log_id, stream = %stream.name);
    let _entered = span.enter();
    let mode = if server.port == 0 {
        Mode::Embedded
    } else {
        server.mode
    };
    let backend = match mode {
        Mode::Embedded => Backend::Embedded(acquire_embedded(server, stream)?),
        #[cfg(all(feature = "shared", unix))]
        Mode::Shared | Mode::Auto => {
            use crate::shared::Failure;
            match crate::shared::acquire(server, stream) {
                Ok(shared) => {
                    tracing::info!(port = shared.port, "Stream acquired from the shared server");
                    Backend::Shared(shared)
                }
                Err(Failure::Engine(e)) => return Err(e),
                Err(Failure::Unavailable(reason)) if mode == Mode::Shared => {
                    return Err(EngineError::Daemon(reason));
                }
                Err(Failure::Unavailable(reason)) => {
                    tracing::warn!(reason, "No shared server, running embedded");
                    Backend::Embedded(acquire_embedded(server, stream)?)
                }
            }
        }
        #[cfg(not(all(feature = "shared", unix)))]
        Mode::Shared => {
            return Err(EngineError::InvalidConfig(
                "mode shared needs the engine feature `shared` (Unix)".into(),
            ));
        }
        #[cfg(not(all(feature = "shared", unix)))]
        Mode::Auto => Backend::Embedded(acquire_embedded(server, stream)?),
    };
    Ok(StreamHandle {
        name: stream.name.clone(),
        backend,
    })
}

/// [`acquire_stream`] in this process.
fn acquire_embedded(
    server: &ServerOptions,
    stream: &StreamOptions,
) -> Result<EmbeddedStream, EngineError> {
    let mut registry = REGISTRY.lock().unwrap();

    if server.port != 0
        && let Some(entry) = registry.get(&server.port).and_then(Weak::upgrade)
    {
        // Release the registry lock first: dropping `entry` on an error path
        // may run ServerEntry::drop, which takes it again.
        drop(registry);
        return claim_existing(entry, stream);
    }

    let entry = start_server(server, stream)?;
    if server.port != 0 {
        registry.insert(server.port, Arc::downgrade(&entry));
    }
    drop(registry);
    claim_existing(entry, stream)
}

fn claim_existing(
    entry: Arc<ServerEntry>,
    stream: &StreamOptions,
) -> Result<EmbeddedStream, EngineError> {
    let tx = {
        // Held while the server adds a stream, so that concurrent acquisitions
        // of one name don't both add it.
        let mut streams = entry.streams.lock().unwrap();
        match streams.get_mut(&stream.name) {
            Some(slot) if slot.claimed => {
                return Err(EngineError::StreamTaken(stream.name.clone()));
            }
            Some(slot) if slot.format == stream.format && slot.codec == stream.codec => {
                slot.claimed = true;
                slot.tx.clone()
            }
            existing => {
                if let Some(slot) = existing {
                    tracing::info!(
                        old = %format!("{} {}", slot.codec, slot.format),
                        new = %format!("{} {}", stream.codec, stream.format),
                        "Replacing the released stream with another format"
                    );
                    send_command(
                        &entry.cmd_tx,
                        ServerCommand::RemoveStream {
                            stream_id: stream.name.clone(),
                        },
                    )?;
                    streams.remove(&stream.name);
                }
                let tx = add_stream_at_runtime(&entry.cmd_tx, stream)?;
                streams.insert(
                    stream.name.clone(),
                    StreamSlot {
                        tx: tx.clone(),
                        codec: stream.codec.clone(),
                        format: stream.format,
                        claimed: true,
                    },
                );
                tx
            }
        }
    };
    Ok(EmbeddedStream {
        server: entry,
        name: stream.name.clone(),
        tx,
    })
}

/// Queue a command for the server without blocking.
fn send_command(
    cmd_tx: &mpsc::Sender<ServerCommand>,
    cmd: ServerCommand,
) -> Result<(), EngineError> {
    use mpsc::error::TrySendError;
    cmd_tx.try_send(cmd).map_err(|e| match e {
        TrySendError::Full(_) => EngineError::Busy,
        TrySendError::Closed(_) => EngineError::ServerGone,
    })
}

/// Add `stream` to a running server and wait (bounded) for its audio sender.
/// Callable from any thread except the engine runtime's own.
fn add_stream_at_runtime(
    cmd_tx: &mpsc::Sender<ServerCommand>,
    stream: &StreamOptions,
) -> Result<mpsc::Sender<AudioFrame>, EngineError> {
    let (response_tx, response_rx) = oneshot::channel();
    send_command(
        cmd_tx,
        ServerCommand::AddAudioStream {
            name: stream.name.clone(),
            config: stream_config(stream),
            response_tx,
        },
    )?;
    // Wait on the runtime, bounded, and hand the answer to this thread.
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    runtime().spawn(async move {
        let _ = done_tx.send(tokio::time::timeout(ADD_STREAM_TIMEOUT, response_rx).await);
    });
    match done_rx.recv() {
        Ok(Ok(Ok(Ok(tx)))) => {
            tracing::info!("Stream added to the running server");
            Ok(tx)
        }
        Ok(Ok(Ok(Err(reason)))) => Err(EngineError::StreamNotAdded {
            stream: stream.name.clone(),
            reason,
        }),
        Ok(Ok(Err(_))) | Err(_) => Err(EngineError::ServerGone),
        Ok(Err(_)) => Err(EngineError::Busy),
    }
}

/// The server-side configuration of a stream: always explicit, because the
/// server's defaults are those of its first stream.
fn stream_config(stream: &StreamOptions) -> StreamConfig {
    StreamConfig {
        codec: Some(stream.codec.clone()),
        sample_format: Some(stream.format.to_string()),
    }
}

fn start_server(
    server: &ServerOptions,
    stream: &StreamOptions,
) -> Result<Arc<ServerEntry>, EngineError> {
    let std_listener = bind_with_retry(server.port)?;
    std_listener
        .set_nonblocking(true)
        .map_err(|e| EngineError::Bind(server.port, e))?;
    let local_port = std_listener
        .local_addr()
        .map_err(|e| EngineError::Bind(server.port, e))?
        .port();
    #[cfg(feature = "control")]
    let control = ControlListeners::bind(&server.control)?;

    let config = ServerConfig {
        buffer_ms: server.buffer_ms,
        codec: stream.codec.clone(),
        sample_format: stream.format.to_string(),
        ..ServerConfig::default()
    };
    let (mut snap, events) = SnapServer::new(config);
    let tx = snap.add_stream_with_config(&stream.name, stream_config(stream));
    let cmd_tx = snap.command_sender();

    // The server outlives the sink that started it: its own span, not the caller's.
    let span = tracing::info_span!(
        parent: None,
        "snapcast_server",
        log_id = %format!("snapserver:{local_port}")
    );
    let rt = runtime();
    let listener = {
        let _guard = rt.enter();
        tokio::net::TcpListener::from_std(std_listener)
            .map_err(|e| EngineError::Bind(server.port, e))?
    };
    rt.spawn(
        async move {
            if let Err(e) = snap.serve(listener).await {
                tracing::error!(error = %e, port = local_port, "Snapcast server failed");
            }
        }
        .instrument(span.clone()),
    );
    #[cfg_attr(not(feature = "control"), allow(unused_mut))]
    let mut tasks = Vec::new();
    #[cfg(feature = "control")]
    let control_ports = control.ports();
    #[cfg(feature = "control")]
    let events = control.spawn(cmd_tx.clone(), events, &span, &mut tasks)?;
    #[cfg(not(feature = "control"))]
    let control_ports = {
        let c = &server.control;
        if c.tcp_port.is_some() || c.http_port.is_some() {
            let _entered = span.enter();
            tracing::warn!(
                "The control API needs the shared server or a build with the feature \
                 `control`; not served"
            );
        }
        (None, None)
    };
    // The event channel is bounded; drain it so the server never stalls.
    let handlers = CommandHandlers::default();
    rt.spawn(drain_events(events, Arc::clone(&handlers)).instrument(span.clone()));

    let mut streams = HashMap::new();
    streams.insert(
        stream.name.clone(),
        StreamSlot {
            tx,
            codec: stream.codec.clone(),
            format: stream.format,
            claimed: false,
        },
    );
    let announcement = match &server.mdns_name {
        Some(name) if server.port != 0 && !name.is_empty() => {
            let _entered = span.enter();
            let mut services = vec![(crate::mdns::SERVICE_TYPE, local_port)];
            let (tcp, http) = control_ports;
            services.extend(tcp.map(|p| (crate::mdns::CONTROL_SERVICE_TYPE, p)));
            services.extend(http.map(|p| (crate::mdns::HTTP_SERVICE_TYPE, p)));
            crate::mdns::announce(name, &services)
                .inspect_err(|e| tracing::warn!(error = %e, "Server runs without mDNS"))
                .ok()
        }
        _ => None,
    };
    Ok(Arc::new(ServerEntry {
        requested_port: server.port,
        options: server.clone(),
        local_port,
        cmd_tx,
        streams: Mutex::new(streams),
        _announcement: announcement,
        control_ports,
        tasks,
        handlers,
    }))
}

/// Logs the server's events until it stops, and hands `Stream.Control` commands
/// to the handler of their stream.
async fn drain_events(mut events: mpsc::Receiver<ServerEvent>, handlers: CommandHandlers) {
    while let Some(event) = events.recv().await {
        tracing::debug!(?event, "Snapcast server event");
        if let ServerEvent::StreamControl {
            stream_id,
            command,
            params,
        } = event
        {
            // Clone the handler out of the lock: it may take a while.
            let handler = handlers.lock().unwrap().get(&stream_id).cloned();
            match handler {
                Some(handler) => handler(StreamCommand {
                    command,
                    params_json: params.to_string(),
                }),
                None => tracing::info!(stream_id, command, "No handler for stream command"),
            }
        }
    }
}

/// Bound, not yet served control listeners.
#[cfg(feature = "control")]
struct ControlListeners {
    tcp: Option<std::net::TcpListener>,
    http: Option<std::net::TcpListener>,
    snapweb_dir: Option<std::path::PathBuf>,
}

#[cfg(feature = "control")]
impl ControlListeners {
    /// Binds the configured ports, so that a taken port fails the acquisition.
    fn bind(options: &ControlOptions) -> Result<Self, EngineError> {
        let bind = |port: Option<u16>| {
            port.map(|port| {
                let listener = bind_with_retry(port)?;
                listener
                    .set_nonblocking(true)
                    .map_err(|e| EngineError::Bind(port, e))?;
                Ok(listener)
            })
            .transpose()
        };
        Ok(Self {
            tcp: bind(options.tcp_port)?,
            http: bind(options.http_port)?,
            snapweb_dir: options.snapweb_dir.clone(),
        })
    }

    fn ports(&self) -> (Option<u16>, Option<u16>) {
        let port = |l: &Option<std::net::TcpListener>| {
            l.as_ref()
                .and_then(|l| l.local_addr().ok())
                .map(|a| a.port())
        };
        (port(&self.tcp), port(&self.http))
    }

    /// Serves the control API on the listeners. Returns the server's events,
    /// passed through once they became notifications (`events` itself without any
    /// listener).
    fn spawn(
        self,
        cmd_tx: mpsc::Sender<ServerCommand>,
        events: mpsc::Receiver<ServerEvent>,
        span: &tracing::Span,
        tasks: &mut Vec<tokio::task::AbortHandle>,
    ) -> Result<mpsc::Receiver<ServerEvent>, EngineError> {
        if self.tcp.is_none() && self.http.is_none() {
            return Ok(events);
        }
        let rt = runtime();
        let to_tokio = |l: std::net::TcpListener| {
            let port = l.local_addr().map_or(0, |a| a.port());
            let _guard = rt.enter();
            tokio::net::TcpListener::from_std(l).map_err(|e| EngineError::Bind(port, e))
        };
        let control = snapcast_control::ControlServer::new(cmd_tx);

        if let Some(listener) = self.tcp {
            let listener = to_tokio(listener)?;
            let control = control.clone();
            let task = rt.spawn(
                async move {
                    if let Err(e) = control.serve_tcp(listener).await {
                        tracing::error!(error = %e, "JSON-RPC control (TCP) failed");
                    }
                }
                .instrument(span.clone()),
            );
            tasks.push(task.abort_handle());
        }
        if let Some(listener) = self.http {
            let listener = to_tokio(listener)?;
            let control = control.clone();
            let snapweb_dir = self.snapweb_dir;
            if let Some(dir) = snapweb_dir.as_ref().filter(|d| !d.is_dir()) {
                let _entered = span.enter();
                tracing::warn!(dir = %dir.display(), "Snapweb directory not found");
            }
            let task = rt.spawn(
                async move {
                    if let Err(e) = control.serve_http(listener, snapweb_dir).await {
                        tracing::error!(error = %e, "JSON-RPC control (HTTP) failed");
                    }
                }
                .instrument(span.clone()),
            );
            tasks.push(task.abort_handle());
        }

        let (passthrough_tx, passthrough_rx) = mpsc::channel(64);
        rt.spawn(
            async move { control.forward_events(events, Some(passthrough_tx)).await }
                .instrument(span.clone()),
        );
        Ok(passthrough_rx)
    }
}

/// Bind `0.0.0.0:port`, retrying briefly on `AddrInUse`: a server stopped
/// just before (e.g. a sink restarting) releases its listener asynchronously.
fn bind_with_retry(port: u16) -> Result<std::net::TcpListener, EngineError> {
    const ATTEMPTS: u32 = 20;
    for attempt in 1.. {
        match std::net::TcpListener::bind(("0.0.0.0", port)) {
            Ok(l) => return Ok(l),
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse && attempt < ATTEMPTS => {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(e) => return Err(EngineError::Bind(port, e)),
        }
    }
    unreachable!()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    fn stream(name: &str) -> StreamOptions {
        StreamOptions {
            name: name.into(),
            codec: DEFAULT_CODEC.into(),
            format: PcmFormat {
                rate: 48000,
                bits: 16,
                channels: 2,
            },
            log_id: String::new(),
        }
    }

    #[test]
    fn stream_is_exclusive_and_server_is_shared_then_released() {
        let opts = ServerOptions {
            port: free_port(),
            buffer_ms: 500,
            ..ServerOptions::default()
        };
        let a = acquire_stream(&opts, &stream("a")).unwrap();
        assert_eq!(a.port(), opts.port);

        assert!(matches!(
            acquire_stream(&opts, &stream("a")),
            Err(EngineError::StreamTaken(_))
        ));
        // Another stream name joins the running server.
        let b = acquire_stream(&opts, &stream("b")).unwrap();
        assert!(Arc::ptr_eq(a.entry(), b.entry()));
        let mut other_format = stream("a");
        other_format.format.rate = 44100;
        drop(a);
        drop(b);

        // Last handle gone: the server stops and the port can be reused,
        // now with a different stream set.
        let c = acquire_stream(&opts, &stream("c")).unwrap();
        assert_eq!(c.port(), opts.port);
        drop(c);
        let _d = acquire_stream(&opts, &other_format).unwrap();
    }

    #[test]
    fn released_stream_can_be_reclaimed_while_server_runs() {
        let opts = ServerOptions {
            port: free_port(),
            buffer_ms: 500,
            ..ServerOptions::default()
        };
        let a = acquire_stream(&opts, &stream("a")).unwrap();
        // Keep the server alive through a second user of the same entry.
        let server = Arc::clone(a.entry());
        drop(a);
        let a2 = acquire_stream(&opts, &stream("a")).unwrap();
        assert!(Arc::ptr_eq(&server, a2.entry()));
        let mut other = stream("a");
        other.codec = "flac".into();
        drop(a2);
        // Released with another codec: replaced on the same server.
        let a3 = acquire_stream(&opts, &other).unwrap();
        assert!(Arc::ptr_eq(&server, a3.entry()));
        drop(a3);
        // An invalid format is the server's to reject.
        let mut invalid = stream("z");
        invalid.codec = "bogus".into();
        assert!(matches!(
            acquire_stream(&opts, &invalid),
            Err(EngineError::StreamNotAdded { .. })
        ));
    }

    /// One JSON-RPC request over TCP; returns the reply line. Notifications the
    /// server pushes meanwhile (e.g. `Server.OnUpdate` after a stream was added)
    /// have no `id` and are skipped.
    #[cfg(feature = "control")]
    fn rpc_over_tcp(port: u16, request: &str) -> String {
        use std::io::{BufRead, Write};
        let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        writeln!(stream, "{request}").unwrap();
        let mut reader = std::io::BufReader::new(stream);
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let message: serde_json::Value = serde_json::from_str(&line).unwrap();
            if message.get("id").is_some() {
                return line;
            }
        }
    }

    #[cfg(feature = "control")]
    #[test]
    fn control_api_runs_and_stops_with_the_server() {
        let opts = ServerOptions {
            port: 0,
            buffer_ms: 500,
            control: ControlOptions {
                tcp_port: Some(0),
                http_port: Some(0),
                snapweb_dir: None,
            },
            ..ServerOptions::default()
        };
        let a = acquire_stream(&opts, &stream("x")).unwrap();
        let tcp = a.control_tcp_port().expect("TCP control port");
        let http = a.control_http_port().expect("HTTP control port");
        assert_ne!(tcp, 0);
        assert_ne!(http, 0);

        let reply = rpc_over_tcp(
            tcp,
            r#"{"jsonrpc":"2.0","id":1,"method":"Server.GetStatus"}"#,
        );
        let reply: serde_json::Value = serde_json::from_str(&reply).unwrap();
        assert_eq!(reply["id"], 1);
        assert_eq!(reply["result"]["server"]["streams"][0]["id"], "x");

        // Metadata shows up as the stream's properties.
        a.set_properties(&StreamProperties {
            metadata: StreamMetadata {
                title: Some("Song".into()),
                artist: vec!["Band".into()],
                ..StreamMetadata::default()
            },
            controls: StreamControls {
                next: true,
                ..StreamControls::default()
            },
            playback: Some(PlaybackStatus::Playing),
        })
        .unwrap();
        let (command_tx, command_rx) = std::sync::mpsc::channel();
        a.on_command(move |c| {
            let _ = command_tx.send(c);
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let reply = rpc_over_tcp(
                tcp,
                r#"{"jsonrpc":"2.0","id":2,"method":"Server.GetStatus"}"#,
            );
            let reply: serde_json::Value = serde_json::from_str(&reply).unwrap();
            let metadata = &reply["result"]["server"]["streams"][0]["properties"]["metadata"];
            if metadata["title"] == "Song" {
                assert_eq!(metadata["artist"], serde_json::json!(["Band"]));
                let properties = &reply["result"]["server"]["streams"][0]["properties"];
                assert_eq!(properties["canControl"], true);
                assert_eq!(properties["canGoNext"], true);
                assert_eq!(properties["canPlay"], false);
                assert_eq!(properties["playbackStatus"], "playing");
                break;
            }
            assert!(std::time::Instant::now() < deadline, "metadata not applied");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }

        // Stream.Control reaches the stream's handler.
        rpc_over_tcp(
            tcp,
            r#"{"jsonrpc":"2.0","id":3,"method":"Stream.Control","params":{"id":"x","command":"next","params":{}}}"#,
        );
        let command = command_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("command");
        assert_eq!(command.command, "next");
        assert_eq!(command.params_json, "{}");

        // The control ports close with the server.
        drop(a);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::net::TcpStream::connect(("127.0.0.1", tcp)).is_ok()
            || std::net::TcpStream::connect(("127.0.0.1", http)).is_ok()
        {
            assert!(
                std::time::Instant::now() < deadline,
                "control ports still open"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    #[cfg(feature = "control")]
    #[test]
    fn controllers_see_every_stream_of_the_server() {
        let opts = ServerOptions {
            port: free_port(),
            buffer_ms: 500,
            control: ControlOptions {
                tcp_port: Some(0),
                ..ControlOptions::default()
            },
            ..ServerOptions::default()
        };
        let x = acquire_stream(&opts, &stream("x")).unwrap();
        let mut flac = stream("y");
        flac.codec = "flac".into();
        let _y = acquire_stream(&opts, &flac).unwrap();
        let tcp = x.control_tcp_port().expect("TCP control port");
        let reply = rpc_over_tcp(
            tcp,
            r#"{"jsonrpc":"2.0","id":1,"method":"Server.GetStatus"}"#,
        );
        let reply: serde_json::Value = serde_json::from_str(&reply).unwrap();
        let ids: Vec<&str> = reply["result"]["server"]["streams"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["x", "y"]);
    }

    #[test]
    fn port_zero_is_ephemeral_and_never_shared() {
        let opts = ServerOptions {
            port: 0,
            buffer_ms: 500,
            ..ServerOptions::default()
        };
        let a = acquire_stream(&opts, &stream("x")).unwrap();
        let b = acquire_stream(&opts, &stream("x")).unwrap();
        assert_ne!(a.port(), 0);
        assert_ne!(a.port(), b.port());
    }
}
