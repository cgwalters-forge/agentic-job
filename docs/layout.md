# Where the code goes

One Cargo workspace, and as few crates as the work allows. This page is
for whoever implements a step of the plan: it says which files are
yours, so that steps written at the same time do not collide.

## Crates

One: `crates/agentic-job`, the released binary and its library, with a
module per command. Add a crate only for code with a second user.

The scripted ACP agent the tests drive is a second binary of the same
crate, `src/bin/fake-agent.rs`. Cargo gives a package's tests the paths
of its own binaries only (`CARGO_BIN_EXE_fake-agent`), and steps 3, 6a
and 6b all need it. The release publishes `agentic-job` alone.

## Modules of `crates/agentic-job`

| Path under `src/` | Step | Holds |
| --- | --- | --- |
| `main.rs`, `cli.rs`, `exit.rs` | 1 | parsing, dispatch, the exit states |
| `config.rs` | 1, then each table's step | the `--config` TOML, one struct per table |
| `policy.rs` | 4 | `policy`: the caller's bounds, the request, and the `Policy` type that `policy.json` holds |
| `check/` | 4 | `check`: the handed-back outputs against the policy, and in `check/patch.rs` the rules for a patch |
| `redact.rs` | 4 | secret-shaped strings; used by `check` and by `run` |
| `files.rs` | 4 | reading a file someone else wrote: no link followed, a size cap; used by `check`, `redact` and `run` |
| `sandbox/setup.rs` | 5 | `sandbox setup` |
| `sandbox/check.rs` | 5 | `sandbox check`; its probes are a function `run` calls again ([what they prove](sandbox-check.md)) |
| `sandbox/enter.rs` | 5 | running a command as the sandbox user with `run0`; `run` starts the agent through it |
| `sandbox/host.rs`, `sandbox/local.rs` | 5 | users and programs of the host; the listeners a user can connect to |
| `session/` | 3 | the ACP session, its limits and transcript: a library, no command ([below](#the-session)) |
| `run/` | 6a, 6b | `run` ([below](#run)): `run/inference.rs` (run token), `run/agent.rs` and `run/launch.rs` (agent configuration), `run/clone.rs`, `run/probe.rs`, `run/enter.rs` in 6a; `run/handback.rs`, `run/summary.rs`, `run/upload.rs` in 6b |

A module that outgrows its file becomes a directory of the same name
(`check.rs` to `check/mod.rs`); its path in `lib.rs` does not change.

Three types are shared. `config.rs` is read by `sandbox setup` (step 5)
and by `run` (step 6a): the file exists now with a struct per table,
and a step changes only the structs of its own tables (`[sandbox]`,
`[egress]` and `[setup]` are step 5's, `[limits]` step 3's,
`[inference]` and `[agent]` step 6a's, `[commit]` step 6b's).
`Policy` is step 4's, and `run` reads it with `Policy::load`: the clone
URL, the caps and the output types allowed. `redact.rs` likewise is
step 4's and used by step 6b, which also takes the name of a patch
(`check::patch_file_name`) and of its base header from `check`.

The egress proxy is not Rust. Step 5 puts mitmproxy's addon, policy and
tests in `egress/` at the top of the repository, as they are in the old
tree's `agent/egress/`.

gh-aw's half of the safe-outputs check, and its handlers, are not Rust
either and are not copied here: the workflows take its scripts from its
setup action, pinned by commit. Step 2a puts what those scripts need in
`safe-outputs/` at the top of the repository. `validation.json` is
generated from the pinned gh-aw (`node safe-outputs/validation.mjs
GH_AW_CHECKOUT`) and regenerated when the pin moves. `fixtures/` holds
the safe-outputs artifacts of two real runs of the old tree, each with
the configuration it ran under and a `source.json` naming the run;
`.github/workflows/safe-outputs-probe.yml` applies them.

## The session

`session::run(Options)` is the whole interface, and `run` its caller. It
starts the agent, drives one task over ACP within the limits, kills
what the agent left running, and writes `acp.jsonl`, `agent-stderr.log`
and `harness.json` to the directory it is given, in the old tree's
schemas. `RunResult::result.exit()` is the run's exit state. What `run`
supplies:

- the agent, an entry of the registry in `session/agents.toml`
  (`session::agents::builtin`). `run` replaces the command of Claude
  Code and of opencode with its launcher (`run/launch.rs`), which
  clears the agent's inherited provider settings (`ANTHROPIC_*`,
  `CLAUDE_*`, `OPENCODE_*`) as the old tree's launchers did; the
  session itself starts nothing with those or with the job's
  `ACTIONS_*` variables;
- `Launch::Sandbox`: the sandbox user, every process of which is killed
  when the session ends, and the wrapper that switches to it (the `run0`
  command line, which is step 5's to build). The kill happens when
  `session::run` returns, not if its future is dropped or the process
  is signalled: `run` sees to those itself, with
  `session::process::SandboxUser::reap`;
- `Limits::from_config` of the `[limits]` table, which refuses a table
  without a timeout, and one that caps neither model requests nor
  spending unless it says `uncapped = true`;
- the run's count of model requests, as a `tokio::sync::watch` channel
  that `run` keeps current from the inference proxy. A request cap
  with no count is refused, and where nothing counts (`token-file`, or
  no proxy) `run` refuses the configuration rather than drop the cap;
- `Policy::for_task(home)`, the permission policy of a task run;
- where the condensed transcript goes, a line per event, which step 6b
  redacts on its way to the job log and `condensed.log`.

`session::digest::Digest` follows a recorded `acp.jsonl` as it follows
a live session; the summary (step 6b) is built on it.

A session keeps a list of attached clients, `session::Clients`, empty
for a task run, to which the agent's notifications are passed on.
Nothing can attach yet, and `session/clients.rs` lists what step 11
has to design before something can.

## `run`

`run::run` checks everything it was given before it starts or spends
anything (an error there is exit state 2), and then, in this order:

1. fetches the agent's configuration repository, if `[agent]` names one,
   and clones the target from the URL in `policy.json`, both as the
   sandbox user (`run/clone.rs`);
2. announces the run to the inference proxy and gets its token
   (`run/inference.rs`): `github-oidc`, `plain` or `token-file`, as
   `[inference] register` says, which has no default;
3. writes the agent's configuration (`run/agent.rs`) with the token in
   one file of the sandbox user's, and probes that it is nowhere else
   (`run/probe.rs`);
4. drives the session, feeding it the proxy's count of the run's model
   requests;
5. ends the run at the proxy, and writes how that went to
   `OUT/work/inference.json`.

A failure in 1 to 3 is exit state 4: the agent never started. The clone
comes before the registration, unlike the plan's summary of `run`,
because it needs no token: a run that cannot clone then leaves nothing
at the proxy. A signal at any point kills the sandbox user's processes
and ends the run at the proxy before `run` exits.

Exit state 4 means trying again is safe, and on the same machine it
works: the checkout an earlier try left is removed first. One case
needs a new attempt of the job and not a second `run` in the same one:
a run that registered and was then ended (a failed probe, say) cannot
register again under the same identity, which the proxy answers with
409.

A cap on model requests is only as good as the proxy's count. `run`
refuses to start if the proxy registered the run without a count, and
the job log says so when the count stops arriving during the run; the
cap then reads the last count, and what still bounds the run is its
timeout and the proxy's own limits. The requests to the proxy and for
the identity token ignore `HTTP_PROXY` and its like in the job's
environment, as the old tree's did: the tokens would pass through it.

Step 6b continues in `run::supervise` after the session: the hand-back,
redaction, the summary and the gate on uploads. Until then the
session's files are in `OUT/work/harness/`, unredacted, and nothing
sorts them into the artifacts. The job log's copy of the condensed
transcript has the run token and the identity-token request masked by
value (`run/secrets.rs`); `redact.rs` replaces that.

`run/enter.rs` is `run`'s use of step 5's way into the sandbox
(`sandbox::enter`): the same `run0` command line, with a limit on what
a command may write back and as a wrapper for the session.
`run/probe.rs` holds only the probes of the run token; `run` does not
yet call `sandbox check`'s probes again.

The agent is not started by its own command but by `agentic-job
launch-agent NAME`, a hidden command that runs as the sandbox user: it
reads the one file with the token, sets the agent's environment and
becomes the agent. So the binary has to be where the sandbox user can
run it (`/usr/local/bin`), which `run` checks.

`run` has one argument the plan does not: a hidden `--config`, for
tests, in place of the root-owned copy.

## Rules that keep steps apart

Each command's module owns its `Args` and its `run(&Args) ->
anyhow::Result<Exit>`. `cli.rs` only lists the commands, so
implementing a command does not edit it (but for `launch-agent`, which
is not in the plan's table). The arguments are the table in
the plan; a change to them is a change to the plan and belongs in the
pull request's description.

`run` returns `Ok(Exit)` for every outcome the plan gives an exit state,
including refusals and failures of the agent. `Err` is for bad input and
internal errors only: `main` prints it and exits 2. So `run` must not
let `?` carry out a failed probe, registration or clone: those are
`Exit::NotStarted`, which tells the caller a retry is safe, and `run`
maps that phase's errors itself.

`main` is synchronous. The session needs an async runtime; `run` builds
it, so the other commands do not pay for it.

Dependencies are declared once, in `[workspace.dependencies]` of the
root `Cargo.toml`, and a crate takes them with `workspace = true`. Two
steps adding dependencies will both touch that table, the crate's own
`[dependencies]` and `Cargo.lock`:
on a rebase keep both sets of lines, take either side's `Cargo.lock`, and
run `cargo metadata`, which adds what is missing and updates nothing
else.

Tests sit beside the code they test (`#[cfg(test)]`); tests of the built
binary go in `crates/agentic-job/tests/`, one file per command, and
their inputs in `crates/agentic-job/tests/data/COMMAND/`. Corpus tests
that run the old tree's code fetch it at the pinned commit in CI. They
do not copy it here. The first is `crates/agentic-job/tests/corpus/`,
for `policy` and `check`: it is Node, because the code it compares with
is, and the top of `corpus.test.mjs` says how to run it.

## CI

`.github/workflows/ci.yml` has one job per kind of check and a last job,
`ci`, that the ruleset requires and that only gathers the others. A step
that needs another runner or another setup (the sandbox probes need
`run0`) adds a job and names it in `ci`'s `needs`. `session-sandbox` is
one: it runs the session's agent as a second user through `run0`, on
ubuntu-26.04, since `run0 --pipe` needs systemd 257. Two steps doing so
both edit that one line; keep both names.

Every dependency has to build for `x86_64-unknown-linux-musl`, since the
release is one static binary; the `static` job builds it on every pull
request.
