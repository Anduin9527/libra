//! End-to-end coverage for vault-signed merge commits.

use std::path::Path;

use tempfile::{TempDir, tempdir};

use super::{
    assert_cli_success, commit_file, configure_identity_via_cli, create_committed_repo_via_cli,
    head_commit, init_repo_via_cli, run_libra_command, run_libra_command_with_stdin,
};

fn divergent_merge_repo(with_vault_key: bool) -> TempDir {
    let repo = tempdir().expect("create merge signing repository");
    let path = repo.path();
    assert_cli_success(
        &run_libra_command(&["init", "--vault", "false"], path),
        "initialize merge signing repository",
    );
    configure_identity_via_cli(path);
    if with_vault_key {
        assert_cli_success(
            &run_libra_command(&["config", "generate-gpg-key"], path),
            "generate repository vault signing key",
        );
    }
    commit_file(path, "base.txt", "base\n", "base");
    assert_cli_success(
        &run_libra_command(&["branch", "feature"], path),
        "create feature branch",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], path),
        "checkout feature branch",
    );
    commit_file(path, "feature.txt", "feature\n", "feature change");
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], path),
        "return to main branch",
    );
    commit_file(path, "main.txt", "main\n", "main change");
    repo
}

fn raw_head_commit(repo: &Path) -> String {
    let output = run_libra_command_with_stdin(&["cat-file", "--batch"], repo, "HEAD\n");
    assert_cli_success(&output, "read raw HEAD commit");
    String::from_utf8(output.stdout).expect("commit object must be utf-8")
}

fn assert_head_is_signed(repo: &Path, context: &str) {
    let raw = raw_head_commit(repo);
    assert!(
        raw.contains("-----BEGIN PGP SIGNATURE-----"),
        "{context} must create a vault-signed merge commit: {raw}"
    );
}

#[test]
fn merge_gpg_sign_automatic_merge_is_signed() {
    let repo = divergent_merge_repo(true);
    assert_cli_success(
        &run_libra_command(&["merge", "-S", "feature"], repo.path()),
        "merge -S feature",
    );
    assert_head_is_signed(repo.path(), "automatic merge");
}

#[test]
fn merge_gpg_sign_continue_keeps_explicit_signing() {
    let repo = divergent_merge_repo(true);
    assert_cli_success(
        &run_libra_command(&["merge", "-S", "--no-commit", "feature"], repo.path()),
        "start signed no-commit merge",
    );
    assert_cli_success(
        &run_libra_command(&["merge", "--continue"], repo.path()),
        "continue signed merge",
    );
    assert_head_is_signed(repo.path(), "continued merge");
}

#[test]
fn merge_gpg_sign_ours_strategy_is_signed() {
    let repo = divergent_merge_repo(true);
    assert_cli_success(
        &run_libra_command(&["merge", "-S", "-s", "ours", "feature"], repo.path()),
        "merge -S -s ours feature",
    );
    assert_head_is_signed(repo.path(), "ours merge");
}

#[test]
fn merge_gpg_sign_octopus_merge_is_signed() {
    let repo = divergent_merge_repo(true);
    let path = repo.path();
    assert_cli_success(
        &run_libra_command(&["branch", "second"], path),
        "create second octopus branch",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "second"], path),
        "checkout second octopus branch",
    );
    commit_file(path, "second.txt", "second\n", "second change");
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], path),
        "return to octopus main branch",
    );
    assert_cli_success(
        &run_libra_command(&["merge", "-S", "feature", "second"], path),
        "signed octopus merge",
    );
    assert_head_is_signed(path, "octopus merge");
}

#[test]
fn merge_gpg_sign_no_gpg_sign_overrides_every_config_default() {
    let repo = divergent_merge_repo(true);
    let path = repo.path();
    assert_cli_success(
        &run_libra_command(&["config", "commit.gpgSign", "true"], path),
        "force signing through commit.gpgSign",
    );
    assert_cli_success(
        &run_libra_command(&["merge", "--no-gpg-sign", "feature"], path),
        "merge with signing explicitly disabled",
    );
    let raw = raw_head_commit(path);
    assert!(
        !raw.contains("-----BEGIN PGP SIGNATURE-----"),
        "--no-gpg-sign must override commit.gpgSign: {raw}"
    );
}

#[test]
fn merge_gpg_sign_config_priority_matches_commit() {
    let force = divergent_merge_repo(true);
    let force_path = force.path();
    assert_cli_success(
        &run_libra_command(&["config", "vault.signing", "false"], force_path),
        "disable vault default",
    );
    assert_cli_success(
        &run_libra_command(&["config", "commit.gpgSign", "true"], force_path),
        "force signing through commit.gpgSign",
    );
    assert_cli_success(
        &run_libra_command(&["merge", "feature"], force_path),
        "merge with commit.gpgSign=true",
    );
    assert_head_is_signed(force_path, "commit.gpgSign=true merge");

    let fallback = divergent_merge_repo(true);
    let fallback_path = fallback.path();
    assert_cli_success(
        &run_libra_command(&["config", "vault.signing", "true"], fallback_path),
        "enable the vault signing fallback",
    );
    assert_cli_success(
        &run_libra_command(&["merge", "feature"], fallback_path),
        "merge with the vault signing fallback",
    );
    assert_head_is_signed(fallback_path, "vault.signing fallback merge");

    let disable = divergent_merge_repo(true);
    let disable_path = disable.path();
    assert_cli_success(
        &run_libra_command(&["config", "vault.signing", "true"], disable_path),
        "enable vault default",
    );
    assert_cli_success(
        &run_libra_command(&["config", "commit.gpgSign", "false"], disable_path),
        "disable signing through commit.gpgSign",
    );
    assert_cli_success(
        &run_libra_command(&["merge", "feature"], disable_path),
        "merge with commit.gpgSign=false",
    );
    let raw = raw_head_commit(disable_path);
    assert!(
        !raw.contains("-----BEGIN PGP SIGNATURE-----"),
        "commit.gpgSign=false must override vault.signing: {raw}"
    );
}

#[test]
fn merge_gpg_sign_failure_keeps_head_unchanged() {
    let repo = divergent_merge_repo(false);
    let path = repo.path();
    let before = head_commit(path);
    let output = run_libra_command(&["merge", "-S", "feature"], path);
    assert!(
        !output.status.success(),
        "-S must fail when the vault has no unseal key"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unseal key"),
        "the signing failure must explain how the vault is unavailable: {stderr}"
    );
    assert_eq!(
        head_commit(path),
        before,
        "a signing failure must not create or move to a merge commit"
    );
}

// ---------------------------------------------------------------------------
// plan-20260921 VG-05: `merge --verify-signatures` must use the local
// allowlist — an imported key verifies, a key this repository never imported
// does not.
// ---------------------------------------------------------------------------

const MERGE_GPG_FIXTURE_SECRET: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/data/fake-gpg/protected-secret.asc"
);
const MERGE_GPG_FIXTURE_PASSPHRASE: &str = "libra-test-fixture-passphrase";

/// Import the shared fixture certificate over whatever key the repository
/// generated at init time.
fn import_merge_fixture_key(repo: &Path) {
    let passfile = repo.join("merge-fixture-pass.txt");
    std::fs::write(&passfile, MERGE_GPG_FIXTURE_PASSPHRASE).unwrap();
    assert_cli_success(
        &run_libra_command(
            &[
                "config",
                "import-gpg-key",
                "--file",
                MERGE_GPG_FIXTURE_SECRET,
                "--passphrase-file",
                &passfile.to_string_lossy(),
                "--replace",
            ],
            repo,
        ),
        "import the fixture certificate",
    );
}

#[test]
fn merge_verify_signatures_accepts_imported_key() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    import_merge_fixture_key(p);
    assert_cli_success(
        &run_libra_command(&["config", "vault.signing", "true"], p),
        "enable vault signing",
    );
    // `commit.gpgSign=true` forces the vault signing path explicitly (the
    // imported certificate is what signs).
    assert_cli_success(
        &run_libra_command(&["config", "commit.gpgSign", "true"], p),
        "force signed commits",
    );
    assert_cli_success(
        &run_libra_command(&["branch", "dev-imported"], p),
        "branch dev-imported",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "dev-imported"], p),
        "checkout dev-imported",
    );
    std::fs::write(p.join("imported.txt"), "imported\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "imported.txt"], p),
        "add imported.txt",
    );
    assert_cli_success(
        &run_libra_command(
            &["commit", "-m", "tip-signed-by-imported-key", "--no-verify"],
            p,
        ),
        "commit signed by the imported key",
    );
    // The tip must really carry a signature, otherwise the merge below would
    // pass for the wrong reason. `cat-file --batch` prints the raw object, which
    // keeps the `gpgsig` header (the pretty form drops it).
    let raw = run_libra_command_with_stdin(&["cat-file", "--batch"], p, "HEAD\n");
    assert_cli_success(&raw, "read the raw tip object");
    assert!(
        String::from_utf8_lossy(&raw.stdout).contains("-----BEGIN PGP SIGNATURE-----"),
        "the imported key must sign the tip: {}",
        String::from_utf8_lossy(&raw.stdout)
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], p),
        "checkout main",
    );
    assert_cli_success(
        &run_libra_command(&["merge", "--verify-signatures", "dev-imported"], p),
        "a tip signed by the imported key must be accepted",
    );
}

#[test]
fn merge_verify_signatures_rejects_foreign_key() {
    // A source repository whose active certificate this host never imports.
    let foreign = tempfile::tempdir().unwrap();
    let fp = foreign.path();
    init_repo_via_cli(fp);
    configure_identity_via_cli(fp);
    std::fs::write(fp.join("base.txt"), "base\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "base.txt"], fp), "add base");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base", "--no-verify"], fp),
        "base commit",
    );
    import_merge_fixture_key(fp);
    assert_cli_success(
        &run_libra_command(&["config", "vault.signing", "true"], fp),
        "enable signing in the foreign repository",
    );
    assert_cli_success(
        &run_libra_command(&["config", "commit.gpgSign", "true"], fp),
        "force signed commits in the foreign repository",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "-b", "foreign"], fp),
        "branch foreign",
    );
    std::fs::write(fp.join("foreign.txt"), "foreign\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "foreign.txt"], fp),
        "add foreign.txt",
    );
    assert_cli_success(
        &run_libra_command(
            &["commit", "-m", "tip-signed-by-foreign-key", "--no-verify"],
            fp,
        ),
        "commit signed by the foreign key",
    );
    // `cat-file --batch` prints the raw object, which keeps the signature.
    let raw = run_libra_command_with_stdin(&["cat-file", "--batch"], fp, "HEAD\n");
    assert_cli_success(&raw, "read the raw foreign tip object");
    assert!(
        String::from_utf8_lossy(&raw.stdout).contains("-----BEGIN PGP SIGNATURE-----"),
        "the foreign tip must carry a signature: {}",
        String::from_utf8_lossy(&raw.stdout)
    );
    // Keep the source HEAD on its default branch so the clone below starts
    // there; `foreign` travels as a remote-tracking branch instead.
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], fp),
        "checkout main",
    );

    // The host is an ordinary repository on its own key: the foreign tip
    // travels in over a remote and the certificate that signed it is never
    // imported here.
    let work = tempfile::tempdir().unwrap();
    let host = work.path().join("host");
    std::fs::create_dir_all(&host).unwrap();
    init_repo_via_cli(&host);
    configure_identity_via_cli(&host);
    std::fs::write(host.join("host.txt"), "host\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "host.txt"], &host),
        "add host.txt",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "host-base", "--no-verify"], &host),
        "host base commit",
    );
    assert_cli_success(
        &run_libra_command(&["remote", "add", "origin", &fp.to_string_lossy()], &host),
        "add the foreign origin",
    );
    assert_cli_success(
        &run_libra_command(&["fetch", "origin"], &host),
        "fetch the foreign branch",
    );
    let head_before = run_libra_command(&["rev-parse", "HEAD"], &host);
    let merge = run_libra_command(&["merge", "--verify-signatures", "origin/foreign"], &host);
    assert!(
        !merge.status.success(),
        "a tip signed by a key this repository never imported must be refused: {}",
        String::from_utf8_lossy(&merge.stdout)
    );
    let err = String::from_utf8_lossy(&merge.stderr);
    assert!(
        err.contains("has a bad GPG signature"),
        "the refusal must name the bad signature: {err}"
    );
    let head_after = run_libra_command(&["rev-parse", "HEAD"], &host);
    assert_eq!(
        String::from_utf8_lossy(&head_before.stdout).trim(),
        String::from_utf8_lossy(&head_after.stdout).trim(),
        "a refused merge must not move HEAD"
    );
}
