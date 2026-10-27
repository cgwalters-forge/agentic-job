//! `agentic-job sandbox`: make the host safe for the agent, and prove it.
//! Step 5 of docs/plan.md. `setup` runs as root, and ends by taking root
//! away from the runner's user; `check` runs as the runner's user and
//! enters the sandbox ([`enter`]) for each probe, through the one
//! privileged command that user keeps ([`helper`], via [`root`]).

use std::path::Path;

use anyhow::{Context, Result};
use clap::Subcommand;

use crate::config::{Config, Egress, HostFile, Sandbox};
use crate::exit::Exit;

pub mod check;
pub(crate) mod session;
pub mod setup;
pub mod timed;

// What secure-host holds already, at the paths callers know.
pub use ::secure_host::{
    egress, enter, host, local, network, proxy, root, view, worker_diagnostics,
};

/// secure-host's helper, with `run` reading root's copy of the
/// configuration as agentic-job's.
pub mod helper {
    pub use ::secure_host::helper::*;

    use anyhow::Result;

    use crate::config::Config;
    use crate::exit::Exit;

    pub fn run(op: &Op) -> Result<Exit> {
        Ok(if ::secure_host::helper::run::<Config>(op)? {
            Exit::Success
        } else {
            Exit::Failure
        })
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::config::HostFile;

        /// `run`'s alone, as the identity token's request is: never the
        /// sandbox user's.
        #[test]
        fn the_runs_credentials_are_unset_in_the_sandbox() {
            assert_eq!(
                Config::UNSET,
                ["AGENTIC_JOB_INFERENCE_KEY", "AGENTIC_JOB_MCP_TOKEN"]
            );
        }
    }
}

/// What agentic-job's file adds for the helper and the way in: its run
/// tables, checked with the rest; the run's credentials, unset in the
/// sandbox; and the session bridge's `session-serve`.
impl HostFile for Config {
    const UNSET: &'static [&'static str] =
        &[crate::run::api_key::API_KEY_VAR, crate::run::mcp::TOKEN_VAR];

    fn load(path: &Path) -> Result<Self> {
        Config::load(path)
    }

    fn sandbox(&self) -> &Sandbox {
        &self.sandbox
    }

    fn egress(&self) -> &Egress {
        &self.egress
    }

    fn tools(&self) -> bool {
        self.setup.tools.is_some()
    }

    fn direct(&self) -> Result<Vec<network::Direct>> {
        Ok(self.check_host()?.direct)
    }

    /// Checked as `run` checks it.
    fn session_serve(&self) -> Result<()> {
        let listen = crate::session::bridge::Listen::from_config(&self.session)?
            .context("session serve needs session.listen")?;
        session::serve(listen.capability.as_deref())
    }
}

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
