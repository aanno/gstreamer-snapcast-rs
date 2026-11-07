# Project memory: state, decisions, limitations, plan

Working memory for agents and humans continuing this project. Keep it current:
update **State** and **Plan** when a step is finished, and add decisions with a date.

Last updated: 2026-11-07 (after commit `54f5a14 initial loopback`).

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
- **C++ interop (checked by hand, not yet a test):** Debian `snapclient` 0.31 plays
  `snapsink` streams with `codec=pcm` and `codec=flac` (440 Hz and RMS verified from
  `--player file` output). `snapserver` and `snapclient` 0.31 are in the devcontainer
  Dockerfile as reference peers. Upstream C++ is at 0.34.0; 0.31 is fine for now.
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

## Known limitations

| # | Limitation | Where | Current workaround | Fix |
|---|---|---|---|---|
| L1 | Streams are fixed once `SnapServer::serve()` runs | snapcast-rs server | One stream per port; a second stream name fails with `StreamsFixed` | Upstream: runtime `add_stream`/`remove_stream` |
| L2 | The client's internal player `Stream` is never drained when you don't use its cpal player, so it grows without bound | snapcast-rs client | `ClientHandle::recv` clears it after each frame | Upstream: make the player stream optional |
| L3 | `TimeSyncComplete` arrives only after 50 quick syncs (5 s) | snapcast-rs client | Schedule frames after `MIN_SYNC_TIME` (500 ms) | Upstream: a "sync usable" event, or a configurable count |
| L4 | **Suspected, not verified:** the FLAC encoder buffers samples, but each output chunk carries the timestamp of the latest input, which could make playback up to ~24 ms late, with jitter | snapcast-rs server `encoder/flac.rs` | Default codec is `pcm` | Write a test first; upstream fix if confirmed |
| L5 | Time-sync updates shift PTS slightly; no smoothing | `snapsrc` | Audio sinks tolerate this (alignment threshold) | Plan step 1 |
| L6 | Sink input is only S16LE/S32LE; no S24_32LE | `snapsink` | `audioconvert` upstream | Plan step 3 |
| L7 | `sync=false` with a non-live source floods clients (nothing paces it) | `snapsink` | Keep `sync=true` | Document, or pace in the sink |
| L8 | No reconnect handling, server volume/mute, mDNS or client id in `snapsrc` | `snapsrc` | — | Plan step 4 |
| L9 | snapcast-rs `tracing` output is not visible in GStreamer logs | engine | — | Bridge `tracing` to the GStreamer debug log |
| L10 | No JSON-RPC/control in the library (binary-only) | snapcast-rs | — | Control-plane discussion |

## Plan

Ordered by the decisions above. Each step ends with `make check` green and this file updated.

1. **Timing robustness** (current priority)
   - Turn the manual C++ interop check into an `#[ignore]`d (or env-gated) test that uses
     the devcontainer's `snapclient`/`snapserver`, in both directions where possible
     (C++ snapserver → `snapsrc`).
   - Test with a real audio sink (`pipewiresink`/`pulsesink`) and check that it slaves to
     the `snapsrc` clock.
   - Measure a two-machine setup and long-running drift (sender vs receiver clocks, hours).
   - Decide on smoothing for `diff_to_server` changes (L5); verify/fix FLAC timestamps (L4).
   - Fix L3 upstream (first snapcast-rs PR candidate).
2. **Multiple streams per server**
   - Upstream runtime `add_stream`/`remove_stream` (L1); then allow several `snapsink`s
     (with different streams) on one port.
   - Clean up L2 upstream.
3. **Finish `snapsink`**
   - S24_32LE input (L6), EOS → stream idle status, pause/flush/seek with non-live sources (L7).
   - **Android build of the sender path:** cross-compile `snapcast-engine` (server) and
     `gst-plugin-snapsink` for Android (cargo-ndk, GStreamer Android SDK) and set up a CI
     job for it.
4. **Finish `snapsrc`** (desktop only): reconnect handling, server volume/mute, `client-id`
   property, optional mDNS discovery (L8).
5. **Control plane:** discuss first (L10). Options: control API in `snapcast-engine`,
   element signals/actions, or a separate controller application (the "DMC").
6. **Infrastructure:** GitHub Actions (fmt, clippy, tests, docs, Android build), the
   tracing→GStreamer log bridge (L9), packaging and install paths.
