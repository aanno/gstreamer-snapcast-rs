//! The shared backend of [`crate::server::StreamHandle`]: a stream acquired from the
//! per-user server daemon (`gst-snapcast-daemon`) over its Unix socket.
//!
//! Design: `docs/shared-server.md`. The acquisition is one connection: `Hello`,
//! `Acquire`, then PCM frames; dropping the stream closes the connection, which
//! releases the stream in the daemon.

use std::io::{self, BufReader};
use std::os::unix::net::UnixStream;
use std::sync::Mutex;
use std::time::Duration;

use crate::EngineError;
use crate::ipc::{self, Acquire, ErrorKind, Hello, IpcError, Message, PROTOCOL_VERSION};
use crate::server::{Mode, ServerOptions, StreamOptions};

/// How long a read from the daemon (its answers) may take.
const READ_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a PCM write may block when the daemon does not read (backpressure
/// beyond this means it hangs).
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);

/// Why the shared backend could not be used.
#[derive(Debug)]
pub(crate) enum Failure {
    /// No usable daemon (none running, no runtime directory, another protocol
    /// version, a broken connection). `expected`: nothing is wrong, there is just
    /// no daemon (the socket does not exist or nobody listens).
    Unavailable {
        /// What happened.
        reason: String,
        /// No daemon running, as opposed to a daemon that misbehaves.
        expected: bool,
    },
    /// The daemon answered the acquisition with an error (e.g. the stream is taken).
    Engine(EngineError),
}

impl Failure {
    fn unavailable(reason: impl Into<String>) -> Self {
        Self::Unavailable {
            reason: reason.into(),
            expected: false,
        }
    }
}

/// A stream fed through the daemon.
pub(crate) struct SharedStream {
    /// Writes are whole frames under this lock.
    writer: Mutex<UnixStream>,
    pub(crate) port: u16,
    pub(crate) control_tcp_port: Option<u16>,
    pub(crate) control_http_port: Option<u16>,
    /// The server-wide options in effect in the daemon.
    pub(crate) server_options: ServerOptions,
}

/// Connect to the daemon and acquire `stream`.
pub(crate) fn acquire(
    server: &ServerOptions,
    stream: &StreamOptions,
) -> Result<SharedStream, Failure> {
    let Some(dir) = ipc::runtime_dir() else {
        return Err(Failure::unavailable("XDG_RUNTIME_DIR is not set"));
    };
    let socket = ipc::socket_path(&dir);
    let connection = match UnixStream::connect(&socket) {
        Ok(connection) => connection,
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
            ) =>
        {
            return Err(Failure::Unavailable {
                reason: format!("no daemon at {}", socket.display()),
                expected: true,
            });
        }
        Err(e) => {
            return Err(Failure::unavailable(format!(
                "cannot connect to {}: {e}",
                socket.display()
            )));
        }
    };
    let broken = |e: io::Error| Failure::unavailable(format!("daemon connection: {e}"));
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
        Some(Message::Error(e)) => return Err(Failure::unavailable(e.message)),
        other => {
            return Err(Failure::unavailable(format!(
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
    let acquired = match ipc::read_message(&mut reader).map_err(broken)? {
        Some(Message::Acquired(acquired)) => acquired,
        Some(Message::Error(e)) => return Err(Failure::Engine(engine_error(e, stream))),
        other => {
            return Err(Failure::unavailable(format!(
                "unexpected answer to Acquire: {other:?}"
            )));
        }
    };
    let mut server_options = acquired.server.to_options();
    server_options.mode = Mode::Shared;
    Ok(SharedStream {
        writer: Mutex::new(writer),
        port: acquired.port,
        control_tcp_port: acquired.control_tcp_port,
        control_http_port: acquired.control_http_port,
        server_options,
    })
}

/// The engine error for an error answer of the daemon.
fn engine_error(e: IpcError, stream: &StreamOptions) -> EngineError {
    match e.kind {
        ErrorKind::StreamTaken => EngineError::StreamTaken(stream.name.clone()),
        ErrorKind::StreamNotAdded => EngineError::StreamNotAdded {
            stream: stream.name.clone(),
            reason: e.message,
        },
        ErrorKind::Bind => EngineError::Daemon(e.message),
        ErrorKind::ServerGone => EngineError::ServerGone,
        ErrorKind::Busy => EngineError::Busy,
        ErrorKind::InvalidConfig => EngineError::InvalidConfig(e.message),
        ErrorKind::Version | ErrorKind::Protocol => EngineError::Daemon(e.message),
    }
}

impl SharedStream {
    /// Send PCM to the daemon. Blocks while the daemon does not read (at most
    /// [`WRITE_TIMEOUT`]).
    pub(crate) fn send_pcm(&self, data: Vec<u8>, timestamp_usec: i64) -> Result<(), EngineError> {
        let mut writer = self.writer.lock().unwrap();
        ipc::write_message(
            &mut *writer,
            &Message::Pcm {
                timestamp_usec,
                data,
            },
        )
        .map_err(|e| match e.kind() {
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => EngineError::Daemon(format!(
                "the daemon took longer than {WRITE_TIMEOUT:?} to read"
            )),
            _ => EngineError::ServerGone,
        })
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
