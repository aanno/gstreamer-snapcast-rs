//! Embedded Snapcast server, shared by all senders in a process.
//!
//! [`acquire_stream`] returns a [`StreamHandle`] for one named stream of the
//! server listening on a given port. The first acquisition for a port creates
//! and starts the server; it is stopped when the last handle is dropped.
//!
//! # Limitation: fixed stream set
//!
//! `snapcast-server` requires all streams to be registered before
//! `SnapServer::serve` is called. Until snapcast-rs grows runtime
//! `add_stream`/`remove_stream`, a running server only has the stream it was
//! created with, so a second, *different* stream on the same port fails with
//! [`EngineError::StreamsFixed`]. Use different ports for now.
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
use tokio::sync::mpsc;
use tracing::Instrument;

use crate::{EngineError, PcmFormat, runtime};

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
    /// The JSON-RPC control API (default: off).
    #[cfg(feature = "control")]
    pub control: ControlOptions,
}

impl Default for ServerOptions {
    fn default() -> Self {
        Self {
            port: snapcast_server::DEFAULT_STREAM_PORT,
            buffer_ms: 1000,
            mdns_name: None,
            #[cfg(feature = "control")]
            control: ControlOptions::default(),
        }
    }
}

/// Where the server offers the JSON-RPC control API. Ports are bound on all
/// interfaces and need no authentication: anyone who can reach them controls the
/// server. With [`ServerOptions::mdns_name`], they are announced too
/// (`_snapcast-ctrl._tcp`, `_snapcast-http._tcp`, as the C++ snapserver does).
#[cfg(feature = "control")]
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
    /// The stream `properties` object of the Snapcast control API.
    fn to_properties(&self) -> HashMap<String, serde_json::Value> {
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
        HashMap::from([("metadata".to_owned(), metadata.into())])
    }
}

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
    local_port: u16,
    cmd_tx: mpsc::Sender<ServerCommand>,
    streams: Mutex<HashMap<String, StreamSlot>>,
    /// The mDNS announcement, withdrawn together with the server.
    _announcement: Option<crate::mdns::Announcement>,
    /// Bound control ports (TCP, HTTP).
    #[cfg(feature = "control")]
    control_ports: (Option<u16>, Option<u16>),
    /// Tasks to abort with the server (the control listeners).
    tasks: Vec<tokio::task::AbortHandle>,
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

/// Exclusive handle to one stream of an embedded server.
pub struct StreamHandle {
    server: Arc<ServerEntry>,
    name: String,
    tx: mpsc::Sender<AudioFrame>,
}

impl std::fmt::Debug for StreamHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamHandle")
            .field("name", &self.name)
            .field("port", &self.server.local_port)
            .finish()
    }
}

impl StreamHandle {
    /// Port the server is actually listening on.
    pub fn port(&self) -> u16 {
        self.server.local_port
    }

    /// Stream name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Port of the JSON-RPC control API over TCP, if the server offers it.
    #[cfg(feature = "control")]
    pub fn control_tcp_port(&self) -> Option<u16> {
        self.server.control_ports.0
    }

    /// Port of the JSON-RPC control API over HTTP/WebSocket (and Snapweb), if the
    /// server offers it.
    #[cfg(feature = "control")]
    pub fn control_http_port(&self) -> Option<u16> {
        self.server.control_ports.1
    }

    /// Publish track information for this stream (replaces the previous one).
    ///
    /// Does not block: fails with [`EngineError::Busy`] if the server's command
    /// queue is full.
    pub fn set_metadata(&self, metadata: &StreamMetadata) -> Result<(), EngineError> {
        use mpsc::error::TrySendError;
        self.server
            .cmd_tx
            .try_send(ServerCommand::SetStreamMeta {
                stream_id: self.name.clone(),
                metadata: metadata.to_properties(),
            })
            .map_err(|e| match e {
                TrySendError::Full(_) => EngineError::Busy,
                TrySendError::Closed(_) => EngineError::ServerGone,
            })
    }

    /// Submit interleaved little-endian PCM in the stream's format.
    ///
    /// `timestamp_usec` is the server time (see [`crate::now_usec`]) of the
    /// first frame in `data`. Blocks while the stream's queue is full; must not
    /// be called from within the async runtime.
    pub fn send_pcm(&self, data: Vec<u8>, timestamp_usec: i64) -> Result<(), EngineError> {
        self.tx
            .blocking_send(AudioFrame {
                data: AudioData::Pcm(data),
                timestamp_usec,
            })
            .map_err(|_| EngineError::ServerGone)
    }
}

impl Drop for StreamHandle {
    fn drop(&mut self) {
        if let Some(slot) = self.server.streams.lock().unwrap().get_mut(&self.name) {
            slot.claimed = false;
        }
    }
}

/// Get exclusive access to stream `stream.name` of the server on `server.port`,
/// starting that server if necessary.
pub fn acquire_stream(
    server: &ServerOptions,
    stream: &StreamOptions,
) -> Result<StreamHandle, EngineError> {
    let span =
        tracing::info_span!("snapcast_stream", log_id = %stream.log_id, stream = %stream.name);
    let _entered = span.enter();
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
) -> Result<StreamHandle, EngineError> {
    let tx = {
        let mut streams = entry.streams.lock().unwrap();
        let Some(slot) = streams.get_mut(&stream.name) else {
            return Err(EngineError::StreamsFixed {
                port: entry.local_port,
                stream: stream.name.clone(),
            });
        };
        if slot.claimed {
            return Err(EngineError::StreamTaken(stream.name.clone()));
        }
        if slot.format != stream.format || slot.codec != stream.codec {
            return Err(EngineError::StreamMismatch {
                stream: stream.name.clone(),
                existing: format!("{} {}", slot.codec, slot.format),
                requested: format!("{} {}", stream.codec, stream.format),
            });
        }
        slot.claimed = true;
        slot.tx.clone()
    };
    Ok(StreamHandle {
        server: entry,
        name: stream.name.clone(),
        tx,
    })
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
    let tx = snap.add_stream_with_config(
        &stream.name,
        StreamConfig {
            codec: Some(stream.codec.clone()),
            sample_format: Some(stream.format.to_string()),
        },
    );
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
    // The event channel is bounded; drain it so the server never stalls.
    rt.spawn(drain_events(events).instrument(span.clone()));

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
            #[allow(unused_mut)]
            let mut services = vec![(crate::mdns::SERVICE_TYPE, local_port)];
            #[cfg(feature = "control")]
            {
                let (tcp, http) = control_ports;
                services.extend(tcp.map(|p| (crate::mdns::CONTROL_SERVICE_TYPE, p)));
                services.extend(http.map(|p| (crate::mdns::HTTP_SERVICE_TYPE, p)));
            }
            crate::mdns::announce(name, &services)
                .inspect_err(|e| tracing::warn!(error = %e, "Server runs without mDNS"))
                .ok()
        }
        _ => None,
    };
    Ok(Arc::new(ServerEntry {
        requested_port: server.port,
        local_port,
        cmd_tx,
        streams: Mutex::new(streams),
        _announcement: announcement,
        #[cfg(feature = "control")]
        control_ports,
        tasks,
    }))
}

/// Logs the server's events until it stops.
async fn drain_events(mut events: mpsc::Receiver<ServerEvent>) {
    while let Some(event) = events.recv().await {
        tracing::debug!(?event, "Snapcast server event");
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
        assert!(matches!(
            acquire_stream(&opts, &stream("b")),
            Err(EngineError::StreamsFixed { .. })
        ));
        let mut other_format = stream("a");
        other_format.format.rate = 44100;
        drop(a);

        // Last handle gone: the server stops and the port can be reused,
        // now with a different stream set.
        let b = acquire_stream(&opts, &stream("b")).unwrap();
        assert_eq!(b.port(), opts.port);
        drop(b);
        let _c = acquire_stream(&opts, &other_format).unwrap();
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
        let server = Arc::clone(&a.server);
        drop(a);
        let a2 = acquire_stream(&opts, &stream("a")).unwrap();
        assert!(Arc::ptr_eq(&server, &a2.server));
        let mut other = stream("a");
        other.codec = "flac".into();
        drop(a2);
        assert!(matches!(
            acquire_stream(&opts, &other),
            Err(EngineError::StreamMismatch { .. })
        ));
    }

    /// One JSON-RPC request over TCP; returns the first reply line.
    #[cfg(feature = "control")]
    fn rpc_over_tcp(port: u16, request: &str) -> String {
        use std::io::{BufRead, Write};
        let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        writeln!(stream, "{request}").unwrap();
        let mut line = String::new();
        std::io::BufReader::new(stream)
            .read_line(&mut line)
            .unwrap();
        line
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
        a.set_metadata(&StreamMetadata {
            title: Some("Song".into()),
            artist: vec!["Band".into()],
            ..StreamMetadata::default()
        })
        .unwrap();
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
                break;
            }
            assert!(std::time::Instant::now() < deadline, "metadata not applied");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }

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
