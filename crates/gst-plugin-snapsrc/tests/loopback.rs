//! End-to-end loopback: `snapsink` → embedded Snapcast server → `snapsrc`.

use std::time::{Duration, Instant};

use gst::prelude::*;

const BUFFER_MS: u64 = 300;
const BUFFERS_TO_CHECK: usize = 60;

fn init() {
    use std::sync::Once;
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        gst::init().unwrap();
        gstsnapsink::plugin_register_static().expect("register snapsink plugin");
        gstsnapsrc::plugin_register_static().expect("register snapsrc plugin");
    });
}

fn wait_for_port(sink: &gst::Element) -> u32 {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let port = sink.property::<u32>("bound-port");
        if port != 0 {
            return port;
        }
        assert!(Instant::now() < deadline, "snapsink did not start a server");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn pcm_loopback_is_continuous_and_scheduled_by_buffer_ms() {
    init();

    // Sender: live 440 Hz sine, default amplitude 0.8.
    let sender = gst::parse::launch(&format!(
        "audiotestsrc is-live=true freq=440 \
         ! audio/x-raw,format=S16LE,rate=48000,channels=2 \
         ! snapsink name=sink port=0 buffer-ms={BUFFER_MS}"
    ))
    .unwrap()
    .downcast::<gst::Pipeline>()
    .unwrap();
    sender.set_state(gst::State::Playing).unwrap();
    let port = wait_for_port(&sender.by_name("sink").unwrap());

    // Receiver: appsink without sync, so we can inspect how far ahead of the
    // clock each buffer arrives.
    let receiver = gst::parse::launch(&format!(
        "snapsrc name=src port={port} ! appsink name=out sync=false"
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

    let mut samples = Vec::new();
    let mut prev_end: Option<gst::ClockTime> = None;
    let mut ahead_ms = Vec::new();
    while ahead_ms.len() < BUFFERS_TO_CHECK {
        let sample = appsink
            .try_pull_sample(gst::ClockTime::from_seconds(5))
            .expect("no audio received from snapsrc");

        let caps = sample.caps().unwrap();
        let info = gst_audio::AudioInfo::from_caps(caps).unwrap();
        assert_eq!(info.format(), gst_audio::AudioFormat::F32le);
        assert_eq!(info.rate(), 48000);
        assert_eq!(info.channels(), 2);

        let buffer = sample.buffer().unwrap();
        let pts = buffer.pts().expect("buffer without PTS");
        let duration = buffer.duration().expect("buffer without duration");

        // Continuity: each buffer starts where the previous one ended. Time
        // sync updates may shift things slightly; allow 2 ms.
        if let Some(prev_end) = prev_end {
            let gap_us = pts.useconds() as i64 - prev_end.useconds() as i64;
            assert!(gap_us.abs() < 2_000, "PTS discontinuity of {gap_us} µs");
        }
        prev_end = Some(pts + duration);

        // Scheduling: the buffer must be played ~BUFFER_MS after it was
        // captured, and it arrives just after capture, so it should be
        // nearly BUFFER_MS ahead of the clock.
        let clock = receiver.clock().unwrap();
        let playout = receiver.base_time().unwrap() + pts;
        ahead_ms.push((playout.nseconds() as i64 - clock.time().nseconds() as i64) / 1_000_000);

        let map = buffer.map_readable().unwrap();
        samples.extend(
            map.as_chunks::<4>()
                .0
                .iter()
                .map(|b| f32::from_le_bytes(*b)),
        );
    }

    receiver.set_state(gst::State::Null).unwrap();
    sender.set_state(gst::State::Null).unwrap();

    // Signal: sine with amplitude 0.8 has RMS 0.8/sqrt(2) ≈ 0.566.
    let rms = (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt();
    assert!((rms - 0.566).abs() < 0.02, "unexpected RMS {rms}");

    // Skip the first buffers: they may have been queued while starting up.
    let steady = &ahead_ms[10..];
    let (min, max) = (*steady.iter().min().unwrap(), *steady.iter().max().unwrap());
    let buffer_ms = BUFFER_MS as i64;
    assert!(
        min > buffer_ms - 150 && max <= buffer_ms + 5,
        "buffers arrived {min}..{max} ms ahead, expected about {buffer_ms} ms"
    );
}
