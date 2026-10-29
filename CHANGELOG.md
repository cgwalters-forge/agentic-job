# Changelog

User-visible changes to the reusable workflows, the actions and the
`agentic-job` binary, newest first. Each version's
[release notes](https://github.com/cgwalters-forge/agentic-job/releases)
list every pull request merged into it; `build-DIGEST` releases are CI
artifacts, not versions. [Stable interfaces](docs/stable-interfaces.md)
says what a caller may rely on across versions.

## Unreleased

- `agentic-job apply --forge gitlab` also publishes
  `push_to_pull_request_branch`, one commit on the policy's open merge
  request's branch in the bounds' fork, under the same patch, head and
  lease rules as Forgejo's; a merge request from another project or
  branch, or whose head moved, is refused before any write. `--status`
  and `--status-sha` now set the caller's commit status on GitLab too,
  name `agentic-job`, with `failure` and `error` both GitLab's `failed`;
  a re-run finding its own status set writes none
  (docs/safe-outputs.md#publishing-to-gitlab).
- The pinned model price table accepts opt-in `estimate = true` rates with
  a required one-line `basis`. Run costs include these rates and label the
  figure estimated in `summary.json` and `summary.md`, naming the estimated
  models; models without rates remain unpriced.
- Run costs price `gpt-6.1-sol`, `gpt-6-astra` and `gpt-6-luna` at
  OpenAI's standard-tier list prices instead of reporting them unpriced.
- `native_handlers` also takes `add_labels`, `remove_labels`,
  `close_issue` and `close_pull_request`: agentic-job's own handlers
  publish them as gh-aw's pinned handlers do, which the publication corpus
  checks for each, now with cases for filtered and missing labels,
  already closed and reopened issues, failed closing comments and a run
  mixing both handlers across a re-run. A routed request must name its
  item by number, and its labels must be plain ones, which `check`
  enforces. Every other type, `push_to_pull_request_branch`,
  `create_issue` and `create_pull_request_review_comment` among them,
  stays gh-aw's (docs/safe-outputs.md#agentic-jobs-own-handlers).
- `agentic-job apply --forge gitlab` also publishes `create_issue`,
  `close_issue`, `close_pull_request`, `add_labels` and `remove_labels`.
  GitLab numbers issues and merge requests apart, so on GitLab a label
  request names its item by exactly one of `issue_number` and
  `pull_request_number` (`item_number` stays refused there), which
  `agentic-job collect` now takes for those types; it refuses an
  `issue_number` or `pull_request_number` a type does not take there
  instead of dropping it. Every label must already exist in the project
  by its exact name, as GitLab would create it; a re-run changes nothing
  more (docs/safe-outputs.md#publishing-to-gitlab).
- `summary.json` gives `cache_ratio`, the share of the run's input read
  from the prompt cache, and `cache_warning`, true for an opencode run
  that read under half from it after more than three requests;
  `summary.md` says both (docs/inference.md#prompt-caching).
- Runs with agent `claude` whose `run` is newer than the pinned
  secure-host image, such as `review.yml` at a commit of main, start
  again. Since the attribution change below they stopped on
  "managed-settings.json is not what this run's configuration calls
  for": the image's release writes the managed settings without the
  attribution switches, which are now in the user settings `run` writes
  in the agent's home.
- `agentic-job apply --forge gitlab` publishes a checked hand-back's
  `add_comment`, as a note on the caller's issue or merge request
  (`--comment-target-kind` says which), and `create_pull_request`, as a
  merge request from the bounds' fork, to GitLab with `GITLAB_TOKEN`.
  Every body has GitLab's mentions (`@all`, `@here`, groups) and quick
  actions (a line starting with `/`) broken, including inside code so
  escaped delimiters cannot bypass the filter; other types and `--status`
  are refused (docs/safe-outputs.md#publishing-to-gitlab). On Forgejo
  too, a pull request's title now has its mentions broken.
- `sandbox setup` and `sandbox check` take less time in the slow tail
  of hosted runs: apt installs suspend needrestart's process scan, the
  network stage cuts a runner's control short after 3 seconds (one that
  has not connected is a note, as before), and the tailnet's refusals
  from a container are made at once in one container. Every probe and
  refusal timeout stays.
- `review.yml`, with the repository variable `REVIEW_HISTORY=true`, gives
  the review agent the pull request's earlier verdicts by the review job,
  their findings and the replies to them, the commits since each and the
  conversation's comments, sanitized, as an untrusted
  `~/review-history.json`; the review task has it settle earlier findings
  first and record resolved ones on `Resolved:` lines. New:
  `agentic-job review history`, `run --review-history` and `agent.yml`'s
  `review-history` input (docs/review.md#review-history).
- `sandbox.close-resolved`, off by default, closes systemd-resolved to
  every user but root and the runner's, so the sandbox user cannot send
  data out in a name lookup around the egress proxy; both `hosted.toml`
  files set it. No release note had named it (docs/sandbox.md).
- `agentic-job dispatch`'s `review` profile, like `review.yml`, asks whether a
  change should exist as written before whether it works, blocking with
  "should not exist as written: X instead", and flags tests that only restate
  the implementation as important findings.
- `review.yml`, with the repository variable `REVIEW_CARRY_APPROVAL=true`,
  carries the previous head's approval to a push that is only that head
  rebased onto the base branch or rustfmt's output of it, instead of
  running the agent; the status `review`, and the submitted review where
  real reviews are on, say whose approval and why. New:
  `agentic-job review carry`; `review submit --accepted` is optional for
  a carried approval (docs/review.md#carried-approvals).
- A run the timeout stops no longer ends well when the agent answers the
  cancel with `end_turn`, as opencode does, without the output the run
  requires: the agent gets its hand-back turn, and a run that still hands
  back nothing exits 124 with `the agent timed out without handing back a
  result`, which the run action makes its error annotation. A review that
  timed out with no verdict had concluded success, so nothing re-ran it
  (#2149). opencode now has a `stall-timeout` of 600 seconds: one of its
  model requests hung for 70 minutes, until the timeout, and a stall's
  cancel it answers with `end_turn` is still a stall (docs/harness.md).
- `agentic-job apply --forge forgejo` publishes `push_to_pull_request_branch`:
  one commit on the policy's open pull request's branch, only in the bounds'
  `push_repo` fork, only at the head the run was asked for, without force and
  under a lease on that head, so a branch that moved is refused. Its new
  `--status STATE --status-sha SHA` sets the caller's commit status, context
  `agentic-job`, after every other write; no request can ask for one
  (docs/safe-outputs.md#pushing-to-a-pull-requests-branch-on-forgejo,
  docs/safe-outputs.md#commit-statuses-on-forgejo).
- A bounds file's `native_handlers` (off by default) has apply publish
  `add_comment` and `create_pull_request` on GitHub with agentic-job's own
  handlers (`agentic-job publish`) instead of gh-aw's. They send what gh-aw's
  pinned handlers send, which the publication corpus checks, and they
  refuse a run that gh-aw would publish differently before writing anything.
  The routed requests must come before every other type but `noop`,
  `missing_tool` and `missing_data`, which `check` enforces
  (docs/safe-outputs.md#agentic-jobs-own-handlers).
- `agentic-job diagnostics handler-errors` takes more than one report.
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
  results storage accounts `productionresultssa31` and
  `productionresultssa30`, alongside gh-aw's first twenty. Hosted runners
  now get these accounts, so `upload-artifact` failed (tracker#974).
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
- `policy.yml` and `agentic-job.yml` can take the task as an artifact,
  `task-artifact`, as an alternative to `task`, which still works: the
  runner drops a job output its secret masker changes, so a task quoting
  credential-shaped text passed this way no longer reaches policy empty.
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
  agent runs, so the run's configured trailers are its only credit
  unless the target repository's settings turn it back on.
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
