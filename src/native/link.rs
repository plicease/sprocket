// SPDX-License-Identifier: MIT
//! Native protocol v1 link layer, host side: stop-and-wait requests with
//! retransmission, `PENDING`, and timeouts (`docs/protocol-v1.md`
//! section 2).
//!
//! The host sends one request at a time and waits for the response with
//! the same `session` and `seq`, ignoring everything else. Damaged frames
//! are dropped silently by both sides, so a lost request or response shows
//! up only as a timeout, after which the identical request is resent.

use std::fmt;
use std::io;
use std::time::{Duration, Instant};

use super::frame::{Decoder, Discard, Event as FrameEvent, Frame, MAX_PAYLOAD, MIN_PAYLOAD};
use crate::transport::Transport;

/// Message type of `PENDING`, which may answer any request.
pub const PENDING: u8 = 0xFE;
/// Message type of `ERROR`, which may answer any request.
pub const ERROR: u8 = 0xFF;
/// Bit set in a request's type to form its response's type.
pub const RESPONSE: u8 = 0x80;

/// Discard a partial frame after this long without a byte.
pub const INTER_BYTE_TIMEOUT: Duration = Duration::from_secs(1);
/// Retransmissions of one request before giving up.
pub const RETRANSMITS: u32 = 8;
/// Default response timeout for storage reads, which may run inside BIOS
/// calls that cannot send `PENDING`.
pub const STORAGE_TIMEOUT: Duration = Duration::from_secs(30);

/// The default response timeout: 0.5 s plus twice the wire time of the
/// largest request and the largest response, at 8N1 (10 bits per byte).
pub fn response_timeout(baud: u32, max_request: usize, max_response: usize) -> Duration {
    // Each frame adds a 6-byte header and CRC and two delimiters.
    let bytes = (max_request + 8 + max_response + 8) as u64;
    let wire = Duration::from_micros(bytes * 10 * 1_000_000 / u64::from(baud.max(1)));
    Duration::from_millis(500) + 2 * wire
}

/// How long to wait for a response, and how often to retransmit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Retry {
    /// Time allowed for each attempt; restarted by each `PENDING`.
    pub timeout: Duration,
    /// Retransmissions after the first attempt.
    pub retransmits: u32,
}

impl Retry {
    pub fn new(timeout: Duration) -> Self {
        Self {
            timeout,
            retransmits: RETRANSMITS,
        }
    }
}

/// Notifications from a [`Link`], for logs and progress display.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// The request is being sent again; `attempt` counts from 2.
    Retransmit { seq: u8, attempt: u32 },
    /// The client reported that the request is still running.
    Pending { seq: u8 },
    /// A valid frame that does not answer the outstanding request.
    Ignored(Frame),
    /// A damaged or abandoned frame.
    Discarded(Discard),
}

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    /// No response arrived after every retransmission. The client may still
    /// be busy, may have finished with its response lost, or may be gone:
    /// the outcome of the request is unknown.
    NoResponse {
        kind: u8,
        seq: u8,
        attempts: u32,
    },
    /// An earlier request got no response, so this session may not send
    /// another one; start a new session.
    SessionLost,
    /// The payload is larger than the negotiated maximum.
    TooLarge {
        len: usize,
        max: usize,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "I/O error: {e}"),
            Error::NoResponse {
                kind,
                seq,
                attempts,
            } => write!(
                f,
                "no response to request type {kind:#04x} (seq {seq}) after {attempts} attempts; \
                 its outcome is unknown"
            ),
            Error::SessionLost => {
                f.write_str("an earlier request got no response; a new session is needed")
            }
            Error::TooLarge { len, max } => {
                write!(f, "payload of {len} bytes exceeds the maximum of {max}")
            }
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

/// The host end of a native link.
pub struct Link<'a> {
    transport: &'a mut dyn Transport,
    decoder: Decoder,
    observer: Box<dyn FnMut(&Event) + 'a>,
    inter_byte: Duration,
    /// Largest payload the host may send.
    send_limit: usize,
    session: u16,
    seq: u8,
    /// False after a request went unanswered, until a new session starts.
    usable: bool,
}

impl<'a> Link<'a> {
    /// A link over `transport` accepting response payloads up to
    /// `max_payload` bytes. Until [`set_send_limit`](Self::set_send_limit)
    /// is called, requests are limited to the 128 bytes every client
    /// accepts.
    pub fn new(transport: &'a mut dyn Transport, max_payload: usize) -> Self {
        Self {
            transport,
            decoder: Decoder::new(max_payload),
            observer: Box::new(|_| {}),
            inter_byte: INTER_BYTE_TIMEOUT,
            send_limit: MIN_PAYLOAD,
            session: 0,
            seq: 0,
            usable: false,
        }
    }

    /// Report link events to `observer`.
    pub fn with_observer(mut self, observer: impl FnMut(&Event) + 'a) -> Self {
        self.observer = Box::new(observer);
        self
    }

    /// Change the inter-byte timeout (tests use a short one).
    pub fn with_inter_byte_timeout(mut self, timeout: Duration) -> Self {
        self.inter_byte = timeout;
        self
    }

    /// Set the largest payload the host may send, once negotiated.
    pub fn set_send_limit(&mut self, max_payload: usize) {
        self.send_limit = max_payload.clamp(MIN_PAYLOAD, MAX_PAYLOAD);
    }

    /// Start using a new session ID, with `seq` back at 0. The next request
    /// should be a `HELLO`.
    pub fn start_session(&mut self, session: u16) {
        assert_ne!(session, 0, "session 0 is reserved");
        self.session = session;
        self.seq = 0;
        self.usable = true;
    }

    pub fn session(&self) -> u16 {
        self.session
    }

    /// Discard received bytes until the line has been quiet for `quiet`.
    pub fn drain(&mut self, quiet: Duration) -> io::Result<()> {
        self.transport.drain(quiet)?;
        self.decoder.timeout();
        Ok(())
    }

    /// Send a request and wait for its response: a frame of type
    /// `kind | 0x80`, or an `ERROR` frame, with this request's `session`
    /// and `seq`. The caller interprets the response.
    pub fn request(&mut self, kind: u8, payload: &[u8], retry: Retry) -> Result<Frame, Error> {
        if !self.usable {
            return Err(Error::SessionLost);
        }
        if payload.len() > self.send_limit {
            return Err(Error::TooLarge {
                len: payload.len(),
                max: self.send_limit,
            });
        }
        let seq = self.seq;
        let wire = Frame {
            kind,
            session: self.session,
            seq,
            payload: payload.to_vec(),
        }
        .encode();

        let attempts = retry.retransmits + 1;
        for attempt in 1..=attempts {
            if attempt > 1 {
                (self.observer)(&Event::Retransmit { seq, attempt });
            }
            self.transport.write_all(&wire)?;
            if let Some(response) = self.await_response(kind, seq, retry.timeout)? {
                self.seq = seq.wrapping_add(1);
                return Ok(response);
            }
        }
        self.usable = false;
        Err(Error::NoResponse {
            kind,
            seq,
            attempts,
        })
    }

    /// Wait for the response to request `seq`; `None` on timeout.
    fn await_response(
        &mut self,
        kind: u8,
        seq: u8,
        timeout: Duration,
    ) -> Result<Option<Frame>, Error> {
        let mut deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(None);
            }
            let wait = if self.decoder.in_frame() {
                remaining.min(self.inter_byte)
            } else {
                remaining
            };
            let event = match self.transport.read_byte(wait)? {
                Some(byte) => self.decoder.push(byte),
                // Only a full inter-byte wait abandons a partial frame; the
                // response deadline is checked at the top of the loop.
                None if wait == self.inter_byte => self.decoder.timeout(),
                None => None,
            };
            match event {
                None => {}
                Some(FrameEvent::Discarded(reason)) => {
                    (self.observer)(&Event::Discarded(reason));
                }
                Some(FrameEvent::Frame(frame)) => {
                    if frame.session != self.session || frame.seq != seq {
                        (self.observer)(&Event::Ignored(frame));
                    } else if frame.kind == PENDING {
                        (self.observer)(&Event::Pending { seq });
                        deadline = Instant::now() + timeout;
                    } else if frame.kind == kind | RESPONSE || frame.kind == ERROR {
                        return Ok(Some(frame));
                    } else {
                        (self.observer)(&Event::Ignored(frame));
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::{MemoryTransport, memory_pair};
    use std::sync::{Arc, Mutex};
    use std::thread;

    const SESSION: u16 = 0x5A3C;
    const PING: u8 = 0x02;
    const SHORT: Duration = Duration::from_millis(100);

    /// What the mock client does in answer to one request.
    enum Action {
        /// Answer with this frame.
        Send(Frame),
        /// Write raw bytes.
        Raw(Vec<u8>),
        Sleep(Duration),
    }

    fn reply(request: &Frame, kind: u8, payload: &[u8]) -> Action {
        Action::Send(Frame {
            kind,
            session: request.session,
            seq: request.seq,
            payload: payload.to_vec(),
        })
    }

    /// Echo every request back as its response, like `PING`.
    fn echo(request: &Frame) -> Vec<Action> {
        vec![reply(request, request.kind | RESPONSE, &request.payload)]
    }

    /// Run a mock client until the host end is dropped. Returns every
    /// request it received.
    fn client(
        mut transport: MemoryTransport,
        mut handler: impl FnMut(&Frame) -> Vec<Action> + Send + 'static,
    ) -> thread::JoinHandle<Vec<Frame>> {
        thread::spawn(move || {
            let mut decoder = Decoder::new(MAX_PAYLOAD);
            let mut received = Vec::new();
            while let Ok(byte) = transport.read_byte(Duration::from_secs(5)) {
                let Some(byte) = byte else { break };
                if let Some(FrameEvent::Frame(frame)) = decoder.push(byte) {
                    for action in handler(&frame) {
                        let sent = match action {
                            Action::Send(f) => transport.write_all(&f.encode()),
                            Action::Raw(bytes) => transport.write_all(&bytes),
                            Action::Sleep(d) => {
                                thread::sleep(d);
                                Ok(())
                            }
                        };
                        if sent.is_err() {
                            return received;
                        }
                    }
                    received.push(frame);
                }
            }
            received
        })
    }

    /// Run `body` against a link to a mock client, collecting link events.
    fn with_link<T>(
        handler: impl FnMut(&Frame) -> Vec<Action> + Send + 'static,
        body: impl FnOnce(&mut Link) -> T,
    ) -> (T, Vec<Event>, Vec<Frame>) {
        let (mut host, remote) = memory_pair();
        let handle = client(remote, handler);
        let events = Arc::new(Mutex::new(Vec::new()));
        let result = {
            let log = Arc::clone(&events);
            let mut link = Link::new(&mut host, MAX_PAYLOAD)
                .with_observer(move |e| log.lock().unwrap().push(e.clone()))
                .with_inter_byte_timeout(Duration::from_millis(50));
            link.start_session(SESSION);
            body(&mut link)
        };
        drop(host);
        let received = handle.join().unwrap();
        let events = events.lock().unwrap().clone();
        (result, events, received)
    }

    #[test]
    fn default_response_timeout() {
        let t = response_timeout(9600, 1024, 1024);
        assert!(t > Duration::from_millis(4700) && t < Duration::from_millis(4900));
    }

    #[test]
    fn request_and_response() {
        let ((first, second), events, received) = with_link(echo, |link| {
            let first = link.request(PING, b"one", Retry::new(SHORT)).unwrap();
            let second = link.request(PING, b"two", Retry::new(SHORT)).unwrap();
            (first, second)
        });
        assert_eq!(
            (first.kind, first.seq, &first.payload[..]),
            (0x82, 0, &b"one"[..])
        );
        assert_eq!((second.seq, &second.payload[..]), (1, &b"two"[..]));
        assert!(events.is_empty());
        assert_eq!(received.iter().map(|f| f.seq).collect::<Vec<_>>(), [0, 1]);
        assert!(received.iter().all(|f| f.session == SESSION));
    }

    #[test]
    fn sequence_wraps() {
        let (last, _, received) = with_link(echo, |link| {
            let mut last = None;
            for _ in 0..258 {
                last = Some(link.request(PING, &[], Retry::new(SHORT)).unwrap());
            }
            last.unwrap()
        });
        assert_eq!(last.seq, 1);
        assert_eq!(received.len(), 258);
    }

    #[test]
    fn lost_response_is_retransmitted_identically() {
        let mut first = true;
        let (response, events, received) = with_link(
            move |request| {
                if std::mem::take(&mut first) {
                    vec![]
                } else {
                    echo(request)
                }
            },
            |link| link.request(PING, b"again", Retry::new(SHORT)).unwrap(),
        );
        assert_eq!(response.payload, b"again");
        assert_eq!(events, [Event::Retransmit { seq: 0, attempt: 2 }]);
        assert_eq!(received.len(), 2);
        assert_eq!(received[0], received[1]);
    }

    #[test]
    fn no_response_loses_the_session() {
        let (results, events, received) = with_link(
            |_| vec![],
            |link| {
                let retry = Retry {
                    timeout: Duration::from_millis(30),
                    retransmits: 2,
                };
                let first = link.request(PING, &[], retry);
                let second = link.request(PING, &[], retry);
                link.start_session(SESSION + 1);
                let third = link.request(PING, &[], retry);
                (first, second, third)
            },
        );
        assert!(matches!(
            results.0,
            Err(Error::NoResponse {
                kind: PING,
                seq: 0,
                attempts: 3
            })
        ));
        assert!(matches!(results.1, Err(Error::SessionLost)));
        // A new session may send again, starting from seq 0.
        assert!(matches!(results.2, Err(Error::NoResponse { seq: 0, .. })));
        assert_eq!(events.len(), 4);
        assert_eq!(received.len(), 6);
        assert_eq!(received[3].session, SESSION + 1);
    }

    #[test]
    fn pending_restarts_the_timer() {
        let (response, events, received) = with_link(
            |request| {
                let mut actions = Vec::new();
                for _ in 0..4 {
                    actions.push(Action::Sleep(Duration::from_millis(60)));
                    actions.push(reply(request, PENDING, &[]));
                }
                actions.push(Action::Sleep(Duration::from_millis(60)));
                actions.extend(echo(request));
                actions
            },
            |link| link.request(PING, b"slow", Retry::new(SHORT)).unwrap(),
        );
        assert_eq!(response.payload, b"slow");
        assert_eq!(events, vec![Event::Pending { seq: 0 }; 4]);
        assert_eq!(received.len(), 1);
    }

    #[test]
    fn unrelated_frames_are_ignored() {
        let (response, events, _) = with_link(
            |request| {
                let other_session = Frame {
                    session: SESSION ^ 1,
                    ..request.clone()
                };
                let other_seq = Frame {
                    seq: request.seq.wrapping_sub(1),
                    ..request.clone()
                };
                vec![
                    Action::Raw(b"\x7egarbage\x7e".to_vec()),
                    reply(&other_session, 0x82, b"old session"),
                    reply(&other_seq, 0x82, b"old seq"),
                    reply(request, 0x83, b"wrong type"),
                    reply(request, 0x82, b"right"),
                ]
            },
            |link| link.request(PING, &[], Retry::new(SHORT)).unwrap(),
        );
        assert_eq!(response.payload, b"right");
        assert_eq!(events[0], Event::Discarded(Discard::BadCrc));
        assert_eq!(events.len(), 4);
        assert!(events[1..].iter().all(|e| matches!(e, Event::Ignored(_))));
    }

    #[test]
    fn error_frame_is_a_response() {
        let (response, _, _) = with_link(
            |request| vec![reply(request, ERROR, &[1, 0, 0, 0])],
            |link| link.request(0x40, &[], Retry::new(SHORT)).unwrap(),
        );
        assert_eq!(
            (response.kind, &response.payload[..]),
            (ERROR, &[1, 0, 0, 0][..])
        );
    }

    #[test]
    fn stalled_partial_frame_is_discarded() {
        let (response, events, _) = with_link(
            |request| {
                let Action::Send(frame) = reply(request, 0x82, b"late") else {
                    unreachable!()
                };
                let wire = frame.encode();
                vec![
                    Action::Raw(wire[..5].to_vec()),
                    Action::Sleep(Duration::from_millis(80)),
                    Action::Raw(wire),
                ]
            },
            |link| {
                link.request(PING, &[], Retry::new(Duration::from_millis(300)))
                    .unwrap()
            },
        );
        assert_eq!(response.payload, b"late");
        assert_eq!(events, [Event::Discarded(Discard::Incomplete)]);
    }

    #[test]
    fn payload_limit() {
        let (results, _, received) = with_link(echo, |link| {
            let too_big = link.request(PING, &[0; MIN_PAYLOAD + 1], Retry::new(SHORT));
            link.set_send_limit(1024);
            let fits = link.request(PING, &[0; 1024], Retry::new(SHORT));
            (too_big, fits)
        });
        assert!(matches!(
            results.0,
            Err(Error::TooLarge { len: 129, max: 128 })
        ));
        assert_eq!(results.1.unwrap().payload.len(), 1024);
        assert_eq!(received.len(), 1);
    }
}
