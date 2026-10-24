// SPDX-License-Identifier: MIT
//! The reread log written next to an image (`NAME.img.reads`): every good
//! read of a unit whose rereads returned different data, so that none of
//! the versions is lost (`docs/imaging.md`, marginal media). The image
//! holds one chosen version ([`choose`]); the map marks the unit as
//! conflicting.
//!
//! The log is JSON lines, one read per line, appended and synced before
//! the map records the conflict. A line torn by a crash can only be the
//! last; it is ignored, and cut off before the next append.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{self, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// One good read of a unit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Read {
    pub unit: u32,
    pub sha256: String,
    /// When it was read, if known: not for a read made before the
    /// conflict showed, which the map records only as an attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<String>,
    /// Which attempt at the unit this was, counting from 1.
    pub attempt: u32,
    /// The `retries` value of the request.
    pub retries: u8,
    /// The client's status for the read; 0 for none.
    pub code: u16,
    #[serde(with = "hex")]
    pub data: Vec<u8>,
}

/// The log, opened for appending when first written.
#[derive(Debug)]
pub struct Log {
    path: PathBuf,
    file: Option<File>,
    /// Bytes of whole lines: where the next append goes.
    len: u64,
    /// Every read in the log, by unit, in order.
    reads: HashMap<u32, Vec<Read>>,
}

impl Log {
    /// The log at `path`, with the reads already in it, or an empty one if
    /// there is no file.
    pub fn open(path: &Path) -> io::Result<Self> {
        let mut reads: HashMap<u32, Vec<Read>> = HashMap::new();
        let mut len = 0;
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let lines: Vec<&str> = text.lines().collect();
                len = text.len() as u64;
                for (i, line) in lines.iter().enumerate() {
                    match serde_json::from_str::<Read>(line) {
                        Ok(read) => reads.entry(read.unit).or_default().push(read),
                        // Torn by a crash while appending.
                        Err(_) if i + 1 == lines.len() && !text.ends_with('\n') => {
                            len -= line.len() as u64;
                        }
                        Err(e) => {
                            return Err(io::Error::new(
                                io::ErrorKind::InvalidData,
                                format!("{} line {}: {e}", path.display(), i + 1),
                            ));
                        }
                    }
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        Ok(Self {
            path: path.to_path_buf(),
            file: None,
            len,
            reads,
        })
    }

    /// The reads of `unit` logged so far.
    pub fn reads(&self, unit: u32) -> &[Read] {
        self.reads.get(&unit).map_or(&[], Vec::as_slice)
    }

    /// Append a read and sync it.
    pub fn append(&mut self, read: Read) -> io::Result<()> {
        if self.file.is_none() {
            let mut file = OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(&self.path)?;
            file.set_len(self.len)?;
            file.seek(SeekFrom::Start(self.len))?;
            self.file = Some(file);
        }
        let file = self.file.as_mut().expect("opened above");
        let mut line = serde_json::to_vec(&read).map_err(io::Error::other)?;
        line.push(b'\n');
        file.write_all(&line)?;
        file.sync_data()?;
        self.reads.entry(read.unit).or_default().push(read);
        Ok(())
    }
}

/// The version of a unit the image should hold: the first read, or, once
/// there are three or more, the data read most often, if one is read more
/// often than any other.
pub fn choose(reads: &[Read]) -> Option<&Read> {
    let first = reads.first()?;
    if reads.len() < 3 {
        return Some(first);
    }
    let mut tally: Vec<(&str, usize, &Read)> = Vec::new();
    for read in reads {
        match tally.iter_mut().find(|t| t.0 == read.sha256) {
            Some(t) => t.1 += 1,
            None => tally.push((&read.sha256, 1, read)),
        }
    }
    tally.sort_by_key(|t| std::cmp::Reverse(t.1));
    if tally.len() > 1 && tally[0].1 == tally[1].1 {
        return Some(first);
    }
    Some(tally[0].2)
}

mod hex {
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub fn serialize<S: Serializer>(data: &[u8], s: S) -> Result<S::Ok, S::Error> {
        let text: String = data.iter().map(|b| format!("{b:02x}")).collect();
        s.serialize_str(&text)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let text = String::deserialize(d)?;
        if text.len() % 2 != 0 {
            return Err(D::Error::custom("odd number of hex digits"));
        }
        (0..text.len())
            .step_by(2)
            .map(|i| {
                text.get(i..i + 2)
                    .and_then(|pair| u8::from_str_radix(pair, 16).ok())
                    .ok_or_else(|| D::Error::custom("bad hex digit"))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::imaging::manifest::sha256;

    fn read(unit: u32, attempt: u32, data: &[u8]) -> Read {
        Read {
            unit,
            sha256: sha256(data),
            time: None,
            attempt,
            retries: 3,
            code: 0,
            data: data.to_vec(),
        }
    }

    #[test]
    fn reads_survive_a_reopen_and_a_torn_line() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("disk.img.reads");
        let mut log = Log::open(&path).unwrap();
        assert!(log.reads(5).is_empty());
        log.append(read(5, 1, &[1, 2, 0xab])).unwrap();
        log.append(read(9, 2, &[3])).unwrap();
        log.append(read(5, 3, &[4])).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains(r#""data":"0102ab""#), "{text}");

        // A crash in the middle of a fourth line.
        std::fs::write(&path, format!("{text}{{\"unit\":5,\"sha")).unwrap();
        let mut log = Log::open(&path).unwrap();
        assert_eq!(log.reads(5), [read(5, 1, &[1, 2, 0xab]), read(5, 3, &[4])]);
        log.append(read(9, 4, &[5])).unwrap();
        let log = Log::open(&path).unwrap();
        assert_eq!(log.reads(9).len(), 2);

        // Damage anywhere else is an error, not silently dropped reads.
        std::fs::write(&path, format!("garbage\n{text}")).unwrap();
        assert!(Log::open(&path).is_err());
    }

    #[test]
    fn the_first_read_until_a_majority() {
        let (a, b, c) = (read(0, 1, b"a"), read(0, 2, b"b"), read(0, 3, b"c"));
        assert_eq!(choose(&[]), None);
        assert_eq!(choose(&[a.clone(), b.clone()]), Some(&a));
        // Three different: no majority.
        assert_eq!(choose(&[a.clone(), b.clone(), c.clone()]), Some(&a));
        let b2 = read(0, 4, b"b");
        assert_eq!(choose(&[a.clone(), b.clone(), b2.clone()]), Some(&b));
        // A tie keeps the first.
        let a2 = read(0, 5, b"a");
        assert_eq!(choose(&[a.clone(), b.clone(), b2, a2]), Some(&a));
    }
}
