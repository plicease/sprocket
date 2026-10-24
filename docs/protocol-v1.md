# Native protocol v1 — ratified

**Status:** Ratified on 2026-10-21 following review in [PR #1](https://github.com/plicease/sprocket/pull/1). This document defines the v1 wire format and behavior for implementation. Changes to ratified wire semantics require explicit versioning under [section 3](#3-hello-and-versioning). The higher-level requirements in `protocol.md` remain applicable.

**Minor version 1** (directory trees, reading files, and either machine as requester) is [protocol v1.1](protocol-v1.1.md), ratified on 2026-10-23.

**Revised after the first review:** duplicate handling now distinguishes requests in progress from completed ones and checks type and payload ([section 2](#duplicate-requests)); an interrupted `FILE_COMMIT` has a stated guarantee and recovery procedure ([section 5](#what-an-interrupted-commit-leaves-behind)); the session ID is 16 bits with stated limits on its uniqueness; host timeouts are configurable, and exhausting retries while the client is busy has defined results ([section 2](#exhausted-retries-while-the-client-is-busy)); rereads of marginal media are allowed ([section 6](#rereads)); and minor-version extensions follow explicit rules ([section 3](#extension-rules)).

## Design premises

The first client is an 8088 at 4.77 MHz with a non-FIFO 8250-class UART at 9600 baud, running real-mode DOS. Later clients may be 6502 machines with a few kilobytes free. Every choice below is made for that end of the link; the Linux host does everything that can be pushed to it.

- **The host leads; the client answers.** Only the host sends requests. The client sends exactly one response frame per request (plus optional `PENDING` frames during slow operations, below). A client never speaks unprompted.
- **One request in flight (stop-and-wait).** While the client is reading a disk or writing a file, the host is waiting and the line is silent. A client can therefore poll its UART, or turn interrupts off around BIOS disk calls, without losing bytes. This one property removes most of the risk of non-FIFO UARTs.
- **Bounded memory.** The client advertises the largest payload it accepts. It needs one receive buffer and one response buffer of that size, and nothing else proportional to transfer size.
- **No host knowledge in the client.** Paths, device identities and error codes are reported in the client's own terms. The host translates them for the user.

## 1. Framing, integrity, frame size

### Framing: HDLC-style byte stuffing

```text
7E | escaped( type | session_lo | session_hi | seq | payload… | crc_lo | crc_hi ) | 7E
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
| 1 | `session` | u16 LE | Session identifier chosen by the host in `HELLO`; never 0 |
| 3 | `seq` | u8 | Request sequence number; a response echoes its request's `seq` |
| 4 | `payload` | bytes | 0 to the negotiated maximum |
| end − 2 | `crc` | u16 LE | CRC over `type` through the last payload byte |

The payload length is the unescaped frame length minus 6. There is no length field: the delimiters already give the length, and the CRC rejects frames truncated or merged by noise. All multi-byte integers in v1 are little-endian, with no padding or alignment.

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

With one request in flight, an 8-bit `seq` cannot be confused with an outstanding one: the host never has more than one unanswered `seq`, and a stale response carries the previous `seq`.

### Errors on the link

- **Damaged or invalid frames are dropped silently.** Neither side sends a NAK; a damaged request leads to no response, and the host retransmits on timeout. This keeps the client free of error-path transmissions.
- **Retransmission:** after a timeout the host resends the identical request (same `session`, `seq`, type and payload). After **8** retransmissions of one request it reports a link failure (see [exhausted retries](#exhausted-retries-while-the-client-is-busy) for what that does and does not mean).

### Duplicate requests

The client keeps one record of the **last request it accepted**: its `seq`, `type`, payload length and frame CRC (6 bytes), plus a state, `in progress` or `complete`. The record is set when execution **starts**, before any side effect.

A request is a **duplicate** when its `session` is current and its `seq`, `type`, length and frame CRC all equal the record. Comparing the frame CRC stands in for comparing the payload.

**Accepted v1 tradeoff:** two *different* requests with the same `seq`, type, length and CRC-16 would be treated as identical, and the second would get the first one's cached response without being executed. With a correct host this cannot happen, because a new request always takes a new `seq`. It needs a host bug that reuses a `seq` with a different payload, and then a CRC-16 collision (about 1 in 65,536 for a random change of the same length). Storing the whole request, or a stronger hash of it, would close the gap, but would cost a second payload-sized buffer or CRC-32 work per frame on the smallest clients. v1 accepts the risk to keep the client's memory and code small.

| Arriving request | Record state | Client action |
| --- | --- | --- |
| Duplicate | `in progress` | Do **not** restart or re-execute. Send `PENDING` if able, otherwise ignore it. |
| Duplicate | `complete` | Do not re-execute. Resend the cached response unchanged. |
| Same `seq`, different type, length or CRC | either | Protocol violation: do not execute. Answer `ERROR` `INVALID_REQUEST`; leave the record and cache unchanged. |
| Different `seq` | `in progress` | Cannot happen with a correct host (one request in flight). Drop it. |
| Different `seq` | `complete` | A new request: execute it. |

The cached response is the client's response buffer, so the cache costs no extra memory. This makes every request safe to repeat, including file writes: a retransmitted `FILE_DATA` or `FILE_COMMIT` never writes or renames twice.

A single-tasking client usually is not reading the UART while it executes (see [design premises](#design-premises)), so most retransmissions sent during a long operation arrive damaged or not at all and are dropped. The table covers the rest: a client that polls between BIOS calls may see a whole duplicate mid-operation.

### Slow operations: `PENDING`

A disk read with BIOS retries can take several seconds. During an operation, the client may send `PENDING` (type `0xFE`, same `session` and `seq`, empty payload). The host restarts its response timer when it receives one and does not count it as a response or as a retransmission.

`PENDING` is optional. A client often **cannot** send it: a single BIOS call (INT 13h on a failing floppy, or a hard disk recalibrating) can block for seconds with nothing else running. A client sends `PENDING` between its own BIOS calls where it can; the host's timeouts must not depend on receiving it.

### Timeouts

All host timeouts are configurable on the command line; the values below are defaults.

| Timer | Side | Default |
| --- | --- | --- |
| Response | host | 0.5 s + 2 × wire time of (max request + max response) at the link rate; ≈ 4.8 s at 9600 with 1024-byte payloads |
| Response, storage reads | host | 30 s, restarted by each `PENDING`. Generous because a whole `STORAGE_READ` may run inside BIOS calls that cannot be interrupted. |
| Inter-byte | both | discard a partial frame after 1 s without a byte |
| Idle | client | none; the client waits for the host indefinitely and exits on a local key (e.g. Esc) or `CLOSE` |

A client should keep the worst case of one request bounded, so that it fits within the host's storage timeout: the request's `retries` field (section 6) caps BIOS retries per unit, and the host chooses `count`. On a range that keeps timing out, the host retries with fewer units and fewer retries per request.

### Exhausted retries while the client is busy

When the host gives up on a request, it knows only that **no matching response arrived**. The client may still be executing, may have finished and lost its response on the line, or may be gone. The host therefore:

- Records the outcome as **unknown**, never as a failure of the operation. For storage reads, the requested units stay *missing* in the image's progress map, not *failed*; only a `STORAGE_READ` response can mark a unit as failed.
- Does not send a different request in the same session, because the client may still be busy with the old one and would drop it (table above).
- Recovers by starting a **new session** with `HELLO`, repeated about once per second until it gets a reply or the operator stops it. When the client finishes, it reads the `HELLO`, discards its stale result, and replies. Its stale response, if it was sent, carries the old session and is ignored.
- For `FILE_COMMIT`, reports that the outcome is unknown and how to recover (section 5).

### Sessions and reconnect

- The host starts a session by sending `HELLO` with `seq = 0` and a new random, nonzero 16-bit `session` that differs from the last session it used on this port (if it knows it). It repeats the `HELLO` about once per second until a `HELLO_REPLY` arrives.
- Before the first `HELLO`, the host discards any pending output from an earlier run and drains input until the line has been quiet for 1 s, so that stale bytes do not reach the new session. While it waits for `HELLO_REPLY`, it ignores every frame that is not a `HELLO_REPLY` with the new `session` and `seq = 0`.
- A client accepts `HELLO` at any time:
  - A `HELLO` that is a **duplicate** (above) of the `HELLO` that opened the current session is answered from the cache. The host's once-per-second repeats therefore never reset a session that has just started.
  - Any other `HELLO`, including one with the current `session` after later requests, starts a new session. The client abandons any operation in progress (for example, it deletes an incomplete temporary file and never activates it), adopts the new session, clears its duplicate record, discards any partial frame, and replies.
- The client drops every non-`HELLO` request whose `session` does not match.
- **Collisions:** session IDs are not unique forever. A stale frame is misattributed only if it carries the current `session` **and** the expected `seq`, type and CRC; with a random 16-bit ID, that needs a 1-in-65,535 collision across a host restart while old frames are still in flight, which the drain step above makes unlikely in practice. A collision on `HELLO` itself is harmless: the client either starts a new session (as above) or resends a reply for an identical `HELLO`.
- `CLOSE` ends the session; the client acknowledges it and may exit.
- Resuming an interrupted **file transfer** across sessions is **not** in v1 (see open questions). Resuming **disk imaging** needs no protocol support: storage reads are idempotent, and the host keeps the progress map ([imaging](imaging.md#resumable-imaging)).

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
- **Minor** versions are backward compatible. Each side uses only features of the lower of the two minors, except where a capability bit says otherwise.
- `platform` and `version` are for display and diagnostics. Behavior is decided by `capabilities` only.

### Extension rules

A minor version may add only the following, and each addition must be safe for a peer that does not understand it:

1. **New message types.** An old client answers them with `ERROR` `UNSUPPORTED`. The host sends them only when the client's minor version or a capability bit says they are supported.
2. **New capability bits.** Reserved bits are sent as 0 and ignored when received.
3. **Trailing fields** in an *extensible* payload (table below). A receiver parses the fields it knows and ignores any bytes after them. A trailing field may only add information; it **must not change the meaning of any earlier field or of the message**, so a receiver that ignores it still acts correctly. A sender includes a trailing field only when the peer's minor version defines it, except in `HELLO` and `HELLO_REPLY`, which are sent before the minor version is known; their trailing fields must always be safe to ignore.
4. **New values of reserved flag bits and enumerations** in requests, used only when the client's minor version or a capability bit says they are supported. A client that receives a request with a reserved flag bit set, or an unknown enumeration value, answers `INVALID_REQUEST` and does nothing. It never ignores such a bit, because a flag can change what a request does (the replace flag in `FILE_BEGIN` is an example).

Anything else, including changing the meaning, size or order of an existing field, needs a new major version, or a new message type or capability bit that the host uses only when the client advertises it.

| Payload | Extensible at the end? |
| --- | --- |
| `HELLO`, `HELLO_REPLY`, `CLOSE` and its response, `PENDING` | yes |
| `ERROR` | yes (the message is length-prefixed) |
| `FILE_BEGIN` and its response, `FILE_COMMIT` and its response, `FILE_ABORT` and its response | yes |
| `STORAGE_LIST` response, and each descriptor in it (descriptors are length-prefixed) | yes |
| `STORAGE_READ` request | yes |
| `PING` and its response, `FILE_DATA`, `STORAGE_READ` response | **no**: their last field runs to the end of the payload. They change only through a new message type or capability. |

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
| `0x10` | `FILE_BEGIN` | `FILE_WRITE` | section 5 | section 5 |
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
| `message_len` | u8 | length of `message`; 0 if none |
| `message` | ASCII | optional human-readable detail |

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

### `FILE_BEGIN` response

| Field | Type | Meaning |
| --- | --- | --- |
| `found` | u8 | what the client found before starting; bit 0: the destination exists; bit 1: a backup exists; bit 2: a leftover temporary file was deleted |

The host uses `found` to tell the user about earlier interrupted transfers (below). It changes nothing in the transfer.

### Transfer

The examples use DOS names: destination `NAME.EXT`, temporary file `NAME.$$$`, backup `NAME.BAK`, all in the destination directory.

1. `FILE_BEGIN`: the client checks the path. If the destination exists and the replace flag is clear, it answers `FILE_ERROR`. If `NAME.$$$` exists, it is left over from an interrupted transfer and is deleted: a temporary file is never activated without the verification in step 3, and v1 does not resume. The client then creates an empty `NAME.$$$`. It does not touch the destination or the backup.
2. `FILE_DATA` frames carry consecutive data starting at offset 0. The client writes them to the temporary file and keeps a running CRC-32 (IEEE 802.3, check value `0xCBF43926`). An `offset` other than the next expected one gets `INVALID_REQUEST` (a retransmitted duplicate is answered from the cache, so it never reaches this check).
3. `FILE_COMMIT` carries the exact size and the host's CRC-32. The client closes the temporary file, which flushes DOS's buffers to disk, and checks the size and CRC against what it wrote. On a mismatch it deletes the temporary file and answers `VERIFY_FAILED`.
4. On a match, the client activates the file in this order:
   1. If the destination exists: delete `NAME.BAK` if present, then rename `NAME.EXT` to `NAME.BAK`.
   2. Rename `NAME.$$$` to `NAME.EXT`.
   3. Answer with its CRC-32.
5. `FILE_ABORT`, a new `HELLO`, or a local exit before step 4 deletes the temporary file.

This meets the upgrade rules in [bootstrap](bootstrap.md#native-deployment-and-upgrades): receive to a separate file, verify length and checksum before activating, keep the old version, and never overwrite the running executable (a running `SPROCKET.EXE` updates itself by receiving a new copy under a temporary name, and is replaced by the user or a loader after it exits).

### What an interrupted commit leaves behind

Renames on FAT are not atomic as a pair, and step 4 is two or three separate directory updates. What v1 guarantees is weaker and checkable:

- **The destination is never partly written.** `NAME.EXT` is only ever a complete old file or a complete, verified new file, or it is missing.
- **Some complete copy always survives**, as long as the disk completes writes in the order DOS issues them. Before step 4.1 the old file is `NAME.EXT`; between 4.1 and 4.2 it is `NAME.BAK` and the verified new file is `NAME.$$$`; after 4.2 the new file is `NAME.EXT` and the old one is `NAME.BAK`.
- A write-behind disk cache (for example SMARTDRV with write caching on) can reorder or lose writes on power failure and **voids the guarantee**. The client documentation tells the user to disable write-behind caching during transfers. A power failure while writing `NAME.$$$` can leave lost clusters for `CHKDSK`, but does not affect the other files.
- Only one backup generation is kept: replacing a file deletes the previous `NAME.BAK` first.

**State after an interruption, and recovery:**

| Interrupted | Files present | Recovery |
| --- | --- | --- |
| Before step 4 | `NAME.EXT` (old, if any), possibly `NAME.$$$` (incomplete or unverified) | Send the file again. `FILE_BEGIN` deletes the leftover (`found` bit 2). |
| Between 4.1 and 4.2 | `NAME.BAK` (old), `NAME.$$$` (new, verified), no `NAME.EXT` | Send the file again. `FILE_BEGIN` reports the destination missing and a backup present (`found` = bits 1 and 2); because the destination is missing, the commit skips step 4.1 and leaves the backup untouched. Or by hand: rename `NAME.BAK` back to `NAME.EXT`. |
| After 4.2, before the response arrives | `NAME.EXT` (new), `NAME.BAK` (old) | Nothing to do. Sending again is harmless: it replaces the file with identical content. |

The host cannot tell the last two cases apart from a lost response, so when a `FILE_COMMIT` gets no response it reports the outcome as **unknown**, says which files may exist, and suggests sending again. When `found` shows bit 1 without bit 0 at the next `FILE_BEGIN`, the host tells the user that an earlier commit was probably interrupted and the old version is in `NAME.BAK`.

**Failures during step 4:**

- Deleting `NAME.BAK` or renaming `NAME.EXT` to `NAME.BAK` fails (for example, a read-only file): nothing has been activated. The client deletes `NAME.$$$` and answers `FILE_ERROR`; the destination is unchanged.
- Renaming `NAME.$$$` to `NAME.EXT` fails after the old file became `NAME.BAK`: the client renames `NAME.BAK` back to `NAME.EXT`. If that works, it deletes `NAME.$$$` and answers `FILE_ERROR`; only the older backup has been lost. If it also fails, it leaves both files in place and answers `FILE_ERROR` with a message naming them (for example `old=NAME.BAK new=NAME.$$$`), and the host shows the user the table above.

## 6. Storage descriptors and read results

### `STORAGE_LIST` response

A count (u8), then one descriptor per device. Each descriptor starts with its own length, so that later minor versions can append fields to it:

| Field | Type | Meaning |
| --- | --- | --- |
| `length` | u8 | bytes in this descriptor after this field; skip to the next descriptor by it |
| `device` | u8 | Session-local identifier, used in `STORAGE_READ` |
| `kind` | u8 | 1 = removable, 2 = fixed, 0 = unknown |
| `addressing` | u8 | bit 0: linear units; bit 1: CHS |
| `unit_size` | u16 | bytes per unit or sector; **not assumed to be 512** |
| `unit_count` | u32 | number of addressable units (linear) |
| `cylinders` | u16 | CHS geometry; zero if not CHS |
| `heads` | u8 | |
| `sectors` | u8 | sectors per track |
| `first_sector` | u8 | number of the first sector (1 for PC BIOS) |
| `meta_len` | u8 | length of `meta`, at most 64 |
| `meta` | bytes | platform metadata as TLV (`tag` u8, `len` u8, value). DOS: tag 1 = BIOS drive number; tag 2 = the drive's largest geometry (cylinders u16, heads u8, sectors u8), within which CHS reads are accepted even past the medium's (probed) geometry. Unknown tags are skipped. |
| `label_len` | u8 | length of `label`, at most 64 |
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

- Class 0 means the unit was read; its data follows. `platform_code` is 0, or a status the platform reported along with good data, which the host records as **suspect**: for example BIOS `11h`, data corrected by ECC.
- Class 4 (`MEDIA_ERROR`) means it could not be read; `platform_code` is the BIOS status, and **no bytes are sent for it**. The client never fills unreadable units with zeros; the host records them as failed in the image's error map.
- A whole-request failure (for example, no disk in the drive) is an `ERROR` frame instead.

Reads are idempotent, so retransmissions never cause duplicate writes on the host. The host tracks offsets by request, not by arrival.

### Rereads

Every `STORAGE_READ` with a new `seq` reads the medium again. The client keeps no sector cache, and only a retransmission (the same `seq`, a duplicate under section 2) is answered from the response cache. The host may therefore reread suspect or failed units as often as its policy says, with any `retries` value, including 0 for a single raw attempt. Comparing and keeping conflicting successful reads is host imaging policy ([imaging](imaging.md#marginal-media)); it needs nothing more on the wire.

## 7. Test vectors and malformed-frame fixtures

### Proposed fixture format

Fixtures live in `tests/vectors/`, as plain text that a C test harness can parse without a JSON library:

```text
# name: one line per field; hex bytes separated by spaces
name   hello
body   01 3c 5a 00 53 50 52 4b 01 00 00 04
crc    20ba
wire   7e 01 3c 5a 00 53 50 52 4b 01 00 00 04 ba 20 7e
```

The same files drive the Rust tests and a host-compiled build of the DOS client's C parser, so both implementations are tested against one source.

### Valid frames (computed for this proposal)

| Name | Wire bytes |
| --- | --- |
| `HELLO`: session `5A3C`, max payload 1024 | `7e 01 3c 5a 00 53 50 52 4b 01 00 00 04 ba 20 7e` |
| `HELLO_REPLY`: max 512, capabilities `FILE_WRITE`+`FILE_REPLACE`, DOS, `"0.1.0"` | `7e 81 3c 5a 00 53 50 52 4b 01 00 00 02 03 00 00 00 01 05 30 2e 31 2e 30 f4 14 7e` |
| `PING` `seq` 1, payload `7e 7d 00 ff` (both escapes) | `7e 02 3c 5a 01 7d 5e 7d 5d 00 ff 3f 8d 7e` |
| `PING_REPLY` to it | `7e 82 3c 5a 01 7d 5e 7d 5d 00 ff be 70 7e` |
| `ERROR` `UNSUPPORTED`, `seq` 2, no message | `7e ff 3c 5a 02 01 00 00 00 36 6a 7e` |
| `PENDING`, `seq` 3 | `7e fe 3c 5a 03 04 d8 7e` |

CRC-16 check value: `"123456789"` → `0x29B1`. CRC-32 check value: `0xCBF43926`.

### Malformed and edge cases (expected result)

| Case | Expected |
| --- | --- |
| One payload bit flipped | dropped (CRC) |
| Frame cut short by `7E` | dropped (CRC or too short) |
| Fewer than 6 unescaped bytes | dropped |
| `7D 7E` inside a frame | frame aborted; the next frame parses normally |
| `7D` followed by a byte other than `5E` / `5D` | dropped |
| Payload over the receiver's maximum | dropped without buffering the rest |
| Leading garbage, then a valid frame | garbage ignored; frame accepted |
| Repeated `7E 7E 7E` | ignored |
| Valid frame, wrong `session` (not `HELLO`) | dropped by the client |
| Duplicate request after its response | cached response resent; not executed again |
| Duplicate request while it is still executing | not restarted; `PENDING` or ignored |
| Same `seq`, different type or payload | `ERROR` `INVALID_REQUEST`; not executed; cache unchanged |
| `HELLO` identical to the one that opened the session | cached `HELLO_REPLY`; session not reset |
| `HELLO` with the current `session` after later requests | new session |
| Request with a reserved flag bit set | `ERROR` `INVALID_REQUEST`; not executed |
| Known payload followed by extra trailing bytes (extensible type) | trailing bytes ignored |
| Unknown request type | `ERROR` `UNSUPPORTED` |
| Response with stale `seq` | ignored by the host |

## Memory budget (DOS client, 1024-byte payloads)

| Item | Bytes |
| --- | --- |
| Receive buffer (header + payload + CRC) | 1030 |
| Response buffer, also the duplicate cache | 1030 |
| Duplicate record (`seq`, type, length, CRC, state) | 7 |
| CRC-16 table (byte-wise) | 512 |
| CRC-32 table (byte-wise) | 1024 |
| Path and state | < 256 |
| **Total** | **< 4 KB** |

A 256-byte-payload client with nibble tables needs well under 1 KB.

## Open questions

1. **Faster links:** a `SET_BAUD` request (switch both sides after the response, with fallback to 9600 if the next `PING` fails). Proposed for a later minor version.
2. **File resume:** resuming a file transfer across sessions needs the client to keep the temporary file and report its length and CRC. Deferred until there is a need. Resumable disk imaging is separate and needs no protocol change ([imaging](imaging.md#resumable-imaging)).
3. **Client → host files** (`FILE_READ`) and directory listing: not needed by the milestones; reserved numbers `0x18`–`0x1F`.
4. **Track-level or format-specific reads** for non-PC media (Apple II nibble data, sector skew): reserved addressing bits, to be designed with the second client.
5. **Windowing:** stop-and-wait costs about one round trip per frame. If measured throughput is too low at higher baud rates, a later minor version could allow a small window for clients that buffer by interrupt.
6. **Path length:** 127 bytes covers DOS paths with room to spare; confirm for future platforms.
