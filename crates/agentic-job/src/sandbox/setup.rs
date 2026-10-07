//! `agentic-job sandbox setup`: create the sandbox user, close its routes
//! to credentials and root, install the network rules and the egress
//! proxy, and last take the calling user's root away.

use std::path::PathBuf;

use anyhow::Result;

use crate::exit::{Exit, NotImplemented};

#[derive(Debug, clap::Args)]
pub struct Args {
    /// The run's configuration (TOML); copied to /etc/agentic-job/
    #[arg(long, value_name = "FILE")]
    pub config: PathBuf,
}

pub fn run(_args: &Args) -> Result<Exit> {
    Err(NotImplemented("sandbox setup").into())
}
