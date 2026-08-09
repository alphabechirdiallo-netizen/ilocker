use colored::Colorize;
// ============================================================
//  commands/log.rs — iloc log [--ids]
//  Prints all snapshots, newest first.
//
//  v1.11.0 — Le message écrit par l'utilisateur est désormais
//  l'élément visuellement prioritaire ; l'ID hexadécimal (« les
//  chiffres ») est masqué par défaut et remplacé par une simple
//  référence numérique séquentielle (#1 = le plus récent), bien
//  plus facile à utiliser avec `iloc undo <N>` ou
//  `iloc undo <N> --file <chemin>`.
//
//  L'ID complet reste disponible à la demande via `iloc log --ids`
//  (utile pour les scripts ou pour partager une référence exacte).
// ============================================================

use crate::commands::init::assert_initialised;
use crate::db;
use crate::utils::{db_path, human_bytes};
use anyhow::Result;

pub fn run(show_ids: bool) -> Result<()> {
    let ilocker_dir = assert_initialised()?;
    let db_file     = db_path(&ilocker_dir);
    let conn        = db::open(&db_file)?;
    let snapshots   = db::list_snapshots(&conn)?;

    if snapshots.is_empty() {
        println!("{}", "No snapshots yet.  Run `iloc save \"message\"`.".yellow());
        return Ok(());
    }

    println!();
    for (i, snap) in snapshots.iter().enumerate() {
        let n = i + 1;
        let badge = if i == 0 {
            format!("#{} {}", n, "(latest)".green())
        } else {
            format!("#{}", n)
        };

        println!("  {} {}", "◉".dimmed(), badge.cyan().bold());
        println!(
            "    {}",
            snap.message.bold()
        );
        if show_ids {
            println!("    {} {}", "id:".dimmed(), snap.id.dimmed());
        }
        println!(
            "    {}  {} files · {}",
            snap.created_at.dimmed(),
            snap.file_count.to_string().yellow(),
            human_bytes(snap.total_bytes as u64).yellow()
        );
        if i < snapshots.len() - 1 {
            println!("  {}", "│".dimmed());
        }
    }
    println!();
    if !show_ids {
        println!(
            "{}",
            "  Astuce : `iloc undo <#>` restaure depuis ce numéro · `iloc log --ids` affiche les ID complets.".dimmed()
        );
        println!();
    }

    Ok(())
}
