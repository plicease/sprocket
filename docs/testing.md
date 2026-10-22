# Testing and emulation

**Status:** Draft strategy. Of the emulator tests described here, only the native client's DOSBox-X smoke test (`client/dos/test/dosbox-hello.sh`) is automated; the 86Box run is manual, and the terminal-program tests are planned.

## Objectives

- Iterate on the DOS client without moving each build to vintage hardware.
- Exercise the **actual** DOS binary and Rust host over a serial-equivalent byte stream.
- Test native protocol framing, corruption detection, timeouts, retransmission, cancellation, and recovery.
- Verify disk captures against deterministic fixtures, while keeping source-media errors distinct from transport errors.
- Detect 8086 incompatibilities and old-UART assumptions before hardware deployment.
- Confirm that `sprocket send` and `sprocket serve` interoperate with real terminal programs, including ZMODEM auto-download and XMODEM fallback.

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

## Resumable imaging

Resume ([imaging](imaging.md#resumable-imaging)) is tested first against a Rust mock client with a fault-injecting disk backend, then with the DOS client under emulation. Each case compares the final image byte-for-byte with an uninterrupted capture of the same fixture, and checks the progress map.

- **Host interrupted:** kill the host at random points (between requests, mid-response, during an image write) and restart it. The capture finishes, no captured unit is requested again, and every unit is requested at least once.
- **Client interrupted:** stop the client or drop the link mid-capture and restart it. The host recovers with a new session and continues from the map.
- **Interrupted while persisting progress:** kill the host between writing image data and syncing it, and while the map is being replaced. A torn or mismatched map makes the host reread the units it cannot vouch for; nothing is marked captured that is not in the image.
- **Failed sectors:** units that fail are recorded with their platform codes and skipped; the pass continues; a later recovery pass rereads only those units. Units that return different data on reread are kept as conflicting reads.
- **Different source disk:** resume against a fixture with different contents, and one with different geometry or capacity. The host refuses to continue without confirmation.
- **Unreadable fingerprint unit:** on resume, make one fingerprint unit fail to read (media error, and separately no response). The host reports the source as unverified and requires operator confirmation; it does not continue on its own.
- **Unknown outcome:** the client is busy past the host's retry limit (a mock BIOS call that blocks). The units stay missing, not failed, and are captured after reconnecting.

## Fault injection and automation

Test at minimum: truncated frames, flipped bytes, duplicate frames (both while the request is still executing and after it completed), a reused `seq` with a different payload, lost acknowledgments, delayed responses, stalled peers, disconnect/reconnect, unexpected commands, and interrupted file deployment, including an interruption at each step of `FILE_COMMIT` with the recovery in [protocol v1](protocol-v1.md#what-an-interrupted-commit-leaves-behind). Test recovery and bounds on retries/memory use.

Initially automate Rust unit tests and protocol tests using a Rust mock client. Add DOSBox-X end-to-end smoke tests when emulator startup and serial redirection can run unattended in CI. 86Box CI may be a later, optional job because machine/ROM configuration and headless automation are more involved. Never treat emulator passes as a substitute for real-hardware acceptance.

## Native client tests

- `make test` in `client/dos` runs the DOS client's C core, built for the host, against the same `tests/vectors/` fixtures as the Rust host, plus its session and duplicate rules.
- `client/dos/test/dosbox-hello.sh [baud]` runs `SPROCKET.EXE` under DOSBox-X (8086, COM1 as a transparent TCP null-modem on port 5100) against `sprocket hello`, then checks the client's counters: one frame per request (`HELLO`, each `PING`, `CLOSE`), none dropped or repeated, and no UART receive errors. With `FAULT=1` the link runs through `client/dos/test/fault-relay.py`, which flips one bit in the first `PING` request and one in the client's first `PING` response; the client must then report exactly one dropped frame and one repeated request answered from its cache, and the host two retransmissions. Both modes passed at 9600, 38400 and 115200 baud on 2026-10-21. DOSBox-X does not model UART overruns faithfully, so this does not show that the polling loop keeps up on an 8088; 86Box and real hardware must.

### 86Box: IBM XT, 8088 at 4.77 MHz

Run on 2026-10-21 with 86Box build 9001: machine `ibmxt86` (IBM XT, 1986 BIOS), 8088 at 4.77 MHz, 640 KB, CGA, a 3.5" 720K drive, and COM1 an 8250 without a FIFO. The boot disk is FreeDOS 1.3's 720K `x86BOOT.img` (FreeDOS kernel 2043 and FreeCOM run on the 8088), with `FDCONFIG.SYS` and `FDAUTO.BAT` replaced with mtools so that it boots straight to `BENCH` and `SPROCKET COM1 9600`.

**Timing.** `BENCH.EXE` repeats the client's per-byte receive work over 1 KB frames:

| Build | µs per byte | Margin at 9600 | at 19200 | at 38400 |
| --- | --- | --- | --- | --- |
| `-os`, stack checks, tick read per byte (first version) | 586 | 1.7× | too slow | too slow |
| `-ox -s`, no tick read per byte (current) | 332 | 3.1× | 1.5× | too slow (0.7×) |

**End to end.** `sprocket hello` completed the handshake, five 1024-byte pings and `CLOSE` against the 8088. Some frames were lost to receive overruns and were recovered by retransmission. These overruns are from the emulator's serial console, not the client: the console device feeds the UART in bursts whenever it reads the PTY, which overruns an 8250 whatever the CPU. Pacing the host's bytes with `client/dos/test/pace-relay.py` reduced but did not remove them: about 1% of bytes at 9600 pacing (no ping got through), and about 0.04% at 4800 and at 2400. A client too slow for the line would have lost far more, and the benchmark shows a 3× margin at 9600. 86Box's "Serial Passthrough" device, which has a baud-rate setting, did not attach to a PTY in this build. Real hardware remains the test of 9600 back to back.

**Setup notes.**

- 86Box must run on a private X display (`Xvfb :77`, then `DISPLAY=:77 QT_QPA_PLATFORM=xcb` with `WAYLAND_DISPLAY` unset), or it opens on the desktop.
- Serial: `serial1_device = stdio` with `[Virtual Console (COM) #1]` `mode = 1` creates a PTY. 86Box pauses on a dialog naming it until Return is pressed (`xdotool key Return`). The PTY number is the `tty-index` in `/proc/<86Box pid>/fdinfo/<fd>` for its `/dev/ptmx` descriptor.
- 86Box ignores SIGTERM here; stop it with SIGKILL.
- Results printed by the guest can be read back from the floppy image with `mtype` after 86Box stops.

## First end-to-end emulator milestone

**Acceptance:** DOSBox-X boots a DOS environment, runs the Open Watcom-built `SPROCKET.EXE`, exchanges a version/capability handshake with the Rust host over the emulated COM1 link, and completes a small verified file transfer. Follow with a repeat under 86Box configured as an 8088-class PC.

## Terminal file server and standard protocols

`sprocket send` and `sprocket serve` talk to ordinary terminal programs rather than the SPROCKET client, so they are tested at every tier without any DOS build.

**Tier 0: automated (`cargo test`).** In-memory transports with test receivers:

- XMODEM: checksum and CRC modes, 1K blocks and fallback, corrupted blocks (NAK and resend), lost ACKs (timeout, duplicate discarded), NAKed EOT, receiver cancel, no receiver, sequence wrap, empty file.
- ZMODEM: exact length, filename and timestamp against the `zmodem2` receiver; corrupted data subpacket (ZRPOS recovery); receiver CAN cancel; no receiver; ZRQINIT repetition.
- Commands: `ls` (alphabetical, sizes, `-R`, symlink loops, Ctrl-C with typeahead kept), `cd`/`pwd` path handling, hidden files refused, Tab completion, `get` wildcards (batch order, stop after a failure), line editing, error messages, idle timeout, the `protocol` command, ZMODEM and XMODEM downloads returning to the prompt, a cancelled ZMODEM returning to the prompt, no-receiver errors, and disconnect.
- File tree: `..`, absolute paths, and symlinks leading outside the root are refused; symlinks inside it are allowed; hidden and escaping entries are not listed.

**Tier 0: interoperability (manual).** Run `sprocket` against lrzsz's `rz`/`rx` over a `socat` PTY pair. lrzsz is a test receiver only, never a SPROCKET dependency; it can be unpacked locally with `apt-get download lrzsz && dpkg-deb -x`, without installing it.

```sh
socat pty,raw,echo=0,link=/tmp/ptyA pty,raw,echo=0,link=/tmp/ptyB &
(cd recv && rz -q < /tmp/ptyB > /tmp/ptyB) &
sprocket send --protocol zmodem --port /tmp/ptyA FILE
cmp recv/FILE FILE        # exact bytes, name and mtime preserved
```

For `serve`, a script on `/tmp/ptyB` presses ENTER, types `get FILE`, starts `rz` when it sees the ZRQINIT signature `**\x18B00` (simulating auto-download), and checks that the prompt returns. For XMODEM it runs `rx`, and compares the received prefix and padded length.

**Tier 1: DOSBox-X.** Run a DOS terminal program (e.g. Telix or Procomm Plus, obtained separately and not vendored) with COM1 as a transparent null-modem, and point `sprocket serve --port tcp://…` at it. Check:

- Auto-download: with auto-ZMODEM enabled, `get FILE` starts the download with no further keys. With it disabled, a manual ZMODEM receive works.
- Fallback: XMODEM and XMODEM-1K via the terminal's manual receive; padding is reported correctly.
- Cancellation from the terminal (Ctrl-X / abort key) during ZMODEM and XMODEM returns to the prompt.
- Tab completion: the terminal sends Tab (0x09) rather than expanding it locally, and the backspace redraw looks right.
- Wildcards: `get *.EXE` with auto-ZMODEM receives every match without further keys.
- Interrupted transfers: kill the emulator or drop the link mid-transfer. The host must log the loss, reopen the port, and present a fresh banner; a partial file is never reported as complete.
- Integrity: compare received files with the host originals byte-for-byte (ZMODEM) or after truncation to the reported length (XMODEM).
- Path traversal: typed paths are resolved lexically and cannot climb above the top; the `files` layer then refuses `..`, hidden names, and symlinks that escape the root or lead to hidden files. The `files` unit tests cover direct traversal attempts.

**Tier 2/3.** Repeat auto-download and fallback on 86Box and on `smokey`/NuXT with real UARTs, at 9600 baud first, then higher rates. Windows 95 HyperTerminal on `smokey` has been verified to auto-download ZMODEM from `sprocket serve` at 38400 baud, with matching MD5 sums (2026-10-20). Its manual XMODEM and XMODEM-1K receives through the menu were also verified (MD5 match after truncation), with the menu returning after each. `sprocket send` over XMODEM to HyperTerminal also matched by MD5 after truncation, and a cancel in HyperTerminal mid-transfer was reported cleanly by the sender. Cancellation through the menu remains to be checked, and DOS-mode terminals still need testing.

The first ZMODEM download after an XMODEM receive was skipped by HyperTerminal (#3). A `--trace` capture showed byte-identical ZFILE frames for the skipped attempt and the successful retry: HyperTerminal answered the first with ZSKIP and the second with ZRPOS. Setting *Transfer → Receive File* back to Zmodem after the XMODEM receive avoided the skip. When retesting the bootstrap-then-serve sequence, reset that setting, or expect and accept one skip.

## Open questions

- Exact DOSBox-X and 86Box versions, launch/configuration commands and ROM provisioning strategy.
- Whether emulator CI can run reliably without an interactive display.
- Which disk formats and geometries to use for the first canonical fixtures.
- Whether to support PTY as a first-class host CLI transport or only as a test adapter.
