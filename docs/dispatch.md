# Dispatching an operator task

The [shipped dispatch caller](../.github/workflows/dispatch.yml) runs the
scripted agent by default: no model, inference service or secret is needed
when the target is your own public repository. Its sessions ignore task text;
they test policy, sandbox, hand-back, check and apply, not agent quality.

## Ten-minute scripted trial

Copy these files to the **same paths** in your repository:

- [dispatch.yml](../.github/workflows/dispatch.yml)
- [dispatch.toml](../.github/agentic-job/dispatch.toml)
- [hosted.toml](../.github/agentic-job/hosted.toml)

In `dispatch.yml`, replace `uses: ./.github/workflows/agentic-job.yml` with
`uses: cgwalters-forge/agentic-job/.github/workflows/agentic-job.yml@COMMIT`,
where `COMMIT` is the full reviewed SHA of the source you copied, **including
the `review-item` and `fake-profile` inputs**. A local reusable call here pins the workflow, binary
and actions to this checkout's commit: it cannot silently run an older release.
In `dispatch.toml`, replace `repos` with your exact public repository name.
Commit the files to your protected default branch and allow the pinned actions
and reusable workflow in Actions settings. Enable Actions to create pull
requests if you want a draft rather than the pushed-branch fallback.

Leave repository variables unset for this trial. Start with an existing issue:

```sh
gh workflow run dispatch.yml -f repo=OWNER/REPO -f item=1 \
  -f kind=triage -f task='Test the dispatch wiring without a model'
```

`triage` and `research` post a canned comment on that issue and leave the tree
clean. `implement` writes `DISPATCH-TRIAL.md` and proposes one draft PR (or
pushes a branch if Actions cannot open a PR). `review` takes an **open,
same-repository pull request number targeting main**, not an issue number. It
posts a clearly scripted verdict naming the pinned head. This is not an
approval or permission to merge. Delete trial branches/comments when finished.

All targets must use `main` and fit the committed bounds. Do not use globs.
Protected documentation is refused by default; to deliberately permit a file,
use `[unprotected_files]` in the bounds file as shown in its commented example.
See [output validation](safe-outputs.md).

## Switch to a real agent

Set operator-controlled repository variables (not dispatch inputs):

- `AGENT`: `opencode` or `claude` (`fake` is the default).
- `AGENT_MODEL`: the model name the selected agent and broker support.
- `AGENT_RUNNER`: JSON runner labels, for example
  `["self-hosted","agentic-job"]`, for a disposable proxy-reachable image.
- `INFERENCE_URL`: the reachable broker URL, for example `http://BROKER:PORT`.
- `INFERENCE_AUDIENCE`: the broker's configured GitHub OIDC audience.
- `AGENT_CONFIG`: the caller's runner configuration path if not using hosted
  Ubuntu 26.04. The default is `.github/agentic-job/hosted.toml`; verify sandbox
  probes on your own image rather than copying hosted-image socket exceptions.

Preflight names **all** missing model, runner, URL and audience variables at
once, before policy/build or agent startup. It rejects unknown agents.
Real agents have a 150-request cap; scripted sessions have no inference and
`max-requests: 0`. Real agents do not install scripted sessions.
Hosted Ubuntu alone cannot reach a private broker. Provide disposable runners
with passwordless sudo initially and systemd 257+, Node and apt-get or dnf;
see [runner requirements](workflow.md#what-a-caller-provides).

Configure the broker to verify GitHub identity tokens with the chosen audience
and admit the caller repository. Confirm registration and per-run budgets
before real tasks. No model API key goes on the runner. Narrowing broker
admission to a specific caller remains optional hardening tracked in
[tracker#452](https://github.com/cgwalters-forge/tracker/issues/452).

For cross-repository targets, set `APPLY_ENVIRONMENT` to a caller environment
restricted to the protected default branch, and store its sole secret
`AGENTIC_JOB_APPLY_TOKEN` there. Use a fine-grained bot PAT restricted to the
exact targets with Contents, Issues and Pull requests read/write, no workflow
write or administration. Preflight names a missing `APPLY_ENVIRONMENT` too.
Only apply enters the environment; a missing environment token fails closed.
Never use `secrets: inherit`. For same-repository trials the apply job uses
its job token instead; no environment is needed.

## Sessions and boundaries

The built-in [patch](../workflow/dispatch-implement.sh),
[comment](../workflow/dispatch-comment.sh) and
[review](../workflow/dispatch-review.sh) session installers come from the pinned
reusable source: you need no setup script. They run as the sandbox user and write
`$HOME/.config/fake-agent/demo.json`. That JSON is an array of scripted steps:
`write` takes `title`, `path` and `content`; `execute` takes `title` and
`command`; `say`, `read` and `cost` are also supported. `{home}` and `{cwd}`
expand to the sandbox home and checkout. An executed command's failure is
reported to the session, not automatically a failing run: make your session
check the result if needed. The current fake-agent upload gate requires at
least one redaction: retain the shipped session's first `execute` step when
customizing. Its dynamically constructed canary is deliberately not a real
credential; the transcript must redact it before publication. This control is
specific to scripted runs, not a requirement of real agents.
To customize, omit `fake-profile` and use your own `setup` script
writing the same JSON path. The shipped sessions show the two hand-back files:
`{home}/out/safe-outputs.jsonl` (one safe-output JSON object per line) and
`{home}/out/outcome.json` (summary, tests, questions and stopped_early).
The implement session needs only the patch and outcome: apply constructs its
draft request from that summary. [Safe outputs](safe-outputs.md) defines the
fields and limits. Editing a session changes the trial, not the real agent.

Preflight has contents read only and fetches issue/PR title and body without
checking out target code. Text reaches the task as fenced JSON data, with
angle brackets escaped so it cannot close the fence. Dispatch inputs are never
pasted into executable code. Default-branch checks apply to manual dispatch;
the workflow-call interface lets same-repository CI exercise the exact caller.
Its caller must be trusted like any other workflow with write permissions.

The reusable policy and check jobs downscope to contents read; the agent job
has read access and runner-only OIDC registration. The sandbox has neither
forge nor OIDC credentials. Only apply receives the configured environment
token; apply and refusal reporting can use the caller's write-capable job
token, and neither executes agent-produced code. Review resolves
the open PR's exact head through the read-only policy API, refuses forks and
wrong bases, and enforces one verdict comment or noop in the clean check job.
Real reviewers start in the base checkout with the pinned head beside it.
Other profiles allow one each of noop, missing_tool and missing_data, three
outputs total; implement permits one draft PR, triage/research one comment.
Comment routing is fixed to the dispatched item, not agent-supplied fields.

## Verification

`node --test workflow/dispatch.test.cjs workflow/review.test.cjs` executes the
actual preflight, all shipped sessions and hostile review routing/output cases.
CI calls the **same dispatch.yml**, running implement, triage and research on
pushes and same-repository PRs, plus review on PRs. Its separate verifier checks
the patch, both issue comments and the review verdict, then existing CI cleanup
removes trial branches, drafts and comments. Fork PRs do not get write jobs.
These tests do not prove real-model quality or connectivity to a private broker.
