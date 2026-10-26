//! refresh-feed: keeps the egress proxy's threat-feed pins current.
//!
//! ```text
//! refresh-feed update
//! GITHUB_REPOSITORY=OWNER/REPO refresh-feed publish
//! ```
//!
//! Run from the repository root, and only from trusted default-branch
//! source, never from a pull request. `update` resolves the newest
//! upstream commit of the feed file `crates/secure-host/src/egress.rs`
//! configures, downloads the feed at that commit, checks its format and
//! rewrites only `DENYLIST_COMMIT` and `DENYLIST_SHA256` in that file.
//! `publish` commits a changed file to `maintenance/threat-feed-COMMIT`
//! and opens a draft pull request for review with `git` and `gh`. It
//! downloads and builds nothing, so the step holding the write token can
//! run an already-built binary.

use anyhow::{Context, Result, bail, ensure};
use regex::Regex;
use serde_json::Value;
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::LazyLock;
use std::time::Duration;

/// The file holding the feed configuration and its pins.
const SOURCE: &str = "crates/secure-host/src/egress.rs";
/// Largest feed accepted, in bytes.
const MAX_FEED_BYTES: usize = 16 * 1024 * 1024;
/// Largest commit-list answer accepted, in bytes.
const MAX_API_BYTES: usize = 256 * 1024;
/// Whole-request limit for each download.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(60);
/// Largest standard output read from one command: what Node's
/// `execFileSync` allowed by default.
const MAX_COMMAND_OUTPUT: usize = 1024 * 1024;
/// The only feed host: raw files at a commit.
const FEED_ORIGIN: &str = "https://raw.githubusercontent.com";
const API_REPOS: &str = "https://api.github.com/repos";
const BRANCH_PREFIX: &str = "maintenance/threat-feed-";
/// `gh pr list --limit`: a full page may hide an open proposal.
const MAX_OPEN_PRS: usize = 100;
const COMMIT_NAME: &str = "user.name=feed maintenance";
const COMMIT_EMAIL: &str = "user.email=feed-maintenance@users.noreply.github.com";
const TITLE: &str = "egress: Keep known malicious destinations covered";
const COMMIT_MESSAGE: &str = "egress: Keep known malicious destinations covered\n\n\
    Refresh the reviewed feed snapshot so newly listed threats are not missed.\n\n\
    Generated-by: AI";
const BODY: &str = "Refresh the commit and checksum together so new threat-feed entries \
    can be reviewed before runtime use. No runtime trust policy changes.\n\n\
    Verified by the refresh job’s feed-format and checksum tests; normal PR CI remains required.";

fn regex(pattern: &str) -> Regex {
    Regex::new(pattern).expect("built-in patterns are valid")
}

// Names are ASCII `[\w.-]`, as JavaScript's `\w` spelled it.
static DOMAIN: LazyLock<Regex> = LazyLock::new(|| regex(r"^(?:[a-z0-9_-]+\.)+[a-z0-9_-]+$"));
static OWNER_REPO: LazyLock<Regex> = LazyLock::new(|| regex(r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$"));
static FILE: LazyLock<Regex> = LazyLock::new(|| regex(r"^[A-Za-z0-9_.-]+(?:/[A-Za-z0-9_.-]+)*$"));

/// Fetches a URL, refusing an answer longer than the given number of bytes.
type Download<'a> = dyn FnMut(&str, usize) -> Result<Vec<u8>> + 'a;

/// Runs a command, returning its trimmed standard output; an error on a
/// non-zero exit.
type Run<'a> = dyn FnMut(&str, &[&str]) -> Result<String> + 'a;

fn is_commit(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn json(bytes: &[u8], what: &str) -> Result<Value> {
    serde_json::from_slice(bytes).with_context(|| format!("parsing {what}"))
}

/// The value of the one `const NAME: &str = "...";` line in TEXT.
fn setting<'a>(text: &'a str, name: &str) -> Result<&'a str> {
    let escaped = regex::escape(name);
    let pattern = regex(&format!(r#"(?m)^const {escaped}: &str = "([^"\n]+)";"#));
    let mut matches = pattern.captures_iter(text);
    match (matches.next(), matches.next()) {
        (Some(m), None) => Ok(m.get(1).map_or("", |v| v.as_str())),
        _ => bail!("expected one {name}"),
    }
}

/// TEXT with the commit and SHA-256 pins set to COMMIT and the hash of the
/// exact BYTES, after checking they are a domain feed.
fn refresh(text: &str, commit: &str, bytes: &[u8]) -> Result<String> {
    ensure!(is_commit(commit), "invalid commit");
    ensure!(
        !bytes.is_empty() && bytes.len() <= MAX_FEED_BYTES,
        "invalid feed size"
    );
    let feed = String::from_utf8_lossy(bytes);
    let lines: Vec<&str> = feed
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect();
    // Refuse an HTML error page or a changed upstream format, not just an empty feed.
    ensure!(
        !lines.is_empty() && lines.iter().all(|line| DOMAIN.is_match(line)),
        "invalid domain feed"
    );
    let hash = hex(ring::digest::digest(&ring::digest::SHA256, bytes).as_ref());
    let mut text = text.to_owned();
    for (name, value) in [("DENYLIST_COMMIT", commit), ("DENYLIST_SHA256", &hash)] {
        let old = setting(&text, name)?;
        let line = |v: &str| format!(r#"const {name}: &str = "{v}";"#);
        text = text.replacen(&line(old), &line(value), 1);
    }
    Ok(text)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Percent-encodes everything but ASCII alphanumerics and these, as
/// `encodeURIComponent` does.
const UNRESERVED: &[u8] = b"-_.!~*'()";

fn encode_component(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b if b.is_ascii_alphanumeric() || UNRESERVED.contains(&b) => (b as char).to_string(),
            b => format!("%{b:02X}"),
        })
        .collect()
}

fn download(url: &str, maximum: usize) -> Result<Vec<u8>> {
    let http: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(DOWNLOAD_TIMEOUT))
        .http_status_as_error(false)
        // Direct, as Node's fetch went, not through a proxy the environment names.
        .proxy(None)
        .max_redirects(0)
        .build()
        .into();
    let fetch = || format!("fetch {url}");
    let response = http.get(url).call().with_context(fetch)?;
    let status = response.status();
    ensure!(
        !status.is_redirection(),
        "fetch {url}: redirect {} refused",
        status.as_u16()
    );
    ensure!(status.is_success(), "fetch {url}: {}", status.as_u16());
    let mut body = Vec::new();
    response
        .into_body()
        .into_reader()
        .take(maximum as u64 + 1)
        .read_to_end(&mut body)
        .with_context(fetch)?;
    ensure!(body.len() <= maximum, "response too large");
    Ok(body)
}

/// TEXT refreshed to the newest upstream commit of the configured feed.
fn propose(text: &str, download: &mut Download<'_>) -> Result<String> {
    // The repository and path come from the runtime's own configuration, not dispatcher input.
    let upstream = setting(text, "DENYLIST_REPOSITORY")?;
    // Exactly the origin and /OWNER/REPO, so the URL needs no normalizing.
    let repository = upstream
        .strip_prefix(FEED_ORIGIN)
        .and_then(|path| path.strip_prefix('/'))
        .filter(|path| OWNER_REPO.is_match(path))
        .filter(|path| !path.split('/').any(|s| s == "." || s == ".."))
        .context("unsupported feed repository")?;
    let file = setting(text, "DENYLIST_FILE")?;
    ensure!(
        FILE.is_match(file) && !file.split('/').any(|s| s == ".."),
        "invalid feed path"
    );
    let path = encode_component(file);
    let api = format!("{API_REPOS}/{repository}/commits?path={path}&per_page=1");
    let entries = json(&download(&api, MAX_API_BYTES)?, "upstream commits")?;
    let commit = entries
        .get(0)
        .and_then(|entry| entry.get("sha"))
        .and_then(Value::as_str)
        .filter(|sha| is_commit(sha))
        .context("invalid upstream commit")?;
    let bytes = download(&format!("{upstream}/{commit}/{file}"), MAX_FEED_BYTES)?;
    refresh(text, commit, &bytes)
}

/// Commits the refreshed pins in TEXT to a new branch of REPO and opens a
/// draft pull request, unless one is open or nothing changed.
fn publish(text: &str, repo: Option<&str>, run: &mut Run<'_>) -> Result<()> {
    let repo = repo
        .filter(|repo| OWNER_REPO.is_match(repo))
        .context("missing repository")?;
    let commit = setting(text, "DENYLIST_COMMIT")?;
    ensure!(is_commit(commit), "invalid commit");
    let branch = format!("{BRANCH_PREFIX}{commit}");
    // One outstanding proposal at a time; never rewrite a branch under review.
    let limit = MAX_OPEN_PRS.to_string();
    let list = [
        "pr",
        "list",
        "--repo",
        repo,
        "--limit",
        &limit,
        "--json",
        "headRefName,isCrossRepository",
    ];
    let open = json(run("gh", &list)?.as_bytes(), "open pull requests")?;
    let open = open.as_array().context("pull requests are not a list")?;
    if open.len() == MAX_OPEN_PRS {
        return Ok(());
    }
    for pr in open {
        // A fork may choose any branch name; it cannot reserve our maintenance namespace.
        if pr.get("isCrossRepository") == Some(&Value::Bool(false)) {
            let head = pr
                .get("headRefName")
                .and_then(Value::as_str)
                .context("pull request without headRefName")?;
            if head.starts_with(BRANCH_PREFIX) {
                return Ok(());
            }
        }
    }
    if run("git", &["diff", "--", SOURCE])?.is_empty() {
        return Ok(());
    }
    run("git", &["switch", "-c", &branch])?;
    run("git", &["add", "--", SOURCE])?;
    run(
        "git",
        &[
            "-c",
            COMMIT_NAME,
            "-c",
            COMMIT_EMAIL,
            "commit",
            "-m",
            COMMIT_MESSAGE,
        ],
    )?;
    // The checkout has no stored credentials. gh supplies the dedicated token only here.
    run("gh", &["auth", "setup-git"])?;
    let refspec = format!("HEAD:refs/heads/{branch}");
    run("git", &["push", "origin", &refspec])?;
    run(
        "gh",
        &[
            "pr", "create", "--repo", repo, "--head", &branch, "--draft", "--title", TITLE,
            "--body", BODY,
        ],
    )?;
    Ok(())
}

/// Runs CMD with ARGS, stderr passed through, and returns its trimmed stdout.
fn run_command(cmd: &str, args: &[&str]) -> Result<String> {
    let what = format!("{cmd} {}", args.first().unwrap_or(&""));
    let mut child = Command::new(cmd)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| format!("running {what}"))?;
    let mut stdout = Vec::new();
    let limit = MAX_COMMAND_OUTPUT as u64 + 1;
    let read = match child.stdout.take() {
        Some(pipe) => pipe.take(limit).read_to_end(&mut stdout),
        None => Ok(0),
    };
    if read.is_err() || stdout.len() > MAX_COMMAND_OUTPUT {
        // Do not wait on a child still writing into a pipe nobody reads.
        let _ = child.kill();
    }
    let status = child.wait();
    let status = status.with_context(|| format!("waiting for {what}"))?;
    read.with_context(|| format!("reading {what}"))?;
    ensure!(
        stdout.len() <= MAX_COMMAND_OUTPUT,
        "{what} wrote more than {MAX_COMMAND_OUTPUT} bytes"
    );
    ensure!(status.success(), "{what} failed: {status}");
    Ok(String::from_utf8_lossy(&stdout).trim().to_owned())
}

fn main() -> Result<()> {
    let text = std::fs::read_to_string(SOURCE)
        .with_context(|| format!("reading {SOURCE} (run from the repository root)"))?;
    match std::env::args().nth(1).as_deref() {
        Some("publish") => {
            let repo = std::env::var("GITHUB_REPOSITORY").ok();
            publish(&text, repo.as_deref(), &mut run_command)
        }
        Some("update") => {
            let text = propose(&text, &mut download)?;
            std::fs::write(SOURCE, text).with_context(|| format!("writing {SOURCE}"))
        }
        _ => bail!("expected update or publish"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: &str =
        "const DENYLIST_COMMIT: &str = \"old\";\n\nconst DENYLIST_SHA256: &str = \"oldhash\";\n";
    const BYTES: &[u8] = b"# feed\nmalware.example\n";

    fn commit() -> String {
        "a".repeat(40)
    }

    fn assert_err<T: std::fmt::Debug>(result: Result<T>, want: &str, case: &str) {
        let err = result.expect_err(case);
        assert!(err.to_string().contains(want), "{case}: {err}");
    }

    #[test]
    fn changes_only_the_two_pins_and_hashes_the_exact_downloaded_bytes() {
        let commit = commit();
        let result = refresh(TEXT, &commit, BYTES).unwrap();
        assert_eq!(setting(&result, "DENYLIST_COMMIT").unwrap(), commit);
        let hash = hex(ring::digest::digest(&ring::digest::SHA256, BYTES).as_ref());
        assert_eq!(setting(&result, "DENYLIST_SHA256").unwrap(), hash);
        let restored = result.replacen(&commit, "old", 1);
        assert_eq!(restored.replacen(&hash, "oldhash", 1), TEXT);
        assert_eq!(refresh(&result, &commit, BYTES).unwrap(), result);
    }

    #[test]
    fn refuses_malformed_commits_oversized_empty_error_feeds_and_ambiguous_source() {
        let commit = commit();
        let long = "a".repeat(41);
        for bad in ["", "main", "../evil", &long] {
            assert_err(refresh(TEXT, bad, BYTES), "commit", bad);
        }
        let huge = "x".repeat(MAX_FEED_BYTES + 1);
        for bad in [
            "",
            "# comments only\n",
            "\r\n#\r\n",
            "<html>error</html>",
            "good.example\nnot a domain",
            "good.example\r\rbad.example",
            &huge,
        ] {
            let case = &bad[..bad.len().min(40)];
            assert_err(refresh(TEXT, &commit, bad.as_bytes()), "feed", case);
        }
        let crlf = b"# feed\r\nmalware.example\r\n";
        assert!(refresh(TEXT, &commit, crlf).is_ok());
        let twice = format!("{TEXT}{TEXT}");
        for (text, case) in [("", "empty"), (twice.as_str(), "twice")] {
            assert_err(refresh(text, &commit, BYTES), "expected one", case);
        }
    }

    #[test]
    fn resolves_configured_upstream_once_then_fetches_only_the_immutable_commit() {
        let commit = commit();
        let repository = "https://raw.githubusercontent.com/example/feed";
        let configured = [
            TEXT,
            &format!("const DENYLIST_REPOSITORY: &str = \"{repository}\";\n"),
            "const DENYLIST_FILE: &str = \"lists/domains.txt\";\n",
        ]
        .concat();
        let answer = serde_json::json!([{ "sha": commit }]).to_string();
        let bodies = [answer.as_bytes(), BYTES];
        let mut calls = Vec::new();
        let result = propose(&configured, &mut |url, maximum| {
            calls.push((url.to_owned(), maximum));
            Ok(bodies[calls.len() - 1].to_vec())
        })
        .unwrap();
        assert_eq!(setting(&result, "DENYLIST_COMMIT").unwrap(), commit);
        let query = "path=lists%2Fdomains.txt&per_page=1";
        let repos = "https://api.github.com/repos";
        let api = format!("{repos}/example/feed/commits?{query}");
        let raw = format!("{repository}/{commit}/lists/domains.txt");
        assert_eq!(calls, [(api, 256 * 1024), (raw, 16 * 1024 * 1024)]);
        // An empty answer fails to parse: a case that downloads fails differently.
        for (from, to, answer, want) in [
            ("", "", r#"[{"sha":"main"}]"#, "invalid upstream commit"),
            ("", "", r#"{"sha":"main"}"#, "invalid upstream commit"),
            ("", "", "[]", "invalid upstream commit"),
            ("raw.github", "evil.github", "", "unsupported"),
            ("https://", "http://", "", "unsupported"),
            ("/example/feed", "/example/..", "", "unsupported"),
            ("/example/feed", "/example/feed/x", "", "unsupported"),
            ("/example/feed", "/example/feed?x", "", "unsupported"),
            ("lists/domains.txt", "../domains.txt", "", "feed path"),
            ("lists/domains.txt", "/domains.txt", "", "feed path"),
            ("lists/domains.txt", "lists//domains", "", "feed path"),
        ] {
            let text = configured.replacen(from, to, 1);
            let result = propose(&text, &mut |_, _| Ok(answer.as_bytes().to_vec()));
            assert_err(result, want, to);
        }
    }

    #[test]
    fn accepts_the_configured_source() {
        let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
        let path = format!("{root}/{SOURCE}");
        let text = std::fs::read_to_string(path).unwrap();
        assert!(is_commit(setting(&text, "DENYLIST_COMMIT").unwrap()));
        let mut urls = Vec::new();
        let result = propose(&text, &mut |url, _| {
            urls.push(url.to_owned());
            bail!("offline")
        });
        assert_err(result, "offline", SOURCE);
        assert!(urls[0].starts_with(API_REPOS), "{urls:?}");
    }

    /// The commands `publish` runs, each answered by ANSWER.
    fn published(answer: impl Fn(&str, &[&str]) -> String) -> Vec<(String, Vec<String>)> {
        let pinned = refresh(TEXT, &commit(), BYTES).unwrap();
        let mut calls = Vec::new();
        publish(&pinned, Some("example/project"), &mut |cmd, args| {
            let owned = args.iter().map(|a| a.to_string()).collect();
            calls.push((cmd.to_owned(), owned));
            Ok(answer(cmd, args))
        })
        .unwrap();
        calls
    }

    #[test]
    fn publishing_is_bounded_and_does_not_touch_an_open_proposal_or_unchanged_source() {
        let full = format!("[{}]", vec![r#"{"headRefName":"other"}"#; 100].join(","));
        let ours = r#"[{"headRefName":"maintenance/threat-feed-older","isCrossRepository":false}]"#;
        for prs in [ours, full.as_str()] {
            assert_eq!(published(|_, _| prs.to_owned()).len(), 1, "{prs:.80}");
        }
        let calls = published(|cmd, _| String::from(if cmd == "gh" { "[]" } else { "" }));
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[1].1[0], "diff");
        let pinned = refresh(TEXT, &commit(), BYTES).unwrap();
        for repo in [None, Some("example"), Some("a/b/c"), Some("a/b c")] {
            let result = publish(&pinned, repo, &mut |_, _| panic!("ran a command"));
            assert_err(result, "missing repository", &format!("{repo:?}"));
        }
    }

    #[test]
    fn publishing_creates_one_draft_with_explicit_repository_and_no_force_push() {
        let fork =
            r#"[{"headRefName":"maintenance/threat-feed-attacker","isCrossRepository":true}]"#;
        let calls = published(|cmd, args| {
            let list = cmd == "gh" && args[..2] == ["pr", "list"];
            String::from(if list { fork } else { "diff" })
        });
        let find = |cmd: &str, arg: usize, value: &str| {
            calls
                .iter()
                .find(|(c, args)| c == cmd && args.get(arg).is_some_and(|a| a == value))
                .map(|(_, args)| args.clone())
                .unwrap()
        };
        let create = find("gh", 1, "create");
        assert!(create.iter().any(|a| a == "--draft"));
        assert!(create.iter().any(|a| a == "example/project"));
        let refspec = format!("HEAD:refs/heads/maintenance/threat-feed-{}", commit());
        let push = find("git", 0, "push");
        assert_eq!(push, ["push", "origin", refspec.as_str()]);
        let args: Vec<&String> = calls.iter().flat_map(|(_, args)| args).collect();
        assert!(!args.iter().any(|a| a.contains("force")));
    }
}
