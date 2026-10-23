// SPDX-License-Identifier: MIT
//! Native protocol v1 session layer, host side: `HELLO` and version and
//! capability negotiation, `PING`, `CLOSE`, and `ERROR` responses
//! (`docs/protocol-v1.md` sections 2 to 4).

use std::fmt;
use std::hash::{BuildHasher, RandomState};
use std::time::{Duration, Instant};

use super::frame::{MAX_PAYLOAD, MIN_PAYLOAD};
use super::link::{self, ERROR, Link, Retry};
use super::tree;
use crate::transport::Transport;

pub const HELLO: u8 = 0x01;
pub const PING: u8 = 0x02;
pub const CLOSE: u8 = 0x03;

/// The first four bytes of `HELLO` and `HELLO_REPLY`.
pub const MAGIC: &[u8; 4] = b"SPRK";
/// The protocol version this host implements.
pub const MAJOR: u8 = 1;
pub const MINOR: u8 = 1;

/// Longest client version string a `HELLO_REPLY` may carry.
const MAX_VERSION_LEN: usize = 32;

/// Host settings for a session.
#[derive(Clone, Debug)]
pub struct Options {
    /// Link rate, used for the default response timeout.
    pub baud: u32,
    /// Largest payload the host accepts, advertised in `HELLO`.
    pub max_payload: usize,
    /// How long to keep repeating `HELLO` before giving up.
    pub wait: Duration,
    /// Interval between `HELLO` repeats.
    pub hello_interval: Duration,
    /// Quiet time to wait for while draining stale input before `HELLO`.
    pub quiet: Duration,
    /// Response timeout for ordinary requests; derived from the baud rate
    /// and payload sizes when `None`.
    pub response_timeout: Option<Duration>,
    /// The previous session ID used on this port, which the new one must
    /// differ from.
    pub previous_session: Option<u16>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            baud: 9600,
            max_payload: MAX_PAYLOAD,
            wait: Duration::from_secs(60),
            hello_interval: Duration::from_secs(1),
            quiet: Duration::from_secs(1),
            response_timeout: None,
            previous_session: None,
        }
    }
}

/// Capability bits from `HELLO_REPLY`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Capabilities(pub u32);

impl Capabilities {
    pub const FILE_WRITE: u32 = 1 << 0;
    pub const FILE_REPLACE: u32 = 1 << 1;
    pub const STORAGE_LIST: u32 = 1 << 2;
    pub const STORAGE_READ_UNITS: u32 = 1 << 3;
    pub const STORAGE_READ_CHS: u32 = 1 << 4;
    pub const FILE_TREE: u32 = 1 << 5;

    const NAMES: [(u32, &'static str); 6] = [
        (Self::FILE_WRITE, "FILE_WRITE"),
        (Self::FILE_REPLACE, "FILE_REPLACE"),
        (Self::STORAGE_LIST, "STORAGE_LIST"),
        (Self::STORAGE_READ_UNITS, "STORAGE_READ_UNITS"),
        (Self::STORAGE_READ_CHS, "STORAGE_READ_CHS"),
        (Self::FILE_TREE, "FILE_TREE"),
    ];

    pub fn has(self, bit: u32) -> bool {
        self.0 & bit != 0
    }
}

impl fmt::Display for Capabilities {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<&str> = Self::NAMES
            .iter()
            .filter(|(bit, _)| self.has(*bit))
            .map(|(_, name)| *name)
            .collect();
        if names.is_empty() {
            f.write_str("none")
        } else {
            f.write_str(&names.join(", "))
        }
    }
}

/// The client's platform, for display only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    Dos,
    AppleII,
    Linux,
    Other(u8),
}

impl Platform {
    /// The platform's number in `HELLO_REPLY`.
    pub fn code(self) -> u8 {
        match self {
            Platform::Dos => 1,
            Platform::AppleII => 2,
            Platform::Linux => 3,
            Platform::Other(code) => code,
        }
    }
}

impl From<u8> for Platform {
    fn from(code: u8) -> Self {
        match code {
            1 => Platform::Dos,
            2 => Platform::AppleII,
            3 => Platform::Linux,
            other => Platform::Other(other),
        }
    }
}

impl fmt::Display for Platform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Platform::Dos => f.write_str("DOS on x86"),
            Platform::AppleII => f.write_str("Apple II"),
            Platform::Linux => f.write_str("Linux"),
            Platform::Other(code) => write!(f, "unknown platform {code}"),
        }
    }
}

/// What the client said about itself in `HELLO_REPLY`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientInfo {
    pub major: u8,
    pub minor: u8,
    pub max_payload: u16,
    pub capabilities: Capabilities,
    pub platform: Platform,
    pub version: String,
}

/// `ERROR` classes (`docs/protocol-v1.md` section 4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorClass {
    Unsupported,
    InvalidRequest,
    ProtocolMismatch,
    MediaError,
    FileError,
    VerifyFailed,
    // Minor version 1 (docs/protocol-v1.1.md section 5).
    NotFound,
    NotADirectory,
    IsADirectory,
    Exists,
    ReadOnly,
    Ambiguous,
    BadName,
    Unknown(u8),
}

impl ErrorClass {
    /// The class's number on the wire.
    pub fn code(self) -> u8 {
        match self {
            ErrorClass::Unsupported => 1,
            ErrorClass::InvalidRequest => 2,
            ErrorClass::ProtocolMismatch => 3,
            ErrorClass::MediaError => 4,
            ErrorClass::FileError => 5,
            ErrorClass::VerifyFailed => 6,
            ErrorClass::NotFound => 7,
            ErrorClass::NotADirectory => 8,
            ErrorClass::IsADirectory => 9,
            ErrorClass::Exists => 10,
            ErrorClass::ReadOnly => 11,
            ErrorClass::Ambiguous => 12,
            ErrorClass::BadName => 13,
            ErrorClass::Unknown(code) => code,
        }
    }
}

impl From<u8> for ErrorClass {
    fn from(code: u8) -> Self {
        match code {
            1 => ErrorClass::Unsupported,
            2 => ErrorClass::InvalidRequest,
            3 => ErrorClass::ProtocolMismatch,
            4 => ErrorClass::MediaError,
            5 => ErrorClass::FileError,
            6 => ErrorClass::VerifyFailed,
            7 => ErrorClass::NotFound,
            8 => ErrorClass::NotADirectory,
            9 => ErrorClass::IsADirectory,
            10 => ErrorClass::Exists,
            11 => ErrorClass::ReadOnly,
            12 => ErrorClass::Ambiguous,
            13 => ErrorClass::BadName,
            other => ErrorClass::Unknown(other),
        }
    }
}

impl fmt::Display for ErrorClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ErrorClass::Unsupported => f.write_str("unsupported"),
            ErrorClass::InvalidRequest => f.write_str("invalid request"),
            ErrorClass::ProtocolMismatch => f.write_str("protocol mismatch"),
            ErrorClass::MediaError => f.write_str("media error"),
            ErrorClass::FileError => f.write_str("file error"),
            ErrorClass::VerifyFailed => f.write_str("verification failed"),
            ErrorClass::NotFound => f.write_str("not found"),
            ErrorClass::NotADirectory => f.write_str("not a directory"),
            ErrorClass::IsADirectory => f.write_str("is a directory"),
            ErrorClass::Exists => f.write_str("exists"),
            ErrorClass::ReadOnly => f.write_str("read-only"),
            ErrorClass::Ambiguous => f.write_str("ambiguous name"),
            ErrorClass::BadName => f.write_str("bad name"),
            ErrorClass::Unknown(code) => write!(f, "error class {code}"),
        }
    }
}

/// An `ERROR` response from the client.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientError {
    pub class: ErrorClass,
    /// Client-specific detail, such as a DOS error or BIOS status.
    pub platform_code: u16,
    pub message: String,
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.class)?;
        if self.platform_code != 0 {
            write!(f, " (code {:#06x})", self.platform_code)?;
        }
        if !self.message.is_empty() {
            write!(f, ": {}", self.message)?;
        }
        Ok(())
    }
}

#[derive(Debug)]
pub enum Error {
    Link(link::Error),
    /// No client answered `HELLO`.
    NoClient {
        waited: Duration,
    },
    /// The client does not support protocol major version 1.
    VersionMismatch {
        major: u8,
        minor: u8,
    },
    /// The client answered with `ERROR`.
    Client(ClientError),
    /// The client's response was malformed.
    Protocol(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Link(e) => write!(f, "{e}"),
            Error::NoClient { waited } => {
                write!(f, "no server answered within {}s", waited.as_secs())
            }
            Error::VersionMismatch { major, minor } => write!(
                f,
                "server speaks protocol {major}.{minor}; this side speaks {MAJOR}.{MINOR}"
            ),
            Error::Client(e) => write!(f, "server reported {e}"),
            Error::Protocol(what) => write!(f, "protocol error: {what}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Link(e) => Some(e),
            _ => None,
        }
    }
}

impl From<link::Error> for Error {
    fn from(e: link::Error) -> Self {
        Error::Link(e)
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Link(link::Error::Io(e))
    }
}

/// An open session with a client.
pub struct Session<'a> {
    link: Link<'a>,
    client: ClientInfo,
    /// Minor version both sides use: the lower of the two.
    minor: u8,
    max_payload: usize,
    retry: Retry,
}

impl<'a> Session<'a> {
    /// Start a session: drain stale input, then repeat `HELLO` with a new
    /// session ID until the client replies or `options.wait` runs out.
    pub fn connect(
        transport: &'a mut dyn Transport,
        options: &Options,
        observer: impl FnMut(&link::Event) + 'a,
    ) -> Result<Self, Error> {
        let mut link = Link::new(transport, options.max_payload).with_observer(observer);
        link.drain(options.quiet)?;
        link.start_session(new_session_id(options.previous_session));

        let mut hello = MAGIC.to_vec();
        hello.extend_from_slice(&[MAJOR, MINOR]);
        hello.extend_from_slice(&(options.max_payload as u16).to_le_bytes());
        let interval = options.hello_interval;
        let retry = Retry {
            timeout: interval,
            retransmits: (options.wait.as_millis() / interval.as_millis().max(1))
                .saturating_sub(1)
                .try_into()
                .unwrap_or(u32::MAX),
        };
        let start = Instant::now();
        let reply = match call(&mut link, HELLO, &hello, retry) {
            Err(Error::Link(link::Error::NoResponse { .. })) => {
                return Err(Error::NoClient {
                    waited: start.elapsed(),
                });
            }
            Err(Error::Client(e)) if e.class == ErrorClass::ProtocolMismatch => {
                let [major, minor] = e.platform_code.to_le_bytes();
                return Err(Error::VersionMismatch { major, minor });
            }
            other => other?,
        };

        let client = parse_hello_reply(&reply)?;
        if client.major != MAJOR {
            return Err(Error::VersionMismatch {
                major: client.major,
                minor: client.minor,
            });
        }
        let max_payload = options.max_payload.min(client.max_payload.into());
        link.set_send_limit(max_payload);
        let timeout = options
            .response_timeout
            .unwrap_or_else(|| link::response_timeout(options.baud, max_payload, max_payload));
        Ok(Self {
            link,
            minor: MINOR.min(client.minor),
            client,
            max_payload,
            retry: Retry::new(timeout),
        })
    }

    pub fn client(&self) -> &ClientInfo {
        &self.client
    }

    pub fn session_id(&self) -> u16 {
        self.link.session()
    }

    /// The protocol minor version in use.
    pub fn minor(&self) -> u8 {
        self.minor
    }

    /// The largest payload either side may send.
    pub fn max_payload(&self) -> usize {
        self.max_payload
    }

    /// Send `PING` with `data` and return the round-trip time.
    pub fn ping(&mut self, data: &[u8]) -> Result<Duration, Error> {
        let start = Instant::now();
        let echo = call(&mut self.link, PING, data, self.retry)?;
        if echo != data {
            return Err(Error::Protocol("PING reply does not match".to_string()));
        }
        Ok(start.elapsed())
    }

    /// Whether the client serves a file tree (protocol v1.1), so that
    /// [`stat`](Self::stat) and the other tree requests may be used.
    pub fn has_tree(&self) -> bool {
        self.minor >= 1 && self.client.capabilities.has(Capabilities::FILE_TREE)
    }

    /// Whether the client accepts files, and replacing existing ones.
    pub fn writable(&self) -> (bool, bool) {
        let caps = self.client.capabilities;
        (
            caps.has(Capabilities::FILE_WRITE),
            caps.has(Capabilities::FILE_REPLACE),
        )
    }

    /// The most file data one `FILE_READ` response can carry.
    pub fn read_chunk(&self) -> usize {
        self.max_payload - tree::READ_HEADER
    }

    /// The most file data one `FILE_DATA` request can carry.
    pub fn write_chunk(&self) -> usize {
        self.max_payload - tree::DATA_HEADER
    }

    /// `STAT`: look up a tree path. The entry's name is the whole path as
    /// spelled on the client.
    pub fn stat(&mut self, path: &str) -> Result<tree::Entry, Error> {
        let mut request = Vec::new();
        tree::put_path(&mut request, checked(path)?);
        let reply = call(&mut self.link, tree::STAT, &request, self.retry)?;
        tree::Entry::decode(&mut Fields {
            data: &reply,
            what: "STAT response",
        })
        .map_err(Error::Protocol)
    }

    /// `DIR_LIST`: every entry of a directory, in the client's order, with
    /// repeated names dropped.
    pub fn list(&mut self, path: &str) -> Result<Vec<tree::Entry>, Error> {
        let path = checked(path)?;
        let mut entries: Vec<tree::Entry> = Vec::new();
        loop {
            let index = u16::try_from(entries.len())
                .map_err(|_| Error::Protocol("directory listing is too long".to_string()))?;
            let mut request = index.to_le_bytes().to_vec();
            tree::put_path(&mut request, path);
            let reply = call(&mut self.link, tree::DIR_LIST, &request, self.retry)?;
            let mut f = Fields {
                data: &reply,
                what: "DIR_LIST response",
            };
            let flags = f.u8()?;
            let count = f.u8()?;
            let mut page = Vec::new();
            for _ in 0..count {
                page.push(tree::Entry::decode(&mut f).map_err(Error::Protocol)?);
            }
            // Repeats can only come from a directory changing between pages.
            let new = page.len();
            for entry in page {
                if !entries.iter().any(|e| e.name == entry.name) {
                    entries.push(entry);
                }
            }
            if flags & tree::MORE == 0 {
                return Ok(entries);
            }
            if new == 0 {
                return Err(Error::Protocol(
                    "DIR_LIST says more entries follow but sent none".to_string(),
                ));
            }
        }
    }

    /// `FILE_READ`: up to `length` bytes from `offset`, with the file's
    /// current size and time.
    pub fn read(&mut self, path: &str, offset: u32, length: u16) -> Result<Chunk, Error> {
        let mut request = offset.to_le_bytes().to_vec();
        request.extend_from_slice(&length.to_le_bytes());
        tree::put_path(&mut request, checked(path)?);
        let reply = call(&mut self.link, tree::FILE_READ, &request, self.retry)?;
        let mut f = Fields {
            data: &reply,
            what: "FILE_READ response",
        };
        let size = f.u32()?;
        let mtime = f.u32()?;
        if f.data.len() > usize::from(length) {
            return Err(Error::Protocol(
                "FILE_READ returned more data than asked".to_string(),
            ));
        }
        Ok(Chunk {
            size,
            mtime,
            data: f.data.to_vec(),
        })
    }

    /// `FILE_BEGIN`: start writing `size` bytes to a tree path. Returns
    /// the `found` bits.
    pub fn begin(&mut self, path: &str, size: u32, replace: bool, mtime: u32) -> Result<u8, Error> {
        let mut request = size.to_le_bytes().to_vec();
        request.push(if replace { tree::REPLACE } else { 0 });
        tree::put_path(&mut request, checked(path)?);
        request.extend_from_slice(&mtime.to_le_bytes());
        let reply = call(&mut self.link, tree::FILE_BEGIN, &request, self.retry)?;
        Ok(reply.first().copied().unwrap_or(0))
    }

    /// `FILE_DATA`: the next part of the file being written.
    pub fn data(&mut self, offset: u32, data: &[u8]) -> Result<(), Error> {
        let mut request = offset.to_le_bytes().to_vec();
        request.extend_from_slice(data);
        call(&mut self.link, tree::FILE_DATA, &request, self.retry)?;
        Ok(())
    }

    /// `FILE_COMMIT`: check the file and put it in place. Returns the
    /// client's CRC-32.
    pub fn commit(&mut self, size: u32, crc: u32) -> Result<u32, Error> {
        let mut request = size.to_le_bytes().to_vec();
        request.extend_from_slice(&crc.to_le_bytes());
        let reply = call(&mut self.link, tree::FILE_COMMIT, &request, self.retry)?;
        Fields {
            data: &reply,
            what: "FILE_COMMIT response",
        }
        .u32()
    }

    /// `FILE_ABORT`: discard the file being written.
    pub fn abort(&mut self) -> Result<(), Error> {
        call(&mut self.link, tree::FILE_ABORT, &[], self.retry)?;
        Ok(())
    }

    /// End the session with `CLOSE`.
    pub fn close(mut self) -> Result<(), Error> {
        call(&mut self.link, CLOSE, &[], self.retry)?;
        Ok(())
    }
}

/// Part of a file, from `FILE_READ`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chunk {
    /// The file's length when this part was read.
    pub size: u32,
    pub mtime: u32,
    pub data: Vec<u8>,
}

/// A tree path short enough to send.
fn checked(path: &str) -> Result<&str, Error> {
    if path.len() > tree::MAX_PATH {
        return Err(Error::Protocol(format!(
            "path of {} bytes is longer than {}",
            path.len(),
            tree::MAX_PATH
        )));
    }
    Ok(path)
}

/// Send a request and return its response payload, turning an `ERROR`
/// response into [`Error::Client`].
fn call(link: &mut Link, kind: u8, payload: &[u8], retry: Retry) -> Result<Vec<u8>, Error> {
    let response = link.request(kind, payload, retry)?;
    if response.kind == ERROR {
        return Err(Error::Client(parse_error(&response.payload)?));
    }
    Ok(response.payload)
}

/// A random, nonzero session ID different from `previous`.
fn new_session_id(previous: Option<u16>) -> u16 {
    let state = RandomState::new();
    (0u32..)
        .map(|i| state.hash_one(i) as u16)
        .find(|&id| id != 0 && Some(id) != previous)
        .expect("a 16-bit ID other than 0 and the previous one")
}

/// Reads little-endian fields from a payload. Bytes after the last field
/// read are ignored, as the extension rules require.
pub(crate) struct Fields<'b> {
    pub data: &'b [u8],
    pub what: &'static str,
}

impl<'b> Fields<'b> {
    pub fn take(&mut self, n: usize) -> Result<&'b [u8], Error> {
        if self.data.len() < n {
            return Err(Error::Protocol(format!("{} is too short", self.what)));
        }
        let (head, rest) = self.data.split_at(n);
        self.data = rest;
        Ok(head)
    }

    pub fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.take(1)?[0])
    }

    pub fn u16(&mut self) -> Result<u16, Error> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    pub fn u32(&mut self) -> Result<u32, Error> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
}

fn parse_hello_reply(payload: &[u8]) -> Result<ClientInfo, Error> {
    let mut f = Fields {
        data: payload,
        what: "HELLO_REPLY",
    };
    if f.take(4)? != MAGIC {
        return Err(Error::Protocol(
            "HELLO_REPLY has the wrong magic".to_string(),
        ));
    }
    let major = f.u8()?;
    let minor = f.u8()?;
    if major != MAJOR {
        // The rest of the layout belongs to another major version.
        return Ok(ClientInfo {
            major,
            minor,
            max_payload: 0,
            capabilities: Capabilities::default(),
            platform: Platform::Other(0),
            version: String::new(),
        });
    }
    let max_payload = f.u16()?;
    if usize::from(max_payload) < MIN_PAYLOAD {
        return Err(Error::Protocol(format!(
            "client maximum payload {max_payload} is below the minimum of {MIN_PAYLOAD}"
        )));
    }
    let capabilities = Capabilities(f.u32()?);
    let platform = Platform::from(f.u8()?);
    let len = usize::from(f.u8()?);
    if len > MAX_VERSION_LEN {
        return Err(Error::Protocol(format!(
            "client version string of {len} bytes is too long"
        )));
    }
    let version = String::from_utf8_lossy(f.take(len)?).into_owned();
    Ok(ClientInfo {
        major,
        minor,
        // A client may not raise the protocol limit.
        max_payload: max_payload.min(MAX_PAYLOAD as u16),
        capabilities,
        platform,
        version,
    })
}

fn parse_error(payload: &[u8]) -> Result<ClientError, Error> {
    let mut f = Fields {
        data: payload,
        what: "ERROR",
    };
    let class = ErrorClass::from(f.u8()?);
    let platform_code = f.u16()?;
    let len = usize::from(f.u8()?);
    let message = String::from_utf8_lossy(f.take(len)?).into_owned();
    Ok(ClientError {
        class,
        platform_code,
        message,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::mock::{self, Config, Log};
    use crate::transport::memory_pair;
    use std::sync::{Arc, Mutex};

    fn options() -> Options {
        Options {
            wait: Duration::from_millis(500),
            hello_interval: Duration::from_millis(50),
            quiet: Duration::from_millis(30),
            response_timeout: Some(Duration::from_millis(200)),
            ..Options::default()
        }
    }

    /// Run `body` against a session with a mock client.
    fn with_client<T>(
        config: Config,
        options: Options,
        body: impl FnOnce(Result<Session, Error>) -> T,
    ) -> (T, Vec<link::Event>, Log) {
        let (mut host, remote) = memory_pair();
        let handle = mock::spawn(remote, config);
        let events = Arc::new(Mutex::new(Vec::new()));
        let result = {
            let log = Arc::clone(&events);
            body(Session::connect(&mut host, &options, move |e| {
                log.lock().unwrap().push(e.clone())
            }))
        };
        drop(host);
        let log = handle.join().unwrap();
        let events = events.lock().unwrap().clone();
        (result, events, log)
    }

    #[test]
    fn handshake_ping_and_close() {
        let config = Config {
            capabilities: Capabilities::FILE_WRITE | Capabilities::FILE_REPLACE,
            version: "0.1.0".to_string(),
            ..Config::default()
        };
        let (info, events, log) = with_client(config, options(), |session| {
            let mut session = session.unwrap();
            assert_ne!(session.session_id(), 0);
            assert_eq!(session.max_payload(), 512);
            assert_eq!(session.minor(), 0);
            session.ping(b"\x7e\x7d\x00\xff").unwrap();
            session.ping(&[0x55; 512]).unwrap();
            let info = session.client().clone();
            session.close().unwrap();
            info
        });
        assert_eq!(
            info,
            ClientInfo {
                major: 1,
                minor: 0,
                max_payload: 512,
                capabilities: Capabilities(3),
                platform: Platform::Dos,
                version: "0.1.0".to_string(),
            }
        );
        assert_eq!(info.capabilities.to_string(), "FILE_WRITE, FILE_REPLACE");
        assert!(events.is_empty());
        assert!(log.closed);
        let seqs: Vec<u8> = log.requests.iter().map(|f| f.seq).collect();
        assert_eq!(seqs, [0, 1, 2, 3]);
    }

    #[test]
    fn hello_repeats_until_the_client_starts() {
        let config = Config {
            ignore_hellos: 3,
            ..Config::default()
        };
        let (_, events, log) = with_client(config, options(), |session| {
            session.unwrap();
        });
        assert_eq!(log.requests.len(), 4);
        // Every repeat is the identical HELLO, in the same session.
        assert!(log.requests.windows(2).all(|w| w[0] == w[1]));
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, link::Event::Retransmit { .. }))
                .count(),
            3
        );
    }

    #[test]
    fn no_client() {
        let (mut host, _remote) = memory_pair();
        let err = Session::connect(&mut host, &options(), |_| {})
            .err()
            .unwrap();
        assert!(matches!(err, Error::NoClient { .. }), "{err}");
    }

    #[test]
    fn version_mismatch() {
        let config = Config {
            major: 2,
            minor: 3,
            ..Config::default()
        };
        let (err, _, _) = with_client(config, options(), |session| session.err().unwrap());
        assert!(
            matches!(err, Error::VersionMismatch { major: 2, minor: 3 }),
            "{err}"
        );
    }

    #[test]
    fn stale_input_is_drained() {
        let old = super::super::frame::Frame {
            kind: HELLO | 0x80,
            session: 0x1234,
            seq: 0,
            payload: b"old reply".to_vec(),
        };
        let mut noise = b"garbage".to_vec();
        noise.extend(old.encode());
        let config = Config {
            noise,
            ..Config::default()
        };
        let (_, events, _) = with_client(config, options(), |session| {
            session.unwrap();
        });
        // The noise was drained before HELLO, so nothing reached the link.
        assert!(events.is_empty());
    }

    #[test]
    fn negotiation_uses_the_smaller_limits() {
        let config = Config {
            minor: 4,
            max_payload: 4096,
            reply_extra: vec![0xAA, 0xBB],
            ..Config::default()
        };
        let options = Options {
            max_payload: 1024,
            ..options()
        };
        let (limits, _, _) = with_client(config, options, |session| {
            let session = session.unwrap();
            (session.max_payload(), session.minor())
        });
        assert_eq!(limits, (1024, MINOR));
    }

    #[test]
    fn new_session_differs_from_previous() {
        for _ in 0..100 {
            let id = new_session_id(None);
            assert_ne!(id, 0);
            assert_ne!(new_session_id(Some(id)), id);
        }
    }

    #[test]
    fn unsupported_request_is_a_client_error() {
        let (err, _, _) = with_client(Config::default(), options(), |session| {
            let mut session = session.unwrap();
            let retry = session.retry;
            call(&mut session.link, 0x40, &[], retry).err().unwrap()
        });
        match err {
            Error::Client(e) => assert_eq!(e.class, ErrorClass::Unsupported),
            other => panic!("{other}"),
        }
    }

    #[test]
    fn hello_reply_parsing() {
        let mut good = MAGIC.to_vec();
        good.extend_from_slice(&[1, 0, 0x00, 0x02, 3, 0, 0, 0, 1, 5]);
        good.extend_from_slice(b"0.1.0");
        let info = parse_hello_reply(&good).unwrap();
        assert_eq!((info.max_payload, info.version.as_str()), (512, "0.1.0"));

        // Trailing fields from a later minor version are ignored.
        let mut longer = good.clone();
        longer.extend_from_slice(&[9, 9, 9]);
        assert_eq!(parse_hello_reply(&longer).unwrap(), info);

        assert!(parse_hello_reply(&good[..good.len() - 1]).is_err());
        let mut bad_magic = good.clone();
        bad_magic[0] = b'X';
        assert!(parse_hello_reply(&bad_magic).is_err());
        let mut small = good.clone();
        small[6..8].copy_from_slice(&64u16.to_le_bytes());
        assert!(parse_hello_reply(&small).is_err());
        let mut huge = good;
        huge[6..8].copy_from_slice(&u16::MAX.to_le_bytes());
        assert_eq!(parse_hello_reply(&huge).unwrap().max_payload, 4090);
    }

    #[test]
    fn error_parsing() {
        let e = parse_error(&[5, 0x05, 0x00, 6, b'd', b'e', b'n', b'i', b'e', b'd']).unwrap();
        assert_eq!(e.to_string(), "file error (code 0x0005): denied");
        assert!(parse_error(&[5, 0, 0, 9, b'x']).is_err());
        assert_eq!(
            parse_error(&[42, 0, 0, 0]).unwrap().class,
            ErrorClass::Unknown(42)
        );
    }
}
