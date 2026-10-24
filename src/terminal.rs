// SPDX-License-Identifier: MIT
//! The user's own terminal as a [`Transport`], so the line editor used on
//! the serial line also runs `sprocket client`. On a tty it switches to raw
//! mode (restored when dropped), so keys arrive one at a time and Ctrl-C
//! and Ctrl-D reach the shell as bytes.

use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::time::Duration;

use crate::transport::Transport;

/// Standard input and output.
pub struct Terminal {
    /// The settings to restore, when standard input is a tty.
    saved: Option<libc::termios>,
}

impl Terminal {
    pub fn new() -> io::Result<Self> {
        let fd = io::stdin().as_raw_fd();
        // SAFETY: isatty, tcgetattr and tcsetattr only read or write the
        // termios struct passed to them.
        if unsafe { libc::isatty(fd) } == 0 {
            return Ok(Self { saved: None });
        }
        let mut saved: libc::termios = unsafe { std::mem::zeroed() };
        if unsafe { libc::tcgetattr(fd, &mut saved) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let mut raw = saved;
        unsafe { libc::cfmakeraw(&mut raw) };
        if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { saved: Some(saved) })
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        if let Some(saved) = &self.saved {
            // SAFETY: as in `new`.
            unsafe { libc::tcsetattr(io::stdin().as_raw_fd(), libc::TCSANOW, saved) };
        }
    }
}

impl Transport for Terminal {
    fn read_byte(&mut self, timeout: Duration) -> io::Result<Option<u8>> {
        let mut poll = libc::pollfd {
            fd: io::stdin().as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let ms = timeout.as_millis().min(i32::MAX as u128) as i32;
        // SAFETY: poll reads and writes only the one pollfd passed.
        match unsafe { libc::poll(&mut poll, 1, ms) } {
            0 => return Ok(None),
            n if n < 0 => {
                let e = io::Error::last_os_error();
                return if e.kind() == io::ErrorKind::Interrupted {
                    Ok(None)
                } else {
                    Err(e)
                };
            }
            _ => {}
        }
        // Read the descriptor directly: Rust's buffered stdin could hold
        // bytes that poll does not see.
        let mut byte = 0u8;
        // SAFETY: reads at most one byte into `byte`.
        match unsafe { libc::read(poll.fd, (&raw mut byte).cast(), 1) } {
            0 => Err(io::Error::new(io::ErrorKind::UnexpectedEof, "end of input")),
            n if n < 0 => Err(io::Error::last_os_error()),
            _ => Ok(Some(byte)),
        }
    }

    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        let mut out = io::stdout().lock();
        out.write_all(buf)?;
        out.flush()
    }
}
