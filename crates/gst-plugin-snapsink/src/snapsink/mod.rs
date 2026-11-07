use gst::glib;
use gst::prelude::*;

mod imp;

glib::wrapper! {
    /// Sink element submitting raw PCM to a stream of the embedded Snapcast server.
    pub struct SnapSink(ObjectSubclass<imp::SnapSink>)
        @extends gst_base::BaseSink, gst::Element, gst::Object;
}

pub(crate) fn register(plugin: &gst::Plugin) -> Result<(), glib::BoolError> {
    gst::Element::register(
        Some(plugin),
        "snapsink",
        gst::Rank::NONE,
        SnapSink::static_type(),
    )
}
