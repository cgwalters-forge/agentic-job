# Requirements and threat model

What agentic-job guarantees, to whom, against whom, and what proves each
guarantee. It began as the requirements document of
[workflow-compiler](https://github.com/cgwalters-forge/workflow-compiler/blob/448c60c769577b345db2ad0028997f743daa5973/docs/requirements.md),
which compiled a whole workflow so that no step a caller wrote could
leave the sandbox. agentic-job has no compiler: it is one binary and a
reusable workflow that holds every job of a run
([the plan](https://github.com/cgwalters-forge/agentic-job/blob/6be67bf645ffefc81197e308e5358616be01c1ef/docs/plan.md)).
The requirements keep their numbers, R1 to R16, so that older issues and
commits still point at the right one. Each is restated for this design.

Each requirement has a **Status**, one or more of:

- *by construction*: the shape of the binary and the reusable workflow
  gives it, and nothing a caller of that workflow passes can undo it;
- *step N proves*: that step of the plan implements it and adds the test
  named under **Proof**. Until the step lands, the requirement is
  intended, not met;
- *compiler only*: workflow-compiler enforced it by generating the
  caller's whole workflow. Here it is a convention, and the text says
  whose;
- *outside*: it belongs to whatever starts runs, not to this repository.

Nothing below is built yet except the command line. A pull request that
lands a step changes the statuses it settles, in the same pull request.

## The problem in one paragraph

A CI job runs every step as the runner's own user. That user holds the
job's tokens, and on most runners it has root. So anything a step runs,
an agent following instructions it found in an issue among them, can
read the tokens, and nothing an earlier step sets up stops a later one
from running as that user again. A job that runs an agent wants the
opposite: prepare the machine with root, then run the agent as a user
that can reach none of it, and let only checked data out.

## Actors

**The caller's maintainers** own the repository whose workflow calls
agentic-job. They choose the runner, the inference proxy, the bounds
file and the environment that holds the write token, and they pin this
workflow and binary. They are trusted; the guarantees are relative to
what they configured.

**Whoever starts a run** supplies the task, the repository and the
requested outputs. They are trusted to start runs, but the request is
still checked against the bounds before a machine starts (R16).

**Everyone whose text the agent reads** is untrusted: authors of issues,
comments, pull requests and review comments, of the target repository's
files (its `AGENTS.md` included), of CI logs and of web pages. None of
them starts the run, and each can put instructions in front of the
agent.

**The agent** is treated as a hostile process. It may have been steered
by anything it read, and can do whatever its user can do. It is never
given a credential for the forge, and what it produces is data for
another job to check.

**Action authors** publish the actions the reusable workflow uses. Each
is pinned by commit and runs outside the sandbox with what its job holds
(R9).

**The platform** is trusted: the CI system and its runner, the kernel,
systemd, sudo and nftables on the runner image, the Rust toolchain and
crates that build the binary, mitmproxy, and gh-aw's scripts at their
pinned commit. So is the inference proxy.

**The runner's neighbours** are not attackers but are reachable: the
cloud metadata service, peers on a private network the runner joined,
services listening on loopback. The network rules exist for them (R3).

## Trust boundaries

The **user boundary** is between the runner's user (and root) and the
sandbox user. It is the one that holds at run time against hostile code.
It is as strong as the kernel's separation of users: it covers files and
processes, and with the network rules, the network. It does not cover
local IPC by itself (R2).

**The binary**, running as root, is the reference monitor on that
boundary. It is the only root code after setup, it starts the agent,
writes what the agent is told, and reads what the agent hands back
without trusting it.

The **job boundary** is between jobs: another VM, another token. The
agent's VM holds no write credential. Outputs are checked on a fresh VM
and applied on a third, the only one that holds a write token.

The **workflow boundary** replaces workflow-compiler's review boundary.
A caller of a reusable workflow cannot add steps to its jobs, raise
their permissions, or change how the sandbox is entered. It passes
inputs and secrets and chooses the runner. A caller that instead writes
its own job around the binary is outside this boundary: what the binary
does still holds, and what the caller's other steps do is theirs.

## Prompt injection

Prompt injection needs no bug, only an agent that reads. Telling the
agent to treat text as data helps, and so does review of that text
before a run (R15), but neither is the boundary. The requirements assume
the injection succeeds: a steered agent still reaches no credential
(R3), cannot leave its user (R1, R2, R4), cannot send data out in a
write (the egress rules), and can only propose changes, which R16
bounds.

## Data flow

```text
policy   small hosted VM     the request against the caller's bounds -> policy.json
agent    the caller's VM     sandbox setup (root), then run: the agent as the
                             sandbox user; root writes run/, transcript, safe-outputs/
check    fresh VM            gh-aw's ingestion, then agentic-job check
apply    fresh VM, the only  gh-aw's handlers on the checked outputs
         write token
```

**Into the sandbox**: the task, the agent's configuration with the
proxy's address and the run token, and a clone of a public repository
that the sandbox user makes itself. **Out of it**: the ACP messages,
which root records; the safe-outputs file and the working tree's
changes, which root turns into one patch; and the exit state.

## Requirements

### R1. Nothing after setup runs outside the sandbox on the agent's behalf

Once `sandbox setup` has returned, the agent and everything it starts
run as the sandbox user, and no input of the workflow changes that.

**Status:** *by construction* for a caller of the reusable workflow
(step 7 writes it): the agent job's steps are this repository's, so
there is no place for a caller's `shell:`, `container:`, `services:`,
job `env:` or extra step. No input may run a caller's script as root;
the `[setup]` script runs as the sandbox user. *Compiler only* for a
caller with its own job around the binary: workflow-compiler refused
such keys, and nothing here inspects that caller's workflow.

**Proof:** step 7's CI run of all four jobs. Review of this repository's
workflow for the input rule.

### R2. The agent runs as another user, in a session of its own

The agent starts through `run0` as the sandbox user, in a login session
of its own, with a fixed `PATH` and none of the job's environment.
Variables named `ACTIONS_*` are refused by name. The sandbox user has no
sudo rule and is denied every polkit action.

**Status:** *step 5 proves* the user and its limits; *step 3 proves* the
session into `run0`. Local IPC crosses the boundary: world-writable and
abstract Unix sockets and loopback services
([#3](https://github.com/cgwalters-forge/agentic-job/issues/3)).

**Proof:** `sandbox check` on a hosted runner that has `run0`: every
probe passes, and each fails when its protection is removed. The
fake-agent test.

### R3. The sandbox holds no credential but its run token

The sandbox user cannot read the job's tokens, nor ask for the job's
identity token: they are not in its environment (`ACTIONS_*` is refused
by name), the runner's processes and home are unreadable to it, the
services setup lists (a container daemon among them) are stopped, and
the network rules reject the cloud metadata service and the private
network except the inference proxy. No checkout is handed in, so there
is no checkout credential to leak: the sandbox user clones a public
repository itself. The one secret it holds is the run token, which is
good only at the inference proxy, lives only in the agent's own
configuration, and with a proxy that has a run API is counted there and
ended before anything is uploaded.

How much that token bounds depends on the proxy. With `github-oidc`
only the job can get one. With `plain`, the default, the sandbox user
can reach the proxy and announce a run of its own, so the cap for each
run is a convention against an agent that tries, and what bounds
spending is the proxy's cap on all runs and the timeout. With
`token-file` there is no run API: requests are not counted and nothing
is ended.

**Status:** *step 5 proves* the environment, files, metadata service and
network; *step 6a proves* the token's handling and the clone. This
closes two gaps workflow-compiler had (the metadata service, and a scan
for credentials in a handed-in checkout, which is no longer needed).

**Proof:** the probes of `sandbox check`, repeated by `run` with the
token in place; the mock-proxy test.

### R4. The runner's user loses root before the agent runs

`sandbox setup` ends by replacing the calling user's sudo rights with
one rule, for this binary, and `ptrace` is restricted to descendants. So
a step that runs as the runner's user after the agent (an upload) holds
that job's tokens but cannot become root or read another process's
memory.

**Status:** *step 5 proves*. The old tree did not do this; the rewrite
fixes it. The remaining rule depends on sudo and sudo-rs agreeing
([#3](https://github.com/cgwalters-forge/agentic-job/issues/3)).

**Proof:** a probe that `sudo -n true` is refused as the runner's user
and that the one rule runs only this binary.

### R5. Nothing of the agent's outlives the run

When `run` returns, for any reason, every process of the sandbox user
is gone, before anything reads what it wrote. The sandbox user cannot
start processes outside the run: cron, at and lingering are denied.

**Status:** *steps 3 and 6b prove* the first sentence, the second of
them on a runner with `run0`, where there is a sandbox user. The second
sentence is what
workflow-compiler did and is not in the plan's parity table, since
the old tree does not do it; step 5 should add it with its probes. Both
assume a runner used for one job; refusing a runner that already has
the sandbox user is
[#3](https://github.com/cgwalters-forge/agentic-job/issues/3).

**Proof:** the fake-agent test, where `run` must return with no process
of the sandbox user left.

### R6. The agent's instructions come from files it cannot change

`run` reads only the root-owned copy of the configuration under
`/etc/agentic-job/`. The task, the prompt preamble and the agent's
managed settings and instructions file are written by root where the
sandbox user can read and not write. An agent configuration taken from a
repository comes from a public one at a ref the caller named.

**Status:** *step 6a proves*. Which ref the task and configuration come
from is the caller's: a run started by a pull request must not take
them from the pull request's head
([#3](https://github.com/cgwalters-forge/agentic-job/issues/3)).

**Proof:** unit tests of the generated files. Those do not show who
owns the files on a real host; a probe for that is not planned yet.

### R7. What leaves the sandbox is checked data

The agent hands back a file of requested outputs and a changed working
tree. Root builds one patch from the tree, running git as the sandbox
user with hooks off, after the agent's processes are gone. Whatever
root copies out of the sandbox user's files (the outputs file) it reads
as that user or without following symlinks, as a regular file, with a
size cap: a symlink to a file only root can read is otherwise a way to
have it uploaded. `check`, on a
fresh VM, refuses outputs outside the policy and patches with secrets,
symlinks, submodules, binaries, mode changes, odd paths, git or CI
configuration, or more size or files than allowed. Logs and transcript
are redacted, and a secret-shaped string in the outputs fails the run.
The transcript and summary are written by root from the ACP messages,
not by the agent's user, so the agent cannot rewrite what they say
about limits and permissions. One channel is unchecked and treated as
untrusted: the exit state.

**Status:** *step 4 proves* `check` and redaction; *step 6b proves* the
hand-back and the transcript. Two things are open, neither in the
plan's parity table, both for step 6b and listed in
[#3](https://github.com/cgwalters-forge/agentic-job/issues/3): the
symlink rule above, with a fake-agent case; and that text the agent
chose and `run` prints must not act as a workflow command in the job
log.

**Proof:** corpus tests that run the old tree's check and the new on the
same inputs; golden files for the patch; a canary the fake agent prints,
which must come out redacted.

### R8. Steps that hold credentials read only what was handed over

In the agent job, the steps after `run` (uploads, with the job's
runtime token) read only the output directory root wrote. `run` refuses
to let them go ahead unless the target is public and, where the proxy
has a run API, the run was ended there. The apply job is on another VM, reads only the
checked outputs, and runs nothing from the agent's VM.

**Status:** *by construction* for apply, by the job boundary (step 7);
*step 6b proves* the upload gate. *Compiler only*: workflow-compiler
refused a credentialed step whose script used a sandboxed step's
outputs. Here that is how this repository's one workflow is written and
reviewed.

**Proof:** step 7's CI run; the upload gate's tests.

### R9. Actions are pinned, and none runs in the sandbox

Every `uses:` in this repository's workflows names a full commit. No
action runs as the sandbox user: the sandbox runs the agent and what it
starts, nothing else. A caller should pin this workflow by commit, and the
workflow fetches the binary by checksum.

**Status:** *by construction* that no action runs in the sandbox.
Pinning is *compiler only*: workflow-compiler refused an unpinned
action, and here it is review of this repository's workflows, with no
check in `ci` yet. A pin does not cover what an action downloads, and
post-steps run as the runner's user after the agent
([#3](https://github.com/cgwalters-forge/agentic-job/issues/3)).

**Proof:** none automated.

### R10. What the guarantees depend on

They hold under these conditions and mean little without them.

- **Only maintainers change the workflow that runs.** Write tokens are
  limited to protected refs: the apply job's token is the secret of an
  environment limited to the default branch.
- **The runner is a VM used for one job**, with systemd 256 or later
  for `run0`, nftables, and root at the start.
- **Each job's token has only what it needs.** The reusable workflow
  declares `permissions` for every job: read for policy, agent and
  check. The agent job also has `id-token: write` when the proxy wants
  proof or the runner joins a tailnet, so the runner's user there can
  ask for the job's identity token; R3 keeps that from the sandbox
  user.
- **The privileged phase runs nothing from an untrusted checkout.** The
  agent job never checks the target out as the runner's user.
- **The target repository is public**, since logs and transcript are.

**Status:** the third and fourth are *by construction* (step 7). The
others are the caller's settings; nothing checks them, and a checklist
or command is
[#3](https://github.com/cgwalters-forge/agentic-job/issues/3).

### R11. Lints are not boundaries

workflow-compiler also refused mistakes that were not escapes: an
expression naming a secret in a sandboxed step, an `env:` name that
would change how its wrapper started.

**Status:** *compiler only*. There is no source to lint. The mistake
has less room here: the binary passes none of the job's environment to
the sandbox, so a secret in the agent job's `env:` does not reach the
agent. A caller with its own job gets no such lint.

### R12. Untrusted text never becomes privileged code

The task, repository, base and output names reach privileged steps only
as data: in `env:` and as arguments and files of the binary, never
expanded into a script. `policy` validates repository, base and output
names before any other job starts.

**Status:** *step 4 proves* the validation. The rest is *compiler
only*: workflow-compiler refused such an expansion in any workflow it
compiled. Here it is how this repository's workflow is written (step
7); a linter for expression injection in `ci` would check it and is not
in the plan.

**Proof:** corpus tests of `policy`.

### R13. Nothing the sandbox produced is run with privileges later

Artifacts from the agent's VM are data in every later job. The check
and apply jobs start from fresh VMs and execute nothing from them, and
no cache is saved from the agent's VM. A patch cannot change CI or git
configuration unless the caller's bounds exempt such a file, so applying
it does not turn the agent's text into workflow code without a person
merging a draft.

**Status:** *by construction* (step 7) for the jobs and caches; *step 4
proves* the protected files. workflow-compiler did not enforce this.

**Proof:** corpus tests of `check`; step 7's CI run.

### R14. Generated files are checked by code a pull request cannot change

**Status:** *compiler only*, and not needed: there are no generated
workflow files. What stands in its place is pinning: a caller names
this workflow by commit if it chooses to, the workflow names the binary
by checksum, and
this repository's main branch takes only pull requests with `ci` green.

### R15. Untrusted text is reviewed before an agent is given it

Before an item's untrusted text is handed to an agent, model calls with
no tools and no credentials read it and can hold it for a person.

**Status:** *outside*. It belongs to whatever decides to start a run
([tracker#226](https://github.com/cgwalters-forge/tracker/issues/226)).
agentic-job's part is to be safe when that review is absent or wrong.

### R16. The agent's writes are capped, checked outputs applied elsewhere

The agent's job holds no write credential for the forge. Everything the
agent wants changed is a requested output of an allowed type. `policy`
fixes, before the agent's machine starts, which types and how many: the
lower of the caller's bound and the request; pull requests are drafts;
an analysis run cannot open one. `check` enforces that on a fresh VM,
and the apply job, alone with the write token, applies what passed and
records it.

**Status:** *step 4 proves* the bounds and the check; *steps 2a and 2b
prove* the apply; step 7 joins them. Two things the original asked for
are not met. The apply token is the bot account's own for now, not a
token minted for the run
([tracker#403](https://github.com/cgwalters-forge/tracker/issues/403)),
so that one job can reach everything the account can. And there are no
rate limits across runs and no kill switch
([tracker#228](https://github.com/cgwalters-forge/tracker/issues/228));
the nearest is one run at a time for each name the caller gives.

**Proof:** corpus tests from the old tree's safe-outputs tests; the
draft pull requests of steps 2a, 2b and 7.

## Summary

| | Requirement | Here |
| --- | --- | --- |
| R1 | Nothing leaves the sandbox | by construction, once step 7 lands; compiler only for a caller's own job |
| R2 | Another user, own session | steps 3, 5 |
| R3 | No credential but the run token | steps 5, 6a; the token's cap depends on the proxy |
| R4 | Runner loses root | step 5 |
| R5 | Nothing outlives the run | steps 3, 6b; cron, at and lingering not planned |
| R6 | Instructions it cannot change | step 6a |
| R7 | Checked data out | steps 4, 6b; two rules not planned |
| R8 | Credentialed steps read handed-over data | by construction, once step 7 lands; step 6b |
| R9 | Actions pinned, none in the sandbox | by construction; pinning by review |
| R10 | Conditions | partly by construction; the caller's settings |
| R11 | Lints | compiler only |
| R12 | Untrusted text is data | step 4; by review of one workflow |
| R13 | Sandbox output never run with privileges | by construction, once step 7 lands; step 4 |
| R14 | Generated files checked from the base | compiler only; not needed |
| R15 | Intake review | outside |
| R16 | Capped, checked writes applied elsewhere | steps 2a, 2b, 4, 7; no per-run token or rate limit |

## Non-goals

**A kernel boundary.** The sandbox is another user on the same kernel:
a local privilege escalation in the kernel, systemd, polkit or sudo
breaks it. A VM for the agent is the next hardening step.

**Protecting against the caller's maintainers.** They choose the
runner, the proxy, the bounds and the token.

**Confidentiality of what the agent can read.** Reads are open to any
host not on the threat feed, so data can leave in a `GET`. Targets must
be public.

**Availability.** The agent can use the whole machine until its limits
stop it.

**Runners without `run0`.** That needs systemd 256 or later.

## Gaps

The gaps carried over from workflow-compiler's issues, each with the
step it belongs to, are in
[#3](https://github.com/cgwalters-forge/agentic-job/issues/3).
