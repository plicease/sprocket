#!/bin/sh
# SPDX-License-Identifier: MIT
#
# Emulator imaging test: attach a 160K floppy image and a small hard disk
# image to DOSBox-X's BIOS (INT 13h only, no DOS drive), run
# `SPROCKET SERVER COM1 baud /D` with COM1 as a TCP null-modem, and image
# both with `sprocket image`, checking the copies byte for byte against
# the originals. The disks hold random data, so a sector read from the
# wrong place shows. It takes about six minutes, nearly all of it 3 MB
# crossing the emulated serial line.
#
# Usage: test/dosbox-image.sh [baud]   (from client/dos, after `make`;
#                                       needs dosbox-x and a host build)
#
# Set SHOW=1 to show the DOSBox-X window instead of running headless.

set -eu

baud=${1:-115200}
port=${PORT:-5111}
here=$(cd "$(dirname "$0")/.." && pwd)
top=$(cd "$here/../.." && pwd)
sprocket=${SPROCKET:-$top/target/debug/sprocket}
work=$(mktemp -d)
dosbox=
trap 'kill $dosbox 2>/dev/null || true; rm -rf "$work"' EXIT

mkdir -p "$work/c" "$work/local"
cp "$here/build/SPROCKET.EXE" "$work/c/"
# 160K: 40 cylinders, 1 head, 8 sectors. The hard disk: 88 cylinders,
# 4 heads, 17 sectors (DOSBox-X takes an image under 2.88M for a floppy).
head -c 163840 /dev/urandom > "$work/floppy.img"
head -c $((88 * 4 * 17 * 512)) /dev/urandom > "$work/hd.img"

cat > "$work/dosbox.conf" <<EOF
[sdl]
output=surface
[dosbox]
quit warning=false
[cpu]
core=normal
cycles=max
[serial]
serial1=nullmodem port:$port transparent:1
serial2=disabled
[autoexec]
mount c $work/c
imgmount 0 $work/floppy.img -t floppy -fs none
imgmount 2 $work/hd.img -t hdd -fs none -size 512,17,4,88
c:
C:\\SPROCKET SERVER COM1 $baud /D > C:\\OUT.TXT
EOF

if [ "${SHOW:-0}" = 1 ]; then
    dosbox-x -conf "$work/dosbox.conf" -nopromptfolder >"$work/dosbox.log" 2>&1 &
else
    SDL_VIDEODRIVER=dummy SDL_AUDIODRIVER=dummy \
        dosbox-x -conf "$work/dosbox.conf" -nopromptfolder >"$work/dosbox.log" 2>&1 &
fi
dosbox=$!

for _ in $(seq 30); do
    ss -ltn "sport = :$port" | grep -q LISTEN && break
    sleep 0.5
done

link="--port tcp://127.0.0.1:$port --baud $baud"
status=0
(
    cd "$work/local"
    # shellcheck disable=SC2086
    "$sprocket" image $link --wait 30 --list &&
        "$sprocket" image $link --device 0 --output floppy.img --yes </dev/null &&
        "$sprocket" image $link --device 1 --output hd.img --yes </dev/null
) > "$work/client.txt" 2>&1 || status=$?

cat "$work/client.txt"
fail=0
check() {
    if grep -q -- "$1" "$work/client.txt"; then
        echo "ok: $2"
    else
        echo "FAIL: $2"
        fail=1
    fi
}
check "device 0: BIOS 00 (A:) 160K, removable, 320 units" "the floppy is listed"
check "device 1: BIOS 80 fixed 2.9M, fixed, 5984 units" "the hard disk is listed"
for disk in floppy hd; do
    if cmp -s "$work/$disk.img" "$work/local/$disk.img"; then
        echo "ok: the $disk image matches"
    else
        echo "FAIL: the $disk image differs"
        fail=1
    fi
    if grep -q '"status": "complete"' "$work/local/$disk.img.json" 2>/dev/null; then
        echo "ok: the $disk manifest says complete"
    else
        echo "FAIL: the $disk manifest is missing or incomplete"
        fail=1
    fi
done

if [ "$status" -ne 0 ] || [ "$fail" -ne 0 ]; then
    echo "--- server output"
    tr -d '\r' < "$work/c/OUT.TXT" 2>/dev/null || true
    exit 1
fi
echo "dosbox-image: ok"
