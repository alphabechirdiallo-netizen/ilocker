// ============================================================
//  cloud_store.rs — BYOC credential storage (multi-profils)
//
//  v1.12.0 — Réécriture complète
//  ───────────────────────────────
//  Ce n'est PAS ilocker qui stocke le projet de l'utilisateur :
//  c'est l'utilisateur qui connecte SON propre cloud (AWS, GCP,
//  Azure-compatible via passerelle S3, DigitalOcean, Supabase,
//  Backblaze, Cloudflare R2, Wasabi, MinIO...), et ilocker n'est
//  qu'un connecteur chiffré de bout en bout entre son disque et
//  son cloud. Aucune authentification auprès d'un serveur ilocker
//  n'est requise — tout fonctionne 100% hors-ligne par rapport à
//  l'éditeur de l'outil.
//
//  Multi-profils : un utilisateur peut enregistrer PLUSIEURS
//  comptes cloud en parallèle (ex: "aws-perso", "client-x-gcp",
//  "backup-secondaire") et choisir lequel utiliser à chaque
//  commande via --profile, ou désigner un profil "actif" par
//  défaut.
//
//  Two-layer storage strategy (par profil) :
//
//  Layer 1 — ~/.config/ilocker/cloud.toml  (non-sensible)
//    nom du profil, provider, bucket, region, endpoint
//
//  Layer 2 — OS native keychain  (sensible)
//    access_key_id, secret_access_key
//    macOS  → Keychain
//    Linux  → kernel keyring
//    Windows→ Credential Manager
// ============================================================

use anyhow::{Context, Result};
use keyring::Entry;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const KR_SERVICE: &str = "ilocker-cloud";
const KR_KEY_ACCESS: &str = "access_key_id";
const KR_KEY_SECRET: &str = "secret_access_key";

// ── Providers ─────────────────────────────────────────────────
//
// La plupart sont compatibles avec l'API S3 (signature AWS SigV4).
// Azure Blob Storage est supporté via un second client HTTP dédié
// (azure_client.rs) car son protocole diffère fondamentalement de
// l'API S3 (auth "Shared Key" au lieu de SigV4). La distinction est
// transparente pour le reste du code grâce à `CloudBackend`
// (cloud_backend.rs).

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CloudProvider {
    S3,
    Backblaze,
    Minio,
    DigitalOcean,
    Supabase,
    Gcs,
    R2,
    Wasabi,
    Azure,
}

impl CloudProvider {
    pub fn label(&self) -> &'static str {
        match self {
            CloudProvider::S3           => "AWS S3",
            CloudProvider::Backblaze    => "Backblaze B2",
            CloudProvider::Minio        => "MinIO (self-hosted)",
            CloudProvider::DigitalOcean => "DigitalOcean Spaces",
            CloudProvider::Supabase     => "Supabase Storage",
            CloudProvider::Gcs          => "Google Cloud Storage",
            CloudProvider::R2           => "Cloudflare R2",
            CloudProvider::Wasabi       => "Wasabi",
            CloudProvider::Azure        => "Azure Blob Storage",
        }
    }

    pub fn slug(&self) -> &'static str {
        match self {
            CloudProvider::S3           => "s3",
            CloudProvider::Backblaze    => "backblaze",
            CloudProvider::Minio        => "minio",
            CloudProvider::DigitalOcean => "digitalocean",
            CloudProvider::Supabase     => "supabase",
            CloudProvider::Gcs          => "gcs",
            CloudProvider::R2           => "r2",
            CloudProvider::Wasabi       => "wasabi",
            CloudProvider::Azure        => "azure",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().replace(['_', ' '], "-").as_str() {
            "s3" | "aws" | "aws-s3"                      => Some(CloudProvider::S3),
            "b2" | "backblaze"                            => Some(CloudProvider::Backblaze),
            "minio"                                       => Some(CloudProvider::Minio),
            "do" | "digitalocean" | "digital-ocean" | "spaces" | "ocean" | "océan" => Some(CloudProvider::DigitalOcean),
            "supabase" | "sb"                             => Some(CloudProvider::Supabase),
            "gcs" | "gcp" | "google" | "google-cloud"     => Some(CloudProvider::Gcs),
            "r2" | "cloudflare" | "cloudflare-r2"         => Some(CloudProvider::R2),
            "wasabi"                                      => Some(CloudProvider::Wasabi),
            "azure" | "azure-blob" | "blob" | "abs"        => Some(CloudProvider::Azure),
            _                                              => None,
        }
    }

    /// Tous les providers supportés, pour affichage (`iloc config cloud add`).
    pub fn all() -> &'static [CloudProvider] {
        &[
            CloudProvider::S3, CloudProvider::Backblaze, CloudProvider::Minio,
            CloudProvider::DigitalOcean, CloudProvider::Supabase, CloudProvider::Gcs,
            CloudProvider::R2, CloudProvider::Wasabi, CloudProvider::Azure,
        ]
    }

    /// Indique si endpoint/region doivent être saisis manuellement
    /// (providers nécessitant un identifiant de compte/projet propre
    /// à l'utilisateur dans l'URL — impossible à deviner).
    pub fn requires_manual_endpoint(&self) -> bool {
        matches!(self, CloudProvider::Minio | CloudProvider::Supabase | CloudProvider::R2)
    }

    /// Endpoint par défaut, gabarit avec `{region}` substitué si présent.
    /// `None` → l'utilisateur DOIT en fournir un (`requires_manual_endpoint`).
    pub fn endpoint_template(&self, region: &str) -> Option<String> {
        match self {
            CloudProvider::S3           => None, // défaut AWS standard, géré par S3Client
            CloudProvider::Backblaze    => Some(format!("https://s3.{}.backblazeb2.com", region)),
            CloudProvider::Minio        => None, // toujours manuel
            CloudProvider::DigitalOcean => Some(format!("https://{}.digitaloceanspaces.com", region)),
            CloudProvider::Supabase     => None, // toujours manuel (URL par projet)
            CloudProvider::Gcs          => Some("https://storage.googleapis.com".to_string()),
            CloudProvider::R2           => None, // toujours manuel (URL par compte Cloudflare)
            CloudProvider::Wasabi       => Some(format!("https://s3.{}.wasabisys.com", region)),
            CloudProvider::Azure        => Some(format!("https://{}.blob.core.windows.net", region)),
        }
    }

    /// Région par défaut suggérée (juste une aide à la saisie).
    pub fn default_region_hint(&self) -> &'static str {
        match self {
            CloudProvider::S3           => "us-east-1",
            CloudProvider::Backblaze    => "us-west-004",
            CloudProvider::Minio        => "us-east-1",
            CloudProvider::DigitalOcean => "nyc3",
            CloudProvider::Supabase     => "us-east-1",
            CloudProvider::Gcs          => "auto",
            CloudProvider::R2           => "auto",
            CloudProvider::Wasabi       => "us-east-1",
            CloudProvider::Azure        => "moncomptestockage",
        }
    }

    /// Astuce affichée pendant l'assistant de configuration.
    pub fn setup_hint(&self) -> &'static str {
        match self {
            CloudProvider::S3 =>
                "Crée un utilisateur IAM avec une politique limitée à ce bucket (PutObject/GetObject/ListBucket/DeleteObject).",
            CloudProvider::Backblaze =>
                "Crée une 'Application Key' scopée à un seul bucket dans le tableau de bord Backblaze B2.",
            CloudProvider::Minio =>
                "Utilise `mc admin user add` ou la console MinIO pour créer une access key dédiée.",
            CloudProvider::DigitalOcean =>
                "Crée une clé d'accès Spaces depuis le panneau 'API' de DigitalOcean (région = celle de ton Space).",
            CloudProvider::Supabase =>
                "Dashboard Supabase → Project Settings → Storage → S3 Connection : active l'accès S3 et génère des clés.",
            CloudProvider::Gcs =>
                "Console GCP → Cloud Storage → Settings → Interoperability : génère une clé d'accès HMAC.",
            CloudProvider::R2 =>
                "Dashboard Cloudflare → R2 → Manage API Tokens : crée un token avec accès au bucket.",
            CloudProvider::Wasabi =>
                "Crée une access key depuis la console Wasabi (region = celle choisie à la création du bucket).",
            CloudProvider::Azure =>
                "Portail Azure → ton compte de stockage → 'Access keys' : copie la clé (key1 ou key2). \
                 Le 'bucket' demandé ci-dessous est le nom du container Blob Storage ; la 'région' demandée \
                 est le NOM DE TON COMPTE DE STOCKAGE (pas une région géographique).",
        }
    }
}

// ── Profil unique (config non-sensible) ──────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudProfile {
    pub name:     String,
    pub provider: CloudProvider,
    pub bucket:   String,
    pub region:   String,
    pub endpoint: Option<String>,
    /// Identifiant unique utilisé comme compte keyring — généré
    /// une fois, jamais réutilisé entre profils (même nom changé).
    pub account:  String,
}

// ── Fichier complet (tous les profils) ───────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CloudProfiles {
    pub active:   Option<String>,
    #[serde(default)]
    pub profiles: Vec<CloudProfile>,
}

// ── Ancien format (v1.10 et antérieur) — pour migration douce ──

#[derive(Debug, Clone, Deserialize)]
struct LegacyCloudConfig {
    provider: CloudProvider,
    bucket:   String,
    region:   String,
    endpoint: Option<String>,
    account:  String,
}

// ── Credentials résolus (avec secrets) ───────────────────────

#[derive(Debug, Clone)]
pub struct CloudCredentials {
    pub profile_name:      String,
    pub provider:          CloudProvider,
    pub bucket:            String,
    pub region:            String,
    pub endpoint:          Option<String>,
    pub access_key_id:     String,
    pub secret_access_key: String,
}

// ── Chemin du fichier de config ──────────────────────────────

pub fn cloud_config_path() -> Result<PathBuf> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .context("Cannot determine home directory")?;
    Ok(PathBuf::from(home).join(".config").join("ilocker").join("cloud.toml"))
}

// ── Chargement (avec migration douce depuis l'ancien format) ──

pub fn load_profiles() -> Result<CloudProfiles> {
    let path = cloud_config_path()?;
    if !path.exists() { return Ok(CloudProfiles::default()); }

    let raw = std::fs::read_to_string(&path)?;

    if let Ok(profiles) = toml::from_str::<CloudProfiles>(&raw) {
        if !profiles.profiles.is_empty() || raw.contains("[[profiles]]") {
            return Ok(profiles);
        }
    }

    // Migration douce : ancien format à un seul profil → "default"
    if let Ok(legacy) = toml::from_str::<LegacyCloudConfig>(&raw) {
        let migrated = CloudProfiles {
            active: Some("default".to_string()),
            profiles: vec![CloudProfile {
                name:     "default".to_string(),
                provider: legacy.provider,
                bucket:   legacy.bucket,
                region:   legacy.region,
                endpoint: legacy.endpoint,
                account:  legacy.account,
            }],
        };
        save_profiles(&migrated)?;
        return Ok(migrated);
    }

    Ok(CloudProfiles::default())
}

pub fn save_profiles(cfg: &CloudProfiles) -> Result<()> {
    let path = cloud_config_path()?;
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    let raw = toml::to_string_pretty(cfg).context("Failed to serialise cloud config")?;
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, raw)?;
    set_owner_only(&tmp)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

// ── Résolution d'un profil par nom (ou actif si None) ────────

pub fn resolve_profile(name: Option<&str>) -> Result<CloudProfile> {
    let cfg = load_profiles()?;
    if cfg.profiles.is_empty() {
        anyhow::bail!(
            "Aucun cloud configuré.  Lancez `iloc config cloud add` pour connecter votre propre AWS/GCP/DigitalOcean/Supabase/Backblaze/MinIO/R2/Wasabi."
        );
    }

    let target = match name {
        Some(n) => n.to_string(),
        None    => cfg.active.clone().ok_or_else(|| anyhow::anyhow!(
            "Plusieurs profils cloud existent mais aucun n'est actif.  Utilisez `iloc config cloud use <nom>` ou passez `--profile <nom>`."
        ))?,
    };

    cfg.profiles.into_iter().find(|p| p.name == target).ok_or_else(|| {
        anyhow::anyhow!("Profil cloud '{}' introuvable.  Voir `iloc config cloud list`.", target)
    })
}

pub fn require_credentials(profile_name: Option<&str>) -> Result<CloudCredentials> {
    let profile = resolve_profile(profile_name)?;
    let (access_key_id, secret_access_key) = load_secrets(&profile.account)?;
    Ok(CloudCredentials {
        profile_name: profile.name,
        provider:     profile.provider,
        bucket:       profile.bucket,
        region:       profile.region,
        endpoint:     profile.endpoint,
        access_key_id,
        secret_access_key,
    })
}

// ── Gestion des profils ────────────────────────────────────────

pub fn upsert_profile(profile: CloudProfile, set_active: bool) -> Result<()> {
    let mut cfg = load_profiles()?;
    let profile_name = profile.name.clone();
    if let Some(existing) = cfg.profiles.iter_mut().find(|p| p.name == profile.name) {
        *existing = profile;
    } else {
        cfg.profiles.push(profile);
    }
    if set_active || cfg.active.is_none() {
        cfg.active = Some(profile_name);
    }
    save_profiles(&cfg)
}

pub fn set_active(name: &str) -> Result<()> {
    let mut cfg = load_profiles()?;
    if !cfg.profiles.iter().any(|p| p.name == name) {
        anyhow::bail!("Profil '{}' introuvable.  Voir `iloc config cloud list`.", name);
    }
    cfg.active = Some(name.to_string());
    save_profiles(&cfg)
}

pub fn remove_profile(name: &str) -> Result<bool> {
    let mut cfg = load_profiles()?;
    let before = cfg.profiles.len();
    let removed_account = cfg.profiles.iter().find(|p| p.name == name).map(|p| p.account.clone());
    cfg.profiles.retain(|p| p.name != name);

    if cfg.profiles.len() == before {
        return Ok(false);
    }
    if let Some(account) = removed_account {
        let _ = delete_secrets(&account);
    }
    if cfg.active.as_deref() == Some(name) {
        cfg.active = cfg.profiles.first().map(|p| p.name.clone());
    }
    save_profiles(&cfg)?;
    Ok(true)
}

pub fn list_profiles() -> Result<CloudProfiles> {
    load_profiles()
}

// ── Keyring: store / retrieve secrets ────────────────────────
//
// Stratégie de repli : sur certains environnements (CI, conteneurs
// minimalistes, kernels restreints sans keyring persistant, serveurs
// sans D-Bus/session), le trousseau natif de l'OS peut être
// indisponible. Plutôt que de bloquer complètement l'usage du
// cloud personnel dans ces cas, on retombe sur un fichier local
// (permissions 0600, propriétaire uniquement) — exactement la
// stratégie par défaut d'outils comme `aws configure`. L'utilisateur
// en est explicitement informé à chaque fois que ce repli est utilisé.

fn fallback_dir() -> Result<PathBuf> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .context("Cannot determine home directory")?;
    Ok(PathBuf::from(home).join(".config").join("ilocker").join("credentials"))
}

fn fallback_path(account: &str) -> Result<PathBuf> {
    Ok(fallback_dir()?.join(format!("{}.vault", account)))
}

/// Chemin de l'ancien format (JSON en clair). Migration automatique uniquement.
fn fallback_path_legacy(account: &str) -> Result<PathBuf> {
    Ok(fallback_dir()?.join(format!("{}.json", account)))
}

fn fallback_save(account: &str, access_key: &str, secret_key: &str) -> Result<()> {
    let dir = fallback_dir()?;
    std::fs::create_dir_all(&dir)?;
    let payload = serde_json::json!({
        "access_key_id": access_key,
        "secret_access_key": secret_key,
    });
    let path      = fallback_path(account)?;
    let encrypted = crate::credential_vault::encrypt_credential(&payload.to_string())?;
    std::fs::write(&path, &encrypted)?;
    set_owner_only(&path)?;
    Ok(())
}

/// Migration transparente depuis l'ancien format en clair (deux
/// champs) — même mécanisme que github_store.rs, adapté à la paire
/// access_key/secret_key de cloud_store.rs.
fn migrate_legacy_if_present(account: &str) -> Option<(String, String)> {
    let legacy_path = fallback_path_legacy(account).ok()?;
    let raw_str = std::fs::read_to_string(&legacy_path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw_str).ok()?;
    let ak = v["access_key_id"].as_str()?.to_string();
    let sk = v["secret_access_key"].as_str()?.to_string();

    if fallback_save(account, &ak, &sk).is_ok() {
        let _ = std::fs::remove_file(&legacy_path);
    }
    Some((ak, sk))
}

fn fallback_load(account: &str) -> Result<(String, String)> {
    let path = fallback_path(account)?;

    if let Ok(raw) = std::fs::read(&path) {
        if let Ok(json) = crate::credential_vault::decrypt_credential(&raw) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&json) {
                if let (Some(ak), Some(sk)) = (v["access_key_id"].as_str(), v["secret_access_key"].as_str()) {
                    if let Ok(legacy) = fallback_path_legacy(account) {
                        let _ = std::fs::remove_file(legacy);
                    }
                    return Ok((ak.to_string(), sk.to_string()));
                }
            }
        }
    }

    if let Some(pair) = migrate_legacy_if_present(account) {
        return Ok(pair);
    }

    anyhow::bail!("No fallback credential file found either")
}

fn fallback_delete(account: &str) {
    if let Ok(path) = fallback_path(account) {
        let _ = std::fs::remove_file(path);
    }
}

pub fn save_secrets(account: &str, access_key: &str, secret_key: &str) -> Result<()> {
    let ak_user = format!("{}.{}", account, KR_KEY_ACCESS);
    let sk_user = format!("{}.{}", account, KR_KEY_SECRET);

    let keyring_ok = (|| -> Result<()> {
        let ak_entry = Entry::new(KR_SERVICE, &ak_user).map_err(|e| anyhow::anyhow!("{}", e))?;
        ak_entry.set_password(access_key).context("set access_key_id")?;
        let sk_entry = Entry::new(KR_SERVICE, &sk_user).map_err(|e| anyhow::anyhow!("{}", e))?;
        sk_entry.set_password(secret_key).context("set secret_access_key")?;

        // Vérification immédiate par relecture : certains environnements
        // acceptent l'écriture (set_password retourne Ok) sans jamais
        // rendre la valeur relisible ensuite — perdant les credentials
        // en silence, SANS déclencher le repli fichier, puisque le code
        // croyait le trousseau fiable. Découvert en testant en conditions
        // réelles (même cause que le bug déjà corrigé dans github_store.rs
        // et vercel_store.rs, jamais reporté ici jusqu'à présent).
        let readback_ak = Entry::new(KR_SERVICE, &ak_user)
            .map_err(|e| anyhow::anyhow!("{}", e))?
            .get_password()
            .context("vérification post-écriture (access_key)")?;
        let readback_sk = Entry::new(KR_SERVICE, &sk_user)
            .map_err(|e| anyhow::anyhow!("{}", e))?
            .get_password()
            .context("vérification post-écriture (secret_key)")?;
        if readback_ak != access_key || readback_sk != secret_key {
            anyhow::bail!("le trousseau a accepté l'écriture mais relit des valeurs différentes");
        }
        Ok(())
    })();

    match keyring_ok {
        Ok(())  => Ok(()),
        Err(e)  => {
            eprintln!(
                "  {} trousseau système indisponible sur cette machine ({}).",
                "⚠".to_string(), e
            );
            eprintln!(
                "  {} repli sur un fichier local chiffré par permissions strictes (0600) : {}",
                "→".to_string(),
                fallback_path(account)?.display()
            );
            eprintln!(
                "  {} sécurité réduite — assurez-vous que le chiffrement de disque est activé sur cette machine.",
                "ℹ".to_string()
            );
            fallback_save(account, access_key, secret_key)
        }
    }
}

pub fn load_secrets(account: &str) -> Result<(String, String)> {
    let try_keyring = (|| -> Result<(String, String)> {
        let ak_entry = Entry::new(KR_SERVICE, &format!("{}.{}", account, KR_KEY_ACCESS))
            .map_err(|e| anyhow::anyhow!("{}", e))?;
        let access_key = ak_entry.get_password().map_err(|e| anyhow::anyhow!("{}", e))?;

        let sk_entry = Entry::new(KR_SERVICE, &format!("{}.{}", account, KR_KEY_SECRET))
            .map_err(|e| anyhow::anyhow!("{}", e))?;
        let secret_key = sk_entry.get_password().map_err(|e| anyhow::anyhow!("{}", e))?;

        Ok((access_key, secret_key))
    })();

    match try_keyring {
        Ok(creds) => Ok(creds),
        Err(_) => fallback_load(account)
            .context("access_key_id/secret_access_key not found in keychain or local fallback — re-run `iloc config cloud add`"),
    }
}

pub fn delete_secrets(account: &str) -> Result<()> {
    for key in [KR_KEY_ACCESS, KR_KEY_SECRET] {
        if let Ok(entry) = Entry::new(KR_SERVICE, &format!("{}.{}", account, key)) {
            let _ = entry.delete_password();
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn use_temp_home() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("iloc_cloud_test_{}", uuid_like()));
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
    fn secrets_fallback_round_trip() {
        let _home = use_temp_home();
        save_secrets("aws-test-account", "AKIAFAKEKEYID", "fakeSecretAccessKey123").unwrap();
        let (ak, sk) = load_secrets("aws-test-account").unwrap();
        assert_eq!(ak, "AKIAFAKEKEYID");
        assert_eq!(sk, "fakeSecretAccessKey123");
        delete_secrets("aws-test-account").unwrap();
        assert!(load_secrets("aws-test-account").is_err());
    }

    #[test]
    fn migrates_legacy_plaintext_json_transparently() {
        let _home = use_temp_home();
        let dir = fallback_dir().unwrap();
        std::fs::create_dir_all(&dir).unwrap();

        let legacy_path = fallback_path_legacy("aws-legacy-account").unwrap();
        std::fs::write(
            &legacy_path,
            serde_json::to_string(&serde_json::json!({
                "access_key_id": "AKIAOLDKEY",
                "secret_access_key": "oldSecret456",
            }))
            .unwrap(),
        )
        .unwrap();

        let (ak, sk) = load_secrets("aws-legacy-account").unwrap();
        assert_eq!(ak, "AKIAOLDKEY");
        assert_eq!(sk, "oldSecret456");
        assert!(!legacy_path.exists());
        assert!(fallback_path("aws-legacy-account").unwrap().exists());

        let (ak2, sk2) = load_secrets("aws-legacy-account").unwrap();
        assert_eq!(ak2, "AKIAOLDKEY");
        assert_eq!(sk2, "oldSecret456");
    }
}
