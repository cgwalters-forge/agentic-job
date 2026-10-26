//! Running a command to completion with a deadline, as Node's
//! execFileSync did for the scripts this replaces.

use anyhow::{Context, Result, bail};
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// The most captured output kept from either stream; more fails the command.
pub const MAX_OUTPUT_BYTES: u64 = 1024 * 1024;
const POLL: Duration = Duration::from_millis(20);

/// Reads a stream up to the limit, then drains it so the child never
/// blocks on a full pipe; `overflow` tells the waiting thread to stop it.
fn reader(
    stream: Option<impl Read + Send + 'static>,
    overflow: Arc<AtomicBool>,
) -> JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut stream) = stream {
            // A read error only shortens the output; the exit status decides.
            let mut limited = stream.by_ref().take(MAX_OUTPUT_BYTES + 1);
            let _ = limited.read_to_end(&mut buf);
            if buf.len() as u64 > MAX_OUTPUT_BYTES {
                overflow.store(true, Ordering::Relaxed);
                let _ = std::io::copy(&mut stream, &mut std::io::sink());
            }
        }
        buf
    })
}

fn stop(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Runs `program` with `args`, failing on a non-zero exit, a signal, or
/// after `timeout`. With `capture`, returns standard output (lossily
/// decoded) and passes standard error through after the command ends,
/// including it in the failure message; otherwise both are inherited.
pub fn run(program: &str, args: &[&str], capture: bool, timeout: Duration) -> Result<String> {
    let line = std::iter::once(program)
        .chain(args.iter().copied())
        .collect::<Vec<_>>()
        .join(" ");
    let mut command = Command::new(program);
    command.args(args).stdin(Stdio::inherit());
    if capture {
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
    }
    let spawned = command.spawn();
    let mut child = spawned.with_context(|| format!("spawning {program}"))?;
    let overflow = Arc::new(AtomicBool::new(false));
    let stdout = reader(child.stdout.take(), overflow.clone());
    let stderr = reader(child.stderr.take(), overflow.clone());
    let deadline = Instant::now() + timeout;
    let status = loop {
        let waited = child.try_wait();
        if let Some(status) = waited.with_context(|| format!("waiting for {line}"))? {
            break status;
        }
        if overflow.load(Ordering::Relaxed) {
            stop(&mut child);
            bail!("{line}: output exceeded {MAX_OUTPUT_BYTES} bytes");
        }
        if Instant::now() >= deadline {
            stop(&mut child);
            bail!("{line}: timed out after {} s", timeout.as_secs());
        }
        std::thread::sleep(POLL);
    };
    let join = |handle: JoinHandle<Vec<u8>>| handle.join().unwrap_or_default();
    let (stdout, stderr) = (join(stdout), join(stderr));
    if overflow.load(Ordering::Relaxed) {
        bail!("{line}: output exceeded {MAX_OUTPUT_BYTES} bytes");
    }
    let stderr = String::from_utf8_lossy(&stderr);
    eprint!("{stderr}");
    if !status.success() {
        bail!("Command failed: {line} ({status})\n{stderr}");
    }
    Ok(String::from_utf8_lossy(&stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_output_and_fails_closed() {
        let t = Duration::from_secs(10);
        assert_eq!(run("echo", &["a", "b"], true, t).unwrap(), "a b\n");
        assert_eq!(run("true", &[], false, t).unwrap(), "");
        let err = run("sh", &["-c", "echo oops >&2; exit 3"], true, t).unwrap_err();
        assert!(err.to_string().contains("oops"), "{err:#}");
        assert!(run("/nonexistent/program", &[], true, t).is_err());
        let err = run("sleep", &["5"], true, Duration::from_millis(100)).unwrap_err();
        assert!(err.to_string().contains("timed out"), "{err:#}");
        let err = run("head", &["-c", "1048577", "/dev/zero"], true, t).unwrap_err();
        assert!(err.to_string().contains("exceeded"), "{err:#}");
    }
}
