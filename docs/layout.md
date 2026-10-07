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
| `config/` | 1, then each table's step | the `--config` TOML, one struct per table; `config/compose.rs` (step 7) is `config`, which writes that file from a caller's file and from settings given one at a time |
| `policy.rs` | 4 | `policy`: the caller's bounds, the request, and the `Policy` type that `policy.json` holds |
| `check/` | 4 | `check`: the handed-back outputs against the policy, and in `check/patch.rs` the rules for a patch |
| `redact.rs` | 4 | secret-shaped strings; used by `check` and by `run` |
| `files.rs` | 4 | reading a file someone else wrote: no link followed, a size cap; used by `check`, `redact` and `run` |
| `sandbox/setup.rs` | 5 | `sandbox setup` |
| `sandbox/check/` | 5 | `sandbox check`; its probes are a function `run` calls again ([what they prove](sandbox-check.md)); `setup` and `check` together are [the step any job can run first](secure-host.md) |
| `sandbox/enter.rs` | 5 | running a command as the sandbox user with `run0`; `run` starts the agent through it |
| `sandbox/helper.rs`, `sandbox/root.rs` | 5 | `helper`, the privileged operations the runner's user keeps after setup, as root through its one sudo rule; and how the runner's user asks for each (the helper, or sudo itself where setup never ran) |
| `sandbox/host.rs`, `sandbox/local.rs` | 5 | users and programs of the host; the listeners a user can connect to |
| `session/` | 3 | the ACP session, its limits and transcript: a library, no command ([below](#the-session)) |
| `run/` | 6a, 6b | `run` ([below](#run)): `run/inference.rs` (run token), `run/agent.rs` and `run/launch.rs` (agent configuration), `run/clone.rs`, `run/probe.rs`, `run/enter.rs` in 6a; `run/brief.rs` (what the agent is told about handing back), `run/handback.rs`, `run/log.rs` (the job log), `run/egress.rs` (the egress proxy's log), `run/summary.rs`, `run/upload.rs` (the gate on uploads) in 6b |

A module that outgrows its file becomes a directory of the same name
(`check.rs` to `check/mod.rs`); its path in `lib.rs` does not change.

Three types are shared. `config/mod.rs` is read by `sandbox setup` (step 5)
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
  no proxy) the configuration is refused rather than the cap dropped
  ([`config`](#config) and `run` both make that check);
- `Policy::for_task(home)`, the permission policy of a task run;
- where the condensed transcript goes, a line per event: `run/log.rs`,
  which redacts each line and keeps it from acting as a command to the
  CI system on its way to the job log and `condensed.log`.

`session::digest::Digest` follows a recorded `acp.jsonl` as it follows
a live session; the summary (`run/summary.rs`) is built on it.

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
   requests. The task is given after a short text that names the two
   files the agent hands back in, their formats and their caps
   (`run/brief.rs`);
5. ends the run at the proxy, and writes how that went to
   `OUT/work/inference.json`;
6. takes what the agent hands back (`run/handback.rs`): its
   `out/outcome.json`, its requests in `out/safe-outputs.jsonl`, and its
   working tree as one patch against the commit it started from, all
   read as the sandbox user;
7. copies the session's files and the egress proxy's log since the run
   began into the transcript, and redacts it and the results;
8. writes `summary.json` and `summary.md` from the redacted copies
   (`run/summary.rs`);
9. checks all of it at the gate (`run/upload.rs`) and only then moves it
   to where it is uploaded from.

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

What a run that ended leaves under `--out`, in the old tree's names and
schemas:

- `run/`: `summary.json` (`agent-run-summary/v1`), `summary.md`,
  `condensed.log`, `outcome.json`;
- `transcript.tar.zst`: `acp.jsonl`, `harness.json`, `agent-stderr.log`
  and, where the egress proxy runs, `access.log`. The old tree's
  `harness-stderr.log` has no counterpart: the harness is this process,
  and what it says is the job's log;
- `safe-outputs/`, only if something was handed back: `outputs.jsonl`,
  `base.json` and `aw-agent-run-ID.patch`, where ID is `run_id` of
  `--meta`, which a run therefore has to be given;
- `work/`, private to the runner's user and never uploaded: the
  session's own files, unredacted, and where the rest is put together.

Those first three exist only for a run that passed the gate: it was
ended at the inference proxy (or has no run there to end), none of the
secrets the run holds and nothing shaped like one is left in any of it,
and, for the fake agent,
whose session prints such a string on purpose, the redaction replaced
something. Otherwise `run` exits with 2 and says why, and there is
nothing to upload: a workflow uploads what is there and needs no check
of its own, but for whether the target is public, which needs the forge.
A run that never started leaves none of them, and neither does one that
a signal stopped, also while its results were being taken: it starts no
further command, and takes back what it had moved out.

The patch is one commit by `[commit] author`. Its message is the title
and body of the pull request the agent asked for, or of the one made up
from its outcome when it changed files and asked for none, followed by
`[commit] trailers`; the old tree used the title alone. A line of that
text is indented if git, applying the patch as a mail, would read it as
the end of the message, the start of a patch, a header that names
another author, date or subject, or somebody's sign-off. A change is
dropped, with a warning and the reason in `summary.json`, when the
policy allows no pull request or the patch is over the policy's size.
An analysis run's change is not handed back at all, and nothing says
so, as in the old tree.

The checkout's git configuration is the agent's, and git runs there as
the agent. `run` takes from it the hooks, the file monitor and signing,
sets who the commit is by through git's environment, and has
`format-patch` add nothing a configuration asks for: headers of the
agent's own, a sign-off, another sender. `check`, on another machine,
does not hold the patch's author to anything
([#22](https://github.com/cgwalters-forge/agentic-job/issues/22)).

The value of every variable of the job's environment whose name ends in
`_TOKEN` counts as a secret of the run's. One that holds an ordinary
word of eight characters or more would be redacted wherever it occurs,
and would fail the gate for a patch that has the word in it.

Standard output is the condensed transcript and nothing else; the rest
of what `run` says is on standard error. `run` prints no command to the
CI system (`::group::`, a step summary): the workflow wraps the step in
the `agent (condensed)` group and appends `summary.md` to the job's
summary itself. Whatever the agent chose that reaches either stream is
kept to one line, `##[` in it is broken up, and no line starts with
`::`, so GitHub's runner takes none of it for a command.

`run/enter.rs` is `run`'s use of step 5's way into the sandbox
(`sandbox::enter`): the same `run0` command line, with a limit on what
a command may write back and as a wrapper for the session. Its `Runner`
is what the hand-back reads the agent's files through, so that the
tests can read a checkout of their own without a second user.
`run/probe.rs` holds only the probes of the run token; `run` does not
yet call `sandbox check`'s probes again.

The agent is not started by its own command but by `agentic-job
launch-agent NAME`, a hidden command that runs as the sandbox user: it
reads the one file with the token, sets the agent's environment and
becomes the agent. So the binary has to be where the sandbox user can
run it (`/usr/local/bin`), which `run` checks.

`run` has one argument the plan does not: a hidden `--config`, for
tests, in place of the root-owned copy.

### Adding an agent

The session drives any agent that speaks ACP on its standard streams,
and to it an agent is one entry of `session/agents.toml`. `run` is
narrower: it configures Claude Code, opencode and the scripted agent,
and a fourth one is a change to this crate. That is deliberate, and
this is what the change consists of.

What is data, in `session/agents.toml`: the command that starts the
agent's ACP server (the launcher becomes that command, so it is named
nowhere else), how the agent is told the model (`model-env`, or ACP's
`model` session option without it), and `notices`. Set `notices` only
for an agent that takes a prompt sent during a turn into that turn; one
that queues it as a turn of its own ends the running turn, and the run
with it, as claude-agent-acp does. The agent's program itself is
installed by the caller's configuration (`[setup] npm` or `packages`).

What is code because it is logic, and why a caller cannot supply it as
a table:

- **Where inference goes** (`run/agent.rs`): a variant of `Kind` (with
  its name, and whether it needs inference at all), and a function
  from the proxy's address and the run token to the agent's files
  (`Configuration`). It has to say which of the proxy's two routes the
  agent speaks, since they take the token differently: the Anthropic
  route in a header of its own beside a placeholder credential
  (`x-run-token`, where the proxy has the run API), the OpenAI route as
  the API key. And it has to close every other way the agent would
  choose a provider: Claude Code's switches to Bedrock, Vertex and
  Foundry are pinned empty, an opencode configuration must define
  exactly one provider and enable only it.
- **What the target repository can override** (`run/agent.rs`,
  `run/launch.rs`): every agent reads some configuration from the
  directory it works in, which is the repository a stranger's task
  names. Claude Code applies a project's `env` over its own
  environment, so its endpoint is pinned in root's managed settings;
  opencode's project configuration is turned off by a variable. For a
  new agent this has to be found out from its documentation and source
  and then tested: got wrong, the agent's requests go where a file in
  the repository says, the run token with them, and only the egress
  proxy's write rules are left to stop that. It is the reason a caller
  cannot add an agent with a command and two variable names.
- **Its configuration repository**, if a caller may supply one
  (`fetch_source`, `check`): which files are taken, and that none of
  them can move the provider.
- **How the limits bind** (`session/digest.rs`, `session/budget.rs`):
  `limits.max-tasks` counts the tool calls that start a subagent, which
  are known by name (`TASK_TOOLS`: opencode's `task`, Claude Code's
  `Task` and `Agent`, the latter read from the adapter's own `_meta`);
  an agent that names its tool otherwise runs with that cap silently
  off. `limits.budget` binds only for an agent that reports a cost in
  ACP's `usage_update`; one that does not has to be capped by
  `max-requests`, which the proxy counts.

What is code today and is only a fact about the agent, each a `match`
on `Kind` or a list that the compiler or a table test makes a new agent
fill in. These could move into the built-in `agents.toml` (never a
caller's file) without loss; they have not, and #60 tracks it:

- the one file that holds the token (`Kind::token_file`). `run` probes
  that the token is in that file, private to the sandbox user, and
  nowhere else that user can read (`run/probe.rs`). An agent that
  takes its credential only from the environment gets it from the
  launcher, which reads it from such a file, as Claude Code's does;
- what it must not inherit: the prefixes of its own variables
  (`NOT_INHERITED` in `session/process.rs`), so that nothing in the
  job's environment selects its provider, credential or configuration,
  and what the launcher sets in their place, the switches that stop it
  calling home for updates among them (`environment` in
  `run/launch.rs`);
- which of the proxy's addresses it uses (`Kind::api_url`);
- the repository's instructions it does not read by itself
  (`Kind::unread_instructions`), which the task then points it at;
- what its reported cost means, for the summary (`aic_pricing` in
  `run/mod.rs`);
- its name where a caller reads it: the `agent` input of
  `.github/workflows/agentic-job.yml`, and the argument of the hidden
  `launch-agent` command.

Each has a table test beside it (`run/agent.rs`, `run/launch.rs`,
`session/process.rs`) that takes a row for the new agent, and
`tests/probe.rs` runs the token probes as another user. None of that
shows the agent works: a run of it against a real proxy does.

## `config`

Not in the plan's table of commands. A workflow gets a run's settings as
strings from whoever called it, and `sandbox setup` wants one TOML
file. `agentic-job config --from FILE --string TABLE.KEY=VALUE
--integer ... --list ...` writes that file: each setting is the value
of its one key and is never read as TOML, so an input that holds a
quote or a table header cannot add a key, as it could in a file that a
shell script pasted together. A setting whose value is empty sets
nothing, which is how an input the caller left out arrives.

The result is checked as its readers will read it, before a machine is
set up. `config/checked.rs` holds that check, and the tables' own
checks can be called from nowhere outside `config/`: `sandbox setup` and every entry
into the sandbox take the machine's part of it (`Config::check_host`:
the sandbox user, the packages, the direct endpoints) and `run` the
whole (`Config::check`: also that the agent is one `run` can configure,
that an agent with a model behind it has an inference proxy it can
reach and a way to register, that the limits bind, that the commit's
author and trailers are ones git takes). A check of the file alone that
is added there is made by all three; one added in `run` or `sandbox
setup` would be found only minutes into a job.

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

`e2e-full`, `e2e-limit` and `e2e-event` call the reusable workflow
(`.github/workflows/agentic-job.yml`, [described here](workflow.md)) as
another repository would, with the scripted agent, and `e2e-verify`
looks at what they made and removes it. Their bounds file and
configuration are the example caller's, in `.github/agentic-job/`, and
the sessions the scripted agent plays are in `.github/agentic-job/e2e/`.
A change to what the apply job makes is a change to `e2e-verify`.
