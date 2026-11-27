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
//!     assets/audio/freejazz_medley.flac [stream [port [control-port]]]
//! ```
//!
//! The server runs in this process (`mode=embedded`) on `port` (1704) with JSON-RPC
//! on `control-port` (1705), and the stream is named `stream` (`default`).

use std::time::Duration;

use gst::prelude::*;

fn uri_of(arg: &str) -> String {
    if arg.contains("://") {
        return arg.to_owned();
    }
    let path = std::fs::canonicalize(arg).unwrap_or_else(|e| panic!("{arg}: {e}"));
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

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(file) = args.first() else {
        eprintln!("usage: session-sender <file or URI> [stream [port [control-port]]]");
        std::process::exit(2);
    };
    let stream = args.get(1).map_or("default", String::as_str);
    let port: u16 = args.get(2).map_or(1704, |p| p.parse().expect("port"));
    let control: u16 = args
        .get(3)
        .map_or(port + 1, |p| p.parse().expect("control port"));

    gst::init().expect("GStreamer");
    gstsnapsink::plugin_register_static().expect("snapsink");
    let pipeline = gst::parse::launch(&format!(
        "uridecodebin uri=\"{}\" ! audioconvert ! audioresample \
         ! audio/x-raw,format=S16LE ! snapsink name=sink stream={stream} port={port} \
           mode=embedded control-port={control} controls=play+pause+seek",
        uri_of(file)
    ))
    .expect("pipeline");
    pipeline
        .set_state(gst::State::Playing)
        .expect("start playing");
    println!(
        "Session: snapcast-tcp://<this host>:{port}/{stream}?control={control} (playing {file})"
    );

    let bus = pipeline.bus().expect("bus");
    let mut ended = false;
    for message in bus.iter_timed(gst::ClockTime::NONE) {
        match message.view() {
            gst::MessageView::Eos(_) => {
                println!("end of the track: the session is stopped (play starts it again)");
                ended = true;
            }
            gst::MessageView::Error(e) => {
                eprintln!("error: {} ({:?})", e.error(), e.debug());
                break;
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
