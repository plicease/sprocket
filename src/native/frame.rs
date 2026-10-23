// SPDX-License-Identifier: MIT
//! Native protocol v1 framing: HDLC-style byte stuffing and CRC-16.
//!
//! A frame on the wire is
//!
//! ```text
//! 7E | escaped( type | session_lo | session_hi | seq | payload… | crc_lo | crc_hi ) | 7E
//! ```
//!
//! Inside a frame, `7E` and `7D` are sent as `7D` followed by the byte XOR
//! `20`. The CRC is CRC-16/CCITT-FALSE over the unescaped header and
//! payload, sent little-endian. See `docs/protocol-v1.md` section 1.

/// Frame delimiter.
pub const FLAG: u8 = 0x7E;
/// Escape prefix; the next byte is XORed with [`ESCAPE_XOR`].
pub const ESCAPE: u8 = 0x7D;
const ESCAPE_XOR: u8 = 0x20;

/// Unescaped bytes before the payload: `type`, `session` (u16), `seq`.
pub const HEADER_LEN: usize = 4;
const CRC_LEN: usize = 2;

/// Smallest maximum payload any implementation may advertise.
pub const MIN_PAYLOAD: usize = 128;
/// Largest payload the protocol allows: the limit for which CRC-16 still
/// detects every double-bit error.
pub const MAX_PAYLOAD: usize = 4090;

/// CRC-16/CCITT-FALSE: polynomial `0x1021`, initial `0xFFFF`, no
/// reflection, no final XOR.
pub fn crc16(data: &[u8]) -> u16 {
    let mut crc = 0xFFFFu16;
    for &byte in data {
        crc ^= u16::from(byte) << 8;
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

/// One decoded frame, without its CRC.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    /// Message type (`type` in the specification).
    pub kind: u8,
    pub session: u16,
    pub seq: u8,
    pub payload: Vec<u8>,
}

impl Frame {
    /// The unescaped header and payload: the bytes the CRC covers.
    pub fn body(&self) -> Vec<u8> {
        let mut body = Vec::with_capacity(HEADER_LEN + self.payload.len());
        body.push(self.kind);
        body.extend_from_slice(&self.session.to_le_bytes());
        body.push(self.seq);
        body.extend_from_slice(&self.payload);
        body
    }

    /// Encode for the wire, with an opening and a closing delimiter.
    ///
    /// # Panics
    ///
    /// If the payload is longer than [`MAX_PAYLOAD`]; callers must keep to
    /// the negotiated limit, which is never larger.
    pub fn encode(&self) -> Vec<u8> {
        assert!(
            self.payload.len() <= MAX_PAYLOAD,
            "payload of {} bytes exceeds the protocol maximum",
            self.payload.len()
        );
        let mut body = self.body();
        let crc = crc16(&body);
        body.extend_from_slice(&crc.to_le_bytes());

        let mut wire = Vec::with_capacity(body.len() + body.len() / 64 + 2);
        wire.push(FLAG);
        for byte in body {
            if byte == FLAG || byte == ESCAPE {
                wire.extend_from_slice(&[ESCAPE, byte ^ ESCAPE_XOR]);
            } else {
                wire.push(byte);
            }
        }
        wire.push(FLAG);
        wire
    }

    /// Split an unescaped body (header and payload) into its fields.
    fn from_body(body: &[u8]) -> Self {
        Self {
            kind: body[0],
            session: u16::from_le_bytes([body[1], body[2]]),
            seq: body[3],
            payload: body[HEADER_LEN..].to_vec(),
        }
    }
}

/// Why a frame was dropped. The protocol drops damaged frames silently;
/// these reasons are for logs and traces only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Discard {
    /// Fewer unescaped bytes than a header and CRC.
    TooShort,
    /// The CRC did not match.
    BadCrc,
    /// `7D` followed by a byte other than `5E` or `5D`.
    BadEscape,
    /// `7D 7E`: the sender aborted the frame.
    Aborted,
    /// The payload exceeded the decoder's maximum.
    TooLong,
    /// A partial frame was abandoned after an inter-byte timeout.
    Incomplete,
}

impl std::fmt::Display for Discard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::TooShort => "too short",
            Self::BadCrc => "bad CRC",
            Self::BadEscape => "bad escape",
            Self::Aborted => "aborted",
            Self::TooLong => "too long",
            Self::Incomplete => "incomplete",
        })
    }
}

/// The result of a completed or abandoned frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Frame(Frame),
    Discarded(Discard),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    /// Waiting for a delimiter; bytes before it are noise.
    Hunt,
    /// Inside a frame (or between frames, with an empty buffer).
    Body,
    /// The previous byte was `7D`.
    Escape,
    /// The frame is already invalid; skip to the next delimiter.
    Skip(Discard),
}

/// Incremental frame decoder, fed one byte at a time.
///
/// Holds at most one frame's unescaped bytes, bounded by the maximum
/// payload. Empty frames (repeated delimiters) are ignored.
pub struct Decoder {
    limit: usize,
    buf: Vec<u8>,
    state: State,
}

impl Decoder {
    /// A decoder accepting payloads up to `max_payload` bytes.
    ///
    /// # Panics
    ///
    /// If `max_payload` is outside [`MIN_PAYLOAD`]..=[`MAX_PAYLOAD`].
    pub fn new(max_payload: usize) -> Self {
        assert!(
            (MIN_PAYLOAD..=MAX_PAYLOAD).contains(&max_payload),
            "maximum payload {max_payload} is outside the protocol limits"
        );
        let limit = HEADER_LEN + max_payload + CRC_LEN;
        Self {
            limit,
            buf: Vec::with_capacity(limit),
            state: State::Hunt,
        }
    }

    /// Feed one received byte. Returns an event when a delimiter completes
    /// or aborts a frame.
    pub fn push(&mut self, byte: u8) -> Option<Event> {
        match self.state {
            State::Hunt => {
                if byte == FLAG {
                    self.start();
                }
                None
            }
            State::Body => match byte {
                FLAG => self.finish(),
                ESCAPE => {
                    self.state = State::Escape;
                    None
                }
                _ => {
                    self.store(byte);
                    None
                }
            },
            State::Escape => match byte {
                FLAG => {
                    // The 7E also opens the next frame.
                    self.start();
                    Some(Event::Discarded(Discard::Aborted))
                }
                0x5E | 0x5D => {
                    self.state = State::Body;
                    self.store(byte ^ ESCAPE_XOR);
                    None
                }
                _ => {
                    self.skip(Discard::BadEscape);
                    None
                }
            },
            State::Skip(reason) => {
                if byte == FLAG {
                    self.start();
                    Some(Event::Discarded(reason))
                } else {
                    None
                }
            }
        }
    }

    /// Abandon a partial frame after the inter-byte timeout. The decoder
    /// then ignores everything until the next delimiter.
    pub fn timeout(&mut self) -> Option<Event> {
        let reason = match self.state {
            State::Hunt => return None,
            State::Body if self.buf.is_empty() => return None,
            State::Body | State::Escape => Discard::Incomplete,
            State::Skip(reason) => reason,
        };
        self.buf.clear();
        self.state = State::Hunt;
        Some(Event::Discarded(reason))
    }

    /// Whether part of a frame has been received.
    pub fn in_frame(&self) -> bool {
        match self.state {
            State::Hunt => false,
            State::Body => !self.buf.is_empty(),
            State::Escape | State::Skip(_) => true,
        }
    }

    fn start(&mut self) {
        self.buf.clear();
        self.state = State::Body;
    }

    fn skip(&mut self, reason: Discard) {
        self.buf.clear();
        self.state = State::Skip(reason);
    }

    fn store(&mut self, byte: u8) {
        if self.buf.len() == self.limit {
            self.skip(Discard::TooLong);
        } else {
            self.buf.push(byte);
        }
    }

    fn finish(&mut self) -> Option<Event> {
        let len = self.buf.len();
        let event = if len == 0 {
            None
        } else if len < HEADER_LEN + CRC_LEN {
            Some(Event::Discarded(Discard::TooShort))
        } else {
            let (body, crc) = self.buf.split_at(len - CRC_LEN);
            if crc16(body) == u16::from_le_bytes([crc[0], crc[1]]) {
                Some(Event::Frame(Frame::from_body(body)))
            } else {
                Some(Event::Discarded(Discard::BadCrc))
            }
        };
        self.start();
        event
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Records from a fixture file: lists of (field, value) pairs.
    fn records(text: &str) -> Vec<Vec<(&str, &str)>> {
        text.split("\n\n")
            .map(|record| {
                record
                    .lines()
                    .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
                    .map(|line| {
                        let (key, value) = line.split_once(' ').unwrap_or((line, ""));
                        (key, value.trim())
                    })
                    .collect::<Vec<_>>()
            })
            .filter(|record| !record.is_empty())
            .collect()
    }

    fn field<'a>(record: &[(&str, &'a str)], key: &str) -> Option<&'a str> {
        record.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
    }

    fn hex(text: &str) -> Vec<u8> {
        text.split_whitespace()
            .map(|b| u8::from_str_radix(b, 16).unwrap())
            .collect()
    }

    fn decode_all(decoder: &mut Decoder, wire: &[u8]) -> Vec<Event> {
        wire.iter().filter_map(|&b| decoder.push(b)).collect()
    }

    #[test]
    fn crc_check_value() {
        assert_eq!(crc16(b"123456789"), 0x29B1);
    }

    #[test]
    fn valid_vectors() {
        let fixtures = records(include_str!("../../tests/vectors/frames.txt"));
        assert!(fixtures.len() >= 6);
        for record in fixtures {
            let name = field(&record, "name").unwrap();
            let body = hex(field(&record, "body").unwrap());
            let crc = u16::from_str_radix(field(&record, "crc").unwrap(), 16).unwrap();
            let wire = hex(field(&record, "wire").unwrap());

            assert_eq!(crc16(&body), crc, "{name}: crc");
            let frame = Frame::from_body(&body);
            assert_eq!(frame.body(), body, "{name}: body");
            assert_eq!(frame.encode(), wire, "{name}: encode");

            let mut decoder = Decoder::new(MAX_PAYLOAD);
            assert_eq!(
                decode_all(&mut decoder, &wire),
                [Event::Frame(frame)],
                "{name}: decode"
            );
        }
    }

    #[test]
    fn malformed_vectors() {
        let fixtures = records(include_str!("../../tests/vectors/malformed.txt"));
        assert!(fixtures.len() >= 10);
        for record in fixtures {
            let name = field(&record, "name").unwrap();
            let wire = hex(field(&record, "wire").unwrap());
            let max = field(&record, "max").map_or(MAX_PAYLOAD, |m| m.parse().unwrap());
            let expected: Vec<Event> = record
                .iter()
                .filter(|(k, _)| *k == "expect")
                .map(|(_, v)| match v.split_once(' ').unwrap_or((v, "")) {
                    ("frame", body) => Event::Frame(Frame::from_body(&hex(body))),
                    ("drop", reason) => Event::Discarded(match reason {
                        "too-short" => Discard::TooShort,
                        "bad-crc" => Discard::BadCrc,
                        "bad-escape" => Discard::BadEscape,
                        "aborted" => Discard::Aborted,
                        "too-long" => Discard::TooLong,
                        other => panic!("{name}: unknown reason {other}"),
                    }),
                    (other, _) => panic!("{name}: unknown expectation {other}"),
                })
                .collect();

            let mut decoder = Decoder::new(max);
            assert_eq!(decode_all(&mut decoder, &wire), expected, "{name}");
        }
    }

    #[test]
    fn overflow_does_not_buffer_the_rest() {
        let frame = Frame {
            kind: 0x02,
            session: 1,
            seq: 0,
            payload: vec![0; MAX_PAYLOAD],
        };
        let mut decoder = Decoder::new(MIN_PAYLOAD);
        let events = decode_all(&mut decoder, &frame.encode());
        assert_eq!(events, [Event::Discarded(Discard::TooLong)]);
        assert!(decoder.buf.capacity() <= HEADER_LEN + MIN_PAYLOAD + CRC_LEN);
    }

    #[test]
    fn timeout_abandons_partial_frame() {
        let frame = Frame {
            kind: 0x02,
            session: 0x5A3C,
            seq: 1,
            payload: b"ping".to_vec(),
        };
        let wire = frame.encode();
        let mut decoder = Decoder::new(MIN_PAYLOAD);

        // Between frames, a timeout is not an error.
        assert_eq!(decoder.timeout(), None);
        decode_all(&mut decoder, &wire);
        assert!(!decoder.in_frame());
        assert_eq!(decoder.timeout(), None);

        // Half a frame, a pause, then the rest: the tail is noise, and the
        // next whole frame is accepted.
        let (head, tail) = wire.split_at(5);
        assert_eq!(decode_all(&mut decoder, head), []);
        assert!(decoder.in_frame());
        assert_eq!(
            decoder.timeout(),
            Some(Event::Discarded(Discard::Incomplete))
        );
        assert_eq!(decode_all(&mut decoder, tail), []);
        assert_eq!(decode_all(&mut decoder, &wire), [Event::Frame(frame)]);
    }

    #[test]
    fn escape_heavy_payload_round_trips() {
        let frame = Frame {
            kind: 0x7E,
            session: 0x7D7E,
            seq: 0x7D,
            payload: [FLAG, ESCAPE].repeat(MIN_PAYLOAD / 2),
        };
        let wire = frame.encode();
        // Every body byte is escaped, so the frame doubles in size.
        assert!(wire.len() > 2 * frame.body().len());
        let mut decoder = Decoder::new(MIN_PAYLOAD);
        assert_eq!(decode_all(&mut decoder, &wire), [Event::Frame(frame)]);
    }

    #[test]
    #[should_panic(expected = "exceeds the protocol maximum")]
    fn oversized_payload_is_refused() {
        Frame {
            kind: 0x02,
            session: 1,
            seq: 0,
            payload: vec![0; MAX_PAYLOAD + 1],
        }
        .encode();
    }
}
