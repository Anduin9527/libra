#!/usr/bin/env bash
# plan-20260921 ER-VG-07 — run the per-card allowlist guard `capture` for every
# card in one command.
#
#   bash tests/harness/vg07_guard_capture.sh --dry-run   # list what would run
#   bash tests/harness/vg07_guard_capture.sh             # run capture for all cards
#
# ER-VG-07 keeps a role split: `capture` (writes the supervisor manifest) and
# `reset` (consumes a permit, writes the ledger) are **supervisor-only**; a card
# executor may only run `bash "$DIR/guard.sh" verify`. This wrapper automates the
# supervisor's loop; it does not change who is allowed to run it.
#
# The materials live outside the repository (evidence dir): each card directory
# holds the verbatim `guard.sh`, a `guard-adapted.sh` whose ROOT points at this
# worktree, and the card's `allowlist.txt`.
#
# Exit codes: 0 = every capture succeeded; 1 = at least one failed; 2 = usage error.
set -euo pipefail

ROOT_DEFAULT="/run/media/genedna/data/libra"
VG_DIR="${VG_DIR:-/tmp/issue-vg}"
ROOT="${ROOT:-$ROOT_DEFAULT}"
OUT="$VG_DIR/guard-capture"
DRY=0

case "${1:-}" in
  --dry-run) DRY=1 ;;
  "") ;;
  *) echo "usage: vg07_guard_capture.sh [--dry-run]" >&2; exit 2 ;;
esac

[ -d "$ROOT/.libra" ] || { echo "FAIL: '$ROOT' is not a Libra worktree (no .libra)" >&2; exit 2; }

cards=()
for i in $(seq -w 1 14); do
  d="$VG_DIR/vg$i"
  [ -d "$d" ] || continue
  [ -f "$d/guard-adapted.sh" ] || { echo "FAIL: $d/guard-adapted.sh missing" >&2; exit 2; }
  [ -f "$d/allowlist.txt" ] || { echo "FAIL: $d/allowlist.txt missing" >&2; exit 2; }
  cards+=("vg$i")
done
[ "${#cards[@]}" -gt 0 ] || { echo "FAIL: no card directories under $VG_DIR" >&2; exit 2; }

echo "ROOT=$ROOT"
echo "cards=${#cards[@]}  (${cards[*]})"
if [ "$DRY" = "1" ]; then
  for c in "${cards[@]}"; do
    echo "  would run: bash $VG_DIR/$c/guard-adapted.sh capture   # writes $VG_DIR/guard-manifests/$c.json (supervisor-owned)"
  done
  echo "DRY RUN: no capture was executed"
  exit 0
fi

mkdir -p "$OUT"
failures=0
for c in "${cards[@]}"; do
  log="$OUT/$c.log"
  set +e
  ( cd "$ROOT" && ROOT="$ROOT" bash "$VG_DIR/$c/guard-adapted.sh" capture ) >"$log" 2>&1
  rc=$?
  set -e
  if [ "$rc" -eq 0 ]; then
    printf 'capture %-6s OK    (log: %s)\n' "$c" "$log"
  else
    printf 'capture %-6s FAIL  rc=%s (log: %s)\n' "$c" "$rc" "$log" >&2
    tail -5 "$log" >&2 || true
    failures=$((failures + 1))
  fi
done

{
  echo "cards=${#cards[@]}"
  echo "failures=$failures"
  date -u +'run_at=%Y-%m-%dT%H:%M:%SZ'
} >"$OUT/SUMMARY.txt"

if [ "$failures" -ne 0 ]; then
  echo "FAIL: $failures capture run(s) failed; see $OUT" >&2
  exit 1
fi
echo "OK: captured ${#cards[@]} cards; summary at $OUT/SUMMARY.txt"
