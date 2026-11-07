use gst::glib;
use gst::prelude::*;

mod imp;

glib::wrapper! {
    /// Live source element receiving PCM from a Snapcast server.
    pub struct SnapSrc(ObjectSubclass<imp::SnapSrc>)
        @extends gst_base::PushSrc, gst_base::BaseSrc, gst::Element, gst::Object;
}

pub(crate) fn register(plugin: &gst::Plugin) -> Result<(), glib::BoolError> {
    gst::Element::register(
        Some(plugin),
        "snapsrc",
        gst::Rank::NONE,
        SnapSrc::static_type(),
    )
}
