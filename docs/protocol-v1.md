# Native protocol v1 — proposal

**Status:** Proposal for review; **not ratified**. This document proposes concrete answers to the seven decisions listed in [protocol](protocol.md#before-implementation-decisions-to-ratify). Nothing here is frozen until it is accepted, and the requirements in `protocol.md` take precedence.

## Design premises

The first client is an 8088 at 4.77 MHz with a non-FIFO 8250-class UART at 9600 baud, running real-mode DOS. Later clients may be 6502 machines with a few kilobytes free. Every choice below is made for that end of the link; the Linux host does everything that can be pushed to it.

- **The host leads; the client answers.** Only the host sends requests. The client sends exactly one response frame per request (plus optional `PENDING` frames during slow operations, below). A client never speaks unprompted.
- **One request in flight (stop-and-wait).** While the client is reading a disk or writing a file, the host is waiting and the line is silent. A client can therefore poll its UART, or turn interrupts off around BIOS disk calls, without losing bytes. This one property removes most of the risk of non-FIFO UARTs.
- **Bounded memory.** The client advertises the largest payload it accepts. It needs one receive buffer and one response buffer of that size, and nothing else proportional to transfer size.
- **No host knowledge in the client.** Paths, device identities and error codes are reported in the client's own terms. The host translates them for the user.

## 1. Framing, integrity, frame size

### Framing: HDLC-style byte stuffing

```text
7E | escaped( type | session | seq | payload… | crc_lo | crc_hi ) | 7E
```

- `7E` delimits frames. Any number of consecutive `7E` bytes is allowed (empty frames are ignored), so a sender may send an extra `7E` before a frame to flush line noise.
- Inside a frame, `7E` and `7D` are sent as `7D` followed by the byte XOR `20`: `7E` → `7D 5E`, `7D` → `7D 5D`. No other byte is escaped.
- `7D` followed by `7E` aborts the current frame. `7D` followed by anything other than `5E` or `5D` makes the frame invalid.

**Why not COBS:** COBS has a fixed, lower worst-case overhead, but encoding needs to look ahead up to 254 bytes. Byte stuffing encodes and decodes one byte at a time with no lookahead, which is a few instructions on a 6502. Random data costs about 0.8% (2 of 256 values are escaped); the worst case doubles a frame, which only matters if a payload is mostly `7E`/`7D`.

**Why not escape XON/XOFF or control characters:** v1 is defined for a raw 8-bit link without software flow control. A later capability can add an extended escape set if a link needs it, without changing v1 framing.

### Header

| Offset | Field | Type | Meaning |
| --- | --- | --- | --- |
| 0 | `type` | u8 | Message type (section 4) |
| 1 | `session` | u8 | Session identifier chosen by the host in `HELLO`; never 0 |
| 2 | `seq` | u8 | Request sequence number; a response echoes its request's `seq` |
| 3 | `payload` | bytes | 0 to the negotiated maximum |
| end − 2 | `crc` | u16 LE | CRC over `type` through the last payload byte |

The payload length is the unescaped frame length minus 5. There is no length field: the delimiters already give the length, and the CRC rejects frames truncated or merged by noise. All multi-byte integers in v1 are little-endian, with no padding or alignment.

### CRC

**CRC-16/CCITT-FALSE:** polynomial `0x1021`, initial value `0xFFFF`, no reflection, no final XOR. Check value: `"123456789"` → `0x29B1`. It is computed over the unescaped header and payload, and sent little-endian.

CRC-16/CCITT detects every error with an odd number of flipped bits, every burst up to 16 bits, and every double-bit error for payloads up to 4090 bytes (the protocol maximum below). A byte-wise table costs 512 bytes; a nibble table costs 32 bytes, for clients short on memory. CRC-32 would be stronger but costs four-byte arithmetic on every byte on 8-bit CPUs; v1 instead uses CRC-32 end-to-end for files (section 5) and SHA-256 on the host for images.

### Frame size

- Every implementation must accept payloads of at least **128 bytes**.
- Each side states its maximum payload in `HELLO` / `HELLO_REPLY` (u16). Both directions then use the smaller of the two. Recommended: 1024 for the DOS client, 256 for small 8-bit clients.
- No payload may exceed **4090 bytes**, the largest for which CRC-16 still detects every double-bit error. The host should support that maximum.
- A frame longer than the receiver's maximum is discarded as soon as it overflows, without buffering the rest.

At 9600 8N1 a 1024-byte payload takes about 1.07 s on the wire; stop-and-wait overhead stays small relative to that.

## 2. Sequencing, retries, timeouts, reconnect

### Normal exchange

1. The host sends a request with `seq = n`.
2. The client validates the frame. If it is valid and for the current session, it executes the request and sends a response with the same `seq`.
3. The host accepts only a response whose `session` and `seq` match the outstanding request. It ignores anything else.
4. The next request uses `seq = n + 1` (mod 256).

### Errors on the link

- **Damaged or invalid frames are dropped silently.** Neither side sends a NAK; a damaged request leads to no response, and the host retransmits on timeout. This keeps the client free of error-path transmissions.
- **Retransmission:** after a timeout the host resends the identical request (same `session` and `seq`). After **8** retransmissions of one request it reports a link failure.
- **Duplicates:** the client remembers the `seq` of the last request it executed and keeps a copy of its last response. A request repeating that `seq` is not executed again; the cached response is resent. This makes every request safe to repeat, including file writes. The cache is the client's response buffer, so it costs no extra memory.

### Slow operations: `PENDING`

A disk read with BIOS retries can take several seconds. During an operation, the client may send `PENDING` (type `0xFE`, same `session` and `seq`, empty payload). The host restarts its response timer when it receives one and does not count it as a response. A client that polls its UART and cannot send during a BIOS call simply sends `PENDING` between its own retries.

### Timeouts

| Timer | Side | Value (proposed) |
| --- | --- | --- |
| Response | host | 0.5 s + 2 × wire time of (max request + max response) at the link rate; ≈ 4.8 s at 9600 with 1024-byte payloads |
| Response, storage reads | host | at least 10 s, restarted by each `PENDING` |
| Inter-byte | both | discard a partial frame after 1 s without a byte |
| Idle | client | none; the client waits for the host indefinitely and exits on a local key (e.g. Esc) or `CLOSE` |

### Sessions and reconnect

- The host starts a session by sending `HELLO` with a new, random, nonzero `session` and `seq = 0`, repeating it about once per second until a `HELLO_REPLY` arrives.
- A client accepts `HELLO` at any time. It abandons any operation in progress (for example, it deletes an incomplete temporary file and never activates it), adopts the new session, clears its duplicate cache, and replies.
- The client drops every non-`HELLO` request whose `session` does not match. A response from an old session therefore can never be mistaken for a current one, and a host that restarts mid-transfer simply begins again with `HELLO`.
- `CLOSE` ends the session; the client acknowledges it and may exit.
- Resuming an interrupted file transfer across sessions is **not** in v1 (see open questions).

## 3. `HELLO` and versioning

### `HELLO` (host → client, `0x01`)

| Field | Type | Value |
| --- | --- | --- |
| `magic` | 4 bytes | `"SPRK"` |
| `major` | u8 | 1 |
| `minor` | u8 | 0 |
| `max_payload` | u16 | host's maximum payload |

### `HELLO_REPLY` (client → host, `0x81`)

| Field | Type | Meaning |
| --- | --- | --- |
| `magic` | 4 bytes | `"SPRK"` |
| `major` | u8 | client's protocol major version |
| `minor` | u8 | client's protocol minor version |
| `max_payload` | u16 | client's maximum payload |
| `capabilities` | u32 | capability bits (section 4) |
| `platform` | u8 | 1 = DOS on x86; 2 = Apple II; others reserved. Descriptive only. |
| `version_len` | u8 | length of `version`, at most 32 |
| `version` | ASCII | client program version, e.g. `"0.1.0"` |

### Compatibility rules

- **Major** versions must match. A client that does not support the host's major replies with `ERROR` class `PROTOCOL_MISMATCH`, carrying its own major and minor in the error's platform-code field (major in the low byte, minor in the high byte). The host reports both versions to the user.
- **Minor** versions are backward compatible. Each side uses only features of the lower of the two minors. A minor version may add message types, capability bits, or fields **at the end** of a payload; receivers ignore trailing payload bytes they do not understand.
- `platform` and `version` are for display and diagnostics. Behavior is decided by `capabilities` only.

## 4. Message types and capabilities

### Numbering

- Requests use `0x01`–`0x7F`. The response to request `t` is `t | 0x80`.
- `0xFE` is `PENDING` and `0xFF` is `ERROR`; either may answer any request.
- Groups: `0x01`–`0x0F` session, `0x10`–`0x1F` files, `0x20`–`0x2F` storage, `0x30`–`0x7F` reserved.

| Type | Name | Capability | Request payload | Response payload |
| --- | --- | --- | --- | --- |
| `0x01` | `HELLO` | always | section 3 | `HELLO_REPLY` |
| `0x02` | `PING` | always | any bytes | the same bytes |
| `0x03` | `CLOSE` | always | empty | empty |
| `0x10` | `FILE_BEGIN` | `FILE_WRITE` | section 5 | empty |
| `0x11` | `FILE_DATA` | `FILE_WRITE` | `offset` u32, data | empty |
| `0x12` | `FILE_COMMIT` | `FILE_WRITE` | `size` u32, `crc32` u32 | `crc32` u32 as computed by the client |
| `0x13` | `FILE_ABORT` | `FILE_WRITE` | empty | empty |
| `0x20` | `STORAGE_LIST` | `STORAGE_LIST` | empty | section 6 |
| `0x21` | `STORAGE_READ` | `STORAGE_READ_UNITS` or `STORAGE_READ_CHS` | section 6 | section 6 |

`HELLO`, `PING` and `CLOSE` are mandatory and are the whole of Milestone 2.

### Capability bits (`u32`)

| Bit | Name | Meaning |
| --- | --- | --- |
| 0 | `FILE_WRITE` | `FILE_BEGIN` / `DATA` / `COMMIT` / `ABORT` |
| 1 | `FILE_REPLACE` | `FILE_BEGIN` may replace an existing file, keeping a backup |
| 2 | `STORAGE_LIST` | `STORAGE_LIST` |
| 3 | `STORAGE_READ_UNITS` | `STORAGE_READ` with linear unit addressing |
| 4 | `STORAGE_READ_CHS` | `STORAGE_READ` with cylinder/head/sector addressing |
| 5–31 | reserved | Must be sent as 0; ignored when received |

A bitmap, rather than a TLV list, is proposed because it is one fixed-size field that an 8-bit client fills from a constant. Per-capability parameters are reported by the operations themselves (for example `STORAGE_LIST`), not in `HELLO_REPLY`. File and storage features are independent, as `protocol.md` requires.

### `ERROR` (`0xFF`)

| Field | Type | Meaning |
| --- | --- | --- |
| `class` | u8 | see below |
| `platform_code` | u16 | client-specific detail, e.g. DOS extended error or BIOS `AH`; 0 if none |
| `message` | ASCII | optional, the rest of the payload |

| Class | Name | Use |
| --- | --- | --- |
| 1 | `UNSUPPORTED` | Unknown type, or a capability the client did not advertise |
| 2 | `INVALID_REQUEST` | Malformed payload, out-of-range value, wrong state (e.g. `FILE_DATA` with no file begun) |
| 3 | `PROTOCOL_MISMATCH` | `HELLO` with an unsupported major version |
| 4 | `MEDIA_ERROR` | The source medium failed (a whole-request failure; per-unit failures are reported in the read response) |
| 5 | `FILE_ERROR` | Filesystem failure: not found, access denied, disk full, invalid name |
| 6 | `VERIFY_FAILED` | `FILE_COMMIT` size or CRC-32 mismatch; nothing was activated |

A serial timeout, a CRC failure on the link, and a host output failure never appear here: they are detected and reported by the host itself. This keeps link errors, media errors, and host errors distinct, as `protocol.md` requires.

## 5. File transfer and exact length

### Paths

Paths are **client-native ASCII strings**, at most 127 bytes, passed through unchanged. For DOS that means `C:\SPROCKET\SPROCKET.EXE`. The host does not interpret or construct them: the user names the destination, and the client validates it (`FILE_ERROR` for an invalid name). This avoids inventing a cross-platform path syntax before a second client exists, while keeping DOS drive letters out of the protocol itself.

### `FILE_BEGIN` (`0x10`)

| Field | Type | Meaning |
| --- | --- | --- |
| `size` | u32 | exact file length |
| `flags` | u8 | bit 0: replace an existing file (requires `FILE_REPLACE`); other bits reserved |
| `path_len` | u8 | length of `path` |
| `path` | ASCII | destination |

### Transfer

1. `FILE_BEGIN`: the client checks the path and that it can create a **temporary file in the destination directory** (DOS: same name with extension `$$$`). It does not touch the destination yet.
2. `FILE_DATA` frames carry consecutive data starting at offset 0. The client writes them to the temporary file and keeps a running CRC-32 (IEEE 802.3, check value `0xCBF43926`). An `offset` other than the next expected one gets `INVALID_REQUEST` (a retransmitted duplicate is answered from the cache, so it never reaches this check).
3. `FILE_COMMIT` carries the exact size and the host's CRC-32. The client checks both against what it wrote:
   - On a mismatch: it deletes the temporary file and answers `VERIFY_FAILED`.
   - On a match: if the destination exists and the replace flag is set, it renames the old file to a backup (DOS: extension `BAK`, replacing an older backup), then renames the temporary file into place. It answers with its CRC-32.
4. `FILE_ABORT`, a new `HELLO`, or a local exit deletes the temporary file. The destination is never left partly written.

This meets the upgrade rules in [bootstrap](bootstrap.md#native-deployment-and-upgrades): receive to a separate file, verify length and checksum before activating, keep the old version, and never overwrite the running executable (a running `SPROCKET.EXE` updates itself by receiving a new copy under a temporary name, and is replaced by the user or a loader after it exits).

## 6. Storage descriptors and read results

### `STORAGE_LIST` response

A count (u8), then one descriptor per device:

| Field | Type | Meaning |
| --- | --- | --- |
| `device` | u8 | Session-local identifier, used in `STORAGE_READ` |
| `kind` | u8 | 1 = removable, 2 = fixed, 0 = unknown |
| `addressing` | u8 | bit 0: linear units; bit 1: CHS |
| `unit_size` | u16 | bytes per unit or sector; **not assumed to be 512** |
| `unit_count` | u32 | number of addressable units (linear) |
| `cylinders` | u16 | CHS geometry; zero if not CHS |
| `heads` | u8 | |
| `sectors` | u8 | sectors per track |
| `first_sector` | u8 | number of the first sector (1 for PC BIOS) |
| `meta_len` | u8 | length of `meta` |
| `meta` | bytes | platform metadata as TLV (`tag` u8, `len` u8, value). DOS: tag 1 = BIOS drive number. |
| `label_len` | u8 | length of `label` |
| `label` | ASCII | human-readable description, e.g. `"BIOS 00 (A:) 1.44M"` |

### `STORAGE_READ` request

| Field | Type | Meaning |
| --- | --- | --- |
| `device` | u8 | from `STORAGE_LIST` |
| `mode` | u8 | 1 = linear, 2 = CHS |
| `address` | 4 bytes | linear: unit index u32; CHS: cylinder u16, head u8, sector u8 |
| `count` | u8 | units to read; the response must fit the payload limit |
| `retries` | u8 | client-side retry budget per unit |

CHS reads must stay within one track. The host does the planning, so the client never splits or reorders requests.

### `STORAGE_READ` response

```text
count u8 | status[count] (class u8, platform_code u16 each) | data of the units with class 0, in order
```

- Class 0 means the unit was read; its data follows.
- Class 4 (`MEDIA_ERROR`) means it could not be read; `platform_code` is the BIOS status, and **no bytes are sent for it**. The client never fills unreadable units with zeros; the host records them as missing in the image's error map.
- A whole-request failure (for example, no disk in the drive) is an `ERROR` frame instead.

Reads are idempotent, so retransmissions never cause duplicate writes on the host. The host tracks offsets by request, not by arrival.

## 7. Test vectors and malformed-frame fixtures

### Proposed fixture format

Fixtures live in `tests/vectors/`, as plain text that a C test harness can parse without a JSON library:

```text
# name: one line per field; hex bytes separated by spaces
name   hello
body   01 5a 00 53 50 52 4b 01 00 00 04
crc    a7fe
wire   7e 01 5a 00 53 50 52 4b 01 00 00 04 fe a7 7e
```

The same files drive the Rust tests and a host-compiled build of the DOS client's C parser, so both implementations are tested against one source.

### Valid frames (computed for this proposal)

| Name | Wire bytes |
| --- | --- |
| `HELLO`: session `5A`, max payload 1024 | `7e 01 5a 00 53 50 52 4b 01 00 00 04 fe a7 7e` |
| `HELLO_REPLY`: max 512, capabilities `FILE_WRITE`+`FILE_REPLACE`, DOS, `"0.1.0"` | `7e 81 5a 00 53 50 52 4b 01 00 00 02 03 00 00 00 01 05 30 2e 31 2e 30 c8 42 7e` |
| `PING` `seq` 1, payload `7e 7d 00 ff` (both escapes) | `7e 02 5a 01 7d 5e 7d 5d 00 ff 6c a9 7e` |
| `PING_REPLY` to it | `7e 82 5a 01 7d 5e 7d 5d 00 ff 95 02 7e` |
| `ERROR` `UNSUPPORTED`, `seq` 2 | `7e ff 5a 02 01 00 00 6b d3 7e` |

CRC-16 check value: `"123456789"` → `0x29B1`. CRC-32 check value: `0xCBF43926`.

### Malformed and edge cases (expected result)

| Case | Expected |
| --- | --- |
| One payload bit flipped | dropped (CRC) |
| Frame cut short by `7E` | dropped (CRC or too short) |
| Fewer than 5 unescaped bytes | dropped |
| `7D 7E` inside a frame | frame aborted; the next frame parses normally |
| `7D` followed by a byte other than `5E` / `5D` | dropped |
| Payload over the receiver's maximum | dropped without buffering the rest |
| Leading garbage, then a valid frame | garbage ignored; frame accepted |
| Repeated `7E 7E 7E` | ignored |
| Valid frame, wrong `session` (not `HELLO`) | dropped by the client |
| Duplicate `seq` | cached response resent; not executed again |
| Unknown request type | `ERROR` `UNSUPPORTED` |
| Response with stale `seq` | ignored by the host |

## Memory budget (DOS client, 1024-byte payloads)

| Item | Bytes |
| --- | --- |
| Receive buffer (header + payload + CRC) | 1029 |
| Response buffer, also the duplicate cache | 1029 |
| CRC-16 table (byte-wise) | 512 |
| CRC-32 table (byte-wise) | 1024 |
| Path and state | < 256 |
| **Total** | **< 4 KB** |

A 256-byte-payload client with nibble tables needs well under 1 KB.

## Open questions

1. **Faster links:** a `SET_BAUD` request (switch both sides after the response, with fallback to 9600 if the next `PING` fails). Proposed for a later minor version.
2. **Resume:** resuming a file transfer across sessions needs the client to keep the temporary file and report its length and CRC. Deferred until there is a need.
3. **Client → host files** (`FILE_READ`) and directory listing: not needed by the milestones; reserved numbers `0x18`–`0x1F`.
4. **Track-level or format-specific reads** for non-PC media (Apple II nibble data, sector skew): reserved addressing bits, to be designed with the second client.
5. **Windowing:** stop-and-wait costs about one round trip per frame. If measured throughput is too low at higher baud rates, a later minor version could allow a small window for clients that buffer by interrupt.
6. **Path length:** 127 bytes covers DOS paths with room to spare; confirm for future platforms.
