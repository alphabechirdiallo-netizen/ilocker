// ============================================================
//  cloud_share_token.rs — Cloud Share Magic Link  (v1.7.0)
//
//  SECURITY MODEL (Signal/Magic Wormhole pattern)
//  ─────────────────────────────────────────────
//  Two separate values must be transmitted over two distinct
//  channels to reconstruct access to the snapshot:
//
//    Channel 1 (semi-public, e.g. email):
//      iloc://cloud-share-<base64url(nonce || ciphertext)>
//
//    Channel 2 (secure, e.g. Signal):
//      iloc://…  (the full project key, already known to the receiver
//                 or shared separately as a short secret)
//
//  The share link is encrypted with:
//    share_key = SHA-256("iloc-share-v2:" ‖ project_key)
//
//  An attacker who intercepts only the link cannot decrypt it
//  without also knowing the project_key.
//  An attacker who knows the project_key but not the link
//  cannot download anything (the pre-signed URLs are inside
//  the encrypted payload).
//
//  Wire format:
//    ciphertext = ChaCha20-Poly1305(
//      key   = SHA-256("iloc-share-v2:" ‖ project_key),
//      nonce = 12 random bytes,
//      plain = JSON(CloudSharePayload)
//    )
//    token = "iloc://cloud-share-" ‖ base64url(nonce ‖ ciphertext)
//
//  Decryption requires BOTH the token AND the project_key.
// ============================================================

use anyhow::{bail, Context, Result};
use chacha20poly1305::{
    aead::{Aead, KeyInit},
    aead::rand_core::RngCore,
    ChaCha20Poly1305, Key, Nonce,
};
use serde::{Deserialize, Serialize};
use sha2::{Sha256, Digest};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

const TOKEN_PREFIX:       &str = "iloc://cloud-share-";
const PROTOCOL_VERSION:    u8  = 2;         // bumped from v1 (bootstrap key)
const KEY_DOMAIN_SEP:     &str = "iloc-share-v2:";

// ── Payload ───────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudSharePayload {
    pub v:            u8,
    /// Full iloc:// project key — also used to derive the decryption key
    pub project_key:  String,
    pub snapshot_id:  String,
    pub manifest_url: String,
    /// sha256 → pre-signed download URL
    pub chunks:       HashMap<String, String>,
    pub expires_at:   u64,
    pub provider:     String,
    pub file_count:   usize,
    pub total_bytes:  u64,
}

impl CloudSharePayload {
    pub fn is_expired(&self) -> bool {
        unix_now() > self.expires_at
    }

    pub fn time_remaining_secs(&self) -> i64 {
        self.expires_at as i64 - unix_now() as i64
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ── Key derivation ────────────────────────────────────────────

/// Derive a 32-byte ChaCha20 key from the project key.
/// Domain-separated to prevent cross-context attacks.
fn derive_key(project_key: &str) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(KEY_DOMAIN_SEP.as_bytes());
    h.update(project_key.as_bytes());
    let hash = h.finalize();
    let mut k = [0u8; 32];
    k.copy_from_slice(&hash);
    k
}

// ── Encode ────────────────────────────────────────────────────

/// Encrypt the payload with the project_key and encode as a token.
/// The resulting token is USELESS without the project_key.
pub fn encode(payload: &CloudSharePayload) -> Result<String> {
    let plain = serde_json::to_vec(payload)
        .context("Failed to serialise payload")?;

    let key_bytes = derive_key(&payload.project_key);
    let cipher    = ChaCha20Poly1305::new(Key::from_slice(&key_bytes));

    let mut nonce_bytes = [0u8; 12];
    // Use OsRng for cryptographically secure random nonce
    chacha20poly1305::aead::OsRng.fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext = cipher
        .encrypt(nonce, plain.as_ref())
        .map_err(|e| anyhow::anyhow!("Encryption failed: {:?}", e))?;

    let mut wire = Vec::with_capacity(12 + ciphertext.len());
    wire.extend_from_slice(&nonce_bytes);
    wire.extend_from_slice(&ciphertext);

    Ok(format!("{}{}", TOKEN_PREFIX, base64url_encode(&wire)))
}

// ── Decode ────────────────────────────────────────────────────

/// Decrypt a cloud share token using the project_key.
/// Both the token AND the project_key are required.
/// Returns an error if either is wrong or the token is expired.
pub fn decode(token: &str, project_key: &str) -> Result<CloudSharePayload> {
    let encoded = token.strip_prefix(TOKEN_PREFIX)
        .ok_or_else(|| anyhow::anyhow!("Not a cloud share token"))?;

    let wire = base64url_decode(encoded)
        .context("Token is malformed — cannot decode")?;

    if wire.len() < 28 {
        bail!("Token is too short — may be truncated");
    }

    let (nonce_bytes, ciphertext) = wire.split_at(12);

    let key_bytes = derive_key(project_key);
    let cipher    = ChaCha20Poly1305::new(Key::from_slice(&key_bytes));
    let nonce     = Nonce::from_slice(nonce_bytes);

    let plaintext = cipher
        .decrypt(nonce, ciphertext)
        .map_err(|_| anyhow::anyhow!(
            "Decryption failed — wrong project key or tampered token.\n\
             Make sure you are using the correct key (iloc://…) that the \
             sharer sent via a secure channel."
        ))?;

    let payload: CloudSharePayload = serde_json::from_slice(&plaintext)
        .context("Token payload is malformed")?;

    if payload.v != PROTOCOL_VERSION {
        bail!(
            "Token version {} is not supported (expected {}). \
             The sharer may be using a different version of ilocker.",
            payload.v, PROTOCOL_VERSION
        );
    }

    // Verify that the provided project_key matches the embedded one
    if payload.project_key != project_key {
        bail!(
            "Project key mismatch — the key you provided does not match \
             the key embedded in this share token."
        );
    }

    if payload.is_expired() {
        let ago = unix_now() - payload.expires_at;
        bail!(
            "This cloud share link expired {} ago.\n\
             Ask the sharer to run `iloc share --cloud` to generate a new link.",
            format_duration(ago)
        );
    }

    Ok(payload)
}

// ── Detection helpers ─────────────────────────────────────────

pub fn is_cloud_share_token(s: &str) -> bool {
    s.starts_with(TOKEN_PREFIX)
}

#[allow(dead_code)]
pub fn is_p2p_key(s: &str) -> bool {
    s.starts_with("iloc://") && !is_cloud_share_token(s)
}

// ── Display ───────────────────────────────────────────────────

pub fn describe(payload: &CloudSharePayload) {
    use colored::Colorize;
    let remaining = payload.time_remaining_secs();
    let time_str  = format_duration(remaining.unsigned_abs() as u64);
    println!();
    println!("{}", "  Cloud Share Link".bold());
    println!("  {} {}", "snapshot:".dimmed(), payload.snapshot_id.cyan());
    println!(
        "  {} {} files · {} chunks",
        "content:".dimmed(), payload.file_count, payload.chunks.len()
    );
    println!("  {} {}", "size:".dimmed(), crate::utils::human_bytes(payload.total_bytes));
    println!("  {} {} remaining", "expires:".dimmed(), time_str.yellow().bold());
    println!("  {} {}", "provider:".dimmed(), payload.provider.bold());
    println!();
}

// ── Base64-URL (no padding) ───────────────────────────────────

fn base64url_encode(data: &[u8]) -> String {
    b64_encode(data)
        .replace('+', "-")
        .replace('/', "_")
        .trim_end_matches('=')
        .to_string()
}

fn base64url_decode(s: &str) -> Result<Vec<u8>> {
    let std_b64 = s.replace('-', "+").replace('_', "/");
    let padded  = match std_b64.len() % 4 {
        2 => format!("{}==", std_b64),
        3 => format!("{}=",  std_b64),
        _ => std_b64,
    };
    b64_decode(&padded)
}

fn b64_encode(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let n  = (b0 << 16) | (b1 << 8) | b2;
        out.push(CHARS[((n >> 18) & 0x3F) as usize] as char);
        out.push(CHARS[((n >> 12) & 0x3F) as usize] as char);
        out.push(if chunk.len() > 1 { CHARS[((n >> 6) & 0x3F) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { CHARS[(n & 0x3F) as usize] as char } else { '=' });
    }
    out
}

fn b64_decode(s: &str) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let bytes: Vec<u8> = s.bytes().collect();
    for chunk in bytes.chunks(4) {
        let v: Vec<u8> = chunk.iter().map(|&b| match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' => 62, b'/' => 63, b'=' => 0, _ => 0,
        }).collect();
        if chunk.len() < 2 { break; }
        let n = ((v[0] as u32) << 18)
              | ((v[1] as u32) << 12)
              | (if chunk.len() > 2 { (v[2] as u32) << 6 } else { 0 })
              | (if chunk.len() > 3 {  v[3] as u32        } else { 0 });
        out.push((n >> 16) as u8);
        if chunk.len() > 2 && chunk[2] != b'=' { out.push((n >> 8) as u8); }
        if chunk.len() > 3 && chunk[3] != b'=' { out.push(n as u8); }
    }
    Ok(out)
}

fn format_duration(secs: u64) -> String {
    if secs >= 3600 { format!("{}h {}m", secs / 3600, (secs % 3600) / 60) }
    else             { format!("{}m {}s", secs / 60, secs % 60) }
}

// ── Tests ─────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_payload() -> CloudSharePayload {
        let mut chunks = HashMap::new();
        chunks.insert("a".repeat(64), "https://s3.example.com/chunk-a".to_string());
        chunks.insert("b".repeat(64), "https://s3.example.com/chunk-b".to_string());
        CloudSharePayload {
            v: PROTOCOL_VERSION,
            project_key: "iloc://test-project-key-abc123".to_string(),
            snapshot_id: "snap-001".to_string(),
            manifest_url: "https://s3.example.com/manifest".to_string(),
            chunks,
            expires_at: u64::MAX,
            provider: "s3".to_string(),
            file_count: 5,
            total_bytes: 1_000_000,
        }
    }

    #[test]
    fn round_trip_with_correct_key() {
        let p       = test_payload();
        let token   = encode(&p).unwrap();
        let decoded = decode(&token, &p.project_key).unwrap();
        assert!(token.starts_with("iloc://cloud-share-"));
        assert_eq!(decoded.project_key,  p.project_key);
        assert_eq!(decoded.snapshot_id,  p.snapshot_id);
        assert_eq!(decoded.chunks.len(), 2);
    }

    #[test]
    fn wrong_key_fails() {
        let p     = test_payload();
        let token = encode(&p).unwrap();
        assert!(decode(&token, "iloc://wrong-key").is_err());
    }

    #[test]
    fn tampered_token_fails() {
        let p     = test_payload();
        let token = encode(&p).unwrap();
        let mut chars: Vec<char> = token.chars().collect();
        let idx = chars.len() - 10;
        chars[idx] = if chars[idx] == 'A' { 'B' } else { 'A' };
        let tampered: String = chars.into_iter().collect();
        assert!(decode(&tampered, &p.project_key).is_err());
    }

    #[test]
    fn expired_token_rejected() {
        let mut p   = test_payload();
        p.expires_at = 1;            // expired in 1970
        let token   = encode(&p).unwrap();
        let err     = decode(&token, &p.project_key).unwrap_err();
        assert!(err.to_string().contains("expired"));
    }

    #[test]
    fn detection() {
        assert!( is_cloud_share_token("iloc://cloud-share-abc"));
        assert!(!is_cloud_share_token("iloc://abc123"));
    }
}
