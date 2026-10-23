// SPDX-License-Identifier: MIT
//! The ftp-style command shell shared by terminal mode (`sprocket serve
//! --mode terminal`) and the native client (`sprocket client`): the command
//! vocabulary and its help, listing format, typed paths, wildcards and Tab
//! completion. See `docs/sprocket-file-transfer-ui.md`.
//!
//! Both interfaces recognize every command; one that a mode cannot run is
//! refused with a message naming the mode, rather than being unknown. The
//! file tree being browsed is reached through [`Tree`], so the same listing
//! and completion code works on a local directory and over the native
//! protocol.

use std::collections::HashSet;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use crate::console::{Completion, Console};
use crate::files::{Entry, Kind, Root};

/// Names longer than this push their size out of the listing's column.
pub const NAME_COLUMN: usize = 40;
/// How deep `ls -R` goes in a tree that cannot report real paths, which
/// guards against symlink loops on the far side.
const MAX_DEPTH: usize = 32;

/// Which interface the shell is running.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// A terminal program on the serial line; downloads by XMODEM or ZMODEM.
    Terminal,
    /// The native client, with a local side as well as the remote one.
    #[allow(dead_code)] // until the native client uses it
    Native,
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Mode::Terminal => "terminal",
            Mode::Native => "native",
        })
    }
}

/// One command of the shared vocabulary.
pub struct Command {
    /// Name, then aliases.
    pub names: &'static [&'static str],
    /// Help line for each mode, or `None` where the command is unavailable.
    pub terminal: Option<&'static str>,
    pub native: Option<&'static str>,
}

impl Command {
    pub fn name(&self) -> &'static str {
        self.names[0]
    }

    pub fn help(&self, mode: Mode) -> Option<&'static str> {
        match mode {
            Mode::Terminal => self.terminal,
            Mode::Native => self.native,
        }
    }
}

/// Every command either interface recognizes, in alphabetical order. Help
/// lines are the usage, padded to column 17, then the description;
/// continuation lines start with spaces.
pub const VOCABULARY: &[Command] = &[
    Command {
        names: &["cd"],
        terminal: Some("cd [DIR]         change directory; with no DIR, go to the top"),
        native: Some("cd [DIR]         change the remote directory; with no DIR, go to the top"),
    },
    Command {
        names: &["dir"],
        terminal: Some("dir [-R] [PATH]  same as ls"),
        native: Some("dir [-R] [PATH]  same as ls"),
    },
    Command {
        names: &["get"],
        terminal: Some(
            "get FILE         download a file; FILE may use * and ? wildcards,\n\
             \x20                  e.g. get *.exe",
        ),
        native: Some(
            "get [-f] FILE    copy remote files here; FILE may use * and ?\n\
             \x20                  wildcards; -f replaces existing files",
        ),
    },
    Command {
        names: &["help", "?"],
        terminal: Some("help             show this list"),
        native: Some("help             show this list"),
    },
    Command {
        names: &["lcd"],
        terminal: None,
        native: Some("lcd [DIR]        change the local directory; with no DIR, go home"),
    },
    Command {
        names: &["ldir"],
        terminal: None,
        native: Some("ldir [PATH]      same as lls"),
    },
    Command {
        names: &["lls"],
        terminal: None,
        native: Some("lls [PATH]       list a local directory"),
    },
    Command {
        names: &["lpwd"],
        terminal: None,
        native: Some("lpwd             show the local directory"),
    },
    Command {
        names: &["ls"],
        terminal: Some("ls [-R] [PATH]   list a directory; -R lists everything below it"),
        native: Some("ls [-R] [PATH]   list a remote directory; -R lists everything below it"),
    },
    Command {
        names: &["protocol"],
        terminal: Some(
            "protocol [NAME]  show or set the download protocol: zmodem (z),\n\
             \x20                  xmodem (x) or xmodem-1k (x-1k); zmodem by default",
        ),
        native: None,
    },
    Command {
        names: &["put"],
        terminal: None,
        native: Some(
            "put [-f] FILE    copy local files to the remote directory; FILE may\n\
             \x20                  use * and ? wildcards; -f replaces existing files",
        ),
    },
    Command {
        names: &["pwd"],
        terminal: Some("pwd              show the current directory"),
        native: Some("pwd              show the remote directory"),
    },
    Command {
        names: &["quit", "bye", "exit"],
        terminal: Some("quit             end the session (or Ctrl-D)"),
        native: Some("quit             end the session (or Ctrl-D)"),
    },
    Command {
        names: &["set"],
        terminal: Some(
            "set editing on|off\n\
             \x20                  turn line editing on or off for this session",
        ),
        native: Some(
            "set editing on|off\n\
             \x20                  turn line editing on or off for this session",
        ),
    },
];

/// Look up a typed command name, ignoring case.
pub fn command(name: &str) -> Option<&'static Command> {
    let name = name.to_ascii_lowercase();
    VOCABULARY.iter().find(|c| c.names.contains(&name.as_str()))
}

/// Names Tab completes in `mode`: the commands available there and their
/// word aliases.
pub fn command_names(mode: Mode) -> Vec<&'static str> {
    let mut names: Vec<&str> = VOCABULARY
        .iter()
        .filter(|c| c.help(mode).is_some())
        .flat_map(|c| c.names.iter().copied())
        .filter(|n| n.chars().all(|c| c.is_ascii_alphabetic()))
        .collect();
    names.sort();
    names
}

/// The `help` text for `mode`, ending with `footer` and then the commands
/// recognized but unavailable in this mode.
pub fn help(mode: Mode, footer: &str) -> String {
    let mut out = String::from("Commands:\n");
    for line in VOCABULARY.iter().filter_map(|c| c.help(mode)) {
        out.push_str(&format!("  {line}\n"));
    }
    out.push_str(footer);
    let missing: Vec<&str> = VOCABULARY
        .iter()
        .filter(|c| c.help(mode).is_none())
        .map(Command::name)
        .collect();
    if !missing.is_empty() {
        out.push_str(&format!(
            "Not available in {mode} mode: {}.\n",
            missing.join(", ")
        ));
    }
    out
}

/// The message for a recognized command that `mode` cannot run.
pub fn unavailable(name: &str, mode: Mode) -> String {
    format!("{}: not available in {mode} mode\n", ascii(name))
}

/// The message for a command nobody recognizes.
pub fn unknown(name: &str) -> String {
    format!(
        "Unknown command: {}\nType help for a list of commands.\n",
        ascii(name)
    )
}

/// Split a typed line into its command and argument.
pub fn split(input: &str) -> (&str, &str) {
    let input = input.trim();
    match input.split_once(' ') {
        Some((command, arg)) => (command, arg.trim()),
        None => (input, ""),
    }
}

/// Take a leading flag such as `-R` or `-f` off an argument.
pub fn flag<'a>(arg: &'a str, flag: &str) -> (bool, &'a str) {
    match arg.strip_prefix(flag) {
        Some(rest) if rest.is_empty() || rest.starts_with(' ') => (true, rest.trim()),
        _ => (false, arg),
    }
}

/// `set [editing [on|off]]`: change `editing` as asked. With no value it is
/// left as it is, for the caller to show; a bad argument gives the usage.
pub fn set_editing(arg: &str, editing: &mut bool) -> Result<(), &'static str> {
    let words = arg.to_ascii_lowercase();
    match words.split_whitespace().collect::<Vec<_>>()[..] {
        [] | ["editing"] => {}
        ["editing", "on"] => *editing = true,
        ["editing", "off"] => *editing = false,
        _ => return Err("Usage: set editing on|off\n"),
    }
    Ok(())
}

/// A browsable file tree: the served root, a local directory, or the far
/// side of a native link. Paths are relative to the tree's top, as
/// [`target`] builds them.
pub trait Tree {
    /// Find a typed path, ignoring case where the tree does; returns its
    /// kind and the path as spelled in the tree.
    fn lookup(&mut self, path: &Path) -> io::Result<(Kind, PathBuf)>;

    /// The entries of a directory, sorted alphabetically ignoring case.
    fn list(&mut self, dir: &Path) -> io::Result<Vec<Entry>>;

    /// A real identity for a directory, used by `ls -R` to list each one
    /// once in spite of symlink loops. Trees that cannot tell return `None`
    /// and are listed to a fixed depth instead.
    fn identity(&mut self, _dir: &Path) -> Option<PathBuf> {
        None
    }
}

impl Tree for Root {
    fn lookup(&mut self, path: &Path) -> io::Result<(Kind, PathBuf)> {
        let found = self.find(path)?;
        Ok((self.stat(&found)?, found))
    }

    fn list(&mut self, dir: &Path) -> io::Result<Vec<Entry>> {
        Root::list(self, dir)
    }

    fn identity(&mut self, dir: &Path) -> Option<PathBuf> {
        self.resolve(dir).ok()
    }
}

/// `ls [-R] [PATH]` (or `dir`, `lls`): one entry per line, directories
/// marked with `/`, with `cwd` the current directory in `tree`.
pub fn ls(con: &mut Console, tree: &mut dyn Tree, cwd: &Path, arg: &str) -> io::Result<()> {
    let (recursive, arg) = flag(arg, "-R");
    let mut lines = Vec::new();
    match tree.lookup(&target(cwd, arg)) {
        Ok((Kind::File { size }, _)) => lines.push((arg.to_string(), Some(size))),
        Ok((Kind::Dir, path)) => {
            let mut seen = HashSet::from_iter(tree.identity(&path));
            let mut walk = Walk {
                tree,
                recursive,
                seen: &mut seen,
                lines: &mut lines,
            };
            match walk.dir(con, &path, "", 0) {
                Ok(true) => {}
                Ok(false) => return con.print("^C\n"),
                Err(e) => return con.print(&format!("{}: {}\n", ascii(arg), reason(&e))),
            }
        }
        Err(e) => return con.print(&format!("{}: {}\n", ascii(arg), reason(&e))),
    }
    for line in listing(&lines).lines() {
        if con.interrupted()? {
            return con.print("^C\n");
        }
        con.print(&format!("{line}\n"))?;
    }
    Ok(())
}

/// Format listing lines: each name with its size, or `None` for a
/// directory, whose name already ends in `/`. Sizes are right-aligned in a
/// column after the longest file name, up to [`NAME_COLUMN`].
pub fn listing(lines: &[(String, Option<u64>)]) -> String {
    let width = lines
        .iter()
        .filter(|(_, size)| size.is_some())
        .map(|(name, _)| name.len())
        .max()
        .unwrap_or(0)
        .min(NAME_COLUMN);
    let mut out = String::new();
    for (name, size) in lines {
        out.push_str(&match size {
            Some(size) => format!("{:<width$}  {:>6}\n", ascii(name), human(*size)),
            None => format!("{}\n", ascii(name)),
        });
    }
    out
}

/// Collects the entries below a directory for [`ls`].
struct Walk<'a> {
    tree: &'a mut dyn Tree,
    recursive: bool,
    /// Identities of the directories already listed.
    seen: &'a mut HashSet<PathBuf>,
    lines: &'a mut Vec<(String, Option<u64>)>,
}

impl Walk<'_> {
    /// Collect the entries of `dir`, named with `prefix`; when recursive,
    /// each directory is followed by its contents. Unreadable
    /// subdirectories are shown but not entered. Returns false if the user
    /// pressed Ctrl-C.
    fn dir(
        &mut self,
        con: &mut Console,
        dir: &Path,
        prefix: &str,
        depth: usize,
    ) -> io::Result<bool> {
        for entry in self.tree.list(dir)? {
            let name = format!("{prefix}{}", entry.name);
            match entry.kind {
                Kind::File { size } => self.lines.push((name, Some(size))),
                Kind::Dir => {
                    let name = format!("{name}/");
                    self.lines.push((name.clone(), None));
                    if !self.recursive {
                        continue;
                    }
                    if con.interrupted()? {
                        return Ok(false);
                    }
                    let sub = dir.join(&entry.name);
                    let enter = match self.tree.identity(&sub) {
                        Some(real) => self.seen.insert(real),
                        None => depth + 1 < MAX_DEPTH,
                    };
                    if enter && !self.dir(con, &sub, &name, depth + 1).unwrap_or(true) {
                        return Ok(false);
                    }
                }
            }
        }
        Ok(true)
    }
}

/// The files `get` or `put` names: one file, or the files in a directory
/// matching `*` and `?` wildcards in the last part of `arg`. Returns their
/// paths in `tree`, in listing order, or the message to show.
pub fn expand(tree: &mut dyn Tree, cwd: &Path, arg: &str) -> Result<Vec<PathBuf>, String> {
    let path = target(cwd, arg);
    let pattern = file_name(&path);
    let fail = |e: io::Error| format!("{}: {}\n", ascii(arg), reason(&e));
    if !pattern.contains(['*', '?']) {
        return match tree.lookup(&path) {
            Ok((Kind::File { .. }, path)) => Ok(vec![path]),
            Ok((Kind::Dir, _)) => Err(format!("{}: Is a directory\n", ascii(arg))),
            Err(e) => Err(fail(e)),
        };
    }
    let dir = match tree.lookup(path.parent().unwrap_or(Path::new(""))) {
        Ok((Kind::Dir, dir)) => dir,
        Ok((Kind::File { .. }, _)) => return Err(format!("{}: Not a directory\n", ascii(arg))),
        Err(e) => return Err(fail(e)),
    };
    let files: Vec<PathBuf> = tree
        .list(&dir)
        .map_err(fail)?
        .into_iter()
        .filter(|e| matches!(e.kind, Kind::File { .. }) && wildcard(&pattern, &e.name))
        .map(|e| dir.join(e.name))
        .collect();
    if files.is_empty() {
        return Err(format!("{}: No match\n", ascii(arg)));
    }
    Ok(files)
}

/// Match `name` against `pattern`, where `*` is any run of characters and
/// `?` is any one character. Case is ignored, as DOS users expect.
pub fn wildcard(pattern: &str, name: &str) -> bool {
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
pub fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// What Tab completes in each argument: which tree, and whether only
/// directories.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Argument {
    /// No completion.
    None,
    /// A name in the remote (or served) tree.
    Remote { dirs_only: bool },
    /// A name in the local tree (native mode).
    #[allow(dead_code)] // until the native client uses it
    Local { dirs_only: bool },
    /// One of a fixed list of words.
    Words(&'static [&'static str]),
}

/// Tab completion: a command name for the first word, otherwise the word
/// being typed in the argument, as `argument` says for that command.
/// `remote` and `local` give each tree with its current directory.
pub fn complete<'a>(
    mode: Mode,
    line: &str,
    argument: &dyn Fn(&str) -> Argument,
    remote: (&'a mut dyn Tree, &'a Path),
    local: Option<(&'a mut dyn Tree, &'a Path)>,
) -> Completion {
    let unchanged = || Completion {
        line: line.to_string(),
        choices: Vec::new(),
    };
    let Some((command, _)) = line.split_once(' ') else {
        return complete_keyword("", line, &command_names(mode));
    };
    let command = command.to_ascii_lowercase();
    let mut start = command.len() + 1;
    start += line[start..].len() - line[start..].trim_start().len();
    if command == "set" {
        let rest = &line[start..];
        return match rest.get(..8) {
            Some(word) if word.eq_ignore_ascii_case("editing ") => {
                let value = rest[8..].trim_start();
                complete_keyword(&line[..line.len() - value.len()], value, &["off", "on"])
            }
            _ => complete_keyword(&line[..start], rest, &["editing"]),
        };
    }
    let (tree, cwd, dirs_only) = match argument(&command) {
        Argument::None => return unchanged(),
        Argument::Words(words) => return complete_keyword(&line[..start], &line[start..], words),
        Argument::Remote { dirs_only } => (remote.0, remote.1, dirs_only),
        Argument::Local { dirs_only } => match local {
            Some((tree, cwd)) => (tree, cwd, dirs_only),
            None => return unchanged(),
        },
    };
    for flag in ["-R ", "-f "] {
        if line[start..].starts_with(flag) {
            start += 3;
            start += line[start..].len() - line[start..].trim_start().len();
        }
    }
    let word = &line[start..];
    let (dir_part, prefix) = match word.rfind('/') {
        Some(i) => word.split_at(i + 1),
        None => ("", word),
    };
    let Ok(entries) = tree
        .lookup(&target(cwd, dir_part))
        .and_then(|(_, dir)| tree.list(&dir))
    else {
        return unchanged();
    };
    let prefix = prefix.to_lowercase();
    let matches: Vec<_> = entries
        .into_iter()
        .filter(|e| e.name.to_lowercase().starts_with(&prefix))
        .filter(|e| !dirs_only || e.kind == Kind::Dir)
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
pub fn complete_keyword(head: &str, word: &str, keywords: &[&str]) -> Completion {
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

/// Resolve a typed path against `cwd` without touching the filesystem.
/// A leading `/` starts at the top, and `..` stops there.
pub fn target(cwd: &Path, arg: &str) -> PathBuf {
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
pub fn reason(e: &io::Error) -> String {
    match e.kind() {
        io::ErrorKind::NotFound => "No such file or directory".into(),
        io::ErrorKind::NotADirectory => "Not a directory".into(),
        io::ErrorKind::IsADirectory => "Is a directory".into(),
        io::ErrorKind::PermissionDenied => "Permission denied".into(),
        io::ErrorKind::AlreadyExists => "File exists".into(),
        _ => e.to_string(),
    }
}

/// A size such as `512B`, `1.5kB`, `12MB` (powers of 1024).
pub fn human(size: u64) -> String {
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

/// A directory as shown to the user, e.g. `/games/infocom`.
pub fn location(cwd: &Path) -> String {
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
pub fn ascii(name: &str) -> String {
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

    #[test]
    fn vocabulary_is_alphabetical_and_complete() {
        let names: Vec<&str> = VOCABULARY.iter().map(Command::name).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
        assert_eq!(
            names,
            [
                "cd", "dir", "get", "help", "lcd", "ldir", "lls", "lpwd", "ls", "protocol", "put",
                "pwd", "quit", "set"
            ]
        );
    }

    #[test]
    fn help_lists_available_commands_and_names_the_rest() {
        let text = help(Mode::Terminal, "");
        let names: Vec<&str> = text
            .lines()
            .filter_map(|l| l.strip_prefix("  "))
            .filter(|l| !l.starts_with(' '))
            .filter_map(|l| l.split_whitespace().next())
            .collect();
        assert_eq!(
            names,
            [
                "cd", "dir", "get", "help", "ls", "protocol", "pwd", "quit", "set"
            ]
        );
        assert!(
            text.ends_with("Not available in terminal mode: lcd, ldir, lls, lpwd, put.\n"),
            "{text}"
        );
        let text = help(Mode::Native, "");
        assert!(text.contains("  put [-f] FILE"), "{text}");
        assert!(
            text.ends_with("Not available in native mode: protocol.\n"),
            "{text}"
        );
        for line in text.lines() {
            assert!(line.len() <= 79, "{line:?}");
        }
    }

    #[test]
    fn commands_are_found_by_name_or_alias() {
        assert_eq!(command("DIR").map(Command::name), Some("dir"));
        assert_eq!(command("?").map(Command::name), Some("help"));
        assert_eq!(command("bye").map(Command::name), Some("quit"));
        assert!(command("frobnicate").is_none());
        assert_eq!(
            unavailable("put", Mode::Terminal),
            "put: not available in terminal mode\n"
        );
        assert_eq!(
            unknown("frobnicate"),
            "Unknown command: frobnicate\nType help for a list of commands.\n"
        );
    }

    #[test]
    fn completion_offers_only_available_commands() {
        assert!(command_names(Mode::Terminal).contains(&"protocol"));
        assert!(!command_names(Mode::Terminal).contains(&"put"));
        assert!(command_names(Mode::Native).contains(&"lcd"));
        assert!(!command_names(Mode::Native).contains(&"protocol"));
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
    fn flags_are_taken_off_arguments() {
        assert_eq!(flag("-R docs", "-R"), (true, "docs"));
        assert_eq!(flag("-R", "-R"), (true, ""));
        assert_eq!(flag("-Rx", "-R"), (false, "-Rx"));
        assert_eq!(flag("docs", "-f"), (false, "docs"));
    }

    #[test]
    fn ascii_replaces_non_ascii() {
        assert_eq!(ascii("caf\u{e9}"), "caf?");
    }
}
