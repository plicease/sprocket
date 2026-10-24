// SPDX-License-Identifier: MIT
//! Native protocol v1 storage (`docs/protocol-v1.md` section 6): message
//! types, device descriptors, and read requests and results, shared by the
//! requester (`sprocket image`) and the responder.

use super::session::{ErrorClass, Fields};

pub const STORAGE_LIST: u8 = 0x20;
pub const STORAGE_READ: u8 = 0x21;

/// `addressing` bits in a descriptor.
pub const LINEAR: u8 = 1 << 0;
pub const CHS: u8 = 1 << 1;

/// `mode` in a `STORAGE_READ` request.
const MODE_LINEAR: u8 = 1;
const MODE_CHS: u8 = 2;

/// Descriptor `meta` tag for DOS: the BIOS drive number.
pub const META_BIOS_DRIVE: u8 = 1;
/// DOS meta tag: the drive's largest geometry (cylinders u16, heads,
/// sectors), within which CHS reads are accepted.
pub const META_DRIVE_GEOMETRY: u8 = 2;

/// Longest `meta` and `label` in a descriptor.
pub const MAX_META: usize = 64;
pub const MAX_LABEL: usize = 64;

/// Bytes of a `STORAGE_READ` response per unit besides its data: the
/// status (class and platform code).
pub const STATUS_LEN: usize = 3;

/// What kind of device a descriptor describes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiskKind {
    Unknown,
    Removable,
    Fixed,
    Other(u8),
}

impl DiskKind {
    pub fn code(self) -> u8 {
        match self {
            DiskKind::Unknown => 0,
            DiskKind::Removable => 1,
            DiskKind::Fixed => 2,
            DiskKind::Other(code) => code,
        }
    }
}

impl From<u8> for DiskKind {
    fn from(code: u8) -> Self {
        match code {
            0 => DiskKind::Unknown,
            1 => DiskKind::Removable,
            2 => DiskKind::Fixed,
            other => DiskKind::Other(other),
        }
    }
}

/// Cylinder/head/sector geometry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Geometry {
    pub cylinders: u16,
    pub heads: u8,
    pub sectors: u8,
    /// Number of the first sector on a track: 1 for the PC BIOS.
    pub first_sector: u8,
}

impl Geometry {
    /// Units the geometry covers.
    pub fn units(&self) -> u32 {
        u32::from(self.cylinders) * u32::from(self.heads) * u32::from(self.sectors)
    }

    /// The CHS address of linear unit `unit`, if the geometry holds it.
    pub fn chs(&self, unit: u32) -> Option<Address> {
        if self.heads == 0 || self.sectors == 0 || unit >= self.units() {
            return None;
        }
        let track = unit / u32::from(self.sectors);
        Some(Address::Chs {
            cylinder: (track / u32::from(self.heads)) as u16,
            head: (track % u32::from(self.heads)) as u8,
            sector: (unit % u32::from(self.sectors)) as u8 + self.first_sector,
        })
    }

    /// The linear unit at a CHS address, if the geometry holds it.
    pub fn linear(&self, cylinder: u16, head: u8, sector: u8) -> Option<u32> {
        let index = sector.checked_sub(self.first_sector)?;
        if cylinder >= self.cylinders || head >= self.heads || index >= self.sectors {
            return None;
        }
        Some(
            (u32::from(cylinder) * u32::from(self.heads) + u32::from(head))
                * u32::from(self.sectors)
                + u32::from(index),
        )
    }
}

/// One device from `STORAGE_LIST`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Descriptor {
    /// Session-local identifier, used in `STORAGE_READ`.
    pub device: u8,
    pub kind: DiskKind,
    /// [`LINEAR`] and [`CHS`] bits.
    pub addressing: u8,
    /// Bytes per unit; not assumed to be 512.
    pub unit_size: u16,
    /// Addressable units (linear).
    pub unit_count: u32,
    /// `None` when the device has no CHS geometry.
    pub geometry: Option<Geometry>,
    /// Platform metadata as tag and value pairs.
    pub meta: Vec<(u8, Vec<u8>)>,
    pub label: String,
}

impl Descriptor {
    /// Bytes the device holds.
    pub fn capacity(&self) -> u64 {
        u64::from(self.unit_size) * u64::from(self.unit_count)
    }

    pub fn linear(&self) -> bool {
        self.addressing & LINEAR != 0
    }

    pub fn chs(&self) -> bool {
        self.addressing & CHS != 0 && self.geometry.is_some()
    }

    /// The BIOS drive number, for DOS clients.
    pub fn bios_drive(&self) -> Option<u8> {
        self.meta
            .iter()
            .find(|(tag, value)| *tag == META_BIOS_DRIVE && value.len() == 1)
            .map(|(_, value)| value[0])
    }

    /// The drive's largest geometry, for DOS clients, which may exceed the
    /// medium's (a floppy's is probed).
    pub fn drive_geometry(&self) -> Option<Geometry> {
        self.meta
            .iter()
            .find(|(tag, value)| *tag == META_DRIVE_GEOMETRY && value.len() == 4)
            .map(|(_, v)| Geometry {
                cylinders: u16::from_le_bytes([v[0], v[1]]),
                heads: v[2],
                sectors: v[3],
                first_sector: self.geometry.map_or(1, |g| g.first_sector),
            })
    }

    /// Append the length-prefixed encoding to `out`.
    pub fn encode(&self, out: &mut Vec<u8>) {
        let geometry = self.geometry.unwrap_or(Geometry {
            cylinders: 0,
            heads: 0,
            sectors: 0,
            first_sector: 0,
        });
        let mut body = vec![self.device, self.kind.code(), self.addressing];
        body.extend_from_slice(&self.unit_size.to_le_bytes());
        body.extend_from_slice(&self.unit_count.to_le_bytes());
        body.extend_from_slice(&geometry.cylinders.to_le_bytes());
        body.extend_from_slice(&[geometry.heads, geometry.sectors, geometry.first_sector]);
        let mut meta = Vec::new();
        for (tag, value) in &self.meta {
            meta.push(*tag);
            meta.push(value.len() as u8);
            meta.extend_from_slice(value);
        }
        body.push(meta.len() as u8);
        body.extend_from_slice(&meta);
        body.push(self.label.len() as u8);
        body.extend_from_slice(self.label.as_bytes());
        out.push(body.len() as u8);
        out.extend_from_slice(&body);
    }

    /// Decode one descriptor, skipping any fields a later minor version
    /// added.
    pub(crate) fn decode(f: &mut Fields) -> Result<Self, String> {
        let len = usize::from(f.u8().map_err(|e| e.to_string())?);
        let body = f.take(len).map_err(|e| e.to_string())?;
        let mut g = Fields {
            data: body,
            what: "storage descriptor",
        };
        let parse = |g: &mut Fields| -> Result<Descriptor, super::session::Error> {
            let device = g.u8()?;
            let kind = DiskKind::from(g.u8()?);
            let addressing = g.u8()?;
            let unit_size = g.u16()?;
            let unit_count = g.u32()?;
            let geometry = Geometry {
                cylinders: g.u16()?,
                heads: g.u8()?,
                sectors: g.u8()?,
                first_sector: g.u8()?,
            };
            let meta_len = usize::from(g.u8()?);
            let mut tlv = Fields {
                data: g.take(meta_len)?,
                what: "storage descriptor meta",
            };
            let mut meta = Vec::new();
            while !tlv.data.is_empty() {
                let tag = tlv.u8()?;
                let len = usize::from(tlv.u8()?);
                meta.push((tag, tlv.take(len)?.to_vec()));
            }
            let label_len = usize::from(g.u8()?);
            let label = String::from_utf8_lossy(g.take(label_len)?).into_owned();
            Ok(Descriptor {
                device,
                kind,
                addressing,
                unit_size,
                unit_count,
                geometry: (geometry.cylinders != 0).then_some(geometry),
                meta,
                label,
            })
        };
        let d = parse(&mut g).map_err(|e| e.to_string())?;
        let meta_len: usize = d.meta.iter().map(|(_, v)| 2 + v.len()).sum();
        if meta_len > MAX_META || d.label.len() > MAX_LABEL {
            return Err("storage descriptor meta or label is too long".to_string());
        }
        if d.unit_size == 0 {
            return Err("storage descriptor has a unit size of 0".to_string());
        }
        Ok(d)
    }
}

/// A `STORAGE_LIST` response.
pub fn encode_list(devices: &[Descriptor]) -> Vec<u8> {
    let mut out = vec![devices.len() as u8];
    for d in devices {
        d.encode(&mut out);
    }
    out
}

/// Decode a `STORAGE_LIST` response.
pub fn decode_list(payload: &[u8]) -> Result<Vec<Descriptor>, String> {
    let mut f = Fields {
        data: payload,
        what: "STORAGE_LIST response",
    };
    let count = f.u8().map_err(|e| e.to_string())?;
    (0..count).map(|_| Descriptor::decode(&mut f)).collect()
}

/// Where a read starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Address {
    Linear(u32),
    Chs { cylinder: u16, head: u8, sector: u8 },
}

/// A `STORAGE_READ` request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReadRequest {
    pub device: u8,
    pub address: Address,
    /// Units to read. A CHS read stays within one track.
    pub count: u8,
    /// Client-side retry budget per unit.
    pub retries: u8,
}

impl ReadRequest {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = vec![self.device];
        match self.address {
            Address::Linear(unit) => {
                out.push(MODE_LINEAR);
                out.extend_from_slice(&unit.to_le_bytes());
            }
            Address::Chs {
                cylinder,
                head,
                sector,
            } => {
                out.push(MODE_CHS);
                out.extend_from_slice(&cylinder.to_le_bytes());
                out.extend_from_slice(&[head, sector]);
            }
        }
        out.extend_from_slice(&[self.count, self.retries]);
        out
    }

    /// Decode a request; an unknown mode is an error, as the extension
    /// rules require for an unknown enumeration value.
    pub fn decode(payload: &[u8]) -> Result<Self, String> {
        let mut f = Fields {
            data: payload,
            what: "STORAGE_READ request",
        };
        let parse = |f: &mut Fields| -> Result<Result<ReadRequest, String>, super::session::Error> {
            let device = f.u8()?;
            let mode = f.u8()?;
            let address = match mode {
                MODE_LINEAR => Address::Linear(f.u32()?),
                MODE_CHS => Address::Chs {
                    cylinder: f.u16()?,
                    head: f.u8()?,
                    sector: f.u8()?,
                },
                other => return Ok(Err(format!("unknown STORAGE_READ mode {other}"))),
            };
            Ok(Ok(ReadRequest {
                device,
                address,
                count: f.u8()?,
                retries: f.u8()?,
            }))
        };
        parse(&mut f).map_err(|e| e.to_string())?
    }
}

/// The result for one unit of a `STORAGE_READ`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unit {
    /// The unit was read. A nonzero `code` is a status the platform reported
    /// along with good data (for example BIOS `11h`, corrected by ECC): the
    /// unit is suspect.
    Read { code: u16, data: Vec<u8> },
    /// The unit could not be read; no data was sent for it.
    Failed { class: ErrorClass, code: u16 },
}

/// Bytes of a `STORAGE_READ` response for `count` units that all read.
pub fn read_response_len(count: usize, unit_size: usize) -> usize {
    1 + count * (STATUS_LEN + unit_size)
}

/// The most units a response within `max_payload` can carry.
pub fn units_per_read(max_payload: usize, unit_size: usize) -> usize {
    (max_payload.saturating_sub(1) / (STATUS_LEN + unit_size)).min(usize::from(u8::MAX))
}

/// A `STORAGE_READ` response.
pub fn encode_read_response(units: &[Unit]) -> Vec<u8> {
    let mut out = vec![units.len() as u8];
    for unit in units {
        let (class, code) = match unit {
            Unit::Read { code, .. } => (0, *code),
            Unit::Failed { class, code } => (class.code(), *code),
        };
        out.push(class);
        out.extend_from_slice(&code.to_le_bytes());
    }
    for unit in units {
        if let Unit::Read { data, .. } = unit {
            out.extend_from_slice(data);
        }
    }
    out
}

/// Decode a `STORAGE_READ` response for a request of `count` units of
/// `unit_size` bytes. The data runs to the end of the payload, so it must
/// be exactly one unit per unit read.
pub fn decode_read_response(
    payload: &[u8],
    count: u8,
    unit_size: usize,
) -> Result<Vec<Unit>, String> {
    let mut f = Fields {
        data: payload,
        what: "STORAGE_READ response",
    };
    let got = f.u8().map_err(|e| e.to_string())?;
    if got != count {
        return Err(format!(
            "STORAGE_READ response has {got} units; {count} were asked for"
        ));
    }
    let mut statuses = Vec::with_capacity(usize::from(count));
    for _ in 0..count {
        let class = f.u8().map_err(|e| e.to_string())?;
        let code = f.u16().map_err(|e| e.to_string())?;
        statuses.push((class, code));
    }
    let read = statuses.iter().filter(|(class, _)| *class == 0).count();
    if f.data.len() != read * unit_size {
        return Err(format!(
            "STORAGE_READ response has {} data bytes for {read} units of {unit_size}",
            f.data.len()
        ));
    }
    let mut data = f.data.chunks_exact(unit_size.max(1));
    Ok(statuses
        .into_iter()
        .map(|(class, code)| match class {
            0 => Unit::Read {
                code,
                data: data.next().unwrap_or_default().to_vec(),
            },
            class => Unit::Failed {
                class: ErrorClass::from(class),
                code,
            },
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn floppy() -> Descriptor {
        Descriptor {
            device: 0,
            kind: DiskKind::Removable,
            addressing: LINEAR | CHS,
            unit_size: 512,
            unit_count: 720,
            geometry: Some(Geometry {
                cylinders: 40,
                heads: 2,
                sectors: 9,
                first_sector: 1,
            }),
            meta: vec![
                (META_BIOS_DRIVE, vec![0x00]),
                (META_DRIVE_GEOMETRY, vec![80, 0, 2, 15]),
            ],
            label: "BIOS 00 (A:) 360K".to_string(),
        }
    }

    #[test]
    fn descriptors_round_trip() {
        let fixed = Descriptor {
            device: 1,
            kind: DiskKind::Fixed,
            addressing: LINEAR,
            unit_size: 256,
            unit_count: 0x0012_3456,
            geometry: None,
            meta: Vec::new(),
            label: String::new(),
        };
        let list = vec![floppy(), fixed];
        let wire = encode_list(&list);
        assert_eq!(decode_list(&wire).unwrap(), list);
        assert_eq!(list[0].capacity(), 368_640);
        assert_eq!(list[0].bios_drive(), Some(0));
        assert_eq!(
            list[0].drive_geometry(),
            Some(Geometry {
                cylinders: 80,
                heads: 2,
                sectors: 15,
                first_sector: 1
            })
        );
        assert_eq!(list[1].drive_geometry(), None);
        assert!(list[0].chs() && list[0].linear());
        assert!(!list[1].chs());
        assert_eq!(decode_list(&[0]).unwrap(), []);
    }

    #[test]
    fn descriptor_fields_from_later_versions_are_skipped() {
        let mut wire = Vec::new();
        floppy().encode(&mut wire);
        wire[0] += 3;
        wire.extend_from_slice(&[7, 8, 9]);
        let mut list = vec![2];
        list.extend_from_slice(&wire);
        list.extend_from_slice(&wire);
        assert_eq!(decode_list(&list).unwrap(), vec![floppy(), floppy()]);
    }

    #[test]
    fn malformed_descriptors_are_refused() {
        let wire = encode_list(&[floppy()]);
        assert!(decode_list(&wire[..wire.len() - 1]).is_err());
        let mut zero = floppy();
        zero.unit_size = 0;
        assert!(decode_list(&encode_list(&[zero])).is_err());
        let mut long = floppy();
        long.label = "x".repeat(65);
        assert!(decode_list(&encode_list(&[long])).is_err());
        // A meta TLV running past the end of meta.
        let mut bad = floppy();
        bad.meta = vec![(1, vec![0])];
        let mut wire = encode_list(&[bad]);
        let meta_at = 1 + 1 + 14;
        assert_eq!(wire[meta_at], 3);
        wire[meta_at + 2] = 9;
        assert!(decode_list(&wire).is_err());
    }

    #[test]
    fn requests_round_trip() {
        for address in [
            Address::Linear(0x0102_0304),
            Address::Chs {
                cylinder: 1023,
                head: 15,
                sector: 63,
            },
        ] {
            let request = ReadRequest {
                device: 2,
                address,
                count: 7,
                retries: 3,
            };
            assert_eq!(ReadRequest::decode(&request.encode()).unwrap(), request);
        }
        assert!(ReadRequest::decode(&[0, 3, 0, 0, 0, 0, 1, 0]).is_err());
        assert!(ReadRequest::decode(&[0, 1, 0, 0]).is_err());
    }

    #[test]
    fn read_responses_round_trip() {
        let units = vec![
            Unit::Read {
                code: 0,
                data: vec![1; 4],
            },
            Unit::Failed {
                class: ErrorClass::MediaError,
                code: 0x10,
            },
            Unit::Read {
                code: 0x11,
                data: vec![2; 4],
            },
        ];
        let wire = encode_read_response(&units);
        assert_eq!(wire.len(), 1 + 3 * 3 + 8);
        assert_eq!(decode_read_response(&wire, 3, 4).unwrap(), units);
        assert!(decode_read_response(&wire, 2, 4).is_err());
        assert!(decode_read_response(&wire[..wire.len() - 1], 3, 4).is_err());
        let mut extra = wire.clone();
        extra.push(0);
        assert!(decode_read_response(&extra, 3, 4).is_err());
    }

    #[test]
    fn units_per_frame() {
        assert_eq!(units_per_read(1024, 512), 1);
        assert_eq!(units_per_read(4090, 512), 7);
        assert_eq!(read_response_len(7, 512), 3606);
        assert_eq!(units_per_read(128, 512), 0);
        assert_eq!(units_per_read(4090, 1), 255);
    }

    #[test]
    fn chs_and_linear_addresses() {
        let g = floppy().geometry.unwrap();
        assert_eq!(g.units(), 720);
        assert_eq!(
            g.chs(0),
            Some(Address::Chs {
                cylinder: 0,
                head: 0,
                sector: 1
            })
        );
        assert_eq!(
            g.chs(9 * 3 + 4),
            Some(Address::Chs {
                cylinder: 1,
                head: 1,
                sector: 5
            })
        );
        assert_eq!(g.chs(720), None);
        for unit in [0, 8, 9, 17, 18, 719] {
            let Some(Address::Chs {
                cylinder,
                head,
                sector,
            }) = g.chs(unit)
            else {
                panic!()
            };
            assert_eq!(g.linear(cylinder, head, sector), Some(unit));
        }
        assert_eq!(g.linear(0, 0, 0), None);
        assert_eq!(g.linear(0, 0, 10), None);
        assert_eq!(g.linear(40, 0, 1), None);
    }
}
