#!/usr/bin/env bash
# Listening tests for snapsink/snapsrc, meant for a desktop with real speakers
# (Fedora host with PipeWire). Uses whatever snapsink/snapsrc GStreamer finds:
# installed plugins (`make install`), or GST_PLUGIN_PATH (the Makefile's LOCAL=1).
#
# Usage: scripts/listen.sh <command>
#   send         sender: SRC ! snapsink (embedded server on PORT)
#   recv         receiver: snapsrc ! SINK
#   cpp-client   receiver: C++ snapclient (SNAPCLIENT)
#   cpp-server   sender: C++ snapserver (SNAPSERVER) fed by SRC through a pipe
#   pair         recv + cpp-client together (needs a running sender)
#   all          send + recv + cpp-client on this machine
#
# Settings (environment variables):
#   SRC        ticks (default) | sine | desktop | mic | file:/path/to/audio
#              ticks: a short click every second. With several receivers on the same
#              speakers, in sync sounds like one click, out of sync like a double click.
#              desktop: whatever the desktop plays (PipeWire default sink monitor).
#   SINK       GStreamer sink for `recv` (default: autoaudiosink)
#   HOST       server for receivers (default 127.0.0.1)
#   PORT       stream port (default 1704)
#   STREAM     stream name (default: default)
#   CODEC      pcm (default) | flac
#   BUFFER_MS  end-to-end buffer (default 1000)
#   LATENCY_MS extra output latency for snapsrc/snapclient (default 0)
#   GST_DEBUG  e.g. snapsink:5,snapsrc:5
set -euo pipefail

SRC=${SRC:-ticks}
SINK=${SINK:-autoaudiosink}
HOST=${HOST:-127.0.0.1}
PORT=${PORT:-1704}
STREAM=${STREAM:-default}
CODEC=${CODEC:-pcm}
BUFFER_MS=${BUFFER_MS:-1000}
LATENCY_MS=${LATENCY_MS:-0}
SNAPCLIENT=${SNAPCLIENT:-snapclient}
SNAPSERVER=${SNAPSERVER:-snapserver}
CAPS="audio/x-raw,format=S16LE,rate=48000,channels=2"

source_pipeline() {
    case "$SRC" in
        ticks)   echo "audiotestsrc is-live=true wave=ticks volume=0.5" ;;
        sine)    echo "audiotestsrc is-live=true wave=sine freq=440 volume=0.2" ;;
        desktop) echo "pipewiresrc stream-properties=\"props,stream.capture.sink=true\" ! audioconvert ! audioresample" ;;
        mic)     echo "autoaudiosrc ! audioconvert ! audioresample" ;;
        file:*)  echo "uridecodebin uri=\"file://$(realpath "${SRC#file:}")\" ! audioconvert ! audioresample" ;;
        *)       echo "unknown SRC '$SRC'" >&2; exit 2 ;;
    esac
}

pids=()
cleanup() {
    for pid in "${pids[@]}"; do kill "$pid" 2>/dev/null || true; done
    wait 2>/dev/null || true
}
trap cleanup EXIT
trap 'exit 130' INT TERM

# Runs a command (or one of the functions below, which all `exec`) in the background,
# prefixing its output with a label. $! is the command itself, so cleanup can kill it.
bg() {
    local label=$1; shift
    ( "$@" ) > >(sed -u "s/^/[$label] /") 2>&1 &
    pids+=($!)
}

send() {
    # shellcheck disable=SC2046 # word splitting of the pipeline description is intended
    eval exec gst-launch-1.0 -e $(source_pipeline) ! "$CAPS" \
        ! snapsink stream="$STREAM" port="$PORT" codec="$CODEC" buffer-ms="$BUFFER_MS"
}

recv() {
    exec gst-launch-1.0 -e snapsrc host="$HOST" port="$PORT" client-name="gst-snapsrc" \
        latency-ms="$LATENCY_MS" ! audioconvert ! audioresample ! $SINK
}

cpp_client() {
    exec "$SNAPCLIENT" -h "$HOST" -p "$PORT" --latency "$LATENCY_MS" --hostID "listen-cpp-client"
}

cpp_server() {
    local dir fifo
    dir=$(mktemp -d)
    fifo="$dir/fifo"
    bg snapserver "$SNAPSERVER" --config "$dir/none.conf" --server.datadir="$dir" \
        --stream.port="$PORT" --stream.codec="$CODEC" --stream.buffer="$BUFFER_MS" \
        --stream.source="pipe://$fifo?name=$STREAM&sampleformat=48000:16:2" --logging.sink=stdout
    for _ in $(seq 100); do [ -p "$fifo" ] && break; sleep 0.05; done
    # shellcheck disable=SC2046
    bg feeder eval exec gst-launch-1.0 -e $(source_pipeline) ! "$CAPS" ! filesink location="$fifo" sync=false
    wait
    rm -rf "$dir"
}

if [ "$SRC" = desktop ] && [ "${1:-}" = all ]; then
    echo "SRC=desktop with 'all' is a feedback loop (the receivers play into the captured" >&2
    echo "desktop output). Run the receivers on another machine or another output (SINK=...)." >&2
    exit 2
fi

case "${1:-}" in
    send)       send ;;
    recv)       recv ;;
    cpp-client) cpp_client ;;
    cpp-server) cpp_server ;;
    pair)       bg snapsrc recv; bg snapclient cpp_client; wait ;;
    all)        bg snapsink send; sleep 1; bg snapsrc recv; bg snapclient cpp_client; wait ;;
    *)          sed -n '2,/^set -euo/p' "$0" | sed '$d; s/^# \{0,1\}//'; exit 2 ;;
esac
