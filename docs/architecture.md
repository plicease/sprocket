# Architecture

**Status:** Draft.

## Purpose and scope

SPROCKET provides (1) serial file transfer, including to systems without a SPROCKET client; (2) an interactive terminal file server ("smart terminal" mode) that any terminal program can browse; (3) installation and updating of vintage clients; and (4) preservation-oriented logical disk imaging. These are complementary, independently useful features.

## Requirements

- Modern host initially runs on Linux, written in Rust; initial deployment target is `twintwist`, built on `athena`.
- Initial vintage client targets real-mode MS-DOS on 8086/8088-compatible processors, built with Open Watcom. NEC V20 in the NuXT is explicitly in scope; no 386 instructions or DOS extenders.
- Future clients can target Apple II, classic Macintosh, and other platforms without changing the conceptual model.
- `client/dos/` contains the DOS implementation; other clients are sibling directories under `client/`.
- Versioned, documented, language-independent binary protocol; simple enough for constrained 8-bit implementations.
- Host handles heavyweight tasks: user interface, output files, metadata, verification, retries, and recovery orchestration. Client code should remain small.
- File-transfer and bootstrap functionality is a permanent product feature, not merely a workaround for the first test machine.
- All original project code, clients, and documentation are MIT licensed; see [licensing](licensing.md).

## Components

```text
Rust host CLI
  ├── transports: serial port / TCP (emulator) / in-memory (tests)
  ├── standard-protocol senders (XMODEM, XMODEM-1K, ZMODEM)
  ├── terminal file server (`sprocket serve`)
  │     ├── plain-ASCII menu and line editing
  │     └── root-confined, read-only file tree
  ├── native SPROCKET protocol engine (`native`)
  │     ├── framing and CRC-16 (`native::frame`)
  │     ├── stop-and-wait requests and retransmission (`native::link`)
  │     ├── session negotiation and capabilities (`native::session`, `sprocket hello`)
  │     ├── file transfer and client deployment
  │     └── storage discovery and imaging
  └── local image writer, verification and metadata
                 │
           serial link
                 │
      platform-specific client
       ├── serial hardware adapter
       ├── local filesystem adapter
       └── disk access adapter
```

Legacy protocol transfers and the terminal file server work with third-party terminal programs and do not require a native client.

## Terminal file server

`sprocket serve` turns the serial line into a small, keyboard-driven download menu. Its layers are kept separate so each can be reused or tested alone:

| Layer | Module | Responsibility |
| --- | --- | --- |
| Transport | `transport` | Byte stream with read timeouts; serial, TCP, or in-memory. Knows nothing about menus or protocols. |
| Menu | `serve` | Banner, listing, paging, line editing, protocol choice, result messages. Output is plain ASCII with CR LF line endings and no ANSI sequences, and fits 80×24. |
| File tree | `files` | Lists and reads files beneath the configured root only. |
| Transfer | `xmodem`, `zmodem` | Send one file over a `Transport`; report progress and typed errors. Shared with `sprocket send`. |

The menu hands the transport to the selected protocol for the duration of a transfer, then discards leftover protocol bytes before redrawing. A transfer failure (cancel, timeout, no receiver) is reported on the terminal and in the host log, and the menu continues. Only transport I/O errors end the session. The CLI then reopens the port, which covers emulator restarts and dropped TCP links.

**Filesystem confinement.** The root is canonicalized at startup. Every requested path must consist only of normal components (no `..`, no absolute paths), and its fully resolved form, after following symlinks, must still lie inside the root. Symlinks within the root are allowed; ones that lead out are hidden from listings and refused if requested. Hidden files, non-UTF-8 names, and special files are not listed. The server is download-only and never writes on the host. The check and the read are not atomic, so the root is assumed to be under the operator's control.

**ZMODEM** uses the [`zmodem2`](https://crates.io/crates/zmodem2) crate (MIT OR Apache-2.0), a caller-driven state machine with no I/O of its own. SPROCKET's `zmodem` module supplies the transport loop, timeouts, ZRQINIT repetition, and detection of a receiver's raw CAN cancel sequence. Data subpackets use CRC-32.

## Interfaces and isolation

The native wire protocol must not expose C structure layout, Rust serialization, DOS drive letters as universal identifiers, BIOS interrupt semantics, or a fixed logical sector size. Platform-specific client code translates local hardware capabilities into documented protocol operations. A client advertises supported operations; unsupported requests fail explicitly.

The host's serial I/O should be separated from packet processing so protocol tests can run against in-memory or pseudo-terminal transports. Platform-specific bootstrap artifacts and instructions live with their client.

## Initial test environment

- Host: Ubuntu on `twintwist`, physical COM1 `/dev/ttyS0`, 16550A UART.
- DOS test: IBM ThinkPad 755CX `smokey`, physical COM1, Windows 95 HyperTerminal and MS-DOS mode.
- Link: null-modem cable, verified bidirectional at 9600 8N1, no flow control.
- Compatibility target: NuXT Turbo XT (NEC V20), including non-FIFO 8250-class UART behavior.

## Non-goals for initial milestones

- Flux-level imaging or preservation of copy-protection timing/analog information.
- Write/restore operations to vintage disks.
- A generic plugin framework for hypothetical clients.
- Mandatory network connectivity on vintage machines.
- Support for every historical serial transfer protocol at launch.

## Open questions

- Whether the host CLI should use `sprocket send`, `sprocket serve`, `sprocket image`, and `sprocket install` exactly as sketched.
- DOS minimum version (3.3 is a provisional target).
- Whether future hosts on macOS/Windows are supported immediately or after Linux stabilization.

## Emulator-backed development

DOSBox-X is the primary rapid-iteration environment for DOS executable, protocol, and image-transfer testing. 86Box provides a second tier with configurable period PC hardware, including 8088-class CPU and UART configurations. Both run on the Linux development machine `athena`; neither replaces final tests on `smokey`, the NuXT, or other real hardware. See [testing](testing.md).

The Rust host's byte-stream transport abstraction should support physical serial ports and a TCP-backed test transport. TCP is a **host-side development adapter**, not a requirement of vintage clients or a change to the native wire protocol.
