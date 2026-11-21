//! `snapsrc` implementation.
//!
//! # Timing
//!
//! `gst-snapcast-engine` hands out frames with a *local play-out time* in the
//! monotonic Snapcast clock domain (server timestamp corrected by Snapcast's
//! time sync plus the configured buffer): the moment the sound must come out of
//! the speaker. A sink renders a buffer at `PTS + pipeline latency` (and audio
//! sinks compensate their device delay within that latency). So we stamp each
//! buffer one pipeline latency *before* its play-out time, sampling the pipeline
//! clock:
//!
//! ```text
//! pts = clock.time() + (playout_usec - now_usec()) - base_time - latency
//! ```
//!
//! Network arrival time is never used. The pipeline announces its latency with a
//! LATENCY event, which sinks forward upstream; it arrives after the first
//! buffers and may change, so a change marks the next buffer DISCONT.
//!
//! Without the `- latency` term, the sound came out one pipeline latency late:
//! +62 ms behind `alsasink` (65 ms latency), while the C++ snapclient was on time
//! (L13; `aloop_*_plays_on_the_server_timeline`, 2026-11-19).
//!
//! Buffers arrive roughly `buffer-ms` ahead of their PTS.
//!
//! # Clock
//!
//! The element does not provide a clock. In a live pipeline the pipeline then
//! uses the system clock (`CLOCK_MONOTONIC`, the Snapcast clock domain): audio
//! sinks can offer their card's clock only after caps negotiation, too late for
//! the clock selection at PAUSED→PLAYING. Another live element (e.g. `alsasrc`)
//! may provide its card's clock instead; the formula above maps onto any clock.
//!
//! Two mechanisms keep the output on the Snapcast timeline:
//!
//! - **The audio sink** slaves its card to the pipeline clock. With its default
//!   `slave-method=skew` it follows the card and jumps back only after
//!   `drift-tolerance` (40 ms), and its start-up calibration left each run off by
//!   up to ~4 ms (L12 and start-up alignment, aloop tests 2026-11-19). With
//!   `slave-method=resample` it inserts/drops single samples continuously: rate
//!   error +4 ppm on a −300 ppm card, within 1 ms of the server timeline.
//!   `adjust-sink` (default on) therefore sets `resample` on the first audio sink
//!   downstream, if it is still at `skew`.
//! - **[`super::align`]** keeps the PTS contiguous, and corrects the difference
//!   between the ideal PTS and the sample count by inserting/dropping single frames.
//!   That absorbs time-sync steps (L5) and the drift of the pipeline clock against
//!   the Snapcast clock, if the pipeline clock is a card clock.
//!
//! Until 2026-11-19 the element provided a monotonic system clock itself. That is
//! equivalent for the sink, but it made snapsrc the clock provider even when
//! another element's clock would have been the better choice.

//! # Volume
//!
//! The server's volume and mute for this client (set by a controller, e.g. on a
//! C++ snapserver) are applied to the samples as a linear gain, like
//! snapclient-rs's software mixer, and shown in the read-only `volume`/`mute`
//! properties. A change is ramped across one buffer instead of jumping (no click).
//! The properties follow the client's events, not the audio: a C++ snapserver
//! sends no audio at all to a muted client, so snapsrc then produces no buffers.
//!
//! # Late frames
//!
//! Downstream decides when `create()` runs: an audio sink blocks the streaming
//! thread until its ring buffer has room, so a frame may leave the client queue
//! only shortly before its render time. A frame is late only once that render
//! time (`PTS + latency`, i.e. its play-out time) has passed; it is then dropped
//! and the next buffer marked DISCONT, e.g. frames queued while PAUSED.
//!
//! An earlier version stamped `pts = playout` but still checked `playout < now`:
//! behind `pulsesink` (130 ms latency, small ring buffer) about every fourth
//! 96 ms block was dropped as "late" although the sink could still render it
//! (audible gaps, 2026-11-19; `aloop_snapsrc_loses_no_frames_behind_a_high_latency_sink`).

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};

use gst::glib;
use gst::prelude::*;
use gst::subclass::prelude::*;
use gst_base::prelude::*;
use gst_base::subclass::base_src::CreateSuccess;
use gst_base::subclass::prelude::*;
use gst_snapcast_engine::client::{self, ClientHandle, ClientOptions};
use gst_snapcast_engine::{now_usec, runtime};

use super::align::{self, Aligner};
use tokio::sync::watch;

static CAT: LazyLock<gst::DebugCategory> = LazyLock::new(|| {
    gst::DebugCategory::new(
        "snapsrc",
        gst::DebugColorFlags::empty(),
        Some("Snapcast client source"),
    )
});

const DEFAULT_HOST: &str = "127.0.0.1";
// Snapcast's default stream port (snapcast_proto::DEFAULT_STREAM_PORT).
const DEFAULT_PORT: u32 = 1704;
const DEFAULT_CLIENT_NAME: &str = "gst-snapsrc";
/// 0 = automatic: the lowest number no other snapsrc on this host uses.
const DEFAULT_INSTANCE: u32 = 0;
const DEFAULT_LATENCY_MS: i32 = 0;
const DEFAULT_ADJUST_SINK: bool = true;

#[derive(Debug, Clone)]
struct Settings {
    host: String,
    port: u32,
    client_name: String,
    instance: u32,
    latency_ms: i32,
    adjust_sink: bool,
    host_id: String,
    mdns_name: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            host: DEFAULT_HOST.to_owned(),
            port: DEFAULT_PORT,
            client_name: DEFAULT_CLIENT_NAME.to_owned(),
            instance: DEFAULT_INSTANCE,
            latency_ms: DEFAULT_LATENCY_MS,
            adjust_sink: DEFAULT_ADJUST_SINK,
            host_id: String::new(),
            mdns_name: String::new(),
        }
    }
}

#[derive(Debug)]
struct Active {
    client: ClientHandle,
    info: Option<gst_audio::AudioInfo>,
    discont: bool,
    /// The pipeline latency the last PTS was computed with.
    latency_usec: i64,
    /// Keeps the output contiguous and on the Snapcast timeline (drift correction).
    aligner: Aligner,
    /// Buffers pushed, for periodic diagnostics.
    buffers: u64,
    /// The gain applied at the end of the last buffer (start of the next ramp).
    gain: f32,
}

pub struct SnapSrc {
    settings: Mutex<Settings>,
    state: Mutex<Option<Active>>,
    /// Set by `unlock()` to interrupt a blocking `create()`.
    flushing: watch::Sender<bool>,
    /// Pipeline latency in ns, from the LATENCY event. Not in `state`: `create()`
    /// holds that lock while waiting for data, and events must not block on it.
    pipeline_latency_ns: AtomicU64,
    /// The server's volume (percent) and mute, for the read-only properties.
    /// Atomics for the same reason as `pipeline_latency_ns`.
    volume: AtomicU32,
    muted: AtomicBool,
}

impl Default for SnapSrc {
    fn default() -> Self {
        Self {
            settings: Mutex::default(),
            state: Mutex::default(),
            flushing: watch::Sender::new(false),
            pipeline_latency_ns: AtomicU64::new(0),
            volume: AtomicU32::new(100),
            muted: AtomicBool::new(false),
        }
    }
}

impl SnapSrc {
    /// Applies the server's volume (see "Volume"), ramping from the previous gain.
    fn apply_volume(
        &self,
        active: &mut Active,
        frame: &client::PlayoutFrame,
        samples: &mut [f32],
        channels: usize,
    ) {
        let (from, to) = (active.gain, frame.gain());
        active.gain = to;
        if from == 1.0 && to == 1.0 {
            return;
        }
        let n = (samples.len() / channels.max(1)).max(1) as f32;
        for (i, f) in samples.chunks_mut(channels.max(1)).enumerate() {
            let g = from + (to - from) * (i as f32 + 1.0) / n;
            for s in f {
                *s *= g;
            }
        }
    }

    /// Keeps the read-only `volume`/`mute` properties up to date, independent of the
    /// audio: a C++ snapserver sends no audio to a muted client. The task ends with
    /// the client (its sender is dropped) or the element.
    fn watch_volume(&self, mut volume: watch::Receiver<(u16, bool)>) {
        let element = self.obj().downgrade();
        runtime().spawn(async move {
            loop {
                let (percent, muted) = *volume.borrow_and_update();
                let Some(obj) = element.upgrade() else { break };
                let imp = obj.imp();
                if imp.volume.swap(u32::from(percent), Ordering::Relaxed) != u32::from(percent) {
                    gst::info!(CAT, obj = obj, "Server volume {percent} %");
                    obj.notify("volume");
                }
                if imp.muted.swap(muted, Ordering::Relaxed) != muted {
                    gst::info!(CAT, obj = obj, "Server mute {muted}");
                    obj.notify("mute");
                }
                drop(obj);
                if volume.changed().await.is_err() {
                    break;
                }
            }
        });
    }

    /// The first audio sink downstream, following pads into and out of bins
    /// (e.g. the child of `autoaudiosink`).
    fn downstream_audio_sink(&self) -> Option<gst_audio::AudioBaseSink> {
        let mut pad = self.obj().static_pad("src")?.peer()?;
        for _ in 0..64 {
            if let Some(ghost) = pad.downcast_ref::<gst::GhostPad>() {
                // Into a bin: continue at the child's pad.
                pad = ghost.target()?;
                continue;
            }
            let Some(element) = pad.parent_element() else {
                // The inner side of a bin's ghost pad: out of the bin.
                let ghost = pad.downcast_ref::<gst::ProxyPad>()?.internal()?;
                pad = ghost.peer()?;
                continue;
            };
            if let Ok(sink) = element.clone().downcast::<gst_audio::AudioBaseSink>() {
                return Some(sink);
            }
            pad = element.src_pads().into_iter().next()?.peer()?;
        }
        None
    }

    /// Sets `slave-method=resample` on the downstream audio sink (see "Clock").
    fn adjust_sink(&self) {
        let Some(sink) = self.downstream_audio_sink() else {
            gst::debug!(
                CAT,
                imp = self,
                "No audio sink downstream, nothing to adjust"
            );
            return;
        };
        let current = sink.property_value("slave-method");
        let nick = current
            .get::<&glib::EnumValue>()
            .ok()
            .map(|v| v.nick().to_owned())
            .unwrap_or_default();
        if nick == "skew" {
            sink.set_property_from_str("slave-method", "resample");
            gst::info!(
                CAT,
                imp = self,
                "Set slave-method=resample on {} (was skew), so that it follows the \
                 Snapcast timeline (adjust-sink=false keeps it)",
                sink.name()
            );
        } else {
            gst::info!(
                CAT,
                imp = self,
                "Leaving {} at slave-method={nick}",
                sink.name()
            );
        }
    }
}

#[glib::object_subclass]
impl ObjectSubclass for SnapSrc {
    const NAME: &'static str = "GstSnapSrc";
    type Type = super::SnapSrc;
    type ParentType = gst_base::PushSrc;
}

impl ObjectImpl for SnapSrc {
    fn properties() -> &'static [glib::ParamSpec] {
        static PROPERTIES: LazyLock<Vec<glib::ParamSpec>> = LazyLock::new(|| {
            vec![
                glib::ParamSpecString::builder("host")
                    .nick("Host")
                    .blurb(
                        "Snapcast server host name or address; empty: find a server via \
                         mDNS (then its announced port is used, not the port property)",
                    )
                    .default_value(Some(DEFAULT_HOST))
                    .mutable_ready()
                    .build(),
                glib::ParamSpecUInt::builder("port")
                    .nick("Port")
                    .blurb("Snapcast server stream port")
                    .minimum(1)
                    .maximum(u16::MAX.into())
                    .default_value(DEFAULT_PORT)
                    .mutable_ready()
                    .build(),
                glib::ParamSpecString::builder("client-name")
                    .nick("Client name")
                    .blurb("Client name shown in Snapcast controllers")
                    .default_value(Some(DEFAULT_CLIENT_NAME))
                    .mutable_ready()
                    .build(),
                glib::ParamSpecUInt::builder("instance")
                    .nick("Instance")
                    .blurb(
                        "Instance number, to tell several clients on one host apart (the \
                         server sees <host-id>#<instance>); 0 = the lowest number no \
                         other snapsrc on this host uses",
                    )
                    .minimum(0)
                    .default_value(DEFAULT_INSTANCE)
                    .mutable_ready()
                    .build(),
                glib::ParamSpecInt::builder("latency-ms")
                    .nick("Latency")
                    .blurb("Additional local output latency in ms, played out that much earlier")
                    .minimum(0)
                    .maximum(10_000)
                    .default_value(DEFAULT_LATENCY_MS)
                    .mutable_ready()
                    .build(),
                glib::ParamSpecString::builder("mdns-name")
                    .nick("mDNS name")
                    .blurb(
                        "With an empty host: only use the server announced under this \
                         mDNS name (empty: the first one found)",
                    )
                    .default_value(Some(""))
                    .mutable_ready()
                    .build(),
                glib::ParamSpecString::builder("host-id")
                    .nick("Host id")
                    .blurb(
                        "Unique id of this client towards the server (empty: the MAC \
                         address), like snapclient's --hostID",
                    )
                    .default_value(Some(""))
                    .mutable_ready()
                    .build(),
                glib::ParamSpecUInt::builder("volume")
                    .nick("Volume")
                    .blurb("The server's volume for this client in percent (applied)")
                    .maximum(100)
                    .default_value(100)
                    .read_only()
                    .build(),
                glib::ParamSpecBoolean::builder("mute")
                    .nick("Mute")
                    .blurb("Muted by the server (applied)")
                    .default_value(false)
                    .read_only()
                    .build(),
                glib::ParamSpecBoolean::builder("adjust-sink")
                    .nick("Adjust sink")
                    .blurb(
                        "Set slave-method=resample on the downstream audio sink (if it is \
                         at skew), so that it follows the Snapcast timeline exactly",
                    )
                    .default_value(DEFAULT_ADJUST_SINK)
                    .mutable_ready()
                    .build(),
            ]
        });
        PROPERTIES.as_ref()
    }

    fn set_property(&self, _id: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
        let mut settings = self.settings.lock().unwrap();
        let string_or = |default: &str| {
            value
                .get::<Option<String>>()
                .expect("type checked upstream")
                .unwrap_or_else(|| default.to_owned())
        };
        match pspec.name() {
            "host" => settings.host = string_or(DEFAULT_HOST),
            "port" => settings.port = value.get().expect("type checked upstream"),
            "client-name" => settings.client_name = string_or(DEFAULT_CLIENT_NAME),
            "instance" => settings.instance = value.get().expect("type checked upstream"),
            "latency-ms" => settings.latency_ms = value.get().expect("type checked upstream"),
            "adjust-sink" => settings.adjust_sink = value.get().expect("type checked upstream"),
            "host-id" => settings.host_id = string_or(""),
            "mdns-name" => settings.mdns_name = string_or(""),
            _ => unimplemented!(),
        }
    }

    fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
        let settings = self.settings.lock().unwrap();
        match pspec.name() {
            "host" => settings.host.to_value(),
            "port" => settings.port.to_value(),
            "client-name" => settings.client_name.to_value(),
            "instance" => settings.instance.to_value(),
            "latency-ms" => settings.latency_ms.to_value(),
            "adjust-sink" => settings.adjust_sink.to_value(),
            "host-id" => settings.host_id.to_value(),
            "mdns-name" => settings.mdns_name.to_value(),
            "volume" => self.volume.load(Ordering::Relaxed).to_value(),
            "mute" => self.muted.load(Ordering::Relaxed).to_value(),
            _ => unimplemented!(),
        }
    }

    fn constructed(&self) {
        self.parent_constructed();
        let obj = self.obj();
        // Snapcast is inherently live; buffers are timestamped in TIME format.
        obj.set_live(true);
        obj.set_format(gst::Format::Time);
        // No PROVIDE_CLOCK: the audio sink's clock drives the pipeline ("Clock" above).
    }
}

impl GstObjectImpl for SnapSrc {}

impl ElementImpl for SnapSrc {
    fn metadata() -> Option<&'static gst::subclass::ElementMetadata> {
        static METADATA: LazyLock<gst::subclass::ElementMetadata> = LazyLock::new(|| {
            gst::subclass::ElementMetadata::new(
                "Snapcast Source",
                "Source/Audio/Network",
                "Receives synchronized PCM from a Snapcast server",
                "aanno",
            )
        });
        Some(&*METADATA)
    }

    fn pad_templates() -> &'static [gst::PadTemplate] {
        static PAD_TEMPLATES: LazyLock<Vec<gst::PadTemplate>> = LazyLock::new(|| {
            // snapcast-client hands out decoded audio as interleaved f32.
            let caps = gst_audio::AudioCapsBuilder::new_interleaved()
                .format(gst_audio::AudioFormat::F32le)
                .build();
            vec![
                gst::PadTemplate::new(
                    "src",
                    gst::PadDirection::Src,
                    gst::PadPresence::Always,
                    &caps,
                )
                .unwrap(),
            ]
        });
        PAD_TEMPLATES.as_ref()
    }
}

impl BaseSrcImpl for SnapSrc {
    fn start(&self) -> Result<(), gst::ErrorMessage> {
        let settings = self.settings.lock().unwrap().clone();
        gst::info!(CAT, imp = self, "Starting with {settings:?}");
        let options = ClientOptions {
            host: settings.host,
            port: settings.port as u16,
            client_name: settings.client_name,
            instance: settings.instance,
            latency_ms: settings.latency_ms,
            host_id: settings.host_id,
            mdns_name: settings.mdns_name,
            log_id: self.obj().name().to_string(),
        };
        let client = client::connect(&options)
            .map_err(|e| gst::error_msg!(gst::ResourceError::OpenRead, ["{e}"]))?;
        let (host, port) = client.server();
        gst::info!(
            CAT,
            imp = self,
            "Client instance {} for server {host}:{port}",
            client.instance()
        );
        self.watch_volume(client.volume());
        // Downstream elements change state before us, so the sink (and the child of
        // an autoaudiosink) exists by now.
        if settings.adjust_sink {
            self.adjust_sink();
        }
        self.flushing.send_replace(false);
        *self.state.lock().unwrap() = Some(Active {
            client,
            info: None,
            discont: true,
            latency_usec: 0,
            aligner: Aligner::default(),
            buffers: 0,
            gain: 1.0,
        });
        Ok(())
    }

    fn stop(&self) -> Result<(), gst::ErrorMessage> {
        // Dropping the handle stops the client.
        *self.state.lock().unwrap() = None;
        gst::info!(CAT, imp = self, "Stopped");
        Ok(())
    }

    fn query(&self, query: &mut gst::QueryRef) -> bool {
        if let gst::QueryViewMut::Latency(q) = query.view_mut() {
            // Buffers arrive ahead of their PTS (by about the Snapcast buffer),
            // so we add no latency, and the client queue lets us hold back
            // data for as long as downstream needs.
            q.set(true, gst::ClockTime::ZERO, gst::ClockTime::NONE);
            return true;
        }
        BaseSrcImplExt::parent_query(self, query)
    }

    /// The stream decides the format, and it is known only once audio arrives:
    /// `create()` then sets the caps. Before that, the default negotiation would
    /// fixate the template to `rate=1, channels=1`, and elements that configure
    /// themselves once (e.g. `wavenc`) could not follow the real format later
    /// (`snapsrc ! audioconvert ! wavenc` failed with not-negotiated).
    fn negotiate(&self) -> Result<(), gst::LoggableError> {
        // The pad's caps, not `state`: `create()` holds that lock while it waits.
        match self.obj().src_pad().current_caps() {
            // Renegotiation (e.g. downstream asked for RECONFIGURE): same format.
            Some(caps) => self
                .obj()
                .set_caps(&caps)
                .map_err(|_| gst::loggable_error!(CAT, "Failed to set caps {caps}")),
            None => Ok(()),
        }
    }

    fn event(&self, event: &gst::Event) -> bool {
        if let gst::EventView::Latency(e) = event.view() {
            gst::debug!(CAT, imp = self, "Pipeline latency {}", e.latency());
            self.pipeline_latency_ns
                .store(e.latency().nseconds(), Ordering::Relaxed);
        }
        self.parent_event(event)
    }

    fn unlock(&self) -> Result<(), gst::ErrorMessage> {
        self.flushing.send_replace(true);
        Ok(())
    }

    fn unlock_stop(&self) -> Result<(), gst::ErrorMessage> {
        self.flushing.send_replace(false);
        Ok(())
    }
}

impl PushSrcImpl for SnapSrc {
    fn create(
        &self,
        _buffer: Option<&mut gst::BufferRef>,
    ) -> Result<CreateSuccess, gst::FlowError> {
        let obj = self.obj();
        let mut state = self.state.lock().unwrap();
        let Some(active) = state.as_mut() else {
            return Err(gst::FlowError::Flushing);
        };
        let mut flushing = self.flushing.subscribe();

        loop {
            let frame = runtime().block_on(async {
                tokio::select! {
                    frame = active.client.recv() => Ok(frame),
                    _ = flushing.wait_for(|f| *f) => Err(gst::FlowError::Flushing),
                }
            })?;
            let Some(frame) = frame else {
                let reason = active.client.last_error().unwrap_or_default();
                gst::element_imp_error!(
                    self,
                    gst::ResourceError::Read,
                    ["Snapcast client terminated: {reason}"]
                );
                return Err(gst::FlowError::Error);
            };

            let now = now_usec();
            // The sink renders at PTS + latency = play-out time (see "Timing" and
            // "Late frames" in the module docs): late only once that has passed.
            if frame.playout_usec < now {
                gst::debug!(
                    CAT,
                    imp = self,
                    "Dropping frame {} µs too late",
                    now - frame.playout_usec
                );
                active.discont = true;
                continue;
            }
            let latency_usec = (self.pipeline_latency_ns.load(Ordering::Relaxed) / 1000) as i64;
            if latency_usec != active.latency_usec {
                // The PTS jumps by the difference: tell downstream.
                gst::debug!(
                    CAT,
                    imp = self,
                    "Pipeline latency {} -> {latency_usec} µs, play-out compensated",
                    active.latency_usec
                );
                active.latency_usec = latency_usec;
                active.discont = true;
            }
            let (Some(clock), Some(base_time)) = (obj.clock(), obj.base_time()) else {
                gst::element_imp_error!(self, gst::CoreError::Clock, ["No clock"]);
                return Err(gst::FlowError::Error);
            };
            // Signed: the PTS lies one latency before play-out, which may be before the
            // pipeline started.
            let ideal_ns = clock.time().nseconds() as i64
                + (frame.playout_usec - latency_usec - now) * 1000
                - base_time.nseconds() as i64;
            if ideal_ns < 0 {
                continue; // before pipeline start
            }

            let info = gst_audio::AudioInfo::builder(
                gst_audio::AudioFormat::F32le,
                frame.rate,
                frame.channels.into(),
            )
            .build()
            .map_err(|_| gst::FlowError::NotNegotiated)?;
            if active.info.as_ref() != Some(&info) {
                let caps = info.to_caps().map_err(|_| gst::FlowError::NotNegotiated)?;
                gst::info!(CAT, imp = self, "Stream format {caps}");
                obj.set_caps(&caps)
                    .map_err(|_| gst::FlowError::NotNegotiated)?;
                active.info = Some(info);
                active.discont = true;
            }

            let channels = usize::from(frame.channels.max(1));
            if active.discont {
                active.aligner.reset();
            }
            let place = active
                .aligner
                .place(ideal_ns, frame.samples.len() / channels, frame.rate);
            if place.resync {
                gst::debug!(
                    CAT,
                    imp = self,
                    "Resync to {} (off by {} µs)",
                    gst::ClockTime::from_nseconds(ideal_ns as u64),
                    (place.pts_ns - ideal_ns) / 1000
                );
                active.discont = true;
            } else if place.correction != 0 {
                gst::trace!(
                    CAT,
                    imp = self,
                    "Drift correction {:+} frames",
                    place.correction
                );
            }
            active.buffers += 1;
            if active.buffers % 256 == 0 {
                let (error_ns, corrected) = active.aligner.stats();
                gst::debug!(
                    CAT,
                    imp = self,
                    "Alignment: error {:+.0} µs, {corrected:+} frames corrected since resync",
                    error_ns / 1000.0
                );
            }
            let mut samples = align::stretch(&frame.samples, channels, place.correction);
            self.apply_volume(active, &frame, &mut samples, channels);
            let Ok(pts_ns) = u64::try_from(place.pts_ns) else {
                continue; // before pipeline start
            };
            let pts = gst::ClockTime::from_nseconds(pts_ns);

            let n_frames = (samples.len() / channels) as u64;
            let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
            let mut buffer = gst::Buffer::from_mut_slice(bytes);
            {
                let buffer = buffer.get_mut().unwrap();
                buffer.set_pts(pts);
                buffer.set_duration(
                    gst::ClockTime::SECOND.mul_div_floor(n_frames, frame.rate.into()),
                );
                if std::mem::take(&mut active.discont) {
                    buffer.set_flags(gst::BufferFlags::DISCONT);
                }
            }
            gst::trace!(CAT, imp = self, "Pushing {n_frames} frames, pts {pts}");
            return Ok(CreateSuccess::NewBuffer(buffer));
        }
    }
}
