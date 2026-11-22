# Project memory: state, decisions, limitations, plan

Working memory for agents and humans continuing this project. Keep it current: update
**Resume here**, **State** and **Plan** when a step is finished, and add decisions with a
date. This file holds only what is current, open or planned. The history up to the end of
the prototype (P1–P4 in detail, every decision with its reasons, the fixed limitations
L2–L13, all measurements) is in [memory-old-2026-11-22.md](memory-old-2026-11-22.md),
frozen; the evaluation in [../evaluation.md](../evaluation.md).

Last updated: 2026-11-22 (daemon steps 1 and 2 done: IPC, daemon, snapsink `mode`, lean build; next: step 3).

## Resume here (2026-11-22)

1. **The feasibility prototype is done: go** (user, 2026-11-22, agreeing to
   `docs/evaluation.md`). F1–F5 met; F1 and F2 with caveats (see Open items).
2. **L1 done (2026-11-22).** Fork commit `1b5b3b5` `feat(server): add and remove
   embedder-fed streams while the server runs` (upstream PR candidate):
   `ServerCommand::AddAudioStream` (answers with the frame sender), `RemoveStream` now
   stops the encoder and forgets the codec header (name reusable), new clients start on
   the first playable stream when the default was removed; URI-based `Stream.AddStream`
   stays rejected. Engine: another stream name on a running port is added at runtime;
   a released stream stays (clients keep their assignment across a sender restart);
   re-acquiring it with another codec/format replaces it. `EngineError::StreamsFixed`
   and `StreamMismatch` are gone, `StreamNotAdded` is new. **Next: the shared-server
   daemon**, design `docs/shared-server.md` (agreed 2026-11-22; its "Decisions"
   section has the answers). **Step 1 done (2026-11-22):** protocol
   `gst_snapcast_engine::ipc` (feature `shared`), crate `gst-snapcast-daemon`
   (`--socket PATH`), tests (a marker plays −14…−21 µs from timestamp + buffer through
   the daemon). **Step 2 done (2026-11-22):** engine `Mode`
   (auto/shared/embedded) and a shared `StreamHandle` backend; `snapsink` `mode`
   (default `auto`), `active-mode`, features `shared` (default) / `control` (user agreed
   to the lean split: release 3.1 MB instead of 5.8 MB). `make install` still builds
   `snapsink` with `control` until steps 3/5 (so `CONTROL=1 make listen-send` keeps
   working); drop `SNAPSINK_FEATURES ?= control` then. **Next: step 3** (autospawn,
   lock-file singleton, grace period, reconnect after a daemon crash). Later: fork PR
   candidate "make `snapcast-control`'s auth an optional feature" (agreed in principle).
3. **Alongside:** the snapsrc wander experiment (Open items), measured as usual.
4. **Pushed** by the user on 2026-11-22: the fork up to `1b5b3b5` and this repo up to
   `b4f135c`. No
   upstream PR is open yet; 13 fork commits are PR candidates.
5. **Housekeeping:** the xerj index (`ax-docs`, `ax-snapcast-rs-docs`) is a snapshot from
   before 2026-11-21; reindex on the host before relying on it (`docs/claude/mcp-xerj.md`).
6. **Offer, not done:** start the C++ client in `listen.sh pair` with `--instance 2`, if the
   user wants `pair` to show Snapweb's instance mechanism (both clients have the same MAC;
   their ids differ already).

## State

What works, with the tests that cover it (all details in the archive and in
`docs/testing.md`):

- **Workspace:** `gst-snapcast-engine` (no GStreamer), `gst-snapcast-common` (logging
  bridge), `gst-plugin-snapsink`, `gst-plugin-snapsrc`. snapcast-rs is a submodule (fork
  `aanno/snapcast-rs`, branch `aanno/fixes`). Crates, rules, threads and logging:
  `docs/architecture.md`.
- **Sender** `snapsink`: embedded server, one per port and process, shared by the sinks of
  that process (registry by port; `port=0` = ephemeral, read `bound-port`); each sink
  feeds its own stream, further names join the running server (L1 done). Paces non-live sources. mDNS announce. Control plane: JSON-RPC (TCP,
  HTTP/WebSocket) and Snapweb (`control-port`, `http-port`, `snapweb-dir`; opt-in), tags as
  stream metadata, `controls` with `Stream.Control` as the bus message
  `snapcast-stream-control`. PCM and FLAC (default `pcm`).
- **Receiver** `snapsrc`: embedded client, PTS from server time; adjusts the downstream
  audio sink to `slave-method=resample` (`adjust-sink`); provides no clock; fills gaps with
  silence flagged GAP; reconnects; client id with automatic `instance`; server volume/mute;
  mDNS discovery.
- **Logging:** `tracing`/`log` bridged into the GStreamer debug log (`snapcast-*`
  categories); `GST_SNAPCAST_TRACING=0` turns it off.
- **Tests:** `make check` (unit, loopback, lifecycle, control, logging, reconnect),
  `make test-interop` (C++ peers both ways), `make test-aloop` (timing on the snd-aloop
  card), `make test-endurance` (F3, an hour), `make test-mdns`. Green in the devcontainer;
  on the host last checked fully on 2026-11-19.
- **Timing design** (details in the module docs of `snapsink/imp.rs`, `snapsrc/imp.rs`,
  `snapsrc/align.rs` and `gst-snapcast-engine/src/client.rs`). Engine times are µs of
  `CLOCK_MONOTONIC` (`gst_snapcast_engine::now_usec`).
  - **Sink:** server timestamp = `now_usec() + (base_time + running_time + latency − clock.time())`;
    without a clock it counts samples.
  - **Source:** play-out = `server_ts − diff_to_server + (buffer − server_latency − client_latency)`,
    ideal PTS = `clock.time() + (playout − now_usec()) − base_time − pipeline latency`.
    `align.rs` keeps the PTS contiguous and steers towards the ideal by inserting/dropping
    single frames (deadband; resync with DISCONT beyond `RESYNC_NS` or on a latency change).
    Network arrival time is never used.
  - **Gaps:** when nothing arrives, silence (GAP) up to 50 ms before render; a hole up to
    returning audio is bridged with silence, an overlap is trimmed (jumps > 5 ms).
  - **Clock:** in live pipelines the monotonic system clock; the audio sink resamples its
    card to it.
  - **Late frames:** dropped when `playout + latency < now`; the next buffer is DISCONT.
- **Measured (2026-11-19…22):** receivers about 1 ms or less from the server timeline;
  snapsrc + snapsrc usually < 0.1 ms apart, snapsrc + C++ within 1 ms; under −300 ppm card
  drift snapsrc follows the server (−17…+4 ppm). An hour: all ticks, no gaps, no growth in
  latency or memory. Numbers: `docs/evaluation.md`.

## Open items (timing and evaluation)

- **snapsrc wander** (the main timing item): in about one run in four or five, snapsrc
  wanders by ±1.5…3 ms against the server within a run; the C++ client does not. Not the
  aligner (0 corrections in those runs) but the audio sink's `resample` slaving, whose rate
  estimate is sometimes restless. Happened as often before the gap filling (2 of 10 runs).
  Makes the 1 ms aloop tests flaky (rerun). **Experiment** (user's idea: "the price for
  GStreamer instead of ALSA directly? RT tricks?"), measured before changing anything:
  real-time priority for the sink's streaming thread; smoother slaving via the sink clock's
  `window-size`/`timeout`; the card clock as pipeline clock with snapsrc's aligner
  correcting (no sink slaving); direct/DMA (mmap) buffers and lock-free paths (user's
  questions); on the host, PipeWire's quantum (default 1024 frames ≈ 21 ms;
  `PIPEWIRE_QUANTUM=256/48000` or `pw-metadata -n settings 0 clock.force-quantum 256`,
  `docs/testing-host.md`). My view: the rate estimate, not the buffer transfer, so RT
  priority and the slaving method first.
- **Run-to-run offset snapsrc ↔ C++ client** (0…+0.8 ms, constant within a run; the C++
  client moves against the server). Inside 1 ms; look at it with the wander.
- **C++ client under drift and after gaps:** +1.7…+2.0 ms off the server on a −300 ppm card
  (so the drift tests allow 2.5 ms), +2…+3.5 ms for ~3 s after a stream gap. The reference's
  behaviour, not ours.
- **Endurance capture jitter:** median 10.5 ms, max 19.3 ms in the second hour-long run,
  close to the 20 ms gate (`ENDURANCE_MAX_JITTER_MS`); raise it if long runs start
  skipping windows.
- **F1:** only short listening sessions so far; a longer one with desktop music is open.
- **Test setup:** both aloop receivers share one card, so "two cards drifting against each
  other" is not covered.

## Decisions in force

Short form; dates and reasons in the archive. CLAUDE.md has the coding rules.

- 2026-11-07: **snapcast-rs changes go upstream** to `metaneutrons/snapcast-rs`: small,
  self-contained PR candidates in the fork, its conventions, no project-specific hacks.
- 2026-11-07: **Android is a near-term target for the sender** (snapsink, engine `server`):
  no Linux-only APIs, no system C libraries beyond GStreamer for Android. **No Android
  receiver** (a websocket-based app exists); snapsrc may stay desktop-only.
- 2026-11-10: **F4 (controller) is optional**; the Android app (WebSocket audio plus
  control) is not needed now, so L11 stays open.
- 2026-11-19: **snapsrc adjusts the downstream audio sink** (`adjust-sink`, opt-out) and
  provides no clock. Why: `docs/architecture.md`, "Audio sink slaving".
- 2026-11-19 (user): **the MAC must not be the only client id**: automatic `instance`,
  coordinated host-wide by lock files; C++ clients need `--instance` above the snapsrc numbers.
- 2026-11-19: **mDNS via `mdns-sd`** (pure Rust), not Avahi. Tests on fixed ports set
  `mdns-name=""`.
- 2026-11-19 (user): **nothing is copied from `snapserver-rs`**; shared code becomes library
  crates in snapcast-rs (done for control: `snapcast-control`).
- 2026-11-21: **the engine stays GStreamer-free** (upstream path, one timing boundary,
  tests without GStreamer, gstreamer-rs churn in the plugins); shared GStreamer code goes
  into `gst-snapcast-common`. **Two plugin crates stay.**
- 2026-11-21: **logging bridge**: global subscriber only if none is set, opt-out
  `GST_SNAPCAST_TRACING=0`, element names via `log_id` spans.
- 2026-11-21: **licence GPL-3.0-only**; **not ready for crates.io** (fork commits
  unreleased; `docs/architecture.md`, "Publishing").
- 2026-11-21: **control direction**: Snapweb is the UI (served from `snapweb-dir`, never
  vendored); JSON-RPC is the only inter-process control interface (no D-Bus, except an
  optional MPRIS add-on); the contract is the trait `SnapControl` in `snapcast-control`.
  Reference submodules are read-only.
- 2026-11-22 (user): **Conventional Commits** here too, enforced by `.githooks/` (`make setup`).
- 2026-11-22 (user): **P4 ends with go**; `docs/evaluation.md` agreed.
- 2026-11-22 (with the user): **shared server across processes**, next after L1. The first
  pipeline with `snapsink` starts one server instance (with control and Snapweb, if
  enabled); each further pipeline adds a *stream*; the instance ends with the last
  pipeline. Outline (details in a design doc first):
  - a separate **daemon executable** (engine + IPC server, no GStreamer) owns server,
    control, Snapweb and mDNS; `snapsink` becomes a thin IPC client; no leader election;
  - **per user**: Unix socket in `$XDG_RUNTIME_DIR`; **autospawn only** (exits after the
    last sink plus a grace period); no systemd;
  - IPC carries PCM with `CLOCK_MONOTONIC` timestamps, so the timing work survives;
  - **default mode is shared** (user's wish); embedded stays available and is the default
    where no daemon can be spawned (Android). Exact `mode` semantics (silent fallback or
    error) to be settled in the design doc;
  - prerequisite: **L1**.

## Known limitations (open)

Fixed ones (L2–L5, L7–L10, L12, L13) are in the archive.

| # | Limitation | Where | Current workaround | Fix |
|---|---|---|---|---|
| L1 | ~~Streams are fixed once `SnapServer::serve()` runs~~ | snapcast-rs server | — | **Fixed 2026-11-22** (fork `1b5b3b5`, upstream PR candidate): `ServerCommand::AddAudioStream` for embedder-fed streams; URI-based `Stream.AddStream` stays rejected (upstream's "application-owned" stance). Engine and snapsink use it. Left: streams are shared only within one process (the daemon) |
| L6 | Sink input is only S16LE/S32LE; no S24_32LE | `snapsink` | `audioconvert` upstream | When finishing snapsink |
| L11 | snapcast-rs cannot serve **audio** over WebSocket (C++ snapserver: `/stream` on port 1780, used by Snapweb's in-browser player). Its WebSocket endpoint carries only JSON-RPC (upstream README: "No WebSocket audio transport") | snapcast-rs server | — | Only if a WebSocket audio client must work with `snapsink`; a separate snapcast-rs change |
| L14 | `snapsink` and `snapsrc` **in one pipeline** never reach PLAYING: snapsink prerolls and waits for PLAYING, the receiver's sink waits for its first buffer | both elements, GStreamer preroll | `async=false` on one sink (e.g. `snapsink async=false`); documented in `docs/pipelines.md` 5.4 | Won't fix (2026-11-21): a rare setup |
| — | No authentication on the control API in `snapsink` (snapcast-control has auth, off) | `snapsink` | opt-in ports, local network | Later, if needed |

## Plan

1. ~~**L1 in the fork**~~ done 2026-11-22 (see Resume here).
2. ~~**Design doc for the shared-server daemon**~~ agreed 2026-11-22 (`docs/shared-server.md`).
3. **The daemon**, in the doc's five steps (step 1 done): engine client and `snapsink`
   `mode` (step 2), autospawn and lifetime (3), properties and commands (4), tests and
   install (5).
4. **Alongside:** the wander experiment (Open items).

**After that** (not scheduled): upstream PRs for the fork commits; finishing `snapsink`
(L6, Android build and CI); infrastructure (CI, packaging); L11 if a WebSocket audio client
must work; a longer F1 listening session.
