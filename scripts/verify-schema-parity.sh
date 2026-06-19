#!/usr/bin/env bash
# verify-schema-parity.sh — wave 13 governance gate.
#
# Asserts every `pub <field>: <type>,` declared on an `Onchain<T>`
# struct in `crates/keeper-rpc/src/accounts.rs` has an explicit row in
# `Docs/SCHEMA-MAPPING.md`. Each row commits to either:
#   - which `FeedSnapshot.*` TS field the Rust field projects into
#   - or an explicit `omitted (rationale)` reason
#
# This forces every protocol schema change to land alongside an
# update to the mapping doc — the two can't drift silently and an
# external auditor reading SCHEMA-MAPPING.md can verify completeness
# at the row level.
#
# Caveats:
#   - The check is purely existential (`<field>` appears somewhere as
#     a word-boundary match). Reviewers must keep the rows scoped to
#     the right struct manually.
#   - Anchor padding fields (`_pad`) and PDA bumps (`bump`) are still
#     required to be documented — they live forever in the on-chain
#     account layout, so an "always omitted" row is the right place
#     to anchor that decision.

set -euo pipefail

ACCOUNTS_FILE="${ACCOUNTS_FILE:-crates/keeper-decoder/src/lib.rs}"
MAPPING_FILE="${MAPPING_FILE:-Docs/SCHEMA-MAPPING.md}"

if [[ ! -f "$ACCOUNTS_FILE" ]]; then
  echo "::error::$ACCOUNTS_FILE not found"
  exit 1
fi

if [[ ! -f "$MAPPING_FILE" ]]; then
  echo "::error::$MAPPING_FILE not found"
  exit 1
fi

# Extract struct + field pairs from accounts.rs. We grep for
# `pub struct Onchain<T> {` openers to track which struct we're in,
# then capture `    pub <field>:` lines until the closing `}`.
#
# Output: one line per field, "<struct>\t<field>".
fields_file="$(mktemp)"
trap 'rm -f "$fields_file"' EXIT

awk '
  /^pub struct Onchain[A-Z][A-Za-z0-9_]*[[:space:]]*\{/ {
    # Capture struct name.
    n = split($0, parts, /[[:space:]]+/)
    for (i = 1; i <= n; i++) {
      if (parts[i] ~ /^Onchain[A-Z]/) {
        # Strip a trailing "{" if it merged into the name.
        gsub(/\{.*$/, "", parts[i])
        cur = parts[i]
        break
      }
    }
    in_struct = 1
    next
  }
  in_struct && /^\}/ {
    in_struct = 0
    cur = ""
    next
  }
  in_struct && /^[[:space:]]+pub[[:space:]]+[A-Za-z_][A-Za-z0-9_]*[[:space:]]*:/ {
    # Extract the field name. Tokens look like:
    #     pub field_name : Type,
    #     pub field_name: Type,
    # We split on `:`, take left side, then the last whitespace-
    # separated token of the left side.
    line = $0
    sub(/:.*$/, "", line)
    m = split(line, lhs, /[[:space:]]+/)
    fname = lhs[m]
    if (fname == "") fname = lhs[m-1]
    if (fname != "" && fname != "pub") {
      printf("%s\t%s\n", cur, fname)
    }
  }
' "$ACCOUNTS_FILE" \
  | sort -u \
  > "$fields_file"

field_count="$(wc -l < "$fields_file" | tr -d '[:space:]')"

if [[ "$field_count" -eq 0 ]]; then
  echo "::error::no Onchain* fields parsed from $ACCOUNTS_FILE — file structure changed?"
  exit 1
fi

echo "Parsed $field_count fields across the Onchain* mirrors."

broken=0
while IFS=$'\t' read -r struct_name field_name; do
  # Match `<field_name>` on word boundaries inside the mapping doc.
  # We accept the field appearing in ANY backticked context (table
  # cell, prose, code reference) — the gate is "documented" not
  # "documented in the right table".
  if ! grep -qE "\\b${field_name}\\b" "$MAPPING_FILE"; then
    echo "::error::$struct_name.$field_name not documented in $MAPPING_FILE"
    broken=$((broken + 1))
  fi
done < "$fields_file"

if [[ "$broken" -gt 0 ]]; then
  echo "::error::$broken undocumented Rust fields. Add a row to $MAPPING_FILE for each (mapped to a TS surface OR explicitly omitted with rationale)."
  exit 1
fi

echo "All $field_count Rust schema fields have an explicit row in $MAPPING_FILE ✓"
