// ============================================================
//  vault.rs — Coffre-fort externalisé & sauvegarde multi-tier
//  (v1.10.0)
//
//  Problème résolu
//  ────────────────
//  Avant cette version, `.ilocker/snapshots/`, `.ilocker/store/`
//  et `.ilocker/chunks/` vivaient DANS le dossier du projet.
//  Conséquences :
//    • un `git push` sans .gitignore correct envoie tout
//      l'historique versionné sur le dépôt distant (fuite).
//    • le coffre-fort partage le même disque / les mêmes
//      permissions que ce qu'il est censé protéger.
//
//  Solution
//  ────────
//  Le contenu LOURD (snapshots/, store/, chunks/) est déplacé
//  vers un "vault" externe, choisi parmi 4 modes :
//
//    InProject — comportement historique (rétro-compatibilité
//                totale, AUCUN changement pour les projets déjà
//                initialisés avant v1.10.0).
//    Sibling   — dossier voisin du projet, même volume
//                (préserve le CoW / reflink natif).
//    System    — répertoire de données standard de l'OS,
//                indexé par l'UUID du projet.
//    Custom    — chemin choisi explicitement par l'utilisateur
//                (autre disque, NAS, clé USB...).
//
//  Le projet ne garde dans `.ilocker/` qu'un pointeur léger
//  (`vault.json`) : quelques octets, sans donnée sensible,
//  qui indique où vivent réellement les données.
//
//  En complément, ce module pilote 3 tiers de sauvegarde
//  secondaire (stratégie 3-2-1), tous optionnels et cumulables :
//    Tier 2 — miroir(s) local/locaux (2e disque, NAS, USB)
//    Tier 3a — Cloud BYOC existant (iloc push : S3/Backblaze/MinIO)
//    Tier 3b — Hyperscale existant (DHT + erasure coding)
// ============================================================

use anyhow::{bail, Context, Result};
use colored::Colorize;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

pub const POINTER_FILE: &str = "vault.json";

// ── Mode de localisation du vault ───────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VaultMode {
    /// Comportement historique : vault dans .ilocker/ (rétro-compat).
    InProject,
    /// Dossier voisin du projet, même volume — préserve le CoW.
    Sibling,
    /// Répertoire de données standard de l'OS.
    System,
    /// Chemin explicite fourni par l'utilisateur.
    Custom,
}

impl Default for VaultMode {
    fn default() -> Self { VaultMode::InProject }
}

impl VaultMode {
    pub fn label(&self) -> &'static str {
        match self {
            VaultMode::InProject => "in-project (legacy)",
            VaultMode::Sibling   => "sibling (même volume, CoW préservé)",
            VaultMode::System    => "system (répertoire de données OS)",
            VaultMode::Custom    => "custom (chemin personnalisé)",
        }
    }

    /// Parse souple depuis une chaîne CLI : accepte toutes les variantes
    /// raisonnables pour ne jamais frustrer l'utilisateur sur la casse
    /// ou les séparateurs.
    pub fn parse(s: &str) -> Result<Self> {
        match s.to_lowercase().replace('_', "-").as_str() {
            "in-project" | "inproject" | "legacy" | "local"   => Ok(VaultMode::InProject),
            "sibling" | "voisin" | "next-to-project"            => Ok(VaultMode::Sibling),
            "system" | "os" | "standard" | "appdata"            => Ok(VaultMode::System),
            "custom" | "perso" | "manual"                       => Ok(VaultMode::Custom),
            other => bail!(
                "Mode de vault inconnu : '{}'.  Valeurs possibles : in-project, sibling, system, custom",
                other
            ),
        }
    }
}

// ── Cibles de sauvegarde secondaire (Tier 2 — miroirs locaux) ───

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MirrorTarget {
    pub path:    PathBuf,
    pub enabled: bool,
}

// ── Configuration persistée (.ilocker/vault.json) ───────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultConfig {
    #[serde(default)]
    pub mode: VaultMode,
    /// Chemin absolu résolu du vault (où vivent snapshots/store/chunks).
    pub vault_path: PathBuf,
    #[serde(default)]
    pub project_id: String,
    /// Tier 2 — miroirs locaux additionnels (2e disque, NAS, USB...).
    #[serde(default)]
    pub mirrors: Vec<MirrorTarget>,
    /// Tier 3a — déclenche `iloc push` (Cloud BYOC) après chaque save.
    #[serde(default)]
    pub cloud_backup_enabled: bool,
    /// Profil cloud à utiliser pour le Tier 3a (None = profil actif)
    #[serde(default)]
    pub cloud_backup_profile: Option<String>,
    /// Tier 3b — déclenche `iloc hyperscale push` après chaque save.
    #[serde(default)]
    pub hyperscale_backup_enabled: bool,
    /// Ajoute automatiquement `.ilocker/` au .gitignore du projet
    /// (défense en profondeur, même avec un vault externalisé).
    #[serde(default)]
    pub auto_patch_gitignore: bool,
}

impl VaultConfig {
    fn legacy(ilocker_dir: &Path) -> Self {
        // Comportement historique : aucune perte de fonctionnalité,
        // aucune migration forcée pour les projets existants.
        VaultConfig {
            mode: VaultMode::InProject,
            vault_path: ilocker_dir.to_path_buf(),
            project_id: String::new(),
            mirrors: Vec::new(),
            cloud_backup_enabled: false,
            cloud_backup_profile: None,
            hyperscale_backup_enabled: false,
            auto_patch_gitignore: false,
        }
    }
}

pub fn pointer_path(ilocker_dir: &Path) -> PathBuf {
    ilocker_dir.join(POINTER_FILE)
}

/// Charge la configuration de vault d'un projet.  Si aucun pointeur
/// n'existe (projet initialisé avant v1.10.0), retourne la config
/// "legacy" qui pointe vers `.ilocker/` — comportement strictement
/// identique à l'ancien code, donc zéro régression.
pub fn load(ilocker_dir: &Path) -> VaultConfig {
    let p = pointer_path(ilocker_dir);
    fs::read_to_string(&p)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_else(|| VaultConfig::legacy(ilocker_dir))
}

pub fn save(cfg: &VaultConfig, ilocker_dir: &Path) -> Result<()> {
    fs::write(pointer_path(ilocker_dir), serde_json::to_string_pretty(cfg)?)
        .context("Impossible d'écrire .ilocker/vault.json")?;
    Ok(())
}

// ── Chemins de données dérivés (remplacent les anciens
//    `ilocker_dir.join("snapshots"/"store"/"chunks")` partout
//    dans le code — drop-in replacement, même signature d'usage) ──

pub fn snapshots_dir(ilocker_dir: &Path) -> PathBuf {
    load(ilocker_dir).vault_path.join("snapshots")
}

pub fn store_dir(ilocker_dir: &Path) -> PathBuf {
    load(ilocker_dir).vault_path.join("store")
}

pub fn chunks_dir(ilocker_dir: &Path) -> PathBuf {
    load(ilocker_dir).vault_path.join("chunks")
}

// ── Résolution du chemin par défaut selon le mode choisi ────────

pub fn resolve_default_path(
    mode:         VaultMode,
    project_root: &Path,
    project_id:   &str,
    custom:       Option<PathBuf>,
) -> Result<PathBuf> {
    match mode {
        VaultMode::InProject => Ok(project_root.join(".ilocker")),

        VaultMode::Sibling => {
            let parent = project_root.parent().unwrap_or(project_root);
            let name = project_root
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "project".to_string());
            Ok(parent.join(format!(".{}-ilocker-vault", name)))
        }

        VaultMode::System => {
            let base = system_data_dir()?;
            Ok(base.join("vaults").join(project_id))
        }

        VaultMode::Custom => custom.ok_or_else(|| {
            anyhow::anyhow!("Le mode 'custom' nécessite --vault-dir <chemin>")
        }),
    }
}

/// Répertoire de données standard, par OS, sans dépendance externe
/// (binaire standalone : on évite d'ajouter la crate `directories`).
fn system_data_dir() -> Result<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        let local = std::env::var("LOCALAPPDATA")
            .context("Variable d'environnement LOCALAPPDATA introuvable")?;
        Ok(PathBuf::from(local).join("ilocker"))
    }
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var("HOME").context("Variable d'environnement HOME introuvable")?;
        Ok(PathBuf::from(home).join("Library/Application Support/ilocker"))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
            if !xdg.is_empty() {
                return Ok(PathBuf::from(xdg).join("ilocker"));
            }
        }
        let home = std::env::var("HOME").context("Variable d'environnement HOME introuvable")?;
        Ok(PathBuf::from(home).join(".local/share/ilocker"))
    }
}

// ── Création physique du vault (utilisé par `iloc init` / `iloc clone`) ──

/// Crée l'arborescence `snapshots/`, `store/`, `chunks/` dans le vault
/// résolu, puis écrit le pointeur `.ilocker/vault.json`.
pub fn provision(
    ilocker_dir: &Path,
    cfg:         &VaultConfig,
) -> Result<()> {
    fs::create_dir_all(cfg.vault_path.join("snapshots"))
        .with_context(|| format!("Impossible de créer {}", cfg.vault_path.join("snapshots").display()))?;
    fs::create_dir_all(cfg.vault_path.join("store"))?;
    fs::create_dir_all(cfg.vault_path.join("chunks"))?;
    save(cfg, ilocker_dir)?;
    Ok(())
}

// ── .gitignore auto-patch (défense en profondeur) ───────────────

/// Ajoute `.ilocker/` au `.gitignore` du projet s'il n'y est pas déjà.
/// N'écrase jamais le fichier — append uniquement, idempotent.
pub fn patch_gitignore(project_root: &Path) -> Result<bool> {
    let path = project_root.join(".gitignore");
    let existing = fs::read_to_string(&path).unwrap_or_default();

    if existing.lines().any(|l| {
        let t = l.trim();
        t == ".ilocker" || t == ".ilocker/" || t == "/.ilocker" || t == "/.ilocker/"
    }) {
        return Ok(false); // déjà présent, rien à faire
    }

    let mut new_content = existing;
    if !new_content.is_empty() && !new_content.ends_with('\n') {
        new_content.push('\n');
    }
    new_content.push_str("\n# ilocker — coffre-fort local (jamais versionné)\n.ilocker/\n");
    fs::write(&path, new_content).context("Impossible d'écrire .gitignore")?;
    Ok(true)
}

// ── Tier 2 : miroir(s) local/locaux ──────────────────────────────

/// Copie récursivement un répertoire de snapshot vers chaque miroir
/// activé.  Best-effort : un miroir indisponible (disque débranché,
/// NAS hors-ligne) ne doit JAMAIS faire échouer `iloc save`.
pub fn mirror_snapshot(cfg: &VaultConfig, snap_id: &str) {
    if cfg.mirrors.is_empty() { return; }

    let src = cfg.vault_path.join("snapshots").join(snap_id);
    if !src.exists() { return; }

    for mirror in &cfg.mirrors {
        if !mirror.enabled { continue; }
        let dest = mirror.path.join("snapshots").join(snap_id);
        match copy_dir_recursive(&src, &dest) {
            Ok(_) => {
                println!(
                    "  {} miroir local synchronisé → {}",
                    "⇄".cyan(),
                    mirror.path.display()
                );
            }
            Err(e) => {
                println!(
                    "  {} miroir {} indisponible ({}) — ignoré, le coffre principal reste intact",
                    "⚠".yellow(),
                    mirror.path.display(),
                    e
                );
            }
        }
    }
}

fn copy_dir_recursive(src: &Path, dest: &Path) -> Result<()> {
    fs::create_dir_all(dest)?;
    for entry in walkdir::WalkDir::new(src).follow_links(false) {
        let entry = entry?;
        let rel = entry.path().strip_prefix(src)?;
        let target = dest.join(rel);
        if entry.file_type().is_dir() {
            fs::create_dir_all(&target)?;
        } else {
            if let Some(parent) = target.parent() { fs::create_dir_all(parent)?; }
            // Si la cible existe déjà et porte le même nom, on évite une
            // copie inutile (les snapshots sont scellés/immuables).
            if !target.exists() {
                let _ = fs::set_permissions(entry.path(), {
                    let mut p = fs::metadata(entry.path())?.permissions();
                    #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; p.set_mode(p.mode() | 0o200); }
                    #[cfg(not(unix))] { p.set_readonly(false); }
                    p
                });
                fs::copy(entry.path(), &target)?;
            }
        }
    }
    Ok(())
}

// ── Orchestration non-bloquante des tiers 2 / 3 après `iloc save` ──

/// Lance, dans un thread séparé (jamais bloquant pour `iloc save`),
/// la synchronisation des tiers de sauvegarde activés :
///   Tier 2  — miroir(s) local/locaux (synchrone, rapide)
///   Tier 3a — Cloud BYOC (`iloc push` existant)
///   Tier 3b — Hyperscale (`iloc hyperscale push` existant)
///
/// Best-effort total : aucune erreur ici ne doit jamais faire
/// échouer la commande `iloc save` qui vient de réussir.
pub fn run_backup_tiers_async(ilocker_dir: PathBuf, snap_id: String) {
    let cfg = load(&ilocker_dir);

    if cfg.mirrors.is_empty() && !cfg.cloud_backup_enabled && !cfg.hyperscale_backup_enabled {
        return;
    }

    std::thread::spawn(move || {
        // Tier 2 — miroirs locaux (pas besoin d'async)
        mirror_snapshot(&cfg, &snap_id);

        // Tier 3a / 3b — nécessitent le runtime tokio existant du
        // projet ; on en crée un dédié et léger dans ce thread,
        // sans toucher au runtime principal de `iloc save`.
        if cfg.cloud_backup_enabled || cfg.hyperscale_backup_enabled {
            let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(rt) => rt,
                Err(_) => return,
            };

            if cfg.cloud_backup_enabled {
                let profile = cfg.cloud_backup_profile.clone();
                rt.block_on(async {
                    if let Err(e) = crate::commands::cloud::run_push(Vec::new(), profile).await {
                        println!(
                            "  {} sauvegarde Cloud BYOC ignorée ({}) — le coffre local reste intact",
                            "⚠".yellow(), e
                        );
                    } else {
                        println!("  {} sauvegarde Cloud BYOC (Tier 3a) terminée", "☁".cyan());
                    }
                });
            }

            if cfg.hyperscale_backup_enabled {
                rt.block_on(async {
                    if let Err(e) = crate::commands::hyperscale::run_push(None, Vec::new()).await {
                        println!(
                            "  {} sauvegarde Hyperscale ignorée ({}) — le coffre local reste intact",
                            "⚠".yellow(), e
                        );
                    } else {
                        println!("  {} sauvegarde Hyperscale (Tier 3b) terminée", "⬡".cyan());
                    }
                });
            }
        }
    });
    // Note : volontairement non-joiné — la commande rend la main
    // immédiatement, sans attendre la fin de la synchronisation.
}

// ── Migration d'un vault existant vers un nouvel emplacement ────

/// Déplace physiquement snapshots/ store/ chunks/ de l'ancien vault
/// vers le nouveau, puis met à jour le pointeur.  Aucune donnée
/// n'est supprimée avant que la copie complète soit vérifiée.
pub fn migrate(
    ilocker_dir:  &Path,
    new_mode:     VaultMode,
    new_path_opt: Option<PathBuf>,
) -> Result<(PathBuf, PathBuf)> {
    let mut old_cfg = load(ilocker_dir);
    let project_root = ilocker_dir
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Impossible de déterminer le dossier du projet"))?;

    let new_path = resolve_default_path(
        new_mode,
        project_root,
        &old_cfg.project_id,
        new_path_opt,
    )?;

    if new_path == old_cfg.vault_path {
        bail!("Le nouveau vault est identique à l'actuel — rien à migrer.");
    }

    let old_path = old_cfg.vault_path.clone();

    // 1. Provisionner la nouvelle arborescence
    fs::create_dir_all(new_path.join("snapshots"))?;
    fs::create_dir_all(new_path.join("store"))?;
    fs::create_dir_all(new_path.join("chunks"))?;

    // 2. Copier intégralement (jamais de move destructeur direct :
    //    si la copie échoue à mi-chemin, l'ancien vault est toujours
    //    intact et le pointeur n'a pas encore changé).
    for sub in ["snapshots", "store", "chunks"] {
        let src = old_path.join(sub);
        if src.exists() {
            copy_dir_recursive(&src, &new_path.join(sub))
                .with_context(|| format!("Échec de la copie de {}", sub))?;
        }
    }

    // 3. Mettre à jour le pointeur seulement après succès complet
    old_cfg.mode = new_mode;
    old_cfg.vault_path = new_path.clone();
    save(&old_cfg, ilocker_dir)?;

    // 4. Nettoyer l'ancien emplacement UNIQUEMENT s'il était dans le
    //    projet (InProject) — sinon on laisse l'ancien vault externe
    //    intact (l'utilisateur peut vouloir le garder en backup manuel).
    if old_path.starts_with(project_root) {
        let _ = fs::remove_dir_all(old_path.join("snapshots"));
        let _ = fs::remove_dir_all(old_path.join("store"));
        let _ = fs::remove_dir_all(old_path.join("chunks"));
    }

    Ok((old_path, new_path))
}

// ── Détection d'orphan (vault déplacé / disque débranché) ───────

#[derive(Debug)]
pub enum VaultHealth {
    Ok,
    Missing(PathBuf),
}

pub fn check_health(ilocker_dir: &Path) -> VaultHealth {
    let cfg = load(ilocker_dir);
    if cfg.vault_path.exists() {
        VaultHealth::Ok
    } else {
        VaultHealth::Missing(cfg.vault_path)
    }
}
