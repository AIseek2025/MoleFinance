#!/usr/bin/env bash
# verify-security-references.sh — wave 13 governance gate.
#
# SECURITY.md cites every invariant against a concrete test (or a
# focused glob like `tests::*`). This script extracts every backtick-
# wrapped reference of shape:
#
#     `<file_path>::<rust_path>::<symbol>`
#
# and asserts:
#   1. `<file_path>` exists in the working tree
#   2. `<symbol>` (or any matching glob) is defined in `<file_path>`
#      via a `fn <symbol>` line
#
# Any broken reference is a CI failure. The intent is that
# SECURITY.md cannot rot independently from the code: a renamed test
# function trips this check on the next PR.

set -euo pipefail

SECURITY_FILE="${SECURITY_FILE:-SECURITY.md}"

if [[ ! -f "$SECURITY_FILE" ]]; then
  echo "::error::$SECURITY_FILE not found" >&2
  exit 1
fi

# Match references with backticks, requiring at least one `::` and
# the path part starting with `crates/` or `programs/`. Discard the
# enclosing backticks via `grep -oP`.
#
# Examples we expect to capture:
#   `crates/foo/src/bar.rs::tests::baz_test`
#   `crates/foo/src/bar.rs::tests::*`
#   `programs/mole-option/src/handlers/x.rs::tests::y`
#
# We deliberately do NOT capture references like
# `crates/foo/Cargo.toml::package` — only `.rs` paths, the only
# symbol-bearing files we care about.
# Portable to bash 3.2 (macOS default): no `mapfile`, no array
# population from process substitution. Stream straight through a
# while-read loop instead.
refs_file="$(mktemp)"
trap 'rm -f "$refs_file"' EXIT

grep -oE '`[a-zA-Z0-9_./-]+\.rs::[a-zA-Z0-9_:*]+`' "$SECURITY_FILE" \
  | sed -E 's/^`(.*)`$/\1/' \
  | sort -u \
  > "$refs_file"

ref_count="$(wc -l < "$refs_file" | tr -d '[:space:]')"

if [[ "$ref_count" -eq 0 ]]; then
  echo "::error::no SECURITY.md test references matched the expected backticked-path pattern; either the format drifted or SECURITY.md is empty"
  exit 1
fi

echo "Verifying $ref_count unique SECURITY.md test references…"

broken=0
while IFS= read -r ref; do
  # Split on "::" — first segment is file path, rest is symbol path.
  file="${ref%%::*}"
  rest="${ref#*::}"

  if [[ ! -f "$file" ]]; then
    echo "::error::missing file: $file (referenced by SECURITY.md as \`$ref\`)"
    broken=$((broken + 1))
    continue
  fi

  # Last `::`-segment is the symbol. If it's `*`, accept *any* `fn`
  # in the file (the glob form is for blanket coverage like
  # `tests::*` meaning "every test in the module").
  symbol="${rest##*::}"

  if [[ "$symbol" == "*" ]]; then
    # Glob: require at least one `fn ` line in the file.
    if ! grep -qE '^\s*(pub\s+)?(async\s+)?fn\s+' "$file"; then
      echo "::error::glob ref \`$ref\` matches nothing — file has zero \`fn\` declarations"
      broken=$((broken + 1))
    fi
    continue
  fi

  # Concrete symbol: require `fn <symbol>` in the file. Anchor at the
  # start of a token to avoid matching `fn unused_fn_<symbol>` or
  # similar.
  if ! grep -qE "fn[[:space:]]+${symbol}\\b" "$file"; then
    echo "::error::missing symbol: \`fn $symbol\` not found in $file (SECURITY.md ref \`$ref\`)"
    broken=$((broken + 1))
    continue
  fi
done < "$refs_file"

if [[ "$broken" -gt 0 ]]; then
  echo "::error::$broken broken SECURITY.md test reference(s); update SECURITY.md or restore the test fn"
  exit 1
fi

echo "All $ref_count SECURITY.md test references resolve to live symbols ✓"
