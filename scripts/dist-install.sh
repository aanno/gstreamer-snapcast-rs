#!/usr/bin/env bash
# Installs gstreamer-snapcast-rs from a release tarball (`make dist`): the GStreamer
# plugins snapsink and snapsrc, and the shared server daemon gst-snapcast-daemon.
# Packed into the tarball as install.sh; run it from the unpacked directory.
#
# Usage: ./install.sh              for the current user (no root):
#                                    plugins  ~/.local/share/gstreamer-1.0/plugins
#                                    daemon   ~/.local/bin
#        sudo ./install.sh --system  system-wide:
#                                    plugins  GStreamer's plugins dir (pkg-config, or
#                                             PLUGIN_DIR=...), daemon /usr/local/bin
#        ./install.sh --uninstall [--system]   remove what install.sh installed
#        ./install.sh -h | --help
#
# PLUGIN_DIR and BIN_DIR override the directories.
set -euo pipefail

usage() { sed -n '2,/^set -euo/p' "$0" | sed '$d; s/^# \{0,1\}//'; }

system=0
uninstall=0
for arg in "$@"; do
    case "$arg" in
        --system) system=1 ;;
        --uninstall) uninstall=1 ;;
        -h | --help) usage; exit 0 ;;
        *) echo "unknown argument: $arg" >&2; usage >&2; exit 2 ;;
    esac
done

here=$(cd "$(dirname "$0")" && pwd)
plugins=(libgstsnapsink.so libgstsnapsrc.so)
daemon=gst-snapcast-daemon

if [ "$system" = 1 ]; then
    if [ -z "${PLUGIN_DIR:-}" ]; then
        PLUGIN_DIR=$(pkg-config --variable=pluginsdir gstreamer-1.0 2>/dev/null || true)
    fi
    if [ -z "$PLUGIN_DIR" ]; then
        echo "cannot find GStreamer's plugins dir (no pkg-config / gstreamer-1.0.pc):" >&2
        echo "set PLUGIN_DIR, e.g. PLUGIN_DIR=/usr/lib64/gstreamer-1.0 (Fedora)" >&2
        exit 1
    fi
    BIN_DIR=${BIN_DIR:-/usr/local/bin}
else
    PLUGIN_DIR=${PLUGIN_DIR:-${XDG_DATA_HOME:-$HOME/.local/share}/gstreamer-1.0/plugins}
    BIN_DIR=${BIN_DIR:-$HOME/.local/bin}
fi

if [ "$uninstall" = 1 ]; then
    for f in "${plugins[@]}"; do rm -f "$PLUGIN_DIR/$f"; done
    rm -f "$BIN_DIR/$daemon"
    echo "Removed the plugins from $PLUGIN_DIR and $daemon from $BIN_DIR."
    exit 0
fi

install -d "$PLUGIN_DIR" "$BIN_DIR"
for f in "${plugins[@]}"; do install -m 0755 "$here/lib/gstreamer-1.0/$f" "$PLUGIN_DIR/"; done
install -m 0755 "$here/bin/$daemon" "$BIN_DIR/"
echo "Installed the plugins into $PLUGIN_DIR and $daemon into $BIN_DIR."

case ":$PATH:" in
    *":$BIN_DIR:"*) ;;
    *) echo "Note: $BIN_DIR is not on PATH, so snapsink cannot start $daemon;" \
        "add it to PATH or set GST_SNAPCAST_DAEMON=$BIN_DIR/$daemon." ;;
esac
if command -v gst-inspect-1.0 >/dev/null; then
    if gst-inspect-1.0 snapsink >/dev/null 2>&1 && gst-inspect-1.0 snapsrc >/dev/null 2>&1; then
        echo "GStreamer finds snapsink and snapsrc."
    else
        echo "GStreamer does not find snapsink/snapsrc yet: is $PLUGIN_DIR scanned" \
            "(GST_PLUGIN_PATH, a per-user dir only for the user who installed)?" \
            "Try: gst-inspect-1.0 snapsink"
    fi
fi
