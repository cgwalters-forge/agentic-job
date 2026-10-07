//! Entering the sandbox: how the supervisor runs a command as the sandbox
//! user, with nothing of its own environment.
//!
//! `run0` starts the command as a transient systemd service through its
//! own PAM stack, so nothing of the caller's environment or cgroup comes
//! along, and `pam_systemd` gives it what an SSH login gets: a runtime
//! directory, the user's systemd manager and session bus, which rootless
//! podman relies on. The command runs in that session's scope, under the
//! user's slice, which is what the supervisor kills when the run ends.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::Shutdown;
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};

use anyhow::{Context, Result, ensure};

use super::host::{self, User};
use crate::config::{Config, Sandbox};

/// `run0` came with systemd 256.
pub const RUN0: &str = "run0";

/// `run0` sets these as sudo would, and tools that see them may act as if
/// run under sudo; `--setenv` cannot unset them.
const SUDO_VARS: &[&str] = &["SUDO_USER", "SUDO_UID", "SUDO_GID"];

/// What lets a process ask for the job's identity token. `run0` passes on
/// none of the caller's environment, so they never reach the sandbox user;
/// they are unset all the same, so that stays true if `run0` changes.
pub const OIDC_REQUEST_VARS: &[&str] = &[
    "ACTIONS_ID_TOKEN_REQUEST_URL",
    "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
];

const LANG: &str = "C.UTF-8";

/// What a command run in the sandbox left: its status and all it wrote.
#[derive(Debug)]
pub struct Output {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl Output {
    pub fn success(&self) -> bool {
        self.status.success()
    }
}

/// The way into the sandbox for one configuration.
#[derive(Debug, Clone)]
pub struct Entry {
    user: User,
    env: BTreeMap<String, String>,
}

impl Entry {
    /// The sandbox user of `config`, which `sandbox setup` has created.
    pub fn new(config: &Config) -> Result<Self> {
        config.sandbox.validate()?;
        let name = &config.sandbox.user;
        let user = User::lookup(name)?
            .with_context(|| format!("no user {name}: `agentic-job sandbox setup` creates it"))?;
        ensure!(user.uid != 0, "the sandbox user {name} is root by uid");
        Ok(Self::for_user(user, &config.sandbox))
    }

    fn for_user(user: User, sandbox: &Sandbox) -> Self {
        let fixed = [
            ("LANG".to_owned(), LANG.to_owned()),
            ("PATH".to_owned(), host::PATH_DIRS.join(":")),
        ];
        let env = fixed.into_iter().chain(sandbox.env.clone()).collect();
        Self { user, env }
    }

    pub fn user(&self) -> &User {
        &self.user
    }

    pub fn home(&self) -> &Path {
        &self.user.home
    }

    /// The `run0` command line for `argv`, in `cwd`, with `env` over the
    /// configured variables.
    fn run0_argv(
        &self,
        argv: &[String],
        cwd: &Path,
        env: &BTreeMap<String, String>,
    ) -> Result<Vec<String>> {
        ensure!(!argv.is_empty(), "an empty command for the sandbox");
        let forbidden: Vec<&str> = env
            .keys()
            .chain(self.env.keys())
            .filter(|name| name.starts_with(Sandbox::FORBIDDEN_ENV_PREFIX))
            .map(String::as_str)
            .collect();
        ensure!(
            forbidden.is_empty(),
            "refusing to pass {} to {}",
            forbidden.join(", "),
            self.user.name
        );
        let vars: BTreeMap<&String, &String> = self.env.iter().chain(env).collect();
        let head = [
            RUN0.to_owned(),
            "--pipe".to_owned(),
            "--no-ask-password".to_owned(),
            "--shell-prompt-prefix=".to_owned(),
            format!("--user={}", self.user.name),
            // Unlike `systemd-run --collect`, run0 leaves a failed unit
            // behind for every command that exits nonzero.
            "--property=CollectMode=inactive-or-failed".to_owned(),
            format!("--chdir={}", cwd.display()),
        ];
        let setenv = vars
            .iter()
            .map(|(name, value)| format!("--setenv={name}={value}"));
        let unset = SUDO_VARS
            .iter()
            .chain(OIDC_REQUEST_VARS)
            .flat_map(|name| ["-u".to_owned(), (*name).to_owned()]);
        Ok(head
            .into_iter()
            .chain(setenv)
            .chain(["--".to_owned(), "env".to_owned()])
            .chain(unset)
            .chain(["--".to_owned()])
            .chain(argv.iter().cloned())
            .collect())
    }

    /// `argv` as the sandbox user, in a login session of its own. The
    /// caller connects its standard streams, and they must be sockets:
    /// run0 hands them to PID 1 over D-Bus, which refuses regular files,
    /// and SELinux keeps PID 1 from reading a pipe this process made.
    /// [`Entry::run`] does that for a command that runs to its end.
    pub fn command(
        &self,
        argv: &[String],
        cwd: Option<&Path>,
        env: &BTreeMap<String, String>,
    ) -> Result<Command> {
        let run0 = self.run0_argv(argv, cwd.unwrap_or(self.home()), env)?;
        host::as_root(&run0)
    }

    /// Runs `argv` in the sandbox user's home, gives it `input`, and waits.
    pub fn run(&self, argv: &[&str], input: &[u8]) -> Result<Output> {
        let argv: Vec<String> = argv.iter().map(|&arg| arg.to_owned()).collect();
        let (mut stdin, child_stdin) = UnixStream::pair().context("socketpair")?;
        let (mut stdout, child_stdout) = UnixStream::pair().context("socketpair")?;
        let (mut stderr, child_stderr) = UnixStream::pair().context("socketpair")?;
        // The command object holds the child's ends: it has to be gone
        // before the reads below can see the streams close.
        let mut child = {
            let mut command = self.command(&argv, None, &BTreeMap::new())?;
            command
                .stdin(Stdio::from(OwnedFd::from(child_stdin)))
                .stdout(Stdio::from(OwnedFd::from(child_stdout)))
                .stderr(Stdio::from(OwnedFd::from(child_stderr)))
                .spawn()
                .with_context(|| format!("starting {RUN0} for {}", argv.join(" ")))?
        };
        let (out, err) = std::thread::scope(|scope| {
            scope.spawn(move || {
                // A command that reads none of its input is not an error.
                let _ = stdin.write_all(input);
                let _ = stdin.shutdown(Shutdown::Write);
            });
            let err = scope.spawn(move || {
                let mut bytes = Vec::new();
                let _ = stderr.read_to_end(&mut bytes);
                bytes
            });
            let mut out = Vec::new();
            let _ = stdout.read_to_end(&mut out);
            (out, err.join().unwrap_or_default())
        });
        let status = child.wait().context("waiting for run0")?;
        Ok(Output {
            status,
            stdout: out,
            stderr: err,
        })
    }

    /// Whether `argv` ran and succeeded as the sandbox user.
    pub fn succeeds(&self, argv: &[&str]) -> Result<bool> {
        Ok(self.run(argv, b"")?.success())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(env: &[(&str, &str)]) -> Entry {
        let sandbox = Sandbox {
            env: env
                .iter()
                .map(|&(k, v)| (k.to_owned(), v.to_owned()))
                .collect(),
            ..Sandbox::default()
        };
        let user = User {
            name: "agent".into(),
            uid: 1001,
            gid: 1001,
            home: "/home/agent".into(),
        };
        Entry::for_user(user, &sandbox)
    }

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|&s| s.to_owned()).collect()
    }

    #[test]
    fn run0_command_line() {
        let entry = entry(&[("PATH", "/opt/bin:/usr/bin"), ("HTTPS_PROXY", "http://p")]);
        let extra = BTreeMap::from([("LANG".to_owned(), "C".to_owned())]);
        let argv = entry
            .run0_argv(&strings(&["git", "status"]), Path::new("/work"), &extra)
            .unwrap();
        assert_eq!(
            argv,
            strings(&[
                "run0",
                "--pipe",
                "--no-ask-password",
                "--shell-prompt-prefix=",
                "--user=agent",
                "--property=CollectMode=inactive-or-failed",
                "--chdir=/work",
                "--setenv=HTTPS_PROXY=http://p",
                // The call's own value wins over the fixed one, and the
                // configuration's over the fixed PATH.
                "--setenv=LANG=C",
                "--setenv=PATH=/opt/bin:/usr/bin",
                "--",
                "env",
                "-u",
                "SUDO_USER",
                "-u",
                "SUDO_UID",
                "-u",
                "SUDO_GID",
                "-u",
                "ACTIONS_ID_TOKEN_REQUEST_URL",
                "-u",
                "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
                "--",
                "git",
                "status",
            ])
        );
    }

    #[test]
    fn the_jobs_variables_are_refused() {
        let cwd = Path::new("/");
        let cmd = strings(&["true"]);
        let leak = BTreeMap::from([("ACTIONS_RUNTIME_TOKEN".to_owned(), "x".to_owned())]);
        let err = entry(&[]).run0_argv(&cmd, cwd, &leak).unwrap_err();
        assert!(err.to_string().contains("ACTIONS_RUNTIME_TOKEN"), "{err}");
        // Also when the configuration, which validation should have
        // refused, carries one.
        let configured = entry(&[("ACTIONS_CACHE_URL", "x")]);
        assert!(configured.run0_argv(&cmd, cwd, &BTreeMap::new()).is_err());
        assert!(entry(&[]).run0_argv(&[], cwd, &BTreeMap::new()).is_err());
    }
}
