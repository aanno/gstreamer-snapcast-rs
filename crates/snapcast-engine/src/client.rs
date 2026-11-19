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

use crate::{EngineError, runtime};

mod instance;

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
    /// The linear gain to apply, see [`gain`].
    pub fn gain(&self) -> f32 {
        gain(self.volume, self.muted)
    }
}

/// Linear gain for a Snapcast volume in percent, like snapclient-rs's software
/// mixer: `percent / 100`, 0 when muted.
pub fn gain(volume: u16, muted: bool) -> f32 {
    if muted {
        0.0
    } else {
        f32::from(volume.min(100)) / 100.0
    }
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
    let run_task = rt.spawn(async move {
        if let Err(e) = client.run().await {
            tracing::error!(error = %e, "Snapcast client failed");
            run_status.lock().unwrap().last_error = Some(e.to_string());
        }
    });
    let event_status = Arc::clone(&status);
    let client_latency = options.latency_ms;
    let event_task = rt.spawn(async move {
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
                }
                ClientEvent::VolumeChanged { volume, muted } => {
                    volume_tx.send_replace((volume, muted));
                }
                ClientEvent::TimeSyncUsable { .. } | ClientEvent::TimeSyncComplete { .. } => {
                    status.time_synced = true;
                }
                ClientEvent::Disconnected { reason } => {
                    status.time_synced = false;
                    status.last_error = Some(reason);
                }
                _ => {}
            }
        }
    });

    Ok(ClientHandle {
        audio_rx,
        cmd_tx,
        status,
        time_provider,
        tasks: vec![run_task, event_task],
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
    fn gain_is_linear_and_zero_when_muted() {
        assert_eq!(gain(100, false), 1.0);
        assert_eq!(gain(50, false), 0.5);
        assert_eq!(gain(0, false), 0.0);
        assert_eq!(gain(80, true), 0.0);
        assert_eq!(gain(150, false), 1.0, "clamped");
    }
}
