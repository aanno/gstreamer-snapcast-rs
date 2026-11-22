//! F5: lifecycle of `snapsink` and `snapsrc`: PLAYING → PAUSED → PLAYING on either side,
//! and a receiver that stops and starts again. A server restart (sender stop/start) is
//! in `reconnect.rs`; the audible side of a sender pause, with the C++ snapclient, is
//! `aloop_sender_pause` in `aloop.rs`.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gst::prelude::*;
use serde_json::{Value, json};

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

/// A sine (RMS 0.35) into snapsink on `port`; `extra` are more snapsink properties.
fn start_sender(port: u16, extra: &str) -> gst::Pipeline {
    let p = gst::parse::launch(&format!(
        "audiotestsrc is-live=true volume=0.5 ! audio/x-raw,format=S16LE,rate=48000,channels=2 \
         ! snapsink port={port} buffer-ms=300 mdns-name=\"\" {extra}"
    ))
    .unwrap()
    .downcast::<gst::Pipeline>()
    .unwrap();
    p.set_state(gst::State::Playing).unwrap();
    p
}

/// `snapsrc ! fakesink`, with a probe that records every buffer snapsrc pushes (a sink
/// would hold back the one that arrives while it is PAUSED, as its preroll buffer).
struct Receiver {
    pipeline: gst::Pipeline,
    seen: Arc<Mutex<Vec<Seen>>>,
}

fn start_receiver(port: u16, start: Instant) -> Receiver {
    let pipeline = gst::parse::launch(&format!(
        "snapsrc name=src port={port} ! fakesink sync=false"
    ))
    .unwrap()
    .downcast::<gst::Pipeline>()
    .unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let store = Arc::clone(&seen);
    let weak = pipeline.downgrade();
    pipeline
        .by_name("src")
        .unwrap()
        .static_pad("src")
        .unwrap()
        .add_probe(gst::PadProbeType::BUFFER, move |_, info| {
            let (Some(buffer), Some(pipeline)) = (info.buffer(), weak.upgrade()) else {
                return gst::PadProbeReturn::Ok;
            };
            let pts = buffer.pts().expect("buffer without PTS");
            let playout = pipeline.base_time().unwrap() + pts;
            let now = pipeline.clock().unwrap().time();
            let map = buffer.map_readable().unwrap();
            store.lock().unwrap().push(Seen {
                at_ms: start.elapsed().as_millis() as u64,
                pts_ns: pts.nseconds(),
                duration_ns: buffer.duration().map_or(0, |d| d.nseconds()),
                ahead_ms: (playout.nseconds() as i64 - now.nseconds() as i64) / 1_000_000,
                rms: rms_f32(&map),
                discont: buffer.flags().contains(gst::BufferFlags::DISCONT),
                gap: buffer.flags().contains(gst::BufferFlags::GAP),
            });
            gst::PadProbeReturn::Ok
        });
    pipeline.set_state(gst::State::Playing).unwrap();
    Receiver { pipeline, seen }
}

/// One received buffer.
#[derive(Debug)]
struct Seen {
    /// Wall time since the test started (ms).
    at_ms: u64,
    pts_ns: u64,
    duration_ns: u64,
    /// How far ahead of its play-out time it arrived (ms), with a pipeline latency of 0.
    ahead_ms: i64,
    /// RMS of the samples, 0.0 = silence.
    rms: f64,
    discont: bool,
    gap: bool,
}

/// snapsrc outputs F32LE.
fn rms_f32(data: &[u8]) -> f64 {
    let samples = data.as_chunks::<4>().0;
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f64 = samples
        .iter()
        .map(|c| f64::from(f32::from_le_bytes(*c)).powi(2))
        .sum();
    (sum / samples.len() as f64).sqrt()
}

impl Receiver {
    /// Waits until `until`; fails on a pipeline error.
    fn run_until(&self, until: Instant) {
        while Instant::now() < until {
            if let Some(msg) = self.pipeline.bus().unwrap().timed_pop_filtered(
                gst::ClockTime::from_mseconds(20),
                &[gst::MessageType::Error],
            ) {
                panic!("receiver failed: {msg:?}");
            }
        }
    }

    /// Stops the receiver and returns what it pushed.
    fn stop(self) -> Vec<Seen> {
        self.pipeline.set_state(gst::State::Null).unwrap();
        std::mem::take(&mut *self.seen.lock().unwrap())
    }
}

/// The timeline as text, one line per buffer that is flagged or not contiguous, plus
/// one per second: for assertion messages.
fn timeline(seen: &[Seen]) -> String {
    let mut out = String::new();
    let mut prev_end: Option<u64> = None;
    let mut last_second = u64::MAX;
    for s in seen {
        let jump_ms = prev_end.map_or(0.0, |e| (s.pts_ns as f64 - e as f64) / 1e6);
        prev_end = Some(s.pts_ns + s.duration_ns);
        if s.at_ms / 1000 != last_second || s.discont || s.gap || jump_ms.abs() > 1.0 {
            last_second = s.at_ms / 1000;
            out += &format!(
                "{:>6} ms  pts {:>6} ms  dur {:>6.1}  ahead {:>4} ms  rms {:.3}{}{}{}\n",
                s.at_ms,
                s.pts_ns / 1_000_000,
                s.duration_ns as f64 / 1e6,
                s.ahead_ms,
                s.rms,
                if s.discont { "  DISCONT" } else { "" },
                if s.gap { "  GAP" } else { "" },
                if jump_ms.abs() > 1.0 {
                    format!("  pts jump {jump_ms:+.1} ms")
                } else {
                    String::new()
                },
            );
        }
    }
    out
}

/// After the first buffer: no DISCONT, and each PTS continues where the previous
/// buffer ended (within 1 ms; the aligner may insert or drop single frames).
fn assert_contiguous(seen: &[Seen]) {
    for w in seen.windows(2) {
        let end = w[0].pts_ns + w[0].duration_ns;
        let jump_ns = w[1].pts_ns as i64 - end as i64;
        assert!(
            !w[1].discont && jump_ns.abs() < 1_000_000,
            "not contiguous at {} ms (pts jump {:+.1} ms):\n{}",
            w[1].at_ms,
            jump_ns as f64 / 1e6,
            timeline(seen)
        );
    }
}

/// Total duration (ms) of the GAP buffers in `seen`, and whether they are all silent.
fn gap_ms(seen: &[Seen]) -> (u64, bool) {
    let mut silent = true;
    let mut total = 0;
    for s in seen.iter().filter(|s| s.gap) {
        silent &= s.rms == 0.0;
        total += s.duration_ns / 1_000_000;
    }
    (total, silent)
}

/// Median of how far ahead of play-out the audio (not the silence) arrived after
/// `after_ms`, and how many such buffers there were.
fn audio_ahead_after(seen: &[Seen], after_ms: u64) -> (i64, usize) {
    let mut ahead: Vec<i64> = seen
        .iter()
        .filter(|s| s.at_ms > after_ms && !s.gap && s.rms > 0.3)
        .map(|s| s.ahead_ms)
        .collect();
    ahead.sort_unstable();
    (
        ahead.get(ahead.len() / 2).copied().unwrap_or(0),
        ahead.len(),
    )
}

/// A sender pause reaches the receiver as silence (GAP buffers) on a continuous
/// timeline, and the audio resumes on schedule.
#[test]
fn snapsrc_fills_a_sender_pause_with_silence() {
    init();
    let port = free_port();
    let sender = start_sender(port, "");
    let start = Instant::now();
    let at = |ms: u64| start + Duration::from_millis(ms);
    let receiver = start_receiver(port, start);

    receiver.run_until(at(2500));
    sender.set_state(gst::State::Paused).unwrap();
    receiver.run_until(at(5500));
    sender.set_state(gst::State::Playing).unwrap();
    receiver.run_until(at(8500));
    let seen = receiver.stop();
    sender.set_state(gst::State::Null).unwrap();

    let (gap, silent) = gap_ms(&seen);
    assert!(silent, "GAP buffers must be silent:\n{}", timeline(&seen));
    // 3 s of pause; the audio already sent before it still plays.
    assert!(
        (2400..=3400).contains(&gap),
        "{gap} ms of silence for a 3 s pause:\n{}",
        timeline(&seen)
    );
    assert_contiguous(&seen);
    // Audio again after the pause, scheduled `buffer-ms` ahead as before.
    let (ahead, n) = audio_ahead_after(&seen, 6000);
    assert!(n > 50, "no audio after the pause:\n{}", timeline(&seen));
    assert!(
        (250..=305).contains(&ahead),
        "audio arrives {ahead} ms ahead after the pause (expected ~300):\n{}",
        timeline(&seen)
    );
}

/// A receiver that pauses drops what arrived meanwhile and continues on the server
/// timeline. Running time stops while it is paused, but the server's time goes on, so
/// the audio after the pause maps onto running time that the audio sent ahead before the
/// pause already covers: that overlap is dropped, the output stays contiguous.
#[test]
fn snapsrc_resumes_on_schedule_after_its_own_pause() {
    init();
    let port = free_port();
    let sender = start_sender(port, "");
    let start = Instant::now();
    let at = |ms: u64| start + Duration::from_millis(ms);
    let receiver = start_receiver(port, start);

    receiver.run_until(at(2000));
    receiver.pipeline.set_state(gst::State::Paused).unwrap();
    std::thread::sleep(Duration::from_secs(2));
    receiver.pipeline.set_state(gst::State::Playing).unwrap();
    receiver.run_until(at(7000));
    let seen = receiver.stop();
    sender.set_state(gst::State::Null).unwrap();

    let (ahead, n) = audio_ahead_after(&seen, 5000);
    assert!(n > 50, "no audio after resuming:\n{}", timeline(&seen));
    assert!(
        (250..=305).contains(&ahead),
        "audio arrives {ahead} ms ahead after resuming (expected ~300):\n{}",
        timeline(&seen)
    );
    assert_contiguous(&seen);
}

/// `Server.GetStatus` over the control port: the clients as (id, connected), or `None`
/// while the port is not open yet (snapsink starts its server with the first caps).
fn clients(control_port: u16) -> Option<Vec<(String, bool)>> {
    let stream = TcpStream::connect(("127.0.0.1", control_port)).ok()?;
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut writer = stream.try_clone().unwrap();
    let request = json!({"jsonrpc": "2.0", "id": 1, "method": "Server.GetStatus"});
    writeln!(writer, "{request}").unwrap();
    let mut reader = BufReader::new(stream);
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).expect("reply");
        let msg: Value = serde_json::from_str(&line).unwrap();
        if msg["id"] == 1 {
            return Some(
                msg["result"]["server"]["groups"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .flat_map(|g| g["clients"].as_array().unwrap())
                    .map(|c| {
                        (
                            c["id"].as_str().unwrap().to_owned(),
                            c["connected"].as_bool().unwrap(),
                        )
                    })
                    .collect(),
            );
        }
    }
}

fn wait_for_clients(
    control_port: u16,
    want: impl Fn(&[(String, bool)]) -> bool,
) -> Vec<(String, bool)> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let c = clients(control_port).unwrap_or_default();
        if !c.is_empty() && want(&c) {
            return c;
        }
        assert!(Instant::now() < deadline, "clients stay at {c:?}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// A receiver that stops and starts again is the same client to the server (same id:
/// its instance number was released), so its settings in a controller stay.
#[test]
fn a_restarted_snapsrc_is_the_same_client() {
    init();
    let (port, control_port) = (free_port(), free_port());
    let sender = start_sender(port, &format!("control-port={control_port}"));

    let receiver = start_receiver(port, Instant::now());
    let first = wait_for_clients(control_port, |c| c.iter().any(|(_, on)| *on));
    receiver.stop();
    wait_for_clients(control_port, |c| c.iter().all(|(_, on)| !*on));

    let receiver = start_receiver(port, Instant::now());
    let second = wait_for_clients(control_port, |c| c.iter().any(|(_, on)| *on));
    receiver.stop();
    sender.set_state(gst::State::Null).unwrap();

    assert_eq!(first.len(), 1, "{first:?}");
    assert_eq!(
        second.len(),
        1,
        "a second client entry after the restart: {second:?}"
    );
    assert_eq!(first[0].0, second[0].0);
}
