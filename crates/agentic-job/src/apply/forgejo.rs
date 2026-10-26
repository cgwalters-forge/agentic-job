//! [`Publisher`] for Forgejo (and Gitea, whose API it keeps): comments
//! through `/api/v1`, and a pull request from a branch pushed over git to
//! a fork the bounds name. Never to the repository itself: there, its CI
//! would run the agent's code as the repository's own, with its secrets
//! and a write token, where a fork's pull request gets neither by default
//! (docs/safe-outputs.md#publishing-to-forgejo).
//!
//! The token goes in one header, never a URL or an argument: git gets it
//! through its environment, which other users cannot read, and every
//! error is scrubbed of it before it leaves here, since a server may echo
//! a request back.

use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};

use super::git::{self, Git, Push};
use super::{Publisher, PullRequest};

/// The most of a response read: what a comment or pull request returns.
pub(super) const MAX_RESPONSE: u64 = 1 << 20;

pub(super) const TIMEOUT: Duration = Duration::from_secs(60);

pub struct Forgejo {
    /// `SERVER/api/v1`.
    api: String,
    /// `SERVER`, under which git finds a repository as `OWNER/NAME.git`.
    git: String,
    /// The repository published to: the policy's, which it has checked.
    repo: String,
    token: String,
    http: ureq::Agent,
    recover_pushed_commit: Option<String>,
}

/// What the forge says of a repository, as far as a push to it needs.
#[derive(Debug)]
struct Repository {
    full_name: String,
    /// Readable by its collaborators only.
    private: bool,
    /// Readable by anyone: not private, and neither its owner (`internal`)
    /// nor the owner's own visibility (`limited`, `private`) restricts it.
    public: bool,
    /// What it is a fork of.
    parent: Option<String>,
}

impl Forgejo {
    /// SERVER is the forge's address, `https://forge.example` or one
    /// under a path; REPO the policy's, which it has checked.
    pub fn new(server: &str, repo: &str, token: String, allow_http: &[String]) -> Result<Self> {
        let server = server.trim_end_matches('/');
        let rest = server
            .strip_prefix("https://")
            .or_else(|| server.strip_prefix("http://"));
        // Credentials, a query or a fragment in the address would go to
        // git and the API as written; the token is the only credential.
        ensure!(
            rest.is_some_and(|rest| !rest.is_empty()
                && !rest
                    .chars()
                    .any(|c| c.is_whitespace() || c.is_control() || "@?#\\".contains(c))),
            "--server {server:?} is not a plain http(s) address"
        );
        if let Some(rest) = server.strip_prefix("http://") {
            let authority = rest.split('/').next().unwrap_or_default();
            // Unapproved HTTP would expose the publish token. Match the whole
            // authority (including port), as clone URLs do, not a DNS suffix.
            // Literal loopback addresses remain available for local fixtures.
            let loopback = matches!(authority, "127.0.0.1" | "[::1]")
                || authority
                    .parse::<std::net::SocketAddr>()
                    .is_ok_and(|address| address.ip().is_loopback());
            ensure!(
                loopback || crate::policy::allows_http(allow_http, authority),
                "--server requires HTTPS except for literal loopback addresses or a host named in the bounds' allow_http"
            );
        }
        let http = ureq::Agent::config_builder()
            .timeout_global(Some(TIMEOUT))
            .proxy(None)
            // The token is not to follow a redirect anywhere.
            .max_redirects(0)
            .http_status_as_error(false)
            .build()
            .into();
        Ok(Self {
            api: format!("{server}/api/v1"),
            git: server.to_owned(),
            repo: repo.to_owned(),
            token,
            http,
            recover_pushed_commit: None,
        })
    }

    /// Only the trusted deployment may supply this, never agent output.
    pub fn recovering(mut self, recover: Option<String>) -> Self {
        self.recover_pushed_commit = recover;
        self
    }

    /// What git fetches REPO from, or pushes to it at.
    fn remote(&self, repo: &str) -> String {
        format!("{}/{repo}.git", self.git)
    }

    /// TEXT without the token.
    fn scrub(&self, text: &str) -> String {
        text.replace(&self.token, "[token]")
    }

    /// Sends a GET, or a POST of BODY, to PATH under the API, and returns
    /// the JSON of a successful answer.
    fn send(&self, path: &str, body: Option<&Value>) -> Result<Value> {
        let url = format!("{}{path}", self.api);
        let authorization = format!("token {}", self.token);
        let (method, response) = match body {
            Some(body) => (
                "POST",
                self.http
                    .post(url)
                    .header("authorization", authorization)
                    .header("accept", "application/json")
                    .header("content-type", "application/json")
                    .send(body.to_string().as_bytes()),
            ),
            None => (
                "GET",
                self.http
                    .get(url)
                    .header("authorization", authorization)
                    .header("accept", "application/json")
                    .call(),
            ),
        };
        let mut response = match response {
            Ok(response) => response,
            Err(err) => bail!("{method} {path}: {}", self.scrub(&err.to_string())),
        };
        let status = response.status();
        let text = match response
            .body_mut()
            .with_config()
            .limit(MAX_RESPONSE)
            .read_to_string()
        {
            Ok(text) => text,
            Err(err) => bail!("{method} {path}: {}", self.scrub(&err.to_string())),
        };
        if !status.is_success() {
            bail!(
                "{method} {path}: HTTP {}: {}",
                status.as_u16(),
                self.shown(&text)
            );
        }
        serde_json::from_str(&text)
            .with_context(|| format!("{method} {path}: the response is not JSON"))
    }

    /// POSTs BODY to PATH under the API and returns the `html_url` of
    /// what it made.
    fn post(&self, path: &str, body: &Value) -> Result<String> {
        self.send(path, Some(body))?["html_url"]
            .as_str()
            .map(str::to_owned)
            .with_context(|| format!("POST {path}: the response names no html_url"))
    }

    /// REPO, as the forge answers for it: refused when the answer does not
    /// say what is needed, rather than taken for anything.
    fn repository(&self, repo: &str) -> Result<Repository> {
        let path = format!("/repos/{repo}");
        let answer = self.send(&path, None)?;
        let missing = |what: &str| format!("GET {path}: the response has no {what}");
        let flag = |key: &str| {
            answer[key]
                .as_bool()
                .with_context(|| missing(&format!("boolean {key:?}")))
        };
        let text = |value: &Value, what: &str| {
            value
                .as_str()
                .map(str::to_owned)
                .with_context(|| missing(what))
        };
        let private = flag("private")?;
        let internal = flag("internal")?;
        let owner = text(&answer["owner"]["visibility"], "owner visibility")?;
        Ok(Repository {
            full_name: text(&answer["full_name"], "full_name")?,
            private,
            public: !private && !internal && owner == "public",
            parent: if flag("fork")? {
                Some(text(&answer["parent"]["full_name"], "parent")?)
            } else {
                None
            },
        })
    }

    /// TEXT as an error shows it: see [`git::shown`].
    fn shown(&self, text: &str) -> String {
        git::shown(&[&self.token], text)
    }

    /// Refuses a push repository that is not a fork of the repository, as
    /// Forgejo reads `OWNER:BRANCH` (the repository itself for its own
    /// owner, else that owner's fork of it); and a fork that more can read
    /// than the repository's audience, where its branch would publish the
    /// code of a repository that is not public, or of a private run.
    fn check_fork(&self, push_repo: &str, private_run: bool) -> Result<()> {
        let target = self.repository(&self.repo)?;
        let fork = self.repository(push_repo)?;
        ensure!(
            target.full_name.eq_ignore_ascii_case(&self.repo)
                && fork.full_name.eq_ignore_ascii_case(push_repo)
                && fork
                    .parent
                    .as_deref()
                    .is_some_and(|parent| parent.eq_ignore_ascii_case(&self.repo)),
            "{push_repo} is not a fork of {}",
            self.repo
        );
        ensure!(
            fork.private || (target.public && !private_run),
            "{push_repo} is not private, and {} is not public: its branch would publish the repository's code",
            self.repo
        );
        Ok(())
    }

    /// Pushes the pull request's branch to its fork: see [`Git::push`].
    fn push(&self, pr: &PullRequest) -> Result<String> {
        let git = Git {
            authorization: format!("token {}", self.token),
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
            },
            self.recover_pushed_commit.as_deref(),
        )
    }
}

impl Publisher for Forgejo {
    fn comment(&self, issue: u64, body: &str) -> Result<String> {
        self.post(
            &format!("/repos/{}/issues/{issue}/comments", self.repo),
            &json!({"body": body}),
        )
    }

    fn open_pull_request(&self, pr: &PullRequest) -> Result<String> {
        // `plan` refuses this too; held here as well, as nothing else
        // stands between the agent's code and the repository's CI. Forgejo
        // reads a head of the repository's own owner as its own branch.
        let owner = super::owner(&pr.push_repo);
        ensure!(
            !owner.eq_ignore_ascii_case(super::owner(&self.repo)),
            "a pull request's branch goes to another owner's fork, not to {}",
            pr.push_repo
        );
        let pushing = || format!("pushing {} to {}", pr.branch, pr.push_repo);
        self.check_fork(&pr.push_repo, pr.private)
            .with_context(pushing)?;
        let commit = self.push(pr).with_context(pushing)?;
        let head = format!("{owner}:{}", pr.branch);
        self.post(
            &format!("/repos/{}/pulls", self.repo),
            &json!({"title": pr.title, "body": pr.body, "head": head, "base": pr.base}),
        )
        .with_context(|| self.scrub(&format!(
            "published {commit} to {} branch {}; PR creation failed; retry the same checked input with --recover-pushed-commit {commit} (no branch overwrite)",
            pr.push_repo, pr.branch
        )))
    }
}

#[cfg(test)]
pub(super) mod tests {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::path::Path;
    use std::process::Command;
    use std::sync::mpsc;

    use super::super::git::SHOWN_CHARS;
    use super::*;

    const TOKEN: &str = "test-token-not-secret";

    /// What the mock forge was sent: the request line, the authorization
    /// header and the body.
    #[derive(Debug)]
    pub(in crate::apply) struct Request {
        pub line: String,
        pub authorization: String,
        pub body: Value,
    }

    /// A forge that answers each request with STATUS and BODY in turn.
    pub(in crate::apply) fn mock(answers: Vec<(u16, String)>) -> (String, mpsc::Receiver<Request>) {
        let mut answers = answers.into_iter();
        serve(answers.len(), move |_| answers.next().unwrap())
    }

    /// A forge that answers COUNT requests, each with the status and body
    /// ANSWER gives for it.
    pub(in crate::apply) fn serve(
        count: usize,
        mut answer: impl FnMut(&Request) -> (u16, String) + Send + 'static,
    ) -> (String, mpsc::Receiver<Request>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let server = format!("http://{}", listener.local_addr().unwrap());
        let (sent, received) = mpsc::channel();
        std::thread::spawn(move || {
            for _ in 0..count {
                let (stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let (mut authorization, mut length) = (String::new(), 0);
                loop {
                    let mut header = String::new();
                    reader.read_line(&mut header).unwrap();
                    if header == "\r\n" {
                        break;
                    }
                    let (name, value) = header.split_once(':').unwrap();
                    match name.to_ascii_lowercase().as_str() {
                        "authorization" => authorization = value.trim().to_owned(),
                        "content-length" => length = value.trim().parse().unwrap(),
                        _ => {}
                    }
                }
                let mut request = vec![0; length];
                reader.read_exact(&mut request).unwrap();
                let request = Request {
                    line: line.trim().to_owned(),
                    authorization,
                    body: if request.is_empty() {
                        Value::Null
                    } else {
                        serde_json::from_slice(&request).unwrap()
                    },
                };
                let (status, body) = answer(&request);
                // A test that stopped listening has its answer already.
                let _ = sent.send(request);
                let mut stream = stream;
                write!(
                    stream,
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });
        (server, received)
    }

    fn forgejo(server: &str) -> Forgejo {
        Forgejo::new(&format!("{server}/"), "owner/name", TOKEN.to_owned(), &[]).unwrap()
    }

    /// The forge's answer about the repository: private, internal (of a
    /// private organization) or public, and its owner's visibility.
    fn target(private: bool, internal: bool, owner: &str) -> Value {
        json!({
            "full_name": "owner/name", "private": private, "internal": internal,
            "fork": false, "parent": null, "owner": {"visibility": owner},
        })
    }

    /// The forge's answer about a public or private fork of the repository.
    fn fork(private: bool) -> Value {
        json!({
            "full_name": "fork/name", "private": private, "internal": false,
            "fork": true, "parent": {"full_name": "owner/name"},
            "owner": {"visibility": "public"},
        })
    }

    fn ok(answer: &Value) -> (u16, String) {
        (200, answer.to_string())
    }

    /// The answers of a public repository and its public fork.
    fn public() -> [(u16, String); 2] {
        [ok(&target(false, false, "public")), ok(&fork(false))]
    }

    #[test]
    fn comments() {
        let (server, requests) = mock(vec![(
            201,
            json!({"id": 3, "html_url": "https://forge.example/owner/name/issues/5#issuecomment-3"})
                .to_string(),
        )]);
        let checked = super::super::tests::checked(&[
            json!({"type": "add_comment", "body": "Done. @user @org/team !12"}),
        ]);
        let mut policy = super::super::tests::policy();
        policy.repo = "owner/name".into();
        let target = super::super::Target {
            repo: "owner/name",
            ..super::super::tests::target(Some(5))
        };
        let ops = super::super::plan(checked.path(), &policy, &target).unwrap();
        let super::super::Op::Comment { body, .. } = &ops[0] else {
            panic!("expected a planned comment");
        };
        let url = forgejo(&server).comment(5, body).unwrap();
        assert_eq!(
            url,
            "https://forge.example/owner/name/issues/5#issuecomment-3"
        );
        let request = requests.recv().unwrap();
        assert_eq!(
            request.line,
            "POST /api/v1/repos/owner/name/issues/5/comments HTTP/1.1"
        );
        assert_eq!(request.authorization, format!("token {TOKEN}"));
        assert_eq!(
            request.body,
            json!({"body": format!("Done. @\u{200b}user @\u{200b}org/team !\u{200b}12\n\n> Generated by [agentic-job]({})", target.run_url)})
        );
    }

    /// A server that echoes the request, token and all, or a refusal of
    /// any kind, never puts the token in the error.
    #[test]
    fn the_token_is_not_in_errors() {
        // The second echo has the token across the point an error is cut at.
        let (server, _requests) = mock(vec![
            (403, format!("{{\"message\": \"bad token {TOKEN}\"}}")),
            (403, format!("{}{TOKEN}", "x".repeat(SHOWN_CHARS - 4))),
            (201, "{}".to_owned()),
            (302, String::new()),
        ]);
        let forgejo = forgejo(&server);
        for _ in 0..4 {
            let err = format!("{:#}", forgejo.comment(5, "Done.").unwrap_err());
            assert!(!err.contains(&TOKEN[..4]), "{err}");
            assert!(
                err.starts_with("POST /repos/owner/name/issues/5/comments"),
                "{err}"
            );
        }
        let err = format!(
            "{:#}",
            Forgejo::new("http://127.0.0.1:1", "owner/name", TOKEN.to_owned(), &[])
                .unwrap()
                .comment(5, "x")
                .unwrap_err()
        );
        assert!(!err.contains(TOKEN), "{err}");
        // Nor when the forge echoes it reading a repository's visibility.
        let (root, pr) = repository();
        let (server, _requests) = mock(vec![
            ok(&target(false, false, "public")),
            (403, format!("{{\"message\": \"bad token {TOKEN}\"}}")),
        ]);
        let mut publisher = self::forgejo(&server);
        publisher.git = root.path().display().to_string();
        let err = format!("{:#}", publisher.open_pull_request(&pr).unwrap_err());
        assert!(err.contains("GET /repos/fork/name: HTTP 403"), "{err}");
        assert!(!err.contains(&TOKEN[..4]), "{err}");
    }

    #[test]
    fn servers() {
        for (server, ok) in [
            ("https://forge.example", true),
            ("https://forge.example/git/", true),
            ("http://forge", false),
            ("http://forge.example/git", false),
            ("http://127.0.0.1:3000/git", true),
            ("http://127.0.0.1", true),
            ("http://[::1]:3000", true),
            ("http://[::1]", true),
            ("http://localhost", false),
            ("http://127.0.0.1.example", false),
            ("http://127.0.0.1:3000@forge.example", false),
            ("http://127.0.0.1:3000.evil", false),
            ("http://[::1].example", false),
            ("http://192.0.2.1", false),
            ("http://[::ffff:192.0.2.1]", false),
            ("https://", false),
            ("ftp://forge.example", false),
            ("https://user:pass@forge.example", false),
            ("https://forge.example/?x=1", false),
            ("https://forge.example/#x", false),
            ("https://forge example", false),
            ("forge.example", false),
        ] {
            assert_eq!(
                Forgejo::new(server, "o/r", TOKEN.to_owned(), &[]).is_ok(),
                ok,
                "{server}"
            );
        }
    }

    #[test]
    fn servers_with_http_bounds() {
        for (server, allowed, ok) in [
            ("http://127.0.0.1:3000", "", true),
            ("http://forgejo:3000", "forgejo:3000", true),
            ("http://FORGEJO:3000/git/", "forgejo:3000", true),
            ("http://forgejo", "forgejo", true),
            ("https://other.example", "", true),
            ("http://forgejo:3000", "", false),
            ("http://forgejo.evil", "forgejo", false),
            ("http://xforgejo", "forgejo", false),
            ("http://forgejo:3001", "forgejo:3000", false),
            ("http://forgejo:3000", "forgejo", false),
            ("http://forgejo@evil", "evil", false),
        ] {
            let hosts = if allowed.is_empty() {
                vec![]
            } else {
                vec![allowed.to_owned()]
            };
            let result = Forgejo::new(server, "o/r", TOKEN.to_owned(), &hosts);
            assert_eq!(result.is_ok(), ok, "{server} with {allowed:?}");
            if !ok && !server.contains('@') {
                assert!(result.err().unwrap().to_string().contains("allow_http"));
            }
        }
    }

    /// The message of the patch's commit, with trailers a merger reads.
    const MESSAGE: &str = "Change things\n\nWhy, at  length.\n\n\
        Generated-by: https://github.com/cgwalters/#llms\nCloses #5\n";

    pub(in crate::apply) fn sh(dir: &Path, args: &[&str]) -> String {
        String::from_utf8(git_out(dir, args))
            .unwrap()
            .trim()
            .to_owned()
    }

    fn git_out(dir: &Path, args: &[&str]) -> Vec<u8> {
        let output = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@example.org",
                "-c",
                "init.defaultBranch=main",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    }

    /// A bare repository with one commit on main, an empty bare fork of
    /// it, and the pull request of a patch on it adding `src/x` and a
    /// line to the README. Every line ends with EOL.
    fn repository_with(eol: &str) -> (tempfile::TempDir, PullRequest) {
        let root = tempfile::tempdir().unwrap();
        let work = root.path().join("work");
        std::fs::create_dir(&work).unwrap();
        sh(&work, &["init", "-q"]);
        std::fs::write(work.join("README"), format!("hi{eol}")).unwrap();
        sh(&work, &["add", "README"]);
        sh(&work, &["commit", "-q", "-m", "Start"]);
        let base = sh(&work, &["rev-parse", "HEAD"]);
        std::fs::write(work.join("README"), format!("hi{eol}more{eol}")).unwrap();
        std::fs::create_dir(work.join("src")).unwrap();
        std::fs::write(work.join("src/x"), format!("x{eol}")).unwrap();
        sh(&work, &["add", "README", "src/x"]);
        let message = root.path().join("message");
        std::fs::write(&message, MESSAGE).unwrap();
        sh(&work, &["commit", "-q", "-F", message.to_str().unwrap()]);
        let patch = sh(&work, &["format-patch", "-1", "--stdout"]) + "\n";
        sh(&work, &["reset", "-q", "--hard", "HEAD~"]);
        let bare = root.path().join("owner/name.git");
        std::fs::create_dir_all(&bare).unwrap();
        sh(&bare, &["init", "-q", "--bare"]);
        sh(
            &work,
            &["push", "-q", bare.to_str().unwrap(), "HEAD:refs/heads/main"],
        );
        let fork = root.path().join("fork/name.git");
        std::fs::create_dir_all(&fork).unwrap();
        sh(&fork, &["init", "-q", "--bare"]);
        let pr = PullRequest {
            base: "main".into(),
            base_commit: base,
            push_repo: "fork/name".into(),
            private: false,
            branch: "agent-run-7".into(),
            title: "WIP: Fix it".into(),
            body: "Why.".into(),
            patch: patch.into_bytes(),
            files: vec!["README".into(), "src/x".into()],
        };
        (root, pr)
    }

    pub(in crate::apply) fn repository() -> (tempfile::TempDir, PullRequest) {
        repository_with("\n")
    }

    #[test]
    fn opens_a_pull_request_from_a_fork() {
        let (root, pr) = repository();
        let mut answers = public().to_vec();
        answers.push((
            201,
            json!({"number": 9, "html_url": "https://forge.example/owner/name/pulls/9"})
                .to_string(),
        ));
        answers.extend(public());
        let (server, requests) = mock(answers);
        let mut forgejo = forgejo(&server);
        forgejo.git = root.path().display().to_string();
        let url = forgejo.open_pull_request(&pr).unwrap();
        assert_eq!(url, "https://forge.example/owner/name/pulls/9");
        for repo in ["owner/name", "fork/name"] {
            let request = requests.recv().unwrap();
            assert_eq!(request.line, format!("GET /api/v1/repos/{repo} HTTP/1.1"));
            assert_eq!(request.authorization, format!("token {TOKEN}"));
        }
        let request = requests.recv().unwrap();
        assert_eq!(request.line, "POST /api/v1/repos/owner/name/pulls HTTP/1.1");
        assert_eq!(request.authorization, format!("token {TOKEN}"));
        assert_eq!(
            request.body,
            json!({"title": "WIP: Fix it", "body": "Why.", "head": "fork:agent-run-7", "base": "main"})
        );
        // The branch is in the fork, and only there.
        assert_eq!(
            sh(
                &root.path().join("owner/name.git"),
                &["for-each-ref", "--format=%(refname)"]
            ),
            "refs/heads/main"
        );
        let bare = root.path().join("fork/name.git");
        assert_eq!(
            sh(&bare, &["rev-parse", "agent-run-7~"]),
            pr.base_commit,
            "one commit on the base"
        );
        assert_eq!(sh(&bare, &["show", "agent-run-7:src/x"]), "x");
        assert_eq!(
            sh(&bare, &["log", "-1", "--format=%an %cn", "agent-run-7"]),
            "t agentic-job"
        );
        // The commit's message is the patch's, byte for byte: the bytes
        // of the commit object after its headers.
        let commit = git_out(&bare, &["cat-file", "commit", "agent-run-7"]);
        let at = commit.windows(2).position(|w| w == b"\n\n").unwrap();
        assert_eq!(&commit[at + 2..], MESSAGE.as_bytes());

        // A second attempt finds the branch there, and opens nothing.
        let err = format!("{:#}", forgejo.open_pull_request(&pr).unwrap_err());
        assert_eq!(
            err,
            "pushing agent-run-7 to fork/name: refs/heads/agent-run-7 exists already"
        );
    }

    /// A push repository that is not the repository's fork, a fork that is
    /// not private when the repository or the run is not public, or one
    /// whose answer or the repository's does not say, gets no push.
    #[test]
    fn fork_identity_and_visibility() {
        let public = target(false, false, "public");
        let with = |mut answer: Value, key: &str, value: Value| {
            answer[key] = value;
            answer
        };
        let not_public = "fork/name is not private, and owner/name is not public";
        let not_fork = "fork/name is not a fork of owner/name";
        // The repository's answer, the fork's, whether the run is private,
        // and a part of the refusal.
        let cases: Vec<(Value, Value, bool, Option<&str>)> = vec![
            (public.clone(), fork(false), false, None),
            (public.clone(), fork(true), false, None),
            (target(true, false, "public"), fork(true), false, None),
            (target(false, true, "private"), fork(true), false, None),
            (public.clone(), fork(true), true, None),
            (
                target(true, false, "public"),
                fork(false),
                false,
                Some(not_public),
            ),
            (
                target(false, true, "private"),
                fork(false),
                false,
                Some(not_public),
            ),
            // A public repository of a limited organization.
            (
                target(false, false, "limited"),
                fork(false),
                false,
                Some(not_public),
            ),
            // A private run, whatever the forge says of the repository.
            (public.clone(), fork(false), true, Some(not_public)),
            (
                public.clone(),
                with(fork(false), "fork", json!(false)),
                false,
                Some(not_fork),
            ),
            (
                public.clone(),
                with(fork(false), "parent", json!({"full_name": "other/name"})),
                false,
                Some(not_fork),
            ),
            (
                public.clone(),
                with(fork(false), "full_name", json!("fork/other")),
                false,
                Some(not_fork),
            ),
            (
                with(public.clone(), "full_name", json!("owner/renamed")),
                fork(false),
                false,
                Some(not_fork),
            ),
            (
                public.clone(),
                with(fork(true), "private", json!("yes")),
                false,
                Some("GET /repos/fork/name: the response has no boolean \"private\""),
            ),
            (
                public.clone(),
                with(fork(false), "internal", Value::Null),
                false,
                Some("GET /repos/fork/name: the response has no boolean \"internal\""),
            ),
            (
                with(public.clone(), "owner", json!({})),
                fork(false),
                false,
                Some("GET /repos/owner/name: the response has no owner visibility"),
            ),
            (
                public.clone(),
                with(fork(false), "parent", Value::Null),
                false,
                Some("GET /repos/fork/name: the response has no parent"),
            ),
            (
                json!([]),
                fork(true),
                false,
                Some("the response has no boolean"),
            ),
        ];
        let created = (
            201,
            json!({"html_url": "https://forge.example/owner/name/pulls/9"}).to_string(),
        );
        for (target, fork, private, want) in cases {
            let (root, mut pr) = repository();
            pr.private = private;
            let (server, _requests) = mock(vec![ok(&target), ok(&fork), created.clone()]);
            let mut forgejo = forgejo(&server);
            forgejo.git = root.path().display().to_string();
            let pushed = || sh(&root.path().join("fork/name.git"), &["branch", "--list"]);
            match (forgejo.open_pull_request(&pr), want) {
                (Ok(_), None) => assert_eq!(pushed(), "agent-run-7"),
                (Err(err), Some(want)) => {
                    let err = format!("{err:#}");
                    assert!(err.contains(want), "{target} {fork}: {err}");
                    assert_eq!(pushed(), "", "{target} {fork}");
                }
                (result, _) => panic!("{target} {fork}: {result:?}"),
            }
        }
        // A repository the forge does not find gets no push either.
        let (root, pr) = repository();
        let (server, _requests) = mock(vec![(404, "{}".to_owned())]);
        let mut forgejo = forgejo(&server);
        forgejo.git = root.path().display().to_string();
        let err = format!("{:#}", forgejo.open_pull_request(&pr).unwrap_err());
        assert!(err.contains("GET /repos/owner/name: HTTP 404"), "{err}");
    }

    /// Neither the repository itself nor another repository of its owner,
    /// which Forgejo would read the pull request's head as, gets a push.
    #[test]
    fn never_pushes_to_the_repository_or_its_owner() {
        for push_repo in ["Owner/Name", "owner/other"] {
            let (root, mut pr) = repository();
            pr.push_repo = push_repo.into();
            let (server, requests) = mock(public().to_vec());
            let mut forgejo = forgejo(&server);
            forgejo.git = root.path().display().to_string();
            let err = format!("{:#}", forgejo.open_pull_request(&pr).unwrap_err());
            assert_eq!(
                err,
                format!("a pull request's branch goes to another owner's fork, not to {push_repo}")
            );
            assert!(requests.try_recv().is_err());
            assert_eq!(
                sh(
                    &root.path().join("owner/name.git"),
                    &["for-each-ref", "--format=%(refname)"]
                ),
                "refs/heads/main"
            );
        }
    }

    #[test]
    fn failed_pr_creation_can_recover_only_the_exact_pushed_commit() {
        let (root, pr) = repository();
        // Each attempt reads both visibilities first; the first fails to
        // open the pull request, three are refused, and the last opens it.
        let mut answers = public().to_vec();
        answers.push((500, format!("failure {TOKEN}")));
        for _ in 0..4 {
            answers.extend(public());
        }
        answers.push((
            201,
            json!({"html_url": "https://forge.example/owner/name/pulls/9"}).to_string(),
        ));
        let (server, requests) = mock(answers);
        let mut forgejo = forgejo(&server);
        forgejo.git = root.path().display().to_string();
        let bare = root.path().join("fork/name.git");
        let err = format!("{:#}", forgejo.open_pull_request(&pr).unwrap_err());
        let commit = sh(&bare, &["rev-parse", &pr.branch]);
        assert!(err.contains("to fork/name branch agent-run-7"), "{err}");
        assert!(
            err.contains(&format!("--recover-pushed-commit {commit}")),
            "{err}"
        );
        assert!(err.contains("HTTP 500"), "{err}");
        assert!(!err.contains(TOKEN), "{err}");
        for _ in 0..3 {
            requests.recv().unwrap();
        }

        forgejo.recover_pushed_commit = Some("0".repeat(40));
        assert!(
            format!("{:#}", forgejo.open_pull_request(&pr).unwrap_err())
                .contains("recovery requires")
        );
        forgejo.recover_pushed_commit = Some(commit.clone());
        let mut changed = pr.clone();
        changed.patch = String::from_utf8(changed.patch)
            .unwrap()
            .replace("+x\n", "+y\n")
            .into_bytes();
        assert!(
            format!("{:#}", forgejo.open_pull_request(&changed).unwrap_err())
                .contains("recovery requires")
        );
        // Even a matching checked patch cannot recover a moved branch.
        sh(
            &bare,
            &[
                "update-ref",
                &format!("refs/heads/{}", pr.branch),
                &pr.base_commit,
            ],
        );
        assert!(
            format!("{:#}", forgejo.open_pull_request(&pr).unwrap_err())
                .contains("recovery requires")
        );
        sh(
            &bare,
            &["update-ref", &format!("refs/heads/{}", pr.branch), &commit],
        );
        assert_eq!(
            forgejo.open_pull_request(&pr).unwrap(),
            "https://forge.example/owner/name/pulls/9"
        );
        let last = requests.iter().last().unwrap();
        assert_eq!(last.body["head"], "fork:agent-run-7");
        assert_eq!(sh(&bare, &["rev-parse", &pr.branch]), commit);
    }

    #[test]
    fn a_patch_that_changes_other_paths_is_not_pushed() {
        let (root, mut pr) = repository();
        let (server, _requests) = mock([public(), public()].concat());
        let mut forgejo = forgejo(&server);
        forgejo.git = root.path().display().to_string();
        pr.files = vec!["src/y".into()];
        let err = format!("{:#}", forgejo.open_pull_request(&pr).unwrap_err());
        assert!(err.contains("other paths than check read"), "{err}");
        pr.files = vec!["README".into(), "src/x".into()];
        pr.base_commit = "2".repeat(40);
        let err = format!("{:#}", forgejo.open_pull_request(&pr).unwrap_err());
        assert!(err.contains("is not on main"), "{err}");
        let bare = root.path().join("fork/name.git");
        assert_eq!(sh(&bare, &["branch", "--list", "agent-run-7"]), "");
    }

    /// A patch whose lines end in CRLF, which check admits, is pushed with
    /// those bytes: its edit of a CRLF file applies, and its new file is
    /// not committed with other line ends than check read.
    #[test]
    fn keeps_carriage_returns() {
        let (root, pr) = repository_with("\r\n");
        let mut answers = public().to_vec();
        answers.push((
            201,
            json!({"number": 9, "html_url": "https://forge.example/owner/name/pulls/9"})
                .to_string(),
        ));
        let (server, _requests) = mock(answers);
        let mut forgejo = forgejo(&server);
        forgejo.git = root.path().display().to_string();
        forgejo.open_pull_request(&pr).unwrap();
        let bare = root.path().join("fork/name.git");
        assert_eq!(
            git_out(&bare, &["show", "agent-run-7:README"]),
            b"hi\r\nmore\r\n"
        );
        assert_eq!(git_out(&bare, &["show", "agent-run-7:src/x"]), b"x\r\n");
    }
}
