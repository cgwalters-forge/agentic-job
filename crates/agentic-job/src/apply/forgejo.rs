//! [`Publisher`] for Forgejo (and Gitea, whose API it keeps): comments,
//! issues, labels and closing through `/api/v1`, and a pull request from
//! a branch pushed over git to a fork the bounds name. Never to the
//! repository itself: there, its CI
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
use super::{Close, Kind, LabelChange, NewIssue, Op, Publisher, PullRequest};

/// The most of a response read: what a comment or pull request returns.
pub(super) const MAX_RESPONSE: u64 = 1 << 20;

pub(super) const TIMEOUT: Duration = Duration::from_secs(60);

const GET: &str = "GET";
const POST: &str = "POST";
const PATCH: &str = "PATCH";
const DELETE: &str = "DELETE";

/// Labels asked for a page, Forgejo's default most.
const LABELS_PER_PAGE: usize = 50;

/// The most pages of a repository's labels read: one with more than a
/// thousand is refused rather than read without end.
const MAX_LABEL_PAGES: usize = 20;

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

/// What the forge says of an issue or a pull request of the repository.
#[derive(Debug)]
struct Item {
    open: bool,
    html_url: String,
    labels: Vec<Label>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Label {
    id: u64,
    name: String,
    exclusive: bool,
}

impl Label {
    /// Its exclusive scope, as Forgejo's `Label.ExclusiveScope` has it: an
    /// item holds at most one label of a scope, and adding one removes
    /// the item's others.
    fn scope(&self) -> Option<&str> {
        let (scope, rest) = self.name.rsplit_once('/')?;
        (self.exclusive && !scope.is_empty() && !rest.is_empty()).then_some(scope)
    }
}

fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Issue => "an issue",
        Kind::PullRequest => "a pull request",
    }
}

/// The labels of ITEM that CHANGE removes: none for a name it lacks, and
/// a refusal for one that names two of its labels in different case.
///
/// Forgejo's DELETE takes a name, looked for among the repository's
/// labels, then the organization's, and only then taken for an ID, so a
/// label is removed by its exact name, and refused where that could name
/// another: an organization's label, a name of digits, or one another
/// repository label has in another case (as a database collation may
/// match it).
fn removals<'a>(
    item: &'a Item,
    repository: &[Label],
    change: &LabelChange,
) -> Result<Vec<&'a Label>> {
    let mut labels: Vec<&Label> = Vec::new();
    for name in &change.labels {
        let Some(label) = find_label(&item.labels, name) else {
            ensure!(
                !item
                    .labels
                    .iter()
                    .any(|label| label.name.eq_ignore_ascii_case(name)),
                "#{} has more than one label named {name:?} in another case",
                change.item
            );
            continue;
        };
        let named = &label.name;
        ensure!(
            repository.contains(label),
            "{named:?} on #{} is not a label of the repository; apply removes only those",
            change.item
        );
        ensure!(
            named.parse::<i64>().is_err(),
            "{named:?} is all digits, which Forgejo may take for another label's ID"
        );
        ensure!(
            !repository
                .iter()
                .any(|other| other.id != label.id
                    && other.name.to_lowercase() == named.to_lowercase()),
            "the repository has another label named {named:?} in another case"
        );
        if !labels.iter().any(|has| has.id == label.id) {
            labels.push(label);
        }
    }
    Ok(labels)
}

/// The labels of WANTED that ITEM, number NUMBER, lacks. Refused when
/// Forgejo would remove one of its others for an exclusive scope, which
/// no request named, or keep only one of two that share one.
fn additions(item: &Item, number: u64, wanted: Vec<Label>) -> Result<Vec<Label>> {
    exclusive(&wanted)?;
    let mut labels = Vec::new();
    for label in wanted {
        if item.labels.iter().any(|has| has.id == label.id) {
            continue;
        }
        if let Some(scope) = label.scope() {
            let replaced: Vec<&str> = item
                .labels
                .iter()
                .filter(|has| has.scope() == Some(scope))
                .map(|has| has.name.as_str())
                .collect();
            ensure!(
                replaced.is_empty(),
                "adding {:?} to #{number} would remove {}, of its exclusive scope {scope:?}",
                label.name,
                replaced.join(", ")
            );
        }
        labels.push(label);
    }
    Ok(labels)
}

/// Refuses LABELS when two share an exclusive scope: Forgejo would keep
/// only one.
fn exclusive(labels: &[Label]) -> Result<()> {
    for (i, label) in labels.iter().enumerate() {
        if let Some(scope) = label.scope() {
            let other = labels[i + 1..]
                .iter()
                .find(|other| other.scope() == Some(scope));
            if let Some(other) = other {
                bail!(
                    "labels {:?} and {:?} share the exclusive scope {scope:?}",
                    label.name,
                    other.name
                );
            }
        }
    }
    Ok(())
}

/// The labels of REPOSITORY NAMES name, once each. One it lacks is
/// refused: Forgejo would drop it, and apply creates none.
fn resolve(repo: &str, repository: &[Label], names: &[String]) -> Result<Vec<Label>> {
    let mut labels: Vec<Label> = Vec::new();
    let mut missing = Vec::new();
    for name in names {
        match find_label(repository, name) {
            Some(label) if labels.contains(label) => {}
            Some(label) => labels.push(label.clone()),
            None => missing.push(format!("{name:?}")),
        }
    }
    ensure!(
        missing.is_empty(),
        "{repo} has no label {}; apply creates none",
        missing.join(", ")
    );
    Ok(labels)
}

/// LABELS read out of an answer.
fn labels_of(labels: &Value) -> Option<Vec<Label>> {
    labels
        .as_array()?
        .iter()
        .map(|label| {
            Some(Label {
                id: label["id"].as_u64()?,
                name: label["name"].as_str()?.to_owned(),
                exclusive: label["exclusive"].as_bool()?,
            })
        })
        .collect()
}

/// The label of LABELS named NAME: exactly, or else in another case if
/// only one is.
fn find_label<'a>(labels: &'a [Label], name: &str) -> Option<&'a Label> {
    labels.iter().find(|label| label.name == name).or_else(|| {
        let mut alike = labels
            .iter()
            .filter(|label| label.name.eq_ignore_ascii_case(name));
        alike.next().filter(|_| alike.next().is_none())
    })
}

/// NAME as one path segment: every byte encoded, `/` and `%` included.
fn segment(name: &str) -> String {
    name.bytes().map(|b| format!("%{b:02X}")).collect()
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
        let method = if body.is_some() { POST } else { GET };
        self.request(method, path, body)
    }

    /// Sends METHOD, with BODY for a POST or a PATCH, to PATH under the
    /// API, and returns the JSON of a successful answer, null for none.
    fn request(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Value> {
        let url = format!("{}{path}", self.api);
        let authorization = format!("token {}", self.token);
        let json = body.map(Value::to_string).unwrap_or_default();
        let response = match method {
            POST => self
                .http
                .post(url)
                .header("authorization", authorization)
                .header("accept", "application/json")
                .header("content-type", "application/json")
                .send(json.as_bytes()),
            PATCH => self
                .http
                .patch(url)
                .header("authorization", authorization)
                .header("accept", "application/json")
                .header("content-type", "application/json")
                .send(json.as_bytes()),
            GET => self
                .http
                .get(url)
                .header("authorization", authorization)
                .header("accept", "application/json")
                .call(),
            DELETE => self
                .http
                .delete(url)
                .header("authorization", authorization)
                .header("accept", "application/json")
                .call(),
            _ => bail!("{method} {path}: not a request apply makes"),
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
        // A DELETE's 204.
        if text.is_empty() {
            return Ok(Value::Null);
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

    /// Issue or pull request NUMBER of the repository, refused unless it
    /// is of KIND, when given, and the forge's answer says it is that
    /// number of this repository: not one moved to another, which the
    /// forge would answer for under the old name.
    fn item(&self, number: u64, kind: Option<Kind>) -> Result<Item> {
        let path = format!("/repos/{}/issues/{number}", self.repo);
        let answer = self.send(&path, None)?;
        let missing = |what: &str| format!("GET {path}: the response has no {what}");
        ensure!(
            answer["number"].as_u64() == Some(number)
                && answer["repository"]["full_name"]
                    .as_str()
                    .is_some_and(|repo| repo.eq_ignore_ascii_case(&self.repo)),
            "#{number} is not an item of {}",
            self.repo
        );
        let found = match &answer["pull_request"] {
            Value::Null => Kind::Issue,
            Value::Object(_) => Kind::PullRequest,
            _ => bail!(missing("pull_request")),
        };
        if let Some(kind) = kind {
            ensure!(
                found == kind,
                "#{number} is {}, not {}",
                kind_name(found),
                kind_name(kind)
            );
        }
        Ok(Item {
            open: match answer["state"].as_str() {
                Some("open") => true,
                Some("closed") => false,
                _ => bail!(missing("state")),
            },
            html_url: answer["html_url"]
                .as_str()
                .map(str::to_owned)
                .with_context(|| missing("html_url"))?,
            labels: labels_of(&answer["labels"]).with_context(|| missing("labels"))?,
        })
    }

    /// The repository's labels, every page.
    fn repository_labels(&self) -> Result<Vec<Label>> {
        let mut labels = Vec::new();
        for page in 1..=MAX_LABEL_PAGES {
            let path = format!(
                "/repos/{}/labels?page={page}&limit={LABELS_PER_PAGE}",
                self.repo
            );
            let answer = self.send(&path, None)?;
            let found = labels_of(&answer)
                .with_context(|| format!("GET {path}: the response is not labels"))?;
            if found.is_empty() {
                return Ok(labels);
            }
            labels.extend(found);
        }
        bail!(
            "{} has more labels than apply reads ({})",
            self.repo,
            MAX_LABEL_PAGES * LABELS_PER_PAGE
        )
    }

    /// The IDs of NAMES among the repository's labels, for a new issue:
    /// see [`resolve`] and [`exclusive`].
    fn label_ids(&self, names: &[String]) -> Result<Vec<u64>> {
        if names.is_empty() {
            return Ok(Vec::new());
        }
        let labels = resolve(&self.repo, &self.repository_labels()?, names)?;
        exclusive(&labels)?;
        Ok(labels.iter().map(|label| label.id).collect())
    }

    /// The item CHANGE is to, and the labels it still adds to it or
    /// removes from it.
    fn label_change(&self, change: &LabelChange) -> Result<(Item, Vec<Label>)> {
        let item = self.item(change.item, None)?;
        let repository = self.repository_labels()?;
        let labels = if change.add {
            let wanted = resolve(&self.repo, &repository, &change.labels)?;
            additions(&item, change.item, wanted)?
        } else {
            removals(&item, &repository, change)?
                .into_iter()
                .cloned()
                .collect()
        };
        Ok((item, labels))
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
    fn preflight(&self, op: &Op) -> Result<()> {
        match op {
            // The push makes its own checks, and it is the first write.
            Op::Comment { .. } | Op::PullRequest(_) => {}
            Op::Issue(issue) => {
                self.label_ids(&issue.labels)?;
            }
            Op::Labels(change) => {
                self.label_change(change)?;
            }
            Op::Close(close) => {
                self.item(close.number, Some(close.kind))?;
            }
        }
        Ok(())
    }

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

    /// Forgejo leaves out the labels of a token that may not set them and
    /// still creates the issue, so the answer's labels are compared.
    fn create_issue(&self, issue: &NewIssue) -> Result<String> {
        let ids = self.label_ids(&issue.labels)?;
        let path = format!("/repos/{}/issues", self.repo);
        let answer = self.send(
            &path,
            Some(&json!({"title": issue.title, "body": issue.body, "labels": ids})),
        )?;
        let url = answer["html_url"]
            .as_str()
            .with_context(|| format!("POST {path}: the response names no html_url"))?;
        let labelled = labels_of(&answer["labels"]).unwrap_or_default();
        ensure!(
            ids.iter()
                .all(|id| labelled.iter().any(|has| has.id == *id)),
            "created {url} without all of its labels {}",
            issue.labels.join(", ")
        );
        Ok(url.to_owned())
    }

    /// Labels are compared with the item's own, so that a re-run writes
    /// only what is still to change. One to remove is looked for there,
    /// and is no change if absent.
    fn change_labels(&self, change: &LabelChange) -> Result<String> {
        let (item, labels) = self.label_change(change)?;
        let path = format!("/repos/{}/issues/{}/labels", self.repo, change.item);
        if change.add {
            if !labels.is_empty() {
                let ids: Vec<u64> = labels.iter().map(|label| label.id).collect();
                self.send(&path, Some(&json!({"labels": ids})))?;
            }
            return Ok(item.html_url);
        }
        let mut removed: Vec<&str> = Vec::new();
        for label in &labels {
            let deleting = self.request(DELETE, &format!("{path}/{}", segment(&label.name)), None);
            match deleting {
                Ok(_) => removed.push(&label.name),
                Err(err) if removed.is_empty() => return Err(err),
                Err(err) => {
                    return Err(err.context(format!(
                        "after removing {} from #{}",
                        removed.join(", "),
                        change.item
                    )));
                }
            }
        }
        Ok(item.html_url)
    }

    /// Forgejo has no closing comment, so the comment goes first: a
    /// failure in between leaves the item open with the reason on it, and
    /// a re-run, without a marker to find it by, comments again.
    fn close(&self, close: &Close) -> Result<String> {
        let item = self.item(close.number, Some(close.kind))?;
        if !item.open {
            return Ok(item.html_url);
        }
        let comment = self.comment(close.number, &close.body)?;
        let path = match close.kind {
            Kind::Issue => format!("/repos/{}/issues/{}", self.repo, close.number),
            Kind::PullRequest => format!("/repos/{}/pulls/{}", self.repo, close.number),
        };
        self.request(PATCH, &path, Some(&json!({"state": "closed"})))
            .with_context(|| format!("after commenting {comment}"))?;
        Ok(item.html_url)
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

    /// REQUEST's method and path under the API.
    fn route(request: &Request) -> String {
        request
            .line
            .replacen(" /api/v1/", " /", 1)
            .trim_end_matches(" HTTP/1.1")
            .to_owned()
    }

    const ITEM: &str = "GET /repos/owner/name/issues/5";
    const ITEM_URL: &str = "https://forge.example/owner/name/issues/5";
    const COMMENT: &str = "POST /repos/owner/name/issues/5/comments";
    const COMMENT_URL: &str = "https://forge.example/owner/name/issues/5#issuecomment-3";
    const CREATE: &str = "POST /repos/owner/name/issues";
    const CREATED_URL: &str = "https://forge.example/owner/name/issues/8";
    const ADD: &str = "POST /repos/owner/name/issues/5/labels";
    /// By name, every byte encoded: `bug` and `status/triaged`.
    const REMOVE_BUG: &str = "DELETE /repos/owner/name/issues/5/labels/%62%75%67";
    const REMOVE_TRIAGED: &str =
        "DELETE /repos/owner/name/issues/5/labels/%73%74%61%74%75%73%2F%74%72%69%61%67%65%64";
    const CLOSE_ISSUE: &str = "PATCH /repos/owner/name/issues/5";
    const CLOSE_PULL: &str = "PATCH /repos/owner/name/pulls/5";

    /// Label ID named NAME as the forge answers for it: exclusive in the
    /// scope `status`.
    fn label(id: u64, name: &str) -> Value {
        json!({"id": id, "name": name, "exclusive": name.starts_with("status/")})
    }

    /// A forge whose repository has the labels triage, bug, Docs, docs,
    /// status/triaged, status/protected and 2, and item 5 ITEM, if any.
    /// It takes every write but those in FAILING, which it fails with the
    /// token in its answer.
    fn forge_of(
        item: Option<Value>,
        failing: &'static [&'static str],
    ) -> (String, mpsc::Receiver<Request>) {
        serve(64, move |request| {
            let line = route(request);
            if failing.contains(&line.as_str()) {
                let answer = json!({"message": format!("failure {TOKEN}")});
                return (500, answer.to_string());
            }
            let answer = match line.as_str() {
                "GET /repos/owner/name/labels?page=1&limit=50" => json!([
                    label(1, "triage"),
                    label(2, "bug"),
                    label(3, "Docs"),
                    label(4, "docs"),
                    label(5, "status/triaged"),
                    label(6, "status/protected"),
                    label(7, "2"),
                ]),
                "GET /repos/owner/name/labels?page=2&limit=50" => json!([]),
                ITEM if item.is_some() => return ok(item.as_ref().unwrap()),
                COMMENT => json!({"html_url": COMMENT_URL}),
                // As for a token that may set only some labels.
                CREATE => json!({"html_url": CREATED_URL, "labels": [label(2, "bug")]}),
                line if line.starts_with("GET ") => return (404, "{}".to_owned()),
                line if line.starts_with("DELETE ") => return (204, String::new()),
                _ => json!({}),
            };
            (201, answer.to_string())
        })
    }

    /// Item 5 of the repository as the forge answers for it: an issue or
    /// a pull request, open or not, with LABELS.
    fn item(pull_request: bool, open: bool, labels: &[(u64, &str)]) -> Value {
        json!({
            "number": 5, "state": if open { "open" } else { "closed" },
            "pull_request": if pull_request { json!({"merged": false}) } else { Value::Null },
            "html_url": "https://forge.example/owner/name/issues/5",
            "repository": {"full_name": "owner/name"},
            "labels": labels.iter().map(|(id, name)| label(*id, name)).collect::<Vec<_>>(),
        })
    }

    /// Each op as the forge sees it, and every write it makes: success,
    /// a re-run that finds it done, refusals before any write, and a
    /// failure after one.
    #[test]
    fn metadata() {
        let labels = |add: bool, names: &[&str]| {
            Op::Labels(LabelChange {
                item: 5,
                add,
                labels: names.iter().map(|name| (*name).to_owned()).collect(),
            })
        };
        let close = |kind| {
            Op::Close(Close {
                number: 5,
                kind,
                body: "Done.".into(),
            })
        };
        let issue = |names: &[&str]| {
            Op::Issue(NewIssue {
                title: "Flaky".into(),
                body: "It fails.".into(),
                labels: names.iter().map(|name| (*name).to_owned()).collect(),
            })
        };
        let open = || Some(item(false, true, &[]));
        let commented = || (COMMENT, json!({"body": "Done."}));
        let closed = |route| (route, json!({"state": "closed"}));
        // The ops, item 5, the writes that fail, the writes made, and the
        // URL or a part of the error.
        type Case = (
            Vec<Op>,
            Option<Value>,
            &'static [&'static str],
            Vec<(&'static str, Value)>,
            Result<&'static str, &'static str>,
        );
        let cases: Vec<Case> = vec![
            (
                vec![labels(true, &["triage", "BUG"])],
                open(),
                &[],
                vec![(ADD, json!({"labels": [1, 2]}))],
                Ok(ITEM_URL),
            ),
            // A re-run: only what is still missing, and then nothing.
            (
                vec![labels(true, &["triage", "bug"])],
                Some(item(false, true, &[(1, "triage")])),
                &[],
                vec![(ADD, json!({"labels": [2]}))],
                Ok(ITEM_URL),
            ),
            (
                vec![labels(true, &["triage"])],
                Some(item(true, true, &[(1, "triage")])),
                &[],
                vec![],
                Ok(ITEM_URL),
            ),
            // Refused before any write: a label the repository lacks, or
            // two of whose names differ only in case.
            (
                vec![close(Kind::Issue), labels(true, &["triage", "wontfix"])],
                open(),
                &[],
                vec![],
                Err("owner/name has no label \"wontfix\"; apply creates none"),
            ),
            (
                vec![labels(true, &["DOCS"])],
                open(),
                &[],
                vec![],
                Err("has no label \"DOCS\""),
            ),
            // Names of other labels in another case are each their own.
            (
                vec![labels(true, &["Docs", "docs"])],
                open(),
                &[],
                vec![(ADD, json!({"labels": [3, 4]}))],
                Ok(ITEM_URL),
            ),
            // Forgejo would remove status/protected for status/triaged, or
            // keep only one of two in a scope.
            (
                vec![labels(true, &["status/triaged"])],
                Some(item(false, true, &[(6, "status/protected")])),
                &[],
                vec![],
                Err(
                    "adding \"status/triaged\" to #5 would remove status/protected, of its exclusive scope \"status\"",
                ),
            ),
            (
                vec![labels(true, &["status/triaged", "bug"])],
                Some(item(false, true, &[(5, "status/triaged")])),
                &[],
                vec![(ADD, json!({"labels": [2]}))],
                Ok(ITEM_URL),
            ),
            (
                vec![labels(true, &["status/triaged", "status/protected"])],
                open(),
                &[],
                vec![],
                Err("labels \"status/triaged\" and \"status/protected\" share the exclusive scope"),
            ),
            (
                vec![issue(&["status/triaged", "status/protected"])],
                None,
                &[],
                vec![],
                Err("share the exclusive scope \"status\""),
            ),
            // Removed by name, and refused where the name could resolve
            // to another label.
            (
                vec![labels(false, &["triage", "bug", "status/triaged"])],
                Some(item(false, true, &[(2, "bug"), (5, "status/triaged")])),
                &[],
                vec![(REMOVE_BUG, Value::Null), (REMOVE_TRIAGED, Value::Null)],
                Ok(ITEM_URL),
            ),
            (
                vec![labels(false, &["bug", "2"])],
                Some(item(false, true, &[(2, "bug"), (7, "2")])),
                &[],
                vec![],
                Err("\"2\" is all digits, which Forgejo may take for another label's ID"),
            ),
            (
                vec![labels(false, &["docs"])],
                Some(item(false, true, &[(4, "docs")])),
                &[],
                vec![],
                Err("the repository has another label named \"docs\" in another case"),
            ),
            (
                vec![labels(false, &["team"])],
                Some(item(false, true, &[(9, "team")])),
                &[],
                vec![],
                Err("\"team\" on #5 is not a label of the repository"),
            ),
            (
                vec![labels(false, &["triage"])],
                open(),
                &[],
                vec![],
                Ok(ITEM_URL),
            ),
            (
                vec![labels(false, &["bug", "DOCS"])],
                Some(item(false, true, &[(2, "bug"), (3, "Docs"), (4, "docs")])),
                &[],
                vec![],
                Err("#5 has more than one label named \"DOCS\" in another case"),
            ),
            (
                vec![labels(false, &["bug", "status/triaged"])],
                Some(item(false, true, &[(2, "bug"), (5, "status/triaged")])),
                &[REMOVE_TRIAGED],
                vec![(REMOVE_BUG, Value::Null), (REMOVE_TRIAGED, Value::Null)],
                Err("after removing bug from #5: DELETE /repos/owner/name/issues/5/labels/%73"),
            ),
            (
                vec![close(Kind::Issue)],
                open(),
                &[],
                vec![commented(), closed(CLOSE_ISSUE)],
                Ok(ITEM_URL),
            ),
            (
                vec![close(Kind::PullRequest)],
                Some(item(true, true, &[])),
                &[],
                vec![commented(), closed(CLOSE_PULL)],
                Ok(ITEM_URL),
            ),
            // A re-run finds it closed.
            (
                vec![close(Kind::Issue)],
                Some(item(false, false, &[])),
                &[],
                vec![],
                Ok(ITEM_URL),
            ),
            (
                vec![close(Kind::Issue)],
                open(),
                &[CLOSE_ISSUE],
                vec![commented(), closed(CLOSE_ISSUE)],
                Err(
                    "after commenting https://forge.example/owner/name/issues/5#issuecomment-3: PATCH",
                ),
            ),
            (
                vec![close(Kind::Issue)],
                open(),
                &[COMMENT],
                vec![commented()],
                Err("POST /repos/owner/name/issues/5/comments: HTTP 500"),
            ),
            // Another kind, another repository's, or none at all.
            (
                vec![close(Kind::Issue)],
                Some(item(true, true, &[])),
                &[],
                vec![],
                Err("#5 is a pull request, not an issue"),
            ),
            (
                vec![close(Kind::PullRequest)],
                open(),
                &[],
                vec![],
                Err("#5 is an issue, not a pull request"),
            ),
            (
                vec![close(Kind::Issue)],
                open().map(|mut moved| {
                    moved["repository"]["full_name"] = json!("other/name");
                    moved
                }),
                &[],
                vec![],
                Err("#5 is not an item of owner/name"),
            ),
            (
                vec![labels(true, &["bug"])],
                open().map(|mut other| {
                    other["number"] = json!(6);
                    other
                }),
                &[],
                vec![],
                Err("#5 is not an item of owner/name"),
            ),
            (
                vec![close(Kind::Issue)],
                None,
                &[],
                vec![],
                Err("GET /repos/owner/name/issues/5: HTTP 404"),
            ),
            (
                vec![issue(&["bug"])],
                None,
                &[],
                vec![(
                    CREATE,
                    json!({"title": "Flaky", "body": "It fails.", "labels": [2]}),
                )],
                Ok(CREATED_URL),
            ),
            (
                vec![issue(&["triage"])],
                None,
                &[],
                vec![(
                    CREATE,
                    json!({"title": "Flaky", "body": "It fails.", "labels": [1]}),
                )],
                Err(
                    "created https://forge.example/owner/name/issues/8 without all of its labels triage",
                ),
            ),
            (
                vec![issue(&["bug", "release"])],
                None,
                &[],
                vec![],
                Err("owner/name has no label \"release\""),
            ),
        ];
        for (ops, item, failing, writes, want) in cases {
            let (server, requests) = forge_of(item, failing);
            let result = super::super::publish(&ops, &forgejo(&server));
            let made: Vec<(String, Value)> = requests
                .try_iter()
                .filter(|request| !request.line.starts_with("GET "))
                .map(|request| {
                    assert_eq!(request.authorization, format!("token {TOKEN}"));
                    (route(&request), request.body)
                })
                .collect();
            let writes: Vec<(String, Value)> = writes
                .into_iter()
                .map(|(line, body)| (line.to_owned(), body))
                .collect();
            assert_eq!(made, writes, "{ops:?}");
            match (result, want) {
                (Ok(urls), Ok(want)) => assert_eq!(urls, [want], "{ops:?}"),
                (Err(err), Err(want)) => {
                    let err = format!("{err:#}");
                    assert!(err.contains(want), "{ops:?}: {err}");
                    assert!(!err.contains(&TOKEN[..4]), "{err}");
                }
                (result, _) => panic!("{ops:?}: {result:?}"),
            }
        }
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
