#!/bin/sh
# SPDX-License-Identifier: MIT
#
# Emulator file-tree test: run `SPROCKET SERVER COM1 baud . /W` in
# C:\SHARE under DOSBox-X as an 8086, with COM1 as a TCP null-modem, drive
# it with `sprocket client` in two sessions, and check the files on both
# sides: a listing, a download, uploads, a replacement that keeps NAME.BAK,
# and a name DOS cannot hold. The second session needs the server to keep
# running after the first client quits.
#
# Usage: test/dosbox-tree.sh [baud]    (from client/dos, after `make`;
#                                       needs dosbox-x and a host build)
#
# Set SHOW=1 to show the DOSBox-X window instead of running headless.

set -eu

baud=${1:-38400}
port=${PORT:-5110}
here=$(cd "$(dirname "$0")/.." && pwd)
top=$(cd "$here/../.." && pwd)
sprocket=${SPROCKET:-$top/target/debug/sprocket}
work=$(mktemp -d)
dosbox=
trap 'kill $dosbox 2>/dev/null || true; rm -rf "$work"' EXIT

mkdir -p "$work/c/SHARE/GAMES" "$work/local"
cp "$here/build/SPROCKET.EXE" "$work/c/"
head -c 3000 /dev/urandom > "$work/c/SHARE/GAMES/DOOM.EXE"
printf 'old readme\r\n' > "$work/c/SHARE/README.TXT"
printf 'uploaded\r\n' > "$work/local/up.txt"
printf 'long name\r\n' > "$work/local/toolongname.txt"

cat > "$work/dosbox.conf" <<EOF
[sdl]
output=surface
[dosbox]
quit warning=false
[cpu]
cputype=8086
core=normal
cycles=fixed 3000
[serial]
serial1=nullmodem port:$port transparent:1
serial2=disabled
[autoexec]
mount c $work/c
c:
cd \\SHARE
C:\\SPROCKET SERVER COM1 $baud . /W > C:\\OUT.TXT
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

status=0
(
    cd "$work/local"
    printf '%s\n' 'ls -R' 'get games/doom.exe' 'get readme.txt' 'put up.txt' \
        'put toolongname.txt' 'quit' |
        "$sprocket" client --port "tcp://127.0.0.1:$port" --baud "$baud" --wait 30
) > "$work/client.txt" 2>&1 || status=$?
# Replace README.TXT with a changed copy, in a second session with the
# same server.
printf 'new readme\r\n' > "$work/local/readme.txt"
(
    cd "$work/local"
    printf '%s\n' 'put readme.txt' 'put -f readme.txt' 'ls' 'quit' |
        "$sprocket" client --port "tcp://127.0.0.1:$port" --baud "$baud" --wait 30
) >> "$work/client.txt" 2>&1 || status=$?

echo "--- sprocket client"
cat "$work/client.txt"
echo "---"

fail() {
    echo "FAIL: $*"
    status=1
}

share=$work/c/SHARE
cmp -s "$share/GAMES/DOOM.EXE" "$work/local/DOOM.EXE" || fail "DOOM.EXE differs"
[ -f "$work/local/README.TXT" ] || fail "README.TXT was not fetched"
cmp -s "$share/UP.TXT" "$work/local/up.txt" || fail "UP.TXT was not uploaded"
grep -q "toolongname.txt: " "$work/client.txt" || fail "a non-8.3 name was not refused"
ls "$share" | grep -qi '^toolongn' && fail "a non-8.3 name was written"
grep -q "File exists" "$work/client.txt" || fail "put without -f did not refuse"
cmp -s "$share/README.TXT" "$work/local/readme.txt" || fail "README.TXT was not replaced"
grep -q "old readme" "$share/README.BAK" 2>/dev/null || fail "README.BAK does not hold the old file"
ls "$share" | grep -q '\$\$\$' && fail "a temporary file was left behind"

if [ "$status" = 0 ]; then
    echo "PASS"
fi
exit "$status"
