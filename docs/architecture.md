# Architecture and interfaces

Why this project exists and how it compares (part 0), how it is built (part 1), what an
application or user sees of it (part 2), and where it stands on licence and publishing
(part 3). Related documents:

- [README.md](../README.md): element properties, getting started, trying the elements.
- [testing.md](testing.md): tests and their gotchas.
- [claude/memory.md](claude/memory.md): current state, decisions with dates, known
  limitations (L1…) and the plan. When this document and memory.md disagree about
  *state*, memory.md wins; this document describes the design.
- [gstreamer-plugins/](gstreamer-plugins/): the original design discussion (background).
- The module docs of `snapsink/imp.rs`, `snapsrc/imp.rs` and
  `gst-snapcast-engine/src/client.rs`: the timing design in detail.

## Part 0: Why Snapcast in GStreamer

### What it is

This is not a port of Snapcast. Two GStreamer elements **embed** the
[snapcast-rs](https://github.com/metaneutrons/snapcast-rs) libraries: `snapsink` runs a
Snapcast server inside the pipeline, `snapsrc` a Snapcast client. The wire protocol,
codecs, time sync and server state all come from snapcast-rs; this project adds the
translation between GStreamer and those libraries, and above all the mapping between the
two clock domains (GStreamer's pipeline clock and Snapcast's server timeline).

### What it is for

- **Any GStreamer source becomes a multiroom stream.** Desktop audio (`pipewiresrc`, no
  loopback device), files and web radio in any codec GStreamer decodes (`uridecodebin`),
  RTSP, WebRTC, a DLNA renderer.
- **Any sink becomes a synchronized room.** PipeWire, PulseAudio, ALSA, Bluetooth via
  PipeWire, a file, or another network stream.
- **Processing in the pipeline,** per room or before sending: EQ, room correction,
  crossovers, LV2, resampling.
- **Multiroom inside applications that already use GStreamer**, without FIFOs or extra
  daemons. The typical case is Mopidy: its usual Snapcast setup writes into a FIFO
  (`… ! filesink location=/tmp/snapfifo`) that a separate snapserver reads. With this
  project it is `… ! snapsink`. Likewise gmrender-resurrect, Rygel, or an Android app built
  on GStreamer for Android (the sender is meant to run there).

**When not to use it:** a plain receiver, e.g. a Raspberry Pi on an amplifier, is simpler
with the C++ `snapclient` or `snapclient-rs`. A full multiroom server with sources,
groups and persistence out of the box is `snapserver`.

### Pros and cons

**Pros**

1. **Sources and outputs:** everything GStreamer can produce or play (see above).
   snapserver-rs reads pipe, file, TCP, process, AirPlay and librespot; snapclient-rs
   plays via cpal/ALSA.
2. **Processing** in the same pipeline, with GStreamer's elements.
3. **Embedding:** one element instead of a FIFO plus a daemon.
4. **Exact sender timestamps:** a pipe source can only stamp audio with the time it read
   it. `snapsink` takes the time from the pipeline clock (running time plus latency).
5. **Possible, not tested:** video in sync with multiroom audio, because `snapsrc`'s
   timestamps are on the Snapcast timeline. C++ Snapcast has no answer for that.
6. **Tooling:** `gst-launch-1.0` for ad-hoc setups, `GST_DEBUG` and GStreamer tracers.

**Cons**

1. **The hard part is ours.** Mapping the GStreamer clock onto the Snapcast timeline was
   most of the work (L12, L13, start-up alignment) and depends on the sink (`pulsesink`
   adds 130 ms latency, `alsasink` defaults to `skew`; see
   [Audio sink slaving](#audio-sink-slaving-adjust-sink)). snapclient-rs writes to a
   device and does not have this problem.
2. **Weight:** the GStreamer runtime is far bigger than a single client binary.
3. **A building block, not a product:** several streams per port only within one
   process, no persistence. An application has to provide what snapserver provides as a daemon.
4. **Lifecycle:** the server lives in the application's process. If the application
   crashes or the pipeline stops, the server or its stream goes with it.
5. **Two moving upstreams:** gstreamer-rs has a breaking minor release once or twice a
   year (0.22 02/2024, 0.23 07/2024, 0.24 07/2025, 0.25 02/2026), and our snapcast-rs fork
   carries patches that are not upstream yet (memory.md lists them).
6. **Licence:** GPL-3.0, inherited from snapcast-rs. See [Licence](#licence).
7. **Receivers already exist everywhere** (C++ snapclient, Snapdroid). `snapsrc` is worth
   it mainly when the receiver needs processing or an unusual output.

### Compared with snapcast-rs

snapcast-rs has two layers: **libraries** (`snapcast-proto`, `snapcast-client`,
`snapcast-server`; protocol, codecs, time sync, server state, no devices, ports or files)
and **binaries** (`snapclient-rs`, `snapserver-rs`; audio devices, sources, config, the
JSON-RPC control plane). This project replaces the binaries' role with GStreamer elements
and reuses the libraries unchanged (apart from upstream PR candidates).

| | snapcast-rs binaries | This project |
|---|---|---|
| Receiver | `snapclient-rs`: ~1,500 lines; cpal/ALSA output; plays to the device's clock | `snapsrc` + client part of the engine: ~1,600 lines; any sink; maps to the pipeline clock and slaves the sink |
| Sender | `snapserver-rs`: ~4,200 lines; managed sources, control plane (TCP, HTTP/WebSocket), auth, config, persistence | `snapsink` + server part of the engine: ~1,000 lines; source is the pipeline; control plane planned (P2) |
| Libraries used | snapcast-client / -server / -proto (~11,000 lines) | the same, unchanged |
| Deployment | daemons, configured by files and command line | elements inside an application or `gst-launch` pipeline |

**Is it simpler?** Not as code. The receiver is about as large as snapclient-rs, because
fitting into GStreamer's timing model (pipeline clock, latency, sink slaving) is more work
than writing to an ALSA device. The sender is much smaller, because GStreamer provides the
sources, and because the control plane is still missing. The simplicity is for **users and
integrators**: one element instead of a FIFO and a daemon.

## Part 1: Architecture

### Crates

| Crate | Depends on GStreamer | Role |
|---|---|---|
| `crates/gst-snapcast-engine` | **no** | Glue around snapcast-rs: the shared tokio runtime, one embedded server per port (`server`), embedded clients with play-out times (`client`), mDNS, client instance numbers |
| `crates/gst-snapcast-common` | yes | Code both plugins need on the GStreamer side; so far the logging bridge |
| `crates/gst-plugin-snapsink` | yes | `snapsink` (`GstBaseSink`): PCM in, Snapcast stream out (`libgstsnapsink.so`) |
| `crates/gst-plugin-snapsrc` | yes | `snapsrc` (live `GstPushSrc`): Snapcast in, timestamped PCM out (`libgstsnapsrc.so`) |
| `snapcast-rs/` (submodule) | no | `snapcast-server`, `snapcast-client`, `snapcast-proto`; our fork, changes go upstream |

```text
gst-plugin-snapsink ──┬──> gst-snapcast-common ──> gstreamer-rs
gst-plugin-snapsrc  ──┤             │
                      └──> gst-snapcast-engine <───┘
                                  │
                                  └──> snapcast-server / snapcast-client / snapcast-proto
```

The engine was called `snapcast-engine` until 2026-11-21. The `gst-` prefix marks it as
part of this project's crate family (on crates.io it would otherwise look like part of
snapcast-rs), not a GStreamer dependency. Two plugin crates rather than one
`gst-plugin-snapcast` with features (decision 2026-11-21): the split between sender
(Android) and receiver (desktop) stays explicit.

### Dependency rules, and why

**`gst-snapcast-engine` does not depend on GStreamer.** (Decision 2026-11-21, which
replaces the earlier reason.) The rule was first justified with the Android port of the
sender. That reason does not hold: GStreamer for Android exists, and `snapsink` needs it
on Android anyway. The rule stays, for these reasons:

1. **Path upstream.** Engine code is a candidate for snapcast-rs (the control plane
   already goes there, L10), and snapcast-rs will never depend on GStreamer.
2. **One timing boundary.** The engine speaks only µs of `CLOCK_MONOTONIC`
   (`gst_snapcast_engine::now_usec`). All conversion from and to GStreamer clocks, running
   time and latency happens in the plugins, so the hard part has one home.
3. **Tests.** Engine tests run without `gst::init` and without the GStreamer libraries.
4. **gstreamer-rs churn** stays in the plugin crates.
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
  Each `snapsink` feeds one named stream; further stream names join the running server
  (`ServerCommand::AddAudioStream`, since L1, 2026-11-22). A released stream stays on the
  server until it stops, so clients keep their assignment across a sender restart.
- Async work runs on `gst_snapcast_engine::runtime()`. GStreamer streaming threads never
  block on async code without a bound.

### Threads

| Thread | Created by | Runs |
|---|---|---|
| GStreamer streaming threads | GStreamer | element code; calls into the engine (`acquire_stream`, `send_pcm`, `connect`, `recv` with a bound) |
| `gst-snapcast-engine` (tokio, multi-threaded) | `gst_snapcast_engine::runtime()`, on first use | snapcast-rs server and client tasks, engine event tasks |
| one per stream | snapcast-rs `SnapServer::serve` | the stream's encoder, on its own current-thread runtime |
| mdns-sd daemon | `mdns-sd` | announcement (snapsink) and discovery (snapsrc) |

### Timing (summary)

- **Sink:** server timestamp = `now_usec() + (base_time + running_time + latency − clock.time())`;
  works with any pipeline clock, falls back to counting samples without one.
- **Source:** play-out = `server_ts − diff_to_server + (buffer − server_latency − client_latency)`;
  PTS = play-out in pipeline time minus the pipeline latency (L13). `snapsrc` provides no
  clock; the audio sink downstream is slaved with `slave-method=resample` (next section).
- Network arrival time is never used.

Details, measurements and the reasoning: the module docs and memory.md ("Timing design").

### Audio sink slaving (`adjust-sink`)

**The problem.** A sound card plays at its own crystal's rate, which is never exactly
48 kHz: a few to a few hundred ppm off. The pipeline, however, runs on the pipeline clock,
and snapsrc's timestamps put each sample on the Snapcast timeline in that clock. So the
card's clock and the pipeline clock drift apart, and the audio sink has to *slave* the
card to the pipeline clock, or the sound slowly leaves the timeline. Every Snapcast client
has this problem; the C++ snapclient solves it by inserting or dropping single samples
itself.

**What GStreamer offers.** Audio sinks (`GstAudioBaseSink`: `alsasink`, `pulsesink`, …)
have the property `slave-method`:

| `slave-method` | Behaviour | Effect on Snapcast sync |
|---|---|---|
| `skew` (default) | The card runs freely; when it is more than `drift-tolerance` (default 40 ms) off, the sink jumps (skips or repeats a chunk) | Sound drifts up to 40 ms off the timeline, then jumps. Measured on a card at −300 ppm: the output followed the card (+300 ppm rate error), not the server (L12). Its start-up calibration also left each run up to ~4 ms off |
| `resample` | Continuously resamples the output to the pipeline clock's rate (inserting/dropping fractions of samples) | Follows the timeline: −17…+4 ppm rate error on the same card, within about 1 ms of the server timeline |
| `none` | No slaving | The card's drift goes straight into the output |

**What snapsrc does.** With `adjust-sink=true` (the default), snapsrc looks for the first
audio sink downstream when it starts, and sets `slave-method=resample` there if the sink
is still at `skew`. It follows pads through bins, so it also finds the sink inside
`autoaudiosink` (or `pipewiresink`/`pulsesink` behind converters and queues; through a
`tee` only the first branch). It logs what it did at INFO in the `snapsrc` category.

**Why snapsrc does it, not the user.** Elements normally don't touch other elements'
properties. But with `autoaudiosink`, the most common sink in examples, the user can't:
it creates its child only at runtime and passes no properties through. Leaving the default
`skew` silently breaks sync by tens of milliseconds, which is the one thing a Snapcast
receiver must not do. So the adjustment is on by default.

**Limits and opt-out:**
- snapsrc can't tell an explicit `slave-method=skew` from the default. If you set `skew`
  on purpose, also set `adjust-sink=false`. Other values (`none`, `resample`, `custom`)
  are left alone.
- `resample` costs a little CPU, and its rate estimate is sometimes restless: single runs
  showed tick-to-tick jitter of ±0.7 ms (memory.md, "Open items"). Smoother slaving via
  the sink clock's `window-size`/`timeout` would be possible, at the cost of slower
  convergence.
- Not chosen: making the card the master clock. An audio sink can provide its clock only
  after caps negotiation, too late for the pipeline's clock selection (PAUSED→PLAYING). So
  snapsrc provides no clock: in a live pipeline that means the system clock
  (`CLOCK_MONOTONIC`, the Snapcast clock domain), or another live element's clock.

snapsrc's own `align.rs` complements this on the timestamp side: it keeps the PTS
contiguous and steers towards the ideal timestamps by inserting/dropping single frames
(absorbs time-sync steps, L5).

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
  and leaves an existing one alone (logged at INFO in the `snapcast-engine` category).
  Global, because events come from threads nobody here creates (the encoder threads, the
  mdns-sd daemon, GStreamer's streaming threads). Each plugin `.so` links its own copy of
  `tracing` and `log`, so the two plugins never compete for the slot.
- **Element names:** the engine runs its work in spans with a `log_id` field
  (`gst_snapcast_engine::LOG_ID_FIELD`): the element name for a client (`snapsrc0`) and
  for a stream acquisition, `snapserver:<port>` for a shared server. The bridge logs an
  event with the innermost `log_id` of its span scope (`gst_debug_log_id`, GStreamer ≥ 1.22),
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
(or `gst-inspect-1.0 snapsink`). `snapsink` keeps GstBaseSink's `sync` and `async`;
`async=false` is needed only when the same pipeline also receives from its server
([pipelines.md](pipelines.md#preroll-and-async-false), L14). Control plane (P2): `snapsink` can serve the Snapcast
JSON-RPC control API over TCP (`control-port`) and HTTP/WebSocket (`http-port`), with
Snapweb's files (`snapweb-dir`) as the user interface; no D-Bus. The API comes from the
snapcast-rs crate `snapcast-control` (contract: trait `SnapControl`); the engine owns it
per shared server (feature `control`, `server::ControlOptions`), and its listeners and
the event loop run on the engine runtime and stop with the server. Tags, `controls` and the
playback status become stream properties (`StreamHandle::set_properties`);
`Stream.Control` reaches the element through `StreamHandle::on_command` and becomes the
element message `snapcast-stream-control`. Plan:
[claude/memory-old-2026-11-22.md](claude/memory-old-2026-11-22.md), P2.

### Environment variables

| Variable | Default | Effect |
|---|---|---|
| `GST_DEBUG` | (GStreamer) | Thresholds per category, e.g. `snapsrc:5,snapcast*:4` |
| `GST_SNAPCAST_TRACING` | on | `0`, `false`, `no` or `off`: don't install the logging bridge (snapcast-rs and engine logs then go to the application's own `tracing` subscriber, or nowhere) |
| `XDG_RUNTIME_DIR` | temp dir | Where snapsrc keeps its instance lock files (`gst-snapcast-engine/instance-<n>.lock`; until 2026-11-21 `snapcast-engine/`) |

The test suites read more (`SNAPCLIENT`, `SNAPSERVER`, `ALOOP_*`); see [testing.md](testing.md).

### Debug categories

| Category | What logs there |
|---|---|
| `snapsink`, `snapsrc` | the elements |
| `snapcast-engine` | the engine glue (crate `gst-snapcast-engine`), and the logging bridge itself |
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

## Part 3: Licence and publishing

### Licence

*Not legal advice; a summary of the reasoning (2026-11-21).*

**Current:** all crates are `GPL-3.0-only`, like snapcast-rs. `plugin_define!` says
`"GPL"`, because GStreamer only accepts fixed licence names.

**What is fixed:**
- snapcast-rs is `GPL-3.0-only` and "a Rust reimplementation of Snapcast" (C++ Snapcast:
  GPL-3.0, Johannes Pohl). Relicensing it would need all its authors, and as a
  reimplementation probably the C++ authors too. Practically impossible.
- Rust links crates statically. Every `.so` we build contains snapcast-rs, so **the
  binary is GPL-3.0 as a whole**, whatever licence our own source carries.
- GStreamer loads a plugin into the application's process and exchanges data with it in
  memory; that is generally treated as one combined work, not two programs talking.

**Who GPL-3.0 restricts.** The GPL governs *distribution*, not use:

| Case | GPL-3.0 plugin |
|---|---|
| A user installs it (e.g. a distro package) and uses it with any application | allowed |
| Distributed together with Apache-2.0 (Mopidy), MIT, MPL-2.0, LGPL or GPL-2.0-or-later software | allowed (the combination falls under GPL-3.0) |
| Distributed together with GPL-2.0-only software | incompatible |
| Distributed together with proprietary software | not allowed |
| Android app that bundles it | the app under a GPL-3.0-compatible licence, with source |
| Inclusion in gst-plugins-rs | excluded: it accepts only MPL-2.0, Apache-2.0 or MIT |

**Options considered:**

| Option | Effect on the binary | Assessment |
|---|---|---|
| Stay `GPL-3.0-only` | GPL-3.0 | Consistent with snapcast-rs; the integrations from part 0 (Mopidy etc.) are fine |
| Our source under LGPL-2.1/3.0-or-later | still GPL-3.0 (snapcast-rs inside) | No gain today. LGPL's "user can relink" model also fits Rust's static linking badly, and it would not get us into gst-plugins-rs either |
| Our source under MPL-2.0 (or MIT/Apache-2.0) | still GPL-3.0 | Only gain: our glue and timing code becomes reusable in non-GPL projects, and the plugins could follow if a non-GPL Snapcast implementation ever appears. MPL-2.0 is GPL-compatible and gst-plugins-rs's main licence. Costs: a mixed-licence explanation |
| Own Snapcast protocol implementation under a permissive licence | could be non-GPL | Contradicts the project's premise (no own wire protocol); snapcast-rs's libraries are ~11,000 lines |

**Decision (2026-11-21, user): GPL-3.0-only**, also for our own source. MPL-2.0 for our
code was considered and rejected: it would gain nothing while snapcast-rs is linked in.

### Publishing (crates.io and packages)

Not planned now; this records what it would take. **Not ready**, in order of weight:

1. **snapcast-rs releases (hard blocker).** Published crates may only depend on
   crates.io crates. crates.io has `snapcast-server`/`-client`/`-proto` 0.18.0, without
   our fork commits. The upstream PRs have to be merged and released first.
2. **All crates in the chain:** `gst-snapcast-engine` and `gst-snapcast-common` too, not
   only the two plugins. All names are free on crates.io (checked 2026-11-21).
3. **Maturity and semver:** a feasibility prototype; P2 and P4 are open. Publishing means
   committing to an API (the engine's is internal so far) and to maintenance.
4. **Housekeeping:** remove `publish = false`; keywords, categories, a README per crate;
   check that docs.rs can build against GStreamer. `gst-plugin-version-helper` handles
   builds from a crates.io tarball; path dev-dependencies are dropped on publish.
5. **Distribution is mostly elsewhere.** crates.io serves Rust applications that link
   statically. End users need a `.so`: `cargo cinstall` (cargo-c, as gst-plugins-rs does,
   with pkg-config files), a distribution package (e.g. a Fedora COPR) or a Flatpak
   extension. "Publishable" should include "packageable".
