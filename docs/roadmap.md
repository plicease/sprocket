# Implementation roadmap

**Status:** Draft; milestone order reflects agreed priorities, not a delivery commitment.

## Milestone 0 — Documentation and Rust template

- Integrate this design set with the user's existing Rust project template.
- Add MIT root license with correct copyright attribution.
- Review and ratify protocol fundamentals before client implementation. **Done:** [protocol v1](protocol-v1.md) ratified on 2026-10-21.
- Keep `client/dos` as the DOS path; no speculative empty platform trees.

## Milestone 1 — Standalone host XMODEM sender

- Implement serial configuration and built-in XMODEM / XMODEM-1K sending in Rust.
- Validate on `twintwist` COM1 → `smokey` HyperTerminal at 9600 8N1, no flow control.
- Test with known small files and deliberate error/timeout conditions.
- Document XMODEM padding and verification limitations.

**Acceptance:** transfer works without calling an external Linux transfer utility.

**Status:** met on 2026-10-20. `sprocket send --protocol xmodem` from `twintwist` to Windows 95 HyperTerminal on `smokey`, at 38400 8N1 rather than the planned 9600, delivered a 43,296-byte DOS executable with a matching MD5 after truncating the XMODEM padding. Cancelling in HyperTerminal was reported cleanly.

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

**Status:** 1A and 1B are implemented and tested against in-memory receivers and lrzsz over a PTY. On real hardware (2026-10-20), `twintwist` → `smokey` Windows 95 HyperTerminal at 38400 8N1, ZMODEM auto-downloaded a file with matching MD5 sums, and XMODEM and XMODEM-1K downloads matched once truncated to the original length. The menu returned after each transfer. This meets the 1A and 1B acceptance criteria. Pending: cancellation through the menu on hardware, DOS terminal programs, and emulator runs. Resume of interrupted ZMODEM transfers (ZCRECOV) is not implemented.

## Milestone 2 — Native protocol and DOS client skeleton

- Freeze a minimal v1 framing/negotiation subset and publish test vectors.
- Implement Rust host and Open Watcom real-mode DOS client with a hello/status command.
- Ensure 8086 instruction compatibility, non-FIFO UART support, and bounded memory use.
- Test on `smokey`, then NuXT.

**Acceptance:** robust two-way native packet exchange with retries and version/capability reporting.

**Status (2026-10-21):** host side implemented: v1 framing with the `tests/vectors/` fixtures, the stop-and-wait link layer, the session layer (`HELLO`, `PING`, `CLOSE`), and `sprocket hello`. The DOS client in `client/dos` implements the same subset with a polled UART; its C core passes the shared fixtures, and `SPROCKET.EXE` completes the handshake and 1024-byte pings under DOSBox-X (8086) at 9600, 38400 and 115200 baud. Under 86Box as an IBM XT (8088 at 4.77 MHz), the handshake and 1 KB pings complete. On real hardware (2026-10-23), a NuXT (`phaistos`, NEC V20) ran 20 clean 1 KB pings at up to 38400 baud at 4.77 MHz and up to 57600 at 9.54 MHz; the next rate up failed in each case, as `BENCH.EXE` predicted (see [testing](testing.md#real-hardware-nuxt-phaistos)). Retransmission has been exercised only under DOSBox-X fault injection, since the clean hardware runs never needed it. `smokey` (P75) passed the handshake and three 1 KB pings at 9600, 38400 and 115200 baud (2026-10-22). Remaining: the corrected `BENCH.EXE` on `smokey` (issue #9).

## Milestone 3 — Native file transfer and recovery

- Exact-length file transfers and end-to-end verification.
- Safe receive-to-temp and upgrade/rollback workflow.
- Optional tiny DOS bootstrap receiver and documented installation methods.
- DOS client user documentation with a prominent warning to disable write-behind disk caching during transfers; interrupted-commit recovery depends on ordered writes.

**Acceptance:** safely deploy a new DOS client over serial without relying on third-party Linux transfer tools; interruption leaves a working recovery path.

**Status (2026-10-23):** file transfer is implemented by the proposed [protocol v1.1](protocol-v1.1.md) and the shell in [sprocket-file-transfer-ui](sprocket-file-transfer-ui.md), in both directions and with either side in either role: `sprocket serve` and `sprocket client` on Linux, and `SPROCKET SERVER` and `SPROCKET CLIENT` in `SPROCKET.EXE`. Writes go to a temporary file and are put in place only when complete (and, for `put`, after the length and CRC-32 check); a DOS server keeps the replaced file as `NAME.BAK`. Tested on Linux with every pairing of the Rust and C sides over a PTY pair, and with `SPROCKET.EXE` under DOSBox-X in both roles ([testing](testing.md#native-client-tests)). The DOS documentation warns against write-behind caching. Remaining for acceptance: runs on real hardware, including replacing `SPROCKET.EXE` on a DOS server with `put -f` and recovering from an interrupted commit; and ratifying v1.1. The bootstrap receiver is not started.

## Milestone 4 — Read-only logical disk imaging

- Enumerate DOS-visible BIOS disks, read supported ranges, and stream them to host.
- Write images and machine-readable error maps/metadata.
- Detect and report source-media read failures separately from link failures.
- Resumable imaging: a persistent, crash-safe progress map (captured, missing and failed units, with platform codes and read attempts); on restart, revalidate the source disk and read only missing ranges, in any order, continuing past bad sectors. See [imaging](imaging.md#resumable-imaging). No change to `STORAGE_READ`; resuming file uploads stays deferred.
- Recovery passes over failed and suspect units, keeping conflicting successful reads ([marginal media](imaging.md#marginal-media)).

**Acceptance:** image a known floppy and a hard disk (subject to BIOS capabilities) with documented completeness and integrity checks. Imaging a hard disk survives interruption: stopping and restarting the host, and separately the client, partway through produces an image byte-for-byte identical to an uninterrupted capture, without rereading units already captured. A PS/2 Model 70 disk of about 80 MB, which takes hours at 38400 baud, is the motivating case.

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
- **Milestone 4:** boot from a **real emulated BIOS disk image**, image it through the virtual serial link, and compare the result with known fixture bytes. Confirm BIOS drive numbering, geometry, and read failure reporting. Interrupt and resume captures as described in [testing](testing.md#resumable-imaging). Repeat representative tests in 86Box and on real media.
- **CI:** prioritize Rust unit tests and deterministic host/virtual-client integration tests first; add headless emulator smoke tests only after startup, ROM provisioning, and serial connectivity are reproducible.

Detailed matrix and setup guidance: [testing](testing.md).
