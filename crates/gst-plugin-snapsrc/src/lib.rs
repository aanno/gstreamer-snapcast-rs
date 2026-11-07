//! GStreamer plugin `snapsrc`: receiver side of Snapcast.
//!
//! Provides the `snapsrc` element, a live [`gst_base::PushSrc`] that connects
//! to a Snapcast server via an embedded Snapcast client (see
//! `snapcast-engine`) and pushes decoded, correctly timestamped raw PCM
//! downstream.
//!
//! ```text
//! gst-launch-1.0 snapsrc host=192.168.1.10 ! audioconvert ! audioresample ! pipewiresink
//! ```

use gst::glib;

mod snapsrc;

pub use snapsrc::SnapSrc;

fn plugin_init(plugin: &gst::Plugin) -> Result<(), glib::BoolError> {
    snapsrc::register(plugin)
}

gst::plugin_define!(
    snapsrc,
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
