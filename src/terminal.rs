// SPDX-License-Identifier: MIT
//! The user's own terminal as a [`Transport`], so the line editor used on
//! the serial line also runs `sprocket client`. On a tty or console it
//! switches to raw mode (restored when dropped), so keys arrive one at a
//! time and Ctrl-C and Ctrl-D reach the shell as bytes.

use std::io::{self, Write};
use std::time::Duration;

use crate::transport::Transport;

#[cfg(unix)]
pub use unix::Terminal;
#[cfg(windows)]
pub use windows::Terminal;

/// Write `buf` to standard output and flush it.
fn write_stdout(buf: &[u8]) -> io::Result<()> {
    let mut out = io::stdout().lock();
    out.write_all(buf)?;
    out.flush()
}

#[cfg(unix)]
mod unix {
    use super::*;
    use std::os::fd::AsRawFd;

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
            write_stdout(buf)
        }
    }
}

#[cfg(windows)]
mod windows {
    use super::*;
    use std::io::Read;
    use std::sync::mpsc;
    use std::thread;
    use windows_sys::Win32::System::Console::{
        ENABLE_ECHO_INPUT, ENABLE_LINE_INPUT, ENABLE_PROCESSED_INPUT,
        ENABLE_VIRTUAL_TERMINAL_INPUT, ENABLE_VIRTUAL_TERMINAL_PROCESSING, GetConsoleMode,
        GetStdHandle, STD_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetConsoleMode,
    };

    /// Standard input and output.
    pub struct Terminal {
        /// The input mode to restore, when standard input is a console.
        saved_in: Option<u32>,
        /// The output mode to restore, when standard output is a console.
        saved_out: Option<u32>,
        /// Bytes from standard input. A console's wait handle also signals
        /// for events that carry no bytes (key releases, focus, resizing),
        /// so a thread does the blocking reads and this waits with a
        /// timeout.
        input: mpsc::Receiver<io::Result<u8>>,
    }

    /// Set the console mode of standard handle `which` to `f` of its
    /// current mode; the old mode, or `None` if `which` is not a console.
    fn change_mode(which: STD_HANDLE, f: impl FnOnce(u32) -> u32) -> io::Result<Option<u32>> {
        // SAFETY: GetConsoleMode and SetConsoleMode only read or write the
        // mode passed to them; GetStdHandle does not take ownership.
        unsafe {
            let handle = GetStdHandle(which);
            let mut mode = 0;
            if GetConsoleMode(handle, &mut mode) == 0 {
                return Ok(None);
            }
            if SetConsoleMode(handle, f(mode)) == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Some(mode))
        }
    }

    impl Terminal {
        pub fn new() -> io::Result<Self> {
            // Keys arrive one at a time, unechoed, with Ctrl-C as a byte and
            // arrow keys as the escape sequences the line editor expects.
            let saved_in = change_mode(STD_INPUT_HANDLE, |mode| {
                mode & !(ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT | ENABLE_PROCESSED_INPUT)
                    | ENABLE_VIRTUAL_TERMINAL_INPUT
            })?;
            let saved_out = change_mode(STD_OUTPUT_HANDLE, |mode| {
                mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING
            })
            .inspect_err(|_| restore(saved_in, None))?;
            let (tx, input) = mpsc::channel();
            thread::spawn(move || {
                let mut buf = [0u8; 256];
                loop {
                    let item = match io::stdin().lock().read(&mut buf) {
                        Ok(0) => Err(io::Error::new(io::ErrorKind::UnexpectedEof, "end of input")),
                        Ok(n) => {
                            if buf[..n].iter().any(|&b| tx.send(Ok(b)).is_err()) {
                                return;
                            }
                            continue;
                        }
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                        Err(e) => Err(e),
                    };
                    let _ = tx.send(item);
                    return;
                }
            });
            Ok(Self {
                saved_in,
                saved_out,
                input,
            })
        }
    }

    /// Put back the console modes that [`Terminal::new`] changed.
    fn restore(saved_in: Option<u32>, saved_out: Option<u32>) {
        if let Some(mode) = saved_in {
            let _ = change_mode(STD_INPUT_HANDLE, |_| mode);
        }
        if let Some(mode) = saved_out {
            let _ = change_mode(STD_OUTPUT_HANDLE, |_| mode);
        }
    }

    impl Drop for Terminal {
        fn drop(&mut self) {
            restore(self.saved_in, self.saved_out);
        }
    }

    impl Transport for Terminal {
        fn read_byte(&mut self, timeout: Duration) -> io::Result<Option<u8>> {
            match self.input.recv_timeout(timeout) {
                Ok(item) => item.map(Some),
                Err(mpsc::RecvTimeoutError::Timeout) => Ok(None),
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    Err(io::Error::new(io::ErrorKind::UnexpectedEof, "end of input"))
                }
            }
        }

        fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
            write_stdout(buf)
        }
    }
}
