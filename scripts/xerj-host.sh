#!/usr/bin/env bash
# Runs the xerj search node and its MCP bridge on the HOST (not in the devcontainer), and
# indexes this repository, so that Claude Code in the devcontainer can use the `xerj` MCP
# server (http://host.docker.internal:GATEWAY_PORT/mcp). Background: docs/claude/mcp-xerj.md.
#
# Before running anything, it prints each command, ready to copy, with a note per option.
#
# Usage: scripts/xerj-host.sh <command>
#        scripts/xerj-host.sh show <command>   only print the commands and notes, run nothing
#        scripts/xerj-host.sh -h | --help      this help
#
# Commands:
#   up        start node and gateway (if not running yet), wait for them, then index
#   start     start node and gateway in the background (logs: see `status`)
#   index     (re)index this repository; the index does not update itself
#   brain     (re)build the link graph (brain) over this repository
#   status    what runs, and whether node, facts model and gateway answer
#   logs      follow both log files (Ctrl+C ends only the viewing)
#   stop      stop node and gateway (only those started by this script)
#   restart   stop, then start
#
# Settings (environment variables):
#   MODEL_DIR     facts model (default: ~/.local/share/xerj/model)
#   DATA_DIR      node data (default: ~/dev/xerj-data)
#   NODE_URL      where the node answers (default: http://127.0.0.1:9200); only for checks
#   GATEWAY_PORT  MCP bridge port (default: 9201)
#   GATEWAY_BIND  MCP bridge address (default: 0.0.0.0, so that the container reaches it)
#   QUIET         1: print the commands without the notes
set -euo pipefail

MODEL_DIR=${MODEL_DIR:-$HOME/.local/share/xerj/model}
DATA_DIR=${DATA_DIR:-$HOME/dev/xerj-data}
NODE_URL=${NODE_URL:-http://127.0.0.1:9200}
GATEWAY_PORT=${GATEWAY_PORT:-9201}
GATEWAY_BIND=${GATEWAY_BIND:-0.0.0.0}
QUIET=${QUIET:-0}

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
STATE_DIR=${XDG_STATE_HOME:-$HOME/.local/state}/gstreamer-snapcast-rs/xerj

usage() {
    sed -n '2,/^set -euo/p' "$0" | sed '$d; s/^# \{0,1\}//'
}

# ---------------------------------------------------------------------------
# Commands are built as environment settings, a program, and options, each with a note.
# The same text is printed for the user and executed, so what you see is what runs.

CMD_PROG="" CMD_PROG_NOTE=""
CMD_PARTS=() CMD_NOTES=()

new_cmd() {
    CMD_PROG=$1 CMD_PROG_NOTE=${2-}
    CMD_ENV=() CMD_ENV_NOTES=() CMD_PARTS=() CMD_NOTES=()
}
env_var() { CMD_ENV+=("$1"); CMD_ENV_NOTES+=("${2-}"); }
part() { CMD_PARTS+=("$1"); CMD_NOTES+=("${2-}"); }

cmd_line() {
    local line="" p
    for p in "${CMD_ENV[@]}"; do line+="$p "; done
    line+=$CMD_PROG
    for p in "${CMD_PARTS[@]}"; do line+=" $p"; done
    printf '%s' "$line"
}

show_cmd() {
    local title=$1 dir=${2-} p i
    echo "== $title"
    [ -n "$dir" ] && echo "cd $dir"
    for p in "${CMD_ENV[@]}"; do echo "$p \\"; done
    if [ ${#CMD_PARTS[@]} -gt 0 ]; then echo "$CMD_PROG \\"; else echo "$CMD_PROG"; fi
    for i in "${!CMD_PARTS[@]}"; do
        if [ "$i" -lt $((${#CMD_PARTS[@]} - 1)) ]; then
            echo "    ${CMD_PARTS[$i]} \\"
        else
            echo "    ${CMD_PARTS[$i]}"
        fi
    done
    if [ "$QUIET" != 1 ]; then
        echo
        for i in "${!CMD_ENV[@]}"; do
            [ -n "${CMD_ENV_NOTES[$i]}" ] && note_line "${CMD_ENV[$i]%%=*}" "${CMD_ENV_NOTES[$i]}"
        done
        [ -n "$CMD_PROG_NOTE" ] && note_line "$CMD_PROG" "$CMD_PROG_NOTE"
        for i in "${!CMD_PARTS[@]}"; do
            [ -n "${CMD_NOTES[$i]}" ] && note_line "${CMD_PARTS[$i]%% *}" "${CMD_NOTES[$i]}"
        done
    fi
    echo
}

note_line() {
    local label=$1 text=$2 first=1 line
    if [ ${#label} -gt 32 ]; then label="${label:0:29}..."; fi
    while IFS= read -r line; do
        if [ $first = 1 ]; then
            printf '    %-32s  %s\n' "$label" "$line"
            first=0
        else
            printf '    %-32s  %s\n' "" "$line"
        fi
    done <<< "$text"
}

# Single quotes for the shell, unless the value needs none.
q() {
    if [[ $1 =~ ^[A-Za-z0-9_./:@%+=-]+$ ]]; then printf '%s' "$1"; else printf "'%s'" "${1//\'/\'\\\'\'}"; fi
}

# ---------------------------------------------------------------------------
# The commands.

build_node() {
    new_cmd xerj "the search node: an Elasticsearch-compatible API at $NODE_URL"
    env_var XERJ_DISABLE_FEEDBACK=true \
        "never send field reports or corpus feedback to xerj.org (they publish session details)"
    env_var XERJ_ALLOW_INSECURE_NETWORK_BIND=true "needed together with --insecure"
    part "--facts-model-dir $(q "$MODEL_DIR")" \
        "model for XERJ-Query (the /_facts endpoint); without it, that endpoint is off"
    part "--insecure" "no TLS and no authentication: the node is for this machine only"
    part "--data-dir $(q "$DATA_DIR")" "where the indices, brains and memories are stored"
}

build_gateway() {
    new_cmd pnpx "runs an npm package without installing it"
    env_var XERJ_DISABLE_FEEDBACK=true "also for the MCP server started below"
    part "-y" "don't ask before downloading"
    part "supergateway" "bridges an MCP server from one transport to another"
    part "--stdio $(q "xerj mcp")" "the MCP server to bridge: xerj's stdio MCP server, a client of the node"
    part "--outputTransport streamableHttp" "serve it as MCP over HTTP (claude mcp add --transport http)"
    part "--port $GATEWAY_PORT" "Claude Code connects to http://host.docker.internal:$GATEWAY_PORT/mcp"
    part "--host $GATEWAY_BIND" "listen address; 0.0.0.0 so the container can reach it.
No API key: anyone who can reach this port can use the node"
}

build_index() {
    new_cmd "xerj autoindex" "index a folder (code symbols and prose sections) into the node"
    part "." "this repository, including the snapcast-rs submodule; run it again after
larger changes, the index is a snapshot"
}

build_brain() {
    new_cmd "xerj brain" "build the link graph between docs and code (xerj_brain_* tools)"
    part "." "this repository; the brain is named after the folder"
}

# ---------------------------------------------------------------------------
# Process handling. Each background process gets its own session (setsid), so that
# `stop` can end it together with its children (pnpx starts node, which starts xerj mcp).

pid_file() { echo "$STATE_DIR/$1.pid"; }
log_file() { echo "$STATE_DIR/$1.log"; }

running_pid() {
    local f pid
    f=$(pid_file "$1")
    [ -f "$f" ] || return 1
    pid=$(<"$f")
    if kill -0 "$pid" 2>/dev/null; then echo "$pid"; else rm -f "$f"; return 1; fi
}

http_code() {
    curl -s -o /dev/null -m 2 -w '%{http_code}' "$1" 2>/dev/null || true
}
node_up() { [[ $(http_code "$NODE_URL/") != 000 ]]; }
gateway_up() { [[ $(http_code "http://127.0.0.1:$GATEWAY_PORT/mcp") != 000 ]]; }

wait_for() {
    local what=$1 check=$2 seconds=$3 i
    for ((i = 0; i < seconds; i++)); do
        if $check; then return 0; fi
        sleep 1
    done
    echo "error: $what did not answer within ${seconds}s; see $(log_file "$what")" >&2
    return 1
}

start_bg() {
    local name=$1 pid
    mkdir -p "$STATE_DIR"
    setsid bash -c "$(cmd_line)" > "$(log_file "$name")" 2>&1 < /dev/null &
    pid=$!
    echo "$pid" > "$(pid_file "$name")"
    echo "started $name (pid $pid, log $(log_file "$name"))"
}

stop_bg() {
    local name=$1 pid i
    if ! pid=$(running_pid "$name"); then
        echo "$name: not started by this script"
        return 0
    fi
    kill -TERM -- "-$pid" 2>/dev/null || kill -TERM "$pid" 2>/dev/null || true
    for ((i = 0; i < 10; i++)); do
        kill -0 "$pid" 2>/dev/null || break
        sleep 1
    done
    if kill -0 "$pid" 2>/dev/null; then kill -KILL -- "-$pid" 2>/dev/null || true; fi
    rm -f "$(pid_file "$name")"
    echo "stopped $name (pid $pid)"
}

# ---------------------------------------------------------------------------

require_host() {
    if [ -f /run/.containerenv ] || [ -f /.dockerenv ] || [ -n "${REMOTE_CONTAINERS:-}" ]; then
        echo "error: this runs on the host, not in the devcontainer (use: $0 show $1)" >&2
        exit 1
    fi
    local tool
    for tool in "$@"; do
        [ "$tool" = "$1" ] && continue
        command -v "$tool" > /dev/null || { echo "error: '$tool' not found in PATH" >&2; exit 1; }
    done
}

do_start() {
    if node_up; then
        echo "node: already answers at $NODE_URL, not starting another"
    else
        build_node; start_bg node
        wait_for node node_up 60
    fi
    if gateway_up; then
        echo "gateway: already answers on port $GATEWAY_PORT, not starting another"
    else
        build_gateway; start_bg gateway
        wait_for gateway gateway_up 60   # the first pnpx run downloads supergateway
    fi
}

do_index() {
    (cd "$ROOT" && eval "$(cmd_line)")
}

do_status() {
    local pid state
    for name in node gateway; do
        if pid=$(running_pid "$name"); then
            echo "$name: started by this script, pid $pid, log $(log_file "$name")"
        else
            echo "$name: not started by this script"
        fi
    done
    if node_up; then
        echo "node: answers at $NODE_URL"
        state=$(curl -s -m 2 "$NODE_URL/_facts" 2>/dev/null \
            | sed -n 's/.*"state"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -n 1)
        echo "facts model: ${state:-unknown (no --facts-model-dir?)}"
    else
        echo "node: does not answer at $NODE_URL"
    fi
    if gateway_up; then
        echo "gateway: answers at http://127.0.0.1:$GATEWAY_PORT/mcp"
    else
        echo "gateway: does not answer on port $GATEWAY_PORT"
    fi
}

show() {
    case $1 in
        up)    show start; show index ;;
        start) build_node; show_cmd "node (background)"
               build_gateway; show_cmd "MCP gateway (background)" ;;
        index) build_index; show_cmd "index this repository" "$ROOT" ;;
        brain) build_brain; show_cmd "build the brain" "$ROOT" ;;
        *) echo "nothing to show for '$1' (commands with output: up, start, index, brain)" >&2
           exit 2 ;;
    esac
}

cmd=${1:-}
case $cmd in
    "" | -h | --help | help) usage ;;
    show) show "${2:-up}" ;;
    up | start | restart)
        require_host "$cmd" xerj pnpx curl setsid
        if [ "$cmd" = restart ]; then stop_bg gateway; stop_bg node; fi
        if [ "$cmd" = up ]; then show up; else show start; fi
        echo "---- running ----"
        do_start
        if [ "$cmd" = up ]; then build_index; do_index; fi
        do_status ;;
    index | brain)
        require_host "$cmd" xerj
        show "$cmd"
        echo "---- running ----"
        node_up || { echo "error: the node does not answer at $NODE_URL; run: $0 start" >&2; exit 1; }
        "build_$cmd"; do_index ;;
    status) do_status ;;
    logs)
        mkdir -p "$STATE_DIR"; touch "$(log_file node)" "$(log_file gateway)"
        tail -n 50 -F "$(log_file node)" "$(log_file gateway)" ;;
    stop) stop_bg gateway; stop_bg node ;;
    *) echo "unknown command '$cmd'" >&2; usage >&2; exit 2 ;;
esac
