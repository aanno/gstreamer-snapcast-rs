#!/usr/bin/env bash
# Reports which tools, GStreamer elements and audio devices this machine has for
# building, installing and testing the plugins. Works on the Fedora host and in the
# devcontainer. It only reads; it never changes anything.
#
# Usage: scripts/doctor.sh [PLUGIN_DIR]
set -u

# Colors only on a terminal, so that `make doctor | tee file` gives plain text (NO_COLOR=1 to disable).
if [ -t 1 ] && [ -z "${NO_COLOR:-}" ]; then
    GREEN=$'\033[32m' YELLOW=$'\033[33m' RED=$'\033[31m' RESET=$'\033[0m'
else
    GREEN='' YELLOW='' RED='' RESET=''
fi
ok()   { printf '  %sok%s    %s\n' "$GREEN" "$RESET" "$*"; }
warn() { printf '  %swarn%s  %s\n' "$YELLOW" "$RESET" "$*"; }
miss() { printf '  %smiss%s  %s\n' "$RED" "$RESET" "$*"; }
section() { printf '\n%s\n' "$*"; }

plugin_dir=${1:-}

section "Build tools"
if command -v cargo >/dev/null; then ok "$(cargo --version)"; else miss "cargo (rustup or 'dnf install rust cargo')"; fi
# make check needs both; Fedora's cargo package ships neither.
if cargo fmt --version >/dev/null 2>&1; then ok "$(cargo fmt --version)"; else miss "rustfmt for make check (rustup component add rustfmt, or 'dnf install rustfmt')"; fi
if cargo clippy --version >/dev/null 2>&1; then ok "$(cargo clippy --version)"; else miss "clippy for make check (rustup component add clippy, or 'dnf install clippy')"; fi
if command -v pkg-config >/dev/null && pkg-config --exists gstreamer-1.0 gstreamer-audio-1.0; then
    ok "GStreamer dev files $(pkg-config --modversion gstreamer-1.0) (plugins dir: $(pkg-config --variable=pluginsdir gstreamer-1.0))"
else
    miss "GStreamer dev files (Fedora: gstreamer1-devel gstreamer1-plugins-base-devel; Debian: libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev)"
fi

section "GStreamer elements"
if ! command -v gst-inspect-1.0 >/dev/null; then
    miss "gst-inspect-1.0 (Fedora: gstreamer1; Debian: gstreamer1.0-tools)"
else
    for e in audiotestsrc audioconvert audioresample autoaudiosink alsasrc alsasink pipewiresrc pipewiresink pulsesink; do
        if gst-inspect-1.0 --exists "$e" 2>/dev/null; then ok "$e"; else warn "$e not available"; fi
    done
    for e in snapsink snapsrc; do
        file=$(gst-inspect-1.0 "$e" 2>/dev/null | sed -n 's/^ *Filename *//p' | head -1)
        if [ -n "$file" ]; then ok "$e from $file"; else warn "$e not found (make install, or set GST_PLUGIN_PATH=target/debug)"; fi
    done
    if [ -n "${GST_PLUGIN_PATH:-}" ]; then
        warn "GST_PLUGIN_PATH=$GST_PLUGIN_PATH (takes precedence over installed plugins)"
    fi
fi

if [ -n "$plugin_dir" ]; then
    section "Installed plugins in $plugin_dir"
    for f in libgstsnapsink.so libgstsnapsrc.so; do
        if [ -f "$plugin_dir/$f" ]; then ok "$f"; else warn "$f not installed"; fi
    done
fi

section "Shared server daemon (snapsink mode auto/shared)"
daemon="${GST_SNAPCAST_DAEMON:-gst-snapcast-daemon}"
if command -v "$daemon" >/dev/null; then
    ok "$("$daemon" --version 2>&1 | head -1) ($(command -v "$daemon"))"
else
    warn "$daemon not found: snapsink falls back to embedded (make install puts it into ~/.local/bin; is that on PATH?)"
fi
if [ -z "${XDG_RUNTIME_DIR:-}" ]; then
    warn "XDG_RUNTIME_DIR not set: no shared server"
elif [ -S "$XDG_RUNTIME_DIR/gst-snapcast/daemon.sock" ]; then
    ok "a daemon is running ($XDG_RUNTIME_DIR/gst-snapcast/daemon.sock; log: daemon.log next to it)"
else
    ok "no daemon running now (the first snapsink starts one)"
fi

section "Reference C++ Snapcast (interop and listening tests)"
for b in "${SNAPCLIENT:-snapclient}" "${SNAPSERVER:-snapserver}"; do
    if command -v "$b" >/dev/null; then ok "$b: $("$b" --version 2>&1 | head -1)"; else warn "$b not found"; fi
done

section "Sound devices"
if [ -r /proc/asound/cards ] && grep -q Loopback /proc/asound/cards; then
    ok "snd-aloop card: $(grep Loopback /proc/asound/cards | head -1 | sed 's/^ *//')"
else
    warn "no snd-aloop card (host: 'sudo modprobe snd-aloop'; persistent: echo snd-aloop | sudo tee /etc/modules-load.d/snd-aloop.conf)"
fi
if [ -d /dev/snd ]; then
    if [ -r /dev/snd/timer ] && [ -w /dev/snd/timer ]; then ok "/dev/snd accessible"; else warn "/dev/snd present but not accessible for $(id -un)"; fi
else
    warn "/dev/snd missing (in the devcontainer: needs --device=/dev/snd and a rebuild)"
fi
if command -v aplay >/dev/null; then ok "alsa-utils (aplay/arecord/amixer)"; else warn "alsa-utils not installed"; fi
if command -v pw-cli >/dev/null || command -v pactl >/dev/null; then ok "PipeWire/Pulse tools present"; else warn "no pw-cli/pactl (normal inside the devcontainer)"; fi
echo
