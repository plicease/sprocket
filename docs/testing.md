# Testing and emulation

**Status:** Draft. This document defines a testing strategy, not a claim that automated emulator tests already work.

## Objectives

- Iterate on the DOS client without moving each build to vintage hardware.
- Exercise the **actual** DOS binary and Rust host over a serial-equivalent byte stream.
- Test native protocol framing, corruption detection, timeouts, retransmission, cancellation, and recovery.
- Verify disk captures against deterministic fixtures, while keeping source-media errors distinct from transport errors.
- Detect 8086 incompatibilities and old-UART assumptions before hardware deployment.

## Environments

| Tier | Environment | Main purpose | Limits |
| --- | --- | --- | --- |
| 0 | Rust unit tests and in-memory/PTY transports | Packet encoding, parsers, state machines, synthetic errors | Does not execute DOS code |
| 1 | DOSBox-X on `athena` | Fast DOS build/run loop, virtual COM port, protocol and basic BIOS disk tests | Timing, UART and BIOS behavior are not fully representative |
| 2 | 86Box on `athena` | Period PC motherboard, 8088/8086-class CPU, UART, BIOS and disk-controller behavior | Requires compatible ROMs and careful machine configuration |
| 3 | Real hardware: `smokey`, NuXT, PS/2 Model 70 | Physical cable, UART, DOS/BIOS and actual disk behavior | Slow iteration; use read-only operations first |

DOSBox-X: https://dosbox-x.com/ and https://github.com/joncampbell123/dosbox-x/releases

86Box: https://86box.net/ and https://github.com/86Box/86Box/releases

86Box ROM collection: https://github.com/86Box/roms/releases . Match the ROM collection to the emulator version; follow 86Box's current ROM search-path documentation. ROM files are external emulator assets, **not** SPROCKET dependencies to vendor into this repository.

## Serial connection design

The Rust host should expose an internal byte-stream interface with implementations for:

- **Physical serial:** `/dev/ttyS0` on `twintwist` (9600 8N1, no flow control initially).
- **TCP test transport:** local TCP stream attached to an emulator's virtual serial/null-modem endpoint.
- **In-memory/fault-injecting transport:** deterministic host tests.

**Proposal:** a CLI argument such as `--port tcp://127.0.0.1:5000`; exact syntax is undecided. The TCP adapter must not silently add modem commands, telnet negotiation, or other bytes to the native protocol. Configure the emulator for a transparent null-modem path, not modem emulation. Confirm client/server connection direction and precise serial configuration against the installed emulator version before codifying launch scripts.

For DOSBox-X, use its null-modem serial configuration, with the Rust host connecting to the corresponding TCP listener (or the reverse if configured accordingly). For 86Box, verify the available virtual COM redirection modes in the selected build; if direct TCP is unavailable, use a pseudo-terminal or serial bridge. Do not assume identical syntax between emulators.

## DOS client execution

- Cross-compile with Open Watcom on `athena` for 16-bit real-mode DOS, targeting 8086-compatible instructions.
- Make the build output accessible to DOSBox-X through a mounted development directory for quick runs.
- For 86Box, use a bootable DOS disk image and an appropriate method to inject or copy the client binary into it.
- Select an 8088/8086-class CPU and conservative memory/UART settings for compatibility tests; the NuXT's NEC V20 remains a real-hardware target.
- Test against the agreed minimum DOS version once decided; DOS 3.3 is currently provisional.

## Disk-imaging fixtures

1. Generate a small disk image with known bytes and a recorded digest.
2. Attach it as a BIOS-accessible emulated floppy or hard disk; do not substitute a host-directory mount.
3. Run the DOS client and request a read-only capture through the Rust host.
4. Compare the captured logical sectors with the expected fixture; check geometry and ordering before asserting byte-for-byte equivalence.
5. Inspect the image manifest for sector count, completeness, source metadata and digest.
6. Test disk-read failures using a controllable mock backend first; emulator support for faulty sectors is not guaranteed.

A transport checksum validates bytes **in transit**, not the readability of the original disk. Preserve these error categories separately.

## Fault injection and automation

Test at minimum: truncated frames, flipped bytes, duplicate frames, lost acknowledgments, delayed responses, stalled peers, disconnect/reconnect, unexpected commands, and interrupted file deployment. Test recovery and bounds on retries/memory use.

Initially automate Rust unit tests and protocol tests using a Rust mock client. Add DOSBox-X end-to-end smoke tests when emulator startup and serial redirection can run unattended in CI. 86Box CI may be a later, optional job because machine/ROM configuration and headless automation are more involved. Never treat emulator passes as a substitute for real-hardware acceptance.

## First end-to-end emulator milestone

**Acceptance:** DOSBox-X boots a DOS environment, runs the Open Watcom-built `SPROCKET.EXE`, exchanges a version/capability handshake with the Rust host over the emulated COM1 link, and completes a small verified file transfer. Follow with a repeat under 86Box configured as an 8088-class PC.

## Open questions

- Exact DOSBox-X and 86Box versions, launch/configuration commands and ROM provisioning strategy.
- Whether emulator CI can run reliably without an interactive display.
- Which disk formats and geometries to use for the first canonical fixtures.
- Whether to support PTY as a first-class host CLI transport or only as a test adapter.
