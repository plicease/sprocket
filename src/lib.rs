// SPDX-License-Identifier: MIT
//! SPROCKET host library: byte-stream transports, standard serial
//! file-transfer protocols, the interactive terminal file server, and the
//! native protocol.

pub(crate) mod console;
pub mod files;
pub mod native;
pub mod serve;
pub(crate) mod shell;
pub mod trace;
pub mod transport;
pub mod xmodem;
pub mod zmodem;
