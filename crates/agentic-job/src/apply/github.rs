//! GitHub's client is the safe-outputs library's
//! ([`safe_outputs::apply::github`]), re-exported here; this reads its
//! token and API from the environment, names agentic-job to it
//! ([`super::PROVENANCE`]) and shows its retry warnings in the step's log.

use anyhow::{Result, ensure};
pub use safe_outputs::apply::github::*;

use super::roles::{self, Role};

/// The environment variable a workflow step gives the token in, as `gh`
/// reads it.
pub const GH_TOKEN_ENV: &str = "GH_TOKEN";

/// The API the runner names (`GITHUB_API_URL`), with the token in the
/// environment variable VAR, which must not be empty.
pub fn from_env(var: &str) -> Result<Github> {
    from_role_env(Role::Publication, var)
}

pub(super) fn from_role_env(role: Role, var: &str) -> Result<Github> {
    let bindings = roles::read(&|key| std::env::var(key).ok())?;
    let api = std::env::var(API_URL_VAR).unwrap_or_else(|_| DEFAULT_API.to_owned());
    if let Some(bindings) = bindings {
        let client = Github::new(&api, String::new(), super::PROVENANCE)?
            .warning_with(warn)
            .with_roles(bindings);
        // Apply validates the complete set before any fork or other write.
        // Status jobs deliberately receive only the status credential.
        for checked in Role::ALL {
            if role != Role::Status || checked == Role::Status {
                client.for_role(checked)?.check_identity()?;
            }
        }
        return client.for_role(role);
    }
    let token = std::env::var(var).unwrap_or_default();
    ensure!(!token.is_empty(), "{var} is not set");
    Ok(Github::new(&api, token, super::PROVENANCE)?.warning_with(warn))
}

/// LINE, which holds the forge's words, as a workflow warning.
fn warn(line: &str) {
    println!("::warning::{}", crate::run::enter::one_line(line));
}
