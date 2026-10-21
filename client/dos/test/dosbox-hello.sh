#!/bin/sh
# SPDX-License-Identifier: MIT
#
# Emulator smoke test: run SPROCKET.EXE under DOSBox-X as an 8086 with COM1
# as a TCP null-modem, and run `sprocket hello` against it.
#
# Usage: test/dosbox-hello.sh [baud]    (from client/dos, after `make`;
#                                        needs dosbox-x and a host build)
#
# Set SHOW=1 to show the DOSBox-X window instead of running headless.

set -eu

baud=${1:-9600}
port=${PORT:-5100}
here=$(cd "$(dirname "$0")/.." && pwd)
top=$(cd "$here/../.." && pwd)
sprocket=${SPROCKET:-$top/target/debug/sprocket}
work=$(mktemp -d)
trap 'kill "$dosbox" 2>/dev/null || true; rm -rf "$work"' EXIT

cp "$here/build/SPROCKET.EXE" "$work/"
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
serial1=nullmodem port:$port transparent:1
serial2=disabled
[autoexec]
mount c $work
c:
SPROCKET COM1 $baud > OUT.TXT
EOF
# DOSBox-X is left running after SPROCKET exits and is stopped when this
# script exits: quitting at once could drop the last bytes sent.

if [ "${SHOW:-0}" = 1 ]; then
    dosbox-x -conf "$work/dosbox.conf" -nopromptfolder >"$work/dosbox.log" 2>&1 &
else
    SDL_VIDEODRIVER=dummy SDL_AUDIODRIVER=dummy \
        dosbox-x -conf "$work/dosbox.conf" -nopromptfolder >"$work/dosbox.log" 2>&1 &
fi
dosbox=$!

# Wait for the emulated COM1 to listen.
for _ in $(seq 30); do
    ss -ltn "sport = :$port" | grep -q LISTEN && break
    sleep 0.5
done

status=0
"$sprocket" hello --port "tcp://127.0.0.1:$port" --baud "$baud" --wait 30 --pings 3 || status=$?

# Give the client time to print its summary and exit.
for _ in 1 2 3 4 5 6 7 8 9 10; do
    grep -q "frames" "$work/OUT.TXT" 2>/dev/null && break
    sleep 1
done
echo "--- SPROCKET.EXE output"
cat "$work/OUT.TXT" 2>/dev/null || echo "(none)"
if ! grep -q "closed by the host" "$work/OUT.TXT" 2>/dev/null; then
    status=1
fi
exit "$status"
