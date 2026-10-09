#!/bin/sh
set -eu
mkdir -p "$HOME/.config/fake-agent"
cat > "$HOME/.config/fake-agent/demo.json" <<'JSON'
[
  {"execute":{"title":"Redaction control","command":"printf 'redaction trial: gh%s_%s\\n' p FAKEREDACTIONCANARY0123456789abcdef"}},
  {"write":{"title":"Trial patch","path":"{cwd}/DISPATCH-TRIAL.md","content":"Scripted dispatch trial. No model was used; the operator task was ignored.\n"}},
  {"write":{"title":"Outcome","path":"{home}/out/outcome.json","content":"{\"summary\":\"Exercise the dispatch patch hand-back without inference.\",\"tests\":[],\"questions\":[],\"stopped_early\":null}\n"}}
]
JSON
