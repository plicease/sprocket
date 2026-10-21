// SPDX-License-Identifier: MIT
//! ZMODEM sender, driving the [`zmodem2`] state machine over a [`Transport`].
//!
//! ZMODEM carries the filename and exact length, and protects data with
//! CRC-32. The opening ZRQINIT header doubles as the auto-download signature
//! that terminal programs such as Telix and Procomm Plus watch for.
//!
//! `zmodem2` does no I/O of its own; this module supplies timeouts, the
//! receiver's raw CAN cancel sequence, and a bound on how long a stalled
//! session may last.

use std::fmt;
use std::io;
use std::time::{Duration, Instant, SystemTime};

use zmodem2::{Action, Event as ZEvent, FileInfo, Position, Sender};

use crate::transport::Transport;

const CAN: u8 = 0x18;

/// Consecutive CAN bytes that mean the receiver gave up. ZDLE is also 0x18
/// but is always followed by an escaped byte, so a valid stream never
/// contains five in a row.
const CANCEL_RUN: u32 = 5;

/// Receiver-to-sender header errors tolerated before giving up.
const MAX_WIRE_ERRORS: u32 = 20;

/// Advice for [`Error::Skipped`]. Windows 95 HyperTerminal declines a ZMODEM
/// auto-download while its Receive File protocol is still set to Xmodem
/// (e.g. after an XMODEM receive), then accepts the next one.
pub const SKIP_HINT: &str = "If the receiver is HyperTerminal, set Transfer > Receive File > \
     protocol to Zmodem (an earlier XMODEM receive leaves it on Xmodem), then try again.";

/// Sender settings.
#[derive(Clone, Debug)]
pub struct Options {
    /// How long to wait for a receiver to answer the initial ZRQINIT.
    pub start_timeout: Duration,
    /// How long to wait for any receiver response once the transfer runs.
    /// ZRQINIT is repeated at this interval while waiting to start.
    pub response_timeout: Duration,
    /// Consecutive response timeouts tolerated once the transfer runs.
    pub max_timeouts: u32,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            start_timeout: Duration::from_secs(60),
            response_timeout: Duration::from_secs(10),
            max_timeouts: 6,
        }
    }
}

/// Progress notifications from [`send`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// File data up to `sent` bytes has been handed to the link.
    Progress { sent: u64, total: u64 },
}

/// Result of a successful transfer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Summary {
    pub bytes: u64,
}

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    /// The file does not fit ZMODEM's 32-bit offsets.
    TooLarge,
    /// No receiver answered within the start timeout.
    NoReceiver,
    /// The receiver cancelled the transfer.
    Cancelled,
    /// The receiver declined the file (ZSKIP), e.g. because it already exists.
    Skipped,
    /// The receiver stopped responding.
    Timeout,
    /// The receiver kept sending damaged or unexpected data.
    Protocol(zmodem2::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "I/O error: {e}"),
            Error::TooLarge => f.write_str("file is too large for ZMODEM (4 GiB limit)"),
            Error::NoReceiver => f.write_str("receiver did not start the transfer"),
            Error::Cancelled => f.write_str("transfer cancelled by receiver"),
            Error::Skipped => f.write_str("receiver skipped the file"),
            Error::Timeout => f.write_str("receiver stopped responding"),
            Error::Protocol(e) => write!(f, "ZMODEM protocol error: {e}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            Error::Protocol(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

impl From<zmodem2::Error> for Error {
    fn from(e: zmodem2::Error) -> Self {
        Error::Protocol(e)
    }
}

/// Send one file named `name` to a ZMODEM receiver over `transport`.
/// `modified` is passed on so the receiver can stamp the file.
///
/// On failure other than a receiver cancellation, the ZMODEM cancel
/// sequence is sent so the receiver does not wait indefinitely.
pub fn send(
    transport: &mut dyn Transport,
    name: &[u8],
    data: &[u8],
    modified: Option<SystemTime>,
    options: &Options,
    mut progress: impl FnMut(&Event),
) -> Result<Summary, Error> {
    let size = u32::try_from(data.len()).map_err(|_| Error::TooLarge)?;
    let mut info = FileInfo::new(name, Some(Position::new(size)));
    // Without a time, some receivers stamp the file with a nonsense date.
    if let Some(secs) = modified
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .and_then(|d| u32::try_from(d.as_secs()).ok())
    {
        info = info.with_modified(secs);
    }
    let mut sender = Sender::new()?;
    sender.start_file(info)?;
    sender.finish()?;

    let result = drive(&mut sender, transport, data, options, &mut progress);
    // A skip has already been answered with ZFIN; cancelling would only
    // put Ctrl-X and backspaces on the receiver's screen.
    if let Err(e) = &result
        && !matches!(e, Error::Cancelled | Error::Skipped | Error::Io(_))
    {
        sender.abort();
        while let Action::WriteWire(bytes) = sender.poll() {
            let n = bytes.len();
            if transport.write_all(bytes).is_err() {
                break;
            }
            sender.wire_written(n);
        }
    }
    result
}

fn drive(
    sender: &mut Sender,
    transport: &mut dyn Transport,
    data: &[u8],
    options: &Options,
    progress: &mut dyn FnMut(&Event),
) -> Result<Summary, Error> {
    let total = data.len() as u64;
    let start_deadline = Instant::now() + options.start_timeout;
    let mut started = false;
    let mut sent = 0u64;
    let mut timeouts = 0;
    let mut wire_errors = 0;
    let mut can_run = 0;
    let mut file_done = false;
    let mut skipped = false;
    let mut session_done = false;

    loop {
        match sender.poll() {
            Action::WriteWire(bytes) => {
                let n = bytes.len();
                transport.write_all(bytes)?;
                sender.wire_written(n);
            }
            Action::ReadFile { offset, max_len } => {
                started = true;
                let start = offset.get() as usize;
                let end = data.len().min(start + max_len);
                sender.submit_file(&data[start..end])?;
                // After a ZRPOS the receiver may rewind; report the furthest
                // point reached.
                sent = sent.max(end as u64);
                progress(&Event::Progress { sent, total });
            }
            Action::Event(ZEvent::FileCompleted) => {
                // The library reports a ZSKIP as a completed file; only a
                // file whose data all went out was actually transferred.
                if sent < total {
                    // There is no other file to offer, so end the session
                    // with ZFIN as the protocol expects.
                    skipped = true;
                    if sender.finish().is_err() {
                        return Err(Error::Skipped);
                    }
                } else {
                    file_done = true;
                }
            }
            Action::Event(ZEvent::SessionCompleted) => session_done = true,
            Action::Event(ZEvent::Aborted) => return Err(Error::Cancelled),
            Action::Event(_) | Action::WriteFile(_) => {}
            Action::Idle if session_done => {
                return if file_done {
                    Ok(Summary { bytes: total })
                } else {
                    Err(Error::Skipped)
                };
            }
            Action::Idle => {
                let wait = if started {
                    options.response_timeout
                } else {
                    let left = start_deadline.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        return Err(Error::NoReceiver);
                    }
                    left.min(options.response_timeout)
                };
                match transport.read_byte(wait)? {
                    // The file was declined; an unanswered ZFIN changes nothing.
                    None if skipped => return Err(Error::Skipped),
                    None => {
                        if started {
                            timeouts += 1;
                            if timeouts > options.max_timeouts {
                                return Err(Error::Timeout);
                            }
                        }
                        sender.timeout()?;
                    }
                    Some(byte) => {
                        timeouts = 0;
                        if byte == CAN {
                            can_run += 1;
                            if can_run >= CANCEL_RUN {
                                return Err(Error::Cancelled);
                            }
                        } else {
                            can_run = 0;
                        }
                        // A damaged header makes the library queue a ZNAK;
                        // keep going unless the line is persistently bad.
                        if let Err(e) = sender.submit_wire(&[byte]) {
                            wire_errors += 1;
                            if wire_errors > MAX_WIRE_ERRORS {
                                return Err(e.into());
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// Convert a host filename into a conservative ZMODEM filename: printable
/// ASCII only, with spaces and anything else replaced by `_`.
pub fn wire_name(name: &str) -> Vec<u8> {
    name.chars()
        .map(|c| {
            if c.is_ascii_graphic() && c != '/' && c != '\\' {
                c as u8
            } else {
                b'_'
            }
        })
        .collect()
}

#[cfg(test)]
pub(crate) mod test_receiver {
    use super::*;
    use crate::transport::MemoryTransport;
    use zmodem2::Receiver;

    pub struct Received {
        pub name: Vec<u8>,
        pub size: Option<u32>,
        pub modified: Option<u32>,
        pub data: Vec<u8>,
        /// Every byte read from the sender.
        pub raw: Vec<u8>,
    }

    /// Receive one session with the `zmodem2` receiver. If `cancel_after`
    /// is set, send a raw CAN run once that many bytes have arrived.
    pub fn receive(t: &mut MemoryTransport, cancel_after: Option<usize>) -> Received {
        run(t, cancel_after, false)
    }

    /// Decline the offered file with ZSKIP, as HyperTerminal does when its
    /// receive protocol is still set to Xmodem.
    pub fn skip(t: &mut MemoryTransport) -> Received {
        run(t, None, true)
    }

    fn run(t: &mut MemoryTransport, cancel_after: Option<usize>, skip: bool) -> Received {
        let mut r = Receiver::new().unwrap();
        r.set_manual_file_accept(skip);
        let mut out = Received {
            name: Vec::new(),
            size: None,
            modified: None,
            data: Vec::new(),
            raw: Vec::new(),
        };
        loop {
            match r.poll() {
                Action::WriteWire(bytes) => {
                    let n = bytes.len();
                    t.write_all(bytes).unwrap();
                    r.wire_written(n);
                }
                Action::WriteFile(bytes) => {
                    let n = bytes.len();
                    out.data.extend_from_slice(bytes);
                    r.file_written(n).unwrap();
                    if cancel_after.is_some_and(|c| out.data.len() >= c) {
                        t.write_all(&[CAN; 10]).unwrap();
                        return out;
                    }
                }
                Action::Event(ZEvent::FileStarted(info)) => {
                    out.name = info.name.to_vec();
                    out.size = info.size.map(Position::get);
                    out.modified = info.modified;
                    if skip {
                        r.skip_file().unwrap();
                    }
                }
                Action::Event(ZEvent::SessionCompleted) => {
                    // The event precedes the receiver's ZFIN reply.
                    while let Action::WriteWire(bytes) = r.poll() {
                        let n = bytes.len();
                        t.write_all(bytes).unwrap();
                        r.wire_written(n);
                    }
                    // Collect the sender's closing "OO".
                    while let Ok(Some(b)) = t.read_byte(Duration::from_millis(200)) {
                        out.raw.push(b);
                    }
                    return out;
                }
                Action::Event(ZEvent::Aborted) => panic!("receiver aborted"),
                Action::Idle => match t.read_byte(Duration::from_secs(5)).unwrap() {
                    Some(b) => {
                        out.raw.push(b);
                        let _ = r.submit_wire(&[b]);
                    }
                    None => r.timeout().unwrap(),
                },
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_receiver::{self, receive};
    use super::*;
    use crate::transport::{Corrupting, memory_pair};
    use std::thread;

    fn fast() -> Options {
        Options {
            start_timeout: Duration::from_secs(2),
            response_timeout: Duration::from_millis(300),
            max_timeouts: 3,
        }
    }

    fn stamp() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)
    }

    fn sample(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i * 13 + i / 256) as u8).collect()
    }

    #[test]
    fn sends_exact_length_and_name() {
        let (mut s, mut r) = memory_pair();
        let data = sample(5000);
        let rx = thread::spawn(move || receive(&mut r, None));
        let mut last = None;
        let summary = send(&mut s, b"TEST.BIN", &data, Some(stamp()), &fast(), |e| {
            last = Some(e.clone())
        })
        .unwrap();
        let rx = rx.join().unwrap();
        assert_eq!(summary.bytes, 5000);
        assert_eq!(rx.name, b"TEST.BIN");
        assert_eq!(rx.size, Some(5000));
        assert_eq!(rx.modified, Some(1_700_000_000));
        assert_eq!(rx.data, data);
        assert_eq!(
            last,
            Some(Event::Progress {
                sent: 5000,
                total: 5000
            })
        );
    }

    #[test]
    fn empty_file() {
        let (mut s, mut r) = memory_pair();
        let rx = thread::spawn(move || receive(&mut r, None));
        send(&mut s, b"EMPTY", &[], None, &fast(), |_| {}).unwrap();
        let rx = rx.join().unwrap();
        assert_eq!(rx.size, Some(0));
        assert!(rx.data.is_empty());
    }

    #[test]
    fn corrupted_data_is_resent() {
        let (s, mut r) = memory_pair();
        // Well inside the first data subpacket.
        let mut s = Corrupting::new(s, 700);
        let data = sample(5000);
        let rx = thread::spawn(move || receive(&mut r, None));
        send(&mut s, b"TEST.BIN", &data, None, &fast(), |_| {}).unwrap();
        assert_eq!(rx.join().unwrap().data, data);
    }

    #[test]
    fn receiver_cancel() {
        let (mut s, mut r) = memory_pair();
        let data = sample(20_000);
        let rx = thread::spawn(move || receive(&mut r, Some(2048)));
        let result = send(&mut s, b"TEST.BIN", &data, None, &fast(), |_| {});
        rx.join().unwrap();
        assert!(matches!(result, Err(Error::Cancelled)), "{result:?}");
    }

    #[test]
    fn skipped_file_ends_session_without_cancel() {
        let (mut s, mut r) = memory_pair();
        let data = sample(5000);
        let rx = thread::spawn(move || test_receiver::skip(&mut r));
        let result = send(&mut s, b"TEST.BIN", &data, None, &fast(), |_| {});
        assert!(matches!(result, Err(Error::Skipped)), "{result:?}");
        let rx = rx.join().unwrap();
        assert_eq!(rx.name, b"TEST.BIN");
        assert!(rx.data.is_empty());
        assert!(
            !rx.raw.windows(5).any(|w| w == [CAN; 5]),
            "cancel sequence sent"
        );
    }

    #[test]
    fn no_receiver() {
        let (mut s, _r) = memory_pair();
        let options = Options {
            start_timeout: Duration::from_millis(100),
            ..fast()
        };
        let result = send(&mut s, b"X", b"x", None, &options, |_| {});
        assert!(matches!(result, Err(Error::NoReceiver)));
    }

    #[test]
    fn zrqinit_repeats_while_waiting() {
        let (mut s, mut r) = memory_pair();
        let options = Options {
            start_timeout: Duration::from_millis(350),
            response_timeout: Duration::from_millis(100),
            max_timeouts: 3,
        };
        let result = send(&mut s, b"X", b"x", None, &options, |_| {});
        assert!(matches!(result, Err(Error::NoReceiver)));
        drop(s);
        let mut seen = Vec::new();
        while let Ok(Some(b)) = r.read_byte(Duration::from_millis(10)) {
            seen.push(b);
        }
        let zrqinit = b"**\x18B00";
        assert!(seen.windows(zrqinit.len()).filter(|w| w == zrqinit).count() >= 3);
    }

    #[test]
    fn wire_name_is_plain_ascii() {
        assert_eq!(wire_name("My File (1).exe"), b"My_File_(1).exe");
        assert_eq!(wire_name("caf\u{e9}.txt"), b"caf_.txt");
    }
}
