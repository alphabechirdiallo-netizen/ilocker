// ============================================================
//  commands/vault.rs — iloc vault <action>
//
//  Pilote le coffre-fort externalisé et la sauvegarde 3-2-1 :
//    iloc vault status                 — état du vault + tiers
//    iloc vault migrate                — déplacer vers un autre mode/chemin
//    iloc vault mirror add <path>      — Tier 2 : ajouter un miroir local
//    iloc vault mirror remove <path>   — Tier 2 : retirer un miroir
//    iloc vault mirror sync            — forcer une synchro immédiate
//    iloc vault backup enable-cloud    — Tier 3a (BYOC, iloc push)
//    iloc vault backup disable-cloud
//    iloc vault backup enable-hyperscale  — Tier 3b
//    iloc vault backup disable-hyperscale
//    iloc vault verify                 — vérifie l'intégrité (SHA-256)
//                                         de tous les snapshots du vault
// ============================================================

use crate::commands::init::assert_initialised;
use crate::db;
use crate::utils::{db_path, human_bytes};
use crate::vault::{self, MirrorTarget, VaultHealth, VaultMode};
use anyhow::{bail, Result};
use colored::Colorize;
use std::path::PathBuf;

pub fn run_status() -> Result<()> {
    let ilocker_dir = assert_initialised()?;
    let cfg = vault::load(&ilocker_dir);

    println!();
    println!("{}", "  Coffre-fort ilocker".bold());
    println!("  {} {}", "mode:".dimmed(), cfg.mode.label().cyan());
    println!("  {} {}", "chemin:".dimmed(), cfg.vault_path.display().to_string().cyan());

    match vault::check_health(&ilocker_dir) {
        VaultHealth::Ok => println!("  {} accessible", "santé:".dimmed()),
        VaultHealth::Missing(p) => println!(
            "  {} {} — {} introuvable (disque débranché ou dossier déplacé ?)\n         Lancez `iloc vault migrate` pour le relocaliser proprement.",
            "santé:".dimmed(), "⚠ ORPHELIN".red().bold(), p.display()
        ),
    }

    let size = dir_size(&cfg.vault_path);
    println!("  {} {}", "taille:".dimmed(), human_bytes(size));

    println!();
    println!("  {}", "Sauvegarde secondaire (stratégie 3-2-1)".bold());
    if cfg.mirrors.is_empty() {
        println!("  {} aucun", "tier2 (miroir local):".dimmed());
    } else {
        for m in &cfg.mirrors {
            let state = if m.enabled { "actif".green() } else { "désactivé".dimmed() };
            println!("  {} {} [{}]", "tier2 (miroir local):".dimmed(), m.path.display(), state);
        }
    }
    println!(
        "  {} {}",
        "tier3a (cloud byoc):".dimmed(),
        if cfg.cloud_backup_enabled { "activé".green().to_string() } else { "désactivé".dimmed().to_string() }
    );
    println!(
        "  {} {}",
        "tier3b (hyperscale):".dimmed(),
        if cfg.hyperscale_backup_enabled { "activé".green().to_string() } else { "désactivé".dimmed().to_string() }
    );
    let sentinel_active = std::env::var("ILOC_SENTINEL_ACTIVE").map(|v| v == "1").unwrap_or(false);
    println!(
        "  {} {}",
        "sentinel (auto-save):".dimmed(),
        if sentinel_active {
            "actif dans cette session".green().to_string()
        } else {
            "inactif ici — `iloc sentinel status` pour les détails".yellow().to_string()
        }
    );
    println!();

    if cfg.mode == VaultMode::InProject {
        println!(
            "  {} le vault vit encore dans .ilocker/ (mode historique).",
            "ℹ".cyan()
        );
        println!(
            "    Si ce projet est versionné avec Git, exécutez :"
        );
        println!(
            "    {}",
            "iloc vault migrate --mode sibling".cyan()
        );
        println!();
    }

    Ok(())
}

pub fn run_migrate(mode_str: Option<String>, dir: Option<PathBuf>) -> Result<()> {
    let ilocker_dir = assert_initialised()?;
    let mode = match &mode_str {
        Some(s) => VaultMode::parse(s)?,
        None    => VaultMode::Sibling, // meilleure option par défaut : même volume, CoW préservé
    };

    println!("{} migration du vault vers le mode '{}'…", "→".cyan(), mode.label());
    let (old_path, new_path) = vault::migrate(&ilocker_dir, mode, dir)?;

    println!();
    println!("{} migration terminée", "✓".green().bold());
    println!("  {} {}", "ancien:".dimmed(), old_path.display());
    println!("  {} {}", "nouveau:".dimmed(), new_path.display());
    println!();
    println!("{}", "  Vérifiez avec `iloc vault status` puis `iloc log`.".dimmed());

    Ok(())
}

pub fn run_mirror_add(path: PathBuf) -> Result<()> {
    let ilocker_dir = assert_initialised()?;
    let mut cfg = vault::load(&ilocker_dir);

    if cfg.mirrors.iter().any(|m| m.path == path) {
        bail!("Ce miroir est déjà configuré : {}", path.display());
    }

    std::fs::create_dir_all(&path)
        .map_err(|e| anyhow::anyhow!("Impossible de créer/accéder à {} : {}", path.display(), e))?;

    cfg.mirrors.push(MirrorTarget { path: path.clone(), enabled: true });
    vault::save(&cfg, &ilocker_dir)?;

    println!("{} miroir ajouté : {}", "✓".green().bold(), path.display());
    println!("{}", "  Il sera synchronisé à chaque `iloc save`.".dimmed());
    Ok(())
}

pub fn run_mirror_remove(path: PathBuf) -> Result<()> {
    let ilocker_dir = assert_initialised()?;
    let mut cfg = vault::load(&ilocker_dir);

    let before = cfg.mirrors.len();
    cfg.mirrors.retain(|m| m.path != path);
    if cfg.mirrors.len() == before {
        bail!("Aucun miroir configuré pour ce chemin : {}", path.display());
    }
    vault::save(&cfg, &ilocker_dir)?;
    println!("{} miroir retiré : {}", "✓".green().bold(), path.display());
    Ok(())
}

pub fn run_mirror_sync() -> Result<()> {
    let ilocker_dir = assert_initialised()?;
    let cfg = vault::load(&ilocker_dir);
    let db_file = db_path(&ilocker_dir);
    let conn = db::open(&db_file)?;
    let snap = db::latest_snapshot(&conn)?
        .ok_or_else(|| anyhow::anyhow!("Aucun snapshot. Lancez `iloc save` d'abord."))?;

    println!("{} synchronisation des miroirs pour le snapshot {}…", "→".cyan(), snap.id.cyan());
    vault::mirror_snapshot(&cfg, &snap.id);
    println!("{} synchronisation terminée", "✓".green().bold());
    Ok(())
}

pub fn run_backup_toggle(target: BackupTarget, enable: bool, profile: Option<String>) -> Result<()> {
    let ilocker_dir = assert_initialised()?;
    let mut cfg = vault::load(&ilocker_dir);
    match target {
        BackupTarget::Cloud => {
            cfg.cloud_backup_enabled = enable;
            if enable { cfg.cloud_backup_profile = profile.clone(); }
        }
        BackupTarget::Hyperscale => cfg.hyperscale_backup_enabled = enable,
    }
    vault::save(&cfg, &ilocker_dir)?;
    let label = match target { BackupTarget::Cloud => "Cloud BYOC (Tier 3a)", BackupTarget::Hyperscale => "Hyperscale (Tier 3b)" };
    println!(
        "{} {} {}",
        "✓".green().bold(),
        label,
        if enable { "activé — déclenché après chaque iloc save".green().to_string() } else { "désactivé".dimmed().to_string() }
    );
    if enable && matches!(target, BackupTarget::Cloud) {
        match &profile {
            Some(p) => println!("  {} {}", "profil ciblé:".dimmed(), p.cyan()),
            None    => println!("{}", "  Utilisera le profil cloud actif (`iloc config cloud list`).".dimmed()),
        }
        println!("{}", "  Assurez-vous d'avoir configuré vos credentials : `iloc config cloud add`".dimmed());
    }
    if enable && matches!(target, BackupTarget::Hyperscale) {
        println!(
            "{}",
            "  ℹ Chaque `iloc save` déclenchera un `iloc hyperscale push` en arrière-plan (non bloquant).".cyan()
        );
    }
    Ok(())
}

#[derive(Clone, Copy)]
pub enum BackupTarget { Cloud, Hyperscale }

/// `iloc vault verify` — recalcule le SHA-256 de chaque fichier de
/// chaque snapshot et le compare à celui enregistré dans l'index
/// SQLite au moment du `save`. Détecte toute corruption silencieuse
/// du vault (disque défaillant, modification externe, etc.).
pub fn run_verify() -> Result<()> {
    let ilocker_dir = assert_initialised()?;
    let db_file = db_path(&ilocker_dir);
    let conn = db::open(&db_file)?;
    let snapshots = db::list_snapshots(&conn)?;

    if snapshots.is_empty() {
        println!("{}", "Aucun snapshot à vérifier.".dimmed());
        return Ok(());
    }

    println!("{} vérification de {} snapshot(s)…", "→".cyan(), snapshots.len());

    let snapshots_root = vault::snapshots_dir(&ilocker_dir);
    let mut total_checked = 0usize;
    let mut total_corrupt = 0usize;
    let mut total_missing = 0usize;

    for snap in &snapshots {
        let records = db::files_for_snapshot(&conn, &snap.id)?;
        let snap_dir = snapshots_root.join(&snap.id);

        for rec in &records {
            let file_path = snap_dir.join(&rec.rel_path);
            total_checked += 1;

            if !file_path.exists() {
                total_missing += 1;
                println!(
                    "  {} {} → {} : fichier manquant",
                    "✗".red(), snap.id.dimmed(), rec.rel_path
                );
                continue;
            }

            match crate::utils::sha256_file(&file_path) {
                Ok(actual) if actual == rec.sha256 => {}
                Ok(actual) => {
                    total_corrupt += 1;
                    println!(
                        "  {} {} → {} : hash différent (attendu {}…, trouvé {}…)",
                        "✗".red(), snap.id.dimmed(), rec.rel_path,
                        &rec.sha256[..12], &actual[..12]
                    );
                }
                Err(e) => {
                    total_corrupt += 1;
                    println!("  {} {} → {} : lecture impossible ({})", "✗".red(), snap.id.dimmed(), rec.rel_path, e);
                }
            }
        }
    }

    println!();
    if total_corrupt == 0 && total_missing == 0 {
        println!(
            "{} intégrité confirmée — {} fichiers vérifiés sur {} snapshot(s)",
            "✓".green().bold(), total_checked, snapshots.len()
        );
    } else {
        println!(
            "{} {} corrompu(s), {} manquant(s) sur {} fichiers vérifiés",
            "⚠".yellow().bold(), total_corrupt, total_missing, total_checked
        );
    }

    Ok(())
}

fn dir_size(path: &std::path::Path) -> u64 {
    walkdir::WalkDir::new(path)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter_map(|e| e.metadata().ok())
        .map(|m| m.len())
        .sum()
}
