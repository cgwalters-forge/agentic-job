//! [`Publisher`] for GitLab, through its REST API v4: a note on the
//! caller's issue or merge request, and a merge request from a branch
//! pushed over git to the fork the bounds name, opened on the fork with
//! the repository as its `target_project_id`. Never a branch of the
//! repository itself, for the reason Forgejo's publisher gives.
//!
//! GitLab numbers issues and merge requests apart within a project (`#5`
//! and `!5`, each an `iid`), so a note's target kind is the caller's
//! (`--comment-target-kind`), never guessed from the number. A project is
//! read by its URL-encoded path, its answer held to that path, and then
//! written to by the ID read, so that a project renamed or moved away is
//! not written to.
//!
//! The text is filtered before it gets here ([`super::mentions`]):
//! GitLab's mentions, `@all` and `@here` among them, and its quick
//! actions, a line starting with `/` that GitLab would run with the
//! token's rights.
//!
//! The token goes in one header, `Authorization: Bearer`, never a URL or
//! an argument; git gets it as `oauth2` over Basic through its
//! environment, and every error is scrubbed of it.

use anyhow::{Context, Result, ensure};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde_json::{Value, json};

use super::forgejo::{self, Api};
use super::git::{Git, Push};
use super::{
    BranchPush, Close, CommitStatus, Kind, LabelChange, Made, NewIssue, Op, Publisher, PullRequest,
};

const GET: &str = "GET";
const POST: &str = "POST";

/// The user git sends a token as: GitLab takes any for an access token,
/// and this one for an OAuth token.
const GIT_USER: &str = "oauth2";

pub struct Gitlab {
    /// `SERVER/api/v4`.
    api: String,
    /// `SERVER`, under which git finds a project as `PATH.git`.
    git: String,
    /// The project published to: the policy's, which it has checked.
    repo: String,
    token: String,
    http: ureq::Agent,
    recover_pushed_commit: Option<String>,
    /// What the caller's comment target is.
    comment_kind: Option<Kind>,
}

/// What GitLab says of a project, as far as a push to it needs.
#[derive(Debug)]
struct Project {
    id: u64,
    path: String,
    /// Readable by its members only.
    private: bool,
    /// Readable by anyone, signed in or not.
    public: bool,
    /// The ID of what it is a fork of.
    forked_from: Option<u64>,
}

/// PATH as one path segment: every byte but a letter, digit or `-._~`
/// encoded, `/` included, as GitLab names a project by its path.
fn encoded(path: &str) -> String {
    path.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                char::from(b).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

/// The API's name for the items of KIND under a project.
fn collection(kind: Kind) -> &'static str {
    match kind {
        Kind::Issue => "issues",
        Kind::PullRequest => "merge_requests",
    }
}

/// What NUMBER of KIND is called on GitLab.
fn reference(kind: Kind, number: u64) -> String {
    match kind {
        Kind::Issue => format!("#{number}"),
        Kind::PullRequest => format!("!{number}"),
    }
}

impl Gitlab {
    /// SERVER is GitLab's address, `https://gitlab.example` or one under a
    /// path; REPO the policy's project path, which it has checked.
    pub fn new(server: &str, repo: &str, token: String, allow_http: &[String]) -> Result<Self> {
        let server = forgejo::server(server, allow_http)?;
        Ok(Self {
            api: format!("{server}/api/v4"),
            git: server.to_owned(),
            repo: repo.to_owned(),
            token,
            http: forgejo::agent(),
            recover_pushed_commit: None,
            comment_kind: None,
        })
    }

    /// Only the trusted deployment may supply this, never agent output.
    pub fn recovering(mut self, recover: Option<String>) -> Self {
        self.recover_pushed_commit = recover;
        self
    }

    /// What the caller's `--comment-target` numbers: without it, no note
    /// is posted.
    pub fn commenting_on(mut self, kind: Option<Kind>) -> Self {
        self.comment_kind = kind;
        self
    }

    fn request(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Value> {
        Api {
            http: &self.http,
            url: &self.api,
            authorization: &format!("Bearer {}", self.token),
            token: &self.token,
        }
        .request(method, path, body)
    }

    /// What git fetches PROJECT from, or pushes to it at.
    fn remote(&self, project: &str) -> String {
        format!("{}/{project}.git", self.git)
    }

    /// PATH, as GitLab answers for it: refused unless the answer is for
    /// that very path, not one it was renamed or moved to, and says what
    /// is needed.
    fn project(&self, path: &str) -> Result<Project> {
        let at = format!("/projects/{}", encoded(path));
        let answer = self.request(GET, &at, None)?;
        let missing = |what: &str| format!("GET {at}: the response has no {what}");
        let id = answer["id"].as_u64().with_context(|| missing("id"))?;
        let named = answer["path_with_namespace"]
            .as_str()
            .with_context(|| missing("path_with_namespace"))?;
        ensure!(
            named.eq_ignore_ascii_case(path),
            "GitLab answers for {path} with {named}: it was renamed or moved"
        );
        let visibility = answer["visibility"]
            .as_str()
            .with_context(|| missing("visibility"))?;
        ensure!(
            ["private", "internal", "public"].contains(&visibility),
            "GET {at}: visibility {visibility:?} is not one GitLab has"
        );
        let forked_from = match &answer["forked_from_project"] {
            Value::Null => None,
            parent => Some(
                parent["id"]
                    .as_u64()
                    .with_context(|| missing("forked_from_project id"))?,
            ),
        };
        Ok(Project {
            id,
            path: named.to_owned(),
            private: visibility == "private",
            public: visibility == "public",
            forked_from,
        })
    }

    /// The repository's project and the fork's, refused unless the fork
    /// is the repository's, and no more readable than the repository's
    /// audience, as [`forgejo`]'s `check_fork` holds them.
    fn check_fork(&self, push_repo: &str, private_run: bool) -> Result<(Project, Project)> {
        let target = self.project(&self.repo)?;
        let fork = self.project(push_repo)?;
        ensure!(
            fork.forked_from == Some(target.id),
            "{push_repo} is not a fork of {}",
            self.repo
        );
        ensure!(
            fork.private || (target.public && !private_run),
            "{push_repo} is not private, and {} is not public: its branch would publish the repository's code",
            self.repo
        );
        Ok((target, fork))
    }

    /// The repository's project ID and the page of NUMBER of KIND in it,
    /// refused unless GitLab answers for it as that item of this project.
    fn item(&self, number: u64, kind: Kind) -> Result<(u64, String)> {
        let project = self.project(&self.repo)?;
        let path = format!(
            "/projects/{}/{}/{number}",
            encoded(&self.repo),
            collection(kind)
        );
        let answer = self.request(GET, &path, None)?;
        ensure!(
            answer["iid"].as_u64() == Some(number)
                && answer["project_id"].as_u64() == Some(project.id),
            "{} is not an item of {}",
            reference(kind, number),
            project.path
        );
        let url = answer["web_url"]
            .as_str()
            .with_context(|| format!("GET {path}: the response has no web_url"))?;
        Ok((project.id, url.to_owned()))
    }

    /// The kind of the caller's comment target: refused without one.
    fn comment_kind(&self) -> Result<Kind> {
        self.comment_kind
            .context("a comment on GitLab, and the caller gave no --comment-target-kind for it")
    }

    /// Refuses a push repository of the repository's top-level group,
    /// as Forgejo's publisher refuses one of its owner.
    fn other_owner(&self, push_repo: &str) -> Result<()> {
        ensure!(
            !super::owner(push_repo).eq_ignore_ascii_case(super::owner(&self.repo)),
            "a merge request's branch goes to another owner's fork, not to {push_repo}"
        );
        Ok(())
    }

    /// Pushes the merge request's branch to its fork: see [`Git::push`].
    fn push(&self, pr: &PullRequest) -> Result<String> {
        let credential = BASE64.encode(format!("{GIT_USER}:{}", self.token));
        let git = Git {
            authorization: format!("Basic {credential}"),
            token: &self.token,
        };
        git.push(
            &Push {
                from: &self.remote(&self.repo),
                base: &pr.base,
                base_commit: &pr.base_commit,
                to: &self.remote(&pr.push_repo),
                branch: &pr.branch,
                patch: &pr.patch,
                files: &pr.files,
                head: None,
                committer: None,
            },
            self.recover_pushed_commit.as_deref(),
        )
    }
}

/// A write `plan` refuses on GitLab, refused here as well.
fn unsupported(what: &str) -> anyhow::Error {
    anyhow::anyhow!("{what}, which apply does not publish to GitLab")
}

impl Publisher for Gitlab {
    fn preflight(&self, op: &Op) -> Result<()> {
        match op {
            Op::Comment { issue, .. } => {
                self.item(*issue, self.comment_kind()?)?;
            }
            // The push makes its own checks, and it is the first write.
            Op::PullRequest(_) => {}
            Op::Push(_) => return Err(unsupported("a push to a merge request's branch")),
            Op::Issue(_) => return Err(unsupported("an issue")),
            Op::Labels(_) => return Err(unsupported("a label change")),
            Op::Close(_) => return Err(unsupported("closing")),
            Op::Status(_) => return Err(unsupported("a commit status")),
        }
        Ok(())
    }

    /// GitLab's note has no page of its own: it is the item's, at the
    /// note's anchor. The item is read again, as a moment may have passed
    /// since the preflight, and the note goes to the project by the ID
    /// read, which a rename since cannot point elsewhere.
    fn comment(&self, issue: u64, body: &str) -> Result<Made> {
        let kind = self.comment_kind()?;
        let (project, url) = self.item(issue, kind)?;
        let path = format!("/projects/{project}/{}/{issue}/notes", collection(kind));
        let answer = self.request(POST, &path, Some(&json!({"body": body})))?;
        let id = answer["id"]
            .as_u64()
            .with_context(|| format!("POST {path}: the response names no note id"))?;
        Ok(Made {
            url: format!("{url}#note_{id}"),
            number: None,
            id: Some(id),
            node_id: None,
        })
    }

    fn open_pull_request(&self, pr: &PullRequest) -> Result<Made> {
        // `plan` refuses this too; held here as well, as nothing else
        // stands between the agent's code and the repository's CI.
        self.other_owner(&pr.push_repo)?;
        let pushing = || format!("pushing {} to {}", pr.branch, pr.push_repo);
        let (target, fork) = self
            .check_fork(&pr.push_repo, pr.private)
            .with_context(pushing)?;
        let commit = self.push(pr).with_context(pushing)?;
        // Opened on the fork, the branch's project, for the repository,
        // both by the IDs `check_fork` read.
        let path = format!("/projects/{}/merge_requests", fork.id);
        let opened = || -> Result<Made> {
            let answer = self.request(
                POST,
                &path,
                Some(&json!({
                    "source_branch": pr.branch,
                    "target_branch": pr.base,
                    "target_project_id": target.id,
                    "title": pr.title,
                    "description": pr.body,
                })),
            )?;
            let url = answer["web_url"]
                .as_str()
                .with_context(|| format!("POST {path}: the response names no web_url"))?;
            Ok(Made {
                url: url.to_owned(),
                number: answer["iid"].as_u64(),
                id: answer["id"].as_u64(),
                node_id: None,
            })
        };
        opened().with_context(|| {
            format!(
                "published {commit} to {} branch {}; merge request creation failed; retry the same checked input with --recover-pushed-commit {commit} (no branch overwrite)",
                pr.push_repo, pr.branch
            )
            .replace(&self.token, "[token]")
        })
    }

    fn push_to_branch(&self, _: &BranchPush) -> Result<String> {
        Err(unsupported("a push to a merge request's branch"))
    }

    fn create_issue(&self, _: &NewIssue) -> Result<String> {
        Err(unsupported("an issue"))
    }

    fn change_labels(&self, _: &LabelChange) -> Result<String> {
        Err(unsupported("a label change"))
    }

    fn close(&self, _: &Close) -> Result<String> {
        Err(unsupported("closing"))
    }

    fn status(&self, _: &CommitStatus) -> Result<String> {
        Err(unsupported("a commit status"))
    }
}

#[cfg(test)]
mod tests {
    use super::super::forgejo::tests::{mock, repository, sh};
    use super::super::tests::{checked, policy, target};
    use super::super::{Forge, Target, plan};
    use super::*;

    const TOKEN: &str = "test-token-not-secret";

    fn gitlab(server: &str) -> Gitlab {
        Gitlab::new(&format!("{server}/"), "owner/name", TOKEN.to_owned(), &[]).unwrap()
    }

    fn ok(answer: &Value) -> (u16, String) {
        (200, answer.to_string())
    }

    /// GitLab's answer about a project: its ID, path and visibility, and
    /// the ID of what it is a fork of.
    fn project(id: u64, path: &str, visibility: &str, forked_from: Option<u64>) -> Value {
        json!({
            "id": id, "path_with_namespace": path, "visibility": visibility,
            "forked_from_project": forked_from.map(|id| json!({"id": id})),
        })
    }

    /// The repository, project 1, as GitLab answers for it.
    fn repo() -> Value {
        project(1, "owner/name", "public", None)
    }

    /// Item IID of KIND in project PROJECT, as GitLab answers for it.
    fn item(kind: Kind, iid: u64, project: u64) -> Value {
        json!({
            "iid": iid, "project_id": project,
            "web_url": format!("https://gitlab.example/owner/name/-/{}/{iid}", collection(kind)),
        })
    }

    /// A planned note goes to the issue or the merge request the caller
    /// says, by its iid, with its mentions and quick actions broken.
    #[test]
    fn comments_on_an_issue_or_a_merge_request() {
        let checked = checked(&[json!({"type": "add_comment",
            "body": "Done. @all @group/team\n/merge\n  /approve"})]);
        let mut policy = policy();
        policy.repo = "owner/name".into();
        policy.forge = crate::policy::Forge::Gitlab;
        let target = Target {
            forge: Forge::Gitlab,
            repo: "owner/name",
            ..target(Some(5))
        };
        let ops = plan(checked.path(), &policy, &target).unwrap();
        let [Op::Comment { body, .. }] = &ops[..] else {
            panic!("expected a planned comment: {ops:?}");
        };
        assert_eq!(
            body,
            &format!(
                "Done. @\u{200b}all @\u{200b}group/team\n\u{200b}/merge\n  \u{200b}/approve\n\n> Generated by [agentic-job]({})",
                target.run_url
            )
        );
        for (kind, items) in [
            (Kind::Issue, "issues"),
            (Kind::PullRequest, "merge_requests"),
        ] {
            let (server, requests) = mock(vec![
                ok(&repo()),
                ok(&item(kind, 5, 1)),
                ok(&repo()),
                ok(&item(kind, 5, 1)),
                (
                    201,
                    json!({"id": 30, "body": body, "noteable_iid": 5}).to_string(),
                ),
            ]);
            let gitlab = gitlab(&server).commenting_on(Some(kind));
            gitlab.preflight(&ops[0]).unwrap();
            let made = gitlab.comment(5, body).unwrap();
            assert_eq!(
                made.url,
                format!("https://gitlab.example/owner/name/-/{items}/5#note_30")
            );
            assert_eq!(made.id, Some(30));
            let lines: Vec<String> = requests
                .iter()
                .map(|request| {
                    assert_eq!(request.authorization, format!("Bearer {TOKEN}"));
                    if request.line.starts_with("POST") {
                        assert_eq!(request.body, json!({"body": body}));
                    }
                    request.line
                })
                .collect();
            let item = format!("GET /api/v4/projects/owner%2Fname/{items}/5 HTTP/1.1");
            let project = "GET /api/v4/projects/owner%2Fname HTTP/1.1";
            assert_eq!(
                lines,
                [
                    project,
                    &item,
                    project,
                    &item,
                    &format!("POST /api/v4/projects/1/{items}/5/notes HTTP/1.1"),
                ]
            );
        }
    }

    /// No note goes to a target GitLab does not answer for as the
    /// caller's: of no kind the caller named, of another project, or
    /// another number; nor to a project renamed or moved.
    #[test]
    fn comment_target_refusals() {
        let op = Op::Comment {
            issue: 5,
            body: "Done.".into(),
        };
        let cases: Vec<(Option<Kind>, Vec<Value>, &str)> = vec![
            (None, vec![], "no --comment-target-kind"),
            (
                Some(Kind::Issue),
                vec![repo(), item(Kind::Issue, 5, 2)],
                "#5 is not an item of owner/name",
            ),
            (
                Some(Kind::PullRequest),
                vec![repo(), item(Kind::PullRequest, 6, 1)],
                "!5 is not an item of owner/name",
            ),
            (
                Some(Kind::Issue),
                vec![project(1, "other/name", "public", None)],
                "GitLab answers for owner/name with other/name",
            ),
        ];
        for (kind, answers, refusal) in cases {
            let (server, requests) = mock(answers.iter().map(ok).collect());
            let gitlab = gitlab(&server).commenting_on(kind);
            let err = format!("{:#}", gitlab.preflight(&op).unwrap_err());
            assert!(err.contains(refusal), "{err}");
            assert!(
                requests.try_iter().all(|r| r.line.starts_with("GET")),
                "{refusal}: nothing written"
            );
        }
    }

    /// Collection preserves Markdown escapes. Planning must not mistake
    /// escaped backticks for code and leave a live mention for GitLab.
    #[test]
    fn escaped_code_mentions_after_collection() {
        let mut policy = policy();
        policy.forge = crate::policy::Forge::Gitlab;
        let target = Target {
            forge: Forge::Gitlab,
            ..target(Some(5))
        };
        for body in [r"\`@all\`", r"\`@group/team\`", "```\n@here\n```"] {
            let collected = crate::collect::collect(
                &json!({"type": "add_comment", "body": body}).to_string(),
                &crate::collect::config(&policy).unwrap(),
                &|_| true,
            );
            assert!(collected.errors.is_empty(), "{:?}", collected.errors);
            let items: Vec<Value> = collected.items.into_iter().map(Value::Object).collect();
            let dir = checked(&items);
            let ops = plan(dir.path(), &policy, &target).unwrap();
            let [Op::Comment { body, .. }] = &ops[..] else {
                panic!("expected a comment: {ops:?}");
            };
            assert!(body.contains("@\u{200b}"), "{body:?}");
            for mention in ["@all", "@group/team", "@here"] {
                assert!(!body.contains(mention), "{body:?}");
            }
        }
    }

    /// Only a comment and a merge request are published; the others are
    /// refused before any request.
    #[test]
    fn other_ops_are_refused() {
        let gitlab = gitlab("http://127.0.0.1:1");
        let ops = [
            Op::Issue(NewIssue {
                title: "t".into(),
                body: "b".into(),
                labels: vec![],
            }),
            Op::Labels(LabelChange {
                item: 5,
                add: true,
                labels: vec!["bug".into()],
            }),
            Op::Close(Close {
                number: 5,
                kind: Kind::Issue,
                body: "b".into(),
            }),
        ];
        for op in ops {
            let err = format!("{:#}", gitlab.preflight(&op).unwrap_err());
            assert!(
                err.ends_with("which apply does not publish to GitLab"),
                "{err}"
            );
        }
    }

    #[test]
    fn opens_a_merge_request_from_a_fork() {
        let (root, pr) = repository();
        let (server, requests) = mock(vec![
            ok(&repo()),
            ok(&project(2, "fork/name", "public", Some(1))),
            (
                201,
                json!({"id": 90, "iid": 9, "project_id": 2, "target_project_id": 1,
                    "web_url": "https://gitlab.example/owner/name/-/merge_requests/9"})
                .to_string(),
            ),
        ]);
        let mut gitlab = gitlab(&server);
        gitlab.git = root.path().display().to_string();
        let made = gitlab.open_pull_request(&pr).unwrap();
        assert_eq!(
            made.url,
            "https://gitlab.example/owner/name/-/merge_requests/9"
        );
        assert_eq!((made.number, made.id), (Some(9), Some(90)));
        for repo in ["owner%2Fname", "fork%2Fname"] {
            let request = requests.recv().unwrap();
            assert_eq!(
                request.line,
                format!("GET /api/v4/projects/{repo} HTTP/1.1")
            );
            assert_eq!(request.authorization, format!("Bearer {TOKEN}"));
        }
        let request = requests.recv().unwrap();
        assert_eq!(
            request.line,
            "POST /api/v4/projects/2/merge_requests HTTP/1.1"
        );
        assert_eq!(
            request.body,
            json!({"source_branch": "agent-run-7", "target_branch": "main",
                "target_project_id": 1, "title": "WIP: Fix it", "description": "Why."})
        );
        // The branch is in the fork, and only there.
        assert_eq!(
            sh(
                &root.path().join("owner/name.git"),
                &["for-each-ref", "--format=%(refname)"]
            ),
            "refs/heads/main"
        );
        let fork = root.path().join("fork/name.git");
        assert_eq!(sh(&fork, &["rev-parse", "agent-run-7~"]), pr.base_commit);
    }

    /// A push project that is not the repository's fork, or one more can
    /// read than the repository's audience, gets no push.
    #[test]
    fn fork_identity_and_visibility() {
        let not_public = "fork/name is not private, and owner/name is not public";
        let not_fork = "fork/name is not a fork of owner/name";
        // The repository's answer, the fork's, whether the run is private,
        // and a part of the refusal.
        let cases: Vec<(Value, Value, bool, &str)> = vec![
            (
                repo(),
                project(2, "fork/name", "public", Some(3)),
                false,
                not_fork,
            ),
            (
                repo(),
                project(2, "fork/name", "public", None),
                false,
                not_fork,
            ),
            (
                project(1, "owner/name", "internal", None),
                project(2, "fork/name", "internal", Some(1)),
                false,
                not_public,
            ),
            (
                project(1, "owner/name", "private", None),
                project(2, "fork/name", "public", Some(1)),
                false,
                not_public,
            ),
            (
                repo(),
                project(2, "fork/name", "public", Some(1)),
                true,
                not_public,
            ),
            (
                repo(),
                project(2, "fork/name", "secret", Some(1)),
                false,
                "visibility \"secret\"",
            ),
            (
                repo(),
                project(2, "someone/else", "private", Some(1)),
                false,
                "GitLab answers for fork/name with someone/else",
            ),
        ];
        for (target, fork, private, refusal) in cases {
            let (root, mut pr) = repository();
            pr.private = private;
            let (server, requests) = mock(vec![ok(&target), ok(&fork)]);
            let mut gitlab = gitlab(&server);
            gitlab.git = root.path().display().to_string();
            let err = format!("{:#}", gitlab.open_pull_request(&pr).unwrap_err());
            assert!(err.contains(refusal), "{err}");
            assert_eq!(requests.iter().count(), 2, "{refusal}: no merge request");
            assert_eq!(
                sh(&root.path().join("fork/name.git"), &["for-each-ref"]),
                "",
                "{refusal}: no push"
            );
        }
        // A private fork of a private repository is pushed to.
        let (root, pr) = repository();
        let (server, _requests) = mock(vec![
            ok(&project(1, "owner/name", "private", None)),
            ok(&project(2, "fork/name", "private", Some(1))),
            ok(
                &json!({"iid": 9, "web_url": "https://gitlab.example/owner/name/-/merge_requests/9"}),
            ),
        ]);
        let mut gitlab = gitlab(&server);
        gitlab.git = root.path().display().to_string();
        gitlab.open_pull_request(&pr).unwrap();
    }

    /// A server that echoes the token, or does not answer, never puts it
    /// in the error.
    #[test]
    fn the_token_is_not_in_errors() {
        let (server, _requests) = mock(vec![
            (401, format!("{{\"message\": \"bad token {TOKEN}\"}}")),
            (302, String::new()),
        ]);
        let gitlab = gitlab(&server).commenting_on(Some(Kind::Issue));
        for _ in 0..2 {
            let err = format!("{:#}", gitlab.comment(5, "Done.").unwrap_err());
            assert!(!err.contains(&TOKEN[..4]), "{err}");
            assert!(err.starts_with("GET /projects/owner%2Fname"), "{err}");
        }
        let err = format!(
            "{:#}",
            self::gitlab("http://127.0.0.1:1")
                .commenting_on(Some(Kind::Issue))
                .comment(5, "x")
                .unwrap_err()
        );
        assert!(!err.contains(TOKEN), "{err}");
    }

    #[test]
    fn project_paths() {
        for (path, want) in [
            ("owner/name", "owner%2Fname"),
            ("group/sub/name", "group%2Fsub%2Fname"),
            ("a.b-c_d/e~f", "a.b-c_d%2Fe~f"),
            ("o/n?x#y", "o%2Fn%3Fx%23y"),
        ] {
            assert_eq!(encoded(path), want);
        }
    }
}
