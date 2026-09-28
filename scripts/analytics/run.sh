#!/usr/bin/env bash
# Code analytics without a UI: size, complexity, module cycles, fan-in/fan-out, types
# that hold or do too much, hotspots and change coupling. Writes one text file per
# analysis to target/analytics/ and prints them. See docs/code-analytics.md.
#
#   scripts/analytics/run.sh [--since 2026-01-01]
#
# Needs: tokei, rust-code-analysis-cli, cargo-modules (cargo install --locked …),
# java (code-maat is downloaded on first use), python3.
set -uo pipefail

ROOT=$(git rev-parse --show-toplevel)
OUT="$ROOT/target/analytics"
HERE="$ROOT/scripts/analytics"
SINCE=2000-01-01
[ "${1:-}" = "--since" ] && SINCE=$2
CODE_MAAT_VERSION=1.0.4
CODE_MAAT="${CODE_MAAT_JAR:-$OUT/code-maat-$CODE_MAAT_VERSION.jar}"
mkdir -p "$OUT"
cd "$ROOT"

missing=()
for tool in tokei rust-code-analysis-cli cargo-modules java python3; do
  command -v "$tool" >/dev/null || missing+=("$tool")
done
if [ ${#missing[@]} -gt 0 ]; then
  echo "missing: ${missing[*]} (cargo install --locked tokei rust-code-analysis-cli cargo-modules)" >&2
  exit 1
fi

section() { printf '\n=== %s\n' "$1"; }

section "Size per crate (tokei)"
for crate in crates/*/; do
  printf '%-10s ' "$(basename "$crate")"
  tokei "$crate" -t Rust -o json | python3 -c '
import json, sys
r = json.load(sys.stdin)["Rust"]
print("{:6} code {:5} comments {:3} files".format(r["code"], r["comments"], len(r["reports"])))'
done | tee "$OUT/size.txt"

section "Complexity (rust-code-analysis)"
rm -rf "$OUT/rca" && mkdir -p "$OUT/rca"
rust-code-analysis-cli -m -O json -p crates -o "$OUT/rca" >/dev/null 2>&1
find "$OUT/rca" -path "*/tests/*" -delete
python3 "$HERE/complexity.py" "$OUT/rca" "$ROOT" 15 | tee "$OUT/complexity.txt"

section "Cycles between modules (cargo-modules)"
: > "$OUT/cycles.txt"
for manifest in crates/*/Cargo.toml; do
  pkg=$(sed -n 's/^name *= *"\(.*\)"/\1/p' "$manifest" | head -1)
  target=--lib
  [ -f "$(dirname "$manifest")/src/main.rs" ] && target="--bin $pkg"
  # Only modules and their "uses" edges: --acyclic would also count a type and its
  # own methods as a cycle. The output only appears when piped.
  result=$(cargo modules dependencies -p "$pkg" $target --no-externs --no-sysroot \
    --no-fns --no-types --no-traits --no-owns 2>/dev/null | cat | python3 "$HERE/cycles.py")
  printf '%-16s %s\n' "$pkg" "$result" | tee -a "$OUT/cycles.txt"
done

section "Fan-in / fan-out per module"
python3 "$HERE/fan.py" "$ROOT" | tee "$OUT/fan.txt"

section "Fields per struct, methods per type"
python3 "$HERE/shapes.py" "$ROOT" 15 | tee "$OUT/shapes.txt"

section "History (code-maat, since $SINCE)"
if [ ! -f "$CODE_MAAT" ]; then
  curl -sSL -o "$CODE_MAAT" \
    "https://github.com/adamtornhill/code-maat/releases/download/v$CODE_MAAT_VERSION/code-maat-$CODE_MAAT_VERSION-standalone.jar"
fi
git log --numstat --date=short --pretty=format:'--%h--%ad--%aN' --no-renames \
  --after="$SINCE" -- crates > "$OUT/git.log"
maat() { java -jar "$CODE_MAAT" -l "$OUT/git.log" -c git2 "$@" 2>/dev/null; }
maat -a summary | tee "$OUT/summary.csv"
echo
echo "Hotspots (changes × lines)"
maat -a revisions | python3 "$HERE/hotspots.py" "$ROOT" 15 | tee "$OUT/hotspots.txt"
echo
echo "Change coupling (files changed together; degree = % of their commits)"
# Files that still exist only (split or deleted ones stay in the history).
maat -a coupling --min-revs 4 --min-coupling 50 \
  | python3 -c '
import csv, os, sys
rows = list(csv.reader(sys.stdin))
print(",".join(rows[0]))
for r in [r for r in rows[1:] if os.path.exists(r[0]) and os.path.exists(r[1])][:20]:
    print(",".join(r))' \
  | column -t -s, | tee "$OUT/coupling.txt"

printf '\nAll results: %s\n' "$OUT"
