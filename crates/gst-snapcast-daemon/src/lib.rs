//! The shared Snapcast server daemon: one process per user that hosts the Snapcast
//! servers for all `snapsink` pipelines of that user.
//!
//! Design: `docs/shared-server.md`. The daemon is a thin process around the
//! engine: each connection on its Unix socket does what an embedded `snapsink`
//! does, one [`server::acquire_stream`], and closing the connection releases the
//! stream. The protocol is in [`gst_snapcast_engine::ipc`].
//!
//! # Threads
//!
//! One thread per connection reads its frames and feeds the stream
//! ([`StreamHandle::send_pcm`](server::StreamHandle::send_pcm) blocks while the
//! stream's queue is full, which in turn fills the socket buffer and blocks the
//! sender: that is the backpressure). The servers themselves run on the engine
//! runtime, as in a pipeline.
//!
//! # Lifetime
//!
//! [`serve`] counts the connections; the binary exits once there has been none for
//! a grace period (default 5 s), which covers senders that restart. It is started
//! by the first sender that finds no daemon (autospawn), and only one runs per
//! user: it holds a lock file next to its socket for its whole life.

#![warn(missing_docs)]

use std::io::{BufReader, BufWriter};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use gst_snapcast_engine::ipc::{
    self, Acquire, Acquired, ErrorKind, IpcError, Message, PROTOCOL_VERSION, ServerSettings,
    Welcome,
};
use gst_snapcast_engine::server;

/// The daemon's package version, sent in [`Welcome`].
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// How many senders are connected, and whether the daemon is shutting down.
#[derive(Debug)]
struct Lifetime {
    connections: usize,
    /// When the number of connections last changed.
    since: Instant,
    /// Set when the daemon decided to exit; later connections are dropped.
    closing: bool,
}

type Shared = Arc<(Mutex<Lifetime>, Condvar)>;

/// Counts a connection while it lives.
struct Connection(Shared);

impl Drop for Connection {
    fn drop(&mut self) {
        let (lock, changed) = &*self.0;
        let mut lifetime = lock.lock().unwrap();
        lifetime.connections -= 1;
        lifetime.since = Instant::now();
        changed.notify_all();
    }
}

/// Serve connections on `listener`, each on its own thread.
///
/// With `grace`, return once no sender has been connected for that long (also
/// right after the start, when the sender that spawned the daemon never
/// connects). Connections that arrive while returning are closed at once, so
/// that their sender retries (and spawns a new daemon). Without `grace`, serve
/// until accepting fails.
pub fn serve(listener: &UnixListener, grace: Option<Duration>) -> std::io::Result<()> {
    let shared: Shared = Arc::new((
        Mutex::new(Lifetime {
            connections: 0,
            since: Instant::now(),
            closing: false,
        }),
        Condvar::new(),
    ));
    let Some(grace) = grace else {
        return accept_loop(listener, &shared);
    };
    let accepting = listener.try_clone()?;
    let for_accept = Arc::clone(&shared);
    std::thread::Builder::new()
        .name("gst-snapcast-daemon-accept".into())
        .spawn(move || {
            if let Err(e) = accept_loop(&accepting, &for_accept) {
                tracing::error!(error = %e, "Accepting connections failed");
            }
        })?;

    let (lock, changed) = &*shared;
    let mut lifetime = lock.lock().unwrap();
    loop {
        let idle = lifetime.since.elapsed();
        if lifetime.connections == 0 && idle >= grace {
            lifetime.closing = true;
            tracing::info!(?grace, "No senders, exiting");
            return Ok(());
        }
        let wait = if lifetime.connections == 0 {
            grace - idle
        } else {
            grace
        };
        lifetime = changed.wait_timeout(lifetime, wait).unwrap().0;
    }
}

fn accept_loop(listener: &UnixListener, shared: &Shared) -> std::io::Result<()> {
    loop {
        let (stream, _) = listener.accept()?;
        {
            let (lock, changed) = &**shared;
            let mut lifetime = lock.lock().unwrap();
            if lifetime.closing {
                // The sender sees the connection closed and retries.
                continue;
            }
            lifetime.connections += 1;
            lifetime.since = Instant::now();
            changed.notify_all();
        }
        let connection = Connection(Arc::clone(shared));
        std::thread::Builder::new()
            .name("gst-snapcast-daemon-conn".into())
            .spawn(move || {
                let _connection = connection;
                if let Err(e) = handle_connection(stream) {
                    tracing::warn!(error = %e, "Connection failed");
                }
            })?;
    }
}

/// Send an error to the sender (best effort); the connection ends afterwards.
fn reject(
    writer: &mut impl std::io::Write,
    kind: ErrorKind,
    message: impl Into<String>,
) -> std::io::Result<()> {
    let error = IpcError {
        kind,
        message: message.into(),
    };
    tracing::info!(?kind, message = %error.message, "Refused");
    ipc::write_message(writer, &Message::Error(error))
}

/// One sender: `Hello`, `Acquire`, then PCM until the connection closes.
fn handle_connection(stream: UnixStream) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut writer = BufWriter::new(stream);

    let hello = match ipc::read_message(&mut reader)? {
        Some(Message::Hello(hello)) => hello,
        Some(_) => return reject(&mut writer, ErrorKind::Protocol, "expected Hello"),
        None => return Ok(()),
    };
    let span = tracing::info_span!(
        "ipc_sender",
        log_id = %hello.log_id,
        pid = hello.pid
    );
    let _entered = span.enter();
    if hello.version != PROTOCOL_VERSION {
        return reject(
            &mut writer,
            ErrorKind::Version,
            format!(
                "protocol version {} of the sender, {PROTOCOL_VERSION} of the daemon {VERSION}",
                hello.version
            ),
        );
    }
    ipc::write_message(
        &mut writer,
        &Message::Welcome(Welcome {
            version: PROTOCOL_VERSION,
            daemon_version: VERSION.into(),
        }),
    )?;

    let Acquire {
        server: server_settings,
        stream: stream_settings,
    } = match ipc::read_message(&mut reader)? {
        Some(Message::Acquire(acquire)) => acquire,
        Some(_) => return reject(&mut writer, ErrorKind::Protocol, "expected Acquire"),
        None => return Ok(()),
    };
    // Mode::Embedded: the server runs here, in the daemon.
    let server_options = server_settings.to_options();
    let handle = match server::acquire_stream(&server_options, &stream_settings.to_options()) {
        Ok(handle) => handle,
        Err(e) => {
            tracing::info!(stream = %stream_settings.name, error = %e, "Acquisition failed");
            return ipc::write_message(&mut writer, &Message::Error((&e).into()));
        }
    };
    tracing::info!(stream = %handle.name(), port = handle.port(), "Stream acquired");
    ipc::write_message(
        &mut writer,
        &Message::Acquired(Acquired {
            port: handle.port(),
            control_tcp_port: handle.control_tcp_port(),
            control_http_port: handle.control_http_port(),
            server: ServerSettings::from_options(handle.server_options()),
        }),
    )?;

    let result = loop {
        match ipc::read_message(&mut reader) {
            Ok(Some(Message::Pcm {
                timestamp_usec,
                data,
            })) => {
                if let Err(e) = handle.send_pcm(data, timestamp_usec) {
                    break ipc::write_message(&mut writer, &Message::Error((&e).into()));
                }
            }
            Ok(Some(_)) => {
                break reject(&mut writer, ErrorKind::Protocol, "only PCM after Acquire");
            }
            Ok(None) => break Ok(()),
            Err(e) => break Err(e),
        }
    };
    tracing::info!(stream = %handle.name(), "Stream released");
    result
}
