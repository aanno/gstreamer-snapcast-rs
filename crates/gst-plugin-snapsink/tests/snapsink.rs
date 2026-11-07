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
    assert_eq!(sink.property::<String>("codec"), "pcm");
    assert_eq!(sink.property::<u32>("buffer-ms"), 1000);
    assert_eq!(sink.property::<u32>("bound-port"), 0);
}

#[test]
fn second_stream_on_running_server_fails() {
    init();
    let port = free_port();
    let launch = |stream: &str| {
        gst::parse::launch(&format!(
            "audiotestsrc is-live=true ! audio/x-raw,format=S16LE,rate=48000,channels=2 \
             ! snapsink port={port} stream={stream}"
        ))
        .unwrap()
    };
    let first = launch("a");
    first.set_state(gst::State::Playing).unwrap();
    let second = launch("b");
    second.set_state(gst::State::Playing).unwrap();

    let msg = second
        .bus()
        .unwrap()
        .timed_pop_filtered(gst::ClockTime::from_seconds(5), &[gst::MessageType::Error])
        .expect("expected an error for the second stream");
    let gst::MessageView::Error(err) = msg.view() else {
        unreachable!()
    };
    assert!(
        err.debug()
            .is_some_and(|d| d.contains("without stream 'b'")),
        "{err:?}"
    );

    second.set_state(gst::State::Null).unwrap();
    first.set_state(gst::State::Null).unwrap();
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[test]
fn accepts_pcm_until_eos() {
    init();
    let pipeline = gst::parse::launch(
        "audiotestsrc num-buffers=10 ! audio/x-raw,format=S16LE,rate=48000,channels=2 \
         ! snapsink port=0",
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
