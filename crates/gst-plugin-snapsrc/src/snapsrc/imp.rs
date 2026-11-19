//! `snapsrc` implementation.
//!
//! # Timing
//!
//! `snapcast-engine` hands out frames with a *local play-out time* in the
//! monotonic Snapcast clock domain (server timestamp corrected by Snapcast's
//! time sync plus the configured buffer). We turn that into a PTS by sampling
//! the pipeline clock:
//!
//! ```text
//! pts = clock.time() + (playout_usec - now_usec()) - base_time
//! ```
//!
//! so the audio sink, synchronizing on the pipeline clock, plays each sample
//! at the right moment. Network arrival time is never used.
//!
//! The element provides a monotonic system clock (the Snapcast clock domain)
//! so that audio sinks slave their device clock to it, rather than the source
//! drifting against an audio device clock. Buffers arrive roughly `buffer-ms`
//! ahead of their PTS.
//!
//! # Late frames
//!
//! Downstream decides when `create()` runs: an audio sink blocks the streaming
//! thread until its ring buffer has room, so a frame may leave the client queue
//! only shortly before (or even after) its play-out time. That is fine: a sink
//! renders a buffer at `PTS + pipeline latency`, and the pipeline announces that
//! latency with a LATENCY event, which sinks forward upstream. A frame is
//! therefore only dropped (and the next buffer marked DISCONT) when
//! `playout + latency < now`, e.g. frames queued while PAUSED.
//!
//! Checking `playout < now` alone dropped about every fourth 96 ms block behind
//! `pulsesink` (130 ms latency, small ring buffer): audible gaps (2026-11-19).
//!
//! Open question (L13 in `docs/claude/memory.md`): the PTS maps play-out onto
//! the sink's *sync point*, so the sound may come out one pipeline latency late.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};

use gst::glib;
use gst::prelude::*;
use gst::subclass::prelude::*;
use gst_base::prelude::*;
use gst_base::subclass::base_src::CreateSuccess;
use gst_base::subclass::prelude::*;
use snapcast_engine::client::{self, ClientHandle, ClientOptions};
use snapcast_engine::{now_usec, runtime};
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
const DEFAULT_INSTANCE: u32 = 1;
const DEFAULT_LATENCY_MS: i32 = 0;

#[derive(Debug, Clone)]
struct Settings {
    host: String,
    port: u32,
    client_name: String,
    instance: u32,
    latency_ms: i32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            host: DEFAULT_HOST.to_owned(),
            port: DEFAULT_PORT,
            client_name: DEFAULT_CLIENT_NAME.to_owned(),
            instance: DEFAULT_INSTANCE,
            latency_ms: DEFAULT_LATENCY_MS,
        }
    }
}

#[derive(Debug)]
struct Active {
    client: ClientHandle,
    info: Option<gst_audio::AudioInfo>,
    discont: bool,
}

pub struct SnapSrc {
    settings: Mutex<Settings>,
    state: Mutex<Option<Active>>,
    /// Set by `unlock()` to interrupt a blocking `create()`.
    flushing: watch::Sender<bool>,
    clock: gst::Clock,
    /// Pipeline latency in ns, from the LATENCY event. Not in `state`: `create()`
    /// holds that lock while waiting for data, and events must not block on it.
    pipeline_latency_ns: AtomicU64,
}

impl Default for SnapSrc {
    fn default() -> Self {
        let clock = glib::Object::builder::<gst::SystemClock>()
            .property("clock-type", gst::ClockType::Monotonic)
            .build()
            .upcast();
        Self {
            settings: Mutex::default(),
            state: Mutex::default(),
            flushing: watch::Sender::new(false),
            clock,
            pipeline_latency_ns: AtomicU64::new(0),
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
                    .blurb("Snapcast server host name or address")
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
                    .blurb("Instance id, to distinguish several clients on one host")
                    .minimum(1)
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
            _ => unimplemented!(),
        }
    }

    fn constructed(&self) {
        self.parent_constructed();
        let obj = self.obj();
        // Snapcast is inherently live; buffers are timestamped in TIME format.
        obj.set_live(true);
        obj.set_format(gst::Format::Time);
        obj.set_element_flags(gst::ElementFlags::PROVIDE_CLOCK);
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

    fn provide_clock(&self) -> Option<gst::Clock> {
        Some(self.clock.clone())
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
        };
        let client = client::connect(&options)
            .map_err(|e| gst::error_msg!(gst::ResourceError::OpenRead, ["{e}"]))?;
        self.flushing.send_replace(false);
        *self.state.lock().unwrap() = Some(Active {
            client,
            info: None,
            discont: true,
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
            // A frame is late only once the sink can no longer render it: it renders
            // at PTS + pipeline latency (see "Late frames" in the module docs).
            let latency_usec = (self.pipeline_latency_ns.load(Ordering::Relaxed) / 1000) as i64;
            if frame.playout_usec + latency_usec < now {
                gst::debug!(
                    CAT,
                    imp = self,
                    "Dropping frame {} µs too late (pipeline latency {latency_usec} µs)",
                    now - frame.playout_usec
                );
                active.discont = true;
                continue;
            }
            let (Some(clock), Some(base_time)) = (obj.clock(), obj.base_time()) else {
                gst::element_imp_error!(self, gst::CoreError::Clock, ["No clock"]);
                return Err(gst::FlowError::Error);
            };
            // Signed: within the latency window, play-out may already be (just) past.
            let pts_ns = clock.time().nseconds() as i64 + (frame.playout_usec - now) * 1000
                - base_time.nseconds() as i64;
            let Ok(pts_ns) = u64::try_from(pts_ns) else {
                continue; // before pipeline start
            };
            let pts = gst::ClockTime::from_nseconds(pts_ns);

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

            let n_frames = frame.samples.len() as u64 / u64::from(frame.channels.max(1));
            let bytes: Vec<u8> = frame.samples.iter().flat_map(|s| s.to_le_bytes()).collect();
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
