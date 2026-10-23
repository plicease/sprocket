# Native protocol v1.1 — proposed: file trees and either-way roles

**Status:** Proposed on 2026-10-23 for the file-transfer shell in [sprocket-file-transfer-ui](sprocket-file-transfer-ui.md). It has not been ratified; the first implementations follow it so it can be reviewed against working code.

This is minor version 1 of [protocol v1](protocol-v1.md). Everything in v1 stays as ratified: framing, CRC, sequencing, retransmission, duplicate handling, `PENDING`, sessions, versioning and the file-write procedure. Every addition here follows v1's [extension rules](protocol-v1.md#extension-rules), so a v1.0 peer still works: it answers the new requests with `UNSUPPORTED`, and never advertises the new capability.

## 1. Roles

v1 names its two ends by machine: the Linux **host** sends requests and the vintage **client** answers them. v1.1 names them by role instead, so that either machine can take either one:

- The **requester** sends requests, one at a time, and owns the session. Everything v1 says the host does, the requester does: it chooses the session ID and sends `HELLO`, retransmits, and waits for each response.
- The **responder** answers. Everything v1 says the client does, the responder does: one response per request, `PENDING` for slow work, the duplicate cache, and the session rules.

In the file-transfer shell, the interactive **client** is the requester and the **server** is the responder, whichever machine each one is. `sprocket hello` and disk imaging keep Linux as the requester.

The v1 design premise "the host leads; the client answers" becomes "the requester leads; the responder answers", and the reasons for it still hold. A vintage machine acting as requester sends a request and then only polls for the response, so the line is idle whenever it prints or reads the keyboard; it never needs to receive and do something slow at the same time.

`platform` values gain `3` = Linux (or another Unix), so a Linux responder can describe itself in `HELLO_REPLY`.

## 2. Capability and version

| Bit | Name | Meaning |
| --- | --- | --- |
| 5 | `FILE_TREE` | The responder serves a directory tree: `STAT`, `DIR_LIST` and `FILE_READ` are supported, and **every path in a file request, including `FILE_BEGIN`'s, is a tree path** (section 3) rather than a v1 client-native path. |

A responder serving a tree advertises `FILE_WRITE` only when writing has been turned on (`--writable`, `/W`), and `FILE_REPLACE` only with it. A read-only server therefore never advertises either, and a requester tells the user that the server is read-only without sending anything.

The requester uses the requests and error classes below only when the session's minor version (the lower of the two) is at least 1 **and** the responder advertised `FILE_TREE`.

## 3. Tree paths

A tree path names an entry under the responder's root, which the user sees as `/`:

- ASCII `0x20`–`0x7E`, components separated by `/`, at most 127 bytes. A leading `/` is allowed and means the same as none. The empty path (or `/`) is the root.
- The requester resolves `.` and `..` itself (stopping at the root) before sending. A responder refuses a path with an empty, `.` or `..` component, a `\` or `:` anywhere, or a component starting with `.`, with `NOT_FOUND`. Hidden names are never reachable, as in terminal mode.
- The responder confines every path to its root, including through symbolic links, and does not rely on comparing string prefixes.
- **Case:** each component matches the entry with exactly that name if there is one; otherwise the one entry whose name differs only in case. Several such entries give `AMBIGUOUS`. Responses spell names as they are on the responder. (A DOS responder gets this from DOS, whose names are case-insensitive.)
- A responder may refuse names its filesystem cannot hold, with `BAD_NAME`. A DOS responder refuses names that are not 8.3, and the DOS device names (`CON`, `AUX`, `PRN`, `NUL`, `CLOCK$`, `COM1`–`COM9` and `LPT1`–`LPT9`, with or without an extension), since opening one of them reaches a device rather than a file.

## 4. Times

`mtime` fields are a DOS date and time packed into a u32: bits 31–25 the year minus 1980, 24–21 the month (1–12), 20–16 the day, 15–11 the hour, 10–5 the minute, 4–0 the seconds divided by 2. This is local time on the machine where the file lives, as DOS keeps it. `0` means unknown.

A Linux peer converts with its local time zone. Times before 1980 or after 2107 are sent as `0`. A DOS peer copies the field to and from DOS unchanged.

## 5. Error classes

Added to the `ERROR` classes of [v1 section 4](protocol-v1.md#error-0xff), and sent only in sessions at minor version 1 or later. `FILE_ERROR` remains for every other filesystem failure, with the platform code and a message.

| Class | Name | Use |
| --- | --- | --- |
| 7 | `NOT_FOUND` | No such entry, or a path the responder refuses (section 3) |
| 8 | `NOT_A_DIRECTORY` | A directory was needed, or a path goes through a file |
| 9 | `IS_A_DIRECTORY` | A file was needed |
| 10 | `EXISTS` | `FILE_BEGIN` without the replace flag, and the destination exists |
| 11 | `READ_ONLY` | A write on a responder that does not allow writing |
| 12 | `AMBIGUOUS` | Several names differ only in case (section 3) |
| 13 | `BAD_NAME` | The responder's filesystem cannot hold this name |

The requester shows these with the same words as terminal mode ("No such file or directory", "File exists", and so on), so a listing or a failed `get` reads the same against any server.

## 6. Messages

Numbers come from the range v1 reserved for reads and listings (`0x18`–`0x1F`).

| Type | Name | Capability | Request payload | Response payload |
| --- | --- | --- | --- | --- |
| `0x18` | `STAT` | `FILE_TREE` | `path_len` u8, `path` | one entry (below) |
| `0x19` | `DIR_LIST` | `FILE_TREE` | `index` u16, `path_len` u8, `path` | `flags` u8, `count` u8, entries |
| `0x1A` | `FILE_READ` | `FILE_TREE` | `offset` u32, `length` u16, `path_len` u8, `path` | `size` u32, `mtime` u32, data |

### Entries

`STAT` responses and `DIR_LIST` responses describe entries in the same layout. Each entry is length-prefixed, so a later minor version can append fields to it:

| Field | Type | Meaning |
| --- | --- | --- |
| `entry_len` | u8 | bytes in this entry after this field |
| `kind` | u8 | 0 = file, 1 = directory |
| `size` | u32 | file length in bytes; 0 for a directory |
| `mtime` | u32 | section 4 |
| `name_len` | u8 | length of `name` |
| `name` | ASCII | `DIR_LIST`: the entry's name. `STAT`: the whole tree path as spelled on the responder, without a leading `/` (empty for the root). |

A receiver skips any bytes of an entry after `name`.

### `STAT`

Looks up a path, ignoring case as section 3 says. It answers `NOT_FOUND`, `NOT_A_DIRECTORY` (a component before the last is a file) or `AMBIGUOUS`. The requester uses it for `cd`, for `ls` of a single name, and before `get`.

### `DIR_LIST`

Lists the directory at `path`, starting at entry `index` (counting from 0) in the responder's order. The response holds as many entries as fit in the negotiated payload, `count` of them; `flags` bit 0 means more entries follow, and the requester asks again with `index` increased by `count`. Other flag bits are reserved and sent as 0.

- The responder's order must be the same from one request to the next while the directory is unchanged; it need not be sorted. The **requester** sorts the whole listing (alphabetically ignoring case, then by exact name), so every server's listings look the same.
- A listing never includes `.`, `..`, hidden names, special files (devices, sockets), names that are not ASCII `0x20`–`0x7E`, or entries that resolve outside the root.
- A directory that changes between requests can make entries repeat or go missing in the listing. The requester drops repeated names. Listings are for display and wildcard matching; a missed file is reported as not found when it is fetched.
- Each request scans from the start of the directory, so a DOS responder keeps no state between requests. A directory of a few hundred entries takes a few requests.

`path` must be a directory: otherwise `NOT_A_DIRECTORY`.

### `FILE_READ`

Reads up to `length` bytes of the file at `path` from `offset`. `length` is at most the negotiated payload minus 8. The response carries the file's current `size` and `mtime`, then the data: exactly `length` bytes, or fewer only where the file ends (none at or after the end).

Each request opens the file, reads and closes it, so the responder keeps no state, and a retransmission or a lost response needs nothing beyond v1's duplicate handling. The requester reads from offset 0 upward and checks that `size` and `mtime` stay the same in every response; if they change, it reports that the file changed during the transfer and discards what it received. Each frame's CRC-16 protects the data on the line, as for every other message, and the requester checks that it received exactly `size` bytes.

`path` must be a file: otherwise `IS_A_DIRECTORY`.

### Writing (`put`)

`FILE_BEGIN`, `FILE_DATA`, `FILE_COMMIT` and `FILE_ABORT` work as in [v1 section 5](protocol-v1.md#5-file-transfer-and-exact-length), with a tree path, and these additions:

- **Trailing field in `FILE_BEGIN`:** `mtime` u32 after `path`. The responder gives the file this time when it activates it at `FILE_COMMIT`; `0` leaves the time it was written.
- The directory part of the path must exist (`NOT_FOUND`, `NOT_A_DIRECTORY`). If the last component matches an existing file (ignoring case), that file is the destination: without the replace flag the answer is `EXISTS`, and with it the file is replaced. If it matches a directory, the answer is `IS_A_DIRECTORY`. Otherwise a new file is created with the name as given.
- A responder that does not allow writing answers every write request with `READ_ONLY` and changes nothing. (It also does not advertise `FILE_WRITE`, so a correct requester never asks.)
- Temporary and backup names belong to the responder. A DOS responder uses v1's `NAME.$$$` and `NAME.BAK`. A Linux responder writes a hidden temporary file, `.NAME.sprocket`, in the destination directory and renames it over the destination, which replaces it atomically; it keeps no backup, so its `found` bit 1 is never set. Hidden names are never listed, and a leftover temporary file is deleted at the next `FILE_BEGIN` for that name.

The requester (the client, for `put`) checks for an existing destination by sending `FILE_BEGIN` without the replace flag and reporting `EXISTS`; `put -f` sets the flag.

### Ending a session

The requester sends `CLOSE` when the user quits. A responder serving a tree stays ready for the next `HELLO` after `CLOSE`, a new `HELLO`, or a link failure; it does not exit.

## 7. The shell's requests

| Command | Requests |
| --- | --- |
| `cd DIR` | `STAT DIR`; must be a directory |
| `ls [PATH]` | `STAT PATH`, then `DIR_LIST` pages for a directory; `-R` lists each subdirectory in turn |
| `get FILE` | `STAT FILE`, then `FILE_READ` from 0 to the end |
| `get PATTERN` | `DIR_LIST` of the pattern's directory, matched by the requester, then each file as above |
| `put FILE` | `FILE_BEGIN`, `FILE_DATA` from 0 to the end, `FILE_COMMIT` |

Wildcards are matched by the requester against the listing. That keeps the responder small, which matters most for a DOS server, and the result is the same as matching on the server: `*` and `?` in the last component, ignoring case.

## 8. Test vectors

The new messages' frames are in `tests/vectors/frames.txt`, under names starting `tree_`, in v1's format.
