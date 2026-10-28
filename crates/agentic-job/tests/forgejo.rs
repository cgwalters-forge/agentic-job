//! `agentic-job apply` against a real Forgejo, where the unit tests of
//! `apply::forgejo` have recorded answers: the API, its token scopes and
//! the git push to a fork, as a release of the forge has them.
//!
//! Needs a fresh Forgejo at `AGENTIC_JOB_TEST_FORGEJO` (`http://127.0.0.1:3000`)
//! running in the container `AGENTIC_JOB_TEST_FORGEJO_CONTAINER`, where
//! `docker exec` makes its admin; a second run needs a new container.
//! CI's `forgejo` job starts one and runs:
//!
//! ```text
//! cargo test --test forgejo -- --ignored --nocapture
//! ```

// Helpers outside a #[test] function are still test code.
#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Value, json};

const BIN: &str = env!("CARGO_BIN_EXE_agentic-job");

const SERVER_VAR: &str = "AGENTIC_JOB_TEST_FORGEJO";
const CONTAINER_VAR: &str = "AGENTIC_JOB_TEST_FORGEJO_CONTAINER";

/// How long a starting server has to answer its health check.
const STARTUP: Duration = Duration::from_secs(120);

/// `admin` is a name Forgejo reserves.
const ADMIN: &str = "forge-admin";
const BOT: &str = "bot";
/// Fixtures of a throwaway server, not secrets.
const PASSWORD: &str = "test-password-not-secret";
/// What apply needs and no more: read the repositories, push the branch,
/// open the pull request and comment.
const SCOPES: &[&str] = &["write:repository", "write:issue"];

const PROXY_VARS: &[&str] = &[
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
];

const NAME: &str = "target";
const RUN_URL: &str = "https://ci.example/runs/1";

/// One user's way in to the API.
enum Auth<'a> {
    Basic(&'a str),
    Token(&'a str),
}

struct Forge {
    server: String,
    http: ureq::Agent,
}

impl Forge {
    /// Sends METHOD to PATH under the API, with BODY, and returns the
    /// status and the JSON answer.
    fn call(&self, auth: &Auth, method: &str, path: &str, body: Option<&Value>) -> (u16, Value) {
        let url = format!("{}/api/v1{path}", self.server);
        let authorization = match auth {
            Auth::Basic(user) => format!("Basic {}", STANDARD.encode(format!("{user}:{PASSWORD}"))),
            Auth::Token(token) => format!("token {token}"),
        };
        let response = match (method, body) {
            ("GET", None) => self
                .http
                .get(&url)
                .header("authorization", &authorization)
                .call(),
            ("PUT", Some(body)) => self
                .http
                .put(&url)
                .header("authorization", &authorization)
                .header("content-type", "application/json")
                .send(body.to_string().as_bytes()),
            ("POST", Some(body)) => self
                .http
                .post(&url)
                .header("authorization", &authorization)
                .header("content-type", "application/json")
                .send(body.to_string().as_bytes()),
            _ => panic!("{method} {path} with {body:?}"),
        };
        let mut response = response.unwrap_or_else(|err| panic!("{method} {path}: {err}"));
        let status = response.status().as_u16();
        let text = response.body_mut().read_to_string().unwrap();
        (status, serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    fn get(&self, auth: &Auth, path: &str) -> Value {
        let (status, answer) = self.call(auth, "GET", path, None);
        assert_eq!(status, 200, "GET {path}: {answer}");
        answer
    }

    fn post(&self, auth: &Auth, path: &str, body: &Value) -> Value {
        let (status, answer) = self.call(auth, "POST", path, Some(body));
        assert!(
            (200..300).contains(&status),
            "POST {path}: {status} {answer}"
        );
        answer
    }
}

/// A server that answers, with its admin made.
fn forge() -> Forge {
    let var = |name: &str| std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set"));
    let server = var(SERVER_VAR).trim_end_matches('/').to_owned();
    let container = var(CONTAINER_VAR);
    let http = ureq::Agent::config_builder()
        .proxy(None)
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(30)))
        .build()
        .into();
    let forge = Forge { server, http };
    let start = Instant::now();
    loop {
        let healthy = forge
            .http
            .get(format!("{}/api/healthz", forge.server))
            .call()
            .is_ok_and(|response| response.status().is_success());
        if healthy {
            break;
        }
        assert!(
            start.elapsed() < STARTUP,
            "{} does not answer",
            forge.server
        );
        std::thread::sleep(Duration::from_secs(2));
    }
    // The API makes no first admin; the server's own CLI does, as its
    // user, not root.
    let output = Command::new("docker")
        .args(["exec", "--user", "git", &container])
        .args(["forgejo", "admin", "user", "create", "--admin"])
        .args(["--username", ADMIN, "--password", PASSWORD])
        .args([
            "--email",
            "admin@example.org",
            "--must-change-password=false",
        ])
        .output()
        .unwrap();
    assert_success("forgejo admin user create", &output);
    forge
}

fn assert_success(what: &str, output: &Output) {
    assert!(
        output.status.success(),
        "{what}: {}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The bounds' outputs for a comment and a pull request.
const PUBLISHING: &str = r#"
create_pull_request = { max = 1 }
add_comment = { max = 1 }
"#;

/// The labels' bounds: `status/protected` blocked, and the label named
/// as `bug`'s ID not allowed.
const LABELLING: &str = r#"
add_labels = { max = 2, allowed = ["bug", "Docs", "docs", "status/triaged"], blocked = ["status/protected"] }
remove_labels = { max = 2, allowed = ["bug", "area/apply"], blocked = ["status/protected"] }
"#;

/// What `policy` writes in DIR as NAME for this server's target, from
/// bounds that name the bot's fork and allow OUTPUTS.
fn policy(forge: &Forge, dir: &Path, name: &str, outputs: &str) -> PathBuf {
    let authority = forge.server.strip_prefix("http://").unwrap();
    let bounds = format!(
        r#"
forge = "forgejo"
hosts = ["{authority}"]
allow_http = ["{authority}"]
repos = ["{ADMIN}/{NAME}"]
bases = ["main"]
push_repo = "{BOT}/{NAME}"
max_outputs = 2
max_patch_bytes = 65536
max_patch_files = 10

[outputs]
{outputs}"#
    );
    let allow = dir.join(format!("{name}.toml"));
    std::fs::write(&allow, bounds).unwrap();
    let types: Vec<&str> = outputs
        .lines()
        .filter_map(|line| line.split_once(" = "))
        .map(|(output, _)| output)
        .collect();
    let output = Command::new(BIN)
        .arg("policy")
        .arg("--allow")
        .arg(&allow)
        .args(["--repo", &format!("{ADMIN}/{NAME}")])
        .args(["--clone-url", &format!("{}/{ADMIN}/{NAME}", forge.server)])
        .args(["--base", "main", "--kind", "branch"])
        .args(["--outputs", &types.join(",")])
        .args(["--max-outputs", "max"])
        .output()
        .unwrap();
    assert_success("agentic-job policy", &output);
    let path = dir.join(format!("{name}.json"));
    std::fs::write(&path, &output.stdout).unwrap();
    path
}

/// A checked upload in DIR, as check's job leaves it: a comment, and a
/// pull request on BRANCH adding FILE on BASE, each mentioning the admin.
fn checked(dir: &Path, base: &str, branch: &str, file: &str) {
    std::fs::create_dir(dir).unwrap();
    let patch = format!(
        "From 2222222222222222222222222222222222222222 Mon Sep 17 00:00:00 2001\n\
         X-GH-AW-Base-Commit: {base}\n\
         From: agent <agent@localhost>\n\
         Date: Tue, 6 Oct 2026 21:02:35 -0400\n\
         Subject: [PATCH] Change things\n\
         \n\
         ---\n \
         {file} | 1 +\n\
         \n\
         diff --git a/{file} b/{file}\nnew file mode 100644\nindex 0000000..587be6b\n\
         --- /dev/null\n+++ b/{file}\n@@ -0,0 +1 @@\n+x\n"
    );
    let name = format!("aw-{branch}.patch");
    std::fs::write(dir.join(&name), &patch).unwrap();
    let report = json!({
        "ok": true,
        "errors": [],
        "items": [
            {"type": "add_comment", "body": format!("Done, @{ADMIN}.")},
            {"type": "create_pull_request", "title": "Fix it", "body": format!("Why, @{ADMIN}."),
             "branch": branch},
        ],
        "patch": {"file": name, "bytes": patch.len(), "base_commit": base, "files": [file]},
    });
    std::fs::write(dir.join("report.json"), report.to_string()).unwrap();
}

/// A checked upload in DIR of ITEMS alone.
fn checked_items(dir: &Path, items: &[Value]) {
    std::fs::create_dir(dir).unwrap();
    let report = json!({"ok": true, "errors": [], "items": items, "patch": null});
    std::fs::write(dir.join("report.json"), report.to_string()).unwrap();
}

/// The names of the labels of issue 1 of REPO, sorted.
fn labels(forge: &Forge, auth: &Auth, repo: &str) -> Vec<String> {
    let answer = forge.get(auth, &format!("/repos/{repo}/issues/1/labels"));
    let mut names: Vec<String> = answer
        .as_array()
        .unwrap()
        .iter()
        .map(|label| label["name"].as_str().unwrap().to_owned())
        .collect();
    names.sort();
    names
}

/// Runs apply as a caller's job does, without a proxy git would send the
/// loopback server's traffic to.
fn apply(forge: &Forge, policy: &Path, checked: &Path, token: &str) -> Output {
    let mut command = Command::new(BIN);
    for name in PROXY_VARS {
        command.env_remove(name);
    }
    command
        .args(["apply", "--forge", "forgejo", "--server", &forge.server])
        .args(["--repo", &format!("{ADMIN}/{NAME}")])
        .arg("--policy")
        .arg(policy)
        .arg("--checked")
        .arg(checked)
        .args(["--comment-target", "1", "--run-url", RUN_URL])
        .args(["--push-repo", &format!("{BOT}/{NAME}")])
        .env("FORGEJO_TOKEN", token)
        .output()
        .unwrap()
}

/// The names of REPO's branches.
fn branches(forge: &Forge, auth: &Auth, repo: &str) -> Vec<String> {
    let answer = forge.get(auth, &format!("/repos/{repo}/branches"));
    let mut names: Vec<String> = answer
        .as_array()
        .unwrap()
        .iter()
        .map(|branch| branch["name"].as_str().unwrap().to_owned())
        .collect();
    names.sort();
    names
}

#[test]
#[ignore = "needs a Forgejo container; CI's forgejo job runs it"]
fn apply_publishes_to_forgejo() {
    let forge = forge();
    let admin = Auth::Basic(ADMIN);
    forge.post(
        &admin,
        "/admin/users",
        &json!({"username": BOT, "email": "bot@example.org", "password": PASSWORD,
                "must_change_password": false}),
    );
    let token = forge.post(
        &Auth::Basic(BOT),
        &format!("/users/{BOT}/tokens"),
        &json!({"name": "apply", "scopes": SCOPES}),
    );
    let token = token["sha1"].as_str().unwrap();
    let bot = Auth::Token(token);
    let target = format!("{ADMIN}/{NAME}");
    let fork = format!("{BOT}/{NAME}");
    forge.post(
        &admin,
        "/user/repos",
        &json!({"name": NAME, "auto_init": true, "default_branch": "main"}),
    );
    let issue = forge.post(
        &admin,
        &format!("/repos/{target}/issues"),
        &json!({"title": "Something to fix"}),
    );
    assert_eq!(issue["number"], 1);
    forge.post(&bot, &format!("/repos/{target}/forks"), &json!({}));
    let main = forge.get(&admin, &format!("/repos/{target}/branches/main"));
    let base = main["commit"]["id"].as_str().unwrap();

    let dir = tempfile::tempdir().unwrap();
    let publishing = policy(&forge, dir.path(), "publishing", PUBLISHING);
    let footer = format!("\n\n> Generated by [agentic-job]({RUN_URL})");
    let neutralized = format!("@\u{200b}{ADMIN}");

    // A patch to the forge's own CI configuration is protected: apply
    // refuses the whole hand-back before its first write.
    let protected = dir.path().join("protected");
    checked(&protected, base, "agent-run-1", ".forgejo/workflows/x.yml");
    let output = apply(&forge, &publishing, &protected, token);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("protected files: .forgejo/workflows/x.yml"),
        "{stderr}"
    );
    assert!(output.stdout.is_empty(), "{output:?}");
    assert_eq!(branches(&forge, &bot, &fork), ["main"]);

    let accepted = dir.path().join("accepted");
    checked(&accepted, base, "agent-run-2", "src/x");
    let output = apply(&forge, &publishing, &accepted, token);
    assert_success("agentic-job apply", &output);
    let urls = String::from_utf8(output.stdout).unwrap();
    let urls: Vec<&str> = urls.lines().collect();
    assert_eq!(urls.len(), 2, "{urls:?}");

    // The comment, by the bot, with its mention neutralized and the footer.
    let comments = forge.get(&admin, &format!("/repos/{target}/issues/1/comments"));
    let [comment] = comments.as_array().unwrap().as_slice() else {
        panic!("{comments}")
    };
    assert_eq!(comment["user"]["login"], BOT);
    assert_eq!(comment["body"], format!("Done, {neutralized}.{footer}"));
    assert_eq!(comment["html_url"], urls[1]);

    // The pull request, from the fork's branch, never the target's.
    let pulls = forge.get(&admin, &format!("/repos/{target}/pulls?state=all"));
    let [pull] = pulls.as_array().unwrap().as_slice() else {
        panic!("{pulls}")
    };
    assert_eq!(pull["html_url"], urls[0]);
    assert_eq!(pull["user"]["login"], BOT);
    assert_eq!(pull["title"], "WIP: Fix it");
    assert_eq!(pull["body"], format!("Why, {neutralized}.{footer}"));
    assert_eq!(pull["head"]["repo"]["full_name"], fork.as_str());
    assert_eq!(pull["head"]["ref"], "agent-run-2");
    assert_eq!(pull["base"]["ref"], "main");
    let files = forge.get(
        &admin,
        &format!("/repos/{target}/pulls/{}/files", pull["number"]),
    );
    let files: Vec<&str> = files
        .as_array()
        .unwrap()
        .iter()
        .map(|file| file["filename"].as_str().unwrap())
        .collect();
    assert_eq!(files, ["src/x"]);
    assert_eq!(branches(&forge, &admin, &target), ["main"]);
    assert_eq!(branches(&forge, &admin, &fork), ["agent-run-2", "main"]);

    // Labels, where Forgejo's endpoints act on more than they name: a
    // removal takes a name before an ID, and an exclusive label replaces
    // the item's other of its scope. Neither may reach a blocked label.
    let mut ids = std::collections::HashMap::new();
    for (name, exclusive) in [
        ("bug", false),
        ("Docs", false),
        ("docs", false),
        ("status/triaged", true),
        ("status/protected", true),
        ("area/apply", false),
    ] {
        let label = forge.post(
            &admin,
            &format!("/repos/{target}/labels"),
            &json!({"name": name, "color": "#ee0701", "exclusive": exclusive}),
        );
        ids.insert(name, label["id"].clone());
    }
    // A blocked label named as the ID of an allowed one.
    let bug = ids["bug"].to_string();
    forge.post(
        &admin,
        &format!("/repos/{target}/labels"),
        &json!({"name": bug, "color": "#ee0701"}),
    );
    forge.post(
        &admin,
        &format!("/repos/{target}/issues/1/labels"),
        &json!({"labels": [bug, "bug", "status/protected", "area/apply"]}),
    );
    // Commenting needs no more, but labelling needs write access.
    let path = format!("/repos/{target}/collaborators/{BOT}");
    let (status, answer) = forge.call(&admin, "PUT", &path, Some(&json!({"permission": "write"})));
    assert_eq!(status, 204, "PUT {path}: {answer}");
    let before = labels(&forge, &admin, &target);
    let labelling = policy(&forge, dir.path(), "labelling", LABELLING);
    let label = |name: &str, output: &str, names: &[&str]| -> Output {
        let checked = dir.path().join(name);
        let item = json!({"type": output, "repo": target, "item_number": 1, "labels": names});
        checked_items(&checked, &[item]);
        apply(&forge, &labelling, &checked, token)
    };

    let output = label("add-exclusive", "add_labels", &["status/triaged"]);
    // What the forge says refuses it as an error, before any write.
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("would remove status/protected"), "{stderr}");
    assert_eq!(labels(&forge, &admin, &target), before);

    // By name, its `/` encoded in one path segment.
    let output = label("remove-bug", "remove_labels", &["bug", "area/apply"]);
    assert_success("agentic-job apply remove_labels", &output);
    let mut want = vec![bug.clone(), "status/protected".to_owned()];
    want.sort();
    assert_eq!(labels(&forge, &admin, &target), want);

    let output = label("add-docs", "add_labels", &["Docs", "docs"]);
    assert_success("agentic-job apply add_labels", &output);
    want.extend(["Docs".to_owned(), "docs".to_owned()]);
    want.sort();
    assert_eq!(labels(&forge, &admin, &target), want);
}
