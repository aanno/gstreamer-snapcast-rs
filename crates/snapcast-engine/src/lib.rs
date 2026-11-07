//! GStreamer-agnostic glue between the GStreamer elements and `snapcast-rs`.
//!
//! This crate must **not** depend on GStreamer. It owns everything that has a
//! lifecycle independent of a single GStreamer element:
//!
//! * the shared tokio runtime that drives the `snapcast-rs` engines,
//! * (feature `server`) embedded servers shared by all senders in a process,
//!   see [`server::acquire_stream`],
//! * (feature `client`) embedded clients producing frames with a local
//!   play-out time, see [`client::connect`].
//!
//! # Time base
//!
//! All timestamps are microseconds of the monotonic clock returned by
//! [`now_usec`] (`CLOCK_MONOTONIC` on Linux), the clock Snapcast uses for
//! time sync. Callers translate between this and their own clock domain.

#![warn(missing_docs)]

use std::fmt;
use std::sync::OnceLock;

#[cfg(feature = "client")]
pub mod client;
#[cfg(feature = "server")]
pub mod server;

/// Current time of the Snapcast clock domain in microseconds.
pub fn now_usec() -> i64 {
    snapcast_proto::time::now_usec()
}

/// Returns the process-wide tokio runtime used to drive Snapcast engines.
///
/// GStreamer streaming threads are not tokio threads, so all async work is
/// spawned onto this runtime. It is created lazily on first use and lives for
/// the remainder of the process (plugins are never unloaded by GStreamer).
pub fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .thread_name("snapcast-engine")
            .enable_all()
            .build()
            .expect("failed to create snapcast-engine tokio runtime")
    })
}

/// Interleaved little-endian integer PCM format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PcmFormat {
    /// Sample rate in Hz.
    pub rate: u32,
    /// Bits per sample (16 or 32; 24 means S24 in a 32 bit container).
    pub bits: u16,
    /// Number of channels.
    pub channels: u16,
}

impl fmt::Display for PcmFormat {
    /// Snapcast notation `rate:bits:channels`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}:{}", self.rate, self.bits, self.channels)
    }
}

/// Errors of the engine API.
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    /// Binding the server port failed.
    #[error("cannot listen on port {0}: {1}")]
    Bind(u16, #[source] std::io::Error),
    /// The stream is already fed by another sender.
    #[error("stream '{0}' is already in use")]
    StreamTaken(String),
    /// The running server was created without this stream.
    #[error(
        "server on port {port} is already running without stream '{stream}' \
         (snapcast-rs cannot add streams at runtime yet; use another port)"
    )]
    StreamsFixed {
        /// Port of the running server.
        port: u16,
        /// Requested stream.
        stream: String,
    },
    /// The stream exists with a different codec or format.
    #[error("stream '{stream}' runs as {existing}, requested {requested}")]
    StreamMismatch {
        /// Stream name.
        stream: String,
        /// Existing codec and format.
        existing: String,
        /// Requested codec and format.
        requested: String,
    },
    /// The server terminated.
    #[error("Snapcast server is gone")]
    ServerGone,
    /// Invalid configuration.
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_is_shared_and_usable() {
        let a = runtime() as *const _;
        let b = runtime() as *const _;
        assert_eq!(a, b);
        assert_eq!(runtime().block_on(async { 21 * 2 }), 42);
    }

    #[test]
    fn pcm_format_uses_snapcast_notation() {
        let f = PcmFormat {
            rate: 48000,
            bits: 16,
            channels: 2,
        };
        assert_eq!(f.to_string(), "48000:16:2");
    }
}
