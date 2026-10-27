//! GitHub's REST API and git, with one token: what the GitHub commands
//! here ([`super::secondary`], [`super::push`], [`super::status`]) ask
//! the forge, in place of apply.yml's github-script steps.

use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde_json::{Value, json};

use super::git::{self, Git};
use super::owner;

const API_URL_VAR: &str = "GITHUB_API_URL";

const DEFAULT_API: &str = "https://api.github.com";

/// The environment variable a workflow step gives the token in, as `gh`
/// reads it.
pub const GH_TOKEN_ENV: &str = "GH_TOKEN";

/// How often, and how far apart, git is asked whether a fork is there:
/// the forge makes it in the background.
pub(super) const FORK_TRIES: u32 = 30;

const FORK_WAIT: Duration = Duration::from_secs(10);

/// Whether A and B name the same repository or login: GitHub's names
/// ignore case.
pub(super) fn same(a: &Value, b: &str) -> bool {
    a.as_str().is_some_and(|a| a.eq_ignore_ascii_case(b))
}

/// Whether NAME is `owner/repo` as GitHub spells one, and so a path of
/// two segments under `/repos/`.
pub(crate) fn is_repo(name: &str) -> bool {
    let part = |part: &str| {
        !part.is_empty()
            && part
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
            && part != "."
            && part != ".."
    };
    name.split_once('/')
        .is_some_and(|(owner, repo)| part(owner) && part(repo))
}

/// TEXT as one path segment of a URL, `/` kept: a branch name the forge
/// gave, which may hold `#`, `%` or `?`.
pub(super) fn segment(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                char::from(b).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// GitHub's REST API and git, with one token.
pub struct Github {
    api: String,
    token: String,
    http: ureq::Agent,
    /// The most an answer may hold.
    max_response: u64,
    /// Between asking git whether the fork is there.
    pub(super) fork_wait: Duration,
}

impl Github {
    /// API is GitHub's REST API, `https://api.github.com` or a server's.
    pub fn new(api: &str, token: String) -> Result<Self> {
        let api = api.trim_end_matches('/');
        ensure!(
            api.starts_with("https://") || is_loopback_http(api),
            "{API_URL_VAR} requires HTTPS"
        );
        let http = ureq::Agent::config_builder()
            .timeout_global(Some(super::forgejo::TIMEOUT))
            .proxy(None)
            // The token is not to follow a redirect anywhere.
            .max_redirects(0)
            .http_status_as_error(false)
            .build()
            .into();
        Ok(Self {
            api: api.to_owned(),
            token,
            http,
            max_response: super::forgejo::MAX_RESPONSE,
            fork_wait: FORK_WAIT,
        })
    }

    /// The API the runner names (`GITHUB_API_URL`), with the token in the
    /// environment variable VAR, which must not be empty.
    pub fn from_env(var: &str) -> Result<Self> {
        let token = std::env::var(var).unwrap_or_default();
        ensure!(!token.is_empty(), "{var} is not set");
        let api = std::env::var(API_URL_VAR).unwrap_or_else(|_| DEFAULT_API.to_owned());
        Self::new(&api, token)
    }

    /// Takes answers of up to MAX bytes.
    pub fn with_max_response(mut self, max: u64) -> Self {
        self.max_response = max;
        self
    }

    /// Sends METHOD with BODY to PATH under the API, and returns the
    /// answer's status and text.
    fn call(&self, method: &str, path: &str, body: Option<&Value>) -> Result<(u16, String)> {
        let url = format!("{}{path}", self.api);
        let authorization = format!("Bearer {}", self.token);
        let request = |builder: ureq::RequestBuilder<ureq::typestate::WithBody>| {
            builder
                .header("authorization", &authorization)
                .header("accept", "application/vnd.github+json")
                .header("user-agent", "agentic-job")
                .header("content-type", "application/json")
                .send(body.map(Value::to_string).unwrap_or_default().as_bytes())
        };
        let response = match method {
            "GET" => self
                .http
                .get(&url)
                .header("authorization", &authorization)
                .header("accept", "application/vnd.github+json")
                .header("user-agent", "agentic-job")
                .call(),
            "POST" => request(self.http.post(&url)),
            "PUT" => request(self.http.put(&url)),
            "PATCH" => request(self.http.patch(&url)),
            _ => bail!("{method} is not a method this sends"),
        };
        let mut response = match response {
            Ok(response) => response,
            Err(err) => bail!("{method} {path}: {}", self.shown(&err.to_string())),
        };
        let status = response.status().as_u16();
        match response
            .body_mut()
            .with_config()
            .limit(self.max_response)
            .read_to_string()
        {
            Ok(text) => Ok((status, text)),
            Err(err) => bail!("{method} {path}: {}", self.shown(&err.to_string())),
        }
    }

    /// Sends METHOD with BODY to PATH under the API, and returns the JSON
    /// of a successful answer (null for none).
    pub(super) fn send(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Value> {
        let (status, text) = self.call(method, path, body)?;
        ensure!(
            (200..300).contains(&status),
            "{method} {path}: HTTP {status}: {}",
            self.shown(&text)
        );
        if text.trim().is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&text)
            .with_context(|| format!("{method} {path}: the response is not JSON"))
    }

    /// GETs PATH, and returns the JSON of a successful answer.
    pub(crate) fn get(&self, path: &str) -> Result<Value> {
        self.send("GET", path, None)
    }

    /// GETs PATH, or None where the forge answers that there is nothing
    /// there (404). Any other failure is an error.
    pub(super) fn get_optional(&self, path: &str) -> Result<Option<Value>> {
        let (status, text) = self.call("GET", path, None)?;
        if status == 404 {
            return Ok(None);
        }
        ensure!(
            (200..300).contains(&status),
            "GET {path}: HTTP {status}: {}",
            self.shown(&text)
        );
        serde_json::from_str(&text)
            .map(Some)
            .with_context(|| format!("GET {path}: the response is not JSON"))
    }

    pub(super) fn shown(&self, text: &str) -> String {
        git::shown(&[&self.token], text)
    }

    /// Git with the token, as GitHub's git takes it.
    pub(super) fn git(&self) -> Git<'_> {
        let basic = BASE64.encode(format!("x-access-token:{}", self.token));
        Git {
            authorization: format!("Basic {basic}"),
            token: &self.token,
        }
    }

    /// The user the token is of, as `GET /user` names it: the identity
    /// whose fork apply pushes to. The job token names nobody.
    pub(super) fn login(&self) -> Result<String> {
        self.send("GET", "/user", None)?["login"]
            .as_str()
            .filter(|login| !login.is_empty())
            .map(str::to_owned)
            .context("GET /user names nobody: SAFE_OUTPUTS_PAT must be a user's token")
    }

    /// ME's own fork of REPO, made or found, with Actions off, that git
    /// reaches at its name under SERVER. Its CI is what a push with a
    /// user's token would start, with the fork's secrets: nothing is
    /// pushed unless the forge says Actions are off there. The repository
    /// itself, or another of its owner's, is never pushed to: there its
    /// CI would run the agent's code with the repository's secrets. Nor
    /// is OWN_REPO, the run's own, should it be ME's fork of REPO: its
    /// Actions are its own CI.
    pub(super) fn fork(
        &self,
        me: &str,
        repo: &str,
        own_repo: &str,
        server: &str,
    ) -> Result<String> {
        ensure!(
            !me.eq_ignore_ascii_case(owner(repo)),
            "{me} owns {repo} and has no fork of it to push to"
        );
        let fork = self.send(
            "POST",
            &format!("/repos/{repo}/forks"),
            Some(&json!({"default_branch_only": true})),
        )?;
        let name = fork["full_name"].as_str().unwrap_or_default().to_owned();
        ensure!(
            fork["fork"] == Value::Bool(true)
                && same(&fork["owner"]["login"], me)
                && owner(&name).eq_ignore_ascii_case(me)
                && same(&fork["parent"]["full_name"], repo),
            "the forge answered a fork of {repo} for {me} with {name:?}, which is not {me}'s own fork of it"
        );
        ensure!(
            !name.eq_ignore_ascii_case(own_repo),
            "{me}'s fork of {repo} is {own_repo}, the run's own repository, which this does not \
             push to"
        );
        let remote = format!("{server}/{name}.git");
        let dir = tempfile::tempdir().context("making a directory for git")?;
        let git = self.git();
        for attempt in 1.. {
            let heads = git.run(dir.path(), &["ls-remote", "--heads", &remote], None);
            if heads.is_ok_and(|heads| heads.contains("\trefs/heads/")) {
                break;
            }
            ensure!(
                attempt < FORK_TRIES,
                "the fork {name} is still not there after {attempt} tries"
            );
            std::thread::sleep(self.fork_wait);
        }
        let permissions = format!("/repos/{name}/actions/permissions");
        let enabled = self
            .send("PUT", &permissions, Some(&json!({"enabled": false})))
            .and_then(|_| self.send("GET", &permissions, None))
            .map(|answer| answer["enabled"].as_bool());
        if !matches!(enabled, Ok(Some(false))) {
            bail!(
                "Actions could not be turned off on the fork {name}, which a push with {me}'s \
                 token would run workflows in: turn them off there, and give SAFE_OUTPUTS_PAT \
                 the repo scope (or a fine-grained token Administration: write on the fork)"
            );
        }
        Ok(name)
    }
}

/// A plain HTTP address on a literal loopback address: the tests' forge.
fn is_loopback_http(api: &str) -> bool {
    api.strip_prefix("http://")
        .and_then(|rest| rest.split('/').next())
        .and_then(|authority| authority.parse::<std::net::SocketAddr>().ok())
        .is_some_and(|address| address.ip().is_loopback())
}

#[cfg(test)]
pub(super) mod tests {
    use std::collections::BTreeMap;
    use std::sync::mpsc;

    use super::super::forgejo::tests::{Request, serve};
    use super::*;

    pub(in crate::apply) const TOKEN: &str = "test-token-not-secret";

    /// A forge that answers `METHOD PATH` with the status and JSON ROUTES
    /// give it, and 404 for anything else, and a client of it whose fork
    /// wait takes no time.
    pub(in crate::apply) fn forge(
        routes: BTreeMap<String, (u16, Value)>,
    ) -> (Github, mpsc::Receiver<Request>) {
        let (server, requests) = serve(usize::MAX, move |request| {
            let asked = request.line.trim_end_matches(" HTTP/1.1");
            let (status, body) = routes.get(asked).cloned().unwrap_or((404, json!({})));
            (status, body.to_string())
        });
        let mut github = Github::new(&server, TOKEN.to_owned()).unwrap();
        github.fork_wait = Duration::ZERO;
        (github, requests)
    }

    #[test]
    fn names() {
        for (name, repo) in [
            ("o/r", true),
            ("O-1/r.x_y", true),
            ("o", false),
            ("o/r/x", false),
            ("/r", false),
            ("o/..", false),
            ("o/r?x", false),
            ("o/r\n", false),
        ] {
            assert_eq!(is_repo(name), repo, "{name:?}");
        }
        assert_eq!(segment("fix/a b#%?"), "fix/a%20b%23%25%3F");
    }
}
