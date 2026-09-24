#!/usr/bin/env bash
# plan-20260921 VG-09 CDN gate (in-repo, versioned — see the VG-09 write set).
#
# Verifies the *published* release objects for one tag, fail-closed:
#   1. the signed stable manifest verifies            (`libra upgrade --check`)
#   2. each of the four platform artifacts is reachable and matches the
#      manifest's `size` + `sha256` byte counts exactly
#   3. windows amd64: the suffixless object and its `.exe` twin are identical
#   4. the bucket-root installers pin this tag as their default version
#
# Usage: bash tests/harness/release_cdn_gate.sh <tag>      (e.g. v0.23.60)
#
# `LIBRA_CDN_BASE` redirects this script's own fetches (the manifest text, the
# four artifacts, the installers), and `LIBRA_BIN` selects the CLI binary. The
# signature check in step 1 (`libra upgrade --check`) deliberately uses that
# CLI's *configured* release channel, so it always validates the real signed
# manifest; pointing `LIBRA_CDN_BASE` at a mirror does not redirect it.
#
# Exit codes: 0 = all gates pass; 1 = a gate failed; 2 = usage error.
set -euo pipefail

V="${1:-}"
if [ -z "$V" ]; then
  echo "usage: release_cdn_gate.sh <tag> (e.g. v0.23.46)" >&2
  exit 2
fi
case "$V" in
  v[0-9]*.[0-9]*.[0-9]*) ;;
  *) echo "FAIL: tag must look like vX.Y.Z, got '$V'" >&2; exit 2 ;;
esac

BASE="${LIBRA_CDN_BASE:-https://download.libra.tools}"
MANIFEST_URL="$BASE/libra/releases/stable/manifest-v1.json"
VERSION="${V#v}"
RELEASES="$BASE/libra/releases/$V"
PLATFORMS="linux-amd64 linux-arm64 darwin-arm64 windows-amd64"
LIBRA_BIN="${LIBRA_BIN:-libra}"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

fail() { echo "FAIL: $*" >&2; exit 1; }

echo "== 1. signed stable manifest =="
curl -fsSL --connect-timeout 10 --max-time 60 "$MANIFEST_URL" -o "$WORK/manifest.json" \
  || fail "manifest unreachable: $MANIFEST_URL"
"$LIBRA_BIN" upgrade --check >"$WORK/upgrade-check.log" 2>&1 \
  || { cat "$WORK/upgrade-check.log" >&2; fail "libra upgrade --check rejected the release channel"; }
jq -e --arg v "$VERSION" '.version == $v' "$WORK/manifest.json" >/dev/null \
  || fail "the stable manifest does not name version $VERSION"
jq -e --arg v "$VERSION" '.artifacts | type == "array" and length == 4' "$WORK/manifest.json" >/dev/null \
  || fail "the manifest must carry exactly four artifacts"
echo "OK: manifest verifies and names $VERSION"

echo "== 2. four artifacts: URL, size, sha256 =="
for platform in $PLATFORMS; do
  url="$RELEASES/libra-$platform"
  curl -fsSL --connect-timeout 10 --max-time 300 "$url" -o "$WORK/libra-$platform" \
    || fail "artifact unreachable: $url"

  want_sha="$(jq -r --arg p "$platform" '.artifacts[] | select(.platform == $p) | .sha256' "$WORK/manifest.json")"
  want_size="$(jq -r --arg p "$platform" '.artifacts[] | select(.platform == $p) | .size' "$WORK/manifest.json")"
  [ -n "$want_sha" ] && [ "$want_sha" != "null" ] || fail "manifest has no sha256 for $platform"
  [ -n "$want_size" ] && [ "$want_size" != "null" ] || fail "manifest has no size for $platform"

  got_size="$(wc -c < "$WORK/libra-$platform" | tr -d '[:space:]')"
  [ "$got_size" = "$want_size" ] \
    || fail "$platform size mismatch: published $got_size, manifest $want_size"

  if command -v sha256sum >/dev/null 2>&1; then
    got_sha="$(sha256sum "$WORK/libra-$platform" | awk '{print $1}')"
  else
    got_sha="$(shasum -a 256 "$WORK/libra-$platform" | awk '{print $1}')"
  fi
  [ "$got_sha" = "$want_sha" ] \
    || fail "$platform sha256 mismatch: published $got_sha, manifest $want_sha"
  echo "OK: $platform size+sha256 match the signed manifest"
done

echo "== 3. windows amd64: suffixless object == .exe twin =="
curl -fsSL --connect-timeout 10 --max-time 300 "$RELEASES/libra-windows-amd64.exe" -o "$WORK/win.exe" \
  || fail "the windows .exe twin is unreachable: $RELEASES/libra-windows-amd64.exe"
cmp -s "$WORK/libra-windows-amd64" "$WORK/win.exe" \
  || fail "the windows .exe twin differs from the suffixless manifest object"
echo "OK: windows amd64 objects are byte-identical"

echo "== 4. installers pin this tag =="
curl -fsSL --connect-timeout 10 --max-time 60 "$BASE/install.sh" -o "$WORK/install.sh" \
  || fail "installer unreachable: $BASE/install.sh"
grep -Fq "DEFAULT_VERSION=\"$V\"" "$WORK/install.sh" \
  || fail "install.sh does not default to $V"
curl -fsSL --connect-timeout 10 --max-time 60 "$BASE/install.ps1" -o "$WORK/install.ps1" \
  || fail "installer unreachable: $BASE/install.ps1"
grep -Fq "\$DefaultVersion = \"$V\"" "$WORK/install.ps1" \
  || fail "install.ps1 does not default to $V"
echo "OK: installers default to $V"

echo "PASS: CDN gate for $V"
