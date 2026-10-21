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

XMODEM bootstrap keeps priority over everything below. It must stay working and documented as the first-install path for any terminal program.

## Milestone 1A — Smart terminal mode: menu and XMODEM

- `sprocket serve`: plain-ASCII, keyboard-driven browser of a configured root directory over the serial line.
- Explicit XMODEM / XMODEM-1K download of a selected file; the receiver is started manually.
- Root confinement (no `..` or symlink escape), read-only, idle timeout, reconnect after link loss.

**Acceptance:** a plain terminal (no ANSI) browses the tree, downloads a file by XMODEM, and returns to the menu after success, cancellation, or error.

## Milestone 1B — ZMODEM and auto-download

- Built-in ZMODEM sender (filename, exact length, CRC-32, cancellation, ZRPOS recovery) for `sprocket serve` and `sprocket send`.
- ZMODEM as the default menu choice; its ZRQINIT start triggers auto-download in terminals that support it.
- Validate with at least one period terminal with auto-download (e.g. Telix or Procomm Plus under DOS).

**Acceptance:** a ZMODEM-capable terminal with auto-receive enabled downloads the selected file without manual receiver setup, byte-identical and under its original name. Terminals without ZMODEM still succeed via XMODEM.

**Status:** 1A and 1B are implemented and tested against in-memory receivers and lrzsz over a PTY. On real hardware (2026-10-20), `twintwist` → `smokey` Windows 95 HyperTerminal at 38400 8N1, ZMODEM auto-downloaded a file with matching MD5 sums, and XMODEM and XMODEM-1K downloads matched once truncated to the original length. The menu returned after each transfer. This meets the 1A and 1B acceptance criteria. Pending: cancellation from the terminal on hardware, DOS terminal programs, and emulator runs. Resume of interrupted ZMODEM transfers (ZCRECOV) is not implemented.

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
- Consider YMODEM, ZMODEM resume and upload, additional host platforms, and other imaging modes.
- Extend only on demonstrated hardware needs, preserving protocol compatibility.

## Cross-cutting checks

- MIT license and third-party notice compliance, including Open Watcom runtime.
- Reproducible builds, tests against synthetic serial failures, and clear error messages.
- Explicit safety barriers against accidental disk writes.

## Emulator and CI acceptance gates

- **Milestone 1:** host XMODEM packet tests with synthetic receiver, then HyperTerminal on `smokey`; emulation of the DOS client is not required yet.
- **Milestones 1A/1B:** scripted menu sessions over in-memory and PTY links, including lrzsz `rz`/`rx` interoperability. Then a DOS terminal program under DOSBox-X connected to `sprocket serve` over TCP, checking ZMODEM auto-download, manual XMODEM, cancellation, and exact file contents. Repeat on real hardware.
- **Milestone 2:** run DOS client under DOSBox-X with a virtual null-modem/TCP link to the Rust host; test protocol handshake, timeout, corruption and retry. Repeat critical paths under 86Box with an 8088-class machine.
- **Milestone 3:** automated transfer and executable deployment tests in emulator, including interrupted transfers and recovery; validate on `smokey`.
- **Milestone 4:** boot from a **real emulated BIOS disk image**, image it through the virtual serial link, and compare the result with known fixture bytes. Confirm BIOS drive numbering, geometry, and read failure reporting. Repeat representative tests in 86Box and on real media.
- **CI:** prioritize Rust unit tests and deterministic host/virtual-client integration tests first; add headless emulator smoke tests only after startup, ROM provisioning, and serial connectivity are reproducible.

Detailed matrix and setup guidance: [testing](testing.md).
