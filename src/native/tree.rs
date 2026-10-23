// SPDX-License-Identifier: MIT
//! Native protocol v1.1 file trees (`docs/protocol-v1.1.md`): message
//! types, entries, tree paths, error classes, DOS times and CRC-32, shared
//! by the requester (`sprocket client`) and the responder (`sprocket serve
//! --mode native`).

use std::io;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::session::{ErrorClass, Fields};

pub const FILE_BEGIN: u8 = 0x10;
pub const FILE_DATA: u8 = 0x11;
pub const FILE_COMMIT: u8 = 0x12;
pub const FILE_ABORT: u8 = 0x13;
pub const STAT: u8 = 0x18;
pub const DIR_LIST: u8 = 0x19;
pub const FILE_READ: u8 = 0x1A;

/// `FILE_BEGIN` flag: replace an existing file.
pub const REPLACE: u8 = 1 << 0;
/// `DIR_LIST` response flag: more entries follow.
pub const MORE: u8 = 1 << 0;

/// Longest tree path, as for v1 paths.
pub const MAX_PATH: usize = 127;
/// Bytes of a `FILE_READ` response before the data.
pub const READ_HEADER: usize = 8;
/// Bytes of a `FILE_DATA` request before the data.
pub const DATA_HEADER: usize = 4;

/// One file or directory, as `STAT` and `DIR_LIST` describe it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub dir: bool,
    pub size: u32,
    /// DOS date and time (section 4); 0 if unknown.
    pub mtime: u32,
    pub name: String,
}

impl Entry {
    /// Append the length-prefixed encoding to `out`.
    pub fn encode(&self, out: &mut Vec<u8>) {
        let name = self.name.as_bytes();
        out.push((10 + name.len()) as u8);
        out.push(u8::from(self.dir));
        out.extend_from_slice(&self.size.to_le_bytes());
        out.extend_from_slice(&self.mtime.to_le_bytes());
        out.push(name.len() as u8);
        out.extend_from_slice(name);
    }

    /// Bytes [`encode`](Self::encode) writes.
    pub fn encoded_len(&self) -> usize {
        11 + self.name.len()
    }

    /// Decode one entry, skipping any fields a later minor version added.
    pub(crate) fn decode(f: &mut Fields) -> Result<Self, String> {
        let len = usize::from(f.u8().map_err(|e| e.to_string())?);
        let body = f.take(len).map_err(|e| e.to_string())?;
        let mut g = Fields {
            data: body,
            what: "entry",
        };
        let parse = |g: &mut Fields| -> Result<Entry, super::session::Error> {
            let kind = g.u8()?;
            let size = g.u32()?;
            let mtime = g.u32()?;
            let name_len = usize::from(g.u8()?);
            let name = g.take(name_len)?;
            Ok(Entry {
                dir: kind == 1,
                size,
                mtime,
                name: String::from_utf8_lossy(name).into_owned(),
            })
        };
        parse(&mut g).map_err(|e| e.to_string())
    }
}

/// A request's path field: `path_len` and the path.
pub fn put_path(out: &mut Vec<u8>, path: &str) {
    out.push(path.len() as u8);
    out.extend_from_slice(path.as_bytes());
}

/// Split a tree path into its components, refusing what section 3 does.
/// The empty path and `/` are the root.
pub fn components(path: &str) -> Option<Vec<&str>> {
    if path.len() > MAX_PATH || !path.bytes().all(|b| (0x20..=0x7e).contains(&b)) {
        return None;
    }
    let path = path.strip_prefix('/').unwrap_or(path);
    if path.is_empty() {
        return Some(Vec::new());
    }
    let parts: Vec<&str> = path.split('/').collect();
    let bad = |p: &&str| p.is_empty() || p.starts_with('.') || p.contains(['\\', ':']);
    if parts.iter().any(bad) {
        return None;
    }
    Some(parts)
}

/// An `ERROR` class for a filesystem error.
pub fn class_of(e: &io::Error) -> ErrorClass {
    if crate::files::is_ambiguous(e) {
        return ErrorClass::Ambiguous;
    }
    match e.kind() {
        io::ErrorKind::NotFound => ErrorClass::NotFound,
        io::ErrorKind::NotADirectory => ErrorClass::NotADirectory,
        io::ErrorKind::IsADirectory => ErrorClass::IsADirectory,
        io::ErrorKind::AlreadyExists => ErrorClass::Exists,
        io::ErrorKind::ReadOnlyFilesystem => ErrorClass::ReadOnly,
        io::ErrorKind::InvalidFilename => ErrorClass::BadName,
        _ => ErrorClass::FileError,
    }
}

/// The filesystem error a requester shows for an `ERROR` class, with the
/// same words as terminal mode; `None` for classes that are not about a
/// path.
pub fn io_error(class: ErrorClass, message: &str) -> Option<io::Error> {
    let kind = match class {
        ErrorClass::NotFound => io::ErrorKind::NotFound,
        ErrorClass::NotADirectory => io::ErrorKind::NotADirectory,
        ErrorClass::IsADirectory => io::ErrorKind::IsADirectory,
        ErrorClass::Exists => io::ErrorKind::AlreadyExists,
        ErrorClass::ReadOnly => {
            return Some(io::Error::new(
                io::ErrorKind::ReadOnlyFilesystem,
                "the server is read-only",
            ));
        }
        ErrorClass::Ambiguous => return Some(crate::files::ambiguous()),
        ErrorClass::BadName => {
            return Some(io::Error::new(
                io::ErrorKind::InvalidFilename,
                "the server cannot hold this name",
            ));
        }
        ErrorClass::FileError if !message.is_empty() => {
            return Some(io::Error::other(message.to_string()));
        }
        _ => return None,
    };
    Some(kind.into())
}

/// A modification time as a DOS date and time in local time; 0 if it is
/// unknown or outside 1980 to 2107.
pub fn dos_time(time: Option<SystemTime>) -> u32 {
    let Some(secs) = time
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .and_then(|d| libc::time_t::try_from(d.as_secs()).ok())
    else {
        return 0;
    };
    // SAFETY: localtime_r writes only to `tm`, which is a plain struct.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&secs, &mut tm) }.is_null() {
        return 0;
    }
    let year = tm.tm_year + 1900;
    if !(1980..=2107).contains(&year) {
        return 0;
    }
    ((year - 1980) as u32) << 25
        | ((tm.tm_mon + 1) as u32) << 21
        | (tm.tm_mday as u32) << 16
        | (tm.tm_hour as u32) << 11
        | (tm.tm_min as u32) << 5
        | (tm.tm_sec as u32 / 2)
}

/// A DOS date and time in local time as a `SystemTime`; `None` for 0 or a
/// value that is not a real date.
pub fn system_time(dos: u32) -> Option<SystemTime> {
    if dos == 0 {
        return None;
    }
    let month = (dos >> 21) & 0xF;
    let day = (dos >> 16) & 0x1F;
    let hour = (dos >> 11) & 0x1F;
    let minute = (dos >> 5) & 0x3F;
    let second = (dos & 0x1F) * 2;
    if !(1..=12).contains(&month) || day == 0 || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    // SAFETY: mktime reads and normalizes only `tm`, a plain struct.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    tm.tm_year = ((dos >> 25) + 80) as i32;
    tm.tm_mon = month as i32 - 1;
    tm.tm_mday = day as i32;
    tm.tm_hour = hour as i32;
    tm.tm_min = minute as i32;
    tm.tm_sec = second as i32;
    tm.tm_isdst = -1;
    let secs = unsafe { libc::mktime(&mut tm) };
    u64::try_from(secs)
        .ok()
        .map(|s| UNIX_EPOCH + Duration::from_secs(s))
}

/// CRC-32 (IEEE 802.3), as `FILE_COMMIT` uses.
#[derive(Clone, Copy, Debug)]
pub struct Crc32(u32);

impl Default for Crc32 {
    fn default() -> Self {
        Self(0xFFFF_FFFF)
    }
}

impl Crc32 {
    const TABLE: [u32; 256] = {
        let mut table = [0u32; 256];
        let mut i = 0;
        while i < 256 {
            let mut c = i as u32;
            let mut k = 0;
            while k < 8 {
                c = if c & 1 != 0 {
                    0xEDB8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
                k += 1;
            }
            table[i] = c;
            i += 1;
        }
        table
    };

    pub fn update(&mut self, data: &[u8]) {
        for &b in data {
            self.0 = Self::TABLE[((self.0 ^ u32::from(b)) & 0xFF) as usize] ^ (self.0 >> 8);
        }
    }

    pub fn value(self) -> u32 {
        !self.0
    }

    pub fn of(data: &[u8]) -> u32 {
        let mut crc = Self::default();
        crc.update(data);
        crc.value()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_check_value() {
        assert_eq!(Crc32::of(b"123456789"), 0xCBF4_3926);
        let mut crc = Crc32::default();
        crc.update(b"1234");
        crc.update(b"56789");
        assert_eq!(crc.value(), 0xCBF4_3926);
    }

    #[test]
    fn entries_round_trip_and_skip_later_fields() {
        let entry = Entry {
            dir: false,
            size: 3000,
            mtime: 0x5B57_6000,
            name: "DATA.BIN".into(),
        };
        let mut out = Vec::new();
        entry.encode(&mut out);
        assert_eq!(out.len(), entry.encoded_len());
        // A later minor version appends a field to the entry.
        out[0] += 2;
        out.extend_from_slice(&[0xAA, 0xBB, 0x42]);
        let mut f = Fields {
            data: &out,
            what: "test",
        };
        assert_eq!(Entry::decode(&mut f).unwrap(), entry);
        assert_eq!(f.data, [0x42]);
    }

    /// The body of a record in `tests/vectors/frames.txt`.
    fn vector(name: &str) -> Vec<u8> {
        let text = std::fs::read_to_string("tests/vectors/frames.txt").unwrap();
        let record = text
            .split("\n\n")
            .find(|r| r.lines().any(|l| l == format!("name   {name}")))
            .unwrap_or_else(|| panic!("no vector {name}"));
        let body = record
            .lines()
            .find_map(|l| l.strip_prefix("body   "))
            .unwrap();
        body.split(' ')
            .map(|b| u8::from_str_radix(b, 16).unwrap())
            .collect()
    }

    #[test]
    fn entries_match_the_vectors() {
        let mtime = (46 << 25) | (10 << 21) | (23 << 16) | (14 << 11) | (30 << 5) | 29;
        let mut stat = Vec::new();
        Entry {
            dir: false,
            size: 5,
            mtime,
            name: "docs/readme.txt".into(),
        }
        .encode(&mut stat);
        assert_eq!(vector("tree_stat_reply")[4..], stat);
        let mut list = vec![0, 2];
        for entry in [
            Entry {
                dir: true,
                size: 0,
                mtime,
                name: "GAMES".into(),
            },
            Entry {
                dir: false,
                size: 3,
                mtime: 0,
                name: "A.TXT".into(),
            },
        ] {
            entry.encode(&mut list);
        }
        assert_eq!(vector("tree_dir_list_reply")[4..], list);
        let mut request = 0u16.to_le_bytes().to_vec();
        put_path(&mut request, "");
        assert_eq!(vector("tree_dir_list")[4..], request);
    }

    #[test]
    fn tree_paths() {
        assert_eq!(components(""), Some(vec![]));
        assert_eq!(components("/"), Some(vec![]));
        assert_eq!(
            components("/GAMES/DOOM.EXE"),
            Some(vec!["GAMES", "DOOM.EXE"])
        );
        assert_eq!(components("a b/c"), Some(vec!["a b", "c"]));
        for bad in [
            "a//b",
            "a/",
            "./a",
            "a/../b",
            ".hidden",
            "a\\b",
            "C:/x",
            "caf\u{e9}",
        ] {
            assert_eq!(components(bad), None, "{bad}");
        }
        assert_eq!(components(&"a".repeat(128)), None);
    }

    #[test]
    fn dos_times_round_trip_in_local_time() {
        let t = system_time(dos_time(Some(SystemTime::now()))).unwrap();
        let now = SystemTime::now();
        assert!(now.duration_since(t).unwrap() < Duration::from_secs(3));
        // 2026-10-23 14:30:58
        let dos = (46 << 25) | (10 << 21) | (23 << 16) | (14 << 11) | (30 << 5) | 29;
        assert_eq!(dos_time(system_time(dos)), dos);
        assert_eq!(dos_time(None), 0);
        assert_eq!(dos_time(Some(UNIX_EPOCH)), 0, "1970 is before DOS dates");
        assert_eq!(system_time(0), None);
        assert_eq!(system_time(13 << 21 | 1 << 16), None, "month 13");
    }
}
