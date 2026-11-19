#!/usr/bin/env bash
# Sender for a listening test with real music: plays a FLAC (or any audio file
# GStreamer can decode) into snapsink. Default: the CC0 jazz loop in assets/audio.
#
# Usage: scripts/host-play.sh [FILE]     (settings as for listen.sh, e.g. CODEC=flac)
#   e.g. scripts/host-play.sh "$HOME/mnt/musik/Anthony Braxton/Six Monk's Compositions/01 Brilliant Corners.flac"
#
# To hear, in a second terminal:
#   make listen-recv               # snapsrc
#   make listen-cpp-client         # or the C++ snapclient, for comparison
set -euo pipefail

# SONG="$HOME/mnt/musik/Anthony Braxton/Six Monk\'s Compositions/01 Brilliant Corners.flac"
SONG=${1:-"$(dirname "$0")/../assets/audio/freejazz_medley.flac"}
make -C "$(dirname "$0")/.." listen-send SRC="file:$(realpath "$SONG")"
