// SPDX-License-Identifier: MIT
//! Built-in XMODEM and XMODEM-1K sender.
//!
//! The receiver starts the transfer by sending NAK (8-bit arithmetic
//! checksum) or `C` (CRC-16). XMODEM-1K uses 1024-byte blocks and requires
//! CRC-16; if the receiver asks for checksum mode, the sender falls back to
//! 128-byte blocks.
//!
//! XMODEM carries neither a filename nor an exact length: the final block is
//! padded with SUB (0x1A), so the received file may be longer than the
//! original. See `docs/bootstrap.md`.

use std::fmt;
use std::io;
use std::time::{Duration, Instant};

use crate::transport::Transport;

const SOH: u8 = 0x01;
const STX: u8 = 0x02;
const EOT: u8 = 0x04;
const ACK: u8 = 0x06;
const NAK: u8 = 0x15;
const CAN: u8 = 0x18;
const CRC_REQUEST: u8 = b'C';

/// Byte used to pad the final block.
pub const PAD: u8 = 0x1A;

const SMALL_BLOCK: usize = 128;
const LARGE_BLOCK: usize = 1024;

/// How long the line must be quiet before stale bytes are considered drained.
const DRAIN_QUIET: Duration = Duration::from_millis(50);

/// Number of CAN bytes sent when the sender aborts.
const CANCEL_COUNT: usize = 8;

/// Protocol variant requested by the operator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Variant {
    /// 128-byte blocks.
    Xmodem,
    /// 1024-byte blocks where the receiver supports CRC-16.
    Xmodem1k,
}

/// Block integrity check negotiated with the receiver.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Checksum {
    /// 8-bit arithmetic sum (receiver started with NAK).
    Arithmetic,
    /// CRC-16/XMODEM (receiver started with `C`).
    Crc16,
}

impl fmt::Display for Checksum {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Checksum::Arithmetic => "checksum",
            Checksum::Crc16 => "CRC-16",
        })
    }
}

/// Sender settings.
#[derive(Clone, Debug)]
pub struct Options {
    pub variant: Variant,
    /// How long to wait for the receiver to start the transfer.
    pub start_timeout: Duration,
    /// How long to wait for ACK/NAK after each block or EOT.
    pub ack_timeout: Duration,
    /// Retransmissions allowed per block (and for EOT) before aborting.
    pub max_retries: u32,
}

impl Options {
    pub fn new(variant: Variant) -> Self {
        Self {
            variant,
            start_timeout: Duration::from_secs(60),
            ack_timeout: Duration::from_secs(10),
            max_retries: 10,
        }
    }
}

/// Why a block was retransmitted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetryReason {
    Nak,
    Timeout,
}

/// Progress notifications from [`send`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// The receiver started the transfer.
    Started {
        checksum: Checksum,
        block_size: usize,
    },
    /// Block `block` (1-based, not wrapped) was acknowledged.
    BlockAcked { block: u32, sent: u64, total: u64 },
    /// Block `block` is being retransmitted (block 0 means EOT).
    Retry { block: u32, reason: RetryReason },
}

/// Result of a successful transfer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Summary {
    pub checksum: Checksum,
    /// Number of blocks sent.
    pub blocks: u32,
    /// Bytes of file data sent.
    pub bytes: u64,
    /// SUB bytes appended to fill the final block.
    pub padding: u64,
}

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    /// The receiver did not start the transfer within the start timeout.
    NoReceiver,
    /// The receiver cancelled the transfer.
    Cancelled,
    /// A block was not acknowledged after the allowed retries.
    TooManyRetries {
        block: u32,
    },
    /// The end-of-transmission was not acknowledged after the allowed retries.
    EotNotAcknowledged,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "I/O error: {e}"),
            Error::NoReceiver => f.write_str("receiver did not start the transfer"),
            Error::Cancelled => f.write_str("transfer cancelled by receiver"),
            Error::TooManyRetries { block } => {
                write!(f, "block {block} not acknowledged after too many retries")
            }
            Error::EotNotAcknowledged => f.write_str("end of transfer not acknowledged"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

/// Send `data` to an XMODEM receiver over `transport`.
///
/// On any failure other than a receiver cancellation, the sender attempts
/// to cancel the transfer so the receiver does not wait indefinitely.
pub fn send(
    transport: &mut dyn Transport,
    data: &[u8],
    options: &Options,
    mut progress: impl FnMut(&Event),
) -> Result<Summary, Error> {
    let result = send_inner(transport, data, options, &mut progress);
    if let Err(e) = &result
        && !matches!(e, Error::Cancelled)
    {
        let _ = transport.write_all(&[CAN; CANCEL_COUNT]);
    }
    result
}

fn send_inner(
    transport: &mut dyn Transport,
    data: &[u8],
    options: &Options,
    progress: &mut dyn FnMut(&Event),
) -> Result<Summary, Error> {
    let checksum = wait_for_start(transport, options.start_timeout)?;
    // Receivers repeat their start byte until a block arrives; discard the
    // extras so they are not mistaken for a response to block 1.
    transport.drain(DRAIN_QUIET)?;

    let large = options.variant == Variant::Xmodem1k && checksum == Checksum::Crc16;
    progress(&Event::Started {
        checksum,
        block_size: if large { LARGE_BLOCK } else { SMALL_BLOCK },
    });

    let total = data.len() as u64;
    let mut offset = 0;
    let mut block: u32 = 0;
    let mut padding = 0;
    while offset < data.len() {
        let remaining = data.len() - offset;
        // A 1K block is used only when it adds no more padding than a run of
        // 128-byte blocks would.
        let size = if large && remaining > LARGE_BLOCK - SMALL_BLOCK {
            LARGE_BLOCK
        } else {
            SMALL_BLOCK
        };
        let chunk = &data[offset..data.len().min(offset + size)];
        block += 1;
        let packet = build_packet(block as u8, chunk, size, checksum);
        send_with_retries(transport, &packet, block, options, progress)?;
        offset += chunk.len();
        padding += (size - chunk.len()) as u64;
        progress(&Event::BlockAcked {
            block,
            sent: offset as u64,
            total,
        });
    }

    send_eot(transport, options, progress)?;
    Ok(Summary {
        checksum,
        blocks: block,
        bytes: total,
        padding,
    })
}

fn wait_for_start(transport: &mut dyn Transport, timeout: Duration) -> Result<Checksum, Error> {
    let deadline = Instant::now() + timeout;
    let mut last_was_can = false;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(Error::NoReceiver);
        }
        match transport.read_byte(remaining)? {
            None => return Err(Error::NoReceiver),
            Some(CRC_REQUEST) => return Ok(Checksum::Crc16),
            Some(NAK) => return Ok(Checksum::Arithmetic),
            Some(CAN) if last_was_can => return Err(Error::Cancelled),
            Some(byte) => last_was_can = byte == CAN,
        }
    }
}

fn build_packet(seq: u8, chunk: &[u8], size: usize, checksum: Checksum) -> Vec<u8> {
    let mut packet = Vec::with_capacity(size + 5);
    packet.push(if size == LARGE_BLOCK { STX } else { SOH });
    packet.push(seq);
    packet.push(!seq);
    packet.extend_from_slice(chunk);
    packet.resize(3 + size, PAD);
    let payload = &packet[3..];
    match checksum {
        Checksum::Arithmetic => {
            let sum = payload.iter().fold(0u8, |acc, &b| acc.wrapping_add(b));
            packet.push(sum);
        }
        Checksum::Crc16 => {
            let crc = crc16(payload);
            packet.extend_from_slice(&crc.to_be_bytes());
        }
    }
    packet
}

fn send_with_retries(
    transport: &mut dyn Transport,
    packet: &[u8],
    block: u32,
    options: &Options,
    progress: &mut dyn FnMut(&Event),
) -> Result<(), Error> {
    for attempt in 0..=options.max_retries {
        transport.write_all(packet)?;
        // A receiver that missed block 1 entirely may repeat its `C`.
        let reason = match wait_response(transport, options.ack_timeout, block == 1)? {
            Response::Ack => return Ok(()),
            Response::Nak => {
                // Discard duplicate NAKs so they are not read as the
                // response to the retransmission.
                transport.drain(DRAIN_QUIET)?;
                RetryReason::Nak
            }
            Response::Timeout => RetryReason::Timeout,
        };
        if attempt < options.max_retries {
            progress(&Event::Retry { block, reason });
        }
    }
    Err(Error::TooManyRetries { block })
}

fn send_eot(
    transport: &mut dyn Transport,
    options: &Options,
    progress: &mut dyn FnMut(&Event),
) -> Result<(), Error> {
    for attempt in 0..=options.max_retries {
        transport.write_all(&[EOT])?;
        // Some receivers deliberately NAK the first EOT.
        let reason = match wait_response(transport, options.ack_timeout, false)? {
            Response::Ack => return Ok(()),
            Response::Nak => RetryReason::Nak,
            Response::Timeout => RetryReason::Timeout,
        };
        if attempt < options.max_retries {
            progress(&Event::Retry { block: 0, reason });
        }
    }
    Err(Error::EotNotAcknowledged)
}

enum Response {
    Ack,
    Nak,
    Timeout,
}

fn wait_response(
    transport: &mut dyn Transport,
    timeout: Duration,
    crc_request_is_nak: bool,
) -> Result<Response, Error> {
    let deadline = Instant::now() + timeout;
    let mut last_was_can = false;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(Response::Timeout);
        }
        match transport.read_byte(remaining)? {
            None => return Ok(Response::Timeout),
            Some(ACK) => return Ok(Response::Ack),
            Some(NAK) => return Ok(Response::Nak),
            Some(CRC_REQUEST) if crc_request_is_nak => return Ok(Response::Nak),
            Some(CAN) if last_was_can => return Err(Error::Cancelled),
            // Anything else is line noise or a stale byte; keep waiting.
            Some(byte) => last_was_can = byte == CAN,
        }
    }
}

/// CRC-16/XMODEM: polynomial 0x1021, initial value 0, no reflection.
pub fn crc16(data: &[u8]) -> u16 {
    let mut crc = 0u16;
    for &byte in data {
        crc ^= (byte as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    crc
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::{MemoryTransport, memory_pair};
    use std::thread;

    const RECEIVER_WAIT: Duration = Duration::from_secs(5);

    /// Faults injected by the synthetic receiver.
    #[derive(Default, Clone)]
    struct Faults {
        /// Withhold the ACK for this (1-based) block once.
        drop_ack: Option<u32>,
        /// Send CAN CAN instead of ACK after this many blocks.
        cancel_after: Option<u32>,
        /// NAK the first EOT.
        nak_first_eot: bool,
    }

    #[derive(Debug, Default)]
    struct Received {
        data: Vec<u8>,
        small_blocks: u32,
        large_blocks: u32,
        naks: u32,
    }

    /// Minimal XMODEM receiver used to exercise the sender.
    fn receive(t: &mut MemoryTransport, start: u8, faults: Faults) -> Received {
        let crc = start == CRC_REQUEST;
        let mut out = Received::default();
        let mut expected: u8 = 1;
        let mut good: u32 = 0;
        let mut dropped = false;
        let mut eot_naked = false;
        t.write_all(&[start]).unwrap();
        loop {
            let header = t.read_byte(RECEIVER_WAIT).unwrap().expect("sender stalled");
            match header {
                EOT if faults.nak_first_eot && !eot_naked => {
                    eot_naked = true;
                    t.write_all(&[NAK]).unwrap();
                }
                EOT => {
                    t.write_all(&[ACK]).unwrap();
                    return out;
                }
                SOH | STX => {
                    let size = if header == SOH {
                        SMALL_BLOCK
                    } else {
                        LARGE_BLOCK
                    };
                    let len = 2 + size + if crc { 2 } else { 1 };
                    let body: Vec<u8> = (0..len)
                        .map(|_| t.read_byte(RECEIVER_WAIT).unwrap().expect("short block"))
                        .collect();
                    let (seq, payload, check) = (body[0], &body[2..2 + size], &body[2 + size..]);
                    let ok_check = if crc {
                        check == crc16(payload).to_be_bytes()
                    } else {
                        check[0] == payload.iter().fold(0u8, |a, &b| a.wrapping_add(b))
                    };
                    if body[1] != !seq || !ok_check {
                        out.naks += 1;
                        t.write_all(&[NAK]).unwrap();
                        continue;
                    }
                    if good > 0 && seq == expected.wrapping_sub(1) {
                        // Duplicate after a lost ACK: acknowledge, discard.
                        t.write_all(&[ACK]).unwrap();
                        continue;
                    }
                    assert_eq!(seq, expected, "out-of-sequence block");
                    out.data.extend_from_slice(payload);
                    if header == SOH {
                        out.small_blocks += 1;
                    } else {
                        out.large_blocks += 1;
                    }
                    expected = expected.wrapping_add(1);
                    good += 1;
                    if faults.cancel_after == Some(good) {
                        t.write_all(&[CAN, CAN]).unwrap();
                        return out;
                    }
                    if faults.drop_ack == Some(good) && !dropped {
                        dropped = true;
                        continue;
                    }
                    t.write_all(&[ACK]).unwrap();
                }
                other => panic!("unexpected byte {other:#04x}"),
            }
        }
    }

    /// Wraps a transport and flips one bit of the `index`th written byte.
    struct Corrupting<T> {
        inner: T,
        index: usize,
        written: usize,
    }

    impl<T: Transport> Transport for Corrupting<T> {
        fn read_byte(&mut self, timeout: Duration) -> io::Result<Option<u8>> {
            self.inner.read_byte(timeout)
        }

        fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
            let mut buf = buf.to_vec();
            if let Some(b) = self
                .index
                .checked_sub(self.written)
                .and_then(|i| buf.get_mut(i))
            {
                *b ^= 0x01;
            }
            self.written += buf.len();
            self.inner.write_all(&buf)
        }
    }

    fn fast(variant: Variant) -> Options {
        Options {
            variant,
            start_timeout: Duration::from_secs(2),
            ack_timeout: Duration::from_millis(200),
            max_retries: 3,
        }
    }

    fn sample(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i * 7 + i / 256) as u8).collect()
    }

    /// Run the sender against the synthetic receiver.
    fn run(
        sender: &mut dyn Transport,
        mut receiver: MemoryTransport,
        start: u8,
        faults: Faults,
        data: &[u8],
        options: &Options,
    ) -> (Result<Summary, Error>, Vec<Event>, Received) {
        let handle = thread::spawn(move || receive(&mut receiver, start, faults));
        let mut events = Vec::new();
        let result = send(sender, data, options, |e| events.push(e.clone()));
        (result, events, handle.join().unwrap())
    }

    fn assert_padded(received: &[u8], data: &[u8]) {
        assert_eq!(&received[..data.len()], data);
        assert!(received[data.len()..].iter().all(|&b| b == PAD));
    }

    #[test]
    fn crc16_check_value() {
        assert_eq!(crc16(b"123456789"), 0x31C3);
    }

    #[test]
    fn checksum_mode() {
        let (mut s, r) = memory_pair();
        let data = sample(300);
        let (result, events, rx) = run(
            &mut s,
            r,
            NAK,
            Faults::default(),
            &data,
            &fast(Variant::Xmodem),
        );
        let summary = result.unwrap();
        assert_eq!(summary.checksum, Checksum::Arithmetic);
        assert_eq!((summary.blocks, summary.padding), (3, 84));
        assert_eq!(rx.data.len(), 384);
        assert_padded(&rx.data, &data);
        assert_eq!(
            events[0],
            Event::Started {
                checksum: Checksum::Arithmetic,
                block_size: 128
            }
        );
    }

    #[test]
    fn crc_mode() {
        let (mut s, r) = memory_pair();
        let data = sample(256);
        let (result, _, rx) = run(
            &mut s,
            r,
            CRC_REQUEST,
            Faults::default(),
            &data,
            &fast(Variant::Xmodem),
        );
        let summary = result.unwrap();
        assert_eq!(summary.checksum, Checksum::Crc16);
        assert_eq!(summary.padding, 0);
        assert_eq!(rx.data, data);
    }

    #[test]
    fn xmodem_1k_uses_small_blocks_for_tail() {
        let (mut s, r) = memory_pair();
        let data = sample(2500);
        let (result, _, rx) = run(
            &mut s,
            r,
            CRC_REQUEST,
            Faults::default(),
            &data,
            &fast(Variant::Xmodem1k),
        );
        let summary = result.unwrap();
        assert_eq!((rx.large_blocks, rx.small_blocks), (2, 4));
        assert_eq!(summary.padding, 60);
        assert_padded(&rx.data, &data);
    }

    #[test]
    fn xmodem_1k_large_tail_when_padding_is_equal() {
        let (mut s, r) = memory_pair();
        let data = sample(1000);
        let (result, _, rx) = run(
            &mut s,
            r,
            CRC_REQUEST,
            Faults::default(),
            &data,
            &fast(Variant::Xmodem1k),
        );
        assert_eq!(result.unwrap().padding, 24);
        assert_eq!((rx.large_blocks, rx.small_blocks), (1, 0));
    }

    #[test]
    fn xmodem_1k_falls_back_without_crc() {
        let (mut s, r) = memory_pair();
        let data = sample(2048);
        let (result, events, rx) = run(
            &mut s,
            r,
            NAK,
            Faults::default(),
            &data,
            &fast(Variant::Xmodem1k),
        );
        result.unwrap();
        assert_eq!(
            events[0],
            Event::Started {
                checksum: Checksum::Arithmetic,
                block_size: 128
            }
        );
        assert_eq!((rx.large_blocks, rx.small_blocks), (0, 16));
        assert_eq!(rx.data, data);
    }

    #[test]
    fn sequence_number_wraps() {
        let (mut s, r) = memory_pair();
        let data = sample(300 * 128);
        let (result, _, rx) = run(
            &mut s,
            r,
            CRC_REQUEST,
            Faults::default(),
            &data,
            &fast(Variant::Xmodem),
        );
        assert_eq!(result.unwrap().blocks, 300);
        assert_eq!(rx.data, data);
    }

    #[test]
    fn empty_file_sends_only_eot() {
        let (mut s, r) = memory_pair();
        let (result, _, rx) = run(
            &mut s,
            r,
            CRC_REQUEST,
            Faults::default(),
            &[],
            &fast(Variant::Xmodem),
        );
        assert_eq!(result.unwrap().blocks, 0);
        assert!(rx.data.is_empty());
    }

    #[test]
    fn corrupted_block_is_retransmitted() {
        let (s, r) = memory_pair();
        // Byte 3 of block 2's payload: 133 bytes per CRC block.
        let mut s = Corrupting {
            inner: s,
            index: 133 + 3 + 3,
            written: 0,
        };
        let data = sample(400);
        let (result, events, rx) = run(
            &mut s,
            r,
            CRC_REQUEST,
            Faults::default(),
            &data,
            &fast(Variant::Xmodem),
        );
        result.unwrap();
        assert_eq!(rx.naks, 1);
        assert!(events.contains(&Event::Retry {
            block: 2,
            reason: RetryReason::Nak
        }));
        assert_padded(&rx.data, &data);
    }

    #[test]
    fn lost_ack_causes_timeout_and_duplicate_is_discarded() {
        let (mut s, r) = memory_pair();
        let data = sample(400);
        let faults = Faults {
            drop_ack: Some(2),
            ..Faults::default()
        };
        let (result, events, rx) = run(
            &mut s,
            r,
            CRC_REQUEST,
            faults,
            &data,
            &fast(Variant::Xmodem),
        );
        result.unwrap();
        assert!(events.contains(&Event::Retry {
            block: 2,
            reason: RetryReason::Timeout
        }));
        assert_eq!(rx.data.len(), 512);
        assert_padded(&rx.data, &data);
    }

    #[test]
    fn nak_on_first_eot_is_retried() {
        let (mut s, r) = memory_pair();
        let data = sample(10);
        let faults = Faults {
            nak_first_eot: true,
            ..Faults::default()
        };
        let (result, events, _) = run(&mut s, r, NAK, faults, &data, &fast(Variant::Xmodem));
        result.unwrap();
        assert!(events.contains(&Event::Retry {
            block: 0,
            reason: RetryReason::Nak
        }));
    }

    #[test]
    fn receiver_cancel() {
        let (mut s, r) = memory_pair();
        let data = sample(1000);
        let faults = Faults {
            cancel_after: Some(2),
            ..Faults::default()
        };
        let (result, _, _) = run(
            &mut s,
            r,
            CRC_REQUEST,
            faults,
            &data,
            &fast(Variant::Xmodem),
        );
        assert!(matches!(result, Err(Error::Cancelled)));
    }

    #[test]
    fn no_receiver_times_out() {
        let (mut s, _r) = memory_pair();
        let options = Options {
            start_timeout: Duration::from_millis(50),
            ..fast(Variant::Xmodem)
        };
        let result = send(&mut s, b"x", &options, |_| {});
        assert!(matches!(result, Err(Error::NoReceiver)));
    }

    #[test]
    fn silent_receiver_exhausts_retries_and_is_cancelled() {
        let (mut s, mut r) = memory_pair();
        let handle = thread::spawn(move || {
            r.write_all(&[CRC_REQUEST]).unwrap();
            let mut seen = Vec::new();
            while let Ok(Some(b)) = r.read_byte(Duration::from_secs(2)) {
                seen.push(b);
            }
            seen
        });
        let options = fast(Variant::Xmodem);
        let mut retries = 0;
        let result = send(&mut s, &sample(10), &options, |e| {
            if matches!(e, Event::Retry { .. }) {
                retries += 1;
            }
        });
        drop(s);
        assert!(matches!(result, Err(Error::TooManyRetries { block: 1 })));
        assert_eq!(retries, options.max_retries);
        let seen = handle.join().unwrap();
        assert_eq!(
            seen.len(),
            133 * (options.max_retries as usize + 1) + CANCEL_COUNT
        );
        assert!(seen.ends_with(&[CAN; CANCEL_COUNT]));
    }
}
