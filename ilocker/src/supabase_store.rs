// ============================================================
//  supabase_store.rs — Stockage des tokens Supabase (multi-profils)
//
//  Même architecture que github_store.rs / vercel_store.rs :
//    Layer 1 — ~/.config/ilocker/supabase.toml   (non-sensible)
//              default_org_id, default_org_slug, connected_at
//    Layer 2 — OS native keychain                 (sensible)
//              access_token (Personal Access Token, préfixe sbp_)
//
//  Un développeur peut avoir PLUSIEURS comptes Supabase (perso +
//  agence + client) — chacun est un profil nommé.
//
//  Commandes concernées :
//    iloc connect supabase          → assistant de connexion
//    iloc supabase <sous-commande>  → profil actif par défaut
//    --supabase-profile <nom>       → profil explicite
// ============================================================

use anyhow::{Context, Result};
use keyring::Entry;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const KR_SERVICE:   &str = "ilocker-supabase";
const KR_TOKEN_KEY: &str = "access_token";

// ── Profil Supabase ────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SupabaseProfile {
    /// Nom local du profil (ex: "perso", "agence", "client-x")
    pub name: String,
    /// Organisation par défaut (slug) — None si un seul org sur le compte
    pub default_org_slug: Option<String>,
    /// Organisation par défaut (ID interne, pour les appels API)
    pub default_org_id: Option<String>,
    /// Clé unique pour le trousseau (UUID stable)
    pub account: String,
    /// Date de connexion
    pub connected_at: String,
}

// ── Fichier de config complet ──────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SupabaseProfiles {
    pub active: Option<String>,
    #[serde(default)]
    pub profiles: Vec<SupabaseProfile>,
}

// ── Credentials résolus (avec token) ───────────────────────────

#[derive(Debug, Clone)]
pub struct SupabaseCredentials {
    pub profile_name:     String,
    pub default_org_slug: Option<String>,
    pub default_org_id:   Option<String>,
    pub token:            String,
}

// ── Chemin du fichier de config ────────────────────────────────

pub fn supabase_config_path() -> Result<PathBuf> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .context("Cannot determine home directory")?;
    Ok(PathBuf::from(home)
        .join(".config")
        .join("ilocker")
        .join("supabase.toml"))
}

// ── Chargement / sauvegarde ─────────────────────────────────────

pub fn load_profiles() -> Result<SupabaseProfiles> {
    let path = supabase_config_path()?;
    if !path.exists() {
        return Ok(SupabaseProfiles::default());
    }
    let raw = std::fs::read_to_string(&path)?;
    Ok(toml::from_str::<SupabaseProfiles>(&raw).unwrap_or_default())
}

pub fn save_profiles(cfg: &SupabaseProfiles) -> Result<()> {
    let path = supabase_config_path()?;
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    let raw = toml::to_string_pretty(cfg)?;
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, &raw)?;
    set_owner_only(&tmp)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

// ── Résolution d'un profil ──────────────────────────────────────

pub fn resolve_profile(name: Option<&str>) -> Result<SupabaseProfile> {
    let cfg = load_profiles()?;
    if cfg.profiles.is_empty() {
        anyhow::bail!(
            "Aucun compte Supabase configuré. Lancez `iloc connect supabase` pour connecter votre compte."
        );
    }
    let target = match name {
        Some(n) => n.to_string(),
        None    => cfg.active.clone().ok_or_else(|| anyhow::anyhow!(
            "Plusieurs profils Supabase existent mais aucun n'est actif. Utilisez `iloc supabase use <nom>`."
        ))?,
    };
    cfg.profiles.into_iter().find(|p| p.name == target).ok_or_else(|| {
        anyhow::anyhow!("Profil Supabase '{}' introuvable. Voir `iloc supabase list`.", target)
    })
}

pub fn require_credentials(profile_name: Option<&str>) -> Result<SupabaseCredentials> {
    let profile = resolve_profile(profile_name)?;
    let token   = load_token(&profile.account)?;
    Ok(SupabaseCredentials {
        profile_name:     profile.name,
        default_org_slug: profile.default_org_slug,
        default_org_id:   profile.default_org_id,
        token,
    })
}

// ── Gestion des profils ──────────────────────────────────────────

pub fn upsert_profile(profile: SupabaseProfile, set_active: bool) -> Result<()> {
    let mut cfg = load_profiles()?;
    let name    = profile.name.clone();
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
        anyhow::bail!("Profil Supabase '{}' introuvable.", name);
    }
    cfg.active = Some(name.to_string());
    save_profiles(&cfg)
}

pub fn remove_profile(name: &str) -> Result<bool> {
    let mut cfg    = load_profiles()?;
    let before     = cfg.profiles.len();
    let rm_account = cfg.profiles.iter()
        .find(|p| p.name == name)
        .map(|p| p.account.clone());
    cfg.profiles.retain(|p| p.name != name);
    if cfg.profiles.len() == before { return Ok(false); }
    if let Some(account) = rm_account { let _ = delete_token(&account); }
    if cfg.active.as_deref() == Some(name) {
        cfg.active = cfg.profiles.first().map(|p| p.name.clone());
    }
    save_profiles(&cfg)?;
    Ok(true)
}

pub fn list_profiles() -> Result<SupabaseProfiles> {
    load_profiles()
}

// ── Keyring ────────────────────────────────────────────────────
//
// LEÇON APPLIQUÉE : save_token vérifie IMMÉDIATEMENT par relecture.
// Un bug réel (découvert en testant github_store/vercel_store) fait
// que certains environnements acceptent set_password() en retournant
// Ok(()) sans jamais rendre la valeur relisible ensuite — perdant le
// token en silence. On ne répète pas cette erreur ici : dès l'écriture
// initiale, save_token relit et bascule sur le fallback fichier si la
// relecture échoue ou diffère.

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
    Ok(fallback_dir()?.join(format!("{}.supabase.vault", account)))
}

/// Chemin de l'ancien format (JSON en clair). Migration automatique uniquement.
fn fallback_path_legacy(account: &str) -> Result<PathBuf> {
    Ok(fallback_dir()?.join(format!("{}.supabase.json", account)))
}

fn fallback_save(account: &str, token: &str) -> Result<()> {
    let dir  = fallback_dir()?;
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

/// Migration transparente depuis l'ancien format en clair — voir
/// github_store.rs pour l'explication complète du mécanisme.
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

    if let Ok(raw) = std::fs::read(&path) {
        if let Ok(token) = crate::credential_vault::decrypt_credential(&raw) {
            if let Ok(legacy) = fallback_path_legacy(account) {
                let _ = std::fs::remove_file(legacy);
            }
            return Ok(token);
        }
    }

    if let Some(token) = migrate_legacy_if_present(account) {
        return Ok(token);
    }

    anyhow::bail!("No fallback credential file found")
}

fn fallback_delete(account: &str) {
    if let Ok(p) = fallback_path(account) { let _ = std::fs::remove_file(p); }
}

pub fn save_token(account: &str, token: &str) -> Result<()> {
    let user  = format!("{}.{}", account, KR_TOKEN_KEY);
    let kr_ok = (|| -> Result<()> {
        let entry = Entry::new(KR_SERVICE, &user).map_err(|e| anyhow::anyhow!("{}", e))?;
        entry.set_password(token).context("set access_token")?;

        // Vérification immédiate par relecture — voir la note ci-dessus.
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
        Ok(t)  => Ok(t),
        Err(_) => fallback_load(account)
            .context("Token Supabase introuvable — relancez `iloc connect supabase`"),
    }
}

pub fn delete_token(account: &str) -> Result<()> {
    if let Ok(e) = Entry::new(KR_SERVICE, &format!("{}.{}", account, KR_TOKEN_KEY)) {
        let _ = e.delete_password();
    }
    fallback_delete(account);
    Ok(())
}

// ── Platform helpers ─────────────────────────────────────────────

#[cfg(unix)]
fn set_owner_only(path: &PathBuf) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_owner_only(_: &PathBuf) -> Result<()> { Ok(()) }

// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn use_temp_home() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("iloc_sb_test_{}", uuid_like()));
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
        let _home = use_temp_home();
        let profile = SupabaseProfile {
            name: "perso".to_string(), default_org_slug: None, default_org_id: None,
            account: "perso-abc123".to_string(), connected_at: "2026-07-09T00:00:00Z".to_string(),
        };
        upsert_profile(profile, true).expect("upsert doit réussir avec org=None");
        let loaded = load_profiles().unwrap();
        assert_eq!(loaded.profiles[0].default_org_slug, None);
        let raw = std::fs::read_to_string(supabase_config_path().unwrap()).unwrap();
        assert!(!raw.contains("null"), "TOML invalide détecté : {}", raw);
    }

    #[test]
    fn round_trip_profile_with_org() {
        let _home = use_temp_home();
        let profile = SupabaseProfile {
            name: "agence".to_string(),
            default_org_slug: Some("mon-org".to_string()),
            default_org_id: Some("org_xyz789".to_string()),
            account: "agence-def456".to_string(), connected_at: "2026-07-09T00:00:00Z".to_string(),
        };
        upsert_profile(profile, true).unwrap();
        let loaded = load_profiles().unwrap();
        assert_eq!(loaded.profiles[0].default_org_slug.as_deref(), Some("mon-org"));
    }

    #[test]
    fn multi_profile_switch_and_remove() {
        let _home = use_temp_home();
        upsert_profile(SupabaseProfile {
            name: "c1".into(), default_org_slug: None, default_org_id: None,
            account: "acc1".into(), connected_at: "t".into(),
        }, true).unwrap();
        upsert_profile(SupabaseProfile {
            name: "c2".into(), default_org_slug: None, default_org_id: None,
            account: "acc2".into(), connected_at: "t".into(),
        }, false).unwrap();
        assert_eq!(load_profiles().unwrap().active.as_deref(), Some("c1"));
        set_active("c2").unwrap();
        assert_eq!(resolve_profile(None).unwrap().name, "c2");
        assert!(remove_profile("c2").unwrap());
        assert_eq!(load_profiles().unwrap().active.as_deref(), Some("c1"));
        assert!(!remove_profile("fantome").unwrap());
    }

    #[test]
    fn token_fallback_round_trip() {
        let _home = use_temp_home();
        save_token("sb-test", "sbp_faketoken123").unwrap();
        assert_eq!(load_token("sb-test").unwrap(), "sbp_faketoken123");
        delete_token("sb-test").unwrap();
        assert!(load_token("sb-test").is_err());
    }

    #[test]
    fn migrates_legacy_plaintext_json_transparently() {
        let _home = use_temp_home();
        let dir = fallback_dir().unwrap();
        std::fs::create_dir_all(&dir).unwrap();

        let legacy_path = fallback_path_legacy("sb-legacy-account").unwrap();
        std::fs::write(
            &legacy_path,
            serde_json::to_string(&serde_json::json!({ "access_token": "sbp_oldtoken789" })).unwrap(),
        )
        .unwrap();

        let token = load_token("sb-legacy-account").unwrap();
        assert_eq!(token, "sbp_oldtoken789");
        assert!(!legacy_path.exists());
        assert!(fallback_path("sb-legacy-account").unwrap().exists());
        assert_eq!(load_token("sb-legacy-account").unwrap(), "sbp_oldtoken789");
    }

    #[test]
    fn resolve_without_profiles_gives_clear_error() {
        let _home = use_temp_home();
        let err = resolve_profile(None).unwrap_err();
        assert!(err.to_string().contains("connect supabase"));
    }
}
