//! The protocol between senders and the shared server daemon (`gst-snapcast-daemon`).
//!
//! Design and reasons: `docs/shared-server.md`. A sender connects to the daemon's
//! Unix socket ([`socket_path`]), says [`Hello`], acquires one stream
//! ([`Acquire`]), then sends [`Message::Pcm`] frames and, when they change, the
//! stream's [`Properties`]. The daemon sends the [`Command`]s that controllers
//! address to the stream. Closing the connection releases the stream.
//!
//! # Framing
//!
//! Every message is one frame: a type byte, the payload length as `u32` little
//! endian, then the payload (at most [`MAX_FRAME_LEN`] bytes). Control messages
//! carry JSON; [`Message::Pcm`] carries the server timestamp as `i64` little endian
//! followed by the interleaved PCM bytes. The protocol is private to this
//! repository and versioned ([`PROTOCOL_VERSION`]): sender and daemon must speak
//! the same version.
//!
//! # Time
//!
//! PCM timestamps are [`crate::now_usec`] values, `CLOCK_MONOTONIC`, which is the
//! same clock in every process of the host. The daemon passes them on unchanged.

use crate::EngineError;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Version of this protocol; [`Hello`] and [`Welcome`] carry it.
pub const PROTOCOL_VERSION: u32 = 2;

/// Largest payload of a frame. A PCM frame of 100 ms at 192 kHz, 32 bit, 8 channels
/// is about 600 kB.
pub const MAX_FRAME_LEN: usize = 4 << 20;

/// Name of the directory below `$XDG_RUNTIME_DIR` with the daemon's socket and lock.
pub const DIR_NAME: &str = "gst-snapcast";

/// The daemon's directory, `$XDG_RUNTIME_DIR/gst-snapcast`. `None` without
/// `XDG_RUNTIME_DIR` (then there is no shared server).
pub fn runtime_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty())?;
    Some(Path::new(&base).join(DIR_NAME))
}

/// The daemon's socket in `dir` (see [`runtime_dir`]).
pub fn socket_path(dir: &Path) -> PathBuf {
    dir.join("daemon.sock")
}

/// The longest Unix socket path, in bytes: `sun_path` holds 108 bytes on Linux
/// (104 on the BSDs and macOS), including the terminating NUL.
#[cfg(target_os = "linux")]
pub const MAX_SOCKET_PATH: usize = 107;
/// The longest Unix socket path, in bytes (see the Linux variant).
#[cfg(not(target_os = "linux"))]
pub const MAX_SOCKET_PATH: usize = 103;

/// Fails with [`EngineError::InvalidConfig`] if `socket` is too long for a Unix
/// socket. That happens when `XDG_RUNTIME_DIR` is a long path (it is
/// `/run/user/<uid>` normally); the daemon could never be reached there.
pub fn check_socket_path(socket: &Path) -> Result<(), EngineError> {
    let len = socket.as_os_str().len();
    if len > MAX_SOCKET_PATH {
        return Err(EngineError::InvalidConfig(format!(
            "the shared server's socket path {} is {len} bytes, but Unix sockets allow \
             at most {MAX_SOCKET_PATH}: set a shorter XDG_RUNTIME_DIR",
            socket.display()
        )));
    }
    Ok(())
}

/// The daemon's lock file in `dir`: whoever holds it is the one daemon.
pub fn lock_path(dir: &Path) -> PathBuf {
    dir.join("daemon.lock")
}

/// First message of a sender.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    /// The sender's [`PROTOCOL_VERSION`].
    pub version: u32,
    /// The sender's process id (for the daemon's log).
    pub pid: u32,
    /// Who is sending (e.g. the element name), for the daemon's log.
    pub log_id: String,
}

/// The daemon's answer to [`Hello`] (or an [`IpcError`] of kind
/// [`ErrorKind::Version`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Welcome {
    /// The daemon's [`PROTOCOL_VERSION`].
    pub version: u32,
    /// The daemon's package version.
    pub daemon_version: String,
}

/// Server-wide options of an acquisition (see `server::ServerOptions`). Only the
/// first acquirer of a port decides them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerSettings {
    /// Stream port; `0`: an ephemeral port, never shared.
    pub port: u16,
    /// End-to-end buffer in milliseconds.
    pub buffer_ms: u32,
    /// mDNS instance name, `None`: not announced.
    pub mdns_name: Option<String>,
    /// JSON-RPC over TCP: `None` off, `Some(0)` ephemeral.
    pub control_tcp_port: Option<u16>,
    /// JSON-RPC over HTTP/WebSocket and Snapweb: `None` off, `Some(0)` ephemeral.
    pub control_http_port: Option<u16>,
    /// Snapweb's files.
    pub snapweb_dir: Option<PathBuf>,
}

/// The stream of an acquisition (see `server::StreamOptions`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamSettings {
    /// Stream name.
    pub name: String,
    /// Codec: `pcm` or `flac`.
    pub codec: String,
    /// Sample rate in Hz.
    pub rate: u32,
    /// Bits per sample.
    pub bits: u16,
    /// Channels.
    pub channels: u16,
    /// Id for log lines of this acquisition.
    pub log_id: String,
}

/// Acquire one stream; at most once per connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Acquire {
    /// Server-wide options.
    pub server: ServerSettings,
    /// The stream.
    pub stream: StreamSettings,
}

/// The daemon acquired the stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Acquired {
    /// The port the server listens on.
    pub port: u16,
    /// Bound control port over TCP, if any.
    pub control_tcp_port: Option<u16>,
    /// Bound control port over HTTP, if any.
    pub control_http_port: Option<u16>,
    /// The server-wide options in effect (those of the server's first acquirer).
    pub server: ServerSettings,
}

/// What went wrong, so that the sender can map it to its own errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErrorKind {
    /// Another sender feeds this stream.
    StreamTaken,
    /// The server refused to add the stream.
    StreamNotAdded,
    /// A port could not be bound.
    Bind,
    /// The server terminated.
    ServerGone,
    /// The server's command queue is full.
    Busy,
    /// Invalid options.
    InvalidConfig,
    /// Sender and daemon speak different protocol versions.
    Version,
    /// A message that is not allowed here, or a malformed one.
    Protocol,
}

/// An error answer of the daemon. After it, the daemon closes the connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IpcError {
    /// Kind of the error.
    pub kind: ErrorKind,
    /// Human-readable description.
    pub message: String,
}

impl std::fmt::Display for IpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for IpcError {}

/// What controllers see of the stream (see `server::StreamProperties`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Properties {
    /// Track title.
    pub title: Option<String>,
    /// Artists.
    pub artist: Vec<String>,
    /// Album.
    pub album: Option<String>,
    /// Track length in seconds.
    pub duration_secs: Option<f64>,
    /// Accepts `play`.
    pub can_play: bool,
    /// Accepts `pause` (and `playPause`).
    pub can_pause: bool,
    /// Accepts `next`.
    pub can_go_next: bool,
    /// Accepts `previous`.
    pub can_go_previous: bool,
    /// Accepts `seek` / `setPosition`.
    pub can_seek: bool,
    /// `playing`, `paused`, `stopped`, or `None`: not reported.
    pub playback: Option<String>,
    /// Playback position in seconds. Optional on the wire (absent from older
    /// peers).
    #[serde(default)]
    pub position_secs: Option<f64>,
}

/// A player command from a controller for the stream (JSON-RPC `Stream.Control`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Command {
    /// The command, as the controller sent it (`play`, `next`, `seek`, …).
    pub command: String,
    /// Its parameters as JSON text.
    pub params_json: String,
}

/// A message of the protocol.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    /// Sender → daemon, first.
    Hello(Hello),
    /// Daemon → sender, answer to `Hello`.
    Welcome(Welcome),
    /// Sender → daemon.
    Acquire(Acquire),
    /// Daemon → sender, answer to `Acquire`.
    Acquired(Acquired),
    /// Daemon → sender; the daemon closes the connection afterwards.
    Error(IpcError),
    /// Sender → daemon, after `Acquired`: the stream's properties (replace the
    /// previous ones).
    Properties(Properties),
    /// Daemon → sender, after `Acquired`: a controller's command for the stream.
    Command(Command),
    /// Sender → daemon: interleaved little-endian PCM in the acquired format.
    Pcm {
        /// Server time ([`crate::now_usec`]) of the first frame in `data`.
        timestamp_usec: i64,
        /// The samples.
        data: Vec<u8>,
    },
}

const HELLO: u8 = 1;
const WELCOME: u8 = 2;
const ACQUIRE: u8 = 3;
const ACQUIRED: u8 = 4;
const ERROR: u8 = 5;
const PCM: u8 = 6;
const PROPERTIES: u8 = 7;
const COMMAND: u8 = 8;

fn json<T: serde::de::DeserializeOwned>(payload: &[u8]) -> io::Result<T> {
    serde_json::from_slice(payload).map_err(io::Error::from)
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

/// Write `message` as one frame (a single `write_all`).
pub fn write_message(w: &mut impl Write, message: &Message) -> io::Result<()> {
    let (kind, payload) = match message {
        Message::Hello(m) => (HELLO, serde_json::to_vec(m)?),
        Message::Welcome(m) => (WELCOME, serde_json::to_vec(m)?),
        Message::Acquire(m) => (ACQUIRE, serde_json::to_vec(m)?),
        Message::Acquired(m) => (ACQUIRED, serde_json::to_vec(m)?),
        Message::Error(m) => (ERROR, serde_json::to_vec(m)?),
        Message::Properties(m) => (PROPERTIES, serde_json::to_vec(m)?),
        Message::Command(m) => (COMMAND, serde_json::to_vec(m)?),
        Message::Pcm {
            timestamp_usec,
            data,
        } => {
            let mut payload = Vec::with_capacity(8 + data.len());
            payload.extend_from_slice(&timestamp_usec.to_le_bytes());
            payload.extend_from_slice(data);
            (PCM, payload)
        }
    };
    if payload.len() > MAX_FRAME_LEN {
        return Err(invalid(format!(
            "frame of {} bytes exceeds {MAX_FRAME_LEN}",
            payload.len()
        )));
    }
    let len = u32::try_from(payload.len()).map_err(|_| invalid("frame too long"))?;
    let mut frame = Vec::with_capacity(5 + payload.len());
    frame.push(kind);
    frame.extend_from_slice(&len.to_le_bytes());
    frame.extend_from_slice(&payload);
    w.write_all(&frame)?;
    w.flush()
}

/// Read one frame. `Ok(None)` when the peer closed the connection between frames.
pub fn read_message(r: &mut impl Read) -> io::Result<Option<Message>> {
    let mut header = [0u8; 5];
    match r.read_exact(&mut header[..1]) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    r.read_exact(&mut header[1..])?;
    let len = u32::from_le_bytes([header[1], header[2], header[3], header[4]]) as usize;
    if len > MAX_FRAME_LEN {
        return Err(invalid(format!(
            "frame of {len} bytes exceeds {MAX_FRAME_LEN}"
        )));
    }
    let mut payload = vec![0u8; len];
    r.read_exact(&mut payload)?;
    Ok(Some(match header[0] {
        HELLO => Message::Hello(json(&payload)?),
        WELCOME => Message::Welcome(json(&payload)?),
        ACQUIRE => Message::Acquire(json(&payload)?),
        ACQUIRED => Message::Acquired(json(&payload)?),
        ERROR => Message::Error(json(&payload)?),
        PROPERTIES => Message::Properties(json(&payload)?),
        COMMAND => Message::Command(json(&payload)?),
        PCM => {
            let Some((timestamp, data)) = payload.split_first_chunk::<8>() else {
                return Err(invalid("PCM frame without timestamp"));
            };
            Message::Pcm {
                timestamp_usec: i64::from_le_bytes(*timestamp),
                data: data.to_vec(),
            }
        }
        other => return Err(invalid(format!("unknown frame type {other}"))),
    }))
}

#[cfg(feature = "server")]
mod convert {
    //! Between the wire types and the engine's server types.

    use super::{Command, ErrorKind, IpcError, Properties, ServerSettings, StreamSettings};
    use crate::server::{
        ControlOptions, Mode, PlaybackStatus, ServerOptions, StreamCommand, StreamControls,
        StreamMetadata, StreamOptions, StreamProperties,
    };
    use crate::{EngineError, PcmFormat};

    impl ServerSettings {
        /// The wire form of `options` (the mode stays on the sender's side).
        pub fn from_options(options: &ServerOptions) -> Self {
            Self {
                port: options.port,
                buffer_ms: options.buffer_ms,
                mdns_name: options.mdns_name.clone(),
                control_tcp_port: options.control.tcp_port,
                control_http_port: options.control.http_port,
                snapweb_dir: options.control.snapweb_dir.clone(),
            }
        }

        /// The engine options, with [`Mode::Embedded`]: in the daemon, the
        /// server runs in the daemon's process.
        pub fn to_options(&self) -> ServerOptions {
            ServerOptions {
                port: self.port,
                buffer_ms: self.buffer_ms,
                mdns_name: self.mdns_name.clone(),
                control: ControlOptions {
                    tcp_port: self.control_tcp_port,
                    http_port: self.control_http_port,
                    snapweb_dir: self.snapweb_dir.clone(),
                },
                mode: Mode::Embedded,
            }
        }
    }

    impl StreamSettings {
        /// The wire form of `options`.
        pub fn from_options(options: &StreamOptions) -> Self {
            Self {
                name: options.name.clone(),
                codec: options.codec.clone(),
                rate: options.format.rate,
                bits: options.format.bits,
                channels: options.format.channels,
                log_id: options.log_id.clone(),
            }
        }

        /// The engine options.
        pub fn to_options(&self) -> StreamOptions {
            StreamOptions {
                name: self.name.clone(),
                codec: self.codec.clone(),
                format: PcmFormat {
                    rate: self.rate,
                    bits: self.bits,
                    channels: self.channels,
                },
                log_id: self.log_id.clone(),
            }
        }
    }

    impl From<&StreamProperties> for Properties {
        fn from(p: &StreamProperties) -> Self {
            Self {
                title: p.metadata.title.clone(),
                artist: p.metadata.artist.clone(),
                album: p.metadata.album.clone(),
                duration_secs: p.metadata.duration_secs,
                can_play: p.controls.play,
                can_pause: p.controls.pause,
                can_go_next: p.controls.next,
                can_go_previous: p.controls.previous,
                can_seek: p.controls.seek,
                playback: p.playback.map(|s| s.as_str().to_owned()),
                position_secs: p.position_secs,
            }
        }
    }

    impl From<&Properties> for StreamProperties {
        /// An unknown `playback` value counts as not reported.
        fn from(p: &Properties) -> Self {
            Self {
                metadata: StreamMetadata {
                    title: p.title.clone(),
                    artist: p.artist.clone(),
                    album: p.album.clone(),
                    duration_secs: p.duration_secs,
                },
                controls: StreamControls {
                    play: p.can_play,
                    pause: p.can_pause,
                    next: p.can_go_next,
                    previous: p.can_go_previous,
                    seek: p.can_seek,
                },
                playback: p.playback.as_deref().and_then(PlaybackStatus::from_str),
                position_secs: p.position_secs,
            }
        }
    }

    impl From<&StreamCommand> for Command {
        fn from(c: &StreamCommand) -> Self {
            Self {
                command: c.command.clone(),
                params_json: c.params_json.clone(),
            }
        }
    }

    impl From<Command> for StreamCommand {
        fn from(c: Command) -> Self {
            Self {
                command: c.command,
                params_json: c.params_json,
            }
        }
    }

    impl From<&EngineError> for IpcError {
        fn from(e: &EngineError) -> Self {
            let kind = match e {
                EngineError::StreamTaken(_) => ErrorKind::StreamTaken,
                EngineError::StreamNotAdded { .. } => ErrorKind::StreamNotAdded,
                EngineError::Bind(..) => ErrorKind::Bind,
                EngineError::ServerGone => ErrorKind::ServerGone,
                EngineError::Busy => ErrorKind::Busy,
                EngineError::Daemon(_) => ErrorKind::Protocol,
                _ => ErrorKind::InvalidConfig,
            };
            Self {
                kind,
                message: e.to_string(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_paths_beyond_the_unix_limit_are_a_configuration_error() {
        let ok = socket_path(Path::new("/run/user/1000/gst-snapcast"));
        assert!(check_socket_path(&ok).is_ok());
        let long = Path::new("/").join("x".repeat(MAX_SOCKET_PATH));
        let err = check_socket_path(&long).unwrap_err();
        assert!(matches!(err, EngineError::InvalidConfig(_)), "{err}");
        assert!(err.to_string().contains("XDG_RUNTIME_DIR"), "{err}");
        let exact = Path::new("/").join("x".repeat(MAX_SOCKET_PATH - 1));
        assert!(
            check_socket_path(&exact).is_ok(),
            "{MAX_SOCKET_PATH} bytes fit"
        );
    }

    fn round_trip(message: Message) {
        let mut buf = Vec::new();
        write_message(&mut buf, &message).unwrap();
        let mut r = buf.as_slice();
        assert_eq!(read_message(&mut r).unwrap(), Some(message));
        assert_eq!(
            read_message(&mut r).unwrap(),
            None,
            "clean end after a frame"
        );
    }

    fn settings() -> (ServerSettings, StreamSettings) {
        (
            ServerSettings {
                port: 1704,
                buffer_ms: 1000,
                mdns_name: Some("Living room".into()),
                control_tcp_port: Some(1705),
                control_http_port: None,
                snapweb_dir: Some("/usr/share/snapweb".into()),
            },
            StreamSettings {
                name: "music".into(),
                codec: "flac".into(),
                rate: 48000,
                bits: 16,
                channels: 2,
                log_id: "snapsink0".into(),
            },
        )
    }

    #[test]
    fn every_message_survives_a_round_trip() {
        let (server, stream) = settings();
        round_trip(Message::Hello(Hello {
            version: PROTOCOL_VERSION,
            pid: 42,
            log_id: "snapsink0".into(),
        }));
        round_trip(Message::Welcome(Welcome {
            version: PROTOCOL_VERSION,
            daemon_version: "0.1.0".into(),
        }));
        round_trip(Message::Acquire(Acquire {
            server: server.clone(),
            stream,
        }));
        round_trip(Message::Acquired(Acquired {
            port: 1704,
            control_tcp_port: Some(1705),
            control_http_port: None,
            server,
        }));
        round_trip(Message::Error(IpcError {
            kind: ErrorKind::StreamTaken,
            message: "stream 'music' is already in use".into(),
        }));
        round_trip(Message::Properties(Properties {
            title: Some("Song".into()),
            artist: vec!["Band".into()],
            album: None,
            duration_secs: Some(201.5),
            can_play: true,
            can_go_next: true,
            playback: Some("playing".into()),
            ..Properties::default()
        }));
        round_trip(Message::Command(Command {
            command: "seek".into(),
            params_json: r#"{"offset":30}"#.into(),
        }));
        round_trip(Message::Pcm {
            timestamp_usec: -5,
            data: vec![1, 2, 3, 4],
        });
        round_trip(Message::Pcm {
            timestamp_usec: i64::MAX,
            data: Vec::new(),
        });
    }

    #[test]
    fn a_pcm_frame_is_type_length_timestamp_samples() {
        let mut buf = Vec::new();
        write_message(
            &mut buf,
            &Message::Pcm {
                timestamp_usec: 0x0102,
                data: vec![0xaa, 0xbb],
            },
        )
        .unwrap();
        assert_eq!(
            buf,
            [6, 10, 0, 0, 0, 0x02, 0x01, 0, 0, 0, 0, 0, 0, 0xaa, 0xbb]
        );
    }

    #[test]
    fn bad_frames_are_errors() {
        // Unknown type.
        assert!(read_message(&mut [99u8, 0, 0, 0, 0].as_slice()).is_err());
        // Too long.
        let len = (MAX_FRAME_LEN as u32 + 1).to_le_bytes();
        assert!(read_message(&mut [PCM, len[0], len[1], len[2], len[3]].as_slice()).is_err());
        // PCM without a timestamp.
        assert!(read_message(&mut [PCM, 2, 0, 0, 0, 1, 2].as_slice()).is_err());
        // Truncated inside a frame (not a clean end).
        assert!(read_message(&mut [HELLO, 10, 0, 0, 0, b'{'].as_slice()).is_err());
        // Malformed JSON.
        assert!(read_message(&mut [HELLO, 2, 0, 0, 0, b'{', b'x'].as_slice()).is_err());
    }

    #[test]
    fn paths_live_in_the_users_runtime_dir() {
        let dir = Path::new("/run/user/1000/gst-snapcast");
        assert_eq!(
            socket_path(dir),
            Path::new("/run/user/1000/gst-snapcast/daemon.sock")
        );
        assert_eq!(
            lock_path(dir),
            Path::new("/run/user/1000/gst-snapcast/daemon.lock")
        );
    }
}
