# Sprocket file-transfer UI design

**Status:** Command names and transfer policies decided 2026-10-23; implemented 2026-10-23 in `sprocket serve`, `sprocket client` and `SPROCKET.EXE` (`SERVER` and `CLIENT`)  
**Date:** 2026-10-23  
**Scope:** Native Sprocket and terminal (XMODEM/ZMODEM) file-transfer interfaces

## Goals

Provide a consistent FTP-like command interface for both native Sprocket and the existing terminal-based server. The existing `sprocket serve` terminal UI is the reference for presentation, directory listing layout, and common behavior.

Both modes **recognize the same command vocabulary**. Commands unavailable in one mode must produce an explicit, mode-specific error rather than being ignored or treated as unknown.

In native mode, **either endpoint can be the server or the interactive client**, and the client can both `get` and `put`. Terminal mode remains download-only, for machines using an ordinary terminal program such as HyperTerminal, Telix, or Procomm Plus.

## Server command line

```sh
# Native is the default
sprocket serve --port /dev/ttyS0 --baud 57600 --root ~/dos

# Explicit modes
sprocket serve --mode native --port /dev/ttyS0 --baud 57600 --root ~/dos
sprocket serve --mode terminal --port /dev/ttyS0 --baud 38400 --root ~/dos
```

`--mode` selects the interface, not a file-transfer protocol. The interactive `protocol` command selects XMODEM, XMODEM-1K, or ZMODEM in **terminal mode only**. `sprocket send` remains a separate bootstrap facility.

**Compatibility:** Today, `sprocket serve` defaults to terminal mode. Changing its default to native is an intentional breaking CLI change. Update help, documentation, and examples; explain that existing terminal workflows must add `--mode terminal`.

## Native roles

- **Server:** exposes a rooted filesystem and responds to requests.
- **Client:** initiates requests and provides the interactive shell.
- **Transport:** serial connection, independent of the UI and OS.

Either Linux/Rust or DOS can run either native role. The roles stay fixed for the session: the client initiates requests; the server responds. File transfer in either direction does **not** require both peers to initiate commands simultaneously.

Invocations (decided 2026-10-23):

```text
Linux server: sprocket serve --mode native --root ~/dos --port /dev/ttyS0 [--writable]
DOS client:   SPROCKET CLIENT COM1 38400

DOS server:   SPROCKET SERVER COM1 38400 C:\SHARE [/W]
Linux client: sprocket client --port /dev/ttyS0 --baud 38400
```

The DOS program is one `.EXE` with subcommands. Both roles share the serial, framing, CRC and file code, so one program is smaller than two, and bootstrap sends a single file. With no subcommand it prints its usage. `SPROCKET SERVER` also answers `HELLO` and `PING`, so `sprocket hello` works against it.

The existing native v1 host-driven session assumption will need to be generalized for role reversal while retaining its framing, CRC, sequencing, retransmission, and duplicate handling. [Protocol v1.1](protocol-v1.1.md) (ratified on 2026-10-23) does this, and adds the directory, stat and read requests.

## Shared commands

Every interface recognizes every command. **Error** means recognized but unavailable in that mode.

| Command | Terminal | Native | Meaning |
|---|---|---|---|
| `ls [-R] [PATH]` | Yes | Yes | List remote entries; `-R` recursively |
| `dir [-R] [PATH]` | Yes | Yes | Exact alias for `ls` |
| `cd [DIR]` | Yes | Yes | Change remote directory; no argument goes to root |
| `pwd` | Yes | Yes | Show remote directory |
| `get PATTERN` | Yes | Yes | Download remote file(s), including wildcards |
| `put PATTERN` | **Error** | Yes | Upload local file(s), including wildcards |
| `lls [PATH]` | **Error** | Yes | List local entries |
| `ldir [PATH]` | **Error** | Yes | Alias for `lls` |
| `lcd [DIR]` | **Error** | Yes | Change local directory |
| `lpwd` | **Error** | Yes | Show local directory |
| `protocol [NAME]` | Yes | **Error** | Show/set XMODEM, XMODEM-1K, ZMODEM |
| `set editing on\|off` | Yes | Platform-dependent | Toggle line editing, or explicitly error |
| `help`, `?` | Yes | Yes | Show vocabulary and availability |
| `quit`, `bye`, `exit` | Yes | Yes | End session |

`get` always means **remote → local**; `put` means **local → remote**, regardless of which machine is the server.

Errors distinguish unsupported commands from unknown commands:

```text
/> protocol zmodem
protocol: not available in native mode

/> put *.EXE
put: not available in terminal mode

/> lpwd
lpwd: not available in terminal mode

/> frobnicate
Unknown command: frobnicate
Type help for a list of commands.
```

`help` should show the full command vocabulary and mark commands unavailable in the active mode. Recognized commands with invalid arguments should show usage errors.

## Appearance and listings

The **current terminal UI is canonical**. Native clients should match:

- Alphabetical order and one entry per line.
- Directories marked with a trailing `/`, without sizes.
- Human-readable file sizes in powers of 1024 (`512B`, `1.5kB`, `12MB`).
- Exact column widths, spacing, recursive `ls -R` presentation, and directory path formatting.
- The existing `/>`-style prompt, reflecting the remote working directory.
- Case-insensitive matching where DOS-like behavior is expected, preserving actual spelling in output.
- Common command responses, errors, and completion displays where feasible.

`lls` and `ldir` use the same listing format for local files. Native directory responses should contain **structured metadata**, not preformatted strings; each client formats them locally. Use shared golden-output fixtures to ensure Rust and DOS implementations agree with terminal-mode output.

## Wildcards and transfers

There are **no `mget` or `mput` commands**. `get` and `put` accept either a filename or `*`/`?` patterns:

```text
/> get DF.EXE
/> get *.EXE
/> get TOOLS/*.COM
/> put README.TXT
/> put *.EXE
/> put GAMES/*.DAT
```

- Expand wildcards on the **source side**: server for `get`, interactive client for `put`. For native `get` the client matches against the server's listing, which keeps a DOS server small and gives the same result ([protocol v1.1](protocol-v1.1.md#7-the-shells-requests)).
- Match case-insensitively for DOS-compatible behavior; handle case-ambiguous names safely.
- Report no matches explicitly.
- Transfer multiple matches in a predictable order, preferably directory-listing order.
- Show results/progress per file; do not recurse into directories by default.
- Preserve exact filename, length, and bytes in native mode.
- Report errors/cancellation and keep the session usable where possible.
- Preserve existing terminal-mode XMODEM/ZMODEM `get` behavior unless deliberately revised.

Decided 2026-10-23:

- **Overwriting:** `get` and `put` refuse to replace an existing file unless asked explicitly with `-f` (`get -f PATTERN`, `put -f PATTERN`). There is no prompting.
- **Multiple files:** the first failed file stops the rest. In terminal mode a file the receiver declines (a ZMODEM skip) is not a failure, as today.
- **Modification times:** native transfers carry each file's modification time and set it on the copy. On the wire it is a DOS date and time (local time, 2-second steps, 1980 to 2107), with 0 meaning unknown; the Linux side converts with its local time zone. A file dated before 1980 is sent as unknown.
- **Atomic writes:** both sides write to a temporary file and rename it into place only after the length and CRC check, as `FILE_COMMIT` already requires.

**Still to decide:** destination path and rename syntax (files keep their names and go to the current directory); resume support.

## Root confinement and permissions

Each server exposes only its configured root, shown remotely as `/`:

```text
Linux root: /home/user/dos  -> remote /TOOLS/DF.EXE
DOS root:   C:\SHARE        -> remote /GAMES/DOOM.EXE
```

Prevent `..` traversal, absolute-path and DOS drive-relative escapes, and symlink escapes where applicable. Avoid naive string-prefix checks. Follow existing terminal-mode hidden-file rules where applicable. Document the trusted-root and concurrent-modification/TOCTOU assumptions.

Servers should support read-only operation. Native `put` requires server-side write permission, which is **off unless turned on explicitly**: `--writable` for `sprocket serve`, `/W` for `SPROCKET SERVER`. A read-only server refuses `put` with an explicit message. Terminal mode remains read-only, and `put` always errors there.

## Editing and completion

| Feature | Linux/Rust native client | Vintage native client (DOS first) | Terminal UI |
|---|---|---|---|
| Command history | Required | Optional | Already implemented |
| Tab completion | Required | Optional | Already implemented |
| Line editing | Required | Basic typing/Backspace/Enter sufficient | Already implemented |
| Ctrl-D at empty prompt | Required | Optional | Already implemented |
| Ctrl-C cancellation | Required where feasible | Optional | Existing behavior |

Linux completion covers command names, local paths, and remote paths. Remote completion may cache listings to avoid excessive serial traffic. Persistent history is desirable, not required initially; a Rust library such as `rustyline` is an implementation choice.

DOS and future vintage clients may omit history, completion, and advanced editing when too expensive. No ANSI terminal dependency is required. **Functional compatibility is required; interactive conveniences are platform-dependent.** If `set editing` is not supported, recognize it and explicitly report that it is unavailable.

The Linux client reuses the terminal server's line editor on its own terminal (in raw mode), rather than a library such as `rustyline`, so its prompt, completion display and editing keys are the same as terminal mode's.

## Implementation boundaries

Separate command parsing/dispatch, listing formatting, filesystem operations, and protocol transport. The native wire protocol exchanges structured filesystem operations/results, not prompts or keystrokes. Use bounded-memory file streaming suitable for DOS-class systems. Keep native framing/reliability mechanisms intact while supporting either endpoint as request initiator. Share command-vocabulary and golden-format tests across implementations.

## Acceptance criteria

1. `serve` defaults to native; `--mode native` and `--mode terminal` both work.
2. Terminal mode retains its XMODEM/ZMODEM transfers and `protocol` selection.
3. Both interfaces recognize the full common vocabulary; `dir` aliases `ls`.
4. Unsupported commands yield explicit mode-specific errors, distinct from unknown commands.
5. Equivalent listings, including `ls -R`, use matching presentation.
6. Native `get`/`put` accept literal filenames and wildcards without `mget`/`mput`.
7. Linux and DOS can exchange native client/server roles, with client-relative `get`/`put` semantics.
8. Root confinement and write authorization are enforced.
9. Linux native shell has history, tab completion, and line editing; DOS may be minimal.
10. Native transfers preserve exact file data and metadata; failures are reported and recoverable where feasible.
11. Tests cover vocabulary, unsupported errors, listing format, wildcard handling, root confinement, and integrity.

## Open decisions

- Destination/rename syntax.
- Resume behavior.
- Unattended-operation policies.

Decided on 2026-10-23: the command names above, write permission off by default, `-f` to overwrite, stopping at the first failed file, modification times as DOS dates, and atomic writes. `set editing` works in the Linux client; the first DOS client recognizes it and reports that it is unavailable.
