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

/// How the daemon serves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    /// Return once no sender has been connected for that long (`None`: serve
    /// until accepting fails).
    pub grace: Option<Duration>,
    /// How long a released stream stays on its server
    /// ([`server::ServerOptions::stream_linger`]). The daemon's setting, not the
    /// senders': it outlives them, and one sender restarting shouldn't decide
    /// for the others.
    pub stream_linger: Option<Duration>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            grace: None,
            stream_linger: Some(server::DEFAULT_STREAM_LINGER),
        }
    }
}

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
/// With [`Options::grace`], return once no sender has been connected for that long (also
/// right after the start, when the sender that spawned the daemon never
/// connects). Connections that arrive while returning are closed at once, so
/// that their sender retries (and spawns a new daemon). Without it, serve until
/// accepting fails.
pub fn serve(listener: &UnixListener, options: &Options) -> std::io::Result<()> {
    let linger = options.stream_linger;
    let shared: Shared = Arc::new((
        Mutex::new(Lifetime {
            connections: 0,
            since: Instant::now(),
            closing: false,
        }),
        Condvar::new(),
    ));
    let Some(grace) = options.grace else {
        return accept_loop(listener, &shared, linger);
    };
    let accepting = listener.try_clone()?;
    let for_accept = Arc::clone(&shared);
    std::thread::Builder::new()
        .name("gst-snapcast-daemon-accept".into())
        .spawn(move || {
            if let Err(e) = accept_loop(&accepting, &for_accept, linger) {
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

fn accept_loop(
    listener: &UnixListener,
    shared: &Shared,
    linger: Option<Duration>,
) -> std::io::Result<()> {
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
                if let Err(e) = handle_connection(stream, linger) {
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

/// One sender: `Hello`, `Acquire`, then PCM and properties until the connection
/// closes; commands for the stream go back to it.
fn handle_connection(stream: UnixStream, linger: Option<Duration>) -> std::io::Result<()> {
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
    let server_options = server::ServerOptions {
        stream_linger: linger,
        ..server_settings.to_options()
    };
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

    // From here on, two directions at once: this thread reads the sender's
    // frames; a writer thread sends the controllers' commands (which arrive on the
    // engine runtime and must not block there) and a final error.
    let (outgoing, to_send) = std::sync::mpsc::channel::<Message>();
    let writer_thread = std::thread::Builder::new()
        .name("gst-snapcast-daemon-write".into())
        .spawn(move || {
            for message in to_send {
                if ipc::write_message(&mut writer, &message).is_err() {
                    break;
                }
            }
        })?;
    let commands = outgoing.clone();
    handle.on_command(move |command| {
        tracing::debug!(command = %command.command, "Stream command for the sender");
        let _ = commands.send(Message::Command((&command).into()));
    });

    let result = loop {
        match ipc::read_message(&mut reader) {
            Ok(Some(Message::Pcm {
                timestamp_usec,
                data,
            })) => {
                if let Err(e) = handle.send_pcm(data, timestamp_usec) {
                    let _ = outgoing.send(Message::Error((&e).into()));
                    break Ok(());
                }
            }
            Ok(Some(Message::Properties(properties))) => {
                if let Err(e) = handle.set_properties(&(&properties).into()) {
                    tracing::warn!(error = %e, "Stream properties not applied");
                }
            }
            Ok(Some(_)) => {
                let error = IpcError {
                    kind: ErrorKind::Protocol,
                    message: "only PCM and Properties after Acquire".into(),
                };
                tracing::info!(message = %error.message, "Refused");
                let _ = outgoing.send(Message::Error(error));
                break Ok(());
            }
            Ok(None) => break Ok(()),
            Err(e) => break Err(e),
        }
    };
    tracing::info!(stream = %handle.name(), "Stream released");
    // Releasing the stream also drops the command handler and its sender, so
    // the writer thread ends once it has sent what is queued.
    drop(handle);
    drop(outgoing);
    let _ = writer_thread.join();
    result
}
