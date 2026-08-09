// ============================================================
//  github_store.rs — Stockage des tokens GitHub (multi-profils)
//
//  Même architecture que cloud_store.rs :
//    Layer 1 — ~/.config/ilocker/github.toml   (non-sensible)
//              login, scopes, default_org, created_at
//    Layer 2 — OS native keychain               (sensible)
//              access_token (PAT ou token OAuth)
//
//  Un développeur peut avoir PLUSIEURS comptes GitHub (perso +
//  boulot + client) — chacun est un profil nommé.
//
//  Commandes concernées :
//    iloc connect github           → assistant interactif
//    iloc github <sous-commande>   → profil actif par défaut
//    --github-profile <nom>        → profil explicite
// ============================================================

use anyhow::{Context, Result};
use keyring::Entry;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const KR_SERVICE: &str = "ilocker-github";
const KR_TOKEN_KEY: &str = "access_token";

// ── Profil GitHub ─────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubProfile {
    /// Nom local (ex: "perso", "travail", "client-x")
    pub name: String,
    /// Login GitHub (@username)
    pub login: String,
    /// Organisation par défaut (pour `iloc repo create`)
    pub default_org: Option<String>,
    /// Scopes accordés au token (ex: ["repo","workflow"])
    pub scopes: Vec<String>,
    /// URL de l'API (https://api.github.com ou GitHub Enterprise)
    pub api_url: String,
    /// Clé unique pour le trousseau (jamais réutilisée même si nom change)
    pub account: String,
    /// Date de connexion
    pub connected_at: String,
}

// ── Fichier de config complet ─────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GitHubProfiles {
    pub active: Option<String>,
    #[serde(default)]
    pub profiles: Vec<GitHubProfile>,
}

// ── Credentials résolus (avec token) ─────────────────────────

#[derive(Debug, Clone)]
pub struct GitHubCredentials {
    pub profile_name: String,
    pub login:        String,
    pub default_org:  Option<String>,
    pub api_url:      String,
    pub token:        String,
}

// ── Chemin du fichier de config ──────────────────────────────

pub fn github_config_path() -> Result<PathBuf> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .context("Cannot determine home directory")?;
    Ok(PathBuf::from(home)
        .join(".config")
        .join("ilocker")
        .join("github.toml"))
}

// ── Chargement ───────────────────────────────────────────────

pub fn load_profiles() -> Result<GitHubProfiles> {
    let path = github_config_path()?;
    if !path.exists() {
        return Ok(GitHubProfiles::default());
    }
    let raw = std::fs::read_to_string(&path)?;
    Ok(toml::from_str::<GitHubProfiles>(&raw).unwrap_or_default())
}

pub fn save_profiles(cfg: &GitHubProfiles) -> Result<()> {
    let path = github_config_path()?;
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    let raw = toml::to_string_pretty(cfg)?;
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, raw)?;
    set_owner_only(&tmp)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

// ── Résolution d'un profil ────────────────────────────────────

pub fn resolve_profile(name: Option<&str>) -> Result<GitHubProfile> {
    let cfg = load_profiles()?;
    if cfg.profiles.is_empty() {
        anyhow::bail!(
            "Aucun compte GitHub configuré. Lancez `iloc connect github` pour connecter votre compte."
        );
    }
    let target = match name {
        Some(n) => n.to_string(),
        None    => cfg.active.clone().ok_or_else(|| anyhow::anyhow!(
            "Plusieurs profils GitHub existent mais aucun n'est actif. Utilisez `iloc connect github use <nom>`."
        ))?,
    };
    cfg.profiles.into_iter().find(|p| p.name == target).ok_or_else(|| {
        anyhow::anyhow!("Profil GitHub '{}' introuvable. Voir `iloc github list`.", target)
    })
}

pub fn require_credentials(profile_name: Option<&str>) -> Result<GitHubCredentials> {
    let profile = resolve_profile(profile_name)?;
    let token   = load_token(&profile.account)?;
    Ok(GitHubCredentials {
        profile_name: profile.name,
        login:        profile.login,
        default_org:  profile.default_org,
        api_url:      profile.api_url,
        token,
    })
}

// ── Gestion des profils ───────────────────────────────────────

pub fn upsert_profile(profile: GitHubProfile, set_active: bool) -> Result<()> {
    let mut cfg = load_profiles()?;
    let name = profile.name.clone();
    if let Some(existing) = cfg.profiles.iter_mut().find(|p| p.name == profile.name) {
        *existing = profile;
    } else {
        cfg.profiles.push(profile);
    }
    if set_active || cfg.active.is_none() {
        cfg.active = Some(name);
    }
    save_profiles(&cfg)
}

pub fn set_active(name: &str) -> Result<()> {
    let mut cfg = load_profiles()?;
    if !cfg.profiles.iter().any(|p| p.name == name) {
        anyhow::bail!("Profil GitHub '{}' introuvable.", name);
    }
    cfg.active = Some(name.to_string());
    save_profiles(&cfg)
}

pub fn remove_profile(name: &str) -> Result<bool> {
    let mut cfg = load_profiles()?;
    let before  = cfg.profiles.len();
    let removed_account = cfg.profiles.iter()
        .find(|p| p.name == name)
        .map(|p| p.account.clone());
    cfg.profiles.retain(|p| p.name != name);
    if cfg.profiles.len() == before {
        return Ok(false);
    }
    if let Some(account) = removed_account {
        let _ = delete_token(&account);
    }
    if cfg.active.as_deref() == Some(name) {
        cfg.active = cfg.profiles.first().map(|p| p.name.clone());
    }
    save_profiles(&cfg)?;
    Ok(true)
}

pub fn list_profiles() -> Result<GitHubProfiles> {
    load_profiles()
}

// ── Keyring: store / retrieve token ──────────────────────────
//
// Même stratégie de repli que cloud_store.rs :
// si le trousseau OS est indisponible, repli sur fichier 0600.

fn fallback_dir() -> Result<PathBuf> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .context("Cannot determine home directory")?;
    Ok(PathBuf::from(home)
        .join(".config")
        .join("ilocker")
        .join("credentials"))
}

fn fallback_path(account: &str) -> Result<PathBuf> {
    Ok(fallback_dir()?.join(format!("{}.github.vault", account)))
}

/// Chemin de l'ancien format (JSON en clair, versions < chiffrement du
/// fallback). Utilisé uniquement pour la migration automatique.
fn fallback_path_legacy(account: &str) -> Result<PathBuf> {
    Ok(fallback_dir()?.join(format!("{}.github.json", account)))
}

fn fallback_save(account: &str, token: &str) -> Result<()> {
    let dir = fallback_dir()?;
    std::fs::create_dir_all(&dir)?;
    let path      = fallback_path(account)?;
    let encrypted = crate::credential_vault::encrypt_credential(token)?;
    std::fs::write(&path, &encrypted)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// Migration automatique et transparente depuis l'ancien format (JSON
/// en clair, versions antérieures au chiffrement du fallback) : si un
/// fichier legacy existe et se parse correctement, on le ré-écrit
/// immédiatement au nouveau format chiffré puis on supprime l'original.
/// L'utilisateur n'a jamais besoin de relancer `iloc connect github`.
fn migrate_legacy_if_present(account: &str) -> Option<String> {
    let legacy_path = fallback_path_legacy(account).ok()?;
    let raw_str = std::fs::read_to_string(&legacy_path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw_str).ok()?;
    let token = v["access_token"].as_str()?.to_string();

    if fallback_save(account, &token).is_ok() {
        let _ = std::fs::remove_file(&legacy_path);
    }
    Some(token)
}

fn fallback_load(account: &str) -> Result<String> {
    let path = fallback_path(account)?;

    // Format actuel (chiffré).
    if let Ok(raw) = std::fs::read(&path) {
        if let Ok(token) = crate::credential_vault::decrypt_credential(&raw) {
            // Nettoyage défensif : un ancien fichier legacy encore
            // présent à côté (migration précédente interrompue) est
            // supprimé maintenant qu'on sait le nouveau format valide.
            if let Ok(legacy) = fallback_path_legacy(account) {
                let _ = std::fs::remove_file(legacy);
            }
            return Ok(token);
        }
    }

    // Migration automatique depuis l'ancien format.
    if let Some(token) = migrate_legacy_if_present(account) {
        return Ok(token);
    }

    anyhow::bail!("No fallback credential file found")
}

fn fallback_delete(account: &str) {
    if let Ok(path) = fallback_path(account) {
        let _ = std::fs::remove_file(path);
    }
}

pub fn save_token(account: &str, token: &str) -> Result<()> {
    let user  = format!("{}.{}", account, KR_TOKEN_KEY);
    let kr_ok = (|| -> Result<()> {
        let entry = Entry::new(KR_SERVICE, &user).map_err(|e| anyhow::anyhow!("{}", e))?;
        entry.set_password(token).context("set access_token")?;

        // Vérification immédiate par relecture — voir vercel_store.rs pour
        // l'explication complète : certains environnements retournent Ok
        // sur set_password sans jamais rendre la valeur relisible, ce qui
        // perdrait le token en silence sans cette vérification.
        let readback = Entry::new(KR_SERVICE, &user)
            .map_err(|e| anyhow::anyhow!("{}", e))?
            .get_password()
            .context("vérification post-écriture")?;
        if readback != token {
            anyhow::bail!("le trousseau a accepté l'écriture mais relit une valeur différente");
        }
        Ok(())
    })();
    match kr_ok {
        Ok(()) => Ok(()),
        Err(e) => {
            eprintln!(
                "  {} trousseau système indisponible ({}). Repli sur fichier 0600.",
                "⚠".to_string(), e
            );
            fallback_save(account, token)
        }
    }
}

pub fn load_token(account: &str) -> Result<String> {
    let try_kr = (|| -> Result<String> {
        let entry = Entry::new(KR_SERVICE, &format!("{}.{}", account, KR_TOKEN_KEY))
            .map_err(|e| anyhow::anyhow!("{}", e))?;
        entry.get_password().map_err(|e| anyhow::anyhow!("{}", e))
    })();
    match try_kr {
        Ok(t) => Ok(t),
        Err(_) => fallback_load(account)
            .context("Token GitHub introuvable — relancez `iloc connect github`"),
    }
}

pub fn delete_token(account: &str) -> Result<()> {
    if let Ok(entry) = Entry::new(KR_SERVICE, &format!("{}.{}", account, KR_TOKEN_KEY)) {
        let _ = entry.delete_password();
    }
    fallback_delete(account);
    Ok(())
}

// ── Platform permission helpers ───────────────────────────────

#[cfg(unix)]
fn set_owner_only(path: &PathBuf) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_owner_only(_path: &PathBuf) -> Result<()> { Ok(()) }

// ── Tests ──────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn use_temp_home() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("iloc_gh_test_{}", uuid_like()));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("HOME", &dir);
        std::env::set_var("USERPROFILE", &dir);
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

    #[test]
    fn round_trip_profile_with_none_org() {
        // Cas le plus fréquent : compte GitHub personnel sans org par défaut.
        let _home = use_temp_home();

        let profile = GitHubProfile {
            name:         "perso".to_string(),
            login:        "testuser".to_string(),
            default_org:  None,
            scopes:       vec!["repo".to_string(), "workflow".to_string()],
            api_url:      "https://api.github.com".to_string(),
            account:      "perso-abc123".to_string(),
            connected_at: "2026-07-06T00:00:00Z".to_string(),
        };

        upsert_profile(profile, true).expect("upsert doit réussir avec default_org=None");

        let loaded = load_profiles().unwrap();
        assert_eq!(loaded.profiles.len(), 1);
        assert_eq!(loaded.profiles[0].default_org, None);
        assert_eq!(loaded.profiles[0].scopes, vec!["repo", "workflow"]);

        let raw = std::fs::read_to_string(github_config_path().unwrap()).unwrap();
        assert!(!raw.contains("null"), "TOML invalide détecté : {}", raw);
    }

    #[test]
    fn round_trip_profile_with_org() {
        let _home = use_temp_home();

        let profile = GitHubProfile {
            name:         "travail".to_string(),
            login:        "devcorp".to_string(),
            default_org:  Some("ma-boite".to_string()),
            scopes:       vec!["repo".to_string()],
            api_url:      "https://api.github.com".to_string(),
            account:      "travail-def456".to_string(),
            connected_at: "2026-07-06T00:00:00Z".to_string(),
        };

        upsert_profile(profile, true).unwrap();
        let loaded = load_profiles().unwrap();
        assert_eq!(loaded.profiles[0].default_org.as_deref(), Some("ma-boite"));
    }

    #[test]
    fn token_fallback_round_trip() {
        let _home = use_temp_home();
        save_token("gh-test-account", "ghp_faketoken123").unwrap();
        assert_eq!(load_token("gh-test-account").unwrap(), "ghp_faketoken123");
        delete_token("gh-test-account").unwrap();
        assert!(load_token("gh-test-account").is_err());
    }

    #[test]
    fn migrates_legacy_plaintext_json_transparently() {
        let _home = use_temp_home();
        let dir = fallback_dir().unwrap();
        std::fs::create_dir_all(&dir).unwrap();

        // Simule un fichier créé par une version antérieure (avant
        // chiffrement du fallback) : JSON en clair, ancienne extension.
        let legacy_path = fallback_path_legacy("gh-legacy-account").unwrap();
        std::fs::write(
            &legacy_path,
            serde_json::to_string(&serde_json::json!({ "access_token": "ghp_oldtoken789" })).unwrap(),
        )
        .unwrap();

        // load_token doit réussir sans que l'utilisateur n'ait rien à refaire.
        let token = load_token("gh-legacy-account").unwrap();
        assert_eq!(token, "ghp_oldtoken789");

        // Le fichier legacy a été supprimé, remplacé par le nouveau format chiffré.
        assert!(!legacy_path.exists(), "l'ancien fichier en clair doit être nettoyé après migration");
        assert!(fallback_path("gh-legacy-account").unwrap().exists(), "le nouveau fichier chiffré doit exister");

        // Un second appel relit bien depuis le nouveau format (pas besoin du legacy).
        assert_eq!(load_token("gh-legacy-account").unwrap(), "ghp_oldtoken789");
    }

    #[test]
    fn resolve_without_profiles_gives_clear_error() {
        let _home = use_temp_home();
        let err = resolve_profile(None).unwrap_err();
        assert!(err.to_string().contains("connect github"));
    }
}

