//! Admission guards use history fetched by the trusted policy job, never
//! by the agent. This is a snapshot check, not an atomic reservation.

use std::path::Path;

use anyhow::{Context, Result, ensure};
use chrono::{DateTime, FixedOffset, Utc};
use serde::Deserialize;

use super::Trigger;

const DAY_SECONDS: i64 = 24 * 60 * 60;
const MAX_HISTORY_BYTES: u64 = 4 * 1024 * 1024;

fn timestamp(value: &str) -> Result<DateTime<FixedOffset>> {
    DateTime::parse_from_rfc3339(value).context("expected an RFC 3339 timestamp")
}

pub(super) fn validate(trigger: &Trigger) -> Result<()> {
    if let Some(deadline) = &trigger.stop_after {
        timestamp(deadline).context("[trigger] stop-after")?;
    }
    ensure!(
        trigger.cooldown != Some(0),
        "[trigger] cooldown must be positive seconds"
    );
    ensure!(
        trigger.max_runs_per_user != Some(0),
        "[trigger] max-runs-per-user must be positive"
    );
    Ok(())
}

/// Trusted history attests complete admitted-start coverage from `since`,
/// regardless of workflow creation time. A paginated created-time query alone
/// cannot establish this coverage: queued or approval-delayed runs may be older.
/// Count mismatches refuse truncated history rather than silently undercounting.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct History {
    coverage: Coverage,
    workflow_id: u64,
    current_run_id: u64,
    since: String,
    total_count: usize,
    workflow_runs: Vec<Run>,
}

/// Required explicitly so legacy created-time histories fail closed.
#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Coverage {
    AdmittedStarts,
}

#[derive(Deserialize)]
struct Run {
    id: u64,
    workflow_id: u64,
    actor: Actor,
    admission: Admission,
}

/// Supplied only from trusted policy/start records, never workflow status.
#[derive(Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
enum Admission {
    NotStarted,
    Started { started_at: String },
}

#[derive(Deserialize)]
struct Actor {
    login: String,
}

pub(super) fn check(trigger: &Trigger, path: Option<&Path>, actor: &str) -> Result<Option<String>> {
    let now = Utc::now().fixed_offset();
    if trigger
        .stop_after
        .as_deref()
        .map(timestamp)
        .transpose()?
        .is_some_and(|deadline| now >= deadline)
    {
        return Ok(Some("the trigger's stop-after deadline has passed".into()));
    }
    if trigger.cooldown.is_none() && trigger.max_runs_per_user.is_none() {
        return Ok(None);
    }
    let Some(path) = path else {
        return Ok(Some(
            "run guards require complete workflow history (--run-history)".into(),
        ));
    };
    let bytes = crate::files::read_regular(path, MAX_HISTORY_BYTES)
        .with_context(|| format!("reading run history {}", path.display()))?;
    let history: History = serde_json::from_slice(&bytes)
        .with_context(|| format!("{}: not workflow run history", path.display()))?;
    evaluate(trigger, &history, actor, now).context("checking workflow run history")
}

fn evaluate(
    trigger: &Trigger,
    history: &History,
    actor: &str,
    now: DateTime<FixedOffset>,
) -> Result<Option<String>> {
    let Coverage::AdmittedStarts = history.coverage;
    let window =
        i64::from(trigger.cooldown.unwrap_or(0)).max(if trigger.max_runs_per_user.is_some() {
            DAY_SECONDS
        } else {
            0
        });
    ensure!(
        history.workflow_id != 0 && history.current_run_id != 0,
        "workflow and current run IDs must be nonzero"
    );
    ensure!(
        history.total_count == history.workflow_runs.len(),
        "incomplete run history: fetch every page"
    );
    ensure!(
        timestamp(&history.since)? <= now - chrono::Duration::seconds(window),
        "admitted-start history does not cover the guard window"
    );
    let mut ids = std::collections::BTreeSet::new();
    let mut user_runs = 0;
    let mut cooling_down = false;
    for run in &history.workflow_runs {
        ensure!(
            run.workflow_id == history.workflow_id,
            "history contains another workflow"
        );
        ensure!(ids.insert(run.id), "history contains duplicate run IDs");
        let started = match &run.admission {
            Admission::NotStarted => continue,
            Admission::Started { started_at } => timestamp(started_at)?,
        };
        if run.id == history.current_run_id {
            continue;
        }
        // Future timestamps count too: clock skew must not bypass a guard.
        let age = (now - started).num_seconds();
        cooling_down |= trigger
            .cooldown
            .is_some_and(|seconds| age < i64::from(seconds));
        if age < DAY_SECONDS && run.actor.login.eq_ignore_ascii_case(actor) {
            user_runs += 1;
        }
    }
    if cooling_down {
        return Ok(Some("the workflow is within its trigger cooldown".into()));
    }
    if trigger
        .max_runs_per_user
        .is_some_and(|max| user_runs >= max)
    {
        return Ok(Some(
            "the actor has reached max-runs-per-user in the last 24 hours".into(),
        ));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deadlines_and_invalid_settings() {
        for (settings, valid, refused) in [
            ("stop-after = '2000-01-01T00:00:00Z'", true, true),
            ("stop-after = '2999-01-01T00:00:00+01:00'", true, false),
            ("stop-after = 'tomorrow'", false, false),
            ("cooldown = 0", false, false),
            ("max-runs-per-user = 0", false, false),
        ] {
            let trigger: Trigger =
                toml::from_str(&format!("events = ['schedule']\n{settings}")).unwrap();
            assert_eq!(validate(&trigger).is_ok(), valid, "{settings}");
            if valid {
                assert_eq!(
                    check(&trigger, None, "alice").unwrap().is_some(),
                    refused,
                    "{settings}"
                );
            }
        }
    }

    #[test]
    fn rolling_windows_and_current_run() {
        let now = timestamp("2026-10-07T12:00:00Z").unwrap();
        for (seconds_ago, login, id, refused) in [
            (0, "alice", 10, false),
            (59, "bob", 11, true),
            (60, "bob", 11, false),
            (86399, "ALICE", 11, true),
            (86400, "alice", 11, false),
            (-1, "bob", 11, true),
        ] {
            let trigger: Trigger =
                toml::from_str("events = ['schedule']\ncooldown = 60\nmax-runs-per-user = 1")
                    .unwrap();
            let history = History {
                coverage: Coverage::AdmittedStarts,
                workflow_id: 1,
                current_run_id: 10,
                since: "2026-10-06T12:00:00Z".into(),
                total_count: 1,
                workflow_runs: vec![Run {
                    id,
                    workflow_id: 1,
                    admission: Admission::Started {
                        started_at: (now - chrono::Duration::seconds(seconds_ago)).to_rfc3339(),
                    },
                    actor: Actor {
                        login: login.into(),
                    },
                }],
            };
            assert_eq!(
                evaluate(&trigger, &history, "alice", now)
                    .unwrap()
                    .is_some(),
                refused,
                "{seconds_ago} {login} {id}"
            );
        }
    }

    #[test]
    fn incomplete_history_fails_closed() {
        let trigger: Trigger = toml::from_str("events = ['schedule']\ncooldown = 60").unwrap();
        let now = timestamp("2026-10-07T12:00:00Z").unwrap();
        for (since, count) in [("2026-10-07T12:00:00Z", 0), ("2026-10-06T12:00:00Z", 1)] {
            let history = History {
                coverage: Coverage::AdmittedStarts,
                workflow_id: 1,
                current_run_id: 10,
                since: since.into(),
                total_count: count,
                workflow_runs: vec![],
            };
            assert!(evaluate(&trigger, &history, "alice", now).is_err());
        }
        assert!(check(&trigger, None, "alice").unwrap().is_some());
    }

    #[test]
    fn refused_attempts_do_not_extend_windows() {
        let now = timestamp("2026-10-07T12:00:00Z").unwrap();
        let trigger: Trigger =
            toml::from_str("events = ['schedule']\ncooldown = 600\nmax-runs-per-user = 2").unwrap();
        let mut history = History {
            coverage: Coverage::AdmittedStarts,
            workflow_id: 1,
            current_run_id: 10,
            since: "2026-10-06T11:00:00Z".into(),
            total_count: 1,
            workflow_runs: vec![Run {
                id: 1,
                workflow_id: 1,
                actor: Actor {
                    login: "alice".into(),
                },
                admission: Admission::Started {
                    started_at: (now - chrono::Duration::seconds(600)).to_rfc3339(),
                },
            }],
        };
        for actor in ["alice", "outsider", "outsider"] {
            history.workflow_runs.push(Run {
                id: history.workflow_runs.len() as u64 + 1,
                workflow_id: 1,
                actor: Actor {
                    login: actor.into(),
                },
                admission: Admission::NotStarted,
            });
            history.total_count += 1;
            assert!(
                evaluate(&trigger, &history, "alice", now)
                    .unwrap()
                    .is_none()
            );
        }
        history.workflow_runs.push(Run {
            id: 5,
            workflow_id: 1,
            actor: Actor {
                login: "ALICE".into(),
            },
            admission: Admission::Started {
                started_at: (now - chrono::Duration::hours(1)).to_rfc3339(),
            },
        });
        history.total_count += 1;
        assert!(
            evaluate(&trigger, &history, "alice", now)
                .unwrap()
                .unwrap()
                .contains("max-runs-per-user")
        );
        for (id, workflow_id) in [(1, 1), (6, 2)] {
            let last = history.workflow_runs.last_mut().unwrap();
            last.id = id;
            last.workflow_id = workflow_id;
            assert!(evaluate(&trigger, &history, "alice", now).is_err());
        }
    }

    #[test]
    fn old_created_recently_started_run_counts() {
        let now = timestamp("2026-10-07T12:00:00Z").unwrap();
        let history: History = serde_json::from_value(serde_json::json!({
            "coverage": "admitted_starts",
            "workflow_id": 1,
            "current_run_id": 10,
            "since": "2026-10-06T11:00:00Z",
            "total_count": 1,
            "workflow_runs": [{
                "id": 2,
                "workflow_id": 1,
                "created_at": "2026-10-06T11:00:00Z",
                "actor": {"login": "alice"},
                "admission": {"status": "started", "started_at": "2026-10-07T11:59:00Z"}
            }]
        }))
        .unwrap();
        for settings in ["max-runs-per-user = 1", "cooldown = 120"] {
            let trigger: Trigger =
                toml::from_str(&format!("events = ['schedule']\n{settings}")).unwrap();
            assert!(
                evaluate(&trigger, &history, "alice", now)
                    .unwrap()
                    .is_some(),
                "{settings}"
            );
        }
    }

    #[test]
    fn created_time_coverage_is_not_admission_coverage() {
        for coverage in [None, Some("created_time"), Some("unknown")] {
            let mut history = serde_json::json!({
                "workflow_id": 1,
                "current_run_id": 10,
                "since": "2026-10-06T12:00:00Z",
                "total_count": 0,
                "workflow_runs": []
            });
            if let Some(coverage) = coverage {
                history["coverage"] = coverage.into();
            }
            assert!(
                serde_json::from_value::<History>(history).is_err(),
                "{coverage:?}"
            );
        }
    }
}
