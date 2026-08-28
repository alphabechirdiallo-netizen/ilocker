// ============================================================
//  commands/studio.rs — Manifeste de commandes + lancement du
//  centre de commandes ilocker (extension VS Code "ilocker Studio")
//
//  Principe : le manifeste JSON n'est JAMAIS un fichier figé au
//  build. Il est régénéré à chaque appel, par introspection directe
//  de la structure clap réelle (`Cli::command()`). C'est le MÊME
//  code qui définit les commandes et qui les décrit — aucune
//  divergence possible entre ce que ce module rapporte et ce que le
//  binaire sait réellement exécuter.
//
//  L'extension VS Code appelle `iloc studio manifest` en
//  sous-processus à chaque ouverture, jamais un fichier mis en
//  cache — garantit une cohérence perpétuelle avec la version
//  d'iloc réellement installée sur la machine, même après
//  `iloc update`.
// ============================================================

use crate::Cli;
use anyhow::{Context, Result};
use clap::{Command, CommandFactory};
use serde::Serialize;
use std::path::PathBuf;

#[derive(Serialize, Debug, Clone)]
pub struct ArgEntry {
    /// Identifiant interne clap (nom du champ Rust)
    pub id: String,
    pub positional: bool,
    pub required: bool,
    pub help: Option<String>,
    pub takes_value: bool,
    /// Flag réel à composer en ligne de commande, ex: "--private".
    /// `None` pour un argument positionnel.
    pub long_flag: Option<String>,
}

#[derive(Serialize, Debug, Clone)]
pub struct CommandEntry {
    /// Chemin complet depuis la racine, ex: ["github", "repo", "create"]
    pub path: Vec<String>,
    pub name: String,
    pub about: Option<String>,
    /// true si cette entrée exécute réellement quelque chose (pas un
    /// simple groupe de sous-commandes comme "github" ou "repo" seuls)
    pub is_leaf: bool,
    pub args: Vec<ArgEntry>,
    /// Contenu éditorial (résumé, exemple, prérequis, risque) — voir
    /// studio_docs.rs. `None` pour les commandes pas encore
    /// documentées (aucune ne devrait rester dans cet état sur une
    /// version publiée : voir le test `every_leaf_command_is_documented`).
    pub doc: Option<crate::commands::studio_docs::CommandDoc>,
}

/// Sous-commandes clap générées automatiquement à filtrer du manifeste
/// (pas de vraies commandes ilocker).
const NOISE_NAMES: &[&str] = &["help"];

fn walk(cmd: &Command, path: &[String], out: &mut Vec<CommandEntry>) {
    for sub in cmd.get_subcommands() {
        let name = sub.get_name().to_string();
        if NOISE_NAMES.contains(&name.as_str()) || sub.is_hide_set() {
            continue;
        }

        let mut new_path = path.to_vec();
        new_path.push(name.clone());

        let is_leaf = sub
            .get_subcommands()
            .all(|s| NOISE_NAMES.contains(&s.get_name()));

        let args: Vec<ArgEntry> = sub
            .get_arguments()
            .filter(|a| a.get_id().as_str() != "help" && a.get_id().as_str() != "version")
            .map(|a| ArgEntry {
                id: a.get_id().to_string(),
                positional: a.is_positional(),
                required: a.is_required_set(),
                help: a.get_help().map(|s| s.to_string()),
                takes_value: !matches!(
                    a.get_action(),
                    clap::ArgAction::SetTrue | clap::ArgAction::SetFalse
                ),
                long_flag: a.get_long().map(|s| format!("--{s}")),
            })
            .collect();

        out.push(CommandEntry {
            path: new_path.clone(),
            name,
            about: sub.get_about().map(|s| s.to_string()),
            is_leaf,
            args,
            doc: None,
        });

        walk(sub, &new_path, out);
    }
}

/// Génère le manifeste complet, par introspection directe de la
/// structure clap réelle du binaire courant, fusionné avec le
/// contenu éditorial (studio_docs) par chemin de commande.
pub fn generate() -> Vec<CommandEntry> {
    let root = Cli::command();
    let mut out = Vec::new();
    walk(&root, &[], &mut out);

    let docs = crate::commands::studio_docs::load();
    for entry in &mut out {
        let key = entry.path.join(".");
        entry.doc = docs.get(&key).cloned();
    }

    out
}

pub fn run_manifest(output: Option<PathBuf>) -> Result<()> {
    let manifest = generate();
    let json = serde_json::to_string_pretty(&manifest)
        .context("Échec de sérialisation du manifeste")?;

    match output {
        Some(path) => {
            std::fs::write(&path, &json)
                .with_context(|| format!("Impossible d'écrire {}", path.display()))?;
        }
        None => println!("{json}"),
    }
    Ok(())
}

/// Historique structuré des snapshots du projet courant — même
/// source de données que `iloc log`, juste sérialisée en JSON au
/// lieu d'un affichage terminal. Consommé par l'extension VS Code
/// pour la vue "Historique des snapshots".
pub fn run_snapshots(limit: Option<usize>) -> Result<()> {
    let ilocker_dir = crate::commands::init::assert_initialised()?;
    let db_file      = crate::utils::db_path(&ilocker_dir);
    let conn         = crate::db::open(&db_file)?;
    let mut snapshots = crate::db::list_snapshots(&conn)?;
    if let Some(n) = limit {
        snapshots.truncate(n);
    }
    println!("{}", serde_json::to_string_pretty(&snapshots)?);
    Ok(())
}

/// État de déploiement structuré (liens GitHub/Vercel/Supabase +
/// dernier déploiement connu) — lecture locale instantanée de
/// .ilocker/deploy.toml, sans appel réseau. Consommé par
/// l'extension VS Code pour la vue "Historique de déploiement".
pub fn run_deploy_status() -> Result<()> {
    let cwd   = std::env::current_dir()?;
    let state = crate::deploy_state::load_state(&cwd)?;
    println!("{}", serde_json::to_string_pretty(&state)?);
    Ok(())
}

#[derive(Serialize)]
struct ProjectStatus {
    initialised:       bool,
    vault_mode:        Option<String>,
    sentinel_active:   bool,
    github_connected:   bool,
    vercel_connected:   bool,
    supabase_connected: bool,
    snapshot_count:     i64,
}

/// Vue d'ensemble consolidée du projet courant — pensée pour la vue
/// dockée (sidebar) de l'extension : ce qui est déjà configuré, ce
/// qui ne l'est pas encore. Best-effort : une source indisponible
/// (ex : pas encore de compte connecté) donne juste `false`/`None`,
/// jamais une erreur qui bloquerait tout l'affichage.
pub fn run_project_status() -> Result<()> {
    let cwd = std::env::current_dir()?;
    let ilocker_dir = cwd.join(".ilocker");
    let initialised = ilocker_dir.exists();

    let vault_mode = if initialised {
        Some(format!("{:?}", crate::vault::load(&ilocker_dir).mode))
    } else {
        None
    };

    let snapshot_count = if initialised {
        let db_file = crate::utils::db_path(&ilocker_dir);
        crate::db::open(&db_file)
            .and_then(|conn| crate::db::list_snapshots(&conn))
            .map(|s| s.len() as i64)
            .unwrap_or(0)
    } else {
        0
    };

    let sentinel_active = std::env::var("ILOC_SENTINEL_ACTIVE").map(|v| v == "1").unwrap_or(false);

    let status = ProjectStatus {
        initialised,
        vault_mode,
        sentinel_active,
        github_connected:   crate::github_store::require_credentials(None).is_ok(),
        vercel_connected:   crate::vercel_store::require_credentials(None).is_ok(),
        supabase_connected: crate::supabase_store::require_credentials(None).is_ok(),
        snapshot_count,
    };

    println!("{}", serde_json::to_string_pretty(&status)?);
    Ok(())
}

/// Vrai/faux : le CLI `code` (VS Code, ou un fork compatible comme
/// Cursor/Windsurf enregistré sous le même nom) est-il dans le PATH ?
///
/// Sous Windows, VS Code s'installe comme un script `code.cmd` (pas
/// `code.exe`). `std::process::Command` ne résout automatiquement que
/// l'extension `.exe` quand aucune extension n'est fournie (comportement
/// documenté de la bibliothèque standard) — jamais `.cmd`/`.bat`, à la
/// différence du CLI Windows (cmd.exe/PowerShell) qui, lui, applique
/// %PATHEXT%. Sans essayer explicitement `code.cmd`, l'éditeur reste
/// introuvable même correctement installé et fonctionnel en tapant
/// `code` dans un terminal.
fn vscode_cli_available() -> Option<String> {
    let candidates: &[&str] = if cfg!(windows) {
        &[
            "code.cmd", "code.exe", "code",
            "code-insiders.cmd", "code-insiders.exe", "code-insiders",
            "cursor.cmd", "cursor.exe", "cursor",
        ]
    } else {
        &["code", "code-insiders", "cursor"]
    };
    for bin in candidates {
        if let Ok(output) = std::process::Command::new(bin).arg("--version").output() {
            if output.status.success() {
                return Some(bin.to_string());
            }
        }
    }
    None
}

/// Nom fixe de l'asset .vsix dans les releases GitHub — indépendant du
/// numéro de version de l'extension (renommé ainsi par release.yml
/// après `vsce package`, qui génère par défaut un nom versionné).
const VSIX_ASSET_NAME: &str = "ilocker-studio.vsix";
const VSIX_EXTENSION_ID: &str = "ilocker.ilocker-studio";

/// L'extension est-elle déjà installée dans cet éditeur ?
fn extension_already_installed(editor_bin: &str) -> bool {
    std::process::Command::new(editor_bin)
        .arg("--list-extensions")
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .any(|l| l.eq_ignore_ascii_case(VSIX_EXTENSION_ID))
        })
        .unwrap_or(false)
}

pub async fn run_open() -> Result<()> {
    let Some(editor_bin) = vscode_cli_available() else {
        println!("⚠ Aucun CLI d'éditeur détecté dans le PATH (code / cursor).");
        println!();
        println!("  Installez VS Code, puis assurez-vous que la commande `code`");
        println!("  est accessible depuis un terminal (VS Code : palette de");
        println!("  commandes → « Shell Command: Install 'code' command in PATH »).");
        return Ok(());
    };

    if !extension_already_installed(&editor_bin) {
        println!("→ Installation de l'extension « ilocker Studio »…");
        match install_extension(&editor_bin).await {
            Ok(()) => println!("✓ Extension installée."),
            Err(e) => {
                println!("⚠ Installation automatique impossible : {e:#}");
                println!("  Téléchargez « {VSIX_ASSET_NAME} » depuis la page Releases");
                println!("  du dépôt, puis : {editor_bin} --install-extension <fichier.vsix>");
                return Ok(());
            }
        }
    }

    let cwd = std::env::current_dir().unwrap_or_default();
    println!("→ Ouverture d'ilocker Studio dans {editor_bin}…");
    let status = std::process::Command::new(&editor_bin)
        .arg(&cwd)
        .arg("--command")
        .arg("ilockerStudio.openCommandCenter")
        .status();

    if status.is_err() || !status.map(|s| s.success()).unwrap_or(false) {
        // Fallback : ouvrir simplement l'éditeur sur le dossier — la
        // commande palette peut être indisponible selon la version du
        // CLI ; l'utilisateur retrouve l'icône ilocker dans la barre
        // d'activité de toute façon.
        let _ = std::process::Command::new(&editor_bin).arg(&cwd).status();
    }
    println!("✓ Commande envoyée à {editor_bin}. Si la fenêtre ne s'affiche pas au premier");
    println!("  plan, vérifiez qu'une fenêtre VS Code n'est pas déjà ouverte en arrière-plan.");

    Ok(())
}

async fn install_extension(editor_bin: &str) -> Result<()> {
    let release = crate::updater::fetch_release_asset(VSIX_ASSET_NAME).await
        .context("Impossible de trouver le .vsix dans la dernière release")?;

    let tmp_dir = std::env::temp_dir();
    let tmp_vsix = tmp_dir.join("ilocker-studio-download.vsix");
    crate::updater::download_binary(&release.asset, &tmp_vsix).await
        .context("Téléchargement du .vsix échoué")?;

    let status = std::process::Command::new(editor_bin)
        .arg("--install-extension")
        .arg(&tmp_vsix)
        .status()
        .context("Impossible de lancer l'installation de l'extension")?;

    let _ = std::fs::remove_file(&tmp_vsix);

    if !status.success() {
        anyhow::bail!("{editor_bin} --install-extension a échoué");
    }
    Ok(())
}
