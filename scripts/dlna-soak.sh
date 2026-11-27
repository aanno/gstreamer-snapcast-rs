#!/usr/bin/env bash
# DLNA PoC long run (docs/dlna-tests.md, test 14): one CSV row per interval with what
# should stay flat over a night of playing: the Snapcast sessions (streams on the
# server, session-sender helpers), and memory (RSS), threads and open files of dms,
# the daemon and the renderers. Growth or leftover sessions show up in the morning.
#
# Usage: scripts/dlna-soak.sh [out.csv]      (default target/dlna-soak-<date>.csv)
# Env:   INTERVAL (seconds, default 60), CONTROL_HOST (127.0.0.1), CONTROL_PORT (1705)
#
# Needs only bash and ps (procps). Stop it with Ctrl-C; the CSV is complete per row.
set -euo pipefail

INTERVAL=${INTERVAL:-60}
CONTROL_HOST=${CONTROL_HOST:-127.0.0.1}
CONTROL_PORT=${CONTROL_PORT:-1705}
out=${1:-target/dlna-soak-$(date +%Y%m%d-%H%M).csv}
mkdir -p "$(dirname "$out")"

# Sum of RSS (kB), threads and open files of all processes named exactly $1 (by the
# kernel's 15-character comm), as "rss;threads;fds;count".
usage_of() {
    local rss=0 thr=0 fds=0 n=0 pid
    for pid in $(pgrep -x "$1" || true); do
        local stat
        stat=$(ps -o rss=,nlwp= -p "$pid" 2>/dev/null) || continue
        read -r r t <<<"$stat"
        rss=$((rss + r)); thr=$((thr + t)); n=$((n + 1))
        fds=$((fds + $(ls /proc/"$pid"/fd 2>/dev/null | wc -l)))
    done
    echo "$rss;$thr;$fds;$n"
}

# Streams on the Snapcast server (Server.GetStatus over JSON-RPC), as
# "all;dms-sessions", or "-;-" if the control port doesn't answer.
streams() {
    local reply
    reply=$(timeout 3 bash -c "exec 3<>/dev/tcp/$CONTROL_HOST/$CONTROL_PORT
        printf '%s\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"Server.GetStatus\"}' >&3
        while read -r line <&3; do case \$line in *'\"id\":1'*) echo \"\$line\"; break;; esac; done" \
        2>/dev/null) || { echo "-;-"; return; }
    # Stream objects carry "uri"; group and client objects don't.
    local all dms
    all=$(grep -o '"uri":{' <<<"$reply" | wc -l)
    # Stream ids (groups refer to them as "stream_id", which doesn't match).
    dms=$(grep -o '"id":"dms-[0-9a-f]*"' <<<"$reply" | sort -u | wc -l)
    echo "$all;$dms"
}

header="time;streams;dms_streams"
for p in dms gst-snapcast-da session-sender gmediarender; do
    header="$header;${p}_rss_kb;${p}_threads;${p}_fds;${p}_count"
done
[ -s "$out" ] || echo "$header" >"$out"
echo "Writing $out every ${INTERVAL}s (Ctrl-C to stop)"

while true; do
    row="$(date +%FT%T);$(streams)"
    # Process names are matched as the kernel shows them (15 characters).
    for p in dms gst-snapcast-da session-sender gmediarender; do
        row="$row;$(usage_of "$p")"
    done
    echo "$row" >>"$out"
    echo "$row"
    sleep "$INTERVAL"
done
