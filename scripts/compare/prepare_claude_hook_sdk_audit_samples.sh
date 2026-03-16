#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
OUTPUT_DIR="${1:-/tmp/libra-hook-vs-sdk-audit}"
HOOK_TRANSCRIPT_PATH="${HOOK_TRANSCRIPT_PATH:-}"
HOOK_SESSION_STATE_PATH="${HOOK_SESSION_STATE_PATH:-$ROOT_DIR/.libra/sessions/debug1.json}"
SDK_FIXTURE_PATH="$ROOT_DIR/tests/data/ai/claude_managed_probe_like.json"
TMP_BUNDLE_DIR=""
HOOK_SESSION_STATE_COPIED=0

usage() {
    cat <<'USAGE'
Usage:
  scripts/compare/prepare_claude_hook_sdk_audit_samples.sh [output_dir]

Environment variables:
  HOOK_TRANSCRIPT_PATH     Optional absolute path to a Claude transcript JSONL for the hook sample.
  HOOK_SESSION_STATE_PATH  Optional session-state JSON to bundle with the hook sample.

Notes:
  - Default output_dir: /tmp/libra-hook-vs-sdk-audit
  - The SDK sample is generated from tests/data/ai/claude_managed_probe_like.json
  - The hook sample is copied from an existing local Claude transcript if one is found
USAGE
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
    usage
    exit 0
fi

find_default_hook_transcript() {
    local candidates=(
        "$HOME/.claude/projects/-Users-anduin9527-Project-libra-claude-e2e/008b2f64-38bc-4ec4-8a58-3c196b6906d8.jsonl"
    )

    local candidate=""
    for candidate in "${candidates[@]}"; do
        if [[ -f "$candidate" ]]; then
            printf '%s\n' "$candidate"
            return 0
        fi
    done

    candidate="$(find "$HOME/.claude/projects" -maxdepth 3 -type f -name '*.jsonl' 2>/dev/null \
        | rg 'claude-e2e|libra-claude-e2e|libra-ab-experiment' \
        | head -n 1 || true)"
    if [[ -n "$candidate" ]]; then
        printf '%s\n' "$candidate"
        return 0
    fi

    return 1
}

prepare_output_dir() {
    rm -rf "$OUTPUT_DIR"
    mkdir -p "$OUTPUT_DIR/sdk-managed" "$OUTPUT_DIR/transparent-hook"
}

prepare_tmp_bundle_dir() {
    TMP_BUNDLE_DIR="$(mktemp -d "${TMPDIR:-/tmp}/libra-hook-vs-sdk-audit.XXXXXX")"
    mkdir -p "$TMP_BUNDLE_DIR/src"
}

cleanup() {
    if [[ -n "$TMP_BUNDLE_DIR" && -d "$TMP_BUNDLE_DIR" ]]; then
        rm -rf "$TMP_BUNDLE_DIR"
    fi
}

generate_sdk_bundle() {
    cat >"$TMP_BUNDLE_DIR/Cargo.toml" <<'EOF'
[package]
name = "libra-managed-bundle-export"
version = "0.1.0"
edition = "2024"

[dependencies]
anyhow = "1"
libra = { path = "/Users/anduin9527/Project/libra" }
serde_json = "1"
EOF

    cat >"$TMP_BUNDLE_DIR/src/main.rs" <<EOF
use std::{fs, path::PathBuf};

use anyhow::{Context, Result};
use libra::internal::ai::hooks::providers::claude::managed::{
    ClaudeManagedArtifact, build_managed_audit_bundle,
};

fn main() -> Result<()> {
    let input = PathBuf::from(r#"$SDK_FIXTURE_PATH"#);
    let output = PathBuf::from(r#"$OUTPUT_DIR"#).join("sdk-managed/managed-audit-bundle.json");

    let content = fs::read_to_string(&input)
        .with_context(|| format!("failed to read '{}'", input.display()))?;
    let artifact: ClaudeManagedArtifact =
        serde_json::from_str(&content).context("failed to parse managed artifact fixture")?;
    let bundle = build_managed_audit_bundle(&artifact)
        .context("failed to build managed audit bundle from fixture")?;

    let serialized =
        serde_json::to_vec_pretty(&bundle).context("failed to serialize managed bundle")?;
    fs::write(&output, serialized)
        .with_context(|| format!("failed to write '{}'", output.display()))?;
    Ok(())
}
EOF

    cargo run --offline --manifest-path "$TMP_BUNDLE_DIR/Cargo.toml" >/dev/null
}

read_transcript_session_id() {
    python3 - "$1" <<'PY'
import json
import sys

with open(sys.argv[1]) as handle:
    for line in handle:
        line = line.strip()
        if not line:
            continue
        try:
            record = json.loads(line)
        except json.JSONDecodeError:
            continue
        session_id = record.get("sessionId")
        if isinstance(session_id, str) and session_id:
            print(session_id)
            break
PY
}

read_session_state_provider_session_id() {
    python3 - "$1" <<'PY'
import json
import sys

with open(sys.argv[1]) as handle:
    record = json.load(handle)

metadata = record.get("metadata")
provider_session_id = metadata.get("provider_session_id") if isinstance(metadata, dict) else None
if isinstance(provider_session_id, str) and provider_session_id:
    print(provider_session_id)
    raise SystemExit(0)

session_id = record.get("id")
if isinstance(session_id, str) and "__" in session_id:
    print(session_id.split("__", 1)[1])
PY
}

copy_hook_sample() {
    local transcript_path="$1"
    cp "$transcript_path" "$OUTPUT_DIR/transparent-hook/transcript.jsonl"

    if [[ -f "$HOOK_SESSION_STATE_PATH" ]]; then
        local transcript_session_id session_state_provider_session_id
        transcript_session_id="$(read_transcript_session_id "$transcript_path")"
        session_state_provider_session_id="$(read_session_state_provider_session_id "$HOOK_SESSION_STATE_PATH")"

        if [[ -n "$transcript_session_id" && -n "$session_state_provider_session_id" && "$transcript_session_id" == "$session_state_provider_session_id" ]]; then
            cp "$HOOK_SESSION_STATE_PATH" "$OUTPUT_DIR/transparent-hook/session-state.json"
            HOOK_SESSION_STATE_COPIED=1
        else
            printf 'skipping session-state sample: transcript session "%s" does not match session-state provider session "%s"\n' \
                "$transcript_session_id" \
                "${session_state_provider_session_id:-<missing>}" >&2
        fi
    fi
}

write_readme() {
    cat >"$OUTPUT_DIR/README.txt" <<EOF
Libra Claude Hook vs SDK audit sample pack
==========================================

Purpose
-------
This directory is for the audit workbench and exists to support one specific question:

  Is Claude Agent SDK a better foundation than the current Claude hook path
  for Libra's future agent object / field extraction design?

What is inside
--------------
sdk-managed/
  managed-audit-bundle.json
    A real Libra-generated managed audit bundle built from:
    ${SDK_FIXTURE_PATH}

transparent-hook/
  transcript.jsonl
    A local Claude transcript copied from:
    ${HOOK_TRANSCRIPT_PATH}
EOF

    if [[ "$HOOK_SESSION_STATE_COPIED" -eq 1 ]]; then
        cat >>"$OUTPUT_DIR/README.txt" <<EOF

  session-state.json
    Copied from:
    ${HOOK_SESSION_STATE_PATH}
    This file matched the transcript provider session id and is included as supporting evidence.
EOF
    else
        cat >>"$OUTPUT_DIR/README.txt" <<EOF

  session-state.json
    Not bundled.
    Reason:
    - no matching session-state was found for the copied transcript
    - the configured fallback was: ${HOOK_SESSION_STATE_PATH}
EOF
    fi

    cat >>"$OUTPUT_DIR/README.txt" <<EOF

What this sample pack can prove
-------------------------------
- The audit workbench can ingest both hook-style evidence and SDK managed bundle evidence.
- SDK currently provides a stronger structured evidence surface:
  - managed result
  - merged tool invocations
  - field provenance
  - ai_session-like bridge payload
- Hook-style evidence still depends more heavily on transcript / prompt-contract behavior.

What this sample pack cannot prove
----------------------------------
- It is NOT a same-task A/B experiment.
- It does NOT prove that SDK is already the correct mainline for Libra.
- It does NOT prove the final object-field map can be satisfied purely by SDK output.

How to use
----------
1. Open the audit workbench.
2. Drag the entire directory:
     ${OUTPUT_DIR}
3. Inspect:
   - completeness
   - managed draft extraction
   - tool evidence
   - field provenance
   - current/future field audit

Recommended interpretation
--------------------------
Treat this directory as an evidence-surface comparison starter.
For a real product decision, run the same task through:

- transparent hook path
- SDK managed path

Then compare which fields are:
- contract-only
- post-extractable
- resolver-computed
EOF
}

main() {
    trap cleanup EXIT

    if [[ ! -f "$SDK_FIXTURE_PATH" ]]; then
        echo "missing SDK fixture: $SDK_FIXTURE_PATH" >&2
        exit 1
    fi

    if [[ -z "$HOOK_TRANSCRIPT_PATH" ]]; then
        HOOK_TRANSCRIPT_PATH="$(find_default_hook_transcript || true)"
    fi

    if [[ -z "$HOOK_TRANSCRIPT_PATH" || ! -f "$HOOK_TRANSCRIPT_PATH" ]]; then
        echo "failed to find a local hook transcript; set HOOK_TRANSCRIPT_PATH=/abs/path/to/*.jsonl" >&2
        exit 1
    fi

    prepare_output_dir
    prepare_tmp_bundle_dir
    generate_sdk_bundle
    copy_hook_sample "$HOOK_TRANSCRIPT_PATH"
    write_readme

    printf 'prepared audit sample pack: %s\n' "$OUTPUT_DIR"
    printf '  sdk-managed: %s\n' "$OUTPUT_DIR/sdk-managed/managed-audit-bundle.json"
    printf '  transparent-hook: %s\n' "$OUTPUT_DIR/transparent-hook/transcript.jsonl"
}

main "$@"
