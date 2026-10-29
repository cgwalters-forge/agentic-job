use std::path::{Path, PathBuf};

use super::*;

fn checkout() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Links as `policy` has them kept: to any host under a top-level domain,
/// which is what the fixtures were generated with.
fn keeps_public(host: &str) -> bool {
    crate::policy::link_domains()
        .iter()
        .any(|tld| host == tld || host.ends_with(&format!(".{tld}")))
}

fn object(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        other => panic!("not an object: {other}"),
    }
}

/// The types and fields are gh-aw's, read at test time from the
/// generated `validation.json`, regenerated from gh-aw's pinned source:
/// a regeneration that changes them fails here. Fields are in the order
/// gh-aw validates them in, which decides the refusal reported.
#[test]
fn types_are_gh_aws() {
    let text = std::fs::read_to_string(checkout().join("safe-outputs/validation.json")).unwrap();
    let validation = object(serde_json::from_str(&text).unwrap());
    for of in TYPES {
        let want = &validation[of.name];
        let names: Vec<&str> = of.fields.iter().map(|field| field.name).collect();
        let order: Vec<&str> = want["fields"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(names, order, "{}", of.name);
        let fields: Map<String, Value> = of
            .fields
            .iter()
            .map(|field| (field.name.to_owned(), Value::Object(field.to_json())))
            .collect();
        let mut got = json!({"defaultMax": of.default_max, "fields": fields});
        if let Some(custom) = &of.custom {
            got["customValidation"] = custom.to_json().into();
        }
        if of.collapse_data {
            got["collapseData"] = true.into();
        }
        assert_eq!(&got, want, "{}", of.name);
    }
    let ours: Vec<&str> = TYPES.iter().map(|of| of.name).collect();
    let theirs: Vec<&str> = validation.keys().map(String::as_str).collect();
    assert_eq!(ours, theirs, "every type of validation.json, in its order");
    // And so every type a policy may allow.
    for name in crate::policy::OUTPUT_TYPES {
        assert!(ours.contains(name), "{name}");
    }
}

/// gh-aw's collector, on each case's `outputs.jsonl` and `config.json`,
/// wrote its `collected.json` (`just collect-fixtures`); this collector
/// must write the same. gh-aw adds an incomplete report to a result with
/// no requests but diagnostics, which `check` drops and this leaves out.
#[test]
fn matches_gh_aws_collector() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/collect");
    let mut cases: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.is_dir())
        .collect();
    cases.sort();
    assert!(cases.len() >= 3, "no cases in {}", dir.display());
    for case in cases {
        let read = |name: &str| std::fs::read_to_string(case.join(name)).unwrap();
        let config = object(serde_json::from_str(&read("config.json")).unwrap());
        let mut want = object(serde_json::from_str(&read("collected.json")).unwrap());
        let mut items: Vec<Map<String, Value>> =
            serde_json::from_value(want.remove("items").unwrap()).unwrap();
        if items
            .last()
            .is_some_and(|last| last["type"] == "report_incomplete")
        {
            items.pop();
        }
        let errors: Vec<String> = serde_json::from_value(want.remove("errors").unwrap()).unwrap();
        let got = collect(
            &read("outputs.jsonl"),
            &config,
            Forge::Github,
            &keeps_public,
        );
        let name = case.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(got.errors, errors, "{name}: errors");
        assert_eq!(got.items.len(), items.len(), "{name}: how many items");
        for (number, (got, want)) in got.items.iter().zip(&items).enumerate() {
            assert_eq!(got, want, "{name}: item {}", number + 1);
        }
    }
}

/// Where this collector refuses what gh-aw's takes or words a refusal
/// its own way; none of these is in the fixtures.
#[test]
fn refusals_of_its_own() {
    let config = object(json!({"add_comment": {"max": 1}, "dispatch_workflow": {"max": 1}}));
    let known: Vec<&str> = TYPES.iter().map(|of| of.name).collect();
    let unsupported = format!(
        "Line 1: Unsupported output type 'dispatch_workflow': agentic-job collect validates only {}",
        known.join(", ")
    );
    let cases = [
        (r#"{"type":"dispatch_workflow"}"#, unsupported.as_str()),
        (
            r#"{"type":"add_comment","body":"x",}"#,
            "Line 1: Invalid JSON - trailing comma at line 1 column 34",
        ),
        ("[1]", "Line 1: Missing required 'type' field"),
        ("null", "Line 1: Missing required 'type' field"),
        (
            r#"{"type":7}"#,
            "Line 1: Invalid JSON - 'type' is not a string",
        ),
    ];
    for (line, want) in cases {
        let got = collect(line, &config, Forge::Github, &keeps_public);
        assert_eq!(got.items, Vec::<Map<String, Value>>::new(), "{line}");
        assert_eq!(got.errors, [want], "{line}");
    }
}

/// On GitLab a label request keeps a target of its kind, and a kind-named
/// target its type does not take there is refused, not dropped; GitHub's
/// and Forgejo's collector stays gh-aw's, which drops it.
#[test]
fn kind_named_targets_by_forge() {
    let config = object(json!({
        "add_labels": {"max": 5}, "remove_labels": {"max": 5},
        "add_comment": {"max": 1}, "close_issue": {"max": 1},
    }));
    let labels = r#"{"type":"add_labels","labels":["bug"],"issue_number":5}"#;
    let merge_request = r#"{"type":"remove_labels","labels":["bug"],"pull_request_number":"5"}"#;
    let comment = r#"{"type":"add_comment","body":"x","issue_number":5}"#;
    let close = r#"{"type":"close_issue","body":"x","issue_number":5,"pull_request_number":5}"#;
    // The line, the forge, the item it keeps, or the refusal.
    let cases: &[(&str, Forge, Result<Value, &str>)] = &[
        (
            labels,
            Forge::Gitlab,
            Ok(json!({"type": "add_labels", "labels": ["bug"], "issue_number": 5})),
        ),
        (
            merge_request,
            Forge::Gitlab,
            Ok(json!({"type": "remove_labels", "labels": ["bug"], "pull_request_number": 5})),
        ),
        (
            labels,
            Forge::Github,
            Ok(json!({"type": "add_labels", "labels": ["bug"]})),
        ),
        (
            labels,
            Forge::Forgejo,
            Ok(json!({"type": "add_labels", "labels": ["bug"]})),
        ),
        (
            comment,
            Forge::Github,
            Ok(json!({"type": "add_comment", "body": "x"})),
        ),
        (
            r#"{"type":"add_labels","labels":["bug"],"issue_number":0}"#,
            Forge::Gitlab,
            Err("Line 1: add_labels 'issue_number' must be a valid positive integer"),
        ),
        (
            comment,
            Forge::Gitlab,
            Err(
                "Line 1: add_comment 'issue_number' is not a field of add_comment on GitLab, and would be dropped",
            ),
        ),
        (
            close,
            Forge::Gitlab,
            Err(
                "Line 1: close_issue 'pull_request_number' is not a field of close_issue on GitLab, and would be dropped",
            ),
        ),
    ];
    for (line, forge, want) in cases {
        let got = collect(line, &config, *forge, &keeps_public);
        match want {
            Ok(item) => {
                assert_eq!(got.errors, Vec::<String>::new(), "{line} {forge:?}");
                assert_eq!(got.items, [object(item.clone())], "{line} {forge:?}");
            }
            Err(error) => {
                assert!(got.items.is_empty(), "{line} {forge:?}");
                assert_eq!(got.errors.len(), 1, "{line} {forge:?}");
                assert!(
                    got.errors[0].starts_with(error),
                    "{forge:?}: {:?}",
                    got.errors
                );
            }
        }
    }
    // The guide names what GitLab takes, and not what check refuses there.
    let guide = field_guide("add_labels", Forge::Gitlab).unwrap();
    assert!(guide.contains("`issue_number`") && guide.contains("`pull_request_number`"));
    assert!(!guide.contains("item_number"), "{guide}");
    let guide = field_guide("add_labels", Forge::Github).unwrap();
    assert!(guide.contains("item_number") && !guide.contains("issue_number"));
}

#[test]
fn parse_int_as_javascript() {
    let cases = [
        ("42", Some(42.0)),
        ("  12abc", Some(12.0)),
        ("+5", Some(5.0)),
        ("-3", Some(-3.0)),
        ("5.9", Some(5.0)),
        ("abc", None),
        ("", None),
        ("0x10", Some(0.0)),
    ];
    for (text, want) in cases {
        assert_eq!(js_parse_int(text), want, "{text:?}");
    }
}

/// The command reads the hand-back's file and the policy's types, and
/// fails where the collector refused a line, as check.yml's step after
/// gh-aw's collector does.
#[test]
fn command_writes_the_result() {
    let dir = tempfile::tempdir().unwrap();
    let mut policy = crate::policy::tests::policy();
    policy.safe_outputs.create_pull_request = None;
    policy.safe_outputs.others.clear();
    policy.safe_outputs.others.insert(
        "noop".to_owned(),
        serde_json::from_value(json!({"max": 1})).unwrap(),
    );
    let policy_path = dir.path().join("policy.json");
    std::fs::write(&policy_path, serde_json::to_string(&policy).unwrap()).unwrap();
    let outputs = dir.path().join("outputs");
    std::fs::create_dir(&outputs).unwrap();
    let collected = dir.path().join("collected.json");
    let args = Args {
        policy: policy_path,
        outputs: outputs.clone(),
        collected: collected.clone(),
    };
    let read = || -> Value {
        serde_json::from_str(&std::fs::read_to_string(&collected).unwrap()).unwrap()
    };

    assert_eq!(run(&args).unwrap(), Exit::Success);
    assert_eq!(read(), json!({"items": [], "errors": []}));

    std::fs::write(
        outputs.join(OUTPUTS_FILE),
        "{\"type\":\"noop\",\"message\":\"Done, @someone\"}\n{\"type\":\"add_comment\",\"body\":\"x\"}\n",
    )
    .unwrap();
    assert_eq!(run(&args).unwrap(), Exit::Failure);
    assert_eq!(
        read(),
        json!({
            "items": [{"type": "noop", "message": "Done, `@someone`"}],
            "errors": ["Line 2: Unexpected output type 'add_comment'. Expected one of: noop"],
        })
    );

    // Not through a link the agent's machine planted, nor past the cap.
    std::fs::remove_file(outputs.join(OUTPUTS_FILE)).unwrap();
    std::os::unix::fs::symlink(&args.policy, outputs.join(OUTPUTS_FILE)).unwrap();
    let err = run(&args).unwrap_err().to_string();
    assert!(err.ends_with("is not a regular file"), "{err}");
    std::fs::remove_file(outputs.join(OUTPUTS_FILE)).unwrap();
    let big = usize::try_from(MAX_OUTPUTS_BYTES).unwrap() + 1;
    std::fs::write(outputs.join(OUTPUTS_FILE), "\n".repeat(big)).unwrap();
    let err = run(&args).unwrap_err().to_string();
    assert!(err.contains("over 1048576"), "{err}");
}

/// The collector is told of a secondary pull request in its count.
#[test]
fn the_policy_as_the_collector_takes_it() {
    let mut policy = crate::policy::tests::policy();
    let max = |config: &Map<String, Value>| config["create_pull_request"]["max"].clone();
    assert_eq!(max(&config(&policy).unwrap()), json!(1));
    policy.secondary_pull_request = Some(crate::policy::Secondary {
        repo: "o/tool".into(),
        clone_url: "https://example.invalid/o/tool".into(),
        base: "main".into(),
        branch_prefix: "agentic-job/".into(),
        max: 1,
        max_patch_bytes: 1 << 20,
        max_patch_files: 3,
        protected_files: Vec::new(),
        protect_top_level_dot_folders: true,
        unprotected_dot_folders: Vec::new(),
        protected_paths: Vec::new(),
    });
    assert_eq!(max(&config(&policy).unwrap()), json!(2));
}
