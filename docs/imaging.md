# Disk imaging and preservation

**Status:** Draft. Initial imaging is read-only.

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

For nonuniform media, choose a format appropriate to the source rather than forcing a misleading raw-sector image. The manifest schema and naming are not yet decided.

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
- On load, a map that is torn, fails its checksum, or does not match the image is treated conservatively: every unit it cannot vouch for is **missing**, and is read again. Over-reading is cheap; trusting a bad map is not.
- Syncing after every response would be slow; the host may batch, as long as no unit is marked captured before a sync that covers it.

### Restarting

1. Reload the map and image, and start a new session.
2. Re-enumerate with `STORAGE_LIST` and **validate that the source is the same disk**: unit size and count, geometry, and capacity must match. A BIOS drive number and geometry alone do not identify a disk, so the host also compares a fingerprint taken at the start, such as the hashes of a few fixed units (unit 0 and a handful spread across the disk), read again on resume. If anything differs or cannot be checked, the host stops and asks the operator to confirm before continuing.
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

## Open questions

- First supported floppy/hard-disk geometry discovery methods.
- Image sidecar format and stable error-map representation.
- Retry policy and fill bytes for unreadable ranges.
- Handling drives larger than CHS BIOS limits on older PCs.

## Emulator-based image validation

Use DOSBox-X for initial end-to-end tests and 86Box for more representative BIOS/controller behavior. Construct deterministic floppy and hard-disk fixtures, expose them as **BIOS-visible emulated drives** (not merely host-directory mounts), capture via the DOS client, and compare against expected logical sector bytes. Raw file equality is valid only when the emulator's presented geometry and logical addressing match the fixture format.

Use fault-injection in a mocked disk backend for deterministic read failures; emulator support for bad sectors varies and must not be assumed. Emulator success does not establish that real disks, unusual controllers, or copy protection are handled correctly. See [testing](testing.md).
