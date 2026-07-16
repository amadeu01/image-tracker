#!/usr/bin/env bash
# Fires on Bash tool calls; only acts on git commit invocations.
input=$(cat)
cmd=$(echo "$input" | grep -o '"command"[[:space:]]*:[[:space:]]*"[^"]*"' | head -1)
if echo "$cmd" | grep -q 'git commit'; then
  echo '{"systemMessage": "Commit detected: dispatch the rust-ddd-reviewer subagent on the new commit (crates/**/*.rs or Cargo.toml/workspace structure changes)."}'
fi
