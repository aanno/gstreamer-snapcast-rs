//! Integration tests for the `snapsink` element.

use gst::prelude::*;

fn init() {
    use std::sync::Once;
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        gst::init().unwrap();
        gstsnapsink::plugin_register_static().expect("register snapsink plugin");
    });
}

#[test]
fn element_has_default_properties() {
    init();
    let sink = gst::ElementFactory::make("snapsink").build().unwrap();
    assert_eq!(sink.property::<String>("stream"), "default");
    assert_eq!(sink.property::<u32>("port"), 1704);
}

#[test]
fn accepts_pcm_until_eos() {
    init();
    let pipeline = gst::parse::launch(
        "audiotestsrc num-buffers=10 ! audio/x-raw,format=S16LE,rate=48000,channels=2 \
         ! snapsink sync=false",
    )
    .unwrap();
    pipeline.set_state(gst::State::Playing).unwrap();

    let bus = pipeline.bus().unwrap();
    let msg = bus
        .timed_pop_filtered(
            gst::ClockTime::from_seconds(5),
            &[gst::MessageType::Eos, gst::MessageType::Error],
        )
        .expect("pipeline timed out");
    pipeline.set_state(gst::State::Null).unwrap();

    assert_eq!(msg.type_(), gst::MessageType::Eos, "got {msg:?}");
}
