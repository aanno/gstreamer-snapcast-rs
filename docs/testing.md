# Testing

Overview of all tests and what each one needs. The setup for each environment, and what
to do when something doesn't work, is in:

- [testing-host.md](testing-host.md): Fedora desktop (or any Linux host with PipeWire),
  the place for real listening tests;
- [testing-devcontainer.md](testing-devcontainer.md): the VS Code devcontainer: no
  speakers, ALSA on the `snd-aloop` card.

## Test levels

| Command | What it tests | Needs | Part of `make check` |
|---|---|---|---|
| `make test` | Unit and integration tests: properties, `snapsink → snapsrc` loopback over TCP (signal, PTS continuity, scheduling), the control API (JSON-RPC over TCP, metadata from tags, `controls` and `Stream.Control` as a bus message, volume reaching snapsrc, Snapweb files over HTTP), lifecycle (F5: a sender pause becomes silence on a continuous timeline, a receiver resumes on schedule after its own pause, a restarted snapsrc is the same client) | GStreamer base/good plugins | yes |
| `make check` | `fmt` + `clippy -D warnings` + `test` + `doc -D warnings` | as above | — |
| `make test-interop` | `snapsink` → C++ `snapclient`, and C++ `snapserver` → `snapsrc`, for `pcm` and `flac` | C++ `snapclient`/`snapserver` (`SNAPCLIENT=`/`SNAPSERVER=`) | no (`#[ignore]`) |
| `make test-aloop` | Timing on a (virtual) sound card: offset between two receivers, and following a drifting card (F2); a sender pause is silent and both receivers are in sync afterwards (F5) | `snd-aloop`, `amixer`, `alsasrc`/`alsasink`, C++ `snapclient` for two of the four tests | no (`#[ignore]`) |
| `make test-endurance` | F3: snapsrc and the C++ snapclient for an hour (`ENDURANCE_MINUTES`) on the loopback card; one CSV row per minute (dropouts, ticks, offsets, memory) and trend checks at the end | as `make test-aloop` | no (`#[ignore]`) |
| `make test-mdns` | snapsrc without a host finds a snapsink via mDNS; a missing server gives a clear error within 5 s; snapsink announces its control ports | multicast on the network (works in the devcontainer and on the host) | no (`#[ignore]`) |
| `make listen-*` | By ear: sender, receivers, combinations with the C++ peers | speakers (host), installed plugins or `LOCAL=1` | no |
| `make doctor` | Not a test: reports tools, elements, plugins and sound devices | — | — |

`make check` must stay green without any devices or C++ binaries; everything that needs
them is `#[ignore]`d and has its own target.

## `make test-aloop` (F2)

`crates/gst-plugin-snapsrc/tests/aloop.rs`. The module docs have the details. In short:

```text
audiotestsrc wave=ticks ! snapsink (embedded server)
    ├─ receiver A ─▶ ALSA "snaptest_left"  ─┐
    └─ receiver B ─▶ ALSA "snaptest_right" ─┤ dmix ─▶ hw:Loopback,0,0
                                            └─ capture: alsasrc hw:Loopback,1,0
```

- A tick (10 ms of 1 kHz) is sent every second. Both receivers play into one dmix on
  loopback subdevice 0, A on the left channel and B on the right. One capture stream
  records both, so the **offset** B−A is the sample distance between the tick onsets
  (about 20 µs resolution; capture timestamps play no role).
- **Drift:** `PCM Rate Shift` makes the loopback card run about 300 ppm slow. A receiver
  that follows the Snapcast timeline must still produce one tick per second of
  `CLOCK_MONOTONIC`. The test measures the card's real rate by a linear fit and reports
  each receiver's **rate error** in ppm.
- **Absolute reference:** every test also measures each receiver against the **server
  timeline**: when each tick is due (its server timestamp + `buffer-ms`) versus when it
  leaves the card, both in `CLOCK_MONOTONIC`. The sender `tee`s a copy of what it sends,
  so the test knows each tick's PTS. The capture is mapped to monotonic time via the
  fitted card rate and the earliest buffer arrival. That is good to about 2–3 ms
  ("capture jitter" in the output). Positive = late. Two tests assert it, one receiver
  each: `aloop_snapsrc_plays_on_the_server_timeline` and
  `aloop_cpp_snapclient_plays_on_the_server_timeline` (tolerance `ALOOP_TOLERANCE_ABS_US`,
  default 5000).
- **Lost frames:** `aloop_snapsrc_loses_no_frames_behind_a_high_latency_sink` sends a
  continuous sine in 96 ms buffers (like a decoded FLAC file) to one snapsrc whose
  pipeline latency (150 ms) is larger than its `alsasink` buffer (40 ms), like `pulsesink`,
  and checks the recording for silent gaps. Before the 2026-11-19 fix it found 104 gaps of
  96 ms in 20 s.
- **Sender pause (F5):** `aloop_sender_pause_is_silent_and_resumes_in_sync` pauses the
  sender for 4 s, with snapsrc and the C++ snapclient as receivers. Both must play digital
  silence meanwhile (peak 0), snapsrc's ticks keep 1 s spacing from 1.5 s after the resume,
  and the pair is within `ALOOP_TOLERANCE_US` before the pause and from 4 s after the
  resume. Between those, `ALOOP_TOLERANCE_SETTLE_US` (5000) applies: the C++ snapclient
  is +2…+3.5 ms off for about 3 s after such a gap (measured 2026-11-22). The live test
  source re-anchors its timestamps after a pause (a hole or overlap of a few ms), which is
  why the first ticks after the resume are not checked.
- The tests run one at a time (`--test-threads=1`) because they share subdevice 0. Each takes about 25 s.
- Settings: `ALOOP_CARD` (default `Loopback`), `ALOOP_SECONDS` (20), `ALOOP_TOLERANCE_US`
  (1000), `ALOOP_TOLERANCE_DRIFT_US` (2500, under emulated drift), `ALOOP_TOLERANCE_SETTLE_US` (5000, see sender pause), `ALOOP_TOLERANCE_PPM` (100), `ALOOP_TOLERANCE_ABS_US` (5000), `SNAPCLIENT`.

## `make test-endurance` (F3)

`aloop_endurance` in the same file, the same setup (snapsrc left, C++ snapclient right),
but for `ENDURANCE_MINUTES` (default 60), analysed in windows of `ENDURANCE_WINDOW_SECS`
(default 60) so that memory stays small. The signal is a tick per second over a quiet
440 Hz sine (`ENDURANCE_SINE_VOLUME`, default 0.05), so a dropout anywhere is a silent gap.

Per window it prints one line and writes one CSV row (`ENDURANCE_CSV`, default
`target/endurance-<unix time>.csv`): ticks found, silent gaps and their length, offset
C++ − snapsrc (mean, min, max), each receiver against the server timeline, RSS of the test
process (sender + snapsrc) and of the C++ client, snapsrc's pipeline latency, the card's
rate. At the end: trends per hour (memory, snapsrc against the server), and checks: no
dropouts, no missing ticks, per window a mean offset within `ALOOP_TOLERANCE_US` and every
tick within `ENDURANCE_TOLERANCE_US` (2500, see below), both receivers within
`ALOOP_TOLERANCE_ABS_US` of the server, snapsrc's offset wandering at most
`ENDURANCE_WANDER_US` (1000) from the second to the last window, and the test process
growing at most `ENDURANCE_RSS_GROWTH_MB` (16) after the first window (warm-up).

The offsets against the server need each window's capture mapped to clock time (see
"Absolute reference"). If the test process stalls, that mapping fails for the window: both
receivers then appear hundreds of ms off while their offset to each other stays tiny
(seen once in the first hour-long run, 2026-11-22). Windows whose capture jitter (CSV
column `capture_jitter_ms`, normally about 3 ms) exceeds `ENDURANCE_MAX_JITTER_MS` (20)
are skipped in the server checks and trends and listed in the summary; more than 10 % of
them fail the run as unreliable.

**Known wander (2026-11-22):** in about one run in four or five, a snapsrc receiver wanders
by up to ±1.5…3 ms against the server timeline within the run (the audio sink's
`resample` slaving; it happened as often before the gap filling of 2026-11-22, 2 of 10
runs of `aloop_two_snapsrc_are_in_sync`). The 20 s tests then fail their 1 ms bound;
rerun. The endurance test checks single ticks against 2500 µs for this reason.

Each test prints one line, e.g.:

```text
aloop: rate shift 100030, card measured -303 ppm; offset B-A over 19 ticks: mean +125 µs,
range +125..+125 µs; rate error A +303.6 ppm, B +303.6 ppm
```

A constant offset (min = max) means both receivers run at the same rate. A growing range
means one of them drifts. A rate error close to the card's drift (with the opposite sign)
means that receiver plays at the card's rate instead of the server's.

### Results (2026-11-19, devcontainer, kernel 7.3, C++ snapclient 0.31)

| Test | Result |
|---|---|
| two `snapsrc`, no drift | **passes**, but the offset varies between runs (0, −21, +104, +646, +854 µs; one run above 1 ms) and stays constant within a run |
| `snapsrc` + C++ `snapclient`, no drift | **fails**: constant −60.6 ms (snapclient plays earlier) |
| two `snapsrc`, card −300 ppm | **fails**: both have a rate error of +300 ppm, i.e. they follow the card, not the server |
| `snapsrc` + C++ `snapclient`, card −300 ppm | **fails**: snapclient −1.9 ppm (follows the server), snapsrc +296 ppm, offset −62…−67 ms and growing |
| snapsrc behind a high-latency sink (added later the same day) | **passes** since the fix: 0 gaps (before: 104 gaps of 96 ms) |
| snapsrc against the server timeline (added later the same day) | failed with +62 ms (pipeline latency 65 ms); **passes since fix 2** (L13): −3…+1 ms |
| C++ snapclient against the server timeline | **passes**: +0.9…+2.1 ms |

**On the host** (Fedora 45, GStreamer 1.28.9, C++ snapclient 0.35.0, same day) the results are
the same: snapsrc+snapsrc passes (offset 0 / −625 µs), snapsrc vs snapclient −61.8…−63.7 ms
(constant), snapsrc follows the drifting card (+300 ppm), and snapclient 0.35 follows the
server (+5…+18 ppm, less exact than 0.31's −1.9 ppm). In `make listen-all` on the host
you hear the ticks, as a **mixture of single and double clicks**.

The absolute reference settled the −62 ms between snapsrc and snapclient: snapsrc played
late by its pipeline latency (fixed, L13).

**Since the L12 fix** (later on 2026-11-19: snapsrc sets `slave-method=resample` on the
sink, and no longer provides its clock), in the devcontainer:

| Test | Result |
|---|---|
| snapsrc / C++ snapclient against the server timeline | **pass**: +0.6…+1.1 ms / +0.9 ms |
| two snapsrc, snapsrc + C++ snapclient, no drift | **pass**: 0.03 ms / 0.25 ms apart |
| two snapsrc, card −300 ppm | **passes** (4/4 runs): rate error −17…+4 ppm (was +300), mostly < 0.1 ms apart |
| snapsrc + C++ snapclient, card −300 ppm | **fails** with +0.9…+1.4 ms: the **C++ snapclient** is +1.7…+2.0 ms off the server under drift, while snapsrc is at +0.6…+0.9 ms |
| no gaps behind a high-latency sink | **passes** |
| FLAC against the server timeline (L4) | before the fix +12 ms (C++ +6…+22 ms); **passes since the fix**: snapsrc +0.5 ms, C++ +0.9 ms |

In some runs snapsrc wanders by ±1.5 ms within a run, probably while the sink's
`resample` slaving converges.

`ALOOP_SINK_PROPS` adds properties to the snapsrc receivers' `alsasink` for experiments,
e.g. `slave-method=skew drift-tolerance=2000`. With `adjust-sink` on, snapsrc only changes
`skew`.

These are findings about the plugins, not test problems. See `docs/claude/memory-old-2026-11-22.md`
(State and Plan, P3) for their analysis.

## Gotchas (both environments)

| Symptom | Cause | Solution |
|---|---|---|
| `make listen-*` never ends | Intended: the sources are live | Ctrl+C. It stops all parts. If it doesn't, that's a bug: please report it |
| `listen.sh: 'snapclient' exited (status 1), stopping the others` | One part died (usually no audio output) and `listen.sh` stops the rest instead of running on half-broken | See the line before it in the log; usually an audio-output problem (environment docs) |
| `listen.sh: warning: no PipeWire/PulseAudio server, and ALSA 'default' cannot be opened` | Nowhere to play | Host: is PipeWire running? Devcontainer: `~/.asoundrc` (see there). Or set `SINK=` and `SOUNDCARD=` |
| `test-aloop`: `no 'Loopback' sound card` | `snd-aloop` not loaded, or `/dev/snd` not passed into the container | `sudo modprobe snd-aloop` on the host. Devcontainer: see there |
| `test-aloop`: `the rate shift did not take effect` | `PCM Rate Shift` must be set on **both** ends of the cable (device 0 and 1). The test does that, so this means a different snd-aloop version or card | Check with `amixer -c Loopback controls \| grep 'Rate Shift'` |
| `test-aloop`: ALSA `Device or resource busy` | Something else holds loopback subdevice 0 (another test run, PipeWire on the host, a `listen.sh` left over) | `fuser -v /dev/snd/*`; on the host see testing-host.md |
| A plugin change has no effect | An installed copy is loaded instead of `target/` (or the other way round) | `make doctor` shows which files GStreamer loads; `GST_PLUGIN_PATH` takes precedence |
| A test that captures GStreamer log lines (`gst::log::add_log_function`) suddenly sees nothing, but only when run together with other tests | gstreamer-rs `remove_log_function` removes by the boxed closure's pointer, and **all capture-less closures share one dangling pointer**: removing one removes every capture-less log function | Don't remove log functions in tests (register them once), or let each closure capture something (e.g. an `Arc`). See `gst-snapcast-common` tests |
| `snapsink … snapsrc …` in **one** gst-launch: no audio, and Ctrl+C with `-e` hangs | L14: the pipeline waits for every sink to preroll, but the receiver's sink only gets data once snapsink plays | `async=false` on one sink, e.g. `snapsink async=false`; or two processes. See `docs/pipelines.md` 5.4 |
| A test script's `timeout -s INT … gst-launch-1.0` also interrupts the calling shell (exit 130, output lost) | `timeout` signals its process group, which a non-interactive shell may share with it; background jobs also ignore SIGINT | Run it as `setsid timeout -k 5 -s INT <secs> gst-launch-1.0 …` with output to a file; `-k` kills it if Ctrl+C handling hangs |
| `GST_DEBUG=snapcast*:5` shows no snapcast-rs lines | The logging bridge is off (`GST_SNAPCAST_TRACING=0`), or a statically linking application installed its own `tracing` subscriber first | `GST_DEBUG=snapcast-engine:4` shows which (the bridge logs it at INFO); see `docs/architecture.md` |
