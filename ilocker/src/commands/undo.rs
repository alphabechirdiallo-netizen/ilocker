// ============================================================
//  commands/undo.rs — iloc undo [<ref>] [--file <path>]...
//
//  Restore the working tree from a snapshot while preserving
//  dependency directories (node_modules, .venv, …) so the
//  project remains immediately executable after restore.
//
//  v1.11.0 — Restauration sélective de fichiers
//  ──────────────────────────────────────────────
//  Avant cette version, `iloc undo` restaurait OBLIGATOIREMENT
//  tout le projet. Désormais :
//
//    iloc undo                              restaure tout (comme avant)
//    iloc undo 3                            restaure tout depuis le
//                                            snapshot #3 (référence
//                                            numérique — voir `iloc log`)
//    iloc undo --file src/app.rs            restaure UNIQUEMENT ce
//                                            fichier depuis le dernier
//                                            snapshot, sans toucher
//                                            au reste du projet
//    iloc undo 3 --file a.rs --file b.rs    restaure UNIQUEMENT ces 2
//                                            fichiers depuis le
//                                            snapshot #3
//
//  En mode sélectif, l'étape de suppression des fichiers "en trop"
//  (présents dans le projet mais absents du snapshot) est désactivée
//  : on ne touche jamais à autre chose que ce que l'utilisateur a
//  explicitement demandé.
//
//  Algorithm (mode complet, inchangé)
//  ───────────────────────────────────
//  1. Resolve target snapshot (référence numérique ou préfixe hex)
//  2. Auto-save a "pre-undo" safety snapshot (so the restore
//     is itself reversible)
//  3. Build a manifest of files to restore vs. to delete
//  4. Walk snapshot dir:
//       • If destination file is a preserved dep dir entry  → skip
//       • Otherwise hard-link / CoW from snapshot into CWD
//  5. Remove files present in CWD but absent from snapshot
//     (again skipping dep dirs) — SAUTÉ en mode sélectif
//  6. Print summary
// ============================================================

use crate::commands::init::assert_initialised;
use crate::commands::save;
use crate::db::{self, FileRecord};
use crate::engine::{link_or_clone, remove_file_safe, unlock_snapshot_file, set_readonly};
use crate::utils::{db_path, IgnoreRules};
use anyhow::{bail, Result};
use colored::Colorize;
use indicatif::{ProgressBar, ProgressStyle};
use std::collections::HashSet;
use std::path::Path;

use std::time::Instant;

/// Directories that are preserved during restore even if they differ
/// from the snapshot.  Add more as needed.
pub const PRESERVED_DEP_DIRS: &[&str] = &[
    "node_modules",
    ".venv",
    "venv",
    "env",
    ".env",
    "target",      // Rust build artefacts
    "vendor",      // Go / PHP
    "__pycache__",
    ".mypy_cache",
    ".pytest_cache",
    ".next",
    ".nuxt",
    "dist",
    "build",
];

/// Vrai si le chemin relatif tombe dans un dossier de dépendances
/// préservé (node_modules, .venv, target, ...). Réutilisé par
/// `iloc undo` ET `iloc pull` (cloud.rs) pour un comportement
/// cohérent : on ne piétine jamais un dossier de deps reconstruit
/// localement.
pub fn is_in_preserved_dir(rel: &str) -> bool {
    let preserve_set: HashSet<&str> = PRESERVED_DEP_DIRS.iter().copied().collect();
    rel.split('/').next().map(|top| preserve_set.contains(top)).unwrap_or(false)
}

pub fn run(id: Option<String>, files: Vec<String>) -> Result<()> {
    let ilocker_dir  = assert_initialised()?;
    let project_root = ilocker_dir.parent().unwrap().to_path_buf();
    let t0           = Instant::now();
    let selective    = !files.is_empty();

    // ── 1. Open DB & resolve target snapshot ─────────────────
    let db_file = db_path(&ilocker_dir);
    let conn    = db::open(&db_file)?;

    let target = match &id {
        Some(reference) => db::resolve_snapshot_ref(&conn, reference)?.ok_or_else(|| {
            anyhow::anyhow!(
                "Aucun snapshot ne correspond à '{}'.  Utilisez `iloc log` pour voir les références disponibles.",
                reference
            )
        })?,
        None => db::latest_snapshot(&conn)?.ok_or_else(|| {
            anyhow::anyhow!(
                "{}  No snapshots exist yet.  Run `iloc save` first.",
                "Nothing to undo.".yellow()
            )
        })?,
    };

    let snap_dir = crate::vault::snapshots_dir(&ilocker_dir).join(&target.id);
    if !snap_dir.exists() {
        bail!("Snapshot directory missing: {}", snap_dir.display());
    }

    println!();
    if selective {
        println!(
            "{} restoring {} file(s) — \"{}\"",
            "◀".cyan().bold(),
            files.len().to_string().yellow().bold(),
            target.message.bold()
        );
    } else {
        println!(
            "{} restoring \"{}\"",
            "◀".cyan().bold(),
            target.message.bold()
        );
    }
    println!("  {} {}", "snapshot:".dimmed(), target.created_at.dimmed());

    // ── 2. Auto-save safety snapshot ─────────────────────────
    println!("{}", "  creating safety snapshot before restore…".dimmed());
    save::run(&format!("(pre-undo safety) before restoring {}", target.id))?;
    println!();

    // ── 3. Load file manifest of target snapshot ──────────────
    // Re-open after save wrote to the DB
    let conn = db::open(&db_file)?;
    let all_records = db::files_for_snapshot(&conn, &target.id)?;

    // ── Mode sélectif : restaure uniquement les fichiers demandés ──
    if selective {
        let (selected, not_found) = db::select_records(&all_records, &files);

        if !not_found.is_empty() {
            for f in &not_found {
                println!(
                    "  {} '{}' introuvable dans ce snapshot — ignoré",
                    "⚠".yellow(), f
                );
            }
        }

        if selected.is_empty() {
            bail!(
                "Aucun des fichiers demandés n'existe dans ce snapshot. Rien à restaurer."
            );
        }

        let mut restored_paths = Vec::new();
        for record in &selected {
            let dest = project_root.join(&record.rel_path);
            let src  = snap_dir.join(&record.rel_path);

            if !src.exists() {
                println!(
                    "  {} '{}' manquant dans le coffre — ignoré",
                    "⚠".yellow(), record.rel_path
                );
                continue;
            }

            if dest.exists() {
                remove_file_safe(&dest)?;
            }
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            unlock_snapshot_file(&src)?;
            link_or_clone(&src, &dest)?;
            set_readonly(&src, true)?;
            restored_paths.push(record.rel_path.clone());
        }

        let elapsed = t0.elapsed();
        println!();
        println!(
            "{} restauration sélective terminée en {:.2}s",
            "✓".green().bold(),
            elapsed.as_secs_f64()
        );
        for p in &restored_paths {
            println!("  {} {}", "→".green(), p);
        }
        println!(
            "  {} {} fichier(s) restauré(s) · reste du projet {}",
            "result:".dimmed(),
            restored_paths.len().to_string().green(),
            "non touché".bold()
        );

        return Ok(());
    }

    // ── Mode complet (comportement historique, inchangé) ───────
    let records = all_records;

    // Build lookup: rel_path → FileRecord
    let snap_files: std::collections::HashMap<String, &FileRecord> =
        records.iter().map(|r| (r.rel_path.clone(), r)).collect();

    // Load deletion manifest (files that were deleted in this snapshot)
    let deleted_in_snap: Vec<String> = {
        let manifest = snap_dir.join(".deleted.json");
        if manifest.exists() {
            let raw = std::fs::read_to_string(&manifest)?;
            serde_json::from_str(&raw).unwrap_or_default()
        } else {
            Vec::new()
        }
    };

    // ── 4. Build preserved-path prefix set ───────────────────
    // (la fonction `is_in_preserved_dir` partagée est définie en haut du module)

    // ── 5. Restore files from snapshot ────────────────────────
    let pb = ProgressBar::new(records.len() as u64);
    pb.set_style(
        ProgressStyle::with_template(
            "  {spinner:.cyan} [{bar:40.green/blue}] {pos}/{len}  {msg}",
        )
        .unwrap()
        .progress_chars("█▉▊▋▌▍▎▏  "),
    );
    pb.set_message("restoring…");

    let mut restored = 0usize;
    let mut skipped  = 0usize;

    for record in &records {
        if is_in_preserved_dir(&record.rel_path) {
            skipped += 1;
            pb.inc(1);
            continue;
        }

        let dest = project_root.join(&record.rel_path);
        let src  = snap_dir.join(&record.rel_path);

        // Supprimer le fichier existant avant de le remplacer
        if dest.exists() {
            remove_file_safe(&dest)?;
        }
        if src.exists() {
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            // Lever le verrou READ-ONLY du snapshot avant de lire
            unlock_snapshot_file(&src)?;
            link_or_clone(&src, &dest)?;
            // Re-verrouiller le fichier source dans le snapshot
            set_readonly(&src, true)?;
            restored += 1;
        }
        pb.inc(1);
    }

    pb.finish_and_clear();

    // ── 6. Remove files that exist now but should not ─────────
    // (files present in CWD that were not in the target snapshot)
    let ignore = IgnoreRules::load(&project_root);
    let mut removed = 0usize;

    for entry in walkdir::WalkDir::new(&project_root)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
    {
        let abs = entry.path();
        let rel = match abs.strip_prefix(&project_root) {
            Ok(r)  => r.to_string_lossy().replace('\\', "/"),
            Err(_) => continue,
        };

        if ignore.is_ignored(Path::new(&rel)) { continue; }
        if is_in_preserved_dir(&rel)          { continue; }
        if snap_files.contains_key(&rel)      { continue; }
        if deleted_in_snap.contains(&rel)     {
            remove_file_safe(abs)?;
            removed += 1;
        }
    }

    let elapsed = t0.elapsed();

    // ── 7. Summary ────────────────────────────────────────────
    println!();
    println!(
        "{} restore complete in {:.2}s",
        "✓".green().bold(),
        elapsed.as_secs_f64()
    );
    println!(
        "  {} {} files restored · {} dep-dir files preserved · {} files removed",
        "result:".dimmed(),
        restored.to_string().green(),
        skipped.to_string().yellow(),
        removed.to_string().red(),
    );
    println!(
        "  {} node_modules / .venv / build dirs were {} and remain usable",
        "⚡".yellow(),
        "untouched".bold()
    );

    Ok(())
}
