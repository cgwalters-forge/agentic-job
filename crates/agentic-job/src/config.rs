//! The `--config` file: one TOML file for a run, with a table per
//! concern. `sandbox setup` reads the caller's file and copies it to
//! [`ROOT_COPY`]; `run` reads only that copy, which the sandbox user
//! cannot change.
//!
//! Each table's struct belongs to the step of docs/plan.md that uses it
//! (see docs/layout.md). The fields here are the ones the plan names;
//! their step may rename or add to them, and checks what the types
//! cannot say (that `github-oidc` has an audience, for one).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
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

/// The sandbox user and what is stopped before it exists. Step 5.
#[derive(Debug, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct Sandbox {
    pub user: String,
    /// systemd units stopped by `sandbox setup`, a container daemon for one.
    pub stop_services: Vec<String>,
}

impl Sandbox {
    /// The old tree's name, which its readers and docs know.
    pub const DEFAULT_USER: &'static str = "runner-sandbox";
}

impl Default for Sandbox {
    fn default() -> Self {
        Self {
            user: Self::DEFAULT_USER.to_owned(),
            stop_services: Vec::new(),
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

/// What stops a run. Zero means no cap, as in the old tree. Step 3.
#[derive(Debug, Default, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct Limits {
    pub timeout_minutes: u64,
    pub max_requests: u64,
    pub max_tasks: u64,
    /// In AIC, hundredths of a dollar.
    pub budget: u64,
}

/// What the task needs installed. Step 5.
#[derive(Debug, Default, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct Setup {
    pub packages: Vec<String>,
    /// Run as the sandbox user, never as root: for toolchains.
    pub script: Option<PathBuf>,
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
