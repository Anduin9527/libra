//! plan-20260825 PS-06: end-to-end zero/one/many credential detection for
//! `libra code` without `--provider`.
//!
//! **Layer:** L1 — deterministic. Every run gets an isolated HOME and an
//! isolated `LIBRA_CONFIG_GLOBAL_DB`, and the six auto-selectable provider
//! keys are removed from the child environment, so ambient developer
//! credentials cannot leak into the verdicts.

use std::{
    io::{BufRead, BufReader},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

const AUTO_SELECTABLE_KEYS: [&str; 6] = [
    "GEMINI_API_KEY",
    "OPENAI_API_KEY",
    "ANTHROPIC_API_KEY",
    "DEEPSEEK_API_KEY",
    "MOONSHOT_API_KEY",
    "ZHIPU_API_KEY",
];

struct Repo {
    _temp: tempfile::TempDir,
    root: std::path::PathBuf,
    home: std::path::PathBuf,
    global_db: std::path::PathBuf,
}

fn init_repo() -> Repo {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("repo");
    let home = temp.path().join("home");
    std::fs::create_dir_all(&root).expect("repo dir");
    std::fs::create_dir_all(home.join(".config")).expect("isolated HOME");
    let global_db = temp.path().join("global-config.db");

    let status = base_command(&home, &global_db)
        .arg("init")
        .current_dir(&root)
        .status()
        .expect("libra init");
    assert!(status.success(), "libra init failed");
    Repo {
        _temp: temp,
        root,
        home,
        global_db,
    }
}

fn base_command(home: &std::path::Path, global_db: &std::path::Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_libra"));
    cmd.env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("USERPROFILE", home)
        .env("LIBRA_CONFIG_GLOBAL_DB", global_db)
        .env("LIBRA_TEST", "1");
    for key in AUTO_SELECTABLE_KEYS {
        cmd.env_remove(key);
    }
    cmd
}

#[test]
fn zero_candidates_exit_auth_with_the_a_mode_guidance() {
    let repo = init_repo();
    let out = base_command(&repo.home, &repo.global_db)
        .args(["code", "--port", "0", "--mcp-port", "0"])
        .current_dir(&repo.root)
        .output()
        .expect("run zero-candidate probe");
    assert_eq!(
        out.status.code(),
        Some(128),
        "zero candidates must exit with the LBR-AUTH-001 code; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no provider credentials configured")
            && stderr.contains("Checked in order:")
            && stderr.contains("    libra code --provider codex")
            && stderr.contains("    libra code --provider ollama --model <name>"),
        "a-mode zero-state guidance missing: {stderr}"
    );
    assert!(
        stderr.contains("Error-Code: LBR-AUTH-001"),
        "stable code missing: {stderr}"
    );
}

#[test]
fn many_candidates_exit_usage_listing_sorted_candidates_and_layers() {
    let repo = init_repo();
    let out = base_command(&repo.home, &repo.global_db)
        .args(["code", "--port", "0", "--mcp-port", "0"])
        .current_dir(&repo.root)
        .env("ZHIPU_API_KEY", "probe-zhipu")
        .env("DEEPSEEK_API_KEY", "probe-deepseek")
        .output()
        .expect("run many-candidate probe");
    assert_eq!(
        out.status.code(),
        Some(129),
        "ambiguity must exit with the LBR-CLI-002 code; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    let deepseek = stderr
        .find("    deepseek  (DEEPSEEK_API_KEY in process environment)")
        .unwrap_or_else(|| panic!("deepseek candidate line missing: {stderr}"));
    let zhipu = stderr
        .find("    zhipu  (ZHIPU_API_KEY in process environment)")
        .unwrap_or_else(|| panic!("zhipu candidate line missing: {stderr}"));
    assert!(deepseek < zhipu, "candidates must be id-sorted: {stderr}");
    assert!(
        !stderr.contains("probe-zhipu") && !stderr.contains("probe-deepseek"),
        "key values must never appear (GC-PS-01): {stderr}"
    );
}

#[test]
fn one_candidate_auto_selects_announcing_on_stderr_only() {
    let repo = init_repo();
    let mut child = base_command(&repo.home, &repo.global_db)
        .args(["code", "--port", "0", "--mcp-port", "0"])
        .current_dir(&repo.root)
        .env("GEMINI_API_KEY", "probe-gemini")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start single-candidate launch");

    struct KillOnDrop(Option<std::process::Child>);
    impl Drop for KillOnDrop {
        fn drop(&mut self) {
            if let Some(mut child) = self.0.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
    let stderr = child.stderr.take().expect("stderr pipe");
    let stdout = child.stdout.take().expect("stdout pipe");
    let mut guard = KillOnDrop(Some(child));

    // The auto-selection note precedes the web boot on stderr.
    let (note_tx, note_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            let _ = note_tx.send(line);
        }
    });
    let (out_tx, out_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let _ = out_tx.send(line);
        }
    });

    let deadline = Instant::now() + Duration::from_secs(60);
    let mut saw_note = false;
    while Instant::now() < deadline && !saw_note {
        if let Ok(line) = note_rx.recv_timeout(Duration::from_millis(200)) {
            assert!(
                !line.contains("probe-gemini"),
                "key value on stderr (GC-PS-01): {line}"
            );
            if line.contains("provider 'gemini' auto-selected")
                && line.contains("GEMINI_API_KEY")
                && line.contains("process environment")
            {
                saw_note = true;
            }
        }
    }
    assert!(saw_note, "auto-selection note did not appear on stderr");

    // stdout must not carry the note (it belongs to diagnostics).
    while let Ok(line) = out_rx.recv_timeout(Duration::from_millis(500)) {
        assert!(
            !line.contains("auto-selected"),
            "auto-selection note leaked to stdout: {line}"
        );
        if line.contains("http://") {
            break; // web boot reached; detection is done
        }
    }

    if let Some(child) = guard.0.as_mut() {
        let _ = child.kill();
    }
}

#[test]
fn model_without_provider_is_rejected_before_env_file_io() {
    // PS-06 terra R1: the pairing guard must precede env-file reading —
    // pointing --env-file at a DIRECTORY would IO-error if the file were
    // read first, so a 129 usage error here proves the ordering.
    let repo = init_repo();
    let out = base_command(&repo.home, &repo.global_db)
        .args([
            "code",
            "--model",
            "arbitrary-model",
            "--env-file",
            ".",
            "--port",
            "0",
            "--mcp-port",
            "0",
        ])
        .current_dir(&repo.root)
        .output()
        .expect("run model-without-provider probe");
    assert_eq!(
        out.status.code(),
        Some(129),
        "pairing guard must fire before env-file IO; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--model without --provider"),
        "pairing message missing: {stderr}"
    );
    assert!(
        !stderr.contains("env-file") || !stderr.contains("LBR-IO-001"),
        "env-file IO must not have been attempted: {stderr}"
    );
}

#[test]
fn explicit_provider_skips_detection_probes() {
    // PS-06 terra R1: with LIBRA_CONFIG_GLOBAL_DB poisoned (a directory),
    // any detection probe would fail loudly with "credential detection
    // failed"; an explicit --provider must therefore never mention it and
    // instead reach the provider's own missing-key a-mode error.
    let repo = init_repo();
    let poisoned = repo.home.join("poisoned-db-dir");
    std::fs::create_dir_all(&poisoned).expect("poisoned dir");
    let out = base_command(&repo.home, &repo.global_db)
        .args([
            "code",
            "--provider",
            "gemini",
            "--port",
            "0",
            "--mcp-port",
            "0",
        ])
        .current_dir(&repo.root)
        .env("LIBRA_CONFIG_GLOBAL_DB", &poisoned)
        .output()
        .expect("run explicit-provider probe");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("credential detection failed"),
        "explicit --provider must not run detection probes: {stderr}"
    );
    assert_eq!(
        out.status.code(),
        Some(128),
        "explicit gemini without a key lands on its own auth error: {stderr}"
    );
    assert!(
        stderr.contains("GEMINI_API_KEY is not configured for provider 'gemini'"),
        "the provider-specific a-mode error is expected: {stderr}"
    );
}

#[test]
fn many_candidates_report_the_global_vault_layer() {
    // PS-06 terra R1: the repo/global layer labels are user-facing — cover
    // the global-vault label end to end by storing one key in the isolated
    // global DB and one in the process environment.
    let repo = init_repo();
    let set = base_command(&repo.home, &repo.global_db)
        .args([
            "config",
            "set",
            "--global",
            "vault.env.ZHIPU_API_KEY",
            "layer-probe-zhipu",
        ])
        .current_dir(&repo.root)
        .output()
        .expect("store global vault key");
    assert!(
        set.status.success(),
        "config set --global failed: {}",
        String::from_utf8_lossy(&set.stderr)
    );
    let out = base_command(&repo.home, &repo.global_db)
        .args(["code", "--port", "0", "--mcp-port", "0"])
        .current_dir(&repo.root)
        .env("DEEPSEEK_API_KEY", "probe-deepseek")
        .output()
        .expect("run mixed-layer probe");
    assert_eq!(out.status.code(), Some(129));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("    deepseek  (DEEPSEEK_API_KEY in process environment)")
            && stderr.contains("    zhipu  (ZHIPU_API_KEY in global vault)"),
        "layer labels must distinguish the sources: {stderr}"
    );
    assert!(
        !stderr.contains("layer-probe-zhipu") && !stderr.contains("probe-deepseek"),
        "values must never leak: {stderr}"
    );
}
