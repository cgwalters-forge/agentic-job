//! The target repository, cloned as the sandbox user from the URL that
//! `policy` recorded for the run. The binary builds no forge URL: what
//! may be cloned is the caller's bounds file's to say, and `policy`
//! checked the URL against it before any machine started.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};

use super::enter::{Sandbox, one_line};
use crate::policy::Policy;

/// Under the sandbox user's home: the checkouts, and what the agent
/// hands back.
pub const WORK_DIR: &str = "work";
pub const OUT_DIR: &str = "out";
/// How much history the agent gets, as in the old tree.
const DEPTH: &str = "50";
/// How long a clone may take, in seconds: `timeout` runs in the sandbox
/// with it, so a stalled transfer does not hold the machine until the
/// job's own limit.
const CLONE_TIMEOUT_S: &str = "900";
const GIT_TIMEOUT_S: &str = "120";
/// The transports a clone may use. `https` is what a caller's bounds
/// admit; `file` reads only what the sandbox user can, and is for tests
/// and a mirror on the machine. Anything else git knows (`ext::`, which
/// runs a command, among them) is refused by git itself with these.
const TRANSPORTS: &[&str] = &["https", "file"];
const MAX_REF_LEN: usize = 200;
/// What a commit id and one line of `git log` fit in.
const MAX_GIT_OUTPUT: usize = 4096;

fn name_chars(text: &str, also: &[u8]) -> bool {
    text.bytes()
        .all(|b| b.is_ascii_alphanumeric() || also.contains(&b))
}

/// Whether NAME can be given to git as a branch or tag.
pub fn check_ref(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty()
            && name.len() <= MAX_REF_LEN
            && name_chars(name, b"_./-")
            && !name.starts_with(['-', '.', '/'])
            && !name.contains(".."),
        "{name:?} is not a branch or tag name of letters, digits and _ . / -"
    );
    Ok(())
}

/// Whether URL is one a clone may use.
pub fn check_url(url: &str) -> Result<()> {
    let transport = url.split_once("://").map(|(scheme, _)| scheme);
    ensure!(
        transport.is_some_and(|scheme| TRANSPORTS.contains(&scheme))
            && !url.contains(|c: char| c.is_whitespace() || c.is_control()),
        "{url:?} is not an {} URL",
        TRANSPORTS.join(" or ")
    );
    Ok(())
}

/// The repository's own name, which the checkout is called.
pub fn checkout_name(policy: &Policy) -> &str {
    policy.repo.rsplit('/').next().unwrap_or_default()
}

/// Whether `run` can clone what POLICY names. `policy` checked all of it
/// against the caller's bounds; it is checked again for what the values
/// are used as here, a directory name and arguments of git, since
/// `policy.json` is only a file by the time `run` reads it.
pub fn check(policy: &Policy) -> Result<()> {
    let name = checkout_name(policy);
    ensure!(
        !name.is_empty() && name_chars(name, b"_.-") && !name.bytes().all(|b| b == b'.'),
        "repo: {:?} does not end in a repository name",
        policy.repo
    );
    check_ref(&policy.base).context("base")?;
    check_url(&policy.clone_url).context("clone_url")
}

/// The clone the agent works in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checkout {
    pub dir: PathBuf,
    /// The commit the agent started from, which the hand-back's patch is
    /// against.
    pub base_commit: String,
}

/// The arguments of a clone of URL into DIR. Only the listed transports,
/// so that neither the URL nor a redirect can make git run something
/// else.
fn clone_args<'a>(
    url: &'a str,
    branch: Option<&'a str>,
    depth: &'a str,
    dir: &'a str,
) -> Vec<&'a str> {
    let mut argv = vec![
        "timeout",
        CLONE_TIMEOUT_S,
        "git",
        "-c",
        "protocol.allow=never",
    ];
    argv.extend(TRANSPORTS.iter().flat_map(|transport| {
        let allow: &'static str = match *transport {
            "https" => "protocol.https.allow=always",
            _ => "protocol.file.allow=always",
        };
        ["-c", allow]
    }));
    argv.extend([
        "clone",
        "--quiet",
        "--no-recurse-submodules",
        "--depth",
        depth,
    ]);
    argv.extend(branch.iter().flat_map(|branch| ["--branch", branch]));
    argv.extend(["--", url, dir]);
    argv
}

/// Clones URL into DIR as the sandbox user: the branch or tag BRANCH, or
/// the repository's default.
pub fn git_clone(
    sandbox: &Sandbox,
    url: &str,
    branch: Option<&str>,
    depth: &str,
    dir: &str,
) -> Result<()> {
    check_url(url)?;
    branch.map(check_ref).transpose()?;
    sandbox
        .checked(&clone_args(url, branch, depth, dir), b"", MAX_GIT_OUTPUT)
        .with_context(|| format!("cloning {url}"))?;
    Ok(())
}

/// PATH as an argument for a command.
pub fn utf8(path: &Path) -> Result<&str> {
    path.to_str()
        .with_context(|| format!("{} is not UTF-8", path.display()))
}

/// Clones the policy's target as the sandbox user, into its home.
pub fn clone(sandbox: &Sandbox, policy: &Policy) -> Result<Checkout> {
    check(policy)?;
    let (work, out) = (sandbox.home.join(WORK_DIR), sandbox.home.join(OUT_DIR));
    let dir = work.join(checkout_name(policy));
    let dir_text = utf8(&dir)?;
    sandbox.checked(
        &["mkdir", "-p", "--", utf8(&work)?, utf8(&out)?],
        b"",
        MAX_GIT_OUTPUT,
    )?;
    // A run that never started may be tried again on the same machine:
    // what an earlier try left is not the base of this one.
    sandbox.checked(&["rm", "-rf", "--", dir_text], b"", MAX_GIT_OUTPUT)?;
    git_clone(
        sandbox,
        &policy.clone_url,
        Some(&policy.base),
        DEPTH,
        dir_text,
    )?;
    // Read as the sandbox user, like everything in the checkout: it is
    // the agent's from here on.
    let git = |args: &[&str]| -> Result<String> {
        let argv = [&["timeout", GIT_TIMEOUT_S, "git", "-C", dir_text], args].concat();
        Ok(sandbox.checked(&argv, b"", MAX_GIT_OUTPUT)?.text())
    };
    let base_commit = git(&["rev-parse", "--verify", "HEAD"])?;
    ensure!(
        (40..=64).contains(&base_commit.len())
            && base_commit.bytes().all(|b| b.is_ascii_hexdigit()),
        "the clone's HEAD is not a commit: {:?}",
        one_line(&base_commit)
    );
    let head = git(&["log", "-1", "--format=%h %s"])?;
    eprintln!(
        "Cloned {} ({}) as {}: {}",
        policy.repo,
        policy.base,
        sandbox.user,
        one_line(&head)
    );
    Ok(Checkout { dir, base_commit })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{Kind, SafeOutputs};

    fn policy(repo: &str, clone_url: &str, base: &str) -> Policy {
        Policy {
            repo: repo.into(),
            clone_url: clone_url.into(),
            base: base.into(),
            kind: Kind::Branch,
            max_outputs: 1,
            max_patch_bytes: 1 << 20,
            safe_outputs: SafeOutputs::default(),
        }
    }

    #[test]
    fn targets_that_are_refused() {
        const URL: &str = "https://github.com/o/r";
        let cases = [
            ("o/..", URL, "main", "repository name"),
            ("o/", URL, "main", "repository name"),
            ("o/a b", URL, "main", "repository name"),
            ("o/r", URL, "", "branch or tag"),
            ("o/r", URL, "--upload-pack=x", "branch or tag"),
            ("o/r", URL, "a..b", "branch or tag"),
            ("o/r", URL, "a b", "branch or tag"),
            ("o/r", URL, &"x".repeat(MAX_REF_LEN + 1), "branch or tag"),
            (
                "o/r",
                "ext::sh -c id",
                "main",
                "is not an https or file URL",
            ),
            ("o/r", "ssh://git@github.com/o/r", "main", "clone_url"),
            ("o/r", "http://github.com/o/r", "main", "clone_url"),
            ("o/r", "github.com/o/r", "main", "clone_url"),
            ("o/r", "--upload-pack=x", "main", "clone_url"),
            ("o/r", "https://github.com/o/r\n", "main", "clone_url"),
        ];
        for (repo, url, base, want) in cases {
            let err = check(&policy(repo, url, base)).expect_err(url);
            assert!(
                format!("{err:#}").contains(want),
                "{repo} {url} {base}: {err:#}"
            );
        }
        assert_eq!(
            checkout_name(&policy("bootc-dev/bootc", URL, "main")),
            "bootc"
        );
        for (url, base) in [(URL, "main"), ("file:///srv/mirror/r.git", "release/1.2_x")] {
            check(&policy("o/r", url, base)).unwrap();
        }
    }

    #[test]
    fn the_clone_names_its_transports() {
        let argv = clone_args(
            "https://github.com/o/r",
            Some("main"),
            DEPTH,
            "/home/agent/work/r",
        );
        assert_eq!(
            argv,
            [
                "timeout",
                CLONE_TIMEOUT_S,
                "git",
                "-c",
                "protocol.allow=never",
                "-c",
                "protocol.https.allow=always",
                "-c",
                "protocol.file.allow=always",
                "clone",
                "--quiet",
                "--no-recurse-submodules",
                "--depth",
                "50",
                "--branch",
                "main",
                "--",
                "https://github.com/o/r",
                "/home/agent/work/r",
            ]
        );
        // Without a branch, the repository's default.
        let argv = clone_args("file:///srv/r.git", None, "1", "d");
        assert_eq!(
            argv[argv.len() - 5..],
            ["--depth", "1", "--", "file:///srv/r.git", "d"]
        );
    }
}
