# Workflow changes at pull request ingress

[`workflow-ingress.yml`](../.github/workflows/workflow-ingress.yml) runs on
`pull_request_target` (opened, synchronize, reopened and edited) for changes
under `.github/` or to `action.yml`/`action.yaml`. It has only `contents: read`,
no secrets or OIDC permission, and never executes PR code. The checker and
SHA-256-verified actionlint 1.7.12 and zizmor 1.30.1 pins come from `github.sha`,
the default branch, not the PR's base or head.

The checker reads changed workflow/action files at the PR head as Git blobs
into an isolated temporary directory. It rejects non-regular files and files
over 1 MiB. Zizmor runs offline, without repository configuration, including
its default audits; actionlint checks workflows with shellcheck/pyflakes disabled.
Any finding in a changed file fails, even if it existed before this PR.
Deleted and unchanged files are not audited. No custom YAML policy, job digest
or finding-identity grandfathering is involved.

For an intentional zizmor finding, add an inline suppression at the reported
location, with a reason, for example:

```yaml
# zizmor: ignore[cache-poisoning] Only parses PR blobs; never runs PR code or restores caches.
```

Keep ignores narrow and review their reasons. Actionlint has no inline-ignore
syntax: fix its findings instead. PR configuration files cannot disable either
analyzer. An inline ignore is reviewable policy, not a security proof.
The trusted `workflow/actionlint.yaml` adds the GitHub-hosted `ubuntu-26.04`
label missing from actionlint's built-in list; it does not disable label checks.

This check does **not** judge whether a workflow edit is wise, prove scripts
safe, or analyze transitive action/reusable-workflow behavior. Human or reviewer
approval before merge does that. Analyzer parser vulnerabilities remain a risk:
the runner has a runtime token and the base branch's cache scope, even though
its repository token is read-only.

An administrator must enforce the trusted workflow with a ruleset; a required
status name alone can be imitated by PR-owned workflows. Installation needs
maintainer review because the old default branch cannot run a new definition.
Events produced with `GITHUB_TOKEN` do not trigger workflows; token-created PRs
need an app/PAT-triggered event. Merge queues and private-repository unauthenticated
fetches are not supported. Hosted event delivery and ruleset enforcement need
verification on GitHub, not just local tests.
