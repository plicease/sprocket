# SPROCKET for DOS

`SPROCKET.EXE`, the native-protocol program for real-mode DOS on 8086/8088 and later CPUs, built with Open Watcom. It implements [protocol v1](../../docs/protocol-v1.md) (`HELLO`, `PING` and `CLOSE`, with the duplicate-request rules) and the file trees of [protocol v1.1](../../docs/protocol-v1.1.md), in either role: as the server that shares a directory, or as the client that browses one and copies files. Disk images come in a later milestone.

```text
SPROCKET CLIENT [port [baud]]                 default: COM1 9600
SPROCKET SERVER [port [baud [dir]]] [/W]      default: COM1 9600
```

`SPROCKET` with no subcommand prints its usage.

`SPROCKET CLIENT` calls a server (`sprocket serve` on Linux, or `SPROCKET SERVER` on another DOS machine) and gives the same `/>` prompt, commands, messages and listings as `sprocket client` on Linux: `ls`, `cd`, `get`, `put`, `lls`, `lcd` and the rest (type `help`). `get` copies remote files into the local directory and `put` sends local files to the server; both take `*` and `?` wildcards, refuse to replace a file without `-f`, and stop a batch at the first file that fails. Received files are written as `NAME.$$$` and renamed into place when complete, with the server's date and time. A remote name that is not a valid 8.3 name is refused rather than shortened. Line input is plain: Backspace erases, Esc clears the line, and Ctrl-D (or Ctrl-Z) on an empty line ends the session; `set editing` is recognized and reported as unavailable. Esc or Ctrl-C stops a long listing or a transfer. Commands can also come from a file: `SPROCKET CLIENT COM1 38400 < CMDS.TXT`.

`SPROCKET SERVER` waits for a client (`sprocket client` or `sprocket hello` on Linux, or `SPROCKET CLIENT`), answers until the client sends `CLOSE`, and exits. Press Esc to exit early. With a directory, such as `SPROCKET SERVER COM1 38400 C:\SHARE`, the client can list and fetch the files below it; `/W` also lets the client send files there. Files arrive as `NAME.$$$` and are put in place only after their length and CRC-32 are checked; replacing a file (`put -f` on the client) keeps the old one as `NAME.BAK`. Without a directory, only `HELLO`, `PING` and `CLOSE` are answered. On exit it prints how many frames it received and dropped, how many requests were repeats, and how many UART receive errors (overruns or framing errors) it saw.

**Disable write-behind disk caching while transferring files**, such as SMARTDRV's write caching. Files are written under a temporary name and put in place only once complete and checked, but that keeps a complete copy through a power failure only if the disk completes writes in the order DOS issues them, which a write-behind cache does not promise ([protocol v1](../../docs/protocol-v1.md#what-an-interrupted-commit-leaves-behind)). If a transfer to a DOS server is interrupted while a file is being replaced, the old version is in `NAME.BAK`.

## Design

- **Polled UART, no interrupts, no FIFO.** The UART's interrupts are left off and its FIFO, if any, is disabled. That works because the host sends one request at a time and waits: the line is silent whenever the client is busy. The port's base address comes from the BIOS data area, so any COM1 to COM4 the BIOS found will work.
- **No screen output between sending a response and reading the next request.** Scrolling the screen on an 8088 is slow enough to overrun a UART without a FIFO, and the host sends its next request as soon as it has a response. The client prints before it sends.
- **Fast enough per byte.** With a polled UART and no FIFO, the client must finish with each byte before the next one arrives: 1041 µs at 9600 baud, 260 µs at 38400. On a NuXT's NEC V20 at 4.77 MHz, the receive path (poll the UART, feed the decoder) takes about 187 µs per byte, measured by `BENCH.EXE`: 1.3× headroom at 38400, and too slow for 57600. At 9.54 MHz it takes 93 µs, enough for 57600 but not 115200. Real runs failed at exactly those limits. Nothing else runs per byte: the timer and keyboard are checked only while the line is idle.
- **Frame work waits for the line to go quiet.** The CRC of a frame is checked when its closing flag arrives, and the response is built after that, while the host waits. On the V20 at 4.77 MHz the check takes about 104 ms for a 1 KB frame, most of the 120 ms the host waits for each reply. Encoding the response happens as it is sent, so it does not add to that wait.
- **The client prints only between requests.** It sends one request at a time and waits for the response with nothing but the UART poll and the decoder per byte, as the server does, so the screen never holds up the port.
- **Bounded memory.** One 1030-byte receive buffer and one 1028-byte response buffer, which is also the server's duplicate cache and the client's request buffer. The CRC-16 uses a 32-byte nibble table. Listings use one fixed 12 KB arena: the lines to print grow from the bottom, and the sorted entries of each directory being read from the top, so `ls -R` needs no heap and frees each directory's entries when it is done with it.
- **Portable core.** `frame.c` (framing and CRC) and `proto.c` (sessions, duplicates, requests) do no I/O. `tree.c` serves the file tree through a small filesystem layer: `dosfs.c` uses DOS find-first/find-next, which gives sizes and DOS-format times directly, and `posixfs.c` does the same on Linux. `dosport.c` is the DOS platform layer; `posixport.c` is a Linux tty/PTY layer, so the same code runs on the host for tests.
- **Stateless reads.** Each `FILE_READ` opens, seeks, reads and closes, and each `DIR_LIST` page rescans the directory up to its index, so nothing is left open between requests except a file being received.

| File | Purpose |
| --- | --- |
| `src/frame.c`, `frame.h` | v1 framing: CRC-16, escaping, incremental decoder |
| `src/proto.c`, `proto.h` | Sessions, the duplicate record and cache, `HELLO` / `PING` / `CLOSE` |
| `src/tree.c`, `tree.h` | The file tree: `STAT`, `DIR_LIST`, `FILE_READ`, and writing with `FILE_BEGIN` / `DATA` / `COMMIT` / `ABORT` |
| `src/crc32.c`, `crc32.h` | CRC-32 for `FILE_COMMIT` |
| `src/fsys.h` | Filesystem layer interface |
| `src/dosfs.c` | DOS directory scans, file times, 8.3 and device-name checks |
| `src/posixfs.c` | The same on Linux, for host testing |
| `src/port.h` | Platform layer interface |
| `src/dosport.c` | 8250-class UART by polling, BIOS timer ticks, Esc key |
| `src/posixport.c` | Linux tty or PTY, for host testing |
| `src/cli.c`, `cli.h` | `SPROCKET CLIENT`: the shell, local paths, `get` and `put` |
| `src/req.c`, `req.h` | The requester: `HELLO`, retransmission, timeouts, cancelling |
| `src/shell.c`, `shell.h` | The vocabulary and help, sizes, listings and their arena, wildcards, typed paths |
| `src/main.c` | Subcommands, and the server's main loop |
| `test/` | Host tests, the DOSBox-X smoke test and its fault-injecting relay, `BENCH.EXE`, and a pacing relay for 86Box |

## Building

Needs Open Watcom (2.0) with `WATCOM` pointing at it (default `~/opt/watcom`), GNU make, and a host C compiler for the tests.

```sh
make                 # build/SPROCKET.EXE: -bt=dos -ms -0 (8086, small model)
make bench           # build/BENCH.EXE: times the receive path on the machine it runs on
make test            # host tests against ../../tests/vectors/ and ../../tests/shell/
make host            # build/sprocket-client, the same program on a Linux tty/PTY
```

The build treats all warnings as errors and optimizes for speed (`-ox`) without per-call stack checks (`-s`; the only recursion is `ls -R`, bounded at 32 levels, and the stack is set to 8 KB). Under 86Box as a 4.77 MHz 8088, those two settings cut `BENCH`'s per-byte figure from about 590 to 330 µs. Those figures come from an earlier `BENCH` that also counted each frame's CRC check as per-byte time, so they overstate the receive path; they still show the relative gain. The executable is about 40 KB.

## Testing

- **`make test`** builds the C code with the host compiler and runs it against the same fixtures as the Rust host: every valid and malformed frame in `tests/vectors/`, plus the request/response pairs (`HELLO` → `HELLO_REPLY`, `PING` → its reply, an unknown type → the `ERROR` fixture), duplicate and refusal handling, sessions, and version mismatch. `test_tree` runs the file tree on a scratch directory: the v1.1 fixtures, case-insensitive and ambiguous lookups, refused paths, paged listings, and writing with its temporary and backup files. `test_shell` checks sizes, listings and the help against `tests/shell/`, which the Rust client is checked against too.
- **Host end to end:** run `build/sprocket-client SERVER PTY 38400 DIR /W` on one end of a `socat` PTY pair and `sprocket client` or `sprocket hello` on the other; or `build/sprocket-client CLIENT PTY 38400` against `sprocket serve`, or the two host builds against each other.
- **DOSBox-X:** `test/dosbox-hello.sh [baud]` runs the real `SPROCKET.EXE` under DOSBox-X as an 8086, with COM1 as a TCP null-modem, and runs `sprocket hello` against it (build the host first with `cargo build`). It then checks the client's counters: one frame per request, nothing dropped or repeated, no receive errors. `FAULT=1` inserts `test/fault-relay.py`, which corrupts one byte of the first `PING` and one of its response, and expects exactly one dropped frame and one repeated request answered from the cache. Both modes pass at 9600, 38400 and 115200 baud. `test/dosbox-tree.sh [baud]` runs `SPROCKET SERVER COM1 baud C:\SHARE /W` and drives it with `sprocket client`: a listing, downloads, uploads, a replacement that keeps `NAME.BAK`, and a name DOS cannot hold. `test/dosbox-client.sh [baud]` runs `SPROCKET CLIENT` with commands from a file against `sprocket serve --writable`, and checks the files and dates on both sides. DOSBox-X's CPU speed and UART timing are not representative, so these check function, not speed.
- **`BENCH.EXE`** (`make bench`) times the receive path, the frame check and the end of a frame on the machine it runs on, and prints the margin at each baud rate. Run it with nothing sending on the serial port; it reads the port's status register but sends nothing.
- **Real hardware:** on a NuXT (NEC V20) at 4.77 and 9.54 MHz, `sprocket hello --pings 20` ran clean at every baud rate `BENCH.EXE` marked as keeping up, and failed at the first rate it marked too slow. See [testing](../../docs/testing.md#real-hardware-nuxt-phaistos).
- **86Box, IBM XT (8088, 4.77 MHz):** end to end, `sprocket hello` works through `test/pace-relay.py`, which paces the host's bytes like a real line; see [testing](../../docs/testing.md#86box-ibm-xt-8088-at-477-mhz) for the setup and its limits.

## Licensing

The source is MIT. The executable links Open Watcom's C runtime; check its redistribution terms before distributing binaries (see [licensing](../../docs/licensing.md)).
