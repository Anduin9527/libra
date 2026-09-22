//! Vault integration module wrapping libvault for PGP key management.
//!
//! Provides helpers to initialize a libvault instance backed by the repository's
//! `.libra/vault.db` SQLite database, generate PGP keys, sign data, and verify
//! signatures. The vault state (sealed/unsealed) is managed transparently.
//!
//! # Secret handling
//!
//! The per-repository unseal key is stored hex-encoded in the user's home
//! directory (`~/.libra/vault-keys/<repo-id>`) — outside the repository — so
//! that anyone with read access to the repo config alone cannot recover the
//! root token. The global unseal key lives beside the global configuration
//! database it protects, at `<config dir>/libra/vault-unseal-key`
//! (plan-20260919 ADR-GCX-04); a pre-XDG `~/.libra/vault-unseal-key` is copied
//! there once and then kept, unmodified, as a downgrade backup. The root token is encrypted (AES-256-GCM) with a key derived from
//! the unseal key before being persisted in the repo config
//! (`vault.roottoken_enc`). It is never stored in plaintext.
//!
//! # Threat model
//!
//! This design protects against casual repo-level read access (e.g. a
//! colleague cloning the repo, or a backup leak). It does NOT protect
//! against full compromise of the user's machine — an attacker with access to
//! the repo plus the user's per-user state (`~/.libra/` for repository keys,
//! `<config dir>/libra/` for the global key) can recover the root token. For
//! stronger guarantees, integrate an OS keychain or hardware token.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result, anyhow};
use libvault::{RustyVault, core::SealConfig, storage::sql::sqlite::SqliteBackend};
use serde_json::Value;

use crate::utils::util::try_get_storage_path;

const VAULT_DB_NAME: &str = "vault.db";
const PGP_KEY_NAME: &str = "libra-signing";
const SSH_ROLE_NAME: &str = "libra-ssh";
const PKI_MOUNT_PATH: &str = "pki";

fn vault_home_dir() -> Option<PathBuf> {
    #[cfg(test)]
    if let Some(path) = std::env::var_os("LIBRA_TEST_HOME") {
        return Some(PathBuf::from(path));
    }

    dirs::home_dir()
}

// ── Encryption helpers for root token ──

/// Derive a 256-bit AES key from the raw unseal key using HKDF-SHA256.
fn derive_token_key(unseal_key: &[u8]) -> Result<ring::aead::LessSafeKey> {
    use ring::{aead, hkdf};
    let salt = hkdf::Salt::new(hkdf::HKDF_SHA256, b"libra-vault-token-enc");
    let prk = salt.extract(unseal_key);
    let okm = prk
        .expand(&[b"token-encryption"], &aead::AES_256_GCM)
        .map_err(|_| anyhow!("failed to derive vault token encryption key"))?;
    let key_bytes: aead::UnboundKey = okm.into();
    Ok(aead::LessSafeKey::new(key_bytes))
}

/// Encrypt `plaintext` with AES-256-GCM using a key derived from `unseal_key`.
/// Returns `nonce || ciphertext || tag` as a single byte vector.
pub fn encrypt_token(unseal_key: &[u8], plaintext: &[u8]) -> Result<Vec<u8>> {
    use ring::{
        aead,
        rand::{SecureRandom, SystemRandom},
    };

    let key = derive_token_key(unseal_key)?;
    let rng = SystemRandom::new();
    let mut nonce_bytes = [0u8; 12];
    rng.fill(&mut nonce_bytes)
        .map_err(|_| anyhow!("failed to generate nonce for vault token encryption"))?;
    let nonce = aead::Nonce::assume_unique_for_key(nonce_bytes);

    let mut in_out = plaintext.to_vec();
    key.seal_in_place_append_tag(nonce, aead::Aad::empty(), &mut in_out)
        .map_err(|_| anyhow!("failed to encrypt vault root token"))?;

    let mut result = nonce_bytes.to_vec();
    result.extend(in_out);
    Ok(result)
}

/// Decrypt `nonce || ciphertext || tag` with AES-256-GCM.
pub fn decrypt_token(unseal_key: &[u8], data: &[u8]) -> Result<String> {
    use ring::aead;

    if data.len() < 12 + aead::AES_256_GCM.tag_len() {
        return Err(anyhow!("encrypted token data too short"));
    }
    let (nonce_bytes, ciphertext_and_tag) = data.split_at(12);
    let nonce = aead::Nonce::try_assume_unique_for_key(nonce_bytes)
        .map_err(|_| anyhow!("invalid nonce"))?;
    let key = derive_token_key(unseal_key)?;
    let mut buf = ciphertext_and_tag.to_vec();
    let plaintext = key
        .open_in_place(nonce, aead::Aad::empty(), &mut buf)
        .map_err(|_| anyhow!("failed to decrypt root token — unseal key may be wrong"))?;
    String::from_utf8(plaintext.to_vec()).context("root token is not valid UTF-8")
}

/// Initialize a new vault instance backed by the given `.libra` directory.
///
/// Creates `vault.db` inside `root_dir`, initializes the vault with a single
/// unseal key (threshold=1, shares=1), mounts the PKI engine, and returns
/// `(unseal_key, encrypted_root_token)`.
#[allow(dead_code)]
pub async fn init_vault(root_dir: &Path) -> Result<(Vec<u8>, Vec<u8>)> {
    let vault = create_vault(root_dir).await?;

    let seal_config = SealConfig {
        secret_shares: 1,
        secret_threshold: 1,
    };
    let init_result = vault
        .init(&seal_config)
        .await
        .map_err(|e| anyhow!("vault init failed: {e}"))?;

    let unseal_key = init_result
        .secret_shares
        .first()
        .ok_or_else(|| anyhow!("no unseal key generated"))?
        .clone();

    let root_token = init_result.root_token.clone();

    vault
        .unseal(&[unseal_key.as_slice()])
        .await
        .map_err(|e| anyhow!("vault unseal failed: {e}"))?;

    vault.set_token(&root_token);

    let pki = PKI_MOUNT_PATH.to_string();
    vault
        .mount(Some(root_token.clone()), pki.clone(), pki)
        .await
        .map_err(|e| anyhow!("vault mount pki failed: {e}"))?;

    vault
        .seal()
        .await
        .map_err(|e| anyhow!("vault seal failed: {e}"))?;

    let enc_token = encrypt_token(&unseal_key, root_token.as_bytes())?;
    Ok((unseal_key, enc_token))
}

/// Generate a PGP key pair in the vault for commit signing.
#[allow(dead_code)]
pub async fn generate_pgp_key(
    root_dir: &Path,
    unseal_key: &[u8],
    user_name: &str,
    user_email: &str,
) -> Result<String> {
    let vault = create_vault(root_dir).await?;

    vault
        .unseal(&[unseal_key])
        .await
        .map_err(|e| anyhow!("vault unseal failed: {e}"))?;

    let root_token = recover_root_token(unseal_key).await?;
    vault.set_token(&root_token);

    let data = serde_json::json!({
        "key_name": PGP_KEY_NAME,
        "key_type": "pgp",
        "name": user_name,
        "email": user_email,
        "key_bits": 2048,
        "ttl": "3650d",
    });

    let resp = vault
        .write(
            Some(root_token),
            format!("{PKI_MOUNT_PATH}/keys/generate/internal"),
            data.as_object().cloned(),
        )
        .await
        .map_err(|e| anyhow!("vault pgp key generation failed: {e}"))?;

    let public_key = resp
        .and_then(|r| r.data)
        .and_then(|d| d.get("public_key").cloned())
        .and_then(|v| v.as_str().map(String::from))
        .ok_or_else(|| anyhow!("no public key in vault response"))?;

    // Store in config so it can be exported without requiring backend-specific
    // read-path support.
    upsert_config_value("vault.gpg.pubkey", &public_key).await;

    vault
        .seal()
        .await
        .map_err(|e| anyhow!("vault seal failed: {e}"))?;

    Ok(public_key)
}

/// Sign data using the vault's PGP key.
///
/// `data` is the raw bytes to sign. Returns the hex-encoded detached signature.
pub async fn pgp_sign(root_dir: &Path, unseal_key: &[u8], data: &[u8]) -> Result<String> {
    let vault = create_vault(root_dir).await?;

    vault
        .unseal(&[unseal_key])
        .await
        .map_err(|e| anyhow!("vault unseal failed: {e}"))?;

    let root_token = recover_root_token(unseal_key).await?;
    vault.set_token(&root_token);

    let data_hex = hex::encode(data);
    let req_data = serde_json::json!({
        "key_name": PGP_KEY_NAME,
        "data": data_hex,
    });

    let resp = vault
        .write(
            Some(root_token),
            format!("{PKI_MOUNT_PATH}/keys/sign"),
            req_data.as_object().cloned(),
        )
        .await
        .map_err(|e| anyhow!("vault pgp sign failed: {e}"))?;

    let signature_hex = resp
        .and_then(|r| r.data)
        .and_then(|d| d.get("signature").cloned())
        .and_then(|v| v.as_str().map(String::from))
        .ok_or_else(|| anyhow!("no signature in vault response"))?;

    vault
        .seal()
        .await
        .map_err(|e| anyhow!("vault seal failed: {e}"))?;

    Ok(signature_hex)
}

/// Verify a hex-encoded PGP `signature` over `data` using the vault PGP key.
/// Mirrors [`pgp_sign`] but calls the `keys/verify` endpoint and returns whether
/// the signature is valid.
pub async fn pgp_verify(
    root_dir: &Path,
    unseal_key: &[u8],
    data: &[u8],
    signature_hex: &str,
) -> Result<bool> {
    let vault = create_vault(root_dir).await?;

    vault
        .unseal(&[unseal_key])
        .await
        .map_err(|e| anyhow!("vault unseal failed: {e}"))?;

    let root_token = recover_root_token(unseal_key).await?;
    vault.set_token(&root_token);

    let req_data = serde_json::json!({
        "key_name": PGP_KEY_NAME,
        "data": hex::encode(data),
        "signature": signature_hex,
    });

    let resp = vault
        .write(
            Some(root_token),
            format!("{PKI_MOUNT_PATH}/keys/verify"),
            req_data.as_object().cloned(),
        )
        .await
        .map_err(|e| anyhow!("vault pgp verify failed: {e}"))?;

    // The PGP verify path returns `{valid: bool}`; the generic key path returns
    // `{result: bool}`. Accept either.
    let valid = resp
        .and_then(|r| r.data)
        .and_then(|d| {
            d.get("valid")
                .or_else(|| d.get("result"))
                .and_then(|v| v.as_bool())
        })
        .ok_or_else(|| anyhow!("no verification result in vault verify response"))?;

    vault
        .seal()
        .await
        .map_err(|e| anyhow!("vault seal failed: {e}"))?;

    Ok(valid)
}

/// Decode an ASCII-armored PGP signature block back into the hex-encoded
/// signature bytes (the inverse of [`signature_to_armored`]). Used to verify a
/// signature that was embedded in an annotated tag.
pub fn armored_to_signature_hex(armored: &str) -> Result<String> {
    use base64::{Engine, engine::general_purpose::STANDARD};

    let b64: String = armored
        .lines()
        .filter(|line| {
            !line.is_empty()
                && !line.starts_with("-----BEGIN PGP SIGNATURE-----")
                && !line.starts_with("-----END PGP SIGNATURE-----")
        })
        .collect();
    let sig_bytes = STANDARD
        .decode(b64.as_bytes())
        .context("failed to base64-decode armored signature")?;
    Ok(hex::encode(sig_bytes))
}

/// Generate an SSH key pair and return (public_key, private_key) without
/// storing them. The caller is responsible for per-remote storage.
#[allow(dead_code)]
pub async fn generate_ssh_key_pair(
    root_dir: &Path,
    unseal_key: &[u8],
    user_name: &str,
) -> Result<(String, String)> {
    let vault = create_vault(root_dir).await?;
    vault
        .unseal(&[unseal_key])
        .await
        .map_err(|e| anyhow!("vault unseal failed: {e}"))?;
    let root_token = recover_root_token(unseal_key).await?;
    vault.set_token(&root_token);

    // Configure SSH CA
    let ca_data = serde_json::json!({ "key_type": "ed25519" });
    vault
        .write(
            Some(root_token.clone()),
            format!("{PKI_MOUNT_PATH}/config/ca/ssh"),
            ca_data.as_object().cloned(),
        )
        .await
        .map_err(|e| anyhow!("vault SSH CA configuration failed: {e}"))?;

    // Create SSH role
    let role_data = serde_json::json!({
        "key_type": "rsa",
        "key_bits": 3072,
        "cert_type_ssh": "user",
        "default_user": "git",
        "allowed_users": "git",
        "ttl": "3650d",
        "max_ttl": "3650d",
    });
    vault
        .write(
            Some(root_token.clone()),
            format!("{PKI_MOUNT_PATH}/roles/ssh/{SSH_ROLE_NAME}"),
            role_data.as_object().cloned(),
        )
        .await
        .map_err(|e| anyhow!("vault SSH role creation failed: {e}"))?;

    // Issue SSH certificate
    let issue_data = serde_json::json!({
        "key_type": "rsa",
        "key_bits": 3072,
        "valid_principals": ["git"],
        "ttl": "3650d",
        "key_id": format!("libra-{user_name}"),
    });
    let resp = vault
        .write(
            Some(root_token),
            format!("{PKI_MOUNT_PATH}/issue/ssh/{SSH_ROLE_NAME}"),
            issue_data.as_object().cloned(),
        )
        .await
        .map_err(|e| anyhow!("vault SSH key issuance failed: {e}"))?;

    let data = resp
        .and_then(|r| r.data)
        .ok_or_else(|| anyhow!("no data in vault SSH issue response"))?;
    let private_key = data
        .get("private_key")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("no private_key in vault SSH response"))?
        .to_string();
    let public_key = data
        .get("public_key")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("no public_key in vault SSH response"))?
        .to_string();

    vault
        .seal()
        .await
        .map_err(|e| anyhow!("vault seal failed: {e}"))?;

    Ok((public_key, private_key))
}

/// Get the path to the SSH private key file for the current repo.
pub async fn ssh_key_path() -> Result<std::path::PathBuf> {
    use crate::internal::config::ConfigKv;
    let home = vault_home_dir().ok_or_else(|| anyhow!("cannot determine home directory"))?;
    let repo_id = ConfigKv::get("libra.repoid")
        .await?
        .map(|e| e.value)
        .ok_or_else(|| anyhow!("libra.repoid not set — was the repo initialized?"))?;
    Ok(home
        .join(".libra")
        .join("ssh-keys")
        .join(repo_id)
        .join("id_ed25519"))
}

/// Convert a hex-encoded PGP detached signature into an armored PGP signature
/// string suitable for embedding in a Git/Libra commit object.
/// Build the ASCII-armored PGP signature block from a hex-encoded signature.
/// Shared by commit `gpgsig` headers and annotated-tag signatures (which append
/// this block verbatim to the tag message rather than indenting it as a header).
pub fn signature_to_armored(signature_hex: &str) -> Result<String> {
    use base64::{Engine, engine::general_purpose::STANDARD};

    let sig_bytes = hex::decode(signature_hex).context("failed to decode signature hex")?;

    let b64 = STANDARD.encode(&sig_bytes);
    let mut armored = String::from("-----BEGIN PGP SIGNATURE-----\n\n");
    for chunk in b64.as_bytes().chunks(76) {
        let line = std::str::from_utf8(chunk).context("base64 signature chunk is not UTF-8")?;
        armored.push_str(line);
        armored.push('\n');
    }
    armored.push_str("-----END PGP SIGNATURE-----");
    Ok(armored)
}

pub fn signature_to_gpgsig(signature_hex: &str) -> Result<String> {
    let armored = signature_to_armored(signature_hex)?;

    let mut gpgsig = String::from("gpgsig ");
    for (i, line) in armored.lines().enumerate() {
        if i > 0 {
            gpgsig.push_str("\n ");
        }
        gpgsig.push_str(line);
    }

    Ok(gpgsig)
}

/// Load the unseal key for a specific configuration scope.
/// - Local scope: reads from `~/.libra/vault-keys/<repo-id>`
/// - Global scope: reads from `<config dir>/libra/vault-unseal-key`
///
/// A global key that exists but cannot be trusted (unreadable, malformed, or
/// present in two locations with different contents) yields `None` together
/// with an actionable warning. It must never be papered over by generating a
/// replacement: see [`lazy_init_vault_for_scope`], which fails instead.
pub async fn load_unseal_key_for_scope(scope: &str) -> Option<Vec<u8>> {
    match scope {
        "global" => match load_global_unseal_key().await {
            Ok(key) => key,
            Err(error) => {
                warn_about_the_global_key_once(format!("{error:#}"));
                None
            }
        },
        _ => load_unseal_key().await, // "local" or default
    }
}

/// Load the local unseal key for the repository backed by `db_path`.
///
/// This is used when callers need to resolve local secrets for an explicit
/// repository target instead of the current working directory repository.
pub async fn load_unseal_key_for_db_path(db_path: &Path) -> Option<Vec<u8>> {
    if let Ok(repo_id) = repo_id_for_db_path(db_path).await
        && let Some(hex_key) = load_unseal_key_from_home_for_repo_id(&repo_id).await
    {
        return hex::decode(hex_key).ok();
    }

    use crate::internal::{config::ConfigKv, db::get_db_conn_instance_for_path};
    let conn = get_db_conn_instance_for_path(db_path).await.ok()?;
    let entry = ConfigKv::get_with_conn(&conn, "vault.unsealkey")
        .await
        .ok()??;
    hex::decode(entry.value).ok()
}

/// File name of the global unseal key in both the new and the legacy layout.
pub(crate) const GLOBAL_UNSEAL_KEY_FILE: &str = "vault-unseal-key";
/// AES-256-GCM key material length. A file of any other length is a corrupt
/// key, never an invitation to generate a new one.
const UNSEAL_KEY_LEN: usize = 32;

/// The directory holding the global unseal key: the same user configuration
/// directory the global config database lives in (ADR-GCX-04), so the key and
/// the values it protects share one domain.
pub(crate) fn global_unseal_key_path() -> Option<PathBuf> {
    #[cfg(test)]
    if let Some(path) = std::env::var_os("LIBRA_TEST_HOME") {
        return Some(
            PathBuf::from(path)
                .join(".config")
                .join("libra")
                .join(GLOBAL_UNSEAL_KEY_FILE),
        );
    }
    crate::internal::config::global_config_dir().map(|dir| dir.join(GLOBAL_UNSEAL_KEY_FILE))
}

/// The key file Libra actually uses right now: the configuration-directory one
/// when it exists, otherwise the legacy file while it is still the active key.
///
/// Callers that harden permissions use this so a migrated legacy file — a
/// backup Libra no longer reads — is left exactly as the user left it.
pub(crate) fn active_global_unseal_key_path() -> Option<PathBuf> {
    let new_path = global_unseal_key_path();
    if new_path.as_deref().is_some_and(Path::exists) {
        return new_path;
    }
    legacy_global_unseal_key_path()
        .filter(|path| path.exists())
        .or(new_path)
}

/// The pre-XDG location. Still read (and copied forward) so no existing key —
/// and therefore no existing ciphertext — is ever lost.
pub(crate) fn legacy_global_unseal_key_path() -> Option<PathBuf> {
    vault_home_dir().map(|home| home.join(".libra").join(GLOBAL_UNSEAL_KEY_FILE))
}

/// Read one hex-encoded key file.
///
/// `Ok(None)` means "absent", which is the ONLY state that permits generating
/// a key. Every other problem is an error: silently treating an unreadable or
/// malformed key as absent would rotate the key and make every value already
/// encrypted with it permanently undecryptable (GC-GCX-03).
async fn read_unseal_key_file(path: &Path) -> Result<Option<Vec<u8>>> {
    let raw = match tokio::fs::read_to_string(path).await {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(anyhow!(
                "cannot read the global vault key '{}': {error}; fix the file's permissions \
                 (it must be readable by you and mode 0600) — it must not be deleted or \
                 regenerated, or every encrypted global value becomes unreadable",
                path.display()
            ));
        }
    };
    let key = hex::decode(raw.trim()).map_err(|error| {
        anyhow!(
            "the global vault key '{}' is not valid hex ({error}); restore it from a backup — \
             replacing it makes every encrypted global value unreadable",
            path.display()
        )
    })?;
    if key.len() != UNSEAL_KEY_LEN {
        return Err(anyhow!(
            "the global vault key '{}' is {} bytes instead of {UNSEAL_KEY_LEN}; restore it from \
             a backup — replacing it makes every encrypted global value unreadable",
            path.display(),
            key.len()
        ));
    }
    Ok(Some(key))
}

/// Load the global unseal key, moving it out of the legacy Libra home once.
///
/// Boundary conditions (ADR-GCX-04):
/// - the configuration-directory file wins; a legacy-only key is copied there
///   and the legacy file is then kept, unmodified, as a downgrade backup;
/// - two files with DIFFERENT contents fail closed, because picking either one
///   would make the values encrypted with the other unreadable;
/// - a failed copy is not fatal: the identical legacy key stays in use and the
///   caller is warned;
/// - `Ok(None)` means no key exists anywhere yet.
async fn load_global_unseal_key() -> Result<Option<Vec<u8>>> {
    let new_path = global_unseal_key_path();
    let legacy_path = legacy_global_unseal_key_path();

    // The configuration-directory file is authoritative, so a problem reading
    // it is fatal.
    let new_key = match new_path.as_deref() {
        Some(path) => read_unseal_key_file(path).await?,
        None => None,
    };
    let legacy_key = match legacy_path.as_deref() {
        Some(path) => match read_unseal_key_file(path).await {
            Ok(key) => key,
            // Once the key has moved, the legacy file is only a backup. An
            // unreadable backup means the conflict check cannot run — it does
            // not mean the command should fail.
            Err(error) if new_key.is_some() => {
                tracing::debug!(
                    path = %path.display(),
                    error = %format!("{error:#}"),
                    "ignoring an unreadable legacy global vault key backup"
                );
                None
            }
            Err(error) => return Err(error),
        },
        None => None,
    };

    match (new_key, legacy_key) {
        (Some(new), Some(legacy)) if new != legacy => Err(anyhow!(
            "two different global vault keys exist: '{}' and '{}'. Libra refuses to guess which \
             one protects your encrypted global values — keep the one that decrypts them and \
             move the other aside, then re-run",
            new_path
                .as_deref()
                .map(|path| path.display().to_string())
                .unwrap_or_default(),
            legacy_path
                .as_deref()
                .map(|path| path.display().to_string())
                .unwrap_or_default(),
        )),
        (Some(new), _) => Ok(Some(new)),
        (None, Some(legacy)) => {
            if let Some(path) = new_path.as_deref()
                && let Err(error) = write_unseal_key_file(path, &legacy).await
            {
                // The key itself is unchanged, so the command can proceed on
                // the legacy file; only the relocation is postponed.
                warn_about_the_global_key_once(format!(
                    "could not move the global vault key to '{}': {error:#}; still using '{}'",
                    path.display(),
                    legacy_path
                        .as_deref()
                        .map(|path| path.display().to_string())
                        .unwrap_or_default()
                ));
            }
            Ok(Some(legacy))
        }
        (None, None) => Ok(None),
    }
}

/// One command can resolve the global key many times — `config list --global`
/// decrypts every encrypted row — so a standing problem with it is reported
/// once, not once per value.
fn warn_about_the_global_key_once(message: String) {
    static REPORTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if REPORTED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    crate::utils::error::emit_warning(message);
}

/// Write a key file atomically with owner-only permissions.
///
/// The temporary file carries the final mode BEFORE the rename, so the key is
/// never observable through a world-readable file, not even briefly.
async fn write_unseal_key_file(path: &Path, unseal_key: &[u8]) -> Result<()> {
    let dir = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| anyhow!("'{}' has no parent directory", path.display()))?;
    tokio::fs::create_dir_all(dir)
        .await
        .with_context(|| format!("failed to create '{}'", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Best effort: an inherited directory keeps the mode the user chose.
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }

    let staging = dir.join(format!(
        ".{}.{}.tmp",
        GLOBAL_UNSEAL_KEY_FILE,
        std::process::id()
    ));
    tokio::fs::write(&staging, hex::encode(unseal_key))
        .await
        .with_context(|| format!("failed to write '{}'", staging.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(error) =
            std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o600))
        {
            let _ = tokio::fs::remove_file(&staging).await;
            return Err(anyhow::Error::new(error).context(format!(
                "failed to restrict '{}' to the owner",
                staging.display()
            )));
        }
    }
    if let Err(error) = tokio::fs::rename(&staging, path).await {
        let _ = tokio::fs::remove_file(&staging).await;
        return Err(
            anyhow::Error::new(error).context(format!("failed to publish '{}'", path.display()))
        );
    }
    Ok(())
}

/// Store the global unseal key in the user configuration directory (0600).
async fn store_global_unseal_key(unseal_key: &[u8]) -> Result<()> {
    let path = global_unseal_key_path()
        .ok_or_else(|| anyhow!("cannot determine the user configuration directory"))?;
    write_unseal_key_file(&path, unseal_key).await
}

/// Lazy-initialize vault for a given scope and return the unseal key.
/// For local scope, initializes the repo vault (.libra/vault.db).
/// For global scope, creates a standalone AES key at
/// `<config dir>/libra/vault-unseal-key`.
pub async fn lazy_init_vault_for_scope(scope: &str) -> Result<Vec<u8>> {
    match scope {
        "global" => {
            // ACTUALLY lazy: reuse the persisted key when one exists —
            // regenerating on every call would rotate the key and make every
            // previously-encrypted global value (auth tokens, encrypted
            // config) permanently undecryptable.
            // A key that exists but cannot be trusted is an ERROR, not a
            // reason to make a new one: `load_global_unseal_key` already
            // rejects an unreadable, malformed or conflicting key, and that
            // error must propagate rather than fall through to generation.
            if let Some(existing) = load_global_unseal_key().await? {
                return Ok(existing);
            }
            use ring::rand::{SecureRandom, SystemRandom};
            let rng = SystemRandom::new();
            let mut key = vec![0u8; 32];
            rng.fill(&mut key)
                .map_err(|_| anyhow!("failed to generate random key"))?;
            store_global_unseal_key(&key).await?;
            Ok(key)
        }
        _ => {
            // Local scope: use the full vault init
            let storage =
                try_get_storage_path(None).map_err(|_| anyhow!("not a libra repository"))?;
            let (unseal_key, enc_token) = init_vault(&storage).await?;
            store_credentials(&unseal_key, &enc_token).await?;
            Ok(unseal_key)
        }
    }
}

/// Read the stored unseal key from the user's home directory.
///
/// The key is stored at `~/.libra/vault-keys/<repo-id>` to keep it
/// separate from the repository config (where the encrypted root token
/// lives). Falls back to the legacy repo-config location
/// (`vault.unsealkey`) for backwards compatibility.
pub async fn load_unseal_key() -> Option<Vec<u8>> {
    // Try the new location first: ~/.libra/vault-keys/<repo-id>
    if let Some(hex_key) = load_unseal_key_from_home().await {
        return hex::decode(hex_key).ok();
    }
    // Fallback: legacy repo-config location
    use crate::internal::config::ConfigKv;
    let entry = ConfigKv::get("vault.unsealkey").await.ok()??;
    hex::decode(entry.value).ok()
}

/// Store the unseal key in `~/.libra/vault-keys/<repo-id>` and the
/// encrypted root token in the repo config.
#[allow(dead_code)]
pub async fn store_credentials(unseal_key: &[u8], encrypted_token: &[u8]) -> Result<()> {
    use crate::internal::config::ConfigKv;
    // Store unseal key outside the repo; do not silently downgrade to repo config.
    store_unseal_key_to_home(unseal_key)
        .await
        .context("failed to store vault unseal key in ~/.libra/")?;

    // Clean up any legacy insecure storage if present.
    let _ = ConfigKv::unset_all("vault.unsealkey").await;

    // Encrypted token always goes in repo config
    ConfigKv::set("vault.roottoken_enc", &hex::encode(encrypted_token), false)
        .await
        .context("failed to store encrypted root token")?;
    Ok(())
}

/// Remove previously stored vault credentials.
///
/// Used to rollback when vault initialization partially succeeds (e.g. credentials
/// are stored but PGP key generation fails).
#[allow(dead_code)]
pub async fn remove_credentials() {
    use crate::internal::config::ConfigKv;
    // Remove from home dir
    let _ = remove_unseal_key_from_home().await;
    // Remove legacy repo-config entries
    let _ = ConfigKv::unset_all("vault.unsealkey").await;
    let _ = ConfigKv::unset_all("vault.roottoken_enc").await;
}

// ── Internal helpers ──

async fn create_vault(root_dir: &Path) -> Result<RustyVault> {
    let db_path = root_dir.join(VAULT_DB_NAME);
    let table_name = "vault".to_string();
    let mut conf = HashMap::new();
    conf.insert(
        "filename".to_string(),
        Value::String(db_path.to_string_lossy().to_string()),
    );
    conf.insert("create_if_missing".to_string(), Value::Bool(true));
    conf.insert("timeout".to_string(), Value::String("5s".to_string()));
    conf.insert("table".to_string(), Value::String(table_name.clone()));

    let backend = Arc::new(
        SqliteBackend::new(&conf)
            .await
            .map_err(|e| anyhow!("vault sqlite backend creation failed: {e}"))?,
    );

    let vault =
        RustyVault::new(backend, None).map_err(|e| anyhow!("vault creation failed: {e}"))?;

    Ok(vault)
}

async fn upsert_config_value(dotted_key: &str, value: &str) {
    use crate::internal::config::ConfigKv;
    // set does upsert for single-value keys; ignore errors for vault internals
    let _ = ConfigKv::set(dotted_key, value, false).await;
}

/// Recover the root token by decrypting the stored encrypted token with the unseal key.
async fn recover_root_token(unseal_key: &[u8]) -> Result<String> {
    use crate::internal::config::ConfigKv;
    let enc_hex = ConfigKv::get("vault.roottoken_enc")
        .await?
        .map(|e| e.value)
        .ok_or_else(|| anyhow!("vault encrypted root token not found in config"))?;
    let enc_bytes = hex::decode(&enc_hex).context("failed to decode encrypted root token hex")?;
    decrypt_token(unseal_key, &enc_bytes)
}

// ── Home-directory unseal key storage ──

/// Resolve the path `~/.libra/vault-keys/<repo-id>` for the current repo.
async fn unseal_key_path() -> Result<std::path::PathBuf> {
    let repo_id = current_repo_id().await?;
    unseal_key_path_for_repo_id(&repo_id)
}

async fn current_repo_id() -> Result<String> {
    use crate::internal::config::ConfigKv;
    ConfigKv::get("libra.repoid")
        .await?
        .map(|e| e.value)
        .ok_or_else(|| anyhow!("libra.repoid not set — was the repo initialized?"))
}

async fn repo_id_for_db_path(db_path: &Path) -> Result<String> {
    use crate::internal::{config::ConfigKv, db::get_db_conn_instance_for_path};
    let conn = get_db_conn_instance_for_path(db_path)
        .await
        .context("failed to open repository config database")?;
    ConfigKv::get_with_conn(&conn, "libra.repoid")
        .await?
        .map(|e| e.value)
        .ok_or_else(|| anyhow!("libra.repoid not set — was the repo initialized?"))
}

fn unseal_key_path_for_repo_id(repo_id: &str) -> Result<std::path::PathBuf> {
    let home = vault_home_dir().ok_or_else(|| anyhow!("cannot determine home directory"))?;
    Ok(home.join(".libra").join("vault-keys").join(repo_id))
}

/// Read the hex-encoded unseal key from `~/.libra/vault-keys/<repo-id>`.
async fn load_unseal_key_from_home() -> Option<String> {
    let path = unseal_key_path().await.ok()?;
    load_unseal_key_from_home_for_repo_id_path(&path).await
}

async fn load_unseal_key_from_home_for_repo_id(repo_id: &str) -> Option<String> {
    let path = unseal_key_path_for_repo_id(repo_id).ok()?;
    load_unseal_key_from_home_for_repo_id_path(&path).await
}

async fn load_unseal_key_from_home_for_repo_id_path(path: &Path) -> Option<String> {
    tokio::fs::read_to_string(path)
        .await
        .ok()
        .map(|s| s.trim().to_string())
}

/// Write the unseal key (hex) to `~/.libra/vault-keys/<repo-id>` with
/// restrictive permissions (owner-only on Unix).
async fn store_unseal_key_to_home(unseal_key: &[u8]) -> Result<()> {
    let path = unseal_key_path().await?;
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .context("failed to create ~/.libra/vault-keys/")?;
        // Restrict directory permissions on Unix
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o700);
            std::fs::set_permissions(parent, perms).with_context(|| {
                format!("failed to set permissions to 700 on '{}'", parent.display())
            })?;
        }
    }
    tokio::fs::write(&path, hex::encode(unseal_key))
        .await
        .context("failed to write unseal key")?;
    // Restrict file permissions on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        std::fs::set_permissions(&path, perms).with_context(|| {
            format!(
                "failed to set permissions to 600 on unseal key '{}'",
                path.display()
            )
        })?;
    }
    Ok(())
}

/// Remove the unseal key file from `~/.libra/vault-keys/<repo-id>`.
async fn remove_unseal_key_from_home() -> Result<()> {
    let path = unseal_key_path().await?;
    if path.exists() {
        tokio::fs::remove_file(&path)
            .await
            .context("failed to remove unseal key file")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::load_unseal_key_for_db_path;
    use crate::internal::{
        config::ConfigKv,
        db::{create_database, reset_db_conn_instance_for_path},
    };

    /// plan-20260919 GCX-03: the global key resolution and its one-time move
    /// out of the legacy Libra home. `LIBRA_TEST_HOME` redirects both layouts,
    /// so these never touch the developer's real key.
    mod global_unseal_key {
        use std::path::Path;

        use crate::{
            internal::vault::{
                UNSEAL_KEY_LEN, decrypt_token, encrypt_token, global_unseal_key_path,
                lazy_init_vault_for_scope, legacy_global_unseal_key_path,
                load_unseal_key_for_scope,
            },
            utils::test::ScopedEnvVar,
        };

        fn key(seed: u8) -> Vec<u8> {
            vec![seed; UNSEAL_KEY_LEN]
        }

        fn write_key(path: &Path, value: &[u8]) {
            std::fs::create_dir_all(path.parent().expect("key parent")).expect("create key dir");
            std::fs::write(path, hex::encode(value)).expect("write key");
        }

        struct Home {
            _env: ScopedEnvVar,
            _root: tempfile::TempDir,
            new_path: std::path::PathBuf,
            legacy_path: std::path::PathBuf,
        }

        fn isolated_home() -> Home {
            let root = tempfile::tempdir().expect("tempdir");
            let env = ScopedEnvVar::set("LIBRA_TEST_HOME", root.path());
            let new_path = global_unseal_key_path().expect("new key path");
            let legacy_path = legacy_global_unseal_key_path().expect("legacy key path");
            assert!(new_path.starts_with(root.path()), "{new_path:?}");
            assert!(legacy_path.starts_with(root.path()), "{legacy_path:?}");
            Home {
                _env: env,
                _root: root,
                new_path,
                legacy_path,
            }
        }

        /// A legacy-only key is adopted, copied into the configuration
        /// directory, and the legacy file is left exactly as it was.
        #[tokio::test]
        #[serial_test::serial(env)]
        async fn legacy_key_is_copied_forward_and_left_untouched() {
            let home = isolated_home();
            write_key(&home.legacy_path, &key(0xAB));
            let legacy_bytes = std::fs::read(&home.legacy_path).expect("read legacy");

            let loaded = load_unseal_key_for_scope("global").await;

            assert_eq!(loaded, Some(key(0xAB)));
            assert!(home.new_path.exists(), "the key must be copied forward");
            assert_eq!(
                std::fs::read(&home.new_path).expect("read new"),
                legacy_bytes
            );
            assert_eq!(
                std::fs::read(&home.legacy_path).expect("read legacy"),
                legacy_bytes,
                "the legacy key file must not be rewritten"
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = std::fs::metadata(&home.new_path)
                    .expect("stat new key")
                    .permissions()
                    .mode()
                    & 0o777;
                assert_eq!(mode, 0o600, "the key must stay owner-only");
                let dir_mode = std::fs::metadata(home.new_path.parent().expect("parent"))
                    .expect("stat key dir")
                    .permissions()
                    .mode()
                    & 0o777;
                assert_eq!(dir_mode, 0o700);
            }
        }

        /// The configuration-directory key wins and the legacy file is ignored.
        #[tokio::test]
        #[serial_test::serial(env)]
        async fn new_key_wins_over_an_identical_legacy_copy() {
            let home = isolated_home();
            write_key(&home.new_path, &key(0x11));
            write_key(&home.legacy_path, &key(0x11));

            assert_eq!(load_unseal_key_for_scope("global").await, Some(key(0x11)));
            assert_eq!(
                std::fs::read_to_string(&home.legacy_path).expect("read legacy"),
                hex::encode(key(0x11))
            );
        }

        /// Two DIFFERENT keys fail closed. Choosing either one would make the
        /// values encrypted with the other permanently unreadable, and
        /// generating a third is worse still (GC-GCX-03).
        #[tokio::test]
        #[serial_test::serial(env)]
        async fn conflicting_keys_fail_closed_without_rotating() {
            let home = isolated_home();
            write_key(&home.new_path, &key(0x11));
            write_key(&home.legacy_path, &key(0x22));

            assert_eq!(load_unseal_key_for_scope("global").await, None);
            let error = lazy_init_vault_for_scope("global")
                .await
                .expect_err("a key conflict must fail closed");
            let rendered = format!("{error:#}");
            assert!(
                rendered.contains("two different global vault keys exist"),
                "unexpected error: {rendered}"
            );
            assert_eq!(
                std::fs::read_to_string(&home.new_path).expect("read new"),
                hex::encode(key(0x11)),
                "no key may be rewritten"
            );
            assert_eq!(
                std::fs::read_to_string(&home.legacy_path).expect("read legacy"),
                hex::encode(key(0x22))
            );
        }

        /// Once the key has moved, an unreadable legacy backup is just a
        /// backup: it cannot be compared, but it must not break the command.
        /// (A directory at the key path is a deterministic "unreadable" that
        /// does not depend on the test user's privileges.)
        #[tokio::test]
        #[serial_test::serial(env)]
        async fn an_unreadable_legacy_backup_is_ignored_once_the_key_has_moved() {
            let home = isolated_home();
            write_key(&home.new_path, &key(0x33));
            std::fs::create_dir_all(&home.legacy_path).expect("occupy the legacy path");

            assert_eq!(load_unseal_key_for_scope("global").await, Some(key(0x33)));
        }

        /// The same unreadable file WITHOUT a new key is fatal: it is the only
        /// candidate, and generating a replacement would rotate the key.
        #[tokio::test]
        #[serial_test::serial(env)]
        async fn an_unreadable_legacy_key_fails_closed_when_it_is_the_only_one() {
            let home = isolated_home();
            std::fs::create_dir_all(&home.legacy_path).expect("occupy the legacy path");

            assert_eq!(load_unseal_key_for_scope("global").await, None);
            let error = lazy_init_vault_for_scope("global")
                .await
                .expect_err("an unreadable sole key must fail closed");
            assert!(
                format!("{error:#}").contains("cannot read the global vault key"),
                "{error:#}"
            );
            assert!(
                !home.new_path.exists(),
                "no replacement key may be generated"
            );
        }

        /// A malformed key is an error, never an invitation to generate one.
        #[tokio::test]
        #[serial_test::serial(env)]
        async fn a_short_key_fails_closed_instead_of_rotating() {
            let home = isolated_home();
            write_key(&home.new_path, &[0x01, 0x02, 0x03]);

            let error = lazy_init_vault_for_scope("global")
                .await
                .expect_err("a malformed key must fail closed");
            let rendered = format!("{error:#}");
            assert!(rendered.contains("3 bytes instead of 32"), "{rendered}");
            assert_eq!(
                std::fs::read_to_string(&home.new_path).expect("read new"),
                hex::encode([0x01, 0x02, 0x03])
            );
        }

        /// With no key anywhere, one is generated — into the configuration
        /// directory, not the legacy Libra home.
        #[tokio::test]
        #[serial_test::serial(env)]
        async fn a_generated_key_lands_in_the_configuration_directory() {
            let home = isolated_home();

            let generated = lazy_init_vault_for_scope("global")
                .await
                .expect("generate a global key");

            assert_eq!(generated.len(), UNSEAL_KEY_LEN);
            assert!(home.new_path.exists());
            assert!(!home.legacy_path.exists(), "the legacy path stays absent");
            assert_eq!(
                lazy_init_vault_for_scope("global").await.expect("reload"),
                generated,
                "a second call must not rotate the key"
            );
        }

        /// The point of the whole card: a value encrypted before the move is
        /// still decryptable after it.
        #[tokio::test]
        #[serial_test::serial(env)]
        async fn values_encrypted_under_the_legacy_key_still_decrypt_after_the_move() {
            let home = isolated_home();
            write_key(&home.legacy_path, &key(0x5A));
            let ciphertext = encrypt_token(&key(0x5A), b"global-secret").expect("encrypt");

            let migrated = load_unseal_key_for_scope("global")
                .await
                .expect("load the migrated key");

            assert!(home.new_path.exists());
            assert_eq!(
                decrypt_token(&migrated, &ciphertext).expect("decrypt"),
                "global-secret"
            );
        }
    }

    #[tokio::test]
    async fn load_unseal_key_for_db_path_falls_back_to_legacy_db_key_without_repo_id() {
        let temp = tempdir().expect("failed to create temp dir");
        let db_path = temp.path().join("libra.db");
        let expected = vec![0x12, 0x34, 0x56, 0x78];

        let conn = create_database(db_path.to_string_lossy().as_ref())
            .await
            .expect("failed to create test database");
        ConfigKv::set_with_conn(&conn, "vault.unsealkey", &hex::encode(&expected), false)
            .await
            .expect("failed to seed legacy vault.unsealkey");
        drop(conn);

        let actual = load_unseal_key_for_db_path(&db_path).await;
        assert_eq!(actual, Some(expected));

        reset_db_conn_instance_for_path(&db_path).await;
    }
}
