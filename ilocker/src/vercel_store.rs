// ============================================================
//  vercel_store.rs — Stockage des tokens Vercel (multi-profils)
//
//  Même architecture que github_store.rs :
//    Layer 1 — ~/.config/ilocker/vercel.toml   (non-sensible)
//              email, username, team, connected_at
//    Layer 2 — OS native keychain               (sensible)
//              access_token (token Vercel)
//
//  Un développeur peut avoir PLUSIEURS comptes Vercel (perso +
//  agence + client) — chacun est un profil nommé.
//
//  Commandes concernées :
//    iloc connect vercel          → assistant de connexion
//    iloc vercel <sous-commande>  → profil actif par défaut
//    --vercel-profile <nom>       → profil explicite
// ============================================================

use anyhow::{Context, Result};
use keyring::Entry;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const KR_SERVICE:   &str = "ilocker-vercel";
const KR_TOKEN_KEY: &str = "access_token";

// ── Profil Vercel ─────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VercelProfile {
    /// Nom local du profil (ex: "perso", "agence", "client-x")
    pub name: String,
    /// Email du compte Vercel
    pub email: String,
    /// Username Vercel
    pub username: String,
    /// Team slug par défaut (None = scope personnel)
    pub default_team: Option<String>,
    /// Team ID interne Vercel (pour les appels API)
    pub default_team_id: Option<String>,
    /// Clé unique pour le trousseau (UUID stable)
    pub account: String,
    /// Date de connexion
    pub connected_at: String,
}

// ── Fichier de config complet ─────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VercelProfiles {
    pub active: Option<String>,
    #[serde(default)]
    pub profiles: Vec<VercelProfile>,
}

// ── Credentials résolus (avec token) ─────────────────────────

#[derive(Debug, Clone)]
pub struct VercelCredentials {
    pub profile_name:    String,
    pub email:           String,
    pub username:        String,
    pub default_team:    Option<String>,
    pub default_team_id: Option<String>,
    pub token:           String,
}

// ── Chemin du fichier de config ──────────────────────────────

pub fn vercel_config_path() -> Result<PathBuf> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .context("Cannot determine home directory")?;
    Ok(PathBuf::from(home)
        .join(".config")
        .join("ilocker")
        .join("vercel.toml"))
}

// ── Chargement / sauvegarde ───────────────────────────────────

pub fn load_profiles() -> Result<VercelProfiles> {
    let path = vercel_config_path()?;
    if !path.exists() {
        return Ok(VercelProfiles::default());
    }
    let raw = std::fs::read_to_string(&path)?;
    Ok(toml::from_str::<VercelProfiles>(&raw).unwrap_or_default())
}

pub fn save_profiles(cfg: &VercelProfiles) -> Result<()> {
    let path = vercel_config_path()?;
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

// ── Résolution d'un profil ────────────────────────────────────

pub fn resolve_profile(name: Option<&str>) -> Result<VercelProfile> {
    let cfg = load_profiles()?;
    if cfg.profiles.is_empty() {
        anyhow::bail!(
            "Aucun compte Vercel configuré. Lancez `iloc connect vercel` pour connecter votre compte."
        );
    }
    let target = match name {
        Some(n) => n.to_string(),
        None    => cfg.active.clone().ok_or_else(|| anyhow::anyhow!(
            "Plusieurs profils Vercel existent mais aucun n'est actif. Utilisez `iloc vercel use <nom>`."
        ))?,
    };
    cfg.profiles.into_iter().find(|p| p.name == target).ok_or_else(|| {
        anyhow::anyhow!("Profil Vercel '{}' introuvable. Voir `iloc vercel list`.", target)
    })
}

pub fn require_credentials(profile_name: Option<&str>) -> Result<VercelCredentials> {
    let profile = resolve_profile(profile_name)?;
    let token   = load_token(&profile.account)?;
    Ok(VercelCredentials {
        profile_name:    profile.name,
        email:           profile.email,
        username:        profile.username,
        default_team:    profile.default_team,
        default_team_id: profile.default_team_id,
        token,
    })
}

// ── Gestion des profils ───────────────────────────────────────

pub fn upsert_profile(profile: VercelProfile, set_active: bool) -> Result<()> {
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
        anyhow::bail!("Profil Vercel '{}' introuvable.", name);
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

pub fn list_profiles() -> Result<VercelProfiles> {
    load_profiles()
}

// ── Keyring ───────────────────────────────────────────────────

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
    Ok(fallback_dir()?.join(format!("{}.vercel.vault", account)))
}

/// Chemin de l'ancien format (JSON en clair). Migration automatique uniquement.
fn fallback_path_legacy(account: &str) -> Result<PathBuf> {
    Ok(fallback_dir()?.join(format!("{}.vercel.json", account)))
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

        // Vérification immédiate par relecture : certains environnements
        // (session dbus éphémère, sandbox sans démon de trousseau persistant)
        // acceptent l'écriture et retournent Ok sans jamais rendre la valeur
        // disponible à une lecture ultérieure. Sans cette vérification,
        // save_token croit avoir réussi, load_token échoue juste après, et
        // le token est perdu en silence — découvert en testant en conditions
        // réelles, pas visible à la compilation ni en relecture de code.
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
            eprintln!("  ⚠ trousseau système indisponible ({}). Repli sur fichier 0600.", e);
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
            .context("Token Vercel introuvable — relancez `iloc connect vercel`"),
    }
}

pub fn delete_token(account: &str) -> Result<()> {
    if let Ok(e) = Entry::new(KR_SERVICE, &format!("{}.{}", account, KR_TOKEN_KEY)) {
        let _ = e.delete_password();
    }
    fallback_delete(account);
    Ok(())
}

// ── Platform helpers ──────────────────────────────────────────

#[cfg(unix)]
fn set_owner_only(path: &PathBuf) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_owner_only(_: &PathBuf) -> Result<()> { Ok(()) }

// ── Tests ──────────────────────────────────────────────────────
//
// Ces tests s'exécutent avec HOME redirigé vers un dossier temporaire
// pour ne jamais toucher une vraie config. `cargo test -- --test-threads=1`
// est nécessaire car HOME est une variable d'environnement globale.

#[cfg(test)]
mod tests {
    use super::*;

    fn use_temp_home() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("iloc_test_{}", uuid_like()));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("HOME", &dir);
        std::env::set_var("USERPROFILE", &dir);
        dir
    }

    // Pas de dépendance uuid ici : on génère un identifiant unique simple
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
    fn round_trip_profile_with_none_team() {
        // Cas réel le plus fréquent : un utilisateur avec un compte Vercel
        // personnel (pas de team) → default_team et default_team_id sont None.
        // Si la sérialisation TOML d'un Option::None panique ou produit un
        // fichier invalide, CE test doit le révéler.
        let _home = use_temp_home();

        let profile = VercelProfile {
            name:            "perso".to_string(),
            email:           "test@example.com".to_string(),
            username:        "testuser".to_string(),
            default_team:    None,
            default_team_id: None,
            account:         "perso-abc123".to_string(),
            connected_at:    "2026-07-06T00:00:00Z".to_string(),
        };

        upsert_profile(profile.clone(), true)
            .expect("upsert_profile doit réussir même avec default_team=None");

        let loaded = load_profiles().expect("load_profiles doit relire ce qu'on vient d'écrire");
        assert_eq!(loaded.profiles.len(), 1);
        assert_eq!(loaded.profiles[0].name, "perso");
        assert_eq!(loaded.profiles[0].default_team, None);
        assert_eq!(loaded.profiles[0].default_team_id, None);
        assert_eq!(loaded.active.as_deref(), Some("perso"));

        // Vérifier aussi que le fichier TOML brut est lisible par un humain
        // (pas de "null" littéral invalide en TOML)
        let raw = std::fs::read_to_string(vercel_config_path().unwrap()).unwrap();
        assert!(!raw.contains("null"), "TOML ne doit jamais contenir 'null' littéral : {}", raw);
    }

    #[test]
    fn round_trip_profile_with_team() {
        let _home = use_temp_home();

        let profile = VercelProfile {
            name:            "agence".to_string(),
            email:           "agence@example.com".to_string(),
            username:        "agencedev".to_string(),
            default_team:    Some("mon-equipe".to_string()),
            default_team_id: Some("team_xyz789".to_string()),
            account:         "agence-def456".to_string(),
            connected_at:    "2026-07-06T00:00:00Z".to_string(),
        };

        upsert_profile(profile, true).unwrap();
        let loaded = load_profiles().unwrap();
        assert_eq!(loaded.profiles[0].default_team.as_deref(), Some("mon-equipe"));
        assert_eq!(loaded.profiles[0].default_team_id.as_deref(), Some("team_xyz789"));
    }

    #[test]
    fn multi_profile_switch_and_remove() {
        let _home = use_temp_home();

        upsert_profile(VercelProfile {
            name: "compte-1".into(), email: "a@x.com".into(), username: "a".into(),
            default_team: None, default_team_id: None,
            account: "acc-1".into(), connected_at: "2026-07-06T00:00:00Z".into(),
        }, true).unwrap();

        upsert_profile(VercelProfile {
            name: "compte-2".into(), email: "b@x.com".into(), username: "b".into(),
            default_team: None, default_team_id: None,
            account: "acc-2".into(), connected_at: "2026-07-06T00:00:00Z".into(),
        }, false).unwrap();

        // compte-1 doit rester actif (set_active=false pour compte-2)
        let cfg = load_profiles().unwrap();
        assert_eq!(cfg.active.as_deref(), Some("compte-1"));
        assert_eq!(cfg.profiles.len(), 2);

        // Changer le profil actif
        set_active("compte-2").unwrap();
        assert_eq!(load_profiles().unwrap().active.as_deref(), Some("compte-2"));

        // resolve_profile sans nom doit retourner le profil actif
        let resolved = resolve_profile(None).unwrap();
        assert_eq!(resolved.name, "compte-2");

        // resolve_profile avec un nom explicite
        let resolved2 = resolve_profile(Some("compte-1")).unwrap();
        assert_eq!(resolved2.name, "compte-1");

        // Supprimer le profil actif → l'autre doit devenir actif automatiquement
        assert!(remove_profile("compte-2").unwrap());
        let cfg2 = load_profiles().unwrap();
        assert_eq!(cfg2.profiles.len(), 1);
        assert_eq!(cfg2.active.as_deref(), Some("compte-1"));

        // Supprimer un profil inexistant → false, pas d'erreur
        assert!(!remove_profile("compte-fantome").unwrap());
    }

    #[test]
    fn resolve_profile_without_any_configured_fails_clearly() {
        let _home = use_temp_home();
        let err = resolve_profile(None).unwrap_err();
        assert!(err.to_string().contains("connect vercel"));
    }

    #[test]
    fn token_save_load_delete_fallback_path() {
        // Sur cette machine de test (Linux sans linux-no-secret-service actif
        // dans Cargo.toml), le trousseau natif n'est pas disponible : ce test
        // exerce donc le chemin de repli fichier 0600, qui est le chemin réel
        // utilisé sur tout Linux sans secret-service (conteneurs, CI, serveurs
        // headless) même en production.
        let _home = use_temp_home();
        let account = "test-token-account";

        save_token(account, "vc_abc123secret").expect("save_token doit réussir (fallback fichier)");
        let loaded = load_token(account).expect("load_token doit relire le token sauvegardé");
        assert_eq!(loaded, "vc_abc123secret");

        delete_token(account).unwrap();
        let after_delete = load_token(account);
        assert!(after_delete.is_err(), "le token ne doit plus être lisible après delete_token");
    }

    #[test]
    fn migrates_legacy_plaintext_json_transparently() {
        let _home = use_temp_home();
        let dir = fallback_dir().unwrap();
        std::fs::create_dir_all(&dir).unwrap();

        let legacy_path = fallback_path_legacy("vc-legacy-account").unwrap();
        std::fs::write(
            &legacy_path,
            serde_json::to_string(&serde_json::json!({ "access_token": "vc_oldtoken789" })).unwrap(),
        )
        .unwrap();

        let token = load_token("vc-legacy-account").unwrap();
        assert_eq!(token, "vc_oldtoken789");
        assert!(!legacy_path.exists());
        assert!(fallback_path("vc-legacy-account").unwrap().exists());
        assert_eq!(load_token("vc-legacy-account").unwrap(), "vc_oldtoken789");
    }
}

