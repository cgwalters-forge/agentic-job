# A first TLA+ model: proposal → check → apply

**Today's pipeline does not satisfy universal at-most-once application.**
Re-running apply on an accepted comment-only artifact can post the comment
again. `RerunWitness.cfg` checks the actual model with `AtMostOnce` and TLC
rejects it. This is a finding, not a deliberately weakened production model.
No production workflow or Rust is changed here, and no follow-up issue is opened.

The passing `Pipeline.cfg` checks three narrower properties: apply consumes
the immutable contents check accepted (`AcceptedOnly`), the writer does not
execute producer code (`CredentialIsolation`), and an existing run-named branch
blocks another patch push (`BranchOnce`). The last property is not a comment
deduplication rule. A green specs job means the expected results were reproduced,
**not** that the desired universal at-most-once property holds.

## Start with a state, then take a step

Open [Pipeline.tla](Pipeline.tla). A *state* is one value for every `VARIABLE`:
for example, an uploaded comment, an accepted artifact, and an empty history.
An *action* describes allowed changes from one state to the next. A prime means
the next value (`phase'`); `UNCHANGED` lists values that must stay put. `/\`
means “and”, `\/` means “or”, and `\E` means “there exists”. An action can choose
any value satisfying its conditions: TLC explores all those choices, not just
the happy path. `Init` defines the starting state; `Next` lists possible actions.

`Spec` allows these steps forever, including doing nothing (“stuttering”).
An *invariant* is a statement that must be true in every reachable state.
TLC searches the finite state graph breadth-first and stops with a trace when
an invariant becomes false. There is no fairness or liveness claim here.
`CHECK_DEADLOCK FALSE` permits terminal/refused/blocked runs; getting stuck is
not the security property we are checking.

## Where the model meets the code

`uploads` maps two immutable producer IDs to representative proposals:
`comment`, `patch`, or `invalid`. Each ID denotes a distinct payload, even if
both payloads have the same kind. `latest` is the ID currently under the same
artifact name. `Upload` represents hand-back from
[`run/handback.rs`](../crates/agentic-job/src/run/handback.rs), or a proposals-only
producer, and the upload action in
[`agentic-job.yml`](../.github/workflows/agentic-job.yml). A same-name upload
without deletion/overwrite conflicts and does not change the state. Successful
replacement has a fresh ID; it cannot mutate the old one. Missing/deleted IDs
fail downloads in reality rather than authorizing different bytes.

`accepted` identifies the checked copy of a proposal, with zero meaning no
applicable artifact. Producer IDs and checked IDs are different namespaces in
the workflow; the model labels a checked copy with its producer index, not with
a claim that the numeric IDs are equal. `Check` abstracts the collector plus
[`check/mod.rs`](../crates/agentic-job/src/check/mod.rs) and
[`check.yml`](../.github/workflows/check.yml): invalid proposals are refused,
and only accepted proposals are uploaded as `checked-outputs`. The legacy
proposals selector uses a name **before** validation; apply still uses check's
returned ID. Choosing any existing producer ID covers both selector routes.

`phase` is the check/apply job gate, not every workflow step. `Apply` abstracts
the writer's immutable-ID download and pinned gh-aw handlers in
[`agentic-job.yml`](../.github/workflows/agentic-job.yml). `history` records
successful effects as `(checked ID, used ID, kind)` for checking invariants;
**it is observation state, not a deduplication ledger in the implementation**.
`branch` represents the persistent `agent-run-RUNID` branch: another patch push
cannot overwrite it. `Retry` represents re-running apply with its old accepted
artifact or re-running check (assuming its upload succeeds). Neither erases
forge effects. Conflicting checked-artifact uploads can instead fail without
another effect.

`credential` and `executes` describe the current writer step: apply holds the
write token but treats producer files as data. Applying a patch is not running
its code. Trusted workflow scripts, pinned handlers and Git are assumed to
interpret that data safely; this model does not verify their implementations.
Separate machines, read-only check permissions, and apply-only token placement
are documented in [the workflow guide](../docs/workflow.md).

The rerun finding also follows from the actual pinned
[add-comment handler](https://github.com/github/gh-aw/blob/c227508baacbbe271be6c5a4f303936840b3c813/actions/setup/js/add_comment.cjs):
its default path calls `issues.createComment`; `processedCount` is local to a
new handler instance. Apply does not read an earlier `applied.json` to consume
requests. Comment-only apply has no checkout or existing-branch gate.

## Run it

Install Java (21 works), Node 22 and curl, then from the repository root:

```sh
bash specs/check.sh
```

`JAVA_BIN` can name a non-default Java executable. The four-line shell launcher
uses [check.mjs](check.mjs) to download the official `tla2tools.jar` v1.8.0 and
verify the SHA-256 pinned in source before execution. This pin matches the
release API's asset digest; downloads and TLC state files stay in a temporary
directory printed at the start. `TMPDIR` can move it outside the checkout.
The script prints full TLC output and keeps each report there. No jar, generated
trace module, or state database is committed.

The good config must exit 0 with TLC's completion message. Each negative control
must exit **12** and name its expected invariant; parse errors, Java failures,
unexpected successes and other failures fail the script. This follows gh-aw's
[negative-control pattern](https://github.com/github/gh-aw/blob/main/specs/work-queue/check.sh).

`BrokenName` resolves the artifact name again at apply, allowing a different
upload to replace what check accepted. It deliberately collapses producer and
checked-artifact names to demonstrate substitution, rather than literally
modeling both name registries. `BrokenCredential` runs producer code in
the credential-bearing step. `BrokenBranch` permits overwriting the run branch.
These are one deliberately broken variant per **passing** invariant, selected
by constants in the same small module. `RerunWitness` has all broken switches
off: it demonstrates the already-false desired property, rather than pretending
that production has a guard it does not have.

The independent [specs CI workflow](../.github/workflows/specs.yml) runs this
command with `contents: read`, no secrets, the existing full checkout action
pin and `persist-credentials: false`. It runs repository model/checker code only
on a read-only machine; it neither depends on nor changes the e2e jobs, production
permissions, handler pins or token placement. It is not added to the aggregate
`ci` job or branch-required checks in this change.

## Reading a real counterexample

With TLC v1.8.0, Java 21, one worker, seed 1 and fingerprint 0, the passing
model reported:

```text
Model checking completed. No error has been found.
383 states generated, 227 distinct states found, 0 states left on queue.
The depth of the complete state graph search is 8.
```

`RerunWitness` exited 12 with `Error: Invariant AtMostOnce is violated.`
Its first three states initialize, upload a comment under ID 1, then accept it.
Here are the last three states from that actual trace (action locations omitted):

```text
State 4: <Apply>
/\ credential = TRUE
/\ history = <<[kind |-> "comment", checked |-> 1, used |-> 1]>>
/\ phase = "done"
/\ accepted = 1
/\ uploads = <<"comment", "absent">>
/\ executes = FALSE
/\ branch = FALSE
/\ latest = 1

State 5: <Retry>
/\ credential = FALSE
/\ history = <<[kind |-> "comment", checked |-> 1, used |-> 1]>>
/\ phase = "apply"
/\ accepted = 1
/\ uploads = <<"comment", "absent">>
/\ executes = FALSE
/\ branch = FALSE
/\ latest = 1

State 6: <Apply>
/\ credential = TRUE
/\ history = << [kind |-> "comment", checked |-> 1, used |-> 1],
   [kind |-> "comment", checked |-> 1, used |-> 1] >>
/\ phase = "done"
/\ accepted = 1
/\ uploads = <<"comment", "absent">>
/\ executes = FALSE
/\ branch = FALSE
/\ latest = 1
```

Read adjacent states as a diff: `Retry` resets job-local credential state but
keeps the prior effect. The second `Apply` appends a second effect for ID 1.
Nothing was swapped and no untrusted code ran, yet `AtMostOnce` is false.
TLC reported 195 generated states, 124 distinct states, and depth 6 before
stopping at this counterexample. This is a model trace, not a live forge rerun.

## Honest scope

This is a bounded model beside the code, **not a proof of the code**. It explores
two producer uploads, one proposal per upload and at most two successful effects
in one workflow run. It abstracts validation into valid/invalid, assumes trusted
policy/binary/handler source selection and immutable artifact IDs, and omits
network failures, Git hooks, sandbox exploits, partial multi-output application,
concurrent apply jobs, artifact expiry, branch deletion and forge/API retries.
Branch persistence is an assumption: deleting the branch removes that guard.
Identical text uploaded under a new ID is not deduplicated by this model either.

`AcceptedOnly` is artifact/proposal identity, not byte-for-byte equality of final
forge text: trusted partial markers, sanitization, handler footers and caller-fixed
destinations may change its presentation. The model does not establish that a
patch changes only its checked paths; the trial/file-list checks and local
`workflow/apply.test.cjs` tests cover that separate implementation boundary.
There is no liveness, TLAPS, generated code, or new production security control.

When changing artifact selectors, check/upload gates, the pinned handlers,
branch behavior or token-bearing steps, review this model's corresponding
action and assumptions too. TLC cannot detect drift in workflow/Rust code it
does not execute; the negative controls test model assertions, not that mapping.
