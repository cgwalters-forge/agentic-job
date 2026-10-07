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

use std::collections::BTreeSet;
use std::io::Read;
use std::net::{Ipv4Addr, SocketAddr, TcpListener, ToSocketAddrs};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, ensure};

use super::enter::{Entry, OIDC_REQUEST_VARS, Output};
use super::host::{self, User};
use super::local::{ABSTRACT_PREFIX, Reached};
use super::setup::{self, SELF_COPY};
use super::{egress, network};
use crate::config::{self, Config, Sandbox};
use crate::exit::Exit;

#[derive(Debug, clap::Args)]
pub struct Args {}

/// A variable of a process of the runner's, standing in for the tokens in
/// the environments of real job steps.
const CANARY_VAR: &str = "AGENT_ISOLATION_CANARY";

const CANARY_BYTES: usize = 12;

/// How long the stand-in process lives if nothing stops it.
const DECOY_SECONDS: &str = "300";

const DECOY_WAIT: Duration = Duration::from_millis(100);

const DECOY_TRIES: u32 = 50;

/// Put before every command a probe runs as the sandbox user.
const PROBE_LIMIT: &[&str] = &["timeout", "--kill-after=10", "600"];

/// The same for the one control that asks a daemon as root.
const CONTROL_LIMIT: &[&str] = &["timeout", "--kill-after=10", "60"];

/// Fails only if `$1` can be neither listed nor entered: a directory of
/// mode 0711 hides its names and still gives up every file whose name is
/// known.
const LIST_OR_ENTER: &str = r#"ls -- "$1" >/dev/null 2>&1 || cd -- "$1""#;

/// Where a CI system says the job's files are. A work or temporary
/// directory outside the runner's home is one more place to close; these
/// are GitHub's and Forgejo's names for them.
const WORK_DIR_VARS: &[&str] = &["RUNNER_TEMP", "RUNNER_WORKSPACE", "GITHUB_WORKSPACE"];

/// Any uid other than root in a rootless container maps to a subordinate
/// uid of the sandbox user.
const CONTAINER_UID: &str = "1000";

/// Where tailscaled listens, on runners that joined a tailnet. The socket
/// itself is open to all; its directory is root's only, by setup.
const TAILSCALE_SOCKET: &str = "/var/run/tailscale/tailscaled.sock";

const LOCALAPI_STATUS: &str = "http://local-tailscaled.sock/localapi/v0/status";

const PTRACE_SCOPE: &str = "/proc/sys/kernel/yama/ptrace_scope";

/// Where the sandbox user can write, and so where a token handed to it
/// could have been left, besides its home and its runtime directory.
const SHARED_WRITABLE: &[&str] = &["/tmp", "/var/tmp", "/dev/shm"];

/// Shorter than this is not a token anyone issued.
const TOKEN_MIN_LEN: usize = 20;

/// Unix sockets every host offers its users, and which answer by who is
/// asking: the system bus, the journal, the user database, PID 1's
/// notification socket and its own and its helpers' Varlink services,
/// and the helper polkit starts to check a password, which newer polkit
/// has in place of a setuid program.
///
/// Named one by one, as seen on the runners this was tried on, and not
/// by directory: systemd-resolved's sockets are under `/run/systemd/`
/// too, and resolve names for any user as its own; and a later systemd
/// may put a service there that hands out user namespaces with uids the
/// network rules do not know. A path ending in `/` is everything under
/// it; one ending in `*` is every path that starts so.
const HOST_SOCKETS: &[&str] = &[
    "/run/dbus/system_bus_socket",
    "/run/systemd/journal/",
    "/run/systemd/userdb/io.systemd.DynamicUser",
    "/run/systemd/userdb/io.systemd.Multiplexer",
    "/run/systemd/notify",
    "/run/systemd/io.systemd.AskPassword",
    "/run/systemd/io.systemd.Credentials",
    "/run/systemd/io.systemd.FactoryReset",
    "/run/systemd/io.systemd.Hostname",
    "/run/systemd/io.systemd.Login",
    "/run/systemd/io.systemd.ManagedOOM",
    "/run/systemd/io.systemd.Manager",
    "/run/systemd/io.systemd.MuteConsole",
    "/run/systemd/io.systemd.sysext",
    "/run/polkit/agent-helper.socket",
];

/// A unit that is active on every host this runs on.
const ALWAYS_ACTIVE_UNIT: &str = "systemd-journald.service";

/// pkexec refuses a caller whose parent is PID 1, which a command started
/// straight from `run0` is; an agent would call it from a shell, so the
/// probe does. The `&&` keeps the shell from replacing itself with it.
const PKEXEC_FROM_A_SHELL: &str = "pkexec true && true";

/// The control directory while it is being made, then open to all
/// without the sticky bit, which is what the search looks for.
const MODE_PRIVATE: u32 = 0o700;

const MODE_OPEN_DIR: u32 = 0o777;

/// A socket any user may connect to.
const MODE_OPEN_SOCKET: u32 = 0o666;

const METADATA_URL: &str = "http://169.254.169.254/metadata/instance?api-version=2021-02-01";

/// Azure's WireServer, which serves the VM's configuration.
const WIRESERVER_URL: &str = "http://168.63.129.16/?comp=versions";

/// With the egress proxy: a write it must refuse, on a host that would
/// take it; a push; and a repository to fetch from.
const WRITE_URL: &str = "https://example.com/";

const PUSH_URL: &str = "https://github.com/bootc-dev/bootc.git/git-receive-pack";

const FETCH_REPO: &str = "https://github.com/bootc-dev/bootc";

const PUBLIC_DNS: &str = "8.8.8.8";

const DNS_PORT: u16 = 53;

const HTTPS_PORT: u16 = 443;

/// What the toolchains fetch through the proxy: a crate and an npm
/// package.
const CARGO_TOML: &str = "[package]\nname = \"egress-check\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[dependencies]\nitoa = \"1\"\n";

const NPM_PACKAGE: &str = "is-number";

const EGRESS_WORK: &str = "egress-check";

/// Where a container sees the proxy's certificate authority, and the
/// mount that puts it there.
const CONTAINER_CA: &str = "/run/egress-ca.pem";

const CA_MOUNT: &str = "/etc/egress-proxy/ca.pem:/run/egress-ca.pem:ro";

/// The header the proxy puts on every refusal of its own, lowercase.
const DENIED_HEADER: &str = "x-egress-denied:";

/// Another port on a direct endpoint's host, which the tailnet's own
/// rules may allow and the sandbox user must not reach: SSH, which most
/// hosts listen on.
const OTHER_TAILNET_PORT: u16 = 22;

/// tailscaled's own address: its DNS, which answers for every node, and
/// its web ports.
const QUAD100: &[&str] = &["100.100.100.100", "fd7a:115c:a1e0::53"];

const QUAD100_PORTS: &[u16] = &[53, 80, 8080];

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
        "`sandbox check` runs as the runner's user, which it compares the sandbox user with; it uses sudo itself to enter the sandbox"
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
    let checker = Checker {
        config,
        entry: Entry::new(config)?,
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

/// The names of the variables in `environs`, the `NAME=value` entries of
/// any number of processes with a NUL after each, that have `needle`
/// anywhere in them. Names only: a value may be what must not be shown.
fn variables_holding(environs: &[u8], needle: &[u8]) -> BTreeSet<String> {
    environs
        .split(|&byte| byte == 0)
        .filter(|entry| contains(entry, needle))
        .map(|entry| {
            let name = entry.split(|&byte| byte == b'=').next().unwrap_or_default();
            String::from_utf8_lossy(name).into_owned()
        })
        .collect()
}

/// A process of the runner's with the canary in its environment; gone
/// when this is dropped.
struct Decoy(Child);

impl Decoy {
    fn start(canary: &str) -> Result<Self> {
        let child = Command::new("sleep")
            .arg(DECOY_SECONDS)
            .env(CANARY_VAR, canary)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("starting sleep")?;
        let decoy = Self(child);
        // Until it has exec'd, its environment is still ours.
        for _ in 0..DECOY_TRIES {
            if decoy.has(canary) {
                break;
            }
            std::thread::sleep(DECOY_WAIT);
        }
        Ok(decoy)
    }

    fn pid(&self) -> u32 {
        self.0.id()
    }

    fn has(&self, canary: &str) -> bool {
        std::fs::read(format!("/proc/{}/environ", self.pid()))
            .is_ok_and(|environ| contains(&environ, canary.as_bytes()))
    }
}

impl Drop for Decoy {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Whether the Unix socket `name` is one the sandbox user may reach.
fn socket_allowed(name: &str, allowed: &[String]) -> bool {
    allowed.iter().any(|entry| {
        if let Some(prefix) = entry.strip_suffix('*') {
            name.starts_with(prefix)
        } else if entry.ends_with('/') {
            name.starts_with(entry.as_str())
        } else {
            name == entry
        }
    })
}

/// systemd's daemons (udevd for one) take notifications from their own
/// children on an abstract datagram socket named by a random 64-bit
/// number, and check who sent each. The name differs on every boot, so
/// no list can hold it.
fn systemd_notification_socket(name: &str) -> bool {
    name.strip_prefix(ABSTRACT_PREFIX).is_some_and(|number| {
        number.bytes().all(|byte| byte.is_ascii_digit()) && number.parse::<u64>().is_ok()
    })
}

struct Checker<'a> {
    config: &'a Config,
    entry: Entry,
    runner: User,
    canary: String,
    report: Report,
}

impl Checker<'_> {
    fn run(mut self, token: Option<RunToken<'_>>) -> Result<Report> {
        self.privileges()?;
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

    /// No sudo, no polkit action, no lingering user manager.
    fn privileges(&mut self) -> Result<()> {
        let user = self.user().to_owned();
        let sudo = ["sudo", "-n", "true"];
        self.report.expect(
            Want::Succeed,
            "sudo-control",
            format!("{} has sudo (control)", self.runner.name),
            host::succeeds(&mut host::command(&sudo)?),
        );
        let got = self.sandbox_succeeds(&sudo)?;
        self.report
            .expect(Want::Fail, "sudo", format!("{user} has no sudo"), got);

        if host::has_program("pkexec") {
            self.report.expect(
                Want::Succeed,
                "polkit-control",
                "root runs a command through pkexec (control)",
                host::succeeds(&mut host::as_root(&["pkexec", "true"])?),
            );
            let got = self.sandbox_succeeds(&["sh", "-c", PKEXEC_FROM_A_SHELL])?;
            self.report.expect(
                Want::Fail,
                "polkit",
                format!("{user} is refused by polkit (pkexec)"),
                got,
            );
        } else {
            self.report
                .note("no pkexec on this host, so no polkit action to try with it");
        }

        // Enabling one's own lingering is a polkit action that is open to
        // every user by default; the control shows logind answers at all.
        let got = self.sandbox_succeeds(&["loginctl", "list-sessions", "--no-legend"])?;
        self.report.expect(
            Want::Succeed,
            "linger-control",
            format!("{user} asks logind for its sessions (control)"),
            got,
        );
        let enabled = self.sandbox_succeeds(&["loginctl", "enable-linger"])?;
        if enabled {
            // Do not leave what the probe was there to find.
            let _ = host::run(&mut host::as_root(&["loginctl", "disable-linger", &user])?);
        }
        self.report.expect(
            Want::Fail,
            "linger",
            format!("{user} can't keep a user manager running after its sessions (enable-linger)"),
            enabled,
        );

        // The groups it really has, whoever made the user.
        let groups = self.sandbox(&["id", "-Gn"], b"")?;
        let groups = String::from_utf8_lossy(&groups.stdout).trim().to_owned();
        self.report.expect(
            Want::Succeed,
            "groups-control",
            format!("{user} lists its groups (control)"),
            !groups.is_empty(),
        );
        let forbidden: Vec<&str> = groups
            .split_whitespace()
            .filter(|group| Sandbox::FORBIDDEN_GROUPS.contains(group))
            .collect();
        self.report.expect(
            Want::Fail,
            "groups",
            if forbidden.is_empty() {
                format!("{user} is in no group that is root by another name ({groups})")
            } else {
                format!("{user} is in {}", forbidden.join(", "))
            },
            !forbidden.is_empty(),
        );
        Ok(())
    }

    /// The runner's processes' environments are closed to the sandbox
    /// user. Returns every environment it can read, NUL-separated.
    fn environments(&mut self) -> Result<Vec<u8>> {
        let user = self.user().to_owned();
        let decoy = Decoy::start(&self.canary)?;
        let targets = [
            (std::process::id(), "environ-self", "this process's"),
            (decoy.pid(), "environ-runner", "another runner process's"),
        ];
        for (pid, id, whose) in targets {
            let got = self.sandbox_succeeds(&["cat", &format!("/proc/{pid}/environ")])?;
            self.report.expect(
                Want::Fail,
                id,
                format!("{user} can't read {whose} environment"),
                got,
            );
        }
        self.report.expect(
            Want::Succeed,
            "environ-control-runner",
            format!(
                "{} reads the canary in its own process's environment (control)",
                self.runner.name
            ),
            decoy.has(&self.canary),
        );
        let environs = self
            .sandbox(
                &["sh", "-c", "cat /proc/[0-9]*/environ 2>/dev/null; true"],
                b"",
            )?
            .stdout;
        let home = format!("HOME={}", self.entry.home().display());
        self.report.expect(
            Want::Succeed,
            "environ-control-sandbox",
            format!("{user} reads its own processes' environments (control)"),
            contains(&environs, home.as_bytes()),
        );
        self.report.expect(
            Want::Fail,
            "environ-canary",
            format!("no environment {user} can read holds the canary"),
            contains(&environs, self.canary.as_bytes()),
        );
        let prefix = Sandbox::FORBIDDEN_ENV_PREFIX;
        let named = variables_holding(&environs, prefix.as_bytes());
        self.report.expect(
            Want::Fail,
            "environ-job-variables",
            if named.is_empty() {
                format!("no environment {user} can read has {prefix} in it")
            } else {
                let names: Vec<&str> = named.iter().map(String::as_str).collect();
                format!(
                    "an environment {user} can read has {prefix} in {}",
                    names.join(", ")
                )
            },
            !named.is_empty(),
        );
        Ok(environs)
    }

    /// The variables that ask for the job's identity token are in every
    /// step's environment when the job may have one, this process's too;
    /// what runs as the sandbox user starts without them.
    fn job_variables(&mut self, environs: &[u8], has_token: bool) -> Result<()> {
        let user = self.user().to_owned();
        let values: Vec<(&str, String)> = OIDC_REQUEST_VARS
            .iter()
            .filter_map(|&name| Some((name, std::env::var(name).ok().filter(|v| !v.is_empty())?)))
            .collect();
        // A run token was got with the identity token, so then the
        // variables must be here to be proven absent from the sandbox.
        if has_token || !values.is_empty() {
            self.report.expect(
                Want::Succeed,
                "oidc-control",
                format!(
                    "this process has {} (control)",
                    OIDC_REQUEST_VARS.join(" and ")
                ),
                values.len() == OIDC_REQUEST_VARS.len(),
            );
        } else {
            self.report.note(
                "this process has no identity-token request variables, so there are none to leak",
            );
        }
        let env = self.sandbox(&["env"], b"")?.stdout;
        let env = String::from_utf8_lossy(&env);
        let home = format!("HOME={}", self.entry.home().display());
        self.report.expect(
            Want::Succeed,
            "env-control",
            format!("{user} runs env (control)"),
            env.lines().any(|line| line == home),
        );
        self.report.expect(
            Want::Fail,
            "env-job-variables",
            format!(
                "{user}'s environment has no {}* variables",
                Sandbox::FORBIDDEN_ENV_PREFIX
            ),
            env.lines()
                .any(|line| line.starts_with(Sandbox::FORBIDDEN_ENV_PREFIX)),
        );
        for (name, value) in values {
            self.report.expect(
                Want::Fail,
                &format!("oidc-value:{name}"),
                format!("no process {user} can read has {name}'s value"),
                contains(environs, value.as_bytes()),
            );
        }
        Ok(())
    }

    /// The directories setup closed, the directory this runs in, and
    /// what on the root filesystem any user may write.
    fn files(&mut self) -> Result<()> {
        let user = self.user().to_owned();
        self.report.expect(
            Want::Succeed,
            "private-dir-control",
            format!(
                "{} lists its own home {} (control)",
                self.runner.name,
                self.runner.home.display()
            ),
            std::fs::read_dir(&self.runner.home).is_ok(),
        );
        let mut dirs = setup::private_dirs(self.config, &self.runner);
        // Where the job's files are, if not under one of those: the
        // directory this runs in and the ones the CI system names. Only
        // what is the runner's own, so that running this from / says
        // nothing.
        let work_dirs = std::env::current_dir().into_iter().chain(
            WORK_DIR_VARS
                .iter()
                .filter_map(std::env::var_os)
                .map(PathBuf::from),
        );
        for dir in work_dirs {
            let ours = std::fs::metadata(&dir).is_ok_and(|m| m.uid() == self.runner.uid);
            if ours && !dirs.iter().any(|closed| dir.starts_with(closed)) {
                dirs.push(dir);
            }
        }
        for dir in dirs {
            let shown = dir.display().to_string();
            if !host::succeeds(&mut host::as_root(&["test", "-d", &shown])?) {
                self.report.note(&format!("no {shown} on this host"));
                continue;
            }
            let got = self.sandbox_succeeds(&["sh", "-c", LIST_OR_ENTER, "sh", &shown])?;
            self.report.expect(
                Want::Fail,
                &format!("private-dir:{shown}"),
                format!("{user} can neither list nor enter {shown}"),
                got,
            );
        }

        // The control is a directory of ours that any user may write,
        // found by the same search the probe makes.
        let open_dir = PathBuf::from(format!("/tmp/agentic-job-{}", self.canary));
        std::fs::DirBuilder::new()
            .mode(MODE_PRIVATE)
            .create(&open_dir)
            .with_context(|| format!("creating {}", open_dir.display()))?;
        std::fs::set_permissions(&open_dir, std::fs::Permissions::from_mode(MODE_OPEN_DIR))?;
        let find = |root: &str| -> Result<String> {
            let mut argv = vec!["find".to_owned()];
            argv.extend(setup::world_writable_find(root));
            // Not its own: what it made world-writable itself, in its
            // home, gives it nothing.
            argv.extend(["!", "-user", user.as_str(), "-print", "-quit"].map(str::to_owned));
            let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
            // find also fails for what it may not read; what it printed counts.
            let output = self.sandbox(&argv, b"")?;
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
        };
        let control = find(&open_dir.display().to_string());
        let found = find("/");
        let _ = std::fs::remove_dir(&open_dir);
        let (control, found) = (control?, found?);
        self.report.expect(
            Want::Succeed,
            "world-writable-control",
            format!("{user} finds a world-writable directory made for it (control)"),
            !control.is_empty(),
        );
        self.report.expect(
            Want::Fail,
            "world-writable",
            if found.is_empty() {
                format!("{user} finds nothing world-writable on the root filesystem outside the temporary directories")
            } else {
                format!("{user} finds {found} world-writable")
            },
            !found.is_empty(),
        );

        let scope = std::fs::read_to_string(PTRACE_SCOPE).unwrap_or_default();
        self.report.expect(
            Want::Succeed,
            "ptrace-scope",
            format!(
                "ptrace is restricted to descendants or more ({PTRACE_SCOPE} is {})",
                scope.trim()
            ),
            scope.trim().parse::<u32>().is_ok_and(|scope| scope >= 1),
        );
        Ok(())
    }

    /// cron and at would run the sandbox user's commands outside any
    /// session, after the supervisor has stopped everything it can see.
    fn schedulers(&mut self) -> Result<()> {
        let user = self.user().to_owned();
        if host::has_program("crontab") {
            let listed = Command::new("crontab")
                .arg("-l")
                .stdin(Stdio::null())
                .output();
            self.report.expect(
                Want::Succeed,
                "cron-control",
                format!("{} may use crontab (control)", self.runner.name),
                listed.is_ok_and(|output| {
                    output.status.success()
                        || String::from_utf8_lossy(&output.stderr).contains("no crontab")
                }),
            );
            let installed = self
                .sandbox(&["crontab", "-"], b"# agentic-job sandbox check\n")?
                .success();
            if installed {
                let _ = host::run(&mut host::as_root(&["crontab", "-r", "-u", &user])?);
            }
            self.report.expect(
                Want::Fail,
                "cron",
                format!("{user} can't install a crontab"),
                installed,
            );
        } else {
            self.report.note("no crontab on this host");
        }
        if host::has_program("at") {
            // Where at has neither of its lists, only root may use it,
            // and the runner's user is no control.
            self.control_or_note("at-control", "at", &["at", "-l"]);
            let got = self.sandbox_succeeds(&["at", "-l"])?;
            self.report
                .expect(Want::Fail, "at", format!("{user} can't use at"), got);
        } else {
            self.report.note("no at on this host");
        }
        Ok(())
    }

    /// The units the configuration says are stopped, are.
    fn services(&mut self) {
        let units = &self.config.sandbox.stop_services;
        if units.is_empty() {
            return;
        }
        let active = |unit: &str| {
            host::succeeds(Command::new("systemctl").args(["is-active", "--quiet", "--", unit]))
        };
        self.report.expect(
            Want::Succeed,
            "service-control",
            format!("{ALWAYS_ACTIVE_UNIT} is active (control)"),
            active(ALWAYS_ACTIVE_UNIT),
        );
        for unit in units {
            self.report.expect(
                Want::Fail,
                &format!("service:{unit}"),
                format!("{unit} is not running"),
                active(unit),
            );
        }
    }

    /// The sandbox user connects to no Unix socket and no local TCP port
    /// but the host's own and the configured ones. The control is a pair
    /// of listeners of ours, which it must find and connect to. The Unix
    /// one is a path, since the prober reports abstract sockets without
    /// trying them.
    fn local_services(&mut self) -> Result<()> {
        let user = self.user().to_owned();
        let control_path = format!("/tmp/agentic-job-{}.sock", self.canary);
        let control_unix =
            UnixListener::bind(&control_path).context("listening on a socket in /tmp")?;
        std::fs::set_permissions(
            &control_path,
            std::fs::Permissions::from_mode(MODE_OPEN_SOCKET),
        )?;
        let control_tcp =
            TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).context("listening on loopback")?;
        let controls = [
            Reached::Unix(control_path.clone()),
            Reached::Tcp(control_tcp.local_addr()?),
        ];
        let output = self.sandbox(&[SELF_COPY, "sandbox", "probe-local"], b"");
        drop((control_unix, control_tcp));
        let _ = std::fs::remove_file(&control_path);
        let output = output?;
        let reached: Vec<Reached> = String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(Reached::parse)
            .collect();
        self.report.expect(
            Want::Succeed,
            "local-control",
            format!(
                "{user} finds and reaches a Unix socket and a TCP port opened for it (control)"
            ),
            output.success() && controls.iter().all(|control| reached.contains(control)),
        );

        let check = &self.config.sandbox.check;
        let allowed: Vec<String> = HOST_SOCKETS
            .iter()
            .map(|&socket| socket.to_owned())
            // Its own session's: its bus and its systemd manager.
            .chain([format!("/run/user/{}/", self.entry.user().uid)])
            .chain(check.allow_sockets.iter().cloned())
            .collect();
        let (unix, tcp): (Vec<&Reached>, Vec<&Reached>) = reached
            .iter()
            .filter(|reached| !controls.contains(*reached))
            .filter(|reached| match reached {
                Reached::Unix(name) => !socket_allowed(name, &allowed),
                Reached::UnixDatagram(name) => {
                    !socket_allowed(name, &allowed) && !systemd_notification_socket(name)
                }
                Reached::Tcp(addr) => {
                    let port = addr.port();
                    // The egress proxy is where it is meant to connect.
                    let proxy = self.config.egress.proxy && port == egress::PROXY_PORT;
                    !proxy && !check.allow_tcp_ports.contains(&port)
                }
            })
            .partition(|reached| !matches!(reached, Reached::Tcp(_)));
        let list = |found: &[&Reached]| {
            found
                .iter()
                .map(|reached| reached.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        };
        self.report.expect(
            Want::Fail,
            "local-sockets",
            if unix.is_empty() {
                format!("{user} connects to no Unix socket but the host's own and the allowed ones")
            } else {
                format!(
                    "{user} connects to {} (stop what listens there, or name it in sandbox.check.allow-sockets)",
                    list(&unix)
                )
            },
            !unix.is_empty(),
        );
        self.report.expect(
            Want::Fail,
            "local-tcp",
            if tcp.is_empty() {
                format!("{user} connects to no local TCP port but the allowed ones")
            } else {
                format!(
                    "{user} connects to {} (stop what listens there, or name the port in sandbox.check.allow-tcp-ports)",
                    list(&tcp)
                )
            },
            !tcp.is_empty(),
        );
        Ok(())
    }

    /// Through its LocalAPI, tailscaled would dial and list the tailnet
    /// for anyone, whatever the packet filter says.
    fn tailscaled(&mut self) -> Result<()> {
        let user = self.user().to_owned();
        if !host::succeeds(&mut host::as_root(&["test", "-S", TAILSCALE_SOCKET])?) {
            self.report
                .note("no tailscaled on this host, so no LocalAPI to reach");
            return Ok(());
        }
        self.report.expect(
            Want::Succeed,
            "tailscale-control",
            "root uses tailscaled's LocalAPI (control)",
            {
                let argv: Vec<&str> = CONTROL_LIMIT
                    .iter()
                    .copied()
                    .chain(["tailscale", "status", "--self"])
                    .collect();
                host::succeeds(&mut host::as_root(&argv)?)
            },
        );
        let got = self.sandbox_succeeds(&["tailscale", "status"])?;
        self.report.expect(
            Want::Fail,
            "tailscale-status",
            format!("{user} can't run tailscale status"),
            got,
        );
        let got = self.sandbox_succeeds(&[
            "curl",
            "-sS",
            "-m",
            "10",
            "-o",
            "/dev/null",
            "--unix-socket",
            TAILSCALE_SOCKET,
            LOCALAPI_STATUS,
        ])?;
        self.report.expect(
            Want::Fail,
            "tailscale-localapi",
            format!("{user} can't call tailscaled's LocalAPI"),
            got,
        );
        Ok(())
    }

    /// The run token: the runner's file is out of reach; the sandbox
    /// user's copy is its agent's configuration, mode 600 in a directory
    /// of mode 700, and nowhere else it can write or read a process's
    /// environment or command line. The token goes to the searches on
    /// standard input, never on a command line.
    fn run_token(&mut self, token: RunToken<'_>, environs: &[u8]) -> Result<()> {
        let user = self.user().to_owned();
        let runner_file = token.runner_file.display().to_string();
        let config_file = token.agent_config.display().to_string();
        let config_dir = token
            .agent_config
            .parent()
            .context("the agent's configuration file has no directory")?
            .display()
            .to_string();
        let config_name = token
            .agent_config
            .file_name()
            .context("the agent's configuration file has no name")?
            .to_string_lossy()
            .into_owned();
        let value = std::fs::read_to_string(token.runner_file).unwrap_or_default();
        let value = value.trim();
        self.report.expect(
            Want::Succeed,
            "token-control",
            format!("{} reads the run token (control)", self.runner.name),
            value.len() >= TOKEN_MIN_LEN && !value.contains(char::is_whitespace),
        );
        let got = self.sandbox_succeeds(&["cat", "--", &runner_file])?;
        self.report.expect(
            Want::Fail,
            "token-runner-file",
            format!("{user} can't read {runner_file}"),
            got,
        );

        // An empty token would match every file.
        let needle = if value.is_empty() {
            self.canary.as_str()
        } else {
            value
        };
        let needle = format!("{needle}\n");
        let holds = self
            .sandbox(
                &["grep", "-qsF", "-f", "-", "--", &config_file],
                needle.as_bytes(),
            )?
            .success();
        self.report.expect(
            Want::Succeed,
            "token-config-control",
            format!("{user}'s {config_file} holds the run token (control)"),
            holds,
        );
        let mode = |path: &str| -> Result<String> {
            let output = host::output(&mut host::as_root(&["stat", "-c", "%U %a", "--", path])?)?;
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
        };
        let (file_mode, dir_mode) = (mode(&config_file)?, mode(&config_dir)?);
        self.report.expect(
            Want::Succeed,
            "token-config-mode",
            format!("{config_file} is {user}'s, mode 600 (it is {file_mode})"),
            file_mode == format!("{user} 600"),
        );
        self.report.expect(
            Want::Succeed,
            "token-config-dir-mode",
            format!("its directory is {user}'s, mode 700 (it is {dir_mode})"),
            dir_mode == format!("{user} 700"),
        );

        let home = self.entry.home().display().to_string();
        let runtime = format!("/run/user/{}", self.entry.user().uid);
        let mut search = vec!["grep", "-rlsF", "-f", "-", "--", &home, &runtime];
        search.extend(SHARED_WRITABLE);
        let found = self.sandbox(&search, needle.as_bytes())?.stdout;
        let found = String::from_utf8_lossy(&found);
        let found: Vec<&str> = found.lines().filter(|line| !line.is_empty()).collect();
        self.report.expect(
            Want::Succeed,
            "token-files",
            format!(
                "{user} finds the token in no other file it can write to ({})",
                if found.is_empty() {
                    "nowhere".to_owned()
                } else {
                    found.join(", ")
                }
            ),
            found.contains(&config_file.as_str()) && found.iter().all(|file| *file == config_file),
        );
        let cmdlines = self
            .sandbox(
                &["sh", "-c", "cat /proc/[0-9]*/cmdline 2>/dev/null; true"],
                b"",
            )?
            .stdout;
        self.report.expect(
            Want::Fail,
            "token-processes",
            format!("no process {user} can read has the token in its environment or command line"),
            contains(environs, value.as_bytes()) || contains(&cmdlines, value.as_bytes()),
        );

        // Containers run rootless as the sandbox user, whose own uid is
        // their root; any other uid in them is a subordinate one.
        if !self.has_podman() {
            self.report
                .note("no podman on this host, so no container to read the token from");
            return Ok(());
        }
        let image = self.config.sandbox.check.container_image.clone();
        let volume = format!("{config_dir}:/config:ro");
        let inside = format!("/config/{config_name}");
        let read_as = |uid: &str| {
            self.sandbox_succeeds(&[
                "podman",
                "run",
                "--rm",
                "--security-opt",
                "label=disable",
                "--user",
                uid,
                "-v",
                &volume,
                &image,
                "cat",
                &inside,
            ])
        };
        let (as_root, as_subuid) = (read_as("0")?, read_as(CONTAINER_UID)?);
        self.report.expect(
            Want::Succeed,
            "token-container-control",
            format!("a container as its root ({user}) reads the configuration (control)"),
            as_root,
        );
        self.report.expect(
            Want::Fail,
            "token-container-subuid",
            format!("a container as subordinate uid {CONTAINER_UID} can't read the configuration"),
            as_subuid,
        );
        Ok(())
    }

    fn has_podman(&self) -> bool {
        host::has_program("podman")
    }

    /// The network as the sandbox user has it, on the host and from a
    /// rootless container: no cloud metadata service, and then what the
    /// egress proxy and the direct endpoints add.
    fn network(&mut self) -> Result<()> {
        let user = self.user().to_owned();
        let url = self.config.sandbox.check.control_url.clone();
        let image = self.config.sandbox.check.container_image.clone();
        let egress = self.config.egress.proxy;
        let got = self.sandbox_succeeds(&curl(&[&url]))?;
        self.report.expect(
            Want::Succeed,
            "network-control",
            format!("{user} reaches {url} (control)"),
            got,
        );
        let metadata = direct(&["-H", "Metadata:true", METADATA_URL]);
        self.control_or_note(
            "metadata-control",
            "the instance metadata service",
            &metadata,
        );
        let got = self.sandbox_succeeds(&metadata)?;
        self.report.expect(
            Want::Fail,
            "metadata",
            format!("{user} can't reach the instance metadata service"),
            got,
        );
        if !self.has_podman() {
            self.report
                .note("no podman on this host, so no container to probe from");
        } else {
            let got = self.sandbox_succeeds(&["podman", "pull", "-q", &image])?;
            self.report.expect(
                Want::Succeed,
                "container-pull",
                format!("{user} pulls {image}"),
                got,
            );
            // The control shows the container and its curl work, so the
            // refusal after it is the filter on subordinate uids.
            let through_proxy = self.proxy_url();
            let via: Vec<&str> = if egress {
                vec!["--cacert", CONTAINER_CA, "-x", &through_proxy]
            } else {
                Vec::new()
            };
            let got = self.in_container(&curl(&[via.as_slice(), &[url.as_str()]].concat()))?;
            self.report.expect(
                Want::Succeed,
                "container-control",
                format!("a container as subordinate uid {CONTAINER_UID} reaches {url} (control)"),
                got,
            );
            let got = self.in_container(&metadata)?;
            self.report.expect(
                Want::Fail,
                "container-metadata",
                format!(
                    "a container as subordinate uid {CONTAINER_UID} can't reach the instance metadata service"
                ),
                got,
            );
        }
        if egress {
            self.egress()?;
        }
        if !self.config.egress.direct.is_empty() {
            self.tailnet()?;
        }
        Ok(())
    }

    fn proxy_url(&self) -> String {
        egress::proxy_url()
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

    /// The egress proxy: the toolchains fetch through it, it refuses
    /// writes to unlisted endpoints (with its own header, so the refusal
    /// is the proxy's), going around it fails, and it reaches nothing the
    /// sandbox may not, from a container either.
    fn egress(&mut self) -> Result<()> {
        let user = self.user().to_owned();
        let url = self.config.sandbox.check.control_url.clone();
        let proxy = self.proxy_url();
        let host_name = url_host(&url).to_owned();
        let work = format!("{}/{EGRESS_WORK}", self.entry.home().display());
        let refused = |output: Output| refused_by_proxy(&String::from_utf8_lossy(&output.stdout));

        let got = self.sandbox_succeeds(&proxied(&["--proxy", &proxy, &url]))?;
        self.report.expect(
            Want::Succeed,
            "egress-read",
            format!("{user} reads {url} through the egress proxy"),
            got,
        );
        self.sandbox(&["rm", "-rf", &work], b"")?;
        if self.sandbox_has("cargo")? {
            let manifest = format!("{work}/crate/Cargo.toml");
            self.sandbox(&["mkdir", "-p", &format!("{work}/crate/src")], b"")?;
            self.sandbox(&["tee", &manifest], CARGO_TOML.as_bytes())?;
            self.sandbox(
                &["tee", &format!("{work}/crate/src/main.rs")],
                b"fn main() {}\n",
            )?;
            let got = self.sandbox_succeeds(&["cargo", "fetch", "--manifest-path", &manifest])?;
            self.report.expect(
                Want::Succeed,
                "egress-cargo",
                format!("{user} fetches a crate with cargo"),
                got,
            );
        } else {
            self.report
                .note("no cargo for the sandbox user, so no crate to fetch");
        }
        if self.sandbox_has("npm")? {
            let prefix = format!("{work}/npm");
            self.sandbox(&["mkdir", "-p", &prefix], b"")?;
            let got = self.sandbox_succeeds(&[
                "npm",
                "install",
                "--no-fund",
                "--prefix",
                &prefix,
                NPM_PACKAGE,
            ])?;
            self.report.expect(
                Want::Succeed,
                "egress-npm",
                format!("{user} installs {NPM_PACKAGE} with npm"),
                got,
            );
        } else {
            self.report
                .note("no npm for the sandbox user, so no package to install");
        }
        self.sandbox(&["rm", "-rf", &work], b"")?;
        let got =
            self.sandbox_succeeds(&["timeout", "60", "git", "ls-remote", FETCH_REPO, "HEAD"])?;
        self.report.expect(
            Want::Succeed,
            "egress-git",
            format!("{user} fetches from git (POST git-upload-pack)"),
            got,
        );

        let post = head(&["-X", "POST", "-d", "egress-check", WRITE_URL]);
        let got = refused(self.sandbox(&post, b"")?);
        self.report.expect(
            Want::Succeed,
            "egress-post",
            format!("the proxy refuses a POST to {WRITE_URL}"),
            got,
        );
        let got = refused(self.sandbox(&head(&["-X", "POST", "-d", "0000", PUSH_URL]), b"")?);
        self.report.expect(
            Want::Succeed,
            "egress-push",
            "the proxy refuses a git push (POST git-receive-pack)",
            got,
        );
        let fronted = head(&["-H", "Host: example.com", &url]);
        let got = refused(self.sandbox(&fronted, b"")?);
        self.report.expect(
            Want::Succeed,
            "egress-fronting",
            "the proxy refuses a Host header naming another host than the connection's (domain fronting)",
            got,
        );

        let around = direct(&[&url]);
        let got = self.sandbox_succeeds(&around)?;
        self.report.expect(
            Want::Fail,
            "egress-direct",
            format!("{user} can't reach {url} around the proxy"),
            got,
        );
        // The address, looked up here: the sandbox user cannot.
        let address = (host_name.as_str(), HTTPS_PORT)
            .to_socket_addrs()
            .ok()
            .and_then(|mut addrs| addrs.find(SocketAddr::is_ipv4))
            .map(|addr| addr.ip().to_string());
        match address {
            Some(address) => {
                let probe = tcp_probe(&address, HTTPS_PORT);
                let probe: Vec<&str> = probe.iter().map(String::as_str).collect();
                self.control_or_note(
                    "egress-tcp-control",
                    &format!("{address}:{HTTPS_PORT}"),
                    &probe,
                );
                let got = self.sandbox_succeeds(&probe)?;
                self.report.expect(
                    Want::Fail,
                    "egress-tcp",
                    format!(
                        "{user} can't open a TCP connection to {address}:{HTTPS_PORT} around the proxy"
                    ),
                    got,
                );
            }
            None => self.report.expect(
                Want::Succeed,
                "egress-tcp-control",
                format!("{} resolves {host_name} (control)", self.runner.name),
                false,
            ),
        }
        let resolve = ["getent", "hosts", host_name.as_str()];
        self.control_or_note(
            "egress-resolve-control",
            &format!("a resolver for {host_name}"),
            &resolve,
        );
        let got = self.sandbox_succeeds(&resolve)?;
        self.report.expect(
            Want::Fail,
            "egress-resolve",
            format!("{user} can't resolve names (getent hosts)"),
            got,
        );
        let dns = tcp_probe(PUBLIC_DNS, DNS_PORT);
        let dns: Vec<&str> = dns.iter().map(String::as_str).collect();
        self.control_or_note(
            "egress-dns-control",
            &format!("{PUBLIC_DNS}:{DNS_PORT}"),
            &dns,
        );
        let got = self.sandbox_succeeds(&dns)?;
        self.report.expect(
            Want::Fail,
            "egress-dns",
            format!("{user} can't reach a public DNS server ({PUBLIC_DNS}:{DNS_PORT})"),
            got,
        );
        let wireserver = direct(&[WIRESERVER_URL]);
        self.control_or_note("egress-wireserver-control", "the WireServer", &wireserver);
        let got = self.sandbox_succeeds(&wireserver)?;
        self.report.expect(
            Want::Fail,
            "egress-wireserver",
            format!("{user} can't reach the WireServer"),
            got,
        );
        let got = self.sandbox_succeeds(&proxied(&["-H", "Metadata:true", METADATA_URL]))?;
        self.report.expect(
            Want::Fail,
            "egress-proxy-metadata",
            "the proxy doesn't reach the instance metadata service",
            got,
        );
        let got = self.sandbox_succeeds(&proxied(&[WIRESERVER_URL]))?;
        self.report.expect(
            Want::Fail,
            "egress-proxy-wireserver",
            "the proxy doesn't reach the WireServer",
            got,
        );
        for endpoint in self.config.egress.direct.clone() {
            let got =
                self.sandbox_succeeds(&proxied(&["--proxy", &proxy, "--noproxy", "", &endpoint]))?;
            self.report.expect(
                Want::Fail,
                "egress-proxy-direct",
                format!("the proxy doesn't reach {endpoint} on the tailnet"),
                got,
            );
        }

        if self.has_podman() {
            let image = self.config.sandbox.check.container_image.clone();
            let post = head(&[
                "--cacert",
                CONTAINER_CA,
                "-x",
                &proxy,
                "-X",
                "POST",
                "-d",
                "x",
                WRITE_URL,
            ]);
            let got = refused(self.sandbox(&self.container(&post), b"")?);
            self.report.expect(
                Want::Succeed,
                "egress-container-post",
                format!(
                    "a container as subordinate uid {CONTAINER_UID} on the host's network is refused a POST too"
                ),
                got,
            );
            let got = self.in_container(&around)?;
            self.report.expect(
                Want::Fail,
                "egress-container-direct",
                format!(
                    "a container as subordinate uid {CONTAINER_UID} can't reach {url} around the proxy"
                ),
                got,
            );
            let own_network = [
                &["podman", "run", "--rm", image.as_str()][..],
                around.as_slice(),
            ]
            .concat();
            let got = self.sandbox_succeeds(&own_network)?;
            self.report.expect(
                Want::Fail,
                "egress-container-own-network",
                format!("a container on its own network can't reach {url}"),
                got,
            );
        }
        Ok(())
    }

    /// Whether the sandbox user has `program` on its path.
    fn sandbox_has(&self, program: &str) -> Result<bool> {
        self.sandbox_succeeds(&["sh", "-c", "command -v \"$1\" >/dev/null", "sh", program])
    }

    /// The tailnet filter: the direct endpoints are reachable, and
    /// nothing else there, over IPv4 or IPv6, tailscaled's own address
    /// (its DNS included) or this node's, whether dialed directly or
    /// through tailscaled. Where the runner's user reaches a target, that
    /// is shown as a control; once the tailnet's own rules are narrowed
    /// to the endpoint's port, only this node's addresses have one.
    fn tailnet(&mut self) -> Result<()> {
        let user = self.user().to_owned();
        let status = tailscale_status();
        let endpoints = network::direct(&self.config.egress)?;
        // The URL as the configuration has it: an https endpoint is not
        // spoken to in plain HTTP.
        for (endpoint, url) in endpoints.into_iter().zip(self.config.egress.direct.clone()) {
            let host_v4 = endpoint.host.to_string();
            // Any HTTP response will do: the point is the connection.
            let got = self.sandbox_succeeds(&curl(&[&url]))?;
            self.report.expect(
                Want::Succeed,
                "tailnet-direct",
                format!("{user} reaches {url} on the tailnet"),
                got,
            );
            if self.has_podman() {
                let got = self.in_container(&curl(&[&url]))?;
                self.report.expect(
                    Want::Succeed,
                    "tailnet-container-direct",
                    format!(
                        "a container as subordinate uid {CONTAINER_UID} reaches {url} on the tailnet"
                    ),
                    got,
                );
            }
            let host_v6 = tailnet_v6(&status, &host_v4);
            let mut targets = vec![(host_v4.clone(), OTHER_TAILNET_PORT)];
            if let Some(v6) = host_v6 {
                targets.extend([(v6.clone(), endpoint.port), (v6, OTHER_TAILNET_PORT)]);
            }
            targets.extend(QUAD100.iter().flat_map(|addr| {
                QUAD100_PORTS
                    .iter()
                    .map(move |&port| ((*addr).to_owned(), port))
            }));
            targets.extend(self_peerapi(&status));
            for (addr, port) in targets {
                let target = if addr.contains(':') {
                    format!("[{addr}]:{port}")
                } else {
                    format!("{addr}:{port}")
                };
                let probe = tcp_probe(&addr, port);
                let probe: Vec<&str> = probe.iter().map(String::as_str).collect();
                self.control_or_note(&format!("tailnet-control:{target}"), &target, &probe);
                let got = self.sandbox_succeeds(&probe)?;
                self.report.expect(
                    Want::Fail,
                    &format!("tailnet:{target}"),
                    format!("{user} can't reach {target}"),
                    got,
                );
                if self.has_podman() {
                    let got = self.in_container(&probe)?;
                    self.report.expect(
                        Want::Fail,
                        &format!("tailnet-container:{target}"),
                        format!(
                            "a container as subordinate uid {CONTAINER_UID} can't reach {target}"
                        ),
                        got,
                    );
                }
            }
            let port = OTHER_TAILNET_PORT.to_string();
            let got =
                self.sandbox_succeeds(&["timeout", "10", "tailscale", "nc", &host_v4, &port])?;
            self.report.expect(
                Want::Fail,
                "tailnet-nc",
                format!("{user} can't dial {host_v4}:{port} through tailscaled (tailscale nc)"),
                got,
            );
        }
        Ok(())
    }
}

const CURL: &[&str] = &["curl", "-sS", "-m", "30", "-o", "/dev/null"];

fn curl<'a>(args: &[&'a str]) -> Vec<&'a str> {
    [CURL, args].concat()
}

/// Not through the egress proxy, whatever the environment says.
fn direct<'a>(args: &[&'a str]) -> Vec<&'a str> {
    [CURL, &["--noproxy", "*"], args].concat()
}

/// Through it, failing on an error status: the proxy answers 502 when it
/// cannot connect.
fn proxied<'a>(args: &[&'a str]) -> Vec<&'a str> {
    [CURL, &["--fail"], args].concat()
}

/// A request whose response headers are printed.
fn head<'a>(args: &[&'a str]) -> Vec<&'a str> {
    [CURL, &["-D", "-"], args].concat()
}

/// A command that opens a plain TCP connection, whatever the service
/// speaks.
fn tcp_probe(addr: &str, port: u16) -> Vec<String> {
    [
        "timeout",
        "10",
        "bash",
        "-c",
        r#"exec 3<>"/dev/tcp/$1/$2""#,
        "probe",
        addr,
        &port.to_string(),
    ]
    .iter()
    .map(|&arg| arg.to_owned())
    .collect()
}

/// Whether response headers are the egress proxy's own refusal: a 403
/// with its header, which an origin's 403 lacks.
fn refused_by_proxy(headers: &str) -> bool {
    let status = headers.lines().any(|line| {
        let mut words = line.split_whitespace();
        words
            .next()
            .is_some_and(|version| version.starts_with("HTTP/"))
            && words.next() == Some("403")
    });
    let marked = headers
        .lines()
        .any(|line| line.to_ascii_lowercase().starts_with(DENIED_HEADER));
    status && marked
}

/// The host of an http or https URL.
fn url_host(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    host.rsplit_once(':')
        .filter(|(_, port)| port.bytes().all(|byte| byte.is_ascii_digit()))
        .map_or(host, |(host, _)| host)
}

/// tailscaled's status, asked as root; null when there is none.
fn tailscale_status() -> serde_json::Value {
    host::as_root(&["tailscale", "status", "--json"])
        .and_then(|mut command| host::output(&mut command))
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| serde_json::from_slice(&output.stdout).ok())
        .unwrap_or(serde_json::Value::Null)
}

/// The tailnet IPv6 address of the peer whose IPv4 address is `addr`.
fn tailnet_v6(status: &serde_json::Value, addr: &str) -> Option<String> {
    status.get("Peer")?.as_object()?.values().find_map(|peer| {
        let ips: Vec<&str> = peer
            .get("TailscaleIPs")?
            .as_array()?
            .iter()
            .filter_map(serde_json::Value::as_str)
            .collect();
        ips.contains(&addr).then(|| {
            ips.iter()
                .find(|ip| ip.contains(':'))
                .map(|&ip| ip.to_owned())
        })?
    })
}

/// The address and port of this node's own peer API, which is reached
/// over loopback.
fn self_peerapi(status: &serde_json::Value) -> Vec<(String, u16)> {
    status
        .get("Self")
        .and_then(|own| own.get("PeerAPIURL"))
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .filter_map(|url| {
            let rest = url.split_once("://")?.1;
            let authority = rest.split('/').next()?;
            let (host, port) = authority.rsplit_once(':')?;
            let host = host.trim_start_matches('[').trim_end_matches(']');
            Some((host.to_owned(), port.parse().ok()?))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_proxys_refusal_and_an_origins() {
        let ours = "HTTP/1.1 200 Connection established\r\n\r\nHTTP/2 403 \r\ncontent-type: text/plain\r\nX-Egress-Denied: a write to an endpoint not in the write allowlist\r\n\r\n";
        assert!(refused_by_proxy(ours));
        let cases = [
            "HTTP/1.1 403 Forbidden\r\nserver: origin\r\n\r\n",
            "HTTP/1.1 200 OK\r\nx-egress-denied: no\r\n\r\n",
            "HTTP/1.1 502 Bad Gateway\r\n\r\n",
            "",
        ];
        for headers in cases {
            assert!(!refused_by_proxy(headers), "{headers:?}");
        }
    }

    #[test]
    fn hosts_of_urls() {
        let cases = [
            ("https://github.com/", "github.com"),
            ("http://example.com:8080/a?b", "example.com"),
            ("https://user@example.org", "example.org"),
            ("http://100.101.102.103:18080/v1", "100.101.102.103"),
        ];
        for (url, host) in cases {
            assert_eq!(url_host(url), host, "{url}");
        }
    }

    #[test]
    fn tailscale_status_fields() {
        let status: serde_json::Value = serde_json::from_str(
            r#"{"Self": {"PeerAPIURL": ["http://100.72.1.2:37098", "http://[fd7a:115c:a1e0::1]:39316"]},
                "Peer": {"a": {"TailscaleIPs": ["100.101.102.103", "fd7a:115c:a1e0::9"]},
                         "b": {"TailscaleIPs": ["100.64.0.7"]}}}"#,
        )
        .unwrap();
        assert_eq!(
            tailnet_v6(&status, "100.101.102.103").as_deref(),
            Some("fd7a:115c:a1e0::9")
        );
        assert_eq!(tailnet_v6(&status, "100.64.0.7"), None);
        assert_eq!(tailnet_v6(&status, "100.64.0.8"), None);
        assert_eq!(
            self_peerapi(&status),
            [
                ("100.72.1.2".to_owned(), 37098),
                ("fd7a:115c:a1e0::1".to_owned(), 39316)
            ]
        );
        assert!(self_peerapi(&serde_json::Value::Null).is_empty());
    }

    #[test]
    fn byte_search() {
        assert!(contains(b"A=1\0HOME=/home/agent\0", b"HOME=/home/agent"));
        assert!(!contains(b"A=1\0", b"HOME="));
        assert!(!contains(b"short", b"much longer than the text"));
        // An empty needle must never read as "found".
        assert!(!contains(b"anything", b""));
    }

    #[test]
    fn variables_are_named_and_their_values_are_not() {
        let environs =
            b"HOME=/home/agent\0ACTIONS_RUNTIME_TOKEN=secret\0\0LANG=C\0COPY=xACTIONS_y\0";
        let named: Vec<String> = variables_holding(environs, b"ACTIONS_")
            .into_iter()
            .collect();
        assert_eq!(named, ["ACTIONS_RUNTIME_TOKEN", "COPY"]);
        assert!(variables_holding(b"HOME=/home/agent\0", b"ACTIONS_").is_empty());
    }

    #[test]
    fn allowed_sockets() {
        let allowed: Vec<String> = [
            "/run/dbus/system_bus_socket",
            "/run/systemd/",
            "/run/x/io.systemd.*",
            "@named",
        ]
        .iter()
        .map(|&s| s.to_owned())
        .collect();
        let cases = [
            ("/run/dbus/system_bus_socket", true),
            ("/run/systemd/journal/stdout", true),
            ("/run/systemd", false),
            ("/run/systemd-other/x", false),
            ("/run/dbus/system_bus_socket2", false),
            ("/run/docker.sock", false),
            ("/run/x/io.systemd.Hostname", true),
            ("/run/x/io.systemd", false),
            ("/run/x/resolve/io.systemd.Resolve", false),
            ("@named", true),
            ("@named-other", false),
        ];
        for (name, want) in cases {
            assert_eq!(socket_allowed(name, &allowed), want, "{name}");
        }
    }

    #[test]
    fn systemd_notification_sockets_by_name() {
        let cases = [
            ("@14028884306257329759", true),
            ("@0", true),
            // More than 64 bits, a name with letters, a path, no name.
            ("@140288843062573297590", false),
            ("@1402888430625732975a", false),
            ("@ISCSIADM_ABSTRACT_NAMESPACE", false),
            ("/run/14028884306257329759", false),
            ("@", false),
        ];
        for (name, want) in cases {
            assert_eq!(systemd_notification_socket(name), want, "{name}");
        }
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
