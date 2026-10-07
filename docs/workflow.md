# Running it on GitHub

[`.github/workflows/agentic-job.yml`](../.github/workflows/agentic-job.yml)
is one agent run as a workflow another repository calls. It is a thin
wrapper: the `agentic-job` binary decides what a run may ask for, runs
it and checks what comes back, and the workflow moves files between
four jobs on four machines. What the workflow decides itself is in the
apply job, and [listed below](#what-the-apply-job-does-before-it-writes).

1. **policy**, on a small hosted machine. It checks that the target
   repository is public, builds the binary, and checks the run's request
   against the caller's bounds file (`agentic-job policy`). A request
   outside the bounds stops here, before a machine is spent on the agent.
2. **agent**, on the caller's runner. `agentic-job sandbox setup` creates
   the unprivileged user, the network rules and the egress proxy;
   `sandbox check` probes them; `run` clones the target, drives the agent
   and leaves what may be uploaded.
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
- **It has not been called from another repository.** CI calls it by
  path, where the workflow's own commit is the run's. That a caller's
  pin by commit selects the source the binary is built from follows
  from GitHub's documentation of `job.workflow_sha` and is untried.
- Not tried at all: `apply-environment`, an `output-repo` other than the
  calling repository, bringing a fork's base branch up to date, the
  tailnet login, a runner that is not GitHub's `ubuntu-26.04`,
  `agent-config-repo`, and `kind: analysis`.

## A caller

[`.github/workflows/example.yml`](../.github/workflows/example.yml) is a
complete one, for the scripted agent. Copy it with the two files it
names, and put your own repository in place of this one in the bounds
file's `repos` and in the default of the example's `repo` input. Every
input is described where it is declared, at the top of the workflow
file; this page says what a caller has to provide for them. The call
itself:

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
setup` refuses a machine it has set up before. `runner` is for the other
three jobs and defaults to `ubuntu-24.04`; the policy job builds the
binary there with rustup, and apt for the musl compiler. Labels resolve
in the calling repository.

**Permissions.** The call needs `contents: read` and `id-token: write`
(the agent job asks for the identity token, for the proxy and the
tailnet, whether or not the run uses either). The policy, agent and check
jobs take no more than that. The apply job names no permissions and so
keeps whatever the call was granted: with `apply-environment` grant
nothing more, and without it add `contents: write`, `issues: write` and
`pull-requests: write` for the job's own token.

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
`tailscale-tags`), and its address goes into `[egress] direct` of the
configuration file so that the run token never crosses the egress
proxy.

What the operator of a praxis-credential-broker has to turn on for a
run of this workflow to be admitted is written up, with what each
setting gives up, in
[tracker#405](https://github.com/cgwalters-forge/tracker/issues/405#issuecomment-6030469435): a
`called_workflows` entry naming this workflow file, the commit the
caller pins and the calling repository, or `any_workflow` for an owner.
Both are merged there and off.

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
whose outputs have none. A comment of the same run then begins with
gh-aw's note that the pull request failed, which names the branch.

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
instead (`workflow_dispatch`, or `pull_request_target` with nothing
checked out from the head), as for any workflow that holds a token.

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

The feed changes several times a day upstream, so a run blocks what was
known when the pin was last moved, and nothing moves it by itself yet.

**Steps that run after the agent.** In the agent job, as the runner's
user: the second check that the repositories are public, the uploads,
the step that turns the run's exit state into the job's result, and the
post-steps of `actions/checkout` (it removes the credential
settings of a checkout that kept none) and of the Tailscale action (it
logs the machine out). None of them reads the sandbox user's files; the
uploads take only what `run` put under its own directory after the gate.

**No build cache.** The policy job builds the binary on every run,
which takes about two minutes on a hosted runner. The plan had it
cached under the workflow's exact commit. That is not enough: a job
that can write caches, as the agent's runner user can, could leave an
entry for a commit no run has built yet, and the first run at that
commit would take it for its binary, the `check` of that run included.

**Inputs a caller forwards.** The bounds file holds `repo`, `base`,
`kind`, `outputs` and `max-outputs`, and nothing holds any other input.
A caller that passes one on from whoever dispatches it gives that
person the choice: of the bounds file itself (`allow`), of the limits,
of the machine that holds the apply token (`runner`), of the repository
that token is aimed at (`output-repo`). Forward the task, the target
and the request, and write the rest in the caller's file.

**Comments gh-aw adds.** When the caller's run was started by an issue
or a pull request, gh-aw's handlers also comment there, in the calling
repository, to say what they made.

**One run at a time.** The workflow sets no `concurrency`: the caller
does, as the example does for each `id`.
