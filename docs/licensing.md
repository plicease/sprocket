# Licensing policy

**Status:** Agreed policy. The DOS client's runtime has been checked (libc16; see below). Other third-party redistribution checks remain required.

## Project license

All **original SPROCKET code** is licensed under the MIT License, including the Rust host, DOS client, future platform clients, build tooling, protocol specifications, and documentation.

The repository root should contain the canonical `LICENSE` text with the correct copyright holder/year supplied by the project owner. This design-document bundle does not invent that attribution or overwrite a license created by the user's Rust template.

Use `license = "MIT"` in Rust package metadata and optional `SPDX-License-Identifier: MIT` headers in source files.

## Third-party material

SPROCKET may use compatible third-party code under licenses other than MIT. Such material **retains its original license**; the project must honor notice, attribution, source-offer, and redistribution conditions where applicable. Compatibility for the intended distribution must be evaluated case by case, including static linkage.

Prefer permissive dependencies where practical. Record incorporated third-party source and required notices in `THIRD_PARTY_LICENSES` (or an equivalent file) when needed. Do not claim that every dependency is MIT-licensed.

## DOS client runtime

`SPROCKET.EXE` and `BENCH.EXE` link no Open Watcom runtime or startup code. Their C runtime is [libc16](https://github.com/gr-dos/libc16) (issue #27):

- **Release:** libc16 0.18, the small-model library `LIBC16.LIB`. `client/dos/fetch-libc16` pins the release archive by SHA-256 (`6656b488…b207`) and refuses any other.
- **License:** MIT, `Copyright (c) 2026 Graham Ollis`: the same text and holder as SPROCKET's own `LICENSE`. libc16 is written independently, not derived from Open Watcom's runtime. What SPROCKET needed and libc16 lacked was added upstream to libc16 (gr-dos/libc16#37, #38, #39) or written in `client/dos/` (the `printf` family in `src/dosfmt.c`, the UART and BIOS keyboard as inline `#pragma aux` code, `include/stdint.h`). See [the migration inventory](libc16-migration.md).
- **What the executables contain:** SPROCKET's own objects, and the libc16 members they use, including the startup code and the compiler's helper routines (`__U4D`, `__U4M` and so on), which libc16 provides.
- **Notice:** MIT requires the copyright and permission notice with copies of the software, and the executables contain a substantial part of libc16. Anyone redistributing `SPROCKET.EXE` (the release asset `sprocket.exe`, or GR-DOS as `C:\DOS\SPROCKET.EXE`) must include the notice. One copy of the root `LICENSE` covers both SPROCKET and libc16, as long as both keep the same holder and text. Each release attaches that copy beside `sprocket.exe`, as the asset `sprocket-exe-license.txt`. Before uploading, the release job checks that the root `LICENSE` and the pinned libc16's `LICENSE` are identical, and fails if they are not. If they ever differ, ship libc16's notice as well and update this section. GR-DOS, which ships `sprocket.exe` as `C:\DOS\SPROCKET.EXE`, must ship the notice with it.

### How this is checked

- The objects are compiled with `wcc -zl -zld`, so they carry no default-library records. `wlink` links them with `option nodefaultlibs` and names no library but `LIBC16.LIB`.
- `make check-map` (`client/dos/test/check-map`) reads each link map and fails unless every module is one of the program's own objects or a member of the pinned `LIBC16.LIB`. It also fails if either executable carries Open Watcom's runtime banner (`Open Watcom C/C++`). CI and the release job run it, and the release attaches nothing that fails.
- `make check-map-negative` relinks SPROCKET's objects twice, once with Open Watcom's own runtime and once with an extra object, and requires `check-map` to reject both.

### Toolchain

Open Watcom is still the compiler and linker: the `2026-10-01-Build` snapshot that libc16 0.18 pins, fetched by `client/dos/fetch-watcom` and checked by SHA-256. Using a compiler does not impose its license on the source it compiles, and with the checks above no Open Watcom runtime code reaches the executables. So the terms of the Sybase Open Watcom Public License for redistributing runtime code are not in play. JWasm builds libc16, but not SPROCKET.

### Tradeoffs measured

The migration roughly halved `BENCH.EXE`. Measured on a NuXT (NEC V20) at 4.77 and 9.54 MHz (details in [testing](testing.md#libc16-build-2026-11-20)):

- **Sizes:** `SPROCKET.EXE` is 32,678 bytes, against 45,944 with Open Watcom's runtime. `BENCH.EXE` is 6,414 bytes, against 13,538.
- **Speed:** the serial receive path is unchanged, and so are the highest reliable baud rates (38400 at 4.77 MHz, 57600 at 9.54 MHz).
- **Cost:** the end of a frame is about 8.7% slower, because libc16 0.18's `memcpy` copies a byte at a time (gr-dos/libc16#42).

## Other toolchains

Inspect the runtime and startup-library terms of any future client toolchain before distributing its binaries, as was done for Open Watcom above.

Rust dependencies should be audited prior to releases. A license scanner can help, but manual review may still be needed for exceptions or bundled native libraries.
