// SPDX-License-Identifier: MIT
//! The manifest written next to an image (`NAME.img.json`): what the image
//! is of, how complete it is, which units failed or are uncertain, and the
//! image's SHA-256. It is regenerated from the progress map at the end of
//! every run, so it always describes the image as it stands.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use sha2::{Digest, Sha256};

use super::map::{Map, Mode, Print, Source, State};

const FORMAT: &str = "sprocket-image";
const VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize)]
pub struct Manifest {
    pub format: &'static str,
    pub version: u32,
    /// The image's file name, relative to the manifest.
    pub image: String,
    pub image_bytes: u64,
    pub sha256: String,
    /// `complete` when every unit is captured (or deliberately skipped),
    /// `incomplete` otherwise.
    pub status: &'static str,
    pub mode: Mode,
    /// What the image holds where a unit was not captured.
    pub fill: &'static str,
    pub source: Source,
    pub host_version: &'static str,
    pub started: String,
    pub finished: String,
    pub counts: Counts,
    pub failed: Vec<Span>,
    pub suspect: Vec<Span>,
    pub conflicting: Vec<Span>,
    /// The reread log holding every version of the conflicting units,
    /// relative to the manifest.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reads: Option<String>,
    pub missing: Vec<Span>,
    pub fingerprint: Vec<Print>,
    /// Times the operator chose to continue although the source could not
    /// be verified as the same disk.
    pub confirmations: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Counts {
    pub units: u64,
    pub captured: u64,
    pub missing: u64,
    pub failed: u64,
    pub skipped: u64,
    pub suspect: u64,
    pub conflicting: u64,
}

/// Units `start..end` with the same outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Span {
    pub start: u32,
    pub end: u32,
    #[serde(skip_serializing_if = "is_zero")]
    pub attempts: u32,
    #[serde(skip_serializing_if = "is_zero")]
    pub code: u16,
}

fn is_zero<T: Default + PartialEq>(v: &T) -> bool {
    *v == T::default()
}

impl Manifest {
    /// Describe `image` as `map` records it, hashing the image.
    pub fn build(map: &Map, image: &Path, finished: SystemTime) -> io::Result<Self> {
        let c = map.counts();
        let spans = |keep: &dyn Fn(&super::map::Entry) -> bool| -> Vec<Span> {
            map.ranges()
                .iter()
                .filter(|r| keep(&r.entry))
                .map(|r| Span {
                    start: r.start,
                    end: r.end,
                    attempts: r.entry.attempts,
                    code: r.entry.code,
                })
                .collect()
        };
        let name = image
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let reads = format!("{name}.reads");
        Ok(Self {
            format: FORMAT,
            version: VERSION,
            reads: image.with_file_name(&reads).exists().then_some(reads),
            image: name,
            image_bytes: fs::metadata(image)?.len(),
            sha256: sha256_file(image)?,
            status: if map.complete() {
                "complete"
            } else {
                "incomplete"
            },
            mode: map.mode,
            fill: "zero",
            source: map.source.clone(),
            host_version: env!("CARGO_PKG_VERSION"),
            started: iso8601(map.started),
            finished: iso8601(seconds(finished)),
            counts: Counts {
                units: u64::from(map.units()),
                captured: c.captured,
                missing: c.missing,
                failed: c.failed,
                skipped: c.skipped,
                suspect: c.suspect,
                conflicting: c.conflicting,
            },
            failed: spans(&|e| e.state == State::Failed),
            suspect: spans(&|e| e.suspect()),
            conflicting: spans(&|e| e.state == State::Captured && e.conflict),
            missing: spans(&|e| e.state == State::Missing),
            fingerprint: map.fingerprint.clone(),
            confirmations: map.confirmations.clone(),
        })
    }

    /// Write the manifest, replacing any earlier one by rename.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let mut json = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        json.push(b'\n');
        let mut name = path.file_name().unwrap_or_default().to_os_string();
        name.push(".tmp");
        let temp = path.with_file_name(name);
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temp)?;
        file.write_all(&json)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temp, path)
    }
}

/// The SHA-256 of some bytes, in lower-case hex.
pub fn sha256(data: &[u8]) -> String {
    format!("{:x}", Sha256::digest(data))
}

/// The SHA-256 of a file's contents, in lower-case hex.
pub fn sha256_file(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0; 1 << 16];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Seconds since the Unix epoch.
pub fn seconds(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// A time as `YYYY-MM-DDTHH:MM:SSZ`, in UTC.
pub fn iso8601(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::super::map::{Entry, Source};
    use super::*;

    #[test]
    fn times_and_hashes() {
        assert_eq!(iso8601(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso8601(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(iso8601(1_792_713_600 + 3_661), "2026-10-23T01:01:01Z");
        assert_eq!(
            sha256(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn manifests_describe_the_map() {
        let tmp = tempfile::tempdir().unwrap();
        let image = tmp.path().join("disk.img");
        fs::write(&image, vec![0; 512 * 10]).unwrap();
        let source = Source {
            device: 1,
            label: "BIOS 80 fixed".to_string(),
            kind: "fixed".to_string(),
            unit_size: 512,
            unit_count: 10,
            geometry: None,
            bios_drive: Some(0x80),
            client_platform: "DOS on x86".to_string(),
            client_version: "0.3.0".to_string(),
        };
        let mut map = Map::new(source, Mode::Raw);
        map.started = 1_792_713_600;
        let captured = Entry {
            state: State::Captured,
            attempts: 1,
            ..Entry::MISSING
        };
        map.set(0, 8, captured);
        map.update(2, |e| e.code = 0x11);
        map.update(5, |e| {
            e.state = State::Failed;
            e.code = 0x10;
            e.attempts = 4;
        });
        let m = Manifest::build(&map, &image, UNIX_EPOCH).unwrap();
        assert_eq!(m.status, "incomplete");
        assert_eq!(m.image, "disk.img");
        assert_eq!(m.image_bytes, 5120);
        assert_eq!(m.sha256, sha256(&[0; 5120]));
        assert_eq!(m.started, "2026-10-23T00:00:00Z");
        assert_eq!(
            (
                m.counts.captured,
                m.counts.missing,
                m.counts.failed,
                m.counts.suspect
            ),
            (7, 2, 1, 1)
        );
        assert_eq!(
            m.failed,
            [Span {
                start: 5,
                end: 6,
                attempts: 4,
                code: 0x10
            }]
        );
        assert_eq!((m.suspect[0].start, m.suspect[0].code), (2, 0x11));
        assert_eq!((m.missing[0].start, m.missing[0].end), (8, 10));

        let path = tmp.path().join("disk.img.json");
        m.save(&path).unwrap();
        let json: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(json["status"], "incomplete");
        assert_eq!(json["mode"], "raw");
        assert_eq!(json["source"]["bios_drive"], 0x80);
        assert_eq!(json["failed"][0]["code"], 16);
        assert!(json["missing"][0].get("code").is_none());
    }
}
