// ============================================================
//  provider_store.rs — Identifiants des providers déclaratifs
//  (multi-profils), généralisation du pattern github_store.rs /
//  vercel_store.rs / supabase_store.rs à un slug arbitraire.
//
//  Même architecture à deux couches que les stores existants :
//    Layer 1 — ~/.config/ilocker/providers/<slug>/profiles.toml (non-sensible)
//              nom du profil, libellé d'identité affiché,
//              override d'URL API, date de connexion
//    Layer 2 — trousseau OS natif (sensible), service dédié
//              "ilocker-provider-<slug>" — isolé de tout autre
//              provider, y compris ceux natifs (github/vercel/
//              supabase gardent leurs propres stores inchangés)
//
//  Décision de sécurité délibérée, plus stricte que ce que ferait
//  strictement nécessaire un manifeste correctement écrit :
//  ───────────────────────────────────────────────────────────
//  Un manifeste peut marquer un champ `auth.fields[].secret = false`
//  (ex : le "username" d'un couple basic auth n'est pas vraiment un
//  secret). Ce store choisit de NE JAMAIS faire confiance à ce
//  drapeau pour décider de l'emplacement de stockage — CONTRAIREMENT
//  à github_store.rs qui sépare volontairement login/org (en clair,
//  écrits par les développeurs ilocker eux-mêmes, donc dignes de
//  confiance) du token (chiffré). Ici, l'auteur du manifeste est un
//  tiers non vérifié : TOUTES les valeurs de auth.fields, sans
//  exception, sont chiffrées ensemble dans le trousseau/coffre. Le
//  drapeau `secret` ne pilote que le masquage de la saisie au
//  terminal (`iloc connect`), jamais l'emplacement de stockage.
// ============================================================

use crate::provider_manifest::ProviderManifest;
use anyhow::{Context, Result};
use keyring::Entry;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

const KR_FIELDS_KEY: &str = "auth_fields";
/// Espace de nommage SÉPARÉ de KR_FIELDS_KEY : le jeton OAuth2 mis en
/// cache n'est jamais mélangé aux identifiants longue-durée (client_id/
/// client_secret / service_account_json) que l'utilisateur a fournis —
/// même isolation de préoccupations que le reste de ce fichier.
const KR_OAUTH_CACHE_KEY: &str = "oauth_cache";

fn kr_service(slug: &str) -> String {
    format!("ilocker-provider-{}", slug)
}

// ── Profil ────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderProfile {
    /// Nom local (ex: "default", "perso", "client-x").
    pub name: String,
    /// Libellé d'identité affiché après vérification réussie (ex :
    /// l'email retourné par l'API) — un IDENTIFIANT, jamais un secret,
    /// sûr à afficher en clair dans `iloc provider list`.
    pub identity_label: Option<String>,
    /// Override de l'URL API (self-hosted / entreprise).
    pub api_url_override: Option<String>,
    /// Clé stable pour le trousseau, indépendante du nom (qui peut
    /// changer) — même principe que `account` dans github_store.rs.
    pub account: String,
    pub connected_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProviderProfiles {
    pub active: Option<String>,
    #[serde(default)]
    pub profiles: Vec<ProviderProfile>,
}

/// Identifiants entièrement résolus, prêts pour provider_engine.rs.
#[derive(Debug, Clone)]
pub struct ResolvedProviderCredentials {
    pub profile_name: String,
    /// Clé stable de trousseau (`ProviderProfile::account`) — nécessaire
    /// en plus de `profile_name` pour que le moteur puisse lire/écrire
    /// le cache de jeton OAuth2 (voir save_oauth_cache/load_oauth_cache),
    /// qui est indexé par account, pas par le nom de profil affiché.
    pub account: String,
    pub api_url: String,
    /// Toutes les valeurs de auth.fields (ex: {"token": "…"} ou
    /// {"username": "…", "password": "…"}), déchiffrées.
    pub fields: HashMap<String, String>,
}

// ── Chemins ───────────────────────────────────────────────────

fn config_dir() -> Result<PathBuf> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .context("Cannot determine home directory")?;
    Ok(PathBuf::from(home).join(".config").join("ilocker"))
}

pub fn provider_config_path(slug: &str) -> Result<PathBuf> {
    Ok(config_dir()?.join("providers").join(slug).join("profiles.toml"))
}

fn fallback_dir() -> Result<PathBuf> {
    Ok(config_dir()?.join("credentials"))
}

fn fallback_path(slug: &str, account: &str) -> Result<PathBuf> {
    Ok(fallback_dir()?.join(format!("{}.provider-{}.vault", account, slug)))
}

// ── Chargement / sauvegarde des profils (non-sensible) ─────────

pub fn load_profiles(slug: &str) -> Result<ProviderProfiles> {
    let path = provider_config_path(slug)?;
    if !path.exists() {
        return Ok(ProviderProfiles::default());
    }
    let raw = std::fs::read_to_string(&path)?;
    Ok(toml::from_str::<ProviderProfiles>(&raw).unwrap_or_default())
}

pub fn save_profiles(slug: &str, cfg: &ProviderProfiles) -> Result<()> {
    let path = provider_config_path(slug)?;
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

// ── Résolution ───────────────────────────────────────────────

pub fn resolve_profile(slug: &str, name: Option<&str>) -> Result<ProviderProfile> {
    let cfg = load_profiles(slug)?;
    if cfg.profiles.is_empty() {
        anyhow::bail!(
            "Aucun compte connecté pour '{}'. Lancez `iloc connect {}`.",
            slug, slug
        );
    }
    let target = match name {
        Some(n) => n.to_string(),
        None => cfg.active.clone().ok_or_else(|| {
            anyhow::anyhow!(
                "Plusieurs profils existent pour '{}' mais aucun n'est actif. Voir `iloc provider profile use {} <nom>`.",
                slug, slug
            )
        })?,
    };
    cfg.profiles
        .into_iter()
        .find(|p| p.name == target)
        .ok_or_else(|| anyhow::anyhow!("Profil '{}' introuvable pour '{}'.", target, slug))
}

pub fn require_credentials(
    slug: &str,
    manifest: &ProviderManifest,
    profile_name: Option<&str>,
) -> Result<ResolvedProviderCredentials> {
    let profile = resolve_profile(slug, profile_name)?;
    let fields = load_fields(slug, &profile.account)
        .context("Identifiants introuvables ou illisibles — relancez `iloc connect`")?;
    let api_url = profile
        .api_url_override
        .clone()
        .unwrap_or_else(|| manifest.api.base_url.clone());
    Ok(ResolvedProviderCredentials {
        profile_name: profile.name,
        account: profile.account,
        api_url,
        fields,
    })
}

// ── Gestion des profils ─────────────────────────────────────────

pub fn upsert_profile(
    slug: &str,
    profile: ProviderProfile,
    fields: &HashMap<String, String>,
    set_active: bool,
) -> Result<()> {
    save_fields(slug, &profile.account, fields)?;

    let mut cfg = load_profiles(slug)?;
    let name = profile.name.clone();
    if let Some(existing) = cfg.profiles.iter_mut().find(|p| p.name == profile.name) {
        *existing = profile;
    } else {
        cfg.profiles.push(profile);
    }
    if set_active || cfg.active.is_none() {
        cfg.active = Some(name);
    }
    save_profiles(slug, &cfg)
}

pub fn set_active(slug: &str, name: &str) -> Result<()> {
    let mut cfg = load_profiles(slug)?;
    if !cfg.profiles.iter().any(|p| p.name == name) {
        anyhow::bail!("Profil '{}' introuvable pour '{}'.", name, slug);
    }
    cfg.active = Some(name.to_string());
    save_profiles(slug, &cfg)
}

pub fn remove_profile(slug: &str, name: &str) -> Result<bool> {
    let mut cfg = load_profiles(slug)?;
    let before = cfg.profiles.len();
    let removed_account = cfg.profiles.iter().find(|p| p.name == name).map(|p| p.account.clone());
    cfg.profiles.retain(|p| p.name != name);
    if cfg.profiles.len() == before {
        return Ok(false);
    }
    if let Some(account) = removed_account {
        let _ = delete_fields(slug, &account);
        clear_oauth_cache(slug, &account);
    }
    if cfg.active.as_deref() == Some(name) {
        cfg.active = cfg.profiles.first().map(|p| p.name.clone());
    }
    save_profiles(slug, &cfg)?;
    Ok(true)
}

pub fn list_profiles(slug: &str) -> Result<ProviderProfiles> {
    load_profiles(slug)
}

/// Supprime intégralement un provider installé : tous ses profils,
/// tous ses identifiants (trousseau + fallback), son fichier de
/// config. Utilisé par `iloc provider remove <slug>`.
pub fn purge_all(slug: &str) -> Result<()> {
    let cfg = load_profiles(slug)?;
    for p in &cfg.profiles {
        let _ = delete_fields(slug, &p.account);
        clear_oauth_cache(slug, &p.account);
    }
    let path = provider_config_path(slug)?;
    if path.exists() {
        std::fs::remove_file(&path)?;
    }
    Ok(())
}

// ── Trousseau / fallback chiffré — stocke un blob JSON de champs ──

fn fallback_save(slug: &str, account: &str, fields_json: &str) -> Result<()> {
    let dir = fallback_dir()?;
    std::fs::create_dir_all(&dir)?;
    let path = fallback_path(slug, account)?;
    let encrypted = crate::credential_vault::encrypt_credential(fields_json)?;
    std::fs::write(&path, &encrypted)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn fallback_load(slug: &str, account: &str) -> Result<String> {
    let path = fallback_path(slug, account)?;
    let raw = std::fs::read(&path).context("Aucun fichier de repli trouvé")?;
    crate::credential_vault::decrypt_credential(&raw)
}

fn fallback_delete(slug: &str, account: &str) {
    if let Ok(path) = fallback_path(slug, account) {
        let _ = std::fs::remove_file(path);
    }
}

/// Chiffre et stocke TOUTES les valeurs de champs d'authentification
/// pour un compte donné, comme un unique blob JSON — jamais séparées,
/// jamais partiellement en clair (voir note de sécurité en tête de
/// fichier).
pub fn save_fields(slug: &str, account: &str, fields: &HashMap<String, String>) -> Result<()> {
    let fields_json = serde_json::to_string(fields).context("Sérialisation des identifiants")?;
    let user = format!("{}.{}", account, KR_FIELDS_KEY);
    let kr_ok = (|| -> Result<()> {
        let entry = Entry::new(&kr_service(slug), &user).map_err(|e| anyhow::anyhow!("{}", e))?;
        entry.set_password(&fields_json).context("écriture trousseau")?;

        // Vérification immédiate par relecture — même garde-fou que
        // github_store.rs/vercel_store.rs : certains environnements
        // acceptent l'écriture sans jamais la rendre relisible.
        let readback = Entry::new(&kr_service(slug), &user)
            .map_err(|e| anyhow::anyhow!("{}", e))?
            .get_password()
            .context("vérification post-écriture")?;
        if readback != fields_json {
            anyhow::bail!("le trousseau a accepté l'écriture mais relit une valeur différente");
        }
        Ok(())
    })();

    match kr_ok {
        Ok(()) => Ok(()),
        Err(e) => {
            eprintln!(
                "  ⚠ trousseau système indisponible ({}). Repli sur fichier chiffré 0600.",
                e
            );
            fallback_save(slug, account, &fields_json)
        }
    }
}

pub fn load_fields(slug: &str, account: &str) -> Result<HashMap<String, String>> {
    let user = format!("{}.{}", account, KR_FIELDS_KEY);
    let try_kr = (|| -> Result<String> {
        let entry = Entry::new(&kr_service(slug), &user).map_err(|e| anyhow::anyhow!("{}", e))?;
        entry.get_password().map_err(|e| anyhow::anyhow!("{}", e))
    })();

    let fields_json = match try_kr {
        Ok(j) => j,
        Err(_) => fallback_load(slug, account)
            .context("Identifiants introuvables — relancez `iloc connect`")?,
    };

    serde_json::from_str(&fields_json).context("Identifiants stockés corrompus")
}

pub fn delete_fields(slug: &str, account: &str) -> Result<()> {
    let user = format!("{}.{}", account, KR_FIELDS_KEY);
    if let Ok(entry) = Entry::new(&kr_service(slug), &user) {
        let _ = entry.delete_password();
    }
    fallback_delete(slug, account);
    Ok(())
}

// ── Cache de jeton OAuth2 (client_credentials / service_account) ──
//
// Même mécanisme de stockage que save_fields/load_fields (trousseau +
// repli chiffré, vérification par relecture), mais dans un espace de
// nommage séparé (KR_OAUTH_CACHE_KEY) : ce n'est pas un identifiant
// fourni par l'utilisateur, c'est un jeton à courte durée de vie que
// le moteur obtient et renouvelle lui-même. L'isoler évite que la
// suppression/l'écrasement de l'un affecte accidentellement l'autre,
// et rend `iloc provider profile remove` (qui purge tout via
// purge_all) trivialement correct : un seul point de suppression par
// compte suffit toujours à tout effacer proprement.

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthTokenCache {
    pub access_token: String,
    /// Timestamp Unix (secondes) d'expiration du jeton.
    pub expires_at: i64,
}

pub fn save_oauth_cache(slug: &str, account: &str, cache: &OAuthTokenCache) -> Result<()> {
    let cache_json = serde_json::to_string(cache).context("Sérialisation du cache OAuth2")?;
    let user = format!("{}.{}", account, KR_OAUTH_CACHE_KEY);
    let kr_ok = (|| -> Result<()> {
        let entry = Entry::new(&kr_service(slug), &user).map_err(|e| anyhow::anyhow!("{}", e))?;
        entry.set_password(&cache_json).context("écriture trousseau (cache OAuth2)")?;
        let readback = Entry::new(&kr_service(slug), &user)
            .map_err(|e| anyhow::anyhow!("{}", e))?
            .get_password()
            .context("vérification post-écriture (cache OAuth2)")?;
        if readback != cache_json {
            anyhow::bail!("le trousseau a accepté l'écriture mais relit une valeur différente");
        }
        Ok(())
    })();

    match kr_ok {
        Ok(()) => Ok(()),
        // Silencieux (pas de warning utilisateur ici, contrairement à
        // save_fields) : un cache est par nature un optimisation, pas
        // une donnée dont la perte est visible ou grave — le prochain
        // appel referait simplement l'échange de jeton.
        Err(_) => fallback_save(slug, &format!("{}.oauth", account), &cache_json),
    }
}

pub fn load_oauth_cache(slug: &str, account: &str) -> Result<Option<OAuthTokenCache>> {
    let user = format!("{}.{}", account, KR_OAUTH_CACHE_KEY);
    let try_kr = (|| -> Result<String> {
        let entry = Entry::new(&kr_service(slug), &user).map_err(|e| anyhow::anyhow!("{}", e))?;
        entry.get_password().map_err(|e| anyhow::anyhow!("{}", e))
    })();

    let cache_json = match try_kr {
        Ok(j) => j,
        Err(_) => match fallback_load(slug, &format!("{}.oauth", account)) {
            Ok(j) => j,
            Err(_) => return Ok(None),
        },
    };

    Ok(serde_json::from_str(&cache_json).ok())
}

pub fn clear_oauth_cache(slug: &str, account: &str) {
    let user = format!("{}.{}", account, KR_OAUTH_CACHE_KEY);
    if let Ok(entry) = Entry::new(&kr_service(slug), &user) {
        let _ = entry.delete_password();
    }
    fallback_delete(slug, &format!("{}.oauth", account));
}

// ── Permissions ──────────────────────────────────────────────

#[cfg(unix)]
fn set_owner_only(path: &PathBuf) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_owner_only(_path: &PathBuf) -> Result<()> { Ok(()) }

// ── Tests ────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn use_temp_home() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("iloc_prov_test_{}", uuid_like()));
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

    fn sample_fields() -> HashMap<String, String> {
        let mut m = HashMap::new();
        m.insert("token".to_string(), "sk_test_abc123XYZ".to_string());
        m
    }

    #[test]
    fn round_trip_single_profile() {
        let _home = use_temp_home();
        let profile = ProviderProfile {
            name: "default".to_string(),
            identity_label: Some("dev@example.com".to_string()),
            api_url_override: None,
            account: "testco-default-acc1".to_string(),
            connected_at: "2026-08-07T00:00:00Z".to_string(),
        };
        upsert_profile("testco", profile, &sample_fields(), true).unwrap();

        let creds = resolve_profile("testco", None).unwrap();
        assert_eq!(creds.name, "default");
        assert_eq!(creds.identity_label.as_deref(), Some("dev@example.com"));

        let loaded_fields = load_fields("testco", "testco-default-acc1").unwrap();
        assert_eq!(loaded_fields.get("token").unwrap(), "sk_test_abc123XYZ");
    }

    #[test]
    fn multiple_profiles_isolated_credentials() {
        let _home = use_temp_home();

        let mut f1 = HashMap::new();
        f1.insert("token".to_string(), "token-perso".to_string());
        let mut f2 = HashMap::new();
        f2.insert("token".to_string(), "token-travail".to_string());

        upsert_profile("testco", ProviderProfile {
            name: "perso".to_string(), identity_label: None, api_url_override: None,
            account: "acc-perso".to_string(), connected_at: "2026-08-07T00:00:00Z".to_string(),
        }, &f1, true).unwrap();

        upsert_profile("testco", ProviderProfile {
            name: "travail".to_string(), identity_label: None, api_url_override: None,
            account: "acc-travail".to_string(), connected_at: "2026-08-07T00:00:00Z".to_string(),
        }, &f2, false).unwrap();

        // Le profil actif reste "perso" (set_active=false pour "travail")
        let active = resolve_profile("testco", None).unwrap();
        assert_eq!(active.name, "perso");

        // Chaque profil garde ses PROPRES identifiants, jamais mélangés
        assert_eq!(load_fields("testco", "acc-perso").unwrap().get("token").unwrap(), "token-perso");
        assert_eq!(load_fields("testco", "acc-travail").unwrap().get("token").unwrap(), "token-travail");
    }

    #[test]
    fn different_providers_never_share_keyring_namespace() {
        // Deux providers différents avec le MÊME nom de compte ne
        // doivent jamais pouvoir lire les identifiants l'un de l'autre
        // — c'est le garde-fou d'isolation du service keyring par slug.
        let _home = use_temp_home();
        let mut fa = HashMap::new();
        fa.insert("token".to_string(), "secret-for-provider-a".to_string());
        let mut fb = HashMap::new();
        fb.insert("token".to_string(), "secret-for-provider-b".to_string());

        save_fields("provider-a", "shared-account-name", &fa).unwrap();
        save_fields("provider-b", "shared-account-name", &fb).unwrap();

        assert_eq!(load_fields("provider-a", "shared-account-name").unwrap().get("token").unwrap(), "secret-for-provider-a");
        assert_eq!(load_fields("provider-b", "shared-account-name").unwrap().get("token").unwrap(), "secret-for-provider-b");
    }

    #[test]
    fn remove_profile_deletes_credentials_too() {
        let _home = use_temp_home();
        upsert_profile("testco", ProviderProfile {
            name: "default".to_string(), identity_label: None, api_url_override: None,
            account: "acc-to-remove".to_string(), connected_at: "2026-08-07T00:00:00Z".to_string(),
        }, &sample_fields(), true).unwrap();

        assert!(load_fields("testco", "acc-to-remove").is_ok());
        assert!(remove_profile("testco", "default").unwrap());
        assert!(load_fields("testco", "acc-to-remove").is_err(), "les identifiants doivent être purgés avec le profil");
    }

    #[test]
    fn basic_auth_stores_both_fields_together() {
        let _home = use_temp_home();
        let mut fields = HashMap::new();
        fields.insert("username".to_string(), "alice".to_string());
        fields.insert("password".to_string(), "hunter2".to_string());

        upsert_profile("basicapi", ProviderProfile {
            name: "default".to_string(), identity_label: None, api_url_override: None,
            account: "basic-acc".to_string(), connected_at: "2026-08-07T00:00:00Z".to_string(),
        }, &fields, true).unwrap();

        let loaded = load_fields("basicapi", "basic-acc").unwrap();
        assert_eq!(loaded.get("username").unwrap(), "alice");
        assert_eq!(loaded.get("password").unwrap(), "hunter2");
    }

    #[test]
    fn resolve_without_profiles_gives_clear_error_with_slug() {
        let _home = use_temp_home();
        let err = resolve_profile("nonexistent-provider", None).unwrap_err();
        assert!(err.to_string().contains("iloc connect nonexistent-provider"), "erreur inattendue : {err}");
    }

    #[test]
    fn purge_all_removes_config_file_and_credentials() {
        let _home = use_temp_home();
        upsert_profile("purgeme", ProviderProfile {
            name: "default".to_string(), identity_label: None, api_url_override: None,
            account: "purge-acc".to_string(), connected_at: "2026-08-07T00:00:00Z".to_string(),
        }, &sample_fields(), true).unwrap();

        assert!(provider_config_path("purgeme").unwrap().exists());
        purge_all("purgeme").unwrap();
        assert!(!provider_config_path("purgeme").unwrap().exists());
        assert!(load_fields("purgeme", "purge-acc").is_err());
    }

    #[test]
    fn api_url_override_takes_precedence() {
        let _home = use_temp_home();
        upsert_profile("selfhosted", ProviderProfile {
            name: "default".to_string(), identity_label: None,
            api_url_override: Some("https://gitlab.mycompany.internal".to_string()),
            account: "sh-acc".to_string(), connected_at: "2026-08-07T00:00:00Z".to_string(),
        }, &sample_fields(), true).unwrap();

        let profile = resolve_profile("selfhosted", None).unwrap();
        assert_eq!(profile.api_url_override.as_deref(), Some("https://gitlab.mycompany.internal"));
    }
}
