# Architecture

**Status:** Draft.

## Purpose and scope

SPROCKET provides (1) serial file transfer, including to systems without a SPROCKET client; (2) installation and updating of vintage clients; and (3) preservation-oriented logical disk imaging. These are complementary, independently useful features.

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
  ├── serial port configuration / I/O
  ├── standard-protocol sender (XMODEM, XMODEM-1K initially)
  ├── native SPROCKET protocol engine
  │     ├── session negotiation and capabilities
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

Legacy protocol transfers work with third-party terminal programs and do not require a native client.

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

- Whether the host CLI should use `sprocket send`, `sprocket image`, and `sprocket install` exactly as sketched.
- DOS minimum version (3.3 is a provisional target).
- Whether future hosts on macOS/Windows are supported immediately or after Linux stabilization.
