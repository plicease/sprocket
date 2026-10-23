// SPDX-License-Identifier: MIT
//! A minimal native v1 client for host tests: `HELLO`, `PING` and `CLOSE`,
//! with the duplicate-request rules of `docs/protocol-v1.md` section 2.

use std::thread;
use std::time::Duration;

use super::frame::{Decoder, Event as FrameEvent, Frame, MAX_PAYLOAD, crc16};
use super::link::{ERROR, RESPONSE};
use super::session::{CLOSE, HELLO, MAGIC, PING};
use crate::transport::{MemoryTransport, Transport};

/// What the mock client reports and how it misbehaves.
#[derive(Clone, Debug)]
pub(crate) struct Config {
    pub major: u8,
    pub minor: u8,
    pub max_payload: u16,
    pub capabilities: u32,
    pub platform: u8,
    pub version: String,
    /// Bytes appended to `HELLO_REPLY`, as a later minor version might.
    pub reply_extra: Vec<u8>,
    /// Drop this many `HELLO` requests before answering, as if the client
    /// started late.
    pub ignore_hellos: usize,
    /// Write these bytes before reading anything, as stale line noise.
    pub noise: Vec<u8>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            major: 1,
            minor: 0,
            max_payload: 512,
            capabilities: 0,
            platform: 1,
            version: "mock".to_string(),
            reply_extra: Vec::new(),
            ignore_hellos: 0,
            noise: Vec::new(),
        }
    }
}

/// What the mock client saw.
#[derive(Debug, Default)]
pub(crate) struct Log {
    /// Every valid request, including duplicates and ignored ones.
    pub requests: Vec<Frame>,
    /// Requests answered from the duplicate cache.
    pub duplicates: usize,
    pub closed: bool,
}

/// The last accepted request: `seq`, type, length and CRC, and the
/// response sent for it.
struct Record {
    seq: u8,
    kind: u8,
    len: usize,
    crc: u16,
    response: Vec<u8>,
}

impl Record {
    fn matches(&self, frame: &Frame, crc: u16) -> bool {
        self.seq == frame.seq
            && self.kind == frame.kind
            && self.len == frame.payload.len()
            && self.crc == crc
    }
}

/// Run the client on its own thread until the host end disconnects or
/// goes quiet for five seconds.
pub(crate) fn spawn(mut transport: MemoryTransport, config: Config) -> thread::JoinHandle<Log> {
    thread::spawn(move || {
        let mut log = Log::default();
        let _ = transport.write_all(&config.noise);
        let mut decoder = Decoder::new(MAX_PAYLOAD);
        let mut session: Option<u16> = None;
        let mut record: Option<Record> = None;
        let mut hellos_ignored = 0;

        while let Ok(Some(byte)) = transport.read_byte(Duration::from_secs(5)) {
            let Some(FrameEvent::Frame(frame)) = decoder.push(byte) else {
                continue;
            };
            log.requests.push(frame.clone());
            let crc = crc16(&frame.body());
            let current = session == Some(frame.session);

            if frame.kind == HELLO && hellos_ignored < config.ignore_hellos {
                hellos_ignored += 1;
                continue;
            }
            if frame.kind != HELLO && !current {
                continue;
            }
            if let Some(rec) = record.as_ref().filter(|_| current) {
                if rec.matches(&frame, crc) {
                    log.duplicates += 1;
                    if transport.write_all(&rec.response).is_err() {
                        break;
                    }
                    continue;
                }
                if rec.seq == frame.seq && frame.kind != HELLO {
                    // Same seq, different request: refuse without caching.
                    let wire = reply(&frame, ERROR, &[2, 0, 0, 0]);
                    if transport.write_all(&wire).is_err() {
                        break;
                    }
                    continue;
                }
            }
            if frame.kind == HELLO {
                session = Some(frame.session);
            }

            let response = match frame.kind {
                HELLO => hello_reply(&frame, &config),
                PING => reply(&frame, PING | RESPONSE, &frame.payload),
                CLOSE => {
                    log.closed = true;
                    reply(&frame, CLOSE | RESPONSE, &[])
                }
                _ => reply(&frame, ERROR, &[1, 0, 0, 0]),
            };
            record = Some(Record {
                seq: frame.seq,
                kind: frame.kind,
                len: frame.payload.len(),
                crc,
                response: response.clone(),
            });
            if transport.write_all(&response).is_err() {
                break;
            }
        }
        log
    })
}

fn reply(request: &Frame, kind: u8, payload: &[u8]) -> Vec<u8> {
    Frame {
        kind,
        session: request.session,
        seq: request.seq,
        payload: payload.to_vec(),
    }
    .encode()
}

fn hello_reply(request: &Frame, config: &Config) -> Vec<u8> {
    let host_major = request.payload.get(4).copied().unwrap_or(0);
    if host_major != config.major {
        let code = u16::from_le_bytes([config.major, config.minor]).to_le_bytes();
        return reply(request, ERROR, &[3, code[0], code[1], 0]);
    }
    let mut payload = MAGIC.to_vec();
    payload.extend_from_slice(&[config.major, config.minor]);
    payload.extend_from_slice(&config.max_payload.to_le_bytes());
    payload.extend_from_slice(&config.capabilities.to_le_bytes());
    payload.push(config.platform);
    payload.push(config.version.len() as u8);
    payload.extend_from_slice(config.version.as_bytes());
    payload.extend_from_slice(&config.reply_extra);
    reply(request, HELLO | RESPONSE, &payload)
}
