#!/bin/sh
# TA-03 (plan-20260825): mechanically restore test parallelism from the
# FROZEN manifest. This converter consumes exactly two inputs —
#   1. tests/SERIAL_MANIFEST.tsv   (the frozen verdicts; never regenerated
#      here — a stale manifest is exit 2, not a silent re-freeze)
#   2. the classifier's machine-readable SITE MAP (same lexer, same
#      attribution contract; SERIAL_CLASSIFY_SITES_FILE channel)
# and performs three rewrites, nothing else:
#   none          -> the unkeyed #[serial] attribute is REMOVED
#   lane:<k>(+..) -> #[serial(<k>, ..)]  (named keys, compound preserved)
#   global        -> #[serial(<FULL key set>)] where the full set is
#                    {env, hash_kind, cwd} UNION every named key appearing
#                    in the manifest — derived mechanically, never written
#                    out by hand (an unkeyed attribute locks only the
#                    empty-string key, so `global` must expand to the whole
#                    resource universe or exclusion strength is LOST).
# Idempotent: converted attributes map back to their own text, so a second
# run writes nothing.
#
# HISTORICAL INPUT (DEFER-NP-02, 2026-09-17): the frozen manifest records
# the 2026-08-28 conversion verdicts, in which fail-closed bodies carry the
# full key universe (`lane:cloud_live+cwd+env+hash_kind+workspace_failpoints`).
# Those attributes have since been narrowed in the tree to the in-process
# closed set `#[serial(cwd, env, hash_kind)]`, and tests/SERIAL_REGISTRY.tsv
# (guarded against the live classifier) is the source of truth. Re-running
# this converter is NOT a way to "restore" the wide form: the manifest is
# stale against today's tree, so the run refuses with exit 2 — and it
# validates every site key BEFORE touching any file, so a stale manifest can
# never leave the tree half-rewritten.
ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT" || { echo "FAIL: cannot reach the repository root" >&2; exit 2; }
[ -f tests/SERIAL_MANIFEST.tsv ] || { echo "FAIL: tests/SERIAL_MANIFEST.tsv missing (freeze it first)" >&2; exit 2; }
SITES=$(mktemp)
SERIAL_CLASSIFY_SITES_FILE="$SITES" sh tests/SERIAL_CLASSIFY.sh > /dev/null || {
  echo "FAIL: classifier failed while emitting the site map" >&2; rm -f "$SITES"; exit 2; }
SITES="$SITES" python3 - <<'CONVERT_PY'
import io, os, sys

manifest = {}
named_keys = set()
for ln in io.open('tests/SERIAL_MANIFEST.tsv', encoding='utf-8'):
    ln = ln.rstrip('\n')
    if not ln:
        continue
    key, verdict = ln.split('\t')[:2]
    manifest[key] = verdict
    if verdict.startswith('lane:'):
        named_keys.update(verdict[5:].split('+'))
FULL_SET = sorted({'env', 'hash_kind', 'cwd'} | named_keys)

sites = {}
for ln in io.open(os.environ['SITES'], encoding='utf-8'):
    key, path, sl, sc, el, ec = ln.rstrip('\n').split('\t')
    sites.setdefault(path, []).append(
        (key, int(sl), int(sc), int(el), int(ec)))

# Fail closed BEFORE the first write: every site key must be adjudicated by
# the frozen manifest, otherwise a stale manifest would rewrite the files
# sorted before the first unknown key and then abort, leaving the tree
# half-converted.
missing = sorted(
    (path, key)
    for path, rows in sites.items()
    for key, _sl, _sc, _el, _ec in rows
    if key not in manifest)
if missing:
    for path, key in missing:
        print('FAIL: %s names %s, absent from the frozen manifest — '
              'the tree drifted; investigate before converting'
              % (path, key), file=sys.stderr)
    print('FAIL: %d site key(s) absent from the frozen manifest; no file was '
          'written' % len(missing), file=sys.stderr)
    sys.exit(2)

changed = 0
for path, rows in sorted(sites.items()):
    lines = io.open(path, encoding='utf-8').read().split('\n')
    dirty = False
    # bottom-up so earlier spans stay valid
    for key, sl, sc, el, ec in sorted(rows, key=lambda r: (-r[1], -r[2])):
        verdict = manifest[key]
        span = lines[sl][sc:ec] if sl == el else (
            lines[sl][sc:] + '\n'
            + '\n'.join(lines[sl + 1:el])
            + ('\n' if el > sl + 1 else '')
            + lines[el][:ec])
        head = span.split('(', 1)[0].split(']', 1)[0]  # e.g. `#[serial`
        if verdict == 'none':
            new = ''
        else:
            keys = (FULL_SET if verdict == 'global'
                    else verdict[5:].split('+'))
            new = head + '(' + ', '.join(keys) + ')]'
        if new == span:
            continue
        if sl == el:
            merged = (lines[sl][:sc] + new + lines[sl][ec:]).rstrip()
        else:
            merged = (lines[sl][:sc] + new + lines[el][ec:]).rstrip()
            del lines[sl + 1:el + 1]
        if merged.strip() == '':
            del lines[sl]
        else:
            lines[sl] = merged
        dirty = True
    if dirty:
        io.open(path, 'w', encoding='utf-8').write('\n'.join(lines))
        changed += 1
print('converted %d file(s); full key set = %s'
      % (changed, ', '.join(FULL_SET)))
CONVERT_PY
rc=$?
rm -f "$SITES"
exit $rc
