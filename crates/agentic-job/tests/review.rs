//! The policy workflow validates routing before its read-only API call, then
//! resolves the API snapshot without fetching or executing the reviewed code.
#![allow(clippy::unwrap_used)]

use std::path::Path;
use std::process::{Command, Output};

use serde_json::{Value, json};

fn review(command: &str, pull: Option<&Path>) -> Output {
    let mut cli = Command::new(env!("CARGO_BIN_EXE_agentic-job"));
    cli.args(["review", command]);
    cli.envs([
        ("REVIEW", "true"),
        ("EVENT", "false"),
        ("KIND", "analysis"),
        ("OUTPUTS", "add_comment,noop"),
        ("MAX_OUTPUTS", "1"),
        ("NOTIFY", "none"),
        ("APPLY_PARTIAL", "false"),
        ("ITEM", "147"),
        ("TARGET", "147"),
        ("REPO", "o/r"),
        ("BASE", "main"),
        ("OUTPUT_REPO", ""),
    ]);
    if let Some(pull) = pull {
        cli.arg("--pull-request").arg(pull);
    }
    cli.output().unwrap()
}

#[test]
fn policy_dispatch_uses_a_validated_target_and_returns_only_the_exact_sha() {
    let target = review("dispatch-target", None);
    assert!(target.status.success(), "{target:?}");
    assert_eq!(
        serde_json::from_slice::<Value>(&target.stdout).unwrap(),
        json!({"owner":"o", "repo":"r", "pull_number":147})
    );

    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("pull.json");
    let sha = "a".repeat(40);
    let mut pull = open_pull();
    pull["head"]["ref"] = json!("--upload-pack=evil");
    pull["head"]["repo"]["full_name"] = json!("fork/r");
    std::fs::write(&file, pull.to_string()).unwrap();
    let resolved = review("resolve-dispatch", Some(&file));
    assert!(resolved.status.success(), "{resolved:?}");
    assert_eq!(resolved.stdout, format!("{sha}\n").as_bytes());

    pull["base"]["repo"]["full_name"] = json!("other/r");
    std::fs::write(&file, pull.to_string()).unwrap();
    let refused = review("resolve-dispatch", Some(&file));
    assert_eq!(refused.status.code(), Some(2), "{refused:?}");
    assert!(refused.stdout.is_empty());
}

#[test]
fn the_task_command_appends_the_base_merge_policy() {
    let dir = tempfile::tempdir().unwrap();
    let task = || {
        Command::new(env!("CARGO_BIN_EXE_agentic-job"))
            .args(["review", "task", "task.md"])
            .current_dir(dir.path())
            .envs([
                ("EVENT", "true"),
                ("KIND", "analysis"),
                (
                    "OUTPUTS",
                    "add_comment,noop,create_pull_request_review_comment",
                ),
                ("MAX_OUTPUTS", "11"),
                ("NOTIFY", "none"),
                ("REPO", "o/r"),
                ("GITHUB_REPOSITORY", "o/r"),
                ("APPLY_PARTIAL", "false"),
                ("AGENT", "fake"),
            ])
            .output()
            .unwrap()
    };
    std::fs::write(dir.path().join("task.md"), "Trusted base task\n").unwrap();
    let unset = task();
    assert!(unset.status.success(), "{unset:?}");
    let unset = String::from_utf8(unset.stdout).unwrap();
    assert!(
        unset.starts_with("Trusted base task\n\n\n## Merge policy\n\nThe default branch has no")
    );
    let policy = dir.path().join(".github/agentic-job/merge.toml");
    std::fs::create_dir_all(policy.parent().unwrap()).unwrap();
    std::fs::write(&policy, "merge = \"agent\"\n").unwrap();
    let agent = String::from_utf8(task().stdout).unwrap();
    assert!(
        agent
            .starts_with("Trusted base task\n\n\n## Merge policy\n\nThe repository owner set this")
    );
    assert!(agent.contains("merge = \"agent\": an approving review"));
    // A malformed policy fails the run instead of being guessed at.
    std::fs::write(&policy, "merge = \"always\"\n").unwrap();
    let refused = task();
    assert!(!refused.status.success());
    assert!(refused.stdout.is_empty());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("may hold only"));
}

/// The policy job's event step: the outputs later jobs route by, one line
/// each whatever the reason holds, and no task left beside a refusal.
#[test]
fn admit_writes_the_event_outputs_and_narrows_a_review() {
    const SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const NARROWED: &str = concat!(
        "review requires an admitted pull request on the configured base, ",
        "at an exact commit"
    );
    const NARROWED_REFUSAL: &str = concat!(
        "Review refused by policy: review requires an admitted pull request ",
        "on the configured base, at an exact commit"
    );
    // A reason can name what the event says; a line break in it must not
    // add an output, nor its text be live in the refusal comment.
    const REFUSED: &str = "@a [push](url) refusal=x";
    const REFUSED_REFUSAL: &str = "Review refused by policy: \\@a \\[push\\]\\(url\\) refusal=x";
    let event = json!({"schema":"agentic-job-event/v1", "admitted":true, "reason":"member",
        "event":"pull_request", "action":"opened", "actor":"octocat",
        "item":{"kind":"pull_request", "number":147}, "base":"main", "head":{"sha":SHA}});
    let refused = json!({"schema":"agentic-job-event/v1", "admitted":false,
        "reason":"@a [push](url)\nrefusal=x", "event":"push"});
    // The decision, the review's base, and the reason, item, review head
    // and refusal written.
    let cases = [
        (&event, None, "member", "147", "", None),
        (&event, Some("main"), "member", "147", SHA, None),
        (
            &event,
            Some("other"),
            NARROWED,
            "",
            "",
            Some(NARROWED_REFUSAL),
        ),
        (&refused, None, REFUSED, "", "", None),
        (
            &refused,
            Some("main"),
            REFUSED,
            "",
            "",
            Some(REFUSED_REFUSAL),
        ),
    ];
    for (decision, base, reason, item, head, refusal) in cases {
        let admitted = !item.is_empty();
        let mut want =
            format!("admitted={admitted}\nreason={reason}\nitem={item}\nreview-head={head}\n");
        if let Some(refusal) = refusal {
            want.push_str(&format!("refusal={refusal}\n"));
        }
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("event.json");
        let task = dir.path().join("task.md");
        let output = dir.path().join("output");
        std::fs::write(&file, decision.to_string()).unwrap();
        std::fs::write(&task, "the task\n").unwrap();
        std::fs::write(&output, "").unwrap();
        let mut cli = Command::new(env!("CARGO_BIN_EXE_agentic-job"));
        cli.args(["review", "admit", "--decision"])
            .arg(&file)
            .arg("--output")
            .arg(&output);
        if let Some(base) = base {
            cli.args(["--base", base]);
        }
        let got = cli.output().unwrap();
        assert!(got.status.success(), "{base:?}: {got:?}");
        assert_eq!(std::fs::read_to_string(&output).unwrap(), want, "{base:?}");
        assert_eq!(task.exists(), admitted, "{base:?}");
        let word = if admitted { "admitted" } else { "refused" };
        assert_eq!(
            String::from_utf8(got.stdout).unwrap(),
            format!("agentic-job event: {word}, {reason}\n")
        );
        // A review's narrowing is what later steps read of the decision.
        let written: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        assert_eq!(written["admitted"], admitted, "{base:?}");
        assert_eq!(written["head"], decision["head"], "{base:?}");
    }
}

/// A pull request the dispatch in `review` accepts.
fn open_pull() -> Value {
    json!({"number":147, "state":"open",
        "base":{"ref":"main", "repo":{"full_name":"o/r"}},
        "head":{"sha":"a".repeat(40), "repo":{"full_name":"o/r"}}})
}

#[test]
fn resolver_refuses_links_and_oversized_api_snapshots_without_a_sha() {
    let dir = tempfile::tempdir().unwrap();
    // The link names a snapshot that is accepted when read directly, so
    // only the link can be why it is refused.
    let valid = dir.path().join("valid.json");
    std::fs::write(&valid, open_pull().to_string()).unwrap();
    assert!(review("resolve-dispatch", Some(&valid)).status.success());
    let link = dir.path().join("link.json");
    std::os::unix::fs::symlink(&valid, &link).unwrap();
    let large = dir.path().join("large.json");
    std::fs::File::create(&large)
        .unwrap()
        .set_len((4 << 20) + 1)
        .unwrap();
    for path in [&large, &link] {
        let refused = review("resolve-dispatch", Some(path));
        assert_eq!(refused.status.code(), Some(2), "{refused:?}");
        assert!(refused.stdout.is_empty());
    }
}

/// check.yml takes the verdict from stdout and, on a refusal, the report
/// its refusal step reads; check then uploads the folded output for apply.
#[test]
fn check_gets_one_verdict_word_or_a_refusal_report_and_the_folded_output() {
    let dir = tempfile::tempdir().unwrap();
    let collected = dir.path().join("agent_output.json");
    let report = dir.path().join("report.json");
    let cli = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_agentic-job"))
            .arg("review")
            .args(args)
            .output()
            .unwrap()
    };
    let verdict = || {
        cli(&[
            "verdict",
            "--target",
            "147",
            "--collected",
            collected.to_str().unwrap(),
            "--report",
            report.to_str().unwrap(),
        ])
    };
    let inline = json!({"type": "create_pull_request_review_comment", "path": "a.rs", "line": 3, "body": "nit: x"});
    let output = json!({"items": [{"type": "add_comment", "body": "VERDICT: CHANGES\nOne."}, inline], "errors": []});
    std::fs::write(&collected, output.to_string()).unwrap();
    let got = verdict();
    assert!(got.status.success(), "{got:?}");
    assert_eq!(got.stdout, b"CHANGES\n");
    assert!(!report.exists());

    let folded = cli(&["fold", collected.to_str().unwrap()]);
    assert!(folded.status.success(), "{folded:?}");
    assert_eq!(
        serde_json::from_slice::<Value>(&folded.stdout).unwrap(),
        json!({"items": [{"type": "add_comment",
            "body": "VERDICT: CHANGES\nOne.\n\n### Inline findings\n\n**a.rs, line 3**\n\nnit: x\n"}],
            "errors": []})
    );

    std::fs::write(&collected, json!({"items": [{"type": "noop"}]}).to_string()).unwrap();
    assert_eq!(verdict().stdout, b"\n");
    std::fs::write(&collected, json!({"items": [inline]}).to_string()).unwrap();
    let refused = verdict();
    assert_eq!(refused.status.code(), Some(1), "{refused:?}");
    assert!(refused.stdout.is_empty());
    assert_eq!(
        serde_json::from_slice::<Value>(&std::fs::read(&report).unwrap()).unwrap(),
        json!({"ok": false, "errors": ["Review must return exactly one comment or noop"]})
    );
}

/// CI's e2e-review agent, scripted by .github/agentic-job/e2e/review.sh,
/// posts a verdict check accepts, naming the head it was run at, which
/// that job looks for on the pull request.
#[test]
fn the_scripted_review_names_the_checked_out_sha() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let home = tempfile::tempdir().unwrap();
    let status = Command::new("sh")
        .arg(root.join(".github/agentic-job/e2e/review.sh"))
        .env("HOME", home.path())
        .status()
        .unwrap();
    assert!(status.success());
    std::fs::create_dir(home.path().join("out")).unwrap();
    let script: Value = serde_json::from_slice(
        &std::fs::read(home.path().join(".config/fake-agent/demo.json")).unwrap(),
    )
    .unwrap();
    let command = script
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|step| step["execute"]["command"].as_str())
        .find(|command| command.contains("safe-outputs.jsonl"))
        .unwrap();
    let ran = Command::new("sh")
        .args(["-c", command])
        .current_dir(&root)
        .env("HOME", home.path())
        .output()
        .unwrap();
    assert!(ran.status.success(), "{ran:?}");
    let line = std::fs::read_to_string(home.path().join("out/safe-outputs.jsonl")).unwrap();
    let item: Value = serde_json::from_str(&line).unwrap();
    let sha = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&root)
        .output()
        .unwrap();
    let sha = String::from_utf8(sha.stdout).unwrap();
    let body = item["body"].as_str().unwrap();
    assert!(
        body.contains(&format!("\nReviewed SHA: {}\n", sha.trim())),
        "{body}"
    );
    let collected = home.path().join("agent_output.json");
    std::fs::write(
        &collected,
        json!({"items": [item], "errors": []}).to_string(),
    )
    .unwrap();
    let verdict = Command::new(env!("CARGO_BIN_EXE_agentic-job"))
        .args(["review", "verdict", "--target", "147", "--collected"])
        .arg(&collected)
        .arg("--report")
        .arg(home.path().join("report.json"))
        .output()
        .unwrap();
    assert!(verdict.status.success(), "{verdict:?}");
    assert_eq!(verdict.stdout, b"APPROVE\n");
}
