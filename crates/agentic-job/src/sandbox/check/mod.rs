//! `agentic-job sandbox check`: prove, as the sandbox user, that the
//! sandbox holds, before an agent is started in it.
//!
//! Each probe is something the sandbox user tries and must not manage,
//! and each has a positive control: the same attempt somewhere it should
//! work. Without the control, a probe that fails because a program is
//! missing reads as a protection that holds.
//!
//! The probes are a function, [`probes`], because `run` repeats them once
//! the run token is in place; the command is that function with no token.
//! docs/sandbox-check.md lists them.
//!
//! The probes are methods of `Checker` in the modules under this one:
//! `on_host`, `run_token`, `runner` (the runner's own user, which setup
//! has taken root from) and `net`.

use std::collections::BTreeSet;
use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result, ensure};

use super::enter::{Entry, Output};
use super::host::{self, User};
use super::root::Root;
use crate::config::{self, Config};
use crate::exit::Exit;

mod net;
mod on_host;
mod run_token;
mod runner;

#[derive(Debug, clap::Args)]
pub struct Args {}

const CANARY_BYTES: usize = 12;

/// Put before every command a probe runs as the sandbox user.
const PROBE_LIMIT: &[&str] = &["timeout", "--kill-after=10", "600"];

/// Any uid other than root in a rootless container maps to a subordinate
/// uid of the sandbox user.
const CONTAINER_UID: &str = "1000";

const CA_MOUNT: &str = "/etc/egress-proxy/ca.pem:/run/egress-ca.pem:ro";

/// The run token and the one place the sandbox user may hold it. `run`
/// passes this once it has put the token there.
#[derive(Debug, Clone, Copy)]
pub struct RunToken<'a> {
    /// The runner's copy of the token.
    pub runner_file: &'a Path,
    /// The agent's configuration file in the sandbox user's home, the
    /// only file of that user's that holds the token.
    pub agent_config: &'a Path,
}

/// What one probe or control found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// Stable across runs: tests and readers of the log match on it.
    pub id: String,
    pub what: String,
    pub passed: bool,
}

/// Every outcome of one pass over the probes, in order.
#[derive(Debug, Default)]
pub struct Report {
    outcomes: Vec<Outcome>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Want {
    Succeed,
    Fail,
}

impl Report {
    /// Records that the attempt `id` did or did not succeed, and prints
    /// the line at once: some probes take a while.
    fn expect(&mut self, want: Want, id: &str, what: impl Into<String>, succeeded: bool) {
        let (what, passed) = (what.into(), succeeded == (want == Want::Succeed));
        println!("{}: [{id}] {what}", if passed { "ok" } else { "FAIL" });
        self.outcomes.push(Outcome {
            id: id.to_owned(),
            what,
            passed,
        });
    }

    fn note(&self, text: &str) {
        println!("note: {text}");
    }

    pub fn outcomes(&self) -> &[Outcome] {
        &self.outcomes
    }

    /// The ids of what did not come out as it must.
    pub fn failures(&self) -> BTreeSet<&str> {
        self.outcomes
            .iter()
            .filter(|outcome| !outcome.passed)
            .map(|outcome| outcome.id.as_str())
            .collect()
    }

    pub fn passed(&self) -> bool {
        self.outcomes.iter().all(|outcome| outcome.passed)
    }
}

pub fn run(_args: &Args) -> Result<Exit> {
    ensure!(
        !host::is_root(),
        "`sandbox check` runs as the runner's user, which it compares the sandbox user with; it enters the sandbox through the helper"
    );
    let config = Config::load(Path::new(config::ROOT_COPY))
        .context("`agentic-job sandbox setup` writes this file")?;
    let report = probes(&config, None)?;
    let failures = report.failures();
    if failures.is_empty() {
        println!("{} sandbox checks passed", report.outcomes().len());
        return Ok(Exit::Success);
    }
    let ids: Vec<&str> = failures.into_iter().collect();
    eprintln!(
        "error: {} sandbox check(s) failed: {}",
        ids.len(),
        ids.join(", ")
    );
    Ok(Exit::Failure)
}

/// Probes the sandbox `config` describes, as its sandbox user, from the
/// runner's user. `Err` is a probe that could not be made at all; a
/// probe that came out wrong is in the report.
pub fn probes(config: &Config, token: Option<RunToken<'_>>) -> Result<Report> {
    let entry = Entry::new(config)?;
    let checker = Checker {
        config,
        root: entry.root(),
        entry,
        runner: User::current()?,
        canary: canary()?,
        report: Report::default(),
    };
    checker.run(token)
}

/// A value nothing else on the host has.
fn canary() -> Result<String> {
    let mut bytes = [0u8; CANARY_BYTES];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .context("reading /dev/urandom")?;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    Ok(format!("isolation-canary-{hex}"))
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

struct Checker<'a> {
    config: &'a Config,
    entry: Entry,
    /// How root is had for the controls that need it.
    root: Root,
    runner: User,
    canary: String,
    report: Report,
}

impl Checker<'_> {
    fn run(mut self, token: Option<RunToken<'_>>) -> Result<Report> {
        self.privileges()?;
        if self.config.sandbox.lock_runner {
            self.runner_privileges()?;
        }
        let environs = self.environments()?;
        self.job_variables(&environs, token.is_some())?;
        self.files()?;
        self.schedulers()?;
        self.services();
        self.local_services()?;
        self.tailscaled()?;
        if let Some(token) = token {
            self.run_token(token, &environs)?;
        }
        self.network()?;
        Ok(self.report)
    }

    fn user(&self) -> &str {
        &self.entry.user().name
    }

    /// Runs `argv` as the sandbox user, with a limit on its time: a probe
    /// that hangs has to fail the check, not hold the job.
    fn sandbox(&self, argv: &[&str], input: &[u8]) -> Result<Output> {
        let limited: Vec<&str> = PROBE_LIMIT.iter().chain(argv).copied().collect();
        self.entry.run(&limited, input)
    }

    fn sandbox_succeeds(&self, argv: &[&str]) -> Result<bool> {
        Ok(self.sandbox(argv, b"")?.success())
    }

    /// Diagnostics for positive controls only: expected refusals are not errors.
    fn sandbox_diagnosed(&self, id: &str, argv: &[&str], input: &[u8]) -> Result<bool> {
        let output = self.sandbox(argv, input)?;
        if !output.success() {
            self.report.note(&format!(
                "{id}: command {argv:?} exited {}; stderr: {}",
                output.status,
                bounded_stderr(&output.stderr),
            ));
        }
        Ok(output.success())
    }

    fn has_podman(&self) -> bool {
        host::has_program("podman")
    }

    /// Runs `argv` in a rootless container on the host's network, as a
    /// subordinate uid; with the egress proxy, its authority is there too.
    fn in_container(&self, argv: &[&str]) -> Result<bool> {
        Ok(self.sandbox(&self.container(argv), b"")?.success())
    }

    fn container<'a>(&'a self, argv: &[&'a str]) -> Vec<&'a str> {
        let mount: &[&str] = if self.config.egress.proxy {
            &["--security-opt", "label=disable", "-v", CA_MOUNT]
        } else {
            &[]
        };
        let parts: [&[&str]; 4] = [
            &[
                "podman",
                "run",
                "--rm",
                "--network=host",
                "--user",
                CONTAINER_UID,
            ],
            mount,
            &[self.config.sandbox.check.container_image.as_str()],
            argv,
        ];
        parts.concat()
    }

    /// A control for something the sandbox user must not reach: the
    /// runner's user reaches it. Where it does not either (another
    /// cloud, a network that already refuses it), there is no control,
    /// and the probe says less; that is noted, not failed.
    fn control_or_note(&mut self, id: &str, what: &str, argv: &[&str]) {
        let reached = host::command(argv)
            .map(|mut command| host::succeeds(&mut command))
            .unwrap_or(false);
        if reached {
            self.report.expect(
                Want::Succeed,
                id,
                format!("{} reaches {what} (control)", self.runner.name),
                true,
            );
        } else {
            self.report.note(&format!(
                "{} can't reach {what} either (no control)",
                self.runner.name
            ));
        }
    }
}

const DIAGNOSTIC_BYTES: usize = 4096;

fn bounded_stderr(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(&stderr[..stderr.len().min(DIAGNOSTIC_BYTES)]);
    // Escape terminal/control characters, including newlines, so a subprocess
    // cannot forge another probe result or a workflow command in the log.
    let mut shown: String = text.chars().flat_map(char::escape_default).collect();
    if stderr.len() > DIAGNOSTIC_BYTES {
        shown.push_str(" [truncated]");
    }
    shown
}

#[cfg(test)]
mod diagnostic_tests {
    use super::*;

    #[test]
    fn stderr_is_bounded_and_escaped() {
        for (input, expected) in [
            (b"".as_slice(), ""),
            (b"denied\n\x1b".as_slice(), "denied\\n\\u{1b}"),
            (b"\xff".as_slice(), "\\u{fffd}"),
        ] {
            assert_eq!(bounded_stderr(input), expected);
        }
        assert_eq!(
            bounded_stderr(&vec![b'x'; DIAGNOSTIC_BYTES]),
            "x".repeat(DIAGNOSTIC_BYTES)
        );
        assert_eq!(
            bounded_stderr(&vec![b'x'; DIAGNOSTIC_BYTES + 1]),
            format!("{} [truncated]", "x".repeat(DIAGNOSTIC_BYTES))
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_search() {
        assert!(contains(b"A=1\0HOME=/home/agent\0", b"HOME=/home/agent"));
        assert!(!contains(b"A=1\0", b"HOME="));
        assert!(!contains(b"short", b"much longer than the text"));
        // An empty needle must never read as "found".
        assert!(!contains(b"anything", b""));
    }

    #[test]
    fn a_report_fails_on_a_wrong_outcome_of_either_kind() {
        let mut report = Report::default();
        report.expect(Want::Fail, "sudo", "no sudo", false);
        report.expect(Want::Succeed, "sudo-control", "runner has sudo", true);
        assert!(report.passed());
        // The protection is gone.
        report.expect(Want::Fail, "cron", "no crontab", true);
        // The control did not work, so its probe proves nothing.
        report.expect(Want::Succeed, "cron-control", "runner may", false);
        assert!(!report.passed());
        assert_eq!(
            report.failures().into_iter().collect::<Vec<_>>(),
            ["cron", "cron-control"]
        );
    }

    #[test]
    fn canaries_differ() {
        let (a, b) = (canary().unwrap(), canary().unwrap());
        assert_ne!(a, b);
        assert_eq!(a.len(), "isolation-canary-".len() + 2 * CANARY_BYTES);
    }
}
