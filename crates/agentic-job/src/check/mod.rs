//! `agentic-job check`: check handed-back outputs and the patch against
//! the policy. Step 4 of docs/plan.md.
//!
//! This is the second half of the check. The first is gh-aw's collector
//! (`collect_ndjson_output.cjs`), which knows the request types, counts
//! them, and validates and sanitizes their fields; it is run before this,
//! configured with the policy's `safe_outputs`, and its result is given
//! here with `--collected`. Nothing of it is done again. What is done
//! here is what gh-aw leaves to the workflow it generates, or does not do
//! at all: the hand-back holds only the files it may, within their sizes
//! and free of secret-shaped strings; the requests fit the policy; a pull
//! request comes with exactly its patch, against the policy's repository
//! and base; and the patch passes the rules of [`patch`].

mod patch;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use anyhow::{Context, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub use self::patch::{BASE_COMMIT_HEADER, PatchReading, read_patch};
use crate::exit::Exit;
use crate::files;
use crate::policy::{CREATE_PULL_REQUEST, Policy};
use crate::redact;

/// The requests the agent wrote, one JSON object to a line.
pub const OUTPUTS_FILE: &str = "outputs.jsonl";

/// Which repository, branch and commit a patch is against.
pub const BASE_FILE: &str = "base.json";

const MAX_OUTPUTS_BYTES: u64 = 1 << 20;
const MAX_BASE_BYTES: u64 = 4096;

/// The collector's result is the requests again, sanitized, as one JSON
/// value: a little more than the requests themselves.
const MAX_COLLECTED_BYTES: u64 = 4 * MAX_OUTPUTS_BYTES;

/// How much of a request's title, or of a reason, one line of the summary
/// or of the log shows.
const SHOWN_CHARS: usize = 300;

/// How much of a commit id the summary shows.
const SHORT_COMMIT: usize = 12;

static PATCH_FILE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^aw-[a-z0-9][a-z0-9._-]{0,99}\.patch$").expect("a valid pattern")
});

static COMMIT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[0-9a-f]{40}$").expect("a valid pattern"));

#[derive(Debug, clap::Args)]
pub struct Args {
    /// The policy.json that `policy` printed for this run
    #[arg(long, value_name = "FILE")]
    pub policy: PathBuf,
    /// The directory holding the handed-back outputs
    #[arg(long, value_name = "DIR")]
    pub outputs: PathBuf,
    /// What gh-aw's collector made of the outputs (its agent_output.json)
    #[arg(long, value_name = "FILE")]
    pub collected: PathBuf,
    /// Where to write the verdict, with the reasons for a refusal, as JSON
    #[arg(long, value_name = "FILE")]
    pub report: Option<PathBuf>,
}

/// What `check` decided. `--report` holds this as JSON.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Verdict {
    pub ok: bool,
    /// Every reason for a refusal.
    pub errors: Vec<String>,
    /// The requests, as the collector sanitized them.
    pub items: Vec<Map<String, Value>>,
    pub patch: Option<PatchInfo>,
}

/// The patch of an accepted pull request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PatchInfo {
    pub file: String,
    pub bytes: usize,
    pub base_commit: String,
    /// The paths the patch names. git can still apply it to others (see
    /// [`patch`]): whoever applies it compares this with what changed.
    pub files: Vec<String>,
}

/// What gh-aw's collector writes: the requests it accepted, and why it
/// refused the others.
#[derive(Debug, Deserialize)]
struct Collected {
    items: Vec<Map<String, Value>>,
    errors: Vec<String>,
}

/// `base.json`.
#[derive(Debug, Deserialize)]
struct Base {
    repo: String,
    r#ref: String,
    commit: String,
}

impl Verdict {
    fn new(
        mut errors: Vec<String>,
        items: Vec<Map<String, Value>>,
        patch: Option<PatchInfo>,
    ) -> Self {
        // Each once, in the order found.
        let mut seen = BTreeSet::new();
        errors.retain(|error| seen.insert(error.clone()));
        Self {
            ok: errors.is_empty(),
            errors,
            items,
            patch,
        }
    }

    /// A Markdown summary, for a job's summary page.
    pub fn markdown(&self, policy: &Policy) -> String {
        let mut lines = vec![
            format!(
                "### Safe outputs: {}",
                if self.ok { "accepted" } else { "refused" }
            ),
            String::new(),
            format!(
                "Policy: {} @ {}, outputs {}, at most {}.",
                policy.repo,
                policy.base,
                policy.safe_outputs.types().collect::<Vec<_>>().join(", "),
                policy.max_outputs
            ),
            String::new(),
        ];
        lines.extend(self.items.iter().map(|item| {
            let text = ["title", "message"]
                .iter()
                .find_map(|key| item.get(*key).and_then(Value::as_str))
                .filter(|text| !text.is_empty());
            match text {
                Some(text) => format!("- `{}`: {}", item_type(item), one_line(text)),
                None => format!("- `{}`", item_type(item)),
            }
        }));
        if self.items.is_empty() {
            lines.push("- no outputs".to_owned());
        }
        if let Some(patch) = &self.patch {
            let commit: String = patch.base_commit.chars().take(SHORT_COMMIT).collect();
            lines.push(format!(
                "- patch `{}` ({} bytes) against {commit}",
                patch.file, patch.bytes
            ));
        }
        lines.extend(
            self.errors
                .iter()
                .map(|error| format!("- refused: {}", one_line(error))),
        );
        lines.join("\n") + "\n"
    }
}

/// `text` as one line of bounded length. The requests and the reasons
/// quote what the agent wrote, and a line of the agent's own in a job's
/// log could be read as a command to the CI system.
fn one_line(text: &str) -> String {
    text.chars()
        .take(SHOWN_CHARS)
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// The output type that opens an issue, and the fields of one that act
/// on other issues.
const CREATE_ISSUE: &str = "create_issue";
const ISSUE_LINKS: &[&str] = &["parent", "blocked_by"];

fn item_type(item: &Map<String, Value>) -> &str {
    item.get("type").and_then(Value::as_str).unwrap_or_default()
}

/// The fields of a request that would take it out of the policy. gh-aw's
/// rules let a request name a repository of its own, and a pull request a
/// base branch and whether it is a draft; whether a handler honours them
/// is the handler's configuration, which this does not see. The policy
/// has fixed all three, so a request that says otherwise is refused.
fn redirections(item: &Map<String, Value>, policy: &Policy) -> Vec<String> {
    let output = item_type(item);
    let differs = |key: &str, same: &dyn Fn(&str) -> bool| {
        item.get(key)
            .filter(|value| !value.as_str().is_some_and(same))
            .map(|value| format!("a {output} with {key} {value}, which is not the policy's"))
    };
    let mut problems: Vec<String> =
        differs("repo", &|repo| repo.eq_ignore_ascii_case(&policy.repo))
            .into_iter()
            .collect();
    if output == CREATE_PULL_REQUEST {
        problems.extend(differs("base", &|base| base == policy.base));
        if item
            .get("draft")
            .is_some_and(|draft| draft != &Value::Bool(true))
        {
            problems.push(format!("a {output} that is not a draft"));
        }
    }
    // gh-aw's handler would make the new issue a sub-issue of, or blocked
    // by, any issue the request names; nothing bounds which, so a request
    // that names one is refused.
    if output == CREATE_ISSUE {
        if let Some(limit) = policy.safe_outputs.others.get(output)
            && (limit.allowed.is_some() || !limit.blocked.is_empty())
            && let Some(labels) = item.get("labels")
        {
            match labels.as_array() {
                Some(labels) => {
                    for label in labels {
                        let permitted = label.as_str().is_some_and(|label| {
                            label == label.trim()
                                && !limit
                                    .blocked
                                    .iter()
                                    .any(|blocked| blocked.eq_ignore_ascii_case(label))
                                && limit.allowed.as_ref().is_none_or(|allowed| {
                                    allowed
                                        .iter()
                                        .any(|allowed| allowed.eq_ignore_ascii_case(label))
                                })
                        });
                        if !permitted {
                            problems.push(format!(
                                "a {output} with label {label}, outside the policy's label limits"
                            ));
                        }
                    }
                }
                None => problems.push(format!("a {output} whose labels are not an array")),
            }
        }
        problems.extend(
            ISSUE_LINKS
                .iter()
                .filter(|key| item.contains_key(**key))
                .map(|key| format!("a {output} with {key}, which links another issue")),
        );
    }
    problems
}

fn is_handback_file(name: &str) -> bool {
    name == OUTPUTS_FILE || name == BASE_FILE || PATCH_FILE_RE.is_match(name)
}

/// The most a file of a hand-back may hold.
fn max_bytes_of(name: &str, policy: &Policy) -> u64 {
    match name {
        OUTPUTS_FILE => MAX_OUTPUTS_BYTES,
        BASE_FILE => MAX_BASE_BYTES,
        _ => policy.max_patch_bytes,
    }
}

/// The name of the patch for a pull request's branch: gh-aw's
/// (`sanitizeForFilename` of `git_patch_utils.cjs`), which its handlers
/// look for.
pub fn patch_file_name(branch: &str) -> String {
    static UNSAFE_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"[/\\:*?"<>|]"#).expect("a valid pattern"));
    static DASHES_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new("-{2,}").expect("a valid pattern"));

    let name = UNSAFE_RE.replace_all(branch, "-");
    let name = DASHES_RE.replace_all(&name, "-");
    let name = name.strip_prefix('-').unwrap_or(&name);
    let name = name.strip_suffix('-').unwrap_or(name);
    format!("aw-{}.patch", name.to_lowercase())
}

/// Checks the hand-back in `dir` (`outputs.jsonl`, `base.json`, one
/// `aw-*.patch`) and the collector's result for it against `policy`.
///
/// An error is something wrong with the check's own inputs, the
/// directory or the collector's result; whatever is wrong with the
/// hand-back is a refusal, in the verdict.
pub fn check_outputs(dir: &Path, collected: &Path, policy: &Policy) -> Result<Verdict> {
    let mut errors = Vec::new();
    let mut names = std::fs::read_dir(dir)
        .and_then(|entries| {
            entries
                .map(|entry| entry.map(|e| e.file_name()))
                .collect::<Result<Vec<_>, _>>()
        })
        .with_context(|| format!("reading the outputs directory {}", dir.display()))?;
    names.sort();

    let mut contents: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for name in &names {
        let Some(name) = name.to_str().filter(|name| is_handback_file(name)) else {
            errors.push(format!("unexpected file {name:?}"));
            continue;
        };
        match files::read_regular(&dir.join(name), max_bytes_of(name, policy)) {
            Ok(content) if redact::is_secret_shaped(&content) => {
                errors.push(format!("a secret-shaped string in {name}"));
            }
            Ok(content) => {
                contents.insert(name.to_owned(), content);
            }
            Err(err) => errors.push(format!("{name} {err}")),
        }
    }
    // Taken from a path of its own and never from `dir`, which the agent's
    // machine uploaded: a result found there could be the agent's.
    let collected_bytes = files::read_regular(collected, MAX_COLLECTED_BYTES)
        .with_context(|| format!("the collector's result {}", collected.display()))?;
    if redact::is_secret_shaped(&collected_bytes) {
        errors.push("a secret-shaped string in the collected outputs".to_owned());
    }
    if !errors.is_empty() {
        return Ok(Verdict::new(errors, Vec::new(), None));
    }

    let collected: Collected = serde_json::from_slice(&collected_bytes).with_context(|| {
        format!(
            "{} is not the result of gh-aw's collector",
            collected.display()
        )
    })?;
    let items = if contents.contains_key(OUTPUTS_FILE) {
        errors.extend(collected.errors);
        collected.items
    } else {
        if !collected.items.is_empty() || !collected.errors.is_empty() {
            errors.push(format!(
                "the collector's result holds outputs, and there is no {OUTPUTS_FILE}"
            ));
        }
        Vec::new()
    };

    // The collector counts each type against the configuration it was
    // given. Counting again costs nothing and shows a collector that was
    // given another configuration than this policy's.
    if items.len() > usize::try_from(policy.max_outputs).unwrap_or(usize::MAX) {
        errors.push(format!(
            "{} outputs, over max_outputs {}",
            items.len(),
            policy.max_outputs
        ));
    }
    let mut counts: BTreeMap<&str, u32> = BTreeMap::new();
    for item in &items {
        *counts.entry(item_type(item)).or_default() += 1;
    }
    for (output, count) in counts {
        match policy.safe_outputs.max_of(output) {
            None => errors.push(format!(
                "an output of type {output:?}, which the policy does not allow"
            )),
            Some(max) if count > max => {
                errors.push(format!("{count} outputs of type {output:?}, over {max}"));
            }
            Some(_) => {}
        }
    }

    errors.extend(items.iter().flat_map(|item| redirections(item, policy)));

    let patch_names: Vec<&str> = contents
        .keys()
        .map(String::as_str)
        .filter(|name| PATCH_FILE_RE.is_match(name))
        .collect();
    let rules = policy.safe_outputs.create_pull_request.as_ref();
    let request = items
        .iter()
        .find(|item| item_type(item) == CREATE_PULL_REQUEST);
    let mut patch = None;
    match (request, rules) {
        (Some(request), Some(rules)) => {
            // The patch is read against any commit base.json names, so that
            // a hand-back refused for its base is told what else is wrong.
            let base = contents
                .get(BASE_FILE)
                .and_then(|content| serde_json::from_slice::<Base>(content).ok())
                .filter(|base| COMMIT_RE.is_match(&base.commit));
            let ours = |base: &Base| {
                base.repo.eq_ignore_ascii_case(&policy.repo) && base.r#ref == policy.base
            };
            if !base.as_ref().is_some_and(ours) {
                errors.push(format!(
                    "{BASE_FILE} is not {{repo: {}, ref: {}, commit: SHA}}",
                    policy.repo, policy.base
                ));
            }
            let branch = request
                .get("branch")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let want = patch_file_name(branch);
            match (patch_names.as_slice(), contents.get(&want), base) {
                ([_], Some(content), Some(base)) => {
                    // What is not UTF-8 in it is in a file's content, where
                    // the rules look only at the first byte of a line.
                    let text = String::from_utf8_lossy(content);
                    let PatchReading { problems, files } = read_patch(&text, rules, &base.commit);
                    errors.extend(problems.iter().map(|problem| format!("{want}: {problem}")));
                    patch = Some(PatchInfo {
                        file: want,
                        bytes: content.len(),
                        base_commit: base.commit,
                        files,
                    });
                }
                ([_], Some(_), None) => {}
                _ => errors.push(format!(
                    "{CREATE_PULL_REQUEST} needs exactly one patch, {want}; there is {}",
                    if patch_names.is_empty() {
                        "none".to_owned()
                    } else {
                        patch_names.join(", ")
                    }
                )),
            }
        }
        // Without the rules for a patch there is no pull request to check:
        // the count above has refused it.
        (Some(_), None) => {}
        (None, _) if !patch_names.is_empty() => {
            errors.push(format!("a patch without a {CREATE_PULL_REQUEST}"));
        }
        (None, _) => {}
    }
    Ok(Verdict::new(errors, items, patch))
}

pub fn run(args: &Args) -> Result<Exit> {
    let policy = Policy::load(&args.policy)?;
    let verdict = check_outputs(&args.outputs, &args.collected, &policy)?;
    if let Some(report) = &args.report {
        let json = serde_json::to_string_pretty(&verdict).context("writing the verdict")?;
        std::fs::write(report, json + "\n")
            .with_context(|| format!("writing the report {}", report.display()))?;
    }
    for error in &verdict.errors {
        eprintln!("error: {}", one_line(error));
    }
    print!("{}", verdict.markdown(&policy));
    Ok(if verdict.ok {
        Exit::Success
    } else {
        Exit::Failure
    })
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use serde_json::json;

    use super::*;
    use crate::policy::tests::policy;

    const BASE: &str = "1111111111111111111111111111111111111111";
    const BRANCH: &str = "agent-run-7";
    const PATCH_NAME: &str = "aw-agent-run-7.patch";

    fn patch_of(path: &str) -> String {
        format!(
            "From 2222222222222222222222222222222222222222 Mon Sep 17 00:00:00 2001\n\
             {BASE_COMMIT_HEADER}: {BASE}\n\
             From: agent <agent@localhost>\n\
             Date: Tue, 6 Oct 2026 21:02:35 -0400\n\
             Subject: [PATCH] Change things\n\
             \n\
             ---\n \
             {path} | 1 +\n\
             \n\
             diff --git a/{path} b/{path}\nnew file mode 100644\nindex 0000000..c600332\n\
             --- /dev/null\n+++ b/{path}\n@@ -0,0 +1 @@\n+x\n"
        )
    }

    fn pull_request() -> Value {
        json!({"type": "create_pull_request", "title": "Fix the thing", "body": "Why.", "branch": BRANCH})
    }

    fn noop() -> Value {
        json!({"type": "noop", "message": "Nothing to do."})
    }

    fn base_json() -> String {
        json!({"repo": "bootc-dev/bootc", "ref": "main", "commit": BASE}).to_string()
    }

    /// A hand-back and the collector's result for it.
    #[derive(Default)]
    struct Handback {
        /// The collector's items; also written as `outputs.jsonl`.
        items: Vec<Value>,
        /// The collector's errors.
        refused: Vec<&'static str>,
        /// Other files of the hand-back.
        files: Vec<(&'static str, String)>,
        /// Leave `outputs.jsonl` out.
        no_outputs: bool,
    }

    impl Handback {
        fn pull_request() -> Self {
            Self {
                items: vec![pull_request()],
                files: vec![
                    (PATCH_NAME, patch_of("src/new.rs")),
                    (BASE_FILE, base_json()),
                ],
                ..Self::default()
            }
        }

        /// The same with one file replaced, added (or removed, for `None`).
        fn with(mut self, name: &'static str, content: Option<String>) -> Self {
            self.files.retain(|(file, _)| *file != name);
            self.files.extend(content.map(|content| (name, content)));
            self
        }

        fn check(&self) -> Verdict {
            let dir = tempfile::tempdir().unwrap();
            let out = dir.path().join("out");
            std::fs::create_dir(&out).unwrap();
            if !self.no_outputs {
                let lines: String = self.items.iter().map(|item| format!("{item}\n")).collect();
                std::fs::write(out.join(OUTPUTS_FILE), lines).unwrap();
            }
            for (name, content) in &self.files {
                std::fs::write(out.join(name), content).unwrap();
            }
            let collected = dir.path().join("agent_output.json");
            let result = json!({"items": self.items, "errors": self.refused});
            std::fs::write(&collected, result.to_string()).unwrap();
            check_outputs(&out, &collected, &policy()).unwrap()
        }
    }

    #[test]
    fn what_is_accepted() {
        let cases = [
            ("a pull request", Handback::pull_request(), 1, true),
            (
                "a pull request and a noop",
                Handback {
                    items: vec![pull_request(), noop()],
                    ..Handback::pull_request()
                },
                2,
                true,
            ),
            (
                "a noop alone",
                Handback {
                    items: vec![noop()],
                    ..Handback::default()
                },
                1,
                false,
            ),
            ("nothing", Handback::default(), 0, false),
            (
                "no file at all",
                Handback {
                    no_outputs: true,
                    ..Handback::default()
                },
                0,
                false,
            ),
        ];
        for (name, handback, items, patch) in cases {
            let verdict = handback.check();
            assert_eq!(verdict.errors, Vec::<String>::new(), "{name}");
            assert!(verdict.ok, "{name}");
            assert_eq!(verdict.items.len(), items, "{name}");
            assert_eq!(verdict.patch.is_some(), patch, "{name}");
        }
        let patch = Handback::pull_request().check().patch.unwrap();
        assert_eq!(
            (patch.file.as_str(), patch.base_commit.as_str()),
            (PATCH_NAME, BASE)
        );
        assert_eq!(patch.bytes, patch_of("src/new.rs").len());
        assert_eq!(patch.files, ["src/new.rs"]);
    }

    /// An issue that names other issues is refused by this rule, and one
    /// that does not passes it.
    #[test]
    fn an_issue_that_links_other_issues_is_refused() {
        let policy = policy();
        let plain = json!({"type": "create_issue", "title": "t", "body": "b"});
        assert!(redirections(plain.as_object().unwrap(), &policy).is_empty());
        for key in ISSUE_LINKS {
            let mut item = plain.clone();
            item[*key] = json!(7);
            assert_eq!(
                redirections(item.as_object().unwrap(), &policy),
                [format!(
                    "a create_issue with {key}, which links another issue"
                )],
                "{key}"
            );
        }
    }

    #[test]
    fn issue_labels_are_checked_not_silently_filtered() {
        let mut policy = policy();
        policy.safe_outputs.others.insert(
            CREATE_ISSUE.to_owned(),
            crate::policy::OutputLimit {
                max: 1,
                allowed: Some(vec!["triage".to_owned(), "blocked".to_owned()]),
                blocked: vec!["blocked".to_owned()],
            },
        );
        for (labels, ok) in [
            (json!([]), true),
            (json!(["TRIAGE"]), true),
            (json!(["triage", "other"]), false),
            (json!(["BLOCKED"]), false),
            (json!(["blocked "]), false),
            (json!([7]), false),
            (json!("triage"), false),
            (Value::Null, false),
        ] {
            let item = json!({"type": CREATE_ISSUE, "labels": labels});
            assert_eq!(
                redirections(item.as_object().unwrap(), &policy).is_empty(),
                ok,
                "{labels}"
            );
        }
        let limit = policy.safe_outputs.others.get_mut(CREATE_ISSUE).unwrap();
        limit.allowed = Some(Vec::new());
        let item = json!({"type": CREATE_ISSUE, "labels": ["triage"]});
        assert!(!redirections(item.as_object().unwrap(), &policy).is_empty());
        policy
            .safe_outputs
            .others
            .get_mut(CREATE_ISSUE)
            .unwrap()
            .allowed = None;
        assert!(redirections(item.as_object().unwrap(), &policy).is_empty());
    }

    #[test]
    fn what_is_refused() {
        let token = format!("ghp_{}", "a1B2".repeat(10));
        let pr = Handback::pull_request;
        let comment = json!({"type": "add_comment", "body": "x", "item_number": 1});
        let other_base = |repo: &str, branch: &str, commit: &str| {
            Some(json!({"repo": repo, "ref": branch, "commit": commit}).to_string())
        };
        let cases = [
            (
                "what the collector refused",
                Handback {
                    refused: vec!["Line 1: Unexpected output type 'add_comment'"],
                    ..Handback::default()
                },
                "Unexpected output type",
            ),
            (
                "a type the policy does not have",
                Handback {
                    items: vec![comment],
                    ..Handback::default()
                },
                "\"add_comment\", which the policy does not allow",
            ),
            (
                "more of a type than its maximum",
                Handback {
                    items: vec![pull_request(), pull_request()],
                    ..pr()
                },
                "2 outputs of type \"create_pull_request\", over 1",
            ),
            (
                "more outputs than max_outputs",
                Handback {
                    items: vec![noop(), noop(), noop(), noop()],
                    ..Handback::default()
                },
                "4 outputs, over max_outputs 3",
            ),
            (
                "a request without a type",
                Handback {
                    items: vec![json!({"message": "x"})],
                    ..Handback::default()
                },
                "\"\", which the policy does not allow",
            ),
            (
                "outputs the collector saw and the hand-back lacks",
                Handback {
                    items: vec![noop()],
                    no_outputs: true,
                    ..Handback::default()
                },
                "there is no outputs.jsonl",
            ),
            (
                "a secret in a request",
                Handback {
                    items: vec![json!({"type": "noop", "message": token})],
                    ..Handback::default()
                },
                "a secret-shaped string in outputs.jsonl",
            ),
            (
                "a secret in the patch",
                pr().with(
                    PATCH_NAME,
                    Some(patch_of("src/k.rs").replace("+x\n", &format!("+{token}\n"))),
                ),
                "a secret-shaped string in aw-agent-run-7.patch",
            ),
            (
                "a pull request without its patch",
                pr().with(PATCH_NAME, None),
                "needs exactly one patch, aw-agent-run-7.patch; there is none",
            ),
            (
                "a patch of another name",
                pr().with(PATCH_NAME, None)
                    .with("aw-other.patch", Some(patch_of("a.rs"))),
                "there is aw-other.patch",
            ),
            (
                "two patches",
                pr().with("aw-other.patch", Some(patch_of("a.rs"))),
                "needs exactly one patch",
            ),
            (
                "a patch without a pull request",
                Handback {
                    items: vec![noop()],
                    ..pr()
                },
                "a patch without a create_pull_request",
            ),
            (
                "a pull request without base.json",
                pr().with(BASE_FILE, None),
                "base.json is not",
            ),
            (
                "a base.json that is no JSON",
                pr().with(BASE_FILE, Some("{".to_owned())),
                "base.json is not",
            ),
            (
                "a base.json for another repo",
                pr().with(BASE_FILE, other_base("bootc-dev/other", "main", BASE)),
                "base.json is not",
            ),
            (
                "a base.json for another base",
                pr().with(BASE_FILE, other_base("bootc-dev/bootc", "bot/x", BASE)),
                "base.json is not",
            ),
            (
                "a base.json with a short commit",
                pr().with(BASE_FILE, other_base("bootc-dev/bootc", "main", "1111111")),
                "base.json is not",
            ),
            (
                "a patch of another base commit",
                pr().with(
                    BASE_FILE,
                    other_base("bootc-dev/bootc", "main", &"0".repeat(40)),
                ),
                "is not the base commit",
            ),
            (
                "a stray file",
                pr().with("notes.txt", Some("hi".to_owned())),
                "unexpected file \"notes.txt\"",
            ),
            (
                "a collector's result among the files",
                pr().with("agent_output.json", Some("{}".to_owned())),
                "unexpected file \"agent_output.json\"",
            ),
            (
                "a base.json too big",
                pr().with(BASE_FILE, Some(" ".repeat(5000))),
                "base.json is 5000 bytes, over 4096",
            ),
            (
                "a protected file",
                pr().with(PATCH_NAME, Some(patch_of("README.md"))),
                "aw-agent-run-7.patch: protected files: README.md",
            ),
            (
                "a pull request for another repository",
                Handback {
                    items: vec![
                        json!({"type": "create_pull_request", "title": "t", "body": "b", "branch": BRANCH, "repo": "evil/other"}),
                    ],
                    ..pr()
                },
                "with repo \"evil/other\", which is not the policy's",
            ),
            (
                "a comment in another repository",
                Handback {
                    items: vec![json!({"type": "noop", "message": "m", "repo": ["x"]})],
                    ..Handback::default()
                },
                "a noop with repo [\"x\"]",
            ),
            (
                "a pull request onto another base",
                Handback {
                    items: vec![
                        json!({"type": "create_pull_request", "title": "t", "body": "b", "branch": BRANCH, "base": "release"}),
                    ],
                    ..pr()
                },
                "with base \"release\", which is not the policy's",
            ),
            (
                "a pull request that is not a draft",
                Handback {
                    items: vec![
                        json!({"type": "create_pull_request", "title": "t", "body": "b", "branch": BRANCH, "draft": false}),
                    ],
                    ..pr()
                },
                "a create_pull_request that is not a draft",
            ),
            (
                "a pull request without a branch",
                Handback {
                    items: vec![json!({"type": "create_pull_request", "title": "t", "body": "b"})],
                    ..pr()
                },
                "needs exactly one patch, aw-.patch",
            ),
        ];
        for (name, handback, want) in cases {
            let verdict = handback.check();
            assert!(!verdict.ok, "{name}");
            assert!(
                verdict.errors.iter().any(|e| e.contains(want)),
                "{name}: {:#?}",
                verdict.errors
            );
        }
    }

    #[test]
    fn a_request_may_repeat_what_the_policy_says() {
        let base = json!({"repo": "Bootc-Dev/BOOTC", "ref": "main", "commit": BASE}).to_string();
        assert!(
            Handback::pull_request()
                .with(BASE_FILE, Some(base))
                .check()
                .ok
        );
        let request = json!({
            "type": "create_pull_request", "title": "t", "body": "b", "branch": BRANCH,
            "repo": "Bootc-Dev/bootc", "base": "main", "draft": true,
        });
        let verdict = Handback {
            items: vec![request],
            ..Handback::pull_request()
        }
        .check();
        assert_eq!(verdict.errors, Vec::<String>::new());
    }

    #[test]
    fn links_and_directories_are_not_read() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out");
        std::fs::create_dir(&out).unwrap();
        let secret = dir.path().join("secret");
        std::fs::write(
            &secret,
            "{\"type\":\"noop\",\"message\":\"the runner's file\"}\n",
        )
        .unwrap();
        symlink(&secret, out.join(OUTPUTS_FILE)).unwrap();
        std::fs::create_dir(out.join(BASE_FILE)).unwrap();
        let collected = dir.path().join("agent_output.json");
        std::fs::write(&collected, r#"{"items":[],"errors":[]}"#).unwrap();

        let verdict = check_outputs(&out, &collected, &policy()).unwrap();
        assert_eq!(
            verdict.errors,
            [
                "base.json is not a regular file",
                "outputs.jsonl is not a regular file"
            ]
        );
        // The collector's result is the check's own input: a bad one is
        // an error and no verdict.
        let empty = dir.path().join("empty");
        std::fs::create_dir(&empty).unwrap();
        for bad in ["not json", r#"{"items":[]}"#] {
            std::fs::write(&collected, bad).unwrap();
            assert!(
                check_outputs(&empty, &collected, &policy()).is_err(),
                "{bad}"
            );
        }
        assert!(check_outputs(&out, &dir.path().join("missing"), &policy()).is_err());
    }

    #[test]
    fn patch_file_names() {
        let cases = [
            ("agent-run-7", "aw-agent-run-7.patch"),
            ("Feature/Fix: it", "aw-feature-fix- it.patch"),
            ("a//b", "aw-a-b.patch"),
            ("/a/", "aw-a.patch"),
            ("a--b", "aw-a-b.patch"),
            ("", "aw-.patch"),
        ];
        for (branch, want) in cases {
            assert_eq!(patch_file_name(branch), want, "{branch}");
        }
    }

    #[test]
    fn the_summary_holds_no_line_of_the_agents_own() {
        let hostile =
            json!({"type": "noop", "message": "done\n::error::owned\r\n::stop-commands::x"});
        let verdict = Handback {
            items: vec![hostile],
            ..Handback::default()
        }
        .check();
        let markdown = verdict.markdown(&policy());
        assert!(
            markdown.starts_with("### Safe outputs: accepted\n"),
            "{markdown}"
        );
        assert!(
            markdown.contains("- `noop`: done ::error::owned  ::stop-commands::x\n"),
            "{markdown}"
        );
        assert!(
            markdown.lines().all(|line| !line.starts_with("::")),
            "{markdown}"
        );

        let refused = Handback::pull_request().with(PATCH_NAME, None).check();
        let markdown = refused.markdown(&policy());
        assert!(
            markdown.starts_with("### Safe outputs: refused\n"),
            "{markdown}"
        );
        assert!(
            markdown.contains("- refused: create_pull_request needs exactly one patch"),
            "{markdown}"
        );
    }
}
