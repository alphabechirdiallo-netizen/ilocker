// ============================================================
//  transfer_state.rs — Resumable transfer manifest  (Phase 4)
//
//  File location: .ilocker/transfer_manifest.json
//  Written atomically after every chunk (rename-based).
//
//  The manifest records which chunks have been sent/received
//  so that a resumed transfer skips completed chunks entirely.
//
//  Lifecycle:
//    • iloc share  — creates/updates the manifest on the sharer side
//    • iloc clone  — creates/updates the manifest on the cloner side
//    • Transfer completes → manifest is deleted automatically
//    • Transfer interrupted → manifest persists for next run
//
//  Format:
//  {
//    "session_id":       "abc123...",
//    "direction":        "send" | "receive",
//    "project_key":      "iloc://...",
//    "snapshot_id":      "...",
//    "total_chunks":     1024,
//    "completed_chunks": ["sha1", "sha2", ...],
//    "started_at":       "2026-05-28T...",
//    "updated_at":       "2026-05-28T...",
//    "peer_addr":        "1.2.3.4:7477"
//  }
// ============================================================

use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    Send,
    Receive,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferManifest {
    pub session_id:        String,
    pub direction:         Direction,
    pub project_key:       String,
    pub snapshot_id:       String,
    pub total_chunks:      usize,
    /// Set of chunk SHA-256 hashes that have been fully transferred.
    pub completed_chunks:  HashSet<String>,
    pub started_at:        String,
    pub updated_at:        String,
    /// Remote peer address (for display / reconnect hint)
    pub peer_addr:         String,
}

impl TransferManifest {
    /// Create a new manifest for a fresh transfer.
    pub fn new(
        session_id:   &str,
        direction:    Direction,
        project_key:  &str,
        snapshot_id:  &str,
        total_chunks: usize,
        peer_addr:    &str,
    ) -> Self {
        let now = Utc::now().to_rfc3339();
        Self {
            session_id:       session_id.to_string(),
            direction,
            project_key:      project_key.to_string(),
            snapshot_id:      snapshot_id.to_string(),
            total_chunks,
            completed_chunks: HashSet::new(),
            started_at:       now.clone(),
            updated_at:       now,
            peer_addr:        peer_addr.to_string(),
        }
    }

    /// Mark a chunk as completed and persist immediately.
    pub fn mark_done(&mut self, sha256: &str, path: &Path) -> Result<()> {
        self.completed_chunks.insert(sha256.to_string());
        self.updated_at = Utc::now().to_rfc3339();
        save(self, path)
    }

    /// Number of chunks still pending.
    #[allow(dead_code)]
    pub fn remaining(&self) -> usize {
        self.total_chunks.saturating_sub(self.completed_chunks.len())
    }

    /// Percentage complete (0.0 – 100.0).
    pub fn progress_pct(&self) -> f64 {
        if self.total_chunks == 0 { return 100.0; }
        100.0 * self.completed_chunks.len() as f64 / self.total_chunks as f64
    }

    /// True if all chunks have been transferred.
    pub fn is_complete(&self) -> bool {
        self.completed_chunks.len() >= self.total_chunks
    }
}

// ── Persistence ───────────────────────────────────────────────

/// Path to the transfer manifest inside .ilocker/
pub fn manifest_path(ilocker_dir: &Path) -> PathBuf {
    ilocker_dir.join("transfer_manifest.json")
}

/// Load an existing manifest, if any.
pub fn load(ilocker_dir: &Path) -> Result<Option<TransferManifest>> {
    let path = manifest_path(ilocker_dir);
    if !path.exists() { return Ok(None); }

    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("Cannot read {}", path.display()))?;

    let manifest: TransferManifest = serde_json::from_str(&raw)
        .with_context(|| "Transfer manifest is corrupt — starting fresh")?;

    Ok(Some(manifest))
}

/// Atomically persist the manifest.
pub fn save(manifest: &TransferManifest, ilocker_dir: &Path) -> Result<()> {
    let path = manifest_path(ilocker_dir);
    let tmp  = path.with_extension("json.tmp");

    let raw = serde_json::to_string_pretty(manifest)?;
    std::fs::write(&tmp, raw)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Delete the manifest after a successful complete transfer.
pub fn remove(ilocker_dir: &Path) -> Result<()> {
    let path = manifest_path(ilocker_dir);
    if path.exists() {
        std::fs::remove_file(&path)
            .with_context(|| format!("Cannot remove {}", path.display()))?;
    }
    Ok(())
}

/// Load an existing manifest if it matches the current transfer,
/// or create a fresh one.
pub fn load_or_create(
    ilocker_dir:  &Path,
    direction:    Direction,
    project_key:  &str,
    snapshot_id:  &str,
    total_chunks: usize,
    peer_addr:    &str,
) -> Result<(TransferManifest, bool)> {
    if let Some(existing) = load(ilocker_dir)? {
        // Resume if it's the same snapshot and direction
        if existing.snapshot_id == snapshot_id && existing.direction == direction {
            let _pct  = existing.progress_pct();
            let _done = existing.completed_chunks.len();
            return Ok((existing, true)); // (manifest, is_resume)
        }
        // Different snapshot — discard and start fresh
        remove(ilocker_dir)?;
    }

    let session_id = crate::utils::new_snapshot_id();
    let manifest = TransferManifest::new(
        &session_id, direction, project_key, snapshot_id, total_chunks, peer_addr,
    );
    save(&manifest, ilocker_dir)?;
    Ok((manifest, false))
}
