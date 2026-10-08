#!/bin/sh
set -eu
mkdir -p "$HOME/.config/fake-agent"
cat > "$HOME/.config/fake-agent/demo.json" <<'JSON'
[
  {"execute": {"title": "Bash", "command": "printf 'fake secret: gh%s_%s\\n' p FAKEREDACTIONCANARY0123456789abcdef"}},
  {"execute": {"title": "Bash", "command": "git log -1 --oneline && git diff --stat origin/main...HEAD"}},
  {"execute": {"title": "Bash", "command": "node -e 'const fs = require(\"node:fs\"); const sha = require(\"node:child_process\").execFileSync(\"git\", [\"rev-parse\", \"HEAD\"], {encoding: \"utf8\"}).trim(); fs.writeFileSync(process.env.HOME + \"/out/safe-outputs.jsonl\", JSON.stringify({type: \"add_comment\", body: \"VERDICT: APPROVE\\nREASON: Scripted review exercises the caller, not a real code review.\\nReviewed SHA: \" + sha + \"\\nNo inference was enabled.\"}) + \"\\n\");'"}},
  {"write": {"title": "Write", "path": "{home}/out/outcome.json", "content": "{\"summary\":\"Scripted review completed.\",\"tests\":[],\"stopped_early\":null}\n"}}
]
JSON
