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

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, Weak};

use snapcast_server::{
    AudioData, AudioFrame, ServerCommand, ServerConfig, SnapServer, StreamConfig,
};
use tokio::sync::mpsc;

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
}

impl Default for ServerOptions {
    fn default() -> Self {
        Self {
            port: snapcast_server::DEFAULT_STREAM_PORT,
            buffer_ms: 1000,
        }
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
}

impl Drop for ServerEntry {
    fn drop(&mut self) {
        let _ = self.cmd_tx.try_send(ServerCommand::Stop);
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

    let config = ServerConfig {
        buffer_ms: server.buffer_ms,
        codec: stream.codec.clone(),
        sample_format: stream.format.to_string(),
        ..ServerConfig::default()
    };
    let (mut snap, mut events) = SnapServer::new(config);
    let tx = snap.add_stream_with_config(
        &stream.name,
        StreamConfig {
            codec: Some(stream.codec.clone()),
            sample_format: Some(stream.format.to_string()),
        },
    );
    let cmd_tx = snap.command_sender();

    let rt = runtime();
    let listener = {
        let _guard = rt.enter();
        tokio::net::TcpListener::from_std(std_listener)
            .map_err(|e| EngineError::Bind(server.port, e))?
    };
    rt.spawn(async move {
        if let Err(e) = snap.serve(listener).await {
            tracing::error!(error = %e, port = local_port, "Snapcast server failed");
        }
    });
    // The event channel is bounded; drain it so the server never stalls.
    rt.spawn(async move {
        while let Some(event) = events.recv().await {
            tracing::debug!(?event, "Snapcast server event");
        }
    });

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
    Ok(Arc::new(ServerEntry {
        requested_port: server.port,
        local_port,
        cmd_tx,
        streams: Mutex::new(streams),
    }))
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
        }
    }

    #[test]
    fn stream_is_exclusive_and_server_is_shared_then_released() {
        let opts = ServerOptions {
            port: free_port(),
            buffer_ms: 500,
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

    #[test]
    fn port_zero_is_ephemeral_and_never_shared() {
        let opts = ServerOptions {
            port: 0,
            buffer_ms: 500,
        };
        let a = acquire_stream(&opts, &stream("x")).unwrap();
        let b = acquire_stream(&opts, &stream("x")).unwrap();
        assert_ne!(a.port(), 0);
        assert_ne!(a.port(), b.port());
    }
}
