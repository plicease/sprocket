// SPDX-License-Identifier: MIT
//! Interactive terminal file server.
//!
//! A vintage machine running an ordinary terminal program browses a
//! read-only file tree and downloads files by ZMODEM (auto-download where
//! the terminal supports it) or XMODEM. The menu is plain ASCII with CR LF
//! line endings and no ANSI sequences, sized for an 80x24 screen.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::files::{Kind, Root};
use crate::transport::Transport;
use crate::{xmodem, zmodem};

/// Entries shown per page; leaves room for the header and prompt on 24 lines.
const PAGE: usize = 16;
/// Longest name shown in a listing before truncation.
const NAME_WIDTH: usize = 50;
/// Longest accepted input line.
const MAX_INPUT: usize = 16;
/// How long to wait for a key at the "press ENTER" banner before checking again.
const BANNER_WAIT: Duration = Duration::from_secs(3600);
/// Quiet period used to discard protocol leftovers after a transfer.
const SETTLE: Duration = Duration::from_millis(500);
/// Quiet period after ZMODEM. Windows 95 HyperTerminal repeats its ZFIN about
/// 730 ms after the session ends; a shorter wait lets it reach the menu as input.
const ZMODEM_SETTLE: Duration = Duration::from_millis(1500);

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
        con.print("\nSPROCKET file server. Press ENTER for the menu.\n")?;
        while !matches!(con.t.read_byte(BANNER_WAIT)?, Some(b'\r' | b'\n')) {}
        log("session started");
        session(&mut con, root, options, log)?;
        log("session ended");
    }
}

/// Line-oriented access to a dumb terminal.
struct Console<'a> {
    t: &'a mut dyn Transport,
    /// The previous byte was CR, so a following LF is part of the same newline.
    after_cr: bool,
}

impl<'a> Console<'a> {
    fn new(t: &'a mut dyn Transport) -> Self {
        Self { t, after_cr: false }
    }

    fn print(&mut self, text: &str) -> io::Result<()> {
        self.t.write_all(text.replace('\n', "\r\n").as_bytes())
    }

    /// Read a line with echo and backspace editing. Returns `None` if no key
    /// arrives within `idle`.
    fn read_line(&mut self, idle: Duration) -> io::Result<Option<String>> {
        let mut line = String::new();
        loop {
            let Some(byte) = self.t.read_byte(idle)? else {
                return Ok(None);
            };
            let after_cr = std::mem::replace(&mut self.after_cr, byte == b'\r');
            match byte {
                b'\n' if after_cr => {}
                b'\r' | b'\n' => {
                    self.print("\n")?;
                    return Ok(Some(line));
                }
                0x08 | 0x7f => {
                    if line.pop().is_some() {
                        self.t.write_all(b"\x08 \x08")?;
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

    /// Discard protocol leftovers so they are not read as menu input.
    fn settle(&mut self, quiet: Duration) -> io::Result<()> {
        self.t.drain(quiet)?;
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
    let mut page = 0;
    loop {
        let entries = match root.list(&cwd) {
            Ok(entries) => entries,
            Err(e) => {
                con.print(&format!("Cannot list {}: {e}\n", location(&cwd)))?;
                if cwd.as_os_str().is_empty() {
                    return Ok(());
                }
                cwd = PathBuf::new();
                continue;
            }
        };
        let pages = entries.len().div_ceil(PAGE).max(1);
        page = page.min(pages - 1);

        let mut screen = format!("\n{}   (page {} of {pages})\n", location(&cwd), page + 1);
        if entries.is_empty() {
            screen.push_str("  (empty)\n");
        }
        for (i, entry) in entries.iter().enumerate().skip(page * PAGE).take(PAGE) {
            let (name, size) = match entry.kind {
                Kind::Dir => (format!("{}/", entry.name), "<DIR>".to_string()),
                Kind::File { size } => (entry.name.clone(), size.to_string()),
            };
            screen.push_str(&format!(
                "{:>4}) {:<NAME_WIDTH$} {:>12}\n",
                i + 1,
                display_name(&name),
                size
            ));
        }
        screen.push_str("Number to open, U)p, N)ext page, P)rev page, Q)uit: ");
        con.print(&screen)?;

        let Some(input) = con.read_line(options.idle_timeout)? else {
            con.print("\nIdle timeout.\n")?;
            return Ok(());
        };
        match input.trim().to_ascii_uppercase().as_str() {
            "" => {}
            "Q" => {
                con.print("Goodbye.\n")?;
                return Ok(());
            }
            "U" | ".." => {
                cwd.pop();
                page = 0;
            }
            "N" => page += 1,
            "P" => page = page.saturating_sub(1),
            choice => match choice.parse::<usize>() {
                Ok(n) if (1..=entries.len()).contains(&n) => {
                    let entry = &entries[n - 1];
                    match entry.kind {
                        Kind::Dir => {
                            cwd.push(&entry.name);
                            page = 0;
                        }
                        Kind::File { .. } => {
                            transfer(con, root, &cwd.join(&entry.name), &entry.name, options, log)?;
                        }
                    }
                }
                _ => con.print("Invalid choice.\n")?,
            },
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Protocol {
    Zmodem,
    Xmodem,
    Xmodem1k,
}

/// Offer `rel` for download. Transfer failures are reported to the user;
/// only link I/O errors are returned.
fn transfer(
    con: &mut Console,
    root: &Root,
    rel: &Path,
    name: &str,
    options: &Options,
    log: &mut dyn FnMut(&str),
) -> io::Result<()> {
    con.print(&format!(
        "\n{}\n  Z) ZMODEM (auto-download)\n  X) XMODEM\n  K) XMODEM-1K\n  C) Cancel\nProtocol [Z]: ",
        display_name(name)
    ))?;
    let Some(choice) = con.read_line(options.idle_timeout)? else {
        return Ok(());
    };
    let protocol = match choice.trim().to_ascii_uppercase().as_str() {
        "" | "Z" => Protocol::Zmodem,
        "X" => Protocol::Xmodem,
        "K" => Protocol::Xmodem1k,
        _ => return con.print("Cancelled.\n"),
    };
    let contents = match root.read_file(rel) {
        Ok(contents) => contents,
        Err(e) => {
            log(&format!("cannot read {}: {e}", rel.display()));
            return con.print(&format!("Cannot read file: {e}\n"));
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
            con.print(&format!("\nTransfer complete: {detail}.\n"))
        }
        Err(reason) => {
            log(&format!("transfer of {} failed: {reason}", rel.display()));
            con.print(&format!("\nTransfer failed: {reason}.\n"))?;
            match hint {
                Some(hint) => con.print(&wrap(hint, 78)),
                None => Ok(()),
            }
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
    display_name(&out)
}

/// Make a name safe for a plain ASCII terminal and fit it to the listing.
fn display_name(name: &str) -> String {
    let ascii: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_graphic() || c == ' ' {
                c
            } else {
                '?'
            }
        })
        .collect();
    if ascii.len() > NAME_WIDTH {
        format!("{}~", &ascii[..NAME_WIDTH - 1])
    } else {
        ascii
    }
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

    const PROMPT: &str = "Q)uit: ";

    /// root/{docs/readme.txt, data.bin (3000 bytes), small.txt}
    fn start(options: Options) -> Client {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir(tmp.path().join("docs")).unwrap();
        fs::write(tmp.path().join("docs/readme.txt"), b"hello").unwrap();
        fs::write(tmp.path().join("data.bin"), data()).unwrap();
        fs::write(tmp.path().join("small.txt"), b"small").unwrap();
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
    fn browse_and_quit() {
        let mut c = start(fast());
        let screen = c.expect(PROMPT);
        assert!(screen.contains("/   (page 1 of 1)"), "{screen}");
        assert!(screen.contains("   1) docs/"), "{screen}");
        assert!(screen.contains("   2) data.bin"), "{screen}");
        assert!(screen.contains("3000\r\n"), "{screen}");
        assert!(!screen.contains('\x1b'), "no ANSI sequences");
        c.send("1\r\n");
        let screen = c.expect(PROMPT);
        assert!(screen.contains("/docs   (page 1 of 1)"), "{screen}");
        assert!(screen.contains("1) readme.txt"), "{screen}");
        c.send("u\r");
        assert!(c.expect(PROMPT).contains("1) docs/"));
        c.send("q\r");
        c.expect("Goodbye.");
        c.finish().unwrap();
    }

    #[test]
    fn line_editing_and_invalid_input() {
        let mut c = start(fast());
        c.expect(PROMPT);
        c.send("9\x08\x089\r");
        c.expect("Invalid choice.");
        c.send("x\x7f\x01q\r");
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
    fn zmodem_download_returns_to_menu() {
        let mut c = start(fast());
        c.expect(PROMPT);
        c.send("2\r");
        c.expect("Protocol [Z]: ");
        c.send("\r");
        c.expect("five times to cancel.\r\n");
        let rx = zmodem::test_receiver::receive(&mut c.t, None);
        assert_eq!(rx.name, b"data.bin");
        assert_eq!(rx.data, data());
        c.expect("Transfer complete: 3000 bytes.");
        c.expect(PROMPT);
        c.send("q\r");
        c.finish().unwrap();
    }

    #[test]
    fn late_zfin_is_not_menu_input() {
        let mut c = start(fast());
        c.expect(PROMPT);
        c.send(
            "2

",
        );
        c.expect("five times to cancel.\r\n");
        zmodem::test_receiver::receive(&mut c.t, None);
        // As captured from HyperTerminal: a repeated ZFIN ~730 ms after "OO".
        thread::sleep(Duration::from_millis(550));
        c.t.write_all(b"**\x18B0800000000022d\r\x8a").unwrap();
        let out = c.expect(PROMPT);
        assert!(out.contains("Transfer complete"), "{out}");
        c.send("q\r");
        let out = c.expect("Goodbye.");
        assert!(!out.contains("Invalid choice"), "{out}");
        assert!(!out.contains("**B08"), "ZFIN echoed: {out}");
        c.finish().unwrap();
    }

    #[test]
    fn zmodem_skip_explains_and_returns_to_menu() {
        let mut c = start(fast());
        c.expect(PROMPT);
        c.send("2\rz\r");
        c.expect("five times to cancel.\r\n");
        zmodem::test_receiver::skip(&mut c.t);
        let out = c.expect(PROMPT);
        assert!(
            out.contains("Transfer failed: receiver skipped the file."),
            "{out}"
        );
        assert!(out.contains("Receive File >"), "{out}");
        assert!(out.lines().all(|l| l.len() <= 80), "{out}");
        c.send("q\r");
        c.finish().unwrap();
    }

    #[test]
    fn wrap_breaks_at_width() {
        assert_eq!(wrap("aaa bbb ccc", 7), "aaa bbb\nccc\n");
        assert_eq!(wrap("aaa", 2), "aaa\n");
    }

    #[test]
    fn zmodem_cancel_returns_to_menu() {
        let mut c = start(fast());
        c.expect(PROMPT);
        c.send("2\rz\r");
        c.expect("five times to cancel.\r\n");
        zmodem::test_receiver::receive(&mut c.t, Some(1));
        c.expect("Transfer failed: transfer cancelled by receiver.");
        c.expect(PROMPT);
        c.send("q\r");
        c.finish().unwrap();
    }

    #[test]
    fn xmodem_download_returns_to_menu() {
        let mut c = start(fast());
        c.expect(PROMPT);
        c.send("2\rx\r");
        c.expect("twice to cancel.\r\n");
        let rx = xmodem::tests::receive(&mut c.t, b'C', Default::default());
        assert_eq!(&rx.data[..3000], data());
        c.expect("padded to 3072; the original is exactly 3000 bytes.");
        c.expect(PROMPT);
        c.send("q\r");
        c.finish().unwrap();
    }

    #[test]
    fn no_receiver_is_reported() {
        let mut c = start(Options {
            start_timeout: Duration::from_millis(200),
            ..fast()
        });
        c.expect(PROMPT);
        c.send("3\rk\r");
        c.expect("Transfer failed: receiver did not start the transfer.");
        c.expect(PROMPT);
        c.send("q\r");
        c.finish().unwrap();
    }

    #[test]
    fn protocol_menu_cancel() {
        let mut c = start(fast());
        c.expect(PROMPT);
        c.send("3\rc\r");
        c.expect("Cancelled.");
        c.expect(PROMPT);
        c.send("q\r");
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
    fn paging() {
        let tmp = tempfile::tempdir().unwrap();
        for i in 0..20 {
            fs::write(tmp.path().join(format!("f{i:02}")), b"").unwrap();
        }
        let root = Root::new(tmp.path()).unwrap();
        let (mut server_end, client) = memory_pair();
        let server = thread::spawn(move || {
            session(
                &mut Console::new(&mut server_end),
                &root,
                &fast(),
                &mut |_| {},
            )
        });
        let mut c = Client {
            t: client,
            _tmp: tmp,
            server,
        };
        let screen = c.expect(PROMPT);
        assert!(screen.contains("(page 1 of 2)") && screen.contains("16) f15"));
        assert!(!screen.contains("17) f16"));
        c.send("n\r");
        let screen = c.expect(PROMPT);
        assert!(screen.contains("(page 2 of 2)") && screen.contains("20) f19"));
        c.send("n\r");
        assert!(c.expect(PROMPT).contains("(page 2 of 2)"));
        c.send("p\rq\r");
        assert!(c.expect(PROMPT).contains("(page 1 of 2)"));
        c.finish().unwrap();
    }

    #[test]
    fn display_name_is_ascii_and_truncated() {
        assert_eq!(display_name("caf\u{e9}"), "caf?");
        let long = "x".repeat(60);
        assert_eq!(display_name(&long).len(), NAME_WIDTH);
    }
}
