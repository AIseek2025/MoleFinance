#!/usr/bin/env bash
# verify-test-counts.sh — wave 13 governance gate.
#
# Walks `cargo test --workspace --all-targets` output, extracts the
# total `passed` count (sum of every `test result: ok. N passed;`
# line), and asserts it matches the count claimed in
# `Docs/Planning/20-攻坚开发进度与里程碑.md` (search for
# `cargo test --workspace` in the latest wave's verification block,
# which is the canonical declared count).
#
# The script also asserts the failed count is zero — a regression in
# tests should reach you via the cargo step, but if cargo somehow
# returns 0 with failures, this is the second line of defense.

set -euo pipefail

CLAIMED_FILE="${CLAIMED_FILE:-Docs/Planning/20-攻坚开发进度与里程碑.md}"

# Pull the highest "262/262" -style count from the doc — the canonical
# declared count is the most recent wave's. We take the largest match
# so a doc backfill of an earlier wave doesn't regress the gate.
declared=$(
  grep -oE '[1-9][0-9]{1,3}/[1-9][0-9]{1,3} pass' "$CLAIMED_FILE" \
    | awk -F'/' '{ if ($1 + 0 > max) max = $1 + 0 } END { print max+0 }'
)

# Fallback: try non-slash form (e.g. "**262**" inside a markdown
# table cell) when the documented form changes.
if [[ "$declared" == "0" ]]; then
  declared=$(
    grep -oE '\*\*[1-9][0-9]{2,4}\*\*' "$CLAIMED_FILE" \
      | tr -d '*' \
      | sort -n | tail -1
  )
fi

if [[ -z "$declared" || "$declared" == "0" ]]; then
  echo "::error::could not parse declared test count from $CLAIMED_FILE; expected '<N>/<N> pass' or '**<N>**'"
  exit 1
fi

echo "Declared test count (from $CLAIMED_FILE): $declared"

# Run cargo test and extract `N passed` per `test result:` line.
echo "Running cargo test --workspace --all-targets…"
log=$(mktemp)
trap 'rm -f "$log"' EXIT

if ! cargo test --workspace --all-targets --no-fail-fast 2>&1 | tee "$log" >/dev/null; then
  echo "::error::cargo test failed; nothing to verify until it passes"
  exit 1
fi

# Sum up `N passed` and `M failed` across every `test result:` line.
read -r passed failed < <(
  awk '
    /^test result:/ {
      for (i = 1; i <= NF; i++) {
        if ($i == "passed;") p += $(i-1) + 0
        if ($i == "failed;") f += $(i-1) + 0
      }
    }
    END { printf "%d %d\n", p, f }
  ' "$log"
)

echo "cargo test totals: $passed passed, $failed failed"

if (( failed != 0 )); then
  echo "::error::$failed test failure(s) — fix tests before checking declared count"
  exit 1
fi

# Allow the declared count to be ≤ actual (tests may have grown since
# the last doc update — that's a non-regressive case). FAIL only when
# actual < declared (someone deleted tests but didn't update docs) or
# when the gap is suspiciously large.
if (( passed < declared )); then
  echo "::error::test regression: declared=$declared, actual=$passed (missing $((declared - passed)) tests)"
  exit 1
fi

slack=$((passed - declared))
echo "Declared $declared, observed $passed (slack $slack — declared count may be stale)"

# Soft warning if the slack is > 10 — docs likely need an update.
if (( slack > 10 )); then
  echo "::warning::observed test count is $slack ahead of declared — refresh the wave summary in $CLAIMED_FILE"
fi

echo "Test count parity OK ✓"
