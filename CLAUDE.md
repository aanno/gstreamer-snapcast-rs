# CLAUDE.md

Guidance for Claude Code (and other agents) working in this repository.

## What this is

Two GStreamer plugins in Rust that bridge GStreamer and Snapcast multiroom audio,
built on the embeddable engines of the `snapcast-rs` submodule:

- `snapsink` (`crates/gst-plugin-snapsink`, `GstBaseSink`): PCM in, embedded Snapcast **server** stream out.
- `snapsrc` (`crates/gst-plugin-snapsrc`, live `GstPushSrc`): embedded Snapcast **client** in, timestamped PCM out.
- `gst-snapcast-engine` (`crates/gst-snapcast-engine`): GStreamer-free glue (tokio runtime,
  servers shared per port, client lifecycle). Features `server` / `client`.
- `gst-snapcast-common` (`crates/gst-snapcast-common`): GStreamer-side code shared by both
  plugins (so far the `tracing`/`log` → GStreamer debug log bridge).

Architecture, dependency rules (with reasons), threads, logging and the interfaces
(environment variables, debug categories): `docs/architecture.md`. Keep it current.
The original design rationale is in `docs/gstreamer-plugins/dlna-rs-gstreamer-2.md`.
`-1.md` is broader DLNA/UPnP background; read it only when needed.

**Read `docs/claude/memory.md` first.** It holds the current state, the decisions, the
known limitations (L1–L10) and the plan. Keep it up to date: update State and Plan when
a step is done, and record new decisions with a date.

Short version (2026-11-21): goal is a **feasibility prototype**. Loopback, C++ interop in
both directions (`make test-interop`), timing (`make test-aloop`, P3), mDNS, reconnect,
volume/mute and logging (L9) work. Next: the control plane (P2), then the evaluation
(P4). Multiple streams per server are postponed.

## Commands

```sh
make check                         # fmt + clippy (-D warnings) + tests + rustdoc (-D warnings)
cargo test -p gst-plugin-snapsink  # single crate
make inspect                       # gst-inspect-1.0 the built plugins
make test-interop                  # #[ignore]d tests against the C++ snapclient/snapserver
make test-aloop                    # #[ignore]d timing tests on the snd-aloop card (offset, drift)
make test-mdns                     # #[ignore]d mDNS discovery tests (need multicast)
make doctor                        # what this machine has (tools, elements, sound devices)
make install / make listen-all     # per-user install; listening tests (scripts/listen.sh)
GST_PLUGIN_PATH=$PWD/target/debug GST_DEBUG=snapsink:6 gst-launch-1.0 audiotestsrc num-buffers=50 ! snapsink
```

Run `make check` before declaring work done.

## Architecture rules

- **`gst-snapcast-engine` must never depend on GStreamer.** Plugin crates only translate
  buffers/caps/events to engine calls. Reasons (2026-11-21; *not* Android, which has
  GStreamer): the path upstream into snapcast-rs, one timing boundary (the engine speaks
  only `CLOCK_MONOTONIC` µs), tests without GStreamer, and gstreamer-rs churn stays in the
  plugins. Shared code that needs GStreamer goes into `gst-snapcast-common`.
- **Android is a near-term target for the sender** (`snapsink` and the engine's `server`
  feature). Keep that path portable: no Linux-only APIs and no extra system C libraries.
  The receiver (`snapsrc`, `client` feature) is desktop-only. Android already has a
  websocket-based Snapcast client app.
- **One shared embedded `SnapServer` per port**, owned by `gst-snapcast-engine`
  (`server::acquire_stream`). Each `snapsink` feeds one named stream (`stream` property).
  Do not create servers anywhere else.
- Async work runs on `gst_snapcast_engine::runtime()`. Never block a GStreamer streaming
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
- Prefer working around a limitation in `gst-snapcast-engine` if that stays clean. Change
  snapcast-rs when the right fix belongs in the library (e.g. runtime `add_stream`/`remove_stream`).
- Keep the `version` of the path deps in sync with `snapcast-rs/Cargo.toml`.

## Code conventions

- Edition 2024, MSRV 1.94.1 (dictated by snapcast-rs). Toolchain: `rust-toolchain.toml` (stable).
- gstreamer-rs 0.25. Follow the gst-plugins-rs layout: `src/<element>/mod.rs` holds the
  `glib::wrapper!` and `register()`, and `src/<element>/imp.rs` holds the subclass implementation.
  Use `LazyLock` statics for the debug category, properties, metadata and pad templates.
- Logging inside elements: `gst::{error,warning,info,debug,trace}!(CAT, imp = self, ...)`.
  In `gst-snapcast-engine`, use `tracing` (as snapcast-rs does); never install a subscriber
  there. The plugins bridge it into the GStreamer debug log (`gst_snapcast_common::log`,
  categories `snapcast-*`); engine work that belongs to an element runs in a span with a
  `log_id` field. Do not use `println!`.
- Errors posted to the bus: `gst::error_msg!` / `gst::element_imp_error!`. No `unwrap()`
  on runtime data paths. `unwrap()` on mutex locks and `expect("type checked upstream")`
  in property setters are fine.
- Properties that cannot change while running use `.mutable_ready()`.
- Lints are configured in the root `Cargo.toml` (`[workspace.lints]`). Every crate has
  `[lints] workspace = true`. Public items of `gst-snapcast-engine` need docs.
- License: GPL-3.0-only (matches snapcast-rs). `plugin_define!` uses `"GPL"`, because
  GStreamer only accepts fixed license names.

## Tests

All tests, their requirements, how to read the aloop results, and gotchas with solutions:
`docs/testing.md`, `docs/testing-host.md`, `docs/testing-devcontainer.md`. When you find a
new problem with the test setup and its fix, add it to the gotchas there.

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
  Name the binaries via `SNAPCLIENT`/`SNAPSERVER` (default: `PATH`), and add them to
  `make test-interop`.
- Tests that need sound devices use the `snd-aloop` card, are `#[ignore]`d, and run via
  `make test-aloop` (one at a time). Loopback subdevice 0 belongs to those tests; 2 is the
  devcontainer's ALSA `default`.
- Makefile targets and `scripts/` must also work on the user's Fedora host (outside the
  container, without `GST_PLUGIN_PATH`). Don't assume Debian paths; use
  `pkg-config --variable=pluginsdir gstreamer-1.0`.
- The tool shell does not get the devcontainer's `containerEnv`, so set `GST_PLUGIN_PATH`
  explicitly when running `gst-launch-1.0` by hand.

## Environment

- Devcontainer: `.devcontainer/Dockerfile` (Debian trixie, GStreamer 1.26 dev packages,
  ALSA elements/tools, C++ snapserver/snapclient 0.31 as interop peers). Host: Fedora 45,
  rootless Podman with SELinux (`--userns=keep-id`, the container runs as `spc_t`), and
  `--device=/dev/snd` for the host's `snd-aloop` card.
  `GST_PLUGIN_PATH=${workspaceFolder}/target/debug` is set.
- The devcontainer has no audio server: `~/.asoundrc` (from `.devcontainer/asoundrc`) makes
  ALSA `default` a dmix on the loopback card. Don't use `pipewiresink` (it deadlocks on stop
  with PipeWire 1.4.2). Claude Code's config is on a named volume (`CLAUDE_CONFIG_DIR=~/.claude`).
- VS Code tasks, CodeLLDB launch configs and rust-analyzer settings are in
  `gstreamer-snapcast-rs.code-workspace` (`.vscode/` is git-ignored).
- `Cargo.lock` is committed (the plugins are shipped artifacts).

## MCP: xerj (local code and doc search)

The `xerj` MCP server indexes this repo (`ax-docs`) and the `snapcast-rs` submodule
(`ax-snapcast-rs-docs`), with a link graph (brain `gstreamer-snapcast-rs`). Usage,
caveats and known problems: `docs/claude/mcp-xerj.md`.

- **Use it for:**
  - Finding a definition in `snapcast-rs` (`xerj_def`, or `xerj_search` with a plain-string query).
  - Questions phrased in prose about the docs or design (`xerj_semantic_search`, `field=body`).
  - Finding which code a doc section refers to (`xerj_brain_ego`).
- **Prefer Grep/Read for:** exact strings, files changed since the last index (the index is
  a host-side snapshot and does not update itself), and anything that must be complete.
- Always pass `_source` and `max_tokens`, or hits return whole files. Check line numbers
  before relying on them after recent edits.
- Never send xerj field reports or corpus feedback, and never use `rerank`, unless the
  user asks. Both send data off the machine.
- If xerj is down (`xerj_status`), fall back to Grep. Node, gateway and indexing run on the
  host: `scripts/xerj-host.sh up` (the user runs it; it refuses to run in the container).

## MCP: memory (persistent agent memory)

The `memory` MCP server (doobidoo mcp-memory-service, on the host) stores memories that
can be searched by meaning. The store is shared with other projects. Setup, tools and
caveats: `docs/claude/mcp-memory.md`.

- **Every memory you write must carry at least one of the tags `gstreamer` or `snapcast`.**
  Add more specific tags as needed. Filter searches by these tags.
- Use it for insights that should outlive a session but don't belong in a commit:
  debugging findings, environment gotchas, approaches that failed and why. Search it at
  the start of a task when past experience could help (e.g. timing, interop, Podman issues).
- `docs/claude/memory.md` stays the source of truth for state, decisions and plan. Don't
  duplicate it into the MCP store, and don't use the MCP store instead of updating it.
- No secrets: the server allows anonymous access. Mark throwaway entries `temporary`
  and delete them.
- If storage calls fail, run `memory_health` first, then see the setup notes in the doc.
