//! The agent's process: started with its standard streams on sockets, and
//! ended with everything it left behind.
//!
//! The session is not a sandbox. The only boundary between it and the
//! agent is the wrapper command of [`Launch`], which `run` sets to enter
//! the sandbox user's login session with `run0`. Without a wrapper the
//! agent runs as the caller's user and inherits its environment, which is
//! only fit for tests.

use std::collections::BTreeMap;
use std::os::fd::OwnedFd;
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use rustix::process::{Pid, Signal, kill_process_group};
use tokio::net::UnixStream;
use tokio::process::{Child, Command};

/// How long a killed process gets to be gone.
const KILL_GRACE: Duration = Duration::from_secs(10);
/// How long each command that stops the sandbox user may take.
const REAP_STEP_TIMEOUT: Duration = Duration::from_secs(30);
/// How often, and how many times, the sandbox user's processes are looked
/// for after they were killed: they do not vanish at once.
const SURVIVOR_POLL: Duration = Duration::from_millis(200);
const SURVIVOR_POLLS: u32 = 50;
/// What pgrep exits with when nothing matched.
const PGREP_NONE: i32 = 1;
const ROOT_UID: u32 = 0;
/// The state of a zombie in `/proc/PID/stat`.
const ZOMBIE: &str = "Z";
/// Becomes root for a reaper command without ever asking for a password.
const SUDO: &[&str] = &["sudo", "-n"];

/// Where the agent runs.
#[derive(Debug, Clone, Default)]
pub enum Launch {
    /// As the caller's user, with its environment: for tests.
    #[default]
    Direct,
    /// As another user, which a wrapper command switches to.
    Sandbox {
        /// The user the agent runs as. When the session ends, every
        /// process of this user is killed: a wrapper such as `run0`
        /// starts the agent in a session of its own, out of reach of a
        /// signal to the process group started here.
        user: String,
        /// The command that runs the agent's argv as that user, such as
        /// `sudo run0 --pipe --user=... --`.
        wrapper: Vec<String>,
    },
}

impl Launch {
    pub(super) fn wrapper(&self) -> &[String] {
        match self {
            Self::Direct => &[],
            Self::Sandbox { wrapper, .. } => wrapper,
        }
    }
}

/// The session's ends of the agent's standard streams.
pub(super) struct Streams {
    pub stdin: UnixStream,
    pub stdout: UnixStream,
    pub stderr: UnixStream,
}

/// The process started for the agent: the wrapper, or the agent itself.
pub(super) struct Agent {
    child: Child,
    /// Its process group, which it leads.
    group: Option<Pid>,
}

/// A connected pair: the session's end, and the one the child inherits.
fn socket_pair() -> std::io::Result<(UnixStream, OwnedFd)> {
    let (ours, theirs) = std::os::unix::net::UnixStream::pair()?;
    ours.set_nonblocking(true)?;
    Ok((UnixStream::from_std(ours)?, theirs.into()))
}

/// Starts ARGV with ENV added to its environment, in a process group of
/// its own.
///
/// Its standard streams are sockets, not pipes: `run0 --pipe` hands them
/// to PID 1 over D-Bus, and under SELinux PID 1 may not read a pipe made
/// by a service such as a CI runner, so with pipes run0 fails with
/// "Connection reset by peer". The old tree put a Node script between
/// the two for this.
pub(super) fn spawn(argv: &[String], env: &BTreeMap<String, String>) -> Result<(Agent, Streams)> {
    let (program, args) = argv.split_first().context("the agent's command is empty")?;
    let (stdin, child_stdin) = socket_pair().context("creating a socket pair")?;
    let (stdout, child_stdout) = socket_pair().context("creating a socket pair")?;
    let (stderr, child_stderr) = socket_pair().context("creating a socket pair")?;
    // The command owns the child's ends until it is dropped, at the end
    // of this statement: kept longer, the session would never see the
    // agent close its streams.
    let child = Command::new(program)
        .args(args)
        .envs(env)
        .stdin(Stdio::from(child_stdin))
        .stdout(Stdio::from(child_stdout))
        .stderr(Stdio::from(child_stderr))
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("starting {program}"))?;
    let group = child
        .id()
        .and_then(|id| i32::try_from(id).ok())
        .and_then(Pid::from_raw);
    Ok((
        Agent { child, group },
        Streams {
            stdin,
            stdout,
            stderr,
        },
    ))
}

impl Agent {
    /// Waits for the process to exit.
    pub async fn wait(&mut self) -> std::io::Result<ExitStatus> {
        self.child.wait().await
    }

    /// Kills the process group, so that what a launcher started goes with
    /// it (`npx` and its `node`).
    ///
    /// The group's id is the process's, which is free again once the
    /// process was waited for and the group is empty: in theory another
    /// process could by now lead a group of that id. Every tool that
    /// kills by group lives with that window.
    pub fn kill_group(&mut self) {
        if let Some(group) = self.group {
            // An error means the group is gone already, or is root's (a
            // wrapper under sudo), which ends when the agent does.
            let _ = kill_process_group(group, Signal::KILL);
        }
        let _ = self.child.start_kill();
    }

    /// Waits for a killed process to be gone, for a while.
    pub async fn gone(mut self) {
        let _ = tokio::time::timeout(KILL_GRACE, self.child.wait()).await;
    }
}

/// Runs one command of the reaper, as root; its standard output if it
/// could be run. Failures are expected (a session already gone, a user
/// with no manager) and the survivor check is what counts.
async fn privileged(argv: &[&str]) -> Option<std::process::Output> {
    let sudo: &[&str] = if rustix::process::geteuid().is_root() {
        &[]
    } else {
        SUDO
    };
    let argv = [sudo, argv].concat();
    let (program, args) = argv.split_first()?;
    let mut command = Command::new(program);
    command.args(args).stdin(Stdio::null()).kill_on_drop(true);
    tokio::time::timeout(REAP_STEP_TIMEOUT, command.output())
        .await
        .ok()?
        .ok()
}

/// The login sessions of the user UID, the agent's among them.
async fn sessions(uid: &str) -> Vec<String> {
    let listing = Command::new("loginctl")
        .args(["show-user", uid, "--property=Sessions", "--value"])
        .stdin(Stdio::null())
        .output()
        .await;
    match listing {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout)
            .split_whitespace()
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

/// The processes of the user UID that are still there, as pgrep lists
/// them.
async fn survivors(uid: &str) -> Result<Option<String>> {
    let out = Command::new("pgrep")
        .args(["-l", "-u", uid])
        .stdin(Stdio::null())
        .output()
        .await
        .context("running pgrep")?;
    match out.status.code() {
        Some(0) => Ok(Some(
            String::from_utf8_lossy(&out.stdout)
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" "),
        )),
        Some(PGREP_NONE) => Ok(None),
        _ => bail!(
            "pgrep -u {uid} failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ),
    }
}

/// The user the agent runs as, known to exist and to be safe to stop.
pub(super) struct SandboxUser {
    name: String,
    uid: u32,
}

impl SandboxUser {
    /// Looks NAME up, and refuses a user whose processes must not all be
    /// killed: root, and the one this process runs as.
    pub async fn find(name: &str) -> Result<Self> {
        let uid: u32 = Command::new("id")
            .args(["-u", "--", name])
            .stdin(Stdio::null())
            .output()
            .await
            .ok()
            .filter(|out| out.status.success())
            .and_then(|out| String::from_utf8_lossy(&out.stdout).trim().parse().ok())
            .with_context(|| format!("there is no sandbox user {name}"))?;
        let own = rustix::process::geteuid();
        ensure!(
            uid != ROOT_UID && uid != own.as_raw(),
            "{name} (uid {uid}) cannot be the sandbox user: the session ends by killing every \
             process of that user"
        );
        Ok(Self {
            name: name.to_owned(),
            uid,
        })
    }

    /// Stops everything the user is running, and makes sure of it.
    ///
    /// Its sessions go first: a session scope holds every process started
    /// in it, including ones that left their process group (setsid,
    /// double forks). Then, as backstops, the user's slice (its service
    /// manager and what that started), a manager it may have kept by
    /// enabling lingering, and stray processes of its uid.
    pub async fn reap(&self) -> Result<()> {
        reap(&self.name, self.uid).await
    }
}

/// By uid throughout, the one `SandboxUser::find` checked: a name could
/// come to mean another user between the check and the kill.
async fn reap(user: &str, uid: u32) -> Result<()> {
    let slice = format!("user-{uid}.slice");
    let manager = format!("user@{uid}.service");
    let uid = &uid.to_string();
    let sessions = sessions(uid).await;
    if !sessions.is_empty() {
        let ids: Vec<&str> = sessions.iter().map(String::as_str).collect();
        privileged(&[&["loginctl", "kill-session", "--signal=KILL"], &ids[..]].concat()).await;
        privileged(&[&["loginctl", "terminate-session"], &ids[..]].concat()).await;
    }
    privileged(&["systemctl", "kill", "--signal=KILL", &slice]).await;
    privileged(&["loginctl", "disable-linger", uid]).await;
    privileged(&["loginctl", "terminate-user", uid]).await;
    for _ in 0..SURVIVOR_POLLS {
        privileged(&["pkill", "-KILL", "-u", uid]).await;
        if survivors(uid).await?.is_none() {
            // Killed like that, the user's manager is left failed.
            privileged(&["systemctl", "reset-failed", &manager]).await;
            return Ok(());
        }
        tokio::time::sleep(SURVIVOR_POLL).await;
    }
    bail!(
        "processes of {user} survived the end of the session: {}",
        survivors(uid).await?.unwrap_or_default()
    )
}

/// Whether process PID exists and is more than a zombie nobody has
/// collected yet. For tests of what a session leaves behind.
pub fn is_running(pid: u32) -> bool {
    // The state follows the command name, which is in parentheses and may
    // itself hold any character.
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|stat| {
            let (_, after) = stat.rsplit_once(')')?;
            after.split_whitespace().next().map(|state| state != ZOMBIE)
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    fn sh(script: &str) -> Vec<String> {
        ["sh", "-c", script].map(str::to_owned).to_vec()
    }

    /// What run0 needs of the streams it is handed, and that they carry
    /// data both ways with the environment set.
    #[tokio::test]
    async fn streams_are_sockets() {
        let script = "test -S /dev/stdin && test -S /dev/stdout && test -S /dev/stderr \
                      && read line && echo \"$GREETING $line\" && echo oops >&2";
        let env = BTreeMap::from([("GREETING".to_owned(), "hello".to_owned())]);
        let (mut agent, mut streams) = spawn(&sh(script), &env).unwrap();
        streams.stdin.write_all(b"agent\n").await.unwrap();
        let (mut out, mut err) = (String::new(), String::new());
        streams.stdout.read_to_string(&mut out).await.unwrap();
        streams.stderr.read_to_string(&mut err).await.unwrap();
        assert_eq!((out.as_str(), err.as_str()), ("hello agent\n", "oops\n"));
        assert!(agent.wait().await.unwrap().success());
    }

    /// A process the agent leaves in its group goes with it.
    #[tokio::test]
    async fn kill_takes_the_group() {
        let (mut agent, mut streams) =
            spawn(&sh("sleep 600 & echo $!; wait"), &BTreeMap::new()).unwrap();
        let mut pid = String::new();
        let mut byte = [0u8; 1];
        while streams.stdout.read_exact(&mut byte).await.is_ok() && byte[0] != b'\n' {
            pid.push(char::from(byte[0]));
        }
        let pid: u32 = pid.parse().unwrap();
        assert!(is_running(pid), "no process {pid}");
        agent.kill_group();
        agent.gone().await;
        // The group is killed at once, but a signal takes a moment.
        for _ in 0..SURVIVOR_POLLS {
            if !is_running(pid) {
                return;
            }
            tokio::time::sleep(SURVIVOR_POLL).await;
        }
        panic!("process {pid} survived");
    }

    #[test]
    fn an_empty_command_is_refused() {
        assert!(spawn(&[], &BTreeMap::new()).is_err());
    }

    /// Killing every process of the sandbox user must never mean this
    /// one's, or root's.
    #[tokio::test]
    async fn sandbox_users_that_are_refused() {
        let own = std::process::Command::new("id")
            .arg("-un")
            .output()
            .unwrap();
        let own = String::from_utf8(own.stdout).unwrap();
        let cases = [
            ("no-such-user-agentic-job", "no sandbox user"),
            ("--help", "no sandbox user"),
            ("root", "cannot be the sandbox user"),
            (own.trim(), "cannot be the sandbox user"),
        ];
        for (name, want) in cases {
            let Err(err) = SandboxUser::find(name).await else {
                panic!("{name} was taken as a sandbox user");
            };
            assert!(format!("{err:#}").contains(want), "{name}: {err:#}");
        }
    }
}
