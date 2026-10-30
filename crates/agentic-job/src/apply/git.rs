//! The push both publishers make: a checked patch applied on its base
//! commit, fetched from the repository, and pushed to a new branch of a
//! fork, or on a pull request's branch at the head the run was asked
//! for, with git given only the configuration set here. The token goes
//! in one header through git's environment, which other users cannot
//! read, never a URL or an argument, and is scrubbed from every error.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail, ensure};

/// How much of a refusal's text an error shows.
pub(super) const SHOWN_CHARS: usize = 300;

/// The committer of the pushed commit. The patch keeps its author, which
/// check holds to the bounds' `commit_author`; the committer names the
/// tool, at an address that cannot receive mail (RFC 2606).
const COMMITTER: (&str, &str) = ("agentic-job", "agentic-job@invalid");

/// Git's environment, which this sets afresh for each command, the
/// variables that could point it at another repository or configuration,
/// and those that would have it print the authorization header, which
/// only [`shown`] scrubs, and only in the forms [`Git::secrets`] names.
const GIT_ENV_REMOVED: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_CONFIG_PARAMETERS",
    "GIT_TRACE",
    "GIT_TRACE_CURL",
    "GIT_TRACE_REDACT",
    "GIT_CURL_VERBOSE",
];

/// TEXT as an error shows it: without any of SECRETS, replaced in their
/// order, then cut, so that no part of a secret is left where the cut
/// falls in it.
pub(super) fn shown(secrets: &[&str], text: &str) -> String {
    secrets
        .iter()
        .filter(|secret| !secret.is_empty())
        .fold(text.to_owned(), |text, secret| {
            text.replace(secret, "[token]")
        })
        .chars()
        .take(SHOWN_CHARS)
        .collect()
}

/// Git with a token.
pub(super) struct Git<'a> {
    /// The `Authorization` header git sends: the forge's own scheme.
    pub authorization: String,
    /// Scrubbed from whatever git says.
    pub token: &'a str,
}

/// One push of a checked patch.
pub(super) struct Push<'a> {
    /// What git fetches `base` from: the repository, never the fork, for
    /// a new branch; the fork, for its own branch.
    pub from: &'a str,
    pub base: &'a str,
    /// The commit check read the patch against, on `base`.
    pub base_commit: &'a str,
    /// The fork `branch` is pushed to.
    pub to: &'a str,
    pub branch: &'a str,
    pub patch: &'a [u8],
    /// The paths check read in the patch: the only ones it may change.
    pub files: &'a [String],
    /// What `branch` must hold in `to`, `base_commit` too: None for a new
    /// branch, which must not exist.
    pub head: Option<&'a str>,
    /// Who commits it, now; by default [`COMMITTER`] at the patch's
    /// author date, so that a retry makes the very same commit.
    pub committer: Option<(&'a str, &'a str)>,
    /// Commits as `committer` at the patch's author date too.
    pub at_author_date: bool,
    /// The most the changed files may hold once applied, in KB, as gh-aw's
    /// push handler counts them: [`TooLarge`] past it.
    pub max_changed_kb: Option<u64>,
}

/// The branch a push is for is no longer at its head: someone pushed to
/// it since, and nothing is pushed over that.
#[derive(Debug)]
pub(super) struct Moved {
    target: String,
    head: String,
}

impl std::fmt::Display for Moved {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} is no longer at {}, the head the run was asked for: it moved, and is not pushed over",
            self.target, self.head
        )
    }
}

impl std::error::Error for Moved {}

/// The files a patch changes hold more than [`Push::max_changed_kb`] once
/// applied, in gh-aw's words.
#[derive(Debug)]
pub(super) struct TooLarge {
    kb: u64,
    max_kb: u64,
}

impl std::fmt::Display for TooLarge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Changed content size ({} KB) exceeds maximum allowed size ({} KB)",
            self.kb, self.max_kb
        )
    }
}

impl std::error::Error for TooLarge {}

impl Git<'_> {
    /// What git's errors are scrubbed of, longest first: the whole header
    /// value, its credential (the base64 of a `Basic` one, which a server
    /// error may echo and anyone can decode), and the raw token.
    fn secrets(&self) -> [&str; 3] {
        let credential = self
            .authorization
            .split_once(' ')
            .map_or(self.authorization.as_str(), |(_, credential)| credential);
        [&self.authorization, credential, self.token]
    }

    /// Runs git in DIR with only the configuration given here: not the
    /// job's (a credential helper, a URL rewrite), no hooks, no redirect.
    pub fn run(&self, dir: &Path, args: &[&str], input: Option<&[u8]>) -> Result<String> {
        self.run_as(dir, args, input, COMMITTER)
    }

    /// [`Git::run`], committing as COMMITTER.
    fn run_as(
        &self,
        dir: &Path,
        args: &[&str],
        input: Option<&[u8]>,
        committer: (&str, &str),
    ) -> Result<String> {
        let config = [
            (
                "http.extraHeader",
                format!("Authorization: {}", self.authorization),
            ),
            ("http.followRedirects", "false".to_owned()),
            ("core.hooksPath", "/dev/null".to_owned()),
            ("user.name", committer.0.to_owned()),
            ("user.email", committer.1.to_owned()),
        ];
        let mut command = Command::new("git");
        command
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_CONFIG_COUNT", config.len().to_string())
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for name in GIT_ENV_REMOVED {
            command.env_remove(name);
        }
        for (i, (key, value)) in config.iter().enumerate() {
            command
                .env(format!("GIT_CONFIG_KEY_{i}"), key)
                .env(format!("GIT_CONFIG_VALUE_{i}"), value);
        }
        let mut child = command.spawn().context("starting git")?;
        // Written before the wait, and its failure (git stopped reading)
        // reported after it, so that git's own reason is the one shown.
        let written = match (input, child.stdin.take()) {
            (Some(input), Some(mut stdin)) => stdin.write_all(input),
            _ => Ok(()),
        };
        let output = child.wait_with_output().context("running git")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("git {}: {}", args[0], shown(&self.secrets(), stderr.trim()));
        }
        written.context("writing to git")?;
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// Applies the patch on its base commit, fetched from the repository,
    /// and pushes it to a branch of the fork that must not exist yet: a
    /// re-run, or anyone's branch of that name, is refused rather than
    /// overwritten. With RECOVER, a commit an earlier attempt pushed,
    /// pushes nothing and requires the branch to hold exactly that commit.
    /// With the push's `head`, the branch must hold exactly that, and gets
    /// one commit on it, under a lease on it: a branch that moved, by a
    /// re-run's push or anyone's, is refused, never forced.
    /// Returns the pushed commit.
    pub fn push(&self, push: &Push<'_>, recover: Option<&str>) -> Result<String> {
        if let Some(head) = push.head {
            ensure!(
                head == push.base_commit && recover.is_none(),
                "a push to an existing branch is on its head, and has no recovery"
            );
        }
        let dir = tempfile::tempdir().context("making a directory for the push")?;
        let dir = dir.path();
        self.run(dir, &["init", "-q"], None)?;
        self.run(
            dir,
            &[
                "fetch",
                "-q",
                "--no-tags",
                push.from,
                &format!("refs/heads/{}", push.base),
            ],
            None,
        )?;
        self.run(
            dir,
            &[
                "merge-base",
                "--is-ancestor",
                push.base_commit,
                "FETCH_HEAD",
            ],
            None,
        )
        .with_context(|| format!("{} is not on {}", push.base_commit, push.base))?;
        self.run(dir, &["checkout", "-q", "--detach", push.base_commit], None)?;
        // The commit's message is the patch's as `git am` reads it, and
        // nothing rewrites it: no hooks, and none of the job's
        // configuration (`am.messageid`, `mailinfo.scissors`) applies.
        // A retry must reconstruct exactly the same object, not a commit
        // with a new committer timestamp. Recovery checks its full SHA.
        // The carriage return check admits before a newline stays, as in
        // the GitHub apply job (`am.keepcr`): without it `git am` would
        // commit other bytes than check read, and a patch to a file with
        // CRLF line ends would not apply at all.
        let am = ["am", "-q", "--keep-cr", "--committer-date-is-author-date"];
        match push.committer {
            None => self.run(dir, &am, Some(push.patch))?,
            Some(committer) if push.at_author_date => {
                self.run_as(dir, &am, Some(push.patch), committer)?
            }
            Some(committer) => self.run_as(dir, &am[..3], Some(push.patch), committer)?,
        };
        // check read the paths the patch names; git applies by its own
        // reading, so what it changed is held to those.
        let changed = self.run(
            dir,
            &[
                "diff",
                "--no-renames",
                "--name-only",
                push.base_commit,
                "HEAD",
            ],
            None,
        )?;
        let mut changed: Vec<&str> = changed.lines().collect();
        let mut named: Vec<&str> = push.files.iter().map(String::as_str).collect();
        changed.sort_unstable();
        named.sort_unstable();
        ensure!(
            changed == named,
            "the applied patch changes other paths than check read in it"
        );
        if let Some(max_kb) = push.max_changed_kb {
            // A deleted file adds nothing, and no path leaves the checkout:
            // check refused `..` and absolute paths.
            let bytes: u64 = changed
                .iter()
                .filter_map(|path| std::fs::symlink_metadata(dir.join(path)).ok())
                .filter(std::fs::Metadata::is_file)
                .map(|metadata| metadata.len())
                .sum();
            let kb = bytes.div_ceil(1024);
            if kb > max_kb {
                bail!(TooLarge { kb, max_kb });
            }
        }
        let target = format!("refs/heads/{}", push.branch);
        let commit = self
            .run(dir, &["rev-parse", "HEAD"], None)?
            .trim()
            .to_owned();
        // The lease below refuses a branch that exists, but git reports a
        // push of the commit a branch already holds (a re-run's, made in
        // the same second) as up to date: asked first, too.
        let existing = self.run(dir, &["ls-remote", push.to, &target], None)?;
        if let Some(expected) = recover {
            ensure!(
                expected == commit && existing.trim() == format!("{commit}\t{target}"),
                "recovery requires {target} to hold the exact checked commit {commit}; no push or PR was made"
            );
            return Ok(commit);
        }
        match push.head {
            Some(head) => {
                if existing.trim() != format!("{head}\t{target}") {
                    bail!(Moved {
                        target,
                        head: head.to_owned()
                    });
                }
            }
            None => ensure!(existing.trim().is_empty(), "{target} exists already"),
        }
        // The lease is the head, or no branch: what was read, still there
        // as the push lands. The lease would allow a push that is not a
        // fast forward; the commit is one only as it was made on the head.
        self.run(
            dir,
            &[
                "push",
                "-q",
                &format!(
                    "--force-with-lease={target}:{}",
                    push.head.unwrap_or_default()
                ),
                push.to,
                &format!("HEAD:{target}"),
            ],
            None,
        )?;
        Ok(commit)
    }
}

#[cfg(test)]
mod tests {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD as BASE64;

    use super::*;

    /// Neither the raw token nor any header form of it, as each publisher
    /// sends it, survives an error, wherever the cut falls.
    #[test]
    fn shown_scrubs_the_authorization() {
        // Synthetic, built at run time so no token-shaped literal is kept.
        let token = ["ghs", "_", &"Ab9".repeat(12)].concat();
        let basic = BASE64.encode(format!("x-access-token:{token}"));
        let cases = [
            ("GitHub secondary", format!("Basic {basic}"), basic.clone()),
            ("Forgejo", format!("token {token}"), token.clone()),
        ];
        for (publisher, authorization, credential) in cases {
            let git = Git {
                authorization: authorization.clone(),
                token: &token,
            };
            let echoes = [
                format!("Authorization: {authorization}"),
                credential.clone(),
                token.clone(),
            ];
            for echo in &echoes {
                // Put the echo at every offset around the cut.
                for pad in SHOWN_CHARS.saturating_sub(echo.len() + 2)..SHOWN_CHARS + 2 {
                    let text = format!("{}{echo} 403", "x".repeat(pad));
                    let got = shown(&git.secrets(), &text);
                    assert!(got.chars().count() <= SHOWN_CHARS, "{publisher}");
                    // No piece long enough to identify the secret remains.
                    for secret in [token.as_str(), basic.as_str()] {
                        for window in secret.as_bytes().windows(8) {
                            let window = std::str::from_utf8(window).unwrap();
                            assert!(
                                !got.contains(window),
                                "{publisher}, pad {pad}: {window:?} in {got:?}"
                            );
                        }
                    }
                }
            }
        }
    }
}
