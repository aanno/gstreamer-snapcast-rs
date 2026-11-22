//! `gst-snapcast-daemon`: the shared Snapcast server of one user. See the library
//! docs and `docs/shared-server.md`.
//!
//! Usage: `gst-snapcast-daemon [--socket PATH]`. Without `--socket`, it listens on
//! `$XDG_RUNTIME_DIR/gst-snapcast/daemon.sock`. Log level: `GST_SNAPCAST_DAEMON_LOG`
//! (`error`, `warn`, `info` (default), `debug`, `trace`); the log goes to stderr.

use std::os::unix::fs::{DirBuilderExt, FileTypeExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::process::ExitCode;

use gst_snapcast_engine::ipc;
use tracing_subscriber::filter::LevelFilter;

fn usage() -> ExitCode {
    eprintln!("usage: gst-snapcast-daemon [--socket PATH]");
    ExitCode::from(2)
}

fn main() -> ExitCode {
    let level = std::env::var("GST_SNAPCAST_DAEMON_LOG")
        .ok()
        .and_then(|v| v.parse::<LevelFilter>().ok())
        .unwrap_or(LevelFilter::INFO);
    tracing_subscriber::fmt()
        .with_max_level(level)
        .with_writer(std::io::stderr)
        .init();

    let mut socket = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--socket" => match args.next() {
                Some(path) => socket = Some(PathBuf::from(path)),
                None => return usage(),
            },
            "--version" => {
                println!("gst-snapcast-daemon {}", gst_snapcast_daemon::VERSION);
                return ExitCode::SUCCESS;
            }
            _ => return usage(),
        }
    }
    let socket = match socket {
        Some(socket) => socket,
        None => match ipc::runtime_dir() {
            Some(dir) => {
                if let Err(e) = std::fs::DirBuilder::new()
                    .recursive(true)
                    .mode(0o700)
                    .create(&dir)
                {
                    tracing::error!(dir = %dir.display(), error = %e, "Cannot create the runtime directory");
                    return ExitCode::FAILURE;
                }
                ipc::socket_path(&dir)
            }
            None => {
                tracing::error!("XDG_RUNTIME_DIR is not set; give --socket");
                return ExitCode::FAILURE;
            }
        },
    };

    // A socket file left by a daemon that died is removed; a live one means
    // another daemon runs. (Step 3 replaces this check with a lock file.)
    if std::fs::symlink_metadata(&socket).is_ok_and(|m| m.file_type().is_socket()) {
        if UnixStream::connect(&socket).is_ok() {
            tracing::error!(socket = %socket.display(), "Another daemon is running");
            return ExitCode::FAILURE;
        }
        let _ = std::fs::remove_file(&socket);
    }
    let listener = match UnixListener::bind(&socket) {
        Ok(listener) => listener,
        Err(e) => {
            tracing::error!(socket = %socket.display(), error = %e, "Cannot listen");
            return ExitCode::FAILURE;
        }
    };
    tracing::info!(
        socket = %socket.display(),
        version = gst_snapcast_daemon::VERSION,
        "Daemon listening"
    );
    match gst_snapcast_daemon::serve(&listener) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!(error = %e, "Daemon failed");
            ExitCode::FAILURE
        }
    }
}
