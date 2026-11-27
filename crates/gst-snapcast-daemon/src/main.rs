//! `gst-snapcast-daemon`: the shared Snapcast server of one user. See the library
//! docs and `docs/shared-server.md`.
//!
//! Usage: `gst-snapcast-daemon [--socket PATH] [--grace-secs S] [--log FILE]`.
//!
//! - `--socket`: default `$XDG_RUNTIME_DIR/gst-snapcast/daemon.sock`. The lock file
//!   is next to it, with the extension `.lock`; whoever holds it is the one daemon.
//!   A second daemon waits up to 2 s for the lock, then exits quietly.
//! - `--grace-secs`: exit after this long without senders (default 5, or
//!   `GST_SNAPCAST_DAEMON_GRACE_SECS`).
//! - `--log`: log into this file (truncated) instead of stderr; senders that
//!   autospawn the daemon pass `daemon.log` next to the socket.
//!
//! Log level: `GST_SNAPCAST_DAEMON_LOG` (`error`, `warn`, `info` (default), `debug`,
//! `trace`).

use std::fs::File;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use gst_snapcast_engine::ipc;
use tracing_subscriber::filter::LevelFilter;

const DEFAULT_GRACE_SECS: f64 = 5.0;
/// How long a second daemon waits for the lock (the first one may be exiting).
const LOCK_WAIT: Duration = Duration::from_secs(2);

struct Args {
    socket: Option<PathBuf>,
    grace: Duration,
    log: Option<PathBuf>,
}

fn parse_args() -> Result<Option<Args>, String> {
    let grace_secs = |v: &str| -> Result<Duration, String> {
        v.parse::<f64>()
            .ok()
            .filter(|s| s.is_finite() && *s >= 0.0)
            .map(Duration::from_secs_f64)
            .ok_or_else(|| format!("invalid grace period '{v}'"))
    };
    let mut args = Args {
        socket: None,
        grace: match std::env::var("GST_SNAPCAST_DAEMON_GRACE_SECS") {
            Ok(v) => grace_secs(&v)?,
            Err(_) => Duration::from_secs_f64(DEFAULT_GRACE_SECS),
        },
        log: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut value = || it.next().ok_or_else(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "--socket" => args.socket = Some(value()?.into()),
            "--grace-secs" => args.grace = grace_secs(&value()?)?,
            "--log" => args.log = Some(value()?.into()),
            "--version" => {
                println!("gst-snapcast-daemon {}", gst_snapcast_daemon::VERSION);
                return Ok(None);
            }
            other => return Err(format!("unknown argument '{other}'")),
        }
    }
    Ok(Some(args))
}

fn socket_path(arg: Option<PathBuf>) -> Result<PathBuf, String> {
    if let Some(socket) = arg {
        return Ok(socket);
    }
    let dir = ipc::runtime_dir().ok_or("XDG_RUNTIME_DIR is not set; give --socket")?;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)
        .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    Ok(ipc::socket_path(&dir))
}

/// Take the daemon lock, waiting up to [`LOCK_WAIT`]. `None`: another daemon has it.
fn lock(path: &PathBuf) -> Result<Option<File>, String> {
    let file = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    let deadline = Instant::now() + LOCK_WAIT;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(Some(file)),
            Err(std::fs::TryLockError::WouldBlock) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(std::fs::TryLockError::WouldBlock) => return Ok(None),
            Err(std::fs::TryLockError::Error(e)) => {
                return Err(format!("cannot lock {}: {e}", path.display()));
            }
        }
    }
}

fn init_logging(log: Option<&PathBuf>) -> Result<(), String> {
    let level = std::env::var("GST_SNAPCAST_DAEMON_LOG")
        .ok()
        .and_then(|v| v.parse::<LevelFilter>().ok())
        .unwrap_or(LevelFilter::INFO);
    let builder = tracing_subscriber::fmt().with_max_level(level);
    match log {
        Some(path) => {
            let file =
                File::create(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
            builder.with_writer(std::sync::Mutex::new(file)).init();
        }
        None => builder.with_writer(std::io::stderr).init(),
    }
    Ok(())
}

fn run() -> Result<(), String> {
    let Some(args) = parse_args()? else {
        return Ok(());
    };
    let socket = socket_path(args.socket)?;
    ipc::check_socket_path(&socket).map_err(|e| e.to_string())?;
    let lock_path = socket.with_extension("lock");
    let Some(_lock) = lock(&lock_path)? else {
        // Not an error: several senders may spawn a daemon at the same time.
        eprintln!(
            "gst-snapcast-daemon: another daemon holds {}",
            lock_path.display()
        );
        return Ok(());
    };
    // From here on, this is the one daemon.
    init_logging(args.log.as_ref())?;

    // A socket left by a daemon that died is ours to replace.
    if std::fs::symlink_metadata(&socket).is_ok_and(|m| m.file_type().is_socket()) {
        let _ = std::fs::remove_file(&socket);
    }
    let listener = UnixListener::bind(&socket)
        .map_err(|e| format!("cannot listen on {}: {e}", socket.display()))?;
    tracing::info!(
        socket = %socket.display(),
        grace = ?args.grace,
        version = gst_snapcast_daemon::VERSION,
        pid = std::process::id(),
        "Daemon listening"
    );
    let result = gst_snapcast_daemon::serve(&listener, Some(args.grace));
    // Remove the socket before the lock is released, so that senders find no
    // daemon (and spawn a new one) rather than a dead socket.
    let _ = std::fs::remove_file(&socket);
    result.map_err(|e| format!("serving failed: {e}"))
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            if tracing::dispatcher::has_been_set() {
                tracing::error!("{e}");
            } else {
                eprintln!("gst-snapcast-daemon: {e}");
            }
            ExitCode::FAILURE
        }
    }
}
