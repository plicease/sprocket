# SPROCKET DOS client

`SPROCKET.EXE`, the native-protocol client for real-mode DOS on 8086/8088 and later CPUs, built with Open Watcom. It implements [protocol v1](../../docs/protocol-v1.md) for Milestone 2: `HELLO`, `PING` and `CLOSE`, with the duplicate-request rules. File transfer and disk reads come in later milestones.

```text
SPROCKET [port [baud]]        default: COM1 9600
```

It waits for the host (`sprocket hello` on Linux), answers until the host sends `CLOSE`, and exits. Press Esc to exit early. On exit it prints how many frames it received and dropped, how many requests were repeats, and how many UART receive errors (overruns or framing errors) it saw.

## Design

- **Polled UART, no interrupts, no FIFO.** The UART's interrupts are left off and its FIFO, if any, is disabled. That works because the host sends one request at a time and waits: the line is silent whenever the client is busy. The port's base address comes from the BIOS data area, so any COM1 to COM4 the BIOS found will work.
- **No screen output between sending a response and reading the next request.** Scrolling the screen on an 8088 is slow enough to overrun a UART without a FIFO, and the host sends its next request as soon as it has a response. The client prints before it sends.
- **Fast enough per byte.** With a polled UART and no FIFO, the client must finish with each byte before the next one arrives: 1041 µs at 9600 baud. On a 4.77 MHz 8088, the receive path (poll the UART, feed the decoder) takes about 330 µs per byte, measured by `BENCH.EXE` under 86Box. That's 3.1× headroom at 9600 and 1.5× at 19200; 38400 is too fast for that CPU. Nothing else runs per byte: the timer and keyboard are checked only while the line is idle. The end of a 1 KB frame (CRC check, response) takes about 330 ms, while the host is waiting.
- **Bounded memory.** One 1030-byte receive buffer and one 1028-byte response buffer, which is also the duplicate cache. The CRC-16 uses a 32-byte nibble table.
- **Portable core.** `frame.c` (framing and CRC) and `proto.c` (sessions, duplicates, requests) do no I/O. `dosport.c` is the DOS platform layer; `posixport.c` is a Linux tty/PTY layer, so the same core runs on the host for tests.

| File | Purpose |
| --- | --- |
| `src/frame.c`, `frame.h` | v1 framing: CRC-16, escaping, incremental decoder |
| `src/proto.c`, `proto.h` | Sessions, the duplicate record and cache, `HELLO` / `PING` / `CLOSE` |
| `src/port.h` | Platform layer interface |
| `src/dosport.c` | 8250-class UART by polling, BIOS timer ticks, Esc key |
| `src/posixport.c` | Linux tty or PTY, for host testing |
| `src/main.c` | The main loop |
| `test/` | Host tests, the DOSBox-X smoke test, `BENCH.EXE`, and a pacing relay for 86Box |

## Building

Needs Open Watcom (2.0) with `WATCOM` pointing at it (default `~/opt/watcom`), GNU make, and a host C compiler for the tests.

```sh
make                 # build/SPROCKET.EXE: -bt=dos -ms -0 (8086, small model)
make bench           # build/BENCH.EXE: times the receive path on the machine it runs on
make test            # host tests against ../../tests/vectors/
make host            # build/sprocket-client, the same client on a Linux tty/PTY
```

The build treats all warnings as errors and optimizes for speed (`-ox`) without per-call stack checks (`-s`; the code has no recursion). On a 4.77 MHz 8088 those two settings cut the receive path from about 590 to 390 µs per byte. The executable is about 13 KB.

## Testing

- **`make test`** builds the C core with the host compiler and runs it against the same fixtures as the Rust host: every valid and malformed frame in `tests/vectors/`, plus the request/response pairs (`HELLO` → `HELLO_REPLY`, `PING` → its reply, an unknown type → the `ERROR` fixture), duplicate and refusal handling, sessions, and version mismatch.
- **Host end to end:** run `build/sprocket-client` on one end of a `socat` PTY pair and `sprocket hello` on the other.
- **DOSBox-X:** `test/dosbox-hello.sh [baud]` runs the real `SPROCKET.EXE` under DOSBox-X as an 8086, with COM1 as a TCP null-modem, and runs `sprocket hello` against it (build the host first with `cargo build`). It passes at 9600, 38400 and 115200 baud. DOSBox-X's CPU speed and UART timing are not representative, so this checks function, not speed.
- **86Box, IBM XT (8088, 4.77 MHz):** `BENCH.EXE` gives the timing above. End to end, `sprocket hello` works through `test/pace-relay.py`, which paces the host's bytes like a real line; see [testing](../../docs/testing.md#86box-ibm-xt-8088-at-477-mhz) for the setup and its limits.

## Licensing

The source is MIT. The executable links Open Watcom's C runtime; check its redistribution terms before distributing binaries (see [licensing](../../docs/licensing.md)).
