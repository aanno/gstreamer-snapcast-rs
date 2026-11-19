# Project memory: state, decisions, limitations, plan

Working memory for agents and humans continuing this project. Keep it current:
update **State** and **Plan** when a step is finished, and add decisions with a date.

Last updated: 2026-11-19 (P1 done: `make test-aloop`; first F2 measurements; devcontainer audio).

## Resume here (2026-11-19, evening)

- P1 done; P3 (a) L13, (b) L12 and (c) start-up alignment are fixed and committed. The
  user asked to work through the limitations **L2, L3, L4, L5, L7, L8** next (agent's
  order, one commit each; snapcast-rs changes in the submodule fork with its own
  conventions, then the pointer commit here). P2 (control plane) comes after that.
- Host: everything green on Fedora 45 (`scripts/test-host.sh`); listening with
  `scripts/host-play.sh` + `make listen-recv` (start the receiver first) is clean since the
  frame-loss fix. Run `make install` again after plugin changes.

- **Limitations done in this round:** L4, L5, L3, L2 (2026-11-19). The fork commits are local:
  push `aanno/snapcast-rs` before others can build the new submodule pointer.

### Still open (2026-11-19)

- **snapsrc + C++ snapclient under drift.** The test failed at a 1 ms tolerance because
  the **C++ snapclient** itself is +1.7…+2.0 ms off the server timeline on a −300 ppm card,
  while snapsrc is at +0.6…+0.9 ms: our receiver is closer to the server than the
  reference. So the drift tests use a separate, wider bound (`ALOOP_TOLERANCE_DRIFT_US`,
  default 2500 µs). Without drift, F2's 1 ms stays.
- **snapsrc wanders by up to ±1.5 ms within some runs.** Measured 2026-11-19
  (`ALOOP_VERBOSE=1`, snapsrc debug "Alignment: …"). The user's hypothesis (too aggressive
  adjusting) is **not** our code: the aligner made **0 corrections** in all runs (error
  stable at about −110 µs, inside its deadband), and `diff_to_server` is stable. The wander
  is tick-to-tick **jitter of ±0.7 ms in single runs and single receivers** (e.g. −0.37,
  +1.04, +0.61 ms…), i.e. the sink's `resample` slaving, whose rate estimate (a regression
  over clock observations) is sometimes restless. Possible remedy, not done: smoother
  slaving via the sink clock's `window-size`/`timeout`, at the cost of slower convergence.
  Within the F2 bounds as is.

## State

- **Workspace:** `snapcast-engine` (no GStreamer dependency), `gst-plugin-snapsink`
  (`libgstsnapsink.so`) and `gst-plugin-snapsrc` (`libgstsnapsrc.so`). snapcast-rs is a
  submodule (fork `aanno/snapcast-rs`), excluded from our workspace, used via path deps.
  It is **unmodified** so far.
- **Loopback works:** `snapsink → embedded server → snapsrc` for PCM
  (`crates/gst-plugin-snapsrc/tests/loopback.rs`).
  - Format F32LE 48 kHz stereo, signal level as expected (RMS 0.566 for a 0.8 sine).
  - PTS gaps below 2 ms.
  - Buffers arrive 292–299 ms ahead of play-out with `buffer-ms=300`, so the end-to-end
    overhead is below 10 ms on localhost.
- **C++ interop is tested in both directions** (`make test-interop`, `#[ignore]`d tests in
  `crates/*/tests/interop_cpp.rs`, binaries overridable with `SNAPCLIENT`/`SNAPSERVER`):
  - `snapsink` → C++ `snapclient` (`--player file:filename=stdout`): 440 Hz at the expected
    RMS, for `pcm` and `flac`;
  - C++ `snapserver` (pipe source) → `snapsrc`: signal, PTS continuity and scheduling, for
    `pcm` and `flac`.
  - Peers: Debian 0.31 in the devcontainer. On the host the user provides their own
    (possibly self-compiled upstream 0.34+).
- **Test tooling** (P1): `make install`/`uninstall` (per user), `install-system`/
  `uninstall-system` (root, `pkg-config` plugins dir), `doctor` (`scripts/doctor.sh`) and
  `listen-*` (`scripts/listen.sh`: send, recv, cpp-client, cpp-server, pair, all; `LOCAL=1`
  uses the tree's release build), and `test-aloop` (see below). The real listening runs
  happen on the user's Fedora host. With `snapsrc`, gst-launch uses the system clock
  ("New clock: GstSystemClock"; snapsrc no longer provides one since 2026-11-19). `listen.sh` stops all parts as soon as one exits,
  and warns when there is no audio output. In the devcontainer, `~/.asoundrc` makes ALSA
  `default` a dmix on loopback subdevice 2, so `listen-*` runs there (inaudibly).
- **F2 measurements** (`make test-aloop`, 2026-11-19, devcontainer, C++ snapclient 0.31).
  The method: both receivers go into one dmix on loopback subdevice 0, one per channel, and
  the onsets are compared in a single capture, so the offset is sample-exact. Drift comes
  from `PCM Rate Shift` on *both* cable ends; the card rate is fitted against `CLOCK_MONOTONIC`.
  1. Two `snapsrc` (same process): offset constant within a run, but 0…+0.85 ms between
     runs (once > 1 ms). This is the start-up alignment of each `alsasink`, never corrected.
  2. `snapsrc` vs C++ `snapclient`: **constant −60.6 ms** (snapclient earlier). Which one is
     right is not known yet: the test has no absolute reference (planned: compare with
     the server timestamps).
  3. Card at −300 ppm: **`snapsrc` → `alsasink` follows the card, not the server** (rate error
     +300 ppm); the C++ snapclient follows the server (−1.9 ppm). The cause: `alsasink` default
     `slave-method=skew` with `drift-tolerance` 40 ms resyncs only after 40 ms of drift.
  4. **Host** (2026-11-19, Fedora 45, GStreamer 1.28.9, C++ 0.35.0): same results. Offset
     to snapclient −61.8…−63.7 ms, snapsrc follows the card, snapclient 0.35 +5…+18 ppm.
- **First listening run on the host (F1)** (`make listen-all`, PipeWire): the ticks are
  audible, as a **mixture of single and double clicks**. The constant ~62 ms from the aloop
  test would sound like a steady double click, so via PipeWire the offset seems to
  move (L12: snapsrc follows the real card's clock and resyncs). Not measured yet.
- **Absolute reference** (2026-11-19): every aloop test now also reports each receiver's
  offset against the server timeline (`CLOCK_MONOTONIC`, the server's clock). Result:
  snapsrc **+62 ms late** (pipeline latency 65 ms), C++ snapclient **+1 ms**. That confirms
  hypothesis 2 (L13, P3 (a)).
- **Fixed 2026-11-19 (L13): snapsrc subtracts the pipeline latency from the PTS**, so the
  sink (which renders at PTS + latency) outputs at the play-out time. A latency change
  marks the next buffer DISCONT; the late check is `playout < now` again, now correct since
  render time = play-out. Results: snapsrc against the server timeline −3…+1 ms (was +62),
  **snapsrc + C++ snapclient in sync, +0.4 ms (passes; was −61 ms)**, still 0 gaps behind
  a high-latency sink. Remaining: (c) start-up alignment. snapsrc lands at about +0.5…+0.9
  *or* about −3 ms per run (constant within a run), so two snapsrc can be ~4 ms apart; the
  step is close to the 5 ms dmix period. And (b) drift (L12).
- **Fixed 2026-11-19: snapsrc lost frames behind `pulsesink`.** Music from a file "wobbled
  with short pauses" on the host. The cause: `create()` dropped frames with `playout < now`,
  but downstream sets the pace (a sink blocks until its ring buffer has room) and renders at
  PTS + pipeline latency (`pulsesink`: 130 ms). So about every fourth 96 ms block was dropped.
  Now snapsrc stores the latency from the LATENCY event and drops only when
  `playout + latency < now`; the PTS is computed signed. Regression test:
  `aloop_snapsrc_loses_no_frames_behind_a_high_latency_sink` (104 gaps before, 0 after).
- **Fixed 2026-11-19 (L12 and start-up alignment).** Both came from the sink's slaving:
  with `slave-method=skew` an audio sink follows its card and jumps back only after 40 ms,
  and its start-up calibration left each run off by up to ~4 ms. snapsrc now sets
  `slave-method=resample` on the first audio sink downstream (new property `adjust-sink`,
  default true; only if that sink is still at `skew`; it follows pads through bins such as
  `autoaudiosink`). Also: snapsrc **no longer provides a clock**. In live pipelines the
  pipeline then uses the system clock anyway (sinks can't offer theirs before negotiation;
  a design with the card as master clock was tried and failed for that reason), but another
  live element's clock can win now. New `src/snapsrc/align.rs` (unit-tested) keeps the PTS
  contiguous and corrects the ideal-vs-sample-count difference by inserting/dropping single
  frames (absorbs time-sync steps, L5, and a drifting pipeline clock). aloop: two snapsrc
  pass under drift (4/4); snapsrc+C++ under drift fails only because the C++ snapclient is
  +1.7…+2.0 ms off the server there. Open: snapsrc sometimes wanders ±1.5 ms within a run.
- **Test audio:** `assets/audio/freejazz_medley.flac` (CC0, OpenGameArt, 38 s, 44.1 kHz) for
  listening tests, used by `scripts/host-play.sh`.
- **Host interop:** `snapsink` → C++ snapclient 0.35 passes (pcm, flac). C++ snapserver
  0.35 → `snapsrc` failed because 0.35 refuses a missing `--config` file. That's fixed
  (an empty file). **Host fully green** since 2026-11-19 10:36: `make check` and all four
  `make test-interop` tests (with 0.35). Host `make check` needed `dnf install rustfmt clippy`.
- **Timing design** (details in the module docs of `snapsink/imp.rs`, `snapsrc/imp.rs` and
  `snapcast-engine/src/client.rs`). All engine times are µs of `CLOCK_MONOTONIC`
  (`snapcast_engine::now_usec`).
  - **Sink:** server timestamp = `now_usec() + (base_time + running_time + latency − clock.time())`,
    which works with any pipeline clock. Without a clock it falls back to counting samples.
  - **Source:** play-out = `server_ts − diff_to_server + (buffer − server_latency − client_latency)`,
    ideal PTS = `clock.time() + (playout − now_usec()) − base_time − pipeline latency` (L13).
    `align.rs` keeps the PTS contiguous and steers towards the ideal by inserting/dropping
    single frames. Network arrival time is never used.
  - **Clock:** `snapsrc` provides none; in live pipelines that's the monotonic system clock.
    The audio sink slaves its card to it with `slave-method=resample`, set by snapsrc
    (`adjust-sink`, L12). Its latency query answers min 0, max unbounded.
  - **Late frames:** frames whose play-out time (= render time) has passed are dropped,
    and the next buffer is marked DISCONT.
- **Server sharing:** a registry keyed by port. The first `snapsink` on a port starts the
  server, and the last one to release it stops it. Each stream can be claimed by only one
  sink at a time. `port=0` gives an ephemeral port that is never shared; read it back from
  the read-only `bound-port` property.

## Decisions

- 2026-11-07: **Timing robustness comes first**, multiple streams per server second.
- 2026-11-07: **snapcast-rs changes are meant to go upstream** to `metaneutrons/snapcast-rs`:
  - follow its conventions (its own `make check`, conventional commits, git hooks, docs, tests);
  - keep each change a small, self-contained PR candidate developed in the fork;
  - avoid project-specific hacks in snapcast-rs.
- 2026-11-07: **Android is a near-term target for the sender** (`snapsink`, i.e. the
  embedded server). The server half of `snapcast-engine` and its dependencies must stay
  Android-buildable: no Linux-only APIs and no system C libraries beyond what GStreamer
  for Android provides.
- 2026-11-07: **No Android receiver.** An existing websocket-based Android Snapcast app
  covers that, so `snapsrc` may stay desktop/Linux-focused.
- 2026-11-07: **The control plane** (JSON-RPC, groups, volume, stream assignment) still
  needs to be worked out and discussed. Not now.
- 2026-11-07: The default codec is `pcm`, until FLAC timestamps are verified (see L4).
- 2026-11-07: The C++ snapserver/snapclient (Debian 0.31) are allowed in the devcontainer,
  for interop testing only.
- 2026-11-10: **Goal now: a feasibility prototype.** Its purpose is to decide whether the
  planned plugins are worth pursuing. This supersedes the 2026-11-07 priority order.
  - Make as few changes to snapcast-rs as possible (engine workarounds are preferred for now).
  - **Multiple streams per server are postponed** until further notice (L1 stays).
  - **Android stays important but is not a prototype target.** Keep the portability rule;
    there is no Android build or CI in the prototype.
  - **The control plane goes into `snapcast-engine`.** For the prototype, copying code from
    `snapserver-rs` is fine; refactoring snapcast-rs (e.g. a library crate for control) comes later.
  - The prototype needs a sane way to test both plugins: Makefile targets for install,
    uninstall and tests that also work **outside the devcontainer** (Fedora 45 host), so the
    user can run real listening tests.
- 2026-11-10 (answers to the open questions):
  - The Android app in question combines a WebSocket audio client with Snapweb-style
    control. It is **not needed for the prototype**, so L11 stays open for later.
  - **The simplest test needs no controller:** a `snapclient` connects to a known stream and
    plays it. Audio in and out is PipeWire/GStreamer's business. F4 (controller) is therefore
    optional for the go/no-go.
  - The control-plane proposal (P2) is accepted as written.
  - The host runs Fedora 45 with **rootless Podman and SELinux**. The devcontainer runs as
    `spc_t` (unconfined, because of `seccomp=unconfined`) with `--userns=keep-id`, so host
    ACLs on `/dev/snd` apply.
  - Host Rust is current (1.99.1, rustup or Fedora). The host provides its own
    snapserver/snapclient, possibly self-compiled from the latest source.
  - A per-user plugin install is the default; a root/system install is fine too.

- 2026-11-19: devcontainer audio is **plain ALSA** (dmix on the loopback card via
  `~/.asoundrc`), not PipeWire. PipeWire works in the container only by hand: no udev, so
  the card has to be added manually, and `pipewiresink` deadlocks on stop with PipeWire 1.4.2.
- 2026-11-19: the devcontainer mounts the host's `/run/udev` read-only, so that WirePlumber
  may discover the sound card (not tested yet; low priority). No PulseAudio in the
  container: redundant with `pipewire-pulse` (reasons in `docs/testing-devcontainer.md`).
- 2026-11-19: **snapsrc adjusts the downstream audio sink** (`adjust-sink`, opt-out):
  `slave-method=resample`, because `autoaudiosink` cannot pass properties to its child and
  the default `skew` breaks sync (L12). snapsrc no longer provides a clock.
- 2026-11-19: Claude Code's config in the devcontainer lives on a named volume
  (`claude-code-config-${devcontainerId}`, `CLAUDE_CONFIG_DIR`).

## Known limitations

| # | Limitation | Where | Current workaround | Fix |
|---|---|---|---|---|
| L1 | Streams are fixed once `SnapServer::serve()` runs. `ServerCommand::AddStream` exists, but it always answers with an error after `run()`; `RemoveStream` works | snapcast-rs server | One stream per port; a second stream name fails with `StreamsFixed` | Upstream: runtime `add_stream` (**postponed**, 2026-11-10) |
| L2 | ~~The client's internal player `Stream` grows without bound~~ | snapcast-rs client | — | **Fixed 2026-11-19**: fork commit `feat(client): option to not fill the player stream` (`ClientConfig::player_stream`, upstream PR candidate); the engine sets it to `false`, and the clear-after-each-frame workaround is gone |
| L3 | ~~`TimeSyncComplete` arrives only after 50 quick syncs (5 s)~~ | snapcast-rs client | — | **Fixed 2026-11-19**: fork commit `feat(client): emit TimeSyncUsable once enough time-sync replies arrived` (after 5 replies, ~0.5 s; upstream PR candidate). The engine schedules on it; `MIN_SYNC_TIME` is gone |
| L4 | ~~FLAC chunks stamped with the newest input's time~~ (confirmed: +12 ms late, C++ +6…+22 ms) | snapcast-rs server | — | **Fixed 2026-11-19** in the fork (`fix(server): stamp encoded chunks with the time of their first sample`, upstream PR candidate): `Encoder::pending_frames`; FLAC now +0.5 ms (snapsrc) / +0.9 ms (C++). Tests: `aloop_*_plays_flac_on_the_server_timeline` |
| L5 | ~~Time-sync updates shift PTS; no smoothing~~ | `snapsrc` | — | **Resolved 2026-11-19**: snapcast-rs already takes the median of 200 syncs (like C++); `align.rs` turns remaining steps into gradual frame corrections. Measured: 0 corrections in 3 drift runs, error stable at about −110 µs |
| L6 | Sink input is only S16LE/S32LE; no S24_32LE | `snapsink` | `audioconvert` upstream | Plan step 3 |
| L7 | `sync=false` with a non-live source floods clients (nothing paces it) | `snapsink` | Keep `sync=true` | Document, or pace in the sink |
| L8 | No reconnect handling, server volume/mute, mDNS or client id in `snapsrc` | `snapsrc` | — | Plan step 4 |
| L9 | snapcast-rs `tracing` output is not visible in GStreamer logs | engine | — | Bridge `tracing` to the GStreamer debug log |
| L10 | JSON-RPC exists only in the `snapserver-rs` binary (`jsonrpc.rs`, `control.rs` for TCP, `http.rs` for HTTP/WS via axum, `auth.rs`). The library only has `ServerCommand`/`ServerEvent` | snapcast-rs | — | Prototype: copy it into `snapcast-engine`. Later, upstream a control library crate |
| L11 | snapcast-rs cannot serve **audio** over WebSocket (C++ snapserver: `/stream` on port 1780, used by Snapweb's player). Its WebSocket endpoint carries only JSON-RPC | snapcast-rs server | — | Only if a WebSocket audio client must work with `snapsink` (open question) |
| L12 | ~~`snapsrc` → `alsasink` (`skew`, 40 ms tolerance) follows a drifting card, not the server~~ | `snapsrc` + audio sink | — | **Fixed 2026-11-19**: snapsrc sets `slave-method=resample` on the downstream audio sink (`adjust-sink`); rate error −17…+4 ppm (was +300) on a −300 ppm card, ~1 ms from the server timeline |
| L13 | ~~`snapsrc` plays late by its pipeline latency~~ (+62 ms with 65 ms latency) | `snapsrc` PTS | — | **Fixed 2026-11-19**: `pts = playout − pipeline latency`; now −3…+1 ms, in sync with the C++ snapclient (+0.4 ms) |

## Plan (2026-11-10, agreed)

Goal: a **feasibility prototype**. Each step ends with `make check` green and this file
updated. Steps P1–P3 are mostly independent; P4 is the actual go/no-go.

**Feasibility criteria:** the prototype is a success if
- F1 (listen): a host desktop sender (`pipewiresrc ! … ! snapsink`) plays in sync on `snapsrc`
  and on a C++ `snapclient` at the same time, with no audible echo, dropouts or drift;
- F2 (measured): two receivers (`snapsrc`+`snapsrc`, `snapsrc`+C++ `snapclient`) are within
  ~1 ms of each other on the loopback device, including under emulated clock drift;
- F3 (endurance): it runs for hours without growing latency or memory, and without glitches;
- F4 (control, optional): a standard controller (Snapweb or Snapdroid) lists the clients, and changes
  to volume, mute and latency take effect in `snapsrc` and in the C++ client;
- F5 (lifecycle): PLAYING→PAUSED→PLAYING, stop/restart and a server restart (reconnect)
  all behave sanely.

**P1. Test infrastructure** (done 2026-11-19)
- Done: Makefile `install`/`uninstall` (release build into the per-user plugin dir
  `~/.local/share/gstreamer-1.0/plugins`, overridable via `PLUGIN_DIR`), `test` (no devices,
  as today), `test-interop` (C++ peers, `#[ignore]`d tests), `test-aloop` (snd-aloop tests),
  `doctor` (reports missing tools, elements and devices). Everything must work on the Fedora
  host as well as in the devcontainer.
- Done: listening scripts (`scripts/listen.sh`, called by `make listen-*`) for sender,
  receiver and combos with the C++ `snapclient`/`snapserver`.
- Done: `--device=/dev/snd` and the ALSA packages, verified after the rebuild.
- Done: `make test-aloop` (4 tests: snapsrc+snapsrc and snapsrc+C++ snapclient, each with
  and without card drift). Limitation: both receivers share one card, so it emulates "card
  drifts against the server", not two cards against each other.

**P2. Control plane** in `snapcast-engine` (feature `control`), copied and trimmed from
`snapserver-rs`:
JSON-RPC over TCP (1705) and WebSocket (1780 `/jsonrpc`), started with the server; `snapsrc`
applies server volume/mute (and exposes them read-only); `snapsink` forwards GStreamer tags
as stream metadata; `Stream.Control` becomes a bus message for the application. Port
property `control-port` (`0` = off); optional `snapweb-dir` for Snapweb's static files. No
auth, no persistence, no mDNS, no `Stream.AddStream` (L1).

**P3. Timing robustness for the prototype:** make the aloop tests pass. That means
(a) ~~snapsrc's absolute offset~~: **done 2026-11-19** (L13, see State).
(b) ~~drift (L12)~~ and (c) ~~start-up alignment~~: **done 2026-11-19** (see State).
Open from it: snapsrc's occasional ±1.5 ms wander within a run; the drift pair test's
1 ms tolerance versus the C++ client's own +2 ms under drift. Also smoothing for `diff_to_server` (L5) if the measurements
call for it, plus reconnect in `snapsrc` (F5).
L2/L3 stay engine workarounds, and L4 stays "pcm default".

**P4. Evaluation:** listening tests on the host, endurance run, write-up, and the decision.

**After the prototype** (unchanged, not scheduled): multiple streams (L1), upstream PRs
(L2, L3, a control crate, L4), finishing `snapsink` (L6, L7, Android build and CI), finishing
`snapsrc` (L8), infrastructure (CI, L9, packaging).
