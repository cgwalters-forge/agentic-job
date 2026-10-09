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
   unprivileged user, the network rules and the egress proxy, removes
   general root access and probes access (`agentic-job sandbox setup` and
   `sandbox check`); `run` clones the target, drives the agent and
   leaves what may be uploaded.
3. **check**, on a machine the agent never touched. gh-aw's collector
   validates and sanitizes the agent's requests, then `agentic-job check`
   holds them and the patch to the policy.
4. **apply**, on a machine of its own, uses a write credential.
   It runs gh-aw's handlers on the checked outputs:
   a branch and a draft pull request, comments.

`activate`, `notify` and `conclude` also inherit the caller's permissions
and can remove labels, post reactions and create or edit status comments.
None of these write-capable jobs executes code produced by the agent.
`policy` and `check` have `contents: read`; `agent` has `contents: read`
and `id-token: write`, not repository write permissions.

## What has run, and what has not

Read this before relying on it.

- The branch pipeline runs in this repository's CI on same-repository pull requests, twice,
  with the scripted agent (`fake`): once to the end, and once stopped at
  a limit. The egress proxy, the network rules and every probe of
  `sandbox check` are real in those runs. The apply job pushes the
  branches and posts the comment with the job's own token.
- Claude Code and opencode have run real tasks on RHEL 10 runners.
  The reusable workflow has been called from
  `cgwalters-bot/agentic-job-trial`: runs
  [37587896350](https://github.com/cgwalters-bot/agentic-job-trial/actions/runs/37587896350)
  and [37588497422](https://github.com/cgwalters-bot/agentic-job-trial/actions/runs/37588497422)
  opened pull requests [#1](https://github.com/cgwalters-bot/agentic-job-trial/pull/1)
  and [#2](https://github.com/cgwalters-bot/agentic-job-trial/pull/2) there.
  [Issue #59](https://github.com/cgwalters-forge/agentic-job/issues/59)
  records these deployments. No caller exists in the operator's runner
  repository, `bootc-dev/cgwalters-devspace-sandbox`, yet.
  A real agent through the reusable workflow
  on GitHub-hosted runners remains untried; the existing broker is private.
- This repository's CI expects Actions PR creation to be refused, with
  the branch pushed. That local setting does not prevent other callers
  from opening PRs with an appropriate token.
- The example caller has been dispatched on this repository's main
  branch: once to the end, with the scripted agent's built-in session,
  and once naming a private repository, which stopped in the policy
  job. homegit's `bot-runs list`, `show`, `log` and `reconcile` read
  the first of those runs.
- The event path (`event: true`) runs in CI on same-repository pull requests and
  push of this repository (`e2e-event`, [below](#event-triggered-callers)):
  admitted on a pull request, with the comments on the pull request;
  refused on a push. The slash-command, label and schedule examples are
  live here with the scripted agent.
- Fork pull requests do not run CI's write end-to-end jobs.
- Not tried at all: `apply-environment`, an `output-repo` other than the
  calling repository, bringing a fork's base branch up to date, the
  tailnet login,
  `agent-config-repo`, `kind: analysis`, and applying a `create_issue`:
  that type is checked through gh-aw's collector and `check` in CI
  (the `corpus` job), and its handler has not run from this workflow.

## CI runner use

CI always starts and reports the required job named `ci`; it does not use
workflow-level path exclusions. Its read-only `changes` job checks the complete
Git diff (PR merge base to head, or push before to after), without an API's
changed-file pagination limit. Only `README.md` and Markdown under `docs/`
skip the six `e2e-*` callers, `review-base` and their write-capable verifier.
Renames check both names; empty diffs, missing history and unknown events run
the suite. Other checks, including sandbox probes, still run. In particular,
`workflow/review.md` is executable reviewer input, not documentation for this
filter. Workflow, sandbox, policy, fixtures and checks changes run every caller.
The aggregate accepts only those named skips when `changes` explicitly reports
`false`; detection failures, unexpected skips and cancelled jobs remain red.

CI already cancels superseded PR runs with its workflow/ref concurrency group;
main pushes are not cancelled. The dedicated review caller now cancels an older
review when an automatic PR event arrives, using its existing per-PR job group.
Comment events cannot cancel an active review before actor authorization. A
non-command comment never enters the group. GitHub still replaces a pending
job in the same group even with cancellation off, so an unauthorized `/review`
can displace a pending review (an existing denial-of-service limit, not write
authority). Cancellation is best-effort: an
already-posted verdict still names its old SHA, and cancelling CI can leave
scratch branches/comments if its cleanup does not finish. No cancellation is
merge authorization or rollback.

For a recent successful same-repository PR run,
[37870929248](https://github.com/cgwalters-forge/agentic-job/actions/runs/37870929248),
the sum of each caller's non-skipped job durations was:

| Caller | Runner-minutes | Jobs that ran | Distinct coverage |
| --- | ---: | ---: | --- |
| `e2e-full` | 7.27 | 5 | Patch/quoted handler inputs, branch and comment application |
| `e2e-limit` | 5.63 | 5 | Task limit, partial hand-back, forced source build |
| `e2e-event` | 6.20 | 7 | Event admission, base selection, notifications and conclusion |
| `e2e-review` | 4.33 | 5 | Base-owned review task, analysis verdict and reviewed SHA |
| `e2e-verify` | 0.33 | 1 | Forge assertions and cleanup |

These are elapsed runner-minutes, excluding queue waits, not billing-rounded
minutes or six minutes multiplied by every job. On main run
[37871861845](https://github.com/cgwalters-forge/agentic-job/actions/runs/37871861845),
event and review each ran only policy to prove refusal (2.22 and 2.08 minutes).
`e2e-analysis` and `e2e-analysis-refused` are not present in those samples, so
their costs remain unmeasured: the former exercises fixed-destination topic
analysis, the latter proves hostile destination refusal before apply.

All callers repeat binary acquisition and sandbox setup, but they prove
different contracts. Keep policy, agent, check and apply on separate machines:
sharing the agent's machine with a checker or writer would erase the boundary
the suite tests. Serial calls still create separate reusable-workflow jobs and
do not save runner-minutes. Combining the analysis pair or full/limit into a
new multi-scenario workflow would add machinery and reduce isolation; no callers
are combined here. The docs-only gate saves the whole caller group instead.

Landing several reviewed commits on one branch with one CI run is an operator
workflow outside this repository. This public organization repository is
eligible for GitHub's merge queue; an administrator must confirm/enable it in
the branch rules. It is **not** single-build batching for free:
[GitHub's queue documentation](https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/configuring-pull-request-merges/managing-a-merge-queue)
explicitly says merge limits do not combine `merge_group` builds. The queue can
avoid repeated manual rebases and limit build concurrency, but still tests
cumulative groups. Enabling it also requires CI's required check on
`merge_group` and updating the event-refusal assertions; neither is enabled
here. Prefer operator batching for the immediate capacity problem.

The only new checkout uses the existing full action pin, no persisted
credential, full history and the workflow's existing contents-read permission.
The new path/test steps execute repository code only in read-only jobs. All
other CI workflow changes are caller dependencies/conditions and narrowly
allowlisted aggregate skips; verifier permissions and cleanup steps are
unchanged. Review changes only its cancellation condition. No action pins,
token placement, sandbox controls or output guards change.

## Proposals from a non-agent job

A caller job can upload an artifact containing `outputs.jsonl` in the
[safe-output format](safe-outputs.md), then call this workflow with
`proposals-artifact` naming it. The producer needs no write credential.
Policy still reads the caller's bounds; check still collects and validates
the requests on a separate read-only machine; only apply receives the write
token. No agent, sandbox setup or inference runs. Event admission, review and
partial application are not available in this mode. Use a trusted caller ref
for the bounds, just as for agent runs.

```yaml
jobs:
  proposals:
    runs-on: ubuntu-24.04
    permissions: {}
    steps:
      - run: mkdir proposals && printf '%s\n' '{"type":"noop","message":"No proposals today."}' > proposals/outputs.jsonl
      - uses: actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a # v7.0.1
        with:
          name: board-proposals
          path: proposals/outputs.jsonl
          if-no-files-found: error
  apply:
    needs: proposals # uploads outputs.jsonl as board-proposals
    uses: cgwalters-forge/agentic-job/.github/workflows/agentic-job.yml@COMMIT
    permissions:
      contents: read
      issues: write # apply may post the allowed comments
      id-token: write # GitHub validates the skipped agent job's permission too
    with:
      id: board
      repo: OWNER/REPO
      allow: .github/agentic-job/allow.toml
      outputs: add_comment,noop
      max-outputs: '2'
      proposals-artifact: board-proposals
```

Copy the [shipped bounds file](../.github/agentic-job/allow.toml) to the path
above, set its `repos` and the call's `repo` to your public repository, and
replace `COMMIT` with a reviewed commit. Put these jobs in a workflow with
your chosen trigger. The producer above sends a noop; replace it with your
job's requests. For comments, set `comment-target` to an existing issue number.
`add_comment,noop` is within the shipped bounds; `create_issue` requires your
own explicit bounds entry. Set `max-outputs` no higher than your bounds file's
`max_outputs` (the workflow default is 3).

Keep the permissions above, or configure `apply-environment` as below and
omit `issues: write`. Even proposals-only callers need `id-token: write`:
GitHub validates the reusable workflow's permission requests before deciding
which jobs to skip. No OIDC token is requested in proposals mode.
CI's `e2e-proposals` runs this noop producer and call without a sandbox or model.
The artifact name selects untrusted data only; it does not select the policy,
binary or handler code. Upload just the hand-back files, not a checkout.
For patch proposals the existing branch, base and patch guards still apply.
`exit` is empty because no agent ran. Issue closure and label additions use the
[bounded issue actions](safe-outputs.md#closing-issues-and-adding-labels).
Project updates use [named project and field bounds](safe-outputs.md#updating-project-fields)
and the apply environment token described below, not the job token.

## A caller

[`.github/workflows/example.yml`](../.github/workflows/example.yml) is a
complete one, for the scripted agent, started by hand. Copy it and
`.github/agentic-job/allow.toml` and `.github/agentic-job/hosted.toml` to
the same paths. Replace `uses: ./.github/workflows/agentic-job.yml` with
`uses: cgwalters-forge/agentic-job/.github/workflows/agentic-job.yml@COMMIT`,
using a reviewed commit. Replace the example's `repo` default and the
bounds file's `repos` with your public repository; the allowed base is
`main`. Keep the permissions. Commit to your default branch, enable
Actions and permit the referenced actions/workflow and branch writes.
The example uses the automatic job token, not a configured secret;
it returns a draft PR when Actions PR creation is enabled, otherwise a
pushed branch. Three more are started by events, [below](#event-triggered-callers):
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

### Real agent checklist

Before replacing `agent: fake`, ask the inference and network operator for
the following. Hosted Ubuntu alone cannot reach a private proxy.

- **Proxy URL and audience:** set repository variables `INFERENCE_URL` and
  `INFERENCE_AUDIENCE`, and pass them as `inference-url` and
  `inference-audience`. These are addresses/identity names, not secrets.
  Keep `inference-register: github-oidc`: no repository secret or provider
  API key is needed for inference in this mode.
- **A network route:** either a disposable self-hosted `agent-runner` on
  the proxy's network, or `tailscale-oauth-client-id`, `tailscale-audience`
  and `tailscale-tags`. The tailnet administrator must create an OAuth
  client configured to trust GitHub's OIDC issuer and the requested audience,
  authorize this workflow's identity to mint ephemeral tagged nodes, and
  grant those tags access to the proxy's TCP port in the tailnet policy.
  The client id is not a client secret; do not supply an OAuth secret.
- **Broker admission:** the broker operator must enable a `called_workflows`
  policy entry for `cgwalters-forge/agentic-job/.github/workflows/agentic-job.yml`,
  the exact commit pinned by the caller (`job_workflow_sha`), and the calling
  repository's numeric id (not just its name). Update that entry when the
  workflow pin changes. See [inference admission](inference.md#which-workflows-a-broker-admits).
- **Agent programs and limits:** `agent: claude` defaults to
  `@agentclientprotocol/claude-agent-acp@0.88.0` (the ACP adapter, which brings
  the Claude Agent SDK); `agent: opencode` defaults to `opencode-ai@1.18.35`.
  Override `npm` with exact `NAME@VERSION` pins if needed, and choose
  `model`, `max-requests`, `budget` and `timeout` with the operator.
  These package pins do not pin every transitive dependency.

The policy job validates the effective configuration, including overrides
of the caller's `config`, before starting the agent machine. After joining
the tailnet, the agent job makes a TCP connection to the effective proxy URL
as the runner user, before sandbox setup. This control sends no credentials
and proves only reachability, not broker admission or model availability.
A failure names the missing network route rather than blaming sandbox rules.

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
jobs take no more than that. Activate, notify, conclude and apply name no
permissions and so
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

The dedicated [review caller](review.md) sets `review: true` and
`review-task: workflow/review.md` to take a fixed task from the base checkout,
review an admitted same-repository head SHA with an analysis-only policy, and
post one validated verdict comment or noop. It requires notifications and
partial application off, and currently supports only fake agents.

With `event: true` the run is decided from the event that started the
calling workflow, by `agentic-job event` in the policy job
([how it decides](events.md)). The caller adds a `[trigger]` table to
its bounds file (which events, which roles, which bots, whether forks,
which commands), keys its `concurrency` on the triggering issue or pull
request, and writes `task` as the standing instruction: the event's text
follows it in the task file, fenced. Three callers in this repository,
with their copy lists below, and a dedicated review caller:

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

Copy each selected caller and its companion files to the **same paths** in
your repository:

- **Slash command:** [`.github/workflows/example-command.yml`](../.github/workflows/example-command.yml),
  [`.github/agentic-job/allow.toml`](../.github/agentic-job/allow.toml),
  [`.github/agentic-job/hosted.toml`](../.github/agentic-job/hosted.toml) and
  [`.github/agentic-job/e2e/event.sh`](../.github/agentic-job/e2e/event.sh).
- **Label:** [`.github/workflows/example-pull-request.yml`](../.github/workflows/example-pull-request.yml),
  [`workflow/label-review.toml`](../workflow/label-review.toml),
  [`.github/agentic-job/hosted.toml`](../.github/agentic-job/hosted.toml) and
  [`.github/agentic-job/e2e/event.sh`](../.github/agentic-job/e2e/event.sh).
  Create the `agent-review` label before using it. For issues as well as
  pull requests, use the [label caller in events.md](events.md#label-caller-and-activation-step).
- **Schedule:** [`.github/workflows/example-schedule.yml`](../.github/workflows/example-schedule.yml),
  [`.github/agentic-job/allow.toml`](../.github/agentic-job/allow.toml) and
  [`.github/agentic-job/hosted.toml`](../.github/agentic-job/hosted.toml).
  Set `comment-target` to an existing issue in your repository. This caller
  uses the scripted agent's built-in file-changing session, not a setup script.
- **Dedicated review:** [`.github/workflows/review.yml`](../.github/workflows/review.yml),
  [`workflow/review.toml`](../workflow/review.toml),
  [`workflow/review.md`](../workflow/review.md),
  [`.github/agentic-job/hosted.toml`](../.github/agentic-job/hosted.toml) and
  [`.github/agentic-job/e2e/review.sh`](../.github/agentic-job/e2e/review.sh).
  This runs on pull request events or `/review`, not a label; see the
  [review guide](review.md).

In each caller, replace `uses: ./.github/workflows/agentic-job.yml` with
`uses: cgwalters-forge/agentic-job/.github/workflows/agentic-job.yml@COMMIT`,
using a reviewed commit. In its bounds file, set `repos` to your public
repository and `bases` to the base branches you allow. Keep the caller's
permissions and commit all its companion files to your default branch.
The setup scripts are read from the trusted caller checkout; do not take
them from an untrusted pull request head. With `fake`, they select a fixed
session for testing the wiring, not a response to the task's text.

Comment-only callers use `kind: analysis` with `outputs: add_comment,noop`,
so no patch is handed back and policy refuses pull request outputs. They
need no `contents: write`; keep the item write permissions for label
activation, status and output comments. The command and schedule callers
also allow pull requests and therefore retain `contents: write` for apply.

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

CI's `e2e-event` job calls the workflow with `event: true` on same-repository pull
request of this repository, where the event is admitted and the
scripted agent's comment lands on the pull request under the status
comment, and on every push to main, where `push` is not among the
bounds' events and the run ends in the policy job.

### GitHub reads from the agent

There is currently no opt-in authenticated GitHub read input. The
sandbox user has no `GITHUB_TOKEN` or `GH_TOKEN`; installing `gh` is
not enough to give it authenticated issue or pull-request access.
Public HTTP reads and git HTTPS fetches still use the existing egress
policy, with writes refused.

The planned integration reuses gh-aw's `gh-proxy` mode, with its token
holder outside the sandbox user. See the
[pinned-source findings](background-ghaw.md#github-cli-proxy-reuse) for
the reusable artifact and the remaining enforcement and CI checks.
Do not pass a forge token through `sandbox.env` to work around this gap.

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

For `update_project`, use that same environment secret, not an additional token:
`AGENTIC_JOB_APPLY_TOKEN` must have organization **Projects: Read and write**
for a fine-grained PAT or GitHub App, and repository **Issues: Read** (plus
Metadata read) on the issue repository. A classic PAT needs **project** scope
and repository access (`repo` for private repositories). Organization approval
or SSO authorization may also be required. Other output types need their own
repository write permissions only if allowed. The handler uses the apply step's
authenticated client; no project credential is given to policy, check or the
producer. The job token cannot substitute for organization Projects access.

The project workflow changes leave permissions, action pins and token placement
unchanged. Handler configuration fixes the issue repository to the checked run's
`repo`, not `output-repo`. The apply-only GraphQL guard prevents gh-aw's implicit
schema creation and normalized-name writes to unlisted fields. A refused project
write fails the final outcome even when the PR-refusal exception is enabled.

Three things about that environment are easy to get wrong. Its branch
rule is evaluated against the ref the caller's run is on, not against
the commit named after `uses:`, which is one more reason to pin by
commit. Whoever may start the caller's workflow on that branch can have
the token used, through checked outputs only. And everything the token
can do is within reach of that one job, so give it no more than pushing
to the output repository and opening pull requests and comments there.

For a patch, when `output-repo` is not the target, it is taken to be a fork: the job
fast-forwards the fork's base branch to the target's and pushes it
before the handlers run. A fork whose base branch has commits of its own
stops the job there.

Without a patch, apply does not check out a repository, configure git or
fetch/advance a base branch. An analysis can therefore inspect a topic branch
and comment in an unrelated tracker repository via `output-repo`, even when
that repository has no such branch. `comment-target` fixes the issue/PR number;
the read-only check job refuses an explicit repository or item that differs,
including PR-number aliases, existing-comment IDs and discussion replies.
Omitting a destination in the request uses the caller's fixed destination.
Cross-repository writes still require an apply token with access to that
repository; no permissions or action pins are broadened.

CI's misdirected-analysis caller sets `expect-check-refusal: true`. Only a
policy-check exit of 1 with a nonempty refusal report counts as success;
collector, setup and other operational failures still fail, as does unexpected
acceptance. The caller exposes `check-refusal`, which CI asserts names the
fixed-item violation. Expected-refusal runs upload no checked outputs and never
start apply, even if the check unexpectedly accepts them. This changes only
result reporting and job/step conditions: permissions, token placement, action
pins and the validation rules are unchanged.

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

A stopped run cannot clean up after its own commands, so the hand-back
of any run leaves untracked binary files (a `__pycache__`, say) out of
the patch. `outcome.json` and `summary.json` name them in
`omitted_untracked_binary_files`, and a summary the agent wrote gains a
line saying how many. A changed binary file that git tracks, or one the
agent staged, stays in the patch, and `check` refuses it.

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
line (`am.keepcr`), for patches to files with CRLF line ends.
`node --test workflow/apply.test.cjs` exercises the workflow's actual
shell steps locally, including CRLF preservation, Unicode paths, file-list
mismatches, unrelated bases and a rename into a protected path. File-list
extraction failures stop the trial; Git's path quoting is disabled for
the comparison, since `check` rejects control characters in paths.

The separately dispatched `safe-outputs-probe.yml` predates these guards
and does not yet exercise them. Its fixture-only handler path still needs
the same checker report, fetched-base ancestry check and trial comparison
before it can serve as evidence for these protections.

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
Source-attested prebuilt releases now replace manual release pins;
their trust model is described below.

**Steps that run after the agent.** In the agent job, as the runner's
user: the second check that the repositories are public, the uploads,
the step that turns the run's exit state into the job's result, and the
post-steps of `actions/checkout` (it removes the credential
settings of a checkout that kept none) and of the Tailscale action (it
logs the machine out). None of them reads the sandbox user's files; the
uploads take only what `run` put under its own directory after the gate.

**Where the binary comes from.**
[`secure-host/binary.mjs`](../secure-host/binary.mjs) hashes the content,
modes and names of `Cargo.lock`, `Cargo.toml`, `crates` and `egress` at
the caller's pinned commit. It looks for `build-FULL_DIGEST`, not the
latest release. The release contains a manifest with that source digest
and the binaries' SHA-256 checksums, plus a GitHub build-provenance bundle.
Before trusting the manifest, `gh attestation verify` checks that it was
signed by this repository's `pin.yml` workflow on `refs/heads/main`, on a
GitHub-hosted runner. Then the script checks the manifest's source,
repository, target and release name, and checks each binary's checksum.
Replacing release assets, signing from a PR or using another workflow
cannot substitute a binary. The trust anchor is GitHub's attestation
service and this repository's main-branch publishing workflow, not a
mutable release tag or the caller repository's cache.

Only a missing manifest (HTTP 404) falls back to building, with a notice
that it takes about two minutes. A download or verification failure stops
the job rather than silently building. A caller can always request a
source build with `build-binary: true`. Fetching requires Node and `gh`
with attestation support; GitHub-hosted runners provide them. The bundle
is downloaded with the release, so verification needs no forge token.

A build cache was not used, though the plan had one. The reason this
page gave before was wrong: the agent is the sandbox user and has no
token to write a cache with. The reason that holds is that a cache is
the calling repository's, where any workflow on the default branch or
on the run's own ref can leave an entry under the key of a commit
nobody has built yet, and the
first run at that commit would take it for its binary, the `check` of
that run included. A cache is also one per calling repository, where a
release is fetched by every caller.

When main's source changes, the `prebuilt` workflow (`pin.yml`) makes an
uncached build and publishes its source-addressed release. No branch,
pull request or human merge is needed. Commits that only change docs or
workflow usage share their source's release. A run started before the
build finishes still builds locally; subsequent runs fetch the prebuilt.
The build job has only contents-read access. The publishing job executes
no built binaries; it has contents-write to publish, OIDC and
attestations-write to sign the manifest, and no pull-requests permission.
Uploads are staged in a draft release and published only when complete;
rerunning repairs an interrupted draft. Repeated builds of the same source
leave the first published release intact.

A release with a version is still made by hand: raise the workspace's
version, merge, and run the `release` workflow on main with that tag.
Versioned releases remain separate from automatic prebuilt selection.

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
`blocked`. `add_labels` also uses these limits, but requires `allowed` explicitly.
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
