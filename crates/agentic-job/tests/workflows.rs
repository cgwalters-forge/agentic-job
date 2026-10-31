//! `workflows check` as a repository's check runs it: over a tree, with
//! its exemptions file. And the workflows' own steps and shapes.
#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use regex::Regex;

const BIN: &str = env!("CARGO_BIN_EXE_agentic-job");
const ERROR: i32 = 2;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// A workflow basename or a repository-relative action, read at test time.
fn workflow(file: &str) -> String {
    let file = if file.contains('/') {
        root().join(file)
    } else {
        root().join(".github/workflows").join(file)
    };
    std::fs::read_to_string(file).unwrap()
}

/// The text of `name`, a job of `text`, up to the next job.
fn job<'a>(text: &'a str, name: &str) -> &'a str {
    let start = text
        .find(&format!("\n  {name}:\n"))
        .unwrap_or_else(|| panic!("no job {name}"))
        + 1;
    let rest = &text[start..];
    let end = Regex::new(r"\n  [a-z][\w-]*:\n")
        .unwrap()
        .find(rest)
        .map_or(rest.len(), |next| next.start() + 1);
    &rest[..end]
}

/// Workflows by basename, composite actions by repository-relative path.
fn definition(file: &str) -> yaml_rust2::Yaml {
    let text = workflow(file);
    yaml_rust2::YamlLoader::load_from_str(&text)
        .unwrap()
        .remove(0)
}

#[test]
fn ordinary_ubuntu_runners_use_current_image() {
    // Check owned definitions, not historical checker fixtures. Architecture
    // suffixes and special runner classes are deliberately outside this rule.
    let ordinary = Regex::new(r"^ubuntu-(latest|[0-9]+\.[0-9]+)$").unwrap();
    for entry in std::fs::read_dir(root().join(".github/workflows")).unwrap() {
        let path = entry.unwrap().path();
        if !matches!(
            path.extension().and_then(|s| s.to_str()),
            Some("yml" | "yaml")
        ) {
            continue;
        }
        let file = path.file_name().unwrap().to_str().unwrap();
        let doc = definition(file);
        for (name, job) in doc["jobs"].as_hash().unwrap() {
            if let Some(label) = job["runs-on"].as_str()
                && ordinary.is_match(label)
            {
                assert_eq!(label, "ubuntu-26.04", "{file}: {name:?}");
            }
        }
    }
    for file in ["agentic-job.yml", "policy.yml"] {
        assert_eq!(
            definition(file)["on"]["workflow_call"]["inputs"]["runner"]["default"].as_str(),
            Some("\"ubuntu-26.04\""),
            "{file}: runner default"
        );
    }
}

fn steps(doc: &yaml_rust2::Yaml) -> Vec<&yaml_rust2::Yaml> {
    if let Some(jobs) = doc["jobs"].as_hash() {
        jobs.values()
            .flat_map(|job| job["steps"].as_vec().into_iter().flatten())
            .collect()
    } else {
        doc["runs"]["steps"].as_vec().unwrap().iter().collect()
    }
}

fn named_step<'a>(doc: &'a yaml_rust2::Yaml, name: &str) -> &'a yaml_rust2::Yaml {
    let mut found = steps(doc)
        .into_iter()
        .filter(|step| step["name"].as_str() == Some(name));
    let step = found.next().unwrap_or_else(|| panic!("no step {name:?}"));
    assert!(found.next().is_none(), "two steps {name:?}");
    step
}

/// Runs the actual workflow or composite step, without job credentials or
/// user configuration from this process. Every invocation has a temporary HOME.
fn run_step(file: &str, name: &str, dir: &Path, env: &[(&str, &str)]) -> Output {
    let doc = definition(file);
    let script = named_step(&doc, name)["run"].as_str().unwrap();
    let mut command = Command::new("bash");
    command
        .args(["-eo", "pipefail", "-c", script])
        .current_dir(dir)
        .env_clear();
    command
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .env("HOME", dir);
    command.envs(env.iter().copied()).output().unwrap()
}

/// A stand-in for a command a step runs.
fn executable(path: &Path, text: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, text).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

fn path(path: &Path) -> &str {
    path.to_str().unwrap()
}

/// The directory of the binary under test, as the steps' `JOB_DIR`.
fn bin_dir() -> &'static str {
    Path::new(BIN).parent().unwrap().to_str().unwrap()
}

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

#[test]
fn prepare_requires_trusted_ids_before_download() {
    let dir = tempfile::tempdir().unwrap();
    for (binary, policy, ok) in [
        ("1", "2", true),
        ("", "2", false),
        ("1", "", false),
        ("0", "2", false),
        ("1,2", "3", false),
        ("1", "policy-name", false),
        ("$(exit 99)", "2", false),
    ] {
        let result = run_step(
            "prepare/action.yml",
            "Require trusted upload IDs",
            dir.path(),
            &[("BINARY", binary), ("POLICY", policy)],
        );
        assert_eq!(
            result.status.success(),
            ok,
            "{binary:?} {policy:?}: {result:?}"
        );
    }
    let doc = definition("prepare/action.yml");
    let steps = steps(&doc);
    let download = steps
        .iter()
        .position(|s| {
            s["uses"]
                .as_str()
                .is_some_and(|s| s.starts_with("actions/download-artifact@"))
        })
        .unwrap();
    assert!(
        steps
            .iter()
            .position(|s| s["name"].as_str() == Some("Require trusted upload IDs"))
            .unwrap()
            < download
    );
    assert_eq!(
        steps[download]["with"]["artifact-ids"].as_str(),
        Some("${{ inputs.binary-artifact-id }},${{ inputs.policy-artifact-id }}")
    );
    assert!(steps[download]["with"]["name"].is_badvalue());
}

#[test]
fn prepare_address_is_a_config_value_and_refusal_is_atomic() {
    use serde_json::json;
    for (address, ok) in [("100.64.0.1", true), ("--help", true), ("refused", false)] {
        let dir = tempfile::tempdir().unwrap();
        executable(
            &dir.path().join("agentic-job"),
            "#!/bin/bash\n[ \"$4\" != --inference-address=refused ] || exit 1\nprintf '%s\\n' \"$@\"\n",
        );
        std::fs::write(dir.path().join("config.toml"), "policy\n").unwrap();
        std::fs::write(dir.path().join("meta.json"), r#"{"item":"x"}"#).unwrap();
        let search = format!("{}:/usr/bin:/bin", dir.path().display());
        let result = run_step(
            "prepare/action.yml",
            "The inference broker's address",
            dir.path(),
            &[
                ("JOB_DIR", path(dir.path())),
                ("ADDRESS", address),
                ("PATH", &search),
            ],
        );
        assert_eq!(result.status.success(), ok, "{result:?}");
        let expected = if ok {
            format!(
                "config\n--from\n{}/config.toml\n--inference-address={address}\n",
                dir.path().display()
            )
        } else {
            "policy\n".into()
        };
        assert_eq!(
            std::fs::read_to_string(dir.path().join("config.toml")).unwrap(),
            expected
        );
        let meta: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.path().join("meta.json")).unwrap()).unwrap();
        assert_eq!(
            meta,
            if ok {
                json!({"item":"x", "inference_address":address})
            } else {
                json!({"item":"x"})
            }
        );
    }
    let doc = definition("prepare/action.yml");
    let step = named_step(&doc, "The inference broker's address");
    assert_eq!(
        step["if"].as_str(),
        Some("${{ inputs.inference-address != '' }}")
    );
    assert_eq!(
        step["env"]["ADDRESS"].as_str(),
        Some("${{ inputs.inference-address }}")
    );
}

#[test]
fn actions_refuse_source_skew_before_using_policy() {
    let dir = tempfile::tempdir().unwrap();
    let git = |args: &[&str]| {
        Command::new("git")
            .args(args)
            .current_dir(dir.path())
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", dir.path())
            .output()
            .unwrap()
    };
    assert!(git(&["init", "-q"]).status.success());
    assert!(
        git(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "source"
        ])
        .status
        .success()
    );
    let head = String::from_utf8(git(&["rev-parse", "HEAD"]).stdout).unwrap();
    let head = head.trim();
    let other = "b".repeat(40);
    let upper = head.to_uppercase();
    let newline = format!("{head}\n");
    let plain = tempfile::tempdir().unwrap();
    for (sha, action_ref, action_path, ok) in [
        (Some(head), "", dir.path(), true),
        (Some(other.as_str()), "", dir.path(), false),
        (Some(head), head, plain.path(), true),
        (Some(other.as_str()), head, plain.path(), false),
        (Some(head), other.as_str(), dir.path(), false),
        (Some(head), "", plain.path(), false),
        (Some(head), "main", plain.path(), false),
        (None, "", dir.path(), false),
        (Some(""), "", plain.path(), false),
        (Some(upper.as_str()), "", dir.path(), false),
        (Some(newline.as_str()), "", dir.path(), false),
    ] {
        let value = sha.map_or_else(
            || serde_json::json!({}),
            |sha| serde_json::json!({"source_sha":sha}),
        );
        std::fs::write(dir.path().join("run.json"), value.to_string()).unwrap();
        for file in ["prepare/action.yml", "run/action.yml"] {
            let result = run_step(
                file,
                "The policy call is this action's commit",
                dir.path(),
                &[
                    ("JOB_DIR", path(dir.path())),
                    ("ACTION_REF", action_ref),
                    ("GITHUB_ACTION_PATH", path(action_path)),
                    ("GIT_CEILING_DIRECTORIES", path(dir.path())),
                ],
            );
            assert_eq!(
                result.status.success(),
                ok,
                "{file} {sha:?} {action_ref}: {result:?}"
            );
        }
    }
}

#[test]
fn run_requires_locked_host_and_consistent_user_before_probing() {
    let doc = definition("run/action.yml");
    let guard = named_step(&doc, "Prove the host is secured")["run"]
        .as_str()
        .unwrap();
    let python = Regex::new("python3 -c '([^']+)'")
        .unwrap()
        .captures(guard)
        .unwrap()[1]
        .to_owned();
    for (sandbox, ok) in [
        ("{}", true),
        (r#"{"lock-runner":true}"#, true),
        (r#"{"lock-runner":false}"#, false),
        (r#"{"lock-runner":"true"}"#, false),
    ] {
        let replacement = format!(
            "__import__('json').loads('{}')",
            serde_json::json!({"sandbox":serde_json::from_str::<serde_json::Value>(sandbox).unwrap()})
        );
        let code = python.replace(
            "tomllib.load(open(\"/etc/agentic-job/config.toml\", \"rb\"))",
            &replacement,
        );
        let result = Command::new("python3")
            .args(["-c", &code])
            .env_clear()
            .output()
            .unwrap();
        assert_eq!(result.status.success(), ok, "{sandbox}: {result:?}");
    }
    for (user, home, message) in [
        ("runner", "", "agent-user runner requires runner-home"),
        (
            "sandbox",
            "/tmp/home",
            "agent-user sandbox takes no runner-home",
        ),
        ("", "/tmp/home", "agent-user must be runner or sandbox"),
        ("root", "", "agent-user must be runner or sandbox"),
        ("runner/home", "", "agent-user must be runner or sandbox"),
        ("runner", "/tmp/home", ""),
        ("sandbox", "", ""),
    ] {
        let dir = tempfile::tempdir().unwrap();
        executable(
            &dir.path().join("python3"),
            "#!/bin/sh\necho lock >&2\nexit 1\n",
        );
        executable(
            &dir.path().join("agentic-job"),
            "#!/bin/sh\necho unexpected\nexit 99\n",
        );
        let search = format!("{}:/usr/bin:/bin", dir.path().display());
        let result = run_step(
            "run/action.yml",
            "Prove the host is secured",
            dir.path(),
            &[
                ("PATH", &search),
                ("AGENT_USER", user),
                ("RUNNER_HOME", home),
            ],
        );
        assert_eq!(result.status.code(), Some(1), "{result:?}");
        assert_eq!(
            String::from_utf8_lossy(&result.stdout),
            if message.is_empty() {
                String::new()
            } else {
                format!("::error::{message}\n")
            }
        );
        assert_eq!(
            String::from_utf8_lossy(&result.stderr).contains("lock"),
            message.is_empty()
        );
    }
    let dir = tempfile::tempdir().unwrap();
    let result = run_step(
        "run/action.yml",
        "Install the built-in scripted session",
        dir.path(),
        &[
            ("AGENT_USER", "sandbox"),
            ("PROFILE", "comment"),
            ("RUNNER_HOME", ""),
        ],
    );
    assert_eq!(result.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&result.stdout),
        "::error::fake-profile requires agent-user runner\n"
    );
    assert_eq!(
        doc["inputs"]["agent-user"]["default"].as_str(),
        Some("runner")
    );
}

/// curl's mock refuses a token on argv and requires the authorization header
/// on stdin; it is shared by the scope and publication probes.
const MOCK_CURL: &str = r#"#!/bin/bash
read -r header
[ "$header" = "Authorization: Bearer $GH_TOKEN" ] || exit 8
case "$*" in *"$GH_TOKEN"*) exit 9 ;; esac
if [ -n "$HEADERS" ]; then printf '%s' "$HEADERS"; exit "$API_EXIT"; fi
if [ "${*: -1}" = "https://api.github.com/repos/$REPO" ]; then answer=$TARGET; else answer=$OTHER; fi
case "$answer" in
  public) echo '{"private":false,"visibility":"public"}' ;;
  private) echo '{"private":true,"visibility":"private"}' ;;
  unknown) echo '{"private":false,"visibility":"internal"}' ;;
esac
exit "$API_EXIT"
"#;

#[test]
fn run_refuses_classic_write_tokens_without_exposing_them() {
    for (token, headers, api_exit, ok) in [
        ("", "", "0", true),
        (
            "job-fixture",
            "HTTP/2 200\r\nx-ratelimit-limit: 5000\r\n",
            "0",
            true,
        ),
        (
            "classic-fixture",
            "HTTP/2 200\r\nx-oauth-scopes: \r\n",
            "0",
            true,
        ),
        (
            "classic-fixture",
            "HTTP/2 200\r\nX-OAuth-Scopes: read:org, read:user, user:email\r\n",
            "0",
            true,
        ),
        (
            "classic-fixture",
            "x-oauth-scopes: public_repo\r\n",
            "0",
            false,
        ),
        (
            "classic-fixture",
            "x-oauth-scopes: read:org, repo\r\n",
            "0",
            false,
        ),
        (
            "classic-fixture",
            "x-oauth-scopes: read:packages,write:packages\r\n",
            "0",
            false,
        ),
        ("classic-fixture", "x-oauth-scopes: gist\r\n", "0", false),
        ("classic-fixture", "x-oauth-scopes: *\r\n", "0", false),
        (
            "classic-fixture",
            "x-oauth-scopes: $(exit 0)\r\n",
            "0",
            false,
        ),
        ("classic-fixture", "", "22", false),
    ] {
        let dir = tempfile::tempdir().unwrap();
        executable(&dir.path().join("curl"), MOCK_CURL);
        let search = format!("{}:/usr/bin:/bin", dir.path().display());
        let result = run_step(
            "run/action.yml",
            "Refuse a GitHub token that can write",
            dir.path(),
            &[
                ("PATH", &search),
                ("GH_TOKEN", token),
                ("HEADERS", headers),
                ("API_EXIT", api_exit),
            ],
        );
        assert_eq!(result.status.success(), ok, "{headers:?}: {result:?}");
        let log = format!(
            "{}{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(
            !log.contains("classic-fixture") && !log.contains("job-fixture"),
            "{log}"
        );
    }
}

#[test]
fn visibility_admission_and_publication_fail_closed() {
    // Target, other repositories, target/other API answers, policy private,
    // policy repo, caller, API status, upload acceptance, admission acceptance.
    for (repo, repos, target, other, private, policy_repo, caller, api_exit, upload, admission) in [
        (
            "owner/repo",
            "",
            "public",
            "public",
            false,
            "owner/repo",
            "owner/caller",
            "0",
            true,
            true,
        ),
        (
            "owner/repo",
            "owner/config",
            "public",
            "public",
            false,
            "owner/repo",
            "owner/caller",
            "0",
            true,
            true,
        ),
        (
            "owner/repo",
            "",
            "private",
            "public",
            false,
            "owner/repo",
            "owner/repo",
            "0",
            false,
            true,
        ),
        (
            "owner/repo",
            "",
            "private",
            "public",
            true,
            "owner/repo",
            "owner/repo",
            "0",
            true,
            true,
        ),
        (
            "owner/repo",
            "",
            "private",
            "public",
            true,
            "owner/other",
            "owner/repo",
            "0",
            false,
            true,
        ),
        (
            "owner/repo",
            "",
            "private",
            "public",
            true,
            "owner/repo",
            "owner/caller",
            "0",
            false,
            false,
        ),
        (
            "owner/repo",
            "",
            "unknown",
            "public",
            true,
            "owner/repo",
            "owner/repo",
            "0",
            false,
            false,
        ),
        (
            "owner/repo",
            "owner/config",
            "private",
            "private",
            true,
            "owner/repo",
            "owner/repo",
            "0",
            false,
            false,
        ),
        (
            "owner/repo",
            "owner/repo",
            "private",
            "private",
            true,
            "owner/repo",
            "owner/repo",
            "0",
            false,
            false,
        ),
        (
            "$(exit 99)",
            "",
            "public",
            "public",
            false,
            "$(exit 99)",
            "owner/caller",
            "0",
            false,
            false,
        ),
        (
            "",
            "",
            "public",
            "public",
            false,
            "",
            "owner/caller",
            "0",
            false,
            false,
        ),
        (
            " ",
            "",
            "public",
            "public",
            false,
            " ",
            "owner/caller",
            "0",
            false,
            false,
        ),
        (
            "owner/repo",
            "invalid",
            "public",
            "public",
            false,
            "owner/repo",
            "owner/caller",
            "0",
            false,
            false,
        ),
        (
            "owner/repo",
            "owner/config owner/item",
            "unknown",
            "unknown",
            false,
            "owner/repo",
            "owner/caller",
            "0",
            false,
            false,
        ),
        (
            "owner/repo",
            "owner/config owner/item",
            "",
            "",
            false,
            "owner/repo",
            "owner/caller",
            "0",
            false,
            false,
        ),
        (
            "owner/repo",
            "owner/config owner/item",
            "public",
            "public",
            false,
            "owner/repo",
            "owner/caller",
            "1",
            false,
            false,
        ),
        (
            "owner/repo",
            "owner/config owner/item",
            "private",
            "private",
            false,
            "owner/repo",
            "owner/caller",
            "0",
            false,
            false,
        ),
        (
            "owner/repo",
            "owner/config",
            "public",
            "private",
            false,
            "owner/repo",
            "owner/caller",
            "0",
            false,
            false,
        ),
    ] {
        for (file, name, ok) in [
            (
                "run/action.yml",
                "Check again that the repositories are public",
                upload,
            ),
            (
                "policy.yml",
                "Check that the repositories are public",
                admission,
            ),
        ] {
            let dir = tempfile::tempdir().unwrap();
            executable(&dir.path().join("curl"), MOCK_CURL);
            executable(
                &dir.path().join("gh"),
                "#!/bin/bash\nif [ \"$2\" = \"repos/$REPO\" ]; then printf '%s\\n' \"$TARGET\"; else printf '%s\\n' \"$OTHER\"; fi\nexit \"$API_EXIT\"\n",
            );
            std::fs::write(
                dir.path().join("policy.json"),
                serde_json::json!({"repo":policy_repo,"private":private}).to_string(),
            )
            .unwrap();
            let output = dir.path().join("output");
            std::fs::write(&output, "").unwrap();
            let search = format!("{}:/usr/bin:/bin", dir.path().display());
            let result = run_step(
                file,
                name,
                dir.path(),
                &[
                    ("PATH", &search),
                    ("GH_TOKEN", "fixture-token"),
                    ("REPO", repo),
                    ("REPOS", repos),
                    ("TARGET", target),
                    ("OTHER", other),
                    ("API_EXIT", api_exit),
                    ("JOB_DIR", path(dir.path())),
                    ("GITHUB_REPOSITORY", caller),
                    ("GITHUB_OUTPUT", path(&output)),
                ],
            );
            assert_eq!(
                result.status.success(),
                ok,
                "{file} {repo:?} {target} {other} {caller}: {result:?}"
            );
            if file == "policy.yml" {
                let marked = target == "private" && repo == caller && api_exit == "0";
                assert_eq!(
                    std::fs::read_to_string(&output).unwrap(),
                    if marked { "private=true\n" } else { "" }
                );
            }
            if !ok && repo == "owner/repo" && repos != "invalid" {
                assert!(
                    String::from_utf8_lossy(&result.stdout).contains("private runs are refused"),
                    "{result:?}"
                );
                assert!(
                    String::from_utf8_lossy(&result.stdout)
                        .contains("docs/private-repositories.md")
                );
            }
        }
    }
}

#[test]
fn runner_admission_precedes_setup_and_setup_is_isolated() {
    // Copy the script at test time into the source-checkout layout used by
    // the real steps, so neither the helper nor Python needs a source rewrite.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".agentic-job-source/workflow")).unwrap();
    std::fs::copy(
        root().join("workflow/runner-setup.py"),
        dir.path()
            .join(".agentic-job-source/workflow/runner-setup.py"),
    )
    .unwrap();
    std::fs::create_dir(dir.path().join(".agentic-job")).unwrap();
    let home = dir.path().join("agent-home");
    let config = dir.path().join(".agentic-job/config.toml");
    for (caller, policy, text, runner, sandbox) in [
        ("false", "{}", "", true, true),
        ("false", r#"{"private":false}"#, "", true, true),
        ("false", "{}", "[egress]\nwrite-policy = true\n", true, true),
        ("true", "{}", "", false, false),
        ("null", "{}", "", false, false),
        ("false", r#"{"private":true}"#, "", false, false),
        (
            "true",
            "{}",
            "[egress]\nwrite-policy = true\n",
            false,
            false,
        ),
        (
            "null",
            "{}",
            "[egress]\nwrite-policy = true\n",
            false,
            false,
        ),
        (
            "true",
            r#"{"private":true}"#,
            "[egress]\nwrite-policy = true\n",
            false,
            false,
        ),
        (
            "false",
            r#"{"private":true}"#,
            "[egress]\nwrite-policy = true\n",
            false,
            false,
        ),
        (
            "false",
            "{}",
            "[inference]\nregister = 'api-key'",
            false,
            false,
        ),
        (
            "false",
            "{}",
            "[mcp.server]\nurl = 'https://example.com'",
            false,
            false,
        ),
        ("false", "{}", "[session]\nlisten = true", false, true),
        (
            "false",
            "{}",
            "[sandbox.check]\nbefore-agent = true",
            false,
            true,
        ),
        ("false", "{}", "[setup.tools]\nrust = 'stable'", false, true),
        ("false", "{}", "[setup]\npackages = []", true, true),
    ] {
        std::fs::write(&config, text).unwrap();
        std::fs::write(dir.path().join(".agentic-job/policy.json"), policy).unwrap();
        for (user, ok) in [
            ("runner", runner),
            ("sandbox", sandbox),
            ("", false),
            ("root", false),
        ] {
            let result = run_step(
                "agent.yml",
                "Admit the runner-user configuration",
                dir.path(),
                &[
                    ("CALLER_PRIVATE", caller),
                    ("AGENT_USER", user),
                    ("AGENT_HOME", path(&home)),
                ],
            );
            assert_eq!(
                result.status.success(),
                ok,
                "{caller} {policy} {text} {user}: {result:?}"
            );
            assert!(!home.exists());
        }
    }
    for (text, script, code) in [
        ("[setup]\npackages = []", "", 0),
        (
            "[setup]\nscript = 'setup.sh'\n[sandbox.env]\nTOOLCHAIN_OPTION = 'configured'",
            "#!/bin/sh\npwd > \"$HOME/pwd\"\nenv > \"$HOME/env\"\n",
            0,
        ),
        ("[setup]\nscript = 'setup.sh'", "#!/bin/sh\nexit 3\n", 3),
    ] {
        std::fs::write(&config, text).unwrap();
        std::fs::write(dir.path().join("setup.sh"), script).unwrap();
        let result = run_step(
            "agent.yml",
            "The setup script",
            dir.path(),
            &[
                ("RUNNER_TEMP", path(dir.path())),
                ("AGENT_HOME", path(&home)),
                ("JOB_CREDENTIAL", "must-not-reach-setup"),
            ],
        );
        assert_eq!(result.status.code(), Some(code), "{result:?}");
        if script.is_empty() {
            assert!(!home.exists());
        }
        if script.contains("pwd") {
            assert_eq!(
                std::fs::read_to_string(home.join("pwd")).unwrap().trim(),
                path(&home)
            );
            let env = std::fs::read_to_string(home.join("env")).unwrap();
            assert!(
                env.lines()
                    .any(|line| line == format!("HOME={}", home.display()))
            );
            assert!(
                env.lines()
                    .any(|line| line == "TOOLCHAIN_OPTION=configured")
            );
            assert!(!env.contains("JOB_CREDENTIAL") && !env.contains("RUNNER_TEMP"));
        }
    }
}

#[test]
fn run_uses_policy_binary_captures_exit_and_gates_partial_results() {
    for user in ["runner", "sandbox"] {
        for exit in [0, 1, 3, 4, 124] {
            let dir = tempfile::tempdir().unwrap();
            executable(
                &dir.path().join("agentic-job"),
                "#!/bin/bash\nprintf '%s\\n' \"$0\" \"$@\" > \"$RECORD\"\nexit \"$STATE\"\n",
            );
            // Artifact downloads do not preserve executable permissions.
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                dir.path().join("agentic-job"),
                std::fs::Permissions::from_mode(0o644),
            )
            .unwrap();
            let record = dir.path().join("record");
            let output = dir.path().join("output");
            let home = dir.path().join("agent-home");
            let result = run_step(
                "run/action.yml",
                "Run the agent",
                dir.path(),
                &[
                    ("JOB_DIR", path(dir.path())),
                    ("AGENT_USER", user),
                    (
                        "RUNNER_HOME",
                        if user == "runner" { path(&home) } else { "" },
                    ),
                    ("REVIEW_HEAD", "review-head"),
                    ("STATE", &exit.to_string()),
                    ("RECORD", path(&record)),
                    ("GITHUB_OUTPUT", path(&output)),
                    ("GITHUB_STEP_SUMMARY", path(&dir.path().join("summary"))),
                ],
            );
            assert!(result.status.success(), "{result:?}");
            assert_eq!(
                std::fs::read_to_string(output).unwrap(),
                format!("exit={exit}\n")
            );
            let args = std::fs::read_to_string(record).unwrap();
            assert!(args.starts_with(&format!("{}/agentic-job\nrun\n", dir.path().display())));
            assert!(args.contains("--review-head\nreview-head\n"));
            assert_eq!(args.contains("--runner-home\n"), user == "runner");
            if user == "runner" {
                assert!(args.ends_with(&format!("--runner-home\n{}\n", home.display())));
            }
            for partial in ["false", "true"] {
                let gate = run_step(
                    "run/action.yml",
                    "The run's result",
                    dir.path(),
                    &[
                        ("JOB_DIR", path(dir.path())),
                        ("EXIT", &exit.to_string()),
                        ("PARTIAL", partial),
                        ("HANDED_BACK", "false"),
                        ("CLASS", ""),
                    ],
                );
                assert_eq!(
                    gate.status.success(),
                    exit == 0 || partial == "true" && [3, 124].contains(&exit),
                    "{exit} {partial}: {gate:?}"
                );
            }
        }
    }
}

#[test]
fn run_only_publishes_scalar_budget_and_safe_secret_locations() {
    use serde_json::json;
    for (summary, over) in [
        (
            json!({"result":"success","aic":565.9,"aic_budget":500}),
            "565.9 of 500 AIC",
        ),
        (json!({"result":"success","aic":499.9,"aic_budget":500}), ""),
        (
            json!({"result":"success","aic":565.9,"aic_budget":null}),
            "",
        ),
        (
            json!({"result":"success","aic":"565.9 $(exit 9)","aic_budget":500}),
            "",
        ),
        (json!({"result":"budget","aic":565.9,"aic_budget":500}), ""),
    ] {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("out/run")).unwrap();
        std::fs::write(dir.path().join("out/run/summary.json"), summary.to_string()).unwrap();
        executable(&dir.path().join("agentic-job"), "#!/bin/sh\nexit 0\n");
        let output = dir.path().join("output");
        let result = run_step(
            "run/action.yml",
            "Run the agent",
            dir.path(),
            &[
                ("JOB_DIR", path(dir.path())),
                ("GITHUB_OUTPUT", path(&output)),
                ("GITHUB_STEP_SUMMARY", path(&dir.path().join("summary"))),
            ],
        );
        assert!(result.status.success(), "{result:?}");
        let extra = if over.is_empty() {
            String::new()
        } else {
            format!("over-budget={over}\n")
        };
        assert_eq!(
            std::fs::read_to_string(output).unwrap(),
            format!("exit=0\n{extra}")
        );
        assert_eq!(
            String::from_utf8_lossy(&result.stdout).contains(&format!(
                "::warning::the agent ended its turn over budget: spent {over}\n"
            )),
            !over.is_empty()
        );
    }
    for (at, wanted) in [
        (
            "safe-outputs/aw-agent-run-1.patch, line 3\n",
            "safe-outputs/aw-agent-run-1.patch, line 3",
        ),
        (
            "transcript/acp.jsonl, line 12\nmore\n",
            "transcript/acp.jsonl, line 12",
        ),
        ("safe-outputs/a`@b`.patch, line 3\n", ""),
        ("../x, line 3\n", ""),
        ("safe-outputs/x.patch, line 3 $(exit 9)\n", ""),
    ] {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("out")).unwrap();
        std::fs::write(dir.path().join("out/secret-at"), at).unwrap();
        executable(&dir.path().join("agentic-job"), "#!/bin/sh\nexit 1\n");
        let output = dir.path().join("output");
        let summary = dir.path().join("summary");
        let result = run_step(
            "run/action.yml",
            "Run the agent",
            dir.path(),
            &[
                ("JOB_DIR", path(dir.path())),
                ("GITHUB_OUTPUT", path(&output)),
                ("GITHUB_STEP_SUMMARY", path(&summary)),
            ],
        );
        assert!(result.status.success(), "{result:?}");
        let extra = if wanted.is_empty() {
            String::new()
        } else {
            format!("secret-at={wanted}\n")
        };
        assert_eq!(
            std::fs::read_to_string(output).unwrap(),
            format!("exit=1\n{extra}")
        );
        assert_eq!(
            std::fs::read_to_string(summary).unwrap_or_default(),
            if wanted.is_empty() {
                String::new()
            } else {
                format!(
                    "Nothing of this run was uploaded: a secret or a private key is in {wanted}.\n"
                )
            }
        );
    }
}

#[test]
fn network_refuses_debug_logging_before_minting() {
    let file = ".github/agentic-job/network/action.yml";
    let doc = definition(file);
    let steps = steps(&doc);
    assert_eq!(steps[0]["name"].as_str(), Some("Refuse step debug logging"));
    assert_eq!(steps[0]["if"].as_str(), Some("${{ inputs.secret == '' }}"));
    assert!(
        steps
            .iter()
            .position(|s| s["name"].as_str() == Some("Mint the agent job's key"))
            .unwrap()
            > 0
    );
    for value in [None, Some(""), Some("0"), Some("1")] {
        let dir = tempfile::tempdir().unwrap();
        let env: Vec<_> = value.map(|v| ("RUNNER_DEBUG", v)).into_iter().collect();
        let result = run_step(file, "Refuse step debug logging", dir.path(), &env);
        let ok = value.is_none_or(str::is_empty);
        assert_eq!(result.status.success(), ok, "{value:?}: {result:?}");
        if !ok {
            assert!(
                String::from_utf8_lossy(&result.stdout).starts_with("::error::Refusing to mint")
            );
        }
    }
    let pinned = Regex::new(r"^[\w-]+/[\w./-]+@[0-9a-f]{40}$").unwrap();
    for step in steps {
        if let Some(action) = step["uses"].as_str() {
            assert!(pinned.is_match(action), "{action}");
        }
    }
}

#[test]
fn composed_agents_keep_policy_source_and_trusted_edges() {
    let pinned = Regex::new(r"^actions/(checkout|download-artifact)@[0-9a-f]{40}$").unwrap();
    for (file, prefix) in [
        ("agent.yml", "fromJSON(inputs.policy)."),
        ("example-compose.yml", "needs.policy.outputs."),
    ] {
        let doc = definition(file);
        let agent = &doc["jobs"]["agent"];
        let steps = agent["steps"].as_vec().unwrap();
        let uses: Vec<_> = steps.iter().filter_map(|s| s["uses"].as_str()).collect();
        let local: Vec<_> = uses
            .iter()
            .copied()
            .filter(|s| !s.starts_with("actions/"))
            .collect();
        let expected = if file == "agent.yml" {
            vec![
                "./.agentic-job-source/prepare",
                "./.github/agentic-job/network",
                "./.agentic-job-source/secure-host",
                "./.agentic-job-source/run",
            ]
        } else {
            vec![
                "./.agentic-job-source/prepare",
                "./.agentic-job-source/secure-host",
                "./.agentic-job-source/run",
            ]
        };
        assert_eq!(local, expected, "{file}");
        for action in uses.iter().filter(|s| s.starts_with("actions/")) {
            assert!(pinned.is_match(action));
        }
        let checkouts: Vec<_> = steps
            .iter()
            .filter(|s| {
                s["uses"]
                    .as_str()
                    .is_some_and(|s| s.starts_with("actions/checkout@"))
            })
            .collect();
        assert_eq!(checkouts.len(), 2);
        for step in &checkouts {
            assert_eq!(step["with"]["persist-credentials"].as_bool(), Some(false));
        }
        for (key, value) in [("repository", "source-repository"), ("ref", "source-sha")] {
            assert_eq!(
                checkouts[1]["with"][key].as_str(),
                Some(format!("${{{{ {prefix}{value} }}}}").as_str())
            );
        }
        assert_eq!(
            checkouts[1]["with"]["path"].as_str(),
            Some(".agentic-job-source")
        );
        assert_eq!(
            checkouts[0]["with"]["ref"].as_str(),
            Some(format!("${{{{ {prefix}ref }}}}").as_str())
        );
        let prepare = steps
            .iter()
            .find(|s| s["uses"].as_str() == Some("./.agentic-job-source/prepare"))
            .unwrap();
        for key in ["binary-artifact-id", "policy-artifact-id"] {
            assert_eq!(
                prepare["with"][key].as_str(),
                Some(format!("${{{{ {prefix}{key} }}}}").as_str())
            );
        }
        for key in ["safe-outputs-artifact-id", "safe-outputs-artifact-digest"] {
            assert_eq!(
                agent["outputs"][key].as_str(),
                Some(format!("${{{{ steps.run.outputs.{key} }}}}").as_str())
            );
        }
        let source_name = "The policy call's source repository and commit";
        let at = |name| {
            steps
                .iter()
                .position(|s| s["name"].as_str() == Some(name))
                .unwrap()
        };
        assert!(
            at(source_name)
                < steps
                    .iter()
                    .position(|s| s["with"]["path"].as_str() == Some(".agentic-job-source"))
                    .unwrap()
        );
        for (repo, sha, ok) in [
            ("o/r", PIN, true),
            ("", PIN, false),
            ("o/r", "", false),
            ("o/r", "main", false),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let result = run_step(
                file,
                source_name,
                dir.path(),
                &[("SOURCE_REPOSITORY", repo), ("SOURCE_SHA", sha)],
            );
            assert_eq!(
                result.status.success(),
                ok,
                "{file} {repo} {sha}: {result:?}"
            );
        }
        let expanded = workflow(file)
            .replace(
                "uses: ./.agentic-job-source/prepare",
                &std::fs::read_to_string(root().join("prepare/action.yml")).unwrap(),
            )
            .replace(
                "uses: ./.agentic-job-source/secure-host",
                &std::fs::read_to_string(root().join("secure-host/action.yml")).unwrap(),
            )
            .replace(
                "uses: ./.agentic-job-source/run",
                &std::fs::read_to_string(root().join("run/action.yml")).unwrap(),
            );
        let mut start = 0;
        for name in [
            "Install the binary",
            "Write what the run is called",
            "Admit the runner-user configuration",
            "Check inference proxy reachability",
            "The host's configuration",
            "Refuse the old action's inputs",
            "The setup script",
            "The session's endpoint",
            "Prove the host is secured",
            "Run the agent",
        ] {
            let needle = format!("- name: {name}\n");
            start += expanded[start..]
                .find(&needle)
                .unwrap_or_else(|| panic!("{file}: no {name} after {start}"))
                + needle.len();
        }
        assert_eq!(
            named_step(&doc, "Check inference proxy reachability")["run"].as_str(),
            Some("agentic-job secure-host inference .agentic-job/config.toml")
        );
        assert_eq!(
            steps
                .iter()
                .filter(|s| s["run"]
                    .as_str()
                    .is_some_and(|s| s.contains("agentic-job secure-host inference")))
                .count(),
            1
        );
    }
    for file in ["agentic-job.yml", "example-compose.yml"] {
        let doc = definition(file);
        for (name, values) in [
            (
                "check",
                vec![
                    ("policy", "${{ toJSON(needs.policy.outputs) }}"),
                    (
                        "safe-outputs-artifact-id",
                        "${{ needs.agent.outputs.safe-outputs-artifact-id }}",
                    ),
                    (
                        "safe-outputs-artifact-digest",
                        "${{ needs.agent.outputs.safe-outputs-artifact-digest }}",
                    ),
                ],
            ),
            (
                "apply",
                vec![
                    ("policy", "${{ toJSON(needs.policy.outputs) }}"),
                    ("agent", "${{ toJSON(needs.agent) }}"),
                    ("check", "${{ toJSON(needs.check) }}"),
                ],
            ),
        ] {
            let with = doc["jobs"][name]["with"].as_hash().unwrap();
            assert_eq!(with.len(), values.len());
            for (key, value) in values {
                assert_eq!(doc["jobs"][name]["with"][key].as_str(), Some(value));
            }
        }
    }
}

#[test]
fn agent_credentials_stay_in_their_deterministic_jobs_and_steps() {
    use yaml_rust2::Yaml;
    let pinned = Regex::new(r"^actions/[\w-]+@[0-9a-f]{40}$").unwrap();
    for file in ["prepare/action.yml", "run/action.yml"] {
        let doc = definition(file);
        for step in steps(&doc) {
            if let Some(action) = step["uses"].as_str() {
                assert!(pinned.is_match(action), "{file}: {action}");
            }
        }
    }
    let reads = Yaml::Hash(
        ["contents", "issues", "pull-requests", "actions"]
            .map(|key| (Yaml::String(key.into()), Yaml::String("read".into())))
            .into_iter()
            .collect(),
    );
    for (file, name) in [
        ("agent.yml", "agent"),
        ("agentic-job.yml", "agent"),
        ("example-compose.yml", "agent"),
    ] {
        assert_eq!(
            definition(file)["jobs"][name]["permissions"],
            reads,
            "{file}"
        );
    }
    let agent = workflow("agent.yml");
    let wrapper = workflow("agentic-job.yml");
    let run = std::fs::read_to_string(root().join("run/action.yml")).unwrap();
    let doc = definition("run/action.yml");
    for (input, key) in [
        ("github-token", "GH_TOKEN"),
        ("inference-api-key", "AGENTIC_JOB_INFERENCE_KEY"),
        ("mcp-token", "AGENTIC_JOB_MCP_TOKEN"),
        ("inference-run-token", "AGENTIC_JOB_INFERENCE_RUN_TOKEN"),
    ] {
        assert_eq!(
            named_step(&doc, "Run the agent")["env"][key].as_str(),
            Some(format!("${{{{ inputs.{input} }}}}").as_str())
        );
        if input != "github-token" {
            assert_eq!(run.matches(&format!("inputs.{input}")).count(), 1);
        }
    }
    assert_eq!(doc["inputs"]["github-token"]["default"].as_str(), Some(""));
    for file in [
        "agentic-job.yml",
        "agent.yml",
        "example-compose.yml",
        "policy.yml",
        "check.yml",
        "apply.yml",
    ] {
        let text = workflow(file);
        let doc = definition(file);
        for (name, value) in doc["jobs"].as_hash().unwrap() {
            let name = name.as_str().unwrap();
            let writes = name == "apply"
                || file == "agentic-job.yml" && name == "policy"
                || file == "policy.yml" && ["notify", "apply-token"].contains(&name)
                || file == "apply.yml" && name == "conclude";
            assert_eq!(
                job(&text, name).contains("secrets.SAFE_OUTPUTS_PAT"),
                writes,
                "{file} {name}"
            );
            if file == "agent.yml" {
                assert!(value["permissions"]["id-token"].is_badvalue());
            }
        }
    }
    for (file, text) in [
        ("agentic-job.yml", wrapper.as_str()),
        ("agent.yml", agent.as_str()),
        ("review.yml", workflow("review.yml").as_str()),
    ] {
        for key in ["MCP_TOKEN", "INFERENCE_API_KEY"] {
            assert!(!text.contains(&format!("secrets.{key}")), "{file}");
        }
    }
    assert!(
        !Regex::new("check-key|mcp-token|inference-api-key")
            .unwrap()
            .is_match(&agent)
    );
    let secrets = Regex::new(r"secrets\.([\w]+)").unwrap();
    assert_eq!(
        secrets
            .captures_iter(&agent)
            .map(|c| c[1].to_owned())
            .collect::<Vec<_>>(),
        ["NETWORK_SECRET", "INFERENCE_RUN_TOKEN"]
    );
    assert_eq!(
        wrapper
            .lines()
            .filter(|l| l.contains("needs.register.outputs"))
            .collect::<Vec<_>>(),
        ["      INFERENCE_RUN_TOKEN: ${{ needs.register.outputs.run-token }}"]
    );
    assert_eq!(
        agent
            .lines()
            .filter(|l| l.contains("secrets.INFERENCE_RUN_TOKEN"))
            .collect::<Vec<_>>(),
        ["          inference-run-token: ${{ secrets.INFERENCE_RUN_TOKEN }}"]
    );
    let doc = definition("agentic-job.yml");
    let register = &doc["jobs"]["register"];
    assert_eq!(register["permissions"].as_hash().unwrap().len(), 1);
    assert_eq!(register["permissions"]["id-token"].as_str(), Some("write"));
    let register_text = job(&wrapper, "register");
    assert!(
        !Regex::new(r"secrets\.|checkout|github\.token")
            .unwrap()
            .is_match(register_text)
    );
    assert_eq!(
        register["outputs"]["run-token"].as_str(),
        Some("${{ steps.register.outputs.run-token }}")
    );
    assert_eq!(
        register["runs-on"].as_str(),
        Some("${{ fromJSON(inputs.agent-runner) }}")
    );
    let uses: Vec<_> = register["steps"]
        .as_vec()
        .unwrap()
        .iter()
        .filter_map(|s| s["uses"].as_str())
        .collect();
    assert_eq!(
        uses,
        ["actions/download-artifact@9000827ccba6bdab643e8b6fd33ac0654aef8333"]
    );
    assert!(named_step(&doc, "Register the run at the inference proxy")["run"].as_str().unwrap().trim_end().ends_with(r#""$JOB_DIR/agentic-job" register --config "$JOB_DIR/config.toml" --output "$GITHUB_OUTPUT""#));
    assert_eq!(
        doc["on"]["workflow_call"]["inputs"]["github-reads"]["default"].as_bool(),
        Some(true)
    );
    assert!(agent.contains("github-token: ${{ inputs.github-reads && github.token || '' }}"));
    // Called workflows cannot elevate their caller's grants, even in skipped jobs.
    for entry in std::fs::read_dir(root().join(".github/workflows")).unwrap() {
        let file = entry.unwrap().path();
        if file.extension().is_none_or(|s| s != "yml") {
            continue;
        }
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(!text.contains("GH_READ_TOKEN"), "{file:?}");
        let doc = &yaml_rust2::YamlLoader::load_from_str(&text).unwrap()[0];
        for (name, job) in doc["jobs"].as_hash().unwrap() {
            if !job["uses"].as_str().is_some_and(|s| {
                [
                    "./.github/workflows/agentic-job.yml",
                    "./.github/workflows/dispatch.yml",
                    "./.github/workflows/example-compose.yml",
                ]
                .contains(&s)
            }) {
                continue;
            }
            for key in ["contents", "issues", "pull-requests", "actions"] {
                assert!(
                    matches!(job["permissions"][key].as_str(), Some("read" | "write")),
                    "{file:?} {name:?} {key}"
                );
            }
            assert_eq!(
                job["permissions"]["id-token"].as_str(),
                Some("write"),
                "{file:?} {name:?}"
            );
        }
    }
}

#[test]
fn wrapper_forwards_policy_settings_without_reinterpretation() {
    let wrapper = definition("agentic-job.yml");
    let policy = definition("policy.yml");
    let own = [
        "agent-runner",
        "github-reads",
        "secure-host-image-artifact-id",
        "agent-user",
    ];
    let inputs = |doc: &yaml_rust2::Yaml| {
        doc["on"]["workflow_call"]["inputs"]
            .as_hash()
            .unwrap()
            .keys()
            .map(|key| key.as_str().unwrap().to_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        inputs(&wrapper)
            .into_iter()
            .filter(|s| !own.contains(&s.as_str()))
            .collect::<Vec<_>>(),
        inputs(&policy)
    );
    assert_eq!(
        wrapper["jobs"]["policy"]["with"].as_hash().unwrap().len(),
        inputs(&policy).len()
    );
    for name in inputs(&policy) {
        assert_eq!(
            wrapper["jobs"]["policy"]["with"][name.as_str()].as_str(),
            Some(format!("${{{{ inputs.{name} }}}}").as_str())
        );
    }
}

#[test]
fn policy_config_uses_its_own_binary_and_host_config_keeps_user_mode() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("bin")).unwrap();
    executable(
        &dir.path().join("bin/agentic-job"),
        "#!/bin/sh\nprintf '%s\\n' \"$@\"\n",
    );
    let mut env = vec![
        ("JOB_DIR", path(dir.path())),
        ("AGENT", "fake"),
        ("REPO", "example/repo"),
    ];
    for key in [
        "CONFIG",
        "MODEL",
        "CONFIG_REPO",
        "CONFIG_REF",
        "CONFIG_PATH",
        "SKILLS",
        "URL",
        "REGISTER",
        "AUDIENCE",
        "TOKEN_FILE",
        "PROVIDER",
        "TIMEOUT",
        "MAX_REQUESTS",
        "MAX_TASKS",
        "BUDGET",
        "BUDGET_USD",
        "PACKAGES",
        "NPM",
        "AGENTS",
        "SETUP",
        "SESSION",
        "IDLE_MINUTES",
        "WRITE_POLICY",
    ] {
        env.push((key, ""));
    }
    let result = run_step("policy.yml", "Write the configuration", dir.path(), &env);
    assert!(result.status.success(), "{result:?}");
    assert!(
        std::fs::read_to_string(dir.path().join("config.toml"))
            .unwrap()
            .starts_with("config\n--repo\nexample/repo\n--string\nagent.name=fake\n")
    );
    let search = format!("{}:/usr/bin:/bin", dir.path().join("bin").display());
    executable(
        &dir.path().join("bin/agentic-job"),
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$RECORD\"\n",
    );
    for (user, flags) in [("runner", "--host\n--no-script\n"), ("sandbox", "--host\n")] {
        let record = dir.path().join("record");
        let result = run_step(
            "agent.yml",
            "The host's configuration",
            dir.path(),
            &[
                ("PATH", &search),
                ("AGENT_USER", user),
                ("RUNNER_TEMP", path(dir.path())),
                ("RECORD", path(&record)),
            ],
        );
        assert!(result.status.success(), "{result:?}");
        assert_eq!(
            std::fs::read_to_string(record).unwrap(),
            format!("config\n{flags}--from\n.agentic-job/config.toml\n")
        );
    }
}

#[test]
fn caller_network_credentials_are_spent_before_the_agent() {
    let network = "./.github/agentic-job/network";
    let review = definition("review.yml");
    let register = &review["jobs"]["register"];
    let steps = register["steps"].as_vec().unwrap();
    for (name, value) in review["jobs"].as_hash().unwrap() {
        if name.as_str() != Some("register") {
            assert!(!value["steps"].as_vec().is_some_and(|steps| {
                steps
                    .iter()
                    .any(|step| step["uses"].as_str() == Some(network))
            }));
        }
    }
    assert_eq!(
        steps
            .iter()
            .filter_map(|s| s["uses"].as_str())
            .map(|s| s.split('@').next().unwrap())
            .collect::<Vec<_>>(),
        ["actions/checkout", network, "actions/download-artifact"]
    );
    let checkout = steps
        .iter()
        .find(|s| {
            s["uses"]
                .as_str()
                .is_some_and(|s| s.starts_with("actions/checkout@"))
        })
        .unwrap();
    assert_eq!(
        checkout["with"]["ref"].as_str(),
        Some("${{ needs.policy.outputs.ref }}")
    );
    assert_eq!(
        checkout["with"]["persist-credentials"].as_bool(),
        Some(false)
    );
    let join = named_step(&review, "The caller's network step");
    assert_eq!(join["id"].as_str(), Some("network"));
    assert_eq!(
        join["if"].as_str(),
        Some("${{ hashFiles('.github/agentic-job/network/action.yml') != '' }}")
    );
    assert_eq!(
        register["outputs"]["network-secret"].as_str(),
        Some("${{ steps.network.outputs.secret }}")
    );
    assert_eq!(
        review["jobs"]["agent"]["secrets"]["NETWORK_SECRET"].as_str(),
        Some("${{ needs.register.outputs.network-secret }}")
    );
    assert_eq!(
        review["jobs"]["agent"]["with"]["network"].as_bool(),
        Some(true)
    );
    let text = workflow("review.yml");
    assert_eq!(
        text.lines()
            .filter(|s| s.contains("network-secret")
                || s.contains("NETWORK_SECRET")
                || s.contains("steps.network."))
            .map(str::trim)
            .collect::<Vec<_>>(),
        [
            "network-secret: ${{ steps.network.outputs.secret }}",
            "NETWORK_SECRET: ${{ needs.register.outputs.network-secret }}"
        ]
    );
    assert!(named_step(&review, "Register the run at the inference proxy")["run"].as_str().unwrap().trim_end().ends_with(r#""$JOB_DIR/agentic-job" register --config "$JOB_DIR/config.toml" --output "$GITHUB_OUTPUT""#));
    assert!(
        !Regex::new(r"secrets\.|github\.token|agentic-job-source")
            .unwrap()
            .is_match(job(&text, "register"))
    );
    let doc = definition("agent.yml");
    let join = named_step(&doc, "The caller's network step");
    assert_eq!(join["uses"].as_str(), Some(network));
    assert_eq!(
        join["if"].as_str(),
        Some("${{ inputs.network && hashFiles('.github/agentic-job/network/action.yml') != '' }}")
    );
    assert_eq!(
        join["with"]["secret"].as_str(),
        Some("${{ secrets.NETWORK_SECRET }}")
    );
    assert_eq!(
        doc["on"]["workflow_call"]["inputs"]["network"]["default"].as_bool(),
        Some(false)
    );
    let agent = workflow("agent.yml");
    assert_eq!(
        agent
            .lines()
            .filter(|s| s.contains("secrets.NETWORK_SECRET"))
            .collect::<Vec<_>>(),
        ["          secret: ${{ secrets.NETWORK_SECRET }}"]
    );
    assert!(
        agent
            .find("- name: Admit the runner-user configuration")
            .unwrap()
            < agent.find(&format!("uses: {network}")).unwrap()
    );
    let forbidden = Regex::new(r"(?i)tailscale|join(?:s|ing)? (?:a|the) tailnet").unwrap();
    let foreign = Regex::new(r"cgwalters-forge/actions(?:/([\w/-]+))?").unwrap();
    let files = std::fs::read_dir(root().join(".github/workflows"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .chain(
            [
                "prepare/action.yml",
                "run/action.yml",
                "secure-host/action.yml",
            ]
            .map(|s| root().join(s)),
        );
    for file in files {
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(!forbidden.is_match(&text), "{file:?}");
        for found in foreign.captures_iter(&text) {
            assert_eq!(
                found.get(1).map(|m| m.as_str()),
                Some("secure-host"),
                "{file:?}"
            );
        }
    }
}

/// Apply's result step only reads what the job wrote and writes step
/// outputs and the summary: it gets no token, keeps the links the handlers
/// keep, and runs whenever they ran, without failing the job.
#[test]
fn the_result_step_passes_no_token() {
    const NAME: &str = "Say where what the run made is";
    let apply = definition("apply.yml");
    let result = named_step(&apply, NAME);
    let handlers = named_step(&apply, "Apply the outputs with gh-aw's handlers");
    assert_eq!(result["id"].as_str(), Some("result"));
    assert_eq!(
        result["if"].as_str(),
        Some("${{ !cancelled() && steps.apply.outcome != 'skipped' }}")
    );
    assert_eq!(result["continue-on-error"].as_bool(), Some(true));
    assert!(result["uses"].is_badvalue() && result["with"].is_badvalue());
    let env: Vec<_> = result["env"]
        .as_hash()
        .unwrap()
        .keys()
        .map(|key| key.as_str().unwrap())
        .collect();
    assert_eq!(
        env,
        [
            "GH_AW_SAFE_OUTPUTS_URLS",
            "GH_AW_ALLOWED_DOMAINS",
            "RUN_URL"
        ]
    );
    assert_eq!(
        result["env"]["GH_AW_ALLOWED_DOMAINS"].as_str(),
        Some("${{ fromJSON(inputs.policy).allowed-domains }}")
    );
    for key in ["GH_AW_SAFE_OUTPUTS_URLS", "GH_AW_ALLOWED_DOMAINS"] {
        assert_eq!(result["env"][key], handlers["env"][key], "{key}");
    }
    assert_eq!(
        result["run"].as_str().unwrap().trim_end().lines().last(),
        Some(
            r#""$RUNNER_TEMP/apply-binary/agentic-job" result --dir "$GH_AW_TMP" --output "$GITHUB_OUTPUT" --summary "$GITHUB_STEP_SUMMARY""#
        )
    );
    let text = workflow("apply.yml");
    let step = text
        .split(&format!("- name: {NAME}\n"))
        .nth(1)
        .unwrap()
        .split("\n      - ")
        .next()
        .unwrap();
    for credential in ["secrets.", "token", "TOKEN"] {
        assert!(!step.contains(credential), "{credential}: {step}");
    }
}

#[test]
fn optional_apply_token_admission_holds_the_pat_in_one_step() {
    let apply = definition("apply.yml");
    assert!(
        !apply["jobs"]["apply"]["if"]
            .as_str()
            .unwrap()
            .contains("always()")
    );
    for file in ["policy.yml", "agentic-job.yml"] {
        let doc = definition(file);
        assert_eq!(
            doc["on"]["workflow_call"]["inputs"]["require-safe-outputs-pat"]["default"].as_bool(),
            Some(false)
        );
    }
    let doc = definition("policy.yml");
    let steps = doc["jobs"]["apply-token"]["steps"].as_vec().unwrap();
    let text = workflow("policy.yml");
    let check = job(&text, "apply-token");
    assert_eq!(check.matches("secrets.SAFE_OUTPUTS_PAT").count(), 1);
    assert!(
        !check
            .split("- name: Check SAFE_OUTPUTS_PAT\n")
            .next()
            .unwrap()
            .contains("secrets.")
    );
    assert_eq!(
        steps
            .iter()
            .filter(|s| s["env"]["SAFE_OUTPUTS_PAT"].as_str()
                == Some("${{ secrets.SAFE_OUTPUTS_PAT }}"))
            .count(),
        1
    );
}

#[test]
fn ci_agents_take_the_build_image_by_id_only() {
    let ci = definition("ci.yml");
    for name in [
        "e2e-full",
        "e2e-limit",
        "e2e-write-policy",
        "e2e-event",
        "e2e-analysis",
        "e2e-analysis-refused",
        "e2e-review",
    ] {
        assert_eq!(ci["jobs"][name]["with"]["agent"].as_str(), Some("fake"));
        assert!(ci["jobs"][name]["with"]["inference-url"].is_badvalue());
    }
    for (name, job) in ci["jobs"].as_hash().unwrap() {
        if job["uses"].as_str() != Some("./.github/workflows/agentic-job.yml")
            || !job["with"]["proposals-artifact"].is_badvalue()
        {
            continue;
        }
        let needs = job["needs"].as_vec().unwrap();
        assert!(
            needs.iter().any(|n| n.as_str() == Some("static")),
            "{name:?}"
        );
        assert_eq!(
            job["with"]["secure-host-image-artifact-id"].as_str(),
            Some("${{ needs.static.outputs.secure-host-image-artifact-id }}")
        );
    }
    let doc = definition("build.yml");
    assert_eq!(
        doc["on"]["workflow_call"]["outputs"]["secure-host-image-artifact-id"]["value"].as_str(),
        Some("${{ jobs.build.outputs.secure-host-image-artifact-id }}")
    );
    assert_eq!(
        doc["jobs"]["build"]["outputs"]["secure-host-image-artifact-id"].as_str(),
        Some("${{ steps.image.outputs.artifact-id }}")
    );
    let image = doc["jobs"]["build"]["steps"]
        .as_vec()
        .unwrap()
        .iter()
        .find(|s| s["id"].as_str() == Some("image"))
        .unwrap();
    assert_eq!(image["with"]["name"].as_str(), Some("secure-host-image"));
    assert!(
        Regex::new(r"^actions/upload-artifact@[0-9a-f]{40}$")
            .unwrap()
            .is_match(image["uses"].as_str().unwrap())
    );
    let doc = definition("agent.yml");
    let download = steps(&doc)
        .into_iter()
        .find(|s| {
            s["with"]["artifact-ids"].as_str()
                == Some("${{ inputs.secure-host-image-artifact-id }}")
        })
        .unwrap();
    assert_eq!(
        download["if"].as_str(),
        Some("${{ inputs.secure-host-image-artifact-id != '' }}")
    );
    assert!(download["with"]["name"].is_badvalue());
}

#[test]
fn composed_example_keeps_the_callers_privilege_boundary() {
    // Shape only: the fresh-host e2e-compose job executes these sudo probes.
    let doc = definition("example-compose.yml");
    let before = named_step(&doc, "A caller's privileged step (example)")["run"]
        .as_str()
        .unwrap();
    let file = Regex::new(r"sudo install -m 0600 -o root -g root /dev/null (/var/lib/[\w-]+)")
        .unwrap()
        .captures(before)
        .unwrap()[1]
        .to_owned();
    let after = named_step(&doc, "A caller's later step (example)")["run"]
        .as_str()
        .unwrap();
    assert!(after.contains(&format!("stat -c '%U %a' {file})\" = 'root 600'")));
    assert!(
        Regex::new(r"if sudo -n true .*exit 1; fi")
            .unwrap()
            .is_match(after)
    );
}

#[test]
fn agent_configuration_provenance_and_step_gates_are_preserved() {
    let policy = definition("policy.yml");
    assert_eq!(
        named_step(&policy, "Write the configuration")["if"].as_str(),
        Some("${{ inputs.proposals-artifact == '' && steps.event.outputs.admitted != 'false' }}")
    );
    assert!(
        workflow("policy.yml")
            .split("agentic-job-policy\n")
            .nth(1)
            .unwrap()
            .split("\n  activate:")
            .next()
            .unwrap()
            .contains(".agentic-job/config.toml")
    );
    assert!(workflow("policy.yml").contains("@agentclientprotocol/claude-agent-acp@0.88.0"));
    for (file, expected) in [
        (
            "agent.yml",
            "[ \"$AGENT_USER\" = sandbox ] || set -- --no-script\nagentic-job config --host \"$@\" --from .agentic-job/config.toml > \"$RUNNER_TEMP/agentic-job-host.toml\"\n",
        ),
        (
            "example-compose.yml",
            "agentic-job config --host --no-script --from .agentic-job/config.toml > \"$RUNNER_TEMP/agentic-job-host.toml\"",
        ),
    ] {
        let doc = definition(file);
        let host = named_step(&doc, "The host's configuration");
        assert_eq!(host["run"].as_str(), Some(expected), "{file}");
        let agent = &doc["jobs"]["agent"];
        let steps = agent["steps"].as_vec().unwrap();
        assert_eq!(
            steps
                .iter()
                .filter(|s| s["run"].as_str().is_some_and(|s| s.contains(" config ")))
                .count(),
            1,
            "{file}"
        );
        let secured = steps
            .iter()
            .find(|s| s["uses"].as_str() == Some("./.agentic-job-source/secure-host"))
            .unwrap();
        assert_eq!(
            secured["with"]["config"].as_str(),
            Some("${{ runner.temp }}/agentic-job-host.toml")
        );
    }
    for (file, name) in [
        ("policy.yml", "Check that the repositories are public"),
        (
            "run/action.yml",
            "Check again that the repositories are public",
        ),
    ] {
        let doc = definition(file);
        assert!(
            named_step(&doc, name)["run"]
                .as_str()
                .unwrap()
                .contains(".private == false and .visibility == \"public\"")
        );
    }
}

#[test]
fn agent_secret_contract_accepts_only_the_intended_credentials() {
    let agent = definition("agent.yml");
    let wrapper = definition("agentic-job.yml");
    let names = |value: &yaml_rust2::Yaml| {
        value
            .as_hash()
            .unwrap()
            .keys()
            .map(|s| s.as_str().unwrap().to_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        names(&agent["on"]["workflow_call"]["secrets"]),
        ["INFERENCE_RUN_TOKEN", "NETWORK_SECRET"]
    );
    assert_eq!(
        names(&wrapper["jobs"]["agent"]["secrets"]),
        ["INFERENCE_RUN_TOKEN"]
    );
    for (file, keys) in [
        ("agentic-job.yml", &["MCP_TOKEN", "INFERENCE_API_KEY"][..]),
        ("review.yml", &["INFERENCE_API_KEY"][..]),
    ] {
        let doc = definition(file);
        for key in keys {
            assert_eq!(
                doc["on"]["workflow_call"]["secrets"][*key]["required"].as_bool(),
                Some(false),
                "{file} {key}"
            );
        }
    }
}

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

/// A refusal by the collector, the policy check or the review verdict
/// reaches check's refusal step and the text of the conclusion comment.
#[test]
fn refusals_reach_the_conclusion_comment() {
    use serde_json::json;
    for phase in ["collector", "policy", "review"] {
        let dir = tempfile::tempdir().unwrap();
        let tmp = path(dir.path());
        let errors = if phase == "collector" {
            json!(["collector refused"])
        } else {
            json!([])
        };
        std::fs::write(
            dir.path().join("agent_output.json"),
            json!({"errors": errors, "items": []}).to_string(),
        )
        .unwrap();
        if phase == "policy" {
            let report = json!({"ok": false, "errors": ["protected files: CONTRIBUTING.md"]});
            std::fs::write(dir.path().join("report.json"), report.to_string()).unwrap();
        }
        if phase == "review" {
            // No comment is no verdict: the binary writes the report the
            // refusal step reads, and fails the step before its output.
            let output = dir.path().join("verdict-output");
            let verdict = run_step(
                "check.yml",
                "Validate the review verdict",
                dir.path(),
                &[
                    ("JOB_DIR", bin_dir()),
                    ("GH_AW_TMP", tmp),
                    ("TARGET", "12"),
                    ("GITHUB_OUTPUT", path(&output)),
                ],
            );
            assert_eq!(verdict.status.code(), Some(1), "{verdict:?}");
            assert!(!output.exists());
        }
        let reasons = dir.path().join("reasons");
        std::fs::write(&reasons, "").unwrap();
        let shown = run_step(
            "check.yml",
            "Show why check refused the outputs",
            dir.path(),
            &[
                ("JOB_DIR", bin_dir()),
                ("GH_AW_TMP", tmp),
                ("GITHUB_OUTPUT", path(&reasons)),
            ],
        );
        assert!(shown.status.success(), "{phase}: {shown:?}");
        assert!(
            String::from_utf8_lossy(&shown.stdout).starts_with("::error::Refused by check: "),
            "{phase}: {shown:?}"
        );
        let reasons = std::fs::read_to_string(&reasons).unwrap();
        let reason = reasons
            .strip_prefix("reason=")
            .and_then(|reason| reason.strip_suffix('\n'))
            .filter(|reason| !reason.is_empty() && !reason.contains('\n'))
            .unwrap_or_else(|| panic!("{phase}: {reasons:?}"));
        let output = dir.path().join("step-output");
        let ended = run_step(
            "apply.yml",
            "How the run ended",
            dir.path(),
            &[
                ("RESULTS", "success/failure/skipped"),
                ("EXIT", "0"),
                ("PR", ""),
                ("REFUSAL", reason),
                ("WORKFLOW", "test"),
                ("RUN_URL", "https://example.invalid/run"),
                ("GITHUB_OUTPUT", path(&output)),
            ],
        );
        assert!(ended.status.success(), "{phase}: {ended:?}");
        assert!(
            std::fs::read_to_string(&output)
                .unwrap()
                .contains(&format!("refused by check: {reason}")),
            "{phase}"
        );
    }
}

/// The collector is agentic-job's, and gh-aw's is not set up: a line it
/// refused fails the step after it, with the reason in agent_output.json,
/// and an error of its own fails its own.
#[test]
fn collector_refusals_fail_the_step_after_it() {
    use serde_json::json;
    let pattern = Regex::new("gh-aw/actions/setup@|collect_ndjson_output").unwrap();
    assert!(!pattern.is_match(&workflow("check.yml")));
    let dir = tempfile::tempdir().unwrap();
    executable(
        &dir.path().join("agentic-job"),
        "#!/bin/bash\nif [ \"$STATUS\" != 2 ]; then echo \"$RESULT\" > \"$GH_AW_TMP/agent_output.json\"; fi\nexit \"$STATUS\"\n",
    );
    let accepted = json!({"items": [{"type": "noop", "message": "m"}], "errors": []}).to_string();
    let refused =
        json!({"items": [], "errors": ["Line 1: add_comment requires a 'body' field (string)"]})
            .to_string();
    // (the collector's status and result, whether its step succeeds, and
    // whether the next one does)
    for (status, result, ingested, checked) in [
        ("0", accepted.as_str(), true, Some(true)),
        ("1", refused.as_str(), true, Some(false)),
        ("2", "", false, None),
    ] {
        let _ = std::fs::remove_file(dir.path().join("agent_output.json"));
        let env = [
            ("JOB_DIR", path(dir.path())),
            ("GH_AW_TMP", path(dir.path())),
            ("STATUS", status),
            ("RESULT", result),
        ];
        let ingest = run_step(
            "check.yml",
            "Ingest the outputs with agentic-job collect",
            dir.path(),
            &env,
        );
        let code = Some(i32::from(!ingested));
        assert_eq!(ingest.status.code(), code, "{status}: {ingest:?}");
        if let Some(checked) = checked {
            let check = run_step(
                "check.yml",
                "The collector refused nothing",
                dir.path(),
                &env,
            );
            let code = Some(i32::from(!checked));
            assert_eq!(check.status.code(), code, "{status}: {check:?}");
        }
    }
}

/// With `expect-check-refusal`, the policy step succeeds only on a refusal
/// whose report says why, shown in a summary that could be written; without
/// it, only on an acceptance.
#[test]
fn an_expected_refusal_needs_a_report_and_a_summary() {
    use serde_json::json;
    let dir = tempfile::tempdir().unwrap();
    executable(
        &dir.path().join("agentic-job"),
        "#!/bin/bash\nif [ \"$REPORT\" != missing ]; then cp \"$REPORT\" \"$GH_AW_TMP/report.json\"; fi\nexit \"$STATUS\"\n",
    );
    let fixture = dir.path().join("fixture.json");
    let refusal = json!({"ok": false, "errors": ["wrong item"]});
    // (expect-check-refusal, check's status, its report, the summary file,
    // the step's status)
    for (expect, status, report, summary, code) in [
        ("true", "1", Some(refusal.clone()), "summary", 0),
        (
            "true",
            "0",
            Some(json!({"ok": true, "errors": []})),
            "summary",
            1,
        ),
        ("true", "1", None, "summary", 2),
        ("true", "2", Some(refusal.clone()), "summary", 1),
        (
            "true",
            "1",
            Some(json!({"ok": false, "errors": []})),
            "summary",
            1,
        ),
        ("true", "1", Some(refusal.clone()), "missing/summary", 1),
        ("false", "1", Some(refusal.clone()), "summary", 1),
        (
            "false",
            "0",
            Some(json!({"ok": true, "errors": [], "patch": null})),
            "summary",
            0,
        ),
    ] {
        let _ = std::fs::remove_file(dir.path().join("report.json"));
        if let Some(report) = &report {
            std::fs::write(&fixture, report.to_string()).unwrap();
        }
        let output = run_step(
            "check.yml",
            "Check the outputs against the policy",
            dir.path(),
            &[
                ("JOB_DIR", path(dir.path())),
                ("GH_AW_TMP", path(dir.path())),
                ("COMMENT_TARGET", "64"),
                ("COMMENT_REPO", "owner/repo"),
                ("EXPECT_REFUSAL", expect),
                ("STATUS", status),
                (
                    "REPORT",
                    report.as_ref().map_or("missing", |_| path(&fixture)),
                ),
                ("GITHUB_OUTPUT", path(&dir.path().join("output"))),
                ("GITHUB_STEP_SUMMARY", path(&dir.path().join(summary))),
            ],
        );
        assert_eq!(
            output.status.code(),
            Some(code),
            "{expect} {status} {report:?} {summary}: {output:?}"
        );
    }
}

/// CI's e2e-analysis-refused holds the reason check shows to the refusal
/// it expects, and to nothing else.
#[test]
fn an_expected_refusal_shows_the_reason_ci_expects() {
    let dir = tempfile::tempdir().unwrap();
    let report = serde_json::json!({"ok": false,
        "errors": ["an add_comment with item_number outside the caller's fixed item"]});
    std::fs::write(dir.path().join("report.json"), report.to_string()).unwrap();
    let output = dir.path().join("output");
    std::fs::write(&output, "").unwrap();
    let shown = Command::new(BIN)
        .args(["diagnostics", "refusals", path(dir.path()), "--output"])
        .arg(&output)
        .output()
        .unwrap();
    assert!(shown.status.success(), "{shown:?}");
    let output = std::fs::read_to_string(&output).unwrap();
    let reason = output
        .strip_prefix("reason=")
        .and_then(|reason| reason.strip_suffix('\n'))
        .unwrap();
    let ci = workflow("ci.yml");
    let assertion = ci
        .lines()
        .find(|line| line.contains("grep -F") && line.contains("$REFUSAL"))
        .unwrap()
        .trim();
    for (value, matches) in [(reason, true), ("unrelated refusal", false), ("", false)] {
        let status = Command::new("bash")
            .args(["-eo", "pipefail", "-c", assertion])
            .env("REFUSAL", value)
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        assert_eq!(status.success(), matches, "{value:?}");
    }
}

/// Workflow text that keeps check, token and refusal separation, as (file,
/// job or "" for the whole file, patterns that match, patterns that do not).
const SHAPES: &[(&str, &str, &[&str], &[&str])] = &[
    (
        "prepare/action.yml",
        "",
        &[r#"sudo install -m 0755 "\$JOB_DIR/agentic-job" /usr/local/bin/agentic-job"#],
        &[r"secrets\.|SAFE_OUTPUTS_PAT"],
    ),
    (
        "run/action.yml",
        "",
        &[
            r"PARTIAL: \$\{\{ steps.settings.outputs.apply_partial == 'true' && steps.safe-outputs.outputs.artifact-id != '' \}\}",
            r"value: \$\{\{ steps.session.outputs.endpoint \}\}",
        ],
        &[
            r"secrets\.|SAFE_OUTPUTS_PAT",
            r"(?m)^\s*(?:[^#\n]*[|;&(]\s*)?gh\s",
        ],
    ),
    (
        "secure-host/action.yml",
        "",
        &[],
        &[r"(?i)inference|session"],
    ),
    (
        "policy.yml",
        "policy",
        &[
            r"source-repository: \$\{\{ job.workflow_repository \}\}",
            r"source-sha: \$\{\{ job.workflow_sha \}\}",
            r#"--arg source_sha "\$SOURCE_SHA" '\$ARGS.named'"#,
            r#"SOURCE_SHA: \$\{\{ job.workflow_sha \}\}\n {8}run: \|\n {10}if \[ -n "\$PROFILE" \]"#,
            r"PRIVATE: \$\{\{ steps.visibility.outputs.private \}\}",
            r#"(?s)set -- "\$@" --private\n.*--output-repo "\$\{OUTPUT_REPO:-\$GITHUB_REPOSITORY\}""#,
        ],
        &["CONFIG_BINARY"],
    ),
    (
        "apply.yml",
        "apply",
        &[r"if: >-\n\s+\$\{\{ !cancelled\(\) && fromJSON\(inputs.check\).result == 'success' &&"],
        &[],
    ),
    (
        "agent.yml",
        "agent",
        &[
            r"agent-user: \$\{\{ inputs.agent-user \}\}",
            r"runner-home: \$\{\{ inputs.agent-user != 'sandbox' && format\('\{0\}/agent-home', runner.temp\) \|\| '' \}\}",
            r"- name: The setup script\n        if: \$\{\{ inputs.agent-user != 'sandbox' \}\}",
            r"session-endpoint: \$\{\{ steps.run.outputs.session-endpoint \}\}",
            r"config: \$\{\{ runner.temp \}\}/agentic-job-host.toml",
            r"artifact-ids: \$\{\{ inputs.secure-host-image-artifact-id \}\}",
            r"image-archive: \$\{\{ inputs.secure-host-image-artifact-id != '' && format\('\{0\}/secure-host-image/secure-host.tar', runner.temp\) \|\| '' \}\}",
        ],
        &[],
    ),
    (
        "agentic-job.yml",
        "",
        &[r"value: \$\{\{ jobs.agent.outputs.session-endpoint \}\}"],
        &[r"\n {6}network:"],
    ),
    (
        "review.yml",
        "",
        &[
            r"inference-register: \$\{\{ vars.INFERENCE_REGISTER \|\| 'github-oidc' \}\}",
            r"inference-provider: \$\{\{ vars.INFERENCE_PROVIDER \}\}",
        ],
        &[r"secrets\.INFERENCE_API_KEY|check-key|inference-api-key"],
    ),
    (
        "policy.yml",
        "apply-token",
        &[
            r"needs: policy\n    if: \$\{\{ inputs.require-safe-outputs-pat && needs.policy.outputs.admitted == 'true' \}\}",
            r"environment: \$\{\{ inputs.apply-environment \}\}\n    permissions:\n      contents: read",
            r#"agentic-job check-apply-token --output-repo "\$OUTPUT_REPO""#,
        ],
        &[r"continue-on-error|always\(\)|cancelled\(\)"],
    ),
    (
        "policy.yml",
        "notify",
        &[
            r"needs: \[policy, activate, apply-token\]",
            r"needs.apply-token.result != 'failure'",
        ],
        &[],
    ),
    (
        "agentic-job.yml",
        "register",
        &[r"needs: policy\n"],
        &[r"always\(\)|cancelled\(\)|failure\(\)"],
    ),
    (
        "agentic-job.yml",
        "agent",
        &[r"needs: \[policy, register\]\n"],
        &[r"always\(\)|cancelled\(\)|failure\(\)"],
    ),
    (
        "example-compose.yml",
        "agent",
        &[r"needs: policy\n"],
        &[r"always\(\)|cancelled\(\)|failure\(\)"],
    ),
    // A refusal is reported only by the opted-in status helper, with the
    // optional PAT.
    (
        "apply.yml",
        "conclude",
        &[
            r"fromJSON\(inputs.policy\).notify != 'none' && fromJSON\(inputs.policy\).notified == 'true'",
            r"REPO: \$\{\{ fromJSON\(inputs.policy\).notify-repo \}\}",
            r#"conclude-binary/agentic-job" status\n"#,
            r"STATUS_TOKEN: \$\{\{ secrets.SAFE_OUTPUTS_PAT \}\}",
        ],
        &["Post the refusal to the comment target"],
    ),
    // Apply starts on neither an expected refusal nor a failed agent
    // unless it handed over proposals, and checks out only where its
    // outputs go.
    (
        "apply.yml",
        "apply",
        &[
            r"&& fromJSON\(inputs.policy\).expect-check-refusal != 'true' \}\}",
            r"fromJSON\(inputs.check\).result == 'success' && \(fromJSON\(inputs.policy\).proposals-artifact != ''",
            r"fromJSON\(inputs.policy\).proposals-artifact == '' && format\('\{0\}', fromJSON\(inputs.agent\).outputs.exit\) != '0'",
            r"(?s)- name: Write applied.json.*EXIT: \$\{\{ fromJSON\(inputs.policy\).proposals-artifact != '' && '0' \|\| fromJSON\(inputs.agent\).outputs.exit \}\}",
            r"if: \$\{\{ fromJSON\(inputs.check\).outputs.has-patch == 'true' && fromJSON\(inputs.policy\).apply-staged != 'true' \}\}\s+with:\s+repository: \$\{\{ env.OUTPUT_REPO \}\}",
        ],
        &[r"pull_request.head|refs/pull"],
    ),
    // Check shows a refusal, uploads inline findings folded into the
    // verdict comment, takes the proposals by upload ID or legacy name,
    // and runs its own commit's source without secrets.
    (
        "check.yml",
        "",
        &[
            r"failure\(\) \|\| steps.checked.outputs.refused == 'true'",
            r"\n {12}\$\{\{ env.GH_AW_TMP \}\}/folded.json\n",
            r#"verdict=\$\("\$JOB_DIR/agentic-job" review verdict --collected "\$GH_AW_TMP/agent_output.json" --target "\$TARGET" --report "\$GH_AW_TMP/report.json"\)"#,
            r"if: \$\{\{ fromJSON\(inputs.policy\).review == 'true' \}\}",
            r"name: \$\{\{ fromJSON\(inputs.policy\).proposals-artifact \}\}",
            r"artifact-ids: \$\{\{ inputs.safe-outputs-artifact-id \}\}",
            r#"agentic-job" check --policy"#,
            r"permissions:\n  contents: read",
            r"repository: \$\{\{ job.workflow_repository \}\}",
            r"ref: \$\{\{ job.workflow_sha \}\}",
        ],
        &[r"secrets\."],
    ),
    // An expected refusal uploads nothing applicable; the job reads the
    // proposals' upload with `actions: read`.
    (
        "check.yml",
        "check",
        &[
            r"- name: Put the patch beside the ingested outputs\n        if: \$\{\{ success\(\) && fromJSON\(inputs.policy\).expect-check-refusal != 'true' \}\}",
            r"- id: upload\n        if: \$\{\{ success\(\) && fromJSON\(inputs.policy\).expect-check-refusal != 'true' \}\}",
            r"permissions:\n      contents: read\n      actions: read\n",
        ],
        &[],
    ),
    // CI expects a refusal, and that nothing was applied; its e2e-review
    // runs when the base has the review files, and is required then.
    (
        "ci.yml",
        "",
        &[
            "expect-check-refusal: true",
            r#"test "\$REFUSED" = success"#,
            r#"test -z "\$REFUSED_APPLIED""#,
            r"e2e-review:\s+needs: \[changes, static\]\n\s+if: \$\{\{ needs\.changes\.outputs\.e2e == 'true' && needs\.changes\.outputs\.review == 'true' \}\}",
            r#"git cat-file -t "\$BASE:\$file""#,
            "Exactly one scripted verdict was posted",
            r"HEAD_SHA: \$\{\{ github.event.pull_request.head.sha \}\}",
            r#"contains\("Reviewed SHA: " \+ \$sha \+ "\\n"\)"#,
            r#"\(\.key == "e2e-review" and \$review != "true"\)"#,
        ],
        &[],
    ),
    // The review caller: the base's policy and actions, the head as data.
    (
        "review.yml",
        "",
        &[
            r"pull_request_target: *(?:#[^\n]*)?\n\s+types: \[opened, synchronize, reopened, ready_for_review\]",
            "kind: analysis",
            "notify: none",
            "review-task: workflow/review.md",
        ],
        &[r"contents: write|secrets: inherit|pull_request.head|refs/pull"],
    ),
    // The review agent analyses: no project toolchain or build cache.
    (
        "review.yml",
        "agent",
        &[
            r"(?s)if: \$\{\{ !cancelled\(\) && needs.register.result == 'success' .*policy: \$\{\{ toJSON\(needs.policy.outputs\) \}\}",
        ],
        &[
            r"rust-toolchain|rust-cache|setup-(?:node|python|go|java)|cargo (?:build|test|fmt|clippy)|npm (?:test|ci)",
        ],
    ),
    (
        "review.yml",
        "register",
        &[r"ref: \$\{\{ needs.policy.outputs.ref \}\}"],
        &[],
    ),
    // The status takes the verdict check set and the head policy admitted,
    // nothing the agent wrote, and runs only the pinned source's binary.
    (
        "review.yml",
        "status",
        &[
            r"VERDICT: \$\{\{ needs.check.outputs.verdict \}\}",
            r#""\$RUNNER_TEMP/bin/agentic-job" review status"#,
            r"REVIEW_URL: \$\{\{ needs.submit.outputs.review-url \}\}",
            r"if: \$\{\{ !cancelled\(\) && needs.policy.outputs.review-head != '' && needs.policy.outputs.binary-artifact-id != '' && needs.agent.result != 'cancelled' \}\}",
            "runs-on: ubuntu-26.04",
            r"HEAD_SHA: \$\{\{ needs.policy.outputs.review-head \}\}",
        ],
        &[
            r"needs\.agent\.outputs|needs\.check\.outputs\.artifact|pull_request\.head|needs\.policy\.outputs\.ref\b",
        ],
    ),
    (
        "review.yml",
        "carry",
        &[
            r"HEAD_SHA: \$\{\{ needs.policy.outputs.review-head \}\}",
            r#"(?s)if: >-\n\s+\$\{\{ vars.REVIEW_CARRY_APPROVAL == 'true' && .*agentic-job" review carry --output "\$GITHUB_OUTPUT""#,
        ],
        &[],
    ),
    // The verdict is posted once: as the review, or else as apply's
    // comment. Only a checked verdict, from check's upload, is submitted;
    // the footer's job link is the agent job's own ID, from Actions.
    (
        "review.yml",
        "submit",
        &[
            r"HEAD_SHA: \$\{\{ needs.policy.outputs.review-head \}\}",
            "vars.AGENT_REAL_REVIEWS == 'true'",
            r"binary-artifact-id != '' && \(needs.carry.outputs.from != '' \|\|\s+\(needs.check.result == 'success' && needs.check.outputs.artifact-id != ''",
            r#"accepted=\(--accepted "\$RUNNER_TEMP/checked/agent_output.json"\)\n.*\n\s+"\$RUNNER_TEMP/bin/agentic-job" review submit "\$\{accepted\[@\]\}""#,
            r"VERDICT: \$\{\{ needs.check.outputs.verdict \}\}",
            r"AGENT_EXIT: \$\{\{ needs.agent.outputs.exit \}\}",
            r"AGENT_JOB: \$\{\{ needs.agent.outputs.job \}\}",
        ],
        &[r"secrets|safe-outputs-artifact-id|pull_request\.head|needs\.policy\.outputs\.ref\b"],
    ),
    (
        "review.yml",
        "apply",
        &[
            r"if: \$\{\{ !cancelled\(\) && needs.policy.result == 'success' && needs.carry.outputs.from == '' && needs.submit.outputs.review-url == '' \}\}",
        ],
        &[],
    ),
    // The refusal of an event review updates the one bounded status
    // comment, with the policy job's binary.
    (
        "review.yml",
        "refusal",
        &[
            "needs.policy.outputs.admitted == 'false' && needs.policy.outputs.binary-artifact-id != ''",
            r"artifact-ids: \$\{\{ needs.policy.outputs.binary-artifact-id \}\}\n",
            r"TEXT: \$\{\{ needs.policy.outputs.refusal \}\}",
            r#""\$RUNNER_TEMP/bin/agentic-job" status\n"#,
            "group: agentic-job-status-",
        ],
        &[],
    ),
    (
        "policy.yml",
        "",
        &[r#"review admit --decision "\$JOB_DIR/event\.json" --output "\$GITHUB_OUTPUT""#],
        &[],
    ),
    // The agent checks out the trusted base, and reports its job's ID.
    (
        "agent.yml",
        "",
        &[
            r"\n {6}job: \$\{\{ job.check_run_id \}\}\n",
            r"\n {6}job:\n {8}value: \$\{\{ jobs.agent.outputs.job \}\}\n",
            r"ref: \$\{\{ fromJSON\(inputs.policy\).ref \}\}",
        ],
        &[],
    ),
    // agentic-job.yml's agent job is agent.yml's, which takes the policy
    // call's outputs as check.yml does; dispatch.yml's configuration goes
    // through unchanged.
    (
        "agentic-job.yml",
        "",
        &[
            r"\n {4}uses: \./\.github/workflows/agent\.yml .*\n(?: {4}.*\n)*? {6}policy: \$\{\{ toJSON\(needs.policy.outputs\) \}\}\n",
            r"(?m)^      config: \$\{\{ inputs\.config \}\}$",
        ],
        &[],
    ),
    // The proposals route keeps check read-only, without secrets, and
    // starts no agent or activation.
    (
        "agentic-job.yml",
        "check",
        &[
            "needs.policy.result == 'success'",
            r"permissions:\n      contents: read",
            r"uses: \./\.github/workflows/check\.yml",
            r"safe-outputs-artifact-id: \$\{\{ needs.agent.outputs.safe-outputs-artifact-id \}\}",
            r"safe-outputs-artifact-digest: \$\{\{ needs.agent.outputs.safe-outputs-artifact-digest \}\}",
        ],
        &[r"secrets[.:]|needs.agent.outputs.(binary|policy|comment)"],
    ),
    (
        "agentic-job.yml",
        "agent",
        &[r"\n    if: .*inputs.proposals-artifact == ''"],
        &[],
    ),
    (
        "policy.yml",
        "activate",
        &[r"\n    if: .*inputs.proposals-artifact == ''"],
        &[],
    ),
];

/// Text each file or job holds, in this order, as (file, job or "", texts).
const TEXTS: &[(&str, &str, &[&str])] = &[
    ("example-compose.yml", "agent", &["uses: ./.agentic-job-source/prepare", "Your privileged steps go here", "name: A caller's privileged step", "End of your privileged steps", "uses: ./.agentic-job-source/secure-host", "uses: ./.agentic-job-source/run", "Your later steps go here", "name: A caller's later step"]),
    ("prepare/action.yml", "", &["artifact-ids:", "The policy call is this action's commit", "- name: Install the binary", "- name: Write what the run is called", "- name: The inference broker's address"]),
    ("run/action.yml", "", &["- name: Prove the host is secured", "The policy call is this action's commit", "- name: Read the run's settings", "- name: Refuse a GitHub token that can write", "- name: Run the agent", "- name: Check again that the repositories are public", "uses: actions/upload-artifact", "id: safe-outputs", "- name: The run's result"]),
    ("policy.yml", "policy", &["Check that the repositories are public", "- name: This workflow"]),
    // Apply posts the comment check folded, before anything that names or
    // marks what it posts.
    (
        "apply.yml",
        "",
        &[
            r#"mv "$GH_AW_TMP/folded.json" "$GH_AW_TMP/agent_output.json""#,
            "- name: Mark what a partial run opens or posts",
            "- name: Leave out what an earlier attempt applied",
        ],
    ),
    (
        "check.yml",
        "",
        &[
            "- name: Put the patch beside the ingested outputs",
            r#"review fold "$GH_AW_TMP/agent_output.json""#,
            "- id: upload",
        ],
    ),
    // The proposals are held to the agent job's digest before they are
    // downloaded, and the download to that digest.
    (
        "check.yml",
        "check",
        &[
            "- name: Require explicit artifact contracts\n",
            "- name: The proposals are the agent job's upload\n",
            "artifact-ids: ${{ inputs.safe-outputs-artifact-id }}",
            "- name: Ingest the outputs",
        ],
    ),
    (
        "check.yml",
        "check",
        &["artifact-ids: ${{ inputs.safe-outputs-artifact-id }}\n          path: .agentic-job/safe-outputs\n          digest-mismatch: error\n"],
    ),
    // Check takes the trusted IDs and the comment target from the policy
    // call, and exports what apply needs.
    (
        "check.yml",
        "",
        &[
            "value: ${{ jobs.detect.outputs.artifact-id }}",
            "value: ${{ jobs.check.outputs.refusal }}",
            "value: ${{ jobs.check.outputs.has-patch }}",
            "${{ fromJSON(inputs.policy).binary-artifact-id }}",
            "${{ fromJSON(inputs.policy).policy-artifact-id }}",
            "${{ fromJSON(inputs.policy).comment-target }}",
        ],
    ),
    (
        "agentic-job.yml",
        "check",
        &["policy: ${{ toJSON(needs.policy.outputs) }}"],
    ),
    // Who starts an automatic review: the owner, members and collaborators,
    // and a Bot whose numeric id REVIEW_BOT_IDS lists as a JSON array; a
    // `/review` comment from anyone, which its bounds hold to write access.
    // Changing it changes who spends inference.
    (
        "review.yml",
        "policy",
        &["    if: >-
      (github.event_name == 'pull_request_target' && !github.event.pull_request.draft &&
        (contains(fromJSON('[\"OWNER\",\"MEMBER\",\"COLLABORATOR\"]'), github.event.pull_request.author_association) ||
          (github.event.pull_request.user.type == 'Bot' && startsWith(vars.REVIEW_BOT_IDS, '[') &&
            contains(fromJSON(vars.REVIEW_BOT_IDS || '[]'), github.event.pull_request.user.id)))) ||
      (github.event.issue.pull_request && startsWith(github.event.comment.body, '/review'))
"],
    ),
    // The policy job reads the bounds at the workflow's commit for
    // pull_request_target, and the agent stays on the event's base.
    (
        "policy.yml",
        "policy",
        &[
            "\n      ref: ${{ inputs.review && !inputs.review-item && (github.event.pull_request.base.sha || inputs.base) || github.sha }}\n",
            "\n          ref: ${{ github.event_name == 'pull_request_target' && github.workflow_sha || (inputs.review && !inputs.review-item && (github.event.pull_request.base.sha || inputs.base) || github.sha) }}\n",
        ],
    ),
    // Without AGENT_CONFIG, review and dispatch read this source's
    // hosted.toml, which the policy job checks out before reading it.
    (
        "policy.yml",
        "policy",
        &[
            "\n          repository: ${{ job.workflow_repository }}\n          ref: ${{ job.workflow_sha }}\n          path: .agentic-job-source\n",
            "- name: Write the configuration",
        ],
    ),
    (
        "review.yml",
        "",
        &["config: ${{ vars.AGENT_CONFIG || '.agentic-job-source/.github/agentic-job/hosted.toml' }}"],
    ),
    (
        "dispatch.yml",
        "",
        &["config: ${{ !inputs.scripted && vars.AGENT_CONFIG || '.agentic-job-source/.github/agentic-job/hosted.toml' }}"],
    ),
    // Policy hands on the issue it checked, not the input.
    (
        "policy.yml",
        "",
        &["\n      issue: ${{ steps.request.outputs.issue }}\n"],
    ),
];

#[test]
fn workflow_shapes() {
    for (file, name, matching, other) in SHAPES {
        let text = workflow(file);
        let text = if name.is_empty() {
            text.as_str()
        } else {
            job(&text, name)
        };
        for (patterns, wanted) in [(matching, true), (other, false)] {
            for pattern in *patterns {
                let found = Regex::new(pattern).unwrap().is_match(text);
                assert_eq!(found, wanted, "{file} {name}: {pattern}");
            }
        }
    }
    for (file, name, texts) in TEXTS {
        let text = workflow(file);
        let text = if name.is_empty() {
            text.as_str()
        } else {
            job(&text, name)
        };
        let at: Vec<usize> = texts
            .iter()
            .map(|want| {
                text.find(want)
                    .unwrap_or_else(|| panic!("{file} {name}: no {want:?}"))
            })
            .collect();
        assert!(at.is_sorted(), "{file} {name}: {texts:?} at {at:?}");
    }
    assert!(root().join(".github/agentic-job/hosted.toml").is_file());
    // Of check's inputs, only the proposals' upload ID and digest are not
    // the policy call's: the producer chooses no binary, policy or target.
    let text = workflow("check.yml");
    let doc = &yaml_rust2::YamlLoader::load_from_str(&text).unwrap()[0];
    let inputs: Vec<&str> = doc["on"]["workflow_call"]["inputs"]
        .as_hash()
        .unwrap()
        .keys()
        .map(|input| input.as_str().unwrap())
        .collect();
    assert_eq!(
        inputs,
        [
            "policy",
            "safe-outputs-artifact-id",
            "safe-outputs-artifact-digest"
        ]
    );
}

/// review.yml's jobs and what each may write: comments in refusal, submit
/// and apply, the status in status, identity tokens in register and
/// check's detector, never in the agent's job, whose only secrets are the
/// register job's run token and its network step's key.
#[test]
fn the_review_caller_keeps_token_separation() {
    let text = workflow("review.yml");
    let doc = &yaml_rust2::YamlLoader::load_from_str(&text).unwrap()[0];
    let names: Vec<&str> = doc["jobs"]
        .as_hash()
        .unwrap()
        .keys()
        .map(|name| name.as_str().unwrap())
        .collect();
    let writes: [(&str, &[&str]); 9] = [
        ("policy", &[]),
        ("refusal", &["pull-requests"]),
        ("carry", &[]),
        ("register", &["id-token"]),
        ("agent", &[]),
        ("check", &["id-token"]),
        ("submit", &["pull-requests"]),
        ("apply", &["pull-requests"]),
        ("status", &["statuses"]),
    ];
    assert_eq!(names, writes.map(|(name, _)| name));
    for (name, wanted) in writes {
        let granted: Vec<&str> = doc["jobs"][name]["permissions"]
            .as_hash()
            .unwrap()
            .iter()
            .filter(|(_, access)| access.as_str() == Some("write"))
            .map(|(permission, _)| permission.as_str().unwrap())
            .collect();
        assert_eq!(granted, wanted, "{name}");
        let secrets = job(&text, name).replace(
            "\n    secrets:\n      INFERENCE_RUN_TOKEN: ${{ needs.register.outputs.run-token }}\n      NETWORK_SECRET: ${{ needs.register.outputs.network-secret }}\n",
            "\n",
        );
        assert!(
            name == "apply" || !secrets.contains("secrets"),
            "{name}: {secrets}"
        );
    }
    let pinned = Regex::new("@[0-9a-f]{40}$").unwrap();
    for action in Regex::new(r"(?m)^ +(?:- )?uses: ([^.\s]\S*)")
        .unwrap()
        .captures_iter(&text)
    {
        assert!(pinned.is_match(&action[1]), "{}", &action[1]);
    }
    // The only uploads read: the policy job's binary of the pinned source,
    // and check's accepted outputs.
    const BINARY: &str = "${{ needs.policy.outputs.binary-artifact-id }}";
    let ids = Regex::new("artifact-ids: (.*)").unwrap();
    for (name, wanted) in [
        ("status", &[BINARY][..]),
        ("carry", &[BINARY]),
        (
            "submit",
            &[BINARY, "${{ needs.check.outputs.artifact-id }}"],
        ),
    ] {
        let found: Vec<&str> = ids
            .captures_iter(job(&text, name))
            .map(|id| id.get(1).unwrap().as_str())
            .collect();
        assert_eq!(found, wanted, "{name}");
    }
}

/// policy.yml's mode check, with `change` over a run that names nothing.
fn mode_gate(dir: &Path, change: &[(&str, &str)]) -> Output {
    let mut env = vec![
        ("RUNNER_TEMP", path(dir)),
        ("PROPOSALS", ""),
        ("EVENT", "false"),
        ("REVIEW", "false"),
        ("PARTIAL", "false"),
        ("TASK", ""),
        ("TASK_ARTIFACT", ""),
    ];
    env.extend_from_slice(change);
    run_step("policy.yml", "Check the proposals mode", dir, &env)
}

/// Proposals come without an event, a review or partial apply; an agent
/// run has a task or derives one.
#[test]
fn the_proposals_mode_gate() {
    let dir = tempfile::tempdir().unwrap();
    for (change, success) in [
        (&[("PROPOSALS", "proposals")][..], true),
        (&[("PROPOSALS", "proposals"), ("EVENT", "true")], false),
        (&[("PROPOSALS", "proposals"), ("REVIEW", "true")], false),
        (&[("PROPOSALS", "proposals"), ("PARTIAL", "true")], false),
        (&[("TASK", "run")], true),
        (&[], false),
        (&[("TASK", "run"), ("PARTIAL", "true")], true),
        (&[("PARTIAL", "true")], false),
        (&[("EVENT", "true")], true),
        (&[("REVIEW", "true")], true),
        (&[("EVENT", "true"), ("REVIEW", "true")], true),
        // The artifact's name is data.
        (&[("PROPOSALS", "$(exit 99)")], true),
    ] {
        let output = mode_gate(dir.path(), change);
        let code = Some(i32::from(!success));
        assert_eq!(output.status.code(), code, "{change:?}: {output:?}");
    }
}

/// Every call of agentic-job.yml here passes the mode check: a task
/// given as prose or an expression is one, uninterpreted.
#[test]
fn every_caller_passes_the_mode_gate() {
    use yaml_rust2::Yaml;
    let dir = tempfile::tempdir().unwrap();
    let mut callers = 0;
    for entry in std::fs::read_dir(root().join(".github/workflows")).unwrap() {
        let file = entry.unwrap().path();
        if file.extension().is_none_or(|extension| extension != "yml") {
            continue;
        }
        let text = std::fs::read_to_string(&file).unwrap();
        let doc = &yaml_rust2::YamlLoader::load_from_str(&text).unwrap()[0];
        for (name, job) in doc["jobs"].as_hash().into_iter().flatten() {
            if job["uses"].as_str() != Some("./.github/workflows/agentic-job.yml") {
                continue;
            }
            let values: Vec<(&str, String)> = [
                ("proposals-artifact", "PROPOSALS"),
                ("event", "EVENT"),
                ("review", "REVIEW"),
                ("apply-partial", "PARTIAL"),
                ("task", "TASK"),
            ]
            .into_iter()
            .filter_map(|(input, variable)| {
                let value = match &job["with"][input] {
                    Yaml::String(value) => value.clone(),
                    Yaml::Boolean(value) => value.to_string(),
                    Yaml::Integer(value) => value.to_string(),
                    _ => return None,
                };
                Some((variable, value))
            })
            .collect();
            let change: Vec<(&str, &str)> = values
                .iter()
                .map(|(variable, value)| (*variable, value.as_str()))
                .collect();
            let output = mode_gate(dir.path(), &change);
            assert!(output.status.success(), "{file:?} {name:?}: {output:?}");
            callers += 1;
        }
    }
    assert!(
        callers >= 8,
        "expected the example, review and CI callers, found {callers}"
    );
}

/// Check takes explicit trusted upload IDs, and one proposal selector:
/// the agent job's upload ID with its digest, or a legacy name.
#[test]
fn the_checker_requires_explicit_artifact_contracts() {
    let dir = tempfile::tempdir().unwrap();
    let digest = "c".repeat(64);
    let prefixed = format!("sha256:{digest}");
    for (change, success) in [
        (&[][..], true),
        (
            &[
                ("PROPOSALS", ""),
                ("DIGEST", ""),
                ("NAME", "board-proposals"),
            ],
            true,
        ),
        (&[("BINARY", "")], false),
        (&[("POLICY", "")], false),
        (&[("PROPOSALS", "")], false),
        (&[("DIGEST", "")], false),
        (&[("DIGEST", prefixed.as_str())], false),
        (&[("DIGEST", &digest[1..])], false),
        (&[("DIGEST", "$(exit 0)")], false),
        (&[("DIGEST", ""), ("NAME", "board-proposals")], false),
        (&[("PROPOSALS", ""), ("NAME", "board-proposals")], false),
        (&[("POLICY", "2,4")], false),
        (&[("PROPOSALS", "$(exit 99)")], false),
        // The legacy name is data.
        (
            &[("PROPOSALS", ""), ("DIGEST", ""), ("NAME", "$(exit 99)")],
            true,
        ),
    ] {
        let mut env = vec![
            ("BINARY", "1"),
            ("POLICY", "2"),
            ("PROPOSALS", "3"),
            ("DIGEST", digest.as_str()),
            ("NAME", ""),
        ];
        env.extend_from_slice(change);
        let output = run_step(
            "check.yml",
            "Require explicit artifact contracts",
            dir.path(),
            &env,
        );
        let code = Some(i32::from(!success));
        assert_eq!(output.status.code(), code, "{change:?}: {output:?}");
    }
}

/// The agent job hands back by upload ID and digest; check holds the upload
/// to that digest, and to this run, before it downloads any of it.
#[test]
fn the_checker_refuses_another_upload_digest() {
    use serde_json::{Value, json};
    let text = workflow("check.yml");
    let doc = &yaml_rust2::YamlLoader::load_from_str(&text).unwrap()[0];
    let step = doc["jobs"]["check"]["steps"]
        .as_vec()
        .unwrap()
        .iter()
        .find(|step| step["name"].as_str() == Some("The proposals are the agent job's upload"))
        .unwrap();
    assert_eq!(
        step["if"].as_str(),
        Some("${{ fromJSON(inputs.policy).proposals-artifact == '' }}")
    );
    assert_eq!(
        step["env"]["DIGEST"].as_str(),
        Some("${{ inputs.safe-outputs-artifact-digest }}")
    );
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    // What the read API says of upload 3.
    executable(
        &bin.join("gh"),
        "#!/bin/bash\n[ \"$*\" = 'api repos/o/r/actions/artifacts/3' ] || exit 9\n[ -n \"$ANSWER\" ] && printf %s \"$ANSWER\"\n",
    );
    let search = format!("{}:{}", path(&bin), std::env::var("PATH").unwrap());
    let digest = "c".repeat(64);
    let artifact = |hash: Value, run: u64| {
        json!({"id": 3, "name": "safe-outputs", "digest": hash, "workflow_run": {"id": run}})
            .to_string()
    };
    let sha = |c: &str| json!(format!("sha256:{}", c.repeat(64)));
    // (the API's answer, the digest the agent job gave, whether check goes on)
    for (answer, given, success) in [
        (artifact(sha("c"), 7), digest.as_str(), true),
        (artifact(sha("d"), 7), &digest, false),
        (artifact(sha("c"), 7), "", false),
        (artifact(Value::Null, 7), &digest, false),
        (artifact(json!(digest), 7), &digest, false),
        (artifact(sha("c"), 8), &digest, false),
        // Not found, or not readable.
        (String::new(), &digest, false),
    ] {
        let output = run_step(
            "check.yml",
            "The proposals are the agent job's upload",
            dir.path(),
            &[
                ("PATH", &search),
                ("PROPOSALS", "3"),
                ("DIGEST", given),
                ("GITHUB_REPOSITORY", "o/r"),
                ("GITHUB_RUN_ID", "7"),
                ("ANSWER", &answer),
            ],
        );
        assert_eq!(
            output.status.success(),
            success,
            "{answer} {given}: {output:?}"
        );
    }
}

/// policy.yml's request step, with a stand-in for the policy command that
/// prints its arguments and `FAKE_SAFE_OUTPUTS` as the policy, and the
/// directory it ran in.
fn request(env: &[(&str, &str)]) -> (Output, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("bin")).unwrap();
    executable(
        &dir.path().join("bin/agentic-job"),
        "#!/bin/bash\nprintf '%s\\0' \"$@\" |\n  jq -Rs --argjson safe \"$FAKE_SAFE_OUTPUTS\" '{args: split(\"\\u0000\")[:-1], safe_outputs: $safe}'\n",
    );
    let output = dir.path().join("output");
    let mut all = vec![
        ("SOURCE_DIR", path(dir.path())),
        ("JOB_DIR", path(dir.path())),
        ("GITHUB_OUTPUT", path(&output)),
        ("GITHUB_REPOSITORY", "o/r"),
        ("ALLOW", "allow"),
        ("REPO", "o/r"),
        ("BASE", "main"),
        ("OUTPUTS", "all"),
        ("TARGET", ""),
        ("ISSUE", ""),
    ];
    all.extend_from_slice(env);
    let output = run_step(
        "policy.yml",
        "Check the request against the caller's bounds",
        dir.path(),
        &all,
    );
    (output, dir)
}

/// The policy command is told when apply holds only the job token, where
/// a push goes and which repository an item is in; an issue is a number,
/// and an item's repository is not for an event, a review or a push.
#[test]
fn the_request_step_tells_the_policy_command_its_token_and_targets() {
    let head = "a".repeat(40);
    let replacement = "b".repeat(40);
    let push = [
        ("PUSH_ITEM", "42"),
        ("PUSH_BRANCH", "agent-run-1"),
        ("PUSH_HEAD", head.as_str()),
        ("REPLACEMENT_BASE", replacement.as_str()),
        ("OUTPUT_REPO", ""),
    ];
    let pushed = |repo: &'static str| {
        let mut args = vec![
            "--pull-request",
            "42",
            "--head",
            head.as_str(),
            "--output-repo",
        ];
        args.push(repo);
        args
    };
    let with = |extra: &[(&'static str, &'static str)]| {
        let mut env = push.to_vec();
        env.extend_from_slice(extra);
        env
    };
    let item = [("ITEM_REPO", "o/tracker")];
    // (apply-environment, issue, other inputs, the policy command's
    // arguments after --max-outputs, or the refusal)
    type Case<'a> = (
        &'a str,
        &'a str,
        Vec<(&'a str, &'a str)>,
        Result<Vec<&'a str>, &'a str>,
    );
    let cases: Vec<Case> = vec![
        ("", "", vec![], Ok(vec!["--job-token"])),
        ("", "64", vec![], Ok(vec!["--job-token"])),
        ("apply", "64", vec![], Ok(vec![])),
        ("apply", "0", vec![], Err("issue is not a number")),
        ("apply", "64 ", vec![], Err("issue is not a number")),
        (
            "apply",
            "owner/repo#64",
            vec![],
            Err("issue is not a number"),
        ),
        ("apply", "$(exit 0)", vec![], Err("issue is not a number")),
        // A push names where apply applies it, for the binary to refuse
        // any repository but the run's: the calling one by default.
        ("apply", "", with(&[]), Ok(pushed("o/r"))),
        (
            "",
            "",
            with(&[]),
            Ok([pushed("o/r"), vec!["--job-token"]].concat()),
        ),
        (
            "apply",
            "",
            with(&[("OUTPUT_REPO", "x/y")]),
            Ok(pushed("x/y")),
        ),
        // The binary holds an item's repository to the bounds; an event's
        // item is the caller's, and a review's or a push's pull request
        // the target's.
        (
            "apply",
            "64",
            item.to_vec(),
            Ok(vec!["--item-repo", "o/tracker"]),
        ),
        (
            "apply",
            "",
            [&item[..], &[("EVENT", "true")]].concat(),
            Err("item-repo is not for an event-triggered run"),
        ),
        (
            "apply",
            "",
            [&item[..], &[("REVIEW_ITEM", "42")]].concat(),
            Err("item-repo is not for a review or a push"),
        ),
        (
            "apply",
            "",
            [&item[..], &[("REVIEW", "true")]].concat(),
            Err("item-repo is not for a review or a push"),
        ),
        (
            "apply",
            "",
            with(&item),
            Err("item-repo is not for a review or a push"),
        ),
    ];
    for (environment, issue, extra, wanted) in cases {
        let mut env = vec![
            ("KIND", "implement"),
            ("MAX_OUTPUTS", "3"),
            ("APPLY_ENVIRONMENT", environment),
            ("ISSUE", issue),
            (
                "FAKE_SAFE_OUTPUTS",
                r#"{"push_to_pull_request_branch": {}}"#,
            ),
        ];
        env.extend_from_slice(&extra);
        let (output, dir) = request(&env);
        let case = format!("{environment:?} {issue:?} {extra:?}: {output:?}");
        let args = match wanted {
            Err(refusal) => {
                assert_eq!(output.status.code(), Some(1), "{case}");
                let stdout = String::from_utf8_lossy(&output.stdout);
                assert!(stdout.contains(&format!("::error::{refusal}")), "{case}");
                continue;
            }
            Ok(args) => args,
        };
        assert!(output.status.success(), "{case}");
        let policy: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.path().join("policy.json")).unwrap())
                .unwrap();
        let given: Vec<&str> = policy["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|arg| arg.as_str().unwrap())
            .collect();
        let at = given
            .iter()
            .position(|arg| *arg == "--max-outputs")
            .unwrap();
        assert_eq!(given[at + 2..], args, "{case}");
        if extra.iter().any(|(key, _)| *key == "PUSH_ITEM") {
            assert_eq!(
                policy["safe_outputs"]["push_to_pull_request_branch"]["replacement_base"],
                replacement.as_str(),
                "{case}"
            );
        }
        let written = std::fs::read_to_string(dir.path().join("output")).unwrap();
        assert!(
            written.lines().any(|line| line == format!("issue={issue}")),
            "{case}"
        );
    }
}

/// An event's item is in the calling repository; with outputs going to
/// another one, check and apply would hide a comment, or act on a review
/// thread, of that number there, an item the event was not about.
#[test]
fn review_thread_types_are_refused_for_an_events_item_elsewhere() {
    for kind in [
        "hide_comment",
        "create_pull_request_review_comment",
        "reply_to_pull_request_review_comment",
        "resolve_pull_request_review_thread",
    ] {
        let safe = format!(r#"{{"noop": {{"max": 1}}, "{kind}": {{"max": 1}}}}"#);
        for (extra, success) in [
            (&[][..], true),
            (&[("EVENT", "true")], true),
            (&[("EVENT", "true"), ("OUTPUT_REPO", "O/R")], true),
            (&[("OUTPUT_REPO", "x/y")], true),
            (&[("EVENT", "true"), ("OUTPUT_REPO", "x/y")], false),
        ] {
            let mut env = vec![
                ("KIND", "analysis"),
                ("MAX_OUTPUTS", "1"),
                ("APPLY_ENVIRONMENT", "apply"),
                ("FAKE_SAFE_OUTPUTS", safe.as_str()),
            ];
            env.extend_from_slice(extra);
            let (output, _dir) = request(&env);
            assert_eq!(
                output.status.success(),
                success,
                "{kind} {extra:?}: {output:?}"
            );
            if !success {
                let refusal = format!(
                    "::error::{kind} is for an event-triggered run only when output-repo is the calling repository"
                );
                assert!(
                    String::from_utf8_lossy(&output.stdout).contains(&refusal),
                    "{kind} {extra:?}: {output:?}"
                );
            }
        }
    }
}
