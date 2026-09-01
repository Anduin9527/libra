//! Auto-upgrade orchestration and startup hooks (plan-20260714 §A.7/§A.8/
//! §A.10).
//!
//! This module composes the verified pieces into the two things the CLI
//! calls at startup:
//!
//! - [`startup_recovery_gate`] — runs BEFORE repo preflight/dispatch and,
//!   if a crashed install transaction is present, drives it to a terminal
//!   state (§A.7). A fatal, unclassifiable transaction stops the process
//!   before any user command runs; a clean recovery or the (overwhelmingly
//!   common) no-transaction case returns quietly.
//! - [`run_auto_upgrade_check`] — the `upgrade.mode=auto` check that fetches
//!   the signed manifest, decides, downloads a candidate and probes +
//!   installs it under the §A.5 lock. Every failure is isolated so it can
//!   never break the user's actual command (§A.8).
//!
//! Both short-circuit to a no-op before any network or filesystem work when
//! the compiled trust table is empty. A build carrying the ceremony public
//! key still fails closed until a valid signed manifest is available.

use std::{path::PathBuf, time::Duration};

use super::{
    flow::{DecisionContext, UpgradeDecision, decide_from_envelope},
    http::{download_artifact_to, fetch_manifest, upgrade_http_client},
    lock::InstallDir,
    manifest::{MANIFEST_URL, ReleaseVersion},
    marker::{TARGET_BINARY_NAME, official_marker_for_target},
    platform::{Platform, PlatformSupport},
    probe,
    settings::{UpgradeMode, effective_mode_for_upgrade},
    state::{
        UpgradeState, backoff_defers, cooldown_permits_skip, merge_acceptance_floors, read_state,
        record_acceptance_floors, register_failure_backoff, write_state,
    },
    trusted_keys::active_trust_table,
    txn::{self, CANDIDATE_NAME, OldTarget, TxnError, TxnOutcome},
};
use crate::utils::error::{CliError, CliResult};

/// Total Phase-A soft budget: 5 s manifest + 10 s download (§A.7).
pub const UPGRADE_BUDGET: Duration = Duration::from_secs(15);
/// Per-probe hard timeout for the recovery/post-install self-check.
pub const RECOVERY_PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// What the auto-upgrade check did this invocation (for the CLI to surface).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutoUpgradeReport {
    /// Nothing applicable / not the right moment / inert.
    Skipped,
    /// A newer version was installed.
    Installed(ReleaseVersion),
    /// An install was attempted but rolled back to the previous target.
    RolledBack,
}

/// Resolved install context: the validated directory and the running
/// platform. `None` whenever this binary is not an upgrade-manageable
/// official-style install (unresolvable path, failed §A.5 validation, or a
/// platform outside the release matrix) — always a non-fatal skip.
struct InstallContext {
    dir: InstallDir,
    dir_path: PathBuf,
    platform: Platform,
}

fn resolve_install_context() -> Option<InstallContext> {
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    // The normal installed target is named `libra` (`libra.exe` on Windows,
    // so an official Windows install classifies as its platform's
    // unsupported-upgrade state rather than as "not official"); anything
    // else (a dev binary, a renamed copy) is not upgrade-manageable.
    let name = exe.file_name()?.to_str()?;
    let name_matches =
        name == TARGET_BINARY_NAME || (cfg!(windows) && name.eq_ignore_ascii_case("libra.exe"));
    if !name_matches {
        return None;
    }
    let dir_path = exe.parent()?.to_path_buf();
    let dir = InstallDir::open_validated(&dir_path).ok()?;
    let platform = Platform::current()?;
    Some(InstallContext {
        dir,
        dir_path,
        platform,
    })
}

/// Synchronous, bounded post-install self-check used during recovery (the
/// recovery path is not inside an obvious async context, so it spawns the
/// target with `std::process` and enforces its own timeout + group kill).
fn sync_post_install_probe(
    dir: &InstallDir,
    expected_version: &str,
    timeout: Duration,
) -> Result<bool, TxnError> {
    let target = dir.path().join(TARGET_BINARY_NAME);
    probe::run_sync_probe(&target, "post-install", expected_version, timeout)
        .map(|o| o.is_healthy())
        .map_err(|e| TxnError::Serde(format!("recovery probe failed to spawn: {e}")))
}

/// Startup recovery gate (§A.7/§A.10). Must run before repo preflight and
/// user-command dispatch.
///
/// - No install context / no transaction ⇒ `Ok(())` (the common case).
/// - A clean recovery (commit / rollback / abort) ⇒ `Ok(())`, with an
///   advisory note on rollback.
/// - A fatal, unclassifiable transaction or corrupt anti-rollback state ⇒
///   `Err`, so the process exits before running the user's command.
pub async fn startup_recovery_gate() -> CliResult<()> {
    // Inert until keys exist: with no trust table there can be no official
    // signed install, hence no upgrade transaction to recover.
    if active_trust_table().is_empty() {
        return Ok(());
    }
    let Some(ctx) = resolve_install_context() else {
        return Ok(());
    };
    let version = env!("CARGO_PKG_VERSION").to_string();
    let outcome = tokio::task::spawn_blocking(move || {
        // Recovery serializes with every state/transaction writer. Keep the
        // lock until the recovery path's final fsync.
        let _lock = ctx.dir.lock_blocking()?;
        // A corrupt state file is fatal for the upgrade subsystem (§A.7):
        // refuse to proceed rather than silently discard rollback history.
        read_state(&ctx.dir).map_err(|e| TxnError::State(e.to_string()))?;
        let probe =
            move |dir: &InstallDir| sync_post_install_probe(dir, &version, RECOVERY_PROBE_TIMEOUT);
        txn::recover(&ctx.dir, &probe)
    })
    .await
    .map_err(|e| CliError::fatal(format!("upgrade recovery task failed: {e}")))?;

    match outcome {
        Ok(TxnOutcome::RolledBack) => {
            crate::utils::error::emit_advisory_warning(
                "a previous auto-upgrade failed its self-check and was rolled back to the prior version",
            );
            Ok(())
        }
        Ok(_) => Ok(()),
        Err(TxnError::FatalRecovery { detail, .. }) => Err(CliError::fatal(format!(
            "auto-upgrade is in an unrecoverable state and must be repaired manually: {detail}"
        ))
        .with_stable_code(crate::utils::error::StableErrorCode::RepoStateInvalid)),
        Err(TxnError::State(detail)) => Err(CliError::fatal(format!(
            "auto-upgrade anti-rollback state is invalid and must be repaired manually: {detail}"
        ))
        .with_stable_code(crate::utils::error::StableErrorCode::RepoStateInvalid)),
        Err(other) => {
            // Non-fatal recovery hiccup: never block the user's command.
            crate::utils::error::emit_advisory_warning(format!(
                "auto-upgrade recovery could not complete this time: {other}"
            ));
            Ok(())
        }
    }
}

/// Pure throttle gate (§A.6 缓存/节流): should this invocation actually go
/// online to check, given the mode, whether keys exist, and the persisted
/// cooldown/backoff. Split out so it is unit-testable.
pub fn should_check_now(
    mode: UpgradeMode,
    trust_is_empty: bool,
    state: &UpgradeState,
    local_now: i64,
) -> bool {
    if mode != UpgradeMode::Auto || trust_is_empty {
        return false;
    }
    if backoff_defers(state, local_now) {
        return false;
    }
    !cooldown_permits_skip(state, local_now)
}

/// Run the `upgrade.mode=auto` check (§A.8). Never returns an error: every
/// failure degrades to [`AutoUpgradeReport::Skipped`] so the user's command
/// is unaffected.
pub async fn run_auto_upgrade_check(local_now: i64) -> AutoUpgradeReport {
    let trust = active_trust_table();
    // Fast, allocation-free short-circuits in the common case.
    if trust.is_empty() {
        return AutoUpgradeReport::Skipped;
    }
    if effective_mode_for_upgrade() != UpgradeMode::Auto {
        return AutoUpgradeReport::Skipped;
    }
    let Some(ctx) = resolve_install_context() else {
        return AutoUpgradeReport::Skipped;
    };
    if ctx.platform.support() != PlatformSupport::Supported {
        return AutoUpgradeReport::Skipped;
    }
    // An install we did not sign is not eligible for auto-upgrade (§A.2).
    if official_marker_for_target(&ctx.dir, ctx.platform.as_str())
        .ok()
        .flatten()
        .is_none()
    {
        return AutoUpgradeReport::Skipped;
    }
    let Ok(state) = read_state(&ctx.dir) else {
        return AutoUpgradeReport::Skipped;
    };
    if !should_check_now(UpgradeMode::Auto, false, &state, local_now) {
        return AutoUpgradeReport::Skipped;
    }

    // Phase A: fetch + decide + download, under the budget. Candidate staging
    // and probing happen under the lock in Phase B so concurrent checks
    // cannot exchange candidates or regress the accepted state. The witness
    // records the accepted new_state the moment verification succeeds, so a
    // later download failure or budget timeout cannot lose the floors the
    // manifest already proved.
    let acceptance_witness = std::sync::Arc::new(std::sync::Mutex::new(None::<UpgradeState>));
    match tokio::time::timeout(
        UPGRADE_BUDGET,
        phase_a(&ctx, &state, trust, local_now, &acceptance_witness),
    )
    .await
    {
        Ok(Ok(Some(plan))) => phase_b(&ctx, plan).await,
        Ok(Ok(None)) => AutoUpgradeReport::Skipped,
        Ok(Err(())) | Err(_) => {
            // Any failure or timeout: compare-and-write a backoff under the
            // upgrade lock. A concurrent accepted manifest must win rather
            // than being overwritten by this stale failure path; an accepted
            // manifest from THIS attempt still advances the floors.
            let accepted = acceptance_witness
                .lock()
                .map(|mut witness| witness.take())
                .unwrap_or(None);
            persist_failure_backoff(&ctx, state, accepted, local_now).await;
            AutoUpgradeReport::Skipped
        }
    }
}

/// The install plan plus a verified in-memory candidate. It is staged only
/// after Phase B obtains the upgrade lock.
struct StagedInstall {
    version: ReleaseVersion,
    marker: super::marker::InstallMarker,
    new_state: UpgradeState,
    expected_state: UpgradeState,
    candidate: Vec<u8>,
    local_now: i64,
}

/// Phase A: fetch the manifest, decide, and download+verify the candidate.
/// `Ok(Some(_))` means "ready to lock, stage and probe". The moment a
/// manifest is ACCEPTED, its new state is recorded in `acceptance_witness`
/// so every later failure path can still persist the advanced floors.
async fn phase_a(
    ctx: &InstallContext,
    state: &UpgradeState,
    trust: &[super::trusted_keys::TrustedKey],
    local_now: i64,
    acceptance_witness: &std::sync::Arc<std::sync::Mutex<Option<UpgradeState>>>,
) -> Result<Option<StagedInstall>, ()> {
    let client = upgrade_http_client().map_err(|_| ())?;
    let fetched = fetch_manifest(&client, MANIFEST_URL)
        .await
        .map_err(|_| ())?;
    let https_date = fetched
        .https_date
        .as_deref()
        .and_then(parse_http_date_to_unix);

    let installed = ReleaseVersion::parse(env!("CARGO_PKG_VERSION")).ok_or(())?;
    let ctx_dec = DecisionContext {
        state,
        https_date,
        local_now,
        trust,
        platform: Some(ctx.platform),
        installed_version: installed,
        installed_at_rfc3339: &now_rfc3339(local_now),
    };
    let decision = decide_from_envelope(&ctx_dec, &fetched.bytes).map_err(|_| ())?;
    let plan = match decision {
        UpgradeDecision::Install(plan) => plan,
        UpgradeDecision::Skip { new_state, .. } => {
            persist_accepted_skip_state(ctx, state.clone(), new_state).await;
            return Ok(None);
        }
    };
    if let Ok(mut witness) = acceptance_witness.lock() {
        *witness = Some(plan.new_state.clone());
    }

    // Download into memory (SizeGate-bounded to ≤128 MiB). Do not touch the
    // shared candidate filename until the Phase-B lock is held.
    let mut buf: Vec<u8> = Vec::new();
    download_artifact_to(
        &client,
        &plan.artifact.url,
        plan.artifact.size,
        &plan.artifact.sha256,
        &mut buf,
    )
    .await
    .map_err(|_| ())?;
    Ok(Some(StagedInstall {
        version: plan.version,
        marker: plan.marker,
        new_state: plan.new_state,
        expected_state: state.clone(),
        candidate: buf,
        local_now,
    }))
}

/// Persist a verified policy update that did not lead to an install (for
/// example, a pause or same-version manifest). The full state is written
/// only if the baseline read before the network request is still current
/// under the install-directory lock; when another process has made progress
/// (or holds the lock), the accepted state's monotone floors are still
/// merged so an accepted high-generation manifest is never forgotten.
async fn persist_accepted_skip_state(
    ctx: &InstallContext,
    expected_state: UpgradeState,
    accepted_state: UpgradeState,
) {
    let dir_path = ctx.dir_path.clone();
    let expected = expected_state.clone();
    let full_state = accepted_state.clone();
    let cas_applied = tokio::task::spawn_blocking(move || {
        let dir = InstallDir::open_validated(&dir_path).map_err(|_| ())?;
        persist_if_state_unchanged(&dir, &expected, &full_state)
    })
    .await;
    if !matches!(cas_applied, Ok(Ok(true))) {
        merge_acceptance_floors_with_retry(ctx.dir_path.clone(), accepted_state).await;
    }
}

/// The state to persist for a failed check attempt: failure backoff on top
/// of the pre-attempt baseline, plus — when this attempt DID accept a
/// manifest before failing (download error, budget timeout) — the accepted
/// snapshot's monotone floors (§A.6/§A.7 anti-rollback).
fn failure_backoff_state(
    expected_state: &UpgradeState,
    accepted: Option<&UpgradeState>,
    local_now: i64,
) -> UpgradeState {
    let base = match accepted {
        Some(accepted) => merge_acceptance_floors(expected_state, accepted),
        None => expected_state.clone(),
    };
    register_failure_backoff(&base, local_now)
}

/// Durably record an accepted manifest's monotone floors through the floors
/// SIDE FILE (`record_acceptance_floors`), which has its own micro-lock —
/// the MAIN upgrade lock is never taken here, so a busy or wedged install
/// lock cannot delay or starve it. Under normal concurrency the write
/// completes before this returns; only an externally-stalled micro-lock
/// holder can exhaust the bounded wait, in which case the error is silently
/// non-fatal to the user's command and the next successful check re-derives
/// the floors (see `record_acceptance_floors` for the exact contract).
async fn merge_acceptance_floors_with_retry(dir_path: std::path::PathBuf, accepted: UpgradeState) {
    let _ = tokio::task::spawn_blocking(move || {
        let dir = InstallDir::open_validated(&dir_path).map_err(|_| ())?;
        record_acceptance_floors(&dir, &accepted).map_err(|_| ())
    })
    .await;
}

/// Persist a failure backoff only when the state observed before Phase A is
/// still current. This prevents a late network timeout from clobbering a
/// newer generation/control floor accepted by another process. When the
/// baseline HAS moved, the accepted floors are still merged monotonically —
/// only the backoff itself is dropped as stale.
async fn persist_failure_backoff(
    ctx: &InstallContext,
    expected_state: UpgradeState,
    accepted: Option<UpgradeState>,
    local_now: i64,
) {
    let accepted_state = failure_backoff_state(&expected_state, accepted.as_ref(), local_now);
    let dir_path = ctx.dir_path.clone();
    let expected = expected_state.clone();
    let cas_applied = tokio::task::spawn_blocking(move || {
        let dir = InstallDir::open_validated(&dir_path).map_err(|_| ())?;
        persist_if_state_unchanged(&dir, &expected, &accepted_state)
    })
    .await;
    if let Some(accepted) = accepted
        && !matches!(cas_applied, Ok(Ok(true)))
    {
        merge_acceptance_floors_with_retry(ctx.dir_path.clone(), accepted).await;
    }
}

/// Lock-protected compare-and-write for accepted non-install decisions.
/// `false` means another process advanced the state or holds the lock; both
/// are safe skips. Errors are deliberately non-fatal to the caller's command.
fn persist_if_state_unchanged(
    dir: &InstallDir,
    expected_state: &UpgradeState,
    accepted_state: &UpgradeState,
) -> Result<bool, ()> {
    let Some(_lock) = dir.try_lock().map_err(|_| ())? else {
        return Ok(false);
    };
    let current = read_state(dir).map_err(|_| ())?;
    if &current != expected_state {
        return Ok(false);
    }
    write_state(dir, accepted_state).map_err(|_| ())?;
    Ok(true)
}

/// Phase B: compare the state baseline, stage+pre-probe the candidate, then
/// install it under the single §A.5 lock via the transaction.
async fn phase_b(ctx: &InstallContext, staged: StagedInstall) -> AutoUpgradeReport {
    let version = staged.version;
    let accepted_floors = staged.new_state.clone();
    match run_locked_install(ctx, staged).await {
        Ok(Ok(Some(TxnOutcome::Installed))) => AutoUpgradeReport::Installed(version),
        Ok(Ok(Some(TxnOutcome::RolledBack))) => AutoUpgradeReport::RolledBack,
        Ok(Ok(None)) => {
            // Lock busy: the install is skipped, but the verified floors are
            // still merged (with a short retry) so they cannot be lost.
            merge_acceptance_floors_with_retry(ctx.dir_path.clone(), accepted_floors).await;
            AutoUpgradeReport::Skipped
        }
        _ => AutoUpgradeReport::Skipped,
    }
}

/// The locked stage→probe→transact core shared by the auto and manual paths:
/// re-opens a dedicated InstallDir on a blocking task, takes the §A.5 lock
/// (`Ok(None)` = busy), CASes the state baseline, durably advances the
/// floors, stages the candidate, probes it and runs the install transaction.
/// The auto wrapper degrades every error to a silent skip; the manual path
/// surfaces them to the user.
async fn run_locked_install(
    ctx: &InstallContext,
    staged: StagedInstall,
) -> Result<Result<Option<TxnOutcome>, TxnError>, tokio::task::JoinError> {
    let version = staged.version;
    let dir_path = ctx.dir_path.clone();
    let platform = ctx.platform;
    tokio::task::spawn_blocking(move || {
        let dir = InstallDir::open_validated(&dir_path).map_err(TxnError::Dir)?;
        let Some(_lock) = dir.try_lock()? else {
            // Another process holds the lock — skip this round.
            return Ok(None);
        };
        // Fetch/download occurred outside the lock. Do not apply that plan
        // over state another process accepted while Phase A was in flight —
        // but DO merge the monotone floors this attempt proved.
        let current_state = read_state(&dir).map_err(|e| TxnError::State(e.to_string()))?;
        if current_state != staged.expected_state {
            let merged = merge_acceptance_floors(&current_state, &staged.new_state);
            if merged != current_state {
                write_state(&dir, &merged).map_err(|e| TxnError::State(e.to_string()))?;
            }
            return Ok(Some(TxnOutcome::NoOp));
        }
        // Durably advance the acceptance floors BEFORE staging or probing:
        // a crash anywhere in the window before the transaction journal
        // exists must not lose them (recovery would see no txn and NoOp).
        let floors = merge_acceptance_floors(&current_state, &staged.new_state);
        write_state(&dir, &floors).map_err(|e| TxnError::State(e.to_string()))?;
        dir.write_file_atomic(CANDIDATE_NAME, &staged.candidate, 0o755)?;
        let candidate_path = dir.path().join(CANDIDATE_NAME);
        let pre_probe_healthy = probe::run_sync_probe(
            &candidate_path,
            "pre-install",
            &version.to_string(),
            RECOVERY_PROBE_TIMEOUT,
        )
        .map(|outcome| outcome.is_healthy())
        .unwrap_or(false);
        if !pre_probe_healthy {
            dir.remove_file(CANDIDATE_NAME)?;
            dir.fsync_dir()?;
            // The manifest WAS accepted — its floors must survive the failed
            // install (a later lower-generation manifest must stay rejected).
            let backed_off = register_failure_backoff(
                &merge_acceptance_floors(&current_state, &staged.new_state),
                staged.local_now,
            );
            write_state(&dir, &backed_off).map_err(|e| TxnError::State(e.to_string()))?;
            return Ok(Some(TxnOutcome::NoOp));
        }
        // A newer control decision may have landed in the floors side file
        // while we staged and probed (its writer does not take the main
        // lock): re-check right before the transaction so a mid-flight floor
        // or control-revision raise aborts this candidate instead of
        // installing under superseded policy.
        let latest = read_state(&dir).map_err(|e| TxnError::State(e.to_string()))?;
        if latest.generation_floor > staged.new_state.generation_floor
            || latest.max_control_revision > staged.new_state.max_control_revision
        {
            dir.remove_file(CANDIDATE_NAME)?;
            dir.fsync_dir()?;
            return Ok(Some(TxnOutcome::NoOp));
        }
        let old_target = current_old_target(&dir, platform)?;
        let new_hash = staged.marker.sha256.clone();
        let expected = version.to_string();
        let probe =
            move |d: &InstallDir| sync_post_install_probe(d, &expected, RECOVERY_PROBE_TIMEOUT);
        txn::run_install(
            &dir,
            old_target,
            &version.to_string(),
            &new_hash,
            staged.marker,
            staged.new_state,
            &probe,
        )
        .map(Some)
    })
    .await
}

/// Snapshot the current target as the transaction's `old_target`.
fn current_old_target(dir: &InstallDir, platform: Platform) -> Result<OldTarget, TxnError> {
    use super::lock::EntryKind;
    match dir.stat_entry(TARGET_BINARY_NAME)? {
        Some(EntryKind::Regular { .. }) => {
            let bytes = dir.read_file(TARGET_BINARY_NAME)?.unwrap_or_default();
            use sha2::Digest as _;
            let hash = hex::encode(sha2::Sha256::digest(&bytes));
            let marker_snapshot = official_marker_for_target(dir, platform.as_str())
                .ok()
                .flatten();
            Ok(OldTarget::Present {
                hash,
                marker_snapshot,
            })
        }
        _ => Ok(OldTarget::Absent),
    }
}

/// Parse an HTTP `Date` header to unix seconds (RFC 1123 / RFC 850 / asctime,
/// via `chrono`). `None` on any parse failure — the caller then rejects the
/// round (§A.6 requires a usable HTTPS Date).
fn parse_http_date_to_unix(raw: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc2822(raw.trim())
        .ok()
        .map(|dt| dt.timestamp())
}

/// RFC3339 timestamp for a unix-seconds instant (marker `installed_at`).
fn now_rfc3339(local_now: i64) -> String {
    chrono::DateTime::from_timestamp(local_now, 0)
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string())
}

// ─── manual `libra upgrade` (§A.8's explicit half) ──────────────────────────
//
// The manual command is the consumer of `upgrade.mode=manual`: it runs the
// SAME verified pipeline as the auto check (signed manifest → anti-rollback
// state → decision → bounded download → locked transaction with probe and
// rollback), but on explicit user request — so it ignores the auto cadence
// gates (`should_check_now`, backoff) and reports real diagnostics instead
// of degrading every failure to a silent skip.
//
// Two invariants the manual path adds on top of the auto flow:
// 1. The moment a manifest is ACCEPTED (whether it offers an install or a
//    skip), its monotone floors are persisted durably and FALLIBLY — the
//    command fails rather than silently forgetting a control decision.
// 2. The confirmation window is unbounded (a human is thinking), so
//    `install()` re-fetches and re-decides before touching anything: a
//    pause/revocation/rotation published while the prompt was open wins.

/// Total wall-clock budget for a manual manifest fetch + decision.
pub const MANUAL_CHECK_BUDGET: Duration = Duration::from_secs(30);
/// Total wall-clock budget for the manual artifact download (≤128 MiB).
pub const MANUAL_DOWNLOAD_BUDGET: Duration = Duration::from_secs(300);

/// A manual check/install failure with a user-facing description.
#[derive(Debug, thiserror::Error)]
pub enum ManualUpgradeError {
    /// The manifest or artifact could not be fetched (network/TLS/size).
    #[error("{0}")]
    Fetch(#[from] super::http::UpgradeHttpError),
    /// The manifest failed verification or was rejected by the persisted
    /// anti-rollback/time state.
    #[error("{0}")]
    Verify(#[from] super::flow::FlowError),
    /// The upgrade state next to the installed binary could not be read.
    #[error("cannot read the upgrade state next to the installed binary: {0}")]
    State(String),
    /// The accepted manifest's floors could not be persisted durably; the
    /// command fails closed rather than proceed on volatile control state.
    #[error("cannot persist the accepted anti-rollback floors: {0}")]
    FloorPersist(String),
    /// A network operation exceeded its total wall-clock budget.
    #[error("{0} timed out")]
    Timeout(&'static str),
    /// The install transaction itself failed after the download; the next
    /// libra command runs startup recovery for any journaled remainder.
    #[error("the install transaction failed: {0}")]
    Txn(String),
}

/// Outcome of the manual availability check.
pub enum ManualCheckOutcome {
    /// This binary is not an official upgrade-manageable install: a dev
    /// build (`cargo run`), a renamed copy, an install directory that fails
    /// §A.5 validation, or a target without the official install marker.
    NotOfficialInstall,
    /// The running platform is outside the supported upgrade matrix.
    UnsupportedPlatform,
    /// Already on the latest signed release.
    UpToDate {
        installed: ReleaseVersion,
        latest: ReleaseVersion,
    },
    /// The publisher has paused releases (emergency stop, §A.6).
    Paused { installed: ReleaseVersion },
    /// The latest published version revokes itself; nothing to install.
    RevokedLatest {
        installed: ReleaseVersion,
        revoked: ReleaseVersion,
    },
    /// A newer signed version is ready to install. Its monotone floors are
    /// ALREADY persisted by the time this is returned.
    Available(Box<ManualUpgrade>),
}

/// What a confirmed manual install did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManualInstallReport {
    /// The new version is on disk; it takes effect on the next command.
    Installed(ReleaseVersion),
    /// The candidate failed its post-install self-check and the previous
    /// binary was restored.
    RolledBack,
    /// Nothing was changed: another process holds the upgrade lock, made
    /// concurrent progress, or the candidate failed its pre-install probe.
    NotApplied,
    /// The publisher's control decision changed between the confirmation
    /// prompt and the install (pause/revocation/a different version):
    /// nothing was installed; `detail` says what changed.
    ControlChanged { detail: String },
}

/// A decided, verified manual upgrade whose floors are already durable.
/// `install()` re-verifies the live manifest before downloading anything.
pub struct ManualUpgrade {
    ctx: InstallContext,
    installed: ReleaseVersion,
    latest: ReleaseVersion,
    artifact_size: u64,
}

impl ManualUpgrade {
    /// The running binary's version.
    pub fn installed(&self) -> ReleaseVersion {
        self.installed
    }

    /// The verified newer version the manifest offered at check time.
    pub fn latest(&self) -> ReleaseVersion {
        self.latest
    }

    /// Signed artifact size in bytes (download + install budget).
    pub fn artifact_size(&self) -> u64 {
        self.artifact_size
    }

    /// Re-verify the live manifest, download the artifact (sha256/size
    /// enforced, ≤128 MiB, bounded wall clock) and run the locked install
    /// transaction with pre/post probes and auto-rollback.
    pub async fn install(self) -> Result<ManualInstallReport, ManualUpgradeError> {
        let client = upgrade_http_client()?;
        let fetched =
            tokio::time::timeout(MANUAL_CHECK_BUDGET, fetch_manifest(&client, MANIFEST_URL))
                .await
                .map_err(|_| ManualUpgradeError::Timeout("manifest re-fetch"))??;
        let local_now = unix_now();
        let https_date_unix = fetched
            .https_date
            .as_deref()
            .and_then(parse_http_date_to_unix);
        self.install_from_envelope(
            &client,
            &fetched.bytes,
            https_date_unix,
            local_now,
            active_trust_table(),
        )
        .await
    }

    /// The re-verify + download + transact core, parameterized on the
    /// envelope so the test build can drive it without a network.
    async fn install_from_envelope(
        self,
        client: &reqwest::Client,
        envelope_bytes: &[u8],
        https_date_unix: Option<i64>,
        local_now: i64,
        trust: &[super::trusted_keys::TrustedKey],
    ) -> Result<ManualInstallReport, ManualUpgradeError> {
        // Fresh baseline: the confirmation window is unbounded, so both the
        // decision AND phase-b's CAS must run against the CURRENT state.
        let state =
            read_state(&self.ctx.dir).map_err(|e| ManualUpgradeError::State(e.to_string()))?;
        let decision = decide_for_state(
            &self.ctx,
            &state,
            envelope_bytes,
            https_date_unix,
            local_now,
            self.installed,
            trust,
        )?;
        let plan = match decision {
            UpgradeDecision::Install(plan) if plan.version == self.latest => plan,
            UpgradeDecision::Install(plan) => {
                persist_floors_strict(self.ctx.dir_path.clone(), plan.new_state.clone()).await?;
                return Ok(ManualInstallReport::ControlChanged {
                    detail: format!(
                        "the signed channel now offers v{} instead of the confirmed v{}",
                        plan.version, self.latest
                    ),
                });
            }
            UpgradeDecision::Skip { reason, new_state } => {
                persist_floors_strict(self.ctx.dir_path.clone(), new_state).await?;
                return Ok(ManualInstallReport::ControlChanged {
                    detail: skip_reason_human(&reason),
                });
            }
        };
        // Durable floors before any download/staging (crash windows included).
        persist_floors_strict(self.ctx.dir_path.clone(), plan.new_state.clone()).await?;

        let mut candidate: Vec<u8> = Vec::new();
        tokio::time::timeout(
            MANUAL_DOWNLOAD_BUDGET,
            download_artifact_to(
                client,
                &plan.artifact.url,
                plan.artifact.size,
                &plan.artifact.sha256,
                &mut candidate,
            ),
        )
        .await
        .map_err(|_| ManualUpgradeError::Timeout("artifact download"))??;

        let staged = StagedInstall {
            version: plan.version,
            marker: plan.marker,
            new_state: plan.new_state,
            expected_state: state,
            candidate,
            local_now,
        };
        match run_locked_install(&self.ctx, staged).await {
            Ok(Ok(Some(TxnOutcome::Installed))) => Ok(ManualInstallReport::Installed(plan.version)),
            Ok(Ok(Some(TxnOutcome::RolledBack))) => Ok(ManualInstallReport::RolledBack),
            Ok(Ok(Some(_))) | Ok(Ok(None)) => Ok(ManualInstallReport::NotApplied),
            Ok(Err(error)) => Err(ManualUpgradeError::Txn(error.to_string())),
            Err(join) => Err(ManualUpgradeError::Txn(join.to_string())),
        }
    }
}

/// Unix seconds now (0 on a pre-epoch clock, matching the auto hook).
fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Human sentence for a skip reason surfaced through `ControlChanged`.
fn skip_reason_human(reason: &super::flow::SkipReason) -> String {
    use super::flow::SkipReason;
    match reason {
        SkipReason::NotNewer { installed, .. } => {
            format!("the installed v{installed} is already the latest signed release")
        }
        SkipReason::Paused => "the publisher has PAUSED releases (emergency stop)".to_string(),
        SkipReason::RevokedTarget(v) => {
            format!("the offered v{v} has been REVOKED by the publisher")
        }
        SkipReason::UnsupportedPlatform(_) | SkipReason::PlatformNotInMatrix => {
            "this platform left the supported upgrade matrix".to_string()
        }
    }
}

/// Durably persist accepted monotone floors, PROPAGATING failure — the
/// manual path must fail closed rather than proceed on volatile control
/// state (the auto path's best-effort variant stays separate).
async fn persist_floors_strict(
    dir_path: PathBuf,
    accepted: UpgradeState,
) -> Result<(), ManualUpgradeError> {
    tokio::task::spawn_blocking(move || {
        let dir =
            InstallDir::open_validated(&dir_path).map_err(|e| format!("install directory: {e}"))?;
        record_acceptance_floors(&dir, &accepted).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| ManualUpgradeError::FloorPersist(e.to_string()))?
    .map_err(ManualUpgradeError::FloorPersist)
}

/// Pure decision step shared by the check and the pre-install re-check.
fn decide_for_state(
    ctx: &InstallContext,
    state: &UpgradeState,
    envelope_bytes: &[u8],
    https_date: Option<i64>,
    local_now: i64,
    installed: ReleaseVersion,
    trust: &[super::trusted_keys::TrustedKey],
) -> Result<UpgradeDecision, ManualUpgradeError> {
    let ctx_dec = DecisionContext {
        state,
        https_date,
        local_now,
        trust,
        platform: Some(ctx.platform),
        installed_version: installed,
        installed_at_rfc3339: &now_rfc3339(local_now),
    };
    Ok(decide_from_envelope(&ctx_dec, envelope_bytes)?)
}

/// Manual availability check: verify the live signed manifest and decide.
/// Unlike the auto check this reports every failure to the caller and does
/// not consult the auto cadence/backoff gates; like the auto check, every
/// verified control decision (install offer or skip) durably advances the
/// floors — and here a floor-persist failure is an ERROR, not a shrug.
pub async fn manual_upgrade_check(
    local_now: i64,
) -> Result<ManualCheckOutcome, ManualUpgradeError> {
    let trust = active_trust_table();
    if trust.is_empty() {
        // A build with no trust table cannot verify anything — treat it like
        // a non-official install (dev builds hit the same guidance).
        return Ok(ManualCheckOutcome::NotOfficialInstall);
    }
    let Some(ctx) = resolve_install_context() else {
        return Ok(ManualCheckOutcome::NotOfficialInstall);
    };
    if ctx.platform.support() != PlatformSupport::Supported {
        return Ok(ManualCheckOutcome::UnsupportedPlatform);
    }
    if official_marker_for_target(&ctx.dir, ctx.platform.as_str())
        .ok()
        .flatten()
        .is_none()
    {
        return Ok(ManualCheckOutcome::NotOfficialInstall);
    }
    let client = upgrade_http_client()?;
    let fetched = tokio::time::timeout(MANUAL_CHECK_BUDGET, fetch_manifest(&client, MANIFEST_URL))
        .await
        .map_err(|_| ManualUpgradeError::Timeout("manifest fetch"))??;
    let https_date_unix = fetched
        .https_date
        .as_deref()
        .and_then(parse_http_date_to_unix);
    manual_check_with_envelope(ctx, &fetched.bytes, https_date_unix, local_now, trust).await
}

/// The decision half of the manual check, parameterized on the envelope so
/// the test build can drive it without a network.
async fn manual_check_with_envelope(
    ctx: InstallContext,
    envelope_bytes: &[u8],
    https_date_unix: Option<i64>,
    local_now: i64,
    trust: &[super::trusted_keys::TrustedKey],
) -> Result<ManualCheckOutcome, ManualUpgradeError> {
    let state = read_state(&ctx.dir).map_err(|e| ManualUpgradeError::State(e.to_string()))?;
    let installed = ReleaseVersion::parse(env!("CARGO_PKG_VERSION")).ok_or_else(|| {
        ManualUpgradeError::State("the running binary's version is not canonical X.Y.Z".to_string())
    })?;
    let decision = decide_for_state(
        &ctx,
        &state,
        envelope_bytes,
        https_date_unix,
        local_now,
        installed,
        trust,
    )?;
    Ok(match decision {
        UpgradeDecision::Install(plan) => {
            // Floors become durable the moment the manifest is accepted —
            // BEFORE the unbounded confirmation window opens (a concurrent
            // process must see the new generation/control floor at once).
            persist_floors_strict(ctx.dir_path.clone(), plan.new_state.clone()).await?;
            ManualCheckOutcome::Available(Box::new(ManualUpgrade {
                ctx,
                installed,
                latest: plan.version,
                artifact_size: plan.artifact.size,
            }))
        }
        UpgradeDecision::Skip { reason, new_state } => {
            // Same acceptance semantics for a skip: floors first (fallibly),
            // then the auto path's best-effort full-state persist for the
            // shared cooldown bookkeeping.
            persist_floors_strict(ctx.dir_path.clone(), new_state.clone()).await?;
            persist_accepted_skip_state(&ctx, state.clone(), new_state).await;
            use super::flow::SkipReason;
            match reason {
                SkipReason::NotNewer {
                    manifest,
                    installed,
                } => ManualCheckOutcome::UpToDate {
                    installed,
                    latest: manifest,
                },
                SkipReason::Paused => ManualCheckOutcome::Paused { installed },
                SkipReason::RevokedTarget(revoked) => {
                    ManualCheckOutcome::RevokedLatest { installed, revoked }
                }
                SkipReason::UnsupportedPlatform(_) | SkipReason::PlatformNotInMatrix => {
                    ManualCheckOutcome::UnsupportedPlatform
                }
            }
        }
    })
}

/// Test-only manual-flow harness (§A.11): drive the check and install cores
/// against an arbitrary validated install directory and in-memory envelopes,
/// skipping only the exe-name/marker resolution (which needs a real official
/// install). Compiled solely with `--features test-upgrade`.
#[cfg(feature = "test-upgrade")]
pub mod manual_test_hooks {
    use super::*;

    /// Run the manual check decision core against `dir_path` + envelope.
    pub async fn manual_check_from_parts(
        dir_path: &std::path::Path,
        platform: Platform,
        envelope_bytes: &[u8],
        https_date_unix: Option<i64>,
        local_now: i64,
        trust: &[super::super::trusted_keys::TrustedKey],
    ) -> Result<ManualCheckOutcome, ManualUpgradeError> {
        let dir = InstallDir::open_validated(dir_path)
            .map_err(|e| ManualUpgradeError::State(e.to_string()))?;
        let ctx = InstallContext {
            dir,
            dir_path: dir_path.to_path_buf(),
            platform,
        };
        manual_check_with_envelope(ctx, envelope_bytes, https_date_unix, local_now, trust).await
    }

    /// Run the install core with an injected envelope + candidate download.
    pub async fn install_with_envelope_and_candidate(
        upgrade: ManualUpgrade,
        envelope_bytes: &[u8],
        https_date_unix: Option<i64>,
        local_now: i64,
        candidate: Vec<u8>,
        trust: &[super::super::trusted_keys::TrustedKey],
    ) -> Result<ManualInstallReport, ManualUpgradeError> {
        // Reuse install_from_envelope's decision/floors path but swap the
        // download for the injected bytes by re-implementing its tail.
        let state =
            read_state(&upgrade.ctx.dir).map_err(|e| ManualUpgradeError::State(e.to_string()))?;
        let decision = decide_for_state(
            &upgrade.ctx,
            &state,
            envelope_bytes,
            https_date_unix,
            local_now,
            upgrade.installed,
            trust,
        )?;
        let plan = match decision {
            UpgradeDecision::Install(plan) if plan.version == upgrade.latest => plan,
            UpgradeDecision::Install(plan) => {
                persist_floors_strict(upgrade.ctx.dir_path.clone(), plan.new_state.clone()).await?;
                return Ok(ManualInstallReport::ControlChanged {
                    detail: format!(
                        "the signed channel now offers v{} instead of the confirmed v{}",
                        plan.version, upgrade.latest
                    ),
                });
            }
            UpgradeDecision::Skip { reason, new_state } => {
                persist_floors_strict(upgrade.ctx.dir_path.clone(), new_state).await?;
                return Ok(ManualInstallReport::ControlChanged {
                    detail: skip_reason_human(&reason),
                });
            }
        };
        persist_floors_strict(upgrade.ctx.dir_path.clone(), plan.new_state.clone()).await?;
        let staged = StagedInstall {
            version: plan.version,
            marker: plan.marker,
            new_state: plan.new_state,
            expected_state: state,
            candidate,
            local_now,
        };
        match run_locked_install(&upgrade.ctx, staged).await {
            Ok(Ok(Some(TxnOutcome::Installed))) => Ok(ManualInstallReport::Installed(plan.version)),
            Ok(Ok(Some(TxnOutcome::RolledBack))) => Ok(ManualInstallReport::RolledBack),
            Ok(Ok(Some(_))) | Ok(Ok(None)) => Ok(ManualInstallReport::NotApplied),
            Ok(Err(error)) => Err(ManualUpgradeError::Txn(error.to_string())),
            Err(join) => Err(ManualUpgradeError::Txn(join.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_check_gates_on_mode_and_trust() {
        let state = UpgradeState::default();
        // Off mode never checks.
        assert!(!should_check_now(UpgradeMode::Off, false, &state, 1_000));
        assert!(!should_check_now(UpgradeMode::Manual, false, &state, 1_000));
        // Auto with empty trust never checks (inert in production).
        assert!(!should_check_now(UpgradeMode::Auto, true, &state, 1_000));
        // Auto with keys and no throttle ⇒ check.
        assert!(should_check_now(UpgradeMode::Auto, false, &state, 1_000));
    }

    #[test]
    fn should_check_respects_backoff_and_cooldown() {
        let state = register_failure_backoff(&UpgradeState::default(), 1_000);
        // Backoff defers.
        assert!(!should_check_now(UpgradeMode::Auto, false, &state, 1_000));
        // After the backoff window, it checks again.
        assert!(should_check_now(
            UpgradeMode::Auto,
            false,
            &state,
            1_000 + state.backoff_seconds + 1
        ));

        // A live cooldown skips checking.
        let cooled = UpgradeState {
            trusted_time_floor: 10_000,
            next_success_check_not_before: Some(10_600),
            ..Default::default()
        };
        assert!(!should_check_now(UpgradeMode::Auto, false, &cooled, 10_100));
        // Past the cooldown, it checks.
        assert!(should_check_now(UpgradeMode::Auto, false, &cooled, 10_601));
    }

    #[test]
    fn http_date_parsing_rejects_garbage() {
        assert!(parse_http_date_to_unix("not a date").is_none());
        let ts = parse_http_date_to_unix("Wed, 01 Jul 2026 00:00:00 GMT")
            .expect("test fixture operation should succeed");
        assert!(ts > 1_700_000_000);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn auto_check_skips_when_the_test_binary_has_no_install_context() {
        // The test binary is not an installed `libra` target, so this reaches
        // the no-install-context gate without making a network request.
        assert_eq!(
            run_auto_upgrade_check(1_000).await,
            AutoUpgradeReport::Skipped
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn recovery_gate_skips_when_the_test_binary_has_no_install_context() {
        assert!(startup_recovery_gate().await.is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn accepted_skip_state_is_atomically_persisted_only_from_matching_baseline() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("test directory must be created");
        let path = temp
            .path()
            .canonicalize()
            .expect("test directory must canonicalize");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
            .expect("test directory permissions must be set");
        let dir = InstallDir::open_validated(&path).expect("test directory must validate");
        let baseline = UpgradeState::default();
        let accepted = UpgradeState {
            generation_floor: 2,
            ..UpgradeState::default()
        };

        assert!(
            persist_if_state_unchanged(&dir, &baseline, &accepted)
                .expect("matching baseline must persist")
        );
        assert_eq!(
            read_state(&dir)
                .expect("persisted state must be readable")
                .generation_floor,
            2
        );

        let concurrent = UpgradeState {
            generation_floor: 3,
            ..UpgradeState::default()
        };
        write_state(&dir, &concurrent).expect("concurrent state fixture must persist");
        let stale_update = UpgradeState {
            generation_floor: 4,
            ..UpgradeState::default()
        };
        assert!(
            !persist_if_state_unchanged(&dir, &accepted, &stale_update)
                .expect("stale baseline must safely skip")
        );
        assert_eq!(
            read_state(&dir)
                .expect("concurrent state must remain readable")
                .generation_floor,
            3
        );
    }

    #[cfg(unix)]
    #[test]
    fn stale_failure_backoff_cannot_clobber_an_advanced_generation_floor() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("test directory must be created");
        let path = temp
            .path()
            .canonicalize()
            .expect("test directory must canonicalize");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
            .expect("test directory permissions must be set");
        let dir = InstallDir::open_validated(&path).expect("test directory must validate");
        let baseline = UpgradeState::default();
        let advanced = UpgradeState {
            generation_floor: 3,
            ..UpgradeState::default()
        };
        write_state(&dir, &advanced).expect("advanced state fixture must persist");

        let stale_backoff = register_failure_backoff(&baseline, 1_000);
        assert!(
            !persist_if_state_unchanged(&dir, &baseline, &stale_backoff)
                .expect("stale failure backoff must safely skip")
        );
        let current = read_state(&dir).expect("advanced state must remain readable");
        assert_eq!(current.generation_floor, 3);
        assert_eq!(current.backoff_not_before, None);
    }

    #[cfg(unix)]
    #[test]
    fn floors_side_file_merges_over_any_baseline_and_never_regresses() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("test directory must be created");
        let path = temp
            .path()
            .canonicalize()
            .expect("test directory must canonicalize");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
            .expect("test directory permissions must be set");
        let dir = InstallDir::open_validated(&path).expect("test directory must validate");
        // The on-disk state has moved past this attempt's baseline: another
        // process registered a backoff AND advanced the floor to 2.
        let moved = UpgradeState {
            generation_floor: 2,
            backoff_not_before: Some(9_999),
            backoff_seconds: 120,
            ..UpgradeState::default()
        };
        write_state(&dir, &moved).expect("moved state fixture must persist");

        // Recording an accepted floor-3 snapshot needs no baseline match and
        // leaves the concurrent backoff untouched (side file, not the CAS'd
        // main state).
        let accepted = UpgradeState {
            generation_floor: 3,
            ..UpgradeState::default()
        };
        record_acceptance_floors(&dir, &accepted).expect("floor recording must not error");
        let current = read_state(&dir).expect("merged state must be readable");
        assert_eq!(current.generation_floor, 3);
        assert_eq!(current.backoff_not_before, Some(9_999));
        assert_eq!(current.backoff_seconds, 120);

        // A stale lower floor is a no-op.
        let lower = UpgradeState {
            generation_floor: 1,
            ..UpgradeState::default()
        };
        record_acceptance_floors(&dir, &lower).expect("floor recording must not error");
        assert_eq!(
            read_state(&dir)
                .expect("state must stay readable")
                .generation_floor,
            3
        );
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn floors_persist_even_while_the_main_upgrade_lock_is_held() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("test directory must be created");
        let path = temp
            .path()
            .canonicalize()
            .expect("test directory must canonicalize");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
            .expect("test directory permissions must be set");
        let dir = InstallDir::open_validated(&path).expect("test directory must validate");
        write_state(&dir, &UpgradeState::default()).expect("baseline state must persist");

        // Another process holds (or wedges) the MAIN upgrade lock for the
        // whole duration: floors must still become durable before the call
        // returns, because they go through the independent side-file lock.
        let _main_lock = dir
            .try_lock()
            .expect("lock probe must not error")
            .expect("test must own the main lock first");
        let accepted = UpgradeState {
            generation_floor: 5,
            ..UpgradeState::default()
        };
        merge_acceptance_floors_with_retry(path.clone(), accepted).await;
        assert_eq!(
            read_state(&dir)
                .expect("state must stay readable")
                .generation_floor,
            5,
            "floors must be durable before the call returns, main lock or not"
        );
    }

    #[test]
    fn failure_backoff_carries_accepted_floors_from_the_failed_attempt() {
        // A download failure or budget timeout AFTER manifest acceptance must
        // still advance the floors the manifest proved.
        let expected = UpgradeState {
            generation_floor: 1,
            ..UpgradeState::default()
        };
        let accepted = UpgradeState {
            generation_floor: 2,
            max_control_revision: 8,
            control_envelope_digest: Some("digest-8".into()),
            ..UpgradeState::default()
        };
        let with_acceptance = failure_backoff_state(&expected, Some(&accepted), 1_000);
        assert_eq!(with_acceptance.generation_floor, 2);
        assert_eq!(with_acceptance.max_control_revision, 8);
        assert_eq!(
            with_acceptance.control_envelope_digest.as_deref(),
            Some("digest-8")
        );
        assert!(with_acceptance.backoff_not_before.is_some());

        // Failures BEFORE acceptance keep the plain baseline behavior.
        let without = failure_backoff_state(&expected, None, 1_000);
        assert_eq!(without.generation_floor, 1);
        assert!(without.backoff_not_before.is_some());
    }
}
