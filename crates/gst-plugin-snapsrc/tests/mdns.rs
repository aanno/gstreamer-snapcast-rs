//! L8: `snapsrc` without a host finds a `snapsink` via mDNS.
//!
//! Ignored by default because it needs multicast on the local network (it works in
//! the devcontainer and on the host). Run it with `make test-mdns`.

use std::time::Duration;

use gst::prelude::*;

fn init() {
    use std::sync::Once;
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        gst::init().unwrap();
        gstsnapsink::plugin_register_static().expect("register snapsink plugin");
        gstsnapsrc::plugin_register_static().expect("register snapsrc plugin");
    });
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[test]
#[ignore = "needs multicast (make test-mdns)"]
fn snapsrc_finds_snapsink_via_mdns() {
    init();
    // A unique name, so that other Snapcast servers on the network don't interfere.
    let name = format!("gst-snapcast-test-{}", std::process::id());
    let port = free_port();
    let sender = gst::parse::launch(&format!(
        "audiotestsrc is-live=true ! audio/x-raw,format=S16LE,rate=48000,channels=2 \
         ! snapsink port={port} mdns-name={name} buffer-ms=300"
    ))
    .unwrap()
    .downcast::<gst::Pipeline>()
    .unwrap();
    sender.set_state(gst::State::Playing).unwrap();

    // No host, and a wrong port: both must come from the announcement.
    let receiver = gst::parse::launch(&format!(
        "snapsrc host=\"\" port=1 mdns-name={name} ! appsink name=out sync=false"
    ))
    .unwrap()
    .downcast::<gst::Pipeline>()
    .unwrap();
    let appsink = receiver
        .by_name("out")
        .unwrap()
        .downcast::<gst_app::AppSink>()
        .unwrap();
    receiver.set_state(gst::State::Playing).unwrap();

    let mut received = 0;
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while received < 20 && std::time::Instant::now() < deadline {
        if let Some(msg) = receiver
            .bus()
            .unwrap()
            .pop_filtered(&[gst::MessageType::Error])
        {
            panic!("receiver failed: {msg:?}");
        }
        if appsink
            .try_pull_sample(gst::ClockTime::from_mseconds(200))
            .is_some()
        {
            received += 1;
        }
    }
    receiver.set_state(gst::State::Null).unwrap();
    sender.set_state(gst::State::Null).unwrap();
    assert!(received >= 20, "only {received} buffers via mDNS discovery");
}

#[test]
#[ignore = "needs multicast (make test-mdns)"]
fn snapsrc_reports_when_no_server_is_found() {
    init();
    let receiver =
        gst::parse::launch("snapsrc host=\"\" mdns-name=no-such-snapcast-server ! fakesink")
            .unwrap();
    let start = std::time::Instant::now();
    // Discovery gives up after DISCOVERY_TIMEOUT (5 s); start() then fails.
    let result = receiver.set_state(gst::State::Playing);
    let msg = receiver
        .bus()
        .unwrap()
        .timed_pop_filtered(gst::ClockTime::from_seconds(10), &[gst::MessageType::Error]);
    receiver.set_state(gst::State::Null).unwrap();
    let elapsed = start.elapsed();

    let msg = msg.expect("no error message although no server exists");
    let text = format!("{msg:?}");
    assert!(
        result.is_err() || text.contains("mDNS"),
        "unexpected: {result:?}, {text}"
    );
    assert!(
        text.contains("no-such-snapcast-server"),
        "error does not name the server: {text}"
    );
    assert!(elapsed < Duration::from_secs(9), "took {elapsed:?}");
}
