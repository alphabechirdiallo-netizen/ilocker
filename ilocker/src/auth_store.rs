// ============================================================
//  auth_store.rs — local credential storage
//
//  File location: ~/.config/ilocker/auth.toml
//  Permissions:   0600 (owner read/write only, enforced on Unix)
//
//  Format (TOML):
//  ──────────────
//  server_url = "https://api.ilocker.dev"
//  email      = "dev@example.com"
//  cli_token  = "<opaque 64-char hex>"
//  jwt        = "<short-lived JWT — refreshed on every login>"
//  logged_in_at = "2026-05-26T10:00:00Z"
//
//  Security notes:
//    • On Unix the file is chmod 0600 at creation time.
//    • On macOS / Windows the OS Keychain / Credential Manager
//      would be the ideal vault for cli_token; that integration
//      is stubbed here and marked as Phase 4+.
//    • The JWT is short-lived (15 min) and is NOT sensitive on
//      its own — its only use is as a Bearer token for API calls.
//      The cli_token is the secret that must stay local.
// ============================================================

// ============================================================
//  auth_store.rs — local credential storage
//
//  File location: ~/.config/ilocker/auth.vault (chiffré)
//  Ancien format (< chiffrement du fallback) : auth.toml, en clair,
//  permissions 0600 — migré automatiquement et silencieusement vers
//  le nouveau format à la première lecture qui suit une mise à jour.
//
//  Chiffrement : ChaCha20-Poly1305 via credential_vault (même
//  primitive et même clé locale que github_store.rs / vercel_store.rs
//  / supabase_store.rs / cloud_store.rs — un seul mécanisme de
//  protection au repos pour tous les secrets locaux d'ilocker).
//
//  Format interne (avant chiffrement) : JSON, sérialisé depuis AuthFile.
//
//  Security notes:
//    • Le fichier chiffré reste protégé en permissions 0600 sur Unix.
//    • Sur macOS / Windows, une intégration Keychain / Credential
//      Manager resterait la meilleure option pour cli_token ; cette
//      intégration est différée (Phase 4+). En attendant, le fichier
//      chiffré est strictement au-dessus du TOML en clair précédent.
//    • Le JWT est de courte durée (15 min) et n'est pas sensible en
//      lui-même — seul cli_token doit rester local et protégé.
// ============================================================

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// ── Auth file schema ──────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthFile {
    /// Base URL of the ilocker Cloud API
    pub server_url:   String,
    pub email:        String,
    /// Long-lived opaque CLI token (SHA-256 of this is stored server-side)
    pub cli_token:    String,
    /// Short-lived JWT — used as Bearer for API calls, re-issued on each login
    pub jwt:          String,
    pub logged_in_at: String,
}

// ── Path helpers ──────────────────────────────────────────────

/// Returns ~/.config/ilocker/auth.vault (format actuel, chiffré)
pub fn auth_file_path() -> Result<PathBuf> {
    let config_dir = dirs_config()?;
    Ok(config_dir.join("ilocker").join("auth.vault"))
}

/// Returns ~/.config/ilocker/auth.toml (ancien format, en clair).
/// Utilisé uniquement pour la migration automatique.
fn auth_file_path_legacy() -> Result<PathBuf> {
    let config_dir = dirs_config()?;
    Ok(config_dir.join("ilocker").join("auth.toml"))
}

fn dirs_config() -> Result<PathBuf> {
    // XDG_CONFIG_HOME → fallback to ~/.config on Unix, %APPDATA% on Windows
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(xdg));
    }
    #[cfg(unix)]
    {
        let home = std::env::var("HOME")
            .context("HOME env var not set")?;
        return Ok(PathBuf::from(home).join(".config"));
    }
    #[cfg(windows)]
    {
        let appdata = std::env::var("APPDATA")
            .context("APPDATA env var not set")?;
        return Ok(PathBuf::from(appdata));
    }
    #[cfg(not(any(unix, windows)))]
    bail!("Unsupported platform for config dir resolution");
}

// ── Read / Write ──────────────────────────────────────────────

/// Migration automatique et transparente depuis l'ancien format TOML
/// en clair : si un fichier legacy existe et se parse correctement, il
/// est ré-écrit immédiatement au nouveau format chiffré, puis supprimé.
/// L'utilisateur n'a jamais besoin de relancer `iloc login`.
fn migrate_legacy_if_present() -> Option<AuthFile> {
    let legacy_path = auth_file_path_legacy().ok()?;
    let raw = std::fs::read_to_string(&legacy_path).ok()?;
    let auth: AuthFile = toml::from_str(&raw).ok()?;

    if save(&auth).is_ok() {
        let _ = std::fs::remove_file(&legacy_path);
    }
    Some(auth)
}

/// Load the auth file.  Returns Ok(None) if not logged in.
pub fn load() -> Result<Option<AuthFile>> {
    let path = auth_file_path()?;

    // Format actuel (chiffré).
    if let Ok(raw) = std::fs::read(&path) {
        if let Ok(json) = crate::credential_vault::decrypt_credential_bytes(&raw) {
            if let Ok(auth) = serde_json::from_slice::<AuthFile>(&json) {
                // Nettoyage défensif d'un éventuel ancien fichier
                // encore présent (migration précédente interrompue).
                if let Ok(legacy) = auth_file_path_legacy() {
                    let _ = std::fs::remove_file(legacy);
                }
                return Ok(Some(auth));
            }
        }
    }

    // Migration automatique depuis l'ancien format TOML en clair.
    if let Some(auth) = migrate_legacy_if_present() {
        return Ok(Some(auth));
    }

    if !path.exists() && !auth_file_path_legacy().map(|p| p.exists()).unwrap_or(false) {
        return Ok(None);
    }

    // Un fichier existe mais n'a pu être ni déchiffré ni migré : corrompu.
    anyhow::bail!("Auth file is malformed — try `iloc login` again")
}

/// Persist the auth file, chiffré, avec permissions strictes.
pub fn save(auth: &AuthFile) -> Result<()> {
    let path = auth_file_path()?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Cannot create config dir at {}", parent.display()))?;
    }

    let json = serde_json::to_vec(auth)
        .context("Failed to serialise auth file")?;
    let encrypted = crate::credential_vault::encrypt_credential_bytes(&json)?;

    // Write atomically via a temp file then rename
    let tmp = path.with_extension("vault.tmp");
    std::fs::write(&tmp, &encrypted)
        .with_context(|| format!("Cannot write to {}", tmp.display()))?;

    set_owner_only(&tmp)?;

    std::fs::rename(&tmp, &path)
        .with_context(|| format!("Cannot rename {} → {}", tmp.display(), path.display()))?;

    Ok(())
}

/// Remove the auth file (logout). Nettoie aussi un éventuel fichier
/// legacy encore présent, pour ne rien laisser en clair sur disque.
pub fn remove() -> Result<()> {
    let path = auth_file_path()?;
    if path.exists() {
        std::fs::remove_file(&path)
            .with_context(|| format!("Cannot remove auth file at {}", path.display()))?;
    }
    if let Ok(legacy) = auth_file_path_legacy() {
        let _ = std::fs::remove_file(legacy);
    }
    Ok(())
}

/// Returns the loaded auth, or a friendly error if not logged in.
pub fn require_auth() -> Result<AuthFile> {
    load()?.ok_or_else(|| anyhow::anyhow!(
        "Not logged in.  Run `iloc login` first."
    ))
}

// ── Platform-specific permission hardening ────────────────────

#[cfg(unix)]
fn set_owner_only(path: &PathBuf) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let perms = std::fs::Permissions::from_mode(0o600);
    std::fs::set_permissions(path, perms)
        .with_context(|| format!("Cannot set 0600 permissions on {}", path.display()))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_owner_only(_path: &PathBuf) -> Result<()> {
    // Windows: the file is in %APPDATA% which is already user-private.
    // A full ACL lockdown is deferred to Phase 4+ (Credential Manager integration).
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn use_temp_home() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("iloc_auth_test_{}", uuid_like()));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("HOME", &dir);
        std::env::set_var("USERPROFILE", &dir);
        std::env::remove_var("XDG_CONFIG_HOME");
        dir
    }

    fn uuid_like() -> String {
        format!(
            "{:x}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )
    }

    fn sample_auth() -> AuthFile {
        AuthFile {
            server_url: "https://api.ilocker.dev".to_string(),
            email: "dev@example.com".to_string(),
            cli_token: "abc123opaquetoken".to_string(),
            jwt: "eyFakeJwt".to_string(),
            logged_in_at: "2026-05-26T10:00:00Z".to_string(),
        }
    }

    #[test]
    fn round_trip() {
        let _home = use_temp_home();
        assert!(load().unwrap().is_none());
        save(&sample_auth()).unwrap();
        let loaded = load().unwrap().expect("devrait être connecté après save()");
        assert_eq!(loaded.cli_token, "abc123opaquetoken");
        assert_eq!(loaded.email, "dev@example.com");
        remove().unwrap();
        assert!(load().unwrap().is_none());
    }

    #[test]
    fn migrates_legacy_plaintext_toml_transparently() {
        let _home = use_temp_home();
        let legacy_path = auth_file_path_legacy().unwrap();
        std::fs::create_dir_all(legacy_path.parent().unwrap()).unwrap();
        std::fs::write(&legacy_path, toml::to_string_pretty(&sample_auth()).unwrap()).unwrap();

        let auth = load().unwrap().expect("devrait migrer depuis l'ancien TOML en clair");
        assert_eq!(auth.cli_token, "abc123opaquetoken");
        assert!(!legacy_path.exists(), "l'ancien TOML en clair doit être nettoyé après migration");
        assert!(auth_file_path().unwrap().exists(), "le nouveau fichier chiffré doit exister");

        // Second appel : relit bien depuis le nouveau format.
        let auth2 = load().unwrap().unwrap();
        assert_eq!(auth2.cli_token, "abc123opaquetoken");
    }

    #[test]
    fn require_auth_gives_clear_error_when_logged_out() {
        let _home = use_temp_home();
        let err = require_auth().unwrap_err();
        assert!(err.to_string().contains("iloc login"));
    }
}
