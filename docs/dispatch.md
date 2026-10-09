# Dispatching an operator task

Copy [dispatch.yml](../examples/dispatch/dispatch.yml) to
`.github/workflows/dispatch.yml` in the repository **the runners belong to**.
Keep [allow.toml](../examples/dispatch/allow.toml) at
`examples/dispatch/allow.toml` there. These are the only two caller files.
The reusable workflow is pinned to a commit, including its binary and actions;
review that commit before use, and review updates to the pin.

Dispatch takes a public task, target repository, issue number and profile.
`implement` is a branch run with at most one draft pull request.
`triage` and `research` are analysis runs with at most one comment, posted by
the run's own apply job directly on the target issue. All three permit one
each of `noop`, `missing_tool` and `missing_data`, with three outputs in total.
The bounds file lists exact target repositories and their common `main` base
and ceilings; targets needing different base or output ceilings are not yet
supported by this example. Do not replace exact repository names with globs.

## Security boundaries

The preflight accepts only default-branch dispatches, checks the profile and
positive issue number, and fetches that issue without checking out target code.
It rejects pull requests and API failures. This prevents a mistaken item from
being treated as an issue task. The reusable policy job separately refuses
repositories outside the committed bounds before starting the agent. The
comment target is fixed to the dispatched item: agent-supplied routing cannot
choose a different issue. Output types and their ceilings remain policy, not
task prose, so prompt injection cannot turn an analysis into a patch or an
implementation into a comment writer.

Only read access and OIDC permission are granted to the reusable call. The
preflight has read access only. Only the separate apply job enters
`agent-apply` and receives its environment token; neither preflight nor the
sandbox holds it. No secrets are inherited. No event notifications are enabled.
The caller forwards no allow/config path, runner selection, apply environment
or arbitrary output allowlist from dispatch inputs. Runner selection and
inference settings come from operator-controlled repository variables.
See [job separation](workflow.md) and [output validation](safe-outputs.md).

## Remaining work and verification

This is a partial implementation of [#171](https://github.com/cgwalters-forge/agentic-job/issues/171).
The `review` choice deliberately fails before an agent starts. Existing
[event reviews](review.md) pin the admitted pull request head and validate
`VERDICT:` and `REASON:`; that interface does not accept a dispatched PR number
or environment token. A dispatch review needs trusted head resolution and
the same check-stage verdict enforcement, not a task asking the agent to fetch
whatever head it finds. Do not use a base-branch analysis as a substitute.

Local contract tests execute the preflight with a mocked API and check the
fixed caller profiles and pin. CLI policy tests use the actual bounds file,
including out-of-bounds repository and disallowed-output refusals.
They do not prove a forge write. Scripted-agent
CI runs of this caller (draft PR, triage/research comments and refusal
artifacts) remain to be added. Existing reusable-workflow CI exercises branch
and event review/comment application, but is not dispatch-profile coverage.
The example leaves runner-image configuration empty; the operator must verify
the sandbox probes on their image rather than assume hosted-image exceptions.

## Operator setup checklist

- [ ] Copy the two files above, protect the default branch, and permit the
  pinned reusable workflow and actions in Actions settings. Dispatch on that
  branch only. Replace `repos` with exact public targets; all must use `main`.
- [ ] Create environment **`agent-apply`** in the caller repository, restricted
  to its protected default branch. Store its sole secret
  **`AGENTIC_JOB_APPLY_TOKEN`** there, not as an agent environment variable.
  Use a fine-grained bot PAT limited to the named target repositories with
  **Contents: read/write**, **Pull requests: read/write**, and
  **Issues: read/write** (Metadata: read is implicit). No workflow-write or
  administration permission is needed. The bot must have access to those
  repositories, and organization approval may be required. Do not pass
  `secrets: inherit`; a missing environment token fails closed.
- [ ] Provide disposable runners with passwordless sudo initially, systemd
  257 or newer, Node and `apt-get` or `dnf`, as described in
  [runner requirements](workflow.md#what-a-caller-provides). Choose labels
  **`self-hosted`**, **`agentic-job`** for your prepared image; never reuse a
  hardened machine for another job.
- [ ] Set repository variable **`AGENT_RUNNER`** to
  `["self-hosted","agentic-job"]`; set **`INFERENCE_URL`** to the reachable
  broker URL (for example `http://BROKER:PORT`) and
  **`INFERENCE_AUDIENCE`** to the broker's configured OIDC audience. Replace
  the address, port and audience with your non-secret deployment values.
  Ensure the image's sandbox egress can reach it. No model API key goes here.
- [ ] Configure the inference proxy to verify GitHub identity tokens with
  that audience and admit workflows of the operator's caller repositories.
  Today's proxy accepts any workflow of those repositories with a verified
  identity token; narrowing to this caller and the pinned called workflow is
  optional hardening, tracked in
  [tracker#452](https://github.com/cgwalters-forge/tracker/issues/452).
  Confirm registration and the broker's per-run budget/request limits before
  dispatching real tasks.
