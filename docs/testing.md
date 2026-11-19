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
| `make test` | Unit and integration tests: properties, `snapsink → snapsrc` loopback over TCP (signal, PTS continuity, scheduling) | GStreamer base/good plugins | yes |
| `make check` | `fmt` + `clippy -D warnings` + `test` + `doc -D warnings` | as above | — |
| `make test-interop` | `snapsink` → C++ `snapclient`, and C++ `snapserver` → `snapsrc`, for `pcm` and `flac` | C++ `snapclient`/`snapserver` (`SNAPCLIENT=`/`SNAPSERVER=`) | no (`#[ignore]`) |
| `make test-aloop` | Timing on a (virtual) sound card: offset between two receivers, and following a drifting card (F2) | `snd-aloop`, `amixer`, `alsasrc`/`alsasink`, C++ `snapclient` for two of the four tests | no (`#[ignore]`) |
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
- The tests run one at a time (`--test-threads=1`) because they share subdevice 0. Each takes about 25 s.
- Settings: `ALOOP_CARD` (default `Loopback`), `ALOOP_SECONDS` (20), `ALOOP_TOLERANCE_US`
  (1000), `ALOOP_TOLERANCE_PPM` (100), `ALOOP_TOLERANCE_ABS_US` (5000), `SNAPCLIENT`.

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
late by its pipeline latency, while the C++ client was on time. Since the fix (L13), snapsrc
+ C++ snapclient **pass** (+0.4 ms apart). Still failing: the two drift tests (L12), and
sometimes two snapsrc (start-up alignment, up to ~4 ms).

These are findings about the plugins, not test problems. See `docs/claude/memory.md`
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
