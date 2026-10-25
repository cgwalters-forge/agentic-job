#!/bin/sh
# What the scripted agent of CI's e2e-secondary-refused does: it asks for
# the secondary pull request without changing the secondary clone, so
# there is no patch for it, and check must refuse the hand-back.
set -eu
mkdir -p "$HOME/.config/fake-agent"
cat > "$HOME/.config/fake-agent/demo.json" <<'JSON'
[
  {"execute": {"title": "Bash", "command": "printf 'fake secret: gh%s_%s\\n' p FAKEREDACTIONCANARY0123456789abcdef"}},
  {"write": {"title": "Write", "path": "{cwd}/e2e-secondary.txt", "content": "From the scripted agent of an end-to-end run.\n"}},
  {"write": {"title": "Write", "path": "{home}/out/safe-outputs.jsonl", "content": "{\"type\": \"create_pull_request\", \"title\": \"e2e: the run's own change\", \"body\": \"From the scripted agent.\"}\n{\"type\": \"create_pull_request\", \"repo\": \"cgwalters-forge/misctool\", \"title\": \"e2e: a secondary request without a change\", \"body\": \"This must be refused.\"}\n"}},
  {"write": {"title": "Write", "path": "{home}/out/outcome.json", "content": "{\"summary\": \"Asked for a secondary pull request without its change.\", \"tests\": [], \"stopped_early\": null}\n"}}
]
JSON
