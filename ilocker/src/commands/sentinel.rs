// ============================================================
//  commands/sentinel.rs — iloc sentinel <subcommand>
//
//  Le Sentinel intercepte les commandes "dangereuses" AVANT leur
//  exécution dans le shell, déclenche un `iloc save` automatique,
//  puis laisse la commande originale s'exécuter normalement.
//
//  v1.10.0 — Réécriture complète
//  ─────────────────────────────
//  Deux bugs bloquants corrigés :
//    1. Le hook généré (bash ET zsh) contenait une accolade `}`
//       orpheline en fin de fichier (le template ajoutait `}}`
//       en plus de celle déjà présente dans le bloc de patterns),
//       ce qui causait une erreur de syntaxe au `source` — le
//       hook ne pouvait donc JAMAIS être chargé, dans aucun shell.
//    2. Le hook était écrit dans `.ilocker/hooks/` du projet
//       courant, alors qu'il est sourcé depuis ~/.bashrc ou
//       ~/.zshrc — donc utilisé par TOUS les terminaux, pour TOUS
//       les projets. Si ce projet précis était déplacé ou supprimé,
//       Sentinel cassait silencieusement pour l'ensemble de
//       l'écosystème ilocker. Le hook vit désormais dans un seul
//       emplacement global : ~/.ilocker/hooks/.
//
//  Activation par défaut, avec choix laissé à l'utilisateur
//  ──────────────────────────────────────────────────────────
//  `iloc init` active Sentinel automatiquement (silencieusement,
//  idempotent — ne touche pas deux fois aux mêmes rc files) sauf
//  si `--no-sentinel` est passé. Activable/désactivable à tout
//  moment via :
//    iloc sentinel enable
//    iloc sentinel disable
//    iloc sentinel status
//    iloc sentinel uninstall   (désinstallation complète)
//
//  Le hook lui-même reste strictement non-bloquant et best-effort :
//  si la commande `iloc save` échoue (hors d'un projet ilocker,
//  pas encore initialisé...), l'erreur est avalée et la commande
//  originale de l'utilisateur s'exécute normalement, sans jamais
//  être retardée de plus de 500 ms.
//
//  v1.11.0 — Hook PowerShell natif (Windows)
//  ──────────────────────────────────────────
//  PowerShell 5.1+ expose `Set-PSReadLineOption -CommandValidationHandler`
//  (via PSReadLine, inclus par défaut dans Windows PowerShell 5.1 et
//  PowerShell 7+). Ce callback est déclenché AVANT l'exécution de
//  chaque commande interactive — exactement comme `trap DEBUG` en Bash
//  ou `preexec` en Zsh.
//
//  Le hook PowerShell est écrit dans :
//    %USERPROFILE%\.ilocker\hooks\iloc_sentinel.ps1
//
//  Et sourcé depuis le profil PowerShell de l'utilisateur :
//    $PROFILE  (généralement Documents\PowerShell\Microsoft.PowerShell_profile.ps1)
//
//  Comportement identique aux hooks Unix :
//    - Zéro overhead si aucun pattern ne matche
//    - `iloc save` lancé en job background (Start-Job), max 500 ms d'attente
//    - La commande originale s'exécute toujours normalement ensuite
//    - Idempotent : ne patche pas deux fois le même profil
//    - Marqueurs # >>> ilocker sentinel >>> identiques pour cohérence
// ============================================================

use anyhow::{Context, Result};
use colored::Colorize;
use std::path::{Path, PathBuf};

// ── Patterns de commandes risquées ──────────────────────────────
//
// FORMAT: ("<pattern>", "<raison humaine>")
const SENTINEL_PATTERNS: &[(&str, &str)] = &[
    // ── Filesystem ──────────────────────────────────────────
    ("rm -rf",        "recursive force-delete"),
    ("rm -fr",        "recursive force-delete"),
    ("rmdir",         "directory removal"),
    ("shred",         "secure file wipe"),
    ("truncate",      "file truncation"),
    ("find . -delete","find + delete"),
    ("find / -delete","find + delete"),
    // ── Git ─────────────────────────────────────────────────
    ("git clean",            "git working-tree wipe"),
    ("git reset --hard",     "git hard reset"),
    ("git checkout -- .",    "git discard all changes"),
    ("git restore",          "git file restore"),
    ("git rebase",           "git rebase (may rewrite history)"),
    ("git push --force",     "git force-push (peut écraser de l'historique distant)"),
    ("git push -f",          "git force-push (peut écraser de l'historique distant)"),
    ("git branch -D",        "suppression forcée de branche"),
    ("git filter-branch",    "réécriture massive de l'historique"),
    // ── Package managers / environment ───────────────────────
    ("npm run migrate",         "DB migration script"),
    ("npx prisma migrate",      "Prisma DB migration"),
    ("npx sequelize db",        "Sequelize DB operation"),
    ("python manage.py migrate","Django migration"),
    ("alembic upgrade",         "SQLAlchemy migration"),
    ("flask db upgrade",        "Flask-Migrate upgrade"),
    ("rake db:",                "Rails DB task"),
    ("rails db:",               "Rails DB task"),
    // ── Build / clean ────────────────────────────────────────
    ("make clean",     "build clean"),
    ("make distclean", "full build wipe"),
    ("cargo clean",    "Rust build wipe"),
    ("go clean",       "Go build wipe"),
    // ── Database direct access ───────────────────────────────
    ("DROP TABLE",    "SQL DROP TABLE"),
    ("DROP DATABASE", "SQL DROP DATABASE"),
    ("TRUNCATE",       "SQL TRUNCATE"),
    // ── Infra / conteneurs ────────────────────────────────────
    ("docker system prune", "purge Docker (images/volumes/cache)"),
    ("docker volume rm",    "suppression de volume Docker"),
    ("docker-compose down -v", "arrêt + suppression des volumes Compose"),
    ("kubectl delete",      "suppression de ressource Kubernetes"),
    ("terraform destroy",   "destruction d'infrastructure Terraform"),
    // ── Scripting / ops ──────────────────────────────────────
    ("chmod -R 000",  "permission wipe"),
    ("chown -R",      "recursive ownership change"),
    ("dd if=",        "raw disk write"),
    ("mkfs",          "filesystem format"),
    ("fdisk",         "disk partitioning"),
];

const MARK_START: &str = "# >>> ilocker sentinel >>>";
const MARK_END:   &str = "# <<< ilocker sentinel <<<";

// Marqueurs PowerShell — identiques en contenu, commentaire PS1 (#)
const PS_MARK_START: &str = "# >>> ilocker sentinel >>>";
const PS_MARK_END:   &str = "# <<< ilocker sentinel <<<";

#[derive(Debug, Clone, Copy)]
enum Shell { Bash, Zsh }

impl Shell {
    fn rc_filename(self) -> &'static str {
        match self { Shell::Bash => ".bashrc", Shell::Zsh => ".zshrc" }
    }
    fn label(self) -> &'static str {
        match self { Shell::Bash => "Bash", Shell::Zsh => "Zsh" }
    }
}

// ── Emplacement global (un seul, partagé par tous les projets) ──

fn home_dir() -> Result<PathBuf> {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(PathBuf::from)
        .context("Impossible de déterminer le dossier personnel ($HOME)")
}

fn global_hooks_dir() -> Result<PathBuf> {
    Ok(home_dir()?.join(".ilocker").join("hooks"))
}

fn hook_path(shell: Shell) -> Result<PathBuf> {
    Ok(global_hooks_dir()?.join(match shell {
        Shell::Bash => "iloc_sentinel.bash",
        Shell::Zsh  => "iloc_sentinel.zsh",
    }))
}

/// Chemin du hook PowerShell global.
fn hook_path_powershell() -> Result<PathBuf> {
    Ok(global_hooks_dir()?.join("iloc_sentinel.ps1"))
}

/// Détecte le fichier de profil PowerShell actif sur ce système.
/// Ordre de priorité : $PROFILE courant → Documents\PowerShell\… → fallback.
/// Retourne None si PowerShell n'est pas détecté (système non-Windows sans pwsh).
#[cfg(windows)]
fn detect_powershell_profile() -> Option<PathBuf> {
    // Tenter de lire $PROFILE via powershell.exe
    let output = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-Command", "echo $PROFILE"])
        .output()
        .ok()?;
    let profile_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !profile_str.is_empty() {
        return Some(PathBuf::from(profile_str));
    }
    // Fallback : construire le chemin standard manuellement
    let docs = std::env::var("USERPROFILE").ok()?;
    Some(PathBuf::from(docs)
        .join("Documents")
        .join("WindowsPowerShell")
        .join("Microsoft.PowerShell_profile.ps1"))
}

/// Sur Unix, tenter de détecter pwsh (PowerShell 7 cross-platform).
#[cfg(not(windows))]
fn detect_powershell_profile() -> Option<PathBuf> {
    // pwsh place son profil dans ~/.config/powershell/ sur Linux/macOS
    let home = home_dir().ok()?;
    let pwsh_profile = home
        .join(".config")
        .join("powershell")
        .join("Microsoft.PowerShell_profile.ps1");
    // Ne retourner que si pwsh est réellement installé
    if std::process::Command::new("pwsh")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        Some(pwsh_profile)
    } else {
        None
    }
}

/// Écrit (ou met à jour) les fichiers de hook globaux. Idempotent —
/// peut être appelé à chaque `iloc init` sans effet de bord, ce qui
/// garantit que tous les projets bénéficient toujours des derniers
/// patterns surveillés, même après une mise à jour d'ilocker.
fn write_hook_files() -> Result<(PathBuf, PathBuf)> {
    let dir = global_hooks_dir()?;
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("Impossible de créer {}", dir.display()))?;

    let bash_path = hook_path(Shell::Bash)?;
    let zsh_path  = hook_path(Shell::Zsh)?;

    std::fs::write(&bash_path, generate_bash_hook())?;
    std::fs::write(&zsh_path,  generate_zsh_hook())?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o644);
        std::fs::set_permissions(&bash_path, perms.clone())?;
        std::fs::set_permissions(&zsh_path,  perms)?;
    }

    // ── Hook PowerShell (best-effort — ne fait jamais échouer l'installation) ──
    if let Ok(ps_path) = hook_path_powershell() {
        let _ = std::fs::write(&ps_path, generate_powershell_hook());
    }

    Ok((bash_path, zsh_path))
}

/// Détecte les rc files présents sur le système (on patche tous
/// ceux qui existent, pas seulement le shell courant — l'utilisateur
/// peut changer de shell entre deux sessions).
fn detect_present_rc_files() -> Result<Vec<(Shell, PathBuf)>> {
    let home = home_dir()?;
    let mut found = Vec::new();
    for shell in [Shell::Bash, Shell::Zsh] {
        let rc = home.join(shell.rc_filename());
        if rc.exists() {
            found.push((shell, rc));
        }
    }
    Ok(found)
}

/// Ajoute le bloc d'activation (délimité par des marqueurs, donc
/// trivialement détectable et supprimable) à un rc file, de façon
/// strictement idempotente : ne fait rien si déjà présent.
fn patch_rc_file(rc: &Path, shell: Shell) -> Result<bool> {
    let existing = std::fs::read_to_string(rc).unwrap_or_default();
    if existing.contains(MARK_START) {
        return Ok(false);
    }

    let hook = hook_path(shell)?;
    let mut new_content = existing;
    if !new_content.is_empty() && !new_content.ends_with('\n') {
        new_content.push('\n');
    }
    new_content.push('\n');
    new_content.push_str(MARK_START);
    new_content.push('\n');
    new_content.push_str("# ilocker Sentinel — auto-save avant les commandes destructrices\n");
    new_content.push_str(&format!(
        "if [ -f \"{}\" ]; then source \"{}\"; fi\n",
        hook.display(), hook.display()
    ));
    new_content.push_str(MARK_END);
    new_content.push('\n');

    std::fs::write(rc, new_content)
        .with_context(|| format!("Impossible d'écrire {}", rc.display()))?;
    Ok(true)
}

/// Retire le bloc d'activation d'un rc file (ne touche à rien
/// d'autre dans le fichier).
fn unpatch_rc_file(rc: &Path) -> Result<bool> {
    let existing = match std::fs::read_to_string(rc) {
        Ok(c) => c,
        Err(_) => return Ok(false),
    };
    if !existing.contains(MARK_START) {
        return Ok(false);
    }

    let mut out = String::new();
    let mut inside = false;
    for line in existing.lines() {
        if line.trim() == MARK_START { inside = true; continue; }
        if line.trim() == MARK_END   { inside = false; continue; }
        if inside { continue; }
        out.push_str(line);
        out.push('\n');
    }
    std::fs::write(rc, out)?;
    Ok(true)
}

// ── Entry points publics ─────────────────────────────────────────

/// Activation silencieuse, appelée automatiquement par `iloc init`
/// (sauf `--no-sentinel`). Best-effort total : un échec ici ne fait
/// jamais échouer `iloc init`.
pub fn auto_enable_silent() -> Vec<String> {
    let mut report = Vec::new();

    if write_hook_files().is_err() {
        report.push("⚠ impossible d'écrire les hooks Sentinel (permissions ?)".to_string());
        return report;
    }

    match detect_present_rc_files() {
        Ok(rc_files) if !rc_files.is_empty() => {
            for (shell, rc) in rc_files {
                match patch_rc_file(&rc, shell) {
                    Ok(true)  => report.push(format!("activé pour {} ({})", shell.label(), rc.display())),
                    Ok(false) => report.push(format!("déjà actif pour {}", shell.label())),
                    Err(e)    => report.push(format!("⚠ {} : {}", shell.label(), e)),
                }
            }
        }
        _ => report.push("⚠ aucun ~/.bashrc ou ~/.zshrc trouvé — lancez `iloc sentinel enable` manuellement".to_string()),
    }

    // ── PowerShell : activation silencieuse best-effort ───────────
    if let Some(ps_profile) = detect_powershell_profile() {
        match patch_powershell_profile(&ps_profile) {
            Ok(true)  => report.push(format!("activé pour PowerShell ({})", ps_profile.display())),
            Ok(false) => report.push("déjà actif pour PowerShell".to_string()),
            Err(_)    => {} // silencieux — PowerShell est optionnel
        }
    }

    report
}

pub fn run_enable() -> Result<()> {
    let (bash_path, zsh_path) = write_hook_files()?;
    let rc_files = detect_present_rc_files()?;

    if rc_files.is_empty() {
        println!(
            "{} Aucun ~/.bashrc ou ~/.zshrc détecté. Ajoutez manuellement :",
            "⚠".yellow()
        );
        println!("    source \"{}\"   # Bash", bash_path.display());
        println!("    source \"{}\"   # Zsh",  zsh_path.display());
        return Ok(());
    }

    println!();
    for (shell, rc) in &rc_files {
        match patch_rc_file(rc, *shell)? {
            true  => println!("{} {} activé → {}", "✓".green().bold(), shell.label(), rc.display()),
            false => println!("{} {} déjà activé ({})", "●".cyan(), shell.label(), rc.display()),
        }
    }

    // ── PowerShell ────────────────────────────────────────────────
    if let Some(ps_profile) = detect_powershell_profile() {
        match patch_powershell_profile(&ps_profile) {
            Ok(true)  => println!("{} PowerShell activé → {}", "✓".green().bold(), ps_profile.display()),
            Ok(false) => println!("{} PowerShell déjà activé ({})", "●".cyan(), ps_profile.display()),
            Err(e)    => println!("{} PowerShell : {}", "⚠".yellow(), e),
        }
    }

    println!();
    println!("{}", "  Ouvrez un nouveau terminal (ou `source ~/.bashrc` / `source ~/.zshrc`) pour l'activer.".dimmed());
    println!("  {} {} patterns surveillés sur tous vos projets ilocker", "⚡".yellow(), SENTINEL_PATTERNS.len());
    println!();
    Ok(())
}

pub fn run_disable() -> Result<()> {
    let rc_files = detect_present_rc_files()?;
    let mut touched = false;
    for (shell, rc) in &rc_files {
        if unpatch_rc_file(rc)? {
            println!("{} {} désactivé ({})", "✓".green().bold(), shell.label(), rc.display());
            touched = true;
        }
    }

    // ── PowerShell ────────────────────────────────────────────────
    if let Some(ps_profile) = detect_powershell_profile() {
        if unpatch_powershell_profile(&ps_profile)? {
            println!("{} PowerShell désactivé ({})", "✓".green().bold(), ps_profile.display());
            touched = true;
        }
    }

    if !touched {
        println!("{} Sentinel n'était activé dans aucun rc file détecté.", "ℹ".cyan());
    } else {
        println!();
        println!("{}", "  Les hooks restent installés (vous pouvez les réactiver avec `iloc sentinel enable`).".dimmed());
        println!("{}", "  Pour ce terminal déjà ouvert : redémarrez-le pour que ça prenne effet.".dimmed());
    }
    Ok(())
}

pub fn run_init() -> Result<()> {
    // Alias rétro-compatible : équivalent à `enable`, conservé pour
    // ne pas casser les scripts/habitudes qui appelaient déjà
    // `iloc sentinel init`.
    run_enable()
}

pub fn run_status() -> Result<()> {
    println!();
    println!("{}", "  ilocker Sentinel".bold());

    let active_here = std::env::var("ILOC_SENTINEL_ACTIVE").map(|v| v == "1").unwrap_or(false);
    println!(
        "  {} {}",
        "session courante:".dimmed(),
        if active_here { "actif".green().bold().to_string() } else { "inactif".yellow().to_string() }
    );

    let hooks_installed = hook_path(Shell::Bash).map(|p| p.exists()).unwrap_or(false);
    println!(
        "  {} {}",
        "hooks globaux:".dimmed(),
        if hooks_installed {
            format!("installés ({})", global_hooks_dir().map(|p| p.display().to_string()).unwrap_or_default()).green().to_string()
        } else {
            "non installés — lancez `iloc sentinel enable`".yellow().to_string()
        }
    );

    match detect_present_rc_files() {
        Ok(rc_files) if !rc_files.is_empty() => {
            for (shell, rc) in rc_files {
                let content = std::fs::read_to_string(&rc).unwrap_or_default();
                let patched = content.contains(MARK_START);
                println!(
                    "  {} {} {}",
                    format!("{}:", shell.label().to_lowercase()).dimmed(),
                    rc.display(),
                    if patched { "[activé]".green().to_string() } else { "[non activé]".dimmed().to_string() }
                );
            }
        }
        _ => println!("  {} aucun ~/.bashrc ou ~/.zshrc trouvé", "rc files:".dimmed()),
    }

    // ── État PowerShell ───────────────────────────────────────────
    let ps_hook_exists = hook_path_powershell().map(|p| p.exists()).unwrap_or(false);
    if let Some(ps_profile) = detect_powershell_profile() {
        let ps_content = std::fs::read_to_string(&ps_profile).unwrap_or_default();
        let ps_patched = ps_content.contains(PS_MARK_START);
        println!(
            "  {} {} {} {}",
            "powershell:".dimmed(),
            ps_profile.display(),
            if ps_patched { "[activé]".green().to_string() } else { "[non activé]".dimmed().to_string() },
            if ps_hook_exists { "" } else { "(hook manquant — relancez `iloc sentinel enable`)" }
        );
    } else {
        println!("  {} PowerShell non détecté sur ce système", "powershell:".dimmed());
    }

    println!();
    println!("  {} {} commandes risquées surveillées", "patterns:".dimmed(), SENTINEL_PATTERNS.len());
    if !active_here {
        println!();
        println!("{}", "  Astuce : si les hooks sont installés mais inactifs ici, ouvrez un nouveau terminal.".dimmed());
    }
    println!();
    Ok(())
}

pub fn run_uninstall() -> Result<()> {
    let rc_files = detect_present_rc_files()?;
    for (shell, rc) in &rc_files {
        if unpatch_rc_file(rc)? {
            println!("{} bloc retiré de {} ({})", "✓".green(), shell.label(), rc.display());
        }
    }

    // ── PowerShell ────────────────────────────────────────────────
    if let Some(ps_profile) = detect_powershell_profile() {
        if unpatch_powershell_profile(&ps_profile)? {
            println!("{} bloc PowerShell retiré de {}", "✓".green(), ps_profile.display());
        }
    }

    if let Ok(dir) = global_hooks_dir() {
        if dir.exists() {
            std::fs::remove_dir_all(&dir).ok();
            println!("{} hooks globaux supprimés ({})", "✓".green(), dir.display());
        }
    }
    println!();
    println!("{}", "  Sentinel complètement désinstallé. Redémarrez vos terminaux ouverts.".dimmed());
    Ok(())
}

// ── Gestion du profil PowerShell ─────────────────────────────────

/// Ajoute le bloc d'activation du hook Sentinel au profil PowerShell,
/// de façon strictement idempotente (ne fait rien si déjà présent).
fn patch_powershell_profile(profile: &Path) -> Result<bool> {
    // Créer le profil s'il n'existe pas encore (comportement standard de PS)
    if let Some(parent) = profile.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Impossible de créer {}", parent.display()))?;
    }

    let existing = std::fs::read_to_string(profile).unwrap_or_default();
    if existing.contains(PS_MARK_START) {
        return Ok(false);
    }

    let ps_hook = hook_path_powershell()?;
    let mut new_content = existing;
    if !new_content.is_empty() && !new_content.ends_with('\n') {
        new_content.push('\n');
    }
    new_content.push('\n');
    new_content.push_str(PS_MARK_START);
    new_content.push('\n');
    new_content.push_str("# ilocker Sentinel — auto-save avant les commandes destructrices\n");
    new_content.push_str(&format!(
        "if (Test-Path \"{}\") {{ . \"{}\" }}\n",
        ps_hook.display(),
        ps_hook.display()
    ));
    new_content.push_str(PS_MARK_END);
    new_content.push('\n');

    std::fs::write(profile, new_content)
        .with_context(|| format!("Impossible d'écrire {}", profile.display()))?;
    Ok(true)
}

/// Retire le bloc Sentinel du profil PowerShell (ne touche à rien d'autre).
fn unpatch_powershell_profile(profile: &Path) -> Result<bool> {
    let existing = match std::fs::read_to_string(profile) {
        Ok(c) => c,
        Err(_) => return Ok(false),
    };
    if !existing.contains(PS_MARK_START) {
        return Ok(false);
    }

    let mut out = String::new();
    let mut inside = false;
    for line in existing.lines() {
        if line.trim() == PS_MARK_START { inside = true;  continue; }
        if line.trim() == PS_MARK_END   { inside = false; continue; }
        if inside { continue; }
        out.push_str(line);
        out.push('\n');
    }
    std::fs::write(profile, out)?;
    Ok(true)
}

/// Génère le contenu du hook PowerShell.
///
/// Mécanisme : `Set-PSReadLineOption -CommandValidationHandler` est un
/// ScriptBlock exécuté par PSReadLine AVANT chaque commande interactive.
/// Il reçoit l'AST de la ligne de commande et peut inspecter le texte brut.
///
/// Compatibilité :
///   - Windows PowerShell 5.1 (PSReadLine ≥ 2.0 — inclus par défaut)
///   - PowerShell 7+ (pwsh) sur Windows, Linux et macOS
///
/// Fallback : si PSReadLine n'est pas disponible (edge case), le hook
/// s'enregistre via $ExecutionContext.InvokeCommand.PreCommandLookupAction
/// qui est une API interne plus ancienne — moins élégant mais compatible
/// jusqu'à PowerShell 3.0.
fn generate_powershell_hook() -> String {
    let patterns_block = build_patterns_block_powershell();

    format!(
r#"# ============================================================
#  ilocker Sentinel — PowerShell hook (global)
#  Compatible : Windows PowerShell 5.1+ et PowerShell 7+ (pwsh)
#
#  Intercepte chaque commande via PSReadLine CommandValidationHandler.
#  Si elle matche un pattern risqué, déclenche `iloc save` en job
#  background (max 500 ms) avant de laisser la commande s'exécuter.
#  Zéro overhead si aucun pattern ne matche.
# ============================================================

$env:ILOC_SENTINEL_ACTIVE = "1"

# ── Fonction d'auto-save ──────────────────────────────────────────
function __IlocAutoSave {{
    param([string]$Pattern, [string]$Reason, [string]$Cmd)

    $shortCmd = if ($Cmd.Length -gt 60) {{ $Cmd.Substring(0, 60) + "..." }} else {{ $Cmd }}
    Write-Host "  `u{{26A1}} Sentinel: auto-saving before `"$shortCmd`" ($Reason)..." -ForegroundColor Yellow

    # Lancer iloc save en job background
    $job = Start-Job -ScriptBlock {{
        param($msg)
        & iloc save $msg 2>$null
    }} -ArgumentList "[AUTO-SAVE] before: $shortCmd"

    # Attendre max 500 ms
    $waited = 0
    while ($job.State -eq 'Running' -and $waited -lt 10) {{
        Start-Sleep -Milliseconds 50
        $waited++
    }}

    # Nettoyer le job silencieusement
    Remove-Job -Job $job -Force -ErrorAction SilentlyContinue
}}

# ── CommandValidationHandler (PSReadLine) ─────────────────────────
# Appelé AVANT chaque ligne de commande interactive.
# $ast : System.Management.Automation.Language.Ast (AST complet)
# $commandAst : le premier CommandAst dans la ligne
# $fake : si $true, PSReadLine ne va pas exécuter la commande (dry-run)
$__IlocSentinelHandler = {{
    param(
        [System.Management.Automation.Language.CommandAst]$commandAst,
        [bool]$fake
    )

    # Ne pas intercepter les commandes iloc elles-mêmes
    $cmdText = $commandAst.Extent.Text
    if ($cmdText -match '^iloc') {{ return }}
    if ([string]::IsNullOrWhiteSpace($cmdText)) {{ return }}

    # ── Pattern matching ──────────────────────────────────────────
{patterns_block}
}}

# ── Enregistrement du handler ─────────────────────────────────────
# Stratégie : PSReadLine en priorité, fallback sur PreCommandLookupAction
$__IlocSentinelRegistered = $false

if (Get-Module -ListAvailable -Name PSReadLine -ErrorAction SilentlyContinue) {{
    try {{
        Set-PSReadLineOption -CommandValidationHandler $__IlocSentinelHandler -ErrorAction Stop
        $__IlocSentinelRegistered = $true
    }} catch {{
        # PSReadLine disponible mais version trop ancienne — fallback
    }}
}}

if (-not $__IlocSentinelRegistered) {{
    # Fallback : PreCommandLookupAction (PowerShell 3.0+)
    # Moins précis (reçoit le nom de commande, pas l'AST complet)
    # mais garantit une couverture minimale sur les environnements sans PSReadLine.
    $ExecutionContext.InvokeCommand.PreCommandLookupAction = {{
        param([string]$CommandName, [System.Management.Automation.CommandLookupEventArgs]$EventArgs)
        if ($CommandName -match '^iloc') {{ return }}

{fallback_patterns_block}
    }}
    $__IlocSentinelRegistered = $true
}}

# ── Message d'activation (une seule fois par session) ─────────────
if (-not $env:__ILOC_SENTINEL_GREETED) {{
    Write-Host "  `u{{25CF}} ilocker Sentinel active ({count} patterns surveilles)" -ForegroundColor DarkGray
    $env:__ILOC_SENTINEL_GREETED = "1"
}}
"#,
        patterns_block         = patterns_block,
        fallback_patterns_block = build_fallback_patterns_block_powershell(),
        count                  = SENTINEL_PATTERNS.len(),
    )
}

/// Bloc if/elseif pour le CommandValidationHandler PSReadLine.
/// Reçoit $cmdText = texte brut de la ligne de commande.
fn build_patterns_block_powershell() -> String {
    let mut lines = Vec::new();
    lines.push("    # ── Pattern matching ─────────────────────────────────────".to_string());
    for (i, (pattern, reason)) in SENTINEL_PATTERNS.iter().enumerate() {
        let kw = if i == 0 { "    if" } else { "    elseif" };
        lines.push(format!(
            "{} ($cmdText -like '*{}*') {{",
            kw, pattern
        ));
        lines.push(format!(
            "        __IlocAutoSave -Pattern '{}' -Reason '{}' -Cmd $cmdText",
            pattern, reason
        ));
        lines.push("    }".to_string());
    }
    lines.join("\n")
}

/// Bloc if/elseif pour le fallback PreCommandLookupAction.
/// Reçoit $CommandName = nom de la commande seul (sans arguments).
/// Moins précis mais couvre les cas les plus dangereux (rm, git, docker…).
fn build_fallback_patterns_block_powershell() -> String {
    let mut lines = Vec::new();
    lines.push("        # ── Fallback pattern matching (nom de commande uniquement) ──".to_string());
    // Dédupliquer sur le premier mot du pattern pour le fallback
    let mut seen_cmds = std::collections::HashSet::new();
    for (i, (pattern, reason)) in SENTINEL_PATTERNS.iter().enumerate() {
        let first_word = pattern.split_whitespace().next().unwrap_or(pattern);
        if seen_cmds.contains(first_word) { continue; }
        seen_cmds.insert(first_word);

        let kw = if i == 0 || lines.len() == 1 { "        if" } else { "        elseif" };
        lines.push(format!(
            "{} ($CommandName -eq '{}') {{",
            kw, first_word
        ));
        lines.push(format!(
            "            __IlocAutoSave -Pattern '{}' -Reason '{}' -Cmd $CommandName",
            pattern, reason
        ));
        lines.push("        }".to_string());
    }
    lines.join("\n")
}

// ── Générateurs de hook (corrigés : plus de double accolade) ─────

fn generate_bash_hook() -> String {
    format!(
r#"# ============================================================
#  ilocker Sentinel — Bash hook (global, partagé par tous vos
#  projets ilocker — ne dépend d'aucun projet en particulier)
#
#  Intercepte chaque commande via le trap DEBUG. Si elle matche un
#  pattern risqué, déclenche `iloc save` en tâche de fond (max
#  500 ms d'attente) avant de laisser la commande s'exécuter.
#  Zéro overhead si aucun pattern ne matche.
# ============================================================

export ILOC_SENTINEL_ACTIVE=1

__iloc_sentinel_check() {{
    # Garde anti-récursion : le trap DEBUG se déclenche aussi pour
    # les commandes internes de ce hook lui-même.
    [[ -n "$__ILOC_SENTINEL_BUSY" ]] && return 0

    local cmd="$BASH_COMMAND"
    [[ -z "$cmd" ]]                  && return 0
    [[ "$cmd" == __iloc_sentinel* ]] && return 0
    [[ "$cmd" == __iloc_auto_save* ]] && return 0
    [[ "$cmd" == iloc* ]]            && return 0

{patterns_block}
    return 0
}}

# Trigger un auto-save avec un délai souple de 500 ms max.
__iloc_auto_save() {{
    __ILOC_SENTINEL_BUSY=1
    local pattern="$1"
    local reason="$2"
    local short_cmd="${{BASH_COMMAND:0:60}}"

    printf '\033[33m  ⚡ Sentinel: auto-saving before "%s" (%s)…\033[0m\n' \
        "$short_cmd" "$reason" >&2

    iloc save "[AUTO-SAVE] before: $short_cmd" &>/dev/null &
    local save_pid=$!
    local waited=0
    while kill -0 "$save_pid" 2>/dev/null && (( waited < 10 )); do
        sleep 0.05
        (( waited++ ))
    done
    disown "$save_pid" 2>/dev/null || true
    unset __ILOC_SENTINEL_BUSY
}}

# N'installer le trap DEBUG que pour les shells interactifs
if [[ $- == *i* ]]; then
    trap '__iloc_sentinel_check' DEBUG
fi

if [[ -z "$__ILOC_SENTINEL_GREETED" ]]; then
    printf '\033[2m  ● ilocker Sentinel active ({count} patterns surveillés)\033[0m\n'
    export __ILOC_SENTINEL_GREETED=1
fi
"#,
        patterns_block = build_patterns_block_bash(),
        count          = SENTINEL_PATTERNS.len(),
    )
}

fn generate_zsh_hook() -> String {
    format!(
r#"# ============================================================
#  ilocker Sentinel — Zsh hook (global, partagé par tous vos
#  projets ilocker — ne dépend d'aucun projet en particulier)
#
#  Utilise le hook natif `preexec` de Zsh — zéro overhead si la
#  commande ne matche aucun pattern risqué.
# ============================================================

export ILOC_SENTINEL_ACTIVE=1

__iloc_sentinel_preexec() {{
    local cmd="$1"
    [[ -z "$cmd" ]]       && return
    [[ "$cmd" == iloc* ]] && return

{patterns_block}
}}

autoload -Uz add-zsh-hook
add-zsh-hook preexec __iloc_sentinel_preexec

__iloc_auto_save_zsh() {{
    local pattern="$1"
    local reason="$2"
    local full_cmd="$3"
    local short_cmd="${{full_cmd[1,60]}}"

    print -P "%F{{yellow}}  ⚡ Sentinel: auto-saving before \"$short_cmd\" ($reason)…%f" >&2

    iloc save "[AUTO-SAVE] before: $short_cmd" &>/dev/null &
    local save_pid=$!
    local waited=0
    while kill -0 "$save_pid" 2>/dev/null && (( waited < 10 )); do
        sleep 0.05
        (( waited++ ))
    done
    disown "$save_pid" 2>/dev/null || true
}}

if [[ -z "$__ILOC_SENTINEL_GREETED" ]]; then
    print -P '%F{{242}}  ● ilocker Sentinel active ({count} patterns surveillés)%f'
    export __ILOC_SENTINEL_GREETED=1
fi
"#,
        patterns_block = build_patterns_block_zsh(),
        count          = SENTINEL_PATTERNS.len(),
    )
}

/// Bloc if/elif (sans accolade de fermeture — c'est le template
/// appelant qui possède et ferme la fonction).
fn build_patterns_block_bash() -> String {
    let mut lines = Vec::new();
    lines.push("    # ── Pattern matching ─────────────────────────────────".to_string());
    for (i, (pattern, reason)) in SENTINEL_PATTERNS.iter().enumerate() {
        let prefix = if i == 0 { "    if" } else { "    elif" };
        lines.push(format!("{} [[ \"$cmd\" == *\"{}\"* ]]; then", prefix, pattern));
        lines.push(format!("        __iloc_auto_save \"{}\" \"{}\"", pattern, reason));
    }
    lines.push("    fi".to_string());
    lines.join("\n")
}

fn build_patterns_block_zsh() -> String {
    let mut lines = Vec::new();
    lines.push("    # ── Pattern matching ─────────────────────────────────".to_string());
    for (i, (pattern, reason)) in SENTINEL_PATTERNS.iter().enumerate() {
        let prefix = if i == 0 { "    if" } else { "    elif" };
        lines.push(format!("{} [[ \"$cmd\" == *\"{}\"* ]]; then", prefix, pattern));
        lines.push(format!("        __iloc_auto_save_zsh \"{}\" \"{}\" \"$cmd\"", pattern, reason));
    }
    lines.push("    fi".to_string());
    lines.join("\n")
}
