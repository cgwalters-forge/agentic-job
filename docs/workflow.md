# Running it on GitHub

[`.github/workflows/agentic-job.yml`](../.github/workflows/agentic-job.yml)
is one agent run as a workflow another repository calls. It is a thin
wrapper: the `agentic-job` binary decides what a run may ask for, runs
it and checks what comes back, and the workflow moves files between
separate jobs on separate machines. The apply job's additional checks are
[listed below](#what-the-apply-job-does-before-it-writes).

1. **policy**, on a small hosted machine. It checks that the target
   repository is public, fetches or builds the binary, and checks the run's request
   against the caller's bounds file (`agentic-job policy`). A request
   outside the bounds stops here, before a machine is spent on the agent.
   Discussion events are refused here until notification and output routing
   can address the discussion namespace safely.
   **activate**, on another hosted machine, removes an admitted command
   label with the caller's write token. The agent and notification jobs
   depend on its success; removal failure starts neither. Runs not started
   by a command label pass through without writing.
2. **agent**, on the caller's runner. [The step that secures a host for
   any job](secure-host.md), the action in `secure-host/`, creates the
   unprivileged user, the network rules and the egress proxy, takes root
   from the job and probes all of it (`agentic-job sandbox setup` and
   `sandbox check`); `run` clones the target, drives the agent and
   leaves what may be uploaded.
3. **check**, on a machine the agent never touched. gh-aw's collector
   validates and sanitizes the agent's requests, then `agentic-job check`
   holds them and the patch to the policy.
4. **apply**, on a machine of its own, the only job with a credential
   that can write. It runs gh-aw's handlers on the checked outputs:
   a branch and a draft pull request, comments.

## What has run, and what has not

Read this before relying on it.

- All four jobs run in this repository's CI on every pull request, twice,
  with the scripted agent (`fake`): once to the end, and once stopped at
  a limit. The egress proxy, the network rules and every probe of
  `sandbox check` are real in those runs. The apply job pushes the
  branches and posts the comment with the job's own token.
- **No real agent has run on this code yet.** Claude Code and opencode
  are configured by code with unit tests, and the run's registration at
  an inference proxy (`github-oidc`, `plain`, `token-file`) by tests
  against a mock proxy. Neither has met a real proxy or a real model
  from this workflow.
- **No pull request has been opened by it.** This organization does not
  let Actions open pull requests, and the token that could
  ([below](#the-apply-job-and-its-token)) is not stored yet. CI requires
  exactly the refusal that follows, with the branch pushed.
- The example caller has been dispatched on this repository's main
  branch: once to the end, with the scripted agent's built-in session,
  and once naming a private repository, which stopped in the policy
  job. homegit's `bot-runs list`, `show`, `log` and `reconcile` read
  the first of those runs.
- The event path (`event: true`) runs in CI on every pull request and
  push of this repository (`e2e-event`, [below](#event-triggered-callers)):
  admitted on a pull request, with the comments on the pull request;
  refused on a push. The slash-command, label and schedule examples are
  live here with the scripted agent.
- **It has not been called from another repository.** CI calls it by
  path, where the workflow's own commit is the run's. That a caller's
  pin by commit selects the source the binary is built from follows
  from GitHub's documentation of `job.workflow_sha` and is untried.
- Not tried at all: `apply-environment`, an `output-repo` other than the
  calling repository, bringing a fork's base branch up to date, the
  tailnet login, a runner that is not GitHub's `ubuntu-26.04`,
  `agent-config-repo`, `kind: analysis`, and applying a `create_issue`:
  that type is checked through gh-aw's collector and `check` in CI
  (the `corpus` job), and its handler has not run from this workflow.

## A caller

[`.github/workflows/example.yml`](../.github/workflows/example.yml) is a
complete one, for the scripted agent, started by hand. Copy it with the
two files it names, and put your own repository in place of this one in
the bounds file's `repos` and in the default of the example's `repo`
input. Three more are started by events, [below](#event-triggered-callers):
a slash command in a comment, a label on a pull request, a schedule.
Every input is described where it is declared, at the top of the
workflow file; this page says what a caller has to provide for them.
The call itself:

```yaml
jobs:
  run:
    uses: cgwalters-forge/agentic-job/.github/workflows/agentic-job.yml@COMMIT
    permissions:
      contents: read
      id-token: write
    with:
      id: ${{ inputs.item }}
      task: ${{ inputs.task }}
      repo: ${{ inputs.repo }}
      allow: .github/agentic-job/allow.toml
      config: .github/agentic-job/runner.toml
      agent: claude
      agent-runner: '["self-hosted", "my-label"]'
      inference-url: ${{ vars.INFERENCE_URL }}
      inference-audience: ${{ vars.INFERENCE_AUDIENCE }}
      output-repo: OWNER/FORK
      apply-environment: agent-apply
      apply-partial: true
```

Name the workflow by a commit you have read, not by a branch. The binary
is built from that same commit, so the pin covers both. Never pass
`secrets: inherit`: the workflow declares the one secret it uses.

### What a caller provides

**A bounds file** in its repository (`allow`): which repositories and
base branches a run may target, which output types and how many, how
large a patch.
[`.github/agentic-job/allow.toml`](../.github/agentic-job/allow.toml)
is this repository's. The inputs `repo`, `base`, `kind`, `outputs` and
`max-outputs` are one run's request, and the policy job refuses a
request the file does not cover. So a workflow that takes those from
whoever dispatches it is still bounded by a file that changes only by a
commit.

For a custom trusted policy job (CLI groundwork only), an organization
wrapper can pin a reusable workflow under
`uses: ORG/.github/.github/workflows/agentic-job.yml@COMMIT` and supply a
second trusted bounds file to `agentic-job policy --org-allow FILE`.
The command checks the request independently against both files, then
takes the lower output and patch caps, the common output types, and the
union of protected file names. Neither file can widen the other. Explicit
requests outside either file are refused; `all`/`max` takes the intersection.
This intersects output policies, not `[trigger]` tables: organization
trigger admission must be checked separately with `event`. The reusable
workflow does not yet expose or fetch an organization bounds file; the
existing policy job cannot be extended by a wrapper merely fetching a file.
A custom policy job must fetch it from a trusted pinned ref before policy
evaluation. Reusable-workflow input and invocation wiring remain part of #118;
the built-in reusable workflow does not support organization bounds yet.

**A configuration file** (`config`) for what depends on the runner
image and not on the run: the services `sandbox setup` stops, the local
sockets `sandbox check` accepts, the egress rules, who the hand-back's
commit is by.
[`.github/agentic-job/hosted.toml`](../.github/agentic-job/hosted.toml)
is the one for GitHub's `ubuntu-26.04`. For another image, start from an
empty `[sandbox]` and read what `sandbox check` fails on: it names each
listener that is left. The inputs for the agent, the proxy, the limits
and the packages are written over this file by `agentic-job config`,
which takes each as the value of one key and never reads it as TOML.

**Runners.** `agent-runner` is the machine the agent works on: thrown
away after the job, with passwordless sudo for the runner's user,
systemd 257 or later (for `run0`), and `apt-get` or `dnf`. `sandbox
setup` refuses a machine it has set up before, and ends by taking root
away from the runner's user: every step after it, this workflow's and
the actions' cleanup steps, runs without it
([docs/sandbox-check.md](sandbox-check.md#after-setup-nothing-has-root)),
which is one more reason the machine must not be one that runs another
job afterwards. `runner` is for the other
three jobs and defaults to `ubuntu-24.04`; where no release fits the
workflow's commit, the policy job builds the binary there with rustup,
and apt for the musl compiler. Labels resolve
in the calling repository.

**Permissions.** The call needs `contents: read` and `id-token: write`
(the agent job asks for the identity token, for the proxy and the
tailnet, whether or not the run uses either). The policy, agent and check
jobs take no more than that. The apply job names no permissions and so
keeps whatever the call was granted: with `apply-environment` grant
nothing more, and without it add `contents: write`, `issues: write`
(comments and issues) and `pull-requests: write` for the job's own token.

**Limits that fit the agent.** The workflow's defaults are for a real
agent behind a proxy. A cap on model requests that nothing counts is
refused by `run`, after the machine is set up: a run without an
inference proxy, the scripted agent's for one, passes `max-requests:
"0"` and keeps `budget`.

**An inference proxy**, for a real agent: `inference-url`, as
`http://ADDRESS:PORT`, and how the run announces itself there
(`inference-register`). The workflow passes `github-oidc` unless told
otherwise: the job proves to the proxy that it is a CI job with its
identity token, for the audience `inference-audience`, which the agent's
user cannot obtain. `plain` sends the run's name and no proof, and
`token-file` reads a token from `inference-token-file` on the runner;
both have to be chosen by name. The proxy speaks
praxis-credential-broker's run API. A proxy on a tailnet is reached by
joining it (`tailscale-oauth-client-id`, `tailscale-audience`,
`tailscale-tags`). When the inference URL is an HTTP(S) IPv4 literal in
100.64.0.0/10, `agentic-job config` defaults `[egress] direct` to that URL
so that the run token never crosses the egress proxy. An explicit `direct`
list in the configuration file (even an empty one), or supplied with
`--list egress.direct=...`, takes precedence; other addresses are not
added automatically. The agent job refuses an address on the tailnet
that is not listed there before it sets the sandbox up, since the agent
could not reach it.

What the operator of a praxis-credential-broker has to turn on for a
run of this workflow to be admitted is written up, with what each
setting gives up, in
[tracker#405](https://github.com/cgwalters-forge/tracker/issues/405#issuecomment-6030469435): a
`called_workflows` entry naming this workflow file, the commit the
caller pins and the calling repository, or `any_workflow` for an owner.
Both are merged there and off.

### Event-triggered callers

With `event: true` the run is decided from the event that started the
calling workflow, by `agentic-job event` in the policy job
([how it decides](events.md)). The caller adds a `[trigger]` table to
its bounds file (which events, which roles, which bots, whether forks,
which commands), keys its `concurrency` on the triggering issue or pull
request, and writes `task` as the standing instruction: the event's text
follows it in the task file, fenced. Three callers in this repository,
each a file to copy:

- [`example-command.yml`](../.github/workflows/example-command.yml):
  `/agent REQUEST` as a comment on an issue or pull request. Every
  comment starts the workflow; its `if` skips the ones that do not start
  with the command, and the policy job refuses the rest that may not
  start a run, in seconds, before the agent's machine is started.
- [`example-pull-request.yml`](../.github/workflows/example-pull-request.yml):
  a pull request labeled `agent-review`, on `pull_request_target` so that
  the bounds and the configuration are read from the base branch. The
  command label is removed after admission, before the agent starts, so
  applying it again can request another run. The run starts from the
  pull request's base and is told its head.
- [`example-schedule.yml`](../.github/workflows/example-schedule.yml): a
  weekly run with no item, so `comment-target` names where a comment
  goes.

What changes with `event: true`. `base` gives way to the pull request's
base branch where the event names one, and the bounds have to cover it.
`comment-target` defaults to the triggering item. The `notify` and
`conclude` jobs put an eyes reaction on what started the run and a
status comment on the item, edited when the run ends (`notify: reaction`
for reactions only, `none` for neither); they write with the call's own
token, so the call grants `issues: write` or `pull-requests: write` for
them; naming no permissions, they hold the whole of what the call was
granted, as the apply job does, and the agent's machine never holds a
token that can comment. A
refused event ends the run in the policy job: a success with the other
jobs skipped, the reason in the job summary, and the workflow's outputs
`trigger` and `trigger-reason` saying what was decided. A pull request
from a fork is refused unless the bounds say `forks = true`, for a
comment on one too: the policy job fetches the pull request to apply the
rule. `workflow_dispatch` with `event: true` admits only a dispatcher
with a listed role, which the plain `example.yml` does not check.

CI's `e2e-event` job calls the workflow with `event: true` on every pull
request of this repository, where the event is admitted and the
scripted agent's comment lands on the pull request under the status
comment, and on every push to main, where `push` is not among the
bounds' events and the run ends in the policy job.

### The apply job and its token

With no `apply-environment`, the apply job uses the job's own token,
which can write only to the calling repository: `repo` then has to be
the calling repository, and `output-repo` is left out. Many organizations do
not let Actions open pull requests. The handlers then push the branch
and the forge refuses the pull request. By default that fails the job,
since a run that asked for a pull request and has none did not do what
it was for. `refused-pull-request: branch` takes the pushed branch for
the result and says so in a warning, as long as the pull request is the
only output that failed. Either way no issue is opened in the pull
request's place: gh-aw's handler would leave one whatever its
`fallback_as_issue` says, so the apply job hands it that refusal as a
pull request that failed, and lets no handler open an issue for a run
whose outputs have none. A comment that comes after the pull request
in the same hand-back then begins with gh-aw's note that the pull
request failed.

With `apply-environment: NAME`, the apply job enters that environment of
the calling repository and uses its secret `AGENTIC_JOB_APPLY_TOKEN`:
a token that can push to `output-repo` and open pull requests and
comments there. The workflow declares that one secret; the caller passes
nothing, and GitHub's documentation says a called job that names an
environment gets the environment's secret of the declared name. That is
untried here. If the environment has no such secret the job stops; it
does not fall back to its own token. To set it up:

1. Create the environment in the calling repository, limited to its
   default branch, and protect that branch.
2. Store the token as the environment's secret `AGENTIC_JOB_APPLY_TOKEN`.
3. Call with `apply-environment: NAME` and, for a fork, `output-repo`.

Three things about that environment are easy to get wrong. Its branch
rule is evaluated against the ref the caller's run is on, not against
the commit named after `uses:`, which is one more reason to pin by
commit. Whoever may start the caller's workflow on that branch can have
the token used, through checked outputs only. And everything the token
can do is within reach of that one job, so give it no more than pushing
to the output repository and opening pull requests and comments there.

When `output-repo` is not the target, it is taken to be a fork: the job
fast-forwards the fork's base branch to the target's and pushes it
before the handlers run. A fork whose base branch has commits of its own
stops the job there.

### A run stopped at a limit

A run that reaches its timeout or a request, task or spending limit is
interrupted to hand back what it has, and `run` ends with exit state 124
or 3. By default the agent job then fails and nothing is applied. With
`apply-partial: true`, a stopped run that handed back is checked like
any other and applied as a draft whose title starts with `[partial:
stopped at the timeout]` or `[partial: stopped at a limit]` and whose
first line says the same; a comment it posts starts with that line
too (gh-aw puts a note of its own before it when a pull request was
refused just before).
The agent job passes with a warning, and `summary.json` still says
`stopped_early`. Those words are the workflow's own, chosen by the exit
state, not text from the agent's machine. The exit state does not say
which of the request, task and spending limits it was, so the marking
does not either. `summary.json` does, and so does the commit's message
where the agent gave no reason of its own for stopping. A stopped run that handed
back neither a pull request nor a comment has nothing to carry them,
and the apply job fails rather than apply it unmarked.

## What a run leaves

Jobs of a called workflow are named `CALLER JOB / JOB`: for the example,
`run / policy`, `run / agent`, `run / check` and `run / apply`. The
condensed transcript is the group `agent (condensed)` of the step `Run
the agent` in `run / agent`, and `summary.md` is that job's summary.

Artifacts, each name after `artifact-prefix`: `agent-run` (`summary.json`,
`summary.md`, `condensed.log`, `outcome.json`; 90 days),
`agent-transcript` (30 days) and `safe-outputs` (30 days) in the old
tree's names and schemas; `agentic-job-policy` (`policy.json`),
`checked-outputs` (what the apply job applied, with `check`'s report)
and `applied` (`applied.json`, 90 days); and `agentic-job-binary` for a
day. `applied.json` names what the apply job made:

```json
{"schema": "agentic-job-applied/v1", "repo": "OWNER/NAME", "partial": false,
 "made": [{"type": "create_pull_request", "number": 12, "url": "..."}],
 "refused": false, "pull_request": {"number": 12, "url": "..."},
 "handlers": {}}
```

`made` is gh-aw's own list. `pull_request` is null when none was opened,
and `refused` then says whether the forge refused it to Actions after
the branch was pushed; nothing in `made` stands for it then.
`handlers` is the configuration the handlers ran with.

The workflow's outputs are `exit` (the exit state of `run`),
`pull-request` and `applied-artifact-id`.

### Auditing downloaded artifacts

Download `agent-run`, `safe-outputs`, `checked-outputs` and, when available,
`check-diagnostics` into directories
of those names under one directory, then run `agentic-job audit DIR`.
For a local `run --out DIR`, the summary is read from `DIR/run` instead.
An artifact downloaded flat is also accepted. With a custom artifact
prefix, name the local directories as above.

The audit also accepts a separate, diagnostics-only `check-diagnostics`
artifact, produced by `node safe-outputs/diagnostics.mjs SOURCE DESTINATION`.
It contains
bounded collector errors (`agent_output.json`) and, if the policy check ran,
its verdict and errors (`report.json`). Requests and patches are omitted.
The successful `checked-outputs` artifact takes precedence when both exist;
diagnostics are never input to apply. Publication requires workflow steps
that run after either refusal gate fails; without that wiring, refusal
evidence may be missing even after all available artifacts have been
downloaded.

The audit reports tool call counts and errors, proxy and permission
denials, raw handed-back requests, collector refusals, the policy check's
verdict and estimated cost. `--json` emits `agentic-job-audit/v1`, with
stable finding codes: `proxy_denied`, `permission_denied`, `run_failure`,
`patch_dropped`, `collector_refused`, `check_refused`, `cost_unknown`,
`collector_missing`, `check_missing` and `handback_missing`. Missing
artifacts are not evidence that checks passed. A readable audit exits
successfully even when it reports refusals; malformed input is an error.

This is an offline inspection, not another check or an attestation: it
executes nothing from the artifacts and does not apply a patch. Tool
counts and denials come from `summary.json`; it does not replay the
transcript. Cost is agent-reported and unverified, even when token counts
come from the inference proxy. Unknown cost is not zero. `summary.md`,
which the workflow appends to the step summary, includes the cost line.
OTLP export is deferred: no collector endpoint or telemetry credentials
are configured, and audit does not send artifact data anywhere.

## What the apply job does before it writes

`check` reads a patch's text on a machine without the repository. Three
things only the job that applies it can settle
([#17](https://github.com/cgwalters-forge/agentic-job/issues/17)):

- **The base.** The commit a patch says it starts from is the
  hand-back's own claim. The job fetches the target's base branch
  itself and requires that commit to be one of its ancestors. The
  checkout is whole, since at depth one gh-aw's handler would apply the
  patch on the branch's tip instead.
- **The files.** The job first applies the patch on that commit with a
  plain `git am`, which takes a patch only where it applies exactly, and
  compares the files that changed with the ones `check` saw the patch
  name. Only then do the handlers apply and push it.
- **Renames.** `merge.renames` is off in the checkout, so that the
  handler's `git am --3way` stops with a conflict where it would
  otherwise look for a file by content and change another path than the
  patch names.

The branch is the fourth thing. Its name is in the hand-back, and the
handlers push the name they are given, so the job requires it to be
`agent-run-RUNID` of this very run before the handlers push anything;
a branch that exists is never overwritten. So a job that is run again
in the same run, after its first attempt pushed, stops at the branch
that is already there.

Both the trial and the handler keep a carriage return at the end of a
line (`am.keepcr`), for patches to files with CRLF line ends. No such
patch has been applied by this job yet.

The patch's `From:` line is not proof of anything: `check` holds it to
nothing ([#22](https://github.com/cgwalters-forge/agentic-job/issues/22)),
and a machine that can upload a hand-back can write any author there.
Who opened the pull request is the token's owner.

The exit state that decides between a whole and a partial run comes from
the agent's machine. A machine that lied about it could have a partial
run applied without its marking, or a whole one with it; it could not
get anything past `check`.

## Things a caller should know

**Callers started by a pull request.** A workflow that runs on
`pull_request` reads its bounds file, its configuration and its setup
script from the pull request's merge commit, so whoever opens the pull
request writes them. Take the task and those files from a trusted ref
instead (`workflow_dispatch`, or `pull_request_target`, which this
workflow reads like `pull_request` and which checks nothing out from the
head), as for any workflow that holds a token; `example-pull-request.yml`
does. With `event: true` the bounds also refuse a pull request from a
fork unless they say otherwise, and a pull request whose actor (whoever
opened, pushed to or labeled it) lacks a listed role.

**What the pinned actions bring.** The workflow names every action by
commit: `actions/checkout`, `actions/upload-artifact`,
`actions/download-artifact`, `actions/github-script`, gh-aw's
`actions/setup` and `tailscale/github-action`. The first five run only
what is in the pinned commit. The Tailscale action downloads the
Tailscale release its pinned commit names as the default version, from
Tailscale's package server, and checks it against a checksum it fetches
from the same place. The build fetches crates as `Cargo.lock` pins them,
and `sandbox setup` installs the caller's packages with the image's
package manager.

**The egress proxy's pins.** `sandbox setup` runs as root and takes two
things from the network before the rules load, each pinned in this
repository. mitmproxy and all it depends on are installed with `pip
--require-hashes --only-binary :all:` from `egress/requirements.txt`,
which names each at one version with the hashes of its files: pip
installs no file that is not one of those, and builds nothing. That
file is generated from `egress/requirements.in`, whose first lines give
the command. The threat feed is fetched at one commit of its repository
and must have the SHA-256 written beside it (`DENYLIST_COMMIT` and
`DENYLIST_SHA256` in `crates/agentic-job/src/sandbox/egress.rs`): a
file that is not that one stops the setup, and a fetch that fails
leaves the proxy without a feed, with a warning, as before. To move the
feed, take the newest commit of the file and the checksum of the file
there, and put the two in a pull request:

```sh
commit=$(gh api 'repos/hagezi/dns-blocklists/commits?path=wildcard/tif.medium-onlydomains.txt&per_page=1' --jq '.[0].sha')
curl -fsSL "https://raw.githubusercontent.com/hagezi/dns-blocklists/$commit/wildcard/tif.medium-onlydomains.txt" | sha256sum
```

The feed changes more than once a day upstream, so a run blocks what
was known when the pin was last moved, and nothing moves it by itself
yet. Should the pinned commit stop being served, every run would go
without a feed and only warn: CI's `sandbox` jobs require the feed to
load, so the next pull request here would show it.

The fetch retries transient curl failures up to three times, with a
60-second retry window and a 60-second timeout per attempt. The retry
window is not a total wall-clock cap: the last attempt can finish after
it. A successful fetch still has to pass the pinned checksum check.

The retry and requirements-file regression tests address only part of
[#108](https://github.com/cgwalters-forge/agentic-job/issues/108). That
issue remains the tracking item for a faster world-write walk with an
equivalent protection argument, automated feed-pin maintenance, and a
CI embedded-source scan that handles `concat!` and multiline macros.
The host-wide walk is unchanged; no narrower protection is claimed here.
The issue also tracks source-attested releases instead of manual release
pins; the current release-pin workflow is described below.

**Steps that run after the agent.** In the agent job, as the runner's
user: the second check that the repositories are public, the uploads,
the step that turns the run's exit state into the job's result, and the
post-steps of `actions/checkout` (it removes the credential
settings of a checkout that kept none) and of the Tailscale action (it
logs the machine out). None of them reads the sandbox user's files; the
uploads take only what `run` put under its own directory after the gate.

**Where the binary comes from.**
[`secure-host/release.json`](../secure-host/release.json) names a
release of this repository, the SHA-256 of the binaries it published,
and a digest of what that release was built from (the content, modes
and names of `Cargo.lock`, `Cargo.toml`, `crates` and `egress`). The
policy job computes the same for the workflow's own commit, with the
script the action that secures a host uses
([`secure-host/binary.mjs`](../secure-host/binary.mjs)). When the two
agree it fetches the release's binaries and checks them against the
checksums, which takes a few seconds; a file that does not match stops
the run. When they do not agree, which is any commit that changed the
source since the release, it builds the binary from that source, which
takes about two minutes on a hosted runner. Either way a caller that
pins the workflow's commit pins the binary: the checksums are in the
file at that commit. What ties a release's bytes to its source is this
repository's `release` workflow and nothing a caller can check for
itself, so a caller that would sooner build than trust that passes
`build-binary: true`.

A build cache was not used, though the plan had one. The reason this
page gave before was wrong: the agent is the sandbox user and has no
token to write a cache with. The reason that holds is that a cache is
the calling repository's, where any workflow on the default branch or
on the run's own ref can leave an entry under the key of a commit
nobody has built yet, and the
first run at that commit would take it for its binary, the `check` of
that run included. A cache is also one per calling repository, where a
release is fetched by every caller.

The pin refreshes itself, but for one click. When a push to main
changes what the binaries are built from, the `pin` workflow builds
them, publishes them as a release named for the source and the run
(`build-DIGEST-RUN`), and pushes a branch `pin/...` whose one commit
puts that release in `secure-host/release.json`. Someone opens that
branch's pull request, since this organization does not let Actions
open one and CI would not start for it if it did; the workflow's
summary has the link. CI's `release-pin` job checks that the tag is of
the source the pin names and that the published binaries have the
pinned checksums, which is all a pin by hand was ever held to. Until
it merges, jobs build their binary.

A release with a version is still made by hand: raise the workspace's
version, merge, and run the `release` workflow on main with that tag.
Its build job's summary prints the `release.json` to pin it with.

**Inputs a caller forwards.** The bounds file holds `repo`, `base`,
`kind`, `outputs` and `max-outputs`, and nothing holds any other input.
A caller that passes one on from whoever dispatches it gives that
person the choice: of the bounds file itself (`allow`), of the limits,
of the machine that holds the apply token (`runner`), of the repository
that token is aimed at (`output-repo`). Forward the task, the target
and the request, and write the rest in the caller's file.

**Issues a run opens.** Issue-label bounds tighten the existing
`create_issue` output; they do not add new output types or complete
[#113](https://github.com/cgwalters-forge/agentic-job/issues/113).
Additional output types, target-specific bounds, computed apply
permissions and fake-agent CI coverage remain separate work for that
issue. Where the bounds list `create_issue`, the issue's
labels can be bounded with `allowed` and `blocked` lists:

```toml
[outputs.create_issue]
max = 1
allowed = ["agent-triage", "documentation"]
blocked = ["release"]
```

`check` refuses the entire hand-back if any label is outside `allowed`
or appears in `blocked`; blocked wins if a label appears in both lists.
Label matching ignores ASCII case. An empty `allowed` permits no labels;
omitting it preserves the existing unrestricted behavior except for
`blocked`. These limits are currently supported only for `create_issue`.
Assignees remain the agent's, sanitized by gh-aw's collector, as gh-aw
allows by default; a bounds rule for them is not written yet
([#113](https://github.com/cgwalters-forge/agentic-job/issues/113)). A
request that names other issues (`parent`, `blocked_by`) is refused by
`check`, since nothing bounds which; gh-aw's handler still makes the new
issue a sub-issue of the issue an event-triggered run was started from,
when it was. The apply job lets no handler open more issues than that
count, none for a run without the type.

**Comments gh-aw adds.** When the caller's run was started by an issue
or a pull request, gh-aw's handlers also comment there, in the calling
repository, to say what they made.

**One run at a time.** The workflow sets no `concurrency`: the caller
does, as `example.yml` does for each `id` and the event-triggered
examples do for each issue or pull request, on the job that calls the
workflow rather than on the workflow: a workflow-level group is joined
before a job's `if` is evaluated, so a comment that is no command would
cancel a command waiting its turn. GitHub keeps one waiting run per
group either way. `event.json` carries the
same key (`issue-N`, `pull-N`, `schedule`, `dispatch`) for whoever
reads a run, but a group is decided before any job runs, so it is the
caller's expression that sets it.
