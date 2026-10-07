//! `agentic-job run`: clone the target, get the run token, configure the
//! agent, start it in the sandbox, drive it within the limits, and end
//! the run at the inference proxy. Step 6a of docs/plan.md, on top of
//! [`crate::session`]; step 6b adds the hand-back, redaction and the
//! summary where [`supervise`] says so.
//!
//! `run` is the runner's user. Everything it does in the agent's files
//! (the clone, the configuration, the probes) it does as the sandbox
//! user, through [`enter`].
//!
//! Three things it must never do, each of which the old tree could:
//!
//! - **Run uncapped without being told to.** A run that cannot announce
//!   itself to the proxy does not start ([`Exit::NotStarted`]), and a cap
//!   on model requests that nothing would count is refused rather than
//!   dropped.
//! - **Leave the agent running.** The session kills the sandbox user's
//!   processes when it returns. `run` does the same when it is told to
//!   stop (a signal), and when it gives up before the session started.
//! - **Leave the token live.** Whatever way the run ends, it is ended at
//!   the proxy, and [`INFERENCE_FILE`] says whether that worked, for the
//!   gate on uploads.
//!
//! The target is cloned before the run is registered, not after as the
//! plan's summary has it: a clone needs no token, and a run that fails to
//! clone then leaves no registration behind for the proxy to remember.

pub mod agent;
pub mod brief;
pub mod clone;
pub mod egress;
pub mod enter;
pub mod handback;
pub mod inference;
pub mod launch;
pub mod probe;
pub mod summary;
mod secrets;

use std::future::Future;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use tokio::signal::unix::{SignalKind, signal};

use self::agent::Kind;
use self::clone::Checkout;
use self::enter::Sandbox;
use self::inference::{Ended, Endpoint, Identity, OidcRequest, Retry, Run};
use crate::config::{self, Config};
use crate::exit::Exit;
use crate::policy;
use crate::session::process::SandboxUser;
use crate::session::{self, Clients, Launch, Limits, Options, Policy, RunResult};

/// Under `--out`: what `run` keeps for itself until step 6b sorts it into
/// the artifacts. Nothing here is redacted yet, and nothing here is
/// uploaded.
pub const WORK_DIR: &str = "work";
/// Under [`WORK_DIR`]: the session's `acp.jsonl`, `agent-stderr.log` and
/// `harness.json`.
pub const HARNESS_DIR: &str = "harness";
/// Under [`WORK_DIR`]: how the run stands at the inference proxy.
pub const INFERENCE_FILE: &str = "inference.json";
pub const INFERENCE_SCHEMA: &str = "agentic-job-inference/v1";
/// The run's identifier and attempt in `--meta`.
const META_RUN_ID: &str = "run_id";
const META_RUN_ATTEMPT: &str = "run_attempt";
/// How long a step of the preparation that was told to stop gets to
/// notice, once the sandbox user's processes are gone.
const STOP_GRACE: Duration = Duration::from_secs(60);
const MAX_PROBE_OUTPUT: usize = 4096;
const PRIVATE_DIR_MODE: u32 = 0o700;

#[derive(Debug, clap::Args)]
pub struct Args {
    /// The policy.json that `policy` printed for this run
    #[arg(long, value_name = "FILE")]
    pub policy: PathBuf,
    /// The task given to the agent
    #[arg(long, value_name = "FILE")]
    pub task: PathBuf,
    /// JSON about the run from the CI system, copied into summary.json
    #[arg(long, value_name = "FILE")]
    pub meta: PathBuf,
    /// The directory the run's results are written to
    #[arg(long, value_name = "DIR")]
    pub out: PathBuf,
    /// The configuration, in place of the root-owned copy `sandbox setup`
    /// made. For tests: a job never passes it, since a file the job can
    /// write is not one the sandbox setup vouched for.
    #[arg(long, value_name = "FILE", hide = true, default_value = config::ROOT_COPY)]
    pub config: PathBuf,
}

/// Everything a run is, checked before anything is started or spent.
struct Plan {
    kind: Kind,
    agent: config::Agent,
    limits: Limits,
    endpoint: Option<Endpoint>,
    identity: Identity,
    /// What `policy` allowed this run: the target, and where it is cloned
    /// from.
    policy: policy::Policy,
    task: String,
    sandbox: Sandbox,
    /// This binary, which the sandbox user runs as the agent's launcher.
    exe: String,
    work: PathBuf,
}

/// What the preparation leaves for the session.
struct Prepared {
    checkout: Checkout,
    /// The model the session asks the agent for.
    model: Option<String>,
    prompt: String,
}

/// What the preparation, on its own thread, shares with whoever may
/// have to stop it.
#[derive(Default)]
struct Shared {
    /// The run, as soon as the proxy has registered it.
    run: Mutex<Option<Arc<Run>>>,
    /// Set when the run was told to stop: the preparation goes no
    /// further than the step it is in.
    stopped: AtomicBool,
}

impl Shared {
    fn run(&self) -> Option<Arc<Run>> {
        self.run
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn go_on(&self) -> Result<()> {
        ensure!(!self.stopped.load(Ordering::SeqCst), "the run was stopped");
        Ok(())
    }
}

/// How a run ended, before that is an exit state.
enum Stage {
    /// It never started: nothing was spent, and trying again is safe.
    NotStarted(anyhow::Error),
    /// A signal stopped it.
    Interrupted(&'static str),
    Ended(Box<RunResult>),
    /// An internal error.
    Broken(anyhow::Error),
}

/// The configuration: the root-owned copy, which the runner's user may
/// need root to read.
fn load_config(path: &Path) -> Result<Config> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
            let out = enter::command("sudo")
                .args(["-n", "cat", "--"])
                .arg(path)
                .stdin(Stdio::null())
                .output()
                .context("running sudo")?;
            ensure!(
                out.status.success(),
                "reading {} as root failed ({})",
                path.display(),
                out.status
            );
            String::from_utf8(out.stdout)
                .with_context(|| format!("{} is not UTF-8", path.display()))?
        }
        Err(err) => {
            return Err(err).with_context(|| {
                format!(
                    "reading {}: `sandbox setup` puts the configuration there",
                    path.display()
                )
            });
        }
    };
    Config::parse(&text).with_context(|| format!("in {}", path.display()))
}

/// The name a run registered without proof asks for: the identifier and
/// attempt `--meta` gives it, which the CI system made unique.
fn run_name(meta: &Value) -> Option<String> {
    let field = |key: &str| match meta.get(key) {
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        Some(Value::Number(n)) => Some(n.to_string()),
        _ => None,
    };
    let id = field(META_RUN_ID)?;
    Some(match field(META_RUN_ATTEMPT) {
        Some(attempt) => format!("{id}-{attempt}"),
        None => id,
    })
}

/// The limits, with the proxy's part in them checked: a cap on model
/// requests binds only where the proxy counts them.
fn limits(config: &config::Limits, endpoint: Option<&Endpoint>) -> Result<Limits> {
    let limits = Limits::from_config(config)?;
    let uncounted = match endpoint {
        None => Some("the run has no inference proxy"),
        Some(endpoint) if !endpoint.mode.has_run_api() => {
            Some("register = \"token-file\" has no run API")
        }
        Some(_) => None,
    };
    if let (Some(cap), Some(why)) = (limits.max_requests, uncounted) {
        bail!(
            "limits.max-requests = {cap} would not bind: nothing counts this run's model \
             requests ({why}). Remove it and cap the run with limits.budget, or say \
             limits.uncapped = true"
        );
    }
    if limits.max_requests.is_none() && limits.budget_aic.is_none() {
        eprintln!(
            "warning: this run is UNCAPPED (limits.uncapped = true): only its timeout of {}s \
             and the proxy's own limits bound what it spends",
            limits.timeout_s
        );
    }
    Ok(limits)
}

impl Plan {
    fn load(args: &Args) -> Result<Self> {
        let config = load_config(&args.config)?;
        let kind = Kind::parse(&config.agent.name)?;
        agent::check(kind, &config.agent)?;
        let endpoint = Endpoint::from_config(&config.inference)?;
        ensure!(
            endpoint.is_some() || !kind.needs_inference(),
            "the agent {} needs inference: set [inference] url and register",
            kind.as_str()
        );
        let limits = limits(&config.limits, endpoint.as_ref())?;
        let read = |path: &Path| {
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))
        };
        let task = read(&args.task)?;
        ensure!(
            !task.trim().is_empty(),
            "the task {} is empty",
            args.task.display()
        );
        let meta: Value = serde_json::from_str(&read(&args.meta)?)
            .with_context(|| format!("parsing {}", args.meta.display()))?;
        ensure!(
            meta.is_object(),
            "{} must hold a JSON object",
            args.meta.display()
        );
        let policy = policy::Policy::load(&args.policy)?;
        clone::check(&policy)?;
        // Asked for only where it is used: nothing else holds what lets
        // a process get the job's identity token.
        let oidc = matches!(
            endpoint.as_ref().map(|endpoint| &endpoint.mode),
            Some(inference::Mode::GithubOidc { .. })
        )
        .then(OidcRequest::from_env)
        .flatten();
        let exe = std::env::current_exe().context("finding this program's own path")?;
        let work = args.out.join(WORK_DIR);
        // Private to the runner's user: the transcript is not redacted
        // yet, and the agent's words in it may hold its token.
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(PRIVATE_DIR_MODE)
            .create(work.join(HARNESS_DIR))
            .with_context(|| format!("creating {}", work.display()))?;
        Ok(Self {
            kind,
            limits,
            endpoint,
            identity: Identity {
                oidc,
                name: run_name(&meta),
            },
            policy,
            task,
            sandbox: Sandbox::new(&config)?,
            exe: clone::utf8(&exe)?.to_owned(),
            work,
            agent: config.agent,
        })
    }

    /// The task, after a pointer to the repository's instructions for
    /// agents that this agent does not read by itself.
    fn prompt(&self, checkout: &Checkout) -> Result<String> {
        let mut found = Vec::new();
        for name in self.kind.unread_instructions() {
            let path = checkout.dir.join(name);
            // Looked for as the sandbox user: the checkout is the agent's.
            let there =
                self.sandbox
                    .run(&["test", "-f", clone::utf8(&path)?], b"", MAX_PROBE_OUTPUT)?;
            if there.success() {
                found.push(*name);
            }
        }
        Ok(if found.is_empty() {
            self.task.clone()
        } else {
            format!(
                "Before starting, read the repository's instructions for agents: {}.\n\n{}",
                found.join(" and "),
                self.task
            )
        })
    }

    /// Everything before the agent starts. A failure here is a run that
    /// never started. The registered run goes into SHARED as soon as
    /// there is one, so that whoever stops this can end it.
    fn prepare(&self, shared: &Shared) -> Result<Prepared> {
        let sandbox = &self.sandbox;
        if self.kind != Kind::Fake {
            // The session starts this binary as the sandbox user.
            sandbox
                .checked(&["test", "-x", &self.exe], b"", MAX_PROBE_OUTPUT)
                .with_context(|| {
                    format!(
                        "{} cannot run {}, which starts its agent: install the binary where \
                         every user can run it, such as /usr/local/bin",
                        sandbox.user, self.exe
                    )
                })?;
        }
        let source = agent::fetch_source(sandbox, &self.agent, self.kind)?;
        shared.go_on()?;
        let checkout = clone::clone(sandbox, &self.policy)?;
        let prompt = self.prompt(&checkout)?;
        shared.go_on()?;
        let run = self
            .endpoint
            .as_ref()
            .map(|endpoint| {
                Run::register(
                    endpoint,
                    &self.identity,
                    Retry::default(),
                    self.limits.timeout_s,
                    &|| shared.stopped.load(Ordering::SeqCst),
                )
                .map(Arc::new)
            })
            .transpose()?;
        *shared.run.lock().unwrap_or_else(PoisonError::into_inner) = run.clone();
        // Stopped while it registered: the token goes to nobody.
        shared.go_on()?;
        if let (Some(cap), Some(run)) = (self.limits.max_requests, &run) {
            // The proxy has the run API and still gave no count: the
            // session would wait for one that never comes, and the cap
            // would never bind.
            ensure!(
                run.counted_at_registration().is_some(),
                "limits.max-requests = {cap} would not bind: the inference proxy registered the \
                 run without a count of its model requests (a {} record)",
                inference::RECORD_SCHEMA
            );
        }
        let configuration = agent::generate(
            self.kind,
            self.endpoint.as_ref(),
            run.as_deref().map(Run::token),
            self.agent.model.as_deref(),
            &source,
        )?;
        agent::install(sandbox, &configuration)?;
        if let (Some(run), Some(file)) = (&run, self.kind.token_file()) {
            let given = match self.endpoint.as_ref().map(|endpoint| &endpoint.mode) {
                Some(inference::Mode::TokenFile { path }) => Some(path.as_path()),
                _ => None,
            };
            probe::require(sandbox, run.token(), file, given)?;
        }
        Ok(Prepared {
            checkout,
            model: configuration.model,
            prompt,
        })
    }

    /// What the session needs to drive the agent on PREPARED.
    fn session(
        &self,
        prepared: Prepared,
        run: Option<&Arc<Run>>,
    ) -> Result<(Options, Option<inference::Counter>)> {
        let mut spec = session::agents::builtin(self.kind.as_str())?;
        if self.kind != Kind::Fake {
            spec.command = launch::command(&self.exe, self.kind);
        }
        let (requests, counter) = run
            .and_then(|run| run.count(inference::COUNT_INTERVAL))
            .unzip();
        // The agent can read its token, and its words are in these lines.
        let secrets = run
            .map(|run| run.token().expose().to_owned())
            .into_iter()
            .chain(self.identity.oidc.iter().map(|oidc| oidc.bearer.clone()));
        let options = Options {
            name: self.kind.as_str().to_owned(),
            agent: spec,
            model: prepared.model,
            cwd: prepared.checkout.dir,
            prompt: prepared.prompt,
            out: self.work.join(HARNESS_DIR),
            permissions: Policy::for_task(&self.sandbox.home)?,
            limits: self.limits.clone(),
            requests,
            launch: Launch::Sandbox {
                user: self.sandbox.user.clone(),
                // The agent works where the session tells it to; the
                // wrapper only needs somewhere that exists.
                wrapper: self.sandbox.wrapper(&self.sandbox.home)?,
            },
            clients: Clients::none(),
            log: Box::new(secrets::Masked::new(std::io::stdout(), secrets)),
        };
        Ok((options, counter))
    }
}

/// The signals that ask a job to stop.
struct Signals {
    terminate: tokio::signal::unix::Signal,
    interrupt: tokio::signal::unix::Signal,
    hangup: tokio::signal::unix::Signal,
}

impl Signals {
    fn new() -> Result<Self> {
        let listen = |kind| signal(kind).context("listening for signals");
        Ok(Self {
            terminate: listen(SignalKind::terminate())?,
            interrupt: listen(SignalKind::interrupt())?,
            hangup: listen(SignalKind::hangup())?,
        })
    }

    /// The name of the next one.
    async fn next(&mut self) -> &'static str {
        tokio::select! {
            _ = self.terminate.recv() => "SIGTERM",
            _ = self.interrupt.recv() => "SIGINT",
            _ = self.hangup.recv() => "SIGHUP",
        }
    }

    /// WORK's result, or the signal that came first. WORK is dropped
    /// then: what it started is the caller's to stop.
    async fn or<T>(&mut self, work: impl Future<Output = T>) -> Result<T, &'static str> {
        tokio::select! {
            done = work => Ok(done),
            name = self.next() => Err(name),
        }
    }
}

/// Ends RUN at the proxy and records how it stands, for the summary and
/// for the gate on uploads. Returns whether the token is known dead, or
/// was never this run's to end.
async fn end_run(run: Arc<Run>, work: &Path) -> Result<bool> {
    let ending = Arc::clone(&run);
    let ended = tokio::task::spawn_blocking(move || ending.end(Retry::default()))
        .await
        .context("ending the run at the proxy")?;
    let (ended, usage) = match ended {
        Ok(Ended::Ended(record)) => (true, record),
        Ok(Ended::NoRunApi) => (false, None),
        Err(err) => {
            eprintln!("error: {err:#}");
            (false, None)
        }
    };
    let state = json!({
        "schema": INFERENCE_SCHEMA,
        "register": run.register_mode().as_str(),
        "run_api": run.has_run_api(),
        "ended": ended,
        "usage": usage.as_ref().map(inference::Record::as_json),
    });
    let path = work.join(INFERENCE_FILE);
    std::fs::write(&path, format!("{state}\n"))
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(ended || !run.has_run_api())
}

/// Runs PLAN, and leaves nothing of it behind: no process of the sandbox
/// user's, and no run the proxy still takes a token for.
async fn supervise(plan: Plan) -> Result<Exit> {
    // Refuses root and this process's own user, before anything of
    // theirs could be killed.
    let user = SandboxUser::find(&plan.sandbox.user).await?;
    let mut signals = Signals::new()?;
    let plan = Arc::new(plan);
    let shared = Arc::new(Shared::default());

    // Its steps block (commands, requests), so it has a thread, which a
    // signal cannot cancel: it ends when its commands are killed, or at
    // the end of the request it is waiting on.
    let mut preparing = tokio::task::spawn_blocking({
        let (plan, shared) = (Arc::clone(&plan), Arc::clone(&shared));
        move || plan.prepare(&shared)
    });
    let prepared = signals.or(&mut preparing).await;
    // A task that has ended must not be waited for again.
    let still_preparing = prepared.is_err();
    let stage = match prepared {
        Err(name) => Stage::Interrupted(name),
        Ok(Err(panic)) => Stage::Broken(anyhow::Error::from(panic).context("preparing the run")),
        Ok(Ok(Err(err))) => Stage::NotStarted(err),
        Ok(Ok(Ok(prepared))) => match plan.session(prepared, shared.run().as_ref()) {
            Err(err) => Stage::Broken(err),
            // Dropped on a signal, the session kills the wrapper it
            // started; the agent itself is the sandbox user's, below.
            Ok((options, _counter)) => match signals.or(session::run(options)).await {
                Err(name) => Stage::Interrupted(name),
                Ok(Err(err)) => Stage::Broken(err),
                Ok(Ok(result)) => Stage::Ended(Box::new(result)),
            },
        },
    };

    // The session stopped the sandbox user if it returned a result. In
    // every other case something of that user's may still run: a clone,
    // or the agent.
    shared.stopped.store(true, Ordering::SeqCst);
    let mut reaped = match &stage {
        Stage::Ended(_) => Ok(()),
        _ => user.reap().await,
    };
    // A run that is known is ended now, not after waiting for a thread.
    let mut settled = match shared.run() {
        Some(run) => Some(end_run(run, &plan.work).await),
        None => None,
    };
    if still_preparing {
        // Until the step it was in has ended, a registration it was in
        // the middle of is not known here, and a command it started
        // after the reaping may be running. One request is shorter than
        // the grace, and it starts no other once it is told to stop.
        let _ = tokio::time::timeout(STOP_GRACE, &mut preparing).await;
        reaped = reaped.and(user.reap().await);
        if let (None, Some(run)) = (&settled, shared.run()) {
            settled = Some(end_run(run, &plan.work).await);
        }
    }

    // However the run ended, what the cleaning up left is said.
    let mut unclean = false;
    for err in [
        reaped.as_ref().err(),
        settled.as_ref().and_then(|settled| settled.as_ref().err()),
    ]
    .into_iter()
    .flatten()
    {
        eprintln!("error: {err:#}");
        unclean = true;
    }
    if matches!(settled, Some(Ok(false))) {
        eprintln!(
            "error: the run was not ended at the inference proxy: its token may still be live, \
             and nothing of this run may be uploaded"
        );
    }
    let exit = match stage {
        Stage::NotStarted(err) => {
            eprintln!("error: the run did not start: {err:#}");
            Exit::NotStarted
        }
        Stage::Interrupted(name) => {
            eprintln!("error: stopped by {name}");
            Exit::Failure
        }
        Stage::Ended(result) => result.result.exit(),
        Stage::Broken(err) => return Err(err),
    };
    ensure!(
        !unclean,
        "the run could not be cleaned up after: see the errors above"
    );
    Ok(exit)
}

pub fn run(args: &Args) -> Result<Exit> {
    let plan = Plan::load(args)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("starting the async runtime")?;
    let exit = runtime.block_on(supervise(plan));
    // A step of the preparation that a signal interrupted may still be
    // waiting on a request: it is not waited for.
    runtime.shutdown_background();
    exit
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run::inference::Mode;

    fn limits_table(text: &str) -> config::Limits {
        Config::parse(&format!("[limits]\ntimeout-minutes = 10\n{text}"))
            .unwrap()
            .limits
    }

    fn endpoint(mode: Mode) -> Endpoint {
        Endpoint {
            url: "http://proxy".into(),
            mode,
            anthropic_url: "http://proxy/anthropic".into(),
            openai_url: "http://proxy/v1".into(),
        }
    }

    /// A cap on model requests is kept where the proxy counts them, and
    /// is never dropped where nothing does: such a configuration is
    /// refused, so that a run is uncapped only by saying so.
    #[test]
    fn a_request_cap_nothing_counts_is_refused() {
        let given = endpoint(Mode::TokenFile {
            path: "/run/token".into(),
        });
        let counted = endpoint(Mode::Plain);
        // (limits, endpoint, the cap kept or the refusal)
        type Case<'a> = (&'a str, Option<&'a Endpoint>, Result<Option<u64>, &'a str>);
        let cases: [Case; 8] = [
            ("max-requests = 150", Some(&counted), Ok(Some(150))),
            (
                "max-requests = 150\nbudget = 500",
                Some(&counted),
                Ok(Some(150)),
            ),
            ("max-requests = 150", Some(&given), Err("would not bind")),
            (
                "max-requests = 150\nbudget = 500",
                Some(&given),
                Err("register = \"token-file\" has no run API"),
            ),
            ("max-requests = 150", None, Err("no inference proxy")),
            ("budget = 500", Some(&given), Ok(None)),
            ("uncapped = true", Some(&given), Ok(None)),
            ("", Some(&given), Err("neither max-requests nor budget")),
        ];
        for (text, endpoint, want) in cases {
            let got = limits(&limits_table(text), endpoint);
            match (got, want) {
                (Ok(limits), Ok(cap)) => assert_eq!(limits.max_requests, cap, "{text}"),
                (Err(err), Err(want)) => {
                    assert!(format!("{err:#}").contains(want), "{text}: {err:#}");
                }
                (got, want) => panic!("{text}: got {got:?}, wanted {want:?}"),
            }
        }
    }

    #[test]
    fn run_names() {
        let cases = [
            (
                json!({"run_id": 37558571698u64, "run_attempt": 2}),
                Some("37558571698-2"),
            ),
            (json!({"run_id": "abc", "run_attempt": "1"}), Some("abc-1")),
            (json!({"run_id": "abc"}), Some("abc")),
            (json!({"run_id": ""}), None),
            (json!({"run_attempt": 1}), None),
            (json!({"run_id": null}), None),
        ];
        for (meta, want) in cases {
            assert_eq!(run_name(&meta).as_deref(), want, "{meta}");
        }
    }
}
