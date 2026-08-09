// ============================================================
//  chunker.rs — Binary block engine (Phase 3)
//
//  Splits files into fixed-size chunks, hashes each one, and
//  builds a chunk manifest.  The manifest is the unit of
//  deduplication: when the receiver already holds a chunk with
//  a matching hash, that block is skipped over the wire.
//
//  Chunk layout (on disk inside .ilocker/chunks/)
//  ───────────────────────────────────────────────
//  .ilocker/chunks/<sha256[0..2]>/<sha256>   ← raw block bytes
//
//  This two-level directory mirrors Git's object store and keeps
//  filesystem directory entry counts bounded even with millions
//  of chunks.
//
//  Wire protocol (bincode-serialised structs)
//  ──────────────────────────────────────────
//  ChunkManifest  → list of ChunkInfo for one file
//  ChunkData      → (chunk_id, encrypted_bytes)
// ============================================================

use anyhow::Result;
use serde::{Deserialize, Serialize};
use sha2::{Sha256, Digest};
use std::fs::{self, File};
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};

/// Size of each block in bytes — 4 MiB is optimal for:
///   • Large-file streaming (keeps memory pressure low)
///   • Network transfer (each chunk is an independent retry unit)
///   • Deduplication granularity (partial-file reuse)
pub const CHUNK_SIZE: usize = 4 * 1024 * 1024;  // 4 MiB

// ── Public types ──────────────────────────────────────────────

/// Metadata for a single chunk of a file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkInfo {
    /// Sequential index within the file (0-based).
    pub index:    u32,
    /// SHA-256 of the raw (pre-encryption) chunk bytes.
    pub sha256:   String,
    /// Byte offset within the source file.
    pub offset:   u64,
    /// Actual byte length (≤ CHUNK_SIZE; last chunk may be smaller).
    pub len:      u32,
}

/// Full manifest for one file: relative path + ordered chunk list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileManifest {
    pub rel_path:     String,
    pub total_size:   u64,
    pub chunks:       Vec<ChunkInfo>,
    /// SHA-256 of the complete file (for integrity check after reassembly).
    pub file_sha256:  String,
}

/// Manifest for an entire snapshot — sent by the sharer to the cloner
/// so the cloner can decide which chunks it already has.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotManifest {
    pub snapshot_id:  String,
    pub project_key:  String,
    pub files:        Vec<FileManifest>,
    pub created_at:   String,
    /// Date d'expiration (timestamp Unix, secondes) — uniquement
    /// renseignée pour les manifests de partage sélectif scellés
    /// (`iloc share --cloud --file ...`). `None` pour les manifests
    /// de snapshot normaux (`iloc push`), qui n'expirent jamais.
    /// `#[serde(default)]` garantit la lecture des anciens manifests
    /// déjà uploadés avant ce champ, sans aucune casse.
    #[serde(default)]
    pub expires_at:   Option<u64>,
}

// ── Chunking ──────────────────────────────────────────────────

/// Split `src` into fixed-size chunks, store each in the chunk store,
/// and return the `FileManifest`.
///
/// Chunks are stored content-addressed:
///   `<chunk_dir>/<sha256[..2]>/<sha256>`
/// so identical blocks across snapshots share a single on-disk copy.
pub fn chunk_file(
    src:       &Path,
    rel_path:  &str,
    chunk_dir: &Path,
) -> Result<FileManifest> {
    let file      = File::open(src)?;
    let meta      = src.metadata()?;
    let total_size = meta.len();
    let mut reader = BufReader::with_capacity(CHUNK_SIZE, file);

    let mut chunks       = Vec::new();
    let mut file_hasher  = Sha256::new();
    let mut buf          = vec![0u8; CHUNK_SIZE];
    let mut offset       = 0u64;
    let mut index        = 0u32;

    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 { break; }

        let slice = &buf[..n];

        // Per-chunk hash
        let chunk_hash = hex::encode(Sha256::digest(slice));

        // Feed into whole-file hasher
        file_hasher.update(slice);

        // Store chunk content-addressed (deduplicated on disk)
        store_chunk(chunk_dir, &chunk_hash, slice)?;

        chunks.push(ChunkInfo {
            index,
            sha256: chunk_hash,
            offset,
            len: n as u32,
        });

        offset += n as u64;
        index  += 1;
    }

    let file_sha256 = hex::encode(file_hasher.finalize());

    Ok(FileManifest {
        rel_path: rel_path.to_string(),
        total_size,
        chunks,
        file_sha256,
    })
}

/// Write a chunk to the content-addressed store.
/// Skips writing if the chunk already exists (deduplication).
fn store_chunk(chunk_dir: &Path, sha256: &str, data: &[u8]) -> Result<()> {
    let prefix  = &sha256[..2];
    let dir     = chunk_dir.join(prefix);
    let outpath = dir.join(sha256);

    if outpath.exists() {
        return Ok(());  // Already stored — pure deduplication
    }

    fs::create_dir_all(&dir)?;
    let mut f = File::create(&outpath)?;
    f.write_all(data)?;
    Ok(())
}

/// Read a chunk from the content-addressed store.
pub fn load_chunk(chunk_dir: &Path, sha256: &str) -> Result<Vec<u8>> {
    let prefix  = &sha256[..2];
    let path    = chunk_dir.join(prefix).join(sha256);
    Ok(fs::read(&path)?)
}

// ── Reassembly ────────────────────────────────────────────────

/// Reconstruct a file from its manifest and the local chunk store.
/// Verifies the whole-file SHA-256 after reassembly.
pub fn reassemble_file(
    manifest:  &FileManifest,
    chunk_dir: &Path,
    dest:      &Path,
) -> Result<()> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }

    let mut out          = File::create(dest)?;
    let mut file_hasher  = Sha256::new();

    // Chunks must be in index order
    let mut ordered = manifest.chunks.clone();
    ordered.sort_by_key(|c| c.index);

    for chunk in &ordered {
        let data = load_chunk(chunk_dir, &chunk.sha256)?;
        file_hasher.update(&data);
        out.write_all(&data)?;
    }

    let actual_hash = hex::encode(file_hasher.finalize());
    if actual_hash != manifest.file_sha256 {
        anyhow::bail!(
            "Integrity check failed for '{}': expected {} got {}",
            manifest.rel_path, manifest.file_sha256, actual_hash
        );
    }

    Ok(())
}

// ── Deduplication diff ────────────────────────────────────────

/// Given a remote `FileManifest` and the local chunk store, return the
/// subset of `ChunkInfo`s that the local node does NOT yet have.
/// These are the only chunks that need to travel over the wire.
pub fn missing_chunks<'a>(
    manifest:  &'a FileManifest,
    chunk_dir: &Path,
) -> Vec<&'a ChunkInfo> {
    manifest.chunks
        .iter()
        .filter(|c| {
            let prefix = &c.sha256[..2];
            !chunk_dir.join(prefix).join(&c.sha256).exists()
        })
        .collect()
}

// ── Chunk-store helpers ───────────────────────────────────────

/// Path to the chunk store — résolu via le vault externalisé
/// (`crate::vault::chunks_dir`).  Pour un projet sans `vault.json`
/// (initialisé avant v1.10.0), cela retombe exactement sur
/// l'ancien chemin `.ilocker/chunks/` — comportement inchangé.
pub fn chunk_dir(ilocker_dir: &Path) -> PathBuf {
    crate::vault::chunks_dir(ilocker_dir)
}
