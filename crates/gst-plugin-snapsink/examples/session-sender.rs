//! A media file as a Snapcast session: plays it into a `snapsink` stream and obeys
//! the stream's player commands (`Stream.Control` from controllers, e.g. a DLNA
//! renderer following the session, `docs/dlna.md`). It stands in for dms until dms
//! feeds sessions itself (PoC step 4), and shows what that feeder must do:
//!
//! - `play`, `pause`, `playPause`: the pipeline's state (`snapsink` reports it as
//!   the stream's `playbackStatus`);
//! - `setPosition {"position": s}`, `seek {"offset": s}`: a flushing seek;
//! - `stop`: pause and go back to the start;
//! - the end of the file: `snapsink` reports `stopped`; `play` starts it again.
//!
//! ```sh
//! cargo run -p gst-plugin-snapsink --features control --example session-sender -- \
//!     [--mode embedded|auto|shared] [--paused] [--idle-exit SECS] \
//!     [--port PORT] [--control-port PORT] \
//!     assets/audio/freejazz_medley.flac [stream [port [control-port]]]
//! ```
//!
//! - `stream` (default `default`), `port` (1704) and `control-port` (port + 1): the
//!   stream and the server.
//! - `--mode`: where the server runs. `embedded` (the default) runs it in this
//!   process, alone on its port. `shared` uses the per-user daemon, so several
//!   senders (sessions) share one server. That's how dms runs it, one process per
//!   session. (`auto` falls back to embedded without the daemon, and then a second
//!   session can't bind the port.)
//! - `--paused`: preroll, but wait for a `play` command.
//! - `--idle-exit SECS`: exit after the session hasn't played for that long
//!   (paused, or ended). dms relies on that to close sessions.
//!
//! Once the stream exists on the server, it prints one line
//! `Session ready: snapcast-tcp://<this host>:<port>/<stream>?control=<control-port>`.
//! dms waits for that line before answering the renderer.

use std::time::{Duration, Instant};

use gst::prelude::*;

fn uri_of(arg: &str) -> String {
    if arg.contains("://") {
        return arg.to_owned();
    }
    // A missing file is the caller's mistake (e.g. dms's -path), not a bug: say so
    // in one line and exit 1, without a panic's backtrace note.
    let path = std::fs::canonicalize(arg).unwrap_or_else(|e| {
        eprintln!("error: cannot open {arg}: {e}");
        std::process::exit(1);
    });
    gst::glib::filename_to_uri(&path, None)
        .expect("a file URI")
        .to_string()
}

fn seek_to(pipeline: &gst::Element, secs: f64) {
    let target = gst::ClockTime::from_nseconds((secs.max(0.0) * 1e9) as u64);
    println!("seek to {target}");
    if pipeline
        .seek_simple(gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE, target)
        .is_err()
    {
        eprintln!("seek to {target} failed");
    }
}

struct Options {
    file: String,
    stream: String,
    port: u16,
    control: u16,
    mode: String,
    paused: bool,
    idle_exit: Option<Duration>,
}

fn usage() -> ! {
    eprintln!(
        "usage: session-sender [--mode embedded|auto|shared] [--paused] [--idle-exit SECS] \\
         [--port PORT] [--control-port PORT] <file or URI> [stream [port [control-port]]]"
    );
    std::process::exit(2);
}

fn port_arg(arg: Option<String>) -> u16 {
    arg.and_then(|p| p.parse().ok()).unwrap_or_else(|| usage())
}

fn options() -> Options {
    let mut positional = Vec::new();
    let mut mode = "embedded".to_owned();
    let mut paused = false;
    let mut idle_exit = None;
    let (mut port_opt, mut control_opt) = (None, None);
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--mode" => mode = args.next().unwrap_or_else(|| usage()),
            "--paused" => paused = true,
            "--port" => port_opt = Some(port_arg(args.next())),
            "--control-port" => control_opt = Some(port_arg(args.next())),
            "--idle-exit" => {
                let secs: u64 = args
                    .next()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or_else(|| usage());
                idle_exit = Some(Duration::from_secs(secs));
            }
            a if a.starts_with("--") => usage(),
            _ => positional.push(arg),
        }
    }
    let mut positional = positional.into_iter();
    let file = positional.next().unwrap_or_else(|| usage());
    let stream = positional.next().unwrap_or_else(|| "default".to_owned());
    // Ports as positional arguments (after the stream) or as options.
    let port = positional
        .next()
        .map(|p| port_arg(Some(p)))
        .or(port_opt)
        .unwrap_or(1704);
    let control = positional
        .next()
        .map(|p| port_arg(Some(p)))
        .or(control_opt)
        .unwrap_or(port + 1);
    Options {
        file,
        stream,
        port,
        control,
        mode,
        paused,
        idle_exit,
    }
}

fn main() {
    let o = options();
    gst::init().expect("GStreamer");
    gstsnapsink::plugin_register_static().expect("snapsink");
    let pipeline = gst::parse::launch(&format!(
        "uridecodebin uri=\"{}\" ! audioconvert ! audioresample \
         ! audio/x-raw,format=S16LE ! snapsink name=sink stream=\"{}\" port={} \
           mode={} control-port={} controls=play+pause+seek",
        uri_of(&o.file),
        o.stream,
        o.port,
        o.mode,
        o.control
    ))
    .expect("pipeline");
    let initial = if o.paused {
        gst::State::Paused
    } else {
        gst::State::Playing
    };
    pipeline.set_state(initial).expect("start");
    // Prerolled: snapsink has its caps, so the stream exists on the server.
    let (result, _, _) = pipeline.state(gst::ClockTime::from_seconds(10));
    if result.is_err() {
        let bus = pipeline.bus().expect("bus");
        while let Some(message) =
            bus.pop_filtered(&[gst::MessageType::Error, gst::MessageType::Warning])
        {
            match message.view() {
                gst::MessageView::Error(e) => eprintln!("error: {} ({:?})", e.error(), e.debug()),
                gst::MessageView::Warning(w) => {
                    eprintln!("warning: {} ({:?})", w.error(), w.debug())
                }
                _ => {}
            }
        }
        eprintln!("the pipeline didn't start");
        std::process::exit(1);
    }
    println!(
        "Session ready: snapcast-tcp://<this host>:{}/{}?control={} ({}, {})",
        o.port,
        o.stream,
        o.control,
        o.file,
        if o.paused { "paused" } else { "playing" }
    );

    let bus = pipeline.bus().expect("bus");
    let mut ended = false;
    let mut last_played = Instant::now();
    loop {
        let (_, state, _) = pipeline.state(gst::ClockTime::ZERO);
        if state == gst::State::Playing && !ended {
            last_played = Instant::now();
        } else if o
            .idle_exit
            .is_some_and(|idle| last_played.elapsed() >= idle)
        {
            println!(
                "idle: the session hasn't played for {:?}, closing it",
                o.idle_exit.unwrap_or_default()
            );
            break;
        }
        let Some(message) = bus.timed_pop(gst::ClockTime::from_seconds(1)) else {
            continue;
        };
        match message.view() {
            gst::MessageView::Eos(_) => {
                println!("end of the track: the session is stopped (play starts it again)");
                ended = true;
            }
            gst::MessageView::Error(e) => {
                eprintln!("error: {} ({:?})", e.error(), e.debug());
                break;
            }
            gst::MessageView::Warning(w) => {
                eprintln!("warning: {} ({:?})", w.error(), w.debug());
            }
            gst::MessageView::Element(e) => {
                let Some(s) = e
                    .structure()
                    .filter(|s| s.name() == "snapcast-stream-control")
                else {
                    continue;
                };
                let command = s.get::<&str>("command").unwrap_or_default();
                let params: serde_json::Value =
                    serde_json::from_str(s.get::<&str>("params").unwrap_or("null"))
                        .unwrap_or_default();
                // `params` is null for commands without parameters (play, pause).
                if params.is_null() {
                    println!("command {command}");
                } else {
                    println!("command {command} {params}");
                }
                let (_, state, _) = pipeline.state(gst::ClockTime::from_mseconds(100));
                match command {
                    "play" => {
                        if std::mem::take(&mut ended) {
                            seek_to(&pipeline, 0.0);
                        }
                        let _ = pipeline.set_state(gst::State::Playing);
                    }
                    "pause" => {
                        let _ = pipeline.set_state(gst::State::Paused);
                    }
                    "playPause" => {
                        let next = if state == gst::State::Playing {
                            gst::State::Paused
                        } else {
                            gst::State::Playing
                        };
                        let _ = pipeline.set_state(next);
                    }
                    "stop" => {
                        let _ = pipeline.set_state(gst::State::Paused);
                        seek_to(&pipeline, 0.0);
                    }
                    "setPosition" => {
                        ended = false;
                        seek_to(&pipeline, params["position"].as_f64().unwrap_or(0.0));
                    }
                    "seek" => {
                        ended = false;
                        let now = pipeline
                            .query_position::<gst::ClockTime>()
                            .map_or(0.0, |t| t.nseconds() as f64 / 1e9);
                        seek_to(&pipeline, now + params["offset"].as_f64().unwrap_or(0.0));
                    }
                    other => println!("(ignored: {other})"),
                }
            }
            _ => {}
        }
    }
    let _ = pipeline.set_state(gst::State::Null);
    std::thread::sleep(Duration::from_millis(100));
}
