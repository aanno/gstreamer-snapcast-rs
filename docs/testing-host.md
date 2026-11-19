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
| `snapserver` 0.35: `Exception: cannot open file: '…/none.conf'` | 0.35 refuses a missing `--config` file; 0.31 used built-in defaults for it | Fixed 2026-11-19: the tests and `listen.sh cpp-server` pass an empty config file. If snapserver fails to start in `make test-interop`, the test prints its output |
| `make check`: `error: no such command: fmt` | Fedora's `cargo` package ships neither rustfmt nor clippy (`make doctor` reports both as `miss`) | `sudo dnf install rustfmt clippy`, or use rustup |
| A log written with `make … \| tee file` lacks the errors | cargo, the tests and gst-launch write to stderr | `make … 2>&1 \| tee file` (as in `scripts/test-host.sh`) |
| `SRC=desktop` records silence or the microphone | It relies on the PipeWire property `stream.capture.sink=true`, i.e. on capturing the default sink's monitor | Check in `pw-top`/`qpwgraph` what `pipewiresrc` is linked to. `SRC=desktop` together with `listen-all` is refused (feedback loop) |
| Echo with `SRC=desktop` | The desktop plays the source locally *and* the receivers play it again | Mute local playback, or play the receivers on another machine/output |
| `test-aloop` (or the devcontainer) gets `Device or resource busy` on `hw:Loopback,…` | PipeWire/WirePlumber treats the loopback card as a normal sound card and may hold it open (e.g. if it became the default output) | Don't pick "Loopback" as output. To hide it from PipeWire completely, see the snippet below (not yet verified on the user's host) |
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
