//! The shared backend of [`crate::server::StreamHandle`]: a stream acquired from the
//! per-user server daemon (`gst-snapcast-daemon`) over its Unix socket.
//!
//! Design: `docs/shared-server.md`. The acquisition is one connection: `Hello`,
//! `Acquire`, then PCM frames; dropping the stream closes the connection, which
//! releases the stream in the daemon.
//!
//! # Autospawn
//!
//! When no daemon answers, the first sender starts one: `GST_SNAPCAST_DAEMON` (a
//! path), else `gst-snapcast-daemon` from `PATH`, in its own process group (so
//! that Ctrl+C on the sender's terminal does not reach it), logging to
//! `daemon.log` next to the socket. Senders that start one at the same time are
//! sorted out by the daemon's lock file. A thread waits for the child, so that it
//! does not linger as a zombie when the sender outlives it.
//!
//! # Reconnect
//!
//! If the connection breaks while sending (the daemon crashed or was killed),
//! the stream connects once more, which starts a new daemon, acquires its stream
//! again, sends the last properties again and resends the frame. Only if that
//! fails, sending fails.
//!
//! # Commands
//!
//! A reader thread per connection receives the controllers' commands for the
//! stream and calls the handler set with [`SharedStream::on_command`]; it ends
//! when the connection closes.

use std::io::{self, BufReader};
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::EngineError;
use crate::ipc::{self, Acquire, Acquired, ErrorKind, Hello, IpcError, Message, PROTOCOL_VERSION};
use crate::server::{Mode, ServerOptions, StreamCommand, StreamOptions, StreamProperties};

/// How long a read from the daemon (its answers) may take.
const READ_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a PCM write may block when the daemon does not read (backpressure
/// beyond this means it hangs).
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);
/// How long to wait for a spawned daemon to listen. It normally takes a few ms,
/// but on a loaded machine (seen in `make check`) almost 2 s.
const SPAWN_WAIT: Duration = Duration::from_secs(5);
/// The daemon binary, unless `GST_SNAPCAST_DAEMON` names one.
const DAEMON_BINARY: &str = "gst-snapcast-daemon";

/// Why the shared backend could not be used: no usable daemon (no runtime
/// directory, the binary is missing, another protocol version, a broken
/// connection), or the daemon refused the acquisition.
#[derive(Debug)]
pub(crate) enum Failure {
    /// No usable daemon; what happened.
    Unavailable(String),
    /// The daemon answered the acquisition with an error (e.g. the stream is taken).
    Engine(EngineError),
}

/// A stream fed through the daemon.
pub(crate) struct SharedStream {
    /// Writes are whole frames under this lock; replaced on reconnect.
    writer: Mutex<UnixStream>,
    pub(crate) port: u16,
    pub(crate) control_tcp_port: Option<u16>,
    pub(crate) control_http_port: Option<u16>,
    /// The server-wide options in effect in the daemon.
    pub(crate) server_options: ServerOptions,
    /// What was asked for, to acquire again after a reconnect.
    requested: (ServerOptions, StreamOptions),
    /// The properties last sent, to send again after a reconnect.
    properties: Mutex<Option<ipc::Properties>>,
    /// Where commands from controllers go (shared with the reader threads).
    handler: CommandSlot,
}

/// The command handler of a stream, read by its reader threads.
type CommandSlot = Arc<Mutex<Option<Arc<dyn Fn(StreamCommand) + Send + Sync>>>>;

/// Read what the daemon sends after `Acquired` until the connection closes:
/// commands go to the handler, an error is logged.
fn spawn_reader(connection: &UnixStream, handler: CommandSlot) -> io::Result<()> {
    let mut reader = BufReader::new(connection.try_clone()?);
    // Commands come whenever a controller sends one: no read timeout.
    connection.set_read_timeout(None)?;
    let span = tracing::Span::current();
    std::thread::Builder::new()
        .name("gst-snapcast-daemon-read".into())
        .spawn(move || {
            let _entered = span.enter();
            loop {
                match ipc::read_message(&mut reader) {
                    Ok(Some(Message::Command(command))) => {
                        // Clone it out of the lock: it may take a while.
                        let handler = handler.lock().unwrap().clone();
                        match handler {
                            Some(handler) => handler(command.into()),
                            None => tracing::info!(
                                command = %command.command,
                                "No handler for stream command"
                            ),
                        }
                    }
                    Ok(Some(Message::Error(e))) => {
                        tracing::warn!(error = %e.message, "The daemon reported an error");
                    }
                    Ok(Some(other)) => {
                        tracing::warn!(?other, "Unexpected message from the daemon");
                    }
                    Ok(None) | Err(_) => break,
                }
            }
        })?;
    Ok(())
}

/// Connect to the daemon (starting one if none answers) and acquire `stream`.
pub(crate) fn acquire(
    server: &ServerOptions,
    stream: &StreamOptions,
) -> Result<SharedStream, Failure> {
    let (writer, acquired) = connect_and_acquire(server, stream)?;
    let handler = CommandSlot::default();
    spawn_reader(&writer, Arc::clone(&handler))
        .map_err(|e| Failure::Unavailable(format!("daemon connection: {e}")))?;
    let mut server_options = acquired.server.to_options();
    server_options.mode = Mode::Shared;
    Ok(SharedStream {
        writer: Mutex::new(writer),
        port: acquired.port,
        control_tcp_port: acquired.control_tcp_port,
        control_http_port: acquired.control_http_port,
        server_options,
        requested: (server.clone(), stream.clone()),
        properties: Mutex::new(None),
        handler,
    })
}

/// Connect (or spawn) and do the handshake. A daemon that is just exiting closes
/// new connections at once; then this tries once more, which spawns a new one.
fn connect_and_acquire(
    server: &ServerOptions,
    stream: &StreamOptions,
) -> Result<(UnixStream, Acquired), Failure> {
    let Some(dir) = ipc::runtime_dir() else {
        return Err(Failure::Unavailable("XDG_RUNTIME_DIR is not set".into()));
    };
    let mut attempt = 0;
    loop {
        attempt += 1;
        let connection = connect_or_spawn(&dir)?;
        match handshake(connection, server, stream) {
            Err(Failure::Unavailable(reason)) if attempt < 2 => {
                tracing::debug!(reason, "Daemon connection failed, trying again");
            }
            result => return result,
        }
    }
}

/// Connect to the daemon's socket in `dir`; if nobody listens, start the daemon
/// and wait (bounded) until it does.
fn connect_or_spawn(dir: &Path) -> Result<UnixStream, Failure> {
    let socket = ipc::socket_path(dir);
    // A configuration error, not "no daemon": falling back to an embedded server
    // would hide it, and the next process would then fail to bind the port.
    ipc::check_socket_path(&socket).map_err(Failure::Engine)?;
    let nobody_listens = |e: &io::Error| {
        matches!(
            e.kind(),
            io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
        )
    };
    match UnixStream::connect(&socket) {
        Ok(connection) => return Ok(connection),
        Err(e) if nobody_listens(&e) => {}
        Err(e) => {
            return Err(Failure::Unavailable(format!(
                "cannot connect to {}: {e}",
                socket.display()
            )));
        }
    }
    spawn_daemon(dir)?;
    let deadline = Instant::now() + SPAWN_WAIT;
    loop {
        match UnixStream::connect(&socket) {
            Ok(connection) => return Ok(connection),
            Err(e) if nobody_listens(&e) && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => {
                return Err(Failure::Unavailable(format!(
                    "started {DAEMON_BINARY}, but it does not listen on {} ({e}); see {}",
                    socket.display(),
                    dir.join("daemon.log").display()
                )));
            }
        }
    }
}

/// Start the daemon, detached from the sender's process group.
fn spawn_daemon(dir: &Path) -> Result<(), Failure> {
    let binary = std::env::var_os("GST_SNAPCAST_DAEMON").unwrap_or_else(|| DAEMON_BINARY.into());
    // The daemon creates its directory too; the log file needs it first.
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|e| Failure::Unavailable(format!("cannot create {}: {e}", dir.display())))?;
    let mut child = std::process::Command::new(&binary)
        .arg("--log")
        .arg(dir.join("daemon.log"))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|e| {
            Failure::Unavailable(format!(
                "cannot start {} ({e}); install it or set GST_SNAPCAST_DAEMON",
                Path::new(&binary).display()
            ))
        })?;
    tracing::info!(pid = child.id(), "Started the shared server daemon");
    // Reap it when it exits, in case this process outlives it.
    let _ = std::thread::Builder::new()
        .name("gst-snapcast-daemon-wait".into())
        .spawn(move || child.wait());
    Ok(())
}

/// `Hello` and `Acquire` on a fresh connection.
fn handshake(
    connection: UnixStream,
    server: &ServerOptions,
    stream: &StreamOptions,
) -> Result<(UnixStream, Acquired), Failure> {
    let broken = |e: io::Error| Failure::Unavailable(format!("daemon connection: {e}"));
    connection
        .set_read_timeout(Some(READ_TIMEOUT))
        .map_err(broken)?;
    connection
        .set_write_timeout(Some(WRITE_TIMEOUT))
        .map_err(broken)?;
    let mut reader = BufReader::new(connection.try_clone().map_err(broken)?);
    let mut writer = connection;

    ipc::write_message(
        &mut writer,
        &Message::Hello(Hello {
            version: PROTOCOL_VERSION,
            pid: std::process::id(),
            log_id: stream.log_id.clone(),
        }),
    )
    .map_err(broken)?;
    match ipc::read_message(&mut reader).map_err(broken)? {
        Some(Message::Welcome(welcome)) => {
            tracing::debug!(daemon_version = %welcome.daemon_version, "Connected to the daemon");
        }
        Some(Message::Error(e)) => return Err(Failure::Unavailable(e.message)),
        None => {
            return Err(Failure::Unavailable(
                "the daemon closed the connection".into(),
            ));
        }
        Some(other) => {
            return Err(Failure::Unavailable(format!(
                "unexpected answer to Hello: {other:?}"
            )));
        }
    }

    ipc::write_message(
        &mut writer,
        &Message::Acquire(Acquire {
            server: ipc::ServerSettings::from_options(server),
            stream: ipc::StreamSettings::from_options(stream),
        }),
    )
    .map_err(broken)?;
    match ipc::read_message(&mut reader).map_err(broken)? {
        Some(Message::Acquired(acquired)) => Ok((writer, acquired)),
        Some(Message::Error(e)) => Err(Failure::Engine(engine_error(e, stream))),
        None => Err(Failure::Unavailable(
            "the daemon closed the connection".into(),
        )),
        Some(other) => Err(Failure::Unavailable(format!(
            "unexpected answer to Acquire: {other:?}"
        ))),
    }
}

/// The engine error for an error answer of the daemon.
fn engine_error(e: IpcError, stream: &StreamOptions) -> EngineError {
    match e.kind {
        ErrorKind::StreamTaken => EngineError::StreamTaken(stream.name.clone()),
        ErrorKind::StreamNotAdded => EngineError::StreamNotAdded {
            stream: stream.name.clone(),
            reason: e.message,
        },
        ErrorKind::ServerGone => EngineError::ServerGone,
        ErrorKind::Busy => EngineError::Busy,
        ErrorKind::InvalidConfig => EngineError::InvalidConfig(e.message),
        ErrorKind::Bind | ErrorKind::Version | ErrorKind::Protocol => {
            EngineError::Daemon(e.message)
        }
    }
}

impl SharedStream {
    /// Publish the stream's properties through the daemon (blocks at most
    /// [`WRITE_TIMEOUT`]).
    pub(crate) fn set_properties(&self, properties: &StreamProperties) -> Result<(), EngineError> {
        let properties = ipc::Properties::from(properties);
        let mut writer = self.writer.lock().unwrap();
        // Remembered also when sending fails: the reconnect sends it.
        *self.properties.lock().unwrap() = Some(properties.clone());
        ipc::write_message(&mut *writer, &Message::Properties(properties))
            .map_err(|e| EngineError::Daemon(format!("sending stream properties: {e}")))
    }

    /// Call `handler` for each command a controller sends to the stream (replaces
    /// a previous one). It runs on a reader thread of this stream.
    pub(crate) fn on_command(&self, handler: Arc<dyn Fn(StreamCommand) + Send + Sync>) {
        *self.handler.lock().unwrap() = Some(handler);
    }

    /// Send PCM to the daemon. Blocks while the daemon does not read (at most
    /// [`WRITE_TIMEOUT`]). Reconnects once if the connection broke.
    pub(crate) fn send_pcm(&self, data: Vec<u8>, timestamp_usec: i64) -> Result<(), EngineError> {
        let message = Message::Pcm {
            timestamp_usec,
            data,
        };
        let mut writer = self.writer.lock().unwrap();
        let error = match ipc::write_message(&mut *writer, &message) {
            Ok(()) => return Ok(()),
            Err(e) => e,
        };
        if matches!(
            error.kind(),
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
        ) {
            return Err(EngineError::Daemon(format!(
                "the daemon took longer than {WRITE_TIMEOUT:?} to read"
            )));
        }
        tracing::warn!(%error, "Lost the shared server daemon, reconnecting");
        let (server, stream) = &self.requested;
        match connect_and_acquire(server, stream) {
            Ok((new_writer, acquired)) => {
                if acquired.port != self.port {
                    tracing::warn!(
                        old = self.port,
                        new = acquired.port,
                        "The server's port changed with the new daemon"
                    );
                }
                spawn_reader(&new_writer, Arc::clone(&self.handler)).map_err(|e| {
                    EngineError::Daemon(format!("reconnected, but cannot read: {e}"))
                })?;
                *writer = new_writer;
                tracing::info!("Reconnected to the shared server daemon");
                if let Some(properties) = self.properties.lock().unwrap().clone() {
                    ipc::write_message(&mut *writer, &Message::Properties(properties))
                        .map_err(|_| EngineError::ServerGone)?;
                }
                ipc::write_message(&mut *writer, &message).map_err(|_| EngineError::ServerGone)
            }
            Err(Failure::Engine(e)) => Err(e),
            Err(Failure::Unavailable(reason)) => Err(EngineError::Daemon(format!(
                "connection lost ({error}), reconnecting failed: {reason}"
            ))),
        }
    }
}

impl Drop for SharedStream {
    fn drop(&mut self) {
        // Closing the connection releases the stream in the daemon.
        if let Ok(writer) = self.writer.get_mut() {
            let _ = writer.shutdown(std::net::Shutdown::Both);
        }
    }
}
