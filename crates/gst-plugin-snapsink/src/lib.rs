//! GStreamer plugin `snapsink`: sender side of Snapcast.
//!
//! Provides the `snapsink` element, a [`gst_base::BaseSink`] that accepts raw
//! interleaved PCM and submits it to a named stream of an embedded Snapcast
//! server (see `snapcast-engine`). All `snapsink` instances in a process share
//! one server; each element feeds exactly one stream.
//!
//! ```text
//! gst-launch-1.0 pipewiresrc ! audioconvert ! audioresample ! snapsink stream=desktop
//! ```

use gst::glib;

mod snapsink;

pub use snapsink::SnapSink;

fn plugin_init(plugin: &gst::Plugin) -> Result<(), glib::BoolError> {
    snapsink::register(plugin)
}

gst::plugin_define!(
    snapsink,
    env!("CARGO_PKG_DESCRIPTION"),
    plugin_init,
    concat!(env!("CARGO_PKG_VERSION"), "-", env!("COMMIT_ID")),
    // GStreamer only knows a fixed set of license names; the crate itself is
    // GPL-3.0-only (see Cargo.toml / LICENSE).
    "GPL",
    env!("CARGO_PKG_NAME"),
    env!("CARGO_PKG_NAME"),
    env!("CARGO_PKG_REPOSITORY"),
    env!("BUILD_REL_DATE")
);
