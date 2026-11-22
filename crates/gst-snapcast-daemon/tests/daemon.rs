//! The daemon over its socket, in process: protocol, sharing, release, timing.

use std::io::{BufReader, BufWriter};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use gst_snapcast_engine::ipc::{
    self, Acquire, Acquired, Command, ErrorKind, Hello, Message, PROTOCOL_VERSION, Properties,
    ServerSettings, StreamSettings,
};
use gst_snapcast_engine::{client, now_usec, runtime};

/// Start a daemon on a fresh socket in a temp dir; returns the socket path.
fn start_daemon() -> PathBuf {
    static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "gst-snapcast-daemon-test-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let socket = ipc::socket_path(&dir);
    let _ = std::fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket).unwrap();
    std::thread::spawn(move || gst_snapcast_daemon::serve(&listener, None));
    socket
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

struct Sender {
    reader: BufReader<UnixStream>,
    writer: BufWriter<UnixStream>,
}

impl Sender {
    fn connect(socket: &PathBuf) -> Self {
        let stream = UnixStream::connect(socket).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        Self {
            reader: BufReader::new(stream.try_clone().unwrap()),
            writer: BufWriter::new(stream),
        }
    }

    fn send(&mut self, message: &Message) {
        ipc::write_message(&mut self.writer, message).unwrap();
    }

    fn recv(&mut self) -> Message {
        ipc::read_message(&mut self.reader)
            .unwrap()
            .expect("a message")
    }

    fn hello(&mut self, version: u32) -> Message {
        self.send(&Message::Hello(Hello {
            version,
            pid: std::process::id(),
            log_id: "test".into(),
        }));
        self.recv()
    }

    /// `Hello`, then `Acquire`; returns the answer to `Acquire`.
    fn acquire(socket: &PathBuf, port: u16, stream: &str) -> (Self, Message) {
        Self::acquire_with_control(socket, port, stream, None)
    }

    /// As [`Self::acquire`], with the control API over TCP on `control_tcp_port`.
    fn acquire_with_control(
        socket: &PathBuf,
        port: u16,
        stream: &str,
        control_tcp_port: Option<u16>,
    ) -> (Self, Message) {
        let mut sender = Self::connect(socket);
        assert!(matches!(
            sender.hello(PROTOCOL_VERSION),
            Message::Welcome(_)
        ));
        sender.send(&Message::Acquire(Acquire {
            server: ServerSettings {
                port,
                buffer_ms: 500,
                mdns_name: None,
                control_tcp_port,
                control_http_port: None,
                snapweb_dir: None,
            },
            stream: StreamSettings {
                name: stream.into(),
                codec: "pcm".into(),
                rate: 48000,
                bits: 16,
                channels: 2,
                log_id: format!("test-{stream}"),
            },
        }));
        let answer = sender.recv();
        (sender, answer)
    }
}

fn error_kind(message: &Message) -> ErrorKind {
    match message {
        Message::Error(e) => e.kind,
        other => panic!("expected an error, got {other:?}"),
    }
}

fn acquired(message: Message) -> Acquired {
    match message {
        Message::Acquired(a) => a,
        other => panic!("expected Acquired, got {other:?}"),
    }
}

#[test]
fn another_protocol_version_is_refused() {
    let socket = start_daemon();
    let mut sender = Sender::connect(&socket);
    assert_eq!(
        error_kind(&sender.hello(PROTOCOL_VERSION + 1)),
        ErrorKind::Version
    );
}

#[test]
fn pcm_before_acquire_is_refused() {
    let socket = start_daemon();
    let mut sender = Sender::connect(&socket);
    sender.hello(PROTOCOL_VERSION);
    sender.send(&Message::Pcm {
        timestamp_usec: 0,
        data: vec![0; 4],
    });
    assert_eq!(error_kind(&sender.recv()), ErrorKind::Protocol);
}

#[test]
fn senders_share_a_port_but_not_a_stream() {
    let socket = start_daemon();
    let port = free_port();
    let (_a, answer) = Sender::acquire(&socket, port, "a");
    let a = acquired(answer);
    assert_eq!(a.port, port);
    assert_eq!(a.server.buffer_ms, 500);

    let (_b, answer) = Sender::acquire(&socket, port, "b");
    assert_eq!(acquired(answer).port, port);

    let (_a2, answer) = Sender::acquire(&socket, port, "a");
    assert_eq!(error_kind(&answer), ErrorKind::StreamTaken);
}

#[test]
fn closing_the_connection_releases_the_stream() {
    let socket = start_daemon();
    let port = free_port();
    let (_keeps_the_server, answer) = Sender::acquire(&socket, port, "keep");
    acquired(answer);
    let (a, answer) = Sender::acquire(&socket, port, "a");
    acquired(answer);
    drop(a);

    // The daemon notices the closed socket on its connection thread.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let (_again, answer) = Sender::acquire(&socket, port, "a");
        match answer {
            Message::Acquired(_) => break,
            Message::Error(e) if e.kind == ErrorKind::StreamTaken => {
                assert!(Instant::now() < deadline, "stream not released");
                std::thread::sleep(Duration::from_millis(20));
            }
            other => panic!("unexpected {other:?}"),
        }
    }
}

/// A sender's timestamps reach the Snapcast clients unchanged: a marker sent
/// with server time `t` plays at `t + buffer`.
#[test]
fn pcm_plays_at_its_timestamp_plus_the_buffer() {
    const BUFFER_US: i64 = 500_000;
    const FRAME_US: i64 = 10_000;
    const FRAMES: usize = 480; // 10 ms at 48 kHz
    const MARKER_AT: i64 = 150; // frame index of the marker

    let socket = start_daemon();
    let (mut sender, answer) = Sender::acquire(&socket, 0, "timed");
    let port = acquired(answer).port;

    let mut player = client::connect(&client::ClientOptions {
        port,
        host_id: "daemon-timing-test".into(),
        log_id: "test-client".into(),
        ..client::ClientOptions::default()
    })
    .unwrap();

    // Send in real time, each frame stamped 100 ms ahead of now.
    let t0 = now_usec() + 100_000;
    let silence = vec![0u8; FRAMES * 4];
    let mut marker = silence.clone();
    marker[..2].copy_from_slice(&10_000i16.to_le_bytes());
    let marker_ts = t0 + MARKER_AT * FRAME_US;
    let send = std::thread::spawn(move || {
        for i in 0..300i64 {
            let ts = t0 + i * FRAME_US;
            while now_usec() + 100_000 < ts {
                std::thread::sleep(Duration::from_millis(1));
            }
            let data = if i == MARKER_AT {
                marker.clone()
            } else {
                silence.clone()
            };
            sender.send(&Message::Pcm {
                timestamp_usec: ts,
                data,
            });
        }
        sender
    });

    let deadline = Duration::from_secs(10);
    let played_at = runtime().block_on(async {
        tokio::time::timeout(deadline, async {
            loop {
                let frame = player.recv().await.expect("client running");
                let channels = usize::from(frame.channels);
                if let Some(i) = frame.samples.iter().position(|s| s.abs() > 0.1) {
                    let offset_us = (i / channels) as i64 * 1_000_000 / i64::from(frame.rate);
                    return frame.playout_usec + offset_us;
                }
            }
        })
        .await
        .expect("the marker reaches the client")
    });
    let _sender = send.join().unwrap();

    let error_us = played_at - (marker_ts + BUFFER_US);
    eprintln!("daemon: marker plays {error_us:+} µs off its timestamp + buffer");
    assert!(
        error_us.abs() < 5_000,
        "marker plays {error_us} µs off its timestamp + buffer"
    );
}

/// One JSON-RPC request over TCP; the reply (notifications have no `id`).
fn rpc(port: u16, request: &str) -> serde_json::Value {
    use std::io::{BufRead, Write};
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    writeln!(stream, "{request}").unwrap();
    let mut reader = BufReader::new(stream);
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let message: serde_json::Value = serde_json::from_str(&line).unwrap();
        if message.get("id").is_some() {
            return message;
        }
    }
}

/// Properties go from the sender to the controllers, commands come back.
#[test]
fn properties_reach_controllers_and_commands_reach_the_sender() {
    let socket = start_daemon();
    let (mut sender, answer) = Sender::acquire_with_control(&socket, 0, "tagged", Some(0));
    let control = acquired(answer)
        .control_tcp_port
        .expect("the daemon serves the control API");

    sender.send(&Message::Properties(Properties {
        title: Some("Song".into()),
        artist: vec!["Band".into()],
        can_go_next: true,
        playback: Some("playing".into()),
        ..Properties::default()
    }));
    let deadline = Instant::now() + Duration::from_secs(5);
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
            assert_eq!(properties["canPlay"], false);
            assert_eq!(properties["playbackStatus"], "playing");
            break;
        }
        assert!(
            Instant::now() < deadline,
            "properties not applied: {properties}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    rpc(
        control,
        r#"{"jsonrpc":"2.0","id":2,"method":"Stream.Control","params":{"id":"tagged","command":"next","params":{}}}"#,
    );
    assert_eq!(
        sender.recv(),
        Message::Command(Command {
            command: "next".into(),
            params_json: "{}".into(),
        })
    );
}
