//! GStreamer-agnostic glue between the GStreamer elements and `snapcast-rs`.
//!
//! This crate must **not** depend on GStreamer. It owns everything that has a
//! lifecycle independent of a single GStreamer element:
//!
//! * the shared tokio runtime that drives the `snapcast-rs` engines,
//! * (feature `server`) one process-wide embedded `SnapServer` shared by all
//!   `snapsink` elements, each element feeding one named stream,
//! * (feature `client`) embedded `SnapClient` instances used by `snapsrc`.
//!
//! The GStreamer plugin crates only translate between GStreamer buffers/caps
//! and the APIs exposed here. Keeping this boundary makes the engine reusable
//! outside GStreamer (e.g. an Android sender).

#![warn(missing_docs)]

use std::sync::OnceLock;

#[cfg(feature = "server")]
pub use snapcast_server as server;

#[cfg(feature = "client")]
pub use snapcast_client as client;

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
}
