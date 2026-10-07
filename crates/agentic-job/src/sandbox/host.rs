//! What `sandbox setup` and `sandbox check` both need of the host: who a
//! user is, whether a program is there, and running one with its failure
//! turned into an error that names it.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use anyhow::{Context, Result, bail, ensure};

/// Where programs are looked for. A fixed list and not the caller's `PATH`:
/// `sandbox setup` runs as root, and the same list is the sandbox user's
/// `PATH` (see [`super::enter`]).
pub const PATH_DIRS: &[&str] = &["/usr/local/bin", "/usr/bin", "/bin"];

/// Root's programs, which the list above leaves out on some hosts.
const SBIN_DIRS: &[&str] = &["/usr/local/sbin", "/usr/sbin", "/sbin"];

/// A user, as `getent passwd` describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct User {
    pub name: String,
    pub uid: u32,
    pub gid: u32,
    pub home: PathBuf,
}

impl User {
    /// One line of passwd(5).
    fn parse(line: &str) -> Result<Self> {
        let fields: Vec<&str> = line.trim_end().split(':').collect();
        let [name, _, uid, gid, _, home, _] = fields[..] else {
            bail!("not a passwd line: {line:?}");
        };
        Ok(Self {
            name: name.to_owned(),
            uid: uid.parse().with_context(|| format!("uid of {name}"))?,
            gid: gid.parse().with_context(|| format!("gid of {name}"))?,
            home: PathBuf::from(home),
        })
    }

    /// The user named or numbered `key`, or `None` when there is none.
    ///
    /// Through `getent`, since the released binary is static and its own
    /// libc reads only `/etc/passwd`.
    pub fn lookup(key: &str) -> Result<Option<Self>> {
        let output = Command::new("getent")
            .args(["passwd", "--", key])
            .stdin(Stdio::null())
            .stderr(Stdio::inherit())
            .output()
            .context("running getent")?;
        if !output.status.success() {
            return Ok(None);
        }
        let text = String::from_utf8(output.stdout).context("getent passwd printed no text")?;
        let line = text.lines().next().unwrap_or_default();
        Self::parse(line).map(Some)
    }

    /// The user this process runs as.
    pub fn current() -> Result<Self> {
        let uid = rustix::process::getuid().as_raw();
        Self::lookup(&uid.to_string())?.with_context(|| format!("uid {uid} is not in passwd"))
    }
}

pub fn is_root() -> bool {
    rustix::process::geteuid().is_root()
}

/// The path of `program`, if the host has it.
pub fn find_program(program: &str) -> Option<PathBuf> {
    PATH_DIRS
        .iter()
        .chain(SBIN_DIRS)
        .map(|dir| Path::new(dir).join(program))
        .find(|path| path.is_file())
}

pub fn has_program(program: &str) -> bool {
    find_program(program).is_some()
}

fn describe(command: &Command) -> String {
    std::iter::once(command.get_program())
        .chain(command.get_args())
        .map(OsStr::to_string_lossy)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Runs `command` to its end with no input; its standard error goes to ours.
pub fn output(command: &mut Command) -> Result<Output> {
    command
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .with_context(|| format!("running {}", describe(command)))
}

/// Runs `command` and returns what it printed, trimmed; an error if it failed.
pub fn run(command: &mut Command) -> Result<String> {
    let output = output(command)?;
    ensure!(
        output.status.success(),
        "{} failed ({})",
        describe(command),
        output.status
    );
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// Whether `command` ran and succeeded, with nothing of it shown.
pub fn succeeds(command: &mut Command) -> bool {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// `argv` as a command.
pub fn command<S: AsRef<OsStr>>(argv: &[S]) -> Result<Command> {
    let (program, args) = argv.split_first().context("an empty command")?;
    let mut command = Command::new(program);
    command.args(args);
    Ok(command)
}

/// `argv` as root: as it is when we are root, through `sudo` otherwise.
/// `-n`, so that a host that would ask for a password fails at once.
pub fn as_root<S: AsRef<OsStr>>(argv: &[S]) -> Result<Command> {
    if is_root() {
        return command(argv);
    }
    let mut command = Command::new("sudo");
    command.arg("-n").arg("--").args(argv);
    Ok(command)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passwd_lines() {
        let user = User::parse("agent:x:1001:1002:An agent,,,:/home/agent:/bin/bash\n").unwrap();
        assert_eq!(
            user,
            User {
                name: "agent".into(),
                uid: 1001,
                gid: 1002,
                home: "/home/agent".into()
            }
        );
        for bad in ["", "agent:x:1001", "agent:x:uid:1:,:/h:/bin/sh"] {
            assert!(User::parse(bad).is_err(), "{bad:?} parsed");
        }
    }
}
