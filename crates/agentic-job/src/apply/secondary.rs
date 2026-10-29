//! `agentic-job secondary-pull-request`: opens the policy's secondary pull
//! request ([`Secondary`], docs/safe-outputs.md) on GitHub, as apply's
//! last step, after its handlers published the run's own outputs: one
//! draft, in the repository the policy names, on its base, from this
//! run's branch under its prefix, pushed to the token's own fork with
//! Actions off, as apply.yml's fork step does for the run's own.
//!
//! The repository, base, branch prefix and patch come from the policy and
//! check's verdict, read again as [`super::plan`] reads them; the request
//! gives the title and body, and a branch that must be the one the policy
//! and the run's identifier make, never an earlier run's, whose open pull
//! request a push would update. Everything is decided before the first
//! request to the forge, so a refusal leaves it as it was.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use serde_json::{Map, Value, json};

use super::git::Push;
pub use super::github::Github;
use super::{COMMIT_RE, Report, footer, mentions, owner, read_report, with_footer};
use crate::check::{self, PatchReading, handler_limits};
use crate::exit::Exit;
use crate::files;
use crate::policy::{CREATE_PULL_REQUEST, Forge, PatchRules, Policy, Secondary};

/// The environment variable holding apply's token, as apply.yml names it:
/// a user's, which can fork.
pub const TOKEN_ENV: &str = "SAFE_OUTPUTS_PAT";

/// Where check.yml puts the secondary's patch in its upload: apart from
/// the run's own, which apply's handlers and trial take.
pub const PATCH_DIR: &str = "secondary";

#[derive(Debug, clap::Args)]
pub struct Args {
    /// The policy.json that `check` checked against
    #[arg(long, value_name = "FILE")]
    pub policy: PathBuf,
    /// The check job's upload: its report.json, and the patch in `secondary/`
    #[arg(long, value_name = "DIR")]
    pub checked: PathBuf,
    /// The identifier `check --run-id` was given: the only run whose branch the pull request may be on
    #[arg(long, value_name = "ID")]
    pub run_id: String,
    /// The run's page, linked from the pull request's body
    #[arg(long, value_name = "URL")]
    pub run_url: String,
}

/// The secondary pull request, everything about it decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequest {
    /// The policy's secondary repository, and where git finds it.
    pub repo: String,
    /// The run's own repository, which may itself be the token's fork of
    /// `repo`: never the fork pushed to, nor where Actions are turned off.
    pub own_repo: String,
    pub clone_url: String,
    pub base: String,
    /// The commit check read the patch against, on `base`.
    pub base_commit: String,
    pub branch: String,
    pub title: String,
    pub body: String,
    pub patch: Vec<u8>,
    /// The paths check read in the patch: the only ones the pushed commit
    /// may change.
    pub files: Vec<String>,
}

/// Decides the secondary pull request of the check job's upload in
/// CHECKED for the run RUN_ID, if it has one, or refuses with every
/// reason found.
pub fn plan(
    checked: &Path,
    policy: &Policy,
    run_id: &str,
    run_url: &str,
) -> Result<Option<PullRequest>, Vec<String>> {
    let refuse = |error: String| Err(vec![error]);
    let report = match read_report(checked) {
        Ok(report) => report,
        Err(err) => return refuse(format!("{err:#}")),
    };
    if !report.ok || !report.errors.is_empty() {
        return refuse("check refused this hand-back".to_owned());
    }
    let requests: Vec<_> = report
        .items
        .iter()
        .filter(|item| check::secondary_target(item, policy).is_some())
        .collect();
    let target = policy.secondary_pull_request.as_ref();
    match (target, &requests[..], &report.secondary_patch) {
        (_, [], None) => Ok(None),
        (_, [], Some(_)) => refuse(format!(
            "a secondary patch without a {CREATE_PULL_REQUEST} for it"
        )),
        (Some(target), [item], Some(_)) => {
            let pr = pull_request(checked, policy, target, &report, item, run_id, run_url)?;
            Ok(Some(pr))
        }
        (Some(target), [_], None) => refuse(format!(
            "a {CREATE_PULL_REQUEST} for {}, and check accepted no patch for it",
            target.repo
        )),
        (Some(target), many, _) => refuse(format!(
            "{} outputs of type {CREATE_PULL_REQUEST:?} for {}, over {}",
            many.len(),
            target.repo,
            target.max
        )),
        // `secondary_target` finds none without a secondary in the policy.
        (None, _, _) => refuse("the policy has no secondary pull request".to_owned()),
    }
}

/// The pull request of ITEM, with its patch read again and held to the
/// secondary's rules, as check held it.
fn pull_request(
    checked: &Path,
    policy: &Policy,
    target: &Secondary,
    report: &Report,
    item: &Map<String, Value>,
    run_id: &str,
    run_url: &str,
) -> Result<PullRequest, Vec<String>> {
    let mut errors = Vec::new();
    // The policy refuses it elsewhere; this forks on GitHub alone.
    if policy.forge != Forge::Github {
        errors.push("a secondary pull request off GitHub".to_owned());
    }
    if let Err(err) = footer(run_url) {
        errors.push(format!("{err:#}"));
    }
    // The base and the draft flag, which only the policy decides.
    errors.extend(check::redirections_to(item, policy, None));
    errors.extend(handler_limits::refusals(item));
    if check::map_holds_marker(item) {
        errors.push(format!(
            "a {CREATE_PULL_REQUEST} holding the marker apply writes"
        ));
    }
    let run_branch = format!("{}{run_id}", check::RUN_BRANCH_PREFIX);
    let branch = target.branch(&run_branch);
    if !check::is_run_branch(&run_branch) {
        errors.push(format!("--run-id {run_id:?} is not a run's identifier"));
    }
    let text = |key: &str| item.get(key).and_then(Value::as_str);
    if text("branch") != Some(branch.as_str()) {
        errors.push(format!(
            "a {CREATE_PULL_REQUEST} for {} on branch {:?}, which is not this run's, {branch:?}",
            target.repo,
            text("branch").unwrap_or_default()
        ));
    }
    let (title, body) = (text("title"), text("body"));
    for (key, value) in [("title", title), ("body", body)] {
        if value.is_none() {
            errors.push(format!("a {CREATE_PULL_REQUEST} without a {key}"));
        }
    }
    let read = report
        .secondary_patch
        .as_ref()
        .context("check accepted no secondary patch")
        .and_then(|patch| {
            let name = check::patch_file_name_in(&target.repo, &branch);
            ensure!(
                patch.file == name,
                "check accepted the secondary patch {:?}, and the pull request's is {name}",
                patch.file
            );
            ensure!(
                patch.commit_files.is_none(),
                "a replacement series as a secondary patch"
            );
            ensure!(
                COMMIT_RE.is_match(&patch.base_commit),
                "check's base commit {:?} is not a commit id",
                patch.base_commit
            );
            // `name` is one plain file name: `patch_file_name_in` takes out `/`.
            let path = checked.join(PATCH_DIR).join(&name);
            let bytes = files::read_regular(&path, target.max_patch_bytes)
                .with_context(|| format!("the patch {PATCH_DIR}/{name}"))?;
            ensure!(
                bytes.len() == patch.bytes,
                "the patch {name} is {} bytes, and check read {}",
                bytes.len(),
                patch.bytes
            );
            let rules = PatchRules {
                commit_author: policy.commit_author.as_deref(),
                ..target.rules()
            };
            let PatchReading { problems, files } =
                check::read_patch(&String::from_utf8_lossy(&bytes), &rules, &patch.base_commit);
            ensure!(
                problems.is_empty(),
                "the patch {name}: {}",
                problems.join("; ")
            );
            ensure!(
                files == patch.files,
                "the patch {name} names other paths than check reported"
            );
            Ok((patch.base_commit.clone(), bytes, files))
        });
    match (read, title, body) {
        (Ok((base_commit, patch, files)), Some(title), Some(body)) if errors.is_empty() => {
            Ok(PullRequest {
                repo: target.repo.clone(),
                own_repo: policy.repo.clone(),
                clone_url: target.clone_url.clone(),
                base: target.base.clone(),
                base_commit,
                branch,
                title: title.to_owned(),
                body: with_footer(&mentions::filter(body, "github"), run_url),
                patch,
                files,
            })
        }
        (read, _, _) => {
            errors.extend(read.err().map(|err| format!("{err:#}")));
            Err(errors)
        }
    }
}

impl Github {
    /// Forks, pushes the branch to the token's own fork (see
    /// [`Github::fork`]; the run's own repository is never the one, as
    /// the secondary makes it no more writable) and opens the draft pull
    /// request from it. Returns its URL.
    pub fn open(&self, pr: &PullRequest) -> Result<String> {
        let server = pr
            .clone_url
            .strip_suffix(&format!("/{}", pr.repo))
            .with_context(|| format!("{} is not where git finds {}", pr.clone_url, pr.repo))?;
        let me = self.login()?;
        let fork = self.fork(&me, &pr.repo, &pr.own_repo, server)?;
        let pushing = || format!("pushing {} to {fork}", pr.branch);
        let commit = self
            .git()
            .push(
                &Push {
                    from: &format!("{server}/{}.git", pr.repo),
                    base: &pr.base,
                    base_commit: &pr.base_commit,
                    to: &format!("{server}/{fork}.git"),
                    branch: &pr.branch,
                    patch: &pr.patch,
                    files: &pr.files,
                    head: None,
                },
                None,
            )
            .with_context(pushing)?;
        let head = format!("{}:{}", owner(&fork), pr.branch);
        let path = format!("/repos/{}/pulls", pr.repo);
        let made = self
            .send(
                "POST",
                &path,
                Some(&json!({"title": pr.title, "body": pr.body, "head": head,
                    "base": pr.base, "draft": true})),
            )
            .with_context(|| {
                format!(
                    "published {commit} to {fork} branch {}; opening its pull request failed",
                    pr.branch
                )
            })?;
        made["html_url"]
            .as_str()
            .map(str::to_owned)
            .with_context(|| format!("POST {path}: the response names no html_url"))
    }
}

pub fn run(args: &Args) -> Result<Exit> {
    let policy = Policy::load(&args.policy)?;
    let pr = match plan(&args.checked, &policy, &args.run_id, &args.run_url) {
        Ok(Some(pr)) => pr,
        Ok(None) => return Ok(Exit::Success),
        Err(errors) => {
            for error in errors {
                eprintln!(
                    "error: the secondary pull request: {}",
                    crate::run::enter::one_line(&error)
                );
            }
            return Ok(Exit::Failure);
        }
    };
    let url = Github::from_env(TOKEN_ENV)?
        .open(&pr)
        .with_context(|| format!("the secondary pull request in {}", pr.repo))?;
    println!("{url}");
    Ok(Exit::Success)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::super::forgejo::tests::{mock, repository, sh};
    use super::super::github::FORK_TRIES;
    use super::*;
    use crate::check::BASE_COMMIT_HEADER;

    const BASE: &str = "1111111111111111111111111111111111111111";
    const RUN: &str = "https://github.com/o/r/actions/runs/7";
    const RUN_ID: &str = "7";
    const BRANCH: &str = "agentic-job/agent-run-7";
    const PATCH_NAME: &str = "aw-o-tool-agentic-job-agent-run-7.patch";
    const TOKEN: &str = "test-token-not-secret";

    fn policy() -> Policy {
        let mut policy = super::super::tests::policy();
        policy.secondary_pull_request = Some(Secondary {
            repo: "o/tool".into(),
            clone_url: "https://github.com/o/tool".into(),
            base: "main".into(),
            branch_prefix: "agentic-job/".into(),
            max: 1,
            max_patch_bytes: 4096,
            max_patch_files: 2,
            protected_files: Vec::new(),
            protect_top_level_dot_folders: true,
            unprotected_dot_folders: Vec::new(),
            protected_paths: Vec::new(),
        });
        policy
    }

    fn patch() -> String {
        format!(
            "From 2222222222222222222222222222222222222222 Mon Sep 17 00:00:00 2001\n\
             {BASE_COMMIT_HEADER}: {BASE}\n\
             From: agent <agent@localhost>\n\
             Date: Tue, 6 Oct 2026 21:02:35 -0400\n\
             Subject: [PATCH] Fix the tool\n\
             \n\
             ---\n \
             tool.rs | 1 +\n\
             \n\
             diff --git a/tool.rs b/tool.rs\nnew file mode 100644\nindex 0000000..587be6b\n\
             --- /dev/null\n+++ b/tool.rs\n@@ -0,0 +1 @@\n+x\n"
        )
    }

    fn request() -> Value {
        json!({"type": "create_pull_request", "repo": "o/tool", "title": "Fix the tool",
            "body": "Why. @user", "branch": BRANCH, "draft": true})
    }

    /// The run's own pull request's upload, as apply's test fakes make
    /// it, with the secondary request and its patch beside.
    fn checked(items: &[Value]) -> tempfile::TempDir {
        let dir = super::super::tests::checked(items);
        let patch = patch();
        std::fs::create_dir(dir.path().join(PATCH_DIR)).unwrap();
        std::fs::write(dir.path().join(PATCH_DIR).join(PATCH_NAME), &patch).unwrap();
        edit_report(dir.path(), |report| {
            report["secondary_patch"] = json!({"file": PATCH_NAME, "bytes": patch.len(),
                "base_commit": BASE, "files": ["tool.rs"]});
        });
        dir
    }

    fn edit_report(dir: &Path, edit: impl FnOnce(&mut Value)) {
        let path = dir.join(super::super::REPORT_FILE);
        let mut report: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        edit(&mut report);
        std::fs::write(&path, report.to_string()).unwrap();
    }

    #[test]
    fn planned() {
        let own = json!({"type": "create_pull_request", "title": "Fix it", "body": "Why.",
            "branch": "agent-run-7"});
        let dir = checked(&[own, request()]);
        let pr = plan(dir.path(), &policy(), RUN_ID, RUN).unwrap().unwrap();
        assert_eq!(
            pr,
            PullRequest {
                repo: "o/tool".into(),
                own_repo: "bootc-dev/bootc".into(),
                clone_url: "https://github.com/o/tool".into(),
                base: "main".into(),
                base_commit: BASE.into(),
                branch: BRANCH.into(),
                title: "Fix the tool".into(),
                body: format!("Why. @\u{200b}user\n\n> Generated by [agentic-job]({RUN})"),
                patch: patch().into_bytes(),
                files: vec!["tool.rs".into()],
            }
        );
        // Nothing to open without a secondary request, a policy that
        // names one or not.
        let dir = super::super::tests::checked(&[json!({"type": "add_comment", "body": "x"})]);
        assert_eq!(plan(dir.path(), &policy(), RUN_ID, RUN), Ok(None));
        let mut without = policy();
        without.secondary_pull_request = None;
        assert_eq!(plan(dir.path(), &without, RUN_ID, RUN), Ok(None));
    }

    /// Every refusal is decided before the forge is asked anything, and
    /// names its reason.
    #[test]
    fn refusals() {
        type Change = fn(&Path, &mut Policy, &mut Value);
        let none: Change = |_, _, _| {};
        let with = |key: &str, value: Value| {
            let mut item = request();
            item[key] = value;
            item
        };
        let cases: &[(Value, Change, &str)] = &[
            // Another run's branch, whose open pull request a push would
            // update, or one outside the prefix.
            (
                with("branch", json!("agentic-job/agent-run-8")),
                none,
                "which is not this run's, \"agentic-job/agent-run-7\"",
            ),
            (
                with("branch", json!("agent-run-7")),
                none,
                "which is not this run's",
            ),
            (
                request(),
                |_, _, run_id| *run_id = json!("7/../8"),
                "--run-id \"7/../8\" is not a run's identifier",
            ),
            (
                with("base", json!("release")),
                none,
                "with base \"release\", which is not the policy's",
            ),
            (with("draft", json!(false)), none, "that is not a draft"),
            (
                request(),
                |_, policy, _| policy.forge = Forge::Forgejo,
                "a secondary pull request off GitHub",
            ),
            (
                with("body", Value::Null),
                none,
                "a create_pull_request without a body",
            ),
            (
                with("body", json!("<!-- agentic-job-applied: 1/0/0 -->")),
                none,
                "holding the marker",
            ),
            (
                request(),
                |dir, _, _| {
                    let report = json!({"ok": false, "errors": ["no"], "items": []});
                    std::fs::write(dir.join(super::super::REPORT_FILE), report.to_string())
                        .unwrap();
                },
                "check refused",
            ),
            (
                request(),
                |dir, _, _| edit_report(dir, |report| report["secondary_patch"] = Value::Null),
                "a create_pull_request for o/tool, and check accepted no patch for it",
            ),
            (
                json!({"type": "add_comment", "body": "x"}),
                none,
                "a secondary patch without a create_pull_request for it",
            ),
            (
                request(),
                |dir, _, _| {
                    edit_report(dir, |report| {
                        let item = report["items"][0].clone();
                        report["items"].as_array_mut().unwrap().push(item);
                    });
                },
                "2 outputs of type \"create_pull_request\" for o/tool, over 1",
            ),
            (
                request(),
                |dir, _, _| {
                    edit_report(dir, |report| {
                        report["secondary_patch"]["file"] = json!("aw-agent-run-7.patch");
                    });
                },
                "check accepted the secondary patch \"aw-agent-run-7.patch\"",
            ),
            // The patch is taken from `secondary/` alone, not beside the
            // run's own, where apply's handlers look.
            (
                request(),
                |dir, _, _| {
                    std::fs::rename(dir.join(PATCH_DIR).join(PATCH_NAME), dir.join(PATCH_NAME))
                        .unwrap();
                },
                "the patch secondary/aw-o-tool-agentic-job-agent-run-7.patch",
            ),
            (
                request(),
                |dir, _, _| {
                    let path = dir.join(PATCH_DIR).join(PATCH_NAME);
                    std::fs::write(&path, patch() + "+z\n").unwrap();
                },
                "bytes, and check read",
            ),
            (
                request(),
                |dir, _, _| {
                    let path = dir.join(PATCH_DIR).join(PATCH_NAME);
                    std::fs::rename(&path, dir.join("elsewhere")).unwrap();
                    std::os::unix::fs::symlink(dir.join("elsewhere"), &path).unwrap();
                },
                "is not a regular file",
            ),
            // Held to the secondary's own budget, not the run's.
            (
                request(),
                |_, policy, _| {
                    let target = policy.secondary_pull_request.as_mut().unwrap();
                    target.max_patch_bytes = 64;
                },
                "the patch secondary/aw-o-tool-agentic-job-agent-run-7.patch",
            ),
            (
                request(),
                |_, policy, _| {
                    let target = policy.secondary_pull_request.as_mut().unwrap();
                    target.protected_files = vec!["tool.rs".into()];
                },
                "the patch aw-o-tool-agentic-job-agent-run-7.patch: ",
            ),
            (
                request(),
                |dir, _, _| {
                    edit_report(dir, |report| {
                        report["secondary_patch"]["files"] = json!(["x"])
                    });
                },
                "names other paths than check reported",
            ),
            (
                request(),
                |dir, _, _| {
                    edit_report(dir, |report| {
                        report["secondary_patch"]["commit_files"] = json!([["tool.rs"]]);
                    });
                },
                "a replacement series",
            ),
        ];
        for (item, change, want) in cases {
            let dir = checked(std::slice::from_ref(item));
            let mut policy = policy();
            let mut run_id = json!(RUN_ID);
            change(dir.path(), &mut policy, &mut run_id);
            let errors = plan(dir.path(), &policy, run_id.as_str().unwrap(), RUN).unwrap_err();
            assert!(
                errors.iter().any(|error| error.contains(want)),
                "{item}: {errors:?}"
            );
        }
        // Nor with a link that is not one.
        let dir = checked(&[request()]);
        let errors = plan(dir.path(), &policy(), RUN_ID, "javascript:x").unwrap_err();
        assert!(errors[0].contains("not a plain http(s) link"), "{errors:?}");
    }

    /// The Forgejo tests' pull request OWN in their repository `owner/name`
    /// under ROOT, as the secondary one.
    fn secondary(root: &Path, own: super::super::PullRequest) -> PullRequest {
        PullRequest {
            repo: "owner/name".into(),
            own_repo: "caller/name".into(),
            clone_url: format!("file://{}/owner/name", root.display()),
            base: own.base,
            base_commit: own.base_commit,
            branch: BRANCH.into(),
            title: "Fix the tool".into(),
            body: "Why.".into(),
            patch: own.patch,
            files: own.files,
        }
    }

    /// The same, with the fork `fork/name` holding the main branch, as a
    /// forge's new fork does.
    fn forked_repository() -> (tempfile::TempDir, PullRequest) {
        let (root, own) = repository();
        let fork = root.path().join("fork/name.git");
        sh(
            &root.path().join("owner/name.git"),
            &["push", "-q", fork.to_str().unwrap(), "main"],
        );
        let pr = secondary(root.path(), own);
        (root, pr)
    }

    /// The branches of the fork other than the main branch it was made with.
    fn pushed(root: &Path) -> String {
        sh(
            &root.join("fork/name.git"),
            &[
                "for-each-ref",
                "--format=%(refname)",
                "--exclude=refs/heads/main",
            ],
        )
    }

    fn github(server: &str) -> Github {
        let mut github = Github::new(server, TOKEN.to_owned()).unwrap();
        github.fork_wait = Duration::ZERO;
        github
    }

    /// The token's user, and the forge's answer to forking `owner/name`.
    fn forked(fork: Value) -> Vec<(u16, String)> {
        vec![
            (200, json!({"login": "fork"}).to_string()),
            (202, fork.to_string()),
        ]
    }

    fn fork() -> Value {
        json!({"full_name": "fork/name", "fork": true, "owner": {"login": "fork"},
            "parent": {"full_name": "owner/name"}})
    }

    /// Actions turned off on the fork, and read back so.
    fn actions_off() -> Vec<(u16, String)> {
        vec![
            (204, String::new()),
            (200, json!({"enabled": false}).to_string()),
        ]
    }

    #[test]
    fn opens_a_draft_from_the_tokens_fork_with_actions_off() {
        let (root, pr) = forked_repository();
        let mut answers = forked(fork());
        answers.extend(actions_off());
        answers.push((
            201,
            json!({"html_url": "https://github.com/owner/name/pull/9"}).to_string(),
        ));
        let (server, requests) = mock(answers);
        let url = github(&server).open(&pr).unwrap();
        assert_eq!(url, "https://github.com/owner/name/pull/9");
        let sent: Vec<_> = requests.try_iter().collect();
        let lines: Vec<_> = sent.iter().map(|r| r.line.as_str()).collect();
        assert_eq!(
            lines,
            [
                "GET /user HTTP/1.1",
                "POST /repos/owner/name/forks HTTP/1.1",
                "PUT /repos/fork/name/actions/permissions HTTP/1.1",
                "GET /repos/fork/name/actions/permissions HTTP/1.1",
                "POST /repos/owner/name/pulls HTTP/1.1",
            ]
        );
        let bearer = format!("Bearer {TOKEN}");
        assert!(sent.iter().all(|r| r.authorization == bearer));
        assert_eq!(sent[2].body, json!({"enabled": false}));
        assert_eq!(
            sent[4].body,
            json!({"title": "Fix the tool", "body": "Why.", "head": format!("fork:{BRANCH}"),
                "base": "main", "draft": true})
        );
        // The branch is in the fork, and only there, on the base.
        assert_eq!(
            sh(
                &root.path().join("owner/name.git"),
                &["for-each-ref", "--format=%(refname)"]
            ),
            "refs/heads/main"
        );
        assert_eq!(pushed(root.path()), format!("refs/heads/{BRANCH}"));
        let bare = root.path().join("fork/name.git");
        assert_eq!(
            sh(&bare, &["rev-parse", &format!("{BRANCH}~")]),
            pr.base_commit
        );

        // A second attempt finds the branch there, and opens nothing.
        let mut answers = forked(fork());
        answers.extend(actions_off());
        let (server, requests) = mock(answers);
        let err = format!("{:#}", github(&server).open(&pr).unwrap_err());
        assert_eq!(
            err,
            format!("pushing {BRANCH} to fork/name: refs/heads/{BRANCH} exists already")
        );
        assert_eq!(requests.try_iter().count(), 4);
    }

    /// No push, and no pull request, from anything but the token's own
    /// fork of the repository with Actions off.
    #[test]
    fn refuses_another_fork_or_actions_on() {
        let not_fork = "which is not fork's own fork of it";
        let with = |key: &str, value: Value| {
            let mut fork = fork();
            fork[key] = value;
            fork
        };
        let cases: Vec<(Vec<(u16, String)>, &str)> = vec![
            (
                vec![(200, json!({"login": "Owner"}).to_string())],
                "Owner owns owner/name and has no fork of it to push to",
            ),
            (vec![(401, "{}".to_owned())], "GET /user: HTTP 401"),
            (
                vec![(200, json!({"login": ""}).to_string())],
                "GET /user names nobody",
            ),
            (forked(with("fork", json!(false))), not_fork),
            (forked(with("owner", json!({"login": "other"}))), not_fork),
            (forked(with("full_name", json!("owner/name"))), not_fork),
            (
                forked(with("parent", json!({"full_name": "other/name"}))),
                not_fork,
            ),
            (
                [forked(fork()), vec![(403, "{}".to_owned())]].concat(),
                "Actions could not be turned off on the fork fork/name",
            ),
            (
                [
                    forked(fork()),
                    vec![
                        (204, String::new()),
                        (200, json!({"enabled": true}).to_string()),
                    ],
                ]
                .concat(),
                "Actions could not be turned off on the fork fork/name",
            ),
        ];
        for (answers, want) in cases {
            let (root, pr) = forked_repository();
            let (server, _requests) = mock(answers);
            let err = format!("{:#}", github(&server).open(&pr).unwrap_err());
            assert!(err.contains(want), "{want}: {err}");
            assert!(!err.contains(TOKEN), "{err}");
            assert_eq!(pushed(root.path()), "");
        }
        // Nor to the run's own repository, where it is the token's fork:
        // asked nothing after the fork, Actions there included.
        let (root, pr) = forked_repository();
        let pr = PullRequest {
            own_repo: "Fork/Name".into(),
            ..pr
        };
        let (server, requests) = mock(forked(fork()));
        let err = format!("{:#}", github(&server).open(&pr).unwrap_err());
        assert_eq!(
            err,
            "fork's fork of owner/name is Fork/Name, the run's own repository, which this does \
             not push to"
        );
        assert_eq!(requests.try_iter().count(), 2);
        assert_eq!(pushed(root.path()), "");
        // Nor to a fork git does not find, with no branch yet.
        let (root, own) = repository();
        let pr = secondary(root.path(), own);
        let (server, _requests) = mock(forked(fork()));
        let err = format!("{:#}", github(&server).open(&pr).unwrap_err());
        assert_eq!(
            err,
            format!("the fork fork/name is still not there after {FORK_TRIES} tries")
        );
        assert_eq!(pushed(root.path()), "");
    }
}
