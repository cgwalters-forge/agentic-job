//! `agentic-job sandbox setup`: create the sandbox user and close its
//! routes to credentials and root. Run as root, once, on a machine that is
//! thrown away after the job.
//!
//! The boundary is the separate uid: it cannot read another uid's
//! `/proc/PID/environ` or memory, and has no sudo. The rest closes what
//! runner images leave open to every local user, which starts to matter
//! once something runs as a user other than the runner's.
//!
//! The runner user's own sudo is left as it is: the supervisor
//! (`sandbox check`, `run`) runs as that user and needs it to enter the
//! sandbox. Taking it away once the sandbox is up is a follow-up in
//! docs/plan.md.

use std::fs;
use std::io::{ErrorKind, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use rustix::fs::Mode;

use super::enter::{Entry, RUN0};
use super::host::{self, User};
use super::{egress, network};
use crate::config::{self, Config, Sandbox};
use crate::exit::Exit;

#[derive(Debug, clap::Args)]
pub struct Args {
    /// The run's configuration (TOML); copied to /etc/agentic-job/
    #[arg(long, value_name = "FILE")]
    pub config: PathBuf,
}

/// Root's: the configuration `run` reads, and what else setup leaves for
/// the supervisor. That it exists also says setup ran here before.
pub const CONFIG_DIR: &str = "/etc/agentic-job";

/// A copy of this program that the sandbox user can run: `sandbox check`
/// starts it there to see the host as that user does. The build it was
/// made from is in the runner's home, which is closed.
pub const SELF_COPY: &str = "/usr/local/libexec/agentic-job";

/// `[setup] script`, where the sandbox user can read it.
const SCRIPT_COPY: &str = "/etc/agentic-job/setup-script";

/// The last matching sudoers rule wins, and this file is read last.
/// No dot in the name: sudo skips such files.
const SUDOERS_DENY: &str = "/etc/sudoers.d/zz-agentic-job-sandbox";

const VISUDO_CHECK: &[&str] = &["visudo", "-q", "-c", "-f"];

/// polkit takes the first rule that answers, and reads this file first;
/// that also covers `pkexec`, and lingering, which is a polkit action.
const POLKIT_RULES_DIR: &str = "/etc/polkit-1/rules.d";

const POLKIT_DENY: &str = "/etc/polkit-1/rules.d/00-agentic-job-sandbox.rules";

/// Something that runs a user's commands later, and its lists of who may
/// use it.
struct Scheduler {
    allow: &'static str,
    deny: &'static str,
    /// Whether every user may use it when neither list exists. Then a
    /// deny list has to be made; where only root may, making one would
    /// open it to everyone the list does not name.
    open_without_lists: bool,
}

/// Nothing may start the sandbox user's processes outside a session,
/// where the supervisor cannot stop them. With neither list, Debian's
/// cron takes everyone (cronie only root, but it ships an empty deny
/// list), and at takes only root.
const SCHEDULERS: &[Scheduler] = &[
    Scheduler {
        allow: "/etc/cron.allow",
        deny: "/etc/cron.deny",
        open_without_lists: true,
    },
    Scheduler {
        allow: "/etc/at.allow",
        deny: "/etc/at.deny",
        open_without_lists: false,
    },
];

/// Sticky world-writable directories, meant to stay that way.
const SHARED_TMP: &[&str] = &["/tmp", "/var/tmp"];

/// Some images leave this group-writable, and ssh refuses to run with a
/// group-writable included configuration: plain ssh and git over ssh then
/// fail for every user but the one the image was built for.
const SSH_CRYPTO_POLICY: &str = "/etc/crypto-policies/back-ends/openssh.config";

const GROUP_WRITE: u32 = 0o020;

/// The permission bits of a mode, without the file type.
const MODE_BITS: u32 = 0o7777;

/// Some images set `XDG_RUNTIME_DIR` to the runner's here, and PAM hands
/// it to every session, which breaks other users' session bus and
/// rootless podman (actions/runner-images#14649). The hosted images also
/// set an `ACTIONS_*` variable here, a path and no secret; it goes too,
/// so that "the sandbox user has no `ACTIONS_*` variable" has no
/// exception to argue about. The runner itself has long read the file.
const ENVIRONMENT: &str = "/etc/environment";

const IMAGE_ENV_VARS: &[&str] = &["XDG_RUNTIME_DIR"];

const PTRACE_SCOPE: &str = "/proc/sys/kernel/yama/ptrace_scope";

const SUBUID: &str = "/etc/subuid";

const RESOLV_CONF: &str = "/etc/resolv.conf";

const PRIVATE_MODE: u32 = 0o700;

/// Root's files that every user reads, and the programs and directories
/// every user enters.
const MODE_FILE: u32 = 0o644;

const MODE_PROGRAM: u32 = 0o755;

/// What sudo wants of a file in sudoers.d.
const MODE_SUDOERS: u32 = 0o440;

const ROOT_UMASK: u32 = 0o022;

/// Package mirrors time out now and then, and one timeout would
/// otherwise fail the whole run.
const INSTALL_ATTEMPTS: u32 = 4;

const INSTALL_PAUSE: Duration = Duration::from_secs(20);

pub fn run(args: &Args) -> Result<Exit> {
    let text = fs::read_to_string(&args.config)
        .with_context(|| format!("reading {}", args.config.display()))?;
    let config = Config::parse(&text).with_context(|| format!("in {}", args.config.display()))?;
    let direct = config.check_host()?.direct;
    ensure!(host::is_root(), "`sandbox setup` must run as root");
    ensure!(
        host::has_program(RUN0),
        "{RUN0} not found: the sandbox is entered with it, and it needs systemd 257 or later"
    );
    // Root's umask is the caller's, and some images build with 000.
    rustix::process::umask(Mode::from_raw_mode(ROOT_UMASK));
    let runner = runner_user(&config)?;
    // Every refusal comes before the first change.
    let existing = preflight(&config, &runner)?;
    if config.egress.proxy {
        let resolv_conf = match fs::read_to_string(RESOLV_CONF) {
            Ok(text) => text,
            Err(err) if err.kind() == ErrorKind::NotFound => String::new(),
            Err(err) => return Err(err).with_context(|| format!("reading {RESOLV_CONF}")),
        };
        let on_tailnet = network::tailnet_resolvers(&resolv_conf);
        ensure!(
            on_tailnet.is_empty(),
            "{RESOLV_CONF} names {} on the tailnet, which the egress proxy may not reach: join the tailnet without its DNS (tailscale up --accept-dns=false)",
            on_tailnet.join(", ")
        );
    }
    fs::create_dir(CONFIG_DIR).with_context(|| format!("creating {CONFIG_DIR}"))?;
    fs::set_permissions(CONFIG_DIR, fs::Permissions::from_mode(MODE_PROGRAM))
        .with_context(|| format!("setting the mode of {CONFIG_DIR}"))?;

    let sandbox = create_user(&config, &runner, existing)?;
    let subuids = subuid_ranges(
        &sandbox,
        &fs::read_to_string(SUBUID).with_context(|| format!("reading {SUBUID}"))?,
    )?;
    drop_image_environment()?;
    close_private_dirs(&config, &runner)?;
    stop_services(&config.sandbox.stop_services)?;
    install_self()?;
    install_packages(&config.setup.packages)?;
    install_npm(&config.setup.npm)?;
    // After the packages: one of them may be what a rule is for (polkit,
    // at), and a rule is written only for what is there.
    deny_privileges(&sandbox.name)?;
    // The proxy first, since the rules name its uid; and both after the
    // installs, which root does on the open network.
    let proxy_uid = config
        .egress
        .proxy
        .then(|| egress::start(&config.egress))
        .transpose()?;
    let uids: Vec<String> = std::iter::once(sandbox.uid.to_string())
        .chain(subuids.iter().cloned())
        .collect();
    network::apply(&network::rules(&uids, &direct, proxy_uid))?;
    match proxy_uid {
        Some(_) => println!(
            "{} reaches the network only through the egress proxy, {}",
            sandbox.name,
            egress::proxy_url()
        ),
        None => println!(
            "{} has no egress proxy: its network is open but for the metadata service and the tailnet",
            sandbox.name
        ),
    }
    // After the installs, which are the last things to write as root.
    strip_world_write()?;
    fix_ssh_crypto_policy()?;
    restrict_ptrace()?;
    println!(
        "Sandbox user {} (uid {}, subordinate uids {}), in addition to {} whose home {} is closed to it",
        sandbox.name,
        sandbox.uid,
        subuids.join(", "),
        runner.name,
        runner.home.display()
    );
    if let Some(script) = &config.setup.script {
        run_setup_script(&config, script)?;
    }
    // Last, so a setup that failed leaves nothing `sandbox check` and
    // `run` would take for a finished one.
    install_file(
        Path::new(config::ROOT_COPY),
        text.as_bytes(),
        MODE_FILE,
        None,
    )?;
    Ok(Exit::Success)
}

/// The user whose job this is: the one that called sudo, unless the
/// configuration names it.
fn runner_user(config: &Config) -> Result<User> {
    let name = match &config.sandbox.runner_user {
        Some(name) => name.clone(),
        None => std::env::var("SUDO_USER").ok().filter(|name| name != "root").context(
            "whose job is this? Start `sandbox setup` with sudo as the runner's user, or set sandbox.runner-user",
        )?,
    };
    let runner = User::lookup(&name)?.with_context(|| format!("no user {name}"))?;
    ensure!(
        runner.uid != 0,
        "the runner user {name} is root: there is nothing to keep from the sandbox user that root's own files do not already hold"
    );
    Ok(runner)
}

/// Where `useradd` makes homes, from its own defaults.
fn home_base() -> Result<PathBuf> {
    let defaults = host::run(Command::new("useradd").arg("-D"))?;
    Ok(defaults
        .lines()
        .find_map(|line| line.strip_prefix("HOME="))
        .map_or_else(|| PathBuf::from("/home"), PathBuf::from))
}

/// The groups of `groups`, as `id -Gn` prints them, that the sandbox
/// user must not be in.
fn forbidden_groups(groups: &str) -> Vec<&str> {
    groups
        .split_whitespace()
        .filter(|group| Sandbox::FORBIDDEN_GROUPS.contains(group))
        .collect()
}

/// Whether any process runs as `uid`. pgrep exits 1 for none; anything
/// else but 0 is pgrep not working, which must not read as none.
fn has_processes(uid: u32) -> Result<bool> {
    let status = Command::new("pgrep")
        .arg("-u")
        .arg(uid.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .status()
        .context("running pgrep")?;
    match status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => bail!("pgrep -u {uid} failed ({status})"),
    }
}

/// Refuses a machine that is not fresh, before anything is changed. A
/// machine that ran a job before would hand this one the previous job's
/// sandbox user, with whatever that agent left to run at its next login.
/// Returns the sandbox user if the image ships it and the configuration
/// says so.
fn preflight(config: &Config, runner: &User) -> Result<Option<User>> {
    ensure!(
        !Path::new(CONFIG_DIR).exists(),
        "{CONFIG_DIR} already exists: `sandbox setup` ran on this machine before, and the sandbox needs a fresh one for every job"
    );
    let name = &config.sandbox.user;
    let Some(user) = User::lookup(name)? else {
        let home = home_base()?.join(name);
        ensure!(
            !home.exists(),
            "{} already exists, with no user {name}: something left it there",
            home.display()
        );
        for group in &config.sandbox.groups {
            ensure!(
                host::succeeds(Command::new("getent").args(["group", "--", group.as_str()])),
                "no group {group}: take it out of sandbox.groups if the sandbox user does not need it"
            );
        }
        return Ok(None);
    };
    ensure!(
        config.sandbox.allow_existing_user,
        "the user {name} already exists: on a fresh machine nothing has made it yet. If the image ships it, set sandbox.allow-existing-user"
    );
    // The configuration's groups were checked; the image's were not.
    let groups = host::run(Command::new("id").args(["-Gn", "--", name.as_str()]))?;
    let forbidden = forbidden_groups(&groups);
    ensure!(
        forbidden.is_empty() && user.gid != 0,
        "the user {name} is in {}: that is root by another name, or reads what the sandbox keeps from the agent",
        if forbidden.is_empty() {
            "root's group".to_owned()
        } else {
            forbidden.join(", ")
        }
    );
    ensure!(
        !has_processes(user.uid)?,
        "processes of {name} are already running"
    );
    ensure_distinct(&user, runner)?;
    Ok(Some(user))
}

/// Also by uid: another name for root or the runner is still them.
fn ensure_distinct(sandbox: &User, runner: &User) -> Result<()> {
    ensure!(
        sandbox.uid != 0 && sandbox.uid != runner.uid,
        "the sandbox user {} has uid {}, root's or {}'s",
        sandbox.name,
        sandbox.uid,
        runner.name
    );
    Ok(())
}

/// The sandbox user: the image's, or a new one with its home, a group of
/// its own, and subordinate uids and gids, which useradd assigns.
fn create_user(config: &Config, runner: &User, existing: Option<User>) -> Result<User> {
    if let Some(user) = existing {
        return Ok(user);
    }
    let name = &config.sandbox.user;
    let mut useradd = Command::new("useradd");
    useradd.args(["--create-home", "--user-group", "--shell", "/bin/bash"]);
    if !config.sandbox.groups.is_empty() {
        useradd.arg("--groups").arg(config.sandbox.groups.join(","));
    }
    host::run(useradd.args(["--", name.as_str()]))?;
    let user = User::lookup(name)?.with_context(|| format!("useradd made no user {name}"))?;
    ensure_distinct(&user, runner)?;
    Ok(user)
}

/// The subordinate uid ranges of `user`, each as `START-END`. Containers
/// the sandbox user starts run as these, so the network rules name them.
pub fn subuid_ranges(user: &User, subuid: &str) -> Result<Vec<String>> {
    // subuid(5) names the owner or gives its uid.
    let uid = user.uid.to_string();
    let ranges: Vec<String> = subuid
        .lines()
        .filter_map(|line| {
            let mut fields = line.split(':');
            let (name, start, count) = (fields.next()?, fields.next()?, fields.next()?);
            let (start, count) = (
                start.parse::<u64>().ok()?,
                count.trim().parse::<u64>().ok()?,
            );
            ((name == user.name || name == uid) && count > 0)
                .then(|| format!("{start}-{}", start + count - 1))
        })
        .collect();
    ensure!(
        !ranges.is_empty(),
        "{} has no subordinate uids in {SUBUID}, which rootless podman needs",
        user.name
    );
    Ok(ranges)
}

fn sudoers_deny(user: &str) -> String {
    format!("{user} ALL=(ALL:ALL) !ALL\n")
}

fn polkit_deny(user: &str) -> String {
    format!(
        "polkit.addRule(function(action, subject) {{\n  if (subject.user == \"{user}\") return polkit.Result.NO;\n}});\n"
    )
}

/// What to add to a deny list `text` so that it names `user`, if it does
/// not already.
fn denial(text: &str, user: &str) -> Option<String> {
    if text.lines().any(|line| line.trim() == user) {
        return None;
    }
    let separator = if text.is_empty() || text.ends_with('\n') {
        ""
    } else {
        "\n"
    };
    Some(format!("{separator}{user}\n"))
}

/// Takes away the sandbox user's sudo, its polkit actions, and every way
/// to leave a process behind for later: cron, at, and a lingering user
/// manager.
fn deny_privileges(user: &str) -> Result<()> {
    install_file(
        Path::new(SUDOERS_DENY),
        sudoers_deny(user).as_bytes(),
        MODE_SUDOERS,
        Some(VISUDO_CHECK),
    )?;
    if Path::new(POLKIT_RULES_DIR).is_dir() {
        install_file(
            Path::new(POLKIT_DENY),
            polkit_deny(user).as_bytes(),
            MODE_FILE,
            None,
        )?;
    }
    for scheduler in SCHEDULERS {
        scheduler.deny(user)?;
    }
    host::run(Command::new("loginctl").args(["disable-linger", "--", user]))?;
    Ok(())
}

impl Scheduler {
    /// Keeps `user` from this scheduler. A deny list that is there is
    /// added to in place, keeping its owner and mode, which its readers
    /// depend on. One we make is world-readable: cron's own programs are
    /// not root, and one that cannot read the list allows everyone.
    fn deny(&self, user: &str) -> Result<()> {
        let (allow, deny) = (Path::new(self.allow), Path::new(self.deny));
        // An allow list is all that is read when there is one.
        match fs::read_to_string(allow) {
            Ok(allowed) => {
                ensure!(
                    !allowed.lines().any(|line| line.trim() == user),
                    "{} lists {user}: the sandbox user must not be able to schedule commands",
                    self.allow
                );
                return Ok(());
            }
            Err(err) if err.kind() == ErrorKind::NotFound => {}
            Err(err) => return Err(err).with_context(|| format!("reading {}", self.allow)),
        }
        match fs::read_to_string(deny) {
            Ok(text) => denial(&text, user).map_or(Ok(()), |line| {
                fs::OpenOptions::new()
                    .append(true)
                    .open(deny)
                    .and_then(|mut file| file.write_all(line.as_bytes()))
                    .with_context(|| format!("adding {user} to {}", self.deny))
            }),
            Err(err) if err.kind() == ErrorKind::NotFound && self.open_without_lists => {
                install_file(deny, format!("{user}\n").as_bytes(), MODE_FILE, None)
            }
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err).with_context(|| format!("reading {}", self.deny)),
        }
    }
}

/// Whether a variable the image sets for every session is one the
/// sandbox user's must not get.
fn image_variable(name: &str) -> bool {
    IMAGE_ENV_VARS.contains(&name) || name.starts_with(Sandbox::FORBIDDEN_ENV_PREFIX)
}

/// `text`, which is environment(5) lines, without those that set a
/// variable `dropped` names; and the names that went.
fn without_variables(text: &str, dropped: impl Fn(&str) -> bool) -> Option<(String, Vec<String>)> {
    let name = |line: &str| -> Option<String> {
        let line = line.trim_start();
        let line = line.strip_prefix("export ").map_or(line, str::trim_start);
        let (name, _) = line.split_once('=')?;
        dropped(name).then(|| name.to_owned())
    };
    let (gone, kept): (Vec<&str>, Vec<&str>) =
        text.split('\n').partition(|line| name(line).is_some());
    let names: Vec<String> = gone.into_iter().filter_map(name).collect();
    (!names.is_empty()).then(|| (kept.join("\n"), names))
}

fn drop_image_environment() -> Result<()> {
    let path = Path::new(ENVIRONMENT);
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(()),
        // Left unread, its variables would stay.
        Err(err) => return Err(err).with_context(|| format!("reading {ENVIRONMENT}")),
    };
    if let Some((kept, names)) = without_variables(&text, image_variable) {
        let mode = fs::metadata(path)
            .with_context(|| format!("reading the mode of {ENVIRONMENT}"))?
            .permissions()
            .mode()
            & MODE_BITS;
        install_file(path, kept.as_bytes(), mode, None)?;
        println!("Dropped {} from {ENVIRONMENT}", names.join(", "));
    }
    Ok(())
}

/// The directories the sandbox user must not see into: the runner's home,
/// which holds the runner's credentials and the job's files; the ones
/// every runner of the old tree had closed; and the configured ones. The
/// configuration adds to the list and cannot shorten it.
pub fn private_dirs(config: &Config, runner: &User) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::iter::once(runner.home.clone())
        .chain(Sandbox::ALWAYS_PRIVATE_DIRS.iter().map(PathBuf::from))
        .chain(config.sandbox.private_dirs.iter().cloned())
        .collect();
    let mut seen = std::collections::BTreeSet::new();
    dirs.retain(|dir| seen.insert(dir.clone()));
    dirs
}

fn close_private_dirs(config: &Config, runner: &User) -> Result<()> {
    for dir in private_dirs(config, runner)
        .iter()
        .filter(|dir| dir.is_dir())
    {
        fs::set_permissions(dir, fs::Permissions::from_mode(PRIVATE_MODE))
            .with_context(|| format!("closing {}", dir.display()))?;
    }
    Ok(())
}

/// `find` arguments that match what is world-writable under `root` and
/// not sticky, on that one filesystem, outside the shared temporary
/// directories. `sandbox check` looks with the same ones.
pub fn world_writable_find(root: &str) -> Vec<String> {
    let prune = SHARED_TMP.iter().flat_map(|dir| ["-path", *dir, "-o"]);
    [root, "-xdev", "("]
        .into_iter()
        .chain(prune)
        .chain([
            "-false", ")", "-prune", "-o", "(", "-type", "f", "-o", "-type", "d", ")", "-perm",
            "-0002", "!", "-perm", "-1000",
        ])
        .map(str::to_owned)
        .collect()
}

/// World-writable system files include ones root loads code from (a
/// polkit rule, a unit): any user could make itself root through them.
/// Some runner images are built with umask 000.
fn strip_world_write() -> Result<()> {
    let fixed = host::run(
        Command::new("find")
            .args(world_writable_find("/"))
            .args(["-print", "-exec", "chmod", "o-w", "{}", "+"]),
    )?;
    let paths: Vec<&str> = fixed.lines().collect();
    if !paths.is_empty() {
        let shown: Vec<&str> = paths.iter().copied().take(10).collect();
        println!(
            "Removed world write access from {} paths, such as:\n  {}",
            paths.len(),
            shown.join("\n  ")
        );
    }
    Ok(())
}

fn fix_ssh_crypto_policy() -> Result<()> {
    let path = Path::new(SSH_CRYPTO_POLICY);
    let Ok(metadata) = fs::metadata(path) else {
        return Ok(());
    };
    let mode = metadata.permissions().mode() & MODE_BITS;
    fs::set_permissions(path, fs::Permissions::from_mode(mode & !GROUP_WRITE))
        .with_context(|| format!("setting the mode of {SSH_CRYPTO_POLICY}"))
}

/// Hardening only (the uid split is the boundary): some images let any
/// process attach to any other of its uid. A stricter setting is left.
fn restrict_ptrace() -> Result<()> {
    let scope = fs::read_to_string(PTRACE_SCOPE)
        .with_context(|| format!("reading {PTRACE_SCOPE} (a kernel without Yama?)"))?;
    if scope.trim() == "0" {
        fs::write(PTRACE_SCOPE, "1\n").with_context(|| format!("writing {PTRACE_SCOPE}"))?;
    }
    Ok(())
}

fn stop_services(units: &[String]) -> Result<()> {
    let (present, absent): (Vec<&String>, Vec<&String>) = units.iter().partition(|unit| {
        host::succeeds(Command::new("systemctl").args(["cat", "--", unit.as_str()]))
    });
    if !absent.is_empty() {
        let names: Vec<&str> = absent.iter().map(|unit| unit.as_str()).collect();
        println!("Not on this machine, so not stopped: {}", names.join(", "));
    }
    if !present.is_empty() {
        host::run(
            Command::new("systemctl")
                .args(["stop", "--"])
                .args(&present),
        )?;
        let names: Vec<&str> = present.iter().map(|unit| unit.as_str()).collect();
        println!("Stopped {}", names.join(", "));
    }
    Ok(())
}

fn install_self() -> Result<()> {
    let own = std::env::current_exe().context("finding this program's own file")?;
    let bytes = fs::read(&own).with_context(|| format!("reading {}", own.display()))?;
    let dest = Path::new(SELF_COPY);
    if let Some(dir) = dest.parent() {
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    install_file(dest, &bytes, MODE_PROGRAM, None)
}

/// How the host installs packages, and what makes a failed attempt worth
/// repeating.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PackageManager {
    Dnf,
    Apt,
}

impl PackageManager {
    fn detect() -> Option<Self> {
        [(Self::Dnf, "dnf"), (Self::Apt, "apt-get")]
            .into_iter()
            .find_map(|(manager, program)| host::has_program(program).then_some(manager))
    }

    fn install(self, packages: &[String]) -> Command {
        let mut command = match self {
            Self::Dnf => {
                let mut command = Command::new("dnf");
                command.args(["install", "-y", "--"]);
                command
            }
            Self::Apt => {
                let mut command = Command::new("apt-get");
                command.env("DEBIAN_FRONTEND", "noninteractive");
                command.args(["install", "-y", "--no-install-recommends", "--"]);
                command
            }
        };
        command.args(packages);
        command
    }

    /// Run before every attempt but dnf's first: a half-fetched index is
    /// not used again, and apt's may be older than the image.
    fn refresh(self) -> Command {
        let (program, args): (&str, &[&str]) = match self {
            Self::Dnf => ("dnf", &["clean", "expire-cache"]),
            Self::Apt => ("apt-get", &["update", "-q"]),
        };
        let mut command = Command::new(program);
        command.args(args);
        command
    }
}

fn install_packages(packages: &[String]) -> Result<()> {
    if packages.is_empty() {
        return Ok(());
    }
    let manager = PackageManager::detect()
        .context("setup.packages: this host has neither dnf nor apt-get")?;
    for attempt in 1..=INSTALL_ATTEMPTS {
        if attempt > 1 || manager == PackageManager::Apt {
            let _ = host::output(&mut manager.refresh());
        }
        // Its output is the job log's: what was installed, or why not.
        if manager
            .install(packages)
            .status()
            .is_ok_and(|status| status.success())
        {
            return Ok(());
        }
        eprintln!("warning: installing packages failed (attempt {attempt} of {INSTALL_ATTEMPTS})");
        if attempt < INSTALL_ATTEMPTS {
            std::thread::sleep(INSTALL_PAUSE * attempt);
        }
    }
    bail!(
        "installing {} failed {INSTALL_ATTEMPTS} times",
        packages.join(" ")
    )
}

/// The agent programs. Their versions are exact (the configuration is
/// refused otherwise), so a run is repeatable; their install scripts run
/// as root here, as in the old tree, which is why the list is the
/// caller's and not the task's.
fn install_npm(specs: &[String]) -> Result<()> {
    if specs.is_empty() {
        return Ok(());
    }
    ensure!(
        host::has_program("npm"),
        "setup.npm: no npm on this host; add it to setup.packages"
    );
    let status = Command::new("npm")
        .args(["install", "-g", "--no-audit", "--no-fund", "--"])
        .args(specs)
        .status()
        .context("running npm")?;
    ensure!(
        status.success(),
        "npm install -g {} failed ({status})",
        specs.join(" ")
    );
    Ok(())
}

/// `[setup] script`, as the sandbox user in its home: for toolchains,
/// which then belong to the user that runs them. Never as root.
fn run_setup_script(config: &Config, script: &Path) -> Result<()> {
    let bytes = fs::read(script).with_context(|| format!("reading {}", script.display()))?;
    install_file(Path::new(SCRIPT_COPY), &bytes, MODE_PROGRAM, None)?;
    let entry = Entry::new(config)?;
    let output = entry.run(&[SCRIPT_COPY], b"")?;
    std::io::stdout().write_all(&output.stdout)?;
    std::io::stderr().write_all(&output.stderr)?;
    ensure!(
        output.success(),
        "setup.script failed as {} ({})",
        config.sandbox.user,
        output.status
    );
    Ok(())
}

/// Installs a root-owned file with `mode`, once `validate` (a command the
/// path is appended to), if given, accepts it. Written beside the
/// destination and renamed, so a reader never sees half of it.
fn install_file(path: &Path, content: &[u8], mode: u32, validate: Option<&[&str]>) -> Result<()> {
    let mut name = path.as_os_str().to_owned();
    name.push(".tmp");
    let tmp = PathBuf::from(name);
    let written = (|| -> Result<()> {
        // Not through a link someone left at that name.
        let _ = fs::remove_file(&tmp);
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&tmp)
            .with_context(|| format!("creating {}", tmp.display()))?;
        file.write_all(content)?;
        // The mode given at creation went through the umask.
        file.set_permissions(fs::Permissions::from_mode(mode))?;
        drop(file);
        if let Some(validate) = validate {
            host::run(host::command(validate)?.arg(&tmp))
                .with_context(|| format!("{} would not be valid", path.display()))?;
        }
        fs::rename(&tmp, path).with_context(|| format!("installing {}", path.display()))
    })();
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written?;
    // SELinux: give the new file the label its place calls for.
    if host::has_program("restorecon") {
        host::run(Command::new("restorecon").arg(path))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subordinate_uid_ranges() {
        let agent = |name: &str| User {
            name: name.to_owned(),
            uid: 1001,
            gid: 1001,
            home: "/home/agent".into(),
        };
        let subuid = "runner:100000:65536\nagent:165536:65536\n1001:300000:1000\nother:1:1\n";
        assert_eq!(
            subuid_ranges(&agent("agent"), subuid).unwrap(),
            ["165536-231071", "300000-300999"]
        );
        for (user, text) in [
            ("agent", ""),
            ("agent", "agent:x:1\n"),
            ("agen", "agent:165536:65536\n"),
            ("agent", "agent:5:0\n"),
        ] {
            assert!(
                subuid_ranges(&agent(user), text).is_err(),
                "{user} in {text:?}"
            );
        }
    }

    #[test]
    fn groups_an_image_gave_the_user() {
        assert_eq!(forbidden_groups("agent kvm render"), Vec::<&str>::new());
        assert_eq!(forbidden_groups("agent docker kvm adm"), ["docker", "adm"]);
    }

    #[test]
    fn private_directories_add_to_the_defaults() {
        let runner = User {
            name: "runner".into(),
            uid: 1001,
            gid: 1001,
            home: "/home/runner".into(),
        };
        let config =
            Config::parse("[sandbox]\nprivate-dirs = [\"/srv/work\", \"/opt/hca\"]").unwrap();
        let dirs: Vec<String> = private_dirs(&config, &runner)
            .iter()
            .map(|dir| dir.display().to_string())
            .collect();
        assert_eq!(
            dirs,
            [
                "/home/runner",
                "/opt/hca",
                "/var/run/tailscale",
                "/srv/work"
            ]
        );
    }

    #[test]
    fn deny_files() {
        assert_eq!(sudoers_deny("agent"), "agent ALL=(ALL:ALL) !ALL\n");
        assert_eq!(
            polkit_deny("agent"),
            "polkit.addRule(function(action, subject) {\n  if (subject.user == \"agent\") return polkit.Result.NO;\n});\n"
        );
        let cases = [
            ("", Some("agent\n")),
            ("daemon\n", Some("agent\n")),
            ("daemon", Some("\nagent\n")),
            ("daemon\nagent\n", None),
            ("agent", None),
            // Another user's name that only contains ours.
            ("agent2\n", Some("agent\n")),
        ];
        for (text, want) in cases {
            assert_eq!(denial(text, "agent").as_deref(), want, "{text:?}");
        }
    }

    #[test]
    fn image_variables_are_dropped_and_nothing_else() {
        let cases = [
            (
                "PATH=/usr/bin\nXDG_RUNTIME_DIR=/run/user/1001\nLANG=C\n",
                Some(("PATH=/usr/bin\nLANG=C\n", vec!["XDG_RUNTIME_DIR"])),
            ),
            ("  XDG_RUNTIME_DIR=/x", Some(("", vec!["XDG_RUNTIME_DIR"]))),
            (
                "A=1\nexport ACTIONS_RUNNER_ACTION_ARCHIVE_CACHE=/opt/cache\nACTIONS_X=\"y\"\n",
                Some((
                    "A=1\n",
                    vec!["ACTIONS_RUNNER_ACTION_ARCHIVE_CACHE", "ACTIONS_X"],
                )),
            ),
            ("PATH=/usr/bin\n", None),
            ("XDG_RUNTIME_DIR_OTHER=1\n", None),
            ("GITHUB_ACTIONS_X=1\nNOTE=ACTIONS_=1\n", None),
            ("# XDG_RUNTIME_DIR=/x\n", None),
        ];
        for (text, want) in cases {
            let got = without_variables(text, image_variable);
            let want = want.map(|(kept, names): (&str, Vec<&str>)| {
                (
                    kept.to_owned(),
                    names.into_iter().map(str::to_owned).collect::<Vec<_>>(),
                )
            });
            assert_eq!(got, want, "{text:?}");
        }
    }

    #[test]
    fn find_arguments_match_the_old_tree() {
        assert_eq!(
            world_writable_find("/").join(" "),
            "/ -xdev ( -path /tmp -o -path /var/tmp -o -false ) -prune -o ( -type f -o -type d ) -perm -0002 ! -perm -1000"
        );
    }

    #[test]
    fn package_commands() {
        let packages = ["just".to_owned(), "gcc".to_owned()];
        let args = |command: &Command| -> Vec<String> {
            command
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect()
        };
        assert_eq!(
            args(&PackageManager::Dnf.install(&packages)),
            ["install", "-y", "--", "just", "gcc"]
        );
        assert_eq!(
            args(&PackageManager::Apt.install(&packages)),
            [
                "install",
                "-y",
                "--no-install-recommends",
                "--",
                "just",
                "gcc"
            ]
        );
    }
}
