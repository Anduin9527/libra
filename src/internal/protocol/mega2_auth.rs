//! Mega2 write-token resolution (plan-20260912 MB-04, ADR-MB-03).
//!
//! Exactly one token can be supplied, resolved with the fixed precedence
//! `--token-file` → `LIBRA_MEGA2_TOKEN` → `--token`. The resolver is
//! process-local: it never reads repository or global configuration, never
//! writes anything, and the token value is redacted from `Debug`/`Display` so
//! accidental logging cannot leak it.
//!
//! The `--token-file`/`--token` flags themselves belong to the public CLI card
//! (MB-05); this module only owns parsing and precedence.

use std::{
    fmt, fs,
    path::{Path, PathBuf},
};

use crate::utils::error::{CliError, CliResult, StableErrorCode};

/// Environment variable holding the optional write token.
pub const ENV_TOKEN_VAR: &str = "LIBRA_MEGA2_TOKEN";

/// Upper bound for a token (file or inline) — headers stay small.
pub const MAX_TOKEN_BYTES: usize = 8 * 1024;

/// A validated write token whose value never appears in `Debug`/`Display`.
#[derive(Clone, PartialEq, Eq)]
pub struct Mega2Token(String);

impl Mega2Token {
    /// Validates and wraps a raw token value.
    pub fn new(raw: impl Into<String>) -> CliResult<Self> {
        let raw: String = raw.into();
        validate_token(&raw)?;
        Ok(Self(raw))
    }

    /// Explicit accessor for the header-building call site.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Mega2Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Mega2Token(<redacted>)")
    }
}

impl fmt::Display for Mega2Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

/// Where a resolved token came from (useful for diagnostics and tests).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenSource {
    TokenFile,
    Environment,
    Flag,
}

fn validate_token(raw: &str) -> CliResult<()> {
    if raw.is_empty() {
        return Err(CliError::fatal("mega2 write token must not be empty")
            .with_stable_code(StableErrorCode::CliInvalidArguments));
    }
    if raw.len() > MAX_TOKEN_BYTES {
        return Err(CliError::fatal(format!(
            "mega2 write token exceeds the {MAX_TOKEN_BYTES}-byte limit"
        ))
        .with_stable_code(StableErrorCode::CliInvalidArguments));
    }
    if raw.chars().any(|c| c == '\0' || c.is_control()) {
        return Err(
            CliError::fatal("mega2 write token contains NUL or control characters")
                .with_stable_code(StableErrorCode::CliInvalidArguments),
        );
    }
    Ok(())
}

fn read_token_file(path: &Path) -> CliResult<String> {
    let metadata = fs::metadata(path).map_err(|err| {
        CliError::fatal(format!(
            "cannot read the mega2 token file '{}': {err}",
            path.display()
        ))
        .with_stable_code(StableErrorCode::IoReadFailed)
    })?;
    if metadata.len() as usize > MAX_TOKEN_BYTES {
        return Err(CliError::fatal(format!(
            "mega2 token file '{}' exceeds the {MAX_TOKEN_BYTES}-byte limit",
            path.display()
        ))
        .with_stable_code(StableErrorCode::CliInvalidArguments));
    }
    let raw = fs::read_to_string(path).map_err(|err| {
        CliError::fatal(format!(
            "cannot read the mega2 token file '{}': {err}",
            path.display()
        ))
        .with_stable_code(StableErrorCode::IoReadFailed)
    })?;
    // Files written by `echo`/editors end with one newline; only that
    // terminator is stripped — internal whitespace is never trimmed.
    let trimmed = raw.trim_end_matches(['\n', '\r']);
    if trimmed.is_empty() {
        return Err(
            CliError::fatal(format!("mega2 token file '{}' is empty", path.display()))
                .with_stable_code(StableErrorCode::CliInvalidArguments),
        );
    }
    Ok(trimmed.to_string())
}

/// Pure resolver over already-collected sources.
///
/// Returns `Ok(None)` when no source is configured (anonymous write attempt).
pub fn resolve_token(
    token_file: Option<&Path>,
    env_token: Option<&str>,
    flag_token: Option<&str>,
) -> CliResult<Option<Mega2Token>> {
    if let Some(path) = token_file {
        let raw = read_token_file(path)?;
        return Ok(Some(Mega2Token::new(raw)?));
    }
    if let Some(env) = env_token {
        let trimmed = env.trim();
        if trimmed.is_empty() {
            return Err(CliError::fatal(format!(
                "environment variable {ENV_TOKEN_VAR} is set but empty"
            ))
            .with_stable_code(StableErrorCode::CliInvalidArguments));
        }
        return Ok(Some(Mega2Token::new(trimmed)?));
    }
    if let Some(flag) = flag_token {
        let trimmed = flag.trim();
        if trimmed.is_empty() {
            return Err(CliError::fatal("--token value must not be empty")
                .with_stable_code(StableErrorCode::CliInvalidArguments));
        }
        return Ok(Some(Mega2Token::new(trimmed)?));
    }
    Ok(None)
}

/// Convenience wrapper reading [`ENV_TOKEN_VAR`] from the current process.
pub fn resolve_token_from_process(
    token_file: Option<&Path>,
    flag_token: Option<&str>,
) -> CliResult<(Option<Mega2Token>, Option<TokenSource>)> {
    let env = std::env::var(ENV_TOKEN_VAR).ok();
    let source = if token_file.is_some() {
        Some(TokenSource::TokenFile)
    } else if env.is_some() {
        Some(TokenSource::Environment)
    } else if flag_token.is_some() {
        Some(TokenSource::Flag)
    } else {
        None
    };
    let token = resolve_token(token_file, env.as_deref(), flag_token)?;
    Ok((token, source))
}

/// Test/token-file helper: the canonical path type for `--token-file`.
pub fn token_file_path(raw: &str) -> PathBuf {
    PathBuf::from(raw)
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use super::*;

    fn temp_token_file(contents: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("token");
        fs::write(&path, contents).expect("write token file");
        (dir, path)
    }

    #[test]
    fn precedence_prefers_file_then_env_then_flag() {
        let (_dir, path) = temp_token_file("file-token\n");

        let (token, source) =
            resolve_token_from_process(Some(&path), Some("flag-token")).expect("file wins");
        assert_eq!(token.expect("token").expose(), "file-token");
        assert_eq!(source, Some(TokenSource::TokenFile));

        let token = resolve_token(None, Some(" env-token "), Some("flag-token"))
            .expect("env wins")
            .expect("token");
        assert_eq!(token.expose(), "env-token");

        let token = resolve_token(None, None, Some(" flag-token "))
            .expect("flag")
            .expect("token");
        assert_eq!(token.expose(), "flag-token");

        assert!(resolve_token(None, None, None).expect("none").is_none());
    }

    #[test]
    fn token_files_reject_empty_content_and_nul() {
        let (_dir, empty) = temp_token_file("");
        let err = resolve_token(Some(&empty), None, None).expect_err("empty file refused");
        assert_eq!(err.stable_code(), StableErrorCode::CliInvalidArguments);

        let (_dir2, newline_only) = temp_token_file("\n");
        assert!(resolve_token(Some(&newline_only), None, None).is_err());

        let (_dir3, nul) = temp_token_file("tok\0en");
        assert!(resolve_token(Some(&nul), None, None).is_err());

        let (_dir4, too_long) = temp_token_file(&"a".repeat(MAX_TOKEN_BYTES + 1));
        assert!(resolve_token(Some(&too_long), None, None).is_err());

        assert!(Mega2Token::new("").is_err());
        assert!(Mega2Token::new("has space\nline").is_err());
    }

    #[test]
    fn token_value_is_redacted_in_debug_and_display() {
        let token = Mega2Token::new("super-secret-value").expect("token");
        let debug = format!("{token:?}");
        let display = format!("{token}");
        assert!(!debug.contains("super-secret-value"), "{debug}");
        assert!(!display.contains("super-secret-value"), "{display}");
        assert!(debug.contains("redacted"), "{debug}");
        // Only the explicit accessor reveals the value.
        assert_eq!(token.expose(), "super-secret-value");
    }
}
