//! `agentic-job check` as the check job runs it: `policy`'s own output,
//! a hand-back that git made (`data/check/handback`), and the exit states.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_agentic-job");

fn data(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name)
}

/// A scratch directory holding the policy of a run that may open a pull
/// request, and a copy of the hand-back.
struct Run {
    dir: tempfile::TempDir,
}

impl Run {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("a scratch directory");
        let policy = Command::new(BIN)
            .args([
                "policy",
                "--repo",
                "bootc-dev/bootc",
                "--base",
                "main",
                "--kind",
                "branch",
            ])
            .args(["--clone-url", "https://github.com/bootc-dev/bootc"])
            .args([
                "--outputs",
                "create_pull_request",
                "--max-outputs",
                "1",
                "--allow",
            ])
            .arg(data("policy/allow.toml"))
            .output()
            .expect("running agentic-job");
        assert!(
            policy.status.success(),
            "{}",
            String::from_utf8_lossy(&policy.stderr)
        );
        std::fs::write(dir.path().join("policy.json"), policy.stdout).expect("writing the policy");
        let out = dir.path().join("out");
        std::fs::create_dir(&out).expect("making the outputs directory");
        for entry in
            std::fs::read_dir(data("check/handback")).expect("the hand-back of the test data")
        {
            let entry = entry.expect("an entry of the test data");
            std::fs::copy(entry.path(), out.join(entry.file_name()))
                .expect("copying the hand-back");
        }
        Self { dir }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn check(&self, collected: &Path) -> Output {
        Command::new(BIN)
            .arg("check")
            .arg("--policy")
            .arg(self.path("policy.json"))
            .arg("--outputs")
            .arg(self.path("out"))
            .arg("--collected")
            .arg(collected)
            .arg("--report")
            .arg(self.path("verdict.json"))
            .output()
            .expect("running agentic-job")
    }

    fn verdict(&self) -> Value {
        let report = std::fs::read(self.path("verdict.json")).expect("the report was written");
        serde_json::from_slice(&report).expect("the report is JSON")
    }
}

#[test]
fn a_hand_back_within_the_policy_is_accepted() {
    let run = Run::new();
    let output = run.check(&data("check/collected.json"));
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).starts_with("### Safe outputs: accepted\n"));
    let verdict = run.verdict();
    assert_eq!(verdict["ok"], true);
    assert_eq!(verdict["errors"], serde_json::json!([]));
    assert_eq!(verdict["items"][0]["title"], "Fix the thing");
    assert_eq!(verdict["patch"]["file"], "aw-agent-run-7.patch");
    assert_eq!(
        verdict["patch"]["files"],
        serde_json::json!(["src/lib.rs", "src/new.rs"])
    );
    assert_eq!(
        verdict["patch"]["base_commit"],
        "5fffa916418485beea9050be41cc38857f49c3a1"
    );
}

#[test]
fn a_hand_back_outside_the_policy_is_refused_with_its_reasons() {
    let run = Run::new();
    std::fs::write(run.path("out/notes.txt"), "hi").unwrap();
    let output = run.check(&data("check/collected.json"));
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("error: unexpected file \"notes.txt\""),
        "{stderr}"
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("- refused: unexpected file"));
    let verdict = run.verdict();
    assert_eq!(verdict["ok"], false);
    assert_eq!(
        verdict["errors"],
        serde_json::json!(["unexpected file \"notes.txt\""])
    );
}

#[test]
fn a_check_that_cannot_be_made_is_an_error_and_writes_no_verdict() {
    let run = Run::new();
    let not_json = run.path("collected.json");
    std::fs::write(&not_json, "the collector failed").unwrap();
    for (collected, want) in [
        (run.path("missing.json"), "the collector's result"),
        (not_json, "is not the result of gh-aw's collector"),
    ] {
        let output = run.check(&collected);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(2), "{stderr}");
        assert!(stderr.contains(want), "{stderr}");
        assert!(!run.path("verdict.json").exists());
    }
}
