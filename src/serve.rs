// SPDX-License-Identifier: MIT
//! Interactive terminal file server.
//!
//! A vintage machine running an ordinary terminal program browses a
//! read-only file tree with ftp-style commands (`ls`, `cd`, `get`) and
//! downloads files by ZMODEM (auto-download where the terminal supports it)
//! or XMODEM. Output is plain ASCII with CR LF line endings and no ANSI
//! sequences. Tab completes command and file names.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::console::{Completer, Console, Input, SCREEN_WIDTH, columns};
use crate::files::{Kind, Root};
use crate::shell::{self, Argument, Mode, Tree, ascii, file_name, location, reason, target};
use crate::transport::Transport;
use crate::{xmodem, zmodem};

/// How long to wait for a key at the "press ENTER" banner before checking again.
const BANNER_WAIT: Duration = Duration::from_secs(3600);
/// Quiet period used to discard protocol leftovers after a transfer.
const SETTLE: Duration = Duration::from_millis(500);
/// Quiet period after ZMODEM. Windows 95 HyperTerminal repeats its ZFIN about
/// 730 ms after the session ends; a shorter wait lets it reach the prompt as input.
const ZMODEM_SETTLE: Duration = Duration::from_millis(1500);
/// Printed after the command list in `help`.
const HELP_FOOTER: &str = "\
With editing on, Tab completes names, Up and Down (or Ctrl-P and Ctrl-N)
recall earlier commands, and Ctrl-D logs out. Turn it off if your terminal
sends odd keystrokes. Ctrl-C stops a long listing.
";

/// Server settings.
#[derive(Clone, Debug)]
pub struct Options {
    /// End a session after this long without input.
    pub idle_timeout: Duration,
    /// How long a transfer waits for the receiver to start.
    pub start_timeout: Duration,
    /// Whether sessions start with line editing (history, Tab completion,
    /// Ctrl-D logout) on; `set editing` changes it within a session.
    pub editing: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            idle_timeout: Duration::from_secs(600),
            start_timeout: Duration::from_secs(60),
            editing: true,
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

fn session(
    con: &mut Console,
    root: &Root,
    options: &Options,
    log: &mut dyn FnMut(&str),
) -> io::Result<()> {
    let mut tree = root.clone();
    let mut cwd = PathBuf::new();
    let mut protocol = Protocol::Zmodem;
    let mut editing = options.editing;
    con.history.clear();
    con.print("\nType help for a list of commands.\n")?;
    loop {
        let prompt = format!("{}> ", location(&cwd));
        con.print(&prompt)?;
        let mut completer = |line: &str| {
            shell::complete(
                Mode::Terminal,
                line,
                &argument,
                (&mut root.clone(), &cwd),
                None,
            )
        };
        let input = match con.read_line(
            options.idle_timeout,
            editing.then_some((prompt.as_str(), &mut completer as Completer)),
        )? {
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
        let (name, arg) = shell::split(&input);
        if name.is_empty() {
            continue;
        }
        let Some(command) = shell::command(name) else {
            con.print(&shell::unknown(name))?;
            continue;
        };
        if command.terminal.is_none() {
            con.print(&shell::unavailable(name, Mode::Terminal))?;
            continue;
        }
        match command.name() {
            "quit" => {
                con.print("Goodbye.\n")?;
                return Ok(());
            }
            "help" => con.print(&shell::help(Mode::Terminal, HELP_FOOTER))?,
            "pwd" => con.print(&format!("{}\n", location(&cwd)))?,
            "cd" => {
                let dir = if arg.is_empty() {
                    PathBuf::new()
                } else {
                    target(&cwd, arg)
                };
                match tree.lookup(&dir) {
                    Ok((Kind::Dir, dir)) => cwd = dir,
                    Ok((Kind::File { .. }, _)) => {
                        con.print(&format!("{}: Not a directory\n", ascii(arg)))?
                    }
                    Err(e) => con.print(&format!("{}: {}\n", ascii(arg), reason(&e)))?,
                }
            }
            "ls" | "dir" => shell::ls(con, &mut tree, &cwd, arg)?,
            "get" if shell::flag(arg, "-f").0 => {
                con.print("get: -f is not available in terminal mode\n")?
            }
            "get" if arg.is_empty() => con.print("Usage: get FILE\n")?,
            "get" => match shell::expand(&mut tree, &cwd, arg) {
                Ok(files) => transfer(con, root, &files, protocol, options, log)?,
                Err(message) => con.print(&message)?,
            },
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
            "set" => match shell::set_editing(arg, &mut editing) {
                Ok(()) => con.print(&format!(
                    "Editing is {}.\n",
                    if editing { "on" } else { "off" }
                ))?,
                Err(usage) => con.print(usage)?,
            },
            other => unreachable!("{other} is available in terminal mode"),
        }
    }
}

/// What Tab completes in each command's argument.
fn argument(command: &str) -> Argument {
    match command {
        "cd" => Argument::Remote { dirs_only: true },
        "ls" | "dir" | "get" => Argument::Remote { dirs_only: false },
        "protocol" => Argument::Words(&["xmodem", "xmodem-1k", "zmodem"]),
        _ => Argument::None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Protocol {
    Zmodem,
    Xmodem,
    Xmodem1k,
}

impl Protocol {
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
            editing: true,
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
        c.expect("Unknown command: frob\r\nType help for a list of commands.\r\n/> ");
        c.send("put x\rLCD /\rlls\rldir\rlpwd\rget -f x\r");
        c.expect("put: not available in terminal mode\r\n/> ");
        c.expect("LCD: not available in terminal mode\r\n/> ");
        c.expect("lls: not available in terminal mode\r\n/> ");
        c.expect("ldir: not available in terminal mode\r\n/> ");
        c.expect("lpwd: not available in terminal mode\r\n/> ");
        c.expect("get: -f is not available in terminal mode\r\n/> ");
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
    fn editing_off_at_startup_is_basic_input() {
        let mut c = start(Options {
            editing: false,
            ..fast()
        });
        c.expect(PROMPT);
        c.send("cd docs\r");
        c.expect("/docs> ");
        // Tab, Ctrl-P, Ctrl-N and Ctrl-D are ignored, and ESC is dropped
        // alone rather than starting a sequence that swallows what follows.
        c.send("pw\t\x10\x0e\x04\x1bd\r");
        let out = c.expect("/docs> ");
        assert_eq!(out, "pwd\r\n/docs\r\n/docs> ");
        c.send("\x04\x1b[A\r");
        let out = c.expect("/docs> ");
        assert_eq!(
            out,
            "[A\r\nUnknown command: [A\r\nType help for a list of commands.\r\n/docs> "
        );
        // Backspace still edits.
        c.send("pwx\x08d\r");
        c.expect("/docs\r\n/docs> ");
        // set editing works in basic mode, and the change is immediate.
        c.send("set editing on\r");
        c.expect("Editing is on.\r\n/docs> ");
        c.send("\x1b[A");
        c.expect("set editing on");
        c.send(&"\x08".repeat("set editing on".len()));
        c.send("\x04");
        c.expect("Goodbye.");
        c.finish().unwrap();
    }

    #[test]
    fn set_editing_switches_modes() {
        let mut c = start(fast());
        c.expect(PROMPT);
        c.send("set\r");
        c.expect("Editing is on.\r\n/> ");
        c.send("set e\t");
        c.expect("diting ");
        c.send("o\t");
        c.expect("\r\noff  on\r\n/> set editing o");
        c.send("ff\r");
        c.expect("Editing is off.\r\n/> ");
        c.send("set editing\r");
        c.expect("Editing is off.\r\n/> ");
        c.send("\x04ls d\t\r");
        let out = c.expect("/> ");
        assert!(
            out.contains("ls d\r\nd: No such file or directory"),
            "{out}"
        );
        c.send("SET EDITING ON\r");
        c.expect("Editing is on.\r\n/> ");
        c.send("set editing maybe\r");
        c.expect("Usage: set editing on|off\r\n/> ");
        c.send("\x04");
        c.expect("Goodbye.");
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
    fn help_shows_every_command() {
        let mut c = start(fast());
        c.expect(PROMPT);
        c.send(
            "help
",
        );
        let out = c.expect("\r\n/> ");
        assert!(out.contains("\r\n  protocol [NAME]"), "{out}");
        assert!(out.contains("Ctrl-C stops a long listing.\r\n"), "{out}");
        assert!(
            out.contains("Not available in terminal mode: lcd, ldir, lls, lpwd, put.\r\n"),
            "{out}"
        );
        c.send("?\r");
        c.expect("Commands:");
        c.send("quit\r");
        c.finish().unwrap();
    }
}
