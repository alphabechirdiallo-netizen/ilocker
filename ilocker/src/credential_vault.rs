// ============================================================
//  credential_vault.rs — Chiffrement au repos du fallback local
//
//  Contexte :
//    github_store.rs / vercel_store.rs / supabase_store.rs /
//    cloud_store.rs tentent d'abord le trousseau système (OS
//    keychain via la crate `keyring`). Quand celui-ci est
//    indisponible (ex : serveur Linux headless, conteneur, config
//    actuelle qui ne compile aucun backend Linux), ils basculent
//    sur un fichier local protégé en permissions 0600.
//
//    Ce module ajoute une couche de chiffrement à ce fichier de
//    secours, pour qu'il ne soit jamais stocké en clair — même
//    en fallback. C'est strictement au-dessus du standard de
//    l'industrie sur ce point précis : à titre de comparaison,
//    le fallback du GitHub CLI officiel (`gh`) écrit le token en
//    texte brut pur dans ~/.config/gh/hosts.yml.
//
//  Primitive : ChaCha20-Poly1305 — la même que cloud_crypto.rs,
//  aucune nouvelle dépendance introduite.
//
//  Clé : générée aléatoirement (CSPRNG) une seule fois au premier
//  usage, stockée séparément du fichier chiffré lui-même —
//  ~/.config/ilocker/.vault-key, permissions 0600.
//
//  Modèle de menace couvert : un tiers qui obtient UNIQUEMENT le
//  fichier de credentials (backup partiel, erreur de partage,
//  export incomplet) ne peut rien en tirer sans le fichier de
//  clé séparé. Ça ne protège pas contre un accès root complet à
//  la machine (rien ne le peut, sur une machine où le process
//  tourne et déchiffre activement) — c'est un renforcement
//  defense-in-depth, pas une garantie absolue.
//
//  Choix délibéré : PAS de backend kernel-keyring (linux-keyutils)
//  en amont de ce fichier. Ce backend perd tout son contenu à
//  chaque redémarrage machine (le noyau vide tous les keyrings au
//  reboot), ce qui casserait silencieusement l'authentification
//  après toute mise à jour de sécurité automatique sur un serveur
//  de production — inacceptable pour un usage entreprise non
//  supervisé. Le fichier chiffré, lui, survit aux redémarrages.
// ============================================================

use anyhow::{Context, Result};
use chacha20poly1305::{
    aead::{Aead, AeadCore, KeyInit, OsRng},
    ChaCha20Poly1305, Key, Nonce,
};
use std::path::PathBuf;

/// Répertoire ~/.config/ilocker (même racine que fallback_dir() dans
/// les *_store.rs, mais un cran au-dessus de credentials/).
fn config_root() -> Result<PathBuf> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .context("Cannot determine home directory")?;
    Ok(PathBuf::from(home).join(".config").join("ilocker"))
}

fn vault_key_path() -> Result<PathBuf> {
    Ok(config_root()?.join(".vault-key"))
}

/// Charge la clé de chiffrement locale, ou en génère une nouvelle
/// (CSPRNG) si c'est le tout premier appel sur cette machine.
fn load_or_create_vault_key() -> Result<[u8; 32]> {
    let path = vault_key_path()?;

    if let Ok(raw) = std::fs::read(&path) {
        if raw.len() == 32 {
            let mut key = [0u8; 32];
            key.copy_from_slice(&raw);
            return Ok(key);
        }
        // Fichier présent mais de taille incorrecte : corrompu.
        anyhow::bail!(
            "Fichier de clé de vault corrompu ({} octets, 32 attendus) : {}",
            raw.len(),
            path.display()
        );
    }

    // Première utilisation : générer une nouvelle clé aléatoire.
    let key = ChaCha20Poly1305::generate_key(&mut OsRng);
    let dir = config_root()?;
    std::fs::create_dir_all(&dir).context("création de ~/.config/ilocker")?;
    std::fs::write(&path, key.as_slice()).context("écriture de la clé de vault")?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .context("permissions sur la clé de vault")?;
    }

    let mut out = [0u8; 32];
    out.copy_from_slice(&key);
    Ok(out)
}

/// Chiffre des octets bruts (ex : un JSON sérialisé) et retourne
/// `[nonce(12) || ciphertext + tag(16)]`, prêt à écrire sur disque.
/// Version générale — `encrypt_credential` (pour une simple `&str`,
/// le cas le plus courant) en est un raccourci ci-dessous.
pub fn encrypt_credential_bytes(plaintext: &[u8]) -> Result<Vec<u8>> {
    let key_bytes = load_or_create_vault_key()?;
    let cipher = ChaCha20Poly1305::new(Key::from_slice(&key_bytes));
    let nonce = ChaCha20Poly1305::generate_nonce(&mut OsRng);

    let ciphertext = cipher
        .encrypt(&nonce, plaintext)
        .map_err(|e| anyhow::anyhow!("Chiffrement du credential échoué : {:?}", e))?;

    let mut out = Vec::with_capacity(12 + ciphertext.len());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// Déchiffre un blob produit par `encrypt_credential_bytes`, retourne
/// les octets bruts (pas nécessairement de l'UTF-8 valide).
pub fn decrypt_credential_bytes(wire: &[u8]) -> Result<Vec<u8>> {
    if wire.len() < 12 + 16 {
        anyhow::bail!(
            "Fichier de credential chiffré trop court ({} octets)",
            wire.len()
        );
    }
    let key_bytes = load_or_create_vault_key()?;
    let (nonce_bytes, ciphertext) = wire.split_at(12);
    let cipher = ChaCha20Poly1305::new(Key::from_slice(&key_bytes));
    let nonce = Nonce::from_slice(nonce_bytes);

    cipher
        .decrypt(nonce, ciphertext)
        .map_err(|_| anyhow::anyhow!("Déchiffrement échoué — clé de vault absente ou fichier corrompu"))
}

/// Chiffre `plaintext` (ex : un token d'accès) — raccourci `&str` de
/// `encrypt_credential_bytes`.
pub fn encrypt_credential(plaintext: &str) -> Result<Vec<u8>> {
    encrypt_credential_bytes(plaintext.as_bytes())
}

/// Déchiffre un blob produit par `encrypt_credential` en `String` —
/// raccourci `&str` de `decrypt_credential_bytes`.
pub fn decrypt_credential(wire: &[u8]) -> Result<String> {
    let plaintext = decrypt_credential_bytes(wire)?;
    String::from_utf8(plaintext).context("Contenu déchiffré invalide (UTF-8)")
}

// ── Tests ─────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        std::env::set_var("HOME", std::env::temp_dir().join("ilocker-vault-test-1"));
        let token = "ghp_exempleDeTokenSecret1234567890";
        let wire = encrypt_credential(token).unwrap();
        let back = decrypt_credential(&wire).unwrap();
        assert_eq!(token, back);
    }

    #[test]
    fn tampered_ciphertext_fails() {
        std::env::set_var("HOME", std::env::temp_dir().join("ilocker-vault-test-2"));
        let mut wire = encrypt_credential("secret-token").unwrap();
        let last = wire.len() - 1;
        wire[last] ^= 0xFF; // corrompt le dernier octet du tag Poly1305
        assert!(decrypt_credential(&wire).is_err());
    }

    #[test]
    fn too_short_fails_cleanly() {
        assert!(decrypt_credential(&[1, 2, 3]).is_err());
    }
}
