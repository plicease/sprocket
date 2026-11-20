#!/bin/sh
# SPDX-License-Identifier: MIT
#
# Emulator formatter test: run build/FMTTEST.EXE (test/fmtcases.c with the
# DOS build's printf, src/dosfmt.c) under DOSBox-X as an 8086, and require
# its output to be build/test_fmtcases's, the same cases with the host C
# library's printf.
#
# Usage: test/dosbox-fmt.sh    (from client/dos, after `make dosbox-fmt`'s
#                               prerequisites; needs dosbox-x)
#
# Set SHOW=1 to show the DOSBox-X window instead of running headless.

set -eu

here=$(cd "$(dirname "$0")/.." && pwd)
work=$(mktemp -d)
dosbox=
trap 'kill $dosbox 2>/dev/null || true; rm -rf "$work"' EXIT

cp "$here/build/FMTTEST.EXE" "$work/"
cat > "$work/dosbox.conf" <<EOF
[sdl]
output=surface
[dosbox]
quit warning=false
[cpu]
cputype=8086
core=normal
cycles=fixed 1000
[serial]
serial1=disabled
serial2=disabled
[autoexec]
mount c $work
c:
FMTTEST > OUT.TXT
EOF

if [ "${SHOW:-0}" = 1 ]; then
    dosbox-x -conf "$work/dosbox.conf" -nopromptfolder >"$work/dosbox.log" 2>&1 &
else
    SDL_VIDEODRIVER=dummy SDL_AUDIODRIVER=dummy \
        dosbox-x -conf "$work/dosbox.conf" -nopromptfolder >"$work/dosbox.log" 2>&1 &
fi
dosbox=$!

# FMTTEST.EXE ends its output with END.
for _ in $(seq 60); do
    grep -q '^END' "$work/OUT.TXT" 2>/dev/null && break
    sleep 0.5
done

"$here/build/test_fmtcases" > "$work/want.txt"
tr -d '\r' < "$work/OUT.TXT" > "$work/got.txt" 2>/dev/null || : > "$work/got.txt"

if diff -u "$work/want.txt" "$work/got.txt"; then
    echo "$(wc -l < "$work/want.txt") lines, the same as the host's"
    echo "PASS"
else
    echo "FAIL: FMTTEST.EXE's output (+) is not the host's (-)"
    exit 1
fi
