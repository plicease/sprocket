# Licensing policy

**Status:** Agreed policy; third-party redistribution checks remain required.

## Project license

All **original SPROCKET code** is licensed under the MIT License, including the Rust host, DOS client, future platform clients, build tooling, protocol specifications, and documentation.

The repository root should contain the canonical `LICENSE` text with the correct copyright holder/year supplied by the project owner. This design-document bundle does not invent that attribution or overwrite a license created by the user's Rust template.

Use `license = "MIT"` in Rust package metadata and optional `SPDX-License-Identifier: MIT` headers in source files.

## Third-party material

SPROCKET may use compatible third-party code under licenses other than MIT. Such material **retains its original license**; the project must honor notice, attribution, source-offer, and redistribution conditions where applicable. Compatibility for the intended distribution must be evaluated case by case, including static linkage.

Prefer permissive dependencies where practical. Record incorporated third-party source and required notices in `THIRD_PARTY_LICENSES` (or an equivalent file) when needed. Do not claim that every dependency is MIT-licensed.

## Toolchains

Using a compiler does not automatically impose its license on original source code. However, compiled programs may incorporate runtime or startup-library code, so inspect the relevant Open Watcom runtime redistribution terms **before distributing DOS binaries**. Apply similar diligence to other future client toolchains.

Rust dependencies should be audited prior to releases. A license scanner can help, but manual review may still be needed for exceptions or bundled native libraries.
