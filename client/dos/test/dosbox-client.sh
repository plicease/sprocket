#!/bin/sh
# SPDX-License-Identifier: MIT
#
# Emulator client test: run `SPROCKET CLIENT COM1 baud < CMDS.TXT` under
# DOSBox-X as an 8086, with COM1 as a TCP null-modem, against
# `sprocket serve --writable` on Linux, and check the files on both sides:
# listings, single and wildcard downloads with their dates, a name DOS
# cannot hold, uploads, and refusing to replace a file without -f.
#
# Usage: test/dosbox-client.sh [baud]    (from client/dos, after `make`;
#                                         needs dosbox-x and a host build)
#
# Set SHOW=1 to show the DOSBox-X window instead of running headless.

set -eu

baud=${1:-38400}
port=${PORT:-5120}
here=$(cd "$(dirname "$0")/.." && pwd)
top=$(cd "$here/../.." && pwd)
sprocket=${SPROCKET:-$top/target/debug/sprocket}
work=$(mktemp -d)
dosbox=
serve=
trap 'kill $dosbox $serve 2>/dev/null || true; [ -n "${KEEP:-}" ] || rm -rf "$work"' EXIT

mkdir -p "$work/c" "$work/srv/games" "$work/srv/docs"
cp "$here/build/SPROCKET.EXE" "$work/c/"
head -c 3000 /dev/urandom > "$work/srv/DATA.BIN"
touch -d '2001-02-03 04:05:06' "$work/srv/DATA.BIN"
printf 'a' > "$work/srv/games/a.exe"
printf 'bb' > "$work/srv/games/b.exe"
printf 'not this' > "$work/srv/games/c.txt"
printf 'long' > "$work/srv/docs/a-long-name.txt"
printf 'uploaded\r\n' > "$work/c/UP.TXT"
printf 'old\r\n' > "$work/c/A.EXE"
printf '%s\r\n' 'ls -R' 'get data.bin' 'cd games' 'get *.exe' 'get -f *.exe' \
    'cd /docs' 'get a-long-name.txt' 'cd ..' 'put up.txt' 'put up.txt' 'lls' \
    'quit' > "$work/c/CMDS.TXT"

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
SPROCKET CLIENT COM1 $baud < CMDS.TXT > OUT.TXT
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

"$sprocket" serve --port "tcp://127.0.0.1:$port" --baud "$baud" --root "$work/srv" \
    --writable > "$work/serve.log" 2>&1 &
serve=$!

status=0
[ -n "${KEEP:-}" ] && echo "work: $work"
for _ in $(seq 60); do
    grep -q "Goodbye" "$work/c/OUT.TXT" 2>/dev/null && break
    sleep 1
done
sleep 1

echo "--- SPROCKET.EXE output"
tr -d '\r' < "$work/c/OUT.TXT" 2>/dev/null || echo "(none)"
echo "---"

fail() {
    echo "FAIL: $*"
    status=1
}

out=$work/c/OUT.TXT
grep -q "Connected to SPROCKET .* on Linux, protocol 1.1, writable" "$out" ||
    fail "no session"
grep -q "games/b.exe" "$out" || fail "ls -R did not list games/b.exe"
cmp -s "$work/srv/DATA.BIN" "$work/c/DATA.BIN" || fail "DATA.BIN differs"
[ "$(date -r "$work/c/DATA.BIN" +%s)" = "$(date -r "$work/srv/DATA.BIN" +%s)" ] ||
    fail "DATA.BIN does not have the server's date"
grep -q "a.exe: File exists; use get -f to replace it" "$out" ||
    fail "get without -f did not refuse"
cmp -s "$work/srv/games/a.exe" "$work/c/A.EXE" || fail "A.EXE was not replaced by get -f"
cmp -s "$work/srv/games/b.exe" "$work/c/B.EXE" || fail "B.EXE differs"
[ ! -e "$work/c/C.TXT" ] || fail "c.txt was fetched by *.exe"
grep -q "a-long-name.txt: not a valid name here" "$out" || fail "a long name was not refused"
cmp -s "$work/c/UP.TXT" "$work/srv/up.txt" 2>/dev/null ||
    cmp -s "$work/c/UP.TXT" "$work/srv/UP.TXT" || fail "UP.TXT was not uploaded"
grep -qi "up.txt: File exists; use put -f to replace it" "$out" ||
    fail "put without -f did not refuse"
ls "$work/c" | grep -q '\$\$\$' && fail "a temporary file was left behind"

if [ "$status" = 0 ]; then
    echo "PASS"
fi
exit "$status"
