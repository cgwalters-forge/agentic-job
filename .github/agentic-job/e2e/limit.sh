#!/bin/sh
# What the scripted agent does in CI's run that stops at a limit: it
# asks for a comment, then starts more subagent tasks than the run
# allows and goes on working, so the session interrupts it to hand back
# and ends the run there.
set -eu
mkdir -p "$HOME/.config/fake-agent"
cat > "$HOME/.config/fake-agent/demo.json" <<'JSON'
[
  {"say": "End-to-end run: a comment, then too many subagent tasks."},
  {"execute": {"title": "Bash", "command": "printf 'fake secret: gh%s_%s\\n' p FAKEREDACTIONCANARY0123456789abcdef"}},
  {"write": {"title": "Write", "path": "{home}/out/safe-outputs.jsonl", "content": "{\"type\": \"add_comment\", \"body\": \"A comment from the scripted agent of a run that is stopped.\"}\n"}},
  {"write": {"title": "Write", "path": "{home}/out/outcome.json", "content": "{\"summary\": \"Commented, then started too many tasks.\", \"tests\": [], \"stopped_early\": \"too many subagent tasks\"}\n"}},
  {"task": "Review the change"},
  {"task": "Review it again"},
  {"sleep": 1},
  {"task": "And once more"},
  {"sleep": 240},
  {"say": "Not reached."}
]
JSON
