// SPDX-License-Identifier: MIT
//! Byte-level trace of a link, for diagnosing transfer problems.
//!
//! Each line has the time since the trace started, a direction (`>` sent,
//! `<` received, `#` note), up to 16 bytes in hex, and the same bytes as
//! printable ASCII:
//!
//! ```text
//!     1.204113 > 2a 2a 18 42 30 30 30 30 30 30 30 30 30 30 30 30  |**.B000000000000|
//!     1.260071 < 2a 2a 18 42 30 31 30 30 30 30 30 30 32 33 62 65  |**.B0100000023be|
//!     1.302518 # session started
//! ```
//!
//! Received bytes arrive one at a time; consecutive ones are gathered into a
//! line until 16 accumulate, a read times out, or something is sent.

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::transport::Transport;

const PER_LINE: usize = 16;

/// A shared trace sink; clones write to the same output.
#[derive(Clone)]
pub struct Trace {
    state: Arc<Mutex<State>>,
}

struct State {
    out: Box<dyn Write + Send>,
    start: Instant,
    /// Received bytes not yet written, and when the first of them arrived.
    pending: Vec<u8>,
    pending_at: Duration,
}

impl Trace {
    /// Append a trace to the file at `path`.
    pub fn create(path: &Path) -> io::Result<Self> {
        let file = File::options().create(true).append(true).open(path)?;
        Ok(Self::new(BufWriter::new(file)))
    }

    pub fn new(out: impl Write + Send + 'static) -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                out: Box::new(out),
                start: Instant::now(),
                pending: Vec::new(),
                pending_at: Duration::ZERO,
            })),
        }
    }

    /// Record a free-form note, such as a log message.
    pub fn note(&self, message: &str) {
        let mut state = self.lock();
        state.flush_received();
        let at = state.start.elapsed();
        for line in message.lines() {
            state.write_line(at, '#', line);
        }
    }

    /// Wrap `inner` so that all traffic through it is recorded.
    pub fn wrap(&self, inner: Box<dyn Transport>) -> Box<dyn Transport> {
        Box::new(TraceTransport {
            inner,
            trace: self.clone(),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        // A panic while tracing must not hide the original error.
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl State {
    fn received(&mut self, byte: u8) {
        if self.pending.is_empty() {
            self.pending_at = self.start.elapsed();
        }
        self.pending.push(byte);
        if self.pending.len() == PER_LINE {
            self.flush_received();
        }
    }

    fn flush_received(&mut self) {
        if !self.pending.is_empty() {
            let bytes = std::mem::take(&mut self.pending);
            let at = self.pending_at;
            self.write_line(at, '<', &format_bytes(&bytes));
        }
    }

    fn sent(&mut self, buf: &[u8]) {
        self.flush_received();
        let at = self.start.elapsed();
        for chunk in buf.chunks(PER_LINE) {
            self.write_line(at, '>', &format_bytes(chunk));
        }
    }

    fn write_line(&mut self, at: Duration, direction: char, text: &str) {
        // Tracing is best effort; a full disk must not break the transfer.
        let _ = writeln!(self.out, "{:>12.6} {direction} {text}", at.as_secs_f64());
        let _ = self.out.flush();
    }
}

fn format_bytes(bytes: &[u8]) -> String {
    let hex: Vec<String> = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let ascii: String = bytes
        .iter()
        .map(|&b| {
            if b.is_ascii_graphic() || b == b' ' {
                b as char
            } else {
                '.'
            }
        })
        .collect();
    format!(
        "{:<width$}  |{ascii}|",
        hex.join(" "),
        width = PER_LINE * 3 - 1
    )
}

struct TraceTransport {
    inner: Box<dyn Transport>,
    trace: Trace,
}

impl Transport for TraceTransport {
    fn read_byte(&mut self, timeout: Duration) -> io::Result<Option<u8>> {
        let result = self.inner.read_byte(timeout);
        let mut state = self.trace.lock();
        match &result {
            Ok(Some(byte)) => state.received(*byte),
            Ok(None) => state.flush_received(),
            Err(e) => {
                state.flush_received();
                let at = state.start.elapsed();
                state.write_line(at, '#', &format!("read error: {e}"));
            }
        }
        result
    }

    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        self.trace.lock().sent(buf);
        let result = self.inner.write_all(buf);
        if let Err(e) = &result {
            self.trace.note(&format!("write error: {e}"));
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::memory_pair;

    /// A `Write` whose contents the test can read back.
    #[derive(Clone, Default)]
    struct Shared(Arc<Mutex<Vec<u8>>>);

    impl Write for Shared {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn lines(out: &Shared) -> Vec<String> {
        String::from_utf8(out.0.lock().unwrap().clone())
            .unwrap()
            .lines()
            .map(|l| {
                let l = l.trim_start();
                l.split_once(' ').map_or(l, |(_, rest)| rest).to_string()
            })
            .collect()
    }

    #[test]
    fn records_both_directions_and_notes() {
        let out = Shared::default();
        let trace = Trace::new(out.clone());
        let (a, mut b) = memory_pair();
        let mut t = trace.wrap(Box::new(a));

        b.write_all(b"**\x18B").unwrap();
        let wait = Duration::from_millis(50);
        for _ in 0..4 {
            t.read_byte(wait).unwrap();
        }
        // Sending ends the received line.
        t.write_all(b"OO").unwrap();
        trace.note("done");

        assert_eq!(
            lines(&out),
            [
                format!("< {:<47}  |**.B|", "2a 2a 18 42"),
                format!("> {:<47}  |OO|", "4f 4f"),
                "# done".to_string(),
            ]
        );
    }

    #[test]
    fn long_runs_are_split() {
        let out = Shared::default();
        let trace = Trace::new(out.clone());
        let (a, _b) = memory_pair();
        let mut t = trace.wrap(Box::new(a));
        t.write_all(&[0u8; 20]).unwrap();
        let lines = lines(&out);
        assert_eq!(lines.len(), 2);
        assert!(lines[1].starts_with("> 00 00 00 00 "));
    }

    #[test]
    fn timeout_ends_received_line() {
        let out = Shared::default();
        let trace = Trace::new(out.clone());
        let (a, mut b) = memory_pair();
        let mut t = trace.wrap(Box::new(a));
        let wait = Duration::from_millis(20);
        b.write_all(b"C").unwrap();
        t.read_byte(wait).unwrap();
        assert!(lines(&out).is_empty());
        assert_eq!(t.read_byte(wait).unwrap(), None);
        assert_eq!(lines(&out), [format!("< {:<47}  |C|", "43")]);
    }
}
