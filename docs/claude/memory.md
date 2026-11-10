# Project memory: state, decisions, limitations, plan

Working memory for agents and humans continuing this project. Keep it current:
update **State** and **Plan** when a step is finished, and add decisions with a date.

Last updated: 2026-11-10 (plan revised: feasibility prototype; P1 test tooling mostly done).

## Resume here (2026-11-10, before a devcontainer rebuild)

- **Uncommitted work** (the user commits): P1 test tooling, meaning the Makefile targets,
  `scripts/doctor.sh`, `scripts/listen.sh` and `crates/*/tests/interop_cpp.rs`, plus
  `devcontainer.json` (`--device=/dev/snd`) and the Dockerfile (`gstreamer1.0-alsa`,
  `alsa-utils`). The docs are updated. `make check` and `make test-interop` were green before
  the rebuild.
- **First, after the rebuild:**
  1. Run `make doctor`. Expect `/dev/snd` to be accessible, the `Loopback` card, `alsasink`/
     `alsasrc`, and `snapserver` + `snapclient` 0.31. If `/dev/snd` is not accessible, look
     at Podman ACLs/groups (maybe `--group-add keep-groups`).
  2. Run `make check` and `make test-interop` again.
  3. Ask the user for the host results (`make install`, `make doctor`, `make listen-all`).
     Watch for two things there: `SRC=desktop` relies on the PipeWire property
     `stream.capture.sink`, and a newer `snapclient` may want `tcp://host:port` instead of
     `-h/-p`.
- **Then:** write `make test-aloop` (P1, open item). After that, stop and discuss before P2.

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
  uses the tree's release build). Smoke-tested in the devcontainer with file/fake sinks only.
  The real listening runs happen on the user's Fedora host. With `snapsrc`, gst-launch picks
  the clock `snapsrc` provides ("New clock: systemclock0").
- **Timing design** (details in the module docs of `snapsink/imp.rs`, `snapsrc/imp.rs` and
  `snapcast-engine/src/client.rs`). All engine times are µs of `CLOCK_MONOTONIC`
  (`snapcast_engine::now_usec`).
  - **Sink:** server timestamp = `now_usec() + (base_time + running_time + latency − clock.time())`,
    which works with any pipeline clock. Without a clock it falls back to counting samples.
  - **Source:** play-out = `server_ts − diff_to_server + (buffer − server_latency − client_latency)`,
    and PTS = `clock.time() + (playout − now_usec()) − base_time`. Network arrival time is
    never used.
  - **Clock:** `snapsrc` provides a monotonic `GstSystemClock`, so audio sinks slave to the
    Snapcast timeline. Its latency query answers min 0, max unbounded.
  - **Late frames:** frames whose play-out time has passed are dropped and the next buffer
    is marked DISCONT.
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

## Known limitations

| # | Limitation | Where | Current workaround | Fix |
|---|---|---|---|---|
| L1 | Streams are fixed once `SnapServer::serve()` runs. `ServerCommand::AddStream` exists, but it always answers with an error after `run()`; `RemoveStream` works | snapcast-rs server | One stream per port; a second stream name fails with `StreamsFixed` | Upstream: runtime `add_stream` (**postponed**, 2026-11-10) |
| L2 | The client's internal player `Stream` is never drained when you don't use its cpal player, so it grows without bound | snapcast-rs client | `ClientHandle::recv` clears it after each frame | Upstream: make the player stream optional |
| L3 | `TimeSyncComplete` arrives only after 50 quick syncs (5 s) | snapcast-rs client | Schedule frames after `MIN_SYNC_TIME` (500 ms) | Upstream: a "sync usable" event, or a configurable count |
| L4 | **Suspected, not verified:** the FLAC encoder buffers samples, but each output chunk carries the timestamp of the latest input, which could make playback up to ~24 ms late, with jitter | snapcast-rs server `encoder/flac.rs` | Default codec is `pcm` | Write a test first; upstream fix if confirmed |
| L5 | Time-sync updates shift PTS slightly; no smoothing | `snapsrc` | Audio sinks tolerate this (alignment threshold) | Plan step 1 |
| L6 | Sink input is only S16LE/S32LE; no S24_32LE | `snapsink` | `audioconvert` upstream | Plan step 3 |
| L7 | `sync=false` with a non-live source floods clients (nothing paces it) | `snapsink` | Keep `sync=true` | Document, or pace in the sink |
| L8 | No reconnect handling, server volume/mute, mDNS or client id in `snapsrc` | `snapsrc` | — | Plan step 4 |
| L9 | snapcast-rs `tracing` output is not visible in GStreamer logs | engine | — | Bridge `tracing` to the GStreamer debug log |
| L10 | JSON-RPC exists only in the `snapserver-rs` binary (`jsonrpc.rs`, `control.rs` for TCP, `http.rs` for HTTP/WS via axum, `auth.rs`). The library only has `ServerCommand`/`ServerEvent` | snapcast-rs | — | Prototype: copy it into `snapcast-engine`. Later, upstream a control library crate |
| L11 | snapcast-rs cannot serve **audio** over WebSocket (C++ snapserver: `/stream` on port 1780, used by Snapweb's player). Its WebSocket endpoint carries only JSON-RPC | snapcast-rs server | — | Only if a WebSocket audio client must work with `snapsink` (open question) |

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

**P1. Test infrastructure** (done, except the snd-aloop tests)
- Done: Makefile `install`/`uninstall` (release build into the per-user plugin dir
  `~/.local/share/gstreamer-1.0/plugins`, overridable via `PLUGIN_DIR`), `test` (no devices,
  as today), `test-interop` (C++ peers, `#[ignore]`d tests), `test-aloop` (snd-aloop tests),
  `doctor` (reports missing tools, elements and devices). Everything must work on the Fedora
  host as well as in the devcontainer.
- Done: listening scripts (`scripts/listen.sh`, called by `make listen-*`) for sender,
  receiver and combos with the C++ `snapclient`/`snapserver`.
- Prepared: `--device=/dev/snd` in `devcontainer.json`, plus `gstreamer1.0-alsa` and
  `alsa-utils` in the Dockerfile. This takes effect after a container rebuild; not verified yet.
- Open: `make test-aloop`, an `#[ignore]`d test that plays two receivers into loopback
  subdevices, records them back and measures their offset (F2). It also uses
  `PCM Rate Shift` to emulate clock drift between "sound cards". It needs the rebuilt
  container (or the host).

**P2. Control plane** in `snapcast-engine` (feature `control`), copied and trimmed from
`snapserver-rs`:
JSON-RPC over TCP (1705) and WebSocket (1780 `/jsonrpc`), started with the server; `snapsrc`
applies server volume/mute (and exposes them read-only); `snapsink` forwards GStreamer tags
as stream metadata; `Stream.Control` becomes a bus message for the application. Port
property `control-port` (`0` = off); optional `snapweb-dir` for Snapweb's static files. No
auth, no persistence, no mDNS, no `Stream.AddStream` (L1).

**P3. Timing robustness for the prototype:** the aloop drift tests (F2) and smoothing for
`diff_to_server` (L5) if the measurements call for it, plus reconnect in `snapsrc` (F5).
L2/L3 stay engine workarounds, and L4 stays "pcm default".

**P4. Evaluation:** listening tests on the host, endurance run, write-up, and the decision.

**After the prototype** (unchanged, not scheduled): multiple streams (L1), upstream PRs
(L2, L3, a control crate, L4), finishing `snapsink` (L6, L7, Android build and CI), finishing
`snapsrc` (L8), infrastructure (CI, L9, packaging).
