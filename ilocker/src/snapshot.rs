// ============================================================
//  snapshot.rs — project tree walker & diff engine
//  Phase 2: adaptive file-tier classification, large-file
//  awareness, parallel hashing with predictable RAM usage.
// ============================================================

use crate::db::FileRecord;
use crate::utils::{sha256_file, inode_of, FileTier, IgnoreRules};
use anyhow::Result;
use chrono::Utc;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
// Sémaphore léger : channel de capacité N = nombre max de threads actifs
use std::sync::mpsc;
use walkdir::WalkDir;

// ── Public types ──────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct WorkingFile {
    pub rel_path:    PathBuf,
    pub abs_path:    PathBuf,
    pub sha256:      String,
    pub size_bytes:  u64,
    pub modified_at: String,
    pub inode:       Option<u64>,
    pub tier:        FileTier,   // NEW: size classification
}

#[derive(Debug)]
pub struct DiffResult {
    pub added:     Vec<WorkingFile>,
    pub modified:  Vec<WorkingFile>,
    pub unchanged: Vec<WorkingFile>,
    pub deleted:   Vec<String>,
}

/// Summary stats from a scan — lets callers warn about large files.
#[derive(Debug, Default)]
pub struct ScanStats {
    pub total_files:  usize,
    pub large_files:  usize,   // ≥ 1 GiB
    pub medium_files: usize,   // 64 MiB – 1 GiB
    pub total_bytes:  u64,
}

// ── Scanner ───────────────────────────────────────────────────

/// Walk `project_root`, classify and hash every non-ignored file.
///
/// Threading model:
///   • Small/medium files   → processed in parallel chunks
///   • Large files (≥ 1 GiB)→ hashed one at a time on a dedicated
///     thread to avoid multiple 16 MiB buffers competing for RAM
pub fn scan_project(
    project_root: &Path,
    ignore:       &IgnoreRules,
) -> Result<(Vec<WorkingFile>, ScanStats)> {

    // ── 1. Walk & classify ────────────────────────────────────
    let mut small_medium: Vec<(PathBuf, u64)> = Vec::new();
    let mut large_files:  Vec<(PathBuf, u64)> = Vec::new();
    let mut stats = ScanStats::default();

    for entry in WalkDir::new(project_root)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
    {
        let abs = entry.into_path();
        let rel = match abs.strip_prefix(project_root) {
            Ok(r)  => r.to_path_buf(),
            Err(_) => continue,
        };
        if ignore.is_ignored(&rel) { continue; }

        let size = std::fs::metadata(&abs).map(|m| m.len()).unwrap_or(0);
        stats.total_files  += 1;
        stats.total_bytes  += size;

        match FileTier::from_size(size) {
            FileTier::Large  => { stats.large_files  += 1; large_files.push((abs, size)); }
            FileTier::Medium => { stats.medium_files += 1; small_medium.push((abs, size)); }
            FileTier::Small  =>                           { small_medium.push((abs, size)); }
        }
    }

    // ── 2. Hash small/medium files en parallèle (RAM-safe) ────
    //
    // Problème de l'ancienne implémentation :
    //   N threads × buffer_size_par_thread peut exploser la RAM.
    //   Ex : 8 threads × 4 MiB (Medium) = 32 MiB simultanés MINIMUM,
    //   mais sur 100 000 fichiers Medium on crée autant de threads que
    //   de chunks, soit potentiellement des centaines de buffers en vol.
    //
    // Solution : sémaphore par canal (mpsc bounded) qui autorise
    //   MAX_PARALLEL threads actifs à la fois.
    //   MAX_PARALLEL = min(ncpus, 8) pour les fichiers Medium (4 MiB buf)
    //                = min(ncpus, 4) pour les fichiers Large  (16 MiB buf)
    //   → RAM peak garanti : 8 × 4 MiB = 32 MiB (Medium)
    //                         1 × 16 MiB = 16 MiB (Large, séquentiel)

    let ncpus         = num_cpus();
    // On sépare les Medium des Small pour plafonner le RAM différemment
    let (small_files, medium_files): (Vec<_>, Vec<_>) = small_medium
        .into_iter()
        .partition(|(abs, size)| FileTier::from_size(*size) == FileTier::Small);

    let results = Arc::new(Mutex::new(Vec::<Result<WorkingFile>>::new()));
    let root    = Arc::new(project_root.to_path_buf());

    // ── 2a. Petits fichiers : jusqu'à ncpus threads simultanés ──
    let max_small = ncpus.min(16);
    hash_parallel(&small_files, &root, &results, max_small);

    // ── 2b. Fichiers Medium : plafonné à 8 threads (4 MiB chacun) ──
    let max_medium = ncpus.min(8);
    hash_parallel(&medium_files, &root, &results, max_medium);

    // ── 3. Hash large files séquentiellement (RAM-safe) ───────
    // Un seul buffer de 16 MiB en vol à la fois.
    for (abs, size) in large_files {
        let wf = hash_one(&abs, &root, size)?;
        results.lock().unwrap().push(Ok(wf));
    }

    // ── 4. Collect results ────────────────────────────────────
    let locked = Arc::try_unwrap(results).unwrap().into_inner().unwrap();
    let mut files = Vec::with_capacity(locked.len());
    for r in locked { files.push(r?); }

    Ok((files, stats))
}

fn hash_one(abs: &Path, root: &Path, size: u64) -> Result<WorkingFile> {
    let rel  = abs.strip_prefix(root)?.to_path_buf();
    let meta = std::fs::metadata(abs)?;
    let modified_at = meta
        .modified()
        .map(|t| { let dt: chrono::DateTime<Utc> = t.into(); dt.to_rfc3339() })
        .unwrap_or_else(|_| Utc::now().to_rfc3339());
    let sha256 = sha256_file(abs)?;
    let inode  = inode_of(abs);
    let tier   = FileTier::from_size(size);
    Ok(WorkingFile {
        rel_path: rel,
        abs_path: abs.to_path_buf(),
        sha256,
        size_bytes: size,
        modified_at,
        inode,
        tier,
    })
}

fn num_cpus() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
}

/// Hash une liste de fichiers en parallèle avec un maximum de
/// `max_concurrent` threads actifs simultanément.
///
/// Implémentation : le canal (tx/rx) fait office de sémaphore —
/// chaque slot « token » doit être rendu avant qu'un nouveau thread
/// soit lancé.  Cela borne strictement le nombre de buffers RAM en
/// vol, quelle que soit la taille de la liste en entrée.
fn hash_parallel(
    files:          &[(PathBuf, u64)],
    root:           &Arc<PathBuf>,
    results:        &Arc<Mutex<Vec<Result<WorkingFile>>>>,
    max_concurrent: usize,
) {
    if files.is_empty() { return; }

    // Canal de jetons : capacité = max_concurrent
    let (token_tx, token_rx) = mpsc::sync_channel::<()>(max_concurrent);
    // Pré-remplissage du canal avec les jetons
    for _ in 0..max_concurrent {
        token_tx.send(()).ok();
    }
    let token_tx = Arc::new(token_tx);
    let token_rx = Arc::new(Mutex::new(token_rx));

    let mut handles = Vec::new();

    for (abs, size) in files {
        let abs     = abs.clone();
        let size    = *size;
        let root    = Arc::clone(root);
        let results = Arc::clone(results);
        let tx      = Arc::clone(&token_tx);
        let rx      = Arc::clone(&token_rx);

        // Attendre un jeton disponible (bloque si max_concurrent atteint)
        rx.lock().unwrap().recv().ok();

        let handle = std::thread::spawn(move || {
            let entry = hash_one(&abs, &root, size);
            results.lock().unwrap().push(entry);
            // Rendre le jeton pour permettre au prochain thread de démarrer
            tx.send(()).ok();
        });
        handles.push(handle);
    }

    for h in handles { h.join().ok(); }
}

// ── Diff ─────────────────────────────────────────────────────

pub fn diff(current: &[WorkingFile], previous: &[FileRecord]) -> DiffResult {
    let prev_map: HashMap<String, &FileRecord> = previous
        .iter()
        .map(|r| (r.rel_path.clone(), r))
        .collect();

    let mut added     = Vec::new();
    let mut modified  = Vec::new();
    let mut unchanged = Vec::new();

    for wf in current {
        let rel_str = wf.rel_path.to_string_lossy().replace('\\', "/");
        match prev_map.get(&rel_str) {
            None       => added.push(wf.clone()),
            Some(prev) => {
                if prev.sha256 == wf.sha256 { unchanged.push(wf.clone()); }
                else                        { modified.push(wf.clone()); }
            }
        }
    }

    let current_set: std::collections::HashSet<String> = current
        .iter()
        .map(|wf| wf.rel_path.to_string_lossy().replace('\\', "/"))
        .collect();
    let deleted: Vec<String> = prev_map
        .keys()
        .filter(|k| !current_set.contains(*k))
        .cloned()
        .collect();

    DiffResult { added, modified, unchanged, deleted }
}

pub fn to_file_record(wf: &WorkingFile, snapshot_id: &str) -> FileRecord {
    FileRecord {
        snapshot_id: snapshot_id.to_string(),
        rel_path:    wf.rel_path.to_string_lossy().replace('\\', "/"),
        sha256:      wf.sha256.clone(),
        size_bytes:  wf.size_bytes as i64,
        modified_at: wf.modified_at.clone(),
        inode:       wf.inode.map(|i| i as i64),
    }
}
