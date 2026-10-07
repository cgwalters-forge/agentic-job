//! `agentic-job check`: check handed-back outputs and the patch against
//! the policy. Step 4 of docs/plan.md.

use std::path::PathBuf;

use anyhow::Result;

use crate::exit::{Exit, NotImplemented};

#[derive(Debug, clap::Args)]
pub struct Args {
    /// The policy.json that `policy` printed for this run
    #[arg(long, value_name = "FILE")]
    pub policy: PathBuf,
    /// The directory holding the handed-back outputs
    #[arg(long, value_name = "DIR")]
    pub outputs: PathBuf,
    /// Where to write the reasons for a refusal
    #[arg(long, value_name = "FILE")]
    pub report: Option<PathBuf>,
}

pub fn run(_args: &Args) -> Result<Exit> {
    Err(NotImplemented("check").into())
}
