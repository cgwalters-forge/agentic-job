# Event and API fixtures

JSON files imitating the GitHub Actions event payload (the file at
`$GITHUB_EVENT_PATH`) for each trigger, plus `permission-*.json`, the
response of `GET /repos/octo/repo/collaborators/LOGIN/permission`.

They are hand-written in the shape of GitHub's webhook payloads and REST
responses, trimmed to the fields the tests need plus a few realistic extras.
They were not recorded from a live run.

Everything refers to one invented repository, `octo/repo` (id 100), unless
the file name says otherwise (`pull_request-fork.json` has a head repository
`mallory/repo`, id 999).
