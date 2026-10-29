# Changelog

User-visible changes to the reusable workflows, the actions and the
`agentic-job` binary, newest first. Each version's
[release notes](https://github.com/cgwalters-forge/agentic-job/releases)
list every pull request merged into it; `build-DIGEST` releases are CI
artifacts, not versions. [Stable interfaces](docs/stable-interfaces.md)
says what a caller may rely on across versions.

## Unreleased

- `review.yml`, with the repository variable `REVIEW_CARRY_APPROVAL=true`,
  carries the previous head's approval to a push that is only that head
  rebased onto the base branch or rustfmt's output of it, instead of
  running the agent; the status `review`, and the submitted review where
  real reviews are on, say whose approval and why. New:
  `agentic-job review carry`; `review submit --accepted` is optional for
  a carried approval (docs/review.md#carried-approvals).
- `agentic-job apply --forge forgejo` also publishes `add_labels`,
  `remove_labels`, `close_issue`, `close_pull_request` and `create_issue`,
  and writes nothing for `noop`, `missing_tool` and `missing_data` instead
  of refusing them. Before any write it reads each target and refuses
  another kind of item or one not of the repository, and a label the
  repository lacks, which it never creates, or a label change Forgejo
  would carry out on other labels; it refuses fields Forgejo has
  no meaning for, such as `temporary_id` or `state_reason: not_planned`
  (docs/safe-outputs.md#labels-issues-and-closing-on-forgejo).
- Status edits drop a run-supplied `agent-board-maintain-plan` marker rather
  than give it bot-last-edited provenance. Fake-forge and workflow tests pin
  this contract, including completion and retries; ordinary caller markers
  are still preserved (docs/workflow.md).
- CI's new `forgejo` job runs `agentic-job apply` against a real Forgejo
  15 LTS container: a comment and a pull request from the bot's fork, a
  neutralized mention and a refused protected path, with a token scoped to
  `write:repository` and `write:issue` (docs/safe-outputs.md#publishing-to-forgejo).
  It is not yet a required check.
- `run` no longer succeeds when the inference proxy no longer knows the
  run's token at its end (HTTP 401, as after a broker restart): it says
  `inference token revoked mid-run (broker restart?)` and exits 1, or 3
  with the change as a partial one if the agent handed one back
  (docs/inference.md#what-run-does). Under `ready_when_complete`, the
  agent's own request from a run that stopped early now carries a
  checkpoint too, so that it opens as a draft. Broker token loss is reported
  as `inference_unreachable`, including for a partial change.
- Under `egress.write-policy`, the runner may also upload to the verified
  results storage account `productionresultssa31`, alongside gh-aw's first
  twenty. Hosted runners now get this account, so `upload-artifact` failed
  (tracker#974).
- With `[session] listen`, `sandbox check` probes that the sandbox user cannot
  reach the session bridge on loopback or at the node's own tailnet addresses,
  against a listener of the runner's user as the control
  (docs/sandbox-check.md#a-sessions-bridge).
- `.github/agentic-job/private-digest/` is a reference caller for a private
  repository: one digest comment on one issue, with the sandbox user, the
  egress allowlist and the job's read token. It is an example to copy and is
  not installed (docs/private-repositories.md#a-reference-caller).
- dispatch.yml's `target` job runs `agentic-job dispatch` in place of its
  github-script step, with the same refusals, outputs and task text. It
  checks out the agentic-job source to get the binary: a copy names
  `cgwalters-forge/agentic-job` and the commit its `run` job pins there
  (docs/dispatch.md).
- `policy.yml` and `agentic-job.yml` take the task as an artifact,
  `task-artifact`, in place of `task`: the runner drops a job output its secret masker changes, so
  a task quoting credential-shaped text no longer reaches policy empty.
- `budget-usd` caps a run's spend in US dollars. `budget` (AIC) is
  deprecated and used only while `budget-usd` is empty; it will be
  removed after one release. Run summaries and opted-in status comments
  report the run's cost in dollars.
- A run a model provider's failure stopped hands back its uncommitted
  change as a partial pull request instead of losing it. Sessions retry
  a transient provider failure for every agent, and Claude Code can fall
  back to `agent.fallback-model` after a long overload. The new
  `failure-class` output names what a failed run died of.
- A run that hands back only `missing_data` is reported as blocked (the
  `blocked` output) and apply succeeds.
- Policy admits a run that asks for the bounds'
  `[secondary_pull_request]`, and apply opens it from
  `SAFE_OUTPUTS_PAT`'s fork, never with the job token.
- `dedupe = true` in the bounds refuses a dispatch that overlaps an
  active run. The policy job now needs `actions: read`: a caller that
  calls `policy.yml` itself must grant it.
- The detector retries registering and ending the run through a proxy
  restart.
- A session admits tagged tailnet nodes by `[session] capability`, and
  `agentic-job session connect` joins one from a terminal.
- `agentic-job collect` validates every safe-output type a run may be
  allowed, and is now the only collector, in `handback check` and in the
  check job; gh-aw's collector no longer runs. Event admission,
  dispatched reviews, label activation, push, status comments and issue
  linking moved from workflow JavaScript into the binary.
- Claude Code's own commit and pull request attribution is turned off in
  agent runs; the run's configured trailers are the only credit.
- The run action's setup duration report is gone.
- The egress proxy refuses an address a refused name resolves to before
  its domain allowlist, so the error names the right cause.

## v0.6.4 - 2026-10-23

- Off GitHub: `agentic-job collect` validates safe outputs in Rust, and
  `agentic-job apply --forge forgejo` publishes a checked hand-back to
  Forgejo. Runs off GitHub are held to a neutral safe-output profile, and
  bounds may admit plain-HTTP clone URLs for hosts they name.
- `run` fronts the inference broker with its own proxy, so the agent
  never holds the run token, and the handback scan refuses a run token.
- The runner's user is the default agent user; `agent-user: sandbox`
  selects the second user. `sandbox.create-user = false` opts out of
  creating it.
- New inputs: `agent-user`, `egress-write-policy`, holding the agent job
  to the egress proxy's write rules, and `status-comment`, naming the
  comment a run's status goes in.
- A hand-back whose requests file is not taken is dropped, and the
  summary says why.
- Bounds may add protected paths as globs, and the per-output pins apply
  to `add_reviewer` and `assign_to_user`.
- Bounds may reserve a `[secondary_pull_request]`; policy still refuses a
  run that would open one.
- Under the runner lock, the metadata, runner-read and Worker-diagnostics
  rules are on by default.
- Apply escapes what its handlers log, so a patch cannot form a workflow
  command, and keeps carriage returns in patches applied to Forgejo.
- The documentation is published as a site on GitHub Pages.

## v0.6.3 - 2026-10-19

- The second user, `secure-host/internal` and `sandbox exec` are
  retired.
- A named inference broker can be given its tailnet address by the agent
  job.
- A session hands back when the agent is stuck on one tool call, and
  retries a rate-limited task turn before asking for a hand-back.
- Review logs why GitHub refused a review submission.

## v0.6.2 - 2026-10-18

- The public `secure-host` action forwards to
  `cgwalters-forge/actions/secure-host`, and the agent runs as the
  runner's user after it.
- Safe outputs are handed back by artifact id and digest, and check
  verifies the digest.
- The inference run is registered in a job before the agent's, which
  loses `id-token`.
- Opt-in: every runner process held to the egress write policy, and the
  runner-read rule turned on from the host configuration.
- Review carries inline findings in the submitted review.
- Status updates on the same repository honor the caller's identity.
- `workflows check` holds proposals to the base's shape policy and pins
  secure-host's new path.

## v0.6.1 - 2026-10-17

- The release's secure-host image is attested, and its digest is read
  without a trailing newline.

## v0.6.0 - 2026-10-17

- The first release with the secure-host image: a FROM-scratch image,
  built and attested, that sets the host up through `nsenter`, and runs
  the egress proxy as a root podman unit.
- Opt-in: refuse reads between the runner's processes.
- Forgotten shell overrides are kept away from runner credentials.
- `workflows check` holds secure-host jobs to the two-job pattern.
- `handback check` refuses a `missing_data` with no reason.
