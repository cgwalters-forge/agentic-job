//! Read-only audit of downloaded artifacts. These are evidence, not a
//! security attestation: the agent's machine can forge its own summary.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use clap::Args as ClapArgs;
use serde_json::{Value, json};

use crate::{exit::Exit, files, run::summary};

const MAX_ARTIFACT_BYTES: u64 = 8 << 20;

#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Directory containing downloaded agent-run, safe-outputs and checked-outputs artifacts
    pub dir: PathBuf,
    /// Print the machine-readable agentic-job-audit/v1 report
    #[arg(long)]
    pub json: bool,
}

/// Fixed paths only; never follow a link in an artifact directory or file.
fn read(dir: &Path, names: &[&str]) -> Result<Option<Vec<u8>>> {
    for name in names {
        let path = dir.join(name);
        if let Some(parent) = path.parent().filter(|p| *p != dir) {
            match std::fs::symlink_metadata(parent) {
                Ok(meta) => ensure!(meta.is_dir(), "{} is not a directory", parent.display()),
                Err(err) if err.kind() == ErrorKind::NotFound => continue,
                Err(err) => return Err(err).context("reading artifact directory"),
            }
        }
        match files::read_regular(&path, MAX_ARTIFACT_BYTES) {
            Ok(bytes) => return Ok(Some(bytes)),
            Err(files::ReadError::Io(err)) if err.kind() == ErrorKind::NotFound => {}
            Err(err) => return Err(err).with_context(|| format!("reading {}", path.display())),
        }
    }
    Ok(None)
}

fn read_json(dir: &Path, names: &[&str]) -> Result<Option<Value>> {
    read(dir, names)?
        .map(|bytes| serde_json::from_slice(&bytes).context("parsing artifact JSON"))
        .transpose()
}

pub fn inspect(dir: &Path) -> Result<Value> {
    let s = read_json(
        dir,
        &["agent-run/summary.json", "run/summary.json", "summary.json"],
    )?
    .context("missing summary.json (download the agent-run artifact)")?;
    ensure!(
        s["schema"] == summary::SCHEMA,
        "unsupported run summary schema"
    );
    let collected = read_json(
        dir,
        &[
            "checked-outputs/agent_output.json",
            "check-diagnostics/agent_output.json",
            "agent_output.json",
        ],
    )?;
    let checked = read_json(
        dir,
        &[
            "checked-outputs/report.json",
            "check-diagnostics/report.json",
            "report.json",
        ],
    )?;
    let outputs = read(dir, &["safe-outputs/outputs.jsonl", "outputs.jsonl"])?;
    let handed_back = outputs
        .map(|bytes| -> Result<Vec<Value>> {
            bytes
                .split(|b| *b == b'\n')
                .filter(|line| !line.iter().all(u8::is_ascii_whitespace))
                .enumerate()
                .map(|(i, line)| {
                    serde_json::from_slice(line)
                        .with_context(|| format!("parsing outputs.jsonl line {}", i + 1))
                })
                .collect()
        })
        .transpose()?;
    let mut findings = Vec::new();
    let mut finding =
        |code: &str, evidence: Value| findings.push(json!({"code": code, "evidence": evidence}));
    if checked.is_none() {
        finding("check_missing", Value::Null);
    }
    if collected.is_none() {
        finding("collector_missing", Value::Null);
    }
    if handed_back.is_none() {
        finding("handback_missing", Value::Null);
    }
    if let Some(check) = &checked {
        ensure!(
            check["ok"].is_boolean() && check["errors"].is_array(),
            "invalid check report"
        );
        if check["ok"] == false {
            finding("check_refused", check["errors"].clone());
        }
    }
    if let Some(collector) = &collected {
        ensure!(
            collector["errors"].is_array() && collector["items"].is_array(),
            "invalid collector report"
        );
        if collector["errors"]
            .as_array()
            .is_some_and(|errors| !errors.is_empty())
        {
            finding("collector_refused", collector["errors"].clone());
        }
    }
    for (code, evidence) in [
        ("proxy_denied", &s["egress_denied"]),
        ("permission_denied", &s["permissions"]["denied"]),
        ("run_failure", &s["failures"]),
    ] {
        if evidence.as_array().is_some_and(|a| !a.is_empty()) {
            finding(code, evidence.clone());
        }
    }
    if !s["patch"]["error"].is_null() {
        finding("patch_dropped", s["patch"]["error"].clone());
    }
    if s["aic"].is_null() {
        finding("cost_unknown", Value::Null);
    }
    Ok(json!({
        "schema": "agentic-job-audit/v1", "run_id": s["run_id"], "result": s["result"],
        "cost": {"aic": s["aic"], "budget": s["aic_budget"], "pricing": s["aic_pricing"],
            "source": "agent-reported, unverified"},
        "tokens": s["tokens"], "tokens_source": s["tokens_source"],
        "requests": s["praxis"]["requests"], "turns": s["turns"], "duration_s": s["duration_s"],
        "tools": s["tools"], "permissions": s["permissions"], "egress_denied": s["egress_denied"],
        "handed_back": handed_back, "patch": s["patch"], "collector": collected,
        "check": checked, "findings": findings,
    }))
}

pub fn run(args: &Args) -> Result<Exit> {
    let report = inspect(&args.dir)?;
    if args.json {
        println!("{}", ci_safe_json(&serde_json::to_string_pretty(&report)?));
    } else {
        // JSON escapes terminal controls; additionally break legacy CI commands,
        // which GitHub recognizes even in the middle of a line.
        println!(
            "Audit run {}: {}",
            ci_safe_json(&report["run_id"].to_string()),
            ci_safe_json(&report["result"].to_string())
        );
        println!(
            "{}",
            summary::cost_line(
                &json!({"aic": report["cost"]["aic"], "aic_budget": report["cost"]["budget"]})
            )
        );
        for key in [
            "duration_s",
            "requests",
            "turns",
            "tokens",
            "tokens_source",
            "tools",
            "egress_denied",
            "permissions",
            "handed_back",
            "patch",
            "collector",
            "check",
            "findings",
        ] {
            println!("{key}: {}", ci_safe_json(&report[key].to_string()));
        }
    }
    Ok(Exit::Success)
}

/// Unicode escapes preserve parsed JSON values without exposing CI commands.
fn ci_safe_json(text: &str) -> String {
    text.replace("##[", "\\u0023\\u0023[")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audit_evidence_and_missing_checks() {
        for refused in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join("summary.json"), json!({"schema": summary::SCHEMA,
                "tools": {"Bash": {"calls": 2}}, "egress_denied": [{"domain": "example.org", "count": 1}],
                "aic": 3.5}).to_string()).unwrap();
            if refused {
                std::fs::write(
                    dir.path().join("report.json"),
                    r#"{"ok":false,"errors":["protected file"]}"#,
                )
                .unwrap();
            }
            let audit = inspect(dir.path()).unwrap();
            assert_eq!(audit["tools"]["Bash"]["calls"], 2);
            assert_eq!(audit["cost"]["aic"], 3.5);
            let expected = if refused {
                "check_refused"
            } else {
                "check_missing"
            };
            assert!(
                audit["findings"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|finding| finding["code"] == expected)
            );
            assert!(audit["handed_back"].is_null());
        }
    }

    #[test]
    fn unsafe_or_malformed_artifacts_fail() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("summary.json");
        for content in ["not json", r#"{"schema":"other"}"#] {
            std::fs::write(&file, content).unwrap();
            assert!(inspect(dir.path()).is_err());
        }
        std::fs::remove_file(&file).unwrap();
        std::os::unix::fs::symlink("/nonexistent", &file).unwrap();
        assert!(inspect(dir.path()).is_err());
        std::fs::remove_file(&file).unwrap();
        std::os::unix::fs::symlink("/nonexistent", dir.path().join("agent-run")).unwrap();
        assert!(inspect(dir.path()).is_err());
    }
}
