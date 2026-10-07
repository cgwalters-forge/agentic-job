#!/bin/sh
# What the scripted agent does in CI's run that stops at a limit: it
# changes a file, then starts more subagent tasks than the run allows
# and goes on working, so the session interrupts it to hand back and
# ends the run there. It asks for a comment and for no pull request: one
# is made up from its report.
set -eu
mkdir -p "$HOME/.config/fake-agent"
cat > "$HOME/.config/fake-agent/demo.json" <<'JSON'
[
  {"say": "End-to-end run: half a change, then too many subagent tasks."},
  {"execute": {"title": "Bash", "command": "printf 'fake secret: gh%s_%s\\n' p FAKEREDACTIONCANARY0123456789abcdef"}},
  {"write": {"title": "Write", "path": "{cwd}/docs/e2e-partial.md", "content": "The first half, written before the run was stopped.\n"}},
  {"write": {"title": "Write", "path": "{home}/out/safe-outputs.jsonl", "content": "{\"type\": \"add_comment\", \"item_number\": 1, \"body\": \"A comment from the scripted agent of a run that is stopped.\"}\n"}},
  {"write": {"title": "Write", "path": "{home}/out/outcome.json", "content": "{\"summary\": \"Wrote the first half of a scratch file.\", \"tests\": [], \"stopped_early\": \"too many subagent tasks\"}\n"}},
  {"task": "Review the change"},
  {"task": "Review it again"},
  {"sleep": 1},
  {"task": "And once more"},
  {"sleep": 240},
  {"say": "Not reached."}
]
JSON
