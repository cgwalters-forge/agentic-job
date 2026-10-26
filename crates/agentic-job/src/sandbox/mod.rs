//! `agentic-job sandbox`: make the host safe for the agent, and prove it.
//! Step 5 of docs/plan.md. `setup` runs as root, and ends by taking root
//! away from the runner's user; `check` runs as the runner's user and
//! enters the sandbox ([`enter`]) for each probe, through the one
//! privileged command that user keeps ([`helper`], via [`root`]).

use anyhow::Result;
use clap::Subcommand;

use crate::exit::Exit;

pub mod check;
pub mod enter;
pub mod helper;
pub mod root;
pub(crate) mod session;
pub mod setup;
pub mod timed;

// What secure-host holds already, at the paths callers know.
pub use ::secure_host::{egress, host, local, network, proxy, view, worker_diagnostics};

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Create the sandbox user, close its routes to credentials and root
    Setup(setup::Args),
    /// Probe the sandbox as the sandbox user, each probe with a positive control
    Check(check::Args),
    /// Run secure-host's setup or probes, and add their time to the job summary
    #[command(hide = true)]
    Timed(timed::Args),
    /// Print the local sockets and ports this user connects to; `check` runs it as the sandbox user
    #[command(hide = true)]
    ProbeLocal,
    /// Run the egress proxy; `setup` starts it as its own user unless `egress.rust-proxy = false`
    #[command(hide = true)]
    EgressProxy(proxy::Args),
}

impl Command {
    pub fn run(&self) -> Result<Exit> {
        match self {
            Self::Setup(args) => setup::run(args),
            Self::Check(args) => check::run(args),
            Self::Timed(args) => timed::run(args),
            Self::ProbeLocal => local::run().map(|()| Exit::Success),
            Self::EgressProxy(args) => proxy::run(args).map(|()| Exit::Success),
        }
    }
}
