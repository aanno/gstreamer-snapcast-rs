#!/usr/bin/env bash
# sudo modprobe snd-aloop                 # if not loaded yet

make install 2>&1 | tee install-host.log
make doctor 2>&1 | tee doctor-host.log
make check 2>&1 | tee check-host.log
# needs snapclient/snapserver on the host (SNAPCLIENT=/SNAPSERVER=)
make test-interop 2>&1 | tee interop-host.log
make test-aloop 2>&1 | tee aloop-host.log
grep -E 'aloop:|panicked|busy' aloop-host.log > aloop-host-errors.log || true
# by ear: one click per second or a double click? Ctrl+C to stop
make listen-all 2>&1 | tee listen-host.log
