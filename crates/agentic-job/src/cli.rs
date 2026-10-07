//! The command line: the table in docs/plan.md, and nothing else.
//!
//! Each command's arguments live in its own module; this file only names
//! the commands and dispatches to them.

use anyhow::Result;
use clap::{Parser, Subcommand};

use crate::exit::Exit;
use crate::{check, policy, run, sandbox};

#[derive(Debug, Parser)]
#[command(name = "agentic-job", version, about, propagate_version = true)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Check one run's request against the caller's bounds and print policy.json
    Policy(policy::Args),
    /// Prepare and probe the host the agent runs on
    #[command(subcommand)]
    Sandbox(sandbox::Command),
    /// Run the agent in the sandbox and take what it hands back
    Run(run::Args),
    /// Check handed-back outputs and patch against the policy
    Check(check::Args),
}

impl Command {
    pub fn run(&self) -> Result<Exit> {
        match self {
            Self::Policy(args) => policy::run(args),
            Self::Sandbox(command) => command.run(),
            Self::Run(args) => run::run(args),
            Self::Check(args) => check::run(args),
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;
    use crate::exit::NotImplemented;

    #[test]
    fn definition_is_consistent() {
        Cli::command().debug_assert();
    }

    /// The command lines of the plan's table, and the name each stub
    /// reports; none for a command that is implemented.
    const PLAN: &[(&[&str], Option<&str>)] = &[
        (
            &[
                "policy",
                "--allow",
                "allow.toml",
                "--repo",
                "owner/name",
                "--clone-url",
                "https://example.org/owner/name",
                "--base",
                "main",
                "--kind",
                "branch",
                "--outputs",
                "create_pull_request,noop",
                "--max-outputs",
                "2",
            ],
            None,
        ),
        (
            &["sandbox", "setup", "--config", "c.toml"],
            Some("sandbox setup"),
        ),
        (&["sandbox", "check"], Some("sandbox check")),
        (
            &[
                "run", "--policy", "p.json", "--task", "task.md", "--meta", "m.json", "--out",
                "out",
            ],
            Some("run"),
        ),
        (
            &[
                "check",
                "--policy",
                "p.json",
                "--outputs",
                "dir",
                "--collected",
                "agent_output.json",
            ],
            None,
        ),
        (
            &[
                "check",
                "--policy",
                "p.json",
                "--outputs",
                "dir",
                "--collected",
                "agent_output.json",
                "--report",
                "r.json",
            ],
            None,
        ),
    ];

    #[test]
    fn plan_commands_parse_and_are_stubs() {
        for (args, name) in PLAN {
            let cli = Cli::try_parse_from(std::iter::once(&"agentic-job").chain(*args))
                .unwrap_or_else(|err| panic!("{args:?}: {err}"));
            let Some(name) = name else { continue };
            let err = cli.command.run().expect_err("stub succeeded");
            assert_eq!(
                err.downcast_ref::<NotImplemented>(),
                Some(&NotImplemented(name)),
                "{args:?}"
            );
        }
    }

    #[test]
    fn bad_command_lines_are_refused() {
        let cases: &[&[&str]] = &[
            &[],
            &["egress", "serve"],
            &["policy"],
            &[
                "policy", "--allow", "a", "--repo", "r", "--base", "b", "--kind", "other",
            ],
            &["sandbox"],
            &["sandbox", "setup"],
            &["run", "--policy", "p.json"],
            &["check", "--outputs", "dir"],
            &["check", "--policy", "p.json", "--outputs", "dir"],
        ];
        for args in cases {
            let parsed = Cli::try_parse_from(std::iter::once(&"agentic-job").chain(*args));
            assert!(parsed.is_err(), "{args:?} parsed");
        }
    }
}
