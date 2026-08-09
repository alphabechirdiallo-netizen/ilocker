// ============================================================
//  crypto.rs — Zero-Knowledge AES-256-GCM layer (Phase 3)
//
//  The project key (iloc://<hex-uuid>) is the ONLY secret.
//  It never leaves the local machine.  The sharer and cloner
//  must exchange the key out-of-band (e.g. over Signal, email).
//
//  Key derivation
//  ──────────────
//  The 32-byte AES key is derived by taking SHA-256 of the raw
//  project ID hex string.  No KDF stretching is needed here
//  because the project ID is already a 128-bit random UUID —
//  collision resistance exceeds the AES-256 security level.
//
//  Per-chunk nonce
//  ───────────────
//  AES-256-GCM requires a unique 96-bit nonce per encryption.
//  We derive it deterministically as:
//    nonce = SHA-256(chunk_sha256 ‖ file_rel_path)[..12]
//  This is collision-resistant for any realistic project size
//  and avoids the need for nonce synchronisation over the wire.
//
//  Wire format for an encrypted chunk
//  ────────────────────────────────────
//  [ 12 bytes nonce ][ ciphertext + 16-byte GCM tag ]
//  Total overhead per chunk: 28 bytes.
// ============================================================

use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Key, Nonce,
};
use anyhow::{bail, Result};
use sha2::{Sha256, Digest};

// ── Key derivation ────────────────────────────────────────────

/// Derive a 32-byte AES-256 key from the project ID string.
///
/// `project_id` is the raw hex UUID without the `iloc://` prefix.
pub fn derive_key(project_id: &str) -> [u8; 32] {
    let hash = Sha256::digest(project_id.as_bytes());
    let mut key = [0u8; 32];
    key.copy_from_slice(&hash);
    key
}

/// Extract the project ID from an `iloc://<id>` key string.
pub fn key_to_project_id(key_str: &str) -> Result<String> {
    if let Some(id) = key_str.strip_prefix("iloc://") {
        Ok(id.to_string())
    } else {
        bail!("Invalid iloc key format: expected `iloc://<id>`, got `{}`", key_str);
    }
}

// ── Nonce derivation ─────────────────────────────────────────

/// Derive a deterministic 96-bit nonce for a specific chunk.
///
/// Inputs are (chunk_sha256, file_rel_path) — both are known to
/// sender and receiver without any extra negotiation.
fn derive_nonce(chunk_sha256: &str, rel_path: &str) -> [u8; 12] {
    let mut hasher = Sha256::new();
    hasher.update(chunk_sha256.as_bytes());
    hasher.update(b"|");
    hasher.update(rel_path.as_bytes());
    let hash = hasher.finalize();
    let mut nonce = [0u8; 12];
    nonce.copy_from_slice(&hash[..12]);
    nonce
}

// ── Encrypt / Decrypt ─────────────────────────────────────────

/// Encrypt `plaintext` and return `[nonce (12 B) || ciphertext+tag]`.
pub fn encrypt_chunk(
    plaintext:    &[u8],
    key_bytes:    &[u8; 32],
    chunk_sha256: &str,
    rel_path:     &str,
) -> Result<Vec<u8>> {
    let key    = Key::<Aes256Gcm>::from_slice(key_bytes);
    let cipher = Aes256Gcm::new(key);

    let nonce_bytes = derive_nonce(chunk_sha256, rel_path);
    let nonce       = Nonce::from_slice(&nonce_bytes);

    let ciphertext = cipher
        .encrypt(nonce, plaintext)
        .map_err(|e| anyhow::anyhow!("AES-GCM encrypt error: {:?}", e))?;

    // Prepend nonce so the receiver can decrypt without extra state
    let mut out = Vec::with_capacity(12 + ciphertext.len());
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// Decrypt a wire blob produced by `encrypt_chunk`.
pub fn decrypt_chunk(
    wire:         &[u8],
    key_bytes:    &[u8; 32],
) -> Result<Vec<u8>> {
    if wire.len() < 12 {
        bail!("Wire blob too short to contain a nonce ({} bytes)", wire.len());
    }

    let (nonce_bytes, ciphertext) = wire.split_at(12);
    let key    = Key::<Aes256Gcm>::from_slice(key_bytes);
    let cipher = Aes256Gcm::new(key);
    let nonce  = Nonce::from_slice(nonce_bytes);

    cipher
        .decrypt(nonce, ciphertext)
        .map_err(|_| anyhow::anyhow!(
            "AES-GCM decryption failed — wrong key or corrupted data"
        ))
}

// ── Convenience: full chunk round-trip ───────────────────────

pub struct CryptoCtx {
    key: [u8; 32],
}

impl CryptoCtx {
    /// Build a context from an `iloc://<id>` key string.
    pub fn from_key_str(key_str: &str) -> Result<Self> {
        let project_id = key_to_project_id(key_str)?;
        Ok(Self { key: derive_key(&project_id) })
    }

    pub fn encrypt(&self, plain: &[u8], chunk_sha256: &str, rel_path: &str) -> Result<Vec<u8>> {
        encrypt_chunk(plain, &self.key, chunk_sha256, rel_path)
    }

    pub fn decrypt(&self, wire: &[u8]) -> Result<Vec<u8>> {
        decrypt_chunk(wire, &self.key)
    }
}

// ── Tests ─────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let ctx  = CryptoCtx::from_key_str("iloc://abc123def456").unwrap();
        let data = b"Hello, ilocker Phase 3!";
        let wire = ctx.encrypt(data, "deadbeef", "src/main.rs").unwrap();
        let back = ctx.decrypt(&wire).unwrap();
        assert_eq!(data.as_ref(), back.as_slice());
    }

    #[test]
    fn wrong_key_fails() {
        let ctx1 = CryptoCtx::from_key_str("iloc://key_one").unwrap();
        let ctx2 = CryptoCtx::from_key_str("iloc://key_two").unwrap();
        let wire = ctx1.encrypt(b"secret", "aabbcc", "file.txt").unwrap();
        assert!(ctx2.decrypt(&wire).is_err());
    }
}
