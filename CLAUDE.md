# CLAUDE.md

Guidance for Claude Code (and other agents) working in this repository.

## What this is

Two GStreamer plugins in Rust that bridge GStreamer and Snapcast multiroom audio,
built on the embeddable engines of the `snapcast-rs` submodule:

- `snapsink` (`crates/gst-plugin-snapsink`, `GstBaseSink`): PCM in, embedded Snapcast **server** stream out.
- `snapsrc` (`crates/gst-plugin-snapsrc`, live `GstPushSrc`): embedded Snapcast **client** in, timestamped PCM out.
- `snapcast-engine` (`crates/snapcast-engine`): GStreamer-free glue (tokio runtime,
  servers shared per port, client lifecycle). Features `server` / `client`.

The design rationale is in `docs/gstreamer-plugins/dlna-rs-gstreamer-2.md`.
`-1.md` is broader DLNA/UPnP background; read it only when needed.

**Read `docs/claude/memory.md` first.** It holds the current state, the decisions, the
known limitations (L1–L10) and the plan. Keep it up to date: update State and Plan when
a step is done, and record new decisions with a date.

Short version: the PCM loopback works (`crates/gst-plugin-snapsrc/tests/loopback.rs`), and
interop with the C++ `snapclient` was verified by hand. Current priority: timing
robustness, then multiple streams per server.

## Commands

```sh
make check                         # fmt + clippy (-D warnings) + tests + rustdoc (-D warnings)
cargo test -p gst-plugin-snapsink  # single crate
make inspect                       # gst-inspect-1.0 the built plugins
GST_PLUGIN_PATH=$PWD/target/debug GST_DEBUG=snapsink:6 gst-launch-1.0 audiotestsrc num-buffers=50 ! snapsink
```

Run `make check` before declaring work done.

## Architecture rules

- **`snapcast-engine` must never depend on GStreamer.** Plugin crates only translate
  buffers/caps/events to engine calls.
- **Android is a near-term target for the sender** (`snapsink` and the engine's `server`
  feature). Keep that path portable: no Linux-only APIs and no extra system C libraries.
  The receiver (`snapsrc`, `client` feature) is desktop-only. Android already has a
  websocket-based Snapcast client app.
- **One shared embedded `SnapServer` per port**, owned by `snapcast-engine`
  (`server::acquire_stream`). Each `snapsink` feeds one named stream (`stream` property).
  Do not create servers anywhere else.
- Async work runs on `snapcast_engine::runtime()`. Never block a GStreamer streaming
  thread on async code without a bound or timeout, and never create ad-hoc runtimes.
- **Timing:** `snapsrc` must derive PTS from Snapcast server time, never from network
  arrival time. `snapsink` must map buffer PTS/running time onto the server timeline.
  Both must answer latency queries properly. This is the hard part of the project;
  document clock decisions in code comments.
- Interoperability with C++ Snapcast: use PCM or FLAC. `f32lz4` is snapcast-rs-only.

## snapcast-rs submodule

- `snapcast-rs/` is a git submodule (fork `aanno/snapcast-rs` of
  `metaneutrons/snapcast-rs`) with **its own Cargo workspace**, excluded from ours.
  Its crates are wired in via path deps in the root `Cargo.toml` `[workspace.dependencies]`.
  Add new uses there, not in individual crates.
- **snapcast-rs changes are meant to go upstream to `metaneutrons/snapcast-rs`.** Each one
  is a small, self-contained PR candidate with tests and docs. Follow its conventions (its
  own `make check`, conventional commits, its git hooks), and keep project-specific hacks
  out of it. Develop in the fork (submodule) and commit there separately. Afterwards,
  commit the updated submodule pointer in this repo as its own commit.
- Prefer working around a limitation in `snapcast-engine` if that stays clean. Change
  snapcast-rs when the right fix belongs in the library (e.g. runtime `add_stream`/`remove_stream`).
- Keep the `version` of the path deps in sync with `snapcast-rs/Cargo.toml`.

## Code conventions

- Edition 2024, MSRV 1.94.1 (dictated by snapcast-rs). Toolchain: `rust-toolchain.toml` (stable).
- gstreamer-rs 0.25. Follow the gst-plugins-rs layout: `src/<element>/mod.rs` holds the
  `glib::wrapper!` and `register()`, and `src/<element>/imp.rs` holds the subclass implementation.
  Use `LazyLock` statics for the debug category, properties, metadata and pad templates.
- Logging inside elements: `gst::{error,warning,info,debug,trace}!(CAT, imp = self, ...)`.
  In `snapcast-engine`, use `tracing` (as snapcast-rs does). Do not use `println!`.
- Errors posted to the bus: `gst::error_msg!` / `gst::element_imp_error!`. No `unwrap()`
  on runtime data paths. `unwrap()` on mutex locks and `expect("type checked upstream")`
  in property setters are fine.
- Properties that cannot change while running use `.mutable_ready()`.
- Lints are configured in the root `Cargo.toml` (`[workspace.lints]`). Every crate has
  `[lints] workspace = true`. Public items of `snapcast-engine` need docs.
- License: GPL-3.0-only (matches snapcast-rs). `plugin_define!` uses `"GPL"`, because
  GStreamer only accepts fixed license names.

## Tests

- Integration tests live in `crates/<plugin>/tests/`. They call `gst::init()` once, then
  `<lib>::plugin_register_static()`, and then build pipelines with `gst::parse::launch`.
  Elements from gst-plugins-base/good (`audiotestsrc`, `audioconvert`, `fakesink`)
  are available in the devcontainer.
- Bound every pipeline wait with a timeout (`bus.timed_pop_filtered`).
- Network tests should bind ephemeral ports so they can run in parallel: `snapsink port=0`,
  then read `bound-port`.
- The C++ reference `snapserver`/`snapclient` (Debian 0.31; upstream is 0.34) are installed
  in the devcontainer for interop testing only. Never link against them. Tests that need
  them must be `#[ignore]`d or env-gated, so `make check` works without them.

## Environment

- Devcontainer: `.devcontainer/Dockerfile` (Debian trixie, GStreamer 1.26 dev packages,
  C++ snapserver/snapclient 0.31 as interop peers).
  `GST_PLUGIN_PATH=${workspaceFolder}/target/debug` is set.
- VS Code tasks, CodeLLDB launch configs and rust-analyzer settings are in
  `gstreamer-snapcast-rs.code-workspace` (`.vscode/` is git-ignored).
- `Cargo.lock` is committed (the plugins are shipped artifacts).
