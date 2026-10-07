//! `agentic-job sandbox check`: the probes that need no run token. `run`
//! repeats them with the token in place, so they are a library function
//! first and a command second.

use anyhow::Result;

use crate::exit::{Exit, NotImplemented};

#[derive(Debug, clap::Args)]
pub struct Args {}

pub fn run(_args: &Args) -> Result<Exit> {
    Err(NotImplemented("sandbox check").into())
}
