#!/usr/bin/env bash
# plan-20260921 VG-09 release runbook (operator-facing).
#
#   bash release-runbook.sh preflight          # local-only checks (safe, no remote writes)
#   bash release-runbook.sh release <tag> --i-authorize-remote-writes
#
# `preflight` never touches the network: it re-runs every check that the plan's
# release script defers to release time, so the remote sequence cannot fail on a
# locally detectable problem.
#
# `release` first runs `preflight`, then performs the plan's remote sequence
# verbatim (push main → annotated tag → push tag → gh release create
# --verify-tag → gh run watch/view assertions → CDN gate). It refuses to run
# without the explicit authorization flag.
set -euo pipefail

REPO="$(cd "$(dirname "$0")/../.." && pwd)"   # tests/harness/<script> -> repo root
CDN_GATE="$REPO/tests/harness/release_cdn_gate.sh"
fail() { echo "FAIL: $*" >&2; exit 1; }

preflight() {
  echo "== version surface: Cargo.toml / install.sh / install.ps1 =="
  local cargo_v sh_v ps1_v
  cargo_v="$(grep -m1 '^version' "$REPO/Cargo.toml" | sed -E 's/.*"([^"]+)".*/\1/')"
  sh_v="$(grep -m1 -oE 'DEFAULT_VERSION="[^"]+"' "$REPO/install.sh" | sed -E 's/.*"([^"]+)"/\1/')"
  ps1_v="$(grep -m1 -oE '\$DefaultVersion = "[^"]+"' "$REPO/install.ps1" | sed -E 's/.*"([^"]+)"/\1/')"
  echo "  Cargo.toml=$cargo_v install.sh=${sh_v:-<none>} install.ps1=${ps1_v:-<none>}"
  [ -n "$cargo_v" ] || fail "Cargo.toml version unreadable"
  # The installers may legitimately pin the previous release until this one is
  # published; the *guard test* `compat_version_surface_sync` is authoritative.
  echo "  (authoritative check: cargo nextest run --test compat_version_surface_sync)"

  echo "== Cargo.lock agrees with the manifest version =="
  local lock_v
  lock_v="$(awk '/^name = "libra"$/ {getline; if ($1=="version") {gsub(/"/,"",$3); print $3; exit}}' "$REPO/Cargo.lock")"
  [ -n "$lock_v" ] || fail "Cargo.lock has no 'libra' package version (run any cargo command to refresh it)"
  [ "$lock_v" = "$cargo_v" ] \
    || fail "Cargo.lock pins libra $lock_v but Cargo.toml says $cargo_v - run a cargo command and commit the lock"

  echo "== the intended tag must be unreleased upstream =="
  # `LIBRA_RELEASE_TAG` lets the collision/ordering guard itself be exercised
  # (e.g. LIBRA_RELEASE_TAG=v0.23.58 must fail: that tag is already published).
  local TAG="${LIBRA_RELEASE_TAG:-v$cargo_v}" LATEST
  if ! REMOTE_TAGS="$(timeout 90 libra ls-remote --tags origin 2>/dev/null)"; then
    echo "  WARNING: could not reach origin; skipping the upstream tag check"
  elif [ -z "$REMOTE_TAGS" ]; then
    echo "  WARNING: origin returned no tags; skipping the upstream tag check"
  else
    LATEST="$(printf '%s\n' "$REMOTE_TAGS" | awk '{print $2}' | sed 's#refs/tags/##; s/\^{}$//' \
      | grep -E '^v[0-9]+\.[0-9]+\.[0-9]+$' | sort -u -V | tail -1)"
    # Upstream releases move quickly, so a failed check should say what to use next.
    local NEXT=""
    if [ -n "$LATEST" ]; then
      NEXT="v$(printf '%s' "${LATEST#v}" | awk -F. '{printf "%d.%d.%d", $1, $2, $3+1}')"
    fi
    printf '%s\n' "$REMOTE_TAGS" | awk '{print $2}' | sed 's#refs/tags/##; s/\^{}$//' \
      | grep -qx "$TAG" && fail "tag $TAG already exists upstream - bump the three version surfaces${NEXT:+ (next free: $NEXT)}"
    if [ -n "$LATEST" ] && [ "$(printf '%s\n%s\n' "$LATEST" "$TAG" | sort -V | tail -1)" != "$TAG" ]; then
      fail "$TAG is behind the latest published tag $LATEST - bump the three version surfaces${NEXT:+ (next free: $NEXT)}"
    fi
    echo "  $TAG is unreleased; latest published tag: ${LATEST:-unknown}"
  fi

  echo "== CHANGELOG carries the version section =="
  grep -q "^## \[$cargo_v\]" "$REPO/CHANGELOG.md" \
    || fail "CHANGELOG.md has no '## [$cargo_v]' section (the release-notes assertion would fail)"

  echo "== CDN gate harness is present, syntax-clean and fail-closed =="
  [ -f "$CDN_GATE" ] || fail "missing $CDN_GATE"
  bash -n "$CDN_GATE" || fail "CDN gate syntax error"
  if LIBRA_CDN_BASE=https://127.0.0.1:1 timeout 30 bash "$CDN_GATE" v9.9.9 >/dev/null 2>&1; then
    fail "the CDN gate passed against an unreachable CDN — it must fail closed"
  fi

  echo "== remote divergence (informational) =="
  ( cd "$REPO" && libra status --short --branch | head -1 )

  echo "PREFLIGHT: OK"
}

release() {
  local tag="${1:-}"
  local ack="${2:-}"
  [ -n "$tag" ] || fail "usage: release-runbook.sh release <tag> --i-authorize-remote-writes"
  [ "$ack" = "--i-authorize-remote-writes" ] \
    || fail "refusing to rewrite the real remote without --i-authorize-remote-writes"
  preflight
  cd "$REPO"
  local V="${tag#v}" SHA
  SHA="$(libra rev-parse HEAD | tail -1)"
  echo "== releasing $tag at $SHA =="
  libra push origin main
  local REMOTE_MAIN
  REMOTE_MAIN="$(libra ls-remote origin refs/heads/main | awk '{print $1}')"
  [ "$REMOTE_MAIN" = "$SHA" ] || fail "remote main ($REMOTE_MAIN) != intended SHA ($SHA)"
  libra tag -a "$tag" -m "Libra $tag"
  libra push origin "$tag"
  local TAG_SHA
  TAG_SHA="$(libra rev-parse "refs/tags/$tag^{}")"
  [ "$TAG_SHA" = "$SHA" ] || fail "tag deref ($TAG_SHA) != intended SHA ($SHA)"
  mkdir -p /tmp/issue-vg/vg09
  { printf 'Libra %s\n\n' "$tag"; sed -n "/^## \\[$V\\]/,/^## \\[/p" CHANGELOG.md | sed '$d'; } \
    >/tmp/issue-vg/vg09/release-notes.md
  grep -q "^## \[$V\]" /tmp/issue-vg/vg09/release-notes.md || fail "version section missing in release notes"
  gh release create "$tag" -R libra-tools/libra --verify-tag --notes-file /tmp/issue-vg/vg09/release-notes.md
  # The tag push registers its release run asynchronously, so poll briefly
  # instead of failing on the first empty listing.
  local RID="" attempt
  for attempt in $(seq 1 24); do
    RID="$(gh run list -R libra-tools/libra --workflow release.yml --limit 10 \
      --json databaseId,headBranch,event \
      -q ".[] | select(.event==\"push\" and .headBranch==\"$tag\") | .databaseId" | head -1)"
    [ -n "$RID" ] && break
    sleep 5
  done
  [ -n "$RID" ] || fail "no matching release run"
  # A failed job must reach the run.json assertions below (they distinguish a
  # Homebrew-only failure), so `set -e` must not abort on the watch's status.
  gh run watch "$RID" -R libra-tools/libra --exit-status || true
  gh run view "$RID" -R libra-tools/libra \
    --json status,conclusion,jobs,event,headBranch,headSha >/tmp/issue-vg/vg09/run.json
  jq -e --arg v "$V" --arg sha "$SHA" \
    '.status=="completed" and .conclusion=="success" and .event=="push" and .headBranch==("v"+$v) and .headSha==$sha' \
    /tmp/issue-vg/vg09/run.json >/dev/null || {
      # Distinguish a tap-only failure from a real artifact failure. The tap job
      # degrades to a warning (exit 0) when its token or the sha256 artifacts are
      # missing, but verify-homebrew-formula then cannot find the
      # formula-commit-sha artifact and fails -- reddening the whole run even
      # though the four platform artifacts and the CDN are fine.
      local failed_jobs failed_jobs_csv
      failed_jobs="$(jq -r '[.jobs[] | select(.conclusion != "success") | .name] | join(", ")' /tmp/issue-vg/vg09/run.json)"
      # Sorted, comma-separated without spaces: the `case` below matches the two
      # known Homebrew jobs by exact list, so the separator must be stable.
      failed_jobs_csv="$(jq -r '[.jobs[] | select(.conclusion != "success") | .name] | sort | join(",")' /tmp/issue-vg/vg09/run.json)"
      case ",$failed_jobs_csv," in
        ",update-homebrew-tap,"|",verify-homebrew-formula,"|",update-homebrew-tap,verify-homebrew-formula,")
          echo "  note: the run failed only in the Homebrew jobs: $failed_jobs" >&2
          if [ "${LIBRA_ALLOW_TAP_ONLY_FAILURE:-0}" != "1" ]; then
            fail "a Homebrew-only failure still fails the run; inspect the tap token, then re-run with LIBRA_ALLOW_TAP_ONLY_FAILURE=1 to accept it (the CDN gate still verifies the published artifacts)"
          fi
          echo "  accepted: Homebrew-only failure (LIBRA_ALLOW_TAP_ONLY_FAILURE=1)" >&2
          jq -e --arg v "$V" --arg sha "$SHA" \
            '.status=="completed" and .event=="push" and .headBranch==("v"+$v) and .headSha==$sha' \
            /tmp/issue-vg/vg09/run.json >/dev/null || fail "run fields mismatch beyond conclusion"
          ;;
        *)
          fail "release run failed in: ${failed_jobs:-<unknown>}"
          ;;
      esac
    }
  # The documented eight-job set: four platform builds plus the four
  # release-side jobs. A renamed, dropped or extra job fails this gate, so a
  # silently skipped platform cannot ship as a green release.
  local EXPECTED_JOBS='["build-and-upload (libra, aarch64-apple-darwin, darwin, arm64, macos-latest)","build-and-upload (libra, aarch64-unknown-linux-gnu, linux, arm64, ubuntu-24.04-arm)","build-and-upload (libra, x86_64-pc-windows-msvc, windows, amd64, windows-latest)","build-and-upload (libra, x86_64-unknown-linux-gnu, linux, amd64, ubuntu-latest)","upload-install-scripts","update-homebrew-tap","verify-homebrew-formula","request-stable-manifest"]'
  jq -e --argjson exp "$EXPECTED_JOBS" \
    '[.jobs[].name] | sort == ($exp | sort)' /tmp/issue-vg/vg09/run.json >/dev/null \
    || fail "release run job set mismatch (expected the eight documented jobs)"
  jq -r '[.jobs[].name] | sort | .[]' /tmp/issue-vg/vg09/run.json >/tmp/issue-vg/vg09/jobs.txt
  bash "$CDN_GATE" "$tag" | tee /tmp/issue-vg/vg09/cdn.log
  # The release workflow's Homebrew job degrades to a warning (exit 0) when its
  # token, the sha256 artifacts or the tap clone are missing, so a green run does
  # not prove the formula moved. Report the tap state explicitly instead.
  local TAP_HEAD
  TAP_HEAD="$(gh api repos/libra-tools/homebrew-libra/commits --jq '.[0].commit.message' 2>/dev/null | head -1)"
  if printf '%s' "$TAP_HEAD" | grep -q "$V"; then
    echo "  homebrew tap: updated (${TAP_HEAD})"
  else
    echo "  homebrew tap: WARNING - no commit mentioning $V yet (latest: ${TAP_HEAD:-unavailable})" >&2
  fi
  echo "RELEASE: complete for $tag"
}

# Bump the three version surfaces (and the CHANGELOG heading) to the first free
# patch version above the latest published tag. Upstream releases move fast, so
# the version is chosen at release time rather than weeks in advance.
bump() {
  local dry="${1:-}"
  local latest next cur
  # NOTE: with `set -euo pipefail` a failing pipeline inside a plain assignment
  # aborts the function *silently*; guard it so the caller gets a reason.
  if ! latest="$(timeout 90 libra ls-remote --tags origin 2>/dev/null | awk '{print $2}' \
      | sed 's#refs/tags/##; s/\^{}$//' | grep -E '^v[0-9]+\.[0-9]+\.[0-9]+$' | sort -u -V | tail -1)"; then
    fail "could not read origin's tags (is the remote reachable?)"
  fi
  [ -n "$latest" ] || fail "origin advertises no vX.Y.Z tags; refusing to guess the next version"
  next="$(printf '%s' "${latest#v}" | awk -F. '{printf "%d.%d.%d", $1, $2, $3+1}')"
  cur="$(grep -m1 '^version' "$REPO/Cargo.toml" | sed -E 's/.*"([^"]+)".*/\1/')"
  echo "latest published: $latest   current: $cur   next free: $next"
  # Idempotent and corrective: every surface is written to `next` regardless of
  # its own current value, so a half-bumped tree converges instead of drifting
  # (a previous run's partial edit, or a surface bumped by hand, both heal).
  local sh_v ps1_v head_v
  sh_v="$(grep -m1 -oE 'DEFAULT_VERSION="v[0-9.]+"' "$REPO/install.sh" | sed -E 's/.*"v([0-9.]+)"/\1/')"
  ps1_v="$(grep -m1 -oE '\$DefaultVersion = "v[0-9.]+"' "$REPO/install.ps1" | sed -E 's/.*"v([0-9.]+)"/\1/')"
  head_v="$(grep -m1 -oE '^## \[[0-9.]+\]' "$REPO/CHANGELOG.md" | sed -E 's/^## \[([0-9.]+)\]/\1/')"
  if [ "$cur" = "$next" ] && [ "$sh_v" = "$next" ] && [ "$ps1_v" = "$next" ] && [ "$head_v" = "$next" ]; then
    echo "nothing to do: all four surfaces already carry $next"
    return 0
  fi
  echo "surfaces: Cargo.toml=$cur install.sh=v${sh_v:-?} install.ps1=v${ps1_v:-?} CHANGELOG=[${head_v:-?}] -> target $next"
  if [ "$dry" = "--dry-run" ]; then
    echo "would set Cargo.toml/install.sh/install.ps1 and the CHANGELOG heading to $next"
    return 0
  fi
  sed -i.bak -E "s/^version = \"[0-9.]+\"/version = \"$next\"/" "$REPO/Cargo.toml" && rm -f "$REPO"/Cargo.toml.bak
  sed -i.bak -E "s/DEFAULT_VERSION=\"v[0-9.]+\"/DEFAULT_VERSION=\"v$next\"/" "$REPO/install.sh" && rm -f "$REPO"/install.sh.bak
  sed -i.bak -E 's/^(\$DefaultVersion = )"v[0-9.]+"/\1"v'"$next"'"/' "$REPO/install.ps1" && rm -f "$REPO"/install.ps1.bak
  # Only the topmost heading is the release section; every older `## [x.y.z]`
  # heading is history and must keep its version (sed would rewrite them all).
  awk -v next_v="$next" '!done && /^## \[[0-9.]+\]/ { sub(/^## \[[0-9.]+\]/, "## [" next_v "]"); done = 1 } { print }' \
    "$REPO/CHANGELOG.md" >"$REPO/CHANGELOG.md.tmp" && mv "$REPO/CHANGELOG.md.tmp" "$REPO/CHANGELOG.md"
  echo "bumped to $next:"
  grep -m1 '^version' "$REPO/Cargo.toml"
  grep -m1 'DEFAULT_VERSION=' "$REPO/install.sh"
  grep -m1 'DefaultVersion = ' "$REPO/install.ps1"
  grep -m1 "^## \[" "$REPO/CHANGELOG.md"
  echo "next: run \`cargo nextest run --test compat_version_surface_sync\`, commit all four files, then run this script's preflight"
}

case "${1:-}" in
  preflight) preflight ;;
  release) shift; release "${1:-}" "${2:-}" ;;
  bump) bump "${2:-}" ;;
  *) echo "usage: release-runbook.sh preflight | bump [--dry-run] | release <tag> --i-authorize-remote-writes" >&2; exit 2 ;;
esac
