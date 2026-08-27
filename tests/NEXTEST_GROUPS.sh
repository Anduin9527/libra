#!/bin/sh
# NEXTEST_GROUPS.sh — mechanically derive .config/nextest.toml from
# tests/SERIAL_REGISTRY.tsv (plan-20260827 NP-01, ADR-NP-01).
#
# Group shape: one union group `external` (nextest test-groups are
# exclusive-membership — a test belongs to the first matching override
# only, so overlapping per-key groups cannot express compound rows).
# Members:
#   - every fn row whose lane key set contains cloud_live or
#     workspace_failpoints            -> filter = 'test(=<fn>)'
#   - every pure-global site row's host target (whole test binary)
#                                     -> filter = 'binary(=<target>)'
# cwd/env/hash_kind never generate groups (in-process locks dissolve
# under one-process-per-test).
#
# usage: sh tests/NEXTEST_GROUPS.sh            # rewrite .config/nextest.toml
#        sh tests/NEXTEST_GROUPS.sh --stdout   # print to stdout (drift check)
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
REG="$ROOT/tests/SERIAL_REGISTRY.tsv"
OUT="$ROOT/.config/nextest.toml"
[ -f "$REG" ] || { echo "FAIL: $REG missing" >&2; exit 2; }

TMP=$(mktemp)
trap 'rm -f "$TMP"' EXIT

LC_ALL=C awk -F'\t' '
NR == 1 { next }
$1 ~ /^<site:/ {
    if ($2 == "global") {
        path = $1
        sub(/^<site:/, "", path); sub(/:.*$/, "", path)
        t = path
        sub(/^tests\//, "", t); sub(/\.rs$/, "", t)
        if (t !~ /^[A-Za-z0-9_]+$/) { print "BADTARGET\t" t; exit 3 }
        targets[t] = 1
    }
    next
}
$2 ~ /(^lane:|[+])(cloud_live|workspace_failpoints)($|[+])/ {
    if ($1 !~ /^[A-Za-z0-9_]+$/) { print "BADFN\t" $1; exit 3 }
    fns[$1] = 1
}
END {
    for (f in fns)      print "F\t" f
    for (t in targets)  print "B\t" t
}
' "$REG" | LC_ALL=C sort > "$TMP"

if grep -q "^BAD" "$TMP"; then
    echo "FAIL: unexpected identifier in registry:" >&2
    grep "^BAD" "$TMP" >&2
    exit 2
fi

emit() {
    printf '%s\n' "# generated — do not edit"
    printf '%s\n' "# regenerate: sh tests/NEXTEST_GROUPS.sh  (source of truth: tests/SERIAL_REGISTRY.tsv)"
    printf '%s\n' "# plan-20260827 NP-01 / ADR-NP-01: union external-resource mutual-exclusion group."
    printf '\n%s\n%s\n' "[test-groups.external]" "max-threads = 1"
    LC_ALL=C sort "$TMP" | while IFS="$(printf '\t')" read -r kind name; do
        printf '\n%s\n' "[[profile.default.overrides]]"
        if [ "$kind" = "F" ]; then
            printf "filter = 'test(=%s)'\n" "$name"
        else
            printf "filter = 'binary(=%s)'\n" "$name"
        fi
        printf '%s\n' "test-group = 'external'"
    done
}

if [ "${1:-}" = "--stdout" ]; then
    emit
else
    mkdir -p "$ROOT/.config"
    emit > "$OUT"
    fn_n=$(grep -c "^filter = 'test(=" "$OUT")
    bin_n=$(grep -c "^filter = 'binary(=" "$OUT")
    echo "wrote $OUT (external group: $fn_n test filters + $bin_n binary filters)"
fi
