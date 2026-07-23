#!/usr/bin/env bash
# CI guardrail (PLAN.md 20.2, replaces the recurring manual-audit discovery
# that flagged app.rs at 872, then state.rs, then side_panel.rs).
#
# Fails the build when a non-test Rust source file body grows past the
# threshold, so bloat is caught at the PR that introduces it instead of
# being rediscovered by an architecture audit every few milestones.
#
# "Body" = the lines before the file's `mod tests` block (and its
# `#[cfg(test)]` attribute, if immediately above it), or the whole file if
# there is no such block. This excludes large unit-test suites from the
# count, since those aren't the readability/navigability problem the
# threshold targets — production code sprawl is.
#
# New files over the threshold fail outright. Existing debt is capped, not
# silently blocked, via the explicit allow-list below: each entry names the
# task that will retire it.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

THRESHOLD=800

# path : reason (task that will retire this entry)
ALLOWLIST_PATHS=(
  "crates/tracker-app/src/tracking.rs"
  "crates/tracker-app/src/compare.rs"
)
ALLOWLIST_REASONS=(
  "PLAN 20.x refactor pending"
  "PLAN 20.x refactor pending"
)

is_allowlisted() {
  local target="$1"
  local i
  for i in "${!ALLOWLIST_PATHS[@]}"; do
    if [ "${ALLOWLIST_PATHS[$i]}" = "$target" ]; then
      return 0
    fi
  done
  return 1
}

allowlist_reason() {
  local target="$1"
  local i
  for i in "${!ALLOWLIST_PATHS[@]}"; do
    if [ "${ALLOWLIST_PATHS[$i]}" = "$target" ]; then
      echo "${ALLOWLIST_REASONS[$i]}"
      return 0
    fi
  done
  echo ""
}

# body_line_count FILE: prints the number of lines before `mod tests`
# (and its `#[cfg(test)]` attribute, if present directly above it), or the
# total line count if the file has no such marker.
body_line_count() {
  local file="$1"
  local mod_line
  mod_line="$(grep -n '^mod tests' "$file" | head -1 | cut -d: -f1 || true)"
  if [ -z "$mod_line" ]; then
    wc -l < "$file" | tr -d ' '
    return 0
  fi
  local cutoff=$((mod_line - 1))
  if [ "$cutoff" -ge 1 ]; then
    local prev_line
    prev_line="$(sed -n "${cutoff}p" "$file")"
    case "$prev_line" in
      *'#[cfg(test)]'*) cutoff=$((cutoff - 1)) ;;
    esac
  fi
  echo "$cutoff"
}

FAILED=0
OFFENDERS=""

while IFS= read -r file; do
  body="$(body_line_count "$file")"
  rel="${file#./}"
  if [ "$body" -gt "$THRESHOLD" ]; then
    if is_allowlisted "$rel"; then
      : # known, capped debt — allowed
    else
      FAILED=1
      OFFENDERS="${OFFENDERS}  ${rel}: ${body} body lines (threshold ${THRESHOLD})\n"
    fi
  fi
done < <(find crates -path '*/target' -prune -o -type f -name '*.rs' -print | sort)

if [ "$FAILED" -ne 0 ]; then
  echo "FAIL: file(s) exceed the ${THRESHOLD}-line non-test body threshold and are not on the allow-list:" >&2
  printf '%b' "$OFFENDERS" >&2
  echo "Either split the file, or add it to ALLOWLIST_PATHS/ALLOWLIST_REASONS in scripts/check-file-sizes.sh with a comment naming the task that will retire it." >&2
  exit 1
fi

echo "OK: no non-allow-listed .rs file body exceeds ${THRESHOLD} lines."
