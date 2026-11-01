#![cfg(feature = "clap")]
#![allow(clippy::unwrap_used)]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tempfile::TempDir;

struct Fixture {
    dir: TempDir,
    api: String,
    writes: Arc<Mutex<Vec<String>>>,
}

impl Fixture {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let api = format!("http://{}", listener.local_addr().unwrap());
        let writes = Arc::new(Mutex::new(Vec::new()));
        let requests = writes.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                let mut bytes = Vec::new();
                let mut buf = [0; 4096];
                loop {
                    let n = stream.read(&mut buf).unwrap();
                    if n == 0 {
                        break;
                    }
                    bytes.extend_from_slice(&buf[..n]);
                    if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..end]);
                        let length = headers
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length: ")
                                    .and_then(|n| n.parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        if bytes.len() >= end + 4 + length {
                            break;
                        }
                    }
                }
                requests
                    .lock()
                    .unwrap()
                    .push(String::from_utf8(bytes).unwrap());
                let body = r#"{"html_url":"https://github.com/owner/repo/issues/7#issuecomment-1","id":1}"#;
                write!(
                    stream,
                    "HTTP/1.1 201 Created\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .unwrap();
            }
        });
        let fixture = Self {
            dir: tempfile::tempdir().unwrap(),
            api,
            writes,
        };
        fixture.context(json!({"add_comment":{"max":1}, "create_issue":{"max":1}}));
        fixture
    }

    fn context(&self, outputs: Value) {
        std::fs::write(
            self.dir.path().join("context.json"),
            json!({
                "repo":"owner/repo", "item":7, "max_outputs":2,
                "api_url":self.api, "outputs":outputs
            })
            .to_string(),
        )
        .unwrap();
    }

    fn input(&self, text: &str) {
        std::fs::write(self.dir.path().join("outputs.jsonl"), text).unwrap();
    }

    fn run(&self, subcommand: &str) -> Output {
        Command::new(env!("CARGO_BIN_EXE_safe-outputs"))
            .current_dir(self.dir.path())
            .env_clear()
            .env("SAFE_OUTPUTS_TOKEN", "loopback-test-token")
            .args([
                subcommand,
                "--context",
                "context.json",
                "--input",
                "outputs.jsonl",
                "--checked",
                "checked.json",
            ])
            .output()
            .unwrap()
    }

    fn assert_ok(&self, command: &str) {
        let result = self.run(command);
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
}

const COMMENT: &str =
    r#"{"type":"add_comment","repo":"owner/repo","item_number":7,"body":"Deterministic proof."}"#;

#[test]
fn valid_comment_without_run_or_ci_environment() {
    let f = Fixture::new();
    f.input(COMMENT);
    f.assert_ok("check");
    assert!(f.writes.lock().unwrap().is_empty());
    f.assert_ok("apply");
    let writes = f.writes.lock().unwrap();
    assert_eq!(writes.len(), 1);
    assert!(writes[0].starts_with("POST /repos/owner/repo/issues/7/comments "));
    assert!(writes[0].contains(r#""body":"Deterministic proof.""#));
}

#[test]
fn invalid_inputs_never_write() {
    for input in [
        "{bad".to_owned(),
        "x".repeat(1024 * 1024 + 1),
        COMMENT.replace("owner/repo", "other/repo"),
        COMMENT.replace(":7", ":8"),
        COMMENT.replace("item_number", "issue_number"),
        format!("{COMMENT}\n{COMMENT}\n{COMMENT}"),
        format!("{}{}", "\n".repeat(1001), COMMENT),
    ] {
        let f = Fixture::new();
        f.input(&input);
        assert!(!f.run("check").status.success());
        assert!(!f.run("apply").status.success());
        assert!(!f.dir.path().join("checked.json").exists());
        assert!(f.writes.lock().unwrap().is_empty());
    }
}

#[test]
fn changed_content_or_context_never_writes() {
    for change_context in [false, true] {
        let f = Fixture::new();
        f.input(COMMENT);
        f.assert_ok("check");
        if change_context {
            f.context(json!({"add_comment":{"max":2}}));
        } else {
            f.input(&COMMENT.replace("proof", "changed"));
        }
        assert!(!f.run("apply").status.success());
        assert!(f.writes.lock().unwrap().is_empty());
    }
}

#[test]
fn unsupported_operation_refuses_entire_batch_before_writing() {
    let f = Fixture::new();
    f.input(&format!("{COMMENT}\n{}", json!({"type":"create_issue", "repo":"owner/repo", "title":"Unsupported", "body":"This operation must make no writes."})));
    f.assert_ok("check");
    let result = f.run("apply");
    assert!(!result.status.success());
    assert!(
        String::from_utf8_lossy(&result.stderr).contains("unsupported standalone native operation")
    );
    assert!(f.writes.lock().unwrap().is_empty());
}
