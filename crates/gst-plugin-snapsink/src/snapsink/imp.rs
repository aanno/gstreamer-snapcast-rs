//! `snapsink` implementation.
//!
//! # Timing
//!
//! A Snapcast chunk timestamp is the server time at which the audio "happens";
//! clients play it `buffer-ms` later. For each buffer we compute the clock time
//! at which GStreamer would render it (`base_time + running_time + latency`)
//! and translate that into the Snapcast clock domain by sampling both clocks:
//!
//! ```text
//! server_ts = now_usec() + (render_clock_time - clock.time())
//! ```
//!
//! This works for any pipeline clock (system, audio device, network...).
//! Without a clock (e.g. `sync=false` outside PLAYING), timestamps fall back to
//! sample counting anchored at the first buffer.
//!
//! # Pacing
//!
//! `render` hands a chunk to the server only once its server timestamp is at
//! most [`MAX_AHEAD_USEC`] in the future, and waits (interruptibly) otherwise.
//! With `sync=true` (the default) basesink already renders on time, so this
//! never waits. With `sync=false` and a non-live source (e.g. a file) nothing
//! else paces the pipeline; before 2026-11-19 that flooded the clients with
//! the whole file at once (L7).
//!
//! The stream is acquired in `set_caps`, because snapcast-rs fixes a stream's
//! sample format when the server starts.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use gst::glib;
use gst::prelude::*;
use gst::subclass::prelude::*;
use gst_base::prelude::*;
use gst_base::subclass::prelude::*;
use snapcast_engine::server::{self, ServerOptions, StreamHandle, StreamOptions};
use snapcast_engine::{PcmFormat, now_usec};

static CAT: LazyLock<gst::DebugCategory> = LazyLock::new(|| {
    gst::DebugCategory::new(
        "snapsink",
        gst::DebugColorFlags::empty(),
        Some("Snapcast server sink"),
    )
});

const DEFAULT_STREAM: &str = "default";
// Snapcast's default stream port (snapcast_proto::DEFAULT_STREAM_PORT).
const DEFAULT_PORT: u32 = 1704;
const DEFAULT_BUFFER_MS: u32 = 1000;
const DEFAULT_MDNS_NAME: &str = "gst-snapsink";
const CODECS: &[&str] = &["pcm", "flac"];
/// Chunks are sent at most this far ahead of their server timestamp ("Pacing").
const MAX_AHEAD_USEC: i64 = 200_000;

#[derive(Debug, Clone)]
struct Settings {
    stream: String,
    port: u32,
    codec: String,
    buffer_ms: u32,
    mdns_name: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            stream: DEFAULT_STREAM.to_owned(),
            port: DEFAULT_PORT,
            codec: server::DEFAULT_CODEC.to_owned(),
            buffer_ms: DEFAULT_BUFFER_MS,
            mdns_name: DEFAULT_MDNS_NAME.to_owned(),
        }
    }
}

#[derive(Debug)]
struct Fallback {
    start_usec: i64,
    frames: u64,
}

#[derive(Debug)]
struct Active {
    info: gst_audio::AudioInfo,
    stream: StreamHandle,
    /// Timestamping without a clock: anchor + sample count.
    fallback: Option<Fallback>,
}

#[derive(Default)]
pub struct SnapSink {
    settings: Mutex<Settings>,
    state: Mutex<Option<Active>>,
    /// Set by `unlock()` to interrupt the pacing wait in `render()`.
    flushing: AtomicBool,
}

impl SnapSink {
    /// Snapcast server time (µs) of the first sample in `buffer`.
    fn server_timestamp(&self, buffer: &gst::BufferRef, active: &mut Active) -> i64 {
        let obj = self.obj();
        let frames = (buffer.size() / active.info.bpf() as usize) as u64;

        let render_time = obj.clock().and_then(|clock| {
            let segment = obj.segment();
            let segment = segment.downcast_ref::<gst::ClockTime>()?;
            let running_time = segment.to_running_time(buffer.pts()?)?;
            let render = obj.base_time()? + running_time + obj.latency();
            Some((clock, render))
        });

        if let Some((clock, render)) = render_time {
            active.fallback = None;
            let ahead_ns = render.nseconds() as i64 - clock.time().nseconds() as i64;
            return now_usec() + ahead_ns / 1000;
        }

        let rate = u64::from(active.info.rate());
        let fb = active.fallback.get_or_insert_with(|| {
            gst::debug!(
                CAT,
                imp = self,
                "No clock/PTS, timestamping by sample count"
            );
            Fallback {
                start_usec: now_usec(),
                frames: 0,
            }
        });
        let ts = fb.start_usec + (fb.frames * 1_000_000 / rate) as i64;
        fb.frames += frames;
        ts
    }
}

#[glib::object_subclass]
impl ObjectSubclass for SnapSink {
    const NAME: &'static str = "GstSnapSink";
    type Type = super::SnapSink;
    type ParentType = gst_base::BaseSink;
}

impl ObjectImpl for SnapSink {
    fn properties() -> &'static [glib::ParamSpec] {
        static PROPERTIES: LazyLock<Vec<glib::ParamSpec>> = LazyLock::new(|| {
            vec![
                glib::ParamSpecString::builder("stream")
                    .nick("Stream")
                    .blurb("Name of the Snapcast stream this sink feeds")
                    .default_value(Some(DEFAULT_STREAM))
                    .mutable_ready()
                    .build(),
                glib::ParamSpecUInt::builder("port")
                    .nick("Port")
                    .blurb(
                        "TCP port of the embedded Snapcast server, shared by all snapsinks \
                         using the same port (0 = ephemeral, never shared)",
                    )
                    .maximum(u16::MAX.into())
                    .default_value(DEFAULT_PORT)
                    .mutable_ready()
                    .build(),
                glib::ParamSpecString::builder("codec")
                    .nick("Codec")
                    .blurb("Snapcast codec of the stream: pcm or flac")
                    .default_value(Some(server::DEFAULT_CODEC))
                    .mutable_ready()
                    .build(),
                glib::ParamSpecUInt::builder("buffer-ms")
                    .nick("Buffer")
                    .blurb(
                        "End-to-end buffer in ms announced to clients \
                         (set by the first snapsink of a server)",
                    )
                    .minimum(20)
                    .maximum(60_000)
                    .default_value(DEFAULT_BUFFER_MS)
                    .mutable_ready()
                    .build(),
                glib::ParamSpecString::builder("mdns-name")
                    .nick("mDNS name")
                    .blurb(
                        "Announce the server via mDNS (_snapcast._tcp) under this name, so \
                         clients find it without a host name; empty: don't. Never for \
                         port=0 (set by the first snapsink of a server)",
                    )
                    .default_value(Some(DEFAULT_MDNS_NAME))
                    .mutable_ready()
                    .build(),
                glib::ParamSpecUInt::builder("bound-port")
                    .nick("Bound port")
                    .blurb("Port the server actually listens on (0 while not running)")
                    .maximum(u16::MAX.into())
                    .read_only()
                    .build(),
            ]
        });
        PROPERTIES.as_ref()
    }

    fn set_property(&self, _id: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
        let mut settings = self.settings.lock().unwrap();
        match pspec.name() {
            "stream" => {
                settings.stream = value
                    .get::<Option<String>>()
                    .expect("type checked upstream")
                    .unwrap_or_else(|| DEFAULT_STREAM.to_owned());
            }
            "port" => settings.port = value.get().expect("type checked upstream"),
            "codec" => {
                let codec = value
                    .get::<Option<String>>()
                    .expect("type checked upstream")
                    .unwrap_or_else(|| server::DEFAULT_CODEC.to_owned());
                if CODECS.contains(&codec.as_str()) {
                    settings.codec = codec;
                } else {
                    gst::warning!(
                        CAT,
                        imp = self,
                        "Unsupported codec '{codec}', keeping '{}'",
                        settings.codec
                    );
                }
            }
            "buffer-ms" => settings.buffer_ms = value.get().expect("type checked upstream"),
            "mdns-name" => {
                settings.mdns_name = value
                    .get::<Option<String>>()
                    .expect("type checked upstream")
                    .unwrap_or_default();
            }
            _ => unimplemented!(),
        }
    }

    fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
        match pspec.name() {
            "bound-port" => {
                let state = self.state.lock().unwrap();
                state
                    .as_ref()
                    .map_or(0u32, |a| a.stream.port().into())
                    .to_value()
            }
            name => {
                let settings = self.settings.lock().unwrap();
                match name {
                    "stream" => settings.stream.to_value(),
                    "port" => settings.port.to_value(),
                    "codec" => settings.codec.to_value(),
                    "buffer-ms" => settings.buffer_ms.to_value(),
                    "mdns-name" => settings.mdns_name.to_value(),
                    _ => unimplemented!(),
                }
            }
        }
    }
}

impl GstObjectImpl for SnapSink {}

impl ElementImpl for SnapSink {
    fn metadata() -> Option<&'static gst::subclass::ElementMetadata> {
        static METADATA: LazyLock<gst::subclass::ElementMetadata> = LazyLock::new(|| {
            gst::subclass::ElementMetadata::new(
                "Snapcast Sink",
                "Sink/Audio/Network",
                "Feeds raw PCM into a stream of an embedded Snapcast server",
                "aanno",
            )
        });
        Some(&*METADATA)
    }

    fn pad_templates() -> &'static [gst::PadTemplate] {
        static PAD_TEMPLATES: LazyLock<Vec<gst::PadTemplate>> = LazyLock::new(|| {
            // Formats Snapcast PCM/FLAC handle without conversion.
            // TODO: S24_32LE (Snapcast's 24 bit is S24 in 32 bit containers).
            let caps = gst_audio::AudioCapsBuilder::new_interleaved()
                .format_list([gst_audio::AudioFormat::S16le, gst_audio::AudioFormat::S32le])
                .build();
            vec![
                gst::PadTemplate::new(
                    "sink",
                    gst::PadDirection::Sink,
                    gst::PadPresence::Always,
                    &caps,
                )
                .unwrap(),
            ]
        });
        PAD_TEMPLATES.as_ref()
    }
}

impl BaseSinkImpl for SnapSink {
    fn start(&self) -> Result<(), gst::ErrorMessage> {
        let settings = self.settings.lock().unwrap().clone();
        gst::info!(CAT, imp = self, "Starting with {settings:?}");
        Ok(())
    }

    fn stop(&self) -> Result<(), gst::ErrorMessage> {
        // Dropping the handle releases the stream (and the server, if last).
        *self.state.lock().unwrap() = None;
        gst::info!(CAT, imp = self, "Stopped");
        Ok(())
    }

    fn unlock(&self) -> Result<(), gst::ErrorMessage> {
        self.flushing.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn unlock_stop(&self) -> Result<(), gst::ErrorMessage> {
        self.flushing.store(false, Ordering::SeqCst);
        Ok(())
    }

    fn set_caps(&self, caps: &gst::Caps) -> Result<(), gst::LoggableError> {
        let info = gst_audio::AudioInfo::from_caps(caps)
            .map_err(|_| gst::loggable_error!(CAT, "Failed to parse caps {caps:?}"))?;
        let format = PcmFormat {
            rate: info.rate(),
            bits: info.format_info().width() as u16,
            channels: info.channels() as u16,
        };

        let mut state = self.state.lock().unwrap();
        if let Some(active) = state.as_mut() {
            if active.info == info {
                return Ok(());
            }
            // The previous format is baked into the running stream.
            *state = None;
        }

        let settings = self.settings.lock().unwrap().clone();
        let server_opts = ServerOptions {
            port: settings.port as u16,
            buffer_ms: settings.buffer_ms,
            mdns_name: Some(settings.mdns_name.clone()).filter(|n| !n.is_empty()),
        };
        let stream_opts = StreamOptions {
            name: settings.stream.clone(),
            codec: settings.codec.clone(),
            format,
            log_id: self.obj().name().to_string(),
        };
        let stream = server::acquire_stream(&server_opts, &stream_opts).map_err(|e| {
            gst::element_imp_error!(self, gst::ResourceError::OpenWrite, ["{e}"]);
            gst::loggable_error!(CAT, "Failed to acquire stream: {e}")
        })?;
        gst::info!(
            CAT,
            imp = self,
            "Feeding stream '{}' ({} {format}) on port {}",
            stream.name(),
            settings.codec,
            stream.port()
        );
        *state = Some(Active {
            info,
            stream,
            fallback: None,
        });
        drop(state);
        self.obj().notify("bound-port");
        Ok(())
    }

    fn render(&self, buffer: &gst::Buffer) -> Result<gst::FlowSuccess, gst::FlowError> {
        let mut state = self.state.lock().unwrap();
        let Some(active) = state.as_mut() else {
            gst::element_imp_error!(self, gst::CoreError::Negotiation, ["No caps set"]);
            return Err(gst::FlowError::NotNegotiated);
        };

        let timestamp_usec = self.server_timestamp(buffer, active);
        // Pacing (see the module docs): wait in small steps, so that unlock() can
        // interrupt us within 10 ms.
        loop {
            let ahead = timestamp_usec - now_usec() - MAX_AHEAD_USEC;
            if ahead <= 0 {
                break;
            }
            if self.flushing.load(Ordering::SeqCst) {
                return Err(gst::FlowError::Flushing);
            }
            std::thread::sleep(Duration::from_micros(ahead.min(10_000) as u64));
        }
        let map = buffer.map_readable().map_err(|_| {
            gst::element_imp_error!(self, gst::CoreError::Failed, ["Failed to map buffer"]);
            gst::FlowError::Error
        })?;
        gst::trace!(
            CAT,
            imp = self,
            "Sending {} bytes, pts {:?} -> server ts {timestamp_usec}",
            map.size(),
            buffer.pts()
        );
        active
            .stream
            .send_pcm(map.as_slice().to_vec(), timestamp_usec)
            .map_err(|e| {
                gst::element_imp_error!(self, gst::ResourceError::Write, ["{e}"]);
                gst::FlowError::Error
            })?;
        Ok(gst::FlowSuccess::Ok)
    }
}
