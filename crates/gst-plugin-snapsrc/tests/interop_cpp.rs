//! Interop: reference C++ `snapserver` → `snapsrc`.
//!
//! Ignored by default because they need an external binary. Run them with
//! `make test-interop`, or with
//! `cargo test -p gst-plugin-snapsrc --test interop_cpp -- --ignored`.
//! Set `SNAPSERVER=/path/to/snapserver` to use a binary that is not on `PATH`.
//!
//! The C++ server reads raw S16LE 48 kHz stereo from a named pipe (fed by a live
//! `audiotestsrc ! filesink` pipeline) and serves it with the codec under test. Its
//! control ports are disabled, and its state goes to a temporary directory.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use gst::prelude::*;

const BUFFER_MS: u32 = 500;
const FREQ: f32 = 440.0;
/// Buffers to analyse after the first ones (start-up) are skipped.
const BUFFERS_TO_CHECK: usize = 60;

fn init() {
    use std::sync::Once;
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        gst::init().unwrap();
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

/// A running C++ snapserver with its temporary directory; killed and removed on drop.
struct CppServer {
    child: Child,
    dir: PathBuf,
}

impl CppServer {
    fn start(codec: &str, port: u16) -> Self {
        let bin = std::env::var("SNAPSERVER").unwrap_or_else(|_| "snapserver".into());
        let dir =
            std::env::temp_dir().join(format!("snapsrc-interop-{}-{codec}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let fifo = dir.join("fifo");
        let child = Command::new(&bin)
            .arg("--config")
            .arg(dir.join("none.conf")) // does not exist: built-in defaults
            .arg(format!("--server.datadir={}", dir.display()))
            .args(["--tcp.enabled=0", "--http.enabled=0", "--logging.sink=null"])
            .arg(format!("--stream.port={port}"))
            .arg("--stream.bind_to_address=127.0.0.1")
            .arg(format!("--stream.codec={codec}"))
            .arg(format!("--stream.buffer={BUFFER_MS}"))
            .arg(format!(
                "--stream.source=pipe://{}?name=default&sampleformat=48000:16:2",
                fifo.display()
            ))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap_or_else(|e| panic!("cannot start '{bin}': {e}"));
        let server = Self { child, dir };
        wait_until("snapserver to create its pipe and listen", || {
            fifo.exists() && std::net::TcpStream::connect(("127.0.0.1", port)).is_ok()
        });
        server
    }

    fn fifo(&self) -> PathBuf {
        self.dir.join("fifo")
    }
}

impl Drop for CppServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn feed(fifo: &Path) -> gst::Pipeline {
    let feeder = gst::parse::launch(&format!(
        "audiotestsrc is-live=true freq={FREQ} \
         ! audio/x-raw,format=S16LE,rate=48000,channels=2 \
         ! filesink location={} sync=false",
        fifo.display()
    ))
    .unwrap()
    .downcast::<gst::Pipeline>()
    .unwrap();
    feeder.set_state(gst::State::Playing).unwrap();
    feeder
}

fn receive_from_cpp_server(codec: &str) {
    init();
    let port = free_port();
    let server = CppServer::start(codec, port);
    let feeder = feed(&server.fifo());

    let receiver = gst::parse::launch(&format!(
        "snapsrc port={port} client-name=interop-{codec} ! appsink name=out sync=false"
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

    let mut left = Vec::new();
    let mut prev_end: Option<gst::ClockTime> = None;
    let mut ahead_ms = Vec::new();
    for i in 0..(10 + BUFFERS_TO_CHECK) {
        let sample = appsink
            .try_pull_sample(gst::ClockTime::from_seconds(10))
            .expect("no audio received from snapsrc");
        let info = gst_audio::AudioInfo::from_caps(sample.caps().unwrap()).unwrap();
        assert_eq!(
            (info.format(), info.rate(), info.channels()),
            (gst_audio::AudioFormat::F32le, 48000, 2)
        );
        let buffer = sample.buffer().unwrap();
        let pts = buffer.pts().expect("buffer without PTS");
        if i < 10 {
            prev_end = Some(pts + buffer.duration().unwrap());
            continue;
        }
        if let Some(prev_end) = prev_end {
            let gap_us = pts.useconds() as i64 - prev_end.useconds() as i64;
            assert!(gap_us.abs() < 2_000, "PTS discontinuity of {gap_us} µs");
        }
        prev_end = Some(pts + buffer.duration().unwrap());

        let clock = receiver.clock().unwrap();
        let playout = receiver.base_time().unwrap() + pts;
        ahead_ms.push((playout.nseconds() as i64 - clock.time().nseconds() as i64) / 1_000_000);

        let map = buffer.map_readable().unwrap();
        left.extend(
            map.as_chunks::<8>()
                .0
                .iter()
                .map(|f| f32::from_le_bytes([f[0], f[1], f[2], f[3]])),
        );
    }

    receiver.set_state(gst::State::Null).unwrap();
    feeder.set_state(gst::State::Null).unwrap();
    drop(server);

    let rms = (left.iter().map(|s| s * s).sum::<f32>() / left.len() as f32).sqrt();
    assert!((rms - 0.566).abs() < 0.02, "unexpected RMS {rms}");
    let crossings = left
        .windows(2)
        .filter(|w| w[0] < 0.0 && w[1] >= 0.0)
        .count();
    let freq = crossings as f32 * 48000.0 / left.len() as f32;
    assert!((freq - FREQ).abs() < 3.0, "frequency {freq:.1} Hz");

    // The C++ server stamps chunks when it reads them from the pipe, so buffers arrive
    // somewhat less than BUFFER_MS ahead of play-out (pipe and chunk delays).
    let (min, max) = (
        *ahead_ms.iter().min().unwrap(),
        *ahead_ms.iter().max().unwrap(),
    );
    let buffer_ms = i64::from(BUFFER_MS);
    assert!(
        min > buffer_ms - 250 && max <= buffer_ms + 5,
        "buffers arrived {min}..{max} ms ahead, expected up to {buffer_ms} ms"
    );
}

#[test]
#[ignore = "needs the C++ snapserver (make test-interop)"]
fn snapsrc_plays_cpp_snapserver_pcm() {
    receive_from_cpp_server("pcm");
}

#[test]
#[ignore = "needs the C++ snapserver (make test-interop)"]
fn snapsrc_plays_cpp_snapserver_flac() {
    receive_from_cpp_server("flac");
}
