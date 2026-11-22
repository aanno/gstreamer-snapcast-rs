//! Timing tests on the snd-aloop loopback card (feasibility criterion F2).
//!
//! Ignored by default because they need the `snd-aloop` kernel module, `amixer`
//! (alsa-utils) and, for the C++ cases, `snapclient`. Run them with `make test-aloop`, or
//! `cargo test -p gst-plugin-snapsrc --test aloop -- --ignored --test-threads=1 --nocapture`.
//! They must run one at a time: they share loopback subdevice 0.
//!
//! # Setup
//!
//! ```text
//! audiotestsrc wave=ticks ! snapsink (embedded server, port=0)
//!     ├─ receiver A ─▶ ALSA "snaptest_left"  ─┐
//!     └─ receiver B ─▶ ALSA "snaptest_right" ─┤ dmix ─▶ hw:Loopback,0,0
//!                                             └─ capture: alsasrc hw:Loopback,1,0
//! ```
//!
//! Both receivers play into one dmix on loopback subdevice 0, A on the left channel and
//! B on the right. One capture stream records both, so their offset is the sample
//! distance between the tick onsets in L and R: about 20 µs resolution, and capture
//! timestamps (and their jitter) play no role.
//!
//! # Drift
//!
//! `PCM Rate Shift 100000` makes the loopback "sound card" run faster or slower than
//! the system clock. A receiver that follows the Snapcast timeline must still produce one
//! tick per second of *server* time, i.e. one tick per `card_rate` captured frames. We
//! measure `card_rate` (captured frames per second of `CLOCK_MONOTONIC`, the clock domain
//! of the Snapcast timeline) by a linear fit over the capture, and compare.
//!
//! Both receivers share the card, so this emulates "sound card drifts against the
//! server", not two cards drifting against each other.
//!
//! # Absolute reference
//!
//! Every test also measures each receiver against the **server timeline**: when a tick
//! is due (its server timestamp + `buffer-ms`) versus when it leaves the card.
//!
//! - Due: snapsink stamps a buffer with its render time, `base_time + running_time +
//!   latency` of the sender's clock, translated to `CLOCK_MONOTONIC` (the server's
//!   clock; on one machine `diff_to_server` is 0). The sender `tee`s a reference copy to
//!   an appsink, so the test knows the PTS of every tick. (Don't assume ticks on full
//!   seconds: `audiotestsrc` puts them about one buffer earlier.)
//! - Out: the capture maps frames to `CLOCK_MONOTONIC` with the fitted card rate and the
//!   earliest buffer arrival (arrival delays are only ever positive). snd-aloop hands
//!   samples to the capture end when the playback end consumes them, which is the moment
//!   a real DAC would play them, within a period of a few ms.
//!
//! Positive = the receiver plays late.
//!
//! # Settings (environment variables)
//!
//! - `ALOOP_CARD`: card name (default `Loopback`)
//! - `ALOOP_SECONDS`: measured seconds after warm-up (default 20)
//! - `ALOOP_TOLERANCE_US`: max allowed offset between receivers (default 1000, F2)
//! - `ALOOP_TOLERANCE_DRIFT_US`: the same under emulated card drift (default 2500: the
//!   C++ snapclient itself is about +2 ms off the server timeline there)
//! - `ALOOP_TOLERANCE_PPM`: max allowed rate error against the server (default 100)
//! - `ALOOP_TOLERANCE_ABS_US`: max allowed absolute offset against the server timeline
//!   (default 5000; the method itself is good to a few ms)
//! - `ALOOP_SINK_PROPS`: extra properties for the snapsrc receivers' `alsasink`,
//!   e.g. `slave-method=skew drift-tolerance=5000`
//! - `ALOOP_VERBOSE`: print each tick's offset against the server timeline
//! - `SNAPCLIENT`: C++ snapclient binary (default: `PATH`)

use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use gst::prelude::*;
use gst_base::prelude::*;
use gst_snapcast_engine::now_usec;

const RATE: u32 = 48_000;
const BUFFER_MS: u32 = 1000;
/// Captured audio before this is ignored (time sync, start-up of the receivers).
const WARMUP: Duration = Duration::from_secs(4);
/// Tick onset: first sample above this after at least `QUIET_FRAMES` below it.
/// Ticks are played at 0.8 full scale.
const THRESHOLD: i16 = 8_000;
const QUIET_FRAMES: usize = RATE as usize * 3 / 10;
/// Nominal value of the `PCM Rate Shift 100000` control.
const RATE_SHIFT_NOMINAL: i64 = 100_000;

fn env_or<T: std::str::FromStr>(name: &str, default: T) -> T {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn card() -> String {
    std::env::var("ALOOP_CARD").unwrap_or_else(|_| "Loopback".into())
}

/// ALSA PCMs for the test: a dmix on subdevice 0 and two `plug`s routing a stereo
/// player to only the left or only the right channel of it.
fn alsa_config(card: &str) -> String {
    format!(
        r#"
pcm.snaptest_mix {{
    type dmix
    ipc_key 47110000
    ipc_perm 0600
    slave {{ pcm "hw:{card},0,0" format S16_LE rate {RATE} channels 2 period_size 240 buffer_size 1920 }}
}}
pcm.snaptest_left {{
    type plug
    slave {{ pcm "snaptest_mix" channels 2 }}
    ttable {{ 0.0 0.5 1.0 0.5 }}
}}
pcm.snaptest_right {{
    type plug
    slave {{ pcm "snaptest_mix" channels 2 }}
    ttable {{ 0.1 0.5 1.1 0.5 }}
}}
"#
    )
}

/// Registers the plugins and adds the test PCMs to ALSA's configuration, once.
fn init() {
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(|| {
        let card = card();
        let cards = std::fs::read_to_string("/proc/asound/cards").unwrap_or_default();
        assert!(
            cards.contains(&format!("[{card}")),
            "no '{card}' sound card: load snd-aloop on the host ('sudo modprobe snd-aloop'), \
             and in the devcontainer pass --device=/dev/snd (see docs/testing-devcontainer.md)"
        );

        // ALSA_CONFIG_PATH replaces the list of top-level config files, so keep the
        // system's own (or the caller's) and append ours.
        let path = std::env::temp_dir().join(format!("snapcast-aloop-{}.conf", std::process::id()));
        std::fs::write(&path, alsa_config(&card)).unwrap();
        let base = std::env::var("ALSA_CONFIG_PATH")
            .unwrap_or_else(|_| "/usr/share/alsa/alsa.conf".into());
        // SAFETY: runs once, before GStreamer, ALSA or any child process exist; the test
        // harness runs these tests with --test-threads=1 (shared hardware), so no other
        // thread reads the environment concurrently.
        unsafe { std::env::set_var("ALSA_CONFIG_PATH", format!("{base}:{}", path.display())) };

        gst::init().unwrap();
        gstsnapsink::plugin_register_static().expect("register snapsink plugin");
        gstsnapsrc::plugin_register_static().expect("register snapsrc plugin");
    });
}

fn pipeline(desc: &str) -> gst::Pipeline {
    gst::parse::launch(desc)
        .unwrap_or_else(|e| panic!("cannot build '{desc}': {e}"))
        .downcast::<gst::Pipeline>()
        .unwrap()
}

/// Sets a pipeline to PLAYING, panicking with the bus error if that fails.
fn play(p: &gst::Pipeline) {
    if p.set_state(gst::State::Playing).is_err() {
        let msg = p
            .bus()
            .and_then(|bus| {
                bus.timed_pop_filtered(gst::ClockTime::from_seconds(1), &[gst::MessageType::Error])
            })
            .map(|m| format!("{m:?}"))
            .unwrap_or_default();
        panic!("pipeline failed to start: {msg}");
    }
}

/// Stops a pipeline when dropped, also when an assertion fails.
struct Running(gst::Pipeline);

impl Running {
    fn start(desc: &str) -> Self {
        let p = pipeline(desc);
        play(&p);
        Self(p)
    }

    /// Panics if the pipeline posted an error (e.g. the ALSA device failed).
    fn assert_no_error(&self, what: &str) {
        if let Some(msg) = self
            .0
            .bus()
            .and_then(|bus| bus.pop_filtered(&[gst::MessageType::Error]))
        {
            panic!("{what} failed: {msg:?}");
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.set_state(gst::State::Null);
    }
}

/// The sender, and what it sent (see [`Reference`]).
struct Sender {
    running: Running,
    port: u32,
    reference: Arc<Mutex<Reference>>,
}

/// What the sender sent: the tick onsets of its left channel, in frames since its
/// first buffer (kept as onsets only, so that hours of sending need little memory).
#[derive(Default)]
struct Reference {
    first_pts: Option<gst::ClockTime>,
    detector: OnsetDetector,
    onsets: Vec<f64>,
}

const TICKS: &str = "audiotestsrc is-live=true wave=ticks freq=1000 volume=0.8";

fn start_sender() -> Sender {
    start_sender_with(TICKS, "pcm")
}

/// Starts `source ! S16LE 48 kHz stereo ! snapsink`, with a `tee` that keeps a
/// reference copy of what was sent.
fn start_sender_with(source: &str, codec: &str) -> Sender {
    let p = pipeline(&format!(
        "{source} \
         ! audio/x-raw,format=S16LE,rate={RATE},channels=2 ! tee name=t \
         t. ! queue ! snapsink name=sink port=0 buffer-ms={BUFFER_MS} codec={codec} \
         t. ! queue ! appsink name=reference sync=false"
    ));
    let reference = Arc::new(Mutex::new(Reference::default()));
    let store = Arc::clone(&reference);
    p.by_name("reference")
        .unwrap()
        .downcast::<gst_app::AppSink>()
        .unwrap()
        .set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |appsink| {
                    let sample = appsink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;
                    let map = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;
                    let mut r = store.lock().unwrap();
                    if r.first_pts.is_none() {
                        r.first_pts = buffer.pts();
                    }
                    let found = r.detector.feed(
                        map.as_chunks::<4>()
                            .0
                            .iter()
                            .map(|f| i16::from_le_bytes([f[0], f[1]])),
                    );
                    r.onsets.extend(found);
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );
    play(&p);
    let sender = Running(p);
    let sink = sender.0.by_name("sink").unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let port = sink.property::<u32>("bound-port");
        if port != 0 {
            return Sender {
                running: sender,
                port,
                reference,
            };
        }
        assert!(Instant::now() < deadline, "snapsink did not start a server");
        std::thread::sleep(Duration::from_millis(20));
    }
}

impl Sender {
    /// When each tick sent so far is due at the output, in seconds of `CLOCK_MONOTONIC`:
    /// its server timestamp (snapsink's formula) plus `buffer-ms`.
    fn due_times(&self) -> Vec<f64> {
        let p = &self.running.0;
        let sink = p
            .by_name("sink")
            .unwrap()
            .downcast::<gst_base::BaseSink>()
            .unwrap();
        let (clock, base_time) = (p.clock().unwrap(), p.base_time().unwrap());
        // snapsink: server_ts = now_usec() + (render - clock.time()), i.e. render + offset.
        let offset_ns = now_usec() * 1000 - clock.time().nseconds() as i64;
        let latency_ns = sink.latency().nseconds() as i64;
        let r = self.reference.lock().unwrap();
        let first_pts = r.first_pts.expect("the sender sent nothing").nseconds() as i64;
        r.onsets
            .iter()
            .map(|&f| {
                let pts_ns = first_pts + (f * 1e9 / f64::from(RATE)) as i64;
                let server_ns = base_time.nseconds() as i64 + pts_ns + latency_ns + offset_ns;
                server_ns as f64 / 1e9 + f64::from(BUFFER_MS) / 1000.0
            })
            .collect()
    }
}

/// Which output of the dmix a receiver plays into.
#[derive(Clone, Copy)]
enum Side {
    Left,
    Right,
}

impl Side {
    fn pcm(self) -> &'static str {
        match self {
            Side::Left => "snaptest_left",
            Side::Right => "snaptest_right",
        }
    }
}

#[derive(Clone, Copy)]
enum Kind {
    Snapsrc,
    CppSnapclient,
}

enum Receiver {
    Snapsrc(Running),
    Cpp(Child),
}

impl Receiver {
    fn start(kind: Kind, side: Side, port: u32) -> Self {
        let (instance, name) = match side {
            Side::Left => (1, "aloop-left"),
            Side::Right => (2, "aloop-right"),
        };
        match kind {
            Kind::Snapsrc => Receiver::Snapsrc(Running::start(&format!(
                "snapsrc port={port} client-name={name} instance={instance} \
                 ! audioconvert ! audioresample ! alsasink device={} {}",
                side.pcm(),
                std::env::var("ALOOP_SINK_PROPS").unwrap_or_default()
            ))),
            Kind::CppSnapclient => {
                let bin = std::env::var("SNAPCLIENT").unwrap_or_else(|_| "snapclient".into());
                let child = Command::new(&bin)
                    .args(["-h", "127.0.0.1", "-p", &port.to_string()])
                    .args(["--player", "alsa", "--soundcard", side.pcm()])
                    .args(["--hostID", name, "--logsink", "null"])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .unwrap_or_else(|e| panic!("cannot start '{bin}': {e}"));
                Receiver::Cpp(child)
            }
        }
    }

    /// The pipeline latency of a snapsrc receiver (None for the C++ client).
    fn latency(&self) -> Option<gst::ClockTime> {
        let Receiver::Snapsrc(p) = self else {
            return None;
        };
        let mut q = gst::query::Latency::new();
        p.0.query(&mut q).then(|| q.result().1)
    }

    fn assert_alive(&mut self, what: &str) {
        match self {
            Receiver::Snapsrc(p) => p.assert_no_error(what),
            Receiver::Cpp(child) => {
                if let Ok(Some(status)) = child.try_wait() {
                    panic!("{what} (C++ snapclient) exited: {status}");
                }
            }
        }
    }
}

impl Drop for Receiver {
    fn drop(&mut self) {
        if let Receiver::Cpp(child) = self {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Sets `PCM Rate Shift` on loopback subdevice 0, and resets it when dropped.
///
/// snd-aloop has one control per *end* of the cable: device 0 (playback) and device 1
/// (capture). Shifting only one end does not change the rate the capture sees (checked
/// 2026-11-19), so both are set: the whole "sound card" runs slower (value > 100000) or
/// faster. A value of 100000 + n changes the rate by about −10·n ppm.
struct RateShift {
    card: String,
}

impl RateShift {
    fn set(card: &str, value: i64) -> Self {
        let shift = Self { card: card.into() };
        shift.write(value);
        shift
    }

    fn write(&self, value: i64) {
        for device in [0, 1] {
            let status = Command::new("amixer")
                .args(["-q", "-c", &self.card, "cset"])
                .arg(format!(
                    "iface=PCM,name=PCM Rate Shift 100000,device={device},subdevice=0"
                ))
                .arg(value.to_string())
                .status()
                .unwrap_or_else(|e| panic!("cannot run amixer (alsa-utils): {e}"));
            assert!(status.success(), "amixer failed to set PCM Rate Shift");
        }
    }
}

impl Drop for RateShift {
    fn drop(&mut self) {
        self.write(RATE_SHIFT_NOMINAL);
    }
}

/// What the capture recorded after warm-up.
struct Capture {
    left: Vec<i16>,
    right: Vec<i16>,
    /// Captured frames per second of `CLOCK_MONOTONIC`.
    card_rate: f64,
    /// Index of `left[0]` among all captured frames (the warm-up came before).
    first_frame: usize,
    /// `CLOCK_MONOTONIC` seconds of captured frame 0: `t(frame) = t0 + frame / card_rate`.
    t0: f64,
    /// Spread of the arrival delays in ms, a measure of the capture's timing jitter.
    jitter_ms: f64,
}

/// The capture side of subdevice 0, read in parts (an endurance run must not keep
/// hours of audio).
struct CaptureStream {
    _rec: Running,
    appsink: gst_app::AppSink,
    /// Frames captured so far.
    frames: usize,
}

impl CaptureStream {
    fn start(card: &str) -> Self {
        let rec = Running::start(&format!(
            "alsasrc device=hw:{card},1,0 \
             ! audio/x-raw,format=S16LE,rate={RATE},channels=2 \
             ! appsink name=out sync=false"
        ));
        let appsink = rec
            .0
            .by_name("out")
            .unwrap()
            .downcast::<gst_app::AppSink>()
            .unwrap();
        Self {
            _rec: rec,
            appsink,
            frames: 0,
        }
    }

    /// Records about `seconds` more (whole buffers), checking the receivers on the way.
    /// The card's rate and the time of frame 0 are fitted over this part only.
    fn record(&mut self, seconds: f64, receivers: &mut [(&str, &mut Receiver)]) -> Capture {
        let first_frame = self.frames;
        let target = first_frame + (seconds * f64::from(RATE)) as usize;
        let (mut left, mut right) = (Vec::new(), Vec::new());
        // (arrival time, frames captured so far) of each buffer we had to wait for. A
        // buffer that was already queued (the test was busy, e.g. analysing the previous
        // part, or stalled) arrives late by an unknown amount: not used for the fit.
        let mut arrivals: Vec<(f64, f64)> = Vec::new();
        while self.frames < target {
            let asked = Instant::now();
            let sample = self
                .appsink
                .try_pull_sample(gst::ClockTime::from_seconds(5))
                .expect("no audio captured from the loopback device");
            let waited = asked.elapsed() >= Duration::from_micros(500);
            let map = sample.buffer().unwrap().map_readable().unwrap();
            for f in map.as_chunks::<4>().0 {
                left.push(i16::from_le_bytes([f[0], f[1]]));
                right.push(i16::from_le_bytes([f[2], f[3]]));
            }
            self.frames += map.len() / 4;
            if waited {
                arrivals.push((now_usec() as f64 / 1e6, self.frames as f64));
            }
            for (what, receiver) in receivers.iter_mut() {
                receiver.assert_alive(what);
            }
        }
        let card_rate = slope(&arrivals);
        // Each buffer arrives (slightly) after its last frame was captured, never before:
        // the earliest arrival gives the time of frame 0.
        let starts: Vec<f64> = arrivals.iter().map(|(t, n)| t - n / card_rate).collect();
        let t0 = starts.iter().copied().fold(f64::MAX, f64::min);
        let latest = starts.iter().copied().fold(f64::MIN, f64::max);
        Capture {
            left,
            right,
            card_rate,
            first_frame,
            t0,
            jitter_ms: (latest - t0) * 1000.0,
        }
    }
}

/// Records `WARMUP + seconds` from the capture side of subdevice 0 and keeps the part
/// after warm-up, checking the receivers on the way.
fn capture(card: &str, seconds: u64, receivers: &mut [(&str, &mut Receiver)]) -> Capture {
    let mut stream = CaptureStream::start(card);
    stream.record(WARMUP.as_secs_f64(), receivers);
    stream.record(seconds as f64, receivers)
}

/// Each tick's offset against the server timeline in ms (positive = late): when it left
/// the card versus when it was due.
fn absolute_offsets_ms(ticks: &[f64], cap: &Capture, due: &[f64]) -> Vec<f64> {
    ticks
        .iter()
        .filter_map(|&f| {
            let out = cap.t0 + (cap.first_frame as f64 + f) / cap.card_rate;
            due.iter()
                .map(|&d| out - d)
                .min_by(|x, y| x.abs().total_cmp(&y.abs()))
                .filter(|d| d.abs() < 0.5)
                .map(|d| d * 1000.0)
        })
        .collect()
}

/// Mean, min and max.
fn stats(values: &[f64]) -> (f64, f64, f64) {
    let mean = values.iter().sum::<f64>() / values.len().max(1) as f64;
    let (min, max) = values
        .iter()
        .fold((f64::MAX, f64::MIN), |(lo, hi), &v| (lo.min(v), hi.max(v)));
    (mean, min, max)
}

/// One line about a receiver's absolute offset, e.g. for the test output.
fn describe_absolute(name: &str, offsets: &[f64], latency: Option<gst::ClockTime>) -> String {
    if std::env::var_os("ALOOP_VERBOSE").is_some() {
        let ticks: Vec<String> = offsets.iter().map(|o| format!("{o:+.2}")).collect();
        eprintln!("aloop: {name} per tick (ms): {}", ticks.join(" "));
    }
    let (mean, min, max) = stats(offsets);
    let latency = latency
        .map(|l| format!(" (its pipeline latency {} ms)", l.mseconds()))
        .unwrap_or_default();
    format!("{name} {mean:+.1} ms [{min:+.1}..{max:+.1}]{latency}")
}

/// Least-squares slope of y over x.
fn slope(points: &[(f64, f64)]) -> f64 {
    let n = points.len() as f64;
    let (mx, my) = points
        .iter()
        .fold((0.0, 0.0), |(a, b), (x, y)| (a + x / n, b + y / n));
    let (sxy, sxx) = points.iter().fold((0.0, 0.0), |(a, b), (x, y)| {
        (a + (x - mx) * (y - my), b + (x - mx) * (x - mx))
    });
    sxy / sxx
}

/// Finds tick onsets in a stream of samples, chunk by chunk, with linear interpolation
/// between samples. Frame numbers count on across chunks, from `start`.
#[derive(Default)]
struct OnsetDetector {
    start: usize,
    n: usize,
    prev: i16,
    last_loud: Option<usize>,
}

impl OnsetDetector {
    fn starting_at(frame: usize) -> Self {
        Self {
            start: frame,
            n: frame,
            ..Self::default()
        }
    }

    fn feed(&mut self, samples: impl IntoIterator<Item = i16>) -> Vec<f64> {
        let mut found = Vec::new();
        for v in samples {
            let i = self.n;
            self.n += 1;
            let prev = std::mem::replace(&mut self.prev, v);
            if v.unsigned_abs() < THRESHOLD as u16 {
                continue;
            }
            let quiet_before = self.last_loud.is_none_or(|l| i - l > QUIET_FRAMES);
            // Skip a tick that was already in progress when the stream started.
            let seen = i - self.start;
            if quiet_before && seen > 0 && (self.last_loud.is_some() || seen > QUIET_FRAMES) {
                let (a, b) = (f64::from(prev), f64::from(v));
                let t = f64::from(THRESHOLD) * b.signum();
                let frac = if b != a {
                    ((t - a) / (b - a)).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                found.push((i - 1) as f64 + frac);
            }
            self.last_loud = Some(i);
        }
        found
    }
}

/// Tick onsets in frames of `channel`.
fn onsets(channel: &[i16]) -> Vec<f64> {
    OnsetDetector::default().feed(channel.iter().copied())
}

/// Rate error of a receiver against the server timeline, in ppm: its tick period in
/// captured frames, compared with the card's frames per second.
fn rate_error_ppm(ticks: &[f64], card_rate: f64) -> f64 {
    let points: Vec<(f64, f64)> = ticks
        .iter()
        .map(|&t| (((t - ticks[0]) / card_rate).round(), t))
        .collect();
    (slope(&points) / card_rate - 1.0) * 1e6
}

fn run(a: Kind, b: Kind, rate_shift: Option<i64>) {
    init();
    let card = card();
    let seconds: u64 = env_or("ALOOP_SECONDS", 20);
    let tol_us: f64 = if rate_shift.is_some() {
        env_or("ALOOP_TOLERANCE_DRIFT_US", 2500.0)
    } else {
        env_or("ALOOP_TOLERANCE_US", 1000.0)
    };
    let tol_ppm: f64 = env_or("ALOOP_TOLERANCE_PPM", 100.0);

    let _shift = RateShift::set(&card, rate_shift.unwrap_or(RATE_SHIFT_NOMINAL));
    let sender = start_sender();
    let port = sender.port;
    let mut left = Receiver::start(a, Side::Left, port);
    let mut right = Receiver::start(b, Side::Right, port);
    let cap = capture(
        &card,
        seconds,
        &mut [
            ("receiver A (left)", &mut left),
            ("receiver B (right)", &mut right),
        ],
    );
    let (lat_a, lat_b) = (left.latency(), right.latency());
    drop((left, right));
    let due = sender.due_times();

    let (l, r) = (onsets(&cap.left), onsets(&cap.right));
    let min_ticks = seconds as usize - 2;
    assert!(
        l.len() >= min_ticks,
        "receiver A: only {} ticks in {seconds} s",
        l.len()
    );
    assert!(
        r.len() >= min_ticks,
        "receiver B: only {} ticks in {seconds} s",
        r.len()
    );

    // Pair each left tick with the nearest right tick (B minus A, in µs).
    let offsets_us: Vec<f64> = l
        .iter()
        .filter_map(|&lt| {
            r.iter()
                .map(|&rt| rt - lt)
                .min_by(|x, y| x.abs().total_cmp(&y.abs()))
                .filter(|d| d.abs() < f64::from(RATE) / 2.0)
                .map(|d| d * 1e6 / f64::from(RATE))
        })
        .collect();
    assert!(
        offsets_us.len() >= min_ticks,
        "only {} paired ticks",
        offsets_us.len()
    );

    let mean = offsets_us.iter().sum::<f64>() / offsets_us.len() as f64;
    let (min, max) = offsets_us
        .iter()
        .fold((f64::MAX, f64::MIN), |(lo, hi), &o| (lo.min(o), hi.max(o)));
    let shift = rate_shift.unwrap_or(RATE_SHIFT_NOMINAL);
    let card_ppm = (cap.card_rate / f64::from(RATE) - 1.0) * 1e6;
    let (ppm_a, ppm_b) = (
        rate_error_ppm(&l, cap.card_rate),
        rate_error_ppm(&r, cap.card_rate),
    );
    eprintln!(
        "aloop: rate shift {shift}, card measured {card_ppm:+.0} ppm; \
         offset B-A over {} ticks: mean {mean:+.0} µs, range {min:+.0}..{max:+.0} µs; \
         rate error A {ppm_a:+.1} ppm, B {ppm_b:+.1} ppm",
        offsets_us.len()
    );
    eprintln!(
        "aloop: against the server timeline: {}; {} (capture jitter {:.1} ms)",
        describe_absolute("A", &absolute_offsets_ms(&l, &cap, &due), lat_a),
        describe_absolute("B", &absolute_offsets_ms(&r, &cap, &due), lat_b),
        cap.jitter_ms
    );

    if rate_shift.is_some() {
        let expected = -10.0 * (shift - RATE_SHIFT_NOMINAL) as f64;
        assert!(
            (card_ppm - expected).abs() < expected.abs() / 2.0,
            "the rate shift did not take effect: card at {card_ppm:+.0} ppm, \
             expected about {expected:+.0} ppm"
        );
    }

    assert!(
        min.abs() <= tol_us && max.abs() <= tol_us,
        "receivers are {min:+.0}..{max:+.0} µs apart (tolerance ±{tol_us} µs)"
    );
    assert!(
        ppm_a.abs() <= tol_ppm && ppm_b.abs() <= tol_ppm,
        "receivers do not follow the server timeline: A {ppm_a:+.1} ppm, B {ppm_b:+.1} ppm \
         (tolerance ±{tol_ppm} ppm)"
    );
}

/// About −300 ppm: a slow but realistic "sound card" (consumer crystals are within
/// ~±100 ppm), measured against `CLOCK_MONOTONIC` in each run.
const DRIFT: i64 = RATE_SHIFT_NOMINAL + 30;

#[test]
#[ignore = "needs snd-aloop (make test-aloop)"]
fn aloop_two_snapsrc_are_in_sync() {
    run(Kind::Snapsrc, Kind::Snapsrc, None);
}

#[test]
#[ignore = "needs snd-aloop and the C++ snapclient (make test-aloop)"]
fn aloop_snapsrc_and_cpp_snapclient_are_in_sync() {
    run(Kind::Snapsrc, Kind::CppSnapclient, None);
}

#[test]
#[ignore = "needs snd-aloop (make test-aloop)"]
fn aloop_two_snapsrc_follow_a_drifting_card() {
    run(Kind::Snapsrc, Kind::Snapsrc, Some(DRIFT));
}

#[test]
#[ignore = "needs snd-aloop and the C++ snapclient (make test-aloop)"]
fn aloop_snapsrc_and_cpp_snapclient_follow_a_drifting_card() {
    run(Kind::Snapsrc, Kind::CppSnapclient, Some(DRIFT));
}

/// Finds silent gaps (at least 2 ms below 1 % full scale) in a continuous signal, after
/// its first loud sample, chunk by chunk: `(start frame, length in frames)`. Frame
/// numbers count on across chunks, from `start`.
#[derive(Default)]
struct GapDetector {
    n: usize,
    started: bool,
    run: usize,
}

impl GapDetector {
    const QUIET: u16 = 328;
    const MIN_FRAMES: usize = RATE as usize / 500;

    fn starting_at(frame: usize) -> Self {
        Self {
            n: frame,
            ..Self::default()
        }
    }

    fn feed(&mut self, samples: impl IntoIterator<Item = i16>) -> Vec<(usize, usize)> {
        let mut gaps = Vec::new();
        for v in samples {
            let i = self.n;
            self.n += 1;
            if !self.started {
                self.started = v.unsigned_abs() > THRESHOLD as u16;
                if !self.started {
                    continue;
                }
            }
            if v.unsigned_abs() < Self::QUIET {
                self.run += 1;
                continue;
            }
            if self.run >= Self::MIN_FRAMES {
                gaps.push((i - self.run, self.run));
            }
            self.run = 0;
        }
        gaps
    }
}

/// Silent gaps in `channel` (see [`GapDetector`]); all of it if it is never loud.
fn silent_gaps(channel: &[i16]) -> Vec<(usize, usize)> {
    let mut detector = GapDetector::default();
    let gaps = detector.feed(channel.iter().copied());
    if !detector.started {
        return vec![(0, channel.len())];
    }
    gaps
}

/// Regression test for frames that snapsrc dropped as "late" although the sink could
/// still render them (2026-11-19, audible gaps behind `pulsesink` on PipeWire).
///
/// The bug needs a pipeline latency larger than the sink's buffer: the sink then lets
/// `create()` run only after a frame's play-out time, but renders it at PTS + latency.
/// `pulsesink` behaves like that (130 ms latency); here it is forced with
/// `set_latency(150 ms)` and a 40 ms `alsasink` buffer. The sender uses 96 ms buffers,
/// like a decoded FLAC file. A continuous sine must arrive without silent gaps.
#[test]
#[ignore = "needs snd-aloop (make test-aloop)"]
fn aloop_snapsrc_loses_no_frames_behind_a_high_latency_sink() {
    init();
    let card = card();
    let seconds: u64 = env_or("ALOOP_SECONDS", 20);

    let sender = start_sender_with(
        "audiotestsrc is-live=true freq=440 volume=0.8 samplesperbuffer=4608",
        "pcm",
    );
    let port = sender.port;
    let receiver = pipeline(&format!(
        "snapsrc port={port} client-name=aloop-latency \
         ! audioconvert ! audioresample \
         ! alsasink device={} buffer-time=40000 latency-time=10000",
        Side::Left.pcm()
    ));
    receiver.set_latency(gst::ClockTime::from_mseconds(150));
    play(&receiver);
    let mut receiver = Receiver::Snapsrc(Running(receiver));
    let cap = capture(&card, seconds, &mut [("snapsrc", &mut receiver)]);
    drop(receiver);

    let gaps = silent_gaps(&cap.left);
    let total_ms: usize = gaps.iter().map(|(_, n)| n * 1000 / RATE as usize).sum();
    eprintln!(
        "aloop: high-latency sink: {} silent gaps ({total_ms} ms) in {seconds} s{}",
        gaps.len(),
        gaps.iter()
            .take(5)
            .map(|(at, n)| format!(
                ", {:.2} s: {} ms",
                *at as f64 / f64::from(RATE),
                n * 1000 / RATE as usize
            ))
            .collect::<String>()
    );
    assert!(
        gaps.is_empty(),
        "snapsrc lost audio: {} silent gaps ({total_ms} ms)",
        gaps.len()
    );
}

/// One receiver alone, measured against the server timeline (see "Absolute reference").
fn run_absolute(kind: Kind, side: Side, codec: &str) {
    init();
    let card = card();
    let seconds: u64 = env_or("ALOOP_SECONDS", 20);
    let tol_ms: f64 = env_or("ALOOP_TOLERANCE_ABS_US", 5000.0) / 1000.0;

    let _shift = RateShift::set(&card, RATE_SHIFT_NOMINAL);
    let sender = start_sender_with(TICKS, codec);
    let mut receiver = Receiver::start(kind, side, sender.port);
    let cap = capture(&card, seconds, &mut [("receiver", &mut receiver)]);
    let latency = receiver.latency();
    drop(receiver);
    let due = sender.due_times();

    let channel = match side {
        Side::Left => &cap.left,
        Side::Right => &cap.right,
    };
    let ticks = onsets(channel);
    let offsets = absolute_offsets_ms(&ticks, &cap, &due);
    let name = match kind {
        Kind::Snapsrc => "snapsrc",
        Kind::CppSnapclient => "C++ snapclient",
    };
    eprintln!(
        "aloop: against the server timeline ({codec}): {} over {} ticks (capture jitter {:.1} ms)",
        describe_absolute(name, &offsets, latency),
        offsets.len(),
        cap.jitter_ms
    );
    assert!(
        offsets.len() >= seconds as usize - 2,
        "only {} of {} ticks matched the server timeline",
        offsets.len(),
        ticks.len()
    );
    let (mean, _, _) = stats(&offsets);
    assert!(
        mean.abs() <= tol_ms,
        "{name} plays {mean:+.1} ms off the server timeline (tolerance ±{tol_ms} ms)"
    );
}

#[test]
#[ignore = "needs snd-aloop (make test-aloop)"]
fn aloop_snapsrc_plays_on_the_server_timeline() {
    run_absolute(Kind::Snapsrc, Side::Left, "pcm");
}

#[test]
#[ignore = "needs snd-aloop and the C++ snapclient (make test-aloop)"]
fn aloop_cpp_snapclient_plays_on_the_server_timeline() {
    run_absolute(Kind::CppSnapclient, Side::Right, "pcm");
}

/// L4: the FLAC encoder buffers samples. Does it stamp its output chunks too late? Both
/// receivers decode the same stream, so a timestamp error shows in both.
#[test]
#[ignore = "needs snd-aloop (make test-aloop)"]
fn aloop_snapsrc_plays_flac_on_the_server_timeline() {
    run_absolute(Kind::Snapsrc, Side::Left, "flac");
}

#[test]
#[ignore = "needs snd-aloop and the C++ snapclient (make test-aloop)"]
fn aloop_cpp_snapclient_plays_flac_on_the_server_timeline() {
    run_absolute(Kind::CppSnapclient, Side::Right, "flac");
}

/// F5: the sender pauses for 4 s. Both receivers must play digital silence meanwhile
/// and be in sync again afterwards. snapsrc stays on the server timeline right away
/// (its ticks keep 1 s spacing); the C++ snapclient is +2…+3.5 ms off for about 3 s
/// after such a gap before it settles (measured 2026-11-22), so its pair offset gets
/// `ALOOP_TOLERANCE_SETTLE_US` (default 5000 µs) until 4 s after the resume.
#[test]
#[ignore = "needs snd-aloop and the C++ snapclient (make test-aloop)"]
fn aloop_sender_pause_is_silent_and_resumes_in_sync() {
    init();
    let card = card();
    let tol_us: f64 = env_or("ALOOP_TOLERANCE_US", 1000.0);
    let settle_us: f64 = env_or("ALOOP_TOLERANCE_SETTLE_US", 5000.0);
    let _shift = RateShift::set(&card, RATE_SHIFT_NOMINAL);
    let sender = start_sender();
    let port = sender.port;
    let mut left = Receiver::start(Kind::Snapsrc, Side::Left, port);
    let mut right = Receiver::start(Kind::CppSnapclient, Side::Right, port);

    // Pause 6 s into the measurement, for 4 s (CLOCK_MONOTONIC seconds recorded).
    let pipeline = sender.running.0.clone();
    let pauser = std::thread::spawn(move || {
        std::thread::sleep(WARMUP + Duration::from_secs(6));
        let paused_at = now_usec() as f64 / 1e6;
        pipeline.set_state(gst::State::Paused).unwrap();
        std::thread::sleep(Duration::from_secs(4));
        let resumed_at = now_usec() as f64 / 1e6;
        pipeline.set_state(gst::State::Playing).unwrap();
        (paused_at, resumed_at)
    });
    let cap = capture(
        &card,
        20,
        &mut [("snapsrc (left)", &mut left), ("C++ (right)", &mut right)],
    );
    let (paused_at, resumed_at) = pauser.join().unwrap();
    drop((left, right));

    let time_of = |frame: f64| cap.t0 + (cap.first_frame as f64 + frame) / cap.card_rate;
    // Seconds after the pause reached the output (it does so `BUFFER_MS` later).
    let buffer_s = f64::from(BUFFER_MS) / 1000.0;
    let rel = |t: f64| t - paused_at - buffer_s;
    let pause_s = resumed_at - paused_at;
    for (name, channel) in [("snapsrc", &cap.left), ("C++", &cap.right)] {
        let ticks: Vec<String> = onsets(channel)
            .iter()
            .map(|&f| format!("{:+.3}", rel(time_of(f))))
            .collect();
        eprintln!(
            "aloop: {name} ticks (s after the pause): {}",
            ticks.join(" ")
        );
        let window = 0.2..pause_s - 0.2;
        let peak = channel
            .iter()
            .enumerate()
            .filter(|(i, _)| window.contains(&rel(time_of(*i as f64))))
            .map(|(_, v)| v.unsigned_abs())
            .max()
            .unwrap_or(0);
        assert_eq!(peak, 0, "{name} is not silent during the pause");
    }

    // snapsrc: the ticks after the resume keep their 1 s spacing. The first two may be
    // off: the live test source re-anchors its timestamps after a pause, by a hole or an
    // overlap of a few ms; snapsrc bridges or trims jumps beyond 5 ms at once, smaller
    // ones are corrected gradually by its aligner (within about a second).
    let after: Vec<f64> = onsets(&cap.left)
        .iter()
        .map(|&f| rel(time_of(f)))
        .filter(|&t| t > pause_s + 1.5)
        .collect();
    assert!(
        after.len() >= 6,
        "snapsrc: only {} ticks after the pause",
        after.len()
    );
    for w in after.windows(2) {
        let spacing_ms = (w[1] - w[0]) * 1000.0;
        assert!(
            (spacing_ms - 1000.0).abs() <= tol_us / 1000.0,
            "snapsrc ticks {spacing_ms:.2} ms apart after the pause"
        );
    }

    let (l, r) = (onsets(&cap.left), onsets(&cap.right));
    let pairs: Vec<(f64, f64)> = l
        .iter()
        .filter_map(|&lt| {
            r.iter()
                .map(|&rt| rt - lt)
                .min_by(|x, y| x.abs().total_cmp(&y.abs()))
                .filter(|d| d.abs() < f64::from(RATE) / 2.0)
                .map(|d| (rel(time_of(lt)), d * 1e6 / f64::from(RATE)))
        })
        .collect();
    let list: Vec<String> = pairs
        .iter()
        .map(|(t, o)| format!("{t:+.1}s:{o:+.0}"))
        .collect();
    eprintln!("aloop: offset C++ − snapsrc (µs): {}", list.join(" "));
    let settled = pause_s + 4.0;
    assert!(
        pairs.iter().filter(|(t, _)| *t > settled).count() >= 4,
        "too few paired ticks after the pause"
    );
    for (t, o) in &pairs {
        let bound = if (pause_s..settled).contains(t) {
            settle_us
        } else {
            tol_us
        };
        assert!(
            o.abs() <= bound,
            "receivers {o:+.0} µs apart at {t:+.1} s (bound ±{bound} µs)"
        );
    }
}

/// Resident memory of a process in KiB (`/proc/<pid>/status`), if readable.
fn rss_kib(pid: &str) -> Option<u64> {
    std::fs::read_to_string(format!("/proc/{pid}/status"))
        .ok()?
        .lines()
        .find_map(|l| l.strip_prefix("VmRSS:"))?
        .trim()
        .trim_end_matches("kB")
        .trim()
        .parse()
        .ok()
}

/// Least-squares slope of `values` over their index, per window.
fn trend(values: &[f64]) -> f64 {
    let points: Vec<(f64, f64)> = values
        .iter()
        .enumerate()
        .map(|(i, &v)| (i as f64, v))
        .collect();
    if points.len() < 2 {
        0.0
    } else {
        slope(&points)
    }
}

/// One window of the endurance run.
struct Window {
    end_s: f64,
    ticks: (usize, usize),
    gaps: (usize, usize),
    gap_ms: (f64, f64),
    pair_us: (f64, f64, f64),
    abs_a_ms: (f64, f64, f64),
    abs_b_ms: (f64, f64, f64),
    rss_kib: (u64, u64),
    latency_ms: u64,
    card_ppm: f64,
    /// Spread of the capture's arrival times (ms): how well this window's samples could
    /// be mapped to clock time. About 3 ms normally; a stall of the test process makes
    /// it large, and then the offsets against the server are not measurable.
    capture_jitter_ms: f64,
}

impl Window {
    const CSV_HEADER: &str = "end_s,ticks_snapsrc,ticks_cpp,gaps_snapsrc,gaps_cpp,\
        gap_ms_snapsrc,gap_ms_cpp,pair_mean_us,pair_min_us,pair_max_us,\
        abs_snapsrc_mean_ms,abs_snapsrc_min_ms,abs_snapsrc_max_ms,\
        abs_cpp_mean_ms,abs_cpp_min_ms,abs_cpp_max_ms,\
        rss_test_kib,rss_cpp_kib,latency_snapsrc_ms,card_ppm,capture_jitter_ms";

    fn csv(&self) -> String {
        format!(
            "{:.0},{},{},{},{},{:.1},{:.1},{:.0},{:.0},{:.0},{:.2},{:.2},{:.2},{:.2},{:.2},{:.2},{},{},{},{:.1},{:.1}",
            self.end_s,
            self.ticks.0,
            self.ticks.1,
            self.gaps.0,
            self.gaps.1,
            self.gap_ms.0,
            self.gap_ms.1,
            self.pair_us.0,
            self.pair_us.1,
            self.pair_us.2,
            self.abs_a_ms.0,
            self.abs_a_ms.1,
            self.abs_a_ms.2,
            self.abs_b_ms.0,
            self.abs_b_ms.1,
            self.abs_b_ms.2,
            self.rss_kib.0,
            self.rss_kib.1,
            self.latency_ms,
            self.card_ppm,
            self.capture_jitter_ms,
        )
    }
}

/// F3, endurance: snapsrc and the C++ snapclient play for `ENDURANCE_MINUTES` (default 60)
/// on the loopback card. Every `ENDURANCE_WINDOW_SECS` (default 60) one line goes to
/// stderr and one row to a CSV file (`ENDURANCE_CSV`, default
/// `target/endurance-<unix time>.csv`): ticks found, silent gaps (dropouts), offset
/// between the receivers, each receiver against the server timeline, resident memory of
/// the test process (sender + snapsrc) and of the C++ client, snapsrc's pipeline latency
/// and the card's rate.
///
/// The signal is a tick per second over a quiet 440 Hz sine, so that a dropout anywhere
/// shows up as a silent gap, not only one that hits a tick.
///
/// `ENDURANCE_SINE_VOLUME=0` sends the ticks alone (then every pause between ticks counts
/// as a gap, so the gap check is skipped).
///
/// At the end it checks, for every window: no missing ticks, the receivers' mean offset
/// within `ALOOP_TOLERANCE_US` (F2) and every single tick within `ENDURANCE_TOLERANCE_US`
/// (default 2500: snapsrc wanders by up to ±1.5 ms in some runs, the restlessness of the
/// audio sink's `resample` slaving, see docs/claude/memory.md), both receivers within
/// `ALOOP_TOLERANCE_ABS_US` of the server timeline. That last check needs the window's
/// capture mapped to clock time; a stall of the test process (once in the first hour-long
/// run, 2026-11-22: both receivers "−499..+489 ms", their offset to each other +19 µs)
/// spoils the mapping, so windows whose capture jitter exceeds `ENDURANCE_MAX_JITTER_MS`
/// (default 20; normally about 3) are skipped there and listed, and more than 10 % of
/// such windows fail the run. Over the whole run: no dropouts, snapsrc's offset against the server not wandering by more
/// than `ENDURANCE_WANDER_US` (default 1000) between the first and the last window, and
/// the test process growing by at most `ENDURANCE_RSS_GROWTH_MB` (default 16) after the
/// first window. Run with `make test-endurance`.
#[test]
#[ignore = "needs snd-aloop and the C++ snapclient, runs for an hour (make test-endurance)"]
fn aloop_endurance() {
    init();
    let card = card();
    let minutes: f64 = env_or("ENDURANCE_MINUTES", 60.0);
    let window_s: f64 = env_or("ENDURANCE_WINDOW_SECS", 60.0);
    let tol_us: f64 = env_or("ALOOP_TOLERANCE_US", 1000.0);
    let tol_tick_us: f64 = env_or("ENDURANCE_TOLERANCE_US", 2500.0);
    let tol_abs_ms: f64 = env_or("ALOOP_TOLERANCE_ABS_US", 5000.0) / 1000.0;
    let wander_ms: f64 = env_or("ENDURANCE_WANDER_US", 1000.0) / 1000.0;
    let max_jitter_ms: f64 = env_or("ENDURANCE_MAX_JITTER_MS", 20.0);
    let rss_growth_kib: f64 = env_or("ENDURANCE_RSS_GROWTH_MB", 16.0) * 1024.0;
    let csv_path = std::env::var("ENDURANCE_CSV").unwrap_or_else(|_| {
        format!(
            "{}/../../target/endurance-{}.csv",
            env!("CARGO_MANIFEST_DIR"),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs()
        )
    });
    let mut csv = std::fs::File::create(&csv_path)
        .unwrap_or_else(|e| panic!("cannot create {csv_path}: {e}"));
    use std::io::Write as _;
    writeln!(csv, "{}", Window::CSV_HEADER).unwrap();
    eprintln!("aloop: endurance for {minutes} min, windows of {window_s} s, CSV: {csv_path}");

    let _shift = RateShift::set(&card, RATE_SHIFT_NOMINAL);
    let sine: f64 = env_or("ENDURANCE_SINE_VOLUME", 0.05);
    let source = if sine > 0.0 {
        format!(
            "audiomixer name=mix \
             {TICKS} ! audio/x-raw,format=S16LE,rate={RATE},channels=2 ! mix. \
             audiotestsrc is-live=true wave=sine freq=440 volume={sine} \
               ! audio/x-raw,format=S16LE,rate={RATE},channels=2 ! mix. \
             mix."
        )
    } else {
        TICKS.to_owned()
    };
    let sender = start_sender_with(&source, "pcm");
    let mut left = Receiver::start(Kind::Snapsrc, Side::Left, sender.port);
    let mut right = Receiver::start(Kind::CppSnapclient, Side::Right, sender.port);
    let cpp_pid = match &right {
        Receiver::Cpp(child) => child.id().to_string(),
        Receiver::Snapsrc(_) => unreachable!(),
    };

    let mut stream = CaptureStream::start(&card);
    stream.record(
        WARMUP.as_secs_f64(),
        &mut [("snapsrc", &mut left), ("C++", &mut right)],
    );
    let start_frame = stream.frames;
    let (mut onsets_l, mut onsets_r) = (
        OnsetDetector::starting_at(start_frame),
        OnsetDetector::starting_at(start_frame),
    );
    let (mut gaps_l, mut gaps_r) = (
        GapDetector::starting_at(start_frame),
        GapDetector::starting_at(start_frame),
    );
    let windows = (minutes * 60.0 / window_s).round().max(1.0) as usize;
    let mut results: Vec<Window> = Vec::new();
    let mut gap_list: Vec<String> = Vec::new();
    for w in 0..windows {
        let cap = stream.record(window_s, &mut [("snapsrc", &mut left), ("C++", &mut right)]);
        sender.running.assert_no_error("sender");
        let l = onsets_l.feed(cap.left.iter().copied());
        let r = onsets_r.feed(cap.right.iter().copied());
        let (gl, gr) = (
            gaps_l.feed(cap.left.iter().copied()),
            gaps_r.feed(cap.right.iter().copied()),
        );
        let t_of = |frame: usize| (frame - start_frame) as f64 / f64::from(RATE);
        for (name, gaps) in [("snapsrc", &gl), ("C++", &gr)] {
            for (at, n) in gaps {
                gap_list.push(format!(
                    "{name} at {:.1} s: {:.1} ms",
                    t_of(*at),
                    *n as f64 * 1000.0 / f64::from(RATE)
                ));
            }
        }
        let gap_ms = |g: &[(usize, usize)]| {
            g.iter().map(|(_, n)| *n as f64).sum::<f64>() * 1000.0 / f64::from(RATE)
        };

        // Offsets: pair each snapsrc tick with the nearest C++ tick (C++ minus snapsrc).
        let pairs: Vec<f64> = l
            .iter()
            .filter_map(|&lt| {
                r.iter()
                    .map(|&rt| rt - lt)
                    .min_by(|x, y| x.abs().total_cmp(&y.abs()))
                    .filter(|d| d.abs() < f64::from(RATE) / 2.0)
                    .map(|d| d * 1e6 / f64::from(RATE))
            })
            .collect();
        let due = sender.due_times();
        let local = |ticks: &[f64]| -> Vec<f64> {
            ticks.iter().map(|f| f - cap.first_frame as f64).collect()
        };
        let abs_a = absolute_offsets_ms(&local(&l), &cap, &due);
        let abs_b = absolute_offsets_ms(&local(&r), &cap, &due);

        let window = Window {
            end_s: t_of(stream.frames),
            ticks: (l.len(), r.len()),
            gaps: (gl.len(), gr.len()),
            gap_ms: (gap_ms(&gl), gap_ms(&gr)),
            pair_us: stats(&pairs),
            abs_a_ms: stats(&abs_a),
            abs_b_ms: stats(&abs_b),
            rss_kib: (rss_kib("self").unwrap_or(0), rss_kib(&cpp_pid).unwrap_or(0)),
            latency_ms: left.latency().map_or(0, |l| l.mseconds()),
            card_ppm: (cap.card_rate / f64::from(RATE) - 1.0) * 1e6,
            capture_jitter_ms: cap.jitter_ms,
        };
        writeln!(csv, "{}", window.csv()).unwrap();
        csv.flush().unwrap();
        eprintln!(
            "aloop: endurance {:>3}/{windows} {:>6.0} s: ticks {}/{}, gaps {}/{}, \
             C++−snapsrc {:+.0} µs [{:+.0}..{:+.0}], vs server snapsrc {:+.2} ms, C++ {:+.2} ms, \
             RSS {} / {} MiB",
            w + 1,
            window.end_s,
            window.ticks.0,
            window.ticks.1,
            window.gaps.0,
            window.gaps.1,
            window.pair_us.0,
            window.pair_us.1,
            window.pair_us.2,
            window.abs_a_ms.0,
            window.abs_b_ms.0,
            window.rss_kib.0 / 1024,
            window.rss_kib.1 / 1024,
        );
        results.push(window);
    }
    drop((left, right));

    // Summary and checks.
    // The first window is warm-up for memory (buffers, caches): trends and the memory
    // baseline start after it.
    let col = |f: &dyn Fn(&Window) -> f64| {
        let skip = usize::from(results.len() > 2);
        results.iter().skip(skip).map(f).collect::<Vec<f64>>()
    };
    let rss_test = col(&|w| w.rss_kib.0 as f64);
    let rss_cpp = col(&|w| w.rss_kib.1 as f64);
    let reliable = |w: &Window| w.capture_jitter_ms <= max_jitter_ms;
    let unreliable: Vec<String> = results
        .iter()
        .enumerate()
        .filter(|(_, w)| !reliable(w))
        .map(|(i, w)| format!("{} ({:.0} ms)", i + 1, w.capture_jitter_ms))
        .collect();
    // Against the server: only windows whose capture could be mapped to clock time.
    let abs_a: Vec<f64> = results
        .iter()
        .skip(usize::from(results.len() > 2))
        .filter(|w| reliable(w))
        .map(|w| w.abs_a_ms.0)
        .collect();
    let per_hour = 3600.0 / window_s;
    eprintln!(
        "aloop: endurance summary: {} windows; trends per hour: RSS test {:+.1} MiB, C++ {:+.1} MiB, \
         snapsrc vs server {:+.3} ms; capture not mappable to clock time in windows: {}; gaps: {}",
        results.len(),
        trend(&rss_test) * per_hour / 1024.0,
        trend(&rss_cpp) * per_hour / 1024.0,
        trend(&abs_a) * per_hour,
        if unreliable.is_empty() {
            "none".into()
        } else {
            unreliable.join(", ")
        },
        if sine <= 0.0 {
            "not checked (no sine)".into()
        } else if gap_list.is_empty() {
            "none".into()
        } else {
            gap_list.join(", ")
        }
    );
    let expected = window_s.round() as usize;
    for (i, w) in results.iter().enumerate() {
        let at = format!("window {} (ending at {:.0} s)", i + 1, w.end_s);
        assert!(
            w.ticks.0 + 1 >= expected && w.ticks.1 + 1 >= expected,
            "{at}: missing ticks: {}/{} of {expected}",
            w.ticks.0,
            w.ticks.1
        );
        assert!(
            w.pair_us.0.abs() <= tol_us,
            "{at}: receivers {:+.0} µs apart on average (tolerance ±{tol_us} µs)",
            w.pair_us.0
        );
        assert!(
            w.pair_us.1.abs() <= tol_tick_us && w.pair_us.2.abs() <= tol_tick_us,
            "{at}: receivers {:+.0}..{:+.0} µs apart (tolerance ±{tol_tick_us} µs)",
            w.pair_us.1,
            w.pair_us.2
        );
        if !reliable(w) {
            continue; // the offsets against the server were not measurable
        }
        for (name, (_, min, max)) in [("snapsrc", w.abs_a_ms), ("C++", w.abs_b_ms)] {
            assert!(
                min.abs() <= tol_abs_ms && max.abs() <= tol_abs_ms,
                "{at}: {name} {min:+.2}..{max:+.2} ms against the server timeline"
            );
        }
    }
    if sine > 0.0 {
        assert!(gap_list.is_empty(), "dropouts: {}", gap_list.join(", "));
    }
    assert!(
        unreliable.len() * 10 <= results.len(),
        "measurement unreliable: the capture could not be mapped to clock time in windows {}",
        unreliable.join(", ")
    );
    let wander = abs_a.last().unwrap() - abs_a[0];
    assert!(
        wander.abs() <= wander_ms,
        "snapsrc wandered {wander:+.2} ms against the server timeline"
    );
    if results.len() > 1 {
        let growth = rss_test.last().unwrap() - rss_test[0];
        assert!(
            growth <= rss_growth_kib,
            "the test process (sender + snapsrc) grew by {:.1} MiB",
            growth / 1024.0
        );
    }
}
