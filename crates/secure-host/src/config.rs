//! The tables of a host's configuration that the host setup reads, as
//! agentic-job's `--config` file and a host's own file both have them.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail, ensure};
use serde::Deserialize;

use crate::network::Direct;

/// Where `sandbox setup` puts the root-owned copy.
pub const ROOT_COPY: &str = "/etc/agentic-job/config.toml";

/// A host's configuration file as the program built on this crate has
/// it: these tables and any of its own, read and checked as that program
/// does. The helper reads the root-owned copy ([`ROOT_COPY`]) through
/// it, and the way into the sandbox ([`crate::enter::Entry::new`]) is
/// made from it. agentic-job's `Config`, with its run tables, is one.
pub trait HostFile: Sized {
    /// Variables of the program's own that no command in the sandbox
    /// gets, besides the job's: agentic-job's run credentials.
    const UNSET: &'static [&'static str] = &[];

    /// Reads the file at PATH.
    fn load(path: &Path) -> Result<Self>;

    fn sandbox(&self) -> &Sandbox;

    fn egress(&self) -> &Egress;

    /// Whether setup installs tools in the sandbox user's home, whose
    /// `.local/bin` then leads that user's PATH.
    fn tools(&self) -> bool;

    /// Checks the tables that set up the host, as setup does, and
    /// returns where the sandbox user connects without the egress proxy.
    fn direct(&self) -> Result<Vec<Direct>>;

    /// The helper's `session-serve`, as root: what the program has the
    /// host serve for a session, decided by this file alone. Refused
    /// unless the program has one.
    fn session_serve(&self) -> Result<()> {
        bail!("this program serves no session")
    }
}

/// The sandbox user, what is closed to it, and what `sandbox check`
/// probes with. Step 5.
#[derive(Debug, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct Sandbox {
    /// Whether `sandbox setup` creates the sandbox user, the separate uid
    /// an agent runs as and `helper enter` is the one way into. On by
    /// default. Off, the host hardens the runner's own user alone (the
    /// lock, the metadata rule, the egress proxy and the rest): for plain
    /// CI that runs nothing as another user, which then has no account
    /// with a home, subordinate uids and a session to leave anything in.
    pub create_user: bool,
    pub user: String,
    /// Supplementary groups of the sandbox user; unset,
    /// [`Self::DEFAULT_GROUPS`] ([`Self::groups`]).
    pub groups: Option<Vec<String>>,
    /// An image may ship the sandbox user; otherwise one that exists
    /// means an earlier job left it, and `sandbox setup` refuses.
    pub allow_existing_user: bool,
    /// Whose home is closed. `sandbox setup` takes it from `SUDO_USER`
    /// when this is not set.
    pub runner_user: Option<String>,
    /// Whether `sandbox setup` ends by taking root away from the runner's
    /// user, leaving it the one sudo rule for the helper
    /// ([`crate::helper`]). On by default; off only for a machine that
    /// must keep root for the runner's user after the job, which the
    /// docs say is no machine an agent should run on.
    pub lock_runner: bool,
    /// Confine the sandbox user's commands to a read-only view of the host,
    /// writable only in its home and the temporary directories. Off by
    /// default; the world-write policy applies either way.
    pub filesystem_view: bool,
    /// Whether the network rules also refuse the runner's user (and its
    /// subordinate uids) the cloud metadata service, as they always
    /// refuse the sandbox user: a step of the job, a third-party action
    /// among them, then cannot ask it for the instance's identity or a
    /// cloud role's credentials. Unset, [`Self::lock_runner`]'s value
    /// ([`Self::deny_runner_metadata`]); needs it, since with sudo the
    /// runner's user could remove the rule.
    pub deny_runner_metadata: Option<bool>,
    /// Whether `sandbox setup` unlinks the .NET diagnostic sockets of
    /// GitHub's `Runner.Worker`, through which any step, as the Worker's
    /// own user, could have it dump its memory and the job's secrets with
    /// it, around Yama ([`crate::worker_diagnostics`]). Unset,
    /// [`Self::lock_runner`]'s value ([`Self::unlink_worker_diagnostics`]);
    /// needs it, since with sudo the runner's user reads the Worker's
    /// memory anyway.
    pub unlink_worker_diagnostics: Option<bool>,
    /// Whether the secure-host image loads the world-write policy with
    /// its runner-read rule on (`secure-host world-write
    /// --deny-runner-reads`), so that a step cannot read the environment,
    /// and the tokens in it, of a process an earlier step left running.
    /// Only the image reads it (agentic-job's `secure_host::image`), and
    /// it refuses a host with no single `Runner.Worker` to exempt. Unset,
    /// [`Self::lock_runner`]'s value ([`Self::deny_runner_reads`]); needs
    /// it, since with sudo the runner's user could clear the rule's map.
    pub deny_runner_reads: Option<bool>,
    /// Whether `sandbox setup` closes systemd-resolved to every user but
    /// root and, on the bus, the runner's. resolved answers any local user over its
    /// Varlink socket and the system bus, as itself, so the uid rules do
    /// not stop the sandbox user sending names out through it, around
    /// the egress proxy. Off by default; a host without resolved has
    /// nothing to close.
    pub close_resolved: bool,
    /// More directories closed to every user but their owner, besides
    /// the runner user's home and [`Sandbox::ALWAYS_PRIVATE_DIRS`]. One
    /// that does not exist is skipped.
    pub private_dirs: Vec<PathBuf>,
    /// systemd units stopped and masked until reboot by `sandbox setup`,
    /// a container daemon for one, with the units that trigger them.
    pub stop_services: Vec<String>,
    /// Variables every command of the sandbox user gets, over the fixed
    /// ones (see [`crate::enter`]).
    pub env: BTreeMap<String, String>,
    pub check: SandboxCheck,
}

impl Sandbox {
    /// The old tree's name, which its readers and docs know.
    pub const DEFAULT_USER: &'static str = "runner-sandbox";

    /// `/dev/kvm` is what the work this was built for needs. Not
    /// `libvirt`: its polkit rule grants all of `qemu:///system`.
    pub const DEFAULT_GROUPS: &'static [&'static str] = &["kvm"];

    /// The hosted compute agent's token on some runner images, and
    /// tailscaled's socket, whose API dials the tailnet for any user.
    /// Closed whatever the configuration says.
    pub const ALWAYS_PRIVATE_DIRS: &'static [&'static str] = &["/opt/hca", "/var/run/tailscale"];

    /// Membership of these is root by another name, or reads what the
    /// sandbox is there to keep from the agent.
    pub const FORBIDDEN_GROUPS: &'static [&'static str] = &[
        "root",
        "wheel",
        "sudo",
        "admin",
        "adm",
        "disk",
        "shadow",
        "docker",
        "podman",
        "lxd",
        "incus",
        "incus-admin",
        "libvirt",
        "systemd-journal",
    ];

    /// Variables that let a process ask for the job's identity token.
    pub const FORBIDDEN_ENV_PREFIX: &'static str = "ACTIONS_";

    pub fn groups(&self) -> Vec<String> {
        self.groups.clone().unwrap_or_else(|| {
            Self::DEFAULT_GROUPS
                .iter()
                .map(|&group| group.to_owned())
                .collect()
        })
    }

    pub fn deny_runner_metadata(&self) -> bool {
        self.deny_runner_metadata.unwrap_or(self.lock_runner)
    }

    pub fn unlink_worker_diagnostics(&self) -> bool {
        self.unlink_worker_diagnostics.unwrap_or(self.lock_runner)
    }

    pub fn deny_runner_reads(&self) -> bool {
        self.deny_runner_reads.unwrap_or(self.lock_runner)
    }

    /// The rules that confine the runner's own user beyond the lock, by
    /// key, whether each is on, and what every later step of the job can
    /// still do without it.
    pub fn runner_rules(&self) -> [(&'static str, bool, &'static str); 3] {
        [
            (
                "deny-runner-metadata",
                self.deny_runner_metadata(),
                "reaches the cloud metadata service, the instance's identity and a cloud role's credentials",
            ),
            (
                "unlink-worker-diagnostics",
                self.unlink_worker_diagnostics(),
                "can have Runner.Worker dump its memory, and the job's secrets, through its .NET diagnostic sockets",
            ),
            (
                "deny-runner-reads",
                self.deny_runner_reads(),
                "reads the environment, and the tokens in it, of a process an earlier step left running",
            ),
        ]
    }

    /// Why each rule a locked host's file turned off leaves the runner's
    /// user less confined, for setup and check to say. Without the lock,
    /// setup's message about it covers them.
    pub fn runner_rule_opt_outs(&self, runner: &str) -> Vec<String> {
        if !self.lock_runner {
            return Vec::new();
        }
        self.runner_rules()
            .into_iter()
            .filter(|(_, on, _)| !on)
            .map(|(key, _, off)| format!("sandbox.{key} = false: {runner} {off}"))
            .collect()
    }

    /// What the types cannot say. The user's name goes into a sudoers
    /// file and a polkit rule, so it is held to the portable character
    /// set and not to what `useradd` happens to accept.
    pub fn validate(&self) -> Result<()> {
        let name_ok = |name: &str| {
            let mut chars = name.chars();
            chars
                .next()
                .is_some_and(|c| c.is_ascii_lowercase() || c == '_')
                && name.len() <= 32
                && chars
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
        };
        ensure!(
            name_ok(&self.user) && self.user != "root",
            "sandbox.user: {:?} is not a user name the sandbox can have",
            self.user
        );
        if let Some(runner) = &self.runner_user {
            ensure!(
                name_ok(runner) && *runner != self.user,
                "sandbox.runner-user: {runner:?} must be a user other than the sandbox user"
            );
        }
        ensure!(
            self.lock_runner || !self.deny_runner_metadata(),
            "sandbox.deny-runner-metadata needs sandbox.lock-runner: a runner's user with sudo could remove the rule"
        );
        ensure!(
            self.lock_runner || !self.unlink_worker_diagnostics(),
            "sandbox.unlink-worker-diagnostics needs sandbox.lock-runner: a runner's user with sudo reads the Worker's memory anyway"
        );
        ensure!(
            self.lock_runner || !self.deny_runner_reads(),
            "sandbox.deny-runner-reads needs sandbox.lock-runner: a runner's user with sudo could clear the rule"
        );
        // What only the sandbox user has, which nothing would make.
        ensure!(
            self.create_user || !self.allow_existing_user,
            "sandbox.allow-existing-user needs sandbox.create-user: without it there is no sandbox user to take from the image"
        );
        ensure!(
            self.create_user || !self.filesystem_view,
            "sandbox.filesystem-view needs sandbox.create-user: the view confines the sandbox user's commands"
        );
        ensure!(
            self.create_user || self.groups.as_ref().is_none_or(Vec::is_empty),
            "sandbox.groups needs sandbox.create-user: they are the sandbox user's groups"
        );
        for group in &self.groups() {
            ensure!(
                name_ok(group),
                "sandbox.groups: {group:?} is not a group name"
            );
            ensure!(
                !Self::FORBIDDEN_GROUPS.contains(&group.as_str()),
                "sandbox.groups: membership of {group} would give the sandbox user root or the host's secrets"
            );
        }
        for dir in &self.private_dirs {
            ensure!(
                dir.is_absolute(),
                "sandbox.private-dirs: {} is not an absolute path",
                dir.display()
            );
        }
        for unit in &self.stop_services {
            ensure!(
                unit.contains('.') && !unit.starts_with('-') && !unit.contains(char::is_whitespace),
                "sandbox.stop-services: {unit:?} is not a unit name such as docker.service"
            );
        }
        for (name, value) in &self.env {
            ensure!(
                !name.is_empty()
                    && !name.starts_with(|c: char| c.is_ascii_digit())
                    && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'),
                "sandbox.env: {name:?} is not a variable name"
            );
            ensure!(
                !name.starts_with(Self::FORBIDDEN_ENV_PREFIX),
                "sandbox.env: {name} is refused: {}* variables are the job's, never the sandbox user's",
                Self::FORBIDDEN_ENV_PREFIX
            );
            ensure!(
                !value.contains(['\0', '\n']),
                "sandbox.env: the value of {name} has a newline or NUL in it"
            );
        }
        self.check.validate()
    }
}

impl Default for Sandbox {
    fn default() -> Self {
        Self {
            create_user: true,
            user: Self::DEFAULT_USER.to_owned(),
            groups: None,
            allow_existing_user: false,
            runner_user: None,
            lock_runner: true,
            filesystem_view: false,
            deny_runner_metadata: None,
            unlink_worker_diagnostics: None,
            deny_runner_reads: None,
            close_resolved: false,
            private_dirs: Vec::new(),
            stop_services: Vec::new(),
            env: BTreeMap::new(),
            check: SandboxCheck::default(),
        }
    }
}

/// What `sandbox check` probes with, `[sandbox.check]`. Step 5.
#[derive(Debug, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct SandboxCheck {
    /// A public page the sandbox user must reach: the positive control
    /// of every probe that expects a connection to fail.
    pub control_url: String,
    /// A small image that has `curl`, for the probes from a container.
    pub container_image: String,
    /// Whether the probes pull and run `container-image`. Off, rootless
    /// Podman stays usable to the sandbox user, but what its subordinate
    /// uids reach on the network, and whether they read the run token,
    /// goes unprobed: each skipped probe is named in the check's notes.
    /// A host that does not accept that should remove podman instead.
    pub containers: bool,
    /// A repository the egress probes fetch from with `git ls-remote`,
    /// or expect the proxy to refuse that fetch where its policy does.
    pub fetch_repo: String,
    /// Unix sockets the sandbox user may connect to, besides the host's
    /// own (`sandbox::check` has that list): a path, a directory ending
    /// in `/` for everything under it, a path ending in `*` for every
    /// path that starts so, or `@name` for an abstract one.
    pub allow_sockets: Vec<String>,
    /// Local TCP ports the sandbox user may connect to.
    pub allow_tcp_ports: Vec<u16>,
    /// Whether `run` makes these probes again once it has cloned the
    /// target, before it registers the run, and also tries to read the
    /// run token from a rootless container as a subordinate uid. Off, the run action's `sandbox check
    /// --unless-passed` before `run` is the last full check: what the
    /// sandbox user did in `run`'s own preparation (the clone, the
    /// agent's files), and a token that a container's other uids could
    /// read, would go unprobed. It costs a full check's time.
    pub before_agent: bool,
}

impl SandboxCheck {
    pub const DEFAULT_CONTROL_URL: &'static str = "https://github.com/";

    pub const DEFAULT_CONTAINER_IMAGE: &'static str =
        "registry.access.redhat.com/ubi10/ubi-minimal";

    pub const DEFAULT_FETCH_REPO: &'static str = "https://github.com/bootc-dev/bootc";

    fn validate(&self) -> Result<()> {
        for (key, url) in [
            ("control-url", &self.control_url),
            ("fetch-repo", &self.fetch_repo),
        ] {
            ensure!(
                url.starts_with("https://") || url.starts_with("http://"),
                "sandbox.check.{key}: {url:?} is not an http or https URL"
            );
        }
        // git appends its own paths to it, which the check judges against
        // the egress policy: a query or fragment would put them after it,
        // and credentials would be printed with every probe.
        let rest = self
            .fetch_repo
            .split_once("://")
            .map_or("", |(_, rest)| rest);
        ensure!(
            !rest.is_empty()
                && !rest.starts_with('/')
                && !rest.contains(['?', '#', '@'])
                && !rest.chars().any(char::is_whitespace),
            "sandbox.check.fetch-repo: {:?} must be a repository URL with a host, and no \
             credentials, query or fragment",
            self.fetch_repo
        );
        ensure!(
            !self.container_image.is_empty() && !self.container_image.starts_with('-'),
            "sandbox.check.container-image: {:?} is not an image name",
            self.container_image
        );
        for socket in &self.allow_sockets {
            ensure!(
                socket.starts_with('/') || socket.starts_with('@'),
                "sandbox.check.allow-sockets: {socket:?} is neither a path nor an @abstract name"
            );
        }
        Ok(())
    }
}

impl Default for SandboxCheck {
    fn default() -> Self {
        Self {
            control_url: Self::DEFAULT_CONTROL_URL.to_owned(),
            container_image: Self::DEFAULT_CONTAINER_IMAGE.to_owned(),
            containers: true,
            fetch_repo: Self::DEFAULT_FETCH_REPO.to_owned(),
            allow_sockets: Vec::new(),
            allow_tcp_ports: Vec::new(),
            before_agent: false,
        }
    }
}

/// The sandbox user's way out to the network.
#[derive(Debug, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct Egress {
    /// Whether the sandbox user reaches the network only through the
    /// egress proxy. Off, its network is open but for the cloud metadata
    /// service and the tailnet.
    pub proxy: bool,
    /// URLs the sandbox user reaches directly and not through the egress
    /// proxy: the inference proxy, so that the run token never crosses
    /// the egress proxy. Each is an IPv4 address on the tailnet, with a
    /// port; with any, the rest of the tailnet is closed to the sandbox
    /// user.
    pub direct: Vec<String>,
    /// The write rules, in place of the ones that come with the binary
    /// (`egress/policy.toml`).
    pub policy: Option<PathBuf>,
    /// Whether the proxy is this binary's own (`sandbox egress-proxy`)
    /// rather than mitmproxy: the same policy, and no Python or package
    /// index at setup. On by default; `false` keeps mitmproxy, which
    /// needs Python's venv module (`python3-venv`) in `[setup] packages`,
    /// until it is removed (#919).
    pub rust_proxy: bool,
    /// Whether every process of the runner's user (and its subordinate
    /// uids), each later step and action of the job and the runner
    /// itself, is held to the proxy's write rules too, with the runner's
    /// own writes added (cgwalters-forge/actions's
    /// secure-host/docs/egress.md): a step can then read from
    /// anywhere but send a job's secrets or sources only where a rule
    /// allows. Off by default; needs the Rust proxy and
    /// `sandbox.lock-runner`, without which the runner's user could
    /// remove the rules.
    pub write_policy: bool,
}

impl Default for Egress {
    fn default() -> Self {
        Self {
            proxy: true,
            direct: Vec::new(),
            policy: None,
            rust_proxy: true,
            write_policy: false,
        }
    }
}
