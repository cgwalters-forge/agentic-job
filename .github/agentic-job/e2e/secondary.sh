#!/bin/sh
# What the scripted agent of CI's e2e-secondary does: one change in its
# checkout and one in the clone of the bounds' secondary repository, each
# proposed by its own create_pull_request, the second naming that
# repository as its `repo`.
set -eu
mkdir -p "$HOME/.config/fake-agent"
cat > "$HOME/.config/fake-agent/demo.json" <<'JSON'
[
  {"say": "End-to-end run: a pull request here and one in misctool."},
  {"execute": {"title": "Bash", "command": "printf 'fake secret: gh%s_%s\\n' p FAKEREDACTIONCANARY0123456789abcdef"}},
  {"write": {"title": "Write", "path": "{cwd}/e2e-secondary.txt", "content": "From the scripted agent of an end-to-end run.\n"}},
  {"write": {"title": "Write", "path": "{home}/work/secondary/cgwalters-forge/misctool/e2e-secondary.txt", "content": "From the scripted agent of an end-to-end run.\n"}},
  {"write": {"title": "Write", "path": "{home}/out/safe-outputs.jsonl", "content": "{\"type\": \"create_pull_request\", \"title\": \"e2e: the run's own change\", \"body\": \"From the scripted agent.\"}\n{\"type\": \"create_pull_request\", \"repo\": \"cgwalters-forge/misctool\", \"title\": \"e2e: the secondary change\", \"body\": \"From the scripted agent.\"}\n"}},
  {"write": {"title": "Write", "path": "{home}/out/outcome.json", "content": "{\"summary\": \"Proposed a change here and one in misctool.\", \"tests\": [], \"stopped_early\": null}\n"}},
  {"say": "Done."}
]
JSON
