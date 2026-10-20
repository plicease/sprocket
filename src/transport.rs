// SPDX-License-Identifier: MIT
//! Byte-stream transports to a remote machine.
//!
//! Protocol engines talk to a [`Transport`] rather than to a serial port
//! directly, so the same code runs over a physical port, a TCP link to an
//! emulator's virtual null-modem, or an in-memory pipe in tests.

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::Duration;

use serialport::{DataBits, FlowControl, Parity, SerialPort, StopBits};

/// Upper bound on bytes discarded by [`Transport::drain`], so a noisy line
/// cannot stall the caller indefinitely.
const DRAIN_LIMIT: usize = 4096;

/// A bidirectional byte stream to a remote machine.
pub trait Transport {
    /// Read one byte, waiting at most `timeout`. Returns `Ok(None)` on timeout.
    fn read_byte(&mut self, timeout: Duration) -> io::Result<Option<u8>>;

    /// Write all of `buf` and flush it toward the remote.
    fn write_all(&mut self, buf: &[u8]) -> io::Result<()>;

    /// Discard received bytes until the line has been quiet for `quiet`.
    fn drain(&mut self, quiet: Duration) -> io::Result<()> {
        for _ in 0..DRAIN_LIMIT {
            if self.read_byte(quiet)?.is_none() {
                break;
            }
        }
        Ok(())
    }
}

/// Open a transport from a port specification.
///
/// `tcp://HOST:PORT` connects to a TCP listener (e.g. an emulator's
/// null-modem endpoint); anything else is treated as a serial device path,
/// configured as `baud` 8N1 with no flow control.
pub fn open(port: &str, baud: u32) -> io::Result<Box<dyn Transport>> {
    match port.strip_prefix("tcp://") {
        Some(addr) => Ok(Box::new(TcpTransport::connect(addr)?)),
        None => Ok(Box::new(SerialTransport::open(port, baud)?)),
    }
}

/// A physical (or OS-provided) serial port.
pub struct SerialTransport {
    port: Box<dyn SerialPort>,
}

impl SerialTransport {
    /// Open `path` at `baud` 8N1 with no flow control.
    pub fn open(path: &str, baud: u32) -> io::Result<Self> {
        let port = serialport::new(path, baud)
            .data_bits(DataBits::Eight)
            .parity(Parity::None)
            .stop_bits(StopBits::One)
            .flow_control(FlowControl::None)
            .timeout(Duration::from_secs(1))
            .open()?;
        Ok(Self { port })
    }
}

impl Transport for SerialTransport {
    fn read_byte(&mut self, timeout: Duration) -> io::Result<Option<u8>> {
        self.port.set_timeout(timeout)?;
        let mut byte = [0u8];
        match self.port.read(&mut byte) {
            Ok(1) => Ok(Some(byte[0])),
            Ok(_) => Ok(None),
            Err(e) if is_timeout(&e) => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        self.port.write_all(buf)?;
        self.port.flush()
    }
}

/// A raw TCP stream, used to reach an emulator's virtual serial port.
///
/// No telnet negotiation or modem commands are added; bytes pass through
/// unchanged, so the emulator must be configured for a transparent link.
pub struct TcpTransport {
    stream: TcpStream,
}

impl TcpTransport {
    /// Connect to `addr` (`HOST:PORT`).
    pub fn connect(addr: &str) -> io::Result<Self> {
        let stream = TcpStream::connect(addr)?;
        stream.set_nodelay(true)?;
        Ok(Self { stream })
    }
}

impl Transport for TcpTransport {
    fn read_byte(&mut self, timeout: Duration) -> io::Result<Option<u8>> {
        // A zero read timeout is rejected by the standard library.
        self.stream
            .set_read_timeout(Some(timeout.max(Duration::from_millis(1))))?;
        let mut byte = [0u8];
        match self.stream.read(&mut byte) {
            Ok(1) => Ok(Some(byte[0])),
            Ok(_) => Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "connection closed by peer",
            )),
            Err(e) if is_timeout(&e) => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        self.stream.write_all(buf)?;
        self.stream.flush()
    }
}

/// One end of an in-memory byte pipe, for deterministic tests.
pub struct MemoryTransport {
    tx: Sender<u8>,
    rx: Receiver<u8>,
}

/// Create two connected in-memory transports.
pub fn memory_pair() -> (MemoryTransport, MemoryTransport) {
    let (a_tx, b_rx) = mpsc::channel();
    let (b_tx, a_rx) = mpsc::channel();
    (
        MemoryTransport { tx: a_tx, rx: a_rx },
        MemoryTransport { tx: b_tx, rx: b_rx },
    )
}

impl Transport for MemoryTransport {
    fn read_byte(&mut self, timeout: Duration) -> io::Result<Option<u8>> {
        match self.rx.recv_timeout(timeout) {
            Ok(byte) => Ok(Some(byte)),
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "peer disconnected",
            )),
        }
    }

    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        for &byte in buf {
            self.tx
                .send(byte)
                .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "peer disconnected"))?;
        }
        Ok(())
    }
}

fn is_timeout(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_pair_round_trip() {
        let (mut a, mut b) = memory_pair();
        a.write_all(b"hi").unwrap();
        let t = Duration::from_millis(100);
        assert_eq!(b.read_byte(t).unwrap(), Some(b'h'));
        assert_eq!(b.read_byte(t).unwrap(), Some(b'i'));
        assert_eq!(b.read_byte(Duration::from_millis(10)).unwrap(), None);
    }

    #[test]
    fn memory_drain_discards_pending() {
        let (mut a, mut b) = memory_pair();
        a.write_all(b"CCC").unwrap();
        b.drain(Duration::from_millis(10)).unwrap();
        assert_eq!(b.read_byte(Duration::from_millis(10)).unwrap(), None);
    }

    #[test]
    fn memory_disconnect_is_eof() {
        let (a, mut b) = memory_pair();
        drop(a);
        let err = b.read_byte(Duration::from_millis(10)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn tcp_round_trip() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut buf = [0u8; 3];
            s.read_exact(&mut buf).unwrap();
            s.write_all(&buf).unwrap();
        });
        let mut t = open(&format!("tcp://{addr}"), 9600).unwrap();
        t.write_all(b"abc").unwrap();
        let wait = Duration::from_secs(2);
        for &want in b"abc" {
            assert_eq!(t.read_byte(wait).unwrap(), Some(want));
        }
        server.join().unwrap();
        let err = t.read_byte(wait).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }
}
