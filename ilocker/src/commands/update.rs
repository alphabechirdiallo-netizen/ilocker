// ============================================================
//  commands/update.rs — iloc update [--check]
//
//  iloc update          → vérifie + télécharge + installe
//  iloc update --check  → vérifie seulement, affiche le statut
//
//  Aucun serveur iLocker requis. Source : GitHub Releases uniquement.
// ============================================================

use crate::updater::{self, UpdateStatus};
use anyhow::Result;
use colored::Colorize;
use std::path::PathBuf;
use std::env;

pub async fn run(check_only: bool) -> Result<()> {
    println!();
    println!("{}", "ilocker — Mise à jour".bold());
    println!();

    println!("{}", "  Vérification de la dernière version…".dimmed());

    let (status, release) = updater::check().await
        .map_err(|e| {
            anyhow::anyhow!(
                "Impossible de vérifier les mises à jour : {}\n\
                 Vérifiez votre connexion internet.",
                e
            )
        })?;

    updater::print_version_info(&release, status);

    if check_only {
        return Ok(());
    }

    match status {
        UpdateStatus::UpToDate => {
            // Rien à faire
            return Ok(());
        }
        UpdateStatus::UpdateAvailable => {}
    }

    // ── Confirmation ──────────────────────────────────────────
    println!(
        "  Mettre à jour {} → {} ? [Y/n] ",
        updater::CURRENT_VERSION.dimmed(),
        release.version.green().bold()
    );
    use std::io::Write;
    std::io::stdout().flush()?;
    let mut ans = String::new();
    std::io::stdin().read_line(&mut ans)?;
    if ans.trim().eq_ignore_ascii_case("n") {
        println!("  Annulé.");
        println!();
        return Ok(());
    }

    // ── Téléchargement ────────────────────────────────────────
    println!();
    println!(
        "  {} Téléchargement de {} ({})…",
        "↓".cyan().bold(),
        release.version.bold(),
        updater::platform_asset_name().dimmed()
    );

    let tmp_dir  = env::temp_dir();
    let tmp_file = tmp_dir.join(format!("iloc-update-{}", release.version));

    updater::download_binary(&release.asset, &tmp_file).await
        .map_err(|e| anyhow::anyhow!("Échec du téléchargement : {}", e))?;

    // ── Remplacement atomique ─────────────────────────────────
    println!("  {} Installation en cours…", "⚙".cyan());

    let installed_at = updater::atomic_replace(&tmp_file)
        .map_err(|e| anyhow::anyhow!(
            "{}\n\n  Si vous n'avez pas les permissions, essayez :\n    sudo iloc update",
            e
        ))?;

    // Nettoyer le fichier temporaire
    let _ = std::fs::remove_file(&tmp_file);

    // ── Succès ────────────────────────────────────────────────
    println!();
    println!(
        "  {} ilocker mis à jour vers {}",
        "✓".green().bold(),
        release.version.green().bold()
    );
    println!(
        "  {} {}",
        "installé dans:".dimmed(),
        installed_at.display().to_string().dimmed()
    );
    println!();
    println!("{}", "  Relancez iloc pour utiliser la nouvelle version.".dimmed());
    println!();

    Ok(())
}
