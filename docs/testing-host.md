# Testing on the host (Fedora desktop)

The host is where the listening tests happen: real speakers, PipeWire, and possibly
receivers on other machines. Overview of all tests: [testing.md](testing.md).

## Setup

You need a Rust toolchain (>= 1.94.1; with rustup, `rust-toolchain.toml` is picked up)
and the GStreamer development packages:

```sh
# Fedora
sudo dnf install pkg-config gstreamer1-devel gstreamer1-plugins-base-devel \
    gstreamer1-plugins-good pipewire-gstreamer alsa-utils
# Debian/Ubuntu
sudo apt install pkg-config libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev \
    gstreamer1.0-tools gstreamer1.0-plugins-base gstreamer1.0-plugins-good gstreamer1.0-alsa
git submodule update --init --recursive
make doctor    # reports anything that is still missing
```

The C++ `snapserver`/`snapclient` are optional. They're used by `make test-interop`,
`make test-aloop` and some listening tests. Set `SNAPCLIENT=`/`SNAPSERVER=` to use
self-compiled binaries.

For `make test-aloop` (and for the devcontainer's sound), load the loopback card:

```sh
sudo modprobe snd-aloop
echo snd-aloop | sudo tee /etc/modules-load.d/snd-aloop.conf    # load it at boot
```

### Install

```sh
make install            # release build into ~/.local/share/gstreamer-1.0/plugins (no root)
make uninstall
make release && sudo make install-system    # or system-wide (GStreamer's own plugins dir)
sudo make uninstall-system
```

`make doctor` shows which `snapsink`/`snapsrc` files GStreamer actually loads.

`scripts/test-host.sh` runs all of the following (install, doctor, check, interop, aloop,
listen-all) and writes `*-host.log` files into the repo folder (git-ignored).

## Listening tests

`make listen-*` runs [scripts/listen.sh](../scripts/listen.sh) with the installed plugins,
or with `LOCAL=1` for a fresh release build of this tree:

```sh
make listen-all                   # sender + snapsrc + C++ snapclient on this machine
make listen-send SRC=desktop      # send what the desktop plays (PipeWire); stop local playback first
make listen-recv HOST=192.168.1.10    # on another machine
make listen-cpp-client HOST=...   # same, with the C++ snapclient
make listen-pair HOST=...         # both receivers at once
make listen-cpp-server            # C++ snapserver as sender; then listen-recv
```

The default source is `SRC=ticks`, a click every second. With two receivers on the same
speakers, in sync sounds like a single click and out of sync like a double click.
Other sources: `sine`, `desktop`, `mic` and `file:/path/to/audio`. Further settings:
`PORT`, `STREAM`, `CODEC=pcm|flac`, `BUFFER_MS`, `LATENCY_MS`, `SINK` (GStreamer sink of
`snapsrc`), `SOUNDCARD` (ALSA device of the C++ `snapclient`). All targets run until
Ctrl+C. If one part dies, the script stops the others and says which one died.

### Which audio path does each test use?

Only `make test-aloop` uses ALSA directly, on purpose: it plays and records on the
`snd-aloop` card and bypasses PipeWire, so that it can measure exactly. Everything you
*hear* goes through PipeWire:

| Test | Audio path on the host |
|---|---|
| `make test`, `make check`, `make test-interop` | no sound device at all |
| `make test-aloop` | ALSA directly: `alsasink`/`alsasrc` on `hw:Loopback` |
| `make listen-*`, snapsrc | `autoaudiosink` → `pulsesink` → PipeWire (pipewire-pulse) → speakers |
| `make listen-*`, C++ snapclient | ALSA `default` → PipeWire (pipewire-alsa) → speakers |

### Switching between PipeWire and ALSA

`SINK` (GStreamer sink of `snapsrc`) and `SOUNDCARD` (ALSA device of the C++
`snapclient`) choose the path; they work with every `listen-*` target and `host-play.sh`:

| Path | snapsrc | C++ snapclient |
|---|---|---|
| PipeWire via its PulseAudio layer (default) | `SINK=autoaudiosink` (→ `pulsesink`) | — |
| PipeWire via its ALSA layer | `SINK=alsasink` | `SOUNDCARD=default` (default) |
| PipeWire natively | `SINK=pipewiresink` (hangs on stop, see gotchas) | — |
| **ALSA directly, without PipeWire** | `SINK="alsasink device=hw:1,0"` | `SOUNDCARD=hw:1,0` |

For ALSA directly, find the card with `aplay -l`. PipeWire must not hold it at the same
time (`fuser -v /dev/snd/*`): pause it for the test (`systemctl --user stop pipewire.socket
pipewire.service wireplumber.service`, then `start` again), or use a second sound card or
USB headset. `hw:` devices accept only formats the hardware supports; `plughw:1,0`
converts automatically. Example, both receivers directly on ALSA:

```sh
make listen-pair SINK="alsasink device=plughw:1,0" SOUNDCARD=plughw:1,0
```

Only one program can use a `hw:` device at a time, so for two receivers on one card use
`dmix` (like `.devcontainer/asoundrc`), or put them on different cards.

### `alsasink` examples

`alsasink` is GStreamer's ALSA output. Its `device` property takes any ALSA PCM name; it
defaults to `default`, which on Fedora is PipeWire's ALSA layer.

```sh
aplay -l                                            # cards and devices: "card 1: ... device 0"
aplay -L | grep -E '^(default|plughw|hw|pipewire)'  # PCM names alsasink accepts

# receiver through PipeWire's ALSA layer (works next to everything else that plays)
make listen-recv SINK=alsasink
make listen-recv SINK="alsasink device=pipewire"    # the same, explicitly

# receiver directly on card 1 device 0, past PipeWire (pause PipeWire first, see above)
make listen-recv SINK="alsasink device=plughw:1,0"  # with format/rate conversion
make listen-recv SINK="alsasink device=hw:1,0"      # no conversion: only what the hardware takes

# C++ snapclient on the same paths
make listen-cpp-client SOUNDCARD=default            # via PipeWire (the default)
make listen-cpp-client SOUNDCARD=plughw:1,0         # directly on ALSA

# test the device on its own, without Snapcast
gst-launch-1.0 audiotestsrc num-buffers=200 ! audioconvert ! alsasink device=plughw:1,0
```

`alsasink` properties that matter here:

| Property | Default | Effect |
|---|---|---|
| `device` | `default` | ALSA PCM name (`default`, `pipewire`, `hw:C,D`, `plughw:C,D`, a name from `~/.asoundrc`) |
| `buffer-time` | 200000 µs | size of the ring buffer; a direct `hw:` device may round it |
| `latency-time` | 10000 µs | period size, i.e. how often data is handed over |
| `slave-method` | `skew` | how it follows the pipeline clock: `skew` jumps after `drift-tolerance`, `resample` adjusts continuously. **snapsrc sets `resample`** on the first audio sink downstream (`adjust-sink=true`, default) unless you chose something other than `skew` (L12) |
| `drift-tolerance` | 40000 µs | how much drift `skew` accepts before it jumps |

Example with all of them, for experiments with L12. `adjust-sink` only changes `skew`,
so the explicit `skew` here stays unless you also disable it in `snapsrc`:

```sh
make listen-recv SINK="alsasink device=plughw:1,0 buffer-time=100000 latency-time=10000 slave-method=none"
gst-launch-1.0 snapsrc adjust-sink=false ! audioconvert ! alsasink slave-method=skew drift-tolerance=5000
```

`pulsesink` has the same four properties with the same defaults (checked with
`gst-inspect-1.0`, GStreamer 1.26), so the same experiments work there. `pipewiresink` is
different: it has only its own `slave-method` (default `none`) and none of the others.

### Music through snapsink → snapsrc

The quickest way to hear how well the plugins work with real music:

1. **Install the current build** (or put `LOCAL=1` in front of the commands below):
   ```sh
   make install
   ```
2. **Terminal 1, the receiver, first.** `snapsrc` plays through PipeWire to your speakers:
   ```sh
   make listen-recv
   ```
   It waits for the server and keeps retrying the connection, so it can start before the
   sender. **Start it first:** the sender plays the file in real time from second 0, whether
   a receiver listens or not, so a receiver started later misses the beginning (with the
   38 s test file, a large part of it).
3. **Terminal 2, the sender.** It plays a music file into `snapsink`. Without an argument,
   `host-play.sh` uses the CC0 jazz loop in [assets/audio](../assets/audio/README.md)
   (38 s, 44.1 kHz):
   ```sh
   scripts/host-play.sh                              # or: scripts/host-play.sh ~/Music/song.flac
   make listen-send SRC=file:$HOME/Music/song.flac   # the same, by hand
   ```
   FLAC, Ogg and MP3 work with the standard Fedora GStreamer packages. AAC/M4A needs
   extra plugins (e.g. `gstreamer1-plugin-libav`). About a second after the sender starts
   (`BUFFER_MS=1000`) you hear the music: the pure snapsink → snapsrc path.
4. **Listen for:** dropouts or crackles, changes of pitch or speed, and whether it stays
   clean over several minutes.
5. **Stop** with Ctrl+C in both terminals. The sender also stops by itself when the file
   ends; the receiver keeps waiting for more.

Variations:

- `CODEC=flac` in terminal 2 sends FLAC instead of PCM. The receiver detects the codec by
  itself.
- Another output path for snapsrc, e.g. ALSA: see the next section.
- `make listen-cpp-client` in terminal 1 (instead of `listen-recv`) plays the same stream
  through the reference C++ client: an A/B comparison of the sound.
- `make listen-pair` in terminal 1 plays both receivers at once. Because of the ~62 ms
  offset between `snapsrc` and the C++ client (L13 in `docs/claude/memory.md`), expect an
  **audible echo or flanging**. With `SRC=ticks`, the same shows as a mixture of single
  and double clicks.

Don't:

- Use `SRC=desktop` with a receiver **on the same machine**: `snapsrc` plays into the
  desktop output that the sender captures, which is a feedback loop. Use it only with
  the receiver on another machine or another output (`SINK=…`).
- Use music with `listen-all` and expect clean sound: it's `listen-pair` plus the sender,
  so it has the same echo.

By hand:

```sh
export GST_PLUGIN_PATH=$PWD/target/debug     # only to use the tree's debug build
# terminal 1: sender
gst-launch-1.0 audiotestsrc is-live=true ! audio/x-raw,format=S16LE,rate=48000,channels=2 \
    ! snapsink buffer-ms=500
# terminal 2: receiver (or any Snapcast client, e.g. `snapclient -h 127.0.0.1`)
gst-launch-1.0 snapsrc ! audioconvert ! audioresample ! autoaudiosink
```

## Gotchas

General ones are in [testing.md](testing.md#gotchas-both-environments).

| Symptom | Cause | Solution |
|---|---|---|
| A receiver on another machine cannot connect | firewalld blocks the stream port | `sudo firewall-cmd --add-port=1704/tcp` (add `--permanent` to keep it), or use another `PORT` |
| `snapclient` 0.35: `Options '--host' and '--port' are deprecated` | Newer versions want the server URI as last argument (`tcp://HOST:PORT`); 0.31 (devcontainer) only knows `-h`/`-p` | Harmless: 0.35 still accepts them, so `listen.sh` and the tests keep `-h`/`-p` for both versions |
| `snapserver` 0.35: `Exception: cannot open file: '…/none.conf'` | 0.35 refuses a missing `--config` file; 0.31 used built-in defaults for it | Fixed 2026-11-19: the tests and `listen.sh cpp-server` pass an empty config file (verified with 0.31 and 0.35). If snapserver fails to start in `make test-interop`, the test prints its output |
| `make check`: `error: no such command: fmt` | Fedora's `cargo` package ships neither rustfmt nor clippy (`make doctor` reports both as `miss`) | `sudo dnf install rustfmt clippy`, or use rustup |
| A log written with `make … \| tee file` lacks the errors | cargo, the tests and gst-launch write to stderr | `make … 2>&1 \| tee file` (as in `scripts/test-host.sh`) |
| `SRC=desktop` records silence or the microphone | It relies on the PipeWire property `stream.capture.sink=true`, i.e. on capturing the default sink's monitor | Check in `pw-top`/`qpwgraph` what `pipewiresrc` is linked to. `SRC=desktop` together with `listen-all` is refused (feedback loop) |
| Echo with `SRC=desktop` | The desktop plays the source locally *and* the receivers play it again | Mute local playback, or play the receivers on another machine/output |
| `test-aloop` (or the devcontainer) gets `Device or resource busy` on `hw:Loopback,…` | PipeWire/WirePlumber treats the loopback card as a normal sound card and may hold it open (e.g. if it became the default output) | Don't pick "Loopback" as output. To hide it from PipeWire completely, see the snippet below (not yet verified on the user's host) |
| Music (or ticks) through `snapsrc` → `pulsesink` "wobbles" with short pauses; `GST_DEBUG=snapsrc:5` shows `Dropping frame … too late` | Fixed 2026-11-19: snapsrc dropped frames as late without counting the pipeline latency (`pulsesink`: 130 ms). About every fourth 96 ms block was lost. Regression test: `aloop_snapsrc_loses_no_frames_behind_a_high_latency_sink` | `make install` again. For an older build: `SINK="queue max-size-time=3000000000 max-size-buffers=0 max-size-bytes=0 ! pulsesink"` |
| `pipewiresink` hangs when the pipeline stops (`Setting pipeline to NULL ...`) | Deadlock in GStreamer's PipeWire plugin (seen with PipeWire 1.4.2, also with `audiotestsrc`, so not ours) | Use `autoaudiosink` (→ `pulsesink` via pipewire-pulse) or `alsasink`. The default `SINK=autoaudiosink` is fine |
| Plugin changes don't show up | `GST_PLUGIN_PATH` points at a build that takes precedence over the installed copy, or vice versa | `make doctor`; `make install` after changes, or `LOCAL=1` |

Hiding the loopback card from WirePlumber (0.5 syntax), in
`~/.config/wireplumber/wireplumber.conf.d/51-hide-snd-aloop.conf`, then
`systemctl --user restart wireplumber`:

```text
monitor.alsa.rules = [
  {
    matches = [ { device.name = "~alsa_card.platform-snd_aloop.*" } ]
    actions = { update-props = { device.disabled = true } }
  }
]
```

Check the exact `device.name` with `wpctl status` and `wpctl inspect <id>` first.
