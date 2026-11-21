#!/usr/bin/env bash
# Downloads Snapweb (the web UI of Snapcast) for snapsink's `snapweb-dir`.
#
# Debian/Ubuntu's snapserver package ships only a placeholder page in
# /usr/share/snapserver/snapweb, so the real UI comes from Snapweb's GitHub release
# (the same version as the reference/snapweb submodule).
#
# Usage: scripts/fetch-snapweb.sh [DIR]     default DIR: ~/.local/share/snapweb
#        scripts/fetch-snapweb.sh -h | --help
#
# Then:  gst-launch-1.0 ... ! snapsink http-port=1780 snapweb-dir=DIR
#        and open http://<this host>:1780/ in a browser.
set -euo pipefail

VERSION=v0.9.3
URL="https://github.com/snapcast/snapweb/releases/download/$VERSION/snapweb.zip"
SHA256=cc258df98b8c474ea19048bc84c0c2e2c5fbc3b0856aef03a838f3a4489fa4c7

case ${1:-} in
    -h | --help) sed -n '2,/^set -euo/p' "$0" | sed '$d; s/^# \{0,1\}//'; exit 0 ;;
esac
DIR=${1:-${XDG_DATA_HOME:-$HOME/.local/share}/snapweb}

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

echo "Downloading Snapweb $VERSION from $URL"
curl -fsSL -o "$tmp/snapweb.zip" "$URL"
echo "$SHA256  $tmp/snapweb.zip" | sha256sum -c --quiet - || {
    echo "error: checksum mismatch for $URL" >&2
    exit 1
}

mkdir -p "$tmp/unpacked"
if command -v unzip > /dev/null; then
    unzip -q "$tmp/snapweb.zip" -d "$tmp/unpacked"
else
    python3 -c 'import sys, zipfile; zipfile.ZipFile(sys.argv[1]).extractall(sys.argv[2])' \
        "$tmp/snapweb.zip" "$tmp/unpacked"
fi
# The archive may or may not wrap the files in a top-level folder.
src=$(dirname "$(find "$tmp/unpacked" -name index.html -print -quit)")
[ -f "$src/index.html" ] || { echo "error: no index.html in the archive" >&2; exit 1; }

rm -rf "$DIR"
mkdir -p "$(dirname "$DIR")"
cp -r "$src" "$DIR"
echo "Snapweb $VERSION installed in $DIR"
echo "Use it with: snapsink http-port=1780 snapweb-dir=$DIR"
