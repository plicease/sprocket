// SPDX-License-Identifier: MIT
//! Capture a client's disk into `NAME.img`, with its progress map
//! (`NAME.img.map`) and manifest (`NAME.img.json`), resuming from the map
//! when there is one (`docs/imaging.md`).
//!
//! A unit is marked captured only after the image has been synced past
//! it, so a crash at any point loses at most units that are then read
//! again. A request that gets no response leaves its units missing: the
//! link is restarted with a new session and the capture continues, after
//! checking that the client still has the same disk.
//!
//! Once every unit has been read, recovery passes (if asked for) reread
//! the failed, suspect and conflicting units one at a time. A failed unit
//! that now reads is captured; a suspect one that rereads the same with no
//! status is no longer suspect; one that rereads differently becomes
//! conflicting, with every version kept in the reread log
//! (`NAME.img.reads`). A captured unit whose reread fails keeps its data,
//! and the failure is recorded beside it, making it suspect.

use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use super::manifest::{self, Manifest};
use super::map::{self, Counts, Entry, LoadError, Map, Mode, Print, Source, State};
use super::reads::{self, Log};
use crate::native::link;
use crate::native::session::{self, ClientInfo, Session};
use crate::native::storage::{self, Address, CHS, Descriptor, DiskKind, LINEAR, ReadRequest, Unit};
use crate::transport::Transport;

/// Units read for the fingerprint: unit 0 and this many more spread across
/// the disk.
const SPREAD: u32 = 7;

/// Capture settings.
#[derive(Clone, Debug)]
pub struct Options {
    /// Device number from `STORAGE_LIST`.
    pub device: u8,
    /// Client-side retries per unit.
    pub retries: u8,
    /// Most units per request; 0 for as many as fit the payload.
    pub max_units: usize,
    /// Recovery passes over failed, suspect and conflicting units, once
    /// every unit has been read.
    pub recover: u32,
    /// Image by this CHS geometry instead of the device's, which for a
    /// floppy is probed and can be wrong. It must fit within the drive's
    /// geometry. A resumed image keeps the geometry it was started with.
    pub geometry: Option<storage::Geometry>,
    /// Sync the image and save the map at least this often.
    pub sync_every: Duration,
    /// Times in a row to restart the link after a request gets no
    /// response, before giving up.
    pub reconnects: u32,
    pub session: session::Options,
    /// Stop abruptly, without syncing or saving, after this many
    /// responses: a crash, for tests.
    #[cfg(test)]
    pub halt_after: Option<usize>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            device: 0,
            retries: 3,
            max_units: 0,
            recover: 0,
            geometry: None,
            sync_every: Duration::from_secs(2),
            reconnects: 3,
            session: session::Options::default(),
            #[cfg(test)]
            halt_after: None,
        }
    }
}

/// The files of one image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paths {
    pub image: PathBuf,
    pub map: PathBuf,
    pub manifest: PathBuf,
    pub reads: PathBuf,
}

impl Paths {
    /// `NAME.img`, `NAME.img.map`, `NAME.img.json` and `NAME.img.reads`.
    pub fn new(image: &Path) -> Self {
        let with = |ext: &str| {
            let mut name = image.as_os_str().to_os_string();
            name.push(ext);
            PathBuf::from(name)
        };
        Self {
            image: image.to_path_buf(),
            map: with(".map"),
            manifest: with(".json"),
            reads: with(".reads"),
        }
    }
}

/// Something the operator must agree to before the capture continues.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Question {
    /// The device reports a different geometry than when imaging started.
    GeometryChanged { was: String, now: String },
    /// A fingerprint unit holds different data: probably another disk.
    FingerprintDiffers { unit: u32 },
    /// A fingerprint unit could not be read, so the disk is unverified.
    FingerprintUnreadable { unit: u32, why: String },
}

impl fmt::Display for Question {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Question::GeometryChanged { was, now } => write!(
                f,
                "the device's geometry was {was} and is now {now}; it may be a different disk"
            ),
            Question::FingerprintDiffers { unit } => write!(
                f,
                "unit {unit} differs from when imaging started; it is probably a different disk"
            ),
            Question::FingerprintUnreadable { unit, why } => write!(
                f,
                "unit {unit} could not be read to check that this is the same disk ({why}); \
                 the disk is unverified"
            ),
        }
    }
}

/// Progress, for display.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// Imaging started or resumed.
    Started {
        source: Source,
        resumed: bool,
        counts: Counts,
        units_per_read: usize,
    },
    /// A response was handled.
    Progress { counts: Counts },
    /// A unit could not be read.
    UnitFailed { unit: u32, code: u16 },
    /// A recovery pass is starting over this many units.
    Recovering { pass: u32, units: usize },
    /// A unit that had failed was read.
    Recovered { unit: u32 },
    /// A reread of a captured unit failed; the image keeps the data read
    /// before.
    RereadFailed { unit: u32, code: u16 },
    /// A reread returned different data than an earlier read.
    Conflict { unit: u32 },
    /// A request got no response; the link is being restarted.
    LinkLost(String),
}

#[derive(Debug)]
pub enum Error {
    /// A host file could not be read or written.
    Io(io::Error),
    Session(session::Error),
    /// The image exists without a progress map, so it is not ours to
    /// change.
    ImageExists(PathBuf),
    /// The progress map is damaged, so nothing says which of the image's
    /// units are good; the image and map are left as they are.
    MapUnusable {
        map: PathBuf,
        why: String,
    },
    NoStorage,
    NoDevice(u8),
    /// The device offers no addressing this host can use, or its units do
    /// not fit a response.
    Unreadable(String),
    /// The geometry asked for cannot be used with the device.
    BadGeometry(String),
    /// The device's units differ from the map's, so the map cannot apply.
    SourceChanged(String),
    /// The operator declined to continue.
    Declined(Question),
    /// The link stayed down after every restart.
    LinkLost(String),
    #[cfg(test)]
    Halted,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "{e}"),
            Error::Session(e) => write!(f, "{e}"),
            Error::ImageExists(path) => write!(
                f,
                "{} exists and has no progress map; refusing to overwrite it",
                path.display()
            ),
            Error::MapUnusable { map, why } => write!(
                f,
                "the progress map {} is unusable ({why}); the image and map are left \
                 untouched: image to a new file, or move them aside to start again",
                map.display()
            ),
            Error::NoStorage => {
                f.write_str("the server offers no disks (on DOS, start it with /D)")
            }
            Error::NoDevice(device) => write!(f, "the server has no device {device}"),
            Error::Unreadable(why) => write!(f, "cannot read the device: {why}"),
            Error::BadGeometry(why) => write!(f, "cannot image by that geometry: {why}"),
            Error::SourceChanged(why) => write!(
                f,
                "the device does not match the progress map ({why}); image it to a new file"
            ),
            Error::Declined(q) => write!(f, "stopped: {q}"),
            Error::LinkLost(why) => write!(f, "the link was lost ({why}); run again to resume"),
            #[cfg(test)]
            Error::Halted => f.write_str("halted"),
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

impl From<session::Error> for Error {
    fn from(e: session::Error) -> Self {
        Error::Session(e)
    }
}

/// How a run ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Summary {
    pub paths: Paths,
    pub counts: Counts,
    pub complete: bool,
}

/// Capture device `options.device` from the server on `transport` into
/// `image`, resuming from its progress map if there is one. `confirm` is
/// asked before continuing with a disk that may not be the one imaged
/// before.
pub fn capture(
    transport: &mut dyn Transport,
    image: &Path,
    options: &Options,
    confirm: &mut dyn FnMut(&Question) -> bool,
    events: &mut dyn FnMut(&Event),
) -> Result<Summary, Error> {
    let paths = Paths::new(image);
    let mut existing = existing_map(&paths)?;
    let mut capture: Option<Capture> = None;
    let mut previous_session = None;
    let mut lost = 0;
    loop {
        let session_options = session::Options {
            previous_session,
            ..options.session.clone()
        };
        let mut session = match Session::connect(transport, &session_options, |_| {}) {
            Ok(session) => session,
            Err(e) => {
                if let Some(c) = capture.as_mut() {
                    c.finish()?;
                }
                return Err(e.into());
            }
        };
        previous_session = Some(session.session_id());
        let c = match capture.as_mut() {
            Some(c) => {
                c.recheck(&mut session, confirm)?;
                c
            }
            None => capture.insert(Capture::start(
                &mut session,
                &paths,
                existing.take(),
                options,
                confirm,
                events,
            )?),
        };
        match c.run(&mut session, events)? {
            None => {
                let summary = c.finish()?;
                let _ = session.close();
                return Ok(summary);
            }
            Some(why) => {
                c.flush()?;
                lost = if c.progressed { 1 } else { lost + 1 };
                if lost > options.reconnects {
                    c.finish()?;
                    return Err(Error::LinkLost(why));
                }
                events(&Event::LinkLost(why));
            }
        }
    }
}

/// The map to resume from: `None` for a new image. An image without a map
/// is refused, and so is a map that does not load: the image may hold
/// units that cannot be read again, and nothing says which.
fn existing_map(paths: &Paths) -> Result<Option<Map>, Error> {
    match Map::load(&paths.map) {
        Ok(map) => Ok(Some(map)),
        Err(LoadError::Io(e)) if e.kind() == io::ErrorKind::NotFound => {
            if paths.image.exists() {
                Err(Error::ImageExists(paths.image.clone()))
            } else {
                Ok(None)
            }
        }
        Err(LoadError::Io(e)) => Err(e.into()),
        Err(LoadError::Invalid(why)) => Err(Error::MapUnusable {
            map: paths.map.clone(),
            why,
        }),
    }
}

fn map_geometry(g: storage::Geometry) -> map::Geometry {
    map::Geometry {
        cylinders: g.cylinders,
        heads: g.heads,
        sectors: g.sectors,
        first_sector: g.first_sector,
    }
}

fn storage_geometry(g: map::Geometry) -> storage::Geometry {
    storage::Geometry {
        cylinders: g.cylinders,
        heads: g.heads,
        sectors: g.sectors,
        first_sector: g.first_sector,
    }
}

/// The device as imaged by `geometry`, if one is given: read by CHS with
/// that geometry, which must fit within the drive's (or, if the client
/// does not report the drive's, the device's own). A drive whose medium
/// has no geometry (a floppy whose probe failed) can be read this way
/// too.
fn with_geometry(
    mut device: Descriptor,
    geometry: Option<storage::Geometry>,
) -> Result<Descriptor, Error> {
    let Some(g) = geometry else {
        return Ok(device);
    };
    let drive = device.drive_geometry();
    let Some(reported) = device.geometry.filter(|_| device.chs()).or(drive) else {
        return Err(Error::BadGeometry(
            "the device does not offer CHS addressing".to_string(),
        ));
    };
    if g.cylinders == 0 || g.heads == 0 || g.sectors == 0 {
        return Err(Error::BadGeometry(
            "cylinders, heads and sectors must each be at least 1".to_string(),
        ));
    }
    let (limit, whose) = match drive {
        Some(drive) => (drive, "the drive's"),
        None => (reported, "the device's"),
    };
    if g.cylinders > limit.cylinders || g.heads > limit.heads || g.sectors > limit.sectors {
        return Err(Error::BadGeometry(format!(
            "{} is larger than {whose} {}",
            describe_geometry(Some(map_geometry(g))),
            describe_geometry(Some(map_geometry(limit)))
        )));
    }
    device.geometry = Some(storage::Geometry {
        first_sector: reported.first_sector,
        ..g
    });
    device.unit_count = g.units();
    // Linear units follow the device's geometry, not this one.
    device.addressing = (device.addressing & !LINEAR) | CHS;
    Ok(device)
}

/// The geometry the device itself reported.
fn reported_geometry(source: &Source) -> Option<map::Geometry> {
    source.reported_geometry.or(source.geometry)
}

/// The map's record of a device: `d` as imaged, `reported` as listed, and
/// whether the operator gave the geometry.
fn source_of(
    d: &Descriptor,
    reported: &Descriptor,
    overridden: bool,
    client: &ClientInfo,
) -> Source {
    Source {
        device: d.device,
        label: d.label.clone(),
        kind: match d.kind {
            DiskKind::Removable => "removable".to_string(),
            DiskKind::Fixed => "fixed".to_string(),
            DiskKind::Unknown => "unknown".to_string(),
            DiskKind::Other(code) => format!("kind {code}"),
        },
        unit_size: d.unit_size,
        unit_count: d.unit_count,
        geometry: d.geometry.map(map_geometry),
        reported_geometry: overridden
            .then(|| reported.geometry.map(map_geometry))
            .flatten(),
        drive_geometry: reported.drive_geometry().map(map_geometry),
        bios_drive: d.bios_drive(),
        client_platform: client.platform.to_string(),
        client_version: client.version.clone(),
    }
}

fn describe_geometry(g: Option<map::Geometry>) -> String {
    match g {
        Some(g) => format!("{}/{}/{}", g.cylinders, g.heads, g.sectors),
        None => "none".to_string(),
    }
}

struct Capture<'o> {
    paths: Paths,
    options: &'o Options,
    map: Map,
    image: File,
    /// The device as imaged, with the operator's geometry if there is one.
    device: Descriptor,
    /// The operator's geometry.
    geometry: Option<storage::Geometry>,
    per_read: usize,
    /// Units read but not yet covered by a sync.
    pending: Vec<(u32, Entry)>,
    /// The map changed since it was saved.
    dirty: bool,
    last_sync: Instant,
    /// A response arrived since the last link restart.
    progressed: bool,
    log: Log,
    /// Recovery passes finished.
    passes: u32,
    /// Units the current recovery pass has still to reread, last first.
    queue: Vec<u32>,
    #[cfg(test)]
    responses: usize,
}

impl<'o> Capture<'o> {
    fn start(
        session: &mut Session,
        paths: &Paths,
        existing: Option<Map>,
        options: &'o Options,
        confirm: &mut dyn FnMut(&Question) -> bool,
        events: &mut dyn FnMut(&Event),
    ) -> Result<Self, Error> {
        let reported = find_device(session, options.device)?;
        // A resumed image keeps the geometry it was started with.
        let started_with = existing
            .as_ref()
            .filter(|map| map.source.reported_geometry.is_some())
            .and_then(|map| map.source.geometry.map(storage_geometry));
        let geometry = options.geometry.or(started_with);
        let device = with_geometry(reported.clone(), geometry)?;
        let source = source_of(&device, &reported, geometry.is_some(), session.client());
        if device.unit_count == 0 {
            return Err(Error::Unreadable(
                "it reports no units (is there a disk in the drive?)".to_string(),
            ));
        }
        let per_read =
            storage::units_per_read(session.max_payload(), usize::from(device.unit_size));
        let per_read = match options.max_units {
            0 => per_read,
            n => per_read.min(n),
        };
        if per_read == 0 {
            return Err(Error::Unreadable(format!(
                "a {}-byte unit does not fit a {}-byte payload",
                device.unit_size,
                session.max_payload()
            )));
        }
        if !device.linear() && !device.chs() {
            return Err(Error::Unreadable(
                "it offers neither linear nor CHS addressing".to_string(),
            ));
        }
        let capacity = device.capacity();
        let (map, image, resumed) = match existing {
            Some(mut map) => {
                if map.source.geometry != source.geometry
                    && (geometry.is_some() || map.source.reported_geometry.is_some())
                {
                    return Err(Error::SourceChanged(format!(
                        "it was imaged by geometry {}, not {}",
                        describe_geometry(map.source.geometry),
                        describe_geometry(source.geometry)
                    )));
                }
                if map.source.unit_size != source.unit_size
                    || map.source.unit_count != source.unit_count
                {
                    return Err(Error::SourceChanged(format!(
                        "{} units of {} bytes, now {} of {}",
                        map.source.unit_count,
                        map.source.unit_size,
                        source.unit_count,
                        source.unit_size
                    )));
                }
                let image = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&paths.image)?;
                map.trim_to(image.metadata()?.len());
                (map, image, true)
            }
            None => {
                let image = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create_new(true)
                    .open(&paths.image)?;
                let mut map = Map::new(source.clone(), Mode::Raw);
                map.started = manifest::seconds(SystemTime::now());
                (map, image, false)
            }
        };
        image.set_len(capacity.max(image.metadata()?.len()))?;
        let mut capture = Self {
            paths: paths.clone(),
            options,
            map,
            image,
            device,
            geometry,
            per_read,
            pending: Vec::new(),
            dirty: true,
            last_sync: Instant::now(),
            progressed: false,
            log: Log::open(&paths.reads)?,
            passes: 0,
            queue: Vec::new(),
            #[cfg(test)]
            responses: 0,
        };
        if resumed {
            capture.check_geometry(&source, confirm)?;
            capture.check_fingerprint(session, confirm)?;
        } else {
            capture.take_fingerprint(session, events)?;
        }
        capture.flush()?;
        events(&Event::Started {
            source: capture.map.source.clone(),
            resumed,
            counts: capture.map.counts(),
            units_per_read: per_read,
        });
        Ok(capture)
    }

    /// After a link restart: the client must still have the same disk.
    fn recheck(
        &mut self,
        session: &mut Session,
        confirm: &mut dyn FnMut(&Question) -> bool,
    ) -> Result<(), Error> {
        let reported = find_device(session, self.options.device)?;
        let device = with_geometry(reported.clone(), self.geometry)?;
        let source = source_of(
            &device,
            &reported,
            self.geometry.is_some(),
            session.client(),
        );
        if source.unit_size != self.map.source.unit_size
            || source.unit_count != self.map.source.unit_count
        {
            return Err(Error::SourceChanged(format!(
                "now {} units of {} bytes",
                source.unit_count, source.unit_size
            )));
        }
        self.check_geometry(&source, confirm)?;
        self.check_fingerprint(session, confirm)?;
        self.device = device;
        self.progressed = false;
        Ok(())
    }

    fn check_geometry(
        &mut self,
        source: &Source,
        confirm: &mut dyn FnMut(&Question) -> bool,
    ) -> Result<(), Error> {
        let (was, now) = (
            reported_geometry(&self.map.source),
            reported_geometry(source),
        );
        if was != now {
            self.ask(
                Question::GeometryChanged {
                    was: describe_geometry(was),
                    now: describe_geometry(now),
                },
                confirm,
            )?;
        }
        Ok(())
    }

    /// Read the fingerprint units again and compare their hashes.
    fn check_fingerprint(
        &mut self,
        session: &mut Session,
        confirm: &mut dyn FnMut(&Question) -> bool,
    ) -> Result<(), Error> {
        for print in self.map.fingerprint.clone() {
            let question = match self.read(session, print.unit, 1) {
                Ok(units) => match &units[0] {
                    Unit::Read { data, .. } if manifest::sha256(data) == print.sha256 => continue,
                    Unit::Read { .. } => Question::FingerprintDiffers { unit: print.unit },
                    Unit::Failed { class, code } => Question::FingerprintUnreadable {
                        unit: print.unit,
                        why: format!("{class}, code {code:#06x}"),
                    },
                },
                Err(session::Error::Client(e)) => Question::FingerprintUnreadable {
                    unit: print.unit,
                    why: e.to_string(),
                },
                Err(e @ session::Error::Link(_)) => Question::FingerprintUnreadable {
                    unit: print.unit,
                    why: e.to_string(),
                },
                Err(e) => return Err(e.into()),
            };
            self.ask(question, confirm)?;
            // Once the operator has accepted the disk, the rest of the
            // fingerprint cannot change that.
            return Ok(());
        }
        Ok(())
    }

    fn ask(
        &mut self,
        question: Question,
        confirm: &mut dyn FnMut(&Question) -> bool,
    ) -> Result<(), Error> {
        if !confirm(&question) {
            return Err(Error::Declined(question));
        }
        let now = manifest::iso8601(manifest::seconds(SystemTime::now()));
        self.map.confirmations.push(format!("{now}: {question}"));
        self.dirty = true;
        Ok(())
    }

    /// Read unit 0 and units spread across the disk first, recording their
    /// hashes so a resumed capture can tell it has the same disk. A unit
    /// that fails is left out.
    fn take_fingerprint(
        &mut self,
        session: &mut Session,
        events: &mut dyn FnMut(&Event),
    ) -> Result<(), Error> {
        let units = self.map.units();
        let mut chosen: Vec<u32> = (0..=SPREAD)
            .map(|k| (u64::from(units) * u64::from(k) / u64::from(SPREAD + 1)) as u32)
            .collect();
        chosen.dedup();
        for unit in chosen.into_iter().filter(|&u| u < units) {
            let result = self.read(session, unit, 1)?;
            if let Unit::Read { data, .. } = &result[0] {
                self.map.fingerprint.push(Print {
                    unit,
                    sha256: manifest::sha256(data),
                });
            }
            for (unit, code) in self.record(unit, &result)? {
                events(&Event::UnitFailed { unit, code });
            }
        }
        Ok(())
    }

    /// Read the missing units, then make the recovery passes. Returns why
    /// the link was lost, if it was.
    fn run(
        &mut self,
        session: &mut Session,
        events: &mut dyn FnMut(&Event),
    ) -> Result<Option<String>, Error> {
        let missing: Vec<(u32, u32)> = self.map.with_state(State::Missing).collect();
        for (start, end) in missing {
            let mut unit = start;
            while unit < end {
                let count = self.chunk(unit, end);
                match self.read(session, unit, count) {
                    Ok(units) => {
                        for (unit, code) in self.record(unit, &units)? {
                            events(&Event::UnitFailed { unit, code });
                        }
                        events(&Event::Progress {
                            counts: self.counts(),
                        });
                        self.responded()?;
                    }
                    Err(e @ session::Error::Link(link::Error::NoResponse { .. }))
                    | Err(e @ session::Error::Link(link::Error::SessionLost)) => {
                        return Ok(Some(e.to_string()));
                    }
                    Err(e) => {
                        self.finish()?;
                        return Err(e.into());
                    }
                }
                unit += count;
                if self.last_sync.elapsed() >= self.options.sync_every {
                    self.flush()?;
                }
            }
        }
        self.recover(session, events)
    }

    /// Recovery passes, each rereading the units that need it one at a
    /// time. Returns why the link was lost, if it was; the pass then
    /// carries on where it stopped.
    fn recover(
        &mut self,
        session: &mut Session,
        events: &mut dyn FnMut(&Event),
    ) -> Result<Option<String>, Error> {
        while self.passes < self.options.recover {
            if self.queue.is_empty() {
                // The map must show what the last pass found.
                self.flush()?;
                self.queue = self.needing_recovery();
                if self.queue.is_empty() {
                    break;
                }
                events(&Event::Recovering {
                    pass: self.passes + 1,
                    units: self.queue.len(),
                });
                self.queue.reverse();
            }
            while let Some(&unit) = self.queue.last() {
                match self.read(session, unit, 1) {
                    Ok(units) => {
                        self.reread(unit, &units[0], events)?;
                        events(&Event::Progress {
                            counts: self.counts(),
                        });
                        self.responded()?;
                    }
                    Err(e @ session::Error::Link(link::Error::NoResponse { .. }))
                    | Err(e @ session::Error::Link(link::Error::SessionLost)) => {
                        return Ok(Some(e.to_string()));
                    }
                    Err(e) => {
                        self.finish()?;
                        return Err(e.into());
                    }
                }
                self.queue.pop();
                if self.last_sync.elapsed() >= self.options.sync_every {
                    self.flush()?;
                }
            }
            self.passes += 1;
        }
        Ok(None)
    }

    /// Units a recovery pass rereads: failed, suspect and conflicting.
    fn needing_recovery(&self) -> Vec<u32> {
        self.map
            .ranges()
            .iter()
            .filter(|r| {
                r.entry.state == State::Failed
                    || r.entry.state == State::Captured && (r.entry.code != 0 || r.entry.conflict)
            })
            .flat_map(|r| r.start..r.end)
            .collect()
    }

    /// Record a recovery pass's reread of `unit`.
    fn reread(
        &mut self,
        unit: u32,
        result: &Unit,
        events: &mut dyn FnMut(&Event),
    ) -> Result<(), Error> {
        self.progressed = true;
        let before = self.map.get(unit);
        let mut after = Entry {
            attempts: before.attempts + 1,
            retries: self.options.retries,
            ..before
        };
        match (before.state, result) {
            (State::Failed, Unit::Failed { code, .. }) => {
                events(&Event::UnitFailed { unit, code: *code });
                after.code = *code;
            }
            (_, Unit::Failed { code, .. }) => {
                // Still captured, with the data read before and its status;
                // the failure is recorded beside them.
                events(&Event::RereadFailed { unit, code: *code });
                after.reread_failures += 1;
                after.reread_code = *code;
            }
            (State::Failed, Unit::Read { code, data }) => {
                events(&Event::Recovered { unit });
                self.write_unit(unit, data)?;
                self.pending.push((
                    unit,
                    Entry {
                        state: State::Captured,
                        code: *code,
                        ..after
                    },
                ));
                return Ok(());
            }
            (_, Unit::Read { code, data }) => {
                let held = self.read_unit(unit)?;
                after.code = *code;
                if !before.conflict && *data == held {
                    // The same again: suspect only if the client still
                    // says so.
                } else {
                    if !before.conflict {
                        events(&Event::Conflict { unit });
                        self.log.append(reads::Read {
                            unit,
                            sha256: manifest::sha256(&held),
                            time: None,
                            attempt: before.attempts,
                            retries: before.retries,
                            code: before.code,
                            data: held.clone(),
                        })?;
                    }
                    self.log.append(reads::Read {
                        unit,
                        sha256: manifest::sha256(data),
                        time: Some(manifest::iso8601(manifest::seconds(SystemTime::now()))),
                        attempt: after.attempts,
                        retries: after.retries,
                        code: *code,
                        data: data.clone(),
                    })?;
                    after.conflict = true;
                    let chosen = reads::choose(self.log.reads(unit))
                        .map(|r| r.data.clone())
                        .unwrap_or(held.clone());
                    if chosen != held {
                        self.write_unit(unit, &chosen)?;
                        self.pending.push((unit, after));
                        return Ok(());
                    }
                }
            }
        }
        self.map.set(unit, unit + 1, after);
        self.dirty = true;
        Ok(())
    }

    fn write_unit(&mut self, unit: u32, data: &[u8]) -> io::Result<()> {
        self.image.seek(SeekFrom::Start(
            u64::from(unit) * u64::from(self.device.unit_size),
        ))?;
        self.image.write_all(data)
    }

    /// The unit as the image holds it.
    fn read_unit(&mut self, unit: u32) -> io::Result<Vec<u8>> {
        let mut data = vec![0; usize::from(self.device.unit_size)];
        self.image.seek(SeekFrom::Start(
            u64::from(unit) * u64::from(self.device.unit_size),
        ))?;
        self.image.read_exact(&mut data)?;
        Ok(data)
    }

    /// Count a response, for tests that stop after so many.
    fn responded(&mut self) -> Result<(), Error> {
        #[cfg(test)]
        {
            self.responses += 1;
            if self.options.halt_after == Some(self.responses) {
                return Err(Error::Halted);
            }
        }
        Ok(())
    }

    /// Units to ask for at `unit`: as many as fit a response, stopping at
    /// `end` and, for CHS, at the end of the track.
    fn chunk(&self, unit: u32, end: u32) -> u32 {
        let mut count = (self.per_read as u32).min(end - unit);
        if !self.device.linear()
            && let Some(g) = self.device.geometry
        {
            let sectors = u32::from(g.sectors);
            let track_end = (unit / sectors + 1) * sectors;
            count = count.min(track_end - unit);
        }
        count
    }

    fn read(
        &self,
        session: &mut Session,
        unit: u32,
        count: u32,
    ) -> Result<Vec<Unit>, session::Error> {
        let address = if self.device.linear() {
            Address::Linear(unit)
        } else {
            self.device
                .geometry
                .and_then(|g| g.chs(unit))
                .ok_or_else(|| {
                    session::Error::Protocol(format!("unit {unit} has no CHS address"))
                })?
        };
        let request = ReadRequest {
            device: self.device.device,
            address,
            count: count as u8,
            retries: self.options.retries,
        };
        session.storage_read(&request, self.device.unit_size)
    }

    /// Write what a response read from `first` on: data into the image,
    /// pending a sync; failures straight into the map. Returns the units
    /// that failed, with their codes.
    fn record(&mut self, first: u32, units: &[Unit]) -> Result<Vec<(u32, u16)>, Error> {
        let mut failed = Vec::new();
        self.progressed = true;
        for (i, result) in units.iter().enumerate() {
            let unit = first + i as u32;
            let before = self.map.get(unit);
            let attempts = before.attempts + 1;
            match result {
                Unit::Read { code, data } => {
                    self.write_unit(unit, data)?;
                    self.pending.push((
                        unit,
                        Entry {
                            state: State::Captured,
                            attempts,
                            retries: self.options.retries,
                            code: *code,
                            conflict: before.conflict,
                            ..Entry::MISSING
                        },
                    ));
                }
                Unit::Failed { code, .. } => {
                    failed.push((unit, *code));
                    self.map.set(
                        unit,
                        unit + 1,
                        Entry {
                            state: State::Failed,
                            attempts,
                            retries: self.options.retries,
                            code: *code,
                            ..Entry::MISSING
                        },
                    );
                    self.dirty = true;
                }
            }
        }
        Ok(failed)
    }

    /// Counts as they will be once pending units are synced.
    fn counts(&self) -> Counts {
        if self.pending.is_empty() {
            return self.map.counts();
        }
        let mut map = self.map.clone();
        for &(unit, entry) in &self.pending {
            map.set(unit, unit + 1, entry);
        }
        map.counts()
    }

    /// Sync the image, then mark the units it covers as captured and save
    /// the map.
    fn flush(&mut self) -> Result<(), Error> {
        if !self.pending.is_empty() {
            self.image.sync_data()?;
            for (unit, entry) in std::mem::take(&mut self.pending) {
                self.map.set(unit, unit + 1, entry);
            }
            self.dirty = true;
        }
        if self.dirty {
            self.map.save(&self.paths.map)?;
            self.dirty = false;
        }
        self.last_sync = Instant::now();
        Ok(())
    }

    /// Flush, write the manifest, and summarize.
    fn finish(&mut self) -> Result<Summary, Error> {
        self.flush()?;
        Manifest::build(&self.map, &self.paths.image, SystemTime::now())?
            .save(&self.paths.manifest)?;
        Ok(Summary {
            paths: self.paths.clone(),
            counts: self.map.counts(),
            complete: self.map.complete(),
        })
    }
}

fn find_device(session: &mut Session, device: u8) -> Result<Descriptor, Error> {
    if !session.has_storage() {
        return Err(Error::NoStorage);
    }
    session
        .storage_list()?
        .into_iter()
        .find(|d| d.device == device)
        .ok_or(Error::NoDevice(device))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::Arc;
    use std::thread::{self, JoinHandle};

    use super::*;
    use crate::files::Root;
    use crate::native::disk::memory::Fault;
    use crate::native::disk::{Disk, MemoryDisk};
    use crate::native::server;
    use crate::native::storage::Geometry;
    use crate::transport::{MemoryTransport, memory_pair};

    /// A server offering `disk` on the far end of the returned transport,
    /// until the transport is dropped.
    fn serve(disk: Arc<MemoryDisk>, max_payload: usize) -> (MemoryTransport, JoinHandle<()>) {
        let (ours, mut theirs) = memory_pair();
        let handle = thread::spawn(move || {
            let tmp = tempfile::tempdir().unwrap();
            let root = Root::new(tmp.path()).unwrap();
            let options = server::Options {
                max_payload,
                disks: vec![disk as Arc<dyn Disk>],
                ..server::Options::default()
            };
            let _ = server::run(&mut theirs, &root, &options, &mut |_| {});
        });
        (ours, handle)
    }

    fn options() -> Options {
        Options {
            sync_every: Duration::ZERO,
            session: session::Options {
                wait: Duration::from_secs(3),
                hello_interval: Duration::from_millis(100),
                quiet: Duration::from_millis(10),
                response_timeout: Some(Duration::from_millis(500)),
                storage_timeout: Duration::from_millis(500),
                ..session::Options::default()
            },
            ..Options::default()
        }
    }

    /// Run a capture of `disk` into `image` and stop the server.
    fn run(
        disk: &Arc<MemoryDisk>,
        image: &Path,
        options: &Options,
        confirm: &mut dyn FnMut(&Question) -> bool,
    ) -> (Result<Summary, Error>, Vec<Event>) {
        let (mut link, server) = serve(disk.clone(), 4090);
        let mut events = Vec::new();
        let result = capture(&mut link, image, options, confirm, &mut |e| {
            events.push(e.clone())
        });
        drop(link);
        server.join().unwrap();
        (result, events)
    }

    fn never(q: &Question) -> bool {
        panic!("unexpected question: {q}")
    }

    fn sorted(mut v: Vec<u32>) -> Vec<u32> {
        v.sort_unstable();
        v
    }

    #[test]
    fn a_disk_is_captured_whole() {
        let tmp = tempfile::tempdir().unwrap();
        let image = tmp.path().join("disk.img");
        let disk = Arc::new(MemoryDisk::new(200, 512, None));
        let (result, events) = run(&disk, &image, &options(), &mut never);
        let summary = result.unwrap();
        assert!(summary.complete);
        assert_eq!(summary.counts.captured, 200);
        assert_eq!(fs::read(&image).unwrap(), disk.image());
        // Every unit read exactly once, the fingerprint included.
        assert_eq!(sorted(disk.reads()), (0..200).collect::<Vec<_>>());
        let Event::Started {
            resumed,
            units_per_read,
            ..
        } = &events[0]
        else {
            panic!("{events:?}")
        };
        assert_eq!((*resumed, *units_per_read), (false, 7));

        let map = Map::load(&summary.paths.map).unwrap();
        assert_eq!(map.fingerprint.len(), 8);
        assert_eq!(map.fingerprint[1].unit, 25);
        let json: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&summary.paths.manifest).unwrap()).unwrap();
        assert_eq!(json["status"], "complete");
        assert_eq!(json["sha256"], manifest::sha256(&disk.image()));
        assert_eq!(json["source"]["bios_drive"], 0x80);
    }

    #[test]
    fn failed_and_suspect_units_are_recorded() {
        let tmp = tempfile::tempdir().unwrap();
        let image = tmp.path().join("disk.img");
        let disk = Arc::new(MemoryDisk::new(100, 512, None));
        disk.fault(
            50,
            Fault::Fail {
                code: 0x10,
                times: 100,
            },
        );
        disk.fault(60, Fault::Suspect(0x11));
        let (result, events) = run(&disk, &image, &options(), &mut never);
        let summary = result.unwrap();
        assert!(!summary.complete);
        assert_eq!((summary.counts.failed, summary.counts.suspect), (1, 1));
        assert!(events.contains(&Event::UnitFailed {
            unit: 50,
            code: 0x10
        }));
        // The failed unit reads as zeros; the rest is the disk.
        let mut expected = disk.image();
        expected[50 * 512..51 * 512].fill(0);
        assert_eq!(fs::read(&image).unwrap(), expected);
        // One attempt and three retries, as asked.
        assert_eq!(disk.reads().iter().filter(|&&u| u == 50).count(), 4);
        let map = Map::load(&summary.paths.map).unwrap();
        assert_eq!(map.get(50).state, State::Failed);
        assert_eq!((map.get(50).attempts, map.get(50).retries), (1, 3));
        assert!(map.get(60).suspect());
        // Unit 50 is a fingerprint unit; failing, it is left out.
        assert!(map.fingerprint.iter().all(|p| p.unit != 50));
        assert_eq!(map.fingerprint.len(), 7);
    }

    #[test]
    fn a_crashed_host_resumes_without_rereading() {
        let tmp = tempfile::tempdir().unwrap();
        let image = tmp.path().join("disk.img");
        let disk = Arc::new(MemoryDisk::new(300, 512, None));
        let crashing = Options {
            halt_after: Some(10),
            ..options()
        };
        let (result, _) = run(&disk, &image, &crashing, &mut never);
        assert!(matches!(result, Err(Error::Halted)));
        let map = Map::load(&Paths::new(&image).map).unwrap();
        let captured: Vec<u32> = map
            .with_state(State::Captured)
            .flat_map(|(start, end)| start..end)
            .collect();
        // The fingerprint and the responses synced before the crash, but
        // not the last response, which was never synced.
        assert!(captured.len() > 8 && captured.len() < 8 + 10 * 7);
        let read_before = disk.reads().len();

        let (result, events) = run(&disk, &image, &options(), &mut never);
        let summary = result.unwrap();
        assert!(summary.complete);
        assert_eq!(fs::read(&image).unwrap(), disk.image());
        assert!(matches!(events[0], Event::Started { resumed: true, .. }));
        let again: Vec<u32> = disk.reads()[read_before..].to_vec();
        let prints: Vec<u32> = map.fingerprint.iter().map(|p| p.unit).collect();
        for unit in &again {
            assert!(
                !captured.contains(unit) || prints.contains(unit),
                "captured unit {unit} was read again"
            );
        }
        // Every unit was read at least once over the two runs.
        let mut all = sorted(disk.reads());
        all.dedup();
        assert_eq!(all, (0..300).collect::<Vec<_>>());
    }

    #[test]
    fn a_lost_response_restarts_the_link() {
        let tmp = tempfile::tempdir().unwrap();
        let image = tmp.path().join("disk.img");
        let disk = Arc::new(MemoryDisk::new(100, 512, None));
        // Longer than every retransmission of a 100 ms storage timeout.
        disk.fault(40, Fault::Slow(Duration::from_millis(1500)));
        let mut opts = options();
        opts.session.response_timeout = Some(Duration::from_millis(100));
        opts.session.storage_timeout = Duration::from_millis(100);
        let (result, events) = run(&disk, &image, &opts, &mut never);
        let summary = result.unwrap();
        assert!(summary.complete);
        assert_eq!(fs::read(&image).unwrap(), disk.image());
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, Event::LinkLost(_)))
                .count(),
            1
        );
    }

    #[test]
    fn a_different_disk_needs_confirmation() {
        let tmp = tempfile::tempdir().unwrap();
        let image = tmp.path().join("disk.img");
        let first = Arc::new(MemoryDisk::new(100, 512, None));
        let crashing = Options {
            halt_after: Some(2),
            ..options()
        };
        assert!(run(&first, &image, &crashing, &mut never).0.is_err());

        let other = Arc::new(MemoryDisk::new(100, 512, None).with_seed(1000));
        let mut asked = Vec::new();
        let (result, _) = run(&other, &image, &options(), &mut |q| {
            asked.push(q.clone());
            false
        });
        assert!(matches!(result, Err(Error::Declined(_))));
        assert_eq!(asked, [Question::FingerprintDiffers { unit: 0 }]);

        // Accepting it continues, and the manifest records the choice.
        let (result, _) = run(&other, &image, &options(), &mut |_| true);
        let summary = result.unwrap();
        let json: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&summary.paths.manifest).unwrap()).unwrap();
        let confirmations = json["confirmations"].as_array().unwrap();
        assert_eq!(confirmations.len(), 1);
        assert!(
            confirmations[0]
                .as_str()
                .unwrap()
                .contains("unit 0 differs")
        );
    }

    #[test]
    fn an_unreadable_fingerprint_unit_is_unverified() {
        let tmp = tempfile::tempdir().unwrap();
        let image = tmp.path().join("disk.img");
        let disk = Arc::new(MemoryDisk::new(100, 512, None));
        let crashing = Options {
            halt_after: Some(2),
            ..options()
        };
        assert!(run(&disk, &image, &crashing, &mut never).0.is_err());
        disk.fault(
            0,
            Fault::Fail {
                code: 0x80,
                times: 100,
            },
        );
        let mut asked = Vec::new();
        let (result, _) = run(&disk, &image, &options(), &mut |q| {
            asked.push(q.clone());
            false
        });
        assert!(matches!(result, Err(Error::Declined(_))));
        assert!(matches!(
            asked[..],
            [Question::FingerprintUnreadable { unit: 0, .. }]
        ));
    }

    #[test]
    fn a_resized_disk_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let image = tmp.path().join("disk.img");
        let crashing = Options {
            halt_after: Some(1),
            ..options()
        };
        let disk = Arc::new(MemoryDisk::new(100, 512, None));
        assert!(run(&disk, &image, &crashing, &mut never).0.is_err());
        let bigger = Arc::new(MemoryDisk::new(101, 512, None));
        let (result, _) = run(&bigger, &image, &options(), &mut never);
        assert!(matches!(result, Err(Error::SourceChanged(_))), "{result:?}");
    }

    #[test]
    fn an_image_without_a_map_is_not_overwritten() {
        let tmp = tempfile::tempdir().unwrap();
        let image = tmp.path().join("disk.img");
        fs::write(&image, b"precious").unwrap();
        let disk = Arc::new(MemoryDisk::new(10, 512, None));
        let (result, _) = run(&disk, &image, &options(), &mut never);
        assert!(matches!(result, Err(Error::ImageExists(_))));
        assert_eq!(fs::read(&image).unwrap(), b"precious");
        assert!(disk.reads().is_empty());
    }

    #[test]
    fn a_damaged_map_leaves_the_image_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let image = tmp.path().join("disk.img");
        let disk = Arc::new(MemoryDisk::new(50, 512, None));
        let crashing = Options {
            halt_after: Some(3),
            ..options()
        };
        assert!(run(&disk, &image, &crashing, &mut never).0.is_err());
        let map = Paths::new(&image).map;
        let text = fs::read(&map).unwrap();
        fs::write(&map, &text[..text.len() / 2]).unwrap();

        let held = fs::read(&image).unwrap();
        let reads = disk.reads().len();

        let (result, _) = run(&disk, &image, &options(), &mut never);
        assert!(
            matches!(result, Err(Error::MapUnusable { .. })),
            "{result:?}"
        );
        assert_eq!(fs::read(&image).unwrap(), held);
        assert_eq!(fs::read(&map).unwrap(), &text[..text.len() / 2]);
        assert_eq!(disk.reads().len(), reads);
    }

    fn recovering(passes: u32) -> Options {
        Options {
            recover: passes,
            ..options()
        }
    }

    #[test]
    fn recovery_rereads_a_failed_unit() {
        let tmp = tempfile::tempdir().unwrap();
        let image = tmp.path().join("disk.img");
        let disk = Arc::new(MemoryDisk::new(100, 512, None));
        // One more failure than the first read's attempt and 3 retries.
        disk.fault(
            70,
            Fault::Fail {
                code: 0x10,
                times: 5,
            },
        );
        let (result, events) = run(&disk, &image, &recovering(2), &mut never);
        let summary = result.unwrap();
        assert!(summary.complete);
        assert_eq!(fs::read(&image).unwrap(), disk.image());
        assert!(events.contains(&Event::Recovering { pass: 1, units: 1 }));
        assert!(events.contains(&Event::Recovered { unit: 70 }));
        // Nothing left for a second pass.
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, Event::Recovering { pass: 2, .. }))
        );
        let map = Map::load(&summary.paths.map).unwrap();
        assert_eq!(map.get(70).state, State::Captured);
        assert_eq!(map.get(70).attempts, 2);
        assert!(!summary.paths.reads.exists());
    }

    #[test]
    fn recovery_leaves_a_dead_unit_failed() {
        let tmp = tempfile::tempdir().unwrap();
        let image = tmp.path().join("disk.img");
        let disk = Arc::new(MemoryDisk::new(30, 512, None));
        disk.fault(
            3,
            Fault::Fail {
                code: 0x10,
                times: 1000,
            },
        );
        let opts = Options {
            retries: 0,
            ..recovering(3)
        };
        let (result, _) = run(&disk, &image, &opts, &mut never);
        let summary = result.unwrap();
        assert!(!summary.complete);
        assert_eq!(summary.counts.failed, 1);
        let map = Map::load(&summary.paths.map).unwrap();
        assert_eq!((map.get(3).attempts, map.get(3).retries), (4, 0));
        assert_eq!(disk.reads().iter().filter(|&&u| u == 3).count(), 4);
    }

    #[test]
    fn a_clean_reread_clears_a_suspect_unit() {
        let tmp = tempfile::tempdir().unwrap();
        let image = tmp.path().join("disk.img");
        let disk = Arc::new(MemoryDisk::new(30, 512, None));
        disk.fault(4, Fault::Suspect(0x11));
        disk.fault(5, Fault::Suspect(0x11));
        let (result, _) = run(&disk, &image, &options(), &mut never);
        assert_eq!(result.unwrap().counts.suspect, 2);
        // Unit 4 now reads with no status; unit 5 still has one.
        disk.clear(4);
        let (result, events) = run(&disk, &image, &recovering(1), &mut never);
        let summary = result.unwrap();
        assert!(events.contains(&Event::Recovering { pass: 1, units: 2 }));
        assert_eq!((summary.counts.suspect, summary.counts.conflicting), (1, 0));
        let map = Map::load(&summary.paths.map).unwrap();
        assert!(!map.get(4).suspect());
        assert!(map.get(5).suspect());
        assert_eq!(map.get(5).attempts, 2);
        assert_eq!(fs::read(&image).unwrap(), disk.image());
    }

    #[test]
    fn a_failed_reread_is_recorded_beside_the_data() {
        let tmp = tempfile::tempdir().unwrap();
        let image = tmp.path().join("disk.img");
        let disk = Arc::new(MemoryDisk::new(30, 512, None));
        disk.fault(8, Fault::Suspect(0x11));
        let (result, _) = run(&disk, &image, &options(), &mut never);
        assert!(result.is_ok());

        // The unit now fails: the image keeps what was read, with its
        // status, and the failure is recorded separately.
        disk.fault(
            8,
            Fault::Fail {
                code: 0x10,
                times: 1000,
            },
        );
        let (result, events) = run(&disk, &image, &recovering(1), &mut never);
        let summary = result.unwrap();
        assert!(events.contains(&Event::RereadFailed {
            unit: 8,
            code: 0x10
        }));
        assert!(summary.complete);
        assert_eq!(fs::read(&image).unwrap(), disk.image());
        let map = Map::load(&summary.paths.map).unwrap();
        let e = map.get(8);
        assert_eq!(e.state, State::Captured);
        assert_eq!((e.code, e.reread_failures, e.reread_code), (0x11, 1, 0x10));
        assert_eq!(e.attempts, 2);

        // A clean reread clears the data's status, but the failure stays
        // on record and the unit stays suspect.
        disk.clear(8);
        let (result, _) = run(&disk, &image, &recovering(1), &mut never);
        let summary = result.unwrap();
        let map = Map::load(&summary.paths.map).unwrap();
        let e = map.get(8);
        assert_eq!((e.code, e.reread_failures, e.reread_code), (0, 1, 0x10));
        assert!(e.suspect());
        let json: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&summary.paths.manifest).unwrap()).unwrap();
        assert_eq!(json["suspect"][0]["start"], 8);
        assert_eq!(json["suspect"][0]["reread_failures"], 1);
        assert_eq!(json["suspect"][0]["reread_code"], 0x10);
    }

    #[test]
    fn rereads_that_disagree_are_kept_and_the_majority_wins() {
        let tmp = tempfile::tempdir().unwrap();
        let image = tmp.path().join("disk.img");
        let disk = Arc::new(MemoryDisk::new(30, 512, None));
        disk.fault(6, Fault::Suspect(0x11));
        let (result, _) = run(&disk, &image, &options(), &mut never);
        assert!(result.is_ok());
        let first = disk.unit(6);
        let other = disk.unit(1000);

        // A reread that differs: a conflict, and the image keeps the
        // first version.
        disk.fault(6, Fault::Data(1000));
        let (result, events) = run(&disk, &image, &recovering(1), &mut never);
        let summary = result.unwrap();
        assert!(events.contains(&Event::Conflict { unit: 6 }));
        assert_eq!(summary.counts.conflicting, 1);
        let image_unit = |n: usize| fs::read(&image).unwrap()[n * 512..(n + 1) * 512].to_vec();
        assert_eq!(image_unit(6), first);
        let log = Log::open(&summary.paths.reads).unwrap();
        let versions: Vec<(&[u8], Option<&str>, u32, u16)> = log
            .reads(6)
            .iter()
            .map(|r| (&r.data[..], r.time.as_deref(), r.attempt, r.code))
            .collect();
        assert_eq!(versions.len(), 2);
        assert_eq!(
            (versions[0].0, versions[0].1, versions[0].2, versions[0].3),
            (&first[..], None, 1, 0x11)
        );
        assert_eq!(
            (versions[1].0, versions[1].2, versions[1].3),
            (&other[..], 2, 0)
        );
        assert!(versions[1].1.is_some());

        // Another pass reads the new version again: two of three, so it
        // goes in the image, and the unit stays conflicting.
        let (result, events) = run(&disk, &image, &recovering(1), &mut never);
        let summary = result.unwrap();
        assert!(!events.iter().any(|e| matches!(e, Event::Conflict { .. })));
        assert_eq!(image_unit(6), other);
        assert_eq!(Log::open(&summary.paths.reads).unwrap().reads(6).len(), 3);
        let map = Map::load(&summary.paths.map).unwrap();
        assert!(map.get(6).conflict);
        assert!(!map.get(6).suspect());
        let json: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&summary.paths.manifest).unwrap()).unwrap();
        assert_eq!(json["conflicting"][0]["start"], 6);
        assert_eq!(json["reads"], "disk.img.reads");
        assert_eq!(json["sha256"], manifest::sha256(&fs::read(&image).unwrap()));
    }

    #[test]
    fn a_recovery_pass_survives_a_lost_link() {
        let tmp = tempfile::tempdir().unwrap();
        let image = tmp.path().join("disk.img");
        let disk = Arc::new(MemoryDisk::new(40, 512, None));
        for unit in [10, 20, 30] {
            disk.fault(
                unit,
                Fault::Fail {
                    code: 0x10,
                    times: 4,
                },
            );
        }
        let (result, _) = run(&disk, &image, &options(), &mut never);
        assert_eq!(result.unwrap().counts.failed, 3);
        disk.fault(20, Fault::Slow(Duration::from_millis(1500)));
        let mut opts = recovering(1);
        opts.session.response_timeout = Some(Duration::from_millis(100));
        opts.session.storage_timeout = Duration::from_millis(100);
        let (result, events) = run(&disk, &image, &opts, &mut never);
        let summary = result.unwrap();
        assert!(summary.complete, "{events:?}");
        assert!(events.iter().any(|e| matches!(e, Event::LinkLost(_))));
        // One pass, carried on after the restart rather than begun again.
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, Event::Recovering { .. }))
                .count(),
            1
        );
        assert_eq!(fs::read(&image).unwrap(), disk.image());
    }

    fn chs(cylinders: u16, heads: u8, sectors: u8) -> Geometry {
        Geometry {
            cylinders,
            heads,
            sectors,
            first_sector: 1,
        }
    }

    #[test]
    fn the_operator_can_give_the_geometry() {
        let tmp = tempfile::tempdir().unwrap();
        let image = tmp.path().join("disk.img");
        let disk = Arc::new(MemoryDisk::new(40, 512, Some(chs(4, 2, 5))));
        let opts = Options {
            geometry: Some(chs(2, 2, 5)),
            ..options()
        };
        let (result, _) = run(&disk, &image, &opts, &mut never);
        let summary = result.unwrap();
        assert!(summary.complete);
        assert_eq!(summary.counts.captured, 20);
        assert_eq!(fs::read(&image).unwrap(), disk.image()[..20 * 512]);
        let map = Map::load(&summary.paths.map).unwrap();
        assert_eq!(map.source.unit_count, 20);
        assert_eq!(map.source.geometry, Some(map_geometry(chs(2, 2, 5))));
        assert_eq!(
            map.source.reported_geometry,
            Some(map_geometry(chs(4, 2, 5)))
        );
        let json: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&summary.paths.manifest).unwrap()).unwrap();
        assert_eq!(json["source"]["geometry"]["cylinders"], 2);
        assert_eq!(json["source"]["reported_geometry"]["cylinders"], 4);
    }

    #[test]
    fn a_resumed_image_keeps_its_geometry() {
        let tmp = tempfile::tempdir().unwrap();
        let image = tmp.path().join("disk.img");
        let disk = Arc::new(MemoryDisk::new(40, 512, Some(chs(4, 2, 5))).chs_only());
        let crashing = Options {
            geometry: Some(chs(3, 2, 5)),
            halt_after: Some(3),
            ..options()
        };
        assert!(run(&disk, &image, &crashing, &mut never).0.is_err());

        // A different geometry is refused; none at all resumes with the
        // one the image was started with.
        let other = Options {
            geometry: Some(chs(4, 2, 5)),
            ..options()
        };
        let (result, _) = run(&disk, &image, &other, &mut never);
        assert!(matches!(result, Err(Error::SourceChanged(_))), "{result:?}");
        let (result, _) = run(&disk, &image, &options(), &mut never);
        let summary = result.unwrap();
        assert!(summary.complete);
        assert_eq!(summary.counts.captured, 30);
        assert_eq!(fs::read(&image).unwrap(), disk.image()[..30 * 512]);
    }

    #[test]
    fn a_geometry_must_fit_the_drive() {
        let tmp = tempfile::tempdir().unwrap();
        let image = tmp.path().join("disk.img");
        let attempt = |disk: MemoryDisk, g: Geometry| {
            let disk = Arc::new(disk);
            let opts = Options {
                geometry: Some(g),
                ..options()
            };
            let (result, _) = run(&disk, &image, &opts, &mut never);
            assert!(disk.reads().is_empty());
            assert!(!image.exists());
            match result {
                Err(Error::BadGeometry(why)) => why,
                other => panic!("{other:?}"),
            }
        };
        let probed = || MemoryDisk::new(40, 512, Some(chs(4, 2, 5)));
        // Without the drive's geometry, the device's is the limit.
        assert!(attempt(probed(), chs(5, 2, 5)).contains("the device's 4/2/5"));
        let drive = || probed().with_drive(chs(8, 2, 9));
        assert!(attempt(drive(), chs(8, 3, 5)).contains("the drive's 8/2/9"));
        assert!(attempt(drive(), chs(8, 2, 10)).contains("larger"));
        assert!(attempt(drive(), chs(0, 2, 5)).contains("at least 1"));
        assert!(attempt(MemoryDisk::new(40, 512, None), chs(4, 2, 5)).contains("CHS"));
    }

    #[test]
    fn a_drive_without_a_medium_geometry_takes_one() {
        let mut d = MemoryDisk::new(0, 512, None)
            .with_drive(chs(80, 2, 18))
            .describe();
        d.addressing = 0;
        let imaged = with_geometry(d.clone(), Some(chs(80, 2, 9))).unwrap();
        assert!(imaged.chs() && !imaged.linear());
        assert_eq!(imaged.unit_count, 1440);
        assert!(with_geometry(d, Some(chs(81, 2, 9))).is_err());
    }

    #[test]
    fn chs_reads_stay_within_a_track() {
        let tmp = tempfile::tempdir().unwrap();
        let image = tmp.path().join("disk.img");
        let geometry = Geometry {
            cylinders: 4,
            heads: 2,
            sectors: 5,
            first_sector: 1,
        };
        let disk = Arc::new(MemoryDisk::new(40, 512, Some(geometry)).chs_only());
        let (result, _) = run(&disk, &image, &options(), &mut never);
        assert!(result.unwrap().complete);
        assert_eq!(fs::read(&image).unwrap(), disk.image());
    }
}
