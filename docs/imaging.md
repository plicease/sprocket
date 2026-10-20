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

Read each requested range; on media error, retry only within a bounded policy, report exact failed addresses, and continue only when the operator's selected policy allows it. Host should mark images with missing data as **incomplete**, not successful. CRC/retry at the serial layer cannot repair a source disk read failure. If a transfer is interrupted, preserve a resumable partial image only with validated metadata and clear status.

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
