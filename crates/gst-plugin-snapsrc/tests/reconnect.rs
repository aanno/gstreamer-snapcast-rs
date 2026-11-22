//! L8/F5: `snapsrc` keeps playing when the server goes away and comes back.

use std::time::{Duration, Instant};

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

fn start_sender(port: u16) -> gst::Pipeline {
    let p = gst::parse::launch(&format!(
        "audiotestsrc is-live=true ! audio/x-raw,format=S16LE,rate=48000,channels=2 \
         ! snapsink port={port} mode=embedded buffer-ms=300 mdns-name=\"\""
    ))
    .unwrap()
    .downcast::<gst::Pipeline>()
    .unwrap();
    p.set_state(gst::State::Playing).unwrap();
    p
}

/// Pulls buffers for `duration`; returns how far ahead of the clock (ms) each one
/// arrived. Fails on a pipeline error.
fn pull_for(appsink: &gst_app::AppSink, receiver: &gst::Pipeline, duration: Duration) -> Vec<i64> {
    let deadline = Instant::now() + duration;
    let mut ahead = Vec::new();
    while Instant::now() < deadline {
        if let Some(msg) = receiver
            .bus()
            .unwrap()
            .pop_filtered(&[gst::MessageType::Error])
        {
            panic!("receiver failed: {msg:?}");
        }
        if let Some(sample) = appsink.try_pull_sample(gst::ClockTime::from_mseconds(100)) {
            let buffer = sample.buffer().expect("sample without buffer");
            // Silence that snapsrc makes while no server sends (F5 gap filling) is
            // scheduled just ahead of render, not by the server: only audio counts.
            if buffer.flags().contains(gst::BufferFlags::GAP) {
                continue;
            }
            let pts = buffer.pts().expect("buffer without PTS");
            let playout = receiver.base_time().unwrap() + pts;
            let now = receiver.clock().unwrap().time();
            ahead.push((playout.nseconds() as i64 - now.nseconds() as i64) / 1_000_000);
        }
    }
    ahead
}

fn median(mut v: Vec<i64>) -> i64 {
    v.sort_unstable();
    v[v.len() / 2]
}

#[test]
fn snapsrc_resumes_after_a_server_restart() {
    init();
    let port = free_port();
    let sender = start_sender(port);

    let receiver = gst::parse::launch(&format!(
        "snapsrc port={port} ! appsink name=out sync=false"
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
    let before = pull_for(&appsink, &receiver, Duration::from_secs(2));
    assert!(before.len() > 10, "no audio before the restart");

    // The last snapsink on a port stops its server.
    sender.set_state(gst::State::Null).unwrap();
    drop(sender);
    std::thread::sleep(Duration::from_millis(500));
    // Drain what was still queued.
    pull_for(&appsink, &receiver, Duration::from_millis(1500));

    let sender = start_sender(port);
    let after = pull_for(&appsink, &receiver, Duration::from_secs(8));
    sender.set_state(gst::State::Null).unwrap();
    receiver.set_state(gst::State::Null).unwrap();
    assert!(
        after.len() > 10,
        "only {} buffers within 8 s after the server restart",
        after.len()
    );
    // Scheduled as before: buffers arrive about buffer-ms (300) ahead of play-out,
    // i.e. the time sync and the play-out mapping were redone for the new connection.
    let (b, a) = (median(before), median(after));
    assert!(
        (150..=305).contains(&a) && (a - b).abs() < 50,
        "buffers arrive {a} ms ahead after the restart (before: {b} ms)"
    );
}
