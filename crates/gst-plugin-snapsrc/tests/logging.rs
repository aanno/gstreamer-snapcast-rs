//! snapcast-rs and engine log lines reach the GStreamer debug log (L9), in
//! their categories and tagged with the element (or shared server) they
//! belong to. Own test binary: it installs a process-wide log function.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gst::prelude::*;

/// One line as GStreamer received it: category, id, message.
type Line = (String, Option<String>, String);

fn wait_for_port(sink: &gst::Element) -> u32 {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let port = sink.property::<u32>("bound-port");
        if port != 0 {
            return port;
        }
        assert!(Instant::now() < deadline, "snapsink did not start a server");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn snapcast_rs_events_reach_the_gstreamer_log_with_their_element() {
    gst::init().unwrap();
    // Registering the plugins installs the bridge (plugin_init).
    gstsnapsink::plugin_register_static().expect("register snapsink plugin");
    gstsnapsrc::plugin_register_static().expect("register snapsrc plugin");

    let lines: Arc<Mutex<Vec<Line>>> = Arc::default();
    let captured = Arc::clone(&lines);
    gst::log::add_log_function(
        move |category, _level, _file, _function, _line, _obj, msg| {
            if category.name().starts_with("snapcast-")
                && let Some(message) = msg.get()
            {
                captured.lock().unwrap().push((
                    category.name().to_owned(),
                    msg.id().map(|id| id.to_string()),
                    message.to_string(),
                ));
            }
        },
    );
    gst::log::set_active(true);
    gst::log::set_threshold_for_name("snapcast-*", gst::DebugLevel::Debug);

    let sender = gst::parse::launch(
        "audiotestsrc is-live=true ! audio/x-raw,format=S16LE,rate=48000,channels=2 \
         ! snapsink name=sink port=0",
    )
    .unwrap();
    sender.set_state(gst::State::Playing).unwrap();
    let port = wait_for_port(
        &sender
            .downcast_ref::<gst::Bin>()
            .unwrap()
            .by_name("sink")
            .unwrap(),
    );
    let receiver = gst::parse::launch(&format!("snapsrc name=src port={port} ! fakesink")).unwrap();
    receiver.set_state(gst::State::Playing).unwrap();

    let server_id = format!("snapserver:{port}");
    let expected: [(&str, &str, &str); 4] = [
        // snapcast-rs server: encoder thread and session task, in the server's span
        ("snapcast-server", &server_id, "Stream encoder started"),
        ("snapcast-server", &server_id, "Client hello"),
        // snapcast-rs client inside snapsrc's client span
        ("snapcast-client", "src", "Codec initialized"),
        // engine
        ("snapcast-engine", "src", "Snapcast client event"),
    ];
    let found = |lines: &[Line], (category, id, message): (&str, &str, &str)| {
        lines
            .iter()
            .any(|(c, i, m)| c == category && i.as_deref() == Some(id) && m.starts_with(message))
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let missing: Vec<_> = {
            let lines = lines.lock().unwrap();
            expected
                .iter()
                .filter(|e| !found(&lines, **e))
                .copied()
                .collect()
        };
        if missing.is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "missing {missing:?} in {:#?}",
            lines.lock().unwrap()
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    receiver.set_state(gst::State::Null).unwrap();
    sender.set_state(gst::State::Null).unwrap();
}
