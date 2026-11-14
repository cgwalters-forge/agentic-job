//! Standalone port of the legacy corpus's check-run admission cases.

use serde_json::{Value, json};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_agentic-job");

#[test]
fn collected_check_runs_keep_the_pinned_name_and_cannot_report_success() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path();
    let bounds = dir.join("allow.toml");
    std::fs::write(
        &bounds,
        r#"hosts = ["github.com"]
repos = ["owner/repo"]
bases = ["main"]
max_outputs = 2
max_patch_bytes = 100000
max_patch_files = 10
[github.outputs]
create_check_run = { max = 1, name = "agent verdict" }
"#,
    )
    .unwrap();
    let policy = Command::new(BIN)
        .args(["policy", "--allow"])
        .arg(&bounds)
        .args([
            "--repo",
            "owner/repo",
            "--clone-url",
            "https://github.com/owner/repo",
            "--base",
            "main",
            "--kind",
            "analysis",
            "--outputs",
            "create_check_run",
            "--max-outputs",
            "2",
        ])
        .output()
        .unwrap();
    assert!(policy.status.success(), "{policy:?}");
    let document: Value = serde_json::from_slice(&policy.stdout).unwrap();
    assert_eq!(
        document["safe_outputs"]["create_check_run"],
        json!({"max": 1, "name": "agent verdict"})
    );
    let policy_path = dir.join("policy.json");
    std::fs::write(&policy_path, policy.stdout).unwrap();
    let out = dir.join("out");
    std::fs::create_dir(&out).unwrap();
    let collected = dir.join("collected.json");
    let report = dir.join("report.json");
    let item = json!({"type": "create_check_run", "conclusion": "neutral", "title": "Verdict", "summary": "Looked.", "pull_request_number": 7});
    for (name, fields, remove_target, count, accepted) in [
        ("accepted", json!({}), false, 1, true),
        (
            "pinned name",
            json!({"name": "agent verdict"}),
            false,
            1,
            true,
        ),
        (
            "wrong name",
            json!({"name": "required-ci"}),
            false,
            1,
            false,
        ),
        ("success", json!({"conclusion": "success"}), false, 1, false),
        ("no pull request", json!({}), true, 1, false),
        ("target alias", json!({"pr_number": 7}), true, 1, false),
        ("over count", json!({}), false, 2, false),
    ] {
        let mut request = item.clone();
        let object = request.as_object_mut().unwrap();
        object.extend(fields.as_object().unwrap().clone());
        if remove_target {
            object.remove("pull_request_number");
        }
        std::fs::write(
            out.join("outputs.jsonl"),
            format!("{request}\n").repeat(count),
        )
        .unwrap();
        let ingest = Command::new(BIN)
            .args(["collect", "--policy"])
            .arg(&policy_path)
            .arg("--outputs")
            .arg(&out)
            .arg("--collected")
            .arg(&collected)
            .output()
            .unwrap();
        assert_eq!(
            ingest.status.code(),
            Some(i32::from(count > 1)),
            "{name}: {ingest:?}"
        );
        let checked = Command::new(BIN)
            .args(["check", "--policy"])
            .arg(&policy_path)
            .arg("--outputs")
            .arg(&out)
            .arg("--collected")
            .arg(&collected)
            .arg("--report")
            .arg(&report)
            .output()
            .unwrap();
        let verdict: Value = serde_json::from_slice(&std::fs::read(&report).unwrap()).unwrap();
        assert_eq!(
            checked.status.code(),
            Some(i32::from(!accepted)),
            "{name}: {checked:?}; {verdict}"
        );
        assert_eq!(verdict["ok"], accepted, "{name}: {verdict}");
        if accepted {
            assert_eq!(
                verdict["items"].as_array().unwrap().len(),
                count,
                "{name}: {verdict}"
            );
            assert_eq!(verdict["errors"], json!([]), "{name}: {verdict}");
        } else {
            assert!(
                !verdict["errors"].as_array().unwrap().is_empty(),
                "{name}: {verdict}"
            );
        }
    }
}
