# gstreamer-snapcast-rs

GStreamer plugins, written in Rust, that connect GStreamer pipelines to
[Snapcast](https://github.com/badaix/snapcast) synchronized multiroom audio.
The plugins embed the Snapcast server and client engines from
[snapcast-rs](https://github.com/metaneutrons/snapcast-rs), so no external
`snapserver`/`snapclient` process is needed.

| Element    | Plugin crate          | Base class    | Role                                                                  |
|------------|-----------------------|---------------|-----------------------------------------------------------------------|
| `snapsink` | `gst-plugin-snapsink` | `GstBaseSink` | Sender: submits raw PCM to a named stream of an embedded Snapcast server |
| `snapsrc`  | `gst-plugin-snapsrc`  | `GstPushSrc`  | Receiver: embedded Snapcast client that pushes synchronized PCM downstream |

```text
sender:    pipewiresrc ! audioconvert ! audioresample ! snapsink stream=desktop
                                                          │  (Snapcast TCP, port 1704)
receiver:  snapsrc host=sender ! audioconvert ! audioresample ! pipewiresink
```

All `snapsink` elements in one process share a single embedded server; each
element feeds one stream. Standard Snapcast clients (C++ `snapclient`,
Snapdroid, ...) can connect to it, provided you use the PCM or FLAC codec.

### Why Snapcast as a GStreamer element?

Snapcast as a pipeline element: any GStreamer source becomes a multiroom stream, and any
sink a synchronized room, without FIFOs or extra daemons.

- **Sources:** desktop audio (`pipewiresrc`), files and web radio in any codec
  (`uridecodebin`), RTSP, WebRTC, a DLNA renderer.
- **Outputs:** PipeWire, PulseAudio, ALSA, Bluetooth via PipeWire, files, other streams.
- **Processing** in the same pipeline, per room or before sending: EQ, room correction,
  crossovers, resampling.
- **Applications that already use GStreamer** get multiroom with one element. Mopidy, for
  example, usually feeds Snapcast through a FIFO (`… ! filesink location=/tmp/snapfifo`)
  and a separate `snapserver`; here it is `… ! snapsink`.

When not to use it: a plain receiver (e.g. a Raspberry Pi on an amplifier) is simpler with
`snapclient`, and a ready multiroom server with sources, groups and persistence is
`snapserver`. Pros and cons, the comparison with snapcast-rs, licence (GPL-3.0) and
publishing: [docs/architecture.md](docs/architecture.md).

> **Status: feasibility prototype.** `snapsink → embedded server → snapsrc` works for
> `pcm` and `flac`, and interoperates with the C++ Snapcast in both directions
> (`snapsink` → `snapclient`, `snapserver` → `snapsrc`; `make test-interop`). Measured on
> a loopback sound card, receivers play within about 1 ms of each other and of the
> server timeline, also on a drifting card (`make test-aloop`). snapsink announces itself
> via mDNS and snapsrc finds it; snapsrc reconnects after a server restart and applies
> the server's volume and mute. snapcast-rs logs into the GStreamer debug log.
> Limitations: one stream per server port (snapcast-rs cannot add streams at runtime
> yet), S16LE/S32LE input only, no control plane yet (JSON-RPC for Snapweb/Snapdroid;
> next step, P2). The current state, decisions, all known limitations and the plan are
> in [docs/claude/memory.md](docs/claude/memory.md); the architecture in
> [docs/architecture.md](docs/architecture.md).

### Element properties

| Element    | Property      | Default     | Meaning |
|------------|---------------|-------------|---------|
| `snapsink` | `stream`      | `default`   | Snapcast stream name |
|            | `port`        | `1704`      | Server port, shared by all snapsinks using it (`0` = ephemeral) |
|            | `codec`       | `pcm`       | `pcm` or `flac` |
|            | `buffer-ms`   | `1000`      | End-to-end buffer announced to clients |
|            | `mdns-name`   | `gst-snapsink` | Announce the server via mDNS (`_snapcast._tcp`) under this name; empty: don't (never for `port=0`) |
|            | `bound-port`  | (read-only) | Port actually listened on |
|            | `async`       | `true`      | (from GstBaseSink) Wait for the first buffer ("preroll") before the pipeline counts as PAUSED. Set `false` when the same pipeline also contains a `snapsrc` (see below) |
|            | `sync`        | `true`      | (from GstBaseSink) Render on the pipeline clock. With `false`, `snapsink` still paces non-live sources (files) to real time itself |
| `snapsrc`  | `host`/`port` | `127.0.0.1`/`1704` | Server to connect to; empty `host`: find one via mDNS (its port is used) |
|            | `mdns-name`   | (empty)     | With an empty `host`: only the server announced under this name |
|            | `client-name` | `gst-snapsrc` | Name shown in Snapcast controllers |
|            | `instance`    | `0` (auto)  | Tells several clients on one host apart (the server sees `<host-id>#<instance>`); `0` picks the lowest number no other snapsrc on this host uses |
|            | `latency-ms`  | `0`         | Extra local output latency to compensate |
|            | `host-id`     | (MAC address) | Unique client id towards the server, like snapclient's `--hostID` |
|            | `volume`/`mute` | (read-only) | The server's volume (0–100) and mute for this client, applied to the audio |
|            | `adjust-sink` | `true`      | Set `slave-method=resample` on the downstream audio sink (if at `skew`) |

**`snapsink async=false`** is needed in one case: a sender and a receiver of the same
server in **one** pipeline (a demo or test; normally they run in separate processes,
usually on separate machines). With the default, nothing plays and Ctrl+C hangs: the
pipeline waits for every sink to receive its first buffer, but the receiver's sink only
gets data once `snapsink` plays. The same applies to any setup where another sink's
first buffer depends on `snapsink` playing. Details:
[docs/pipelines.md](docs/pipelines.md#preroll-and-async-false).

`snapsrc` outputs F32LE, timestamped so that the sound leaves the speaker at the
Snapcast play-out time (the pipeline latency is compensated). The audio sink follows the
pipeline clock with `slave-method=resample`, which `snapsrc` sets for you
(`adjust-sink`), so a drifting sound card stays on the Snapcast timeline.

Background and design discussion:
[docs/gstreamer-plugins/](docs/gstreamer-plugins/) (see `dlna-rs-gstreamer-2.md`
first).

## Repository layout

```text
.
├── Cargo.toml                 # Cargo workspace (excludes snapcast-rs/)
├── scripts/                   # doctor.sh, listen.sh (used by the Makefile), host test/play helpers,
│                              #   xerj-host.sh (xerj search for Claude Code, on the host)
├── assets/audio/              # CC0 music for listening tests (see its README)
├── crates/
│   ├── gst-snapcast-engine/   # GStreamer-free glue: tokio runtime, shared server/client lifecycle
│   ├── gst-snapcast-common/   # GStreamer-side code shared by both plugins (logging bridge)
│   ├── gst-plugin-snapsink/   # -> libgstsnapsink.so  (element: snapsink)
│   └── gst-plugin-snapsrc/    # -> libgstsnapsrc.so   (element: snapsrc)
├── snapcast-rs/               # git submodule (fork of metaneutrons/snapcast-rs), own Cargo workspace
├── reference/                 # read-only submodules, not fetched by default: C++ snapcast, snapweb
├── docs/
│   ├── architecture.md        # crates and their rules, threads, logging, interfaces
│   ├── pipelines.md           # cookbook: your own pipelines with snapsink/snapsrc
│   ├── testing*.md            # tests, host and devcontainer setup, gotchas
│   ├── gstreamer-plugins/     # design discussion (background)
│   └── claude/memory.md       # current state, decisions, limitations, plan
└── .devcontainer/             # VS Code devcontainer (Rust + GStreamer dev packages)
```

`snapcast-rs` stays a separate project. We depend on its library crates
(`snapcast-server`, `snapcast-client`, `snapcast-proto`) through path
dependencies declared once in the root `Cargo.toml`. Changes to snapcast-rs
are committed in the submodule, never mixed into commits of this repository,
and are meant to be upstreamed to
[metaneutrons/snapcast-rs](https://github.com/metaneutrons/snapcast-rs).

## Getting started

### Devcontainer (recommended)

1. Clone with submodules:
   `git clone --recurse-submodules git@github.com:aanno/gstreamer-snapcast-rs.git`
2. Open `gstreamer-snapcast-rs.code-workspace` in VS Code and choose
   **Reopen in Container**.

The container provides the Rust toolchain (pinned to stable via
`rust-toolchain.toml`), the GStreamer 1.26 development packages, `gst-launch-1.0`
and `gst-inspect-1.0`, rust-analyzer (checking with clippy) and CodeLLDB. It also
installs the reference C++ `snapserver`/`snapclient` (Debian's 0.31.0; upstream is at
0.34.0), for interop testing only.
`GST_PLUGIN_PATH` points to `target/debug`, so freshly built plugins are found
automatically.

The container has no speakers. It gets the host's `snd-aloop` loopback card for the
timing tests (`sudo modprobe snd-aloop` on the host first), and ALSA's default device
plays into it. Claude Code's configuration lives on a named volume, so it survives
rebuilds. Details and gotchas: [docs/testing-devcontainer.md](docs/testing-devcontainer.md).

### Without the devcontainer (e.g. on a Fedora desktop)

You need a Rust toolchain (>= 1.94.1) and the GStreamer development packages; the
package lists, the optional C++ Snapcast peers and `snd-aloop` are in
[docs/testing-host.md](docs/testing-host.md#setup). `make doctor` reports anything that
is still missing.

### Install

```sh
make install            # release build into ~/.local/share/gstreamer-1.0/plugins (no root)
make uninstall
make release && sudo make install-system    # or system-wide (GStreamer's own plugins dir)
sudo make uninstall-system
```

`make doctor` shows which `snapsink`/`snapsrc` files GStreamer actually loads. Note that
`GST_PLUGIN_PATH` (set in the devcontainer) takes precedence over installed copies.

## Development

```sh
make build     # cargo build --workspace
make test      # cargo test --workspace
make clippy    # lints, warnings are errors
make fmt       # rustfmt check (make fmt-fix to apply)
make doc       # rustdoc, warnings are errors (cargo doc --open to browse)
make check     # all of the above (except build)
make inspect   # gst-inspect-1.0 the freshly built plugins
make test-interop  # against the C++ snapclient/snapserver (not part of make check)
make test-aloop    # timing on the snd-aloop card: receiver offset, drift (not part of make check)
make doctor    # tools, elements, plugins and sound devices on this machine
```

### Testing

[docs/testing.md](docs/testing.md) describes all tests, what they need and how to read the
results. Setup, listening tests (`make listen-*`) and gotchas with their solutions:
[on the host](docs/testing-host.md) and [in the devcontainer](docs/testing-devcontainer.md).

### Trying the elements

From the command line:

```sh
export GST_PLUGIN_PATH=$PWD/target/debug     # already set in the devcontainer
gst-inspect-1.0 snapsink

# terminal 1: sender
gst-launch-1.0 audiotestsrc is-live=true ! audio/x-raw,format=S16LE,rate=48000,channels=2 \
    ! snapsink buffer-ms=500
# terminal 2: receiver (or any Snapcast client, e.g. `snapclient -h 127.0.0.1`)
gst-launch-1.0 snapsrc ! audioconvert ! audioresample ! autoaudiosink
```

More recipes (desktop audio, files, several streams, devices, EQ, recording) and an
introduction to `gst-launch-1.0`: [docs/pipelines.md](docs/pipelines.md).
`make listen-show` prints the commands of the listening tests with a note per element.

### Sender and receivers on different machines

That's the normal multiroom setup, and it works as is: `snapsink`'s server listens on all
interfaces (`0.0.0.0`, port `1704` by default). Install the plugins on each machine
(`make install`), then:

```sh
# sender machine, e.g. 192.168.1.10: play something into snapsink
gst-launch-1.0 audiotestsrc is-live=true ! audio/x-raw,format=S16LE,rate=48000,channels=2 \
    ! snapsink

# each receiver machine: name the sender ...
gst-launch-1.0 snapsrc host=192.168.1.10 ! audioconvert ! audioresample ! autoaudiosink
# ... or let mDNS find it (empty host; add mdns-name=... if there are several servers).
# The quotes must reach gst-launch, hence the single quotes around them.
gst-launch-1.0 snapsrc 'host=""' ! audioconvert ! audioresample ! autoaudiosink
# ... or use any Snapcast client, e.g. the C++ one
snapclient -h 192.168.1.10
```

With the listening scripts: `make listen-send` on the sender, `make listen-recv HOST=192.168.1.10`
(or `HOST=` for mDNS) on each receiver.

Things to know:

- **Firewall on the sender:** open the stream port for TCP, and mDNS if receivers should find
  it by themselves. Fedora: `sudo firewall-cmd --add-port=1704/tcp --add-service=mdns`
  (add `--permanent` to keep it).
- **Synchronization** works across machines: every receiver plays each chunk at the sender's
  server time + `buffer-ms`, using Snapcast's own time sync. No NTP or PTP setup is needed.
- **`buffer-ms`** (on `snapsink`, default 1000) must cover the network: over Wi-Fi keep
  ~1000 ms or more, on a wired LAN a few hundred ms are usually enough.
- **Several receivers on one machine** (e.g. line-out and a Bluetooth speaker) are fine:
  each `snapsrc` gets its own instance number automatically.
- **`latency-ms`** on `snapsrc` compensates an output device's own delay (e.g. a Bluetooth
  speaker), so that it stays in sync with the others.

More in [docs/testing-host.md](docs/testing-host.md) (mDNS, several receivers, gotchas).

### Debugging

`gstreamer-snapcast-rs.code-workspace` contains CodeLLDB launch
configurations that:

* run `gst-launch-1.0` with a test pipeline around `snapsink` / `snapsrc`, with
  the plugin loaded from `target/debug` (breakpoints in plugin code work), and
* debug each plugin's integration tests.

`GST_DEBUG=snapsink:6` (or `snapsrc:6`) enables the elements' debug logs.
The logs of snapcast-rs and of the engine are in the GStreamer debug log too, in the
categories `snapcast-client`, `snapcast-server`, `snapcast-engine`, `snapcast-mdns` and
`snapcast-deps`, tagged with the element (`<snapsrc0>`) or the shared server
(`<snapserver:1704>`):

```sh
GST_DEBUG=snapsrc:5,snapcast*:5 gst-launch-1.0 snapsrc ! autoaudiosink
```

`GST_SNAPCAST_TRACING=0` turns that bridge off. Details:
[docs/architecture.md](docs/architecture.md#logging).

## License

GPL-3.0-only, the same license as snapcast-rs. See [LICENSE](LICENSE).
GStreamer reports the plugins' license as `GPL`, because it only accepts a
fixed set of license names.
