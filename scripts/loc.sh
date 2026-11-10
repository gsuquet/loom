#!/bin/sh
# Report non-test Go LOC (non-blank, non-comment-only lines) per package and
# fail when the total exceeds the design budget (docs/design.md section 1).
set -eu
# Hard-coded on purpose: raising it is a design change (ADR 0004), not an env override.
BUDGET=4500
cd "$(dirname "$0")/.."

find . -name '*.go' ! -name '*_test.go' ! -path '*/testdata/*' ! -path './.*' -print |
  LC_ALL=C sort |
  xargs awk -v budget="$BUDGET" '
    FNR == 1 { pkg = FILENAME; sub(/^\.\//, "", pkg); sub(/\/[^\/]*$/, "", pkg); inblock = 0 }
    {
      line = $0; gsub(/^[ \t]+|[ \t]+$/, "", line)
      if (inblock) { if (line ~ /\*\//) inblock = 0; next }
      if (line == "" || line ~ /^\/\//) next
      if (line ~ /^\/\*/) { if (line !~ /\*\//) inblock = 1; next }
      loc[pkg]++; total++
    }
    END {
      for (p in loc) printf "%6d  %s\n", loc[p], p | "sort -k2"
      close("sort -k2")
      printf "%6d  total (budget %d)\n", total, budget
      if (total > budget) { printf "LOC budget exceeded by %d; raising the budget needs an ADR (see docs/adr/0004)\n", total - budget > "/dev/stderr"; exit 1 }
    }'
