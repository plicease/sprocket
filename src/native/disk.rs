// SPDX-License-Identifier: MIT
//! Devices a native responder can offer for imaging: raw image files, and
//! (in tests) an in-memory disk that can fail, mark units suspect, or
//! return different data on each read.

use std::fmt;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use super::session::ErrorClass;
use super::storage::{CHS, Descriptor, DiskKind, Geometry, LINEAR, Unit};

/// A device a responder can read, one unit at a time.
pub trait Disk: Send + Sync + fmt::Debug {
    /// The device's descriptor; the responder sets `device`.
    fn describe(&self) -> Descriptor;

    /// Read linear unit `unit`, which the responder has checked is in
    /// range, trying up to `retries` more times after a failure.
    fn read(&self, unit: u32, retries: u8) -> Unit;
}

/// BIOS status for a sector that could not be found, used for a read past
/// the end of an image file.
const SECTOR_NOT_FOUND: u16 = 0x04;

/// Standard PC floppy sizes and their geometry.
const FLOPPIES: [(u64, u16, u8, u8, &str); 8] = [
    (163_840, 40, 1, 8, "160K"),
    (184_320, 40, 1, 9, "180K"),
    (327_680, 40, 2, 8, "320K"),
    (368_640, 40, 2, 9, "360K"),
    (737_280, 80, 2, 9, "720K"),
    (1_228_800, 80, 2, 15, "1.2M"),
    (1_474_560, 80, 2, 18, "1.44M"),
    (2_949_120, 80, 2, 36, "2.88M"),
];

/// A raw image file of 512-byte units. An image the size of a standard
/// floppy is offered as a removable disk with its geometry; anything else
/// as a fixed disk with linear addressing only, unless a geometry is
/// given.
#[derive(Debug)]
pub struct ImageDisk {
    file: Mutex<File>,
    path: PathBuf,
    units: u32,
    geometry: Option<Geometry>,
    kind: DiskKind,
    label: String,
}

impl ImageDisk {
    pub fn open(path: &Path, geometry: Option<Geometry>) -> io::Result<Self> {
        let file = File::open(path)?;
        let len = file.metadata()?.len();
        let units = u32::try_from(len / 512).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "image is too large to offer")
        })?;
        let floppy = FLOPPIES.iter().find(|f| f.0 == len);
        let (geometry, kind) = match (geometry, floppy) {
            (Some(g), _) => (Some(g), DiskKind::Fixed),
            (None, Some(&(_, cylinders, heads, sectors, _))) => (
                Some(Geometry {
                    cylinders,
                    heads,
                    sectors,
                    first_sector: 1,
                }),
                DiskKind::Removable,
            ),
            (None, None) => (None, DiskKind::Fixed),
        };
        let name = path.file_name().map_or_else(
            || path.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        let size = floppy.map_or_else(
            || format!("{:.1}M", len as f64 / 1_048_576.0),
            |f| f.4.to_string(),
        );
        let mut label = format!("{name} {size}");
        label.truncate(super::storage::MAX_LABEL);
        Ok(Self {
            file: Mutex::new(file),
            path: path.to_path_buf(),
            units,
            geometry,
            kind,
            label,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Disk for ImageDisk {
    fn describe(&self) -> Descriptor {
        Descriptor {
            device: 0,
            kind: self.kind,
            addressing: LINEAR | if self.geometry.is_some() { CHS } else { 0 },
            unit_size: 512,
            unit_count: self.units,
            geometry: self.geometry,
            meta: Vec::new(),
            label: self.label.clone(),
        }
    }

    fn read(&self, unit: u32, _retries: u8) -> Unit {
        let mut data = vec![0; 512];
        let result = (|| {
            let mut file = self.file.lock().unwrap_or_else(|e| e.into_inner());
            file.seek(SeekFrom::Start(u64::from(unit) * 512))?;
            file.read_exact(&mut data)
        })();
        match result {
            Ok(()) => Unit::Read { code: 0, data },
            Err(_) => Unit::Failed {
                class: ErrorClass::MediaError,
                code: SECTOR_NOT_FOUND,
            },
        }
    }
}

#[cfg(test)]
pub(crate) use memory::MemoryDisk;

#[cfg(test)]
pub(crate) mod memory {
    use std::collections::HashMap;
    use std::sync::Mutex;
    use std::thread;
    use std::time::Duration;

    use super::*;

    /// How a unit misbehaves.
    #[derive(Clone, Copy, Debug)]
    pub(crate) enum Fault {
        /// Fails with this BIOS status this many times, then reads.
        Fail { code: u16, times: u32 },
        /// Reads, with this status reported alongside (suspect).
        Suspect(u16),
        /// Reads, with different data each time.
        Flaky,
        /// Takes this long to read once, as a BIOS call that blocks, then
        /// reads normally.
        Slow(Duration),
        /// Reads as if the unit held this stamp: the disk changed.
        Data(u32),
    }

    /// A disk in memory whose unit `n` holds `n` stamped into every 4
    /// bytes, so that misplaced units show.
    #[derive(Debug)]
    pub(crate) struct MemoryDisk {
        pub units: u32,
        pub unit_size: u16,
        pub geometry: Option<Geometry>,
        /// Offers linear addressing; without it, only CHS.
        pub linear: bool,
        /// Added to each unit's stamp, to make a different disk.
        pub seed: u32,
        /// The drive's geometry, reported in `meta` as a DOS client does.
        pub drive: Option<Geometry>,
        faults: Mutex<HashMap<u32, Fault>>,
        /// Every unit read, in order, including failed attempts.
        pub reads: Mutex<Vec<u32>>,
        flaky_reads: Mutex<u32>,
    }

    impl MemoryDisk {
        pub fn new(units: u32, unit_size: u16, geometry: Option<Geometry>) -> Self {
            Self {
                units,
                unit_size,
                geometry,
                linear: true,
                seed: 0,
                drive: None,
                faults: Mutex::new(HashMap::new()),
                reads: Mutex::new(Vec::new()),
                flaky_reads: Mutex::new(0),
            }
        }

        pub fn with_seed(mut self, seed: u32) -> Self {
            self.seed = seed;
            self
        }

        pub fn with_drive(mut self, drive: Geometry) -> Self {
            self.drive = Some(drive);
            self
        }

        pub fn chs_only(mut self) -> Self {
            self.linear = false;
            self
        }

        pub fn fault(&self, unit: u32, fault: Fault) {
            self.faults.lock().unwrap().insert(unit, fault);
        }

        /// The unit reads cleanly from now on.
        pub fn clear(&self, unit: u32) {
            self.faults.lock().unwrap().remove(&unit);
        }

        /// The contents of unit `unit` when it reads cleanly.
        pub fn unit(&self, unit: u32) -> Vec<u8> {
            stamp(unit.wrapping_add(self.seed), usize::from(self.unit_size))
        }

        /// The whole disk as it reads cleanly.
        pub fn image(&self) -> Vec<u8> {
            (0..self.units).flat_map(|u| self.unit(u)).collect()
        }

        pub fn reads(&self) -> Vec<u32> {
            self.reads.lock().unwrap().clone()
        }
    }

    fn stamp(value: u32, len: usize) -> Vec<u8> {
        value
            .to_le_bytes()
            .iter()
            .copied()
            .cycle()
            .take(len)
            .collect()
    }

    impl Disk for MemoryDisk {
        fn describe(&self) -> Descriptor {
            Descriptor {
                device: 0,
                kind: DiskKind::Fixed,
                addressing: if self.linear { LINEAR } else { 0 }
                    | if self.geometry.is_some() { CHS } else { 0 },
                unit_size: self.unit_size,
                unit_count: self.units,
                geometry: self.geometry,
                meta: {
                    use super::super::storage::{META_BIOS_DRIVE, META_DRIVE_GEOMETRY};
                    let mut meta = vec![(META_BIOS_DRIVE, vec![0x80])];
                    if let Some(g) = self.drive {
                        let c = g.cylinders.to_le_bytes();
                        meta.push((META_DRIVE_GEOMETRY, vec![c[0], c[1], g.heads, g.sectors]));
                    }
                    meta
                },
                label: "memory".to_string(),
            }
        }

        fn read(&self, unit: u32, retries: u8) -> Unit {
            let mut code = 0;
            for _ in 0..=retries {
                match self.attempt(unit) {
                    Ok(read) => return read,
                    Err(status) => code = status,
                }
            }
            Unit::Failed {
                class: ErrorClass::MediaError,
                code,
            }
        }
    }

    impl MemoryDisk {
        /// One attempt at a unit: what it read, or the BIOS status.
        fn attempt(&self, unit: u32) -> Result<Unit, u16> {
            self.reads.lock().unwrap().push(unit);
            let clean = |code| Unit::Read {
                code,
                data: self.unit(unit),
            };
            let mut faults = self.faults.lock().unwrap();
            match faults.get(&unit).copied() {
                Some(Fault::Fail { code, times }) => {
                    if times == 0 {
                        faults.remove(&unit);
                        return Ok(clean(0));
                    }
                    faults.insert(
                        unit,
                        Fault::Fail {
                            code,
                            times: times - 1,
                        },
                    );
                    Err(code)
                }
                Some(Fault::Suspect(code)) => Ok(clean(code)),
                Some(Fault::Flaky) => {
                    let mut n = self.flaky_reads.lock().unwrap();
                    *n += 1;
                    Ok(Unit::Read {
                        code: 0,
                        data: stamp(0xF1A0_0000 + *n, usize::from(self.unit_size)),
                    })
                }
                Some(Fault::Slow(delay)) => {
                    faults.remove(&unit);
                    drop(faults);
                    thread::sleep(delay);
                    Ok(clean(0))
                }
                Some(Fault::Data(value)) => Ok(Unit::Read {
                    code: 0,
                    data: stamp(value, usize::from(self.unit_size)),
                }),
                None => Ok(clean(0)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::memory::Fault;
    use super::*;

    #[test]
    fn floppy_images_get_their_geometry() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("disk.img");
        let data: Vec<u8> = (0..368_640u32).map(|i| (i / 512) as u8).collect();
        std::fs::write(&path, &data).unwrap();
        let disk = ImageDisk::open(&path, None).unwrap();
        let d = disk.describe();
        assert_eq!(d.kind, DiskKind::Removable);
        assert_eq!(d.unit_count, 720);
        assert_eq!(d.geometry.unwrap().sectors, 9);
        assert_eq!(d.label, "disk.img 360K");
        assert!(d.chs());
        assert_eq!(
            disk.read(3, 0),
            Unit::Read {
                code: 0,
                data: vec![3; 512]
            }
        );
    }

    #[test]
    fn other_images_are_fixed_and_linear() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("hd.img");
        std::fs::write(&path, vec![7; 512 * 1000 + 100]).unwrap();
        let disk = ImageDisk::open(&path, None).unwrap();
        let d = disk.describe();
        assert_eq!(
            (d.kind, d.unit_count, d.chs()),
            (DiskKind::Fixed, 1000, false)
        );
        assert!(matches!(disk.read(999, 0), Unit::Read { .. }));
        assert!(matches!(disk.read(1000, 0), Unit::Failed { code: 4, .. }));
    }

    #[test]
    fn memory_disk_faults() {
        let disk = MemoryDisk::new(10, 8, None);
        assert_eq!(disk.unit(2), vec![2, 0, 0, 0, 2, 0, 0, 0]);
        disk.fault(
            1,
            Fault::Fail {
                code: 0x10,
                times: 2,
            },
        );
        disk.fault(2, Fault::Suspect(0x11));
        disk.fault(3, Fault::Flaky);
        // Two failures, so one retry is not enough and two are.
        assert_eq!(
            disk.read(1, 1),
            Unit::Failed {
                class: ErrorClass::MediaError,
                code: 0x10
            }
        );
        assert_eq!(
            disk.read(1, 0),
            Unit::Read {
                code: 0,
                data: disk.unit(1)
            }
        );
        assert_eq!(disk.reads(), [1, 1, 1]);
        assert!(matches!(disk.read(2, 0), Unit::Read { code: 0x11, .. }));
        assert_ne!(disk.read(3, 0), disk.read(3, 0));
        disk.fault(4, Fault::Slow(std::time::Duration::from_millis(1)));
        assert_eq!(
            disk.read(4, 0),
            Unit::Read {
                code: 0,
                data: disk.unit(4)
            }
        );
        assert_ne!(
            MemoryDisk::new(10, 8, None).with_seed(1).unit(2),
            disk.unit(2)
        );
        disk.fault(5, Fault::Data(9));
        assert_eq!(
            disk.read(5, 0),
            Unit::Read {
                code: 0,
                data: disk.unit(9)
            }
        );
        disk.clear(5);
        assert_eq!(
            disk.read(5, 0),
            Unit::Read {
                code: 0,
                data: disk.unit(5)
            }
        );
    }
}
