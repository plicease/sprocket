# libc16 migration: runtime inventory

Issue [#27](https://github.com/plicease/sprocket/issues/27), task 1: what
`SPROCKET.EXE` and `BENCH.EXE` take from a C runtime, and how that compares
with [libc16](https://github.com/gr-dos/libc16) **v0.17** (latest, 2026-11-19).
The build now pins **v0.18**, which added three of the gaps found here; see
"libc16 v0.18" below.

## Method

Built at `e54296a` with the pinned Open Watcom (`2026-10-01-Build`, the same
snapshot libc16 v0.17 and libkitchen16 v0.02 pin) and the Makefile's flags
plus `-fm`. Then each object was linked alone with `option nodefaultlibs`;
the undefined references, less the symbols SPROCKET's own objects define,
are its runtime imports. The real maps give the Watcom members those imports
pull in. Call sites are from `grep` of the sources.

Baseline: `SPROCKET.EXE` 45,944 bytes (memory size 72,800, stack 8,192);
`BENCH.EXE` 13,370 bytes. They link 136 and 69 members of `clibs.lib`.

## Direct imports, per object

| Object | Imports (Watcom symbol names) |
|---|---|
| `frame`, `proto`, `crc32` | none (`_small_code_` only) |
| `storage` | `__U4M` |
| `req` | `__I4D __I4M __U4D __U4M` |
| `tree` | `fopen fclose fread fwrite fseek remove rename strrchr` |
| `place` | `remove rename strrchr` |
| `shell` | `printf sprintf memmove strcspn strncmp strrchr`; `__U8DQ __U8DR __U8M` |
| `cli` | `printf sprintf fopen fclose fread fwrite fputc fflush remove getcwd signal strcspn strncat strpbrk ___iob`; `__U4D __U4M __U8DQ __U8M` |
| `biosdisk` | `int86 int86x segread sprintf`; `__U4D __U4M` |
| `dosfs` | `_dos_findfirst _dos_findnext _dos_findclose _dos_open _dos_close _dos_setftime strnicmp errno _doserrno` |
| `dosport` | `_bios_keybrd isatty read printf strnicmp`; `__I4D` |
| `main` | `_cstart_ __argc printf getenv atol` |
| `bench` (test) | `_cstart_ __argc printf`; `__U4D __U4M` |

`BENCH.EXE` is `frame proto dosport bench`, so it needs only startup,
`printf`, `_bios_keybrd`, `isatty`, `read`, `strnicmp` and the 32-bit helpers.

Not in the list because Watcom inlines them under `-ox` with its own headers:
`inp`/`outp` (UART access, every byte), and `memcpy memset memcmp memchr
strlen strcpy strcmp strchr`. `MK_FP`, `FP_SEG`, `FP_OFF` are macros. The
first build against libc16's headers found `memchr` and `<stdint.h>`
missing; both are in the table below.

## Against libc16 v0.17

**Provided, same interface:** startup (`_cstart_`, `__argc`), `exit`;
`fopen fclose fread fwrite fseek fflush fputc putchar remove rename`,
`stdout`; `getcwd` (`<direct.h>`), `getenv`, `isatty`, `read`; `strrchr
strcspn strncat strpbrk strncmp strnicmp memmove` and the inlined string
functions; `int86 int86x segread`, `MK_FP FP_SEG FP_OFF`, `_dos_findfirst
_dos_findnext _dos_open _dos_close`, `struct find_t`, `_A_*`; `errno`;
`signal` with `SIGINT` (Ctrl-C via INT 23h); every 32- and 64-bit helper
listed above (`__I4D __I4M __U4D __U4M __U8DQ __U8DR __U8M`).

**Gaps:**

| Need | Where | libc16 | Proposed |
|---|---|---|---|
| `printf`, `sprintf` | 107 + 12 calls, cli/main/shell/biosdisk/dosport/bench | not provided, by design (decision 3) | see "Formatted output" |
| `inp`, `outp` | dosport, per received byte | not in `<conio.h>` | inline `#pragma aux` `in`/`out` in `client/dos` — must stay inline for the receive-path timing |
| `_bios_keybrd` (`<bios.h>`) | dosport: Esc check and keys | no `<bios.h>` | local INT 16h wrapper in `dosport.c` (or small `#pragma aux`); a libc16 `_bios_keybrd` is also reasonable (Microsoft C 5.1) |
| `_dos_setftime` | dosfs: set mtime after upload | missing | local inline INT 21h 5701h in `dosfs.c`, on a read-only handle; requested upstream as gr-dos/libc16#37, **added in 0.18** |
| `_dos_findclose` | dosfs | missing | drop: a no-op on DOS (Watcom/OS2 compatibility only) |
| `_doserrno` | `fs_error_code()`, the DOS code sent to the host after a failed `fopen`/`fwrite`/`rename` | not provided, by design (**added in 0.18**, gr-dos/libc16#38) | send `errno` (as the POSIX build already does), or wrap those calls with own DOS calls; libc16 maps most codes to `EACCES`/`ENOENT`, so detail is lost. Note Watcom's `_doserrno` is already stale after a short `fwrite` (disk full is not a DOS error) |
| `atol` | main: `PORT`/baud parsing | only `atoi`, `strtol` | `strtol(s, NULL, 10)` |
| `<bios.h>`, `<i86.h>` | dosport, biosdisk | headers absent | include `<dos.h>` instead |
| `<stdint.h>` | most headers | absent | `client/dos/include/stdint.h`, the exact-width types for Open Watcom's 16-bit targets |
| `memchr` | tree: path splitting | missing (C89) | a local loop in `tree.c`; **added in 0.18** (gr-dos/libc16#39) |

**Interface differences to handle:**

- **Calling convention.** libc16's functions are `__cdecl`; callbacks are
  `__cdecl` function pointers. `cli.c`'s `on_break` must be declared
  `__cdecl` or it will not compile against `<signal.h>`.
- **String intrinsics are lost.** With libc16's headers `-ox` emits
  `call _memcpy`, `_strlen` etc. instead of inline code (checked with `wdis`).
  None is on the per-byte receive path (`frame.c`, `port_get`); `proto.c`
  calls `memcpy` once per frame. Expect a size change and no timing change,
  but BENCH will confirm. It did, for the receive path: on the NuXT at 4.77
  and 9.54 MHz, the per-byte time and frame check are the same as with
  Watcom's runtime. The end of a frame (CRC, handling, encoding a PING's
  response) is about 8.7% slower: 137.3 ms against 126.3 ms at 9.54 MHz,
  and 277.3 against 255.4 ms at 4.77 MHz, most likely from that `memcpy`. See
  [testing](testing.md#libc16-build-2026-11-20).
- **Unbuffered streams.** libc16 streams start unbuffered; Watcom's are
  buffered. Uploads (`fwrite` of one DATA payload) and downloads (`fread`)
  already move a payload per call, so DOS call counts barely change. Console
  output would, if each character were written alone; but the local
  `printf` hands each message to `fwrite` in 64-byte blocks, and the only
  other console output is `putchar` echoing a key. So no `setvbuf` (which
  would link the heap).
- **Library name.** The small-model library is `LIBC16.LIB`, not
  `LIBC16S.LIB` as the issue says.
- **Toolchain.** libc16 v0.17 pins the same Open Watcom snapshot
  (`2026-10-01-Build`, same SHA-256) that `client/dos/fetch-watcom` already
  uses, so no toolchain change.
- **Signals.** As with Watcom, `SIGINT` arrives only inside a DOS call that
  checks Ctrl-C; the UART polling loop makes none, so Esc (BIOS) stays the
  reliable way to stop. No behaviour change expected.

## Formatted output

The formats SPROCKET uses are integers and strings only: `%s %c %d %u %ld
%lu %x %X %lx`, with `0` padding (`%02X %04X %02lu`), widths (`%6s %7ld
%8lu`), `-`, and `*` widths and precisions (`%*s %.*s %-*.*s`). No floating
point anywhere.

**Option A — libkitchen16 v0.02** (`printf`, `sprintf`, `vsprintf`,
`snprintf`). Call sites unchanged; add `<kitchen16.h>` and link `LIBK16.LIB`
before `LIBC16.LIB`. Built against libc16 0.07 (newer is supported) with the
same Watcom snapshot. Costs: the engine is linked whole including its exact
`%e %f %g` code, because it cannot tell at link time that no `double` is
passed — about 6 KB (`fmt` 6,017 object bytes, `vfprintf` 482, `sprintf`
347). Stack: `fprintf` measures 806 bytes, which must be checked against the
`ls -R` recursion within the 8 KB stack. A second pinned dependency, and its
members must be allowed in the link-map check. Output goes to `fwrite` in
blocks of up to 128 characters, so it is efficient even unbuffered.

**Option B — replace `printf`/`sprintf` in SPROCKET.** Two ways:

1. A small local formatter in `client/dos/` (MIT, SPROCKET's own code)
   implementing exactly the subset above — a few hundred bytes, no floating
   point — behind `printf`/`sprintf`-compatible names used only by the DOS
   build (the host build keeps the system's). Call sites unchanged. libc16's
   `libc16_ultoa`/`libc16_ltoa` can do the digits.
2. Rewrite the ~119 call sites as `fputs` plus `libc16_ultoa` helpers. The
   smallest code, but a large, error-prone diff to `cli.c`/`shell.c`, which
   are shared with the host build and its tests.

**Decision:** B1. The subset is small and fixed, it saves the ~6 KB
and the stack of a float-capable engine on a program that never formats a
`double`, keeps one runtime dependency, and leaves the shared sources and the
host tests untouched. It is SPROCKET-specific, so it belongs in
`client/dos/`, not libc16, which the issue allows. Option A remains the
fallback if the subset grows, and is the better choice if SPROCKET ever
needs floating point.

## Decisions

- **Formatting:** a local integer-and-string formatter in `client/dos/`
  (option B1), not libkitchen16.
- **`_dos_setftime`:** libc16's (0.18), opening the file read-only so that
  read-only files can be stamped too (Watcom's build opened it read-write,
  which failed on them; libc16's spec confirms 5701h needs no write access).
  The time is set last, just before the close, as libc16 advises for
  DOSBox-X. Until 0.18 this was a local inline 5701h call.
- **`fs_error_code()`:** returns `_doserrno` (0.18), the DOS error code, as
  the Watcom build did. Until 0.18 it returned `errno`.
- **`memchr`:** libc16's (0.18). Until 0.18, `tree.c` had a local loop.

## libc16 v0.18

0.18 added `memchr` (gr-dos/libc16#39), `_doserrno` (#38) and
`_dos_getftime`/`_dos_setftime` (#37) for this migration, and SPROCKET uses
the first three, restoring its pre-migration calls. Nothing in SPROCKET reads a file's
time from an open handle (`fs_stat` and the directory listing take it from
`struct find_t`), so `_dos_getftime` has no caller. libc16's `_doserrno` is
set only when a DOS call fails, as Watcom's was in effect: after an error
the runtime finds itself (a short write's `ENOSPC`) it is stale, so the
code sent to the host can be from an earlier failure, as before.

## Proposed additions to libc16

`_dos_getftime`/`_dos_setftime`, `_doserrno` and `memchr` were upstreamed
in 0.18.
`_bios_keybrd` is a candidate if a second consumer wants `<bios.h>`;
otherwise keep it local. Everything else above is handled in `client/dos/`.
