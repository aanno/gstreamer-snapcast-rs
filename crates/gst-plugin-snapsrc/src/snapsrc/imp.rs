//! `snapsrc` implementation.
//!
//! Status: skeleton. Properties and pad template are in place; `start` fails
//! with a "not implemented" error. Next step: run a `SnapClient` from
//! `snapcast-engine`, translate Snapcast server time into buffer PTS (never
//! use network arrival time) and report latency to the pipeline.

use std::sync::{LazyLock, Mutex};

use gst::glib;
use gst::prelude::*;
use gst::subclass::prelude::*;
use gst_base::prelude::*;
use gst_base::subclass::prelude::*;

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

#[derive(Debug, Clone)]
struct Settings {
    host: String,
    port: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            host: DEFAULT_HOST.to_owned(),
            port: DEFAULT_PORT,
        }
    }
}

#[derive(Default)]
pub struct SnapSrc {
    settings: Mutex<Settings>,
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
            ]
        });
        PROPERTIES.as_ref()
    }

    fn set_property(&self, _id: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
        let mut settings = self.settings.lock().unwrap();
        match pspec.name() {
            "host" => {
                settings.host = value
                    .get::<Option<String>>()
                    .expect("type checked upstream")
                    .unwrap_or_else(|| DEFAULT_HOST.to_owned());
            }
            "port" => settings.port = value.get().expect("type checked upstream"),
            _ => unimplemented!(),
        }
    }

    fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
        let settings = self.settings.lock().unwrap();
        match pspec.name() {
            "host" => settings.host.to_value(),
            "port" => settings.port.to_value(),
            _ => unimplemented!(),
        }
    }

    fn constructed(&self) {
        self.parent_constructed();
        let obj = self.obj();
        // Snapcast is inherently live; buffers are timestamped in TIME format.
        obj.set_live(true);
        obj.set_format(gst::Format::Time);
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
            let caps = gst_audio::AudioCapsBuilder::new_interleaved()
                .format_list([gst_audio::AudioFormat::S16le, gst_audio::AudioFormat::S32le])
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
        Err(gst::error_msg!(
            gst::CoreError::NotImplemented,
            ["snapsrc is not implemented yet"]
        ))
    }

    fn stop(&self) -> Result<(), gst::ErrorMessage> {
        gst::info!(CAT, imp = self, "Stopped");
        Ok(())
    }
}

impl PushSrcImpl for SnapSrc {}
