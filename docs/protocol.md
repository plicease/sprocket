# Native wire protocol

**Status:** Requirements for the native protocol. The wire specification that meets them is [protocol v1](protocol-v1.md), **ratified on 2026-10-21**; implement against that document. Where this document describes a design choice in general terms, protocol v1 is authoritative.

## Requirements

- Binary, compact, deterministic, independently implementable in Rust, 8086 C, or 6502 assembly.
- Explicit version negotiation and capability discovery; optional operations must be discoverable.
- Framing robust against arbitrary binary payloads, noise, truncated frames, and lost synchronization.
- Detect corruption, lost/duplicated packets, timeouts, and interrupted sessions; bounded retransmissions and clear errors.
- Explicit maximum payload negotiated or conservatively fixed; no unbounded allocation on the client.
- Fixed-width integers and specified endianness; no compiler-dependent structure padding.
- Correct at 9600 8N1 with no flow control on non-FIFO UARTs. Higher baud rates and hardware flow control are optional.
- No assumption of 512-byte sectors, DOS drive letters, filesystem availability, or seekable storage.
- Read-only storage access in the initial protocol feature set.

## Proposed layers

1. **Link**: framing, integrity, sequence IDs, ACK/NACK, retry and resynchronization.
2. **Session**: hello/version, capability negotiation, errors, cancellation.
3. **Operations**: file send/receive, storage enumeration, storage reads, status.

The standard-protocol senders (XMODEM, XMODEM-1K, ZMODEM) and the interactive terminal file server (`sprocket serve`) are **separate host facilities**, not part of this protocol. They talk to ordinary terminal programs and share only the host's transport layer with the native engine. Nothing in their menus or ZMODEM framing is a native message type, and they impose no requirements on native framing.

## Wire format

The frame layout, CRC, sequencing, messages and test vectors are defined by the ratified [protocol v1](protocol-v1.md). An earlier illustrative layout in this document (with version, flags and length fields) was superseded by it and has been removed.

## Session flow

```text
host                    client
  |---- HELLO ------------>|
  |<--- HELLO_REPLY -------|  version, platform, capabilities, limits
  |---- REQUEST ---------->|
  |<--- RESPONSE / DATA ---|
  |---- ACK / NEXT ------->|
  |---- CLOSE ------------>|
```

Either side must be able to time out, abort safely, and restart without confusing stale responses with new transactions. Client identity/platform information is descriptive, not a substitute for capabilities.

## Capability model

Independent advertised capabilities (v1 defines them as a bitmap in `HELLO_REPLY`), for example:

- File receive / file send / safe replacement support.
- Enumerate logical or physical storage devices.
- Read fixed-size addressable units and/or variable-length track records.
- Report media errors and geometry/format descriptors.
- Resume at an acknowledged offset (where supported).

Avoid requiring a client to implement file transfer merely to implement disk reads, or vice versa.

## Storage model

A storage device has an opaque session-local identifier and an advertised addressing mode. A logical block device may expose a unit size (not necessarily 512), count, and supported read ranges. Other devices may expose tracks/records or format-specific descriptors. Host commands must validate bounds. A DOS BIOS drive number can be reported as platform-specific metadata, but is not a universal device ID.

## Reliability and integrity

Per-frame integrity and ACK/retry protect the serial hop. End-to-end file/image hashes computed by the host, plus per-range read outcomes, protect the resulting artifact. Retransmission of a response must not cause duplicate writes or ambiguous offsets. Initial imaging requests are idempotent reads.

Define distinct error classes for unsupported operation, invalid request, serial timeout, protocol mismatch, source-media read failure, and host output failure. A disk read failure is not a corrupted serial packet.

## Security assumptions

Initial use is a directly attached, operator-controlled serial cable. No authentication or encryption is promised. The host must not silently execute received data or perform destructive operations. File writes must use explicit destination selection and safe replacement semantics.

## Decisions ratified in v1

All seven are decided in [protocol v1](protocol-v1.md), ratified on 2026-10-21 after review in PR #1.

1. Framing and byte escaping, CRC parameters, maximum packet size.
2. Sequence/ACK/retry behavior, timeout and reconnect semantics.
3. Exact HELLO fields and version compatibility rules.
4. Stable command numbers and capability bitmap/TLV scheme.
5. File path representation and exact-length transfer semantics.
6. Storage addressing descriptors and bad-sector result encoding.
7. Golden test vectors and malformed-frame fixtures.
