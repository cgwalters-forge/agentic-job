#!/bin/sh
set -eu
mkdir -p "$HOME/.config/fake-agent"
cat > "$HOME/.config/fake-agent/demo.json" <<'JSON'
[
  {"execute":{"title":"Redaction control","command":"printf 'redaction trial: gh%s_%s\\n' p FAKEREDACTIONCANARY0123456789abcdef"}},
  {"write":{"title":"Comment","path":"{home}/out/safe-outputs.jsonl","content":"{\"type\":\"add_comment\",\"body\":\"Scripted dispatch completed. This tests wiring, not real triage or research; no model was used.\"}\n"}},
  {"write":{"title":"Outcome","path":"{home}/out/outcome.json","content":"{\"summary\":\"Scripted comment dispatch completed.\",\"tests\":[],\"questions\":[],\"stopped_early\":null}\n"}}
]
JSON
