#!/usr/bin/env bash
# Listening tests for snapsink/snapsrc, meant for a desktop with real speakers
# (Fedora host with PipeWire). Uses whatever snapsink/snapsrc GStreamer finds:
# installed plugins (`make install`), or GST_PLUGIN_PATH (the Makefile's LOCAL=1).
#
# Before running anything, it prints each command it runs, ready to copy, with a short
# note per element: a starting point for your own pipelines (more: docs/pipelines.md).
#
# Usage: scripts/listen.sh <command>
#        scripts/listen.sh show <command>   only print the commands and notes, run nothing
#        scripts/listen.sh -h | --help      this help
#
# Commands:
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
#   SOUNDCARD  ALSA PCM for the C++ snapclient (default: default), e.g. hw:Loopback,0,1
#   HOST       server for receivers (default 127.0.0.1; empty, i.e. HOST=, finds one via mDNS)
#   PORT       stream port (default 1704)
#   STREAM     stream name (default: default)
#   CODEC      pcm (default) | flac
#   BUFFER_MS  end-to-end buffer (default 1000)
#   LATENCY_MS extra output latency for snapsrc/snapclient (default 0)
#   CONTROL    1: the sender also offers the control API: JSON-RPC on 1705 (Snapdroid,
#              Home Assistant) and Snapweb on http://<host>:1780/ (needs SNAPWEB_DIR)
#   SNAPWEB_DIR Snapweb's files (default ~/.local/share/snapweb; get them with
#              scripts/fetch-snapweb.sh)
#   QUIET      1: print the commands without the notes
#   GST_DEBUG  e.g. snapsink:5,snapsrc:5,snapcast*:4
set -euo pipefail

SRC=${SRC:-ticks}
SINK=${SINK:-autoaudiosink}
HOST=${HOST-127.0.0.1}   # set but empty = mDNS discovery
PORT=${PORT:-1704}
STREAM=${STREAM:-default}
CODEC=${CODEC:-pcm}
BUFFER_MS=${BUFFER_MS:-1000}
LATENCY_MS=${LATENCY_MS:-0}
CONTROL=${CONTROL:-0}
SNAPWEB_DIR=${SNAPWEB_DIR:-${XDG_DATA_HOME:-$HOME/.local/share}/snapweb}
SOUNDCARD=${SOUNDCARD:-default}
SNAPCLIENT=${SNAPCLIENT:-snapclient}
SNAPSERVER=${SNAPSERVER:-snapserver}
QUIET=${QUIET:-0}
CAPS="audio/x-raw,format=S16LE,rate=48000,channels=2"

usage() {
    sed -n '2,/^set -euo/p' "$0" | sed '$d; s/^# \{0,1\}//'
}

# ---------------------------------------------------------------------------
# Commands are built as a program plus parts (pipeline elements, or option groups),
# each with a note. The same text is printed for the user and executed via eval, so
# what you see is exactly what runs.

CMD_PROG=""     # e.g. "gst-launch-1.0 -e"
CMD_SEP=""      # between parts: " ! " for pipelines, " " for command-line options
CMD_PROG_NOTE=""
CMD_PARTS=()
CMD_NOTES=()

new_cmd() {
    CMD_PROG=$1 CMD_SEP=$2 CMD_PROG_NOTE=${3-}
    CMD_PARTS=() CMD_NOTES=()
}

part() {
    CMD_PARTS+=("$1")
    CMD_NOTES+=("${2-}")
}

# The command as one line of shell.
cmd_line() {
    local line="$CMD_PROG" i
    for i in "${!CMD_PARTS[@]}"; do
        if [ "$i" -eq 0 ]; then line+=" "; else line+="$CMD_SEP"; fi
        line+="${CMD_PARTS[$i]}"
    done
    printf '%s' "$line"
}

run_cmd() {
    eval "exec $(cmd_line)"
}

# Prints the command (one part per line, still valid shell) and the notes.
show_cmd() {
    local title=$1 i last prefix label note first rest
    echo "== $title"
    if [[ $CMD_PROG == gst-launch* ]] && [ -n "${GST_PLUGIN_PATH:-}" ]; then
        echo "   (plugins from GST_PLUGIN_PATH=$GST_PLUGIN_PATH: export it to run this yourself)"
    fi
    echo "$CMD_PROG \\"
    last=$((${#CMD_PARTS[@]} - 1))
    for i in "${!CMD_PARTS[@]}"; do
        prefix="    "
        if [ "$i" -gt 0 ] && [ "$CMD_SEP" = " ! " ]; then prefix="  ! "; fi
        if [ "$i" -lt "$last" ]; then
            echo "$prefix${CMD_PARTS[$i]} \\"
        else
            echo "$prefix${CMD_PARTS[$i]}"
        fi
    done
    if [ "$QUIET" != 1 ]; then
        echo
        [ -n "$CMD_PROG_NOTE" ] && note_line "${CMD_PROG%% *}" "$CMD_PROG_NOTE"
        for i in "${!CMD_PARTS[@]}"; do
            note=${CMD_NOTES[$i]}
            [ -z "$note" ] && continue
            label=${CMD_PARTS[$i]%% *}
            [[ $label == *,* ]] && label="${label%%,*},..."   # caps: audio/x-raw,...
            note_line "$label" "$note"
        done
    fi
    echo
}

# One note: a label column, then the text; further lines of the text are indented.
note_line() {
    local label=$1 text=$2 first=1 line
    if [ ${#label} -gt 22 ]; then label="${label:0:19}..."; fi
    while IFS= read -r line; do
        if [ $first = 1 ]; then
            printf '    %-22s  %s\n' "$label" "$line"
            first=0
        else
            printf '    %-22s  %s\n' "" "$line"
        fi
    done <<< "$text"
}

# ---------------------------------------------------------------------------
# The commands.

# Test sources have no tags; give them a title, so that controllers (Snapweb) show
# something. Files and web radio bring their own tags.
add_test_tags() {
    part "taginject tags=\"title=$1,artist=gstreamer-snapcast-rs\"" \
        "adds track tags; snapsink passes title, artist, album on to controllers (Snapweb)"
}

add_source() {
    case "$SRC" in
        ticks)
            part "audiotestsrc is-live=true wave=ticks volume=0.5" \
                "test source, a click every second; is-live: paced like a real device.
In sync, several receivers sound like one click, out of sync like a double click"
            add_test_tags Ticks ;;
        sine)
            part "audiotestsrc is-live=true wave=sine freq=440 volume=0.2" \
                "test source, a 440 Hz tone; is-live: paced like a real device"
            add_test_tags Sine ;;
        desktop)
            part 'pipewiresrc stream-properties="props,stream.capture.sink=true"' \
                "captures what the desktop plays (the monitor of PipeWire's default output)"
            part "audioconvert" "converts the sample format (e.g. F32 → S16LE below)"
            part "audioresample" "converts the sample rate (to the 48 kHz below)" ;;
        mic)
            part "autoaudiosrc" "the system's default input (microphone)"
            part "audioconvert" "converts the sample format"
            part "audioresample" "converts the sample rate" ;;
        file:*)
            part "uridecodebin uri=\"file://$(realpath "${SRC#file:}")\"" \
                "reads and decodes the file, any format GStreamer knows (also http:// URIs).
Not live: snapsink paces it to real time"
            part "audioconvert" "converts the decoder's sample format"
            part "audioresample" "converts the sample rate (to the 48 kHz below)" ;;
        *) echo "unknown SRC '$SRC'" >&2; exit 2 ;;
    esac
}

add_caps() {
    part "$CAPS" "caps filter: fixes the format between its neighbours to S16LE, 48 kHz,
stereo (snapsink takes S16LE or S32LE; the stream's format follows)"
}

build_send() {
    new_cmd "gst-launch-1.0 -e" " ! " \
        "-e: on Ctrl+C, end the stream first, so that everything stops cleanly"
    add_source
    add_caps
    local sink="snapsink stream=$STREAM port=$PORT codec=$CODEC buffer-ms=$BUFFER_MS"
    local note="embedded Snapcast server on port $PORT (shared by all snapsinks on that port),
feeding stream \"$STREAM\"; announced via mDNS. Any Snapcast client can connect.
codec: pcm or flac. buffer-ms: how far ahead of play-out the audio is sent"
    if [ "$CONTROL" = 1 ]; then
        sink+=" control-port=1705 http-port=1780"
        note+="
control-port: JSON-RPC control API (Snapdroid, Home Assistant, scripts).
http-port: the same over HTTP/WebSocket, plus Snapweb: open http://<this host>:1780/.
No authentication: anyone on the network can control the server"
        if [ -f "$SNAPWEB_DIR/index.html" ]; then
            sink+=" snapweb-dir=$SNAPWEB_DIR"
            note+="
snapweb-dir: Snapweb's files (scripts/fetch-snapweb.sh)"
        else
            note+="
(no Snapweb in $SNAPWEB_DIR: run scripts/fetch-snapweb.sh, or set SNAPWEB_DIR)"
        fi
    fi
    part "$sink" "$note"
}

build_recv() {
    local host_arg="host=$HOST" where="$HOST:$PORT"
    # gst-launch joins its arguments into one string: an empty value must be "" or the
    # next property becomes the host. Empty host = mDNS discovery.
    if [ -z "$HOST" ]; then host_arg="host='\"\"'"; where="a server found via mDNS"; fi
    new_cmd "gst-launch-1.0 -e" " ! " \
        "-e: on Ctrl+C, end the stream first, so that everything stops cleanly"
    part "snapsrc $host_arg port=$PORT client-name=gst-snapsrc latency-ms=$LATENCY_MS" \
        "Snapcast client for $where; outputs F32LE stamped so that it is heard
at the Snapcast play-out time. latency-ms: extra delay of your output device.
It sets slave-method=resample on the audio sink below (adjust-sink)"
    part "audioconvert" "converts F32LE to what the sink wants"
    part "audioresample" "converts the rate if the sink wants another one"
    if [ "$SINK" = autoaudiosink ]; then
        part "$SINK" "plays on the system's default output (PipeWire, PulseAudio or ALSA).
Choose another with SINK=..., e.g. SINK=\"alsasink device=plughw:1,0\""
    else
        part "$SINK" "your SINK"
    fi
}

build_cpp_client() {
    new_cmd "$SNAPCLIENT" " " "the C++ reference client"
    if [ -n "$HOST" ]; then
        part "-h $HOST -p $PORT" "the server (without -h/-p: found via mDNS, needs avahi-daemon)"
    fi
    part "--latency $LATENCY_MS" "extra output latency in ms, like snapsrc's latency-ms"
    part "--soundcard $SOUNDCARD" "the ALSA device to play on"
    part "--hostID listen-cpp-client" "its client id towards the server (default: MAC address)"
}

# $dir: a directory for snapserver's config, state and FIFO.
build_cpp_server() {
    new_cmd "$SNAPSERVER" " " "the C++ reference server; it creates the FIFO itself"
    part "--config $dir/snapserver.conf" "an empty file: built-in defaults (0.35 needs the file)"
    part "--server.datadir=$dir" "its state files"
    part "--stream.port=$PORT --stream.codec=$CODEC --stream.buffer=$BUFFER_MS" \
        "like snapsink's port, codec and buffer-ms"
    part "'--stream.source=pipe://$dir/fifo?name=$STREAM&sampleformat=48000:16:2'" \
        "reads raw S16LE PCM from a FIFO, as stream \"$STREAM\""
    part "--logging.sink=stdout"
}

build_feeder() {
    new_cmd "gst-launch-1.0 -e" " ! " \
        "-e: on Ctrl+C, end the stream first, so that everything stops cleanly"
    add_source
    add_caps
    part "filesink location=$dir/fifo sync=false" \
        "writes raw PCM into snapserver's FIFO. This setup (also Mopidy's usual one) is what
snapsink replaces: the FIFO carries no timestamps, snapserver stamps the audio when it reads it"
}

explain() {
    case "$1" in
        send)       build_send;       show_cmd "sender: SRC → snapsink" ;;
        recv)       build_recv;       show_cmd "receiver: snapsrc → SINK" ;;
        cpp-client) build_cpp_client; show_cmd "receiver: C++ snapclient" ;;
        cpp-server) echo "== preparation: a directory with an empty config file"
                    echo "mkdir -p $dir && : > $dir/snapserver.conf"
                    echo
                    build_cpp_server; show_cmd "sender, part 1: C++ snapserver reading a FIFO"
                    build_feeder;     show_cmd "sender, part 2: SRC → the FIFO" ;;
        pair)       explain recv; explain cpp-client; return ;;
        all)        explain send; explain recv; explain cpp-client; return ;;
    esac
}

explain_footer() {
    [ "$QUIET" = 1 ] && return
    echo "More: docs/pipelines.md, gst-inspect-1.0 snapsink (or snapsrc) for all properties,"
    echo "      GST_DEBUG=snapsrc:5,snapcast*:4 for logs. QUIET=1 hides these notes."
    echo
}

# ---------------------------------------------------------------------------
# Running.

pids=()
labels=()
cleanup() {
    for pid in "${pids[@]}"; do kill "$pid" 2>/dev/null || true; done
    wait 2>/dev/null || true
}

# Runs a command (or one of the functions below, which all `exec`) in the background,
# prefixing its output with a label. $! is the command itself, so cleanup can kill it.
bg() {
    local label=$1; shift
    ( "$@" ) > >(sed -u "s/^/[$label] /") 2>&1 &
    pids+=($!)
    labels+=("$label")
}

# Waits until the first background part exits, reports it and exits with its status
# (the EXIT trap then stops the others). Without this, `all` would keep running with a
# dead receiver, which looks like a hang.
wait_any() {
    local who status=0 i
    wait -n -p who "${pids[@]}" || status=$?
    for i in "${!pids[@]}"; do
        if [ "${pids[$i]}" = "$who" ]; then
            echo "listen.sh: '${labels[$i]}' exited (status $status), stopping the others" >&2
        fi
    done
    exit "$status"
}

# Warns if neither a PipeWire/PulseAudio server nor ALSA's `default` device is usable:
# autoaudiosink and the C++ snapclient (SOUNDCARD=default) then have nowhere to play.
# Typical in the devcontainer without its ~/.asoundrc (docs/testing-devcontainer.md).
check_audio_output() {
    local rt=${XDG_RUNTIME_DIR:-/run/user/$(id -u)}
    if [ -S "$rt/pipewire-0" ] || [ -S "$rt/pulse/native" ] || [ -n "${PULSE_SERVER:-}" ]; then
        return
    fi
    if timeout 5 aplay -q -D "$SOUNDCARD" -f S16_LE -r 48000 -c 2 -s 480 /dev/zero 2>/dev/null; then
        return
    fi
    echo "listen.sh: warning: no PipeWire/PulseAudio server, and ALSA '$SOUNDCARD' cannot be opened." >&2
    echo "  Receivers will fail or play nowhere. Set SINK=... and SOUNDCARD=..., or see" >&2
    echo "  docs/testing-devcontainer.md (inside the devcontainer) / docs/testing-host.md." >&2
}

send()       { build_send;       run_cmd; }
recv()       { build_recv;       run_cmd; }
cpp_client() { build_cpp_client; run_cmd; }
feeder()     { build_feeder;     run_cmd; }

cpp_server() {
    # Empty config: built-in defaults (0.35 refuses a missing file, 0.31 accepts both).
    : > "$dir/snapserver.conf"
    bg snapserver eval "build_cpp_server; run_cmd"
    for _ in $(seq 100); do [ -p "$dir/fifo" ] && break; sleep 0.05; done
    bg feeder feeder
    wait_any
}

mode=run
case "${1:-}" in
    -h|--help|help) usage; exit 0 ;;
    show)           mode=show; shift ;;
esac
what=${1:-}
case "$what" in
    send|recv|cpp-client|cpp-server|pair|all) ;;
    *) usage >&2; exit 2 ;;
esac

if [ "$what" = cpp-server ]; then
    if [ "$mode" = run ]; then
        # Global, not local: the EXIT trap needs it.
        dir=$(mktemp -d)
    else
        dir=/tmp/snapserver-listen   # illustrative; any empty directory
    fi
fi

if [ "$mode" = show ]; then
    explain "$what"
    explain_footer
    exit 0
fi

if [ "$SRC" = desktop ] && [ "$what" = all ]; then
    echo "SRC=desktop with 'all' is a feedback loop (the receivers play into the captured" >&2
    echo "desktop output). Run the receivers on another machine or another output (SINK=...)." >&2
    exit 2
fi

case "$what" in
    recv|cpp-client|pair|all) check_audio_output ;;
esac

if [ "$what" = cpp-server ]; then
    trap 'rm -rf "$dir"; cleanup' EXIT
else
    trap cleanup EXIT
fi
trap 'exit 130' INT TERM

explain "$what"
explain_footer
echo "---- running (Ctrl+C stops everything) ----"

case "$what" in
    send)       send ;;
    recv)       recv ;;
    cpp-client) cpp_client ;;
    cpp-server) cpp_server ;;
    pair)       bg snapsrc recv; bg snapclient cpp_client; wait_any ;;
    all)        bg snapsink send; sleep 1; bg snapsrc recv; bg snapclient cpp_client; wait_any ;;
esac
