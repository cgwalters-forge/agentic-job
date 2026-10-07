//! `agentic-job run`: get the run token, configure the agent, start it in
//! the sandbox, drive it within the limits, and take the hand-back.
//! Steps 6a and 6b of docs/plan.md, on top of [`crate::session`].

pub mod agent;
pub mod clone;
pub mod enter;
pub mod inference;
pub mod launch;

use std::path::PathBuf;

use anyhow::Result;

use crate::exit::{Exit, NotImplemented};

#[derive(Debug, clap::Args)]
pub struct Args {
    /// The policy.json that `policy` printed for this run
    #[arg(long, value_name = "FILE")]
    pub policy: PathBuf,
    /// The task given to the agent
    #[arg(long, value_name = "FILE")]
    pub task: PathBuf,
    /// JSON about the run from the CI system, copied into summary.json
    #[arg(long, value_name = "FILE")]
    pub meta: PathBuf,
    /// The directory the run's results are written to
    #[arg(long, value_name = "DIR")]
    pub out: PathBuf,
}

pub fn run(_args: &Args) -> Result<Exit> {
    Err(NotImplemented("run").into())
}
