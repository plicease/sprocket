// SPDX-License-Identifier: MIT
//! The progress map kept next to an image (`docs/imaging.md`, resumable
//! imaging): which units are captured, missing, failed or deliberately
//! skipped, with attempts and platform codes, plus what is needed to check
//! on resume that the source is the same disk.
//!
//! On disk it is JSON followed by a line `crc32 XXXXXXXX` covering the JSON
//! bytes. It is saved by writing a new copy, syncing it, and renaming it
//! over the old one, so a crash leaves one or the other. A map that fails
//! its check does not load: the caller must treat every unit as missing.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::native::tree::Crc32;

/// Format version written in the map.
const VERSION: u32 = 1;

/// What is known about a unit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    /// Not read yet, or the outcome is unknown.
    Missing,
    /// Read, and its bytes are durable in the image.
    Captured,
    /// The client reported a read error.
    Failed,
    /// Deliberately not read, under the imaging mode; the image holds
    /// zeros there.
    Skipped,
}

/// Everything recorded for a unit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub state: State,
    /// Read attempts so far (each a `STORAGE_READ` that answered for it).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub attempts: u32,
    /// The `retries` value of the last attempt.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub retries: u8,
    /// The platform code of the last attempt: the error for a failed unit;
    /// for a captured one, a status reported with good data (suspect).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub code: u16,
    /// Rereads returned different good data.
    #[serde(default, skip_serializing_if = "is_false")]
    pub conflict: bool,
}

impl Entry {
    pub const MISSING: Entry = Entry {
        state: State::Missing,
        attempts: 0,
        retries: 0,
        code: 0,
        conflict: false,
    };

    /// Captured, but with a status reported alongside the data.
    pub fn suspect(&self) -> bool {
        self.state == State::Captured && self.code != 0
    }
}

fn is_zero<T: Default + PartialEq>(v: &T) -> bool {
    *v == T::default()
}

fn is_false(v: &bool) -> bool {
    !*v
}

/// Units `start..end` sharing one entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Range {
    pub start: u32,
    pub end: u32,
    #[serde(flatten)]
    pub entry: Entry,
}

/// The source device, as reported when imaging started.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    pub device: u8,
    pub label: String,
    pub kind: String,
    pub unit_size: u16,
    pub unit_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub geometry: Option<Geometry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bios_drive: Option<u8>,
    pub client_platform: String,
    pub client_version: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Geometry {
    pub cylinders: u16,
    pub heads: u8,
    pub sectors: u8,
    pub first_sector: u8,
}

/// A unit whose hash identifies the source disk on resume.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Print {
    pub unit: u32,
    pub sha256: String,
}

/// How units are chosen. Only `raw` exists; allocated-only imaging
/// (issue #13) would skip units deliberately.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Raw,
}

/// Counts of units by what is known about them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub captured: u64,
    pub missing: u64,
    pub failed: u64,
    pub skipped: u64,
    /// Captured with a status (included in `captured`).
    pub suspect: u64,
    /// Captured with conflicting rereads (included in `captured`).
    pub conflicting: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Map {
    version: u32,
    pub source: Source,
    pub mode: Mode,
    pub fingerprint: Vec<Print>,
    ranges: Vec<Range>,
}

/// Why a map did not load.
#[derive(Debug)]
pub enum LoadError {
    Io(io::Error),
    /// Torn, failing its check, or not a map this version understands.
    Invalid(String),
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoadError::Io(e) => write!(f, "{e}"),
            LoadError::Invalid(why) => write!(f, "progress map is not usable: {why}"),
        }
    }
}

impl std::error::Error for LoadError {}

impl Map {
    /// A map of a source with every unit missing.
    pub fn new(source: Source, mode: Mode) -> Self {
        let units = source.unit_count;
        Self {
            version: VERSION,
            source,
            mode,
            fingerprint: Vec::new(),
            ranges: if units == 0 {
                Vec::new()
            } else {
                vec![Range {
                    start: 0,
                    end: units,
                    entry: Entry::MISSING,
                }]
            },
        }
    }

    pub fn units(&self) -> u32 {
        self.source.unit_count
    }

    pub fn ranges(&self) -> &[Range] {
        &self.ranges
    }

    /// The entry for one unit.
    pub fn get(&self, unit: u32) -> Entry {
        let i = self.ranges.partition_point(|r| r.end <= unit);
        match self.ranges.get(i) {
            Some(r) if r.start <= unit => r.entry,
            _ => Entry::MISSING,
        }
    }

    /// Give units `start..end` one entry.
    pub fn set(&mut self, start: u32, end: u32, entry: Entry) {
        let end = end.min(self.units());
        if start >= end {
            return;
        }
        let mut out = Vec::with_capacity(self.ranges.len() + 2);
        let mut placed = false;
        for r in &self.ranges {
            if r.end <= start || r.start >= end {
                if r.start >= end && !placed {
                    out.push(Range { start, end, entry });
                    placed = true;
                }
                out.push(*r);
                continue;
            }
            if r.start < start {
                out.push(Range { end: start, ..*r });
            }
            if !placed {
                out.push(Range { start, end, entry });
                placed = true;
            }
            if r.end > end {
                out.push(Range { start: end, ..*r });
            }
        }
        if !placed {
            out.push(Range { start, end, entry });
        }
        // Merge neighbours with equal entries.
        let mut merged: Vec<Range> = Vec::with_capacity(out.len());
        for r in out {
            match merged.last_mut() {
                Some(last) if last.end == r.start && last.entry == r.entry => last.end = r.end,
                _ => merged.push(r),
            }
        }
        self.ranges = merged;
    }

    /// Change one unit's entry.
    pub fn update(&mut self, unit: u32, change: impl FnOnce(&mut Entry)) {
        let mut entry = self.get(unit);
        change(&mut entry);
        self.set(unit, unit + 1, entry);
    }

    /// Ranges of units in `state`, as `start..end`.
    pub fn with_state(&self, state: State) -> impl Iterator<Item = (u32, u32)> + '_ {
        self.ranges
            .iter()
            .filter(move |r| r.entry.state == state)
            .map(|r| (r.start, r.end))
    }

    pub fn counts(&self) -> Counts {
        let mut c = Counts::default();
        for r in &self.ranges {
            let n = u64::from(r.end - r.start);
            match r.entry.state {
                State::Captured => c.captured += n,
                State::Missing => c.missing += n,
                State::Failed => c.failed += n,
                State::Skipped => c.skipped += n,
            }
            if r.entry.suspect() {
                c.suspect += n;
            }
            if r.entry.state == State::Captured && r.entry.conflict {
                c.conflicting += n;
            }
        }
        c
    }

    /// Every unit captured or deliberately skipped.
    pub fn complete(&self) -> bool {
        let c = self.counts();
        c.missing == 0 && c.failed == 0
    }

    /// Mark captured units that lie beyond `image_len` bytes as missing,
    /// for an image file shorter than the map claims.
    pub fn trim_to(&mut self, image_len: u64) {
        let unit_size = u64::from(self.source.unit_size.max(1));
        let whole = u32::try_from(image_len / unit_size).unwrap_or(u32::MAX);
        let beyond: Vec<(u32, u32)> = self
            .with_state(State::Captured)
            .filter(|&(_, end)| end > whole)
            .map(|(start, end)| (start.max(whole), end))
            .collect();
        for (start, end) in beyond {
            self.set(start, end, Entry::MISSING);
        }
    }

    /// Save by writing a new copy, syncing it and renaming it into place.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let json = serde_json::to_vec(self).map_err(io::Error::other)?;
        let mut crc = Crc32::default();
        crc.update(&json);
        let temp = temp_path(path);
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temp)?;
        file.write_all(&json)?;
        writeln!(file, "\ncrc32 {:08x}", crc.value())?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temp, path)?;
        sync_dir(path);
        Ok(())
    }

    pub fn load(path: &Path) -> Result<Self, LoadError> {
        let bytes = fs::read(path).map_err(LoadError::Io)?;
        let invalid = |why: &str| LoadError::Invalid(why.to_string());
        let text = std::str::from_utf8(&bytes).map_err(|_| invalid("not text"))?;
        let (json, trailer) = text
            .trim_end_matches('\n')
            .rsplit_once('\n')
            .ok_or(invalid("no check line"))?;
        let expected = trailer
            .strip_prefix("crc32 ")
            .and_then(|hex| u32::from_str_radix(hex, 16).ok())
            .ok_or(invalid("no check line"))?;
        let mut crc = Crc32::default();
        crc.update(json.as_bytes());
        if crc.value() != expected {
            return Err(invalid("check failed"));
        }
        let map: Map = serde_json::from_str(json).map_err(|e| LoadError::Invalid(e.to_string()))?;
        if map.version != VERSION {
            return Err(LoadError::Invalid(format!("version {}", map.version)));
        }
        map.validate().map_err(LoadError::Invalid)?;
        Ok(map)
    }

    /// Ranges in order, without gaps or overlaps, covering every unit.
    fn validate(&self) -> Result<(), String> {
        let mut next = 0;
        for r in &self.ranges {
            if r.start != next || r.end <= r.start {
                return Err(format!("ranges are not contiguous at unit {next}"));
            }
            next = r.end;
        }
        if next != self.units() {
            return Err(format!("ranges end at {next}, not {}", self.units()));
        }
        Ok(())
    }
}

/// `NAME.map.tmp` beside `NAME.map`.
fn temp_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    path.with_file_name(name)
}

/// Make a rename in `path`'s directory durable, where the platform allows.
fn sync_dir(path: &Path) {
    #[cfg(unix)]
    if let Some(dir) = path.parent() {
        let dir = if dir.as_os_str().is_empty() {
            Path::new(".")
        } else {
            dir
        };
        if let Ok(d) = File::open(dir) {
            let _ = d.sync_all();
        }
    }
    #[cfg(not(unix))]
    let _ = path;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(units: u32) -> Source {
        Source {
            device: 0,
            label: "test".to_string(),
            kind: "fixed".to_string(),
            unit_size: 512,
            unit_count: units,
            geometry: None,
            bios_drive: Some(0x80),
            client_platform: "DOS on x86".to_string(),
            client_version: "0.3.0".to_string(),
        }
    }

    fn captured() -> Entry {
        Entry {
            state: State::Captured,
            attempts: 1,
            ..Entry::MISSING
        }
    }

    #[test]
    fn ranges_split_and_merge() {
        let mut map = Map::new(source(100), Mode::Raw);
        assert_eq!(map.ranges().len(), 1);
        map.set(10, 20, captured());
        map.set(20, 30, captured());
        assert_eq!(
            map.with_state(State::Captured).collect::<Vec<_>>(),
            [(10, 30)]
        );
        assert_eq!(
            map.with_state(State::Missing).collect::<Vec<_>>(),
            [(0, 10), (30, 100)]
        );
        map.update(15, |e| {
            e.state = State::Failed;
            e.code = 0x10;
            e.attempts += 1;
        });
        assert_eq!(map.ranges().len(), 5);
        assert_eq!(map.get(15).attempts, 2);
        assert_eq!(map.get(15).state, State::Failed);
        map.set(15, 16, captured());
        assert_eq!(map.ranges().len(), 3);
        map.set(0, 100, Entry::MISSING);
        assert_eq!(map.ranges().len(), 1);
        // Out of range is clipped.
        map.set(90, 200, captured());
        assert_eq!(map.ranges().last().unwrap().end, 100);
        map.set(0, 10, captured());
        assert_eq!(map.get(0).state, State::Captured);
        assert_eq!(map.get(99).state, State::Captured);
        assert_eq!(map.get(50).state, State::Missing);
        assert!(map.validate().is_ok());
    }

    #[test]
    fn counts_and_completeness() {
        let mut map = Map::new(source(10), Mode::Raw);
        assert!(!map.complete());
        map.set(0, 10, captured());
        assert!(map.complete());
        map.update(3, |e| e.code = 0x11);
        map.update(4, |e| e.conflict = true);
        map.update(5, |e| e.state = State::Failed);
        map.update(6, |e| e.state = State::Skipped);
        let c = map.counts();
        assert_eq!(
            (c.captured, c.failed, c.skipped, c.suspect, c.conflicting),
            (8, 1, 1, 1, 1)
        );
        assert!(!map.complete());
    }

    #[test]
    fn save_and_load() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("disk.img.map");
        let mut map = Map::new(source(1000), Mode::Raw);
        map.fingerprint.push(Print {
            unit: 0,
            sha256: "ab".repeat(32),
        });
        map.set(0, 500, captured());
        map.update(7, |e| {
            *e = Entry {
                state: State::Failed,
                attempts: 3,
                retries: 3,
                code: 0x10,
                conflict: false,
            }
        });
        map.save(&path).unwrap();
        assert!(!temp_path(&path).exists());
        assert_eq!(Map::load(&path).unwrap(), map);

        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains(r#""state":"failed","attempts":3,"retries":3,"code":16"#));
        // Torn: cut short.
        fs::write(&path, &text[..text.len() / 2]).unwrap();
        assert!(matches!(Map::load(&path), Err(LoadError::Invalid(_))));
        // Changed without updating the check.
        fs::write(&path, text.replacen("\"captured\"", "\"skipped\"", 1)).unwrap();
        assert!(matches!(Map::load(&path), Err(LoadError::Invalid(_))));
        assert!(matches!(
            Map::load(&tmp.path().join("none")),
            Err(LoadError::Io(_))
        ));
    }

    #[test]
    fn maps_with_gaps_do_not_load() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("m");
        let mut map = Map::new(source(10), Mode::Raw);
        map.ranges[0].end = 9;
        map.save(&path).unwrap();
        assert!(matches!(Map::load(&path), Err(LoadError::Invalid(_))));
    }

    #[test]
    fn short_images_lose_captured_units() {
        let mut map = Map::new(source(100), Mode::Raw);
        map.set(0, 100, captured());
        map.trim_to(512 * 40 + 100);
        assert_eq!(
            map.with_state(State::Missing).collect::<Vec<_>>(),
            [(40, 100)]
        );
        map.trim_to(512 * 100);
        assert_eq!(map.counts().captured, 40);
    }
}
