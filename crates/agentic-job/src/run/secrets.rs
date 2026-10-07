//! Keeps the values this process holds out of what it prints.
//!
//! The session's condensed transcript carries the agent's words, and the
//! agent can read its run token. The lines go to the job's log, so the
//! token is replaced on the way. This knows only exact values; the
//! secret-shaped strings of `redact` (step 4) are applied to the log and
//! to every file of the run in step 6b.

use std::io::{self, Write};

/// What a value is replaced by.
const MASK: &str = "***";
/// A value shorter than this is not replaced: it would match by chance.
const MIN_SECRET_LEN: usize = 8;

/// A writer that passes on whole lines, with every one of its secrets
/// replaced.
pub struct Masked<W: Write> {
    inner: W,
    secrets: Vec<String>,
    /// The line being written, until its newline.
    line: Vec<u8>,
}

impl<W: Write> Masked<W> {
    pub fn new(inner: W, secrets: impl IntoIterator<Item = String>) -> Self {
        Self {
            inner,
            secrets: secrets
                .into_iter()
                .filter(|secret| secret.len() >= MIN_SECRET_LEN)
                .collect(),
            line: Vec::new(),
        }
    }

    fn emit(&mut self, line: &[u8]) -> io::Result<()> {
        let text = String::from_utf8_lossy(line);
        let clean = self
            .secrets
            .iter()
            .fold(text.into_owned(), |text, secret| text.replace(secret, MASK));
        self.inner.write_all(clean.as_bytes())
    }
}

impl<W: Write> Write for Masked<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.line.extend_from_slice(buf);
        while let Some(end) = self.line.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.line.drain(..=end).collect();
            self.emit(&line)?;
        }
        Ok(buf.len())
    }

    /// What is held of an unfinished line stays held: a secret may be
    /// half written.
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl<W: Write> Drop for Masked<W> {
    fn drop(&mut self) {
        let rest = std::mem::take(&mut self.line);
        if !rest.is_empty() {
            let _ = self.emit(&rest);
        }
        let _ = self.inner.flush();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

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

    #[test]
    fn secrets_are_masked_even_when_written_in_pieces() {
        let out = Shared::default();
        let secrets = ["praxis-run-abcdef", "second-secret", "short"].map(str::to_owned);
        let mut masked = Masked::new(out.clone(), secrets);
        for piece in [
            "the token is praxis-run-",
            "abcdef, twice: praxis-run-abcdef\nsecond-",
            "secret and short\nno newline, second-secret",
        ] {
            masked.write_all(piece.as_bytes()).unwrap();
            masked.flush().unwrap();
        }
        let written = || String::from_utf8(out.0.lock().unwrap().clone()).unwrap();
        assert_eq!(written(), "the token is ***, twice: ***\n*** and short\n");
        drop(masked);
        assert_eq!(
            written(),
            "the token is ***, twice: ***\n*** and short\nno newline, ***"
        );
    }
}
