#!/usr/bin/env bash
# Prints the BUILD-INFO file of a release tarball (`make dist`).
# Usage: scripts/dist-build-info.sh VERSION COMMIT [SNAPSINK_FEATURES]
set -euo pipefail

version=$1
commit=$2
features=${3:-}

echo "gstreamer-snapcast-rs $version, commit $commit, built $(date -u +%Y-%m-%dT%H:%MZ)"
echo "architecture: $(uname -m)"
echo "glibc: $(ldd --version | head -1)"
echo "built against GStreamer $(pkg-config --modversion gstreamer-1.0) (needs 1.22 or newer)"
echo "rustc: $(rustc --version)"
echo "snapsink features: default (shared)${features:+ + $features}"
