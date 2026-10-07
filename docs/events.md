# Event-triggered runs

A caller's workflow can be started by anything GitHub delivers: an
issue, a comment, a pull request, a schedule, a dispatch. What makes
that safe and useful is one command, `agentic-job event`, which is the
only thing that reads the event's payload. It is gh-aw's
`pre_activation` and `activation` jobs in one place
([the validation against gh-aw](https://github.com/cgwalters-forge/agentic-job/blob/bot/initial-docs/docs/background-ghaw.md)): who may start a run, from
which events, and how the event's text reaches the agent.

**State.** The command and its tests are on main; the workflow does not
call it yet. Wiring it into the policy and agent jobs, example callers
for a slash command, a pull request and a schedule, a CI case, and a
real event-triggered run are
[#111](https://github.com/cgwalters-forge/agentic-job/issues/111).

## The decision

```text
agentic-job event --allow allow.toml --event-name "$GITHUB_EVENT_NAME" \
    --event "$GITHUB_EVENT_PATH" --actor "$GITHUB_ACTOR" \
    --repository "$GITHUB_REPOSITORY" --actor-permission permission.json \
    --task task.md --out DIR
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
# pull_request_review_comment, schedule, workflow_dispatch.
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
   `labeled` for issues; `opened`, `reopened`, `synchronize` and
   `ready_for_review` for pull requests; `created` for comments. An edit
   is not one: the editor may not be the author, and the text that was
   read is gone.
5. A comment's actor is its author.
6. A comment's body starts with one of `commands`, at the very first
   character, as gh-aw matches it. Everything after the command is the
   request.
7. A pull request's head is in the repository, or `forks = true`. A
   fork's head is never reported as something to start from. The payload
   of a comment on a pull request does not say where the head is, so
   such a comment is admitted only with `forks = true` until the workflow
   fetches the pull request and passes it in.

What `event.json` carries for the workflow: the item (`issue` or
`pull_request`, its number, and its URL when it is under the
repository's; no text of the event is in this file), the default target of an
`add_comment`; the command; for a pull request its base branch and, when
the head is the repository's own, the head's ref and sha; a
`concurrency` key (`issue-N`, `pull-N`, `schedule`, `dispatch`) for the
caller's `concurrency.group`; and `react_to`, the comment or item the
workflow should put an eyes reaction on so the human sees the run
started.

## The task file

The caller's own task text comes first, unchanged. Then a line saying
which event on which item by whom started the run, and a notice that
what follows is text from GitHub, data for the task and not instructions.
Then each piece of the event's text, named (the request, the item's
title, its body, a review comment's place in the diff), inside a fence
of backticks longer than any run of backticks in it, so that nothing in
the text can close the fence early. gh-aw substitutes its sanitized text
unfenced where the prompt names it; the fence is the difference.

Before it is fenced, each piece is stripped of ANSI escape sequences,
control characters other than tab and newline (so a carriage return
cannot overwrite a line in the job log), and the invisible characters
that hide text from a reader (zero-width spaces and joiners, bidi
controls, the byte-order mark); and capped at 64 KiB and 2000 lines,
with 192 KiB for all of it, since `run` takes a task of 256 KiB at most.
Mentions, issue references and URLs are left as written: they matter
when the agent copies text into an output, and gh-aw's collector handles
them there. Unicode normalization and homoglyph mapping, which gh-aw
also does, are not done.

## What the workflow will do with it

In the policy job, after the binary is in place: fetch the actor's
permission with the job's token, run `event`, and stop the run quietly
when it is refused (the decision in the job summary). The task file
replaces the `task` input for the agent job; `comment-target` defaults
to the item; the caller's `concurrency.group` uses the key; a step with
`issues: write` or `pull-requests: write` adds the reaction. A pull
request's base becomes the run's base. The event's text never reaches a
script: the payload is read from `GITHUB_EVENT_PATH` by the binary, and
the task file is passed by path.
