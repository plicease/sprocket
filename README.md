# SPROCKET — Serial Protocol Rocket

SPROCKET is a cross-platform serial file-transfer, deployment, and disk-imaging project for vintage computers. A modern host performs complex operations; small platform-specific clients provide access to vintage hardware.

**Status (2026-10-21):** in development. The standard-protocol modes (`sprocket send`, `sprocket serve`) are implemented and validated on real hardware. Native protocol v1 is ratified; its Milestone 2 handshake is implemented on the host and in the DOS client and tested in emulators, but not yet on real hardware. Native file transfer and disk imaging are designed but not implemented. See [validation status](#validation-status) and the [roadmap](docs/roadmap.md).

## Design documents

- [Architecture](docs/architecture.md): scope, components, platform independence, milestones.
- [Protocol](docs/protocol.md): requirements for the native protocol.
- [Protocol v1](docs/protocol-v1.md): the ratified native wire specification: framing, sequencing, messages and test vectors.
- [Bootstrap and transfers](docs/bootstrap.md): standard protocols, smart terminal mode, first installation, upgrades, recovery.
- [Disk imaging](docs/imaging.md): addressing, integrity, error reporting, safety.
- [Licensing](docs/licensing.md): MIT project policy and third-party obligations.
- [Roadmap](docs/roadmap.md): staged implementation and acceptance criteria.
- [Testing and emulation](docs/testing.md): DOSBox-X, 86Box, CI, and real-hardware validation.

## Ways to move files

SPROCKET offers three complementary modes. None replaces another; each needs less from the vintage machine than the next.

| Mode | Command | Vintage machine needs | Exact length and filename |
| --- | --- | --- | --- |
| Bootstrap send | `sprocket send` | Any terminal program with an XMODEM (or ZMODEM) receiver | XMODEM: no (padded). ZMODEM: yes |
| Smart terminal | `sprocket serve` | Any terminal program; ZMODEM optional | ZMODEM: yes. XMODEM: no |
| Native SPROCKET | `sprocket hello` (more planned) | The SPROCKET client (`SPROCKET.EXE`) | Yes, plus disk imaging |

All modes use the serial line as 8N1 with no flow control, at the `--baud` given (default 9600). `--port` takes a serial device such as `/dev/ttyS0`, or `tcp://HOST:PORT` for an emulator's null-modem link. A TCP link is a raw byte stream, so configure the emulator's null-modem port to be transparent (no telnet or handshake escaping). Transfers are built in; no `sz`, `sx`, or other external program is used. `send`, `serve` and `hello` accept `--trace FILE`, which appends a timestamped hex/ASCII log of every byte in each direction, plus host log messages, for diagnosing transfer problems.

### Bootstrap send

Push one file to a receiver that the user has already started, for example HyperTerminal's *Receive File* on a machine with no SPROCKET software:

```sh
sprocket send --protocol xmodem    --port /dev/ttyS0 --baud 9600 SPROCKET.EXE
sprocket send --protocol xmodem-1k --port /dev/ttyS0 --baud 9600 SPROCKET.EXE
sprocket send --protocol zmodem    --port /dev/ttyS0 --baud 9600 SPROCKET.EXE
```

XMODEM is the lowest common denominator and remains the first-install path. It pads the final block with `0x1A`, so the host reports the exact original length for truncation and checking. See [bootstrap](docs/bootstrap.md#xmodem-exact-length-limitation).

### Smart terminal (file server)

Serve a read-only directory, browsed with ftp-style commands, to whatever terminal program the vintage machine runs:

```sh
sprocket serve --port /dev/ttyS0 --baud 38400 --root ~/vintage/files
```

On the vintage machine, connect at the same settings and press ENTER. Then, at the `/> ` prompt:

| Command | Does |
|---|---|
| `ls [-R] [PATH]` | One entry per line, alphabetical: name and size (`512B`, `1.5kB`, `12MB`, in powers of 1024). Directories end in `/` and show no size. `-R` lists everything below, with paths relative to the listed directory. Ctrl-C stops a long listing. |
| `cd [DIR]` | Change directory; `..` goes up, a leading `/` starts at the top, and no `DIR` goes to the top. |
| `get FILE` | Download a file by the current protocol. The last part of `FILE` may use `*` and `?` (case-insensitive), e.g. `get *.exe`, to send every match in turn. |
| `protocol [NAME]` | Show or set the download protocol for the session: `zmodem` (`z`, the default), `xmodem` (`x`) or `xmodem-1k` (`x-1k`). |
| `pwd`, `help`, `quit` | Show the current directory, list commands, end the session (`bye` and `exit` also work). |

Tab completes command names, protocol names and file names (only directories after `cd`). If several names match, a second Tab is not needed: the choices are listed and the line is redrawn. The protocols:

- **ZMODEM** (default): terminals with ZMODEM auto-download, such as Telix or Procomm Plus, start receiving by themselves. With other ZMODEM-capable terminals, start a ZMODEM download manually. ZMODEM preserves the filename and exact length.
- **XMODEM / XMODEM-1K**: for terminals without ZMODEM; set it with `protocol x` or `protocol x-1k`, then start the XMODEM receive manually after each `get`.

The prompt returns after each transfer, cancellation (Ctrl-X), or error. When `get` matched several files, a failed or cancelled transfer stops the rest (a ZMODEM skip moves on to the next file). With XMODEM, start a receive for each file as it is announced. Only files under `--root` are reachable: `..` and symlinks that lead outside it are refused, and hidden files (names starting with `.`, or symlinks to them) are never listed, completed or sent. Nothing on the host is ever written. A session ends on `quit`, or after `--idle` seconds (default 600) without input. `sprocket serve` keeps running and reopens the port if it fails or a TCP link drops.

Windows 95 HyperTerminal auto-downloads ZMODEM (verified on real hardware), but only while its *Transfer → Receive File* protocol is set to Zmodem. After an XMODEM receive, that setting stays on Xmodem, and HyperTerminal declines the next ZMODEM auto-download (reported as "receiver skipped the file"). Set the protocol back to Zmodem first, or just retry. For other terminals, do not assume ZMODEM or auto-download support. XMODEM is always offered.

### Native SPROCKET

The native protocol is [protocol v1](docs/protocol-v1.md), ratified on 2026-10-21. The Milestone 2 handshake is implemented on both sides: the host's `sprocket hello`, and the DOS client [`SPROCKET.EXE`](client/dos/README.md) (Open Watcom, 8086, polled UART). File transfer and disk imaging are not implemented yet. See the [roadmap](docs/roadmap.md).

```sh
sprocket hello --port /dev/ttyS0 --baud 9600
```

`hello` calls the client (repeating `HELLO` for up to `--wait` seconds, default 60), then shows its version, platform, maximum payload and capabilities. It sends `--pings` full-size pings (default 3) that carry every byte value, so frame escaping is exercised in both directions, and then closes the session. On the vintage machine, run `SPROCKET COM1 9600` (port and baud as needed). Validation so far is listed below; it has not yet run on real hardware.

### Validation status

Emulator results check function and, under 86Box, CPU timing; they do not replace real-hardware validation, which is listed separately.

**`sprocket send` and `sprocket serve`.** Tested with in-memory receivers (`cargo test`), and over a Linux PTY pair against lrzsz `rz`/`rx`.

Real hardware, 2026-10-20: `sprocket serve` on `twintwist` (`/dev/ttyS0`) to Windows 95 HyperTerminal on `smokey`, over a null-modem cable at 38400 8N1, no flow control. The ZMODEM download started automatically, with matching MD5 sums on both sides. XMODEM and XMODEM-1K downloads through the menu, started manually in HyperTerminal, also matched by MD5 once truncated to the original length. The menu returned after every transfer.

`sprocket send --protocol xmodem` to HyperTerminal on `smokey` (38400 8N1) delivered a 43,296-byte file. It matched by MD5 once truncated to the original length, which can be done with DEBUG on DOS/Windows. Cancelling in HyperTerminal mid-transfer stopped the sender cleanly with "transfer cancelled by receiver".

Not yet validated on real hardware: cancellation through the `serve` menu or during ZMODEM, and `sprocket send` with XMODEM-1K or ZMODEM. These modes have not yet been tested in emulators or with DOS terminal programs (Telix, Procomm Plus).

**Native protocol (`sprocket hello` and `SPROCKET.EXE`).**

- Host and client tests: the Rust host against a mock client (`cargo test`), and the DOS client's C core against the shared `tests/vectors/` fixtures (`make test` in `client/dos`).
- The C client built for Linux, over a PTY pair.
- Emulators, 2026-10-21: `SPROCKET.EXE` under DOSBox-X (8086) completed the handshake and 1 KB pings at 9600, 38400 and 115200 baud with no dropped frames or receive errors (`client/dos/test/dosbox-hello.sh`). With one byte corrupted in each direction, the client dropped the damaged request and answered the host's retransmitted request from its duplicate cache. Under 86Box as an IBM XT (8088 at 4.77 MHz) the handshake and 1 KB pings also completed, and the client's receive path measured about 330 µs per byte, a 3× margin at 9600 baud. 86Box's serial console delivers bytes in bursts, so back-to-back 9600 baud is not yet shown.
- Real hardware: not yet run.

See [testing](docs/testing.md) for details.

## Repository layout

```text
sprocket/
├── Cargo.toml          # Rust host crate (the `sprocket` command)
├── src/                # host: transports, XMODEM/ZMODEM senders, terminal file server,
│   └── native/         #   trace; native protocol v1 framing, link and session layers
├── client/
│   └── dos/            # SPROCKET.EXE: Open Watcom, 8086-compatible DOS client, and its tests
├── docs/               # design documents and the protocol v1 specification
└── tests/
    └── vectors/        # protocol v1 frame fixtures shared by the Rust and C tests
```

Other clients will be sibling directories under `client/`.

## Design status vocabulary

- **Requirement**: an agreed project goal.
- **Proposal**: an implementation choice requiring review.
- **Open question**: intentionally undecided.

See the documents for details. Native protocol v1 was ratified on 2026-10-21. Its Milestone 2 subset (`HELLO`, `PING`, `CLOSE`) is implemented in the host and the DOS client; file transfer and disk imaging are not implemented yet.
