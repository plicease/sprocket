#!/bin/sh
# SPDX-License-Identifier: MIT
#
# Emulator smoke test: run SPROCKET.EXE under DOSBox-X as an 8086 with COM1
# as a TCP null-modem, run `sprocket hello` against it, and check the
# client's counters.
#
# Usage: test/dosbox-hello.sh [baud]    (from client/dos, after `make`;
#                                        needs dosbox-x and a host build)
#
# By default the link is clean, and the client must report every request
# as a new frame, with nothing dropped or repeated and no UART errors.
#
# FAULT=1 runs the link through test/fault-relay.py, which corrupts one byte
# of the first PING request and one byte of the client's first PING
# response. The client must then report exactly one dropped frame (the
# damaged request, which the host resends) and one repeated request (the
# host resending after the damaged response, answered from the cache).
#
# Set SHOW=1 to show the DOSBox-X window instead of running headless.

set -eu

baud=${1:-9600}
pings=3
port=${PORT:-5100}
relay_port=$((port + 1))
here=$(cd "$(dirname "$0")/.." && pwd)
top=$(cd "$here/../.." && pwd)
sprocket=${SPROCKET:-$top/target/debug/sprocket}
work=$(mktemp -d)
dosbox=
relay=
trap 'kill $dosbox $relay 2>/dev/null || true; rm -rf "$work"' EXIT

if [ "${FAULT:-0}" = 1 ]; then
    # Counted from the client's first byte: byte 50 sent to the client is in
    # the first PING; the HELLO_REPLY is under 50 bytes, so byte 77 from the
    # client is in its first PING response.
    expect_frames=$((pings + 3))
    expect_dropped=1
    expect_repeated=1
else
    expect_frames=$((pings + 2))
    expect_dropped=0
    expect_repeated=0
fi

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
SPROCKET SERVER COM1 $baud > OUT.TXT
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

target=$port
if [ "${FAULT:-0}" = 1 ]; then
    python3 -I "$here/test/fault-relay.py" "$relay_port" "$port" 50 77 &
    relay=$!
    for _ in $(seq 20); do
        ss -ltn "sport = :$relay_port" | grep -q LISTEN && break
        sleep 0.25
    done
    target=$relay_port
fi

status=0
"$sprocket" hello --port "tcp://127.0.0.1:$target" --baud "$baud" --wait 30 \
    --pings "$pings" || status=$?

# Give the client time to print its summary and exit.
for _ in 1 2 3 4 5 6 7 8 9 10; do
    grep -q "frames" "$work/OUT.TXT" 2>/dev/null && break
    sleep 1
done
echo "--- SPROCKET.EXE output"
cat "$work/OUT.TXT" 2>/dev/null || echo "(none)"
echo "---"

fail() {
    echo "FAIL: $*"
    status=1
}

grep -q "closed by the client" "$work/OUT.TXT" 2>/dev/null ||
    fail "SPROCKET did not report the session closed by the client"

# "N frames, N dropped, N repeated, N receive errors."
counters=$(tr -d '\r' < "$work/OUT.TXT" 2>/dev/null |
    sed -n 's/^\([0-9]*\) frames, \([0-9]*\) dropped, \([0-9]*\) repeated, \([0-9]*\) receive errors\.$/\1 \2 \3 \4/p')
if [ -z "$counters" ]; then
    fail "no counters in the client's output"
else
    set -- $counters
    [ "$1" = "$expect_frames" ] || fail "$1 frames, expected $expect_frames"
    [ "$2" = "$expect_dropped" ] || fail "$2 dropped, expected $expect_dropped"
    [ "$3" = "$expect_repeated" ] || fail "$3 repeated, expected $expect_repeated"
    [ "$4" = 0 ] || fail "$4 receive errors, expected 0"
fi

if [ "$status" = 0 ]; then
    echo "PASS"
fi
exit "$status"
