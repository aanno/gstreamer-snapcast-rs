# Project memory: state, decisions, limitations, plan

Working memory for agents and humans continuing this project. Keep it current:
update **State** and **Plan** when a step is finished, and add decisions with a date.

Last updated: 2026-11-22 (P4: F5 lifecycle met, F3 endurance met (an hour on the aloop card); write-up `docs/evaluation.md` done, the go/no-go is the user's).

## Resume here (2026-11-22)

**2026-11-22, P4 in progress** (order agreed with the user: F5 → F3 → F1 in parallel →
write-up `docs/evaluation.md`; the go/no-go is the user's):
1. **F5 done.** Measured first: a sender pause gave receivers *no buffers* (they heard
   silence only because the audio sink underran). User: receivers should hear silence.
   Now snapsrc outputs silence buffers flagged GAP when nothing arrives (50 ms before
   render), bridges the hole up to returning audio with silence, and trims overlaps
   (jumps > 5 ms; smaller ones stay with the aligner; latency changes still resync with
   DISCONT, otherwise the drift tests got noisy). Tests: `tests/lifecycle.rs` (sender
   pause, receiver pause, restarted snapsrc = same client id) in `make check`;
   `aloop_sender_pause_is_silent_and_resumes_in_sync` in `make test-aloop`. Suspected
   snapsink resume bug was **not** ours: `audiotestsrc is-live` re-anchors its timestamps
   after PAUSED→PLAYING (hole or overlap of a few ms). The C++ snapclient is +2…+3.5 ms
   off for ~3 s after a stream gap (its own re-sync), snapsrc is not.
2. **F3 met (2026-11-22)** (`make test-endurance`, `aloop_endurance`; docs/testing.md).
   Two hour-long runs on the aloop card (snapsink → snapsrc + C++ snapclient, ticks plus a
   quiet sine). **Run 1** failed only on a measurement artifact in window 17 (the test
   process stalled, so the capture could not be mapped to clock time: both receivers
   appeared −499…+489 ms off the server while the pair stayed at +19 µs); otherwise all
   ticks, no gaps, pair +8…+68 µs per window, both receivers +0.3…+0.4 ms against the
   server. Fixed in the tooling (`capture_jitter_ms`, only waited-for buffers count,
   unreliable windows are skipped and listed, > 10 % fail). **Run 2 passed**: 3599/3599
   ticks in both receivers, no gaps, no unreliable windows; pair +776…+831 µs per window
   (single ticks +396…+1167 µs); against the server snapsrc +0.06…+0.14 ms, C++
   +0.86…+0.95 ms; trend snapsrc vs server +0.008 ms/h; RSS of the test process 41–47 MiB
   (−3.4 MiB/h after the first window), C++ 9 MiB flat. The pair offset is constant within
   a run but differs between runs (run 1 ~0, run 2 ~+0.8 ms; it is the C++ client that
   moved), so it is a per-start offset, not drift. Capture jitter in run 2: median 10.5 ms,
   max 19.3 ms, close to the 20 ms gate (the 2-minute check had 4–5 ms); raise
   `ENDURANCE_MAX_JITTER_MS` if long runs start skipping windows.
   **Write-up done: `docs/evaluation.md`** (2026-11-22; recommendation: go). **Next: the
   user's go/no-go**, then split this file (item 5), then L1.
   **F1:** the user listened to `listen-pair` on 2026-11-22: fine (a ~30 s track only).
4. **The known snapsrc wander, measured (2026-11-22):** in about one receiver run in four
   or five, snapsrc wanders by ±1.5…3 ms against the server within a 20 s run; it also
   happens without the gap filling (2 of 10 runs of the old code), so it is not new. The
   F2 tests (1 ms) are flaky because of it. The C++ snapclient does not wander. Main open
   timing item for the write-up. **Experiment after F3** (user's idea: "the price for
   GStreamer instead of ALSA directly? RT tricks?"), measured as usual: real-time priority
   for the sink's streaming thread; smoother slaving via the sink clock's
   `window-size`/`timeout`; the card clock as pipeline clock with snapsrc's aligner
   correcting (no sink slaving); the user also asked about direct/DMA (mmap) buffers and
   lock-free paths, and PipeWire's quantum ("quaternia" was a typo): on the host,
   snapsrc plays through PipeWire (`autoaudiosink` → pulsesink on pipewire-pulse), whose
   graph period (quantum, default 1024 frames ≈ 21 ms at 48 kHz) sets how coarsely the
   sink's clock advances; a small fixed quantum (`PIPEWIRE_QUANTUM=256/48000`, or
   `pw-metadata -n settings 0 clock.force-quantum 256`) may calm the slaving there. Host
   only: the aloop tests use ALSA directly, without PipeWire. My view: the wander comes from the
   slaving's rate estimate, not from the buffer transfer, so RT priority and the slaving
   method are the first candidates.
5. **After P4 (user, 2026-11-22): split this file.** Done and decided things go to
   `docs/claude/memory-old-2026-11-22.md`; this file keeps what is open and planned.
3. Snapweb "same ID" report (2026-11-22): not a bug, Snapweb's *MAC* field is the same
   for both clients on one host; the IDs differ (`listen.sh` passes `--hostID
   listen-cpp-client` to the C++ client). Offer: `--instance 2` instead, if the user
   wants `pair` to show the instance mechanism.


**End of 2026-11-21** (items 1 and 2 done on 2026-11-22: the browser re-test was fine; P4 started):
1. **Browser re-test of Snapweb** with the user (after `make install` on the host;
   `docs/testing-host.md`, "Control with Snapweb (F4)", has the checklist). Expect the
   client details filled in now: Client `gst-snapsrc <version>`, IP, OS as the
   distribution name (e.g. "Fedora Linux 45 …"), ID = MAC for instance 1. With
   `controls=play+pause+next` on snapsink, Snapweb's player buttons and
   `snapcast-stream-control` messages (`gst-launch-1.0 -m`). The user's first check
   looked fine.
2. Then P4 (evaluation).
3. **Unpushed fork commits** (`aanno/fixes`, each an upstream PR candidate): `9a4c2b4`
   feat(control) snapcast-control crate, `152dce6` fix(proto) stream properties
   camelCase, `7aede14` fix(server) stream properties in the status, `5b77ff7`
   fix(server) client info in the status, `4f8cb71` feat(client) distribution name as
   OS. The user pushes; this repo's pointer commits are on `main`.

**2026-11-21:** devcontainer rebuilt and checked: `make doctor`, `make check`,
`make test-interop` green; xerj and memory MCP up. **L9 done** (logging bridge, see
`docs/architecture.md`, which is new). Two new fork commits, **not pushed yet**:
`fix(server): run spawned tasks and the encoder thread in the caller's tracing context`
(upstream PR candidate, groundwork for L9) and `docs(server): link SnapServer::serve
instead of the removed run` (separate PR candidate; rustdoc `-D warnings` was broken).

Notes of 2026-11-19, still valid:

**Where we are:** P1 (test tooling) and P3 (timing) are done; the limitations round
(L2, L3, L4, L5, L7, L8 incl. mDNS, L12, L13) is done, one commit each. The fork commits
for snapcast-rs are **pushed** to `aanno/snapcast-rs` (user, 2026-11-19): FLAC timestamps
(L4), `TimeSyncUsable` (L3), `player_stream` (L2), client instance ids (L8). Each is an
upstream PR candidate for `metaneutrons/snapcast-rs`; no PR opened yet.

**Pending results** (the user runs them):
1. `make test-aloop` in the devcontainer: the first full run since the volume and mDNS
   changes. Expected: all 11 tests pass (bounds in `docs/testing.md`).
2. `scripts/test-host.sh` on the Fedora host (now with `make test-mdns`, and the
   `*-host.log` files land in the repo folder). After `make install`: listen to
   `make listen-pair` (should no longer echo since L13) and `HOST=` (mDNS) receivers.

**Next, in this order:**
1. Look at the pending results; fix what fails, record gotchas in `docs/testing*.md`.
2. **P2, control plane: done** (see Plan). The user checked Snapweb in a browser on
   2026-11-21 (volume works, client visible and renamable; the empty client details led
   to the `ClientInfo` fix). Open bits: ~~OS shows `linux`~~ (fixed 2026-11-21, fork `4f8cb71`:
   PRETTY_NAME from /etc/os-release, as C++), Snapweb's in-browser player (`/stream`), authentication. Fork commits **not pushed
   yet**: `9a4c2b4` feat(control), `152dce6` fix(proto), `7aede14` fix(server),
   `5b77ff7` fix(server) client info.
3. P4, evaluation: F3 endurance (hours, memory/latency), F5 lifecycle
   (PLAYING→PAUSED→PLAYING, stop/restart; reconnect is done), F1 listening, write-up.

**Housekeeping:** the xerj index (`ax-docs`, `ax-snapcast-rs-docs`) is a snapshot from
before today's code changes; reindex on the host before relying on it
(`docs/claude/mcp-xerj.md`). The memory MCP server holds the cross-project gotchas from
today (tags `gstreamer`, `snapcast`).

### Still open (2026-11-19)

Not blocking, left over from P3:

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

- **Workspace:** `gst-snapcast-engine` (no GStreamer dependency), `gst-snapcast-common`
  (GStreamer-side code shared by both plugins, so far the logging bridge),
  `gst-plugin-snapsink` (`libgstsnapsink.so`) and `gst-plugin-snapsrc` (`libgstsnapsrc.so`).
  snapcast-rs is a submodule (fork `aanno/snapcast-rs`, branch `aanno/fixes`), excluded
  from our workspace, used via path deps. The fork carries our upstream PR candidates
  (L2, L3, L4, L8, the tracing context for L9, a rustdoc fix). Crates, rules, threads
  and logging: `docs/architecture.md`.
- **listen.sh and cookbook (2026-11-21):** `scripts/listen.sh` prints every command it runs
  (copyable, a note per element; `show <cmd>`, `--help`, `QUIET=1`, `make listen-show`).
  New `docs/pipelines.md`: gst-launch introduction and recipes, each tested here unless
  marked. Found while testing the recipes: **fixed** snapsrc negotiating `rate=1,
  channels=1` before the stream format was known (`snapsrc ! audioconvert ! wavenc` failed
  with not-negotiated; now `negotiate()` waits for the format; regression test
  `first_caps_are_the_stream_format`), and **L14** (sender and receiver in one pipeline; won't fix, `snapsink async=false` documented).
- **Logging (L9, 2026-11-21):** snapcast-rs and engine `tracing` events and `log` records
  (mdns-sd, symphonia) go to the GStreamer debug log, categories `snapcast-client`,
  `-server`, `-engine`, `-mdns`, `-deps`, tagged `<snapsrc0>` / `<snapserver:<port>>` via
  span field `log_id`. Installed by `plugin_init` as the global subscriber if none is
  set; `GST_SNAPCAST_TRACING=0` turns it off. Tests: `gst-snapcast-common` unit tests,
  `crates/gst-plugin-snapsrc/tests/logging.rs` (end to end), fork
  `snapcast-server/tests/tracing_context.rs`.
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
  `gst-snapcast-engine/src/client.rs`). All engine times are µs of `CLOCK_MONOTONIC`
  (`gst_snapcast_engine::now_usec`).
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
  embedded server). The server half of `gst-snapcast-engine` and its dependencies must stay
  Android-buildable: no Linux-only APIs and no system C libraries beyond what GStreamer
  for Android provides. (2026-11-21: this is *not* why the engine is GStreamer-free;
  see that decision.)
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
  - **The control plane goes into `gst-snapcast-engine`.** ~~For the prototype, copying code from
    `snapserver-rs` is fine; refactoring snapcast-rs (e.g. a library crate for control) comes later.~~
    Superseded on 2026-11-19: no copying, a control library crate in snapcast-rs instead (see below).
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
- 2026-11-19: **snapsrc adjusts the downstream audio sink** (`adjust-sink`, opt-out) and
  no longer provides a clock (L12). What that means and why:
  `docs/architecture.md`, "Audio sink slaving (`adjust-sink`)".
- 2026-11-19 (user): **the MAC must not be the only client id**: several snapsrc on one
  host are normal (synchronized line-out together with a Bluetooth speaker). So
  `instance` defaults to automatic, coordinated host-wide by lock files (C++ snapclients
  don't take part; give them `--instance` above the snapsrc numbers), and the snapcast-rs
  server distinguishes instances like the C++ one.
- 2026-11-19: **mDNS via `mdns-sd`** (pure Rust, as snapclient-rs uses), not Avahi:
  keeps the sender Android-portable and needs no daemon. Discovery runs once at
  `connect` (blocks up to 5 s); reconnects keep that address. Tests on fixed ports set
  `mdns-name=""` so they don't announce themselves on the user's network.
- 2026-11-19 (user): **L10 / P2: we do not copy anything from `snapserver-rs`.** The JSON-RPC
  control plane becomes a **control library crate in snapcast-rs** (developed in the fork,
  meant for upstream, following snapcast-rs's conventions). `snapserver-rs` and
  `gst-snapcast-engine` both use it, so there is one implementation. This replaces the
  2026-11-10 "copying is fine for the prototype".
- 2026-11-19: Claude Code's config in the devcontainer lives on a named volume
  (`claude-code-config-${devcontainerId}`, `CLAUDE_CONFIG_DIR`).
- 2026-11-21 (with the user): **`gst-snapcast-engine` stays GStreamer-free, but not because
  of Android** (GStreamer for Android exists, and `snapsink` needs it there). Reasons:
  the path upstream into snapcast-rs, one timing boundary (the engine speaks only
  `CLOCK_MONOTONIC` µs), tests without GStreamer, gstreamer-rs churn stays in the plugins.
  Shared GStreamer code goes into the new crate **`gst-snapcast-common`**. Written down in
  `docs/architecture.md` (new: architecture and interfaces), CLAUDE.md, README.
- 2026-11-21 (with the user): **L9 bridge design.** Categories `snapcast-client`,
  `snapcast-server`, `snapcast-engine`, `snapcast-mdns`, `snapcast-deps` (by the crate of
  the target). A **global** subscriber, set only if none is set yet (covers threads we
  don't create; each plugin `.so` has its own `tracing` copy). Opt-out by environment
  variable `GST_SNAPCAST_TRACING=0`. Element names via spans with `log_id`, now (not later).
  Our own `log::Log` instead of `tracing-log`, so mdns-sd's daemon thread needs no tracing
  context. `tracing-gstreamer` is not used (other direction; stuck on gstreamer-rs 0.24).
- 2026-11-21 (with the user): **motivation, pros/cons, comparison with snapcast-rs,
  licence and publishing** are written down in `docs/architecture.md` (part 0 and 3), the
  motivation in short in the README. In short: the gain is for users and integrators (any
  source/sink, processing, one element instead of a FIFO plus daemon, e.g. Mopidy), not
  simpler code.
- 2026-11-21 (with the user): **the engine is renamed `gst-snapcast-engine`** (was
  `snapcast-engine`, which looked like part of snapcast-rs on crates.io), including its
  thread name and the instance lock directory (`$XDG_RUNTIME_DIR/gst-snapcast-engine`).
  The debug category stays `snapcast-engine`, so `GST_DEBUG=snapcast*` covers everything.
- 2026-11-21 (with the user): **two plugin crates stay** (not one `gst-plugin-snapcast` with
  features): the sender/receiver split stays explicit.
- 2026-11-21 (user): **licence stays GPL-3.0-only**, also for our own source (the binaries
  are GPL-3.0 anyway because snapcast-rs is linked in; LGPL for our code would gain
  nothing and would not get us into gst-plugins-rs, which takes only
  MPL-2.0/Apache-2.0/MIT; MPL-2.0 for our code was considered and rejected). Analysis:
  `docs/architecture.md`, "Licence".
- 2026-11-21: **not ready for crates.io** (in principle; no publishing planned): blocked
  by unreleased snapcast-rs fork commits, plus maturity and housekeeping. List:
  `docs/architecture.md`, "Publishing".
- 2026-11-21 (with the user): **P2 direction.**
  - **Snapweb is the user-facing control UI.** It is only served as static files
    (`snapweb-dir`), never vendored. ~~Debian ships it~~: Debian's `/usr/share/snapserver/snapweb` is only a placeholder page (found 2026-11-21); `scripts/fetch-snapweb.sh` downloads the release (v0.9.3, checksum pinned).
  - **No separate control application, no D-Bus.** JSON-RPC (TCP and WebSocket) is the
    inter-process interface: it is network-capable, language-neutral, and already spoken
    by Snapweb, Snapdroid and Home Assistant. D-Bus has no session bus on Android and is
    local-only. A separate process, if ever needed, is a JSON-RPC proxy. The HTTP stack
    stays out of builds that don't want it via the engine feature `control`, and out of
    pipelines at runtime via `control-port=0`. D-Bus only for an optional MPRIS add-on.
  - **The control contract is a Rust trait `SnapControl`** in the new control crate in
    snapcast-rs (one async method per `ServerCommand`, plus event subscription),
    implemented for `mpsc::Sender<ServerCommand>`. The JSON-RPC server is generic over
    it. `snapcast-server` stays unchanged. Later, optionally, a `RemoteControl` over
    JSON-RPC.
  - **Read-only reference submodules** `reference/snapcast` (v0.35.0) and
    `reference/snapweb` (v0.9.3): shallow, `update = none` (not fetched by
    `--recurse-submodules`), for the API spec and Snapweb's calls. Never built or linked.
- 2026-11-22 (user): **Conventional Commits in this repo too**, enforced by
  `.githooks/commit-msg` (`make setup`, the devcontainer runs it); pre-commit runs
  `cargo fmt --check`, pre-push `make check`. Pointer commits: `build(deps): bump
  snapcast-rs (...)`. Older commits keep their `area: ...` style.
- 2026-11-22 (with the user): **shared server across processes, after P4.** Goal: the
  first pipeline with `snapsink` starts one server instance (with the control API and
  Snapweb, if enabled); each further pipeline adds a *stream* to it; the instance ends
  with the last pipeline. Design (agreed in outline, details in a design doc first):
  - a separate **daemon executable** (engine + IPC server, no GStreamer) owns server,
    control, Snapweb and mDNS; `snapsink` becomes a thin IPC client. No leader election
    (the first pipeline's exit would kill everyone's server);
  - **per user**: Unix socket in `$XDG_RUNTIME_DIR`; **autospawn only** (first snapsink
    starts it, it exits after the last sink plus a short grace period); no systemd;
  - IPC carries PCM with `CLOCK_MONOTONIC` timestamps, so the timing work survives
    (unlike feeding a C++ snapserver through an untimed pipe/tcp source);
  - **default mode is shared** (user's wish); embedded stays available as a mode and is
    the default where no daemon can be spawned (Android). Exact `mode` semantics (silent
    fallback or error) to be settled in the design doc;
  - prerequisite: **L1** (runtime `add_stream` in snapcast-rs), un-postponed for this.

## Known limitations

| # | Limitation | Where | Current workaround | Fix |
|---|---|---|---|---|
| L1 | Streams are fixed once `SnapServer::serve()` runs. `ServerCommand::AddStream` exists, but it always answers with an error after `run()`; `RemoveStream` works | snapcast-rs server | One stream per port; a second stream name fails with `StreamsFixed` | Upstream: runtime `add_stream` (**postponed**, 2026-11-10) |
| L2 | ~~The client's internal player `Stream` grows without bound~~ | snapcast-rs client | — | **Fixed 2026-11-19**: fork commit `feat(client): option to not fill the player stream` (`ClientConfig::player_stream`, upstream PR candidate); the engine sets it to `false`, and the clear-after-each-frame workaround is gone |
| L3 | ~~`TimeSyncComplete` arrives only after 50 quick syncs (5 s)~~ | snapcast-rs client | — | **Fixed 2026-11-19**: fork commit `feat(client): emit TimeSyncUsable once enough time-sync replies arrived` (after 5 replies, ~0.5 s; upstream PR candidate). The engine schedules on it; `MIN_SYNC_TIME` is gone |
| L4 | ~~FLAC chunks stamped with the newest input's time~~ (confirmed: +12 ms late, C++ +6…+22 ms) | snapcast-rs server | — | **Fixed 2026-11-19** in the fork (`fix(server): stamp encoded chunks with the time of their first sample`, upstream PR candidate): `Encoder::pending_frames`; FLAC now +0.5 ms (snapsrc) / +0.9 ms (C++). Tests: `aloop_*_plays_flac_on_the_server_timeline` |
| L5 | ~~Time-sync updates shift PTS; no smoothing~~ | `snapsrc` | — | **Resolved 2026-11-19**: snapcast-rs already takes the median of 200 syncs (like C++); `align.rs` turns remaining steps into gradual frame corrections. Measured: 0 corrections in 3 drift runs, error stable at about −110 µs |
| L6 | Sink input is only S16LE/S32LE; no S24_32LE | `snapsink` | `audioconvert` upstream | Plan step 3 |
| L7 | ~~`sync=false` with a non-live source floods clients~~ | `snapsink` | — | **Fixed 2026-11-19**: snapsink paces itself, sending a chunk at most 200 ms ahead of its server timestamp (interruptible wait). Test `paces_a_non_live_source_without_sync` (before: 1.07 s of audio in 3 ms) |
| L8 | ~~No reconnect handling, server volume/mute, mDNS or client id in `snapsrc`~~ | `snapsrc` | — | **Done 2026-11-19**: reconnect (verified), client id (`host-id`, automatic `instance`, server appends `#instance`), server volume/mute (linear gain, read-only properties), **mDNS** via the pure-Rust `mdns-sd`: snapsink announces (`mdns-name`, not for `port=0`), snapsrc with an empty `host` discovers (optional `mdns-name` filter, 5 s, skips IPv6 link-local). Tests: `make test-mdns` (devcontainer: passes; the C++ peers need `avahi-daemon`, host only) |
| L9 | ~~snapcast-rs `tracing` output is not visible in GStreamer logs~~ | engine | — | **Fixed 2026-11-21**: `gst-snapcast-common::log` bridges `tracing` and `log` into the GStreamer debug log (`snapcast-*` categories, element names via `log_id` spans); fork commit for the server's tasks and encoder threads (upstream PR candidate). See `docs/architecture.md` |
| L10 | JSON-RPC exists only in the `snapserver-rs` binary (`jsonrpc.rs`, `control.rs` for TCP, `http.rs` for HTTP/WS via axum, `auth.rs`). The library only has `ServerCommand`/`ServerEvent` | snapcast-rs | — | **Changed 2026-11-19 (user): nothing is copied.** Create a control library crate in snapcast-rs (fork first, upstream PR candidate) that both `snapserver-rs` and `gst-snapcast-engine` use. 2026-11-21: the event → notification loop is in `main.rs` too and must move as well; gap list in Plan, P2. **Fixed in the fork 2026-11-21** (`9a4c2b4`, crate `snapcast-control`); used by the engine from P2 step 3 |
| L11 | snapcast-rs cannot serve **audio** over WebSocket (C++ snapserver: `/stream` on port 1780, used by Snapweb's player). Its WebSocket endpoint carries only JSON-RPC | snapcast-rs server | — | Only if a WebSocket audio client must work with `snapsink` (open question) |
| L12 | ~~`snapsrc` → `alsasink` (`skew`, 40 ms tolerance) follows a drifting card, not the server~~ | `snapsrc` + audio sink | — | **Fixed 2026-11-19**: snapsrc sets `slave-method=resample` on the downstream audio sink (`adjust-sink`); rate error −17…+4 ppm (was +300) on a −300 ppm card, ~1 ms from the server timeline |
| L13 | ~~`snapsrc` plays late by its pipeline latency~~ (+62 ms with 65 ms latency) | `snapsrc` PTS | — | **Fixed 2026-11-19**: `pts = playout − pipeline latency`; now −3…+1 ms, in sync with the C++ snapclient (+0.4 ms) |
| L14 | `snapsink` and `snapsrc` **in one pipeline** never reach PLAYING (no audio; Ctrl+C with `-e` hangs): snapsink prerolls and waits for PLAYING, the receiver's sink waits for its first buffer, which needs snapsink to send | both elements, GStreamer preroll | `async=false` on one sink (e.g. `snapsink async=false`); separate processes are not affected. Documented in `docs/pipelines.md` 5.4 | Won't fix (2026-11-21): a rare setup (sender and receivers normally run in separate processes); `snapsink` keeps the GstBaseSink default `async=true`. `async`/`sync` are now in the README property table and `docs/pipelines.md` (section 2 and 5.4) |

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

**Status (2026-11-19):** F1 partly (music through snapsrc on the host is clean; `listen-pair`
not re-listened since the L13 fix). F2 met in the aloop tests (≤ 1 ms; under drift ≤ 2.5 ms
because of the C++ reference). F3 not started. F4 needs P2. F5: reconnect done
(`snapsrc_resumes_after_a_server_restart`), the rest untested.

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

**P2. Control plane** in `gst-snapcast-engine` (feature `control`), built on a **new control
library crate in snapcast-rs** (2026-11-19: nothing is copied from `snapserver-rs`; the
crate is extracted there, `snapserver-rs` uses it too, upstream PR candidate, L10).
Direction agreed 2026-11-21 (see Decisions): Snapweb as the UI, JSON-RPC as the only
inter-process interface, contract = trait `SnapControl`. Already done before P2
(2026-11-19, L8): `snapsrc` applies the server's volume/mute (read-only `volume`/`mute`).

1. **Analysis: done 2026-11-21.** Reference submodules added; gap list below.
2. **Control crate in the fork: done 2026-11-21** (fork commit `9a4c2b4`, one upstream PR
   candidate). New library crate **`snapcast-control`** (name and `http` feature agreed
   with the user): `jsonrpc.rs`, `notify.rs`, `tcp.rs` (was `control.rs`), `http.rs`,
   `auth.rs` moved from `snapserver-rs` with `git mv`; the event → notification loop from
   `main.rs` became `ControlServer::forward_events`. API: trait **`SnapControl`** (one
   async method per `ServerCommand` the API needs; returns `impl Future + Send`, impl for
   `mpsc::Sender<ServerCommand>`), **`ControlServer<C>`** (`new`, `with_auth`,
   `with_extensions`, `notify`, `serve_tcp(listener)`, `serve_http(listener, doc_root)`,
   `forward_events(events, passthrough)`), `ControlEvent`, `ControlError`, `auth`.
   Differences from the 2026-11-21 sketch: events are **not** in the trait (they come
   from `SnapServer::new`, not the command channel), and `ServerEvent` is not `Clone`, so
   `forward_events` passes each event on through an optional channel instead of
   broadcasting it. Feature `http` (default) gates axum/tower-http; auth is always in
   (pure Rust, off by default). `snapcast-server` unchanged; `snapserver-rs` behaves as
   before (smoke-tested: TCP, HTTP POST, Snapweb from `doc_root`). New end-to-end tests
   `snapcast-tests/tests/control.rs` (TCP with a real client, WebSocket, POST, static files).
3. **Engine and snapsink: done 2026-11-21** (except `Stream.Control`, see below). Engine
   feature `control`: `ServerOptions::control` (`ControlOptions`: `tcp_port`, `http_port`,
   `Some(0)` = ephemeral, `snapweb_dir`), bound in `acquire_stream` (a taken port fails
   it), served on the engine runtime, aborted with the server;
   `StreamHandle::control_tcp_port`/`control_http_port`; mDNS announces
   `_snapcast-ctrl._tcp`/`_snapcast-http._tcp` with the stream port (one daemon).
   `StreamHandle::set_metadata(StreamMetadata)` (try_send, never blocks). `snapsink`:
   `control-port`, `http-port` (0 = off, default: opt-in because there is no auth and
   tests/C++ peers use 1705/1780), `snapweb-dir`; tag events → metadata, republished only
   on change. Needed two snapcast-rs fixes (fork, PR candidates): `fix(proto)`
   `StreamProperties` camelCase and no `null`s (Snapweb crashes on `"metadata": null`),
   `fix(server)` `to_status` dropped stream properties. Tests: engine
   `control_api_runs_and_stops_with_the_server`, snapsrc `tests/control.rs` (JSON-RPC
   status with tags, `Client.SetVolume` reaches snapsrc, Snapweb files over HTTP).
   `scripts/fetch-snapweb.sh`; `listen.sh` `CONTROL=1`.
   **`Stream.Control` (2026-11-21, design agreed with the user: property plus bus
   message):** snapsink flags property `controls` (`play+pause+next+previous+seek`,
   mutable in PLAYING) → `canPlay`/… and `canControl`; `playbackStatus` follows the
   element state (READY→PAUSED and PLAYING→PAUSED: paused, PAUSED→PLAYING: playing).
   Engine: `StreamProperties` (metadata + `StreamControls` + `PlaybackStatus`) replaces
   `set_metadata` (the server replaces the whole map, so they go together);
   `StreamHandle::on_command` gets `StreamCommand { command, params_json }`, routed from
   `ServerEvent::StreamControl` in the event loop. snapsink posts element message
   `snapcast-stream-control` (`stream`, `command`, `params`); it does not act on it.
   **Browser check findings (user, 2026-11-21):** "Unknown title/artist" with ticks
   (no tags; `listen.sh` now adds `taginject` for ticks/sine); details Client, IP, OS
   empty → snapcast-rs `fix(server)` `5b77ff7` (`state::ClientInfo` from the Hello and
   the peer address); ID = MAC is right for instance 1 (C++ `getUniqueId`: `#n` only for
   instance ≠ 1); snapsrc's `client-name` is the client *software* name (blurb fixed).
   Original plan text:
   feature `control`; one control endpoint per shared server.
   The crate takes over the server's `mpsc::Receiver<ServerEvent>` (the engine only logs
   it today) and fans it out (notifications, engine, logging). `snapsink`: properties
   `control-port` (`0` = off) and `snapweb-dir`; GStreamer tags as stream metadata;
   `Stream.Control` as a bus message. mDNS for the control ports as C++ does:
   `_snapcast-ctrl._tcp` (TCP control) and `_snapcast-http._tcp` (HTTP). No auth, no
   persistence, no `Stream.AddStream` (L1).
4. **F4 check:** Snapweb lists our `snapsrc` and a C++ `snapclient`, changes volume, mute,
   name, latency and the group's stream.

**Gap list (2026-11-21; C++ snapcast v0.35.0 `doc/json_rpc_api/`, Snapweb v0.9.3
`src/snapcontrol.ts`, `snapserver-rs` on `aanno/fixes`):**

| Area | C++ spec | Snapweb uses | snapserver-rs |
|---|---|---|---|
| Methods | `Server.{GetRPCVersion,GetStatus,DeleteClient}`, `Client.{GetStatus,SetVolume,SetLatency,SetName}`, `Group.{GetStatus,SetMute,SetStream,SetClients,SetName}`, `Stream.{Control,SetProperty,AddStream,RemoveStream}` | `Server.{GetStatus,DeleteClient}`, `Client.{SetVolume,SetLatency,SetName}`, `Group.{SetMute,SetStream,SetClients}`, `Stream.Control` | all of the spec, plus `Server.{GetToken,Authenticate}` (auth); RPC version 2.0.0 like C++ |
| Notifications | `Client.On{Connect,Disconnect,VolumeChanged,LatencyChanged,NameChanged}`, `Group.On{Mute,StreamChanged,NameChanged}`, `Stream.On{Update,Properties}`, `Server.OnUpdate` | all of these except `Group.OnNameChanged` | all of the spec; but the event loop that emits them is in the binary (`main.rs`), not reusable yet |
| Endpoints | TCP 1705, HTTP/WS 1780: `/jsonrpc`, `/stream`, static files | `ws://<page host>/jsonrpc`; `/stream` for the in-browser player | `/jsonrpc` (WS and POST), `doc_root`; **no `/stream`** |
| mDNS | `_snapcast._tcp`, `_snapcast-ctrl._tcp`, `_snapcast-http._tcp`, `_snapcast-https._tcp` | – | (embedded case: our engine announces `_snapcast._tcp` only) |

Consequences: method and notification coverage is complete for Snapweb; the work is the
extraction (including the event loop) and the engine wiring. **Not covered:** Snapweb's
in-browser player (`/stream`, a binary Snapcast stream over WebSocket); a later,
separate snapcast-rs PR. Still to verify at F4: that the JSON shapes (`Server.GetStatus`,
`Stream.OnProperties`) match what Snapweb reads; streams without properties (no
`canControl`) should simply hide Snapweb's player controls.

**P3. Timing robustness for the prototype:** **done 2026-11-19.** (a) absolute offset
(L13), (b) drift (L12), (c) start-up alignment, plus L5 (resolved by measurement) and
reconnect (verified, F5 part). All aloop tests pass with the bounds in `docs/testing.md`.
Left open, not blocking: snapsrc's occasional tick-to-tick jitter (±0.7 ms in single runs,
from the sink's `resample` slaving; could be smoothed via the sink clock's
`window-size`/`timeout`); the drift tests use 2.5 ms because the C++ snapclient itself is
~2 ms off there. L2, L3, L4 are fixed in the fork (no engine workarounds any more).

| P3 item | State (2026-11-19) |
|---|---|
| (a) absolute offset (L13) | fixed: snapsrc and C++ snapclient each about 1 ms from the server timeline |
| (b) drift (L12) | fixed: snapsrc follows the server on a −300 ppm card (rate error −17…+4 ppm) |
| (c) start-up alignment | fixed: two snapsrc usually < 0.1 ms apart |
| L5, reconnect | L5 needs nothing (measured); reconnect verified |
| left open, not blocking | tick-to-tick jitter ±0.7 ms in single runs (sink's `resample` slaving); drift tests allow 2.5 ms because the C++ client itself is ~2 ms off there |

**P4. Evaluation:** listening tests on the host, endurance run, write-up, and the decision.
Write-up: `docs/evaluation.md` (2026-11-22).

| Criterion | State (2026-11-19) |
|---|---|
| F1 listening (desktop sender, snapsrc + C++ in sync) | mostly: music through snapsrc on the host is clean since the frame-loss fix; `listen-pair` fine (user, 2026-11-22, a ~30 s track); a longer desktop-music session is still open |
| F2 measured (≤ 1 ms, also under drift) | met in the aloop tests; under drift only to 2.5 ms, because of the C++ reference |
| F3 endurance (hours, no growth) | **met (2026-11-22)**: an hour on the aloop card, all ticks, no gaps, no growth in latency or memory (run 2; run 1 failed only on a measurement artifact, since fixed) |
| F4 controller (optional) | met: Snapweb in the browser (user, 2026-11-21/22) |
| F5 lifecycle | **met (2026-11-22)**: sender pause → silence (GAP) on a continuous timeline, in sync after the resume (aloop, with the C++ client); receiver pause resumes on schedule; restarted snapsrc keeps its client id; reconnect after a server restart |

**Right after P4** (decided 2026-11-22, see Decisions): L1 in the fork, then the shared
server daemon (design doc first).

**After the prototype** (unchanged, not scheduled): upstream PRs
(the fork commits for L2, L3, L4, the client-instance fix, the tracing context and the
rustdoc fix; the control crate is part of P2 now), finishing `snapsink` (L6, L7, Android
build and CI), finishing `snapsrc` (L8), infrastructure (CI, packaging).
