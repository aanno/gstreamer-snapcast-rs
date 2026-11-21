//! Bridge from `tracing` and `log` to the GStreamer debug log (L9).
//!
//! snapcast-rs and `snapcast-engine` log with [`tracing`]; some of their
//! dependencies (`mdns-sd`, `symphonia`) with [`log`]. Without a subscriber
//! all of that is lost. [`init`] installs, once per process (or per plugin
//! `.so`, which links its own copy of `tracing`):
//!
//! * a global `tracing` subscriber whose only layer writes each event to a
//!   GStreamer debug category, **if no global subscriber is set yet**, and
//! * a [`log::Log`] doing the same, if no logger is set yet.
//!
//! Set [`DISABLE_ENV`] to `0` (or `false`, `no`, `off`) to install neither,
//! e.g. in an application that links the plugins statically and installs its
//! own subscriber after registering them.
//!
//! # Categories and levels
//!
//! The crate of an event's target picks the category; see [`category_for`].
//! `GST_DEBUG=snapcast*:5` shows all of them. Levels map ERROR → ERROR,
//! WARN → WARNING, INFO → INFO, DEBUG → DEBUG and TRACE → LOG. An event below
//! its category's threshold costs one threshold check: callsites are
//! registered as "sometimes", so `GST_DEBUG` changes at runtime apply.
//!
//! # Element names
//!
//! An event inside a span with a `log_id` field
//! ([`snapcast_engine::LOG_ID_FIELD`]) is logged with that id
//! (`gst_debug_log_id`), so the line shows e.g. `<snapsrc0>`; the innermost
//! such span wins. The engine opens these spans around its tasks.
//!
//! # Loops
//!
//! A GStreamer log function that turns lines back into `tracing` events (as
//! `tracing-gstreamer` does) or `log` records would feed them to this bridge
//! again. Events and records emitted while this thread is already forwarding
//! one are dropped. (`tracing` alone already drops events emitted while it
//! dispatches one; the `log` crate has no such guard.)

use std::cell::Cell;
use std::fmt::{self, Write as _};
use std::sync::{LazyLock, Once};

use gst::glib;
use gst::glib::IntoGStr;
use gst::{DebugCategory, DebugColorFlags, DebugLevel, DebugLogger as _};
use tracing::span::{Attributes, Id};
use tracing::subscriber::Interest;
use tracing::{Event, Metadata, Subscriber};
use tracing_subscriber::layer::{Context, Layer, SubscriberExt as _};
use tracing_subscriber::registry::LookupSpan;

/// Environment variable that turns the bridge off (`0`, `false`, `no`, `off`).
pub const DISABLE_ENV: &str = "GST_SNAPCAST_TRACING";

static CLIENT: LazyLock<DebugCategory> = LazyLock::new(|| {
    DebugCategory::new(
        "snapcast-client",
        DebugColorFlags::empty(),
        Some("snapcast-rs client library (tracing)"),
    )
});
static SERVER: LazyLock<DebugCategory> = LazyLock::new(|| {
    DebugCategory::new(
        "snapcast-server",
        DebugColorFlags::empty(),
        Some("snapcast-rs server library (tracing)"),
    )
});
static ENGINE: LazyLock<DebugCategory> = LazyLock::new(|| {
    DebugCategory::new(
        "snapcast-engine",
        DebugColorFlags::empty(),
        Some("snapcast-engine glue (tracing) and the logging bridge itself"),
    )
});
static MDNS: LazyLock<DebugCategory> = LazyLock::new(|| {
    DebugCategory::new(
        "snapcast-mdns",
        DebugColorFlags::empty(),
        Some("mdns-sd discovery and announcement (log)"),
    )
});
static DEPS: LazyLock<DebugCategory> = LazyLock::new(|| {
    DebugCategory::new(
        "snapcast-deps",
        DebugColorFlags::empty(),
        Some("other libraries of the Snapcast plugins, e.g. snapcast-proto, symphonia"),
    )
});

/// The GStreamer debug category for events with this `tracing`/`log` target.
///
/// The target's first path segment (the crate) decides:
///
/// | Crate | Category |
/// |---|---|
/// | `snapcast_client` | `snapcast-client` |
/// | `snapcast_server` | `snapcast-server` |
/// | `snapcast_engine` | `snapcast-engine` |
/// | `mdns_sd` | `snapcast-mdns` |
/// | anything else | `snapcast-deps` |
pub fn category_for(target: &str) -> DebugCategory {
    match target.split("::").next().unwrap_or(target) {
        "snapcast_client" => *CLIENT,
        "snapcast_server" => *SERVER,
        "snapcast_engine" => *ENGINE,
        "mdns_sd" => *MDNS,
        _ => *DEPS,
    }
}

fn gst_level(level: tracing::Level) -> DebugLevel {
    match level {
        tracing::Level::ERROR => DebugLevel::Error,
        tracing::Level::WARN => DebugLevel::Warning,
        tracing::Level::INFO => DebugLevel::Info,
        tracing::Level::DEBUG => DebugLevel::Debug,
        tracing::Level::TRACE => DebugLevel::Log,
    }
}

fn gst_level_from_log(level: log::Level) -> DebugLevel {
    match level {
        log::Level::Error => DebugLevel::Error,
        log::Level::Warn => DebugLevel::Warning,
        log::Level::Info => DebugLevel::Info,
        log::Level::Debug => DebugLevel::Debug,
        log::Level::Trace => DebugLevel::Log,
    }
}

/// Whether the value of [`DISABLE_ENV`] leaves the bridge on.
fn enabled_by_env(value: Option<&str>) -> bool {
    let value = value.map(|v| v.trim().to_ascii_lowercase());
    !matches!(value.as_deref(), Some("0" | "false" | "no" | "off"))
}

/// Install the bridge (see the module docs). Idempotent; call it from
/// `plugin_init`. Leaves an existing global `tracing` subscriber or `log`
/// logger alone (and logs that at INFO in `snapcast-engine`).
pub fn init() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        // Register all categories now, so `--gst-debug-help` lists them.
        for target in [
            "snapcast_client",
            "snapcast_server",
            "snapcast_engine",
            "mdns_sd",
            "",
        ] {
            category_for(target);
        }
        if !enabled_by_env(std::env::var(DISABLE_ENV).ok().as_deref()) {
            gst::info!(ENGINE, "Logging bridge turned off by {DISABLE_ENV}");
            return;
        }
        let subscriber = tracing_subscriber::registry().with(GstLayer);
        if tracing::subscriber::set_global_default(subscriber).is_err() {
            gst::info!(
                ENGINE,
                "Another global tracing subscriber is installed; snapcast-rs events go there"
            );
        }
        match log::set_logger(&LOG_BRIDGE) {
            Ok(()) => log::set_max_level(log::LevelFilter::Trace),
            Err(_) => gst::info!(
                ENGINE,
                "Another log logger is installed; mdns-sd and symphonia messages go there"
            ),
        }
    });
}

thread_local! {
    /// This thread is forwarding an event right now (see "Loops").
    static FORWARDING: Cell<bool> = const { Cell::new(false) };
}

/// Run `f` unless this thread is already inside `forward`.
fn forward(f: impl FnOnce()) {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            FORWARDING.with(|busy| busy.set(false));
        }
    }
    if FORWARDING.with(|busy| busy.replace(true)) {
        return;
    }
    let _reset = Reset;
    f();
}

/// The `log_id` of a span, kept in its extensions.
struct LogId(glib::GString);

/// Picks the `log_id` field out of a span's attributes.
#[derive(Default)]
struct LogIdVisitor(Option<String>);

impl tracing::field::Visit for LogIdVisitor {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == snapcast_engine::LOG_ID_FIELD {
            self.0 = Some(value.to_owned());
        }
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn fmt::Debug) {
        if field.name() == snapcast_engine::LOG_ID_FIELD {
            self.0 = Some(format!("{value:?}"));
        }
    }
}

/// An event's message followed by its other fields as ` name=value`.
#[derive(Default)]
struct EventText(String);

impl tracing::field::Visit for EventText {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.0.insert_str(0, value);
        } else {
            let _ = write!(self.0, " {}={value}", field.name());
        }
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn fmt::Debug) {
        if field.name() == "message" {
            self.0.insert_str(0, &format!("{value:?}"));
        } else {
            let _ = write!(self.0, " {}={value:?}", field.name());
        }
    }
}

/// The `tracing` layer writing events to the GStreamer debug log.
struct GstLayer;

impl<S> Layer<S> for GstLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn register_callsite(&self, _metadata: &'static Metadata<'static>) -> Interest {
        // Thresholds can change at runtime: ask `enabled` every time.
        Interest::sometimes()
    }

    fn enabled(&self, metadata: &Metadata<'_>, _ctx: Context<'_, S>) -> bool {
        // Spans are needed for their log ids whatever the level.
        metadata.is_span()
            || category_for(metadata.target()).above_threshold(gst_level(*metadata.level()))
    }

    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let mut visitor = LogIdVisitor::default();
        attrs.record(&mut visitor);
        if let Some(log_id) = visitor.0.filter(|log_id| !log_id.is_empty())
            && let Some(span) = ctx.span(id)
        {
            span.extensions_mut().insert(LogId(log_id.into()));
        }
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let metadata = event.metadata();
        let category = category_for(metadata.target());
        let level = gst_level(*metadata.level());
        if !category.above_threshold(level) {
            return;
        }
        forward(|| {
            let mut text = EventText::default();
            event.record(&mut text);
            let log_id = ctx.event_scope(event).and_then(|mut scope| {
                scope.find_map(|span| span.extensions().get::<LogId>().map(|id| id.0.clone()))
            });
            let function = metadata.module_path().unwrap_or(metadata.target());
            let line = metadata.line().unwrap_or(0);
            metadata
                .file()
                .unwrap_or("")
                .run_with_gstr(|file| match log_id {
                    Some(log_id) => {
                        category.log_id(
                            log_id,
                            level,
                            file,
                            function,
                            line,
                            format_args!("{}", text.0),
                        );
                    }
                    None => category.log(
                        None::<&glib::Object>,
                        level,
                        file,
                        function,
                        line,
                        format_args!("{}", text.0),
                    ),
                });
        });
    }
}

/// The `log` logger writing records to the GStreamer debug log.
struct LogBridge;

static LOG_BRIDGE: LogBridge = LogBridge;

impl log::Log for LogBridge {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        category_for(metadata.target()).above_threshold(gst_level_from_log(metadata.level()))
    }

    fn log(&self, record: &log::Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        forward(|| {
            let category = category_for(record.target());
            let function = record.module_path().unwrap_or(record.target());
            record.file().unwrap_or("").run_with_gstr(|file| {
                category.log(
                    None::<&glib::Object>,
                    gst_level_from_log(record.level()),
                    file,
                    function,
                    record.line().unwrap_or(0),
                    *record.args(),
                );
            });
        });
    }

    fn flush(&self) {}
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    /// One line as GStreamer received it.
    #[derive(Debug, Clone)]
    struct Line {
        category: String,
        level: DebugLevel,
        file: String,
        line: u32,
        id: Option<String>,
        message: String,
    }

    static LINES: Mutex<Vec<Line>> = Mutex::new(Vec::new());

    fn setup() {
        static SETUP: Once = Once::new();
        SETUP.call_once(|| {
            gst::init().unwrap();
            gst::log::remove_default_log_function();
            gst::log::add_log_function(|category, level, file, _function, line, _obj, msg| {
                let Some(message) = msg.get() else { return };
                if !message.starts_with("t-") {
                    return;
                }
                LINES.lock().unwrap().push(Line {
                    category: category.name().to_owned(),
                    level,
                    file: file.to_string(),
                    line,
                    id: msg.id().map(|id| id.to_string()),
                    message: message.to_string(),
                });
            });
            // Turn GStreamer lines back into `log` records, as a GStreamer-to-log
            // bridge would (see `forwarding_does_not_loop`). Never removed:
            // `remove_log_function` identifies a function by its boxed closure,
            // and all capture-less closures share one dangling pointer, so it
            // would remove the capturing function above as well.
            gst::log::add_log_function(|_cat, _level, _file, _function, _line, _obj, msg| {
                if msg.get().is_some_and(|m| m.starts_with("t-loop")) {
                    log::warn!(target: "snapcast_engine::echo", "t-loop-echo");
                }
            });
            gst::log::set_active(true);
            gst::log::set_threshold_for_name("snapcast-*", DebugLevel::Log);
        });
    }

    /// The lines whose message starts with `prefix`.
    fn lines(prefix: &str) -> Vec<Line> {
        let lines = LINES.lock().unwrap();
        lines
            .iter()
            .filter(|l| l.message.starts_with(prefix))
            .cloned()
            .collect()
    }

    fn only(prefix: &str) -> Line {
        let found = lines(prefix);
        assert_eq!(found.len(), 1, "lines starting with {prefix:?}: {found:?}");
        found[0].clone()
    }

    /// Run `f` with the layer as this thread's subscriber.
    fn with_layer(f: impl FnOnce()) {
        setup();
        tracing::subscriber::with_default(tracing_subscriber::registry().with(GstLayer), f);
    }

    #[test]
    fn events_land_in_the_category_of_their_crate() {
        with_layer(|| {
            tracing::warn!(target: "snapcast_client::controller", "t-cat-client");
            tracing::warn!(target: "snapcast_server::session", "t-cat-server");
            tracing::warn!(target: "snapcast_engine::client", "t-cat-engine");
            tracing::warn!(target: "mdns_sd::service_daemon", "t-cat-mdns");
            tracing::warn!(target: "symphonia_core::probe", "t-cat-deps");
        });
        for (prefix, category) in [
            ("t-cat-client", "snapcast-client"),
            ("t-cat-server", "snapcast-server"),
            ("t-cat-engine", "snapcast-engine"),
            ("t-cat-mdns", "snapcast-mdns"),
            ("t-cat-deps", "snapcast-deps"),
        ] {
            assert_eq!(only(prefix).category, category, "{prefix}");
        }
    }

    #[test]
    fn levels_fields_and_location_are_kept() {
        let mut line = 0;
        with_layer(|| {
            tracing::error!(target: "snapcast_engine::t", "t-lvl-error");
            tracing::warn!(target: "snapcast_engine::t", "t-lvl-warn");
            tracing::info!(target: "snapcast_engine::t", "t-lvl-info");
            tracing::debug!(target: "snapcast_engine::t", "t-lvl-debug");
            tracing::trace!(target: "snapcast_engine::t", "t-lvl-trace");
            line = line!() + 1;
            tracing::info!(target: "snapcast_engine::t", peer = "10.0.0.1:1704", count = 3, "t-fields");
        });
        for (prefix, level) in [
            ("t-lvl-error", DebugLevel::Error),
            ("t-lvl-warn", DebugLevel::Warning),
            ("t-lvl-info", DebugLevel::Info),
            ("t-lvl-debug", DebugLevel::Debug),
            ("t-lvl-trace", DebugLevel::Log),
        ] {
            assert_eq!(only(prefix).level, level, "{prefix}");
        }
        let fields = only("t-fields");
        assert_eq!(fields.message, "t-fields peer=10.0.0.1:1704 count=3");
        assert!(fields.file.ends_with("log.rs"), "{}", fields.file);
        assert_eq!(fields.line, line);
        assert_eq!(fields.id, None);
    }

    #[test]
    fn events_below_the_threshold_are_dropped() {
        setup();
        // Only this test uses targets outside the known crates below WARN.
        gst::log::set_threshold_for_name("snapcast-deps", DebugLevel::Warning);
        with_layer(|| {
            tracing::info!(target: "other_crate", "t-thr-info");
            tracing::warn!(target: "other_crate", "t-thr-warn");
        });
        assert!(lines("t-thr-info").is_empty());
        only("t-thr-warn");
    }

    #[test]
    fn the_innermost_log_id_tags_the_line() {
        with_layer(|| {
            tracing::warn!(target: "snapcast_engine::t", "t-id-none");
            let outer = tracing::info_span!("outer", log_id = "snapsrc7");
            let _outer = outer.enter();
            let plain = tracing::info_span!("plain", stream = "default");
            let _plain = plain.enter();
            tracing::warn!(target: "snapcast_engine::t", "t-id-outer");
            let server = tracing::info_span!("server", log_id = %"snapserver:1704");
            server.in_scope(|| tracing::warn!(target: "snapcast_server::t", "t-id-inner"));
            let empty = tracing::info_span!("empty", log_id = "");
            empty.in_scope(|| tracing::warn!(target: "snapcast_engine::t", "t-id-empty"));
        });
        assert_eq!(only("t-id-none").id, None);
        assert_eq!(only("t-id-outer").id.as_deref(), Some("snapsrc7"));
        assert_eq!(only("t-id-inner").id.as_deref(), Some("snapserver:1704"));
        assert_eq!(only("t-id-empty").id.as_deref(), Some("snapsrc7"));
    }

    #[test]
    fn forwarding_does_not_loop() {
        setup();
        init(); // the `log` bridge is global
        // The echo log function from `setup` turns each of these lines into a
        // `log` record; without the guard it would be forwarded, echoed, ...
        with_layer(|| tracing::warn!(target: "snapcast_engine::t", "t-loop-start"));
        log::warn!(target: "snapcast_engine::t", "t-loop-log");
        only("t-loop-start");
        only("t-loop-log");
        assert!(lines("t-loop-echo").is_empty());
    }

    #[test]
    fn init_bridges_tracing_and_log_on_every_thread() {
        setup();
        init();
        init();
        tracing::warn!(target: "snapcast_client::t", "t-init-tracing");
        log::warn!(target: "mdns_sd::service_daemon", "t-init-log");
        std::thread::spawn(|| tracing::warn!(target: "snapcast_server::t", "t-init-thread"))
            .join()
            .unwrap();
        assert_eq!(only("t-init-tracing").category, "snapcast-client");
        assert_eq!(only("t-init-log").category, "snapcast-mdns");
        assert_eq!(only("t-init-thread").category, "snapcast-server");
    }

    #[test]
    fn the_environment_variable_turns_the_bridge_off() {
        assert!(enabled_by_env(None));
        assert!(enabled_by_env(Some("1")));
        assert!(enabled_by_env(Some("")));
        for off in ["0", "false", "No", " off "] {
            assert!(!enabled_by_env(Some(off)), "{off:?}");
        }
    }
}
