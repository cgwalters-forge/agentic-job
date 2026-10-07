//! `agentic-job sandbox`: make the host safe for the agent, and prove it.
//! Step 5 of docs/plan.md. `setup` runs as root; `check` runs as the
//! runner's user and enters the sandbox ([`enter`]) for each probe.

use anyhow::Result;
use clap::Subcommand;

use crate::exit::Exit;

pub mod check;
pub mod enter;
pub mod host;
pub mod local;
pub mod setup;

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Create the sandbox user, close its routes to credentials and root
    Setup(setup::Args),
    /// Probe the sandbox as the sandbox user, each probe with a positive control
    Check(check::Args),
    /// Print the local sockets and ports this user connects to; `check` runs it as the sandbox user
    #[command(hide = true)]
    ProbeLocal,
}

impl Command {
    pub fn run(&self) -> Result<Exit> {
        match self {
            Self::Setup(args) => setup::run(args),
            Self::Check(args) => check::run(args),
            Self::ProbeLocal => local::run(),
        }
    }
}
