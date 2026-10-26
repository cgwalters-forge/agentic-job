//! `workflows check` as a repository's check runs it: over a tree, with
//! its exemptions file.

use std::path::Path;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_agentic-job");
const ERROR: i32 = 2;

/// The status comment keeps the agent's cost only in the run action's
/// grammar, which cannot end the HTML comment it goes in.
#[test]
fn status_cost_is_optional_and_bounded() {
    use agentic_job::apply::status::{MARKER, body};
    let long = format!("usd={} partial=false", "9".repeat(100));
    for (cost, wanted) in [
        ("usd=0 partial=false", true),
        ("usd=1.2345 partial=false", true),
        ("usd=2.5 partial=true", true),
        ("usd=-1 partial=false", false),
        ("usd=NaN partial=false", false),
        ("usd=1 partial=null", false),
        ("usd=1 partial=false\n", false),
        ("usd=1 partial=false --> forged", false),
        ("usd=1. partial=false", false),
        (long.as_str(), false),
    ] {
        let text = body("finished", None, Some(cost), "100").unwrap();
        let line = format!("<!-- agentic-job-cost: 100 {cost} -->");
        assert_eq!(text.contains("agentic-job-cost:"), wanted, "{cost:?}");
        assert_eq!(text.contains(&line), wanted, "{cost:?}");
        assert!(text.ends_with(&format!("<!-- agentic-job-run: 100 -->\n\n{MARKER}")));
    }
    assert!(
        !body("finished", None, None, "101")
            .unwrap()
            .contains("agentic-job-cost:")
    );
}

/// Without an event naming the item, policy.yml's notify job comments
/// only on a numeric comment target: never `*`, nothing, or shell text.
#[test]
fn notify_takes_only_a_numeric_target_without_an_event() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let text = std::fs::read_to_string(root.join(".github/workflows/policy.yml")).unwrap();
    let doc = &yaml_rust2::YamlLoader::load_from_str(&text).unwrap()[0];
    let script = doc["jobs"]["notify"]["steps"]
        .as_vec()
        .unwrap()
        .iter()
        .find(|step| step["name"].as_str() == Some("What to react to, and where to comment"))
        .unwrap()["run"]
        .as_str()
        .unwrap();
    for event in [None, Some(r#"{"item": null}"#)] {
        for target in ["7", "*", "", "0", "$(exit 9)"] {
            let dir = tempfile::tempdir().unwrap();
            let output = dir.path().join("output");
            std::fs::write(&output, "").unwrap();
            if let Some(event) = event {
                std::fs::write(dir.path().join("event.json"), event).unwrap();
            }
            let status = Command::new("bash")
                .args(["-eo", "pipefail", "-c", script])
                .env("JOB_DIR", dir.path())
                .env("TARGET", target)
                .env("GITHUB_OUTPUT", &output)
                .status()
                .unwrap();
            let numeric = target == "7";
            assert_eq!(status.success(), numeric, "{event:?} {target:?}");
            assert_eq!(
                std::fs::read_to_string(&output).unwrap(),
                if numeric { "item=7\nwhat=item\n" } else { "" },
                "{event:?} {target:?}"
            );
        }
    }
}

/// A task passed as `task-artifact` is downloaded before the mode check,
/// refused beside `task` or when empty, and becomes the policy's task.md;
/// the composed example passes its task that way, as CI's e2e-compose.
#[test]
fn policy_takes_the_task_as_an_artifact() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let text = std::fs::read_to_string(root.join(".github/workflows/policy.yml")).unwrap();
    let doc = &yaml_rust2::YamlLoader::load_from_str(&text).unwrap()[0];
    let steps = doc["jobs"]["policy"]["steps"].as_vec().unwrap();
    let index = |name: &str| {
        steps
            .iter()
            .position(|step| step["name"].as_str() == Some(name))
            .unwrap()
    };
    let gate = index("Check the proposals mode");
    let download = steps
        .iter()
        .position(|step| step["with"]["name"].as_str() == Some("${{ inputs.task-artifact }}"))
        .unwrap();
    assert!(download < gate);
    assert_eq!(
        steps[download]["with"]["path"].as_str(),
        Some("${{ runner.temp }}/task-artifact")
    );
    let script = |index: usize| steps[index]["run"].as_str().unwrap();
    let write = index("Write the task and the run's settings");
    // [task, task-artifact, task.md in it, proposals, event, status]
    for (task, artifact, file, proposals, event, success) in [
        ("", "task", Some("run\n"), "", "false", true),
        ("", "$(exit 0)", Some("run\n"), "", "false", true),
        ("", "task", Some("run\n"), "", "true", true),
        ("run", "task", Some("run\n"), "", "false", false),
        ("", "task", Some(""), "", "false", false),
        ("", "task", Some(" \n"), "", "false", false),
        ("", "task", None, "", "false", false),
        ("", "task", None, "proposals", "false", false),
        ("run", "", None, "", "false", true),
        ("", "", None, "", "false", false),
        ("", "", None, "proposals", "false", true),
    ] {
        let case = format!("{task:?} {artifact:?} {file:?} {proposals:?} {event:?}");
        let dir = tempfile::tempdir().unwrap();
        if let Some(file) = file {
            std::fs::create_dir(dir.path().join("task-artifact")).unwrap();
            std::fs::write(dir.path().join("task-artifact/task.md"), file).unwrap();
        }
        let output = Command::new("bash")
            .args(["-eo", "pipefail", "-c", script(gate)])
            .env("RUNNER_TEMP", dir.path())
            .env("TASK", task)
            .env("TASK_ARTIFACT", artifact)
            .env("PROPOSALS", proposals)
            .env("EVENT", event)
            .env("REVIEW", "false")
            .env("PARTIAL", "false")
            .output()
            .unwrap();
        assert_eq!(output.status.success(), success, "{case}: {output:?}");
        if task.is_empty() && artifact.is_empty() && proposals.is_empty() {
            assert!(String::from_utf8_lossy(&output.stdout).contains("secret masker"));
        }
    }
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("task-artifact")).unwrap();
    std::fs::write(dir.path().join("task-artifact/task.md"), "a task\n").unwrap();
    let status = Command::new("bash")
        .args(["-eo", "pipefail", "-c", script(write)])
        .env("RUNNER_TEMP", dir.path())
        .env("JOB_DIR", dir.path())
        .env("EVENT", "false")
        .env("TASK", "")
        .env("TASK_ARTIFACT", "task")
        .env("PROFILE", "")
        .env("ISSUE", "")
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(
        std::fs::read_to_string(dir.path().join("task.md")).unwrap(),
        "a task\n"
    );
    let compose =
        std::fs::read_to_string(root.join(".github/workflows/example-compose.yml")).unwrap();
    let compose = &yaml_rust2::YamlLoader::load_from_str(&compose).unwrap()[0];
    let upload = &compose["jobs"]["task"]["steps"][1]["with"];
    let policy = &compose["jobs"]["policy"];
    assert_eq!(policy["needs"].as_str(), Some("task"));
    assert_eq!(upload["name"].as_str(), Some("compose-task"));
    assert_eq!(upload["path"].as_str(), Some("task/task.md"));
    assert_eq!(
        policy["with"]["task-artifact"].as_str(),
        Some("compose-task")
    );
    assert!(policy["with"]["task"].is_badvalue());
}

#[test]
fn run_action_forwards_summary_cost_without_inventing_unknown_cost() {
    use serde_json::json;
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let action = std::fs::read_to_string(root.join("run/action.yml")).unwrap();
    let line = action
        .lines()
        .find(|line| line.contains("jq -r '.cost |"))
        .unwrap()
        .trim();
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("out/run")).unwrap();
    let mut produced = Vec::new();
    for (cost, expected) in [
        (
            json!({"usd": 1.2345, "partial": false}),
            "cost=usd=1.2345 partial=false\n",
        ),
        (
            json!({"usd": 0, "partial": true}),
            "cost=usd=0 partial=true\n",
        ),
        (json!({"usd": null, "partial": true}), ""),
        (json!({"usd": -1, "partial": false}), ""),
        (json!({"usd": "1\nforged=1", "partial": false}), ""),
        (json!({"usd": 1, "partial": null}), ""),
        (serde_json::Value::Null, ""),
        // What jq would otherwise print as a 17-digit fraction or an
        // exponent, which the status grammar refuses.
        (
            json!({"usd": 0.12345678901234567_f64, "partial": false}),
            "cost=usd=0.1235 partial=false\n",
        ),
        (
            json!({"usd": 0.0000001, "partial": true}),
            "cost=usd=0 partial=true\n",
        ),
        (
            json!({"usd": 0.29, "partial": false}),
            "cost=usd=0.29 partial=false\n",
        ),
        (
            json!({"usd": 123456789.98765, "partial": false}),
            "cost=usd=123456789.9877 partial=false\n",
        ),
        (json!({"usd": 1e12, "partial": false}), ""),
    ] {
        std::fs::write(
            dir.path().join("out/run/summary.json"),
            json!({"cost":cost}).to_string(),
        )
        .unwrap();
        let output_file = dir.path().join("outputs");
        std::fs::write(&output_file, "").unwrap();
        let output = Command::new("bash")
            .args(["-ec", line])
            .env("JOB_DIR", dir.path())
            .env("GITHUB_OUTPUT", &output_file)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            std::fs::read_to_string(output_file).unwrap(),
            expected,
            "{cost}"
        );
        if let Some(value) = expected.strip_prefix("cost=") {
            produced.push(value.trim_end().to_owned());
        }
    }
    // Everything the producer forwards is a cost the status comment keeps.
    for cost in produced {
        let text = agentic_job::apply::status::body("finished", None, Some(&cost), "100").unwrap();
        assert!(
            text.contains(&format!("<!-- agentic-job-cost: 100 {cost} -->")),
            "{cost}"
        );
    }
    let agent = std::fs::read_to_string(root.join(".github/workflows/agent.yml")).unwrap();
    assert!(agent.contains("cost: ${{ steps.run.outputs.cost }}"));
    assert!(agent.contains("value: ${{ jobs.agent.outputs.cost }}"));
    let apply = std::fs::read_to_string(root.join(".github/workflows/apply.yml")).unwrap();
    assert!(apply.contains("COST: ${{ fromJSON(inputs.agent).outputs.cost }}"));
}

/// check.yml hands apply the patches check reported, each by its name:
/// the secondary pull request's apart, in `secondary/`, so that apply's
/// trial of the run's own patch, which takes the one `check` reported,
/// never applies it. Its request is left to `secondary-pull-request`, out
/// of the handlers' file, which would otherwise fail it.
#[test]
fn the_secondary_patch_is_handed_on_apart() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let text = std::fs::read_to_string(root.join(".github/workflows/check.yml")).unwrap();
    let doc = &yaml_rust2::YamlLoader::load_from_str(&text).unwrap()[0];
    let steps = doc["jobs"]["check"]["steps"].as_vec().unwrap();
    let step = |name: &str| {
        steps
            .iter()
            .find(|step| step["name"].as_str() == Some(name))
            .unwrap()
    };
    let script = step("Put the patch beside the ingested outputs")["run"]
        .as_str()
        .unwrap();
    let uploaded = steps
        .iter()
        .find(|step| step["id"].as_str() == Some("upload"))
        .unwrap()["with"]["path"]
        .as_str()
        .unwrap();
    for file in ["secondary/aw-*.patch", "replacement.json", "policy.json"] {
        assert!(uploaded.contains(&format!("${{{{ env.GH_AW_TMP }}}}/{file}\n")));
    }
    const OWN: &str = "aw-agent-run-7.patch";
    const TOOL: &str = "aw-o-tool-agentic-job-agent-run-7.patch";
    // A replacement series is reported, and read by apply, by this name.
    const SERIES: &str = "replacement.json";
    let own_request = serde_json::json!({"type": "create_pull_request", "title": "T"});
    let tool_request = serde_json::json!({"type": "create_pull_request", "repo": "o/tool"});
    let items = serde_json::json!([own_request, tool_request]);
    let secondary = serde_json::json!({"secondary_pull_request": {"repo": "o/tool"}});
    // (report, policy, the patch reported for the run's own request, whether
    // the secondary's is handed on, the requests the handlers get)
    for (report, policy, own, tool, handled) in [
        (
            serde_json::json!({"items": items, "patch": {"file": OWN},
                "secondary_patch": {"file": TOOL}}),
            &secondary,
            Some(OWN),
            true,
            serde_json::json!([own_request]),
        ),
        (
            serde_json::json!({"items": [tool_request], "secondary_patch": {"file": TOOL}}),
            &secondary,
            None,
            true,
            serde_json::json!([]),
        ),
        (
            serde_json::json!({"items": [own_request], "patch": {"file": OWN}}),
            &secondary,
            Some(OWN),
            false,
            serde_json::json!([own_request]),
        ),
        (
            serde_json::json!({"items": [], "patch": {"file": SERIES}}),
            &serde_json::json!({}),
            Some(SERIES),
            false,
            serde_json::json!([]),
        ),
        // Without a secondary in the policy, every request is the handlers'.
        (
            serde_json::json!({"items": items, "patch": {"file": OWN}}),
            &serde_json::json!({}),
            Some(OWN),
            false,
            items.clone(),
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let (job, tmp) = (dir.path().join("job"), dir.path().join("gh-aw"));
        std::fs::create_dir_all(job.join("safe-outputs")).unwrap();
        std::fs::create_dir(&tmp).unwrap();
        for name in [OWN, TOOL, SERIES] {
            std::fs::write(job.join("safe-outputs").join(name), name).unwrap();
        }
        std::fs::write(job.join("policy.json"), policy.to_string()).unwrap();
        std::fs::write(tmp.join("report.json"), report.to_string()).unwrap();
        let output = Command::new("bash")
            .args(["-e", "-c", script])
            .env("JOB_DIR", &job)
            .env("GH_AW_TMP", &tmp)
            .env("RUNNER_TEMP", dir.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        for name in [OWN, SERIES] {
            assert_eq!(tmp.join(name).exists(), own == Some(name), "{report}");
        }
        assert!(!tmp.join(TOOL).exists(), "{report}");
        assert_eq!(tmp.join("secondary").join(TOOL).exists(), tool, "{report}");
        let output: serde_json::Value =
            serde_json::from_slice(&std::fs::read(tmp.join("agent_output.json")).unwrap()).unwrap();
        assert_eq!(output, serde_json::json!({"items": handled, "errors": []}));
        assert_eq!(
            std::fs::read_to_string(tmp.join("policy.json")).unwrap(),
            policy.to_string()
        );
    }
    // Apply's trial takes the one patch check reported for the run's own
    // request, not every file a glob matches.
    let apply = std::fs::read_to_string(root.join(".github/workflows/apply.yml")).unwrap();
    assert!(apply.contains(r#"am --keep-cr "$GH_AW_TMP/$(jq -er '.patch.file' "$REPORT")""#));
    assert!(!apply.contains("aw-*.patch"));
    let doc = &yaml_rust2::YamlLoader::load_from_str(&apply).unwrap()[0];
    let steps = doc["jobs"]["apply"]["steps"].as_vec().unwrap();
    let position = |name: &str| {
        steps
            .iter()
            .position(|step| step["name"].as_str() == Some(name))
            .unwrap()
    };
    let open = &steps[position("Open the secondary pull request")];
    assert!(position("Every output was applied") < position("Open the secondary pull request"));
    assert!(
        open["run"]
            .as_str()
            .unwrap()
            .contains("secondary-pull-request --policy \"$GH_AW_TMP/policy.json\"")
    );
    // A user's token, which can fork; never the job token in its place.
    assert_eq!(
        open["env"]["SAFE_OUTPUTS_PAT"].as_str(),
        Some("${{ secrets.SAFE_OUTPUTS_PAT }}")
    );
}

#[test]
fn workflow_budgets_convert_dollars_and_keep_explicit_legacy_aic() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for name in ["agentic-job", "policy"] {
        let text =
            std::fs::read_to_string(root.join(format!(".github/workflows/{name}.yml"))).unwrap();
        let doc = &yaml_rust2::YamlLoader::load_from_str(&text).unwrap()[0];
        let inputs = &doc["on"]["workflow_call"]["inputs"];
        assert_eq!(inputs["budget-usd"]["default"].as_str(), Some(""));
        assert_eq!(inputs["budget"]["default"].as_str(), Some("500"));
        if name == "policy" {
            assert!(text.contains("BUDGET: ${{ inputs.budget-usd == '' && inputs.budget || '' }}"));
            assert!(text.contains("BUDGET_USD: ${{ inputs.budget-usd }}"));
            assert!(text.contains("--integer limits.budget=\"$BUDGET\""));
            assert!(text.contains("--budget-usd \"$BUDGET_USD\""));
        } else {
            assert_eq!(
                doc["jobs"]["policy"]["with"]["budget"].as_str(),
                Some("${{ inputs.budget }}")
            );
            assert_eq!(
                doc["jobs"]["policy"]["with"]["budget-usd"].as_str(),
                Some("${{ inputs.budget-usd }}")
            );
        }
    }
    // Composed as the policy job does, from a file that caps at $1: an
    // unchanged caller, `budget: ""` included, keeps its cap.
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("config.toml");
    std::fs::write(
        &file,
        "[agent]\nname = \"fake\"\n[limits]\ntimeout-minutes = 10\nbudget = 100\n",
    )
    .unwrap();
    for (usd, aic, expected) in [
        ("", "500", 500),
        ("", "", 100),
        ("", "300", 300),
        ("", "0", 0),
        ("2", "500", 200),
        ("0.5", "", 50),
        ("1.239", "900", 123),
    ] {
        let budget = if usd.is_empty() { aic } else { "" };
        let uncapped = if expected == 0 { "true" } else { "" };
        let output = Command::new(BIN)
            .arg("config")
            .arg("--from")
            .arg(&file)
            .args(["--boolean", &format!("limits.uncapped={uncapped}")])
            .args(["--integer", &format!("limits.budget={budget}")])
            .args(["--budget-usd", usd])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let table: toml::Table = toml::from_str(&String::from_utf8_lossy(&output.stdout)).unwrap();
        assert_eq!(
            table["limits"]["budget"].as_integer(),
            Some(expected),
            "budget-usd {usd:?}, budget {aic:?}"
        );
    }
}

const PIN: &str = "0123456789abcdef0123456789abcdef01234567";

fn passing() -> String {
    format!(
        "on: push
jobs:
  build:
    permissions: {{contents: read}}
    runs-on: ubuntu-26.04
    steps:
      - uses: actions/checkout@{PIN}
        with: {{persist-credentials: false}}
      - uses: cgwalters-forge/agentic-job/secure-host@{PIN}
      - run: make check
"
    )
}

const FAILING: &str = "on: push
jobs:
  build:
    runs-on: ubuntu-26.04
    steps:
      - name: Fetch
        uses: actions/checkout@v7
";

fn check(root: &Path, extra: &[&str]) -> Output {
    Command::new(BIN)
        .args(["workflows", "check", "--root"])
        .arg(root)
        .args(extra)
        .output()
        .expect("test setup")
}

/// A repository holding `files`, by path, and the result of checking it.
fn checked(files: &[(&str, &str)], extra: &[&str]) -> (i32, String, String) {
    let dir = tempfile::tempdir().expect("test setup");
    for (path, text) in files {
        let path = dir.path().join(path);
        std::fs::create_dir_all(path.parent().expect("test setup")).expect("test setup");
        std::fs::write(path, text).expect("test setup");
    }
    let extra: Vec<String> = extra
        .iter()
        .map(|a| a.replace("ROOT", dir.path().to_str().expect("test setup")))
        .collect();
    let extra: Vec<&str> = extra.iter().map(String::as_str).collect();
    let output = check(dir.path(), &extra);
    (
        output.status.code().expect("test setup"),
        String::from_utf8(output.stdout).expect("test setup"),
        String::from_utf8(output.stderr).expect("test setup"),
    )
}

const LIST: &str = ".github/agentic-job/workflows.toml";

/// Files by path, extra arguments, the exit status, and what stdout or
/// stderr must say.
type Case<'a> = (&'a [(&'a str, &'a str)], &'a [&'a str], i32, &'a [&'a str]);

#[test]
fn trees() {
    let good = passing();
    let cases: &[Case] = &[
        (&[], &[], 0, &["0 problem(s)"]),
        (&[(".github/workflows/a.yml", &good)], &[], 0, &[]),
        (
            &[
                (".github/workflows/a.yml", &good),
                (".github/workflows/b.yaml", FAILING),
                (".github/workflows/notes.txt", "not a workflow"),
                (".github/workflows/sub/c.yml", FAILING),
            ],
            &[],
            1,
            &[
                ".github/workflows/b.yaml: job build has no `permissions:` map",
                ".github/workflows/b.yaml: job build neither runs secure-host",
                "2 problem(s); 2 workflow file(s) checked, 0 exempt",
            ],
        ),
        (
            &[
                (".github/workflows/b.yml", FAILING),
                (LIST, "[exempt]\n\"b.yml\" = \"tests the checker\"\n"),
            ],
            &[],
            0,
            &[
                "b.yml: exempt: tests the checker",
                "0 workflow file(s) checked, 1 exempt",
            ],
        ),
        (
            &[
                (".github/workflows/b.yml", FAILING),
                (LIST, "[exempt]\n\"b.yml\" = \"from the proposal\"\n"),
                ("base.toml", "[exempt]\n"),
            ],
            &["--exemptions", "ROOT/base.toml"],
            1,
            &["job build neither runs secure-host"],
        ),
        (
            &[(LIST, "[exempt]\n\"gone.yml\" = \"was here\"\n")],
            &[],
            1,
            &["exempts gone.yml, which is not in .github/workflows"],
        ),
        (
            &[
                (".github/workflows/b.yml", FAILING),
                (LIST, "[exempt]\n\"b.yml\" = \"  \"\n"),
            ],
            &[],
            ERROR,
            &["b.yml has no reason"],
        ),
        (
            &[(LIST, "[exempt]\n\"../b.yml\" = \"why\"\n")],
            &[],
            ERROR,
            &["not a workflow file's name"],
        ),
        (
            &[(LIST, "exempt = []\nother = 1\n")],
            &[],
            ERROR,
            &["parsing"],
        ),
        (
            &[],
            &["--exemptions", "ROOT/missing.toml"],
            ERROR,
            &["missing.toml"],
        ),
        (
            &[(".github/workflows/a.yml", "jobs: [\n")],
            &[],
            1,
            &[".github/workflows/a.yml: cannot be checked"],
        ),
    ];
    for (files, extra, code, wants) in cases {
        let (got, stdout, stderr) = checked(files, extra);
        let all = format!("{stdout}{stderr}");
        assert_eq!(got, *code, "{files:?} {extra:?}: {all}");
        for want in *wants {
            assert!(all.contains(want), "{files:?}: no {want:?} in {all}");
        }
    }
}

#[test]
fn a_linked_workflow_is_not_followed() {
    let dir = tempfile::tempdir().unwrap();
    let workflows = dir.path().join(".github/workflows");
    std::fs::create_dir_all(&workflows).unwrap();
    std::fs::write(dir.path().join("elsewhere.yml"), passing()).unwrap();
    std::os::unix::fs::symlink("../../elsewhere.yml", workflows.join("a.yml")).unwrap();
    let output = check(dir.path(), &[]);
    assert_eq!(output.status.code(), Some(1));
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("a.yml: cannot be checked: is not a regular file"),
        "{stdout}"
    );
}

/// The jobs under agentic-job.yml that can mint identity tokens when the
/// caller grants the call `id-token: write`, as docs/workflow.md's
/// permissions paragraph names them. A job that names no `permissions`
/// keeps the call's grant, so a new such job, or a call that stops
/// narrowing, changes this list and must change the documentation too.
#[test]
fn id_token_holders_are_the_documented_ones() {
    fn walk(dir: &Path, file: &str, holds: bool, found: &mut Vec<String>) {
        let text = std::fs::read_to_string(dir.join(file)).unwrap();
        let doc = &yaml_rust2::YamlLoader::load_from_str(&text).unwrap()[0];
        for (name, job) in doc["jobs"].as_hash().unwrap() {
            let name = name.as_str().unwrap();
            let permissions = &job["permissions"];
            // Named permissions only narrow what the call was granted.
            let holds = holds
                && (permissions.is_badvalue()
                    || permissions.as_str() == Some("write-all")
                    || permissions["id-token"].as_str() == Some("write"));
            match job["uses"].as_str() {
                Some(called) => {
                    let called = called.strip_prefix("./.github/workflows/").unwrap();
                    walk(dir, called, holds, found);
                }
                None if holds => found.push(format!("{file}:{name}")),
                None => {}
            }
        }
    }
    let dir = Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../.github/workflows"
    ));
    let mut found = Vec::new();
    walk(dir, "agentic-job.yml", true, &mut found);
    found.sort();
    assert_eq!(
        found,
        [
            "agentic-job.yml:register",
            "apply.yml:apply",
            "apply.yml:conclude",
            "check.yml:detect",
            "policy.yml:activate",
            "policy.yml:notify",
        ]
    );
}

/// This repository's own workflows pass or are listed, read at test time.
#[test]
fn this_repository_passes() {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
    let output = check(Path::new(root), &[]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}
