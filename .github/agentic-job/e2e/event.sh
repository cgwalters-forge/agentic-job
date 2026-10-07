#!/bin/sh
# The scripted agent's session for an event-triggered run: one comment,
# which the apply job sends to the issue or pull request the run was
# started from, and nothing else but the secret-shaped string `run`
# insists was redacted.
set -eu
mkdir -p "$HOME/.config/fake-agent"
cat > "$HOME/.config/fake-agent/demo.json" <<'JSON'
[
  {"say": "Event-triggered run: one comment back to where it started."},
  {"execute": {"title": "Bash", "command": "git log --oneline -1 && id -un"}},
  {"execute": {"title": "Bash", "command": "printf 'fake secret: gh%s_%s\\n' p FAKEREDACTIONCANARY0123456789abcdef"}},
  {"write": {"title": "Write", "path": "{home}/out/safe-outputs.jsonl", "content": "{\"type\": \"add_comment\", \"body\": \"A comment from the scripted agent of an event-triggered run.\"}\n"}},
  {"write": {"title": "Write", "path": "{home}/out/outcome.json", "content": "{\"summary\": \"Commented where the run started.\", \"tests\": [], \"stopped_early\": null}\n"}},
  {"cost": {"usd": 0.01}},
  {"say": "Done."}
]
JSON
