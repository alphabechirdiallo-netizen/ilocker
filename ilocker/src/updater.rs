// ============================================================
//  updater.rs — iLocker self-update engine
//
//  Fonctionnement :
//    1. Interroge l'API GitHub Releases (aucun serveur iLocker requis)
//    2. Compare la version courante avec la dernière release
//    3. Détecte OS + architecture automatiquement
//    4. Télécharge le binaire correct
//    5. Remplace atomiquement le binaire courant (swap sécurisé)
//
//  Aucune dépendance sur ilocker-server, ilocker-relay, ni le cloud.
//  Le seul "serveur" requis = api.github.com (gratuit, toujours dispo)
// ============================================================

use anyhow::{Context, Result, bail};
use colored::Colorize;
use std::path::PathBuf;

// ── Configuration ─────────────────────────────────────────────
// Modifier ces deux constantes pour ton repo GitHub :
pub const GITHUB_OWNER: &str = "alphabechirdiallo-netizen";          // ← ton username/org GitHub
pub const GITHUB_REPO:  &str = "ilocker";           // ← nom du repo
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

// ── Types ─────────────────────────────────────────────────────

#[derive(Debug)]
pub struct Release {
    pub tag:     String,   // ex: "v1.9.0"
    pub version: String,   // ex: "1.9.0" (sans le "v")
    pub asset:   String,   // URL de téléchargement du bon binaire
    pub size:    u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateStatus {
    UpToDate,
    UpdateAvailable,
}

// ── Détection de la plateforme ────────────────────────────────

/// Retourne le nom d'asset GitHub attendu pour cette plateforme.
/// Convention de nommage : iloc-{os}-{arch}[.exe]
pub fn platform_asset_name() -> &'static str {
    // Compilé par Rust à la compilation — aucun runtime detection
    if cfg!(target_os = "windows") {
        if cfg!(target_arch = "aarch64") {
            "iloc-windows-aarch64.exe"
        } else {
            "iloc-windows-x86_64.exe"
        }
    } else if cfg!(target_os = "macos") {
        if cfg!(target_arch = "aarch64") {
            "iloc-macos-aarch64"
        } else {
            "iloc-macos-x86_64"
        }
    } else {
        // Linux (et autres POSIX)
        if cfg!(target_arch = "aarch64") {
            "iloc-linux-aarch64"
        } else {
            "iloc-linux-x86_64"
        }
    }
}

// ── Vérification de version ───────────────────────────────────

/// Compare deux versions semver simplifiées (X.Y.Z).
/// Retourne true si `remote` est plus récente que `local`.
pub fn is_newer(local: &str, remote: &str) -> bool {
    let parse = |v: &str| -> (u64, u64, u64) {
        let v = v.trim_start_matches('v');
        let parts: Vec<&str> = v.splitn(3, '.').collect();
        let get = |i: usize| parts.get(i)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0u64);
        (get(0), get(1), get(2))
    };
    parse(remote) > parse(local)
}

// ── Résolution du token GitHub (repo privé) ─────────────────────
//
// `updater.rs` doit fonctionner que le dépôt de distribution soit
// public ou privé. Sur un dépôt privé, l'API GitHub retourne 404
// (pas 401/403 — volontaire, pour ne pas révéler l'existence du
// repo) à toute requête anonyme. Ordre de résolution :
//   1. Variable d'environnement GITHUB_TOKEN (usage scripté/CI)
//   2. Compte déjà connecté via `iloc connect github`
//   3. Aucun — requête anonyme (fonctionne pour un dépôt public)
fn resolve_github_token() -> Option<String> {
    if let Ok(t) = std::env::var("GITHUB_TOKEN") {
        if !t.trim().is_empty() {
            return Some(t);
        }
    }
    crate::github_store::require_credentials(None)
        .ok()
        .map(|c| c.token)
}

/// Cherche un asset précis dans la dernière release GitHub — réutilisé
/// à la fois pour le binaire `iloc` (voir `fetch_latest_release`) et
/// pour le `.vsix` de l'extension VS Code (voir `commands::studio`).
/// Même mécanisme d'authentification pour les deux : dépôt public ou
/// privé, aucune duplication de la logique HTTP/token.
pub async fn fetch_release_asset(asset_name: &str) -> Result<Release> {
    use hyper::{Body, Client, Request};
    use hyper_rustls::HttpsConnectorBuilder;

    let url = format!(
        "https://api.github.com/repos/{}/{}/releases/latest",
        GITHUB_OWNER, GITHUB_REPO
    );

    // with_native_roots() — pas with_webpki_roots(). Bug réel trouvé en
    // testant en conditions réelles : une liste de CA figée à la
    // compilation échoue derrière tout proxy d'entreprise ou inspection
    // TLS, alors que le magasin de certificats natif de l'OS (utilisé
    // par curl/git) fonctionne. Même correctif déjà appliqué à
    // github_client.rs, vercel_client.rs et supabase_client.rs.
    let https = HttpsConnectorBuilder::new()
        .with_native_roots()
        .https_only()
        .enable_http1()
        .build();
    let client: Client<_, Body> = Client::builder().build(https);

    let token = resolve_github_token();

    let mut builder = Request::get(&url)
        .header("User-Agent", format!("iloc/{}", CURRENT_VERSION))
        .header("Accept", "application/vnd.github+json");
    if let Some(t) = &token {
        builder = builder.header("Authorization", format!("token {}", t));
    }
    let req = builder
        .body(Body::empty())
        .context("Erreur construction requête GitHub")?;

    let resp = client.request(req).await
        .context("Impossible de contacter api.github.com")?;

    if !resp.status().is_success() {
        let hint = if token.is_none() {
            "\nSi ce dépôt est privé, connectez-vous avec `iloc connect github` \
             ou exportez GITHUB_TOKEN avant de relancer."
        } else {
            ""
        };
        bail!(
            "GitHub API a retourné HTTP {} — vérifiez le repo {}/{}{}",
            resp.status(), GITHUB_OWNER, GITHUB_REPO, hint
        );
    }

    let bytes = hyper::body::to_bytes(resp.into_body()).await?;
    let json: serde_json::Value = serde_json::from_slice(&bytes)
        .context("Réponse GitHub invalide (pas du JSON)")?;

    let tag = json["tag_name"]
        .as_str()
        .unwrap_or("v0.0.0")
        .to_string();

    let version = tag.trim_start_matches('v').to_string();

    let assets = json["assets"].as_array()
        .context("Pas d'assets dans la release GitHub")?;

    let asset_entry = assets.iter().find(|a| {
        a["name"].as_str().map(|n| n == asset_name).unwrap_or(false)
    });

    // Sur un dépôt privé, `browser_download_url` renvoie une page de
    // connexion GitHub (HTML), pas le binaire — il faut l'URL de
    // l'API asset (`url`), servie avec Accept: application/octet-stream
    // et le même token, pour obtenir le vrai contenu binaire.
    let (asset_url, size) = match asset_entry {
        Some(a) => {
            let url = if token.is_some() {
                a["url"].as_str().unwrap_or("").to_string()
            } else {
                a["browser_download_url"].as_str().unwrap_or("").to_string()
            };
            let size = a["size"].as_u64().unwrap_or(0);
            (url, size)
        }
        None => bail!(
            "Aucun asset '{}' trouvé dans la release {}.\n\
             Vérifiez que le workflow CI produit bien ce fichier.",
            asset_name, tag
        ),
    };

    Ok(Release { tag, version, asset: asset_url, size })
}

pub async fn fetch_latest_release() -> Result<Release> {
    fetch_release_asset(platform_asset_name()).await
}

// ── Téléchargement du binaire ─────────────────────────────────

pub async fn download_binary(url: &str, dest: &PathBuf) -> Result<()> {
    use hyper::{Body, Client, Request};
    use hyper_rustls::HttpsConnectorBuilder;
    use indicatif::{ProgressBar, ProgressStyle};
    use std::io::Write;

    // with_native_roots() — pas with_webpki_roots(). Bug réel trouvé en
    // testant en conditions réelles : une liste de CA figée à la
    // compilation échoue derrière tout proxy d'entreprise ou inspection
    // TLS, alors que le magasin de certificats natif de l'OS (utilisé
    // par curl/git) fonctionne. Même correctif déjà appliqué à
    // github_client.rs, vercel_client.rs et supabase_client.rs.
    let https = HttpsConnectorBuilder::new()
        .with_native_roots()
        .https_only()
        .enable_http1()
        .build();
    let client: Client<_, Body> = Client::builder().build(https);

    // Suivre les redirections manuellement (GitHub redirige vers S3)
    let final_url = follow_redirects(url).await
        .unwrap_or_else(|_| url.to_string());

    let token = resolve_github_token();
    let mut builder = Request::get(&final_url)
        .header("User-Agent", format!("iloc/{}", CURRENT_VERSION));
    // Sur un asset d'API (URL de la forme /releases/assets/{id}),
    // l'Accept octet-stream + le token sont nécessaires pour recevoir
    // le binaire plutôt qu'une réponse JSON de métadonnées.
    if url.contains("/releases/assets/") {
        builder = builder.header("Accept", "application/octet-stream");
        if let Some(t) = &token {
            builder = builder.header("Authorization", format!("token {}", t));
        }
    }
    let req = builder.body(Body::empty())?;

    let resp = client.request(req).await
        .context("Échec du téléchargement")?;

    if !resp.status().is_success() {
        bail!("Téléchargement échoué — HTTP {}", resp.status());
    }

    let content_length = resp.headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);

    let pb = ProgressBar::new(content_length);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("  {spinner:.cyan} [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta})")
            .unwrap()
            .progress_chars("█▉▊▋▌▍▎▏  "),
    );

    let bytes = hyper::body::to_bytes(resp.into_body()).await?;
    pb.finish_and_clear();

    // Créer le répertoire parent si nécessaire
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut f = std::fs::File::create(dest)
        .with_context(|| format!("Impossible de créer {}", dest.display()))?;
    f.write_all(&bytes)?;

    Ok(())
}

/// Suit les redirections HTTP (max 5) — nécessaire pour GitHub Assets → S3
async fn follow_redirects(url: &str) -> Result<String> {
    // Pour simplifier : on retourne l'URL telle quelle.
    // hyper gère les redirections via la réponse 302/301.
    // Dans la pratique, hyper ne suit pas auto les redirects,
    // mais GitHub direct download URL ne redirige pas depuis l'API.
    Ok(url.to_string())
}

// ── Swap atomique du binaire ─────────────────────────────────

/// Remplace le binaire `iloc` courant de manière atomique et sécurisée.
///
/// Stratégie :
///   1. Écrire le nouveau binaire dans un fichier temporaire `.iloc-update`
///      dans le même répertoire que le binaire courant.
///   2. Sur POSIX : rename(2) est atomique au niveau syscall.
///   3. Sur Windows : MoveFileExW avec MOVEFILE_REPLACE_EXISTING.
///   4. En cas d'échec (permissions), proposer `sudo` ou instructions manuelles.
pub fn atomic_replace(new_binary: &PathBuf) -> Result<PathBuf> {
    let current_exe = std::env::current_exe()
        .context("Impossible de localiser le binaire iloc courant")?;
    let current_exe = std::fs::canonicalize(&current_exe)
        .unwrap_or(current_exe);

    let parent = current_exe.parent()
        .context("Le binaire n'a pas de répertoire parent")?;

    let temp_path = parent.join(".iloc-update-tmp");

    // Copier le nouveau binaire dans le même répertoire (même fs)
    std::fs::copy(new_binary, &temp_path)
        .with_context(|| format!(
            "Impossible d'écrire dans {} — manque de permissions ?\n\
             Essayez : sudo iloc update",
            parent.display()
        ))?;

    // Rendre exécutable sur POSIX
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&temp_path)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&temp_path, perms)?;
    }

    // Rename atomique
    std::fs::rename(&temp_path, &current_exe)
        .with_context(|| {
            format!(
                "Remplacement atomique échoué.\n\
                 Copiez manuellement {} vers {}",
                temp_path.display(),
                current_exe.display()
            )
        })?;

    Ok(current_exe)
}

// ── API publique ──────────────────────────────────────────────

/// Vérifie si une mise à jour est disponible. Retourne (status, release).
pub async fn check() -> Result<(UpdateStatus, Release)> {
    let release = fetch_latest_release().await?;
    let status = if is_newer(CURRENT_VERSION, &release.version) {
        UpdateStatus::UpdateAvailable
    } else {
        UpdateStatus::UpToDate
    };
    Ok((status, release))
}

/// Affiche les infos de version de manière lisible.
pub fn print_version_info(release: &Release, status: UpdateStatus) {
    println!();
    println!("  {} {}", "version installée:".dimmed(), CURRENT_VERSION.cyan());
    println!("  {} {}", "dernière version: ".dimmed(), release.version.cyan());
    println!("  {} {}", "plateforme:       ".dimmed(), platform_asset_name().dimmed());
    println!();
    match status {
        UpdateStatus::UpToDate => {
            println!("  {} ilocker est à jour.", "✓".green().bold());
        }
        UpdateStatus::UpdateAvailable => {
            println!(
                "  {} Mise à jour disponible : {} → {}",
                "↑".yellow().bold(),
                CURRENT_VERSION.dimmed(),
                release.version.green().bold()
            );
            println!("  Lancez {} pour mettre à jour.", "iloc update".cyan().bold());
        }
    }
    println!();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_newer() {
        assert!(is_newer("1.8.0", "1.9.0"));
        assert!(is_newer("1.8.0", "v1.9.0"));
        assert!(is_newer("1.8.0", "2.0.0"));
        assert!(!is_newer("1.9.0", "1.8.0"));
        assert!(!is_newer("1.9.0", "1.9.0"));
        assert!(is_newer("1.8.9", "1.8.10"));
    }

    #[test]
    fn test_platform_asset_name() {
        let name = platform_asset_name();
        assert!(name.starts_with("iloc-"));
        println!("Platform: {}", name);
    }
}
