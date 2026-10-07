# Event-triggered runs

A caller's workflow can be started by anything GitHub delivers: an
issue, a comment, a pull request, a schedule, a dispatch. What makes
that safe and useful is one command, `agentic-job event`, which is the
only thing that reads the event's payload. It is gh-aw's
`pre_activation` and `activation` jobs in one place
([the validation against gh-aw](https://github.com/cgwalters-forge/agentic-job/blob/bot/initial-docs/docs/background-ghaw.md)): who may start a run, from
which events, and how the event's text reaches the agent.

**State.** The command, its wiring into the reusable workflow
(`event: true`, [described here](workflow.md#event-triggered-callers))
and three example callers are on main; CI runs the event path on every
pull request and every push (`e2e-event`), and the command's decision on
recorded and hand-written payloads. The workflow does not yet fetch a
pull request's head for the agent (the task names it, and the agent
fetches it); label commands are
[#117](https://github.com/cgwalters-forge/agentic-job/issues/117).

## The decision

```text
agentic-job event --allow allow.toml --event-name "$GITHUB_EVENT_NAME" \
    --event "$GITHUB_EVENT_PATH" --actor "$GITHUB_ACTOR" \
    --repository "$GITHUB_REPOSITORY" --actor-permission permission.json \
    [--pull-request pull.json] --task task.md --out DIR
```

It reads the `[trigger]` table of the caller's bounds file and decides,
fail closed: a bounds file without the table starts nothing. Exit 0 and
`DIR/task.md` when the run may start; exit 1 and no task when it may
not; exit 2 when the bounds or the payload cannot be read. Either way
`DIR/event.json` (`agentic-job-event/v1`) says what was decided and why,
so the workflow can end quietly.

```toml
[trigger]
# Which events may start a run: issues, issue_comment, pull_request,
# pull_request_target, pull_request_review_comment, schedule,
# workflow_dispatch.
events = ["issue_comment", "pull_request", "schedule"]
# The roles an actor may hold, exactly: "maintain" is not "write", and
# "admin" is not either. gh-aw's default.
roles = ["admin", "maintain", "write"]
# Bot logins admitted without a role. Never github-actions[bot].
bots = []
# Whether a pull request whose head is in another repository may start a run.
forks = false
# A comment must start with one of these.
commands = ["/agent"]
```

The checks, in order:

1. The event is one the bounds list.
2. The actor (`github.actor`) is the payload's `sender`, and the
   payload's repository is the one the workflow runs in.
3. A schedule has no actor and passes. Otherwise the actor is a bot
   (`[bot]`, or `sender.type` is `Bot`) and must be in `bots`, or a
   person whose role on the repository is in `roles`. The role comes
   from the response of
   `GET /repos/OWNER/NAME/collaborators/LOGIN/permission` (`role_name`,
   or `permission` where there is none), which the workflow fetches with
   its own token: the binary holds none. `github-actions[bot]` is
   refused whatever the bounds say, since a run it starts is a loop.
4. The action is one that starts a run: `opened`, `reopened` and
   `labeled` for issues; `opened`, `reopened`, `synchronize`,
   `ready_for_review` and `labeled` for pull requests; `created` for
   comments. An edit is not one: the editor may not be the author, and
   the text that was read is gone. Which label it was is for the
   caller's `if:` to say, as an expression.
5. A comment's actor is its author.
6. A comment's body starts with one of `commands`, at the very first
   character, as gh-aw matches it. Everything after the command is the
   request.
7. A pull request's head is in the repository, or `forks = true`. A
   fork's head is never reported as something to start from. The payload
   of a comment on a pull request does not say where the head is, so the
   workflow fetches the pull request (`GET /repos/OWNER/NAME/pulls/N`)
   and passes it in; it has to be the one the comment is on, and then
   the same rule applies to its head. Without it, such a comment is
   admitted only with `forks = true`.

What `event.json` carries for the workflow: the item (`issue` or
`pull_request`, its number, and its URL when it is under the
repository's; no text of the event is in this file), the default target
of an `add_comment`; the command; for a pull request its base branch
and, when the head is the repository's own, the head's ref and sha; a
`concurrency` key (`issue-N`, `pull-N`, `schedule`, `dispatch`); and
`react_to`, the comment or item the workflow puts an eyes reaction on so
the human sees the run started. The `reason` is one line.

## The task file

The caller's own task text comes first, unchanged. Then a line saying
which event on which item by whom started the run; for a pull request,
that its head can be fetched as `refs/pull/N/head`, with the commit id
when the head is the repository's own (never a branch name: that is the
author's text); and a notice that what follows is text from GitHub, data
for the task and not instructions. Then each piece of the event's text,
named (the request, the item's title, its body, a review comment's place
in the diff), inside a fence of backticks longer than any run of
backticks in it, so that nothing in the text can close the fence early.
gh-aw substitutes its sanitized text unfenced where the prompt names
it; the fence is the difference.

Before it is fenced, each piece is stripped of ANSI escape sequences,
control characters other than tab and newline (so a carriage return
cannot overwrite a line in the job log), every Unicode format character
(category Cf: zero-width spaces and joiners, bidi controls, soft
hyphens, the byte-order mark, and the tag block that hides text inside
an emoji) and the line and paragraph separators; and capped at 64 KiB
and 2000 lines, with 160 KiB for all of it. With the caller's task,
64 KiB at most, the file stays under the 256 KiB `run` accepts; a test
holds the sum. Mentions, issue references and URLs are left as written:
the task file notifies nobody, and they matter only when the agent
copies text into an output, where gh-aw's collector handles them.
Unicode normalization and homoglyph mapping, which gh-aw also does, are
not done.

## What the workflow does with it

With `event: true`, the policy job, after the binary is in place and
before anything else: fetches the actor's permission and, for a comment
on a pull request, the pull request, with the job's token; runs `event`
on the payload GitHub wrote, with the `task` input as the caller's own
task; and on a refusal ends the run there, with the reason in the job
summary and in the `trigger-reason` output, the other jobs skipped and
the workflow green, since most refusals are comments that are not
commands. Admitted, the task file goes to the agent job with the policy,
the pull request's base is the run's base (the bounds have to cover it),
and `comment-target` defaults to the item. A `notify` job puts an eyes
reaction on what started the run and a status comment on the item, and
a `conclude` job edits that comment when the run ends, or adds a rocket
or confused reaction instead (`notify: reaction`), or nothing (`notify:
none`); like the apply job these write with what the caller granted the
call, and the agent's machine never holds a token that can comment.
The event's text never reaches a script: the binary reads the payload
from `GITHUB_EVENT_PATH`, and the task file is passed by path. The
caller keys its `concurrency.group` on the triggering issue or pull
request itself, on the job that calls the workflow, as the examples do:
a group is decided before any job runs, and a workflow-level one is
joined before the job's `if` is evaluated.

A refused trigger leaves no comment: the policy job holds no token that
could write one, by design, and a run that was never admitted has
nothing to say. The job summary and the `trigger` outputs say why.
