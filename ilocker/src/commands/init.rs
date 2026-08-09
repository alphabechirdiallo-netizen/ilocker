// ============================================================
//  iloc init
//  Creates .ilocker/ (control-plane léger) + le vault de données
//  (snapshots/store/chunks), SQLite database, et la clé unique
//  iloc://<uuid>
//
//  v1.10.0 — Vault externalisé
//  ───────────────────────────
//  Par défaut, le vault est désormais créé en mode "Sibling"
//  (dossier voisin du projet, même volume → CoW préservé), ce qui
//  évite que les snapshots se retrouvent dans l'arbre versionné
//  par Git. Trois autres modes restent disponibles :
//    --vault-mode in-project   (comportement historique)
//    --vault-mode system       (répertoire de données de l'OS)
//    --vault-mode custom --vault-dir <chemin>   (NAS, 2e disque…)
//
//  Sauvegarde secondaire (3-2-1), toutes optionnelles et cumulables :
//    --mirror <chemin>          (répétable — Tier 2, miroir local)
//    --cloud-backup             (Tier 3a — iloc push après chaque save)
//    --hyperscale-backup        (Tier 3b — iloc hyperscale push)
//    --no-gitignore-patch       (désactive l'ajout auto de .ilocker/
//                                au .gitignore — activé par défaut)
// ============================================================

use crate::db;
use crate::utils;
use crate::vault::{self, MirrorTarget, VaultConfig, VaultMode};
use anyhow::{bail, Result};
use colored::Colorize;
use std::fs;
use std::path::PathBuf;

#[derive(Default)]
pub struct InitOptions {
    pub vault_mode:           Option<VaultMode>,
    pub vault_dir:            Option<PathBuf>,
    pub mirrors:              Vec<PathBuf>,
    pub cloud_backup:         bool,
    pub hyperscale_backup:    bool,
    pub no_gitignore_patch:   bool,
    pub sentinel:             bool,
}

pub fn run(opts: InitOptions) -> Result<()> {
    let root = std::env::current_dir()?;
    let ilocker_dir = root.join(".ilocker");

    // Refuse double-initialisation
    if ilocker_dir.exists() {
        bail!(
            "{}",
            "ilocker is already initialised in this directory.".yellow()
        );
    }

    // ── 1. Créer le control-plane local (.ilocker/) ───────────
    // Contient : config.json, iloc.db, vault.json (pointeur léger).
    // AUCUNE donnée lourde n'y est plus écrite par défaut.
    fs::create_dir_all(&ilocker_dir)?;

    // ── 2. Générer la clé du projet ────────────────────────────
    let project_id = utils::new_project_id();
    let key        = format!("iloc://{}", project_id);

    // ── 3. Résoudre & provisionner le vault de données ─────────
    let mode = opts.vault_mode.unwrap_or(VaultMode::Sibling);
    let vault_path = vault::resolve_default_path(mode, &root, &project_id, opts.vault_dir.clone())?;

    let mirrors: Vec<MirrorTarget> = opts.mirrors.iter()
        .map(|p| MirrorTarget { path: p.clone(), enabled: true })
        .collect();

    let vault_cfg = VaultConfig {
        mode,
        vault_path: vault_path.clone(),
        project_id: project_id.clone(),
        mirrors,
        cloud_backup_enabled: opts.cloud_backup,
        cloud_backup_profile: None,
        hyperscale_backup_enabled: opts.hyperscale_backup,
        auto_patch_gitignore: !opts.no_gitignore_patch,
    };

    vault::provision(&ilocker_dir, &vault_cfg)
        .map_err(|e| anyhow::anyhow!(
            "Impossible de créer le vault à {} : {}\n\
             Astuce : essayez `iloc init --vault-mode in-project` pour revenir au mode historique.",
            vault_path.display(), e
        ))?;

    // ── 4. Write config.json ──────────────────────────────────
    let config = serde_json::json!({
        "version":    "0.1.0",
        "project_id": project_id,
        "key":        key,
        "created_at": chrono::Utc::now().to_rfc3339(),
    });
    fs::write(
        ilocker_dir.join("config.json"),
        serde_json::to_string_pretty(&config)?,
    )?;

    // ── 5. Write default .ilockerignore ───────────────────────
    let default_ignore_path = root.join(".ilockerignore");
    if !default_ignore_path.exists() {
        fs::write(
            &default_ignore_path,
            default_ignore_rules(),
        )?;
        println!(
            "  {} {}",
            "created".green(),
            ".ilockerignore (smart defaults)"
        );
    }

    // ── 5b. Auto-patch .gitignore (défense en profondeur) ──────
    if vault_cfg.auto_patch_gitignore {
        match vault::patch_gitignore(&root) {
            Ok(true)  => println!("  {} {}", "updated".green(), ".gitignore (.ilocker/ ajouté)"),
            Ok(false) => {} // déjà présent
            Err(e)    => println!("  {} impossible de mettre à jour .gitignore : {}", "⚠".yellow(), e),
        }
    }

    // ── 6. Initialise SQLite database (reste local, léger) ────
    let db_path = ilocker_dir.join("iloc.db");
    db::init(&db_path)?;

    // ── 6b. Activer le Sentinel par défaut (sauf --no-sentinel) ──
    // Hook global, partagé par tous les projets ilocker de cet
    // utilisateur — best-effort, ne fait jamais échouer `iloc init`.
    let sentinel_report = if opts.sentinel {
        crate::commands::sentinel::auto_enable_silent()
    } else {
        Vec::new()
    };

    // ── 7. Print summary ──────────────────────────────────────
    println!();
    println!(
        "{} ilocker initialised",
        "✓".green().bold()
    );
    println!(
        "  {} {}",
        "key:".dimmed(),
        key.cyan().bold()
    );
    println!(
        "  {} {}",
        "index:".dimmed(),
        ".ilocker/iloc.db"
    );
    println!(
        "  {} {} {}",
        "vault:".dimmed(),
        vault_path.display().to_string().cyan(),
        format!("[{}]", mode.label()).dimmed()
    );
    if !vault_cfg.mirrors.is_empty() {
        println!(
            "  {} {} miroir(s) local/locaux configuré(s)",
            "tier2:".dimmed(),
            vault_cfg.mirrors.len().to_string().yellow()
        );
    }
    if vault_cfg.cloud_backup_enabled {
        println!("  {} activé (Cloud BYOC après chaque save)", "tier3a:".dimmed());
    }
    if vault_cfg.hyperscale_backup_enabled {
        println!("  {} activé (Hyperscale après chaque save)", "tier3b:".dimmed());
    }
    if opts.sentinel {
        for line in &sentinel_report {
            println!("  {} {}", "sentinel:".dimmed(), line);
        }
        if !sentinel_report.is_empty() {
            println!(
                "  {} ouvrez un nouveau terminal pour activer l'auto-save avant les commandes destructrices",
                "⚡".yellow()
            );
        }
    } else {
        println!("  {} désactivé (--no-sentinel) — activable avec `iloc sentinel enable`", "sentinel:".dimmed());
    }
    println!();
    println!(
        "{}",
        "Run `iloc save \"initial snapshot\"` to capture the current state.".dimmed()
    );
    println!(
        "{}",
        "Run `iloc vault status` to inspect the vault at any time.".dimmed()
    );

    Ok(())
}

/// Returns sensible default ignore rules covering the most common project types.
fn default_ignore_rules() -> &'static str {
    r#"# ilocker ignore rules — edit freely
# Syntax: one glob pattern per line; lines starting with # are comments.

# ── Node.js ──────────────────────────────────────────────────
node_modules/
.npm/
.yarn/
dist/
build/
.next/
.nuxt/

# ── Python ───────────────────────────────────────────────────
.venv/
venv/
env/
__pycache__/
*.pyc
*.pyo
.mypy_cache/
.pytest_cache/
dist/
*.egg-info/

# ── Rust ─────────────────────────────────────────────────────
target/

# ── Go ───────────────────────────────────────────────────────
vendor/

# ── General build/cache artefacts ────────────────────────────
.cache/
tmp/
temp/
*.log
*.swp
*.DS_Store
.idea/
.vscode/
*.o
*.a
*.so
*.dylib
*.dll

# ── ilocker itself (always excluded) ─────────────────────────
.ilocker/
"#
}

/// Checks whether the current directory (or any parent) has already been
/// initialised — useful for guard-clauses in other commands.
pub fn assert_initialised() -> Result<std::path::PathBuf> {
    let root = std::env::current_dir()?;
    let ilocker_dir = root.join(".ilocker");
    if !ilocker_dir.exists() {
        bail!(
            "{}  Run `iloc init` first.",
            "Not an ilocker project.".red()
        );
    }
    Ok(ilocker_dir)
}
