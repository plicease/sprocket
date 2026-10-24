// SPDX-License-Identifier: MIT
//! Native protocol responder serving a directory tree: `sprocket serve
//! --mode native` (`docs/protocol-v1.1.md`), and optionally disks to image
//! (`docs/protocol-v1.md` section 6).
//!
//! It answers one request at a time with the duplicate rules of v1
//! section 2, so a retransmitted request is never executed twice, and
//! keeps answering new sessions until the link fails. Writing is refused
//! unless it was turned on, in which case files arrive in a hidden
//! temporary file that is renamed into place only after its length and
//! CRC-32 check.

use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use super::disk::Disk;
use super::frame::{Decoder, Event as FrameEvent, Frame, MAX_PAYLOAD, MIN_PAYLOAD, crc16};
use super::link::{ERROR, INTER_BYTE_TIMEOUT, RESPONSE};
use super::session::{CLOSE, Capabilities, ErrorClass, Fields, HELLO, MAGIC, PING, Platform};
use super::storage::{self, Address, ReadRequest};
use super::tree::{self, Crc32, Entry};
use crate::files::{Kind, Root};
use crate::transport::Transport;

/// Server settings.
#[derive(Clone, Debug)]
pub struct Options {
    /// Accept files (`put`), and replacing existing ones when asked.
    pub writable: bool,
    /// Largest payload this server accepts.
    pub max_payload: usize,
    /// Abandon a partial frame after this long without a byte.
    pub inter_byte: Duration,
    /// Disks offered for imaging, numbered from 0 in this order.
    pub disks: Vec<Arc<dyn Disk>>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            writable: false,
            max_payload: MAX_PAYLOAD,
            inter_byte: INTER_BYTE_TIMEOUT,
            disks: Vec::new(),
        }
    }
}

/// Answer requests on `transport` until it fails (for example, the TCP
/// peer disconnects).
pub fn run(
    transport: &mut dyn Transport,
    root: &Root,
    options: &Options,
    log: &mut dyn FnMut(&str),
) -> io::Result<()> {
    let mut decoder = Decoder::new(options.max_payload);
    let mut server = Server::new(root, options);
    loop {
        let event = match transport.read_byte(options.inter_byte)? {
            Some(byte) => decoder.push(byte),
            None => decoder.timeout(),
        };
        if let Some(FrameEvent::Frame(frame)) = event
            && let Some(wire) = server.handle(&frame, log)
        {
            transport.write_all(&wire)?;
        }
    }
}

/// The last request accepted: `seq`, type, length and CRC, and the
/// response sent for it.
struct Record {
    seq: u8,
    kind: u8,
    len: usize,
    crc: u16,
    response: Vec<u8>,
}

/// A file being received.
struct Upload {
    file: File,
    temp: PathBuf,
    dest: PathBuf,
    /// The destination as the user named it, for the log.
    shown: String,
    size: u32,
    mtime: u32,
    next: u32,
    crc: Crc32,
}

/// A refusal: an `ERROR` class with optional detail.
struct Refusal {
    class: ErrorClass,
    code: u16,
    message: String,
}

impl Refusal {
    fn new(class: ErrorClass) -> Self {
        Self {
            class,
            code: 0,
            message: String::new(),
        }
    }

    fn invalid() -> Self {
        Self::new(ErrorClass::InvalidRequest)
    }
}

impl From<io::Error> for Refusal {
    fn from(e: io::Error) -> Self {
        let class = tree::class_of(&e);
        Self {
            class,
            code: e
                .raw_os_error()
                .and_then(|c| u16::try_from(c).ok())
                .unwrap_or(0),
            message: if class == ErrorClass::FileError {
                e.to_string()
            } else {
                String::new()
            },
        }
    }
}

impl From<super::session::Error> for Refusal {
    fn from(_: super::session::Error) -> Self {
        Refusal::invalid()
    }
}

struct Server<'a> {
    root: &'a Root,
    options: &'a Options,
    session: Option<u16>,
    record: Option<Record>,
    /// Minor version in use: the lower of the requester's and ours.
    minor: u8,
    /// Largest payload the requester accepts.
    send_limit: usize,
    upload: Option<Upload>,
}

impl<'a> Server<'a> {
    fn new(root: &'a Root, options: &'a Options) -> Self {
        Self {
            root,
            options,
            session: None,
            record: None,
            minor: 0,
            send_limit: MIN_PAYLOAD,
            upload: None,
        }
    }

    /// Handle one valid frame; returns the encoded response, if any.
    fn handle(&mut self, frame: &Frame, log: &mut dyn FnMut(&str)) -> Option<Vec<u8>> {
        if frame.session == 0 || frame.kind >= RESPONSE {
            return None;
        }
        let current = self.session == Some(frame.session);
        if frame.kind != HELLO && !current {
            return None;
        }
        let crc = crc16(&frame.body());
        if let Some(rec) = self.record.as_ref().filter(|_| current) {
            if rec.seq == frame.seq
                && rec.kind == frame.kind
                && rec.len == frame.payload.len()
                && rec.crc == crc
            {
                return Some(rec.response.clone());
            }
            if rec.seq == frame.seq && frame.kind != HELLO {
                // Same seq, different request: refuse, keep the cache.
                return Some(self.refuse(frame, Refusal::invalid()));
            }
        }
        if frame.kind == HELLO {
            self.discard_upload();
            self.session = Some(frame.session);
            log(&format!("session {:04x} started", frame.session));
        }
        let response = match self.execute(frame, log) {
            Ok((kind, payload)) => reply(frame, kind, &payload),
            Err(refusal) => self.refuse(frame, refusal),
        };
        self.record = Some(Record {
            seq: frame.seq,
            kind: frame.kind,
            len: frame.payload.len(),
            crc,
            response: response.clone(),
        });
        Some(response)
    }

    fn refuse(&self, frame: &Frame, refusal: Refusal) -> Vec<u8> {
        let mut class = refusal.class;
        let mut message = refusal.message;
        // Classes from minor version 1 only go to requesters that know them.
        if self.minor == 0 && class.code() > 6 {
            if message.is_empty() {
                message = class.to_string();
            }
            class = ErrorClass::FileError;
        }
        message.truncate(self.send_limit.saturating_sub(4).min(255));
        let mut payload = vec![class.code()];
        payload.extend_from_slice(&refusal.code.to_le_bytes());
        payload.push(message.len() as u8);
        payload.extend_from_slice(message.as_bytes());
        reply(frame, ERROR, &payload)
    }

    fn execute(
        &mut self,
        frame: &Frame,
        log: &mut dyn FnMut(&str),
    ) -> Result<(u8, Vec<u8>), Refusal> {
        let kind = frame.kind | RESPONSE;
        let p = &frame.payload;
        match frame.kind {
            HELLO => self.hello(p).map(|payload| (kind, payload)),
            PING if p.len() <= self.send_limit => Ok((kind, p.clone())),
            PING => Err(Refusal::invalid()),
            CLOSE => {
                self.discard_upload();
                log("session closed");
                Ok((kind, Vec::new()))
            }
            tree::STAT => self.stat(p).map(|payload| (kind, payload)),
            tree::DIR_LIST => self.dir_list(p).map(|payload| (kind, payload)),
            tree::FILE_READ => {
                let payload = self.read(p)?;
                // Log each file once, at its first part.
                if p.len() >= 4 && p[..4] == [0, 0, 0, 0] {
                    let size = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
                    let path = String::from_utf8_lossy(p.get(7..).unwrap_or_default());
                    log(&format!(
                        "sending /{} ({size} bytes)",
                        path.trim_start_matches('/')
                    ));
                }
                Ok((kind, payload))
            }
            tree::FILE_BEGIN => self.begin(p).map(|payload| (kind, payload)),
            tree::FILE_DATA => self.data(p).map(|()| (kind, Vec::new())),
            tree::FILE_COMMIT => self.commit(p, log).map(|payload| (kind, payload)),
            tree::FILE_ABORT => {
                self.discard_upload();
                Ok((kind, Vec::new()))
            }
            storage::STORAGE_LIST if !self.options.disks.is_empty() => {
                self.storage_list().map(|payload| (kind, payload))
            }
            storage::STORAGE_READ if !self.options.disks.is_empty() => {
                self.storage_read(p).map(|payload| (kind, payload))
            }
            _ => Err(Refusal::new(ErrorClass::Unsupported)),
        }
    }

    fn hello(&mut self, p: &[u8]) -> Result<Vec<u8>, Refusal> {
        if p.len() < 8 || &p[..4] != MAGIC {
            return Err(Refusal::invalid());
        }
        if p[4] != super::session::MAJOR {
            return Err(Refusal {
                class: ErrorClass::ProtocolMismatch,
                code: u16::from_le_bytes([super::session::MAJOR, super::session::MINOR]),
                message: String::new(),
            });
        }
        self.minor = p[5].min(super::session::MINOR);
        let host_max = usize::from(u16::from_le_bytes([p[6], p[7]]));
        self.send_limit = host_max
            .clamp(MIN_PAYLOAD, MAX_PAYLOAD)
            .min(self.options.max_payload);
        let mut caps = Capabilities::FILE_TREE;
        if self.options.writable {
            caps |= Capabilities::FILE_WRITE | Capabilities::FILE_REPLACE;
        }
        if !self.options.disks.is_empty() {
            caps |= Capabilities::STORAGE_LIST | Capabilities::STORAGE_READ_UNITS;
            if self.options.disks.iter().any(|d| d.describe().chs()) {
                caps |= Capabilities::STORAGE_READ_CHS;
            }
        }
        let version = env!("CARGO_PKG_VERSION");
        let mut out = MAGIC.to_vec();
        out.extend_from_slice(&[super::session::MAJOR, super::session::MINOR]);
        out.extend_from_slice(&(self.options.max_payload as u16).to_le_bytes());
        out.extend_from_slice(&caps.to_le_bytes());
        out.push(Platform::Linux.code());
        out.push(version.len() as u8);
        out.extend_from_slice(version.as_bytes());
        Ok(out)
    }

    /// Find a tree path, ignoring case; returns its path in the root.
    fn find(&self, path: &str) -> Result<PathBuf, Refusal> {
        let parts = tree::components(path).ok_or(Refusal::new(ErrorClass::NotFound))?;
        Ok(self.root.find(&parts.iter().collect::<PathBuf>())?)
    }

    fn entry(&self, rel: &Path, name: String) -> Result<Entry, Refusal> {
        let kind = self.root.stat(rel)?;
        let modified = fs::metadata(self.root.resolve(rel)?)?.modified().ok();
        Ok(match kind {
            Kind::Dir => Entry {
                dir: true,
                size: 0,
                mtime: tree::dos_time(modified),
                name,
            },
            Kind::File { size } => Entry {
                dir: false,
                size: u32::try_from(size).unwrap_or(u32::MAX),
                mtime: tree::dos_time(modified),
                name,
            },
        })
    }

    fn stat(&self, p: &[u8]) -> Result<Vec<u8>, Refusal> {
        let path = path_field(&mut fields(p))?;
        let rel = self.find(&path)?;
        let entry = self.entry(&rel, tree_path(&rel))?;
        let mut out = Vec::new();
        entry.encode(&mut out);
        Ok(out)
    }

    fn dir_list(&self, p: &[u8]) -> Result<Vec<u8>, Refusal> {
        let mut f = fields(p);
        let index = usize::from(f.u16()?);
        let path = path_field(&mut f)?;
        let rel = self.find(&path)?;
        if self.root.stat(&rel)? != Kind::Dir {
            return Err(Refusal::new(ErrorClass::NotADirectory));
        }
        // Names the line can carry, and that fit a page by themselves.
        let longest = self.send_limit - 2 - 11;
        let names: Vec<_> = self
            .root
            .list(&rel)?
            .into_iter()
            .filter(|e| e.name.len() <= longest.min(255))
            .filter(|e| e.name.bytes().all(|b| (0x20..=0x7e).contains(&b)))
            .collect();
        let mut body = Vec::new();
        let mut count = 0u8;
        let mut next = index;
        while let Some(item) = names.get(next) {
            let Ok(entry) = self.entry(&rel.join(&item.name), item.name.clone()) else {
                next += 1;
                continue;
            };
            if 2 + body.len() + entry.encoded_len() > self.send_limit || count == u8::MAX {
                break;
            }
            entry.encode(&mut body);
            count += 1;
            next += 1;
        }
        let more = next < names.len();
        let mut out = vec![if more { tree::MORE } else { 0 }, count];
        out.extend_from_slice(&body);
        Ok(out)
    }

    fn read(&self, p: &[u8]) -> Result<Vec<u8>, Refusal> {
        let mut f = fields(p);
        let offset = f.u32()?;
        let length = usize::from(f.u16()?).min(self.send_limit - tree::READ_HEADER);
        let path = path_field(&mut f)?;
        let rel = self.find(&path)?;
        let Kind::File { size } = self.root.stat(&rel)? else {
            return Err(Refusal::new(ErrorClass::IsADirectory));
        };
        let size = u32::try_from(size).map_err(|_| Refusal {
            class: ErrorClass::FileError,
            code: 0,
            message: "file is larger than 4 GB".to_string(),
        })?;
        let real = self.root.resolve(&rel)?;
        let mut file = File::open(&real)?;
        let mtime = tree::dos_time(file.metadata()?.modified().ok());
        let mut out = size.to_le_bytes().to_vec();
        out.extend_from_slice(&mtime.to_le_bytes());
        if offset < size {
            file.seek(SeekFrom::Start(u64::from(offset)))?;
            let mut data = Vec::with_capacity(length);
            file.take(length as u64).read_to_end(&mut data)?;
            out.extend_from_slice(&data);
        }
        Ok(out)
    }

    fn begin(&mut self, p: &[u8]) -> Result<Vec<u8>, Refusal> {
        if !self.options.writable {
            return Err(Refusal::new(ErrorClass::ReadOnly));
        }
        self.discard_upload();
        let mut f = fields(p);
        let size = f.u32()?;
        let flags = f.u8()?;
        if flags & !tree::REPLACE != 0 {
            return Err(Refusal::invalid());
        }
        let path = path_field(&mut f)?;
        // A trailing field from minor version 1.
        let mtime = if f.data.len() >= 4 { f.u32()? } else { 0 };
        let mut parts = tree::components(&path).ok_or(Refusal::new(ErrorClass::NotFound))?;
        let Some(name) = parts.pop() else {
            return Err(Refusal::new(ErrorClass::IsADirectory));
        };
        let dir = self.root.find(&parts.iter().collect::<PathBuf>())?;
        if self.root.stat(&dir)? != Kind::Dir {
            return Err(Refusal::new(ErrorClass::NotADirectory));
        }
        let existing = self.root.find(&dir.join(name));
        let (name, exists) = match existing {
            Ok(rel) => {
                if self.root.stat(&rel)? == Kind::Dir {
                    return Err(Refusal::new(ErrorClass::IsADirectory));
                }
                if flags & tree::REPLACE == 0 {
                    return Err(Refusal::new(ErrorClass::Exists));
                }
                let name = rel.file_name().map(|n| n.to_string_lossy().into_owned());
                (name.unwrap_or_default(), true)
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => (name.to_string(), false),
            Err(e) => return Err(e.into()),
        };
        let real_dir = self.root.resolve(&dir)?;
        // A name of its own, so nothing already there is touched; found bit
        // 2 (a leftover deleted) is never set.
        let (file, temp) = tree::create_temp(&real_dir, &name)?;
        let found = u8::from(exists);
        self.upload = Some(Upload {
            file,
            temp,
            dest: real_dir.join(&name),
            shown: tree_path(&dir.join(&name)),
            size,
            mtime,
            next: 0,
            crc: Crc32::default(),
        });
        Ok(vec![found])
    }

    fn data(&mut self, p: &[u8]) -> Result<(), Refusal> {
        let mut f = fields(p);
        let offset = f.u32()?;
        let data = f.data;
        let up = self.upload.as_mut().ok_or(Refusal::invalid())?;
        let end = u64::from(offset) + data.len() as u64;
        if offset != up.next || end > u64::from(up.size) {
            return Err(Refusal::invalid());
        }
        up.file.write_all(data)?;
        up.crc.update(data);
        up.next = end as u32;
        Ok(())
    }

    fn commit(&mut self, p: &[u8], log: &mut dyn FnMut(&str)) -> Result<Vec<u8>, Refusal> {
        let mut f = fields(p);
        let size = f.u32()?;
        let crc = f.u32()?;
        let up = self.upload.take().ok_or(Refusal::invalid())?;
        let ours = up.crc.value();
        if size != up.size || up.next != size || crc != ours {
            let _ = fs::remove_file(&up.temp);
            return Err(Refusal::new(ErrorClass::VerifyFailed));
        }
        let result = (|| {
            up.file.sync_all()?;
            if let Some(time) = tree::system_time(up.mtime) {
                up.file.set_modified(time)?;
            }
            fs::rename(&up.temp, &up.dest)
        })();
        if let Err(e) = result {
            let _ = fs::remove_file(&up.temp);
            return Err(e.into());
        }
        log(&format!("received /{} ({size} bytes)", up.shown));
        Ok(ours.to_le_bytes().to_vec())
    }

    fn storage_list(&self) -> Result<Vec<u8>, Refusal> {
        let devices: Vec<_> = self
            .options
            .disks
            .iter()
            .enumerate()
            .map(|(i, disk)| storage::Descriptor {
                device: i as u8,
                ..disk.describe()
            })
            .take(usize::from(u8::MAX))
            .collect();
        let out = storage::encode_list(&devices);
        if out.len() > self.send_limit {
            return Err(Refusal::new(ErrorClass::InvalidRequest));
        }
        Ok(out)
    }

    fn storage_read(&self, p: &[u8]) -> Result<Vec<u8>, Refusal> {
        let request = ReadRequest::decode(p).map_err(|_| Refusal::invalid())?;
        let disk = self
            .options
            .disks
            .get(usize::from(request.device))
            .ok_or(Refusal::invalid())?;
        let d = disk.describe();
        let count = u32::from(request.count);
        let start = match request.address {
            Address::Linear(unit) if d.linear() => unit,
            Address::Chs {
                cylinder,
                head,
                sector,
            } if d.chs() => {
                let g = d.geometry.ok_or(Refusal::invalid())?;
                let unit = g.linear(cylinder, head, sector).ok_or(Refusal::invalid())?;
                // A CHS read stays within one track.
                if u32::from(sector - g.first_sector) + count > u32::from(g.sectors) {
                    return Err(Refusal::invalid());
                }
                unit
            }
            _ => return Err(Refusal::invalid()),
        };
        let fits = storage::read_response_len(usize::from(request.count), usize::from(d.unit_size))
            <= self.send_limit;
        if !fits || u64::from(start) + u64::from(count) > u64::from(d.unit_count) {
            return Err(Refusal::invalid());
        }
        let units: Vec<_> = (start..start + count)
            .map(|unit| disk.read(unit, request.retries))
            .collect();
        Ok(storage::encode_read_response(&units))
    }

    /// Delete the file being received, if any.
    fn discard_upload(&mut self) {
        if let Some(up) = self.upload.take() {
            let _ = fs::remove_file(&up.temp);
        }
    }
}

impl Drop for Server<'_> {
    fn drop(&mut self) {
        self.discard_upload();
    }
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

fn fields(data: &[u8]) -> Fields<'_> {
    Fields {
        data,
        what: "request",
    }
}

/// A request's `path_len` and `path`.
fn path_field(f: &mut Fields) -> Result<String, Refusal> {
    let len = usize::from(f.u8()?);
    let bytes = f.take(len)?;
    String::from_utf8(bytes.to_vec()).map_err(|_| Refusal::new(ErrorClass::NotFound))
}

/// A path in the root as a tree path: components joined by `/`.
fn tree_path(rel: &Path) -> String {
    rel.iter()
        .map(|c| c.to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::disk::memory::Fault;
    use crate::native::disk::{ImageDisk, MemoryDisk};
    use crate::native::session::{self, ClientInfo, Error, Session};
    use crate::native::storage::Unit;
    use crate::transport::memory_pair;
    use std::thread::{self, JoinHandle};
    use std::time::Duration;

    /// root/{data.bin (3000 bytes), docs/{readme.txt, .secret}, .hidden,
    /// Mixed.TXT, mixed.txt}
    fn tree() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();
        fs::create_dir(base.join("docs")).unwrap();
        fs::write(base.join("docs/readme.txt"), b"hello").unwrap();
        fs::write(base.join("docs/.secret"), b"secret").unwrap();
        fs::write(base.join(".hidden"), b"hidden").unwrap();
        fs::write(base.join("data.bin"), data(3000)).unwrap();
        fs::write(base.join("Mixed.TXT"), b"upper").unwrap();
        fs::write(base.join("mixed.txt"), b"lower").unwrap();
        tmp
    }

    fn data(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i * 31 % 251) as u8).collect()
    }

    /// Start a server on `dir` and connect a session to it, offering
    /// `max_payload`.
    fn connect(dir: &Path, options: Options, max_payload: usize, body: impl FnOnce(&mut Session)) {
        let root = Root::new(dir).unwrap();
        let (mut ours, mut theirs) = memory_pair();
        let server: JoinHandle<()> = thread::spawn(move || {
            let _ = run(&mut theirs, &root, &options, &mut |_| {});
        });
        let options = session::Options {
            max_payload,
            wait: Duration::from_secs(2),
            hello_interval: Duration::from_millis(100),
            quiet: Duration::from_millis(10),
            response_timeout: Some(Duration::from_millis(500)),
            ..session::Options::default()
        };
        {
            let mut session = Session::connect(&mut ours, &options, |_| {}).unwrap();
            body(&mut session);
            session.close().unwrap();
        }
        drop(ours);
        server.join().unwrap();
    }

    fn class(e: Error) -> ErrorClass {
        match e {
            Error::Client(e) => e.class,
            other => panic!("not a client error: {other}"),
        }
    }

    fn writable() -> Options {
        Options {
            writable: true,
            ..Options::default()
        }
    }

    #[test]
    fn hello_reports_a_read_only_tree() {
        let tmp = tree();
        connect(tmp.path(), Options::default(), 1024, |s| {
            let info: &ClientInfo = s.client();
            assert_eq!(info.minor, 1);
            assert_eq!(info.platform, Platform::Linux);
            assert_eq!(info.capabilities.to_string(), "FILE_TREE");
            assert!(s.has_tree());
            assert_eq!(s.writable(), (false, false));
            assert_eq!(s.minor(), 1);
            assert_eq!(s.max_payload(), 1024);
        });
        connect(tmp.path(), writable(), 1024, |s| {
            assert_eq!(s.writable(), (true, true));
        });
    }

    #[test]
    fn stat_finds_paths_ignoring_case() {
        let tmp = tree();
        connect(tmp.path(), Options::default(), 1024, |s| {
            let e = s.stat("/DOCS/README.TXT").unwrap();
            assert_eq!(
                (e.dir, e.size, e.name.as_str()),
                (false, 5, "docs/readme.txt")
            );
            assert_ne!(e.mtime, 0);
            let e = s.stat("docs").unwrap();
            assert!(e.dir);
            assert_eq!(s.stat("").unwrap().name, "");
            assert_eq!(s.stat("/").unwrap().name, "");
            assert_eq!(class(s.stat("nope").unwrap_err()), ErrorClass::NotFound);
            assert_eq!(class(s.stat(".hidden").unwrap_err()), ErrorClass::NotFound);
            assert_eq!(
                class(s.stat("docs/.secret").unwrap_err()),
                ErrorClass::NotFound
            );
            assert_eq!(class(s.stat("../etc").unwrap_err()), ErrorClass::NotFound);
            assert_eq!(
                class(s.stat("data.bin/x").unwrap_err()),
                ErrorClass::NotADirectory
            );
            assert_eq!(
                class(s.stat("MIXED.txt").unwrap_err()),
                ErrorClass::Ambiguous
            );
            assert_eq!(s.stat("mixed.txt").unwrap().size, 5);
        });
    }

    #[test]
    fn listings_are_paged() {
        let tmp = tempfile::tempdir().unwrap();
        for i in 0..40 {
            fs::write(tmp.path().join(format!("file{i:02}.dat")), data(i)).unwrap();
        }
        fs::create_dir(tmp.path().join("sub")).unwrap();
        fs::write(tmp.path().join(".hidden"), b"x").unwrap();
        // 128-byte payloads hold about six entries per page.
        connect(tmp.path(), Options::default(), 128, |s| {
            let entries = s.list("/").unwrap();
            assert_eq!(entries.len(), 41);
            assert_eq!(entries[0].name, "file00.dat");
            assert_eq!(entries[39].size, 39);
            assert!(entries[40].dir);
            assert_eq!(
                class(s.list("file01.dat").unwrap_err()),
                ErrorClass::NotADirectory
            );
            assert_eq!(s.list("sub").unwrap(), []);
        });
    }

    #[test]
    fn large_listings_are_complete_and_in_order() {
        let tmp = tempfile::tempdir().unwrap();
        for i in 0..500 {
            fs::write(tmp.path().join(format!("f{i:03}.dat")), b"x").unwrap();
        }
        // About six entries per page: over 80 pages.
        connect(tmp.path(), Options::default(), 128, |s| {
            let names: Vec<String> = s.list("/").unwrap().into_iter().map(|e| e.name).collect();
            let expected: Vec<String> = (0..500).map(|i| format!("f{i:03}.dat")).collect();
            assert_eq!(names, expected);
        });
    }

    #[test]
    fn files_are_read_in_parts() {
        let tmp = tree();
        connect(tmp.path(), Options::default(), 1024, |s| {
            assert_eq!(s.read_chunk(), 1016);
            let mut got = Vec::new();
            loop {
                let chunk = s.read("DATA.BIN", got.len() as u32, 1016).unwrap();
                assert_eq!(chunk.size, 3000);
                if chunk.data.is_empty() {
                    break;
                }
                got.extend_from_slice(&chunk.data);
            }
            assert_eq!(got, data(3000));
            // Asking for more than fits gets what fits.
            assert_eq!(s.read("data.bin", 0, 60000).unwrap().data.len(), 1016);
            assert_eq!(
                class(s.read("docs", 0, 10).unwrap_err()),
                ErrorClass::IsADirectory
            );
        });
    }

    #[test]
    fn a_read_only_server_refuses_writes() {
        let tmp = tree();
        connect(tmp.path(), Options::default(), 1024, |s| {
            assert_eq!(
                class(s.begin("new.txt", 3, false, 0).unwrap_err()),
                ErrorClass::ReadOnly
            );
        });
        assert!(!tmp.path().join("new.txt").exists());
    }

    #[test]
    fn files_are_written_verified_and_dated() {
        let tmp = tree();
        let content = data(2500);
        // 2026-10-23 14:30:58 local time.
        let mtime = (46 << 25) | (10 << 21) | (23 << 16) | (14 << 11) | (30 << 5) | 29;
        connect(tmp.path(), writable(), 1024, |s| {
            assert_eq!(s.begin("docs/NEW.BIN", 2500, false, mtime).unwrap(), 0);
            // The temporary file is hidden from listings.
            assert_eq!(s.list("docs").unwrap().len(), 1);
            for (i, part) in content.chunks(s.write_chunk()).enumerate() {
                s.data((i * s.write_chunk()) as u32, part).unwrap();
            }
            let crc = Crc32::of(&content);
            assert_eq!(s.commit(2500, crc).unwrap(), crc);
        });
        let path = tmp.path().join("docs/NEW.BIN");
        assert_eq!(fs::read(&path).unwrap(), content);
        let modified = fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(modified, tree::system_time(mtime).unwrap());
        let names: Vec<_> = fs::read_dir(tmp.path().join("docs")).unwrap().collect();
        assert_eq!(names.len(), 3, "no temporary file left");
    }

    #[test]
    fn existing_files_need_the_replace_flag() {
        let tmp = tree();
        connect(tmp.path(), writable(), 1024, |s| {
            assert_eq!(
                class(s.begin("DOCS/README.TXT", 3, false, 0).unwrap_err()),
                ErrorClass::Exists
            );
            assert_eq!(
                class(s.begin("docs", 3, true, 0).unwrap_err()),
                ErrorClass::IsADirectory
            );
            assert_eq!(
                class(s.begin("nope/x.txt", 3, false, 0).unwrap_err()),
                ErrorClass::NotFound
            );
            assert_eq!(
                class(s.begin("MIXED.TXT", 3, true, 0).unwrap_err()),
                ErrorClass::Ambiguous
            );
            // Replacing keeps the name as it is on disk.
            assert_eq!(s.begin("DOCS/README.TXT", 3, true, 0).unwrap(), 1);
            s.data(0, b"new").unwrap();
            s.commit(3, Crc32::of(b"new")).unwrap();
        });
        let names: Vec<_> = fs::read_dir(tmp.path().join("docs"))
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(names.len(), 2, "{names:?}");
        assert_eq!(
            fs::read(tmp.path().join("docs/readme.txt")).unwrap(),
            b"new"
        );
    }

    #[test]
    fn a_bad_checksum_or_abort_leaves_nothing() {
        let tmp = tree();
        connect(tmp.path(), writable(), 1024, |s| {
            s.begin("a.txt", 3, false, 0).unwrap();
            s.data(0, b"abc").unwrap();
            assert_eq!(
                class(s.commit(3, 1234).unwrap_err()),
                ErrorClass::VerifyFailed
            );
            s.begin("b.txt", 3, false, 0).unwrap();
            s.data(0, b"ab").unwrap();
            assert_eq!(
                class(s.data(0, b"c").unwrap_err()),
                ErrorClass::InvalidRequest,
                "wrong offset"
            );
            s.abort().unwrap();
            s.begin("c.txt", 2, false, 0).unwrap();
            assert_eq!(
                class(s.data(0, b"abc").unwrap_err()),
                ErrorClass::InvalidRequest,
                "past the size"
            );
        });
        let mut names: Vec<_> = fs::read_dir(tmp.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        assert_eq!(
            names,
            [".hidden", "Mixed.TXT", "data.bin", "docs", "mixed.txt"]
        );
    }

    #[test]
    fn a_retransmitted_write_is_not_repeated() {
        let tmp = tree();
        let root = Root::new(tmp.path()).unwrap();
        let options = writable();
        let mut server = Server::new(&root, &options);
        let frame = |kind, seq, payload: Vec<u8>| Frame {
            kind,
            session: 0x1234,
            seq,
            payload,
        };
        let mut hello = MAGIC.to_vec();
        hello.extend_from_slice(&[1, 1, 0, 4]);
        let mut begin = 3u32.to_le_bytes().to_vec();
        begin.push(0);
        tree::put_path(&mut begin, "x.txt");
        let mut data = 0u32.to_le_bytes().to_vec();
        data.extend_from_slice(b"abc");
        let mut commit = 3u32.to_le_bytes().to_vec();
        commit.extend_from_slice(&Crc32::of(b"abc").to_le_bytes());
        // Files that look like temporary ones are not this transfer's.
        let lookalike = tmp.path().join(".x.txt.sprocket");
        fs::write(&lookalike, b"keep").unwrap();
        let log = &mut |_: &str| {};
        server.handle(&frame(HELLO, 0, hello), log).unwrap();
        server
            .handle(&frame(tree::FILE_BEGIN, 1, begin), log)
            .unwrap();
        let first = server
            .handle(&frame(tree::FILE_DATA, 2, data.clone()), log)
            .unwrap();
        let again = server
            .handle(&frame(tree::FILE_DATA, 2, data), log)
            .unwrap();
        assert_eq!(first, again);
        let reply = server
            .handle(&frame(tree::FILE_COMMIT, 3, commit), log)
            .unwrap();
        assert_eq!(reply[1], tree::FILE_COMMIT | RESPONSE, "{reply:02x?}");
        assert_eq!(fs::read(tmp.path().join("x.txt")).unwrap(), b"abc");
        assert_eq!(fs::read(&lookalike).unwrap(), b"keep");
        let temps = fs::read_dir(tmp.path())
            .unwrap()
            .filter(|e| {
                let name = e.as_ref().unwrap().file_name();
                name.to_string_lossy().ends_with(".sprocket")
            })
            .count();
        assert_eq!(temps, 1, "only the lookalike is left");
    }

    fn with_disks(disks: Vec<Arc<dyn Disk>>) -> Options {
        Options {
            disks,
            ..Options::default()
        }
    }

    fn geometry(cylinders: u16, heads: u8, sectors: u8) -> storage::Geometry {
        storage::Geometry {
            cylinders,
            heads,
            sectors,
            first_sector: 1,
        }
    }

    fn read(device: u8, address: Address, count: u8) -> ReadRequest {
        ReadRequest {
            device,
            address,
            count,
            retries: 0,
        }
    }

    #[test]
    fn disks_are_listed_and_read() {
        let tmp = tree();
        let memory = Arc::new(MemoryDisk::new(100, 512, Some(geometry(10, 2, 5))));
        let floppy = tmp.path().join("floppy.img");
        fs::write(&floppy, vec![0xE5; 368_640]).unwrap();
        let image = Arc::new(ImageDisk::open(&floppy, None).unwrap());
        let options = with_disks(vec![memory.clone(), image]);
        connect(tmp.path(), options, 4090, |s| {
            assert!(s.has_storage());
            assert_eq!(
                s.client().capabilities.to_string(),
                "STORAGE_LIST, STORAGE_READ_UNITS, STORAGE_READ_CHS, FILE_TREE"
            );
            let list = s.storage_list().unwrap();
            assert_eq!(list.len(), 2);
            assert_eq!((list[0].device, list[0].unit_count), (0, 100));
            assert_eq!(
                (list[1].device, list[1].label.as_str()),
                (1, "floppy.img 360K")
            );

            let units = s
                .storage_read(&read(0, Address::Linear(7), 7), 512)
                .unwrap();
            let expected: Vec<Unit> = (7..14)
                .map(|u| Unit::Read {
                    code: 0,
                    data: memory.unit(u),
                })
                .collect();
            assert_eq!(units, expected);

            // Cylinder 1, head 1, sector 3 is unit (1 * 2 + 1) * 5 + 2.
            let chs = Address::Chs {
                cylinder: 1,
                head: 1,
                sector: 3,
            };
            let units = s.storage_read(&read(0, chs, 3), 512).unwrap();
            let at_17: Vec<Unit> = (17..20)
                .map(|u| Unit::Read {
                    code: 0,
                    data: memory.unit(u),
                })
                .collect();
            assert_eq!(units, at_17);
            let units = s
                .storage_read(&read(1, Address::Linear(719), 1), 512)
                .unwrap();
            assert_eq!(
                units,
                [Unit::Read {
                    code: 0,
                    data: vec![0xE5; 512]
                }]
            );
        });
    }

    #[test]
    fn unit_failures_are_reported_per_unit() {
        let tmp = tree();
        let memory = Arc::new(MemoryDisk::new(20, 512, None));
        memory.fault(
            3,
            Fault::Fail {
                code: 0x10,
                times: 5,
            },
        );
        memory.fault(4, Fault::Suspect(0x11));
        connect(tmp.path(), with_disks(vec![memory.clone()]), 4090, |s| {
            let request = ReadRequest {
                retries: 2,
                ..read(0, Address::Linear(2), 3)
            };
            let units = s.storage_read(&request, 512).unwrap();
            assert_eq!(
                units[0],
                Unit::Read {
                    code: 0,
                    data: memory.unit(2)
                }
            );
            assert_eq!(
                units[1],
                Unit::Failed {
                    class: ErrorClass::MediaError,
                    code: 0x10
                }
            );
            assert_eq!(
                units[2],
                Unit::Read {
                    code: 0x11,
                    data: memory.unit(4)
                }
            );
        });
        // One attempt for unit 2, three for unit 3, one for unit 4.
        assert_eq!(memory.reads(), [2, 3, 3, 3, 4]);
    }

    #[test]
    fn bad_storage_reads_are_refused() {
        let tmp = tree();
        let memory = Arc::new(MemoryDisk::new(100, 512, Some(geometry(10, 2, 5))));
        let linear_only = Arc::new(MemoryDisk::new(10, 512, None));
        connect(
            tmp.path(),
            with_disks(vec![memory, linear_only]),
            1024,
            |s| {
                let refused = |s: &mut Session, request: ReadRequest| {
                    class(s.storage_read(&request, 512).unwrap_err())
                };
                // Two units do not fit a 1024-byte payload.
                assert_eq!(
                    refused(s, read(0, Address::Linear(0), 2)),
                    ErrorClass::InvalidRequest
                );
                assert_eq!(
                    refused(s, read(0, Address::Linear(100), 1)),
                    ErrorClass::InvalidRequest
                );
                assert_eq!(
                    refused(s, read(2, Address::Linear(0), 1)),
                    ErrorClass::InvalidRequest
                );
                let chs = |sector| Address::Chs {
                    cylinder: 0,
                    head: 0,
                    sector,
                };
                assert_eq!(refused(s, read(0, chs(0), 1)), ErrorClass::InvalidRequest);
                assert_eq!(refused(s, read(1, chs(1), 1)), ErrorClass::InvalidRequest);
                assert!(s.storage_read(&read(0, chs(5), 1), 512).is_ok());
            },
        );
        connect(
            tmp.path(),
            with_disks(vec![Arc::new(MemoryDisk::new(
                100,
                512,
                Some(geometry(10, 2, 5)),
            ))]),
            4090,
            |s| {
                let chs = Address::Chs {
                    cylinder: 0,
                    head: 0,
                    sector: 4,
                };
                // Sectors 4 and 5 fit the track; 4 to 6 do not.
                assert!(s.storage_read(&read(0, chs, 2), 512).is_ok());
                assert_eq!(
                    class(s.storage_read(&read(0, chs, 3), 512).unwrap_err()),
                    ErrorClass::InvalidRequest
                );
            },
        );
    }

    #[test]
    fn no_disks_no_storage() {
        let tmp = tree();
        connect(tmp.path(), Options::default(), 1024, |s| {
            assert!(!s.has_storage());
            assert_eq!(
                class(s.storage_list().unwrap_err()),
                ErrorClass::Unsupported
            );
        });
    }
}
