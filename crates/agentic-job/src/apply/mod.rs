//! `agentic-job apply`: publish what `check` accepted to a forge that is
//! not GitHub, from a job of its own that holds the write token. GitHub's
//! apply is still gh-aw's handlers in apply.yml, but for the types the
//! bounds route to [`publish`] (comments, new pull requests, labels and
//! closing);
//! this is the first slice of moving publishing behind [`Publisher`]
//! (#1346). On GitHub, [`secondary`] opens the policy's secondary pull
//! request after the handlers.
//!
//! The plan of every write, the Forgejo and GitLab publishers, their push
//! and the filtering of what they post, and GitHub's client and native
//! publishing, are the safe-outputs library's ([`safe_outputs::apply`]),
//! re-exported here at their old paths. This is its adapter: it reads the
//! policy, the check job's upload without following a link, and the token,
//! or the caller's role bindings and their tokens ([`roles::Routed`]),
//! from the environment, and gives the library agentic-job's footer,
//! commit status context and committer ([`PROVENANCE`]).

pub mod applied;
pub mod github;
pub mod issue_reference;
pub mod publish;
pub mod push;
pub mod replacement;
pub mod result;
pub mod secondary;
pub mod status;

use std::path::{Path, PathBuf};

use anyhow::Result;
pub use safe_outputs::apply::*;

use crate::exit::Exit;
use crate::files;
use crate::policy::{Load, Policy};

/// The environment variable holding the forge's token: never an
/// argument, which any user of the machine can read in `/proc`.
pub const TOKEN_ENV: &str = "FORGEJO_TOKEN";

/// GitLab's, as [`TOKEN_ENV`] is Forgejo's.
pub const GITLAB_TOKEN_ENV: &str = "GITLAB_TOKEN";

/// The context of every commit status apply writes: the caller's, never
/// a request's, so that a branch protection rule naming it can trust it.
pub const STATUS_CONTEXT: &str = "agentic-job";

/// How agentic-job names what it publishes: the footer's link text, the
/// commit status context, and the committer of a pushed commit, which
/// names the tool at an address that cannot receive mail (RFC 2606).
pub const PROVENANCE: Provenance<'static> = Provenance {
    name: "agentic-job",
    status_context: STATUS_CONTEXT,
    committer: ("agentic-job", "agentic-job@invalid"),
};

fn token_env(forge: Forge) -> &'static str {
    match forge {
        Forge::Forgejo => TOKEN_ENV,
        Forge::Gitlab => GITLAB_TOKEN_ENV,
    }
}

/// A file of the check job's upload, read without following a link.
pub(crate) fn read_checked(path: &Path, max: u64) -> Result<Vec<u8>> {
    Ok(files::read_regular(path, max)?)
}

/// The check job's upload in DIR, as the library reads it.
pub(crate) fn upload(dir: &Path) -> Upload<'_> {
    Upload {
        dir,
        read: &read_checked,
    }
}

#[derive(Debug, clap::Args)]
pub struct Args {
    /// The forge to publish to
    #[arg(long, value_enum)]
    pub forge: Forge,
    /// The forge's address, without `/api/v1`
    #[arg(long, value_name = "URL")]
    pub server: String,
    /// The repository to publish to: the policy's
    #[arg(long, value_name = "OWNER/NAME")]
    pub repo: String,
    /// The policy.json that `check` checked against
    #[arg(long, value_name = "FILE")]
    pub policy: PathBuf,
    /// The check job's upload: its report.json and the patch it accepted
    #[arg(long, value_name = "DIR")]
    pub checked: PathBuf,
    /// The issue or pull request a comment goes to, from the caller
    #[arg(long, value_name = "NUMBER", value_parser = clap::value_parser!(u64).range(1..))]
    pub comment_target: Option<u64>,
    /// What --comment-target numbers, on GitLab, where an issue and a
    /// merge request may share a number
    #[arg(long, value_enum, requires = "comment_target")]
    pub comment_target_kind: Option<Kind>,
    /// The run's page, linked from the footer of what is posted
    #[arg(long, value_name = "URL")]
    pub run_url: String,
    /// The fork a pull request's branch is pushed to: the policy's `push_repo`
    #[arg(long, value_name = "OWNER/NAME")]
    pub push_repo: Option<String>,
    /// Open the PR for this exact previously pushed commit, without pushing again
    #[arg(long, value_name = "SHA")]
    pub recover_pushed_commit: Option<String>,
    /// After every other write, set this commit status, context `agentic-job`, on --status-sha
    #[arg(long, value_enum, requires = "status_sha")]
    pub status: Option<State>,
    /// The commit the caller's --status is for
    #[arg(long, value_name = "SHA", requires = "status")]
    pub status_sha: Option<String>,
    /// GitLab status's trusted pipeline ref (never read from agent output)
    #[arg(long, value_name = "REF", requires_all = ["status", "status_pipeline_id"])]
    pub status_ref: Option<String>,
    /// GitLab status's trusted project-local pipeline ID
    #[arg(long, value_name = "ID", requires_all = ["status", "status_ref"], value_parser = clap::value_parser!(u64).range(1..))]
    pub status_pipeline_id: Option<u64>,
}

pub fn run(args: &Args) -> Result<Exit> {
    anyhow::ensure!(
        args.forge == Forge::Gitlab
            || (args.status_ref.is_none() && args.status_pipeline_id.is_none()),
        "--status-ref and --status-pipeline-id are GitLab-only"
    );
    if let Some(commit) = &args.recover_pushed_commit {
        anyhow::ensure!(
            COMMIT_RE.is_match(commit),
            "--recover-pushed-commit requires a full commit SHA"
        );
    }
    anyhow::ensure!(
        args.forge == Forge::Gitlab || args.comment_target_kind.is_none(),
        "--comment-target-kind is GitLab's: Forgejo numbers issues and pull requests together"
    );
    let policy = Policy::load(&args.policy)?;
    // Only the caller's environment binds roles: never a request or the
    // agent's output. With a mapping, the forge token is not read at all.
    let publisher: Box<dyn Publisher> = match roles::read(&|key| std::env::var(key).ok())? {
        None => {
            let token_env = token_env(args.forge);
            let token = std::env::var(token_env).unwrap_or_default();
            anyhow::ensure!(!token.is_empty(), "{token_env} is not set");
            forge_publisher(args, &policy, token, None, None)?
        }
        Some(credentials) => Box::new(roles::Routed::new(&credentials, |_, credential, push| {
            forge_publisher(
                args,
                &policy,
                credential.token.clone(),
                push.map(str::to_owned),
                Some(credential.identity.actor_id),
            )
        })?),
    };
    let target = Target {
        forge: args.forge,
        repo: &args.repo,
        push_repo: args.push_repo.as_deref(),
        comment_target: args.comment_target,
        run_url: &args.run_url,
        status: args.status.zip(args.status_sha.as_deref()),
        provenance: PROVENANCE,
    };
    let ops = match plan(&upload(&args.checked), &policy, &target) {
        Ok(ops) => ops,
        Err(errors) => {
            for error in errors {
                eprintln!("error: {}", crate::run::enter::one_line(&error));
            }
            return Ok(Exit::Failure);
        }
    };
    for url in publish(&ops, publisher.as_ref())? {
        println!("{url}");
    }
    Ok(Exit::Success)
}

/// The forge's publisher, with TOKEN for its API; PUSH, if given, for git
/// (see [`roles::Routed`]); and OWNER, the numeric actor TOKEN is bound to.
fn forge_publisher(
    args: &Args,
    policy: &Policy,
    token: String,
    push: Option<String>,
    owner: Option<u64>,
) -> Result<Box<dyn Publisher>> {
    Ok(match args.forge {
        Forge::Forgejo => Box::new(
            forgejo::Forgejo::new(&args.server, &policy.repo, token, &policy.allow_http)?
                .recovering(args.recover_pushed_commit.clone())
                .pushing_with(push),
        ),
        Forge::Gitlab => Box::new(
            gitlab::Gitlab::new(&args.server, &policy.repo, token, &policy.allow_http)?
                .recovering(args.recover_pushed_commit.clone())
                .commenting_on(args.comment_target_kind)
                .status_context(args.status_ref.clone().zip(args.status_pipeline_id))
                .pushing_with(push)
                .owned_by(owner),
        ),
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::check::BASE_COMMIT_HEADER;

    const HEAD: &str = crate::policy::tests::HEAD;
    const RUN: &str = "https://forge.example/o/r/actions/runs/7";

    /// A push run's upload: pull request 42's branch `agent-run-12` at
    /// `HEAD`, and its patch, which LINK, if set, makes a link to it.
    fn checked_push(link: bool) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let patch = format!(
            "From {HEAD} Mon Sep 17 00:00:00 2001\n\
             {BASE_COMMIT_HEADER}: {HEAD}\n\
             From: agent <agent@localhost>\n\
             Date: Tue, 6 Oct 2026 21:02:35 -0400\n\
             Subject: [PATCH] Change things\n\
             \n\
             ---\n \
             src/x | 1 +\n\
             \n\
             diff --git a/src/x b/src/x\nnew file mode 100644\nindex 0000000..587be6b\n\
             --- /dev/null\n+++ b/src/x\n@@ -0,0 +1 @@\n+x\n"
        );
        let name = "aw-agent-run-12.patch";
        let path = dir.path().join(name);
        if link {
            std::fs::write(dir.path().join("elsewhere"), &patch).unwrap();
            std::os::unix::fs::symlink(dir.path().join("elsewhere"), &path).unwrap();
        } else {
            std::fs::write(&path, &patch).unwrap();
        }
        let report = json!({
            "ok": true, "errors": [],
            "items": [
                {"type": "push_to_pull_request_branch", "message": "m", "branch": "agent-run-12"},
                {"type": "add_comment", "body": "Done."},
            ],
            "patch": {"file": name, "bytes": patch.len(), "base_commit": HEAD, "files": ["src/x"]},
        });
        std::fs::write(dir.path().join(REPORT_FILE), report.to_string()).unwrap();
        dir
    }

    fn policy() -> Policy {
        let mut policy = crate::policy::tests::push_policy_to("bot/bootc");
        policy.safe_outputs.others.insert(
            "add_comment".to_owned(),
            crate::policy::OutputLimit {
                max: 1,
                ..Default::default()
            },
        );
        policy
    }

    fn target() -> Target<'static> {
        Target {
            forge: Forge::Forgejo,
            repo: "bootc-dev/bootc",
            push_repo: Some("bot/bootc"),
            comment_target: Some(64),
            run_url: RUN,
            status: Some((State::Success, HEAD)),
            provenance: PROVENANCE,
        }
    }

    /// What apply has always written as agentic-job, byte for byte: its
    /// footer, its commit status context and its committer.
    #[test]
    fn publishes_as_agentic_job() {
        let dir = checked_push(false);
        let ops = plan(&upload(dir.path()), &policy(), &target()).unwrap();
        let committer = Committer {
            name: "agentic-job".to_owned(),
            email: "agentic-job@invalid".to_owned(),
        };
        let Op::Push(push) = &ops[0] else {
            panic!("{ops:?}")
        };
        assert_eq!(push.committer, committer);
        assert_eq!(
            ops[1],
            Op::Comment {
                issue: 64,
                body: format!("Done.\n\n> Generated by [agentic-job]({RUN})"),
            }
        );
        assert_eq!(
            ops[2],
            Op::Status(CommitStatus {
                sha: HEAD.to_owned(),
                state: State::Success,
                target_url: RUN.to_owned(),
                context: "agentic-job".to_owned(),
            })
        );
    }

    /// The upload is read without following a link: a patch that is one
    /// is refused before any write.
    #[test]
    fn refuses_a_linked_patch() {
        let dir = checked_push(true);
        let errors = plan(&upload(dir.path()), &policy(), &target()).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|error| error.contains("the patch aw-agent-run-12.patch")
                    && error.contains("is not a regular file")),
            "{errors:?}"
        );
    }
}
