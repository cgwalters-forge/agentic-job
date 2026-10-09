#!/bin/sh
# What the scripted agent does in CI's end-to-end run: `sandbox setup`
# runs this as the sandbox user, and `fake-agent demo` then plays the
# session it leaves. It prints a string shaped like a credential (which
# the run insists was redacted), changes a file, and asks for a pull
# request and a comment.
set -eu
mkdir -p "$HOME/.config/fake-agent"
cat > "$HOME/.config/fake-agent/demo.json" <<'JSON'
[
  {"say": "End-to-end run: one change, one pull request, one comment."},
  {"execute": {"title": "Bash", "command": "printf 'fake secret: gh%s_%s\\n' p FAKEREDACTIONCANARY0123456789abcdef"}},
  {"execute": {"title": "Bash", "command": "git log --oneline -1 && id -un"}},
  {"write": {"title": "Write", "path": "{cwd}/docs/e2e-scratch.md", "content": "Written by the scripted agent of an end-to-end run.\n"}},
  {"write": {"title": "Write", "path": "{home}/out/safe-outputs.jsonl", "content": "{\"type\": \"create_pull_request\", \"title\": \"docs: Scratch change of an end-to-end run\", \"body\": \"Opened by CI's end-to-end run, which closes it again.\"}\n{\"type\": \"add_comment\", \"body\": \"A comment from the scripted agent of an end-to-end run.\"}\n"}},
  {"write": {"title": "Write", "path": "{home}/out/outcome.json", "content": "{\"summary\": \"Added a scratch file.\", \"tests\": [{\"command\": \"git log --oneline -1\", \"exit_code\": 0, \"duration_s\": 0}], \"stopped_early\": null}\n"}},
  {"cost": {"usd": 0.01}},
  {"say": "Done."}
]
JSON
