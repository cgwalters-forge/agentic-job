//! Keep untrusted log text from becoming GitHub Actions workflow commands.

use std::io::{Read, Write};

use anyhow::{Context, Result};

/// A private, per-invocation command suspension. Neither the token nor this
/// writer is passed to the child. Drop also covers errors and cancellation.
pub(crate) struct Suspended<W: Write> {
    writer: W,
    token: String,
}

impl<W: Write> Suspended<W> {
    pub(crate) fn new(writer: W) -> Result<Self> {
        let mut random = [0; 32];
        std::fs::File::open("/dev/urandom")
            .context("opening the workflow-command token source")?
            .read_exact(&mut random)
            .context("reading the workflow-command token")?;
        let token: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
        let mut guard = Self { writer, token };
        // One write each, so that a concurrent writer cannot split the line.
        let stop = format!("\n::stop-commands::{}\n", guard.token);
        guard
            .writer
            .write_all(stop.as_bytes())
            .and_then(|()| guard.writer.flush())
            .context("suspending workflow commands")?;
        Ok(guard)
    }
}

impl<W: Write> Write for Suspended<W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.writer.write(bytes)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.writer.flush()
    }
}

impl<W: Write> Drop for Suspended<W> {
    fn drop(&mut self) {
        // Terminate a partial line (including one ending in CR) before the
        // resume command. All relays must have stopped before dropping us.
        let resume = format!("\n::{}::\n", self.token);
        let _ = self.writer.write_all(resume.as_bytes());
        let _ = self.writer.flush();
    }
}

/// Breaks `::` and `##[` with a space, also where they span two calls on
/// one stream. The runner only reads a command at the start of a line, after
/// leading whitespace (`TryParseV2` after `TrimStart`); breaking them anywhere
/// is defense in depth, and no simpler to get right than line tracking.
/// Used for stderr and constructed log lines: they need no byte fidelity and
/// cannot race a suspension/resumption on the other standard stream.
#[derive(Debug, Default)]
pub(crate) struct Escaper {
    last: [u8; 2],
}

impl Escaper {
    /// Whether a space goes before `byte`, given what came before it.
    fn space_before(&mut self, byte: u8) -> bool {
        let space = match byte {
            b':' => self.last[1] == b':',
            b'[' => self.last == *b"##",
            _ => false,
        };
        self.last = [if space { b' ' } else { self.last[1] }, byte];
        space
    }

    /// `bytes`, escaped, appended to `shown`. No UTF-8 assumption: the bytes
    /// matched are ASCII, which never occurs inside a multibyte sequence.
    fn push(&mut self, bytes: &[u8], shown: &mut Vec<u8>) {
        for &byte in bytes {
            if self.space_before(byte) {
                shown.push(b' ');
            }
            shown.push(byte);
        }
    }
}

/// A writer that passes on what it is given through one [`Escaper`].
pub(crate) struct Escaping<W: Write> {
    writer: W,
    escaper: Escaper,
    shown: Vec<u8>,
}

impl<W: Write> Escaping<W> {
    pub(crate) fn new(writer: W) -> Self {
        Self {
            writer,
            escaper: Escaper::default(),
            shown: Vec::new(),
        }
    }
}

impl<W: Write> Write for Escaping<W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.shown.clear();
        self.escaper.push(bytes, &mut self.shown);
        self.writer.write_all(&self.shown)?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.writer.flush()
    }
}

/// `text` with both command syntaxes broken; see [`Escaper`].
pub(crate) fn escaped(text: &str) -> String {
    let mut escaper = Escaper::default();
    let mut shown = String::with_capacity(text.len());
    for c in text.chars() {
        // Other characters only need to reset the state.
        if escaper.space_before(u8::try_from(c).unwrap_or(0)) {
            shown.push(' ');
        }
        shown.push(c);
    }
    shown
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrapper_resumes_after_partial_lines_and_errors() {
        for payload in [
            "::set-output name=a::b",
            "::add-mask::hidden\r",
            "partial",
            "",
        ] {
            let mut bytes = Vec::new();
            {
                let mut log = Suspended::new(&mut bytes).unwrap();
                log.write_all(payload.as_bytes()).unwrap();
            }
            let text = String::from_utf8(bytes).unwrap();
            let mut lines = text.lines();
            assert_eq!(lines.next(), Some(""));
            let stop = lines.next().unwrap();
            let token = stop.strip_prefix("::stop-commands::").unwrap();
            assert_eq!(token.len(), 64);
            assert!(text.ends_with(&format!("\n::{token}::\n")));
            assert!(text.contains(payload));
        }
    }

    #[test]
    fn tokens_are_fresh_and_escaping_breaks_both_syntaxes() {
        let (mut a, mut b) = (Vec::new(), Vec::new());
        drop(Suspended::new(&mut a).unwrap());
        drop(Suspended::new(&mut b).unwrap());
        assert_ne!(a, b);
        for (text, expected) in [
            ("::set-output name=a::b", ": :set-output name=a: :b"),
            ("::::add-mask::x\r\n", ": : : :add-mask: :x\r\n"),
            ("prefix ##[error]x", "prefix ## [error]x"),
            ("###[x] #[y] ##x", "### [x] #[y] ##x"),
            // Only the two syntaxes: URLs, times and single `#` stay.
            ("https://x/#a 12:34", "https://x/#a 12:34"),
            ("é::ü##[", "é: :ü## ["),
        ] {
            assert_eq!(escaped(text), expected);
            assert_eq!(escaped(expected), expected, "idempotent");
            // The same, a byte at a time, as stderr arrives.
            let mut shown = Escaping::new(Vec::new());
            for byte in text.as_bytes() {
                shown.write_all(std::slice::from_ref(byte)).unwrap();
            }
            assert_eq!(shown.writer, expected.as_bytes());
        }
    }
}
