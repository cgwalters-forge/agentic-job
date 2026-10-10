#!/bin/sh
set -eu
mkdir -p "$HOME/.config/fake-agent"
cat > "$HOME/.config/fake-agent/demo.json" <<'JSON'
[
  {"execute":{"title":"Redaction control","command":"printf 'redaction trial: gh%s_%s\\n' p FAKEREDACTIONCANARY0123456789abcdef"}},
  {"execute":{"title":"Toolchain","command":"rm -f \"$HOME/toolchain\"; [ ! -f Cargo.lock ] || { v=$(cargo --version) && cargo metadata --locked --format-version 1 >/dev/null && printf '%s\\n' \"$v\" > \"$HOME/toolchain\"; }"}},
  {"execute":{"title":"Pinned head","command":"node -e 'const fs=require(\"node:fs\"); const sha=require(\"node:child_process\").execFileSync(\"git\",[\"rev-parse\",\"HEAD\"],{encoding:\"utf8\"}).trim(); let tool=\"none\"; try { tool=fs.readFileSync(process.env.HOME+\"/toolchain\",\"utf8\").trim(); } catch {} fs.writeFileSync(process.env.HOME+\"/out/safe-outputs.jsonl\",JSON.stringify({type:\"add_comment\",body:\"VERDICT: APPROVE\\nREASON: Scripted dispatch tests wiring, not code quality.\\nReviewed SHA: \"+sha+\"\\nToolchain: \"+tool})+\"\\n\");'"}},
  {"write":{"title":"Outcome","path":"{home}/out/outcome.json","content":"{\"summary\":\"Scripted dispatch review completed.\",\"tests\":[],\"questions\":[],\"stopped_early\":null}\n"}}
]
JSON
