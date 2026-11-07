//! `snapsink` implementation.
//!
//! Status: skeleton. Caps negotiation and properties are in place; `render`
//! currently drops buffers. Next step: hand buffers to a stream of the shared
//! embedded server in `snapcast-engine`, mapping PTS/running time onto the
//! server timeline.

use std::sync::{LazyLock, Mutex};

use gst::glib;
use gst::prelude::*;
use gst::subclass::prelude::*;
use gst_base::subclass::prelude::*;

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

#[derive(Debug, Clone)]
struct Settings {
    stream: String,
    port: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            stream: DEFAULT_STREAM.to_owned(),
            port: DEFAULT_PORT,
        }
    }
}

#[derive(Debug, Default)]
struct State {
    info: Option<gst_audio::AudioInfo>,
}

#[derive(Default)]
pub struct SnapSink {
    settings: Mutex<Settings>,
    state: Mutex<State>,
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
                    .blurb("TCP port of the embedded Snapcast server (shared by all snapsinks)")
                    .minimum(1)
                    .maximum(u16::MAX.into())
                    .default_value(DEFAULT_PORT)
                    .mutable_ready()
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
            _ => unimplemented!(),
        }
    }

    fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
        let settings = self.settings.lock().unwrap();
        match pspec.name() {
            "stream" => settings.stream.to_value(),
            "port" => settings.port.to_value(),
            _ => unimplemented!(),
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
            // Start with what Snapcast PCM/FLAC handle without conversion.
            // TODO: 24 bit (Snapcast uses S24 in 32 bit containers).
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
        *self.state.lock().unwrap() = State::default();
        gst::info!(CAT, imp = self, "Stopped");
        Ok(())
    }

    fn set_caps(&self, caps: &gst::Caps) -> Result<(), gst::LoggableError> {
        let info = gst_audio::AudioInfo::from_caps(caps)
            .map_err(|_| gst::loggable_error!(CAT, "Failed to parse caps {caps:?}"))?;
        gst::debug!(
            CAT,
            imp = self,
            "Configured for {} Hz, {} ch, {:?}",
            info.rate(),
            info.channels(),
            info.format()
        );
        self.state.lock().unwrap().info = Some(info);
        Ok(())
    }

    fn render(&self, buffer: &gst::Buffer) -> Result<gst::FlowSuccess, gst::FlowError> {
        gst::trace!(
            CAT,
            imp = self,
            "Dropping buffer (not implemented yet): pts {:?}, {} bytes",
            buffer.pts(),
            buffer.size()
        );
        Ok(gst::FlowSuccess::Ok)
    }
}
