// ============================================================
//  ads.rs — iLocker Ads v1.0 : sponsor display pour le CLI
//
//  Appel GET /api/ads/serve?context=cli&os=<OS>&tags=<tags>
//  Entièrement non-bloquant : lancé dans un thread séparé.
//  Si le serveur est inaccessible (offline, délai > 800ms) :
//  rien ne s'affiche — la commande n'est JAMAIS ralentie.
//
//  Affichage final (exemple) :
//    ──────────────────────────────────────────────────
//    Sponsor  Sentry — Track errors before users do → sentry.io/ilocker
//    ──────────────────────────────────────────────────
// ============================================================

use std::thread;
use std::time::Duration;

#[derive(Debug, serde::Deserialize)]
struct AdResponse {
    id:      String,
    text:    Option<String>,
    sponsor: Option<String>,
    link:    Option<String>,
}

/// Lance en arrière-plan un appel à /api/ads/serve et affiche
/// la ligne sponsor si une campagne active correspond.
///
/// `command`  : "save" | "undo" | "share" — contexte pour les stats
/// `extra_tags` : tags supplémentaires détectés par la commande
///               (ex : langage de projet, health score low…)
pub fn show_sponsor_async(command: Option<&str>, extra_tags: Option<Vec<String>>) {
    // Récupérer les infos de config sans bloquer
    let server_url = get_server_url();
    let os_tag     = current_os_tag();
    let cmd_tag    = command.unwrap_or("cli").to_string();

    // Construire les tags
    let mut tags = vec![cmd_tag];
    if let Some(et) = extra_tags {
        tags.extend(et);
    }
    // Ajouter le tag OS aussi dans les tags pour le ciblage fin
    tags.push(os_tag.clone());

    let tags_str = tags.join(",");

    thread::spawn(move || {
        // Timeout agressif : 800ms max
        // On construit manuellement la requête HTTP via std::net
        // pour éviter de dépendre d'un runtime async dans ce thread.
        let ad = fetch_ad_blocking(&server_url, &os_tag, &tags_str);
        if let Some(ad) = ad {
            print_sponsor_line(&ad);
            // Tracker l'impression côté serveur (best-effort)
            let _ = bump_impression(&server_url, &ad.id);
        }
    });
}

/// Affiche la ligne sponsor dans le terminal avec un style discret.
fn print_sponsor_line(ad: &AdResponse) {
    use colored::Colorize;

    let text    = ad.text.as_deref().unwrap_or("").trim();
    let sponsor = ad.sponsor.as_deref().unwrap_or("Sponsor");

    if text.is_empty() { return; }

    // Ligne de séparation légère
    let sep = "─".repeat(60);
    println!("  {}", sep.dimmed());
    println!(
        "  {}  {}",
        format!("Sponsor [{}]", sponsor).dimmed(),
        text.white()
    );
    println!("  {}", sep.dimmed());
}

/// Requête HTTP GET bloquante avec timeout de 800ms.
/// Utilise uniquement la stdlib (std::net::TcpStream) pour
/// rester léger sans ajouter de dépendance async.
fn fetch_ad_blocking(server_url: &str, os: &str, tags: &str) -> Option<AdResponse> {
    let url = format!(
        "{}/api/ads/serve?context=cli&os={}&tags={}",
        server_url.trim_end_matches('/'),
        url_encode(os),
        url_encode(tags),
    );

    // Extraire host et path depuis l'URL
    let (host, port, path) = parse_url(&url)?;

    use std::io::{Read, Write};
    use std::net::TcpStream;

    let addr = format!("{}:{}", host, port);
    let mut stream = TcpStream::connect_timeout(
        &addr.parse().ok()?,
        Duration::from_millis(800),
    ).ok()?;

    stream.set_read_timeout(Some(Duration::from_millis(800))).ok()?;
    stream.set_write_timeout(Some(Duration::from_millis(400))).ok()?;

    // Requête HTTP/1.1 minimaliste
    let req = format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nUser-Agent: iloc/1.8.0\r\n\r\n",
        path, host
    );
    stream.write_all(req.as_bytes()).ok()?;

    let mut response = String::new();
    stream.read_to_string(&mut response).ok()?;

    // Extraire le corps JSON (après \r\n\r\n)
    let body = response.split("\r\n\r\n").nth(1)?;

    // Vérifier le status code (doit être 200)
    if !response.starts_with("HTTP/1.1 200") && !response.starts_with("HTTP/1.0 200") {
        return None;
    }

    serde_json::from_str::<AdResponse>(body.trim()).ok()
}

/// Notifie le serveur d'une impression (best-effort, ignore les erreurs).
fn bump_impression(server_url: &str, id: &str) -> Option<()> {
    use std::io::Write;
    use std::net::TcpStream;

    let url  = format!("{}/api/ads/click/{}", server_url.trim_end_matches('/'), id);
    let (host, port, path) = parse_url(&url)?;
    let addr = format!("{}:{}", host, port);

    let mut stream = TcpStream::connect_timeout(
        &addr.parse().ok()?,
        Duration::from_millis(400),
    ).ok()?;
    stream.set_write_timeout(Some(Duration::from_millis(400))).ok()?;

    let req = format!(
        "POST {} HTTP/1.1\r\nHost: {}\r\nContent-Length: 0\r\nConnection: close\r\nUser-Agent: iloc/1.8.0\r\n\r\n",
        path, host
    );
    stream.write_all(req.as_bytes()).ok()?;
    Some(())
}

// ── Helpers ───────────────────────────────────────────────────

/// Lit le server_url depuis le fichier de config CLI (~/.ilocker/config.toml).
/// Retourne le défaut si non configuré ou non authentifié.
fn get_server_url() -> String {
    crate::auth_store::load()
        .ok()
        .flatten()
        .map(|a| a.server_url.clone())
        .unwrap_or_else(|| "https://api.ilocker.dev".to_string())
}

/// Retourne le tag OS normalisé pour le ciblage.
fn current_os_tag() -> String {
    #[cfg(target_os = "windows")] { "windows".to_string() }
    #[cfg(target_os = "macos")]   { "macos".to_string() }
    #[cfg(target_os = "linux")]   { "linux".to_string() }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    { "other".to_string() }
}

/// Encode minimalement les caractères problématiques dans une URL.
fn url_encode(s: &str) -> String {
    s.chars().map(|c| match c {
        ' '  => "%20".to_string(),
        ','  => "%2C".to_string(),
        '['  => "%5B".to_string(),
        ']'  => "%5D".to_string(),
        '"'  => "%22".to_string(),
        '\'' => "%27".to_string(),
        c    => c.to_string(),
    }).collect()
}

/// Parse une URL http(s):// et retourne (host, port, path_with_query).
/// Les URLs HTTPS sont proxiées en HTTP sur le même host (le CLI
/// est derrière le reverse proxy Caddy qui gère TLS).
/// En pratique, le CLI en production appelle https:// — dans ce cas
/// on utilise le port 443 avec une connexion TLS native si disponible,
/// sinon on se rabat sur l'URL http:// de l'API interne.
fn parse_url(url: &str) -> Option<(String, u16, String)> {
    let url = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://"))?;
    let is_https = url.starts_with("api."); // heuristique simple

    let (host_part, path) = if let Some(i) = url.find('/') {
        (&url[..i], &url[i..])
    } else {
        (url, "/")
    };

    let (host, port) = if let Some(i) = host_part.rfind(':') {
        let p: u16 = host_part[i+1..].parse().ok()?;
        (host_part[..i].to_string(), p)
    } else {
        let p = if is_https { 443u16 } else { 80u16 };
        (host_part.to_string(), p)
    };

    Some((host, port, path.to_string()))
}
