# Architecture and interfaces

How gstreamer-snapcast-rs is built (part 1) and what an application or user sees of it
(part 2). Related documents:

- [README.md](../README.md): element properties, getting started, trying the elements.
- [testing.md](testing.md): tests and their gotchas.
- [claude/memory.md](claude/memory.md): current state, decisions with dates, known
  limitations (L1…) and the plan. When this document and memory.md disagree about
  *state*, memory.md wins; this document describes the design.
- [gstreamer-plugins/](gstreamer-plugins/): the original design discussion (background).
- The module docs of `snapsink/imp.rs`, `snapsrc/imp.rs` and
  `snapcast-engine/src/client.rs`: the timing design in detail.

## Part 1: Architecture

### Crates

| Crate | Depends on GStreamer | Role |
|---|---|---|
| `crates/snapcast-engine` | **no** | Glue around snapcast-rs: the shared tokio runtime, one embedded server per port (`server`), embedded clients with play-out times (`client`), mDNS, client instance numbers |
| `crates/gst-snapcast-common` | yes | Code both plugins need on the GStreamer side; so far the logging bridge |
| `crates/gst-plugin-snapsink` | yes | `snapsink` (`GstBaseSink`): PCM in, Snapcast stream out (`libgstsnapsink.so`) |
| `crates/gst-plugin-snapsrc` | yes | `snapsrc` (live `GstPushSrc`): Snapcast in, timestamped PCM out (`libgstsnapsrc.so`) |
| `snapcast-rs/` (submodule) | no | `snapcast-server`, `snapcast-client`, `snapcast-proto`; our fork, changes go upstream |

```text
gst-plugin-snapsink ──┬──> gst-snapcast-common ──> gstreamer-rs
gst-plugin-snapsrc  ──┤             │
                      └──> snapcast-engine <───────┘
                                  │
                                  └──> snapcast-server / snapcast-client / snapcast-proto
```

### Dependency rules, and why

**`snapcast-engine` does not depend on GStreamer.** (Decision 2026-11-21, which replaces
the earlier reason.) The rule was first justified with the Android port of the sender.
That reason does not hold: GStreamer for Android exists, and `snapsink` needs it on
Android anyway. The rule stays, for these reasons:

1. **Path upstream.** Engine code is a candidate for snapcast-rs (the control plane
   already goes there, L10), and snapcast-rs will never depend on GStreamer.
2. **One timing boundary.** The engine speaks only µs of `CLOCK_MONOTONIC`
   (`snapcast_engine::now_usec`). All conversion from and to GStreamer clocks, running
   time and latency happens in the plugins, so the hard part has one home.
3. **Tests.** Engine tests run without `gst::init` and without the GStreamer libraries.
4. **gstreamer-rs churn.** gstreamer-rs has a breaking release about twice a year; the
   rule keeps that churn in the plugin crates.
5. Minor: a non-GStreamer embedder (e.g. an Android app on Oboe/AAudio) could use the
   engine as is.

Shared code that needs GStreamer goes into **`gst-snapcast-common`**, never into the engine.

**Android portability** is a separate rule and still applies to the sender path
(`snapsink`, `gst-snapcast-common`, the engine's `server` feature): no Linux-only APIs and
no system C libraries beyond what GStreamer for Android provides. The receiver
(`snapsrc`, `client`) is desktop-only. On Android the plugins are linked statically into
the app; see [Static linking](#static-linking-rust-applications).

Other rules (see also CLAUDE.md):

- One embedded `SnapServer` per port, owned by the engine (`server::acquire_stream`).
  Each `snapsink` feeds one named stream. Multiple streams per server are postponed (L1).
- Async work runs on `snapcast_engine::runtime()`. GStreamer streaming threads never
  block on async code without a bound.

### Threads

| Thread | Created by | Runs |
|---|---|---|
| GStreamer streaming threads | GStreamer | element code; calls into the engine (`acquire_stream`, `send_pcm`, `connect`, `recv` with a bound) |
| `snapcast-engine` (tokio, multi-threaded) | `snapcast_engine::runtime()`, on first use | snapcast-rs server and client tasks, engine event tasks |
| one per stream | snapcast-rs `SnapServer::serve` | the stream's encoder, on its own current-thread runtime |
| mdns-sd daemon | `mdns-sd` | announcement (snapsink) and discovery (snapsrc) |

### Timing (summary)

- **Sink:** server timestamp = `now_usec() + (base_time + running_time + latency − clock.time())`;
  works with any pipeline clock, falls back to counting samples without one.
- **Source:** play-out = `server_ts − diff_to_server + (buffer − server_latency − client_latency)`;
  PTS = play-out in pipeline time minus the pipeline latency (L13). `snapsrc` provides no
  clock and sets `slave-method=resample` on the downstream audio sink (L12).
- Network arrival time is never used.

Details, measurements and the reasoning: the module docs and memory.md ("Timing design").

### Logging

snapcast-rs and the engine log with [`tracing`](https://docs.rs/tracing); some
dependencies (`mdns-sd`, `symphonia`) with [`log`](https://docs.rs/log). The engine
installs nothing: it only emits. The plugins route all of it into the GStreamer debug
log, so `GST_DEBUG` controls everything (L9, 2026-11-21).

- **The bridge** (`gst-snapcast-common::log`) is a `tracing_subscriber` layer on a bare
  `Registry` (no fmt, no env filter) plus a `log::Log`. Both pick a GStreamer category by
  the crate of the event's target (table in part 2) and map the level.
- **Installation:** each plugin's `plugin_init` calls `gst_snapcast_common::log::init()`.
  It sets a **global** `tracing` subscriber and `log` logger, **only if none is set yet**,
  and leaves an existing one alone (logged at INFO in `snapcast-engine`). Global, because
  events come from threads nobody here creates (the encoder threads, the mdns-sd
  daemon, GStreamer's streaming threads). Each plugin `.so` links its own copy of
  `tracing` and `log`, so the two plugins never compete for the slot.
- **Element names:** the engine runs its work in spans with a `log_id` field
  (`snapcast_engine::LOG_ID_FIELD`): the element name for a client (`snapsrc0`) and for a
  stream acquisition, `snapserver:<port>` for a shared server. The bridge logs an event
  with the innermost `log_id` of its span scope (`gst_debug_log_id`, GStreamer ≥ 1.22),
  so lines show `<snapsrc0>`. snapcast-rs's server keeps the caller's span in its own
  tasks and encoder threads (fork commit `fix(server): run spawned tasks and the encoder
  thread in the caller's tracing context`, upstream PR candidate). snapcast-client spawns
  no tasks of its own.
- **Cost:** callsites are registered as "sometimes", so each event costs one category
  threshold check, and `GST_DEBUG` changes at runtime apply.
- **Loops:** a GStreamer log function that turns lines back into `tracing` events or
  `log` records (see `tracing-gstreamer` below) would feed the bridge again. The bridge
  drops what a thread emits while it is already forwarding (`tracing` drops nested events
  by itself; `log` does not).

## Part 2: Interfaces

### Elements

`snapsink` and `snapsrc`, with the properties listed in the [README](../README.md#element-properties)
(or `gst-inspect-1.0 snapsink`). Planned with the control plane (P2): a `control-port`
property, JSON-RPC over TCP and WebSocket, and `Stream.Control` as a bus message.

### Environment variables

| Variable | Default | Effect |
|---|---|---|
| `GST_DEBUG` | (GStreamer) | Thresholds per category, e.g. `snapsrc:5,snapcast*:4` |
| `GST_SNAPCAST_TRACING` | on | `0`, `false`, `no` or `off`: don't install the logging bridge (snapcast-rs and engine logs then go to the application's own `tracing` subscriber, or nowhere) |
| `XDG_RUNTIME_DIR` | temp dir | Where snapsrc keeps its instance lock files (`snapcast-engine/instance-<n>.lock`) |

The test suites read more (`SNAPCLIENT`, `SNAPSERVER`, `ALOOP_*`); see [testing.md](testing.md).

### Debug categories

| Category | What logs there |
|---|---|
| `snapsink`, `snapsrc` | the elements |
| `snapcast-engine` | the engine glue, and the logging bridge itself |
| `snapcast-server` | snapcast-rs server library (sessions, encoders, state) |
| `snapcast-client` | snapcast-rs client library (connection, time sync, decoder) |
| `snapcast-mdns` | mdns-sd (via `log`) |
| `snapcast-deps` | everything else, e.g. snapcast-proto, symphonia |

Levels: ERROR → ERROR, WARN → WARNING, INFO → INFO, DEBUG → DEBUG, TRACE → LOG.
Fields of an event follow its message as `name=value`:

```text
$ GST_DEBUG=snapsrc:4,snapcast*:5 gst-launch-1.0 snapsrc port=1704 ! autoaudiosink
INFO  snapcast-client …/controller.rs:107:snapcast_client::controller:<snapsrc0> Connected scheme=tcp host=127.0.0.1 port=1704
DEBUG snapcast-client …/controller.rs:314:snapcast_client::controller:<snapsrc0> Time sync usable diff_ms=0.036

$ GST_DEBUG=snapcast-server:5 gst-launch-1.0 audiotestsrc is-live=true ! snapsink
DEBUG snapcast-server …/lib.rs:549:snapcast_server:<snapserver:1704> Stream encoder started stream=default codec=pcm
INFO  snapcast-server …/session.rs:308:snapcast_server::session:<snapserver:1704> Client hello id=… name=…
```

`gst-launch-1.0 --gst-debug-help | grep snapcast` lists the categories.

### Static linking (Rust applications)

When an application links the plugins statically (`plugin_register_static()`, also the
usual way on Android) it shares one copy of `tracing` and `log` with them:

- If the application installs its own subscriber or logger **before** registering the
  plugins, the bridge leaves it alone; snapcast-rs events go there.
- If it installs one **after** registering, its `init()` fails (`tracing_subscriber`'s
  `.init()` panics) because the bridge took the slot. Install first, or set
  `GST_SNAPCAST_TRACING=0`.
- An Android app in Kotlin/Java installs no Rust subscriber; nothing to do.

### Related: `tracing-gstreamer` (not used)

[`tracing-gstreamer`](https://github.com/standard-ai/tracing-gstreamer) bridges the other
way: GStreamer log → `tracing`, plus GStreamer tracer hooks (pad pushes, state changes)
as spans for profilers such as Tracy or Chrome tracing. It does not replace this bridge.
Version 0.9.0 (2025-07) depends on gstreamer-rs 0.24, we use 0.25, so it can't be used as
is. Possible later use: profiling the pipelines during the endurance run (F3), once it
supports 0.25. The two don't feed each other in the usual setups: with `.so` plugins they
use different copies of `tracing`, and a statically linking application that installs
`tracing-gstreamer` has its own subscriber, so the bridge is not installed. Should they
ever share one subscriber, the loop guards (above) stop the cycle.
