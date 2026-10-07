//! The `--config` file: one TOML file for a run, with a table per
//! concern. `sandbox setup` reads the caller's file and copies it to
//! [`ROOT_COPY`]; `run` reads only that copy, which the sandbox user
//! cannot change.
//!
//! Each table's struct belongs to the step of docs/plan.md that uses it
//! (see docs/layout.md). The fields here are the ones the plan names;
//! their step may rename or add to them, and checks what the types
//! cannot say (that `github-oidc` has an audience, for one).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use serde::Deserialize;

/// Where `sandbox setup` puts the root-owned copy.
pub const ROOT_COPY: &str = "/etc/agentic-job/config.toml";

#[derive(Debug, Default, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub sandbox: Sandbox,
    pub egress: Egress,
    pub inference: Inference,
    pub agent: Agent,
    pub limits: Limits,
    pub setup: Setup,
    pub commit: Commit,
}

impl Config {
    pub fn parse(text: &str) -> Result<Self> {
        toml::from_str(text).context("parsing the configuration")
    }

    pub fn load(path: &Path) -> Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("in {}", path.display()))
    }
}

/// The sandbox user, what is closed to it, and what `sandbox check`
/// probes with. Step 5.
#[derive(Debug, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct Sandbox {
    pub user: String,
    /// Supplementary groups of the sandbox user.
    pub groups: Vec<String>,
    /// An image may ship the sandbox user; otherwise one that exists
    /// means an earlier job left it, and `sandbox setup` refuses.
    pub allow_existing_user: bool,
    /// Whose home is closed. `sandbox setup` takes it from `SUDO_USER`
    /// when this is not set.
    pub runner_user: Option<String>,
    /// More directories closed to every user but their owner, besides
    /// the runner user's home and [`Sandbox::ALWAYS_PRIVATE_DIRS`]. One
    /// that does not exist is skipped.
    pub private_dirs: Vec<PathBuf>,
    /// systemd units stopped by `sandbox setup`, a container daemon for
    /// one. A unit a socket starts is listed with its socket.
    pub stop_services: Vec<String>,
    /// Variables every command of the sandbox user gets, over the fixed
    /// ones (see `sandbox::enter`).
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
        for group in &self.groups {
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
            user: Self::DEFAULT_USER.to_owned(),
            groups: Self::DEFAULT_GROUPS.iter().map(|&g| g.to_owned()).collect(),
            allow_existing_user: false,
            runner_user: None,
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
    /// Unix sockets the sandbox user may connect to, besides the host's
    /// own (`sandbox::check` has that list): a path, a directory ending
    /// in `/` for everything under it, a path ending in `*` for every
    /// path that starts so, or `@name` for an abstract one.
    pub allow_sockets: Vec<String>,
    /// Local TCP ports the sandbox user may connect to.
    pub allow_tcp_ports: Vec<u16>,
}

impl SandboxCheck {
    pub const DEFAULT_CONTROL_URL: &'static str = "https://github.com/";

    pub const DEFAULT_CONTAINER_IMAGE: &'static str =
        "registry.access.redhat.com/ubi10/ubi-minimal";

    fn validate(&self) -> Result<()> {
        ensure!(
            self.control_url.starts_with("https://") || self.control_url.starts_with("http://"),
            "sandbox.check.control-url: {:?} is not an http or https URL",
            self.control_url
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
            allow_sockets: Vec::new(),
            allow_tcp_ports: Vec::new(),
        }
    }
}

/// The egress rules. Step 5 defines the fields.
#[derive(Debug, Default, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct Egress {}

/// The inference proxy and how a run announces itself to it. Step 6a.
#[derive(Debug, Default, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct Inference {
    pub url: String,
    pub register: Register,
    /// For `github-oidc`: the audience of the identity token.
    pub audience: Option<String>,
    /// For `token-file`: where the token is.
    pub token_file: Option<PathBuf>,
}

#[derive(Debug, Default, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Register {
    /// Send the run's identifier and no proof.
    #[default]
    Plain,
    /// Send the CI system's identity token.
    GithubOidc,
    /// No run API: the token is given.
    TokenFile,
}

/// Which agent, and where its configuration comes from. Step 6a.
#[derive(Debug, Default, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct Agent {
    pub name: String,
    pub model: Option<String>,
    /// A public repository holding the agent's configuration.
    pub config_repo: Option<String>,
    pub config_ref: Option<String>,
    pub config_path: Option<PathBuf>,
}

/// What stops a run. Zero means not set. `session::Limits::from_config`
/// checks it: a run needs a timeout, and one of `max-requests` and
/// `budget` unless it says `uncapped`. Step 3.
#[derive(Debug, Default, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct Limits {
    pub timeout_minutes: u64,
    /// Model requests, as the inference proxy counts them.
    pub max_requests: u64,
    /// Subagent tasks the agent may start.
    pub max_tasks: u64,
    /// In AIC, hundredths of a dollar, of the cost the agent reports.
    pub budget: u64,
    /// Asks for a run with neither `max-requests` nor `budget`.
    pub uncapped: bool,
}

/// What the task needs installed. Step 5.
#[derive(Debug, Default, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct Setup {
    /// Names for the host's package manager.
    pub packages: Vec<String>,
    /// The agent programs, each `NAME@VERSION` with an exact version,
    /// installed for every user with `npm install -g`.
    pub npm: Vec<String>,
    /// Run as the sandbox user, never as root: for toolchains.
    pub script: Option<PathBuf>,
}

impl Setup {
    /// A name is handed to a package manager run as root, so it must not
    /// read as an option; and an agent program without an exact version
    /// is whatever the registry serves that day.
    pub fn validate(&self) -> Result<()> {
        let plain = |name: &str| {
            !name.is_empty() && !name.starts_with('-') && !name.contains(char::is_whitespace)
        };
        for package in &self.packages {
            ensure!(
                plain(package),
                "setup.packages: {package:?} is not a package name"
            );
        }
        for spec in &self.npm {
            let version = spec
                .rsplit_once('@')
                .map(|(name, version)| (plain(name), version));
            ensure!(
                plain(spec)
                    && matches!(version, Some((true, v)) if v.starts_with(|c: char| c.is_ascii_digit())
                        && v.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'))),
                "setup.npm: {spec:?} is not NAME@VERSION with an exact version"
            );
        }
        Ok(())
    }
}

/// The hand-back commit. Step 6b.
#[derive(Debug, Default, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct Commit {
    pub author: Option<String>,
    pub trailers: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = r#"
        [sandbox]
        user = "agent"
        stop-services = ["docker.service"]

        [inference]
        url = "http://proxy.example:18080"
        register = "github-oidc"
        audience = "proxy"

        [agent]
        name = "claude"
        model = "opus"

        [limits]
        timeout-minutes = 75
        max-requests = 150

        [setup]
        packages = ["just"]

        [commit]
        author = "A Bot <bot@example.com>"
        trailers = ["Generated-by: AI"]
    "#;

    #[test]
    fn empty_is_the_defaults() {
        let config = Config::parse("").unwrap();
        assert_eq!(config, Config::default());
        assert_eq!(config.sandbox.user, Sandbox::DEFAULT_USER);
        assert_eq!(config.inference.register, Register::Plain);
    }

    #[test]
    fn every_table_of_the_plan_parses() {
        let config = Config::parse(FULL).unwrap();
        assert_eq!(config.sandbox.stop_services, ["docker.service"]);
        assert_eq!(config.inference.register, Register::GithubOidc);
        assert_eq!(config.inference.audience.as_deref(), Some("proxy"));
        assert_eq!(config.agent.name, "claude");
        assert_eq!(config.limits.timeout_minutes, 75);
        assert_eq!(config.setup.packages, ["just"]);
        assert_eq!(config.commit.trailers, ["Generated-by: AI"]);
    }

    #[test]
    fn sandbox_and_setup_refuse_what_the_types_allow() {
        let good = r#"
            [sandbox]
            groups = ["kvm", "render"]
            runner-user = "runner"
            private-dirs = ["/opt/hca"]
            stop-services = ["docker.socket", "docker.service"]
            env = { PATH = "/home/agent/.cargo/bin:/usr/bin" }
            [sandbox.check]
            allow-sockets = ["/run/snapd.socket", "/run/pcscd/", "@ISCSIADM_ABSTRACT_NAMESPACE"]
            allow-tcp-ports = [3128]
            [setup]
            packages = ["just", "gcc-c++"]
            npm = ["opencode-ai@1.2.3", "@agentclientprotocol/claude-agent-acp@0.4.0-beta.1"]
        "#;
        let config = Config::parse(good).unwrap();
        config.sandbox.validate().unwrap();
        config.setup.validate().unwrap();
        Config::default().sandbox.validate().unwrap();

        let bad = [
            "[sandbox]\nuser = \"root\"",
            "[sandbox]\nuser = \"a b\"",
            "[sandbox]\nuser = \"x) ALL=(ALL) NOPASSWD: ALL #\"",
            "[sandbox]\nuser = \"agent\"\nrunner-user = \"agent\"",
            "[sandbox]\ngroups = [\"docker\"]",
            "[sandbox]\ngroups = [\"wheel\"]",
            "[sandbox]\nprivate-dirs = [\"relative\"]",
            "[sandbox]\nstop-services = [\"--now\"]",
            "[sandbox]\nstop-services = [\"docker\"]",
            "[sandbox]\nenv = { ACTIONS_ID_TOKEN_REQUEST_URL = \"x\" }",
            "[sandbox]\nenv = { \"A=B\" = \"x\" }",
            "[sandbox]\nenv = { A = \"x\\ny\" }",
            "[sandbox.check]\ncontrol-url = \"ftp://example.com\"",
            "[sandbox.check]\ncontainer-image = \"--privileged\"",
            "[sandbox.check]\nallow-sockets = [\"run/x\"]",
            "[setup]\npackages = [\"--installroot=/\"]",
            "[setup]\nnpm = [\"opencode-ai\"]",
            "[setup]\nnpm = [\"opencode-ai@latest\"]",
            "[setup]\nnpm = [\"opencode-ai@^1.2.3\"]",
            "[setup]\nnpm = [\"@scope/name\"]",
        ];
        for text in bad {
            let config = Config::parse(text).unwrap_or_else(|err| panic!("{text:?}: {err:#}"));
            assert!(
                config
                    .sandbox
                    .validate()
                    .and(config.setup.validate())
                    .is_err(),
                "{text:?} was accepted"
            );
        }
    }

    /// A misspelled key must not silently leave a protection at its default.
    #[test]
    fn unknown_keys_and_values_are_refused() {
        let cases = [
            "[sandbox]\nusr = \"x\"",
            "[sandbx]\nuser = \"x\"",
            "[inference]\nregister = \"oidc\"",
            "[limits]\ntimeout = 5",
            "[limits]\nmax-requests = -1",
        ];
        for text in cases {
            assert!(Config::parse(text).is_err(), "{text:?} parsed");
        }
    }
}
