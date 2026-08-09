// ============================================================
//  scanner.rs — Détection automatique du projet local
//
//  Utilisé par `iloc deploy` pour comprendre, sans qu'on ait à le
//  préciser : quel framework, si Supabase est impliqué, si le
//  projet est déjà lié à GitHub/Vercel/Supabase localement, et
//  quel fichier d'environnement utiliser.
//
//  Toutes les fonctions ici sont pures ou quasi-pures (lecture
//  disque uniquement, aucun appel réseau) — c'est le scanner.rs,
//  pas le reconciler. La décision "adopter ou créer" se prend
//  dans commands/deploy.rs, qui croise ce scan avec des appels API.
// ============================================================

use anyhow::Result;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct ProjectScan {
    /// Nom du projet : "name" de package.json si présent et valide,
    /// sinon le nom du dossier, toujours normalisé (minuscules,
    /// alphanumériques + tirets) pour être valide à la fois comme
    /// nom de repo GitHub et comme nom de projet Vercel.
    pub project_name: String,
    /// Framework détecté depuis les dépendances de package.json.
    pub framework: Option<String>,
    /// (owner, repo) si `.git` existe avec un remote "origin" GitHub.
    pub git_remote: Option<(String, String)>,
    /// Branche par défaut du repo git local, si détectable.
    pub git_default_branch: Option<String>,
    /// true si `.git` existe (même sans remote configuré).
    pub has_git: bool,
    /// project_id Vercel si `.vercel/project.json` existe déjà.
    pub vercel_linked: Option<String>,
    /// project_ref Supabase si `.supabase/project.json` existe déjà.
    pub supabase_linked: Option<String>,
    /// true si le code utilise Supabase (dépendance npm ou dossier
    /// supabase/ présent) — signal pour proposer le provider même
    /// si rien n'est encore lié.
    pub uses_supabase: bool,
    /// Dossier de migrations Supabase, si non vide.
    pub supabase_migrations_dir: Option<PathBuf>,
    /// Fichier d'environnement à utiliser pour la synchronisation
    /// des variables : .env.local > .env.production > .env, dans
    /// cet ordre de préférence (le premier trouvé gagne).
    pub env_file: Option<PathBuf>,
}

/// Normalise un nom pour qu'il soit valide comme nom de repo GitHub
/// ET comme nom de projet Vercel (les deux acceptent : minuscules,
/// chiffres, tirets — Vercel est le plus strict des deux).
fn normalize_name(raw: &str) -> String {
    let lower = raw.to_lowercase();
    let mut out = String::with_capacity(lower.len());
    let mut last_was_dash = false;
    for c in lower.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
            last_was_dash = false;
        } else if !last_was_dash && !out.is_empty() {
            out.push('-');
            last_was_dash = true;
        }
    }
    while out.ends_with('-') { out.pop(); }
    if out.is_empty() { "mon-projet".to_string() } else { out }
}

/// Parse une URL de remote git (SSH ou HTTPS) en (owner, repo).
/// Version compacte dédiée au scanner — la logique complète avec
/// gestion d'erreurs riches vit dans commands/github.rs; celle-ci
/// retourne simplement None en cas d'échec, ce qui est le bon
/// comportement pour un scan best-effort.
fn parse_git_remote(url: &str) -> Option<(String, String)> {
    let cleaned = url.trim().trim_end_matches(".git").trim_end_matches('/');
    if let Some(rest) = cleaned.strip_prefix("git@github.com:") {
        let parts: Vec<&str> = rest.splitn(2, '/').collect();
        if parts.len() == 2 { return Some((parts[0].to_string(), parts[1].to_string())); }
    }
    for prefix in &["https://github.com/", "http://github.com/"] {
        if let Some(rest) = cleaned.strip_prefix(prefix) {
            let parts: Vec<&str> = rest.splitn(2, '/').collect();
            if parts.len() == 2 { return Some((parts[0].to_string(), parts[1].to_string())); }
        }
    }
    None
}

fn read_package_json(dir: &Path) -> Option<serde_json::Value> {
    let raw = std::fs::read_to_string(dir.join("package.json")).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Détecte le framework depuis les dépendances déclarées. L'ordre
/// est important : des frameworks comme Next.js dépendent de React,
/// donc on doit tester les plus spécifiques avant les plus génériques.
fn detect_framework(pkg: &serde_json::Value) -> Option<String> {
    let mut deps = serde_json::Map::new();
    if let Some(d) = pkg["dependencies"].as_object() { deps.extend(d.clone()); }
    if let Some(d) = pkg["devDependencies"].as_object() { deps.extend(d.clone()); }

    let has = |name: &str| deps.contains_key(name);

    if has("next")               { return Some("nextjs".to_string()); }
    if has("nuxt") || has("nuxt3") { return Some("nuxtjs".to_string()); }
    if has("@remix-run/react")   { return Some("remix".to_string()); }
    if has("astro")              { return Some("astro".to_string()); }
    if has("gatsby")             { return Some("gatsby".to_string()); }
    if has("@sveltejs/kit")      { return Some("sveltekit".to_string()); }
    if has("svelte")             { return Some("svelte".to_string()); }
    if has("@angular/core")      { return Some("angular".to_string()); }
    if has("vue")                { return Some("vue".to_string()); }
    if has("react")              { return Some("create-react-app".to_string()); }
    None
}

fn detect_uses_supabase(dir: &Path, pkg: &Option<serde_json::Value>) -> bool {
    if let Some(pkg) = pkg {
        let has_dep = pkg["dependencies"].get("@supabase/supabase-js").is_some()
            || pkg["devDependencies"].get("@supabase/supabase-js").is_some();
        if has_dep { return true; }
    }
    dir.join("supabase").join("config.toml").exists()
        || dir.join("supabase").join("migrations").is_dir()
}

fn find_migrations_dir(dir: &Path) -> Option<PathBuf> {
    let candidate = dir.join("supabase").join("migrations");
    if !candidate.is_dir() { return None; }
    let has_sql = std::fs::read_dir(&candidate).ok()?
        .filter_map(|e| e.ok())
        .any(|e| e.path().extension().and_then(|s| s.to_str()) == Some("sql"));
    if has_sql { Some(candidate) } else { None }
}

fn find_env_file(dir: &Path) -> Option<PathBuf> {
    for candidate in &[".env.local", ".env.production", ".env"] {
        let p = dir.join(candidate);
        if p.exists() { return Some(p); }
    }
    None
}

fn read_vercel_link(dir: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(dir.join(".vercel").join("project.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    v["projectId"].as_str().map(|s| s.to_string())
}

fn read_supabase_link(dir: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(dir.join(".supabase").join("project.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    v["projectRef"].as_str().map(|s| s.to_string())
}

fn git_default_branch(dir: &Path) -> Option<String> {
    // Lit .git/HEAD directement plutôt que d'invoquer `git` — évite
    // une dépendance au binaire git pour cette seule lecture, et
    // fonctionne même si git n'est pas dans le PATH.
    let head = std::fs::read_to_string(dir.join(".git").join("HEAD")).ok()?;
    let head = head.trim();
    head.strip_prefix("ref: refs/heads/").map(|b| b.to_string())
}

/// Point d'entrée principal : scanne le dossier donné et retourne
/// tous les signaux détectés. Ne fait aucun appel réseau — c'est
/// délibéré, pour que `iloc deploy --dry-run` puisse s'exécuter
/// instantanément même hors-ligne pour la partie scan.
pub fn scan_project(dir: &Path) -> Result<ProjectScan> {
    let pkg = read_package_json(dir);

    let project_name = pkg.as_ref()
        .and_then(|p| p["name"].as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            dir.file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "mon-projet".to_string())
        });
    let project_name = normalize_name(&project_name);

    let framework = pkg.as_ref().and_then(detect_framework);

    let has_git = dir.join(".git").exists();
    let git_remote = if has_git {
        std::process::Command::new("git")
            .args(["remote", "get-url", "origin"])
            .current_dir(dir)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| parse_git_remote(&String::from_utf8_lossy(&o.stdout)))
    } else {
        None
    };
    let git_default_branch = if has_git { git_default_branch(dir) } else { None };

    let uses_supabase = detect_uses_supabase(dir, &pkg);

    Ok(ProjectScan {
        project_name,
        framework,
        git_remote,
        git_default_branch,
        has_git,
        vercel_linked: read_vercel_link(dir),
        supabase_linked: read_supabase_link(dir),
        uses_supabase,
        supabase_migrations_dir: find_migrations_dir(dir),
        env_file: find_env_file(dir),
    })
}

// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_name_lowercases_and_dashes() {
        assert_eq!(normalize_name("Mon Super Projet"), "mon-super-projet");
        assert_eq!(normalize_name("my_app.js"), "my-app-js");
        assert_eq!(normalize_name("ALREADY-lower-case"), "already-lower-case");
    }

    #[test]
    fn normalize_name_collapses_repeated_separators() {
        assert_eq!(normalize_name("foo___bar"), "foo-bar");
        assert_eq!(normalize_name("foo -- bar"), "foo-bar");
    }

    #[test]
    fn normalize_name_strips_trailing_dash() {
        assert_eq!(normalize_name("mon-projet!!!"), "mon-projet");
    }

    #[test]
    fn normalize_name_empty_input_has_safe_fallback() {
        assert_eq!(normalize_name("---"), "mon-projet");
        assert_eq!(normalize_name(""), "mon-projet");
    }

    #[test]
    fn parses_ssh_and_https_remotes() {
        assert_eq!(
            parse_git_remote("git@github.com:bechir/ilocker.git"),
            Some(("bechir".to_string(), "ilocker".to_string()))
        );
        assert_eq!(
            parse_git_remote("https://github.com/bechir/ilocker.git"),
            Some(("bechir".to_string(), "ilocker".to_string()))
        );
    }

    #[test]
    fn rejects_non_github_remote() {
        assert_eq!(parse_git_remote("https://gitlab.com/bechir/ilocker.git"), None);
    }

    #[test]
    fn detects_nextjs_framework() {
        let pkg = serde_json::json!({
            "dependencies": { "next": "^14.0.0", "react": "^18.0.0" }
        });
        assert_eq!(detect_framework(&pkg), Some("nextjs".to_string()));
    }

    #[test]
    fn detects_plain_react_when_no_meta_framework() {
        let pkg = serde_json::json!({ "dependencies": { "react": "^18.0.0" } });
        assert_eq!(detect_framework(&pkg), Some("create-react-app".to_string()));
    }

    #[test]
    fn no_framework_detected_returns_none() {
        let pkg = serde_json::json!({ "dependencies": { "lodash": "^4.0.0" } });
        assert_eq!(detect_framework(&pkg), None);
    }

    #[test]
    fn scan_on_empty_temp_dir_does_not_panic() {
        let dir = std::env::temp_dir().join(format!("iloc_scan_test_{}", uuid_like()));
        std::fs::create_dir_all(&dir).unwrap();
        let scan = scan_project(&dir).expect("scan doit réussir même sur un dossier vide");
        assert!(!scan.project_name.is_empty());
        assert_eq!(scan.framework, None);
        assert!(!scan.has_git);
        assert!(!scan.uses_supabase);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn scan_detects_supabase_via_migrations_dir() {
        let dir = std::env::temp_dir().join(format!("iloc_scan_test_{}", uuid_like()));
        let migrations = dir.join("supabase").join("migrations");
        std::fs::create_dir_all(&migrations).unwrap();
        std::fs::write(migrations.join("20260101000000_init.sql"), "select 1;").unwrap();

        let scan = scan_project(&dir).unwrap();
        assert!(scan.uses_supabase);
        assert!(scan.supabase_migrations_dir.is_some());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn scan_finds_env_local_before_env() {
        let dir = std::env::temp_dir().join(format!("iloc_scan_test_{}", uuid_like()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(".env"), "A=1").unwrap();
        std::fs::write(dir.join(".env.local"), "A=1").unwrap();

        let scan = scan_project(&dir).unwrap();
        assert_eq!(scan.env_file, Some(dir.join(".env.local")));
        std::fs::remove_dir_all(&dir).ok();
    }

    fn uuid_like() -> String {
        format!("{:x}", std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos())
    }
}
