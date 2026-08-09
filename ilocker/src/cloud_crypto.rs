// ============================================================
//  cloud_crypto.rs — Zero-Knowledge cloud encryption  (Phase 3)
//
//  Algorithm: ChaCha20-Poly1305
//    • 256-bit key, 96-bit nonce, 128-bit authentication tag
//    • Faster than AES-GCM on systems without hardware AES
//      (especially ARM-based developer machines)
//    • IETF standardised (RFC 8439)
//
//  Key derivation:
//    The encryption key is derived from the user's ilocker
//    project key (already a 128-bit UUID) via SHA-256.
//    This is the same key used by the P2P layer (Phase 3 reuses
//    the crypto.rs model but with ChaCha20 for cloud uploads).
//
//    cloud_key = SHA-256("ilocker-cloud:" ‖ project_id)
//
//  Nonce derivation (deterministic, no coordination needed):
//    nonce = SHA-256("nonce:" ‖ chunk_sha256)[..12]
//    Safe because chunks are content-addressed (immutable).
//
//  Wire format:
//    [12 B nonce][ciphertext + 16 B Poly1305 tag]
//    Overhead: 28 bytes per 4 MiB chunk (negligible).
//
//  Zero-Knowledge guarantee:
//    The encryption key never leaves the local machine.
//    The S3 bucket only ever sees ciphertext blobs.
//    Even if the bucket is compromised, files are unreadable
//    without the ilocker project key.
// ============================================================

use anyhow::{bail, Context, Result};
use chacha20poly1305::{
    aead::{Aead, KeyInit},
    ChaCha20Poly1305, Key, Nonce,
};
use sha2::{Sha256, Digest};

// ── Key derivation ────────────────────────────────────────────

/// Derive a 32-byte ChaCha20-Poly1305 key from the project key string.
/// `project_key` is the full "iloc://<uuid>" string.
pub fn derive_cloud_key(project_key: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"ilocker-cloud:");
    hasher.update(project_key.as_bytes());
    let hash = hasher.finalize();
    let mut key = [0u8; 32];
    key.copy_from_slice(&hash);
    key
}

/// Derive a deterministic 12-byte nonce from a chunk's SHA-256.
fn derive_nonce(chunk_sha256: &str) -> [u8; 12] {
    let mut hasher = Sha256::new();
    hasher.update(b"nonce:");
    hasher.update(chunk_sha256.as_bytes());
    let hash = hasher.finalize();
    let mut nonce = [0u8; 12];
    nonce.copy_from_slice(&hash[..12]);
    nonce
}

// ── Encryption context ────────────────────────────────────────

pub struct CloudCrypto {
    key: [u8; 32],
}

impl CloudCrypto {
    /// Build from a project key string (iloc://<id>).
    pub fn from_project_key(project_key: &str) -> Self {
        Self { key: derive_cloud_key(project_key) }
    }

    /// Encrypt `plaintext` and return `[nonce(12) || ciphertext+tag]`.
    ///
    /// `chunk_sha256` is the SHA-256 of the plaintext — used to
    /// derive a unique, deterministic nonce.
    pub fn encrypt(&self, plaintext: &[u8], chunk_sha256: &str) -> Result<Vec<u8>> {
        let cipher      = ChaCha20Poly1305::new(Key::from_slice(&self.key));
        let nonce_bytes = derive_nonce(chunk_sha256);
        let nonce       = Nonce::from_slice(&nonce_bytes);

        let ciphertext = cipher
            .encrypt(nonce, plaintext)
            .map_err(|e| anyhow::anyhow!("ChaCha20-Poly1305 encrypt error: {:?}", e))?;

        let mut out = Vec::with_capacity(12 + ciphertext.len());
        out.extend_from_slice(&nonce_bytes);
        out.extend_from_slice(&ciphertext);
        Ok(out)
    }

    /// Renvoie la clé dérivée en tant que chaîne utilisable pour créer
    /// un second CloudCrypto identique (pour les spawns Tokio qui ne
    /// peuvent pas borrower self directement).
    pub fn project_key_for_cloning(&self) -> String {
        // On ré-encode la clé dérivée en hex (pas le project_key original —
        // mais c'est suffisant pour recréer un CloudCrypto identique via
        // from_derived_key).
        hex::encode(self.key)
    }

    /// Construit un CloudCrypto à partir d'une clé dérivée (hex) — utilisé
    /// par les spawns Tokio pour éviter les problèmes de lifetime.
    pub fn from_derived_key_hex(key_hex: &str) -> Result<Self> {
        let bytes = hex::decode(key_hex)
            .context("Clé dérivée hex invalide")?;
        let key: [u8; 32] = bytes.try_into()
            .map_err(|_| anyhow::anyhow!("Clé dérivée : longueur incorrecte (attendu 32 octets)"))?;
        Ok(Self { key })
    }
    pub fn decrypt(&self, wire: &[u8]) -> Result<Vec<u8>> {
        if wire.len() < 28 {
            bail!(
                "Encrypted blob too short ({} bytes) — expected at least 28",
                wire.len()
            );
        }
        let (nonce_bytes, ciphertext) = wire.split_at(12);
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&self.key));
        let nonce  = Nonce::from_slice(nonce_bytes);

        cipher
            .decrypt(nonce, ciphertext)
            .map_err(|_| anyhow::anyhow!(
                "Decryption failed — wrong project key or corrupted data"
            ))
    }
}

// ── Tests ─────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let ctx   = CloudCrypto::from_project_key("iloc://test-key-abc123");
        let plain = b"Hello, ilocker Phase 3 cloud encryption!";
        let wire  = ctx.encrypt(plain, "deadbeef").unwrap();
        let back  = ctx.decrypt(&wire).unwrap();
        assert_eq!(plain.as_ref(), back.as_slice());
    }

    #[test]
    fn wrong_key_fails() {
        let ctx1 = CloudCrypto::from_project_key("iloc://key-one");
        let ctx2 = CloudCrypto::from_project_key("iloc://key-two");
        let wire = ctx1.encrypt(b"secret", "aabbcc").unwrap();
        assert!(ctx2.decrypt(&wire).is_err());
    }

    #[test]
    fn different_chunks_different_nonces() {
        let ctx  = CloudCrypto::from_project_key("iloc://same-key");
        let d1   = ctx.encrypt(b"chunk-one", "sha256-of-chunk-one").unwrap();
        let d2   = ctx.encrypt(b"chunk-two", "sha256-of-chunk-two").unwrap();
        // Nonces (first 12 bytes) must differ
        assert_ne!(&d1[..12], &d2[..12]);
    }
}
