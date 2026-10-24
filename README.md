# SPROCKET — Serial Protocol Rocket

SPROCKET is a cross-platform serial file-transfer, deployment, and disk-imaging project for vintage computers. A modern host performs complex operations; small platform-specific clients provide access to vintage hardware.

**Status (2026-10-23):** in development. The standard-protocol modes (`sprocket send`, `sprocket serve`) are implemented and validated on real hardware. Native protocol v1 is ratified; its Milestone 2 handshake is implemented on the host and in the DOS program, tested in emulators, and validated on a NuXT (NEC V20) at up to 57600 baud. Native file transfer (protocol v1.1, ratified) is implemented in both directions and either role on Linux and DOS, and tested on Linux, in DOSBox-X, and on a NuXT at 57600 baud. Disk imaging (Milestone 4) is implemented on the host and in the DOS program and tested in DOSBox-X, but not yet on real hardware. See [validation status](#validation-status) and the [roadmap](docs/roadmap.md).

## Design documents

- [Architecture](docs/architecture.md): scope, components, platform independence, milestones.
- [Protocol](docs/protocol.md): requirements for the native protocol.
- [Protocol v1](docs/protocol-v1.md): the ratified native wire specification: framing, sequencing, messages and test vectors.
- [Protocol v1.1](docs/protocol-v1.1.md): ratified: file trees (`STAT`, `DIR_LIST`, `FILE_READ`) and either side in either role.
- [File-transfer UI](docs/sprocket-file-transfer-ui.md): the shell shared by terminal mode and the native clients.
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
| Smart terminal | `sprocket serve --mode terminal` | Any terminal program; ZMODEM optional | ZMODEM: yes. XMODEM: no |
| Native SPROCKET | `sprocket client`, `sprocket serve`, `sprocket hello` | `SPROCKET.EXE` | Yes, with dates; also disk imaging |

All modes use the serial line as 8N1 with no flow control, at the `--baud` given (default `SPROCKET_BAUD` from the environment if set, else 9600). `--port` takes a serial device (default `/dev/ttyS0` on Linux, `COM1` on Windows), or `tcp://HOST:PORT` for an emulator's null-modem link. A TCP link is a raw byte stream, so configure the emulator's null-modem port to be transparent (no telnet or handshake escaping). Transfers are built in; no `sz`, `sx`, or other external program is used. `send`, `serve`, `client` and `hello` accept `--trace FILE`, which appends a timestamped hex/ASCII log of every byte in each direction, plus host log messages, for diagnosing transfer problems.

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
sprocket serve --mode terminal --port /dev/ttyS0 --baud 38400 --root ~/vintage/files
```

**Changed 2026-10-23:** `sprocket serve` now defaults to native mode, for the SPROCKET client. Terminal programs need `--mode terminal`.

On the vintage machine, connect at the same settings and press ENTER. Then, at the `/> ` prompt:

| Command | Does |
|---|---|
| `ls [-R] [PATH]`, `dir` | One entry per line, alphabetical: name and size (`512B`, `1.5kB`, `12MB`, in powers of 1024). Directories end in `/` and show no size. `-R` lists everything below, with paths relative to the listed directory. Ctrl-C stops a long listing. |
| `cd [DIR]` | Change directory; `..` goes up, a leading `/` starts at the top, and no `DIR` goes to the top. |
| `get FILE` | Download a file by the current protocol. The last part of `FILE` may use `*` and `?` (case-insensitive), e.g. `get *.exe`, to send every match in turn. |
| `protocol [NAME]` | Show or set the download protocol for the session: `zmodem` (`z`, the default), `xmodem` (`x`) or `xmodem-1k` (`x-1k`). |
| `set editing on\|off` | Turn line editing (below) on or off for the rest of the session. |
| `pwd`, `help`, `quit` | Show the current directory, list commands, end the session (`bye`, `exit`, or Ctrl-D on an empty line also work). |

The native client's commands (`put`, `lls`, `ldir`, `lcd`, `lpwd`) are recognized too, and answer, for example, `put: not available in terminal mode`; `help` lists them as unavailable. A command neither mode knows gets `Unknown command: NAME`.

Names are matched ignoring case, as on DOS: `get UNZIP.EXE` finds `unzip.exe`, and `cd GAMES` shows the directory's real name in the prompt. If a directory holds names that differ only in case, the exact spelling picks one, and any other spelling is refused as ambiguous. Tab completes command names, protocol names and file names (only directories after `cd`). If several names match, a second Tab is not needed: the choices are listed and the line is redrawn. Up and Down recall the session's earlier commands; on terminals whose arrow keys send nothing, use Ctrl-P and Ctrl-N. Tab completion, history and Ctrl-D logout make up *line editing*. If a terminal sends keystrokes that confuse it, `set editing off` (or `--editing=off` on the host, which sets the starting mode for every session) leaves only typing, Backspace and Enter; all other control keys, including ESC, are ignored. The protocols:

- **ZMODEM** (default): terminals with ZMODEM auto-download, such as Telix or Procomm Plus, start receiving by themselves. With other ZMODEM-capable terminals, start a ZMODEM download manually. ZMODEM preserves the filename and exact length.
- **XMODEM / XMODEM-1K**: for terminals without ZMODEM; set it with `protocol x` or `protocol x-1k`, then start the XMODEM receive manually after each `get`.

The prompt returns after each transfer, cancellation (Ctrl-X), or error. When `get` matched several files, a failed or cancelled transfer stops the rest (a ZMODEM skip moves on to the next file). With XMODEM, start a receive for each file as it is announced. Only files under `--root` are reachable: `..` and symlinks that lead outside it are refused, and hidden files (names starting with `.`, or symlinks to them) are never listed, completed or sent. Nothing on the host is ever written. A session ends on `quit`, or after `--idle` seconds (default 600) without input. `sprocket serve` keeps running and reopens the port if it fails or a TCP link drops.

Windows 95 HyperTerminal auto-downloads ZMODEM (verified on real hardware), but only while its *Transfer → Receive File* protocol is set to Zmodem. After an XMODEM receive, that setting stays on Xmodem, and HyperTerminal declines the next ZMODEM auto-download (reported as "receiver skipped the file"). Set the protocol back to Zmodem first, or just retry. For other terminals, do not assume ZMODEM or auto-download support. XMODEM is always offered.

### Native SPROCKET

The native protocol is [protocol v1](docs/protocol-v1.md), ratified on 2026-10-21, with the file trees of [protocol v1.1](docs/protocol-v1.1.md), ratified on 2026-10-23. One side is the *client*, which runs an interactive shell and makes requests; the other is the *server*, which shares a directory. Either machine can take either role, and files go both ways in one session: `get` copies from the server, `put` copies to it. The DOS side is [`SPROCKET.EXE`](client/dos/README.md) (Open Watcom, 8086, polled UART), one program with both roles. A DOS server can also offer its disks for [imaging](#disk-imaging).

```text
Linux server: sprocket serve --port /dev/ttyS0 --baud 38400 --root ~/dos [--writable]
DOS client:   SPROCKET CLIENT COM1 38400

DOS server:   SPROCKET SERVER COM1 38400 C:\SHARE [/W]
Linux client: sprocket client --port /dev/ttyS0 --baud 38400
```

The client's prompt, listings and messages are those of terminal mode, with local commands added: `lls` (or `ldir`), `lcd` and `lpwd` work on the client's own directory, `get [-f] FILE` copies remote files into it and `put [-f] FILE` sends local files to the server's current directory. Both take `*` and `?` wildcards, refuse to replace an existing file unless given `-f`, and stop a batch at the first file that fails. Files are written under a temporary name and renamed into place once complete and checked (with a CRC-32 for `put`), and keep their modification time. A server shares its directory read-only unless started with `--writable` (`/W` on DOS); a DOS server replacing a file keeps the old one as `NAME.BAK`, and refuses names that are not 8.3. The Linux client has Tab completion, history and line editing, as in terminal mode; the DOS client has plain line input (Backspace, Esc to clear the line, Ctrl-D to end), and can read commands from a file (`SPROCKET CLIENT COM1 38400 < CMDS.TXT`). Esc or Ctrl-C stops a long listing or a transfer.

```sh
sprocket hello --port /dev/ttyS0 --baud 9600
```

`hello` calls a server (repeating `HELLO` for up to `--wait` seconds, default 60), then shows its version, platform, maximum payload and capabilities. It sends `--pings` full-size pings (default 3) that carry every byte value, so frame escaping is exercised in both directions, and then closes the session. On the vintage machine, run `SPROCKET SERVER COM1 9600` (port and baud as needed). Validation so far is listed below. On a slow machine, run `BENCH.EXE` first: it shows the fastest baud rate SPROCKET can keep up with there.

### Disk imaging

```text
DOS server:   SPROCKET SERVER COM1 38400 /D
Linux:        sprocket image --port /dev/ttyS0 --baud 38400 --list
              sprocket image --port /dev/ttyS0 --baud 38400 --device 0 --output floppy.img
```

`SPROCKET SERVER` offers its BIOS disks only when started with `/D`, and can serve a directory at the same time. `--list` shows each disk with its BIOS drive number, size and geometry, and the drive's own geometry. `--device N --output NAME.img` images one into `NAME.img`, and writes its progress map `NAME.img.map` and manifest `NAME.img.json` beside it ([imaging](docs/imaging.md)). Imaging is read-only. Before starting, it asks for confirmation, showing the geometry it will read by and how long the read will take at least; `--yes` skips the question. A floppy's geometry is probed from the inserted disk and can be wrong, so the question proposes it and also takes a format name or C/H/S as the answer, to choose another. `--format` gives a conventional IBM PC floppy format: `360` (40/2/9), `720` (80/2/9), `1200` (80/2/15) or `1440` (80/2/18), also written `360k`, `720k`, `1.2m` and `1.44m`. These are the usual formats, which a given disk need not follow. `--geometry C/H/S` (such as `80/2/18`) gives any geometry. Either must fit the drive's geometry, and the two cannot be combined. With `--yes`, a floppy needs one of them, so that a script never images by a guess. To resume an interrupted image, run the same command again: it checks that the disk is the same one, then reads only the units not yet captured. A unit that still fails after the server's retries (`--retries`, default 3) is zero-filled, recorded as failed, and makes the image incomplete. `--recover N` then makes up to N passes that reread failed units, suspect units (read with a BIOS warning, such as ECC-corrected) and conflicting units. When a reread returns different data, every version is kept in `NAME.img.reads`. For testing, `sprocket serve --disk FILE` offers an image file as a disk.

### Validation status

Emulator results check function and, under 86Box, CPU timing; they do not replace real-hardware validation, which is listed separately.

**`sprocket send` and `sprocket serve`.** Tested with in-memory receivers (`cargo test`), and over a Linux PTY pair against lrzsz `rz`/`rx`.

Real hardware, 2026-10-20: `sprocket serve` on `twintwist` (`/dev/ttyS0`) to Windows 95 HyperTerminal on `smokey`, over a null-modem cable at 38400 8N1, no flow control. The ZMODEM download started automatically, with matching MD5 sums on both sides. XMODEM and XMODEM-1K downloads through the menu, started manually in HyperTerminal, also matched by MD5 once truncated to the original length. The menu returned after every transfer.

`sprocket send --protocol xmodem` to HyperTerminal on `smokey` (38400 8N1) delivered a 43,296-byte file. It matched by MD5 once truncated to the original length, which can be done with DEBUG on DOS/Windows. Cancelling in HyperTerminal mid-transfer stopped the sender cleanly with "transfer cancelled by receiver".

Not yet validated on real hardware: cancellation through the `serve` menu or during ZMODEM, and `sprocket send` with XMODEM-1K or ZMODEM. These modes have not yet been tested in emulators or with DOS terminal programs (Telix, Procomm Plus).

**Native protocol (`sprocket hello` and `SPROCKET.EXE`).**

- Host and client tests: the Rust host against a mock client (`cargo test`), and the DOS program's C code against the shared `tests/vectors/` and `tests/shell/` fixtures (`make test` in `client/dos`).
- The C program built for Linux, over a PTY pair.
- File transfer, 2026-10-23: every pairing of the Rust and C clients and servers over a Linux PTY pair, and `SPROCKET.EXE` under DOSBox-X (8086) as server against `sprocket client` (`client/dos/test/dosbox-tree.sh`) and as client against `sprocket serve` (`client/dos/test/dosbox-client.sh`), at 38400 baud: listings, wildcard downloads, uploads, replacing with and without `-f`, dates, and names DOS cannot hold. On real hardware, 2026-10-23 (version 0.3.0): `twintwist` and `phaistos` (NuXT at 9.54 MHz), each as client and as server, at 57600 baud: `get` and `put` of files up to 153,744 bytes in both directions, and replacing with `-f`, including `SPROCKET.EXE` on the running DOS server. An interrupted replacement left the old file intact.
- Disk imaging, 2026-10-23: `sprocket image` against `sprocket serve --disk` and against the C program's Linux build over a PTY pair, byte-for-byte; a 20 MB image killed partway and run again finished identical, and a build with a 4090-byte payload carried seven units per request. `SPROCKET.EXE` under DOSBox-X imaged BIOS-attached 160K floppy and 3 MB hard disk images byte-for-byte (`client/dos/test/dosbox-image.sh`). Not yet on real hardware.
- Emulators, 2026-10-21: `SPROCKET.EXE` under DOSBox-X (8086) completed the handshake and 1 KB pings at 9600, 38400 and 115200 baud with no dropped frames or receive errors (`client/dos/test/dosbox-hello.sh`). With one byte corrupted in each direction, the client dropped the damaged request and answered the host's retransmitted request from its duplicate cache. Under 86Box as an IBM XT (8088 at 4.77 MHz) the handshake and 1 KB pings also completed. 86Box's serial console delivers bytes in bursts, so it could not show sustained back-to-back bytes.
- Real hardware, 2026-10-23: `sprocket hello --pings 20` on `twintwist` (`/dev/ttyS0`) to `SPROCKET.EXE` on `phaistos`, a NuXT with an NEC V20, over a null-modem cable. At 4.77 MHz it ran clean at 9600, 19200 and 38400 baud and failed at 57600; at 9.54 MHz it ran clean up to 57600 and failed at 115200. Each clean run ended with `22 frames, 0 dropped, 0 repeated, 0 receive errors` and took the same time for every ping. `BENCH.EXE` predicted exactly these limits: 187 µs per byte at 4.77 MHz and 93 µs at 9.54 MHz. On 2026-10-22, `smokey` (P75) passed the handshake and three 1 KB pings at 9600, 38400 and 115200 baud, at 0.20 s per ping at 115200.

See [testing](docs/testing.md) for details.

## Repository layout

```text
sprocket/
├── Cargo.toml          # Rust host crate (the `sprocket` command)
├── src/                # host: transports, XMODEM/ZMODEM senders, terminal file server,
│   ├── native/         #   trace, native client shell; native framing, link, session, server
│   └── imaging/        #   disk imaging: capture, progress map, manifest, reread log
├── client/
│   └── dos/            # SPROCKET.EXE: Open Watcom, 8086-compatible DOS program, and its tests
├── docs/               # design documents and the protocol v1 specification
└── tests/
    ├── vectors/        # protocol frame fixtures shared by the Rust and C tests
    └── shell/          # shell output fixtures (sizes, listings, help) shared the same way
```

Other clients will be sibling directories under `client/`.

## Design status vocabulary

- **Requirement**: an agreed project goal.
- **Proposal**: an implementation choice requiring review.
- **Open question**: intentionally undecided.

See the documents for details. Native protocol v1 was ratified on 2026-10-21. Its Milestone 2 subset (`HELLO`, `PING`, `CLOSE`) and the v1.1 file trees, ratified on 2026-10-23, are implemented on Linux and DOS; disk imaging is not implemented yet.
