# Bootstrap, legacy transfer, and client deployment

**Status:** Draft.

## Core requirement

The Rust SPROCKET host **must implement standard serial file transfer directly**, without spawning or requiring `sz`, `sx`, `lrzsz`, or other transfer executables. This is a permanent feature useful even when the target has no SPROCKET client.

## Initial compatibility

- Built-in XMODEM and XMODEM-1K **send** operations, including checksum/CRC negotiation as required for compatibility.
- Later candidates: YMODEM (filenames and exact lengths), ZMODEM, receive direction, and Kermit if demand justifies it.
- The initial real-world target is Windows 95 HyperTerminal on `smokey` connected to `twintwist` over a null-modem cable at 9600 8N1, no flow control.

**Proposed CLI** (syntax not final):

```sh
sprocket send --protocol xmodem --port /dev/ttyS0 --baud 9600 SPROCKET.EXE
sprocket send --protocol xmodem-1k --port /dev/ttyS0 --baud 9600 SPROCKET.EXE
```

The terminal receiver must already be in receive mode. HyperTerminal supports Xmodem and 1K Xmodem. The host should print clear progress, retries, and success/failure. Never treat completion of serial transmission alone as proof that the received file is byte-identical.

### XMODEM exact-length limitation

Classic XMODEM sends fixed-size blocks padded at EOF, with no standard exact file length or filename. A received executable can therefore contain trailing padding. Do not claim byte-exact deployment or automatic integrity verification through XMODEM alone. Prefer YMODEM or a native transfer once available, or provide a documented size/hash verification procedure. A tiny bootstrap receiver can implement a format that includes an explicit length.

## Initial installation paths

There is no universal zero-software bootstrap across all vintage machines. Document multiple options per platform:

1. Existing terminal software + built-in SPROCKET XMODEM sender (first demonstration).
2. Minimal receiver entered or delivered through platform-available facilities (e.g. DOS DEBUG script or BASIC program, where available).
3. Removable media or existing network transfer when practical.

Initial installation should not require the full native client to already be installed.

## Persistent bootstrap receiver

**Proposal:** `SERLOAD.COM` for DOS, a small 8086-compatible receiver distinct from `SPROCKET.EXE`. Its purpose is to accept a length-delimited, integrity-checked file and write it safely. It may implement a small native deployment subset or another documented tiny protocol; it does not need disk-imaging commands. Its use is optional where HyperTerminal or other transfer software is available.

## Native deployment and upgrades

Once installed, SPROCKET should transfer arbitrary files with exact lengths and end-to-end integrity checks. Client upgrades must be failure-safe:

- Receive to a separate temporary filename, not the running executable.
- Verify full expected length and checksum/hash before activation.
- Preserve a known-good client or recovery receiver.
- Perform replacement only after the current program has exited or via a small separate loader.
- On interrupted transfer, retain the old executable; delete or resume incomplete temporary data explicitly.
- Avoid overwriting an active executable or assuming DOS rename/replace is atomic across all filesystems.

The host may eventually offer `sprocket install --client dos`, but automatic platform detection and deployment strategy are **future proposals**, not an initial requirement.

## Tests

- HyperTerminal XMODEM and XMODEM-1K receives a known test file.
- Lost ACK, NAK, timeout, CAN cancellation, duplicate block, checksum/CRC mismatch.
- Partial transfer never activates an incomplete client.
- A transferred DOS executable runs in real-mode DOS on `smokey`; eventually on NuXT.
- No Linux external transfer executable is needed.

## Open questions

- Do we prioritize YMODEM early to preserve exact lengths for the first client executable?
- Which minimal DOS bootstrap strategy works on systems without DEBUG or working floppy drives?
- How should native transfer resume after disconnection?
