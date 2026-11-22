//! The daemon's lifetime with the real binary: senders start it, only one runs,
//! a sender reconnects after it was killed, and it exits after the grace period.
//!
//! One test, because the engine finds the daemon through process-wide
//! environment variables (`XDG_RUNTIME_DIR`, `GST_SNAPCAST_DAEMON`), which this
//! test sets for its whole process (a private directory, never the user's).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use gst_snapcast_engine::server::{self, Mode, ServerOptions, StreamOptions};
use gst_snapcast_engine::{PcmFormat, ipc, now_usec};

const BINARY: &str = env!("CARGO_BIN_EXE_gst-snapcast-daemon");

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Pids of the daemons of this test: our binary with our `XDG_RUNTIME_DIR`.
fn daemons(runtime_dir: &Path) -> Vec<u32> {
    let wanted = format!("XDG_RUNTIME_DIR={}", runtime_dir.display()).into_bytes();
    let mut pids = Vec::new();
    for entry in std::fs::read_dir("/proc").unwrap().flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let Ok(exe) = std::fs::read_link(entry.path().join("exe")) else {
            continue;
        };
        if exe != Path::new(BINARY) {
            continue;
        }
        let Ok(environ) = std::fs::read(entry.path().join("environ")) else {
            continue;
        };
        if environ.split(|b| *b == 0).any(|var| var == wanted) {
            pids.push(pid);
        }
    }
    pids
}

/// Wait until `condition` holds, at most `timeout`.
fn wait_for(what: &str, timeout: Duration, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn options(port: u16) -> ServerOptions {
    ServerOptions {
        port,
        buffer_ms: 500,
        mode: Mode::Shared,
        ..ServerOptions::default()
    }
}

fn stream(name: &str) -> StreamOptions {
    StreamOptions {
        name: name.into(),
        codec: "pcm".into(),
        format: PcmFormat {
            rate: 48000,
            bits: 16,
            channels: 2,
        },
        log_id: format!("autospawn-{name}"),
    }
}

/// 10 ms of silence, stamped now.
fn send(handle: &server::StreamHandle) -> Result<(), gst_snapcast_engine::EngineError> {
    handle.send_pcm(vec![0; 480 * 4], now_usec())
}

#[test]
fn senders_spawn_one_daemon_survive_its_crash_and_let_it_exit() {
    // Short: a socket path must fit into 108 bytes.
    let runtime_dir: PathBuf = std::env::temp_dir().join(format!("gsd-{}", std::process::id()));
    std::fs::create_dir_all(&runtime_dir).unwrap();
    // SAFETY: the only test in this binary sets them before the engine starts
    // any thread that reads the environment.
    unsafe {
        std::env::set_var("XDG_RUNTIME_DIR", &runtime_dir);
        std::env::set_var("GST_SNAPCAST_DAEMON", BINARY);
        std::env::set_var("GST_SNAPCAST_DAEMON_GRACE_SECS", "0.5");
    }
    let socket = ipc::socket_path(&runtime_dir.join(ipc::DIR_NAME));
    let port = free_port();

    // Four senders at once find no daemon; each may spawn one, one wins.
    let handles: Vec<_> = (0..4)
        .map(|i| {
            std::thread::spawn(move || {
                server::acquire_stream(&options(port), &stream(&format!("s{i}"))).unwrap()
            })
        })
        .collect();
    let handles: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    for h in &handles {
        assert!(h.is_shared());
        assert_eq!(h.port(), port);
        send(h).unwrap();
    }
    // The losers give up the lock wait (2 s) and exit.
    wait_for("a single daemon", Duration::from_secs(5), || {
        daemons(&runtime_dir).len() == 1
    });
    let first = daemons(&runtime_dir)[0];

    // Kill it: the next sends reconnect, which spawns a new daemon.
    let killed = std::process::Command::new("kill")
        .args(["-9", &first.to_string()])
        .status()
        .unwrap();
    assert!(killed.success());
    // Dead means reaped (our wait thread does that): an exiting process loses its
    // `exe` link early, and its main thread is a zombie while the other threads
    // still hold its sockets open. Only then do the senders' writes fail.
    wait_for(
        "the old daemon to be reaped",
        Duration::from_secs(5),
        || !Path::new(&format!("/proc/{first}")).exists(),
    );
    for h in &handles {
        for _ in 0..3 {
            send(h).expect("a send after the daemon was killed reconnects");
        }
    }

    let second = daemons(&runtime_dir);
    let log_path = runtime_dir.join(ipc::DIR_NAME).join("daemon.log");
    assert_eq!(
        second.len(),
        1,
        "{second:?}; log: {}",
        std::fs::read_to_string(&log_path).unwrap_or_default()
    );
    assert_ne!(second[0], first);

    // All senders gone: the daemon exits after its grace period and removes its socket.
    drop(handles);
    wait_for("the daemon to exit", Duration::from_secs(5), || {
        daemons(&runtime_dir).is_empty()
    });
    assert!(!socket.exists(), "socket removed");
    let log = std::fs::read_to_string(runtime_dir.join(ipc::DIR_NAME).join("daemon.log")).unwrap();
    assert!(log.contains("No senders, exiting"), "{log}");

    let _ = std::fs::remove_dir_all(&runtime_dir);
}
