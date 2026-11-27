//! The control API end to end: `snapsink` with `control-port`/`http-port`, a
//! `snapsrc` receiver, and a controller speaking JSON-RPC like Snapweb does.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use gst::prelude::*;
use serde_json::{Value, json};

const TIMEOUT: Duration = Duration::from_secs(10);

fn init() {
    use std::sync::Once;
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        gst::init().unwrap();
        gstsnapsink::plugin_register_static().expect("register snapsink plugin");
        gstsnapsrc::plugin_register_static().expect("register snapsrc plugin");
    });
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn wait_until<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// A JSON-RPC controller over TCP (newline-delimited), like Snapdroid.
struct Controller {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
    next_id: u64,
}

impl Controller {
    fn connect(port: u16) -> Self {
        let stream = TcpStream::connect(("127.0.0.1", port)).expect("control port");
        stream.set_read_timeout(Some(TIMEOUT)).unwrap();
        Self {
            reader: BufReader::new(stream.try_clone().unwrap()),
            writer: stream,
            next_id: 1,
        }
    }

    /// Calls `method` and returns its `result`; notifications in between are skipped.
    fn call(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        let request = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        writeln!(self.writer, "{request}").unwrap();
        loop {
            let mut line = String::new();
            self.reader.read_line(&mut line).expect("reply");
            let msg: Value = serde_json::from_str(&line).unwrap();
            if msg["id"] == id {
                assert!(msg.get("error").is_none(), "{method} failed: {msg}");
                return msg["result"].clone();
            }
        }
    }
}

#[test]
fn snapweb_style_control_of_a_snapsink_server() {
    init();
    let (control_port, http_port) = (free_port(), free_port());
    let snapweb = std::env::temp_dir().join(format!("snapsink-snapweb-{}", std::process::id()));
    std::fs::create_dir_all(&snapweb).unwrap();
    std::fs::write(
        snapweb.join("index.html"),
        "<title>snapweb stand-in</title>",
    )
    .unwrap();

    let sender = gst::parse::launch(&format!(
        "audiotestsrc is-live=true \
         ! taginject tags=\"title=Song,artist=Band,album=Record\" \
         ! audio/x-raw,format=S16LE,rate=48000,channels=2 \
         ! snapsink name=sink port=0 mdns-name=\"\" buffer-ms=300 \
           control-port={control_port} http-port={http_port} snapweb-dir={} \
           controls=play+pause+next",
        snapweb.display()
    ))
    .unwrap();
    sender.set_state(gst::State::Playing).unwrap();
    let sink = sender
        .downcast_ref::<gst::Bin>()
        .unwrap()
        .by_name("sink")
        .unwrap();
    let port = wait_until("snapsink's server", || {
        Some(sink.property::<u32>("bound-port")).filter(|p| *p != 0)
    });

    let receiver = gst::parse::launch(&format!(
        "snapsrc name=src port={port} client-name=controlled ! fakesink sync=false"
    ))
    .unwrap();
    receiver.set_state(gst::State::Playing).unwrap();
    let src = receiver
        .downcast_ref::<gst::Bin>()
        .unwrap()
        .by_name("src")
        .unwrap();

    let mut ctl = Controller::connect(control_port);

    // The receiver shows up as a connected client; the stream carries the tags.
    let client_id = wait_until("the receiver in Server.GetStatus", || {
        let status = ctl.call("Server.GetStatus", json!({}));
        status["server"]["groups"]
            .as_array()?
            .iter()
            .flat_map(|g| g["clients"].as_array().into_iter().flatten())
            .find(|c| c["connected"] == true)
            .map(|c| c["id"].as_str().unwrap().to_owned())
    });
    let metadata = wait_until("the stream's metadata", || {
        let status = ctl.call("Server.GetStatus", json!({}));
        let metadata = status["server"]["streams"][0]["properties"]["metadata"].clone();
        (metadata["title"] == "Song").then_some(metadata)
    });
    assert_eq!(metadata["artist"], json!(["Band"]));
    assert_eq!(metadata["album"], "Record");

    // The controls and the playback status are stream properties too.
    let status = ctl.call("Server.GetStatus", json!({}));
    let properties = &status["server"]["streams"][0]["properties"];
    assert_eq!(properties["canControl"], true, "{properties}");
    assert_eq!(properties["canPlay"], true);
    assert_eq!(properties["canPause"], true);
    assert_eq!(properties["canGoNext"], true);
    assert_eq!(properties["canGoPrevious"], false);
    assert_eq!(properties["canSeek"], false);
    assert_eq!(properties["playbackStatus"], "playing");

    // `controls` can change while playing.
    sink.set_property_from_str("controls", "play+pause+next+seek");
    wait_until("canSeek after changing controls", || {
        let status = ctl.call("Server.GetStatus", json!({}));
        (status["server"]["streams"][0]["properties"]["canSeek"] == true).then_some(())
    });

    // Stream.Control arrives as an element message on the sender's bus.
    ctl.call(
        "Stream.Control",
        json!({"id": "default", "command": "next", "params": {}}),
    );
    let bus = sender.bus().unwrap();
    let message = bus
        .timed_pop_filtered(
            gst::ClockTime::from_seconds(10),
            &[gst::MessageType::Element],
        )
        .expect("no snapcast-stream-control message");
    let structure = message.structure().unwrap();
    assert_eq!(structure.name(), "snapcast-stream-control");
    assert_eq!(structure.get::<&str>("command").unwrap(), "next");
    assert_eq!(structure.get::<&str>("stream").unwrap(), "default");
    assert_eq!(structure.get::<&str>("params").unwrap(), "{}");

    // Changing the volume reaches snapsrc.
    let result = ctl.call(
        "Client.SetVolume",
        json!({"id": client_id, "volume": {"percent": 30, "muted": false}}),
    );
    assert_eq!(result["volume"]["percent"], 30);
    wait_until("snapsrc to apply the volume", || {
        (src.property::<u32>("volume") == 30).then_some(())
    });

    // HTTP: Snapweb's files.
    let mut http = TcpStream::connect(("127.0.0.1", http_port)).expect("http port");
    http.set_read_timeout(Some(TIMEOUT)).unwrap();
    write!(
        http,
        "GET /index.html HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut page = String::new();
    http.read_to_string(&mut page).unwrap();
    assert!(page.starts_with("HTTP/1.1 200"), "{page}");
    assert!(page.contains("snapweb stand-in"));

    receiver.set_state(gst::State::Null).unwrap();
    sender.set_state(gst::State::Null).unwrap();
    // The control ports close with the server.
    wait_until("the control port to close", || {
        TcpStream::connect(("127.0.0.1", control_port))
            .is_err()
            .then_some(())
    });
    std::fs::remove_dir_all(&snapweb).ok();
}

/// `playbin uri=snapcast-tcp://…/b` picks `snapsrc` (URI handler), which joins
/// stream `b` via the control API, although the server puts new clients on the
/// first stream (`docs/dlna.md`, PoC step 2).
#[test]
fn playbin_plays_a_snapcast_tcp_uri_and_joins_its_stream() {
    init();
    let (port, control_port) = (free_port(), free_port());
    let launch = |stream: &str, extra: &str| {
        let p = gst::parse::launch(&format!(
            "audiotestsrc is-live=true ! audio/x-raw,format=S16LE,rate=48000,channels=2 \
             ! snapsink port={port} mode=embedded stream={stream} mdns-name=\"\" {extra}"
        ))
        .unwrap();
        p.set_state(gst::State::Playing).unwrap();
        p
    };
    let first = launch("a", &format!("control-port={control_port}"));
    let mut ctl = wait_until("the control port", || {
        TcpStream::connect(("127.0.0.1", control_port))
            .ok()
            .map(|_| Controller::connect(control_port))
    });
    let second = launch("b", "");

    let uri = format!("snapcast-tcp://127.0.0.1:{port}/b?control={control_port}");
    let player = gst::ElementFactory::make("playbin")
        .property("uri", &uri)
        .property(
            "audio-sink",
            gst::ElementFactory::make("fakesink").build().unwrap(),
        )
        .build()
        .unwrap();
    player.set_state(gst::State::Playing).unwrap();

    // The client ends up in a group playing `b`.
    wait_until("the playbin client on stream b", || {
        let status = ctl.call("Server.GetStatus", json!({}));
        status["server"]["groups"]
            .as_array()?
            .iter()
            .find(|g| {
                g["stream_id"] == "b"
                    && g["clients"]
                        .as_array()
                        .is_some_and(|c| c.iter().any(|c| c["connected"] == true))
            })
            .map(|_| ())
    });
    // And it plays: audio reaches the sink.
    let sink = player.property::<gst::Element>("audio-sink");
    wait_until("audio at playbin's sink", || {
        sink.property::<Option<gst::Sample>>("last-sample")
            .map(|_| ())
    });
    let src = player.property::<gst::Element>("source");
    assert_eq!(src.factory().unwrap().name(), "snapsrc");
    assert_eq!(src.property::<String>("stream"), "b");
    assert_eq!(src.property::<u32>("control-port"), u32::from(control_port));
    assert_eq!(
        src.dynamic_cast_ref::<gst::URIHandler>()
            .unwrap()
            .uri()
            .unwrap(),
        uri
    );

    for p in [&player, &second, &first] {
        p.set_state(gst::State::Null).unwrap();
    }
}

/// The latest `snapcast-session` message on `bus` that satisfies `want`.
fn wait_for_session(bus: &gst::Bus, what: &str, want: impl Fn(&gst::StructureRef) -> bool) {
    let deadline = Instant::now() + TIMEOUT;
    while Instant::now() < deadline {
        let Some(message) = bus.timed_pop_filtered(
            gst::ClockTime::from_mseconds(100),
            &[gst::MessageType::Element, gst::MessageType::Error],
        ) else {
            continue;
        };
        if let gst::MessageView::Error(e) = message.view() {
            panic!("error while waiting for {what}: {}", e.error());
        }
        let s = message.structure().unwrap();
        if s.name() == "snapcast-session" && want(s) {
            return;
        }
    }
    panic!("timed out waiting for {what}");
}

/// A session (`docs/dlna.md`, PoC step 3): snapsrc follows the stream's properties
/// (`snapcast-session` messages) and sends `Stream.Control` and `Client.SetVolume`
/// (action signals); the sender acts on the command as dms will.
#[test]
fn snapsrc_follows_and_controls_a_session() {
    init();
    let (port, control_port) = (free_port(), free_port());
    let sender = gst::parse::launch(&format!(
        "audiotestsrc is-live=true ! audio/x-raw,format=S16LE,rate=48000,channels=2 \
         ! taginject tags=\"title=Song,duration=(guint64)90000000000\" \
         ! snapsink port={port} mode=embedded stream=s mdns-name=\"\" buffer-ms=300 \
           control-port={control_port} controls=play+pause+seek"
    ))
    .unwrap();
    sender.set_state(gst::State::Playing).unwrap();
    wait_until("the control port", || {
        TcpStream::connect(("127.0.0.1", control_port)).ok()
    });

    let receiver = gst::parse::launch(&format!(
        "snapsrc name=src port={port} stream=s control-port={control_port} ! fakesink sync=false"
    ))
    .unwrap();
    receiver.set_state(gst::State::Playing).unwrap();
    let src = receiver
        .downcast_ref::<gst::Bin>()
        .unwrap()
        .by_name("src")
        .unwrap();
    let bus = receiver.bus().unwrap();

    // Playing, with a position, the tags and what the stream accepts.
    wait_for_session(&bus, "a playing session with a position", |s| {
        s.get::<&str>("playback-status") == Ok("playing")
            && s.get::<f64>("position").is_ok_and(|p| p >= 0.0)
    });
    wait_for_session(&bus, "the session's title and controls", |s| {
        s.get::<&str>("title") == Ok("Song")
            && s.get::<f64>("duration") == Ok(90.0)
            && s.get::<bool>("can-pause") == Ok(true)
            && s.get::<bool>("can-seek") == Ok(true)
            && s.get::<&str>("stream") == Ok("s")
    });

    // Pause from the receiver reaches the sender, which pauses; the receiver sees it.
    assert!(src.emit_by_name::<bool>("stream-control", &[&"pause", &""]));
    let message = sender
        .bus()
        .unwrap()
        .timed_pop_filtered(
            gst::ClockTime::from_seconds(10),
            &[gst::MessageType::Element],
        )
        .expect("no snapcast-stream-control message");
    let command = message.structure().unwrap();
    assert_eq!(command.get::<&str>("command").unwrap(), "pause");
    sender.set_state(gst::State::Paused).unwrap();
    wait_for_session(&bus, "the paused session", |s| {
        s.get::<&str>("playback-status") == Ok("paused")
    });

    // Parameters that aren't JSON are refused, not sent as null (gmrender under a
    // German locale wrote a decimal comma, and every seek went to the start).
    assert!(!src.emit_by_name::<bool>(
        "stream-control",
        &[&"setPosition", &r#"{"position":20,000}"#]
    ));
    // setPosition carries its parameters.
    assert!(src.emit_by_name::<bool>("stream-control", &[&"setPosition", &r#"{"position":42.5}"#]));
    let message = sender
        .bus()
        .unwrap()
        .timed_pop_filtered(
            gst::ClockTime::from_seconds(10),
            &[gst::MessageType::Element],
        )
        .expect("no snapcast-stream-control message");
    let command = message.structure().unwrap();
    assert_eq!(command.get::<&str>("command").unwrap(), "setPosition");
    let params: Value = serde_json::from_str(command.get::<&str>("params").unwrap()).unwrap();
    assert_eq!(params["position"], 42.5);

    // The volume goes to this client on the server and comes back.
    assert!(src.emit_by_name::<bool>("set-server-volume", &[&40u32, &false]));
    wait_until("the server volume back at snapsrc", || {
        (src.property::<u32>("volume") == 40).then_some(())
    });

    receiver.set_state(gst::State::Null).unwrap();
    sender.set_state(gst::State::Null).unwrap();
}
