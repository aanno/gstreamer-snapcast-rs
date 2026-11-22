//! `snapsink`'s `mode`: embedded, or the user's shared server daemon.
//!
//! One test, because the daemon is found through `XDG_RUNTIME_DIR`, which this
//! test sets for its whole process (a private directory, never the user's).

#![cfg(unix)]

use std::os::unix::net::UnixListener;
use std::time::Duration;

use gst::prelude::*;
use gst_snapcast_engine::{client, ipc, runtime};

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn launch(port: u16, stream: &str, mode: &str) -> gst::Element {
    gst::parse::launch(&format!(
        "audiotestsrc is-live=true ! audio/x-raw,format=S16LE,rate=48000,channels=2 \
         ! snapsink name=sink port={port} stream={stream} mode={mode} mdns-name=\"\""
    ))
    .unwrap()
}

fn sink(pipeline: &gst::Element) -> gst::Element {
    pipeline
        .downcast_ref::<gst::Bin>()
        .unwrap()
        .by_name("sink")
        .unwrap()
}

/// Start `pipeline`; the active mode once its stream runs.
fn play(pipeline: &gst::Element) -> String {
    pipeline.set_state(gst::State::Playing).unwrap();
    let (result, state, _) = pipeline.state(gst::ClockTime::from_seconds(5));
    assert!(result.is_ok() && state == gst::State::Playing, "{result:?}");
    let mode = sink(pipeline).property_value("active-mode");
    let mode = mode.get::<&gst::glib::EnumValue>().unwrap();
    mode.nick().to_owned()
}

fn first_error(pipeline: &gst::Element) -> String {
    let msg = pipeline
        .bus()
        .unwrap()
        .timed_pop_filtered(gst::ClockTime::from_seconds(5), &[gst::MessageType::Error])
        .expect("an error");
    let gst::MessageView::Error(err) = msg.view() else {
        unreachable!()
    };
    format!("{} {:?}", err.error(), err.debug())
}

#[test]
fn modes_choose_between_the_shared_server_and_an_embedded_one() {
    let runtime_dir = std::env::temp_dir().join(format!("snapsink-shared-{}", std::process::id()));
    std::fs::create_dir_all(&runtime_dir).unwrap();
    // SAFETY: the only test in this binary sets it before GStreamer or the engine
    // start any thread that reads the environment.
    unsafe { std::env::set_var("XDG_RUNTIME_DIR", &runtime_dir) };
    gst::init().unwrap();
    gstsnapsink::plugin_register_static().unwrap();

    // No daemon yet: auto runs embedded, shared fails.
    let auto = launch(free_port(), "a", "auto");
    assert_eq!(play(&auto), "embedded");
    auto.set_state(gst::State::Null).unwrap();
    let shared = launch(free_port(), "a", "shared");
    let _ = shared.set_state(gst::State::Playing);
    let error = first_error(&shared);
    assert!(error.contains("shared Snapcast server"), "{error}");
    shared.set_state(gst::State::Null).unwrap();

    // Start a daemon on the private socket.
    let dir = runtime_dir.join(ipc::DIR_NAME);
    std::fs::create_dir_all(&dir).unwrap();
    let listener = UnixListener::bind(ipc::socket_path(&dir)).unwrap();
    std::thread::spawn(move || gst_snapcast_daemon::serve(&listener));

    // Now both modes go to the daemon, and the streams share its port.
    let port = free_port();
    let a = launch(port, "a", "shared");
    assert_eq!(play(&a), "shared");
    let b = launch(port, "b", "auto");
    assert_eq!(play(&b), "shared");
    for p in [&a, &b] {
        assert_eq!(sink(p).property::<u32>("bound-port"), u32::from(port));
    }
    // A name in use is still refused.
    let a2 = launch(port, "a", "shared");
    let _ = a2.set_state(gst::State::Playing);
    let error = first_error(&a2);
    assert!(error.contains("already in use"), "{error}");
    a2.set_state(gst::State::Null).unwrap();

    // The audio reaches a Snapcast client of the daemon's server.
    let mut player = client::connect(&client::ClientOptions {
        port,
        host_id: "snapsink-shared-test".into(),
        ..client::ClientOptions::default()
    })
    .unwrap();
    runtime().block_on(async {
        tokio::time::timeout(Duration::from_secs(10), player.recv())
            .await
            .expect("audio from the shared server")
            .expect("client running");
    });

    a.set_state(gst::State::Null).unwrap();
    b.set_state(gst::State::Null).unwrap();
    let _ = std::fs::remove_dir_all(&runtime_dir);
}
