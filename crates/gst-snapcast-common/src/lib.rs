//! Code shared by the `snapsink` and `snapsrc` GStreamer plugins.
//!
//! Unlike `snapcast-engine`, this crate may (and does) depend on GStreamer.
//! It holds what both plugins need on the GStreamer side, so far the bridge
//! from `tracing`/`log` to the GStreamer debug log ([`log`]).

#![warn(missing_docs)]

pub mod log;
