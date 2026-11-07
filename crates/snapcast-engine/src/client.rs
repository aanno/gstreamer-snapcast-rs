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
//! been received and time sync has had [`MIN_SYNC_TIME`] to collect samples
//! (snapcast-client only reports completion after 50 quick syncs, i.e. 5 s;
//! its median estimate keeps improving while we already play).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use snapcast_client::{ClientCommand, ClientConfig, ClientEvent, SnapClient};
use tokio::sync::mpsc;

use crate::{EngineError, runtime};

/// Time sync runs every 100 ms during the first seconds after connecting;
/// after this long, enough samples exist for a usable median.
pub const MIN_SYNC_TIME: Duration = Duration::from_millis(500);

/// Client options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientOptions {
    /// Server host name or address (mDNS discovery is not supported).
    pub host: String,
    /// Server stream port.
    pub port: u16,
    /// Client name shown in Snapcast controllers.
    pub client_name: String,
    /// Instance id, to run several clients on one host.
    pub instance: u32,
    /// Additional local latency in milliseconds (e.g. of the audio device).
    pub latency_ms: i32,
}

impl Default for ClientOptions {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_owned(),
            port: snapcast_client::DEFAULT_STREAM_PORT,
            client_name: "gst-snapsrc".to_owned(),
            instance: 1,
            latency_ms: 0,
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
}

#[derive(Debug, Default)]
struct Status {
    /// Effective buffer: server buffer minus server and client latency.
    buffer_ms: Option<i32>,
    time_synced: bool,
    connected_at: Option<Instant>,
    last_error: Option<String>,
}

/// A running client. Dropping it stops the client.
pub struct ClientHandle {
    audio_rx: mpsc::Receiver<snapcast_client::AudioFrame>,
    cmd_tx: mpsc::Sender<ClientCommand>,
    status: Arc<Mutex<Status>>,
    time_provider: Arc<Mutex<snapcast_client::time_provider::TimeProvider>>,
    player_stream: Arc<Mutex<snapcast_client::stream::Stream>>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl std::fmt::Debug for ClientHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientHandle")
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
}

/// Start a client connecting to `options.host:options.port`.
pub fn connect(options: &ClientOptions) -> Result<ClientHandle, EngineError> {
    if options.host.is_empty() {
        return Err(EngineError::InvalidConfig("host must not be empty".into()));
    }
    let config = ClientConfig {
        host: options.host.clone(),
        port: options.port,
        client_name: options.client_name.clone(),
        instance: options.instance,
        latency: options.latency_ms,
        ..ClientConfig::default()
    };
    let (mut client, mut events, audio_rx) = SnapClient::new(config);
    let cmd_tx = client.command_sender();
    let time_provider = Arc::clone(&client.time_provider);
    let player_stream = Arc::clone(&client.stream);
    let status = Arc::new(Mutex::new(Status::default()));

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
                    buffer_ms, latency, ..
                } => status.buffer_ms = Some((buffer_ms - latency - client_latency).max(0)),
                ClientEvent::Connected { .. } => status.connected_at = Some(Instant::now()),
                ClientEvent::TimeSyncComplete { .. } => status.time_synced = true,
                ClientEvent::Disconnected { reason } => {
                    status.time_synced = false;
                    status.connected_at = None;
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
        player_stream,
        tasks: vec![run_task, event_task],
    })
}

impl ClientHandle {
    /// Wait for the next frame that can be scheduled.
    ///
    /// Returns `None` if the client has terminated.
    pub async fn recv(&mut self) -> Option<PlayoutFrame> {
        loop {
            let frame = self.audio_rx.recv().await?;

            // snapcast-client also queues every chunk into its shared player
            // `Stream`, which only a cpal-style player drains. We schedule via
            // GStreamer instead, so keep that queue from growing unbounded.
            // TODO(snapcast-rs): make the player stream optional upstream.
            self.player_stream
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clear();

            let (buffer_ms, synced) = {
                let status = self.status.lock().unwrap();
                let synced = status.time_synced
                    || status
                        .connected_at
                        .is_some_and(|t| t.elapsed() >= MIN_SYNC_TIME);
                (status.buffer_ms, synced)
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
            });
        }
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
