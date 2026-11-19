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
//! - `ALOOP_TOLERANCE_PPM`: max allowed rate error against the server (default 100)
//! - `ALOOP_TOLERANCE_ABS_US`: max allowed absolute offset against the server timeline
//!   (default 5000; the method itself is good to a few ms)
//! - `ALOOP_SINK_PROPS`: extra properties for the snapsrc receivers' `alsasink`,
//!   e.g. `slave-method=skew drift-tolerance=5000`
//! - `SNAPCLIENT`: C++ snapclient binary (default: `PATH`)

use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use gst::prelude::*;
use gst_base::prelude::*;
use snapcast_engine::now_usec;

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

/// The sender, and what it sent: a reference copy of its left channel.
struct Sender {
    running: Running,
    port: u32,
    reference: Arc<Mutex<Reference>>,
}

#[derive(Default)]
struct Reference {
    first_pts: Option<gst::ClockTime>,
    left: Vec<i16>,
}

fn start_sender() -> Sender {
    start_sender_with("audiotestsrc is-live=true wave=ticks freq=1000 volume=0.8")
}

/// Starts `source ! S16LE 48 kHz stereo ! snapsink`, with a `tee` that keeps a
/// reference copy of what was sent.
fn start_sender_with(source: &str) -> Sender {
    let p = pipeline(&format!(
        "{source} \
         ! audio/x-raw,format=S16LE,rate={RATE},channels=2 ! tee name=t \
         t. ! queue ! snapsink name=sink port=0 buffer-ms={BUFFER_MS} \
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
                    r.left.extend(
                        map.as_chunks::<4>()
                            .0
                            .iter()
                            .map(|f| i16::from_le_bytes([f[0], f[1]])),
                    );
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
        onsets(&r.left)
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

/// Records `WARMUP + seconds` from the capture side of subdevice 0 and keeps the part
/// after warm-up, checking the receivers on the way.
fn capture(card: &str, seconds: u64, receivers: &mut [(&str, &mut Receiver)]) -> Capture {
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

    let warmup_frames = (WARMUP.as_secs() * u64::from(RATE)) as usize;
    let total_frames = warmup_frames + (seconds * u64::from(RATE)) as usize;
    let (mut left, mut right) = (Vec::new(), Vec::new());
    // (arrival time, frames captured so far) of each buffer after warm-up.
    let mut arrivals: Vec<(f64, f64)> = Vec::new();
    let mut frames = 0usize;
    while frames < total_frames {
        let sample = appsink
            .try_pull_sample(gst::ClockTime::from_seconds(5))
            .expect("no audio captured from the loopback device");
        let map = sample.buffer().unwrap().map_readable().unwrap();
        for f in map.as_chunks::<4>().0 {
            if frames >= warmup_frames {
                left.push(i16::from_le_bytes([f[0], f[1]]));
                right.push(i16::from_le_bytes([f[2], f[3]]));
            }
            frames += 1;
        }
        if frames > warmup_frames {
            arrivals.push((now_usec() as f64 / 1e6, frames as f64));
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
        first_frame: warmup_frames,
        t0,
        jitter_ms: (latest - t0) * 1000.0,
    }
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

/// Tick onsets in frames, with linear interpolation between samples.
fn onsets(channel: &[i16]) -> Vec<f64> {
    let mut found = Vec::new();
    let mut last_loud: Option<usize> = None;
    for (i, &v) in channel.iter().enumerate() {
        if v.unsigned_abs() < THRESHOLD as u16 {
            continue;
        }
        let quiet_before = last_loud.is_none_or(|l| i - l > QUIET_FRAMES);
        // Skip a tick that was already in progress when the capture window opened.
        if quiet_before && i > 0 && (last_loud.is_some() || i > QUIET_FRAMES) {
            let (a, b) = (f64::from(channel[i - 1]), f64::from(v));
            let t = f64::from(THRESHOLD) * b.signum();
            let frac = if b != a {
                ((t - a) / (b - a)).clamp(0.0, 1.0)
            } else {
                0.0
            };
            found.push((i - 1) as f64 + frac);
        }
        last_loud = Some(i);
    }
    found
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
    let tol_us: f64 = env_or("ALOOP_TOLERANCE_US", 1000.0);
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

/// Silent gaps (at least 2 ms below 1 % full scale) in a continuous signal, after its
/// first loud sample: `(start frame, length in frames)`.
fn silent_gaps(channel: &[i16]) -> Vec<(usize, usize)> {
    const QUIET: u16 = 328;
    const MIN_FRAMES: usize = RATE as usize / 500;
    let Some(start) = channel
        .iter()
        .position(|v| v.unsigned_abs() > THRESHOLD as u16)
    else {
        return vec![(0, channel.len())];
    };
    let mut gaps = Vec::new();
    let mut run = 0;
    for (i, v) in channel.iter().enumerate().skip(start) {
        if v.unsigned_abs() < QUIET {
            run += 1;
            continue;
        }
        if run >= MIN_FRAMES {
            gaps.push((i - run, run));
        }
        run = 0;
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

    let sender =
        start_sender_with("audiotestsrc is-live=true freq=440 volume=0.8 samplesperbuffer=4608");
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
fn run_absolute(kind: Kind, side: Side) {
    init();
    let card = card();
    let seconds: u64 = env_or("ALOOP_SECONDS", 20);
    let tol_ms: f64 = env_or("ALOOP_TOLERANCE_ABS_US", 5000.0) / 1000.0;

    let _shift = RateShift::set(&card, RATE_SHIFT_NOMINAL);
    let sender = start_sender();
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
        "aloop: against the server timeline: {} over {} ticks (capture jitter {:.1} ms)",
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
    run_absolute(Kind::Snapsrc, Side::Left);
}

#[test]
#[ignore = "needs snd-aloop and the C++ snapclient (make test-aloop)"]
fn aloop_cpp_snapclient_plays_on_the_server_timeline() {
    run_absolute(Kind::CppSnapclient, Side::Right);
}
