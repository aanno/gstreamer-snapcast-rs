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

> **Status: early skeleton.** Both elements register, negotiate caps and
> expose their properties. `snapsink` currently drops buffers and `snapsrc`
> fails on start with "not implemented". The first milestone is a
> `snapsink ! (Snapcast) ! snapsrc` PCM loopback.

Background and design discussion:
[docs/gstreamer-plugins/](docs/gstreamer-plugins/) (see `dlna-rs-gstreamer-2.md`
first).

## Repository layout

```text
.
├── Cargo.toml                 # Cargo workspace (excludes snapcast-rs/)
├── crates/
│   ├── snapcast-engine/       # GStreamer-free glue: tokio runtime, shared server/client lifecycle
│   ├── gst-plugin-snapsink/   # -> libgstsnapsink.so  (element: snapsink)
│   └── gst-plugin-snapsrc/    # -> libgstsnapsrc.so   (element: snapsrc)
├── snapcast-rs/               # git submodule (fork of metaneutrons/snapcast-rs), own Cargo workspace
├── docs/                      # design notes
└── .devcontainer/             # VS Code devcontainer (Rust + GStreamer dev packages)
```

`snapcast-rs` stays a separate project. We depend on its library crates
(`snapcast-server`, `snapcast-client`, `snapcast-proto`) through path
dependencies declared once in the root `Cargo.toml`. Changes to snapcast-rs
are committed in the submodule (and ideally upstreamed), never mixed into
commits of this repository.

## Getting started

### Devcontainer (recommended)

1. Clone with submodules:
   `git clone --recurse-submodules git@github.com:aanno/gstreamer-snapcast-rs.git`
2. Open `gstreamer-snapcast-rs.code-workspace` in VS Code and choose
   **Reopen in Container**.

The container provides the Rust toolchain (pinned to stable via
`rust-toolchain.toml`), the GStreamer 1.26 development packages, `gst-launch-1.0`
and `gst-inspect-1.0`, rust-analyzer (checking with clippy) and CodeLLDB.
`GST_PLUGIN_PATH` points to `target/debug`, so freshly built plugins are found
automatically.

### Without the devcontainer

You need a Rust toolchain (>= 1.94.1) and the GStreamer development packages.
On Debian/Ubuntu:

```sh
sudo apt install pkg-config libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev \
    gstreamer1.0-tools gstreamer1.0-plugins-base gstreamer1.0-plugins-good
git submodule update --init --recursive
```

## Development

```sh
make build     # cargo build --workspace
make test      # cargo test --workspace
make clippy    # lints, warnings are errors
make fmt       # rustfmt check (make fmt-fix to apply)
make doc       # rustdoc, warnings are errors (cargo doc --open to browse)
make check     # all of the above (except build)
make inspect   # gst-inspect-1.0 the freshly built plugins
```

Try the elements from the command line:

```sh
export GST_PLUGIN_PATH=$PWD/target/debug     # already set in the devcontainer
gst-inspect-1.0 snapsink
GST_DEBUG=snapsink:6 gst-launch-1.0 audiotestsrc num-buffers=100 ! snapsink
```

### Debugging

`gstreamer-snapcast-rs.code-workspace` contains CodeLLDB launch
configurations that:

* run `gst-launch-1.0` with a test pipeline around `snapsink` / `snapsrc`, with
  the plugin loaded from `target/debug` (breakpoints in plugin code work), and
* debug each plugin's integration tests.

`GST_DEBUG=snapsink:6` (or `snapsrc:6`) enables the elements' debug logs.

## License

GPL-3.0-only, the same license as snapcast-rs. See [LICENSE](LICENSE).
GStreamer reports the plugins' license as `GPL`, because it only accepts a
fixed set of license names.
