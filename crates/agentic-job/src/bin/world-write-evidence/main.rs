//! world-write-evidence: the privileged acceptance proof of the world-write
//! policy, published as a bounded step summary. Fresh disposable hosts only:
//! never run its `host` or evidence modes inside an existing sandbox.
//!
//! ```text
//! world-write-evidence OUT AGENTIC_JOB          # needs GITHUB_STEP_SUMMARY
//! world-write-evidence host before|after        # as root
//! world-write-evidence worker DIR DENIED        # as the unprivileged worker
//! world-write-evidence append PATH true|false   # as the unprivileged worker
//! ```
//!
//! The evidence mode admits the host, records its source, installs the
//! packages, builds the object, then runs `sudo` this executable's `host
//! before`, loads the policy with AGENTIC_JOB, and runs `host after`. The
//! host mode stages a copy of this executable for the worker, which runs
//! the operation matrix (`worker`) and the owner-write check (`append`);
//! DENIED is `true`, `false` or comma-separated operation names.

mod cases;
mod command;
mod evidence;
mod host;

use anyhow::{Context, Result, bail};
use std::time::Instant;

const SUMMARY_ENV: &str = "GITHUB_STEP_SUMMARY";
const OS_RELEASE: &str = "/etc/os-release";
const USAGE: &str = "usage: world-write-evidence OUT AGENTIC_JOB | host before|after | \
                     worker DIR DENIED | append PATH true|false";

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        ["host", mode] => host::host(mode),
        ["worker", directory, denied] => host::worker(directory, denied),
        ["append", path, expected] => host::append(path, expected),
        [out, binary] => {
            let summary = std::env::var(SUMMARY_ENV).unwrap_or_default();
            if out.is_empty() || binary.is_empty() || summary.is_empty() {
                bail!("requires output directory, agentic-job binary and {SUMMARY_ENV}");
            }
            let binary = std::path::absolute(binary).context("resolving AGENTIC_JOB")?;
            let exe = std::env::current_exe().context("locating this executable")?;
            let os_release = std::fs::read_to_string(OS_RELEASE)
                .with_context(|| format!("reading {OS_RELEASE}"))?;
            let mut real = evidence::RealHost {
                summary,
                start: Instant::now(),
            };
            evidence::evidence(
                out,
                &binary.to_string_lossy(),
                &exe.to_string_lossy(),
                &os_release,
                &mut real,
            )
        }
        _ => bail!("{USAGE}"),
    }
}
