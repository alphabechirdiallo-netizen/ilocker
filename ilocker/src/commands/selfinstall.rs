// ============================================================
//  commands/selfinstall.rs — iloc selfinstall
//
//  Permet au binaire de s'installer lui-même dans le PATH système
//  depuis N'IMPORTE OÙ : dossier Téléchargements, clé USB,
//  fichier reçu via Xender, Bluetooth, email, etc.
//
//  Usage :
//    ./iloc selfinstall              → installe dans /usr/local/bin
//    ./iloc selfinstall --dir ~/.local/bin  → répertoire custom
//    ./iloc selfinstall --check      → vérifie si iloc est dans PATH
//
//  Après installation :
//    iloc init    (fonctionne dans n'importe quel projet)
//    iloc save "msg"
//    ...
//
//  Shells supportés (configure_path_posix) :
//    Bash  → ~/.bashrc / ~/.bash_profile / ~/.profile
//    Zsh   → ~/.zshrc / ~/.zprofile
//    Fish  → fish_add_path (syntaxe native fish, pas export)
//    Autre → ~/.profile (POSIX fallback)
// ============================================================

use anyhow::{Context, Result};
use colored::Colorize;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

pub fn run(target_dir: Option<PathBuf>, check_only: bool, yes: bool) -> Result<()> {
    println!();
    println!("{}", "ilocker — Installation système".bold());
    println!();

    if check_only {
        return check_installation();
    }

    let source = std::env::current_exe()
        .context("Impossible de localiser le binaire iloc courant")?;
    let source = std::fs::canonicalize(&source).unwrap_or(source);

    // ── Choisir le répertoire cible ───────────────────────────
    let install_dir = match target_dir {
        Some(d) => d,
        None    => best_install_dir()?,
    };

    println!(
        "  {} {}",
        "source:        ".dimmed(),
        source.display().to_string().dimmed()
    );
    println!(
        "  {} {}",
        "destination:   ".dimmed(),
        install_dir.display().to_string().cyan()
    );
    println!();

    // ── Créer le dossier si nécessaire ───────────────────────
    if !install_dir.exists() {
        std::fs::create_dir_all(&install_dir)
            .with_context(|| format!(
                "Impossible de créer {}\nEssayez : sudo iloc selfinstall",
                install_dir.display()
            ))?;
        println!("  {} Dossier créé : {}", "→".cyan(), install_dir.display());
    }

    // ── Nom du binaire cible ──────────────────────────────────
    let bin_name = if cfg!(target_os = "windows") { "iloc.exe" } else { "iloc" };
    let dest = install_dir.join(bin_name);

    // ── Vérifier si déjà installé ────────────────────────────
    if dest.exists() {
        let current_installed = std::fs::canonicalize(&dest).unwrap_or_else(|_| dest.clone());
        if current_installed == source {
            println!(
                "  {} iloc est déjà installé dans {}",
                "✓".green().bold(),
                install_dir.display()
            );
            println!();
            return Ok(());
        }
        println!(
            "  {} Une version de iloc existe déjà dans {}",
            "⚠".yellow(),
            install_dir.display()
        );

        if yes {
            println!("  --yes fourni : remplacement sans confirmation.");
        } else if !std::io::stdin().is_terminal() {
            // Pas de TTY : lire stdin ici bloquerait indéfiniment en
            // attendant une frappe qui ne viendra jamais (confirmé par
            // test réel — le processus restait actif plusieurs secondes
            // sans jamais rendre la main). C'est exactement le genre de
            // blocage silencieux à éviter pour une commande pensée pour
            // être lancée depuis n'importe où (USB, Xender, script...).
            println!(
                "  Entrée non-interactive détectée — relancez avec {} pour confirmer,",
                "--yes".cyan()
            );
            println!("  ou exécutez cette commande depuis un terminal interactif.");
            println!("  Annulé (rien n'a été modifié).");
            return Ok(());
        } else {
            print!("  Remplacer ? [Y/n] ");
            use std::io::Write;
            std::io::stdout().flush()?;
            let mut ans = String::new();
            std::io::stdin().read_line(&mut ans)?;
            if ans.trim().eq_ignore_ascii_case("n") {
                println!("  Annulé.");
                return Ok(());
            }
        }
        println!();
    }

    // ── Copier le binaire ─────────────────────────────────────
    std::fs::copy(&source, &dest)
        .with_context(|| format!(
            "Impossible d'écrire dans {}\n\
             Essayez : sudo iloc selfinstall\n\
             Ou :      iloc selfinstall --dir ~/.local/bin",
            dest.display()
        ))?;

    // ── Rendre exécutable sur POSIX ──────────────────────────
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&dest)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&dest, perms)?;
    }

    println!(
        "  {} iloc installé dans {}",
        "✓".green().bold(),
        dest.display().to_string().green()
    );
    println!();

    // ── Configurer le PATH ────────────────────────────────────
    let in_path = is_in_path(&install_dir);
    if !in_path {
        println!("{}", "  Configuration du PATH :".bold());
        configure_path(&install_dir)?;
    } else {
        println!(
            "  {} {} est déjà dans votre PATH.",
            "✓".green(),
            install_dir.display().to_string().dimmed()
        );
    }

    println!();
    println!("{}", "  Installation terminée !".green().bold());
    println!();
    println!("  Commandes disponibles partout :");
    println!("    {}  — initialise un projet",      "iloc init".cyan());
    println!("    {}  — crée un snapshot",           "iloc save \"msg\"".cyan());
    println!("    {}   — historique",                "iloc log".cyan());
    println!("    {}  — partage P2P",                "iloc share".cyan());
    println!("    {} — met à jour iloc",             "iloc update".cyan());
    println!();

    // ── Avertissement redémarrage shell ──────────────────────
    if !in_path {
        println!("{}", "  ⚠  Redémarrez votre terminal pour activer la commande iloc.".yellow());
        println!();
    }

    Ok(())
}

// ── Vérification de l'installation ───────────────────────────

fn check_installation() -> Result<()> {
    // Chercher iloc dans le PATH
    match which_iloc() {
        Some(path) => {
            println!(
                "  {} iloc trouvé dans le PATH :",
                "✓".green().bold()
            );
            println!("    {}", path.display().to_string().cyan());
            println!();

            // Afficher la version
            let output = std::process::Command::new(&path)
                .arg("--version")
                .output();
            if let Ok(out) = output {
                let version = String::from_utf8_lossy(&out.stdout);
                println!("  {} {}", "version:".dimmed(), version.trim().cyan());
            }
        }
        None => {
            println!("  {} iloc n'est pas dans le PATH.", "✗".red());
            println!();
            println!("  Lancez {} pour installer.", "iloc selfinstall".cyan().bold());
        }
    }
    println!();
    Ok(())
}

// ── Détection du meilleur répertoire d'installation ──────────

fn best_install_dir() -> Result<PathBuf> {
    if cfg!(target_os = "windows") {
        // Windows : %LOCALAPPDATA%\ilocker\bin (pas besoin d'admin)
        let local_app_data = std::env::var("LOCALAPPDATA")
            .unwrap_or_else(|_| {
                dirs_home().map(|h| h.join("AppData").join("Local").display().to_string())
                    .unwrap_or_else(|| "C:\\Users\\User\\AppData\\Local".to_string())
            });
        Ok(PathBuf::from(local_app_data).join("ilocker").join("bin"))
    } else {
        // POSIX : essayer /usr/local/bin en premier, sinon ~/.local/bin
        let usr_local_bin = PathBuf::from("/usr/local/bin");
        if is_writable(&usr_local_bin) {
            Ok(usr_local_bin)
        } else {
            // Fallback sans sudo
            let home = dirs_home()
                .context("Impossible de trouver le dossier HOME")?;
            Ok(home.join(".local").join("bin"))
        }
    }
}

fn is_writable(path: &Path) -> bool {
    if !path.exists() { return false; }
    // Tenter de créer un fichier temporaire pour vérifier l'écriture
    let test = path.join(".iloc-write-test");
    match std::fs::File::create(&test) {
        Ok(_) => { let _ = std::fs::remove_file(&test); true }
        Err(_) => false,
    }
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var("HOME").ok().map(PathBuf::from)
        .or_else(|| std::env::var("USERPROFILE").ok().map(PathBuf::from))
}

// ── Gestion du PATH ───────────────────────────────────────────

fn is_in_path(dir: &Path) -> bool {
    if let Ok(path_var) = std::env::var("PATH") {
        for entry in std::env::split_paths(&path_var) {
            if entry == dir { return true; }
        }
    }
    false
}

fn which_iloc() -> Option<PathBuf> {
    let bin_name = if cfg!(target_os = "windows") { "iloc.exe" } else { "iloc" };
    if let Ok(path_var) = std::env::var("PATH") {
        for entry in std::env::split_paths(&path_var) {
            let candidate = entry.join(bin_name);
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }
    None
}

fn configure_path(dir: &PathBuf) -> Result<()> {
    let dir_str = dir.display().to_string();

    if cfg!(target_os = "windows") {
        configure_path_windows(dir)?;
    } else {
        configure_path_posix(&dir_str)?;
    }

    Ok(())
}

#[cfg(unix)]
fn configure_path_posix(dir_str: &str) -> Result<()> {
    use std::io::Write;

    let home = dirs_home().context("HOME introuvable")?;

    // ── Détection du shell courant ───────────────────────────
    let shell = std::env::var("SHELL").unwrap_or_default();

    // ── Fish shell : utilise fish_add_path, pas export ───────
    if shell.contains("fish") {
        return configure_path_fish(dir_str, &home);
    }

    // ── Bash / Zsh / POSIX fallback ──────────────────────────
    let rc_files: Vec<PathBuf> = if shell.contains("zsh") {
        vec![home.join(".zshrc"), home.join(".zprofile")]
    } else {
        // bash ou shell POSIX générique
        vec![home.join(".bashrc"), home.join(".bash_profile"), home.join(".profile")]
    };

    // Trouver le premier fichier rc qui existe
    let rc_file = rc_files.iter()
        .find(|f| f.exists())
        .cloned()
        .unwrap_or_else(|| home.join(".profile"));

    // Vérifier si la ligne existe déjà
    if rc_file.exists() {
        let content = std::fs::read_to_string(&rc_file).unwrap_or_default();
        if content.contains(dir_str) {
            println!(
                "  {} PATH déjà configuré dans {}",
                "✓".green(),
                rc_file.display()
            );
            return Ok(());
        }
    }

    // Ajouter la ligne au fichier rc
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(&rc_file)?;

    writeln!(f, "# ilocker")?;
    writeln!(f, "export PATH=\"{}:$PATH\"", dir_str)?;

    println!(
        "  {} PATH configuré dans {}",
        "✓".green(),
        rc_file.display().to_string().dimmed()
    );
    println!(
        "  {} source {}",
        "  Activez maintenant :".dimmed(),
        rc_file.display().to_string().cyan()
    );

    Ok(())
}

/// Configure le PATH pour Fish shell en utilisant la commande native
/// `fish_add_path` — syntaxe correcte, persistante, idempotente.
///
/// `fish_add_path` est disponible depuis Fish 3.2.0 (2021-03-01).
/// Pour les versions antérieures, on retombe sur `set -gx PATH` dans
/// config.fish, ce qui est également correct sous fish.
#[cfg(unix)]
fn configure_path_fish(dir_str: &str, home: &PathBuf) -> Result<()> {
    use std::io::Write;

    // Tenter fish_add_path d'abord (Fish ≥ 3.2, méthode préférée :
    // idempotente, ajoute au fish_user_paths universel)
    let fish_add_path_ok = std::process::Command::new("fish")
        .args(["-c", &format!("fish_add_path {}", dir_str)])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if fish_add_path_ok {
        println!(
            "  {} PATH configuré via fish_add_path (Fish ≥ 3.2)",
            "✓".green()
        );
        println!("{}", "  Aucun redémarrage requis — actif immédiatement.".dimmed());
        return Ok(());
    }

    // Fallback : écrire dans config.fish avec la syntaxe fish correcte
    // (`set -gx`, PAS `export PATH=...`)
    let config_fish = home.join(".config").join("fish").join("config.fish");

    if let Some(parent) = config_fish.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Vérifier si déjà présent
    if config_fish.exists() {
        let content = std::fs::read_to_string(&config_fish).unwrap_or_default();
        if content.contains(dir_str) {
            println!(
                "  {} PATH déjà configuré dans {}",
                "✓".green(),
                config_fish.display()
            );
            return Ok(());
        }
    }

    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(&config_fish)?;

    // Syntaxe FISH correcte — NE PAS utiliser `export PATH=...`
    writeln!(f, "# ilocker")?;
    writeln!(f, "set -gx PATH {} $PATH", dir_str)?;

    println!(
        "  {} PATH configuré dans {} (syntaxe fish native)",
        "✓".green(),
        config_fish.display().to_string().dimmed()
    );
    println!(
        "  {} source {}",
        "  Activez maintenant :".dimmed(),
        config_fish.display().to_string().cyan()
    );

    Ok(())
}

#[cfg(not(unix))]
fn configure_path_posix(_dir_str: &str) -> Result<()> { Ok(()) }

#[cfg(windows)]
fn configure_path_windows(dir: &PathBuf) -> Result<()> {
    use std::io::Write;

    let dir_str = dir.display().to_string();

    // Modifier le PATH utilisateur via le registre Windows
    // Fallback : afficher les instructions manuelles
    println!("  {} Ajout au PATH utilisateur Windows…", "→".cyan());

    // Tenter via reg.exe (disponible sur tous les Windows modernes)
    let output = std::process::Command::new("reg")
        .args(&[
            "query",
            "HKCU\\Environment",
            "/v", "PATH"
        ])
        .output();

    match output {
        Ok(out) if out.status.success() => {
            let current_path = String::from_utf8_lossy(&out.stdout);
            // Extraire la valeur PATH actuelle
            if let Some(line) = current_path.lines()
                .find(|l| l.trim_start().starts_with("PATH")) {
                let parts: Vec<&str> = line.splitn(3, "    ").collect();
                if parts.len() >= 3 {
                    let existing = parts[2].trim();
                    if !existing.contains(&dir_str) {
                        let new_path = format!("{};{}", dir_str, existing);
                        let _ = std::process::Command::new("reg")
                            .args(&[
                                "add", "HKCU\\Environment",
                                "/v", "PATH",
                                "/t", "REG_EXPAND_SZ",
                                "/d", &new_path,
                                "/f"
                            ])
                            .output();
                        println!(
                            "  {} PATH utilisateur mis à jour.",
                            "✓".green()
                        );
                        println!("  {} Redémarrez votre terminal (ou PowerShell).",
                            "⚠".yellow());
                    }
                }
            }
        }
        _ => {
            // Fallback : instructions manuelles
            println!("  {} Ajoutez manuellement à votre PATH :", "⚠".yellow());
            println!("    {}", dir_str.cyan());
            println!();
            println!("  Via PowerShell (en tant qu'admin) :");
            println!(
                "    {}",
                format!("[Environment]::SetEnvironmentVariable('PATH', '{}' + ';' + $env:PATH, 'User')", dir_str).dimmed()
            );
        }
    }

    Ok(())
}

#[cfg(not(windows))]
fn configure_path_windows(_dir: &PathBuf) -> Result<()> { Ok(()) }
