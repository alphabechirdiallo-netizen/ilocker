// ============================================================
//  db.rs — SQLite index layer
//
//  Tables
//  ──────
//  snapshots  : one row per `iloc save` call
//  file_index : one row per (snapshot_id, relative_path) pair,
//               storing the content hash and metadata
// ============================================================

use anyhow::Result;
use rusqlite::{Connection, params};
use serde::Serialize;
use std::path::Path;

// ── Schema ────────────────────────────────────────────────────

const SCHEMA: &str = r#"
PRAGMA journal_mode = WAL;
PRAGMA synchronous  = NORMAL;

CREATE TABLE IF NOT EXISTS snapshots (
    id          TEXT PRIMARY KEY,
    message     TEXT NOT NULL,
    parent_id   TEXT,
    created_at  TEXT NOT NULL,
    file_count  INTEGER NOT NULL DEFAULT 0,
    total_bytes INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS file_index (
    snapshot_id  TEXT    NOT NULL REFERENCES snapshots(id) ON DELETE CASCADE,
    rel_path     TEXT    NOT NULL,
    sha256       TEXT    NOT NULL,
    size_bytes   INTEGER NOT NULL,
    modified_at  TEXT    NOT NULL,
    inode        INTEGER,              -- used for hard-link tracking
    PRIMARY KEY (snapshot_id, rel_path)
);

CREATE INDEX IF NOT EXISTS idx_file_sha256 ON file_index(sha256);
"#;

// ── Public helpers ────────────────────────────────────────────

/// Initialise the database and apply the schema.
pub fn init(path: &Path) -> Result<()> {
    let conn = open(path)?;
    conn.execute_batch(SCHEMA)?;
    Ok(())
}

/// Open (or create) the database at the given path.
pub fn open(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)?;
    // Enable foreign keys
    conn.execute_batch("PRAGMA foreign_keys = ON;")?;
    Ok(conn)
}

// ── Snapshot CRUD ─────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct Snapshot {
    pub id:          String,
    pub message:     String,
    pub parent_id:   Option<String>,
    pub created_at:  String,
    pub file_count:  i64,
    pub total_bytes: i64,
}

/// Insert a new snapshot record.  Returns the inserted snapshot.
pub fn insert_snapshot(
    conn:       &Connection,
    id:         &str,
    message:    &str,
    parent_id:  Option<&str>,
    created_at: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO snapshots (id, message, parent_id, created_at, file_count, total_bytes)
         VALUES (?1, ?2, ?3, ?4, 0, 0)",
        params![id, message, parent_id, created_at],
    )?;
    Ok(())
}

/// Update the aggregate stats of a snapshot after indexing is complete.
pub fn update_snapshot_stats(
    conn:        &Connection,
    id:          &str,
    file_count:  i64,
    total_bytes: i64,
) -> Result<()> {
    conn.execute(
        "UPDATE snapshots SET file_count = ?1, total_bytes = ?2 WHERE id = ?3",
        params![file_count, total_bytes, id],
    )?;
    Ok(())
}

/// Return all snapshots ordered from newest to oldest.
pub fn list_snapshots(conn: &Connection) -> Result<Vec<Snapshot>> {
    let mut stmt = conn.prepare(
        "SELECT id, message, parent_id, created_at, file_count, total_bytes
         FROM snapshots ORDER BY created_at DESC",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(Snapshot {
            id:          row.get(0)?,
            message:     row.get(1)?,
            parent_id:   row.get(2)?,
            created_at:  row.get(3)?,
            file_count:  row.get(4)?,
            total_bytes: row.get(5)?,
        })
    })?;
    let mut result = Vec::new();
    for row in rows {
        result.push(row?);
    }
    Ok(result)
}

/// Return the most-recent snapshot, if any.
pub fn latest_snapshot(conn: &Connection) -> Result<Option<Snapshot>> {
    let mut stmt = conn.prepare(
        "SELECT id, message, parent_id, created_at, file_count, total_bytes
         FROM snapshots ORDER BY created_at DESC LIMIT 1",
    )?;
    let mut rows = stmt.query_map([], |row| {
        Ok(Snapshot {
            id:          row.get(0)?,
            message:     row.get(1)?,
            parent_id:   row.get(2)?,
            created_at:  row.get(3)?,
            file_count:  row.get(4)?,
            total_bytes: row.get(5)?,
        })
    })?;
    Ok(rows.next().transpose()?)
}

/// Return a specific snapshot by ID (prefix search).
pub fn get_snapshot(conn: &Connection, id: &str) -> Result<Option<Snapshot>> {
    let pattern = format!("{}%", id);
    let mut stmt = conn.prepare(
        "SELECT id, message, parent_id, created_at, file_count, total_bytes
         FROM snapshots WHERE id LIKE ?1 LIMIT 1",
    )?;
    let mut rows = stmt.query_map(params![pattern], |row| {
        Ok(Snapshot {
            id:          row.get(0)?,
            message:     row.get(1)?,
            parent_id:   row.get(2)?,
            created_at:  row.get(3)?,
            file_count:  row.get(4)?,
            total_bytes: row.get(5)?,
        })
    })?;
    Ok(rows.next().transpose()?)
}

/// Résout une référence de snapshot saisie par l'utilisateur :
///   - un nombre entier ≥ 1  → position dans l'historique (1 = le plus
///     récent, 2 = le précédent, etc.) — c'est la référence "visible"
///     par défaut puisque les ID hexadécimaux sont désormais masqués
///     dans `iloc log` sauf demande explicite (`--ids`).
///   - sinon                → préfixe hexadécimal de l'ID (comportement
///     historique, toujours disponible pour les scripts/power users).
pub fn resolve_snapshot_ref(conn: &Connection, raw: &str) -> Result<Option<Snapshot>> {
    if let Ok(n) = raw.trim().parse::<usize>() {
        if n >= 1 {
            let snaps = list_snapshots(conn)?;
            return Ok(snaps.into_iter().nth(n - 1));
        }
    }
    get_snapshot(conn, raw)
}

// ── File-index CRUD ───────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct FileRecord {
    pub snapshot_id:  String,
    pub rel_path:     String,
    pub sha256:       String,
    pub size_bytes:   i64,
    pub modified_at:  String,
    pub inode:        Option<i64>,
}

/// Bulk-insert file records for a snapshot (efficient batch).
pub fn insert_file_records(conn: &mut Connection, records: &[FileRecord]) -> Result<()> {
    let tx = conn.transaction()?;
    {
        let mut stmt = tx.prepare(
            "INSERT OR REPLACE INTO file_index
             (snapshot_id, rel_path, sha256, size_bytes, modified_at, inode)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        for r in records {
            stmt.execute(params![
                r.snapshot_id,
                r.rel_path,
                r.sha256,
                r.size_bytes,
                r.modified_at,
                r.inode,
            ])?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// Return all file records for a given snapshot.
pub fn files_for_snapshot(conn: &Connection, snapshot_id: &str) -> Result<Vec<FileRecord>> {
    let mut stmt = conn.prepare(
        "SELECT snapshot_id, rel_path, sha256, size_bytes, modified_at, inode
         FROM file_index WHERE snapshot_id = ?1",
    )?;
    let rows = stmt.query_map(params![snapshot_id], |row| {
        Ok(FileRecord {
            snapshot_id: row.get(0)?,
            rel_path:    row.get(1)?,
            sha256:      row.get(2)?,
            size_bytes:  row.get(3)?,
            modified_at: row.get(4)?,
            inode:       row.get(5)?,
        })
    })?;
    let mut result = Vec::new();
    for row in rows {
        result.push(row?);
    }
    Ok(result)
}

/// Filtre une liste de `FileRecord` selon une liste de chemins demandés
/// explicitement par l'utilisateur (`--file <chemin>`, répétable).
///
/// Comparaison tolérante : accepte les séparateurs `\` (Windows), les
/// préfixes `./`, et les chemins absolus dans le projet courant (on ne
/// garde alors que la partie relative).
///
/// Retourne `(fichiers_trouvés, chemins_demandés_introuvables)` — la
/// liste des introuvables permet d'avertir l'utilisateur sans faire
/// échouer toute l'opération pour une simple faute de frappe.
pub fn select_records(
    records: &[FileRecord],
    wanted:  &[String],
) -> (Vec<FileRecord>, Vec<String>) {
    if wanted.is_empty() {
        return (records.to_vec(), Vec::new());
    }

    fn normalize(p: &str) -> String {
        p.replace('\\', "/")
            .trim_start_matches("./")
            .trim_start_matches('/')
            .to_string()
    }

    let normalized_wanted: Vec<String> = wanted.iter().map(|w| normalize(w)).collect();

    let mut selected = Vec::new();
    let mut found_flags = vec![false; normalized_wanted.len()];

    for rec in records {
        let rel_norm = normalize(&rec.rel_path);
        for (i, w) in normalized_wanted.iter().enumerate() {
            if rel_norm == *w {
                selected.push(rec.clone());
                found_flags[i] = true;
            }
        }
    }

    let not_found: Vec<String> = normalized_wanted.iter()
        .zip(found_flags.iter())
        .filter(|(_, found)| !**found)
        .map(|(w, _)| w.clone())
        .collect();

    (selected, not_found)
}
