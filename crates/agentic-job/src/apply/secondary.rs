//! `agentic-job secondary-pull-request`: opens the policy's secondary pull
//! request on GitHub, as apply's last step, which is the safe-outputs
//! library's ([`safe_outputs::apply::secondary`]), re-exported here. This
//! reads the policy, the check job's upload without following a link and
//! apply's token from the environment, and names agentic-job in the pull
//! request's footer ([`super::PROVENANCE`]).

use std::path::PathBuf;

use anyhow::{Context, Result};
pub use safe_outputs::apply::secondary::*;

use crate::exit::Exit;
use crate::policy::{Load, Policy};

/// The environment variable holding apply's token, as apply.yml names it:
/// a user's, which can fork.
pub const TOKEN_ENV: &str = "SAFE_OUTPUTS_PAT";

#[derive(Debug, clap::Args)]
pub struct Args {
    /// The policy.json that `check` checked against
    #[arg(long, value_name = "FILE")]
    pub policy: PathBuf,
    /// The check job's upload: its report.json, and the patch in `secondary/`
    #[arg(long, value_name = "DIR")]
    pub checked: PathBuf,
    /// The identifier `check --run-id` was given: the only run whose branch the pull request may be on
    #[arg(long, value_name = "ID")]
    pub run_id: String,
    /// The run's page, linked from the pull request's body
    #[arg(long, value_name = "URL")]
    pub run_url: String,
}

pub fn run(args: &Args) -> Result<Exit> {
    let policy = Policy::load(&args.policy)?;
    let checked = super::upload(&args.checked);
    let planned = plan(
        &checked,
        &policy,
        &args.run_id,
        &args.run_url,
        &super::PROVENANCE,
    );
    let pr = match planned {
        Ok(Some(pr)) => pr,
        Ok(None) => return Ok(Exit::Success),
        Err(errors) => {
            for error in errors {
                eprintln!(
                    "error: the secondary pull request: {}",
                    crate::run::enter::one_line(&error)
                );
            }
            return Ok(Exit::Failure);
        }
    };
    let url = open(&super::github::from_env(TOKEN_ENV)?, &pr)
        .with_context(|| format!("the secondary pull request in {}", pr.repo))?;
    println!("{url}");
    Ok(Exit::Success)
}
