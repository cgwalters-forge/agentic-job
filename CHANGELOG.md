# Changelog

User-visible changes to the reusable workflows, the actions and the
`agentic-job` binary, newest first. Each version's
[release notes](https://github.com/cgwalters-forge/agentic-job/releases)
list every pull request merged into it; `build-DIGEST` releases are CI
artifacts, not versions. [Stable interfaces](docs/stable-interfaces.md)
says what a caller may rely on across versions.

## Unreleased

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
