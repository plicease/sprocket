// SPDX-License-Identifier: MIT
//! Host side of disk imaging (`docs/imaging.md`): the progress map, the
//! manifest, and the capture that fills an image from a native client's
//! disk, resuming where an earlier run stopped.

pub mod capture;
pub mod manifest;
pub mod map;
