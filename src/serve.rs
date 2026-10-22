// SPDX-License-Identifier: MIT
//! Interactive terminal file server.
//!
//! A vintage machine running an ordinary terminal program browses a
//! read-only file tree with ftp-style commands (`ls`, `cd`, `get`) and
//! downloads files by ZMODEM (auto-download where the terminal supports it)
//! or XMODEM. Output is plain ASCII with CR LF line endings and no ANSI
//! sequences. Tab completes command and file names.

use std::collections::{HashSet, VecDeque};
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::files::{Kind, Root};
use crate::transport::Transport;
use crate::{xmodem, zmodem};

/// Names longer than this push their size out of the listing's column.
const NAME_COLUMN: usize = 40;
/// Screen width used to lay out completion choices.
const SCREEN_WIDTH: usize = 80;
/// Longest accepted input line.
const MAX_INPUT: usize = 200;
/// Commands remembered for recall with the arrow keys.
const HISTORY: usize = 50;
/// How long to wait for a key at the "press ENTER" banner before checking again.
const BANNER_WAIT: Duration = Duration::from_secs(3600);
/// Quiet period used to discard protocol leftovers after a transfer.
const SETTLE: Duration = Duration::from_millis(500);
/// Quiet period after ZMODEM. Windows 95 HyperTerminal repeats its ZFIN about
/// 730 ms after the session ends; a shorter wait lets it reach the prompt as input.
const ZMODEM_SETTLE: Duration = Duration::from_millis(1500);
/// Command names offered by Tab completion.
const COMMANDS: &[&str] = &[
    "bye", "cd", "dir", "exit", "get", "help", "ls", "protocol", "pwd", "quit",
];
const HELP: &str = "\
Commands:
  ls [-R] [PATH]   list a directory; -R lists everything below it
  dir [-R] [PATH]  same as ls
  cd [DIR]         change directory; with no DIR, go to the top
  get FILE         download a file; FILE may use * and ? wildcards,
                   e.g. get *.exe
  protocol [NAME]  show or set the download protocol: zmodem (z),
                   xmodem (x) or xmodem-1k (x-1k); zmodem by default
  pwd              show the current directory
  quit             end the session (or Ctrl-D)
Tab completes names. Up and Down (or Ctrl-P and Ctrl-N) recall earlier
commands. Ctrl-C stops a long listing.
";

/// Server settings.
#[derive(Clone, Debug)]
pub struct Options {
    /// End a session after this long without input.
    pub idle_timeout: Duration,
    /// How long a transfer waits for the receiver to start.
    pub start_timeout: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            idle_timeout: Duration::from_secs(600),
            start_timeout: Duration::from_secs(60),
        }
    }
}

/// Serve sessions on `transport` until it fails (e.g. the TCP peer
/// disconnects). Each session starts when the user presses ENTER.
pub fn run(
    transport: &mut dyn Transport,
    root: &Root,
    options: &Options,
    log: &mut dyn FnMut(&str),
) -> io::Result<()> {
    let mut con = Console::new(transport);
    loop {
        con.print("\nSPROCKET file server. Press ENTER to start.\n")?;
        while !matches!(con.t.read_byte(BANNER_WAIT)?, Some(b'\r' | b'\n')) {}
        log("session started");
        session(&mut con, root, options, log)?;
        log("session ended");
    }
}

/// Computes the [`Completion`] for an input line.
type Completer<'a> = &'a dyn Fn(&str) -> Completion;

/// What Tab does to the input line.
struct Completion {
    /// The line after completion; unchanged if nothing could be added.
    line: String,
    /// Possible names to show when the line could not be extended.
    choices: Vec<String>,
}

/// Line-oriented access to a dumb terminal.
struct Console<'a> {
    t: &'a mut dyn Transport,
    /// The previous byte was CR, so a following LF is part of the same newline.
    after_cr: bool,
    /// Keys typed while output was being sent, kept for the next prompt.
    typeahead: VecDeque<u8>,
    /// Earlier input lines, oldest first, recalled with Up and Down.
    history: Vec<String>,
}

/// How [`Console::read_line`] ended.
enum Input {
    Line(String),
    /// Ctrl-D on an empty line.
    Eof,
    /// No key arrived within the idle timeout.
    Idle,
}

/// Progress through an ANSI escape sequence sent by a cursor key.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Escape {
    None,
    /// Received ESC.
    Start,
    /// Received `ESC [` or `ESC O`; waiting for the final byte.
    Body,
}

impl<'a> Console<'a> {
    fn new(t: &'a mut dyn Transport) -> Self {
        Self {
            t,
            after_cr: false,
            typeahead: VecDeque::new(),
            history: Vec::new(),
        }
    }

    fn print(&mut self, text: &str) -> io::Result<()> {
        self.t.write_all(text.replace('\n', "\r\n").as_bytes())
    }

    /// Read a line with echo and backspace editing. Tab calls `complete`, if
    /// given with the prompt to redraw after listing choices. Up and Down
    /// (or Ctrl-P and Ctrl-N) step through the history, and a non-empty line
    /// is added to it. Ctrl-D on an empty line ends input, as in a shell.
    fn read_line(
        &mut self,
        idle: Duration,
        complete: Option<(&str, Completer)>,
    ) -> io::Result<Input> {
        let mut line = String::new();
        // Position in the history; `history.len()` is the line being typed,
        // which is kept in `draft` while an older one is shown.
        let mut recalled = self.history.len();
        let mut draft = String::new();
        let mut escape = Escape::None;
        loop {
            let byte = match self.typeahead.pop_front() {
                Some(byte) => byte,
                None => match self.t.read_byte(idle)? {
                    Some(byte) => byte,
                    None => return Ok(Input::Idle),
                },
            };
            let after_cr = std::mem::replace(&mut self.after_cr, byte == b'\r');
            // Cursor keys arrive as ESC [ A or ESC O A (Up), B (Down), and so
            // on; only Up and Down do anything.
            let key = match (escape, byte) {
                (Escape::None, 0x1b) => {
                    escape = Escape::Start;
                    continue;
                }
                (Escape::Start, b'[' | b'O') => {
                    escape = Escape::Body;
                    continue;
                }
                (Escape::Body, 0x20..=0x3f) => continue,
                (Escape::Body, _) => {
                    escape = Escape::None;
                    match byte {
                        b'A' => 0x10,
                        b'B' => 0x0e,
                        _ => continue,
                    }
                }
                (_, byte) => {
                    escape = Escape::None;
                    byte
                }
            };
            match key {
                b'\n' if after_cr => {}
                b'\r' | b'\n' => {
                    self.print("\n")?;
                    let entry = line.trim();
                    if !entry.is_empty() && self.history.last().map(String::as_str) != Some(entry) {
                        if self.history.len() == HISTORY {
                            self.history.remove(0);
                        }
                        self.history.push(entry.to_string());
                    }
                    return Ok(Input::Line(line));
                }
                0x04 if line.is_empty() => {
                    self.print("\n")?;
                    return Ok(Input::Eof);
                }
                // Ctrl-P: previous line; Ctrl-N: next line.
                0x10 | 0x0e => {
                    let next = match key {
                        0x10 if recalled > 0 => recalled - 1,
                        0x0e if recalled < self.history.len() => recalled + 1,
                        _ => {
                            self.t.write_all(b"\x07")?;
                            continue;
                        }
                    };
                    if recalled == self.history.len() {
                        draft = line.clone();
                    }
                    recalled = next;
                    let shown = match self.history.get(recalled) {
                        Some(old) => old.clone(),
                        None => std::mem::take(&mut draft),
                    };
                    self.retype(&line, &shown)?;
                    line = shown;
                }
                0x08 | 0x7f => {
                    if line.pop().is_some() {
                        self.t.write_all(b"\x08 \x08")?;
                    }
                }
                b'\t' => {
                    let Some((prompt, complete)) = complete else {
                        continue;
                    };
                    let done = complete(&line);
                    if done.line != line && done.line.len() <= MAX_INPUT {
                        self.retype(&line, &done.line)?;
                        line = done.line;
                    } else if done.choices.is_empty() {
                        self.t.write_all(b"\x07")?;
                    } else {
                        let choices = columns(&done.choices, SCREEN_WIDTH);
                        self.print(&format!("\n{choices}{prompt}{line}"))?;
                    }
                }
                0x20..=0x7e if line.len() < MAX_INPUT => {
                    line.push(byte as char);
                    self.t.write_all(&[byte])?;
                }
                _ => {}
            }
        }
    }

    /// Replace `old`, already echoed, with `new` on screen.
    fn retype(&mut self, old: &str, new: &str) -> io::Result<()> {
        let same = old
            .bytes()
            .zip(new.bytes())
            .take_while(|(a, b)| a == b)
            .count();
        let mut out = b"\x08 \x08".repeat(old.len() - same);
        out.extend_from_slice(&new.as_bytes()[same..]);
        self.t.write_all(&out)
    }

    /// Whether the user pressed Ctrl-C or Ctrl-X. Other keys are kept as
    /// typeahead for the next prompt.
    fn interrupted(&mut self) -> io::Result<bool> {
        while let Some(byte) = self.t.read_byte(Duration::ZERO)? {
            if matches!(byte, 0x03 | 0x18) {
                self.typeahead.clear();
                return Ok(true);
            }
            self.typeahead.push_back(byte);
        }
        Ok(false)
    }

    /// Discard protocol leftovers so they are not read as commands.
    fn settle(&mut self, quiet: Duration) -> io::Result<()> {
        self.t.drain(quiet)?;
        self.typeahead.clear();
        self.after_cr = false;
        Ok(())
    }
}

fn session(
    con: &mut Console,
    root: &Root,
    options: &Options,
    log: &mut dyn FnMut(&str),
) -> io::Result<()> {
    let mut cwd = PathBuf::new();
    let mut protocol = Protocol::Zmodem;
    con.history.clear();
    con.print("\nType help for a list of commands.\n")?;
    loop {
        let prompt = format!("{}> ", location(&cwd));
        con.print(&prompt)?;
        let completer = |line: &str| complete(root, &cwd, line);
        let input = match con.read_line(options.idle_timeout, Some((&prompt, &completer)))? {
            Input::Line(line) => line,
            Input::Eof => {
                con.print("Goodbye.\n")?;
                return Ok(());
            }
            Input::Idle => {
                con.print("\nIdle timeout.\n")?;
                return Ok(());
            }
        };
        let input = input.trim();
        let (command, arg) = match input.split_once(' ') {
            Some((command, arg)) => (command, arg.trim()),
            None => (input, ""),
        };
        match command.to_ascii_lowercase().as_str() {
            "" => {}
            "quit" | "bye" | "exit" => {
                con.print("Goodbye.\n")?;
                return Ok(());
            }
            "help" | "?" => con.print(HELP)?,
            "pwd" => con.print(&format!("{}\n", location(&cwd)))?,
            "cd" => {
                let dir = if arg.is_empty() {
                    PathBuf::new()
                } else {
                    target(&cwd, arg)
                };
                match lookup(root, &dir) {
                    Ok((Kind::Dir, dir)) => cwd = dir,
                    Ok((Kind::File { .. }, _)) => {
                        con.print(&format!("{}: Not a directory\n", ascii(arg)))?
                    }
                    Err(e) => con.print(&format!("{}: {}\n", ascii(arg), reason(&e)))?,
                }
            }
            "ls" | "dir" => ls(con, root, &cwd, arg)?,
            "get" if arg.is_empty() => con.print("Usage: get FILE\n")?,
            "get" => get(con, root, &cwd, arg, protocol, options, log)?,
            "protocol" if arg.is_empty() => {
                con.print(&format!("Protocol: {}\n", protocol.name()))?
            }
            "protocol" => match Protocol::parse(arg) {
                Some(choice) => {
                    protocol = choice;
                    con.print(&format!("Protocol set to {}.\n", protocol.name()))?
                }
                None => con.print(
                    "Unknown protocol. Choose zmodem (z), xmodem (x) or xmodem-1k (x-1k).\n",
                )?,
            },
            _ => con.print("Unknown command. Type help for a list of commands.\n")?,
        }
    }
}

/// `get FILE`, where the last part of FILE may hold `*` and `?` wildcards.
fn get(
    con: &mut Console,
    root: &Root,
    cwd: &Path,
    arg: &str,
    protocol: Protocol,
    options: &Options,
    log: &mut dyn FnMut(&str),
) -> io::Result<()> {
    let path = target(cwd, arg);
    let pattern = file_name(&path);
    if !pattern.contains(['*', '?']) {
        return match lookup(root, &path) {
            Ok((Kind::File { .. }, path)) => transfer(con, root, &[path], protocol, options, log),
            Ok((Kind::Dir, _)) => con.print(&format!("{}: Is a directory\n", ascii(arg))),
            Err(e) => con.print(&format!("{}: {}\n", ascii(arg), reason(&e))),
        };
    }
    let dir = match root.find(path.parent().unwrap_or(Path::new(""))) {
        Ok(dir) => dir,
        Err(e) => return con.print(&format!("{}: {}\n", ascii(arg), reason(&e))),
    };
    let entries = match root.list(&dir) {
        Ok(entries) => entries,
        Err(e) => return con.print(&format!("{}: {}\n", ascii(arg), reason(&e))),
    };
    let files: Vec<PathBuf> = entries
        .into_iter()
        .filter(|e| matches!(e.kind, Kind::File { .. }) && wildcard(&pattern, &e.name))
        .map(|e| dir.join(e.name))
        .collect();
    if files.is_empty() {
        return con.print(&format!("{}: No match\n", ascii(arg)));
    }
    transfer(con, root, &files, protocol, options, log)
}

/// Match `name` against `pattern`, where `*` is any run of characters and
/// `?` is any one character. Case is ignored, as DOS users expect.
fn wildcard(pattern: &str, name: &str) -> bool {
    let pattern: Vec<char> = pattern.to_lowercase().chars().collect();
    let name: Vec<char> = name.to_lowercase().chars().collect();
    let (mut p, mut n) = (0, 0);
    // Where to resume after the most recent `*`: (pattern, name) positions.
    let mut star = None;
    while n < name.len() {
        match pattern.get(p) {
            Some('*') => {
                star = Some((p + 1, n));
                p += 1;
            }
            Some(&c) if c == '?' || c == name[n] => {
                p += 1;
                n += 1;
            }
            _ => match star {
                Some((sp, sn)) => {
                    star = Some((sp, sn + 1));
                    p = sp;
                    n = sn + 1;
                }
                None => return false,
            },
        }
    }
    pattern[p..].iter().all(|&c| c == '*')
}

/// The last component of `path`, or empty for the top.
fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// `ls [-R] [PATH]` (or `dir`): one entry per line, directories marked with `/`.
fn ls(con: &mut Console, root: &Root, cwd: &Path, arg: &str) -> io::Result<()> {
    let (recursive, arg) = match arg.strip_prefix("-R") {
        Some(rest) if rest.is_empty() || rest.starts_with(' ') => (true, rest.trim()),
        _ => (false, arg),
    };
    let mut lines = Vec::new();
    match lookup(root, &target(cwd, arg)) {
        Ok((Kind::File { size }, _)) => lines.push((arg.to_string(), Some(size))),
        Ok((Kind::Dir, path)) => {
            let mut seen = HashSet::from_iter(root.resolve(&path).ok());
            if let Err(e) = walk(root, &path, "", recursive, &mut seen, &mut lines) {
                return con.print(&format!("{}: {}\n", ascii(arg), reason(&e)));
            }
        }
        Err(e) => return con.print(&format!("{}: {}\n", ascii(arg), reason(&e))),
    }
    let width = lines
        .iter()
        .filter(|(_, size)| size.is_some())
        .map(|(name, _)| name.len())
        .max()
        .unwrap_or(0)
        .min(NAME_COLUMN);
    for (name, size) in lines {
        if con.interrupted()? {
            return con.print("^C\n");
        }
        con.print(&match size {
            Some(size) => format!("{:<width$}  {:>6}\n", ascii(&name), human(size)),
            None => format!("{}\n", ascii(&name)),
        })?;
    }
    Ok(())
}

/// Collect the entries of `dir`, named with `prefix`; with `recursive`, each
/// directory is followed by its contents. `seen` holds the real paths
/// already listed, so symlink loops are listed once. Unreadable
/// subdirectories are shown but not entered.
fn walk(
    root: &Root,
    dir: &Path,
    prefix: &str,
    recursive: bool,
    seen: &mut HashSet<PathBuf>,
    lines: &mut Vec<(String, Option<u64>)>,
) -> io::Result<()> {
    for entry in root.list(dir)? {
        let name = format!("{prefix}{}", entry.name);
        match entry.kind {
            Kind::File { size } => lines.push((name, Some(size))),
            Kind::Dir => {
                let name = format!("{name}/");
                lines.push((name.clone(), None));
                let sub = dir.join(&entry.name);
                if recursive
                    && let Ok(real) = root.resolve(&sub)
                    && seen.insert(real)
                {
                    let _ = walk(root, &sub, &name, true, seen, lines);
                }
            }
        }
    }
    Ok(())
}

/// Tab completion: a command name for the first word, otherwise the name
/// being typed in the argument (directories only for `cd`).
fn complete(root: &Root, cwd: &Path, line: &str) -> Completion {
    let unchanged = || Completion {
        line: line.to_string(),
        choices: Vec::new(),
    };
    let Some((command, _)) = line.split_once(' ') else {
        return complete_keyword("", line, COMMANDS);
    };
    let command = command.to_ascii_lowercase();
    let mut start = command.len() + 1;
    start += line[start..].len() - line[start..].trim_start().len();
    if command == "protocol" {
        let names = Protocol::ALL.map(Protocol::name);
        return complete_keyword(&line[..start], &line[start..], &names);
    }
    if matches!(command.as_str(), "ls" | "dir") && line[start..].starts_with("-R ") {
        start += 3;
        start += line[start..].len() - line[start..].trim_start().len();
    }
    let word = &line[start..];
    let (dir_part, prefix) = match word.rfind('/') {
        Some(i) => word.split_at(i + 1),
        None => ("", word),
    };
    let Ok(entries) = root
        .find(&target(cwd, dir_part))
        .and_then(|dir| root.list(&dir))
    else {
        return unchanged();
    };
    let prefix = prefix.to_lowercase();
    let matches: Vec<_> = entries
        .into_iter()
        .filter(|e| e.name.to_lowercase().starts_with(&prefix))
        .filter(|e| command != "cd" || e.kind == Kind::Dir)
        .filter(|e| e.name.bytes().all(|b| (0x20..=0x7e).contains(&b)))
        .collect();
    let head = &line[..start];
    match &matches[..] {
        [] => unchanged(),
        [only] => Completion {
            line: match only.kind {
                Kind::Dir => format!("{head}{dir_part}{}/", only.name),
                Kind::File { .. } => format!("{head}{dir_part}{} ", only.name),
            },
            choices: Vec::new(),
        },
        [first, rest @ ..] => {
            let common = rest.iter().fold(first.name.len(), |n, e| {
                first.name[..n]
                    .bytes()
                    .zip(e.name.bytes())
                    .take_while(|(a, b)| a.eq_ignore_ascii_case(b))
                    .count()
            });
            if common > prefix.len() {
                Completion {
                    line: format!("{head}{dir_part}{}", &first.name[..common]),
                    choices: Vec::new(),
                }
            } else {
                Completion {
                    line: line.to_string(),
                    choices: matches
                        .iter()
                        .map(|e| match e.kind {
                            Kind::Dir => format!("{}/", e.name),
                            Kind::File { .. } => e.name.clone(),
                        })
                        .collect(),
                }
            }
        }
    }
}

/// Complete `word`, which follows `head` on the line, from a fixed list.
fn complete_keyword(head: &str, word: &str, keywords: &[&str]) -> Completion {
    let lower = word.to_ascii_lowercase();
    let names: Vec<&str> = keywords
        .iter()
        .copied()
        .filter(|k| k.starts_with(&lower))
        .collect();
    let line = format!("{head}{word}");
    match names[..] {
        [] => Completion {
            line,
            choices: Vec::new(),
        },
        [name] => Completion {
            line: format!("{head}{name} "),
            choices: Vec::new(),
        },
        _ => Completion {
            line,
            choices: names.iter().map(|n| n.to_string()).collect(),
        },
    }
}

/// Find a typed path ignoring case; returns its kind and on-disk path.
fn lookup(root: &Root, path: &Path) -> io::Result<(Kind, PathBuf)> {
    let found = root.find(path)?;
    Ok((root.stat(&found)?, found))
}

/// Resolve a typed path against `cwd` without touching the filesystem.
/// A leading `/` starts at the top, and `..` stops there.
fn target(cwd: &Path, arg: &str) -> PathBuf {
    let mut path = if arg.starts_with('/') {
        PathBuf::new()
    } else {
        cwd.to_path_buf()
    };
    for part in arg.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                path.pop();
            }
            _ => path.push(part),
        }
    }
    path
}

/// A short error message for a path the user typed.
fn reason(e: &io::Error) -> String {
    match e.kind() {
        io::ErrorKind::NotFound => "No such file or directory".into(),
        io::ErrorKind::NotADirectory => "Not a directory".into(),
        io::ErrorKind::PermissionDenied => "Permission denied".into(),
        _ => e.to_string(),
    }
}

/// A size such as `512B`, `1.5kB`, `12MB` (powers of 1024).
fn human(size: u64) -> String {
    const UNITS: [&str; 5] = ["B", "kB", "MB", "GB", "TB"];
    let mut value = size as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit > 0 && value.round() >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    let number = if unit > 0 && value < 9.95 {
        format!("{value:.1}")
    } else {
        format!("{value:.0}")
    };
    format!("{}{}", number.trim_end_matches(".0"), UNITS[unit])
}

/// Lay out `names` in columns across `width`, one row per line.
fn columns(names: &[String], width: usize) -> String {
    let cell = names.iter().map(String::len).max().unwrap_or(0) + 2;
    let per_row = (width / cell).max(1);
    let mut out = String::new();
    for row in names.chunks(per_row) {
        let cells: Vec<_> = row.iter().map(|n| format!("{n:<cell$}")).collect();
        out.push_str(cells.concat().trim_end());
        out.push('\n');
    }
    out
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Protocol {
    Zmodem,
    Xmodem,
    Xmodem1k,
}

impl Protocol {
    const ALL: [Protocol; 3] = [Protocol::Zmodem, Protocol::Xmodem, Protocol::Xmodem1k];

    fn name(self) -> &'static str {
        match self {
            Protocol::Zmodem => "zmodem",
            Protocol::Xmodem => "xmodem",
            Protocol::Xmodem1k => "xmodem-1k",
        }
    }

    /// A name as typed to `protocol`, in full or abbreviated (`z`, `x`, `x-1k`).
    fn parse(text: &str) -> Option<Self> {
        match text.to_ascii_lowercase().as_str() {
            "z" | "zmodem" => Some(Protocol::Zmodem),
            "x" | "xmodem" => Some(Protocol::Xmodem),
            "x-1k" | "xmodem-1k" => Some(Protocol::Xmodem1k),
            _ => None,
        }
    }
}

/// Send `files` in order by `protocol`. A failed transfer ends the batch,
/// except a ZMODEM skip, which moves on to the next file. Transfer failures
/// are reported to the user; only link I/O errors are returned.
fn transfer(
    con: &mut Console,
    root: &Root,
    files: &[PathBuf],
    protocol: Protocol,
    options: &Options,
    log: &mut dyn FnMut(&str),
) -> io::Result<()> {
    let names: Vec<String> = files.iter().map(|f| file_name(f)).collect();
    if names.len() > 1 {
        con.print(&format!(
            "Sending {} files by {}:\n{}",
            names.len(),
            protocol.name().to_ascii_uppercase(),
            columns(&names, SCREEN_WIDTH)
        ))?;
    }
    for (i, (rel, name)) in files.iter().zip(&names).enumerate() {
        if files.len() > 1 {
            con.print(&format!(
                "\n({} of {}) {}\n",
                i + 1,
                files.len(),
                ascii(name)
            ))?;
        }
        if !send_file(con, root, rel, name, protocol, options, log)? && i + 1 < files.len() {
            return con.print("Remaining files not sent.\n");
        }
    }
    Ok(())
}

/// Send one file. Returns whether a batch should go on to the next file.
fn send_file(
    con: &mut Console,
    root: &Root,
    rel: &Path,
    name: &str,
    protocol: Protocol,
    options: &Options,
    log: &mut dyn FnMut(&str),
) -> io::Result<bool> {
    let contents = match root.read_file(rel) {
        Ok(contents) => contents,
        Err(e) => {
            log(&format!("cannot read {}: {e}", rel.display()));
            con.print(&format!("Cannot read file: {e}\n"))?;
            return Ok(false);
        }
    };
    let data = &contents.data;
    log(&format!(
        "sending {} ({} bytes) via {protocol:?}",
        rel.display(),
        data.len()
    ));

    let mut hint = None;
    let outcome = match protocol {
        Protocol::Zmodem => {
            con.print(&format!(
                "Sending {} bytes by ZMODEM. If your terminal does not start receiving\n\
                 automatically, start a ZMODEM download now. Press Ctrl-X five times to cancel.\n",
                data.len()
            ))?;
            let zoptions = zmodem::Options {
                start_timeout: options.start_timeout,
                ..zmodem::Options::default()
            };
            match zmodem::send(
                con.t,
                &zmodem::wire_name(name),
                data,
                contents.modified,
                &zoptions,
                |_| {},
            ) {
                Ok(summary) => Ok(format!("{} bytes", summary.bytes)),
                Err(zmodem::Error::Io(e)) => return Err(e),
                Err(e @ zmodem::Error::Skipped) => {
                    hint = Some(zmodem::SKIP_HINT);
                    Err(e.to_string())
                }
                Err(e) => Err(e.to_string()),
            }
        }
        Protocol::Xmodem | Protocol::Xmodem1k => {
            let (variant, label) = match protocol {
                Protocol::Xmodem1k => (xmodem::Variant::Xmodem1k, "XMODEM-1K"),
                _ => (xmodem::Variant::Xmodem, "XMODEM"),
            };
            con.print(&format!(
                "Start your {label} receive now. The file is {} bytes; XMODEM may pad it\n\
                 with ^Z characters. Press Ctrl-X twice to cancel.\n",
                data.len()
            ))?;
            let xoptions = xmodem::Options {
                start_timeout: options.start_timeout,
                ..xmodem::Options::new(variant)
            };
            match xmodem::send(con.t, data, &xoptions, |_| {}) {
                Ok(summary) if summary.padding > 0 => Ok(format!(
                    "{} bytes, padded to {}; the original is exactly {} bytes",
                    summary.bytes,
                    summary.bytes + summary.padding,
                    summary.bytes
                )),
                Ok(summary) => Ok(format!("{} bytes", summary.bytes)),
                Err(xmodem::Error::Io(e)) => return Err(e),
                Err(e) => Err(e.to_string()),
            }
        }
    };

    con.settle(match protocol {
        Protocol::Zmodem => ZMODEM_SETTLE,
        Protocol::Xmodem | Protocol::Xmodem1k => SETTLE,
    })?;
    match outcome {
        Ok(detail) => {
            log(&format!("sent {}: {detail}", rel.display()));
            con.print(&format!("\nTransfer complete: {detail}.\n"))?;
            Ok(true)
        }
        Err(reason) => {
            log(&format!("transfer of {} failed: {reason}", rel.display()));
            con.print(&format!("\nTransfer failed: {reason}.\n"))?;
            if let Some(hint) = hint {
                con.print(&wrap(hint, 78))?;
            }
            // A skipped file was declined; the rest may still be wanted.
            Ok(hint.is_some())
        }
    }
}

/// Word-wrap `text` to `width` columns for terminals that do not wrap.
fn wrap(text: &str, width: usize) -> String {
    let mut out = String::new();
    let mut column = 0;
    for word in text.split_whitespace() {
        if column > 0 && column + 1 + word.len() > width {
            out.push('\n');
            column = 0;
        } else if column > 0 {
            out.push(' ');
            column += 1;
        }
        out.push_str(word);
        column += word.len();
    }
    out.push('\n');
    out
}

/// The current directory as shown to the user, e.g. `/games/infocom`.
fn location(cwd: &Path) -> String {
    let mut out = String::new();
    for part in cwd.iter() {
        out.push('/');
        out.push_str(&part.to_string_lossy());
    }
    if out.is_empty() {
        out.push('/');
    }
    ascii(&out)
}

/// Make a name safe for a plain ASCII terminal.
fn ascii(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_graphic() || c == ' ' {
                c
            } else {
                '?'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::{MemoryTransport, memory_pair};
    use crate::{xmodem, zmodem};
    use std::fs;
    use std::thread::{self, JoinHandle};
    use std::time::Instant;

    struct Client {
        t: MemoryTransport,
        _tmp: tempfile::TempDir,
        server: JoinHandle<io::Result<()>>,
    }

    impl Client {
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

        fn finish(self) -> io::Result<()> {
            self.server.join().unwrap()
        }
    }

    const PROMPT: &str = "> ";

    /// root/{.hidden, data.bin (3000 bytes), docs/{readme.txt, .secret}, small.txt}
    fn start(options: Options) -> Client {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir(tmp.path().join("docs")).unwrap();
        fs::write(tmp.path().join("docs/readme.txt"), b"hello").unwrap();
        fs::write(tmp.path().join("docs/.secret"), b"secret").unwrap();
        fs::write(tmp.path().join(".hidden"), b"hidden").unwrap();
        fs::write(tmp.path().join("data.bin"), data()).unwrap();
        fs::write(tmp.path().join("small.txt"), b"small").unwrap();
        start_in(tmp, options)
    }

    fn start_in(tmp: tempfile::TempDir, options: Options) -> Client {
        let root = Root::new(tmp.path()).unwrap();
        let (mut server_end, client_end) = memory_pair();
        let server = thread::spawn(move || {
            let mut con = Console::new(&mut server_end);
            session(&mut con, &root, &options, &mut |_| {})
        });
        Client {
            t: client_end,
            _tmp: tmp,
            server,
        }
    }

    fn fast() -> Options {
        Options {
            idle_timeout: Duration::from_secs(5),
            start_timeout: Duration::from_secs(2),
        }
    }

    fn data() -> Vec<u8> {
        (0..3000u32).map(|i| (i * 31 % 251) as u8).collect()
    }

    #[test]
    fn ls_cd_pwd_and_quit() {
        let mut c = start(fast());
        c.expect("/> ");
        c.send("ls\r\n");
        let out = c.expect("/> ");
        assert!(
            out.contains("\r\ndata.bin    2.9kB\r\ndocs/\r\nsmall.txt      5B\r\n"),
            "{out}"
        );
        assert!(!out.contains("hidden"), "{out}");
        assert!(!out.contains('\x1b'), "no ANSI sequences");
        c.send("cd docs\r");
        c.expect("/docs> ");
        c.send("ls\r");
        let out = c.expect("/docs> ");
        assert!(out.contains("\r\nreadme.txt      5B\r\n"), "{out}");
        assert!(!out.contains("secret"), "{out}");
        c.send("cd ..\rpwd\r");
        c.expect("\r\n/\r\n/> ");
        c.send("cd /docs/../docs\rCD\r");
        c.expect("/docs> ");
        c.expect("\r\n/> ");
        c.send("ls docs\r");
        c.expect("readme.txt      5B\r\n/> ");
        c.send("quit\r");
        c.expect("Goodbye.");
        c.finish().unwrap();
    }

    #[test]
    fn ls_recursive() {
        let mut c = start(fast());
        c.expect(PROMPT);
        c.send("ls -R\r");
        let out = c.expect("\r\n/> ");
        assert!(
            out.contains(
                "\r\ndata.bin          2.9kB\r\ndocs/\r\ndocs/readme.txt      5B\r\nsmall.txt            5B\r\n"
            ),
            "{out}"
        );
        c.send("cd docs\rDIR -R /\r");
        c.expect("/docs> ");
        let out = c.expect("\r\n/docs> ");
        assert!(out.contains("docs/readme.txt"), "{out}");
        c.send("bye\r");
        c.finish().unwrap();
    }

    #[test]
    fn ls_recursive_survives_symlink_loop() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir(tmp.path().join("a")).unwrap();
        std::os::unix::fs::symlink(tmp.path(), tmp.path().join("a/up")).unwrap();
        let mut c = start_in(tmp, fast());
        c.expect(PROMPT);
        c.send("ls -R\r");
        let out = c.expect("\r\n/> ");
        assert!(out.contains("\r\na/\r\na/up/\r\n/> "), "{out}");
        c.send("quit\r");
        c.finish().unwrap();
    }

    #[test]
    fn ctrl_c_stops_listing_and_typeahead_is_kept() {
        let mut c = start(fast());
        c.expect(PROMPT);
        c.send("ls\r\x03");
        let out = c.expect("\r\n/> ");
        assert!(out.contains("ls\r\n^C\r\n"), "{out}");
        c.send("ls\rpwd\r");
        let out = c.expect("\r\n/\r\n/> ");
        assert!(out.contains("small.txt"), "{out}");
        c.send("quit\r");
        c.finish().unwrap();
    }

    #[test]
    fn errors_are_reported() {
        let mut c = start(fast());
        c.expect(PROMPT);
        c.send("cd small.txt\r");
        c.expect("small.txt: Not a directory");
        c.send("get docs\r");
        c.expect("docs: Is a directory");
        c.send("get nope\r");
        c.expect("nope: No such file or directory");
        c.send("ls nope\r");
        c.expect("nope: No such file or directory");
        c.send("get\r");
        c.expect("Usage: get FILE");
        c.send("frob\r");
        c.expect("Unknown command.");
        c.send("cd /../..\rpwd\r");
        c.expect("\r\n/\r\n");
        c.send("quit\r");
        c.finish().unwrap();
    }

    #[test]
    fn literal_paths_ignore_case() {
        let mut c = start(fast());
        c.expect(PROMPT);
        c.send("CD DOCS\r");
        c.expect("/docs> ");
        c.send("LS README.TXT\rDIR /DOCS\rcd /\rls Docs/Readme.Txt\r");
        let out = c.expect("\r\n/> ");
        assert!(out.contains("README.TXT      5B\r\n/docs> "), "{out}");
        assert!(out.contains("readme.txt      5B\r\n/docs> "), "{out}");
        let out = c.expect("\r\n/> ");
        assert!(out.contains("Docs/Readme.Txt      5B"), "{out}");
        // Completion through a directory typed in the wrong case.
        c.send("ls DOCS/R\t");
        c.expect("eadme.txt ");
        c.send("\r");
        c.expect("\r\n/> ");
        c.send("GET DOCS/README.TXT\r");
        c.expect("five times to cancel.\r\n");
        let rx = zmodem::test_receiver::receive(&mut c.t, None);
        assert_eq!(
            (&rx.name[..], &rx.data[..]),
            (&b"readme.txt"[..], &b"hello"[..])
        );
        c.expect("\r\n/> ");
        c.send("get DOCS/*.TXT\r");
        c.expect("five times to cancel.\r\n");
        let rx = zmodem::test_receiver::receive(&mut c.t, None);
        assert_eq!(rx.name, b"readme.txt");
        c.expect("\r\n/> ");
        c.send("get .HIDDEN\rget DOCS/.SECRET\rquit\r");
        let out = c.expect("Goodbye.");
        assert_eq!(out.matches("No such file or directory").count(), 2, "{out}");
        c.finish().unwrap();
    }

    #[test]
    fn names_differing_only_in_case() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("readme.txt"), b"lower").unwrap();
        fs::write(tmp.path().join("README.TXT"), b"UPPER").unwrap();
        fs::create_dir(tmp.path().join("Games")).unwrap();
        fs::create_dir(tmp.path().join("games")).unwrap();
        let mut c = start_in(tmp, fast());
        c.expect(PROMPT);
        // Each exact name reaches its own entry.
        c.send("cd Games\rcd ..\rcd games\r");
        c.expect("/Games> ");
        c.expect("/games> ");
        c.send("cd /\rls README.TXT\rget Readme.txt\rcd GAMES\r");
        c.expect("\r\n/> ");
        let out = c.expect("/> ");
        assert!(out.contains("README.TXT      5B"), "{out}");
        let out = c.expect("/> ");
        assert!(
            out.contains("Readme.txt: several names differ only in case"),
            "{out}"
        );
        let out = c.expect("/> ");
        assert!(
            out.contains("GAMES: several names differ only in case"),
            "{out}"
        );
        c.send("get readme.txt\r");
        c.expect("five times to cancel.\r\n");
        let rx = zmodem::test_receiver::receive(&mut c.t, None);
        assert_eq!(rx.data, b"lower");
        c.expect("/> ");
        c.send("quit\r");
        c.finish().unwrap();
    }

    #[test]
    fn hidden_files_are_unreachable() {
        let mut c = start(fast());
        c.expect(PROMPT);
        for cmd in [
            "get .hidden",
            "get docs/.secret",
            "ls .hidden",
            "cd .hidden",
        ] {
            c.send(&format!("{cmd}\r"));
            let out = c.expect("\r\n/> ");
            assert!(out.contains("No such file or directory"), "{cmd}: {out}");
        }
        c.send("cd docs\rget .s\t");
        let out = c.expect("get .s");
        c.send("\r");
        c.expect("No such file or directory");
        assert!(
            out.ends_with("\x07get .s") || !out.contains("secret"),
            "{out}"
        );
        c.send("quit\r");
        c.finish().unwrap();
    }

    #[test]
    fn tab_completion() {
        let mut c = start(fast());
        c.expect(PROMPT);
        // A unique command, then a unique file.
        c.send("g\t");
        c.expect("get ");
        c.send("sm\t");
        c.expect("all.txt ");
        c.send(&"\x08".repeat("get small.txt ".len()));
        // Protocol names complete too.
        c.send("protocol xmodem-\t");
        c.expect("1k ");
        c.send("\r");
        c.expect("Protocol set to xmodem-1k.");
        // cd completes only directories; a directory gets a trailing slash.
        c.send("cd d\t");
        c.expect("ocs/");
        c.send("\x08\x08\x08\x08\x08\x08\x08\x08");
        // Ambiguous: list the choices and redraw the line.
        c.send("ls d\t");
        c.expect("\r\ndata.bin  docs/\r\n/> ls d");
        // Case-insensitive, completing through a directory.
        c.send("\x08docs/R\t");
        c.expect("docs/R\x08 \x08readme.txt ");
        c.send("\r");
        c.expect("readme.txt      5B");
        // No match rings the bell.
        c.send("ls zz\t");
        c.expect("\x07");
        c.send("\x08\x08\x08\x08\x08quit\r");
        c.expect("Goodbye.");
        c.finish().unwrap();
    }

    #[test]
    fn line_editing() {
        let mut c = start(fast());
        c.expect(PROMPT);
        c.send("x\x7f\x01pwx\x08d\r");
        c.expect("\r\n/\r\n");
        c.send("quit\r");
        c.expect("Goodbye.");
        c.finish().unwrap();
    }

    #[test]
    fn history_recalls_earlier_lines() {
        let mut c = start(fast());
        c.expect(PROMPT);
        c.send("cd docs\rpwd\r");
        c.expect("\r\n/docs\r\n/docs> ");
        // Up recalls the newest line; Enter runs it again, and the repeat is
        // stored once.
        c.send("\x1b[A\r");
        c.expect("pwd\r\n/docs\r\n/docs> ");
        let erase = |n: usize| "\x08 \x08".repeat(n);
        let steps = [
            ("ls", "ls".to_string()),
            ("\x1b[A", erase(2) + "pwd"),
            ("\x1bOA", erase(3) + "cd docs"),
            ("\x10", "\x07".to_string()),
            ("\x1b[B", erase(7) + "pwd"),
            ("\x1b[D\x1b[3~\x0e", erase(3) + "ls"),
            ("\x0e", "\x07".to_string()),
            ("\x10", erase(2) + "pwd"),
        ];
        for (keys, echo) in steps {
            c.send(keys);
            assert_eq!(c.expect(&echo), echo, "{keys:?}");
        }
        c.send("\r");
        let out = c.expect("/docs> ");
        assert_eq!(out, "\r\n/docs\r\n/docs> ");
        c.send("quit\r");
        c.finish().unwrap();
    }

    #[test]
    fn ctrl_d_logs_out_only_on_an_empty_line() {
        let mut c = start(fast());
        c.expect(PROMPT);
        c.send("pw\x04d\r");
        c.expect("pwd\r\n/\r\n/> ");
        c.send("\x04");
        assert_eq!(c.expect("Goodbye."), "\r\nGoodbye.");
        c.finish().unwrap();
    }

    #[test]
    fn idle_timeout_ends_session() {
        let mut c = start(Options {
            idle_timeout: Duration::from_millis(200),
            ..fast()
        });
        c.expect("Idle timeout.");
        c.finish().unwrap();
    }

    #[test]
    fn zmodem_download_returns_to_prompt() {
        let mut c = start(fast());
        c.expect(PROMPT);
        c.send("get data.bin\r");
        c.expect("five times to cancel.\r\n");
        let rx = zmodem::test_receiver::receive(&mut c.t, None);
        assert_eq!(rx.name, b"data.bin");
        assert_eq!(rx.data, data());
        c.expect("Transfer complete: 3000 bytes.");
        c.expect(PROMPT);
        c.send("cd docs\rget readme.txt\r");
        c.expect("five times to cancel.\r\n");
        let rx = zmodem::test_receiver::receive(&mut c.t, None);
        assert_eq!(rx.name, b"readme.txt");
        assert_eq!(rx.data, b"hello");
        c.expect("/docs> ");
        c.send("quit\r");
        c.finish().unwrap();
    }

    #[test]
    fn late_zfin_is_not_input() {
        let mut c = start(fast());
        c.expect(PROMPT);
        c.send("get data.bin\r");
        c.expect("five times to cancel.\r\n");
        zmodem::test_receiver::receive(&mut c.t, None);
        // As captured from HyperTerminal: a repeated ZFIN ~730 ms after "OO".
        thread::sleep(Duration::from_millis(550));
        c.t.write_all(b"**\x18B0800000000022d\r\x8a").unwrap();
        let out = c.expect(PROMPT);
        assert!(out.contains("Transfer complete"), "{out}");
        c.send("quit\r");
        let out = c.expect("Goodbye.");
        assert!(!out.contains("Unknown command"), "{out}");
        assert!(!out.contains("**B08"), "ZFIN echoed: {out}");
        c.finish().unwrap();
    }

    #[test]
    fn zmodem_skip_explains_and_returns_to_prompt() {
        let mut c = start(fast());
        c.expect(PROMPT);
        c.send("get data.bin\r");
        c.expect("five times to cancel.\r\n");
        zmodem::test_receiver::skip(&mut c.t);
        let out = c.expect("\r\n/> ");
        assert!(
            out.contains("Transfer failed: receiver skipped the file."),
            "{out}"
        );
        assert!(out.contains("Receive File >"), "{out}");
        assert!(out.lines().all(|l| l.len() <= 80), "{out}");
        c.send("quit\r");
        c.finish().unwrap();
    }

    #[test]
    fn wrap_breaks_at_width() {
        assert_eq!(wrap("aaa bbb ccc", 7), "aaa bbb\nccc\n");
        assert_eq!(wrap("aaa", 2), "aaa\n");
    }

    #[test]
    fn zmodem_cancel_returns_to_prompt() {
        let mut c = start(fast());
        c.expect(PROMPT);
        c.send("get data.bin\r");
        c.expect("five times to cancel.\r\n");
        zmodem::test_receiver::receive(&mut c.t, Some(1));
        c.expect("Transfer failed: transfer cancelled by receiver.");
        c.expect(PROMPT);
        c.send("quit\r");
        c.finish().unwrap();
    }

    #[test]
    fn xmodem_download_returns_to_prompt() {
        let mut c = start(fast());
        c.expect(PROMPT);
        c.send("protocol x\rget data.bin\r");
        c.expect("twice to cancel.\r\n");
        let rx = xmodem::tests::receive(&mut c.t, b'C', Default::default());
        assert_eq!(&rx.data[..3000], data());
        c.expect("padded to 3072; the original is exactly 3000 bytes.");
        c.expect(PROMPT);
        c.send("quit\r");
        c.finish().unwrap();
    }

    #[test]
    fn no_receiver_is_reported() {
        let mut c = start(Options {
            start_timeout: Duration::from_millis(200),
            ..fast()
        });
        c.expect(PROMPT);
        c.send("protocol x-1k\rget small.txt\r");
        c.expect("Transfer failed: receiver did not start the transfer.");
        c.expect(PROMPT);
        c.send("quit\r");
        c.finish().unwrap();
    }

    #[test]
    fn protocol_shows_and_sets() {
        let mut c = start(fast());
        c.expect(PROMPT);
        c.send("protocol\r");
        c.expect("Protocol: zmodem\r\n");
        for (typed, name) in [
            ("x", "xmodem"),
            ("X-1K", "xmodem-1k"),
            ("zmodem", "zmodem"),
            ("xmodem", "xmodem"),
            ("xmodem-1k", "xmodem-1k"),
            ("z", "zmodem"),
        ] {
            c.send(&format!("protocol {typed}\r"));
            c.expect(&format!("Protocol set to {name}.\r\n"));
        }
        c.send("protocol kermit\rprotocol\r");
        c.expect("Unknown protocol.");
        c.expect("Protocol: zmodem\r\n");
        c.send("quit\r");
        c.finish().unwrap();
    }

    #[test]
    fn disconnect_is_an_error() {
        let c = start(fast());
        let Client { t, server, _tmp } = c;
        drop(t);
        assert!(server.join().unwrap().is_err());
    }

    #[test]
    fn get_wildcard_sends_each_match() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir(tmp.path().join("bin")).unwrap();
        fs::write(tmp.path().join("bin/A.EXE"), b"a").unwrap();
        fs::write(tmp.path().join("bin/b.exe"), b"bb").unwrap();
        fs::write(tmp.path().join("bin/c.txt"), b"c").unwrap();
        fs::write(tmp.path().join("bin/.d.exe"), b"d").unwrap();
        fs::create_dir(tmp.path().join("bin/dir.exe")).unwrap();
        let mut c = start_in(tmp, fast());
        c.expect(PROMPT);
        c.send("get bin/*.exe\r");
        let out = c.expect("(1 of 2)");
        assert!(
            out.contains("Sending 2 files by ZMODEM:\r\nA.EXE  b.exe\r\n"),
            "{out}"
        );
        let rx = zmodem::test_receiver::receive(&mut c.t, None);
        assert_eq!((&rx.name[..], &rx.data[..]), (&b"A.EXE"[..], &b"a"[..]));
        c.expect("(2 of 2) b.exe");
        let rx = zmodem::test_receiver::receive(&mut c.t, None);
        assert_eq!((&rx.name[..], &rx.data[..]), (&b"b.exe"[..], &b"bb"[..]));
        c.expect("Transfer complete: 2 bytes.");
        c.expect("\r\n/> ");
        c.send("get *.zip\r");
        c.expect("*.zip: No match");
        c.send("quit\r");
        c.finish().unwrap();
    }

    #[test]
    fn get_wildcard_stops_after_a_failure() {
        let mut c = start(Options {
            start_timeout: Duration::from_millis(200),
            ..fast()
        });
        c.expect(PROMPT);
        c.send("protocol x-1k\rget *.*\r");
        c.expect("Transfer failed: receiver did not start the transfer.");
        let out = c.expect("\r\n/> ");
        assert!(out.contains("Remaining files not sent."), "{out}");
        assert!(!out.contains("(2 of 2)"), "{out}");
        c.send("quit\r");
        c.finish().unwrap();
    }

    #[test]
    fn wildcard_matching() {
        assert!(wildcard("*.exe", "GAME.EXE"));
        assert!(wildcard("*", "x"));
        assert!(wildcard("a?c*", "abcdef"));
        assert!(wildcard("*a*b", "xaxxb"));
        assert!(!wildcard("*.exe", "game.exe.txt"));
        assert!(!wildcard("a?c", "ac"));
    }

    #[test]
    fn human_sizes() {
        let cases = [
            (0, "0B"),
            (1, "1B"),
            (1023, "1023B"),
            (1024, "1kB"),
            (1536, "1.5kB"),
            (10 * 1024, "10kB"),
            (1024 * 1024 - 1, "1MB"),
            (1474560, "1.4MB"),
            (5 << 30, "5GB"),
        ];
        for (size, text) in cases {
            assert_eq!(human(size), text, "{size}");
        }
    }

    #[test]
    fn target_resolves_typed_paths() {
        let cwd = Path::new("a/b");
        assert_eq!(target(cwd, ""), Path::new("a/b"));
        assert_eq!(target(cwd, "c/./d/"), Path::new("a/b/c/d"));
        assert_eq!(target(cwd, ".."), Path::new("a"));
        assert_eq!(target(cwd, "../../../.."), Path::new(""));
        assert_eq!(target(cwd, "/x"), Path::new("x"));
    }

    #[test]
    fn columns_fit_width() {
        let names: Vec<String> = ["aa", "bbbb", "c"].map(String::from).into();
        assert_eq!(columns(&names, 80), "aa    bbbb  c\n");
        assert_eq!(columns(&names, 12), "aa    bbbb\nc\n");
    }

    #[test]
    fn ascii_replaces_non_ascii() {
        assert_eq!(ascii("caf\u{e9}"), "caf?");
    }
}
