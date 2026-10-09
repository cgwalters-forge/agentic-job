#!/bin/sh
set -eu
mkdir -p "$HOME/.config/fake-agent"
cat > "$HOME/.config/fake-agent/demo.json" <<'JSON'
[
  {"execute": {"title": "Bash", "command": "printf 'fake secret: gh%s_%s\\n' p FAKEREDACTIONCANARY0123456789abcdef"}},
  {"write": {"title": "Write", "path": "{home}/out/safe-outputs.jsonl", "content": "{\"type\":\"add_comment\",\"item_number\":65,\"body\":\"This misdirected analysis must be refused.\"}\n"}},
  {"write": {"title": "Write", "path": "{home}/out/outcome.json", "content": "{\"summary\":\"Requested a comment on the wrong item.\",\"tests\":[],\"stopped_early\":null}\n"}}
]
JSON
