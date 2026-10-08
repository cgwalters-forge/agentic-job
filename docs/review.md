# Pull request review caller

[`review.yml`](../.github/workflows/review.yml) starts on same-repository pull
requests (opened, synchronize, ready for review) and `/review` comments by
an actor with exactly the admin, maintain or write role. It reuses the
[event admission](events.md) path, including permission lookup and fork
refusal. Comments on issues and other bases are refused. It does not approve,
merge or write code: its verdict is a comment, not a GitHub review approval.

The caller uses `pull_request_target`, not `pull_request`, so the workflow,
bounds, configuration, setup and standing task are from the trusted base.
For comments, these are from the configured base (`main`); review admission
requires the pull request to target that same base. The standing task lives in
[`workflow/review.md`](../workflow/review.md), points at base-branch AGENTS.md
and treats the head's instructions, pull request text and diff as hostile data.
The reusable workflow reads a regular task file, at most 64 KiB, without
symlinks, before fencing the event text with the existing event command.

The review is `kind: analysis`, with one output allowed: `add_comment` or
`noop`. Notifications are off, so there is no second status comment. The
sandbox clones the base history and fetches the exact admitted head SHA before
starting the agent; branch names from the author are not used. The clean check
job uses code from the reusable workflow's pinned commit to require a first
line `VERDICT: APPROVE`, `VERDICT: CHANGES` or `VERDICT: REJECT`, then `REASON:`
with at most 200 Unicode characters in that entire line. It rejects all
agent-supplied addressing/editing fields and extra
outputs. Findings may follow. The apply job does not check out the caller's
repository at all for reviews: it posts checked text with pinned gh-aw code,
and never runs code from the pull request or the agent.

## Copying and enabling it

Copy the caller, `workflow/review.toml`, `workflow/review.md`, the hosted
configuration and `.github/agentic-job/e2e/review.sh` to the same paths. Change
`repos` in the bounds to your repository and, if necessary, change the caller's
`base` and the bounds' `bases` together. Pin the reusable workflow's `uses` to
an agentic-job commit you have reviewed, as in [workflow.md](workflow.md).
The permission grant is contents read for policy and cloning, id-token write
for broker registration on the secured agent machine, and pull-requests write
for the clean apply job. Policy and check explicitly restrict themselves to
contents read; the agent has contents read and id-token write, never a comment
token. No `contents: write`, inherited secret or apply environment is needed.
All actions retain their existing full commit pins.

**Nothing in this change enables real inference.** The default agent is
scripted (`fake`), on the disposable `ubuntu-26.04` agent runner; its verdict
explicitly says it is not a real code review. Real agents are currently refused
by both workflow admission and the runner. Before supporting a real agent,
enforce instruction-loading isolation and test it adversarially against the
pinned runtime. Enabling inference would then require the operator to remove
the scripted setup, configure `inference-url` with `github-oidc` registration,
and set a nonzero request cap. The inference proxy's disabled
`called_workflows` admission must be enabled for the reviewed
`cgwalters-forge/agentic-job/.github/workflows/agentic-job.yml` commit
(`job_workflow_sha`) and the calling repository IDs. Do not substitute a broad
`any_workflow` or unproven policy. The operator must also select a disposable
agent runner with systemd 257 and connectivity to that proxy (the existing
private-network deployment requires a reachable runner or configured tailnet);
hosted Ubuntu alone does not make a private proxy reachable.

## Coverage and limits

CI runs the admission, task-source, request-bound and hostile-output tests in
`node --test workflow/review.test.cjs`, and the scripted sandbox review test
in `cargo test --locked --test run` on the session-sandbox runner. That test
checks that the admitted SHA, not the current branch tip, is reviewed, and that
no patch is handed back. The `e2e-review` CI call exercises the existing policy,
agent, collector, check and comment handler, with a single scripted verdict
checked, including the admitted head SHA, by `e2e-verify`. The first pull
request adding these files skips that hosted call because the standing task
and bounds are not on its base yet; it
never falls back to untrusted head files. Once installed on the base, every
same-repository CI pull request runs it, and pushes test event refusal.
Hosted comment posting still needs GitHub Actions; it cannot be exercised on
an unprivileged runner without a forge token.

Review is read-only in authority and outputs, not a read-only filesystem:
tests may write scratch files in the sandbox, but no code change can be
published. Admission still requires a write-role actor for automatic events;
external contributors without that role need a maintainer's `/review` comment
on a same-repository pull request. Forks never start an agent. A queued review
reports its admitted SHA, which may no longer be the newest head; it is not
merge authorization. There is no real-agent quality assertion in scripted CI.
Claude and opencode are refused in review mode until automatic project
instruction loading is demonstrably isolated. Launching either in the head
could elevate a pull request's AGENTS.md or CLAUDE.md into instructions.
Opencode's project-configuration switches do not establish instruction-loading
isolation; scripted tests do not prove real-runtime behavior.
