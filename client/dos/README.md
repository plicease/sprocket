# SPROCKET DOS client

`SPROCKET.EXE`, the native-protocol client for real-mode DOS on 8086/8088 and later CPUs, built with Open Watcom. It implements [protocol v1](../../docs/protocol-v1.md) for Milestone 2: `HELLO`, `PING` and `CLOSE`, with the duplicate-request rules. File transfer and disk reads come in later milestones.

```text
SPROCKET [port [baud]]        default: COM1 9600
```

It waits for the host (`sprocket hello` on Linux), answers until the host sends `CLOSE`, and exits. Press Esc to exit early. On exit it prints how many frames it received and dropped, how many requests were repeats, and how many UART receive errors (overruns or framing errors) it saw.

## Design

- **Polled UART, no interrupts, no FIFO.** The UART's interrupts are left off and its FIFO, if any, is disabled. That works because the host sends one request at a time and waits: the line is silent whenever the client is busy. The port's base address comes from the BIOS data area, so any COM1 to COM4 the BIOS found will work.
- **No screen output between sending a response and reading the next request.** Scrolling the screen on an 8088 is slow enough to overrun a UART without a FIFO, and the host sends its next request as soon as it has a response. The client prints before it sends.
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
| `test/` | Host tests and the DOSBox-X smoke test |

## Building

Needs Open Watcom (2.0) with `WATCOM` pointing at it (default `~/opt/watcom`), GNU make, and a host C compiler for the tests.

```sh
make                 # build/SPROCKET.EXE: -bt=dos -ms -0 (8086, small model)
make test            # host tests against ../../tests/vectors/
make host            # build/sprocket-client, the same client on a Linux tty/PTY
```

The build treats all warnings as errors. The executable is about 13 KB.

## Testing

- **`make test`** builds the C core with the host compiler and runs it against the same fixtures as the Rust host: every valid and malformed frame in `tests/vectors/`, plus the request/response pairs (`HELLO` → `HELLO_REPLY`, `PING` → its reply, an unknown type → the `ERROR` fixture), duplicate and refusal handling, sessions, and version mismatch.
- **Host end to end:** run `build/sprocket-client` on one end of a `socat` PTY pair and `sprocket hello` on the other.
- **DOSBox-X:** `test/dosbox-hello.sh [baud]` runs the real `SPROCKET.EXE` under DOSBox-X as an 8086, with COM1 as a TCP null-modem, and runs `sprocket hello` against it (build the host first with `cargo build`). It passes at 9600, 38400 and 115200 baud. DOSBox-X does not model UART overruns faithfully, so 86Box and real hardware are still needed to show that the polling loop keeps up.

## Licensing

The source is MIT. The executable links Open Watcom's C runtime; check its redistribution terms before distributing binaries (see [licensing](../../docs/licensing.md)).
