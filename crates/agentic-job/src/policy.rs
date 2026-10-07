//! `agentic-job policy`: check one run's request against the caller's
//! bounds and print `policy.json`. Step 4 of docs/plan.md.

use std::path::PathBuf;

use anyhow::Result;
use clap::ValueEnum;

use crate::exit::{Exit, NotImplemented};

#[derive(Debug, clap::Args)]
pub struct Args {
    /// The caller's bounds file
    #[arg(long, value_name = "FILE")]
    pub allow: PathBuf,
    /// The repository the agent works on, as OWNER/NAME
    #[arg(long, value_name = "REPO")]
    pub repo: String,
    /// The branch the agent starts from
    #[arg(long, value_name = "BRANCH")]
    pub base: String,
    /// What the run may hand back: a branch, or an analysis only
    #[arg(long, value_enum)]
    pub kind: Kind,
    /// The output types requested, comma-separated
    #[arg(long, value_name = "LIST", value_delimiter = ',', required = true)]
    pub outputs: Vec<String>,
    /// The most outputs of any one type
    #[arg(long, value_name = "N")]
    pub max_outputs: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Kind {
    Branch,
    Analysis,
}

pub fn run(_args: &Args) -> Result<Exit> {
    Err(NotImplemented("policy").into())
}
