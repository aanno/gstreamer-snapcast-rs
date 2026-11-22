# Evaluation of the feasibility prototype (P4)

As of 2026-11-22. This is the write-up for the go/no-go on the prototype: can GStreamer
pipelines send to and receive from Snapcast, in sync with the C++ Snapcast, using the
embeddable engines of `snapcast-rs`? The decision itself is the project owner's; the last
section gives a recommendation.

The criteria F1–F5 were set at the start (2026-11-10, `docs/claude/memory.md`, Plan). How
the measurements work and how to repeat them: `docs/testing.md`.

## Summary

| Criterion | Result |
|---|---|
| F1 listening: desktop sender, `snapsrc` and C++ `snapclient` in sync, no echo, dropouts or drift | **Mostly met.** Music through `snapsrc` on the host is clean; `make listen-pair` (snapsrc + C++ client) sounded right to the user. Only short sessions so far (a ~30 s track) |
| F2 measured: two receivers within ~1 ms on the loopback card, also under drift | **Met, with two caveats:** under emulated drift only to 2.5 ms, because the C++ reference itself is ~2 ms off; and the occasional snapsrc wander (below) |
| F3 endurance: hours without growing latency or memory, no glitches | **Met** for one hour: all ticks, no gaps, no drift, no memory growth |
| F4 control (optional): a standard controller lists the clients and changes take effect | **Met** with Snapweb, served by `snapsink` itself |
| F5 lifecycle: pause/resume, stop/restart, server restart | **Met** |

## Setup

- **Sender:** `snapsink` (`GstBaseSink`) embeds a `snapcast-rs` server, one per port, shared by
  all sinks in the process. PCM or FLAC on the wire, so the C++ client can play it.
- **Receivers:** `snapsrc` (live `GstPushSrc`) embeds a `snapcast-rs` client and outputs PCM
  timestamped from the server's time. The C++ `snapclient` (Debian 0.31) is the reference.
- **Measurements** run on the `snd-aloop` card in the devcontainer, with ALSA directly and no
  sound server. Both receivers play into the same card (snapsrc left channel, the other
  receiver right), and the test records it. The sender plays a tick per second, so the
  distance between the tick onsets in the recording is the offset between the receivers,
  to about 20 µs. Each receiver is also measured against the server timeline (when a tick is
  due versus when it leaves the card), to about 2–3 ms.
- **Listening** runs on the Fedora host through PipeWire (`make listen-*`).

## F1: listening

Music from the host desktop through `snapsink` → `snapsrc` plays cleanly since the
frame-loss fix of 2026-11-19 (a receiver whose pipeline latency is larger than its sink's
buffer lost frames). `make listen-pair`, snapsrc and the C++ client side by side, has no
audible echo since the latency fix L13 (snapsrc used to play late by its pipeline latency,
62 ms). The user checked it on 2026-11-22.

**Open:** a longer session with real desktop music (an album rather than a track).

## F2: two receivers in sync

`make test-aloop`, 20 s per test:

| Pair | Offset |
|---|---|
| snapsrc + snapsrc | usually < 0.1 ms |
| snapsrc + C++ snapclient | within 1 ms |
| either, under −300 ppm card drift | within 2.5 ms; snapsrc is +0.6…+0.9 ms off the server, the C++ client +1.7…+2.0 ms |

Against the server timeline, both receivers play about 1 ms late or less (PCM and FLAC). On a
card that runs 300 ppm slow, snapsrc follows the server with a rate error of −17…+4 ppm.
It sets `slave-method=resample` on its audio sink, so the sink resamples to the pipeline
clock instead of following the card.

**Caveat: snapsrc wander.** In about one run in four or five, snapsrc wanders by ±1.5…3 ms
against the server within a run; the C++ client does not. The aligner in snapsrc is not
the cause: it makes no corrections in those runs. The cause is the audio sink's `resample`
slaving, whose rate estimate is sometimes restless. It happened just as often before the
latest snapsrc changes. It makes the 1 ms tests flaky (rerun), and it is the main open
timing item. Candidates for a measured experiment are listed under "Open items".

**Caveat: the setup.** Both receivers share one card, so the tests emulate "the card drifts
against the server", not two cards drifting against each other.

## F3: endurance

`make test-endurance`: an hour of snapsink → snapsrc + C++ client on the aloop card, ticks
over a quiet sine (so a dropout anywhere is a silent gap), analysed per minute with a CSV row
each. Two runs on 2026-11-22:

| | Run 1 | Run 2 |
|---|---|---|
| Ticks, gaps | all, none | 3599/3599 in both receivers, none |
| C++ − snapsrc, per minute | +8…+68 µs | +776…+831 µs |
| snapsrc against the server | +0.3…+0.4 ms | +0.06…+0.14 ms; trend +0.008 ms per hour |
| C++ client against the server | +0.3…+0.4 ms | +0.86…+0.95 ms |
| Memory (sender + snapsrc process; C++ client) | 42–48 MiB flat; 9 MiB | 41–47 MiB, −3.4 MiB per hour; 9 MiB |
| Result | failed in minute 17 on a measurement artifact | **passed** |

- **The run 1 failure was in the test, not the receivers.** The test process stalled, so its
  recording could not be mapped to clock time for that minute. Both receivers then appeared
  half a second off the server, while their offset to each other stayed at +19 µs. The test
  now measures how reliable that mapping is per minute, and leaves unreliable minutes out of
  the server checks.
- **The offset between the receivers differs from run to run** (about 0 and +0.8 ms), but
  stays constant within a run. It is the C++ client that moved against the server, so this is
  an offset set at start, not drift.

## F4: control

`snapsink` serves the Snapcast JSON-RPC API (TCP and HTTP/WebSocket) and Snapweb, through
the new crate `snapcast-control` in the `snapcast-rs` fork. Checked by the user in a browser
(2026-11-21/22): Snapweb lists snapsrc and the C++ client with their details, and the
volume and name changes took effect. A server volume change reaching snapsrc is also
covered by a test (`tests/control.rs`); latency and group changes were not checked one by
one. GStreamer tags show up as stream metadata, and
Snapweb's play/pause/next buttons reach the application as a bus message
(`snapcast-stream-control`) when the `controls` property allows them.

Not covered: Snapweb's in-browser player (L11: `snapcast-rs` cannot serve audio over
WebSocket), authentication, and adding or removing streams at runtime (L1).

## F5: lifecycle

Covered by `tests/lifecycle.rs` and `tests/reconnect.rs` (in `make check`) and by an aloop
test with the C++ client:

- **Sender pause:** the receivers play silence. snapsrc outputs silence buffers flagged GAP
  when nothing arrives, on a continuous timeline, and it is back in sync with the server and
  the C++ client after the resume. The C++ client is +2…+3.5 ms off for about 3 s after such
  a pause; snapsrc is not.
- **Receiver pause:** snapsrc resumes on schedule.
- **Receiver restart:** a restarted snapsrc is the same client to the server (same id), so
  its volume and group stay.
- **Server restart:** snapsrc reconnects.

## Open items

| Item | Weight | Where it would be fixed |
|---|---|---|
| snapsrc wander (±1.5…3 ms in some runs) | the main timing item; within F2 most of the time | a measured experiment: real-time priority for the sink thread, smoother slaving (sink clock `window-size`/`timeout`), the card clock as pipeline clock with snapsrc's own aligner, PipeWire's quantum on the host |
| Run-to-run offset between snapsrc and the C++ client (0…+0.8 ms) | inside the 1 ms bound | look into it together with the wander |
| One stream per server (L1) | blocks several senders sharing one server, and the shared-server daemon | runtime `add_stream` in `snapcast-rs` |
| No audio over WebSocket (L11) | only Snapweb's in-browser player | `snapcast-rs`, a separate change |
| Sender input only S16LE/S32LE (L6) | `audioconvert` upstream works | `snapsink` |
| `snapsink` and `snapsrc` in one pipeline need `async=false` (L14) | a rare setup; documented | won't fix |
| Android build of the sender | a near-term target, never built | build and CI |
| F1 only in short sessions | small | a longer listening session |
| Fork commits not yet upstream | 12 commits on `aanno/snapcast-rs`, each an upstream PR candidate | PRs to `metaneutrons/snapcast-rs` |

## Recommendation

**Go.** All five criteria are met, F1 and F2 with the caveats above. The hard part, timing,
works: both elements keep to the Snapcast server timeline within about 1 ms, follow card
drift, survive pauses and restarts, and stay in sync with the C++ Snapcast for an hour. The
engines in `snapcast-rs` needed only small, upstreamable fixes; the GStreamer side follows the
gst-plugins-rs conventions.

Agreed as the next steps (2026-11-22): L1 in the fork, then the per-user shared server
daemon (design doc first), with "shared" as `snapsink`'s default mode. The wander
experiment can run alongside, measured as before.
