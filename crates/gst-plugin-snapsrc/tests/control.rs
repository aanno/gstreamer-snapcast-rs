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
