# Writing your own pipelines

A cookbook for using `snapsink` and `snapsrc` in your own GStreamer pipelines and scripts,
for readers new to GStreamer. The listening script prints the commands it runs, with a
note per element; that is a good starting point:

```sh
scripts/listen.sh show all        # or: make listen-show (WHAT=send|recv|cpp-server|...)
```

Why these elements exist and how they work inside: [architecture.md](architecture.md).
All properties: the [README](../README.md#element-properties) or `gst-inspect-1.0 snapsink`.

## 1. Make GStreamer find the plugins

- `make install` puts them into `~/.local/share/gstreamer-1.0/plugins`, which GStreamer
  scans by itself.
- Or point `GST_PLUGIN_PATH` at a build: `export GST_PLUGIN_PATH=$PWD/target/release`
  (`make release`) or `target/debug` (`cargo build`). In the devcontainer it is set already.
- Check: `gst-inspect-1.0 snapsink` lists its properties. `make doctor` shows which file
  GStreamer loads.

## 2. gst-launch-1.0 in a few minutes

```sh
gst-launch-1.0 -e audiotestsrc is-live=true wave=ticks ! audio/x-raw,rate=48000,channels=2 ! snapsink
```

- **Elements** are linked with `!`; data flows from left to right. Sources (`…src`) produce
  data, sinks (`…sink`) consume it, everything in between converts or processes.
- **Properties** follow the element as `name=value`. `gst-inspect-1.0 <element>` lists them.
- **Caps filters** such as `audio/x-raw,format=S16LE,rate=48000,channels=2` stand between
  two elements and pin the format there. **Converters** adapt formats around them:
  `audioconvert` (sample format, channels), `audioresample` (rate). When in doubt, put
  `audioconvert ! audioresample` between a source and a sink.
- **Live sources** (`is-live=true`, microphones, `snapsrc`) deliver data in real time.
  Non-live sources (files) deliver as fast as downstream takes it; `snapsink` then paces
  them itself.
- **`-e`**: on Ctrl+C, send end-of-stream first, so that files are finished properly
  (e.g. a WAV header) and everything stops cleanly.
- **Quoting:** gst-launch joins its arguments into one description. Values with spaces or
  commas need quotes *inside* that description, and an empty value must be `""`:
  `snapsrc host='""'` (the single quotes are for the shell). Without them, the next
  property becomes the value.
- **Several chains in one command:** just write them one after the other, separated by a
  space (see 4.5).
- **Preroll:** before a pipeline plays, every sink waits for its first buffer, so that
  playback starts without a gap. A sink with `async=false` (a GstBaseSink property, which
  `snapsink` has) skips that wait. This matters when one sink's data depends on another
  sink already playing; see [Preroll and `async=false`](#preroll-and-async-false).
- **Seeing what happens:**
  - `-v` prints the negotiated caps of every link;
  - `GST_DEBUG=snapsrc:5,snapcast*:4` shows the logs of our elements and of snapcast-rs
    (categories: [architecture.md](architecture.md#debug-categories));
  - `GST_DEBUG_DUMP_DOT_DIR=/tmp` writes graphs of the pipeline (`.dot`, view with Graphviz).

## 3. The two elements in brief

**`snapsink`** (sender) runs an embedded Snapcast server and feeds one stream of it.
- Input: S16LE or S32LE, any rate and channel count (they become the stream's format);
  put `audioconvert ! audioresample ! audio/x-raw,format=S16LE,rate=48000,channels=2`
  in front if the source delivers something else.
- `port` (1704): all `snapsink`s on one port share one server; `stream`: the stream name;
  `codec`: `pcm` or `flac` (both work with every Snapcast client); `buffer-ms`: the
  end-to-end delay; `mdns-name`: announced via mDNS under this name (`""`: not at all).
- Any Snapcast client can connect: `snapsrc`, the C++ `snapclient`, Snapdroid, …
- From GstBaseSink: `sync` (default `true`; with `false`, `snapsink` still paces files to
  real time) and `async` (default `true`; set `async=false` when the same pipeline also
  receives from this server, see [5.4](#preroll-and-async-false)).

**`snapsrc`** (receiver) runs an embedded Snapcast client.
- Output: F32LE, timestamped so that the sound is heard at the Snapcast play-out time.
  Put `audioconvert ! audioresample` before the sink.
- `host`/`port`: the server; an empty host (`host='""'`) finds one via mDNS.
- `latency-ms`: the extra delay of your output device (e.g. Bluetooth), so that it stays
  in sync with the other rooms.
- It sets `slave-method=resample` on the audio sink downstream (`adjust-sink`), which keeps
  the output on the Snapcast timeline; see
  [architecture.md](architecture.md#audio-sink-slaving-adjust-sink).

## 4. Senders

**4.1 Test tone or clicks** (tested):

```sh
gst-launch-1.0 -e audiotestsrc is-live=true wave=ticks volume=0.5 \
    ! audio/x-raw,format=S16LE,rate=48000,channels=2 ! snapsink
```

**4.2 What the desktop plays** (PipeWire; used by `make listen-send SRC=desktop` on the host):

```sh
gst-launch-1.0 -e pipewiresrc stream-properties="props,stream.capture.sink=true" \
    ! audioconvert ! audioresample ! audio/x-raw,format=S16LE,rate=48000,channels=2 \
    ! snapsink stream=desktop
```

Stop local playback on the same machine, or you hear it twice.

**4.3 A file or web radio** (tested with a FLAC file):

```sh
gst-launch-1.0 -e uridecodebin uri=file:///path/to/music.flac \
    ! audioconvert ! audioresample ! audio/x-raw,format=S16LE,rate=48000,channels=2 \
    ! snapsink stream=music
```

`uri=https://…` works for web radio too. A file is not live; `snapsink` plays it in real
time and the pipeline ends when the file does.

**4.4 Microphone or line-in:** `autoaudiosrc ! audioconvert ! audioresample ! …` (the
default input), or `alsasrc device=hw:1,0` for a specific card.

**4.5 Two streams** (tested): one server per port and one stream per server for now
(L1), so use two ports:

```sh
gst-launch-1.0 -e \
    audiotestsrc is-live=true ! audio/x-raw,format=S16LE,rate=48000,channels=2 \
        ! snapsink stream=music port=1704 \
    audiotestsrc is-live=true wave=ticks ! audio/x-raw,format=S16LE,rate=48000,channels=2 \
        ! snapsink stream=ticks port=1714
```

**4.6 Mopidy** (not tested yet): Mopidy's usual Snapcast setup writes into a FIFO for a
separate `snapserver`. With `snapsink` installed for the user Mopidy runs as, its output
becomes:

```ini
[audio]
output = audioresample ! audioconvert ! audio/x-raw,rate=48000,channels=2,format=S16LE ! snapsink
```

## 5. Receivers

**5.1 The default output** (tested):

```sh
gst-launch-1.0 -e snapsrc host=192.168.1.10 ! audioconvert ! audioresample ! autoaudiosink
```

**5.2 A specific device**, and a Bluetooth speaker with its own delay:

```sh
gst-launch-1.0 -e snapsrc ! audioconvert ! audioresample ! alsasink device=plughw:1,0
gst-launch-1.0 -e snapsrc latency-ms=150 ! audioconvert ! audioresample ! pulsesink device=bluez_output.XX_XX
```

(`pactl list short sinks` lists PulseAudio/PipeWire device names.) Two receivers on one
host are fine: each gets its own client id automatically (`instance`).

**5.3 Processing and recording** (tested): EQ in front of the output, or a WAV file:

```sh
gst-launch-1.0 -e snapsrc ! audioconvert ! equalizer-10bands band0=6 band1=4 \
    ! audioconvert ! audioresample ! autoaudiosink
gst-launch-1.0 -e snapsrc ! audioconvert ! wavenc ! filesink location=room.wav
```

<a id="preroll-and-async-false"></a>

**5.4 Sender and receiver in one pipeline: preroll and `async=false`** (tested). Rare in
practice (sender and receivers normally run in separate processes, usually on separate
machines), but handy for demos and tests. It needs `async=false` on one of the sinks,
best on `snapsink` (`autoaudiosink` has no such property; a plain sink such as
`alsasink` or `filesink` does):

```sh
gst-launch-1.0 -e \
    audiotestsrc is-live=true wave=ticks ! audio/x-raw,format=S16LE,rate=48000,channels=2 \
        ! snapsink async=false \
    snapsrc ! audioconvert ! audioresample ! autoaudiosink
```

Without it, nothing plays and Ctrl+C hangs (L14): a pipeline goes to PLAYING only after
every sink has received its first buffer ("preroll"). `snapsink` prerolls and then waits
for PLAYING, but the receiver's sink gets data only once `snapsink` sends, which it only
does in PLAYING. `async=false` lets one sink skip the preroll wait. Two separate
`gst-launch-1.0` processes (like `make listen-all`) don't have this problem.

The same applies to your own applications: whenever one sink's first buffer depends on
another sink already playing (here through the network), give that other sink
`async=false`. Leaving the default `async=true` on `snapsink` costs nothing in the usual
setups.

## 6. From gst-launch to your own program

The text after `gst-launch-1.0` is a pipeline description that programs can use as is.
In Rust (with `gstreamer = "0.25"`; the plugins must be installed or on `GST_PLUGIN_PATH`):

```rust
use gst::prelude::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    gst::init()?;
    // The same text as on the gst-launch-1.0 command line.
    let pipeline = gst::parse::launch(
        "snapsrc host=192.168.1.10 ! audioconvert ! audioresample ! autoaudiosink",
    )?;
    pipeline.set_state(gst::State::Playing)?;

    // Wait until the stream ends or fails.
    let bus = pipeline.bus().expect("a pipeline has a bus");
    for msg in bus.iter_timed(gst::ClockTime::NONE) {
        match msg.view() {
            gst::MessageView::Eos(..) => break,
            gst::MessageView::Error(err) => {
                eprintln!("{}: {}", err.src().map(|s| s.path_string()).unwrap_or_default(), err.error());
                break;
            }
            _ => {}
        }
    }
    pipeline.set_state(gst::State::Null)?;
    Ok(())
}
```

(Compiled against gstreamer-rs 0.25, 2026-11-21.) Python's `Gst.parse_launch()` takes the
same text. Our integration tests (`crates/gst-plugin-*/tests/`) are further examples:
they build pipelines this way, read properties such as `bound-port`, and pull buffers
with `appsink`.
