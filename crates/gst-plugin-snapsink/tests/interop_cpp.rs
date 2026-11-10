//! Interop: `snapsink` → reference C++ `snapclient`.
//!
//! Ignored by default because they need an external binary. Run them with
//! `make test-interop`, or with
//! `cargo test -p gst-plugin-snapsink --test interop_cpp -- --ignored`.
//! Set `SNAPCLIENT=/path/to/snapclient` to use a binary that is not on `PATH`
//! (e.g. a self-compiled upstream build).
//!
//! The C++ client decodes the stream with its `file` player and writes raw PCM
//! (the stream's sample format, here S16LE 48 kHz stereo) to stdout. The test checks
//! that the decoded signal is the 440 Hz sine that was sent, at the expected level.

use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use gst::prelude::*;

const RATE: usize = 48_000;
const FRAME_BYTES: usize = 4; // S16LE, 2 channels
const FREQ: f64 = 440.0;
/// RMS of a sine with amplitude 0.8 (audiotestsrc's default volume).
const EXPECTED_RMS: f64 = 0.8 / std::f64::consts::SQRT_2;

fn init() {
    use std::sync::Once;
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        gst::init().unwrap();
        gstsnapsink::plugin_register_static().expect("register snapsink plugin");
    });
}

fn snapclient_bin() -> String {
    std::env::var("SNAPCLIENT").unwrap_or_else(|_| "snapclient".into())
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

/// Sends a 440 Hz sine with `codec` and returns about 2 s of what the C++ client decoded,
/// as left-channel samples in [-1, 1].
fn decode_with_cpp_client(codec: &str) -> Vec<f64> {
    init();
    let sender = gst::parse::launch(&format!(
        "audiotestsrc is-live=true freq={FREQ} \
         ! audio/x-raw,format=S16LE,rate={RATE},channels=2 \
         ! snapsink name=sink port=0 codec={codec} buffer-ms=500"
    ))
    .unwrap()
    .downcast::<gst::Pipeline>()
    .unwrap();
    sender.set_state(gst::State::Playing).unwrap();
    let port = wait_for_port(&sender.by_name("sink").unwrap());

    let mut child = Command::new(snapclient_bin())
        .args(["-h", "127.0.0.1", "-p", &port.to_string()])
        .args([
            "--hostID",
            &format!("gst-interop-{}-{codec}", std::process::id()),
        ])
        .args(["--player", "file:filename=stdout", "--logsink", "null"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap_or_else(|e| panic!("cannot start '{}': {e}", snapclient_bin()));

    // Skip the first second (connect, time sync, buffer fill), keep the next two.
    let skip = RATE * FRAME_BYTES;
    let keep = 2 * RATE * FRAME_BYTES;
    let mut stdout = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = vec![0u8; skip + keep];
        let result = stdout.read_exact(&mut buf).map(|()| buf);
        let _ = tx.send(result);
    });
    let result = rx.recv_timeout(Duration::from_secs(15));

    let _ = child.kill();
    let _ = child.wait();
    sender.set_state(gst::State::Null).unwrap();

    let bytes = result
        .expect("snapclient produced too little audio within 15 s")
        .expect("reading snapclient output");
    bytes[skip..]
        .as_chunks::<FRAME_BYTES>()
        .0
        .iter()
        .map(|f| f64::from(i16::from_le_bytes([f[0], f[1]])) / 32768.0)
        .collect()
}

fn assert_is_sine(samples: &[f64]) {
    let rms = (samples.iter().map(|s| s * s).sum::<f64>() / samples.len() as f64).sqrt();
    // Rising zero crossings per second ≈ frequency.
    let crossings = samples
        .windows(2)
        .filter(|w| w[0] < 0.0 && w[1] >= 0.0)
        .count();
    let freq = crossings as f64 * RATE as f64 / samples.len() as f64;
    assert!(
        (rms - EXPECTED_RMS).abs() < 0.02,
        "rms {rms:.3}, expected {EXPECTED_RMS:.3}"
    );
    assert!((freq - FREQ).abs() < 3.0, "frequency {freq:.1} Hz");
}

#[test]
#[ignore = "needs the C++ snapclient (make test-interop)"]
fn cpp_snapclient_decodes_pcm() {
    assert_is_sine(&decode_with_cpp_client("pcm"));
}

#[test]
#[ignore = "needs the C++ snapclient (make test-interop)"]
fn cpp_snapclient_decodes_flac() {
    assert_is_sine(&decode_with_cpp_client("flac"));
}
