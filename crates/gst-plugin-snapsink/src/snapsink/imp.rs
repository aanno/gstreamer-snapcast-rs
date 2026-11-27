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
//!
//! # Control API and metadata
//!
//! `control-port` / `http-port` make the (shared) server offer the Snapcast
//! JSON-RPC control API, `snapweb-dir` serves Snapweb with it; all are set by the
//! first snapsink of a server. Tag events become the stream's metadata (title,
//! artist, album, duration), shown by controllers. `controls` announces which player
//! commands the stream accepts (Snapweb shows those buttons); each command arrives as
//! an element message `snapcast-stream-control` (fields `stream`, `command`,
//! `params`) on the bus, and the application acts on it. The playback status follows
//! the element's state. These stream properties are republished only when they
//! change, not for every tag event (decoders send bitrate updates).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use gst::glib;
use gst::prelude::*;
use gst::subclass::prelude::*;
use gst_base::prelude::*;
use gst_base::subclass::prelude::*;
use gst_snapcast_engine::server::{
    self, ControlOptions, PlaybackStatus, ServerOptions, StreamControls, StreamHandle,
    StreamMetadata, StreamOptions, StreamProperties,
};
use gst_snapcast_engine::{PcmFormat, now_usec};

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

/// Where the Snapcast server runs (property `mode`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, glib::Enum)]
#[enum_type(name = "GstSnapSinkMode")]
pub enum Mode {
    /// The user's shared server daemon if one runs, else embedded.
    #[default]
    #[enum_value(name = "Shared server if available, else embedded", nick = "auto")]
    Auto,
    #[enum_value(
        name = "Shared server (gst-snapcast-daemon), or an error",
        nick = "shared"
    )]
    Shared,
    #[enum_value(name = "Embedded in this process", nick = "embedded")]
    Embedded,
}

impl Mode {
    fn to_engine(self) -> server::Mode {
        match self {
            Self::Auto => server::Mode::Auto,
            Self::Shared => server::Mode::Shared,
            Self::Embedded => server::Mode::Embedded,
        }
    }
}

/// Player commands the stream accepts from controllers (property `controls`).
#[glib::flags(name = "GstSnapSinkControls")]
pub enum Controls {
    #[flags_value(name = "Play", nick = "play")]
    PLAY = 1 << 0,
    #[flags_value(name = "Pause (and play/pause)", nick = "pause")]
    PAUSE = 1 << 1,
    #[flags_value(name = "Next track", nick = "next")]
    NEXT = 1 << 2,
    #[flags_value(name = "Previous track", nick = "previous")]
    PREVIOUS = 1 << 3,
    #[flags_value(name = "Seek", nick = "seek")]
    SEEK = 1 << 4,
}

impl Controls {
    fn to_stream_controls(self) -> StreamControls {
        StreamControls {
            play: self.contains(Self::PLAY),
            pause: self.contains(Self::PAUSE),
            next: self.contains(Self::NEXT),
            previous: self.contains(Self::PREVIOUS),
            seek: self.contains(Self::SEEK),
        }
    }
}

#[derive(Debug, Clone)]
struct Settings {
    stream: String,
    port: u32,
    codec: String,
    buffer_ms: u32,
    mdns_name: String,
    control_port: u32,
    http_port: u32,
    snapweb_dir: String,
    controls: Controls,
    mode: Mode,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            stream: DEFAULT_STREAM.to_owned(),
            port: DEFAULT_PORT,
            codec: server::DEFAULT_CODEC.to_owned(),
            buffer_ms: DEFAULT_BUFFER_MS,
            mdns_name: DEFAULT_MDNS_NAME.to_owned(),
            control_port: 0,
            http_port: 0,
            snapweb_dir: String::new(),
            controls: Controls::empty(),
            mode: Mode::default(),
        }
    }
}

impl Settings {
    fn control_options(&self) -> ControlOptions {
        let port = |p: u32| (p != 0).then_some(p as u16);
        ControlOptions {
            tcp_port: port(self.control_port),
            http_port: port(self.http_port),
            snapweb_dir: Some(self.snapweb_dir.as_str())
                .filter(|d| !d.is_empty())
                .map(Into::into),
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
    /// Stream properties last published.
    published: Option<StreamProperties>,
}

#[derive(Default)]
pub struct SnapSink {
    settings: Mutex<Settings>,
    state: Mutex<Option<Active>>,
    /// All tags received so far (merged), also before the stream is acquired.
    tags: Mutex<Option<gst::TagList>>,
    /// Playback status reported to controllers, from the element's state (and
    /// `stopped` after EOS).
    playback: Mutex<Option<PlaybackStatus>>,
    /// Playback position reported to controllers, in whole seconds of stream time
    /// (see `track_position`).
    position_secs: Mutex<Option<u64>>,
    /// The track length from a duration query upstream, for streams whose tags
    /// don't carry it (most files: demuxers answer the query, few send the tag).
    queried_duration_secs: Mutex<Option<f64>>,
    /// Set by `unlock()` to interrupt the pacing wait in `render()`.
    flushing: AtomicBool,
}

/// The Snapcast metadata in `tags`.
fn metadata_from_tags(tags: &gst::TagListRef) -> StreamMetadata {
    StreamMetadata {
        title: tags.get::<gst::tags::Title>().map(|v| v.get().to_owned()),
        artist: tags
            .iter_tag::<gst::tags::Artist>()
            .map(|v| v.get().to_owned())
            .collect(),
        album: tags.get::<gst::tags::Album>().map(|v| v.get().to_owned()),
        duration_secs: tags
            .get::<gst::tags::Duration>()
            .map(|v| v.get().nseconds() as f64 / 1e9),
    }
}

impl SnapSink {
    /// Publishes the stream properties (metadata from the tags received so far,
    /// controls, playback status), if they changed. Lock order: state, then
    /// settings, then tags.
    fn publish_properties(&self, active: &mut Active) {
        let mut metadata = self
            .tags
            .lock()
            .unwrap()
            .as_deref()
            .map(metadata_from_tags)
            .unwrap_or_default();
        if metadata.duration_secs.is_none() {
            metadata.duration_secs = *self.queried_duration_secs.lock().unwrap();
        }
        let properties = StreamProperties {
            metadata,
            controls: self.settings.lock().unwrap().controls.to_stream_controls(),
            playback: *self.playback.lock().unwrap(),
            position_secs: self.position_secs.lock().unwrap().map(|s| s as f64),
        };
        if active.published.as_ref() == Some(&properties) {
            return;
        }
        gst::debug!(
            CAT,
            imp = self,
            "Publishing stream properties {properties:?}"
        );
        match active.stream.set_properties(&properties) {
            Ok(()) => active.published = Some(properties),
            Err(e) => gst::warning!(CAT, imp = self, "Cannot publish stream properties: {e}"),
        }
    }

    /// Follows the stream position of `buffer` for the `position` property (DLNA
    /// renderers show it, `docs/dlna.md`), in whole seconds, so that controllers
    /// get one update per second. It is what is being sent now, which listeners hear
    /// one Snapcast buffer later, so that much is subtracted. Rendering after EOS (a
    /// seek back, a restart) also means playing again. Returns whether anything
    /// changed.
    fn track_position(&self, buffer: &gst::BufferRef) -> bool {
        let mut changed = false;
        {
            let mut playback = self.playback.lock().unwrap();
            if *playback == Some(PlaybackStatus::Stopped) {
                *playback = Some(PlaybackStatus::Playing);
                changed = true;
            }
        }
        let segment = self.obj().segment();
        let stream_time = segment
            .downcast_ref::<gst::ClockTime>()
            .and_then(|s| s.to_stream_time(buffer.pts()?));
        if let Some(stream_time) = stream_time {
            let buffer_ms = u64::from(self.settings.lock().unwrap().buffer_ms);
            let heard = stream_time.mseconds().saturating_sub(buffer_ms) / 1000;
            let mut position = self.position_secs.lock().unwrap();
            if *position != Some(heard) {
                *position = Some(heard);
                changed = true;
                // Once a second: the length (it may change with the next track).
                let duration = self
                    .obj()
                    .static_pad("sink")
                    .and_then(|pad| pad.peer_query_duration::<gst::ClockTime>())
                    .map(|d| d.nseconds() as f64 / 1e9);
                *self.queried_duration_secs.lock().unwrap() = duration;
            }
        }
        changed
    }

    /// Republishes the stream properties if a stream is active.
    fn republish(&self) {
        if let Some(active) = self.state.lock().unwrap().as_mut() {
            self.publish_properties(active);
        }
    }

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
                glib::ParamSpecUInt::builder("control-port")
                    .nick("Control port")
                    .blurb(
                        "TCP port for the Snapcast JSON-RPC control API (Snapcast's default \
                         is 1705; 0 = off). No authentication: anyone who can reach it \
                         controls the server (set by the first snapsink of a server)",
                    )
                    .maximum(u16::MAX.into())
                    .default_value(0)
                    .mutable_ready()
                    .build(),
                glib::ParamSpecUInt::builder("http-port")
                    .nick("HTTP port")
                    .blurb(
                        "HTTP port for the JSON-RPC control API over HTTP and WebSocket, and \
                         for snapweb-dir (Snapcast's default is 1780; 0 = off). No \
                         authentication (set by the first snapsink of a server)",
                    )
                    .maximum(u16::MAX.into())
                    .default_value(0)
                    .mutable_ready()
                    .build(),
                glib::ParamSpecString::builder("snapweb-dir")
                    .nick("Snapweb directory")
                    .blurb(
                        "Directory with Snapweb's files, served on http-port (e.g. \
                         ~/.local/share/snapweb from scripts/fetch-snapweb.sh); empty: none \
                         (set by the first snapsink of a server)",
                    )
                    .mutable_ready()
                    .build(),
                glib::ParamSpecFlags::builder::<Controls>("controls")
                    .nick("Controls")
                    .blurb(
                        "Player commands this stream accepts from controllers (Snapweb shows \
                         those buttons). Each command is posted as element message \
                         'snapcast-stream-control' (fields stream, command, params as JSON) \
                         for the application to act on",
                    )
                    .mutable_playing()
                    .build(),
                glib::ParamSpecEnum::builder::<Mode>("mode")
                    .nick("Mode")
                    .blurb(
                        "Where the Snapcast server runs: in the user's shared server daemon \
                         (gst-snapcast-daemon), so that pipelines in several processes share \
                         one server, or embedded in this process. auto: shared if the daemon \
                         runs, else embedded. port=0 is always embedded",
                    )
                    .default_value(Mode::default())
                    .mutable_ready()
                    .build(),
                glib::ParamSpecEnum::builder::<Mode>("active-mode")
                    .nick("Active mode")
                    .blurb(
                        "Where the server of the running stream is: shared or embedded \
                         (auto while no stream runs)",
                    )
                    .default_value(Mode::Auto)
                    .read_only()
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
        if pspec.name() == "controls" {
            self.settings.lock().unwrap().controls = value.get().expect("type checked upstream");
            // Without holding the settings lock (see publish_properties).
            self.republish();
            return;
        }
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
            "mode" => settings.mode = value.get().expect("type checked upstream"),
            "control-port" => settings.control_port = value.get().expect("type checked upstream"),
            "http-port" => settings.http_port = value.get().expect("type checked upstream"),
            "snapweb-dir" => {
                settings.snapweb_dir = value
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
            "active-mode" => {
                let state = self.state.lock().unwrap();
                match state.as_ref() {
                    None => Mode::Auto,
                    Some(a) if a.stream.is_shared() => Mode::Shared,
                    Some(_) => Mode::Embedded,
                }
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
                    "control-port" => settings.control_port.to_value(),
                    "http-port" => settings.http_port.to_value(),
                    "snapweb-dir" => settings.snapweb_dir.to_value(),
                    "controls" => settings.controls.to_value(),
                    "mode" => settings.mode.to_value(),
                    _ => unimplemented!(),
                }
            }
        }
    }
}

impl GstObjectImpl for SnapSink {}

impl ElementImpl for SnapSink {
    fn change_state(
        &self,
        transition: gst::StateChange,
    ) -> Result<gst::StateChangeSuccess, gst::StateChangeError> {
        let ret = self.parent_change_state(transition)?;
        let playback = match transition {
            gst::StateChange::ReadyToPaused | gst::StateChange::PlayingToPaused => {
                Some(PlaybackStatus::Paused)
            }
            gst::StateChange::PausedToPlaying => Some(PlaybackStatus::Playing),
            gst::StateChange::PausedToReady => None,
            _ => return Ok(ret),
        };
        {
            let mut current = self.playback.lock().unwrap();
            // Stopped (EOS) stays stopped when the pipeline is merely paused.
            if !(*current == Some(PlaybackStatus::Stopped)
                && playback == Some(PlaybackStatus::Paused))
            {
                *current = playback;
            }
        }
        if playback.is_none() {
            *self.position_secs.lock().unwrap() = None;
        }
        self.republish();
        Ok(ret)
    }

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
        *self.tags.lock().unwrap() = None;
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

    fn event(&self, event: gst::Event) -> bool {
        if let gst::EventView::Eos(_) = event.view() {
            // The end of the track: controllers (and DLNA renderers following the
            // session) see `stopped`.
            *self.playback.lock().unwrap() = Some(PlaybackStatus::Stopped);
            self.republish();
        }
        if let gst::EventView::Tag(tag) = event.view() {
            {
                let mut tags = self.tags.lock().unwrap();
                let merged = match tags.as_ref() {
                    Some(old) => old.merge(tag.tag(), gst::TagMergeMode::Replace),
                    None => tag.tag_owned(),
                };
                *tags = Some(merged);
            }
            self.republish();
        }
        self.parent_event(event)
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
            control: settings.control_options(),
            mode: settings.mode.to_engine(),
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
            "Feeding stream '{}' ({} {format}) on port {} ({}); control: TCP {:?}, HTTP {:?}",
            stream.name(),
            settings.codec,
            stream.port(),
            if stream.is_shared() {
                "shared server"
            } else {
                "embedded server"
            },
            stream.control_tcp_port(),
            stream.control_http_port()
        );
        // Only the first sink of a server decides its options.
        let effective = stream.server_options();
        if effective.buffer_ms != server_opts.buffer_ms
            || effective.mdns_name != server_opts.mdns_name
            || effective.control != server_opts.control
        {
            gst::warning!(
                CAT,
                imp = self,
                "The server on port {} was started with other options; in effect: \
                 buffer-ms {}, mdns-name {:?}, control {:?}",
                stream.port(),
                effective.buffer_ms,
                effective.mdns_name,
                effective.control
            );
        }
        let element = self.obj().downgrade();
        let stream_name = settings.stream.clone();
        stream.on_command(move |command| {
            let Some(element) = element.upgrade() else {
                return;
            };
            gst::info!(
                CAT,
                obj = element,
                "Stream command '{}' {}",
                command.command,
                command.params_json
            );
            let structure = gst::Structure::builder("snapcast-stream-control")
                .field("stream", &stream_name)
                .field("command", &command.command)
                .field("params", &command.params_json)
                .build();
            let _ = element.post_message(
                gst::message::Element::builder(structure)
                    .src(&element)
                    .build(),
            );
        });
        let active = state.insert(Active {
            info,
            stream,
            fallback: None,
            published: None,
        });
        self.publish_properties(active);
        drop(state);
        self.obj().notify("bound-port");
        self.obj().notify("active-mode");
        Ok(())
    }

    fn render(&self, buffer: &gst::Buffer) -> Result<gst::FlowSuccess, gst::FlowError> {
        let mut state = self.state.lock().unwrap();
        let Some(active) = state.as_mut() else {
            gst::element_imp_error!(self, gst::CoreError::Negotiation, ["No caps set"]);
            return Err(gst::FlowError::NotNegotiated);
        };

        let timestamp_usec = self.server_timestamp(buffer, active);
        if self.track_position(buffer) {
            self.publish_properties(active);
        }
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
