// SPDX-License-Identifier: MIT
//! `sprocket client`: the interactive native shell on Linux, talking to a
//! SPROCKET server (`SPROCKET SERVER` on DOS, or `sprocket serve`).
//!
//! It has the same prompt, listings, completion and editing keys as
//! terminal mode, with a local side as well: `get` copies remote files
//! into the local directory and `put` copies local files to the remote
//! one. Both refuse to replace an existing file without `-f`, and a batch
//! stops at the first file that fails. Received files are written to a
//! hidden temporary file and renamed into place once complete, with the
//! sender's modification time.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::console::{Completer, Console, Input, SCREEN_WIDTH, columns};
use crate::files::{Entry, Kind};
use crate::native::session::{self, Session};
use crate::native::tree::{self, Crc32};
use crate::shell::{self, Argument, Mode, Tree, ascii, file_name, location, reason, target};
use crate::transport::Transport;

/// Printed after the command list in `help`.
const HELP_FOOTER: &str = "\
With editing on, Tab completes names, Up and Down (or Ctrl-P and Ctrl-N)
recall earlier commands, and Ctrl-D ends the session. Ctrl-C stops a long
listing or a transfer.
";
/// How long the prompt waits for a key before checking again.
const PROMPT_WAIT: Duration = Duration::from_secs(3600);

/// Client settings.
#[derive(Clone, Debug)]
pub struct Options {
    /// Whether line editing starts on.
    pub editing: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self { editing: true }
    }
}

/// Run the shell on `terminal` over an open session, with `local` the
/// starting local directory. Returns when the user quits, after closing
/// the session, or when the link fails.
pub fn run(
    terminal: &mut dyn Transport,
    session: Session,
    local: &Path,
    options: &Options,
) -> anyhow::Result<()> {
    let mut con = Console::new(terminal);
    let mut shell = Client {
        remote: Remote {
            session,
            lost: None,
        },
        rcwd: PathBuf::new(),
        local: Local,
        lcwd: local.strip_prefix("/").unwrap_or(local).to_path_buf(),
        editing: options.editing,
    };
    let result = shell.run(&mut con);
    if let Some(lost) = shell.remote.lost.take() {
        let _ = con.print(&format!("Connection lost: {lost}\n"));
        return Err(anyhow::anyhow!("connection lost: {lost}"));
    }
    match result {
        Ok(()) => {
            shell.remote.session.close()?;
            Ok(())
        }
        // The terminal closed (end of piped input): end the session too.
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
            shell.remote.session.close()?;
            Ok(())
        }
        Err(e) => Err(e.into()),
    }
}

struct Client<'a> {
    remote: Remote<'a>,
    /// Remote directory, relative to the server's root.
    rcwd: PathBuf,
    local: Local,
    /// Local directory, relative to `/`.
    lcwd: PathBuf,
    editing: bool,
}

/// The server's tree, over the native link.
struct Remote<'a> {
    session: Session<'a>,
    /// Set when the link failed; the session cannot be used again.
    lost: Option<String>,
}

impl Remote<'_> {
    /// Turn a protocol error into the filesystem error the shell shows,
    /// noting a link failure.
    fn error(&mut self, e: session::Error) -> io::Error {
        if let session::Error::Client(c) = &e
            && let Some(io) = tree::io_error(c.class, &c.message)
        {
            return io;
        }
        if !matches!(e, session::Error::Client(_) | session::Error::Protocol(_)) {
            self.lost = Some(e.to_string());
        }
        io::Error::other(e.to_string())
    }
}

/// A tree path for the server: components joined by `/`.
fn tree_path(rel: &Path) -> String {
    rel.iter()
        .map(|c| c.to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

impl Tree for Remote<'_> {
    fn lookup(&mut self, path: &Path) -> io::Result<(Kind, PathBuf)> {
        let entry = self
            .session
            .stat(&tree_path(path))
            .map_err(|e| self.error(e))?;
        let kind = if entry.dir {
            Kind::Dir
        } else {
            Kind::File {
                size: entry.size.into(),
            }
        };
        let found: PathBuf = entry.name.split('/').filter(|p| !p.is_empty()).collect();
        Ok((kind, found))
    }

    fn list(&mut self, dir: &Path) -> io::Result<Vec<Entry>> {
        let entries = self
            .session
            .list(&tree_path(dir))
            .map_err(|e| self.error(e))?;
        let mut entries: Vec<Entry> = entries
            .into_iter()
            .map(|e| Entry {
                kind: if e.dir {
                    Kind::Dir
                } else {
                    Kind::File {
                        size: e.size.into(),
                    }
                },
                name: e.name,
            })
            .collect();
        entries.sort_by(|a, b| {
            (a.name.to_lowercase(), &a.name).cmp(&(b.name.to_lowercase(), &b.name))
        });
        Ok(entries)
    }
}

/// This machine's filesystem, with paths relative to `/`. Names are
/// matched exactly, and hidden names are left out of listings.
struct Local;

impl Local {
    fn real(path: &Path) -> PathBuf {
        Path::new("/").join(path)
    }
}

impl Tree for Local {
    fn lookup(&mut self, path: &Path) -> io::Result<(Kind, PathBuf)> {
        let meta = fs::metadata(Self::real(path))?;
        let kind = if meta.is_dir() {
            Kind::Dir
        } else if meta.is_file() {
            Kind::File { size: meta.len() }
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not a regular file",
            ));
        };
        Ok((kind, path.to_path_buf()))
    }

    fn list(&mut self, dir: &Path) -> io::Result<Vec<Entry>> {
        let mut entries = Vec::new();
        for item in fs::read_dir(Self::real(dir))? {
            let item = item?;
            let Ok(name) = item.file_name().into_string() else {
                continue;
            };
            if name.starts_with('.') {
                continue;
            }
            let Ok(meta) = fs::metadata(item.path()) else {
                continue;
            };
            let kind = if meta.is_dir() {
                Kind::Dir
            } else if meta.is_file() {
                Kind::File { size: meta.len() }
            } else {
                continue;
            };
            entries.push(Entry { name, kind });
        }
        entries.sort_by(|a, b| {
            (a.name.to_lowercase(), &a.name).cmp(&(b.name.to_lowercase(), &b.name))
        });
        Ok(entries)
    }

    fn identity(&mut self, dir: &Path) -> Option<PathBuf> {
        Self::real(dir).canonicalize().ok()
    }
}

/// What Tab completes in each command's argument.
fn argument(command: &str) -> Argument {
    match command {
        "cd" => Argument::Remote { dirs_only: true },
        "ls" | "dir" | "get" => Argument::Remote { dirs_only: false },
        "lcd" => Argument::Local { dirs_only: true },
        "lls" | "ldir" | "put" => Argument::Local { dirs_only: false },
        _ => Argument::None,
    }
}

/// Why a transfer did not finish.
enum Failed {
    /// Shown to the user; the session goes on.
    Message(String),
    /// The terminal failed.
    Io(io::Error),
}

impl From<io::Error> for Failed {
    fn from(e: io::Error) -> Self {
        Failed::Io(e)
    }
}

impl Client<'_> {
    fn run(&mut self, con: &mut Console) -> io::Result<()> {
        let info = self.remote.session.client().clone();
        let access = match self.remote.session.writable() {
            (true, _) => "writable",
            (false, _) => "read-only",
        };
        con.print(&format!(
            "Connected to SPROCKET {} on {}, protocol {}.{}, {access}.\n",
            if info.version.is_empty() {
                "(no version)"
            } else {
                &info.version
            },
            info.platform,
            session::MAJOR,
            self.remote.session.minor(),
        ))?;
        if !self.remote.session.has_tree() {
            con.print("This server does not share files (it needs protocol 1.1 and FILE_TREE).\n")?;
            return Ok(());
        }
        con.print("Type help for a list of commands.\n")?;
        loop {
            if self.remote.lost.is_some() {
                return Ok(());
            }
            let prompt = format!("{}> ", location(&self.rcwd));
            con.print(&prompt)?;
            let input = {
                let Client {
                    remote,
                    rcwd,
                    local,
                    lcwd,
                    editing,
                } = self;
                let mut completer = |line: &str| {
                    shell::complete(
                        Mode::Native,
                        line,
                        &argument,
                        (remote, rcwd),
                        Some((local, lcwd)),
                    )
                };
                con.read_line(
                    PROMPT_WAIT,
                    editing.then_some((prompt.as_str(), &mut completer as Completer)),
                )?
            };
            let input = match input {
                Input::Line(line) => line,
                Input::Idle => continue,
                Input::Eof => {
                    con.print("Goodbye.\n")?;
                    return Ok(());
                }
            };
            let (name, arg) = shell::split(&input);
            if name.is_empty() {
                continue;
            }
            let Some(command) = shell::command(name) else {
                con.print(&shell::unknown(name))?;
                continue;
            };
            if command.native.is_none() {
                con.print(&shell::unavailable(name, Mode::Native))?;
                continue;
            }
            match command.name() {
                "quit" => {
                    con.print("Goodbye.\n")?;
                    return Ok(());
                }
                "help" => con.print(&shell::help(Mode::Native, HELP_FOOTER))?,
                "pwd" => con.print(&format!("{}\n", location(&self.rcwd)))?,
                "lpwd" => con.print(&format!("{}\n", location(&self.lcwd)))?,
                "cd" => {
                    let dir = if arg.is_empty() {
                        PathBuf::new()
                    } else {
                        target(&self.rcwd, arg)
                    };
                    if let Some(dir) = change_dir(con, &mut self.remote, &dir, arg)? {
                        self.rcwd = dir;
                    }
                }
                "lcd" => {
                    let dir = local_dir(&self.lcwd, arg);
                    if let Some(dir) = change_dir(con, &mut self.local, &dir, arg)? {
                        self.lcwd = dir;
                    }
                }
                "ls" | "dir" => shell::ls(con, &mut self.remote, &self.rcwd, arg)?,
                "lls" | "ldir" => {
                    let (recursive, path) = shell::flag(arg, "-R");
                    let path = local_arg(path);
                    let arg = if recursive {
                        format!("-R {path}")
                    } else {
                        path
                    };
                    shell::ls(con, &mut self.local, &self.lcwd, &arg)?
                }
                "get" => self.get(con, arg)?,
                "put" => self.put(con, arg)?,
                "set" => match shell::set_editing(arg, &mut self.editing) {
                    Ok(()) => con.print(&format!(
                        "Editing is {}.\n",
                        if self.editing { "on" } else { "off" }
                    ))?,
                    Err(usage) => con.print(usage)?,
                },
                other => unreachable!("{other} is available in native mode"),
            }
        }
    }

    /// `get [-f] FILE`: copy remote files into the local directory.
    fn get(&mut self, con: &mut Console, arg: &str) -> io::Result<()> {
        let (force, arg) = shell::flag(arg, "-f");
        if arg.is_empty() {
            return con.print("Usage: get [-f] FILE\n");
        }
        let files = match shell::expand(&mut self.remote, &self.rcwd, arg) {
            Ok(files) => files,
            Err(message) => return con.print(&message),
        };
        batch(con, "Getting", &files)?;
        for (i, file) in files.iter().enumerate() {
            match self.receive(con, file, force, &counter(i, files.len())) {
                Ok(()) => {}
                Err(Failed::Io(e)) => return Err(e),
                Err(Failed::Message(message)) => {
                    con.print(&message)?;
                    if i + 1 < files.len() {
                        con.print("Remaining files not copied.\n")?;
                    }
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    /// Copy one remote file into the local directory.
    fn receive(
        &mut self,
        con: &mut Console,
        file: &Path,
        force: bool,
        counter: &str,
    ) -> Result<(), Failed> {
        let name = file_name(file);
        let dir = Local::real(&self.lcwd);
        let dest = dir.join(&name);
        if !force && fs::symlink_metadata(&dest).is_ok() {
            return Err(Failed::Message(format!(
                "{counter}{}: File exists; use get -f to replace it\n",
                ascii(&name)
            )));
        }
        let temp = dir.join(format!(".{name}.sprocket"));
        let mut out = File::create(&temp).map_err(|e| local_failure(&name, &e))?;
        let result = self.fetch(con, file, &name, counter, &mut out);
        let done = result.and_then(|mtime| {
            out.sync_all().map_err(|e| local_failure(&name, &e))?;
            if let Some(time) = tree::system_time(mtime) {
                out.set_modified(time)
                    .map_err(|e| local_failure(&name, &e))?;
            }
            fs::rename(&temp, &dest).map_err(|e| local_failure(&name, &e))
        });
        if done.is_err() {
            let _ = fs::remove_file(&temp);
        }
        done
    }

    /// Read a remote file into `out`; returns its modification time.
    fn fetch(
        &mut self,
        con: &mut Console,
        file: &Path,
        name: &str,
        counter: &str,
        out: &mut File,
    ) -> Result<u32, Failed> {
        let path = tree_path(file);
        let chunk = self.remote.session.read_chunk().min(usize::from(u16::MAX)) as u16;
        let start = Instant::now();
        let mut offset = 0u32;
        let mut expected: Option<(u32, u32)> = None;
        let mut progress = Progress::new(con, &format!("{counter}Receiving"), name)?;
        loop {
            if con.interrupted()? {
                return Err(Failed::Message("^C\nTransfer cancelled.\n".to_string()));
            }
            let part = self
                .remote
                .session
                .read(&path, offset, chunk)
                .map_err(|e| {
                    let e = self.remote.error(e);
                    Failed::Message(format!("\n{}: {}\n", ascii(name), reason(&e)))
                })?;
            match expected {
                None => expected = Some((part.size, part.mtime)),
                Some(first) if first != (part.size, part.mtime) => {
                    return Err(Failed::Message(format!(
                        "\n{}: the file changed on the server during the transfer\n",
                        ascii(name)
                    )));
                }
                Some(_) => {}
            }
            out.write_all(&part.data)
                .map_err(|e| local_failure(name, &e))?;
            offset += part.data.len() as u32;
            progress.update(con, u64::from(offset), u64::from(part.size))?;
            if offset >= part.size {
                if offset > part.size {
                    return Err(Failed::Message(format!(
                        "\n{}: the server sent more than the file's length\n",
                        ascii(name)
                    )));
                }
                progress.done(con, u64::from(offset), start.elapsed())?;
                return Ok(part.mtime);
            }
            if part.data.is_empty() {
                return Err(Failed::Message(format!(
                    "\n{}: the file ended early on the server\n",
                    ascii(name)
                )));
            }
        }
    }

    /// `put [-f] FILE`: copy local files to the remote directory.
    fn put(&mut self, con: &mut Console, arg: &str) -> io::Result<()> {
        let (force, arg) = shell::flag(arg, "-f");
        if arg.is_empty() {
            return con.print("Usage: put [-f] FILE\n");
        }
        match self.remote.session.writable() {
            (false, _) => return con.print("put: the server is read-only\n"),
            (true, false) if force => {
                return con.print("put: the server does not replace files\n");
            }
            _ => {}
        }
        let files = match shell::expand(&mut self.local, &self.lcwd, &local_arg(arg)) {
            Ok(files) => files,
            Err(message) => return con.print(&message),
        };
        batch(con, "Putting", &files)?;
        for (i, file) in files.iter().enumerate() {
            match self.send(con, file, force, &counter(i, files.len())) {
                Ok(()) => {}
                Err(Failed::Io(e)) => return Err(e),
                Err(Failed::Message(message)) => {
                    con.print(&message)?;
                    if i + 1 < files.len() {
                        con.print("Remaining files not copied.\n")?;
                    }
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    /// Copy one local file to the remote directory.
    fn send(
        &mut self,
        con: &mut Console,
        file: &Path,
        force: bool,
        counter: &str,
    ) -> Result<(), Failed> {
        let name = file_name(file);
        let mut input = File::open(Local::real(file)).map_err(|e| local_failure(&name, &e))?;
        let meta = input.metadata().map_err(|e| local_failure(&name, &e))?;
        let Ok(size) = u32::try_from(meta.len()) else {
            return Err(Failed::Message(format!(
                "{}: larger than 4 GB, which the protocol cannot send\n",
                ascii(&name)
            )));
        };
        let dest = tree_path(&self.rcwd.join(&name));
        let mtime = tree::dos_time(meta.modified().ok());
        let remote_failure = |remote: &mut Remote, e| {
            let e = remote.error(e);
            let hint = if e.kind() == io::ErrorKind::AlreadyExists {
                "; use put -f to replace it"
            } else {
                ""
            };
            Failed::Message(format!("{counter}{}: {}{hint}\n", ascii(&name), reason(&e)))
        };
        let found = self
            .remote
            .session
            .begin(&dest, size, force, mtime)
            .map_err(|e| remote_failure(&mut self.remote, e))?;
        if found & 0b010 != 0 && found & 0b001 == 0 {
            con.print(&format!(
                "Note: an earlier copy of {} was interrupted; the old version is in its .BAK file.\n",
                ascii(&name)
            ))?;
        }
        let start = Instant::now();
        let mut progress = Progress::new(con, &format!("{counter}Sending"), &name)?;
        let mut buf = vec![0u8; self.remote.session.write_chunk()];
        let mut crc = Crc32::default();
        let mut offset = 0u32;
        while offset < size {
            if con.interrupted()? {
                let _ = self.remote.session.abort();
                return Err(Failed::Message("^C\nTransfer cancelled.\n".to_string()));
            }
            let want = buf.len().min((size - offset) as usize);
            let got =
                read_up_to(&mut input, &mut buf[..want]).map_err(|e| local_failure(&name, &e))?;
            if got == 0 {
                let _ = self.remote.session.abort();
                return Err(Failed::Message(format!(
                    "\n{}: the file got shorter while it was being sent\n",
                    ascii(&name)
                )));
            }
            self.remote
                .session
                .data(offset, &buf[..got])
                .map_err(|e| remote_failure(&mut self.remote, e))?;
            crc.update(&buf[..got]);
            offset += got as u32;
            progress.update(con, u64::from(offset), u64::from(size))?;
        }
        let theirs = self
            .remote
            .session
            .commit(size, crc.value())
            .map_err(|e| remote_failure(&mut self.remote, e))?;
        if theirs != crc.value() {
            return Err(Failed::Message(format!(
                "\n{}: the server's CRC-32 differs; it may not have the file\n",
                ascii(&name)
            )));
        }
        progress.done(con, u64::from(size), start.elapsed())?;
        Ok(())
    }
}

/// Fill `buf` from `input` unless the file ends first.
fn read_up_to(input: &mut File, buf: &mut [u8]) -> io::Result<usize> {
    let mut got = 0;
    while got < buf.len() {
        match input.read(&mut buf[got..])? {
            0 => break,
            n => got += n,
        }
    }
    Ok(got)
}

/// `(2 of 5) ` in a batch of several files, otherwise nothing.
fn counter(i: usize, n: usize) -> String {
    if n > 1 {
        format!("({} of {n}) ", i + 1)
    } else {
        String::new()
    }
}

/// Announce a batch of several files.
fn batch(con: &mut Console, verb: &str, files: &[PathBuf]) -> io::Result<()> {
    if files.len() < 2 {
        return Ok(());
    }
    let names: Vec<String> = files.iter().map(|f| ascii(&file_name(f))).collect();
    con.print(&format!(
        "{verb} {} files:\n{}",
        names.len(),
        columns(&names, SCREEN_WIDTH)
    ))
}

fn local_failure(name: &str, e: &io::Error) -> Failed {
    Failed::Message(format!("\n{}: {} (here)\n", ascii(name), reason(e)))
}

/// A typed local directory for `lcd`: `~` is the home directory, and no
/// argument goes there too.
fn local_dir(cwd: &Path, arg: &str) -> PathBuf {
    target(cwd, &local_arg(if arg.is_empty() { "~" } else { arg }))
}

/// Expand a leading `~` to the home directory.
fn local_arg(arg: &str) -> String {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/".to_string());
    match arg.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => format!("{home}{rest}"),
        _ => arg.to_string(),
    }
}

/// `cd` or `lcd`: the directory found, or `None` after reporting why not.
fn change_dir(
    con: &mut Console,
    tree: &mut dyn Tree,
    dir: &Path,
    arg: &str,
) -> io::Result<Option<PathBuf>> {
    match tree.lookup(dir) {
        Ok((Kind::Dir, dir)) => Ok(Some(dir)),
        Ok((Kind::File { .. }, _)) => {
            con.print(&format!("{}: Not a directory\n", ascii(arg)))?;
            Ok(None)
        }
        Err(e) => {
            con.print(&format!("{}: {}\n", ascii(arg), reason(&e)))?;
            Ok(None)
        }
    }
}

/// A transfer's progress, shown as a percentage on one line.
struct Progress {
    label: String,
    shown: Option<u64>,
}

impl Progress {
    fn new(con: &mut Console, verb: &str, name: &str) -> io::Result<Self> {
        let label = format!("{verb} {}", ascii(name));
        con.print(&format!("{label}: "))?;
        Ok(Self { label, shown: None })
    }

    fn update(&mut self, con: &mut Console, done: u64, total: u64) -> io::Result<()> {
        let percent = (done * 100).checked_div(total).unwrap_or(100);
        if self.shown != Some(percent) && done < total {
            self.shown = Some(percent);
            con.print(&format!("\r{}: {percent}%", self.label))?;
        }
        Ok(())
    }

    fn done(&self, con: &mut Console, bytes: u64, elapsed: Duration) -> io::Result<()> {
        let rate = (bytes as f64 / elapsed.as_secs_f64().max(0.001)) as u64;
        let unit = if bytes == 1 { "byte" } else { "bytes" };
        con.print(&format!(
            "\r{}: {bytes} {unit} in {:.1}s ({}/s)\n",
            self.label,
            elapsed.as_secs_f64(),
            shell::human(rate)
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::files::Root;
    use crate::native::server;
    use crate::transport::{MemoryTransport, memory_pair};
    use std::thread::{self, JoinHandle};

    /// A user at the client's terminal, with the server and its tree.
    struct User {
        t: MemoryTransport,
        client: JoinHandle<anyhow::Result<()>>,
        server: JoinHandle<()>,
        remote: tempfile::TempDir,
        local: tempfile::TempDir,
    }

    impl User {
        fn send(&mut self, keys: &str) {
            self.t.write_all(keys.as_bytes()).unwrap();
        }

        /// Read output until `needle` appears; returns everything read.
        fn expect(&mut self, needle: &str) -> String {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut out = Vec::new();
            while !String::from_utf8_lossy(&out).contains(needle) {
                assert!(
                    Instant::now() < deadline,
                    "no {needle:?} in {:?}",
                    String::from_utf8_lossy(&out)
                );
                if let Some(b) = self.t.read_byte(Duration::from_millis(50)).unwrap() {
                    out.push(b);
                }
            }
            String::from_utf8(out).unwrap()
        }

        /// Quit and wait for both ends.
        fn quit(mut self) -> (tempfile::TempDir, tempfile::TempDir) {
            self.send("quit\r");
            self.expect("Goodbye.");
            self.client.join().unwrap().unwrap();
            self.server.join().unwrap();
            (self.remote, self.local)
        }
    }

    /// remote/{data.bin (3000 bytes), docs/{readme.txt}, games/{a.dat, b.dat}}
    /// local/{up.txt, two/{x.txt, y.txt}}
    fn start(writable: bool) -> User {
        let remote = tempfile::tempdir().unwrap();
        let r = remote.path();
        fs::create_dir(r.join("docs")).unwrap();
        fs::write(r.join("docs/readme.txt"), b"hello").unwrap();
        fs::write(r.join("data.bin"), data()).unwrap();
        fs::create_dir(r.join("games")).unwrap();
        fs::write(r.join("games/a.dat"), b"aaa").unwrap();
        fs::write(r.join("games/b.dat"), b"bbb").unwrap();
        let local = tempfile::tempdir().unwrap();
        let l = local.path();
        fs::write(l.join("up.txt"), b"uploaded").unwrap();
        fs::create_dir(l.join("two")).unwrap();
        fs::write(l.join("two/x.txt"), b"x").unwrap();
        fs::write(l.join("two/y.txt"), b"y").unwrap();

        let root = Root::new(r).unwrap();
        let (mut link, mut far) = memory_pair();
        let server = thread::spawn(move || {
            let options = server::Options {
                writable,
                max_payload: 512,
                ..server::Options::default()
            };
            let _ = server::run(&mut far, &root, &options, &mut |_| {});
        });
        let (t, mut term) = memory_pair();
        let dir = l.to_path_buf();
        let client = thread::spawn(move || {
            let options = session::Options {
                wait: Duration::from_secs(2),
                hello_interval: Duration::from_millis(100),
                quiet: Duration::from_millis(10),
                response_timeout: Some(Duration::from_millis(500)),
                ..session::Options::default()
            };
            let session = Session::connect(&mut link, &options, |_| {})?;
            run(&mut term, session, &dir, &Options::default())
        });
        let mut user = User {
            t,
            client,
            server,
            remote,
            local,
        };
        user.expect("/> ");
        user
    }

    fn data() -> Vec<u8> {
        (0..3000u32).map(|i| (i * 31 % 251) as u8).collect()
    }

    #[test]
    fn browse_the_remote_tree() {
        let mut u = start(false);
        u.send("ls\r");
        let out = u.expect("\r\n/> ");
        assert!(
            out.contains("\r\ndata.bin   2.9kB\r\ndocs/\r\ngames/\r\n"),
            "{out}"
        );
        u.send("ls -R games\r");
        u.expect("a.dat      3B\r\nb.dat      3B\r\n/> ");
        u.send("cd DOCS\rpwd\r");
        u.expect("/docs\r\n/docs> ");
        u.send("cd nope\r");
        u.expect("nope: No such file or directory\r\n/docs> ");
        u.send("cd ../data.bin\r");
        u.expect("../data.bin: Not a directory\r\n/docs> ");
        u.send("cd\r");
        u.expect("\r\n/> ");
        u.send("protocol z\rfrob\r");
        u.expect("protocol: not available in native mode\r\n/> ");
        u.expect("Unknown command: frob\r\nType help for a list of commands.\r\n/> ");
        u.send("help\r");
        let out = u.expect("\r\n/> ");
        assert!(out.contains("  put [-f] FILE"), "{out}");
        assert!(
            out.contains("Not available in native mode: protocol."),
            "{out}"
        );
        u.quit();
    }

    #[test]
    fn get_copies_files_and_refuses_to_replace_them() {
        let mut u = start(false);
        u.send("get DATA.BIN\r");
        u.expect("Receiving data.bin: 3000 bytes in");
        u.send("get data.bin\r");
        u.expect("data.bin: File exists; use get -f to replace it\r\n/> ");
        fs::write(u.local.path().join("data.bin"), b"old").unwrap();
        u.send("get -f data.bin\r");
        u.expect("Receiving data.bin: 3000 bytes in");
        u.send("get docs\r");
        u.expect("docs: Is a directory\r\n/> ");
        u.send("get games/*.DAT\r");
        u.expect("Getting 2 files:\r\na.dat  b.dat\r\n");
        u.expect("\r(1 of 2) Receiving a.dat: 3 bytes");
        u.expect("\r(2 of 2) Receiving b.dat: 3 bytes");
        u.send("get games/*.dat\r");
        u.expect("(1 of 2) a.dat: File exists; use get -f to replace it\r\nRemaining files not copied.\r\n/> ");
        u.send("get *.zip\r");
        u.expect("*.zip: No match\r\n/> ");
        let (remote, local) = u.quit();
        assert_eq!(fs::read(local.path().join("data.bin")).unwrap(), data());
        assert_eq!(fs::read(local.path().join("b.dat")).unwrap(), b"bbb");
        // The modification time came across, to DOS's two seconds.
        let time = |p: &Path| fs::metadata(p).unwrap().modified().unwrap();
        let theirs = time(&remote.path().join("data.bin"));
        let ours = time(&local.path().join("data.bin"));
        assert!(theirs.duration_since(ours).unwrap() < Duration::from_secs(2));
        let names: Vec<_> = fs::read_dir(local.path()).unwrap().collect();
        assert_eq!(names.len(), 5, "no temporary files left");
    }

    #[test]
    fn put_needs_a_writable_server() {
        let mut u = start(false);
        u.send("put up.txt\r");
        u.expect("put: the server is read-only\r\n/> ");
        u.quit();
    }

    #[test]
    fn put_copies_files_and_refuses_to_replace_them() {
        let mut u = start(true);
        u.send("cd docs\rput up.txt\r");
        u.expect("Sending up.txt: 8 bytes in");
        u.send("put UP.TXT\r");
        u.expect("UP.TXT: No such file or directory\r\n/docs> ");
        u.send("put up.txt\r");
        u.expect("up.txt: File exists; use put -f to replace it\r\n/docs> ");
        u.send("put -f up.txt\rput two/*.txt\r");
        u.expect("Sending up.txt: 8 bytes in");
        u.expect("\r(2 of 2) Sending y.txt: 1 byte in");
        u.send("ls\r");
        u.expect("readme.txt      5B\r\nup.txt          8B\r\nx.txt           1B\r\ny.txt           1B\r\n/docs> ");
        let (remote, _local) = u.quit();
        assert_eq!(
            fs::read(remote.path().join("docs/up.txt")).unwrap(),
            b"uploaded"
        );
        assert_eq!(fs::read(remote.path().join("docs/y.txt")).unwrap(), b"y");
    }

    #[test]
    fn local_commands() {
        let mut u = start(false);
        let here = location(u.local.path().strip_prefix("/").unwrap());
        u.send("lpwd\r");
        u.expect(&format!("{here}\r\n/> "));
        u.send("lls\r");
        u.expect("two/\r\nup.txt      8B\r\n/> ");
        u.send("lcd two\rlpwd\rldir\r");
        u.expect(&format!("{here}/two\r\n/> "));
        u.expect("x.txt      1B\r\ny.txt      1B\r\n/> ");
        u.send("lcd nope\r");
        u.expect("nope: No such file or directory\r\n/> ");
        u.send("get docs/readme.txt\r");
        u.expect("Receiving readme.txt: 5 bytes");
        let (_remote, local) = u.quit();
        assert_eq!(
            fs::read(local.path().join("two/readme.txt")).unwrap(),
            b"hello"
        );
    }

    #[test]
    fn tab_completes_both_sides() {
        let mut u = start(false);
        u.send("get da\t");
        u.expect("ta.bin ");
        u.send(&"\x08".repeat("get data.bin ".len()));
        u.send("put u\t");
        u.expect("p.txt ");
        u.send(&"\x08".repeat("put up.txt ".len()));
        u.send("lcd t\t");
        u.expect("wo/");
        u.send(&"\x08".repeat("lcd two/".len()));
        u.send("p\t");
        u.expect("\r\nput  pwd\r\n/> p");
        u.send("\x08");
        u.quit();
    }

    #[test]
    fn ctrl_d_ends_the_session() {
        let mut u = start(false);
        u.send("\x04");
        u.expect("Goodbye.");
        u.client.join().unwrap().unwrap();
        u.server.join().unwrap();
    }
}
