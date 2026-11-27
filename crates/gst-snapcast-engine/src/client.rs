//! Embedded Snapcast client delivering frames with a local play-out time.
//!
//! [`connect`] spawns a `SnapClient` on the engine runtime. [`ClientHandle::recv`]
//! yields decoded audio with [`PlayoutFrame::playout_usec`]: the local
//! monotonic time (same domain as [`crate::now_usec`]) at which the frame must
//! be heard. It is derived as
//!
//! ```text
//! playout = server_timestamp - diff_to_server + (buffer_ms - server_latency - client_latency)
//! ```
//!
//! matching the C++ snapclient. `diff_to_server` comes from Snapcast's own
//! time sync, so drift between the sender's and receiver's clocks is absorbed
//! into the timestamps. Frames are withheld until the server settings have
//! been received and snapcast-client reports `TimeSyncUsable` (after
//! `TIME_SYNC_USABLE_REPLIES` time-sync replies, about 0.5 s; its median
//! estimate keeps improving while we already play). Until 2026-11-19 the engine
//! guessed that point with a fixed 500 ms after connecting (L3).

use std::sync::{Arc, Mutex};

use snapcast_client::{ClientCommand, ClientConfig, ClientEvent, SnapClient};
use tokio::sync::{mpsc, watch};
use tracing::Instrument;

use crate::{EngineError, runtime};

mod instance;
mod session;

pub use session::SessionProperties;

/// Sends commands to a session's stream and server (see
/// [`ClientHandle::session_control`]). Requests are queued while the control
/// connection is down; they fail only when the queue is full or the client is gone.
#[derive(Debug, Clone)]
pub struct SessionControl {
    requests: mpsc::Sender<session::Request>,
}

impl SessionControl {
    /// A player command for the session's stream (`Stream.Control`): `command`
    /// such as `play`, `pause`, `setPosition`, with its parameters as JSON text
    /// (e.g. `{"position":90}`; empty: none).
    /// Fails if `params_json` isn't JSON (e.g. a number with a decimal comma).
    pub fn stream_control(&self, command: &str, params_json: &str) -> Result<(), EngineError> {
        let params = if params_json.trim().is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_str(params_json).map_err(|e| {
                EngineError::InvalidConfig(format!(
                    "{command}: parameters are not JSON ({e}): {params_json}"
                ))
            })?
        };
        self.send(session::Request::StreamControl {
            command: command.to_owned(),
            params,
        })
    }

    /// This client's volume on the server (`Client.SetVolume`), as a controller
    /// would set it; the server sends it back as the client's volume.
    pub fn set_server_volume(&self, percent: u16, muted: bool) -> Result<(), EngineError> {
        self.send(session::Request::SetVolume {
            percent: percent.min(100),
            muted,
        })
    }

    fn send(&self, request: session::Request) -> Result<(), EngineError> {
        self.requests.try_send(request).map_err(|e| match e {
            mpsc::error::TrySendError::Full(_) => EngineError::Busy,
            mpsc::error::TrySendError::Closed(_) => {
                EngineError::InvalidConfig("the session's client is gone".into())
            }
        })
    }
}

/// Client options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientOptions {
    /// Server host name or address. Empty: find a server via mDNS
    /// ([`crate::mdns::discover`], up to [`DISCOVERY_TIMEOUT`]) when connecting.
    pub host: String,
    /// Server stream port.
    pub port: u16,
    /// Client name shown in Snapcast controllers.
    pub client_name: String,
    /// Instance number, to run several clients on one host: the server identifies
    /// a client as `<host id>#<instance>`. `0` (the default) picks the lowest number
    /// no other engine client on this host uses (see the `instance` module); an
    /// explicit number is used as is, with a warning if another client holds it.
    pub instance: u32,
    /// Additional local latency in milliseconds (e.g. of the audio device).
    pub latency_ms: i32,
    /// With an empty `host`: only accept a server announced under this mDNS instance
    /// name (empty: the first one found).
    pub mdns_name: String,
    /// Unique host id sent to the server; empty means snapcast-client's default,
    /// the MAC address. Like the C++ snapclient's `--hostID`.
    pub host_id: String,
    /// A stream to play instead of the one the server assigns (empty: the server's
    /// choice). The client then joins it via the control API, after every
    /// (re)connect, and follows it as a session (see the `session` module and
    /// [`ClientHandle::session_properties`]).
    pub stream: String,
    /// The server's JSON-RPC control port (TCP) for joining `stream`; 0: the stream
    /// port + 1 (Snapcast's default layout, 1704/1705).
    pub control_port: u16,
    /// Id for log lines of this client (e.g. the element name), as the
    /// [`crate::LOG_ID_FIELD`] of the span its work runs in. Empty: none.
    pub log_id: String,
}

impl Default for ClientOptions {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_owned(),
            port: snapcast_client::DEFAULT_STREAM_PORT,
            client_name: "gst-snapsrc".to_owned(),
            instance: 0,
            latency_ms: 0,
            host_id: String::new(),
            mdns_name: String::new(),
            stream: String::new(),
            control_port: 0,
            log_id: String::new(),
        }
    }
}

/// Decoded audio with its local play-out time.
#[derive(Debug, Clone)]
pub struct PlayoutFrame {
    /// Interleaved f32 samples.
    pub samples: Vec<f32>,
    /// Sample rate in Hz.
    pub rate: u32,
    /// Number of channels.
    pub channels: u16,
    /// Local monotonic time in µs at which the first sample must be played.
    pub playout_usec: i64,
    /// The server's volume for this client in percent (0–100) when the frame was
    /// handed out; 100 until the first server settings arrive.
    pub volume: u16,
    /// Muted by the server.
    pub muted: bool,
}

impl PlayoutFrame {
    /// The amplitude factor to apply, see [`gain`].
    pub fn gain(&self) -> f32 {
        gain(self.volume, self.muted)
    }
}

/// The amplitude factor for a Snapcast volume in percent, 0 when muted.
///
/// The curve is the C++ snapclient's default software mixer
/// (`Player::setVolume_exp` with base 10, `client/player/player.cpp`):
///
/// ```text
/// gain = (10^v - 1) / 9,   v = percent / 100
/// ```
///
/// We hear loudness roughly logarithmically in amplitude, so a volume slider that
/// feels even needs a gain that grows exponentially with it. This one is close to
/// even steps in dB over most of the range (100 % = 0 dB, 75 % ≈ -5.8 dB,
/// 50 % ≈ -12.4 dB, 25 % ≈ -21.3 dB, 10 % ≈ -30.8 dB), and it reaches true silence at
/// 0 %. Using the same curve as the reference client also means that rooms at the
/// same percent sound equally loud, whichever client plays them.
///
/// Until 2026-11-27 the gain was linear (`percent / 100`, like snapclient-rs):
/// 40 % was only -8 dB, so most of a controller's slider barely changed anything
/// (DLNA test 11), and snapsrc rooms played louder than C++ ones at the same
/// percent.
pub fn gain(volume: u16, muted: bool) -> f32 {
    if muted {
        return 0.0;
    }
    let v = f64::from(volume.min(100)) / 100.0;
    ((10f64.powf(v) - 1.0) / 9.0) as f32
}

#[derive(Debug, Default)]
struct Status {
    /// Effective buffer: server buffer minus server and client latency.
    buffer_ms: Option<i32>,
    /// snapcast-client reported a usable clock difference on this connection.
    time_synced: bool,
    last_error: Option<String>,
}

/// A running client. Dropping it stops the client.
pub struct ClientHandle {
    audio_rx: mpsc::Receiver<snapcast_client::AudioFrame>,
    cmd_tx: mpsc::Sender<ClientCommand>,
    status: Arc<Mutex<Status>>,
    time_provider: Arc<Mutex<snapcast_client::time_provider::TimeProvider>>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    instance: u32,
    /// The server address actually used (after mDNS discovery, if any).
    server: (String, u16),
    /// The server's volume (percent) and mute state for this client.
    volume: watch::Receiver<(u16, bool)>,
    /// The control connection of [`ClientOptions::stream`], if set.
    session: Option<session::Session>,
    /// Keeps the instance number claimed while the client lives.
    _instance_lock: Option<instance::InstanceLock>,
}

impl std::fmt::Debug for ClientHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientHandle")
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
}

/// The snapcast-client configuration for `options`.
fn client_config(options: &ClientOptions, instance: u32) -> ClientConfig {
    ClientConfig {
        host: options.host.clone(),
        port: options.port,
        client_name: options.client_name.clone(),
        instance,
        latency: options.latency_ms,
        host_id: options.host_id.clone(),
        // We schedule the AudioFrames ourselves; nothing would drain the player
        // stream (it grew without bound before this option existed, L2).
        player_stream: false,
        ..ClientConfig::default()
    }
}

/// The instance number to use for `requested` (0 = automatic), and its lock.
///
/// Lock problems (e.g. no writable runtime dir) never stop the client: it then
/// runs without coordination, as instance 1 if automatic.
fn claim_instance(requested: u32) -> (u32, Option<instance::InstanceLock>) {
    let dir = instance::lock_dir();
    let result = if requested == 0 {
        instance::claim_free(&dir).map(Some)
    } else {
        instance::claim(&dir, requested)
    };
    match result {
        Ok(Some(lock)) => {
            tracing::debug!(instance = lock.number, "Claimed client instance");
            (lock.number, Some(lock))
        }
        Ok(None) => {
            tracing::warn!(
                instance = requested,
                "Client instance is already used by another client on this host; \
                 the server will mix them up (leave the instance at 0 for automatic)"
            );
            (requested, None)
        }
        Err(e) => {
            let fallback = requested.max(1);
            tracing::warn!(error = %e, dir = %dir.display(), instance = fallback,
                "Cannot coordinate client instances on this host");
            (fallback, None)
        }
    }
}

/// How long [`connect`] looks for a server via mDNS when no host is given.
pub const DISCOVERY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Start a client connecting to `options.host:options.port`.
///
/// With an empty `host`, the first server found via mDNS within
/// [`DISCOVERY_TIMEOUT`] is used, host *and* port (this blocks that long at most).
/// Discovery runs once: reconnects go to the same address.
pub fn connect(options: &ClientOptions) -> Result<ClientHandle, EngineError> {
    // Discovery, instance claim and both tasks log in this span.
    let span = tracing::info_span!("snapcast_client", log_id = %options.log_id);
    let _entered = span.enter();
    let discovered;
    let options = if options.host.is_empty() {
        let name = Some(options.mdns_name.as_str()).filter(|n| !n.is_empty());
        let (host, port) = crate::mdns::discover(DISCOVERY_TIMEOUT, name)?;
        discovered = ClientOptions {
            host,
            port,
            ..options.clone()
        };
        &discovered
    } else {
        options
    };
    let (instance, instance_lock) = claim_instance(options.instance);
    let (mut client, mut events, audio_rx) = SnapClient::new(client_config(options, instance));
    let cmd_tx = client.command_sender();
    let time_provider = Arc::clone(&client.time_provider);
    let status = Arc::new(Mutex::new(Status::default()));
    // Full volume until the server says otherwise. Not part of `status`: it must reach
    // consumers even without audio (a C++ snapserver sends none to muted clients).
    let (volume_tx, volume_rx) = watch::channel((100u16, false));

    let rt = runtime();
    let run_status = Arc::clone(&status);
    let run_task = rt.spawn(
        async move {
            if let Err(e) = client.run().await {
                tracing::error!(error = %e, "Snapcast client failed");
                run_status.lock().unwrap().last_error = Some(e.to_string());
            }
        }
        .in_current_span(),
    );
    let event_status = Arc::clone(&status);
    let client_latency = options.latency_ms;
    let mut tasks = vec![run_task];
    let session = (!options.stream.is_empty()).then(|| {
        let _runtime = rt.enter(); // session::start spawns its task
        let (session, task) = {
            session::start(session::Target {
                host: options.host.clone(),
                port: if options.control_port == 0 {
                    options.port.saturating_add(1)
                } else {
                    options.control_port
                },
                client_id: session::client_id(&options.host_id, instance),
                stream: options.stream.clone(),
            })
        };
        tasks.push(task);
        session
    });
    let rejoin = session.as_ref().map(|s| Arc::clone(&s.rejoin));
    let mut was_disconnected = false;
    let event_task = rt.spawn(
        async move {
            while let Some(event) = events.recv().await {
                tracing::debug!(?event, "Snapcast client event");
                let mut status = event_status.lock().unwrap();
                match event {
                    ClientEvent::ServerSettings {
                        buffer_ms,
                        latency,
                        volume,
                        muted,
                    } => {
                        status.buffer_ms = Some((buffer_ms - latency - client_latency).max(0));
                        volume_tx.send_replace((volume, muted));
                        // Connected again: the server may have put this client on
                        // another stream (the first connection joins by itself).
                        if std::mem::take(&mut was_disconnected)
                            && let Some(rejoin) = &rejoin
                        {
                            rejoin.notify_one();
                        }
                    }
                    ClientEvent::VolumeChanged { volume, muted } => {
                        volume_tx.send_replace((volume, muted));
                    }
                    ClientEvent::TimeSyncUsable { .. } | ClientEvent::TimeSyncComplete { .. } => {
                        status.time_synced = true;
                    }
                    ClientEvent::Disconnected { reason } => {
                        was_disconnected = true;
                        status.time_synced = false;
                        status.last_error = Some(reason);
                    }
                    _ => {}
                }
            }
        }
        .in_current_span(),
    );

    Ok(ClientHandle {
        audio_rx,
        cmd_tx,
        status,
        time_provider,
        tasks: {
            tasks.push(event_task);
            tasks
        },
        session,
        instance,
        server: (options.host.clone(), options.port),
        volume: volume_rx,
        _instance_lock: instance_lock,
    })
}

impl ClientHandle {
    /// Wait for the next frame that can be scheduled.
    ///
    /// Returns `None` if the client has terminated.
    pub async fn recv(&mut self) -> Option<PlayoutFrame> {
        loop {
            let frame = self.audio_rx.recv().await?;

            let (volume, muted) = *self.volume.borrow();
            let (buffer_ms, synced) = {
                let status = self.status.lock().unwrap();
                (status.buffer_ms, status.time_synced)
            };
            let Some(buffer_ms) = buffer_ms.filter(|_| synced) else {
                continue; // not yet schedulable
            };
            let diff = self
                .time_provider
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .diff_to_server_usec();
            return Some(PlayoutFrame {
                playout_usec: frame.timestamp_usec - diff + i64::from(buffer_ms) * 1000,
                samples: frame.samples,
                rate: frame.sample_rate,
                channels: frame.channels,
                volume,
                muted,
            });
        }
    }

    /// The server's volume (percent) and mute state for this client, updated as the
    /// server changes them, also while no audio flows (e.g. when muted).
    pub fn volume(&self) -> watch::Receiver<(u16, bool)> {
        self.volume.clone()
    }

    /// The server this client connects to, as `(host, port)`: the configured one,
    /// or the one found via mDNS.
    pub fn server(&self) -> (&str, u16) {
        (&self.server.0, self.server.1)
    }

    /// The instance number this client uses (see [`ClientOptions::instance`]).
    pub fn instance(&self) -> u32 {
        self.instance
    }

    /// The session's stream properties as the server reports them (`None` until
    /// known), updated on every change. `None` without [`ClientOptions::stream`].
    pub fn session_properties(&self) -> Option<watch::Receiver<Option<SessionProperties>>> {
        self.session.as_ref().map(|s| s.properties.clone())
    }

    /// A handle to send commands to the session (`None` without
    /// [`ClientOptions::stream`]). It can be cloned and used from any thread,
    /// independent of this handle.
    pub fn session_control(&self) -> Option<SessionControl> {
        self.session.as_ref().map(|s| SessionControl {
            requests: s.requests.clone(),
        })
    }

    /// Last error reported by the client, if any.
    pub fn last_error(&self) -> Option<String> {
        self.status.lock().unwrap().last_error.clone()
    }
}

impl Drop for ClientHandle {
    fn drop(&mut self) {
        let _ = self.cmd_tx.try_send(ClientCommand::Stop);
        for task in &self.tasks {
            task.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_map_to_the_client_config() {
        let options = ClientOptions {
            host: "10.0.0.2".into(),
            port: 1800,
            client_name: "kitchen".into(),
            instance: 3,
            latency_ms: 40,
            host_id: "kitchen-pi".into(),
            mdns_name: String::new(),
            stream: String::new(),
            control_port: 0,
            log_id: "snapsrc0".into(),
        };
        let c = client_config(&options, options.instance);
        assert_eq!((c.host.as_str(), c.port), ("10.0.0.2", 1800));
        assert_eq!(
            (c.client_name.as_str(), c.instance, c.latency),
            ("kitchen", 3, 40)
        );
        assert_eq!(c.host_id, "kitchen-pi");
        assert!(
            !c.player_stream,
            "the engine never drains the player stream"
        );
        // Empty host id: snapcast-client's default (the MAC address).
        assert_eq!(client_config(&ClientOptions::default(), 1).host_id, "");
        assert_eq!(ClientOptions::default().instance, 0, "automatic by default");
    }

    #[test]
    fn gain_follows_the_cpp_client_and_is_zero_when_muted() {
        assert_eq!(gain(100, false), 1.0);
        assert_eq!(gain(0, false), 0.0);
        assert_eq!(gain(80, true), 0.0);
        assert_eq!(gain(150, false), 1.0, "clamped");
        // (10^0.5 - 1) / 9: the C++ snapclient's default software mixer.
        assert!(
            (gain(50, false) - 0.240_25).abs() < 1e-4,
            "{}",
            gain(50, false)
        );
        let db = |p| 20.0 * gain(p, false).log10();
        assert!((db(50) - -12.4).abs() < 0.1, "50 %: {} dB", db(50));
        assert!((db(10) - -30.8).abs() < 0.1, "10 %: {} dB", db(10));
        // Monotonic: every step is a little louder.
        assert!((1..=100).all(|p| gain(p, false) > gain(p - 1, false)));
    }
}
