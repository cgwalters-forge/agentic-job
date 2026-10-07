# Where the code goes

One Cargo workspace, and as few crates as the work allows. This page is
for whoever implements a step of the plan: it says which files are
yours, so that steps written at the same time do not collide.

## Crates

One: `crates/agentic-job`, the released binary and its library, with a
module per command. Add a crate only for code with a second user.

The scripted ACP agent the tests drive arrives with step 3 as a second
binary of the same crate, `src/bin/fake-agent.rs`. Cargo gives a
package's tests the paths of its own binaries only
(`CARGO_BIN_EXE_fake-agent`), and steps 3, 6a and 6b all need it. The
release publishes `agentic-job` alone.

## Modules of `crates/agentic-job`

| Path under `src/` | Step | Holds |
| --- | --- | --- |
| `main.rs`, `cli.rs`, `exit.rs` | 1 | parsing, dispatch, the exit states |
| `config.rs` | 1, then each table's step | the `--config` TOML, one struct per table |
| `policy.rs` | 4 | `policy`: the caller's bounds, the request, and the `Policy` type that `policy.json` holds |
| `check.rs` | 4 | `check`: the handed-back outputs and patch against the policy |
| `redact.rs` | 4 | secret-shaped strings; used by `check` and by `run` |
| `sandbox/setup.rs` | 5 | `sandbox setup` |
| `sandbox/check.rs` | 5 | `sandbox check`; its probes are a function `run` calls again |
| `session/` | 3 | the ACP session, its limits and transcript: a library, no command |
| `run/` | 6a, 6b | `run`: `run/inference.rs` (run token), `run/agent.rs` (agent configuration), `run/clone.rs` in 6a; `run/handback.rs`, `run/summary.rs`, `run/upload.rs` in 6b |

A module that outgrows its file becomes a directory of the same name
(`check.rs` to `check/mod.rs`); its path in `lib.rs` does not change.

Three types are shared. `config.rs` is read by `sandbox setup` (step 5)
and by `run` (step 6a): the file exists now with a struct per table,
and a step changes only the structs of its own tables (`[sandbox]`,
`[egress]` and `[setup]` are step 5's, `[limits]` step 3's,
`[inference]` and `[agent]` step 6a's, `[commit]` step 6b's).
`Policy` is step 4's, and `run` reads it: steps 6a and 6b need step 4
merged before they use it, and until then take the path and do not
parse it. `redact.rs` likewise is step 4's and used by step 6b.

The egress proxy is not Rust. Step 5 puts mitmproxy's addon, policy and
tests in `egress/` at the top of the repository, as they are in the old
tree's `agent/egress/`.

gh-aw's half of the safe-outputs check, and its handlers, are not Rust
either and are not copied here: the workflows take its scripts from its
setup action, pinned by commit. Step 2a puts what those scripts need in
`safe-outputs/` at the top of the repository. `validation.json` is
generated from the pinned gh-aw (`node safe-outputs/validation.mjs
GH_AW_CHECKOUT`) and regenerated when the pin moves. `fixtures/` holds
the safe-outputs artifacts of two real runs of the old tree, each with
the configuration it ran under and a `source.json` naming the run;
`.github/workflows/safe-outputs-probe.yml` applies them.

## Rules that keep steps apart

Each command's module owns its `Args` and its `run(&Args) ->
anyhow::Result<Exit>`. `cli.rs` only lists the commands, so
implementing a command does not edit it. The arguments are the table in
the plan; a change to them is a change to the plan and belongs in the
pull request's description.

`run` returns `Ok(Exit)` for every outcome the plan gives an exit state,
including refusals and failures of the agent. `Err` is for bad input and
internal errors only: `main` prints it and exits 2. So `run` must not
let `?` carry out a failed probe, registration or clone: those are
`Exit::NotStarted`, which tells the caller a retry is safe, and `run`
maps that phase's errors itself.

`main` is synchronous. The session needs an async runtime; `run` builds
it, so the other commands do not pay for it.

Dependencies are declared once, in `[workspace.dependencies]` of the
root `Cargo.toml`, and a crate takes them with `workspace = true`. Two
steps adding dependencies will both touch that table, the crate's own
`[dependencies]` and `Cargo.lock`:
on a rebase keep both sets of lines, take either side's `Cargo.lock`, and
run `cargo metadata`, which adds what is missing and updates nothing
else.

Tests sit beside the code they test (`#[cfg(test)]`); tests of the built
binary go in `crates/agentic-job/tests/`, one file per command, and
their inputs in `crates/agentic-job/tests/data/COMMAND/`. Corpus tests
that run the old tree's code fetch it at the pinned commit in CI. They
do not copy it here.

## CI

`.github/workflows/ci.yml` has one job per kind of check and a last job,
`ci`, that the ruleset requires and that only gathers the others. A step
that needs another runner or another setup (the sandbox probes need
`run0`) adds a job and names it in `ci`'s `needs`. Two steps doing so
both edit that one line; keep both names.

Every dependency has to build for `x86_64-unknown-linux-musl`, since the
release is one static binary; the `static` job builds it on every pull
request.
