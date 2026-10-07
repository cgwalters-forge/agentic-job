//! The text `run` puts before the task: where the agent leaves what it
//! hands back, in what format, and how much. In the old tree only the
//! caller's own brief said this, so every caller had to copy it. What
//! the files are is this binary's to know, since this binary reads them;
//! how to work, and what to work on, stays the caller's task.

use std::fmt::Write;
use std::path::Path;

use super::handback::{
    AGENT_OUTCOME, AGENT_OUTPUTS, MAX_LINES, MAX_OUTCOME_BYTES, MAX_OUTPUTS_BYTES,
};
use crate::policy::{CREATE_PULL_REQUEST, Kind, Policy};

const KIB: usize = 1 << 10;

/// The text for a run of POLICY whose agent has the home HOME and works
/// in CHECKOUT. It ends with an empty line.
pub fn hand_back(policy: &Policy, home: &Path, checkout: &Path) -> String {
    let outputs = &policy.safe_outputs;
    let pull_request = policy.kind == Kind::Branch && outputs.create_pull_request.is_some();
    let mut text = String::from(
        "How this run hands back its results. Nothing else leaves this machine, and all of it \
         is published.\n\n",
    );
    let tree = if pull_request {
        format!(
            "everything you change, add or delete in it (ignored files excepted) is collected \
             when you finish, as one patch of at most {} KiB against the commit you started \
             from, and proposed as a pull request. Leave your changes uncommitted, and do not \
             push.",
            policy.max_patch_bytes / (KIB as u64)
        )
    } else {
        "this run proposes no change to the repository, so nothing you change in it is handed \
         back."
            .to_owned()
    };
    let _ = writeln!(text, "- The working tree, `{}`: {tree}", checkout.display());

    let types: Vec<String> = outputs
        .types()
        .filter(|&name| pull_request || name != CREATE_PULL_REQUEST)
        .map(|name| match outputs.max_of(name) {
            Some(max) => format!("`{name}` (at most {max})"),
            None => format!("`{name}`"),
        })
        .collect();
    let requests = home.join(AGENT_OUTPUTS);
    if types.is_empty() {
        let _ = writeln!(
            text,
            "- `{}`: this run may make no requests; leave it unwritten.",
            requests.display()
        );
    } else {
        let _ = writeln!(
            text,
            "- `{}` (optional): your requests, as JSON Lines: one JSON object to a line, each \
             with a `type` and that type's fields as gh-aw's safe outputs define them. At most \
             {} KiB, {MAX_LINES} lines and {} requests in all. This run allows: {}. A request \
             of another type, or one that is malformed, gets all of them refused, the change \
             included.",
            requests.display(),
            MAX_OUTPUTS_BYTES / KIB,
            policy.max_outputs,
            types.join(", "),
        );
    }
    if pull_request {
        text.push_str(
            "  To word the pull request yourself, write `{\"type\": \"create_pull_request\", \
             \"title\": \"...\", \"body\": \"...\"}`: the title and the body are also the \
             message of the commit. Without one, a pull request is made from the `summary` \
             below.\n",
        );
    }
    let _ = writeln!(
        text,
        "- `{}`: your report, written last, as one JSON object of at most {} KiB: \
         `{{\"summary\": \"...\", \"tests\": [{{\"command\": \"...\", \"exit_code\": 0, \
         \"duration_s\": 12}}], \"stopped_early\": null}}`. `summary` says what you did and \
         why; `tests` lists the commands you ran to check it, with their real exit codes; \
         `stopped_early` is null, or why you stopped before the task was done.",
        home.join(AGENT_OUTCOME).display(),
        MAX_OUTCOME_BYTES / KIB,
    );
    text.push('\n');
    text
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::policy::SafeOutputs;

    fn policy(kind: Kind, outputs: serde_json::Value) -> Policy {
        Policy {
            repo: "o/r".into(),
            clone_url: "https://example.invalid/o/r".into(),
            base: "main".into(),
            kind,
            max_outputs: 3,
            max_patch_bytes: 1 << 20,
            safe_outputs: serde_json::from_value::<SafeOutputs>(outputs).unwrap(),
        }
    }

    fn text(policy: &Policy) -> String {
        hand_back(
            policy,
            Path::new("/home/agent"),
            Path::new("/home/agent/work/r"),
        )
    }

    #[test]
    fn the_text_names_both_files_their_formats_and_their_caps() {
        let pull = json!({
            "create_pull_request": {
                "max": 1, "protected_files": [], "protect_top_level_dot_folders": true,
                "protected_files_policy": "blocked", "draft": true, "max_patch_size": 1024,
                "max_patch_files": 100,
            },
            "noop": {"max": 1},
        });
        let branch = text(&policy(Kind::Branch, pull.clone()));
        // (what a branch run with pull requests is told)
        for want in [
            "`/home/agent/work/r`: everything you change",
            "one patch of at most 1024 KiB",
            "Leave your changes uncommitted, and do not push.",
            "`/home/agent/out/safe-outputs.jsonl` (optional)",
            "At most 1024 KiB, 1000 lines and 3 requests in all",
            "This run allows: `create_pull_request` (at most 1), `noop` (at most 1).",
            "{\"type\": \"create_pull_request\", \"title\": \"...\", \"body\": \"...\"}",
            "the message of the commit",
            "`/home/agent/out/outcome.json`: your report",
            "one JSON object of at most 64 KiB",
            "\"stopped_early\": null}`",
        ] {
            assert!(branch.contains(want), "{want}\n{branch}");
        }
        assert!(branch.ends_with(".\n\n"), "{branch:?}");

        // An analysis run is offered no pull request, whatever the policy
        // file says; a run with no outputs is told to write none.
        let analysis = text(&policy(Kind::Analysis, pull));
        assert!(
            analysis.contains("this run proposes no change"),
            "{analysis}"
        );
        assert!(
            analysis.contains("This run allows: `noop` (at most 1)."),
            "{analysis}"
        );
        assert!(!analysis.contains("create_pull_request"), "{analysis}");
        let nothing = text(&policy(Kind::Branch, json!({})));
        assert!(nothing.contains("this run proposes no change"), "{nothing}");
        assert!(
            nothing.contains("this run may make no requests"),
            "{nothing}"
        );
        assert!(nothing.contains("outcome.json"), "{nothing}");
    }
}
