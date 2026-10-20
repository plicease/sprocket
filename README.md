# SPROCKET — Serial Protocol Rocket

SPROCKET is a cross-platform serial file-transfer, deployment, and disk-imaging project for vintage computers. A modern host performs complex operations; small platform-specific clients provide access to vintage hardware.

**Status:** Initial design draft (2026-10-20). This document set is intended for review, not an implemented protocol specification.

## Design documents

- [Architecture](docs/architecture.md): scope, components, platform independence, milestones.
- [Protocol](docs/protocol.md): wire-level requirements and proposed packet design.
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
| Native SPROCKET | *planned* | The SPROCKET client (`SPROCKET.EXE`) | Yes, plus disk imaging |

All modes use the serial line as 8N1 with no flow control, at the `--baud` given (default 9600). `--port` takes a serial device such as `/dev/ttyS0`, or `tcp://HOST:PORT` for an emulator's null-modem link. A TCP link is a raw byte stream, so configure the emulator's null-modem port to be transparent (no telnet or handshake escaping). Transfers are built in; no `sz`, `sx`, or other external program is used.

### Bootstrap send

Push one file to a receiver that the user has already started, for example HyperTerminal's *Receive File* on a machine with no SPROCKET software:

```sh
sprocket send --protocol xmodem    --port /dev/ttyS0 --baud 9600 SPROCKET.EXE
sprocket send --protocol xmodem-1k --port /dev/ttyS0 --baud 9600 SPROCKET.EXE
sprocket send --protocol zmodem    --port /dev/ttyS0 --baud 9600 SPROCKET.EXE
```

XMODEM is the lowest common denominator and remains the first-install path. It pads the final block with `0x1A`, so the host reports the exact original length for truncation and checking. See [bootstrap](docs/bootstrap.md#xmodem-exact-length-limitation).

### Smart terminal (file server)

Serve a read-only directory as a plain-ASCII menu to whatever terminal program the vintage machine runs:

```sh
sprocket serve --port /dev/ttyS0 --baud 38400 --root ~/vintage/files
```

On the vintage machine, connect at the same settings and press ENTER. Pick a file by number, then a protocol:

- **ZMODEM** (default): terminals with ZMODEM auto-download, such as Telix or Procomm Plus, start receiving by themselves. With other ZMODEM-capable terminals, start a ZMODEM download manually. ZMODEM preserves the filename and exact length.
- **XMODEM / XMODEM-1K**: for terminals without ZMODEM; start the XMODEM receive manually after choosing it.

The menu returns after each transfer, cancellation (Ctrl-X), or error. Only files under `--root` are reachable: `..` and symlinks that lead outside it are refused, and hidden files are not listed. Nothing on the host is ever written. A session ends on `Q`, or after `--idle` seconds (default 600) without input. `sprocket serve` keeps running and reopens the port if it fails or a TCP link drops.

Do not assume a given terminal supports ZMODEM or auto-download (for example, Windows 95 HyperTerminal is unverified). XMODEM is always offered.

### Native SPROCKET

The native protocol and DOS client are designed but not yet implemented; see [protocol](docs/protocol.md) and the [roadmap](docs/roadmap.md).

### Validation status

Tested with in-memory receivers (`cargo test`), and over a Linux PTY pair against lrzsz `rz`/`rx`. Not yet validated on `smokey`, in emulators, or with period terminal programs. See [testing](docs/testing.md).

## Intended repository layout

```text
sprocket/
├── Cargo.toml                 # Existing Rust template / workspace as appropriate
├── host/                      # Rust host crate
├── client/
│   └── dos/                   # Open Watcom, 8086-compatible DOS client
├── docs/
└── tests/                     # Integration fixtures, as needed
```

The Rust project template is maintained separately; these documents do not prescribe changes to its existing files or license headers.

## Design status vocabulary

- **Requirement**: an agreed project goal.
- **Proposal**: an implementation choice requiring review.
- **Open question**: intentionally undecided.

See the documents for details. No protocol version has been ratified yet.
