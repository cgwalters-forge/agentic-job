//! The built `agentic-job run`, from its command line to its exit state.
//!
//! What a configuration is refused for needs nothing but the binary.
//! The rest runs the fake agent as a second user against the mock
//! inference proxy (`support`) and a repository on this machine, so it
//! needs `AGENTIC_JOB_TEST_SANDBOX_USER`: a user this one can become
//! with `sudo run0`, with `fake-agent` on its PATH. Without it those
//! tests pass without having run, as in `session.rs`.

// clippy.toml lets test functions unwrap, but not the helpers they share,
// which here are most of the file.
#![allow(clippy::unwrap_used)]

mod support;

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use agentic_job::policy::{Kind, Policy, SafeOutputs};
use serde_json::{Value, json};
use support::{Fault, Proxy};

const BIN: &str = env!("CARGO_BIN_EXE_agentic-job");
const SANDBOX_USER_VAR: &str = "AGENTIC_JOB_TEST_SANDBOX_USER";
const EXIT_SUCCESS: i32 = 0;
const EXIT_FAILURE: i32 = 1;
const EXIT_ERROR: i32 = 2;
const EXIT_NOT_STARTED: i32 = 4;
const RUN_ID: &str = "4242";
const PROXY_VARS: [&str; 6] = [
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
];
const DEAD_PROXY: &str = "http://127.0.0.1:1";
const LIMITS: &str = "[limits]\ntimeout-minutes = 5\nmax-requests = 50\n";

/// The sandbox user is one, and ending a run kills every process of it:
/// the tests that use it take turns.
static SANDBOX: Mutex<()> = Mutex::new(());

fn sandbox_user() -> Option<(String, MutexGuard<'static, ()>)> {
    let user = std::env::var(SANDBOX_USER_VAR).ok()?;
    Some((user, SANDBOX.lock().unwrap_or_else(|e| e.into_inner())))
}

fn sh(script: &str) -> String {
    let out = Command::new("sh").args(["-c", script]).output().unwrap();
    assert!(
        out.status.success(),
        "{script}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// The files of one run, in a directory other users can look into.
struct Job {
    dir: PathBuf,
    /// The repository's name, new for every run: the checkout of an
    /// earlier one is still in the sandbox user's home.
    name: String,
}

impl Job {
    fn new(case: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("agentic-job-test-{}-{case}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let job = Self {
            name: format!("target-{}-{case}", std::process::id()),
            dir,
        };
        job.write("task.md", "Do the fake task.\n");
        job.write(
            "meta.json",
            &json!({"run_id": RUN_ID, "run_attempt": 1}).to_string(),
        );
        job.write("policy.json", &job.policy("https://example.invalid/none"));
        job
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    fn write(&self, name: &str, content: &str) {
        std::fs::write(self.path(name), content).unwrap();
    }

    /// A policy as `policy` writes one, for a URL it would not admit:
    /// the bounds are that command's, and `run` clones what it is given.
    fn policy(&self, clone_url: &str) -> String {
        let policy = Policy {
            repo: format!("local/{}", self.name),
            clone_url: clone_url.to_owned(),
            base: "main".to_owned(),
            kind: Kind::Branch,
            max_outputs: 1,
            max_patch_bytes: 1 << 20,
            safe_outputs: SafeOutputs::default(),
        };
        serde_json::to_string(&policy).unwrap()
    }

    /// A repository to clone, which the sandbox user owns: git refuses
    /// to serve one of another user's.
    fn repository(&self, user: &str) {
        let repo = self.path("repo");
        sh(&format!(
            "set -e; chmod 0755 {dir}; git init -q -b main {repo}; cd {repo}; \
             echo 'A target for tests.' > README.md; echo 'Read me first.' > AGENTS.md; \
             git add .; git -c user.name=test -c user.email=test@example.invalid commit -q -m 'Add a target'; \
             sudo -n chown -R {user}: {repo}",
            dir = self.dir.display(),
            repo = repo.display(),
        ));
        self.write(
            "policy.json",
            &self.policy(&format!("file://{}", repo.display())),
        );
    }

    fn command(&self, config: &str) -> Command {
        self.write("config.toml", config);
        let mut command = Command::new(BIN);
        command
            .arg("run")
            .args(["--policy", "policy.json", "--task", "task.md"])
            .args(["--meta", "meta.json", "--out", "out"])
            .args(["--config", "config.toml"])
            .current_dir(&self.dir)
            // A proxy the job's environment names is not used for the
            // run token or the identity token: with one that answers
            // nothing, as here, every registration would fail.
            .envs(PROXY_VARS.map(|name| (name, DEAD_PROXY)))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    fn run(&self, config: &str) -> Finished {
        Finished::of(self.command(config).output().unwrap())
    }

    fn json(&self, path: &str) -> Value {
        let text =
            std::fs::read_to_string(self.path(path)).unwrap_or_else(|e| panic!("{path}: {e}"));
        serde_json::from_str(&text).unwrap()
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        // Part of it may be the sandbox user's by now.
        let _ = Command::new("sudo")
            .args(["-n", "rm", "-rf", "--"])
            .arg(&self.dir)
            .stderr(Stdio::null())
            .status();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

struct Finished {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

impl Finished {
    fn of(output: Output) -> Self {
        Self {
            code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    fn all(&self) -> String {
        format!(
            "exit {:?}\n--- stdout\n{}--- stderr\n{}",
            self.code, self.stdout, self.stderr
        )
    }
}

fn config(user: &str, inference: &str) -> String {
    format!("[sandbox]\nuser = \"{user}\"\n[agent]\nname = \"fake\"\n{LIMITS}{inference}")
}

fn plain(proxy: &Proxy) -> String {
    format!(
        "[inference]\nurl = \"{}\"\nregister = \"plain\"\n",
        proxy.url
    )
}

/// What the sandbox user is still running.
fn processes_of(user: &str) -> String {
    let out = Command::new("pgrep")
        .args(["-l", "-u", user])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// A file of the sandbox user's home, read as root.
fn sandbox_file(user: &str, path: &str) -> Option<String> {
    let out = Command::new("sudo")
        .args([
            "-n",
            "sh",
            "-c",
            "cat -- \"$(getent passwd \"$1\" | cut -d: -f6)/$2\"",
            "sh",
            user,
            path,
        ])
        .output()
        .unwrap();
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// A configuration that would run the agent uncapped, or with a mode
/// nobody chose, is refused before anything starts: exit state 2, and
/// the proxy is never asked.
#[test]
fn configurations_that_are_refused() {
    let proxy = Proxy::start();
    let url = format!("url = \"{}\"\n", proxy.url);
    let fake = "[agent]\nname = \"fake\"\n";
    let cases = [
        (
            format!("{fake}{LIMITS}[inference]\n{url}"),
            "inference.register is not set",
        ),
        (
            format!(
                "{fake}{LIMITS}[inference]\n{url}register = \"token-file\"\ntoken-file = \"/run/t\"\n"
            ),
            "limits.max-requests = 50 would not bind",
        ),
        (
            format!("{fake}{LIMITS}"),
            "limits.max-requests = 50 would not bind",
        ),
        (
            format!("{fake}[limits]\ntimeout-minutes = 5\n"),
            "neither max-requests nor budget",
        ),
        (
            format!("{fake}[limits]\nbudget = 100\n"),
            "limits.timeout-minutes is not set",
        ),
        (
            "[agent]\nname = \"claude\"\n[limits]\ntimeout-minutes = 5\nbudget = 100\n".to_owned(),
            "the agent claude needs inference",
        ),
        (
            "[agent]\nname = \"opencode\"\n[limits]\ntimeout-minutes = 5\nbudget = 100\n"
                .to_owned(),
            "agent.model is not set",
        ),
        (LIMITS.to_owned(), "agent.name is not set"),
        (
            format!(
                "[agent]\nname = \"fake\"\nconfig-repo = \"https://example.invalid/x\"\n{LIMITS}"
            ),
            "the fake agent has no configuration",
        ),
    ];
    let job = Job::new("refused");
    for (config, want) in cases {
        let got = job.run(&config);
        assert_eq!(got.code, Some(EXIT_ERROR), "{config}\n{}", got.all());
        assert!(got.stderr.contains(want), "{want}: {}", got.all());
    }
    assert!(proxy.seen().is_empty(), "{:?}", proxy.seen());
    assert!(!job.path("out/work/harness/harness.json").exists());
}

/// One run, whole: the target is cloned as the sandbox user, the run is
/// announced under its name, the agent works, and the run is ended at
/// the proxy with nothing of the sandbox user's left running.
#[test]
fn a_run_from_clone_to_the_end_of_its_run() {
    let Some((user, _turn)) = sandbox_user() else {
        return;
    };
    let proxy = Proxy::start();
    proxy.admit_unproven();
    let job = Job::new("whole");
    job.repository(&user);
    let got = job.run(&config(&user, &plain(&proxy)));
    assert_eq!(got.code, Some(EXIT_SUCCESS), "{}", got.all());

    let harness = job.json("out/work/harness/harness.json");
    assert_eq!(harness["result"], "success", "{}", got.all());
    assert_eq!(harness["limits"]["max_requests"], 50);
    // The session ran the agent as the sandbox user, in the clone.
    let whoami = format!("work/{}/FAKE.md", job.name);
    assert!(sandbox_file(&user, &whoami).is_some(), "{}", got.all());
    let owner = sh(&format!(
        "sudo -n stat -c %U \"$(getent passwd {user} | cut -d: -f6)/work/{}\"",
        job.name
    ));
    assert_eq!(owner, user);
    assert!(got.stdout.contains("Fake run"), "{}", got.all());
    assert!(got.stderr.contains("Cloned local/"), "{}", got.all());

    let seen = proxy.seen();
    assert_eq!(
        seen.first().map(String::as_str),
        Some("POST /v1/runs"),
        "{seen:?}"
    );
    assert!(seen.iter().any(|r| r == "DELETE /v1/runs/self"), "{seen:?}");
    assert!(seen.iter().any(|r| r == "GET /v1/runs/self"), "{seen:?}");
    assert_eq!(seen.iter().filter(|r| *r == "POST /v1/runs").count(), 1);
    assert_eq!(
        proxy.requests()[0].header("x-run-id"),
        Some(format!("{RUN_ID}-1").as_str())
    );
    assert_eq!(proxy.run_states(), ["finished"]);
    let inference = job.json("out/work/inference.json");
    assert_eq!(inference["schema"], "agentic-job-inference/v1");
    assert_eq!(inference["register"], "plain");
    assert_eq!(inference["ended"], true);
    assert_eq!(inference["usage"]["state"], "finished");
    assert_eq!(processes_of(&user), "");
    // The fake agent has no model: the token was handed to nobody.
    assert!(!got.all().contains("praxis-run-"), "{}", got.all());
}

/// A run that cannot announce itself is exit state 4, and its agent
/// never starts: not uncapped, as the old tree ran it against a proxy
/// with no run API.
#[test]
fn a_run_that_cannot_register_never_starts() {
    let Some((user, _turn)) = sandbox_user() else {
        return;
    };
    // (whether the proxy has the run API, the name of the case)
    for (run_api, case) in [(true, "unadmitted"), (false, "noapi")] {
        let proxy = Proxy::start();
        if !run_api {
            proxy.without_run_api();
        }
        let job = Job::new(case);
        job.repository(&user);
        let got = job.run(&config(&user, &plain(&proxy)));
        assert_eq!(got.code, Some(EXIT_NOT_STARTED), "{case}: {}", got.all());
        assert!(
            got.stderr.contains("the run did not start"),
            "{}",
            got.all()
        );
        assert!(
            !job.path("out/work/harness/harness.json").exists(),
            "{case}"
        );
        assert!(!job.path("out/work/inference.json").exists(), "{case}");
        assert!(!got.stdout.contains("Fake run"), "{}", got.all());
        assert_eq!(proxy.seen(), ["POST /v1/runs"], "{case}");
        assert_eq!(processes_of(&user), "", "{case}");
    }
    // Neither does one whose target cannot be cloned, and that one is
    // not even announced.
    let proxy = Proxy::start();
    proxy.admit_unproven();
    let job = Job::new("noclone");
    job.write(
        "policy.json",
        &job.policy("file:///nonexistent/agentic-job-test"),
    );
    let got = job.run(&config(&user, &plain(&proxy)));
    assert_eq!(got.code, Some(EXIT_NOT_STARTED), "{}", got.all());
    assert!(
        got.stderr.contains("cloning file:///nonexistent"),
        "{}",
        got.all()
    );
    assert!(proxy.seen().is_empty(), "{:?}", proxy.seen());
}

/// A cap on model requests binds only if the proxy counts them. A proxy
/// that registers the run and gives no count leaves the run capped by
/// nothing, so it does not start, and the run it registered is ended.
#[test]
fn a_run_whose_requests_would_not_be_counted_never_starts() {
    let Some((user, _turn)) = sandbox_user() else {
        return;
    };
    let proxy = Proxy::start();
    proxy.admit_unproven().odd_records();
    let job = Job::new("uncounted");
    job.repository(&user);
    let got = job.run(&config(&user, &plain(&proxy)));
    assert_eq!(got.code, Some(EXIT_NOT_STARTED), "{}", got.all());
    assert!(
        got.stderr
            .contains("limits.max-requests = 50 would not bind"),
        "{}",
        got.all()
    );
    assert!(!got.stdout.contains("Fake run"), "{}", got.all());
    assert_eq!(proxy.seen(), ["POST /v1/runs", "DELETE /v1/runs/self"]);
    assert_eq!(proxy.run_states(), ["finished"]);
    assert_eq!(processes_of(&user), "");
}

/// Exit state 4 says a retry is safe, and one on the same machine works:
/// the checkout the first try left is not in its way.
#[test]
fn a_run_that_never_started_can_be_tried_again() {
    let Some((user, _turn)) = sandbox_user() else {
        return;
    };
    let job = Job::new("again");
    job.repository(&user);
    let refusing = Proxy::start();
    let got = job.run(&config(&user, &plain(&refusing)));
    assert_eq!(got.code, Some(EXIT_NOT_STARTED), "{}", got.all());
    let admitting = Proxy::start();
    admitting.admit_unproven();
    let got = job.run(&config(&user, &plain(&admitting)));
    assert_eq!(got.code, Some(EXIT_SUCCESS), "{}", got.all());
    assert_eq!(admitting.run_states(), ["finished"]);
}

/// Told to stop, `run` does not leave the agent running or the token
/// live: the sandbox user's processes are killed and the run is ended.
#[test]
fn a_signal_ends_the_run_and_the_agent() {
    let Some((user, _turn)) = sandbox_user() else {
        return;
    };
    // (the case, what the proxy does to the registration, the request
    // after which the signal is sent)
    let cases = [
        // While the session is starting the agent: the count of the
        // run's requests is first read as the session is handed over.
        ("session", None, "GET /v1/runs/self"),
        // While the run is being registered, on a thread a signal
        // cannot interrupt.
        (
            "registering",
            Some(Fault::Slow(Duration::from_secs(3))),
            "POST /v1/runs",
        ),
    ];
    for (case, fault, after) in cases {
        let proxy = Proxy::start();
        proxy.admit_unproven().fail_registrations(fault);
        let job = Job::new(case);
        job.repository(&user);
        let child = job.command(&config(&user, &plain(&proxy))).spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(120);
        while !proxy.seen().iter().any(|r| r == after) {
            assert!(
                Instant::now() < deadline,
                "{case}: no {after} in {:?}",
                proxy.seen()
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        let pid = rustix::process::Pid::from_child(&child);
        rustix::process::kill_process(pid, rustix::process::Signal::TERM).unwrap();
        let got = Finished::of(child.wait_with_output().unwrap());
        assert_eq!(got.code, Some(EXIT_FAILURE), "{case}: {}", got.all());
        assert!(
            got.stderr.contains("stopped by SIGTERM"),
            "{case}: {}",
            got.all()
        );
        assert_eq!(processes_of(&user), "", "{case}");
        assert_eq!(proxy.run_states(), ["finished"], "{case}: {}", got.all());
        assert_eq!(job.json("out/work/inference.json")["ended"], true, "{case}");
        assert!(
            !got.stdout.contains("Done: the fake run finished"),
            "{case}: {}",
            got.all()
        );
    }
}
