// ============================================================
//  protocol.rs — ilocker P2P wire protocol (Phase 3)
//
//  Transport: plain TCP (TLS upgrade is Phase 3+)
//  Framing:   4-byte little-endian length prefix + bincode payload
//
//  Message flow
//  ────────────
//  Cloner → Sharer   Hello { project_key }
//  Sharer → Cloner   Manifest { SnapshotManifest }
//  Cloner → Sharer   NeedChunks { Vec<chunk_sha256> }
//  Sharer → Cloner   ChunkData { sha256, rel_path, encrypted_bytes }
//                    … (one message per needed chunk) …
//  Sharer → Cloner   Done
//  Cloner → Sharer   Ack
//
//  All ChunkData payloads are AES-256-GCM encrypted.
//  The 4-byte length prefix allows streaming reassembly without
//  buffering an entire message before processing.
// ============================================================

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::chunker::SnapshotManifest;

// ── Wire messages ─────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub enum Message {
    /// Cloner identifies itself and proves it knows the project key.
    Hello {
        /// Full `iloc://<id>` key — used server-side to locate the project.
        project_key: String,
        /// Protocol version for forward-compatibility.
        version: u8,
    },

    /// Sharer sends the complete snapshot manifest.
    Manifest(SnapshotManifest),

    /// Cloner lists chunk hashes it does NOT yet have.
    NeedChunks(Vec<String>),

    /// Sharer sends one encrypted chunk at a time.
    ChunkData {
        /// Content hash of the plaintext chunk (used as chunk ID).
        sha256:           String,
        /// Relative file path — needed by the receiver to derive the nonce.
        rel_path:         String,
        /// AES-256-GCM encrypted bytes (nonce prepended).
        encrypted_bytes:  Vec<u8>,
    },

    /// Sharer signals end of transfer.
    Done,

    /// Cloner acknowledges successful receipt.
    Ack,

    /// Either side signals an error.
    Error(String),
}

// ── Framing helpers (async) ───────────────────────────────────

const MAX_MESSAGE_SIZE: u32 = 64 * 1024 * 1024; // 64 MiB hard limit

/// Send a `Message` over a TCP stream.
/// Wire format: `[ u32 LE length ][ bincode(message) ]`
pub async fn send_msg(stream: &mut TcpStream, msg: &Message) -> Result<()> {
    let payload = bincode::serialize(msg)?;
    let len     = payload.len() as u32;

    if len > MAX_MESSAGE_SIZE {
        bail!("Outgoing message too large ({} bytes)", len);
    }

    stream.write_all(&len.to_le_bytes()).await?;
    stream.write_all(&payload).await?;
    stream.flush().await?;
    Ok(())
}

/// Receive a `Message` from a TCP stream.
pub async fn recv_msg(stream: &mut TcpStream) -> Result<Message> {
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await?;
    let len = u32::from_le_bytes(len_buf);

    if len > MAX_MESSAGE_SIZE {
        bail!("Incoming message too large ({} bytes) — possible corruption", len);
    }

    let mut payload = vec![0u8; len as usize];
    stream.read_exact(&mut payload).await?;
    Ok(bincode::deserialize(&payload)?)
}

// ── Constants ─────────────────────────────────────────────────

pub const DEFAULT_PORT: u16 = 7477;
pub const PROTOCOL_VERSION: u8 = 1;
