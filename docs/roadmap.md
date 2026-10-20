# Implementation roadmap

**Status:** Draft; milestone order reflects agreed priorities, not a delivery commitment.

## Milestone 0 — Documentation and Rust template

- Integrate this design set with the user's existing Rust project template.
- Add MIT root license with correct copyright attribution.
- Review and ratify protocol fundamentals before client implementation.
- Keep `client/dos` as the DOS path; no speculative empty platform trees.

## Milestone 1 — Standalone host XMODEM sender

- Implement serial configuration and built-in XMODEM / XMODEM-1K sending in Rust.
- Validate on `twintwist` COM1 → `smokey` HyperTerminal at 9600 8N1, no flow control.
- Test with known small files and deliberate error/timeout conditions.
- Document XMODEM padding and verification limitations.

**Acceptance:** transfer works without calling an external Linux transfer utility.

## Milestone 2 — Native protocol and DOS client skeleton

- Freeze a minimal v1 framing/negotiation subset and publish test vectors.
- Implement Rust host and Open Watcom real-mode DOS client with a hello/status command.
- Ensure 8086 instruction compatibility, non-FIFO UART support, and bounded memory use.
- Test on `smokey`, then NuXT.

**Acceptance:** robust two-way native packet exchange with retries and version/capability reporting.

## Milestone 3 — Native file transfer and recovery

- Exact-length file transfers and end-to-end verification.
- Safe receive-to-temp and upgrade/rollback workflow.
- Optional tiny DOS bootstrap receiver and documented installation methods.

**Acceptance:** safely deploy a new DOS client over serial without relying on third-party Linux transfer tools; interruption leaves a working recovery path.

## Milestone 4 — Read-only logical disk imaging

- Enumerate DOS-visible BIOS disks, read supported ranges, and stream them to host.
- Write images and machine-readable error maps/metadata.
- Detect and report source-media read failures separately from link failures.

**Acceptance:** image a known floppy and a hard disk (subject to BIOS capabilities) with documented completeness and integrity checks.

## Milestone 5 — Broader hardware and protocols

- Add capabilities needed by a real second client (e.g. Apple II).
- Consider YMODEM/ZMODEM, additional host platforms, and other imaging modes.
- Extend only on demonstrated hardware needs, preserving protocol compatibility.

## Cross-cutting checks

- MIT license and third-party notice compliance, including Open Watcom runtime.
- Reproducible builds, tests against synthetic serial failures, and clear error messages.
- Explicit safety barriers against accidental disk writes.
