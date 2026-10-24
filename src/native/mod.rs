// SPDX-License-Identifier: MIT
//! The SPROCKET native protocol, v1 (`docs/protocol-v1.md`).
//!
//! Unlike the standard-protocol senders, this talks to the SPROCKET client
//! running on the vintage machine.

pub mod disk;
pub mod frame;
pub mod link;
#[cfg(test)]
mod mock;
pub mod server;
pub mod session;
pub mod storage;
pub mod tree;
