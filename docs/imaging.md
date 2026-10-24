# Disk imaging and preservation

**Status:** Implemented (2026-10-23) as described under [decisions](#decisions) and [implementation](#implementation), and tested on Linux and in DOSBox-X; not yet on real hardware. Imaging is read-only.

## Goals

- Capture logical contents of floppy disks and hard disks through the vintage machine's available disk interfaces.
- Preserve enough metadata to interpret the image and understand unreadable regions.
- Distinguish source-media failures from serial-link failures and host-side write failures.
- Never silently substitute zero-filled data for unreadable sectors without recording that substitution.
- Support non-PC clients and non-512-byte logical units in the protocol design.

## Initial DOS implementation

Use DOS/BIOS-appropriate low-level disk access (initially BIOS INT 13h) to discover and read available devices. The DOS client must run on 8086/8088-compatible systems and should work with non-FIFO UARTs. Avoid assuming Enhanced Disk Drive (EDD) extensions exist; CHS support is needed for older hardware. Validate geometry and read bounds carefully; document BIOS-specific limitations.

**Important:** A DOS drive letter (`C:`) is not a BIOS drive number (`0x80`); user-facing commands must identify devices unambiguously and require confirmation before large reads.

## Platform-neutral storage descriptors

The client reports the storage access modes it actually supports, such as:

- Fixed-size logical units: size in bytes, number of units, and range limits.
- Geometry-oriented CHS access when relevant.
- Future track/record-based or format-specific modes.

Apple II DOS 3.3 sectors can be 256 bytes; ProDOS blocks are 512 bytes. A fixed 512-byte unit in the wire protocol would be a mistake. Track/sector organization, sector skew, and controller-specific behavior must remain representable where required.

## Image outputs

**Proposal:** write a raw data image where the source is representable as uniform fixed-size units, plus a machine-readable sidecar manifest containing:

- Source platform/client and reported device identity.
- Unit size/count, addressing method, and available geometry.
- Capture timestamps and host/client versions.
- Per-unit or per-range read status, retries, and substituted data policy.
- Image length and a host-computed digest (e.g. SHA-256).

For nonuniform media, choose a format appropriate to the source rather than forcing a misleading raw-sector image. File naming and formats are under [decisions](#decisions); the manifest's exact schema is settled with the first implementation.

## Failure behavior

Read each requested range; on media error, retry only within a bounded policy, report exact failed addresses, and continue only when the operator's selected policy allows it. Host should mark images with missing data as **incomplete**, not successful. CRC/retry at the serial layer cannot repair a source disk read failure. If a transfer is interrupted, the partial image and its progress map are kept so that imaging can resume (below).

## Resumable imaging

A hard disk at serial speeds takes hours: about 80 MB at 38400 baud is roughly six hours of payload alone, and at 9600 nearly a day. An interruption (a power cut, a dropped cable, a crashed host or client) must not mean starting again. Resume is a **host feature**: `STORAGE_READ` is idempotent and addresses units directly, so no protocol change is needed. It is separate from resuming a file upload, which stays deferred.

### Persistent state

Next to the image file, the host keeps a **progress map** recording, per unit or per range:

- **captured**: the bytes are in the image and durable;
- **missing**: not yet read, or the outcome is unknown (for example, the host gave up waiting for a response);
- **failed**: the client reported a read error, with its platform code (e.g. the BIOS status);
- the number of read attempts and the `retries` value used.

It also records the source identity (below), the unit size and count, and the host and client versions. The image file's length never implies completeness: an image can be full length with failed or missing units, and a sparse or preallocated file says nothing about which units were written.

### Crash safety

- A unit is marked **captured only after its bytes are durable**: the host writes the data, syncs the image file (`fsync`), and only then records the units in the map.
- The map is updated by writing a new copy and renaming it over the old one (or by appending checksummed records to a journal), so that a crash leaves either the old map or the new one.
- On load, a map that is torn or fails its checksum is **refused**, and the image and map are left untouched. Nothing then says which of the image's units are good, and the image may hold units of failing media that cannot be read again, so rereading into it could destroy them. The operator images to a new file, or moves the old files aside. A valid map that does not match the image (an image shorter than the map says) is treated conservatively: every unit beyond the image's end is **missing**, and is read again.
- Syncing after every response would be slow; the host may batch, as long as no unit is marked captured before a sync that covers it.

### Restarting

1. Reload the map and image, and start a new session.
2. Re-enumerate with `STORAGE_LIST` and **validate that the source is the same disk**: unit size and count, geometry, and capacity must match. A BIOS drive number and geometry alone do not identify a disk, so the host also compares a fingerprint taken at the start, such as the hashes of a few fixed units (unit 0 and a handful spread across the disk), read again on resume. If anything differs or cannot be checked, the host stops and asks the operator to confirm before continuing. In particular, if a fingerprint unit **cannot be read** on resume (a read error, or no response), the source identity is **unverified**: the host never treats an unreadable unit as a match, never silently drops it from the fingerprint, and does not continue without explicit operator confirmation, which it records in the manifest.
3. Request only **missing** ranges. Requests need not be in order, and a failed unit never stops the pass: it is recorded and the host moves on.
4. Afterwards, the operator may run **recovery passes** over failed and suspect units with a different retry policy (see marginal media).

The image is reported as **complete** only when every unit is captured, and as **incomplete** otherwise, with the counts of missing and failed units.

## Marginal media

A unit that read successfully may still be wrong or unstable on a failing disk. The host's imaging policy, not the protocol, decides how to treat it:

- A unit that failed, or was reported as suspect (good data with a platform status such as BIOS `11h`, ECC-corrected), can be read again in a later pass. Each new `STORAGE_READ` rereads the medium ([protocol v1](protocol-v1.md#rereads)).
- Rereads that return **different** successful data are all kept, in a sidecar with each read's hash, the time, the attempt number, the `retries` value, and the client's status. The image holds one chosen version (the first, or the majority once there are three or more), and the map marks the unit as **conflicting** so that nothing claims it is certain.
- The final manifest lists failed, suspect and conflicting units separately.

## Verification

- Validate received payloads via protocol checksums and sequence tracking.
- Verify final image size and host digest.
- Where practical, optionally reread the source and compare independently; host hashing alone proves artifact integrity, not correctness of source reads.
- Preserve source-read error maps for later recapture attempts.

## Safety

No disk-writing/restoration commands in the initial implementation. Avoid making the source writable unnecessarily. File destinations on the host must be explicit; refuse accidental overwrite by default.

## Preservation limits

BIOS/logical reads do not preserve magnetic flux, weak bits, timing, copy-protection tricks, or some nonstandard formats. SPROCKET is a logical imaging tool, not a replacement for flux-imaging hardware.

## Decisions

Agreed on 2026-10-23 for Milestone 4.

- **Host command.** `sprocket image --port … --list` shows the server's disks; `sprocket image --port … --device N --output NAME.img` images one. Running the same command again with an existing progress map resumes. It refuses to overwrite an image that has no map, and asks for confirmation before a large read.
- **Files.** `NAME.img` is the raw image. `NAME.img.json` is the manifest (JSON): source, geometry, host and client versions, counts of captured, missing, failed, suspect and conflicting units, and the image's SHA-256. `NAME.img.map` is the progress map: a list of unit ranges with their state, attempts and platform codes, replaced by writing a new copy and renaming it, and protected by a checksum.
- **Disks are opt-in on DOS.** `SPROCKET SERVER` offers disks only when started with a switch (`/D`), as `/W` gates writes, and only then advertises `STORAGE_LIST` and the `STORAGE_READ` capabilities. Imaging is read-only, but a raw disk includes deleted files and free space.
- **Geometry.** INT 13h function 08h for drive parameters, with hard disks counted from the BIOS data area and floppies from the equipment word. For a floppy, function 08h gives the drive's geometry, not the disk's, so the client probes the inserted media. A BIOS without function 08h (the original 1981 PC/XT BIOS) is not supported at first.
- **Large disks.** CHS only at first, with capacity reported as the BIOS gives it. The EDD extensions (functions 41h/42h) become the linear-addressing path when a disk needs them.
- **Retries and fill.** By default the host asks for 3 client-side retries per unit, with a disk reset between attempts. Unreadable units are zero-filled in the image and always recorded as failed in the map and manifest, never only in the image.
- **DMA boundary.** INT 13h fails (status 09h) when a read buffer crosses a 64 KB physical boundary, which depends on where DOS loads the program. The client detects this and reads through a bounce buffer.
- **Labels.** A descriptor's label gives the BIOS drive number, kind and size, with a drive letter only for floppies (for example `"BIOS 80 fixed 79.8M"`): which hard disk partition is `C:` is not reliable from the BIOS alone.
- **Payload size: to be measured.** The DOS program accepts 1024-byte payloads, so a `STORAGE_READ` response carries one 512-byte unit (two would need 1031 bytes). The protocol allows 4090, enough for seven. The buffer size becomes a build constant, both sizes are timed on real hardware, and the result decides the default.
- **Allocated-only imaging ([#13](https://github.com/plicease/sprocket/issues/13)) is out of scope for Milestone 4,** but nothing here may rule it out. It needs no protocol change, since the host chooses which units to read. So: the host plans every read; the image is written at unit offsets into a file of full logical size, never appended, so skipped ranges can be sparse holes; the map can record ranges deliberately not read (as distinct from missing or failed); and the manifest records the imaging mode (`raw` for now).

## Implementation

- **Host.** `src/imaging/`: `capture.rs` runs a capture; `map.rs` is the progress map; `manifest.rs` the manifest; `reads.rs` the reread log. `sprocket serve --disk FILE` offers an image file as a disk, which is how the host side is tested end to end.
- **Progress map** (`NAME.img.map`). JSON: the source's descriptor and client, the imaging mode, the start time, the fingerprint (unit numbers and SHA-256s), operator confirmations, and runs of units with the same state (`missing`, `captured`, `failed`, `skipped`), attempts, `retries`, platform code, conflict flag, and for a captured unit the count and last code of rereads that failed. A final `crc32 XXXXXXXX` line protects it. It is replaced by writing a temporary file and renaming it. A unit is marked captured only after the image is synced past it.
- **Manifest** (`NAME.img.json`). `format` (`sprocket-image`) and `version` (1), the image's name, length and SHA-256, `status` (`complete` or `incomplete`), `mode` (`raw`), `fill` (`zero`), the source, host version, start and finish times, counts, and lists of failed, suspect, conflicting and missing unit ranges (with attempts, codes, and failed rereads), plus the fingerprint, the confirmations, and `reads`, which names the reread log if there is one. It is rewritten from the map at the end of every run.
- **Recovery passes** (`--recover N`). These run after every unit has been read. Each pass rereads the failed, suspect and conflicting units one at a time. A failed unit that reads is captured. A suspect unit that rereads the same data with no status loses that status; with a status, it stays suspect. A captured unit whose reread fails keeps its data and that data's status, and the failure is recorded beside them (a count of failed rereads and the last one's code), which makes the unit suspect for good. When a reread differs from the data in the image, the earlier version and the new one go in the reread log (`NAME.img.reads`), and the unit becomes conflicting for good. Every later reread of it is logged, and the image holds the first version, or the majority once there are three or more reads. The log is JSON lines, each holding a unit, its SHA-256, time (absent for the first version, whose time was not recorded), attempt, `retries`, status and data in hex. Each line is synced before the map changes.
- **DOS.** `client/dos/src/storage.c` answers the requests; `biosdisk.c` does the INT 13h work. Only CHS is offered (`STORAGE_READ` by linear unit gets `UNSUPPORTED`).

## Open questions

- The default payload size, after the measurements above.
- 86Box fixtures, for a more representative BIOS and controller than DOSBox-X's.

## Emulator-based image validation

Use DOSBox-X for initial end-to-end tests and 86Box for more representative BIOS/controller behavior. Construct deterministic floppy and hard-disk fixtures, expose them as **BIOS-visible emulated drives** (not merely host-directory mounts), capture via the DOS client, and compare against expected logical sector bytes. Raw file equality is valid only when the emulator's presented geometry and logical addressing match the fixture format.

Use fault-injection in a mocked disk backend for deterministic read failures; emulator support for bad sectors varies and must not be assumed. Emulator success does not establish that real disks, unusual controllers, or copy protection are handled correctly. See [testing](testing.md).
