// ============================================================
//  commands/save.rs — iloc save "[message]"
//  Phase 2: uses ScanStats to surface large-file info and
//  reports the adaptive buffer tier in the summary.
// ============================================================

use crate::commands::init::assert_initialised;
use crate::db::{self, FileRecord};
use crate::engine::{link_or_clone, seal_snapshot_dir, LinkMethod};
use crate::snapshot::{self, to_file_record};
use crate::utils::{db_path, human_bytes, new_snapshot_id, IgnoreRules, ProjectType};
use anyhow::Result;
use chrono::Utc;
use colored::Colorize;
use indicatif::{ProgressBar, ProgressStyle};
use std::time::Instant;

pub fn run(message: &str) -> Result<()> {
    let ilocker_dir  = assert_initialised()?;
    let project_root = ilocker_dir.parent().unwrap().to_path_buf();
    let t0           = Instant::now();

    // ── 1. Detect project type (Smart Ignore hint) ────────────
    let project_types = ProjectType::detect(&project_root);
    let type_labels: Vec<&str> = project_types.iter().map(|t| t.label()).collect();

    // ── 2. Load ignore rules ──────────────────────────────────
    let ignore = IgnoreRules::load(&project_root);

    // ── 3. Scan working tree (adaptive SHA-256) ───────────────
    println!(
        "  {} {} project detected — scanning…",
        "→".cyan(),
        type_labels.join(" + ").bold()
    );
    let (current_files, scan_stats) = snapshot::scan_project(&project_root, &ignore)?;

    // Warn if we found giant files (datasets, model weights…)
    if scan_stats.large_files > 0 {
        println!(
            "  {} {} large file(s) ≥ 1 GiB detected — using 16 MiB streaming buffers",
            "⚠".yellow(),
            scan_stats.large_files
        );
    }

    // ── 4. Load previous snapshot index ──────────────────────
    let db_file = db_path(&ilocker_dir);
    let mut conn = db::open(&db_file)?;

    let previous_records = match db::latest_snapshot(&conn)? {
        Some(ref prev) => db::files_for_snapshot(&conn, &prev.id)?,
        None           => Vec::new(),
    };
    let parent_id = db::latest_snapshot(&conn)?.map(|s| s.id);

    // ── 5. Diff ───────────────────────────────────────────────
    let diff = snapshot::diff(&current_files, &previous_records);

    let n_changed   = diff.added.len() + diff.modified.len();
    let n_unchanged = diff.unchanged.len();
    let n_deleted   = diff.deleted.len();
    let total       = current_files.len();

    // ── 6. Create snapshot directory ─────────────────────────
    let snap_id  = new_snapshot_id();
    let snap_dir = crate::vault::snapshots_dir(&ilocker_dir).join(&snap_id);
    std::fs::create_dir_all(&snap_dir)?;

    // ── 7. Hard-link / clone all files into snapshot dir ─────
    let pb = ProgressBar::new(total as u64);
    pb.set_style(
        ProgressStyle::with_template(
            "  {spinner:.green} [{bar:40.cyan/blue}] {pos}/{len}  {msg}",
        )
        .unwrap()
        .progress_chars("█▉▊▋▌▍▎▏  "),
    );
    pb.set_message("cloning/copying files…");

    let mut records: Vec<FileRecord> = Vec::with_capacity(total);
    let mut cow_count  = 0usize;
    let mut copy_count = 0usize;
    let mut total_bytes: u64 = 0;

    for wf in diff.unchanged.iter()
        .chain(diff.added.iter())
        .chain(diff.modified.iter())
    {
        let dest   = snap_dir.join(&wf.rel_path);
        let method = link_or_clone(&wf.abs_path, &dest)?;
        match method {
            LinkMethod::RefLink => cow_count  += 1,
            LinkMethod::Copy    => copy_count += 1,
        }
        records.push(to_file_record(wf, &snap_id));
        total_bytes += wf.size_bytes;
        pb.inc(1);
    }

    pb.finish_and_clear();

    // ── 8. Write deletion manifest ────────────────────────────
    if !diff.deleted.is_empty() {
        let manifest_path = snap_dir.join(".deleted.json");
        std::fs::write(
            manifest_path,
            serde_json::to_string_pretty(&diff.deleted)?,
        )?;
    }

    // ── 8b. Seal snapshot directory (READ-ONLY) ───────────────
    // Scelle TOUS les fichiers du snapshot (y compris .deleted.json)
    // après leur écriture complète.  Sur Windows cela pose le flag
    // FILE_ATTRIBUTE_READONLY via SetFileAttributesW ; sur POSIX cela
    // retire les bits d'écriture.  iloc undo lève le verrou
    // temporairement avant restauration.
    seal_snapshot_dir(&snap_dir)?;

    // ── 9. Persist to SQLite ──────────────────────────────────
    let created_at = Utc::now().to_rfc3339();
    db::insert_snapshot(&conn, &snap_id, message, parent_id.as_deref(), &created_at)?;
    db::insert_file_records(&mut conn, &records)?;
    db::update_snapshot_stats(&conn, &snap_id, records.len() as i64, total_bytes as i64)?;

    let elapsed = t0.elapsed();

    // ── 10. Summary ───────────────────────────────────────────
    println!();
    println!(
        "{} \"{}\"",
        "✓".green().bold(),
        message.bold()
    );
    println!(
        "  {} saved in {:.2}s",
        "snapshot:".dimmed(),
        elapsed.as_secs_f64()
    );
    println!("  {} {}", "id:".dimmed(), snap_id.dimmed());
    println!(
        "  {} {} added/modified · {} unchanged · {} deleted",
        "changes:".dimmed(),
        n_changed.to_string().yellow(),
        n_unchanged.to_string().green(),
        n_deleted.to_string().red(),
    );
    println!(
        "  {} {} logical  ({} files)",
        "size:".dimmed(),
        human_bytes(total_bytes),
        total
    );

    if scan_stats.medium_files > 0 || scan_stats.large_files > 0 {
        println!(
            "  {} adaptive buffers: {} small · {} medium (4 MiB buf) · {} large (16 MiB buf)",
            "⚡".yellow(),
            total - scan_stats.medium_files - scan_stats.large_files,
            scan_stats.medium_files,
            scan_stats.large_files,
        );
    }

    if cow_count > 0 || copy_count > 0 {
        println!(
            "  {} {} CoW clone(s) · {} physical copy(ies) · snapshot sealed READ-ONLY",
            "⚡".yellow(), cow_count, copy_count
        );
    } else {
        println!(
            "  {} snapshot sealed READ-ONLY (tamper-proof)",
            "🔒".yellow()
        );
    }

    // ── 12. Sauvegarde secondaire (Tier 2/3) ───────────────────
    // Miroir local / Cloud BYOC / Hyperscale — uniquement si configurés
    // via `iloc vault`. Best-effort : ne peut jamais faire échouer save
    // (les erreurs sont affichées mais n'empêchent pas le snapshot local,
    // déjà scellé à ce stade).
    //
    // Le thread est explicitement attendu (join) : `iloc save` est une
    // commande CLI de courte durée, son process se termine dès que cette
    // fonction retourne. Un thread lancé sans être joint mourrait avec
    // le process avant d'avoir eu la moindre chance de terminer un push
    // réseau réel — confirmé par test contre un vrai backend S3, où le
    // bucket restait systématiquement vide sans ce join. Le snapshot
    // LOCAL, lui, est déjà confirmé à l'utilisateur ci-dessus avant ce
    // point : ce join n'ajoute donc pas de latence perçue sur la partie
    // qui compte le plus, seulement sur la sauvegarde secondaire elle-même.
    if let Some(handle) = crate::vault::run_backup_tiers_async(ilocker_dir.clone(), snap_id.clone()) {
        let _ = handle.join();
    }

    Ok(())
}
