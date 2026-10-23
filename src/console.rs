// SPDX-License-Identifier: MIT
//! Line-oriented access to a dumb terminal over a [`Transport`]: echo,
//! Backspace, Tab completion, history, and Ctrl-C checks during long
//! output. Output is plain ASCII with CR LF line endings and no ANSI
//! sequences, so it suits vintage terminal programs and, in raw mode, a
//! Linux terminal.

use std::collections::VecDeque;
use std::io;
use std::time::Duration;

use crate::transport::Transport;

/// Screen width used to lay out completion choices.
pub(crate) const SCREEN_WIDTH: usize = 80;
/// Longest accepted input line.
const MAX_INPUT: usize = 200;
/// Commands remembered for recall with the arrow keys.
const HISTORY: usize = 50;

/// Computes the [`Completion`] for an input line.
pub(crate) type Completer<'a> = &'a mut dyn FnMut(&str) -> Completion;

/// What Tab does to the input line.
pub(crate) struct Completion {
    /// The line after completion; unchanged if nothing could be added.
    pub line: String,
    /// Possible names to show when the line could not be extended.
    pub choices: Vec<String>,
}

/// Line-oriented access to a dumb terminal.
pub(crate) struct Console<'a> {
    pub t: &'a mut dyn Transport,
    /// The previous byte was CR, so a following LF is part of the same newline.
    after_cr: bool,
    /// Keys typed while output was being sent, kept for the next prompt.
    typeahead: VecDeque<u8>,
    /// Earlier input lines, oldest first, recalled with Up and Down.
    pub history: Vec<String>,
}

/// How [`Console::read_line`] ended.
pub(crate) enum Input {
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
    pub fn new(t: &'a mut dyn Transport) -> Self {
        Self {
            t,
            after_cr: false,
            typeahead: VecDeque::new(),
            history: Vec::new(),
        }
    }

    pub fn print(&mut self, text: &str) -> io::Result<()> {
        self.t.write_all(text.replace('\n', "\r\n").as_bytes())
    }

    /// Read a line with echo and backspace editing; a non-empty line is
    /// added to the history. With `editor` (the prompt, to redraw after
    /// listing choices, and a completer), Tab completes, Up and Down (or
    /// Ctrl-P and Ctrl-N) step through the history, and Ctrl-D on an empty
    /// line ends input, as in a shell. Without it, every other control byte,
    /// including ESC, is ignored on its own, so no key sequence can leave
    /// input waiting.
    pub fn read_line(
        &mut self,
        idle: Duration,
        mut editor: Option<(&str, Completer)>,
    ) -> io::Result<Input> {
        let editing = editor.is_some();
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
                _ if !editing => byte,
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
                0x04 if editing && line.is_empty() => {
                    self.print("\n")?;
                    return Ok(Input::Eof);
                }
                // Ctrl-P: previous line; Ctrl-N: next line.
                0x10 | 0x0e if editing => {
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
                    let Some((prompt, complete)) = editor.as_mut() else {
                        continue;
                    };
                    let done = (*complete)(&line);
                    if done.line != line && done.line.len() <= MAX_INPUT {
                        self.retype(&line, &done.line)?;
                        line = done.line;
                    } else if done.choices.is_empty() {
                        self.t.write_all(b"\x07")?;
                    } else {
                        let choices = columns(&done.choices, SCREEN_WIDTH);
                        let redraw = format!("\n{choices}{prompt}{line}");
                        self.print(&redraw)?;
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
    pub fn interrupted(&mut self) -> io::Result<bool> {
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
    pub fn settle(&mut self, quiet: Duration) -> io::Result<()> {
        self.t.drain(quiet)?;
        self.typeahead.clear();
        self.after_cr = false;
        Ok(())
    }
}

/// Lay out `names` in columns across `width`, one row per line.
pub(crate) fn columns(names: &[String], width: usize) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_fit_width() {
        let names: Vec<String> = ["aa", "bbbb", "c"].map(String::from).into();
        assert_eq!(columns(&names, 80), "aa    bbbb  c\n");
        assert_eq!(columns(&names, 12), "aa    bbbb\nc\n");
    }
}
