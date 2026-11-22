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
    // No autospawn here (the daemon runs in this process below): a binary that
    // does not exist, so that an installed daemon is never started.
    unsafe {
        std::env::set_var("XDG_RUNTIME_DIR", &runtime_dir);
        std::env::set_var("GST_SNAPCAST_DAEMON", "/nonexistent/gst-snapcast-daemon");
    }
    gst::init().unwrap();
    gstsnapsink::plugin_register_static().unwrap();

    // No daemon, and none can be started: auto runs embedded, shared fails.
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
    std::thread::spawn(move || gst_snapcast_daemon::serve(&listener, None));

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

    tags_and_commands_pass_through_the_daemon();
    let _ = std::fs::remove_dir_all(&runtime_dir);
}

/// One JSON-RPC request over TCP; the reply (notifications have no `id`).
fn rpc(port: u16, request: &str) -> serde_json::Value {
    use std::io::{BufRead, Write};
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    writeln!(stream, "{request}").unwrap();
    let mut reader = std::io::BufReader::new(stream);
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let message: serde_json::Value = serde_json::from_str(&line).unwrap();
        if message.get("id").is_some() {
            return message;
        }
    }
}

/// With the daemon running: the sink's tags and `controls` reach the daemon's
/// control API, and `Stream.Control` comes back as the element message.
fn tags_and_commands_pass_through_the_daemon() {
    let port = free_port();
    let control = free_port();
    let pipeline = gst::parse::launch(&format!(
        "audiotestsrc is-live=true ! audio/x-raw,format=S16LE,rate=48000,channels=2 \
         ! taginject tags=\"title=Song,artist=Band\" \
         ! snapsink name=sink port={port} stream=tagged mode=shared control-port={control} \
           controls=next mdns-name=\"\""
    ))
    .unwrap();
    assert_eq!(play(&pipeline), "shared");

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let status = rpc(
            control,
            r#"{"jsonrpc":"2.0","id":1,"method":"Server.GetStatus"}"#,
        );
        let properties = &status["result"]["server"]["streams"][0]["properties"];
        if properties["metadata"]["title"] == "Song" {
            assert_eq!(
                properties["metadata"]["artist"],
                serde_json::json!(["Band"])
            );
            assert_eq!(properties["canGoNext"], true);
            assert_eq!(properties["playbackStatus"], "playing");
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "tags not shown: {properties}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    rpc(
        control,
        r#"{"jsonrpc":"2.0","id":2,"method":"Stream.Control","params":{"id":"tagged","command":"next","params":{}}}"#,
    );
    let msg = pipeline
        .bus()
        .unwrap()
        .timed_pop_filtered(
            gst::ClockTime::from_seconds(5),
            &[gst::MessageType::Element],
        )
        .expect("the command as an element message");
    let structure = msg.structure().unwrap();
    assert_eq!(structure.name(), "snapcast-stream-control");
    assert_eq!(structure.get::<&str>("command").unwrap(), "next");
    assert_eq!(structure.get::<&str>("stream").unwrap(), "tagged");
    pipeline.set_state(gst::State::Null).unwrap();
}
