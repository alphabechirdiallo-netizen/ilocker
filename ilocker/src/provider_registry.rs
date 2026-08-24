// ═══════════════════════════════════════════════════════════════
//  provider_registry.rs — client du registre communautaire de
//  providers (session août 2026)
// ═══════════════════════════════════════════════════════════════
//
// Cohérent avec la philosophie "zéro serveur" d'ilocker : le registre
// n'est PAS un backend dédié que Béchir doit héberger et faire
// tourner. C'est un dépôt Git public (ex: GitHub) contenant :
//   - un fichier `index.json` listant les providers publiés
//   - un fichier `providers/<slug>.toml` par provider
// L'index est simplement lu en HTTP via raw.githubusercontent.com
// (ou GitHub Pages) — exactement le même modèle que `iloc update`
// utilise déjà pour les releases GitHub. Publier revient à ouvrir une
// pull request contre ce dépôt (voir run_publish dans
// commands/provider.rs) ; aucune donnée n'est jamais envoyée à un
// serveur contrôlé par ilocker lui-même.
//
// Sécurité : voir fetch_manifest_bytes — le sha256 annoncé par l'index
// est vérifié après téléchargement, avant toute installation. Sans
// cette vérification, un miroir ou un CDN compromis pourrait
// substituer un manifeste malveillant à un manifeste légitime déjà
// référencé, sans que rien ne le détecte.

use anyhow::{bail, Context, Result};
use hyper::Client as HyperClient;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::Duration;

/// URL par défaut de l'index du registre communautaire. Remplaçable via
/// la variable d'environnement ILOC_PROVIDER_REGISTRY_URL — pratique pour
/// tester contre un registre privé/local pendant le développement.
///
/// Le registre vit comme sous-dossier du dépôt ilocker lui-même
/// (ilocker-registry/), pas dans un dépôt séparé — décision de Béchir
/// (session août 2026) : plus simple à maintenir seul qu'un second dépôt,
/// et cohérent avec la publication officielle du dépôt ilocker (qui doit
/// devenir public pour que raw.githubusercontent.com soit lisible sans
/// authentification par n'importe quel utilisateur d'`iloc provider
/// search/install`).
pub const DEFAULT_REGISTRY_INDEX_URL: &str =
    "https://raw.githubusercontent.com/alphabechirdiallo-netizen/ilocker/main/ilocker-registry/index.json";

/// URL du dépôt contenant le registre (pour la pull request de publication).
pub const DEFAULT_REGISTRY_REPO_URL: &str =
    "https://github.com/alphabechirdiallo-netizen/ilocker";

pub fn registry_index_url() -> String {
    std::env::var("ILOC_PROVIDER_REGISTRY_URL").unwrap_or_else(|_| DEFAULT_REGISTRY_INDEX_URL.to_string())
}

pub fn registry_repo_url() -> String {
    std::env::var("ILOC_PROVIDER_REGISTRY_REPO_URL").unwrap_or_else(|_| DEFAULT_REGISTRY_REPO_URL.to_string())
}

/// Même exception localhost que api.base_url/auth.token_url dans
/// provider_manifest.rs — nécessaire pour développer/tester un registre
/// local avant de le publier, jamais pour une utilisation en production
/// (le défaut codé en dur est déjà en HTTPS ; seul un
/// ILOC_PROVIDER_REGISTRY_URL personnalisé peut pointer vers localhost).
fn require_https_or_localhost(uri: &hyper::Uri, context_label: &str) -> Result<()> {
    let is_https = uri.scheme_str() == Some("https");
    let is_local = matches!(uri.host(), Some("127.0.0.1") | Some("localhost"));
    if !is_https && !is_local {
        bail!(
            "{} doit être en HTTPS (reçu : {}). Exception : http://127.0.0.1 ou http://localhost, \
             pour tester un registre local pendant le développement.",
            context_label, uri
        );
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryEntry {
    pub slug: String,
    pub name: String,
    pub description: String,
    pub author: String,
    pub version: String,
    /// URL HTTPS brute du fichier .toml (ex: raw.githubusercontent.com/...).
    pub manifest_url: String,
    /// sha256 hexadécimal du contenu exact du manifeste — vérifié après
    /// téléchargement, voir fetch_manifest_bytes.
    pub manifest_sha256: String,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryIndex {
    pub version: u32,
    #[serde(default)]
    pub providers: Vec<RegistryEntry>,
}

fn https_client() -> HyperClient<hyper_rustls::HttpsConnector<hyper::client::HttpConnector>> {
    // with_native_roots() — cohérent avec le reste du projet (github/
    // vercel/supabase clients, updater.rs, provider_engine.rs, et les
    // correctifs récents sur s3_client.rs/azure_client.rs/cloud_share.rs).
    let connector = hyper_rustls::HttpsConnectorBuilder::new()
        .with_native_roots()
        .https_or_http()
        .enable_http1()
        .build();
    HyperClient::builder().build(connector)
}

/// Télécharge et parse l'index du registre. Échoue clairement (message
/// explicite) plutôt que de laisser deviner une erreur réseau opaque —
/// même standard que le reste du projet.
pub async fn fetch_index() -> Result<RegistryIndex> {
    let url = registry_index_url();
    let uri: hyper::Uri = url.parse().with_context(|| format!("URL de registre invalide : {}", url))?;
    require_https_or_localhost(&uri, "L'index du registre")?;

    let client = https_client();
    let resp = tokio::time::timeout(Duration::from_secs(15), client.get(uri))
        .await
        .context("Délai dépassé en contactant le registre communautaire")?
        .context("Échec de connexion au registre communautaire")?;

    let status = resp.status();
    let body = hyper::body::to_bytes(resp.into_body())
        .await
        .context("Lecture de la réponse du registre")?;

    if !status.is_success() {
        bail!(
            "Le registre communautaire a répondu HTTP {} — vérifiez votre connexion, ou \
             ILOC_PROVIDER_REGISTRY_URL si vous en avez défini un personnalisé.",
            status
        );
    }

    serde_json::from_slice(&body).context("Index du registre illisible (format JSON inattendu)")
}

/// Filtre insensible à la casse sur slug/nom/description/tags — recherche
/// simple par sous-chaîne, suffisante tant que le registre reste de
/// taille modeste.
pub fn search_index(index: &RegistryIndex, query: &str) -> Vec<RegistryEntry> {
    let q = query.to_lowercase();
    index
        .providers
        .iter()
        .filter(|p| {
            p.slug.to_lowercase().contains(&q)
                || p.name.to_lowercase().contains(&q)
                || p.description.to_lowercase().contains(&q)
                || p.tags.iter().any(|t| t.to_lowercase().contains(&q))
        })
        .cloned()
        .collect()
}

/// Télécharge le manifeste d'une entrée du registre et VÉRIFIE que son
/// sha256 correspond exactement à celui annoncé par l'index avant de le
/// retourner. C'est la seule protection contre un manifeste substitué a
/// posteriori (miroir/CDN compromis, faute de frappe dans une PR, etc.) —
/// l'appelant ne doit jamais écrire ces octets sur disque avant cet appel.
pub async fn fetch_manifest_bytes(entry: &RegistryEntry) -> Result<Vec<u8>> {
    let uri: hyper::Uri = entry
        .manifest_url
        .parse()
        .with_context(|| format!("URL de manifeste invalide dans l'entrée du registre : {}", entry.manifest_url))?;
    require_https_or_localhost(&uri, &format!("Le manifeste '{}'", entry.slug))?;

    let client = https_client();
    let resp = tokio::time::timeout(Duration::from_secs(15), client.get(uri))
        .await
        .context("Délai dépassé en téléchargeant le manifeste")?
        .context("Échec de téléchargement du manifeste")?;

    let status = resp.status();
    let body = hyper::body::to_bytes(resp.into_body())
        .await
        .context("Lecture du manifeste téléchargé")?
        .to_vec();

    if !status.is_success() {
        bail!("Téléchargement du manifeste '{}' échoué — HTTP {}", entry.slug, status);
    }

    let mut hasher = Sha256::new();
    hasher.update(&body);
    let actual_sha256 = hex::encode(hasher.finalize());

    if !actual_sha256.eq_ignore_ascii_case(&entry.manifest_sha256) {
        bail!(
            "Intégrité invalide pour '{}' : le manifeste téléchargé ne correspond pas au sha256 \
             annoncé par le registre (attendu {}, obtenu {}). Installation refusée — signalez ceci \
             au registre, ne réessayez pas sans comprendre pourquoi.",
            entry.slug, entry.manifest_sha256, actual_sha256
        );
    }

    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_index() -> RegistryIndex {
        RegistryIndex {
            version: 1,
            providers: vec![
                RegistryEntry {
                    slug: "stripe-like".into(), name: "StripeLike".into(),
                    description: "Paiements de démonstration".into(), author: "Test".into(),
                    version: "0.1.0".into(), manifest_url: "https://example/stripe-like.toml".into(),
                    manifest_sha256: "0".repeat(64), tags: vec!["payments".into(), "form-encoding".into()],
                },
                RegistryEntry {
                    slug: "linear-like".into(), name: "LinearLike".into(),
                    description: "Suivi de projet GraphQL".into(), author: "Test".into(),
                    version: "0.1.0".into(), manifest_url: "https://example/linear-like.toml".into(),
                    manifest_sha256: "1".repeat(64), tags: vec!["graphql".into(), "project-management".into()],
                },
            ],
        }
    }

    #[test]
    fn search_matches_slug_name_description_and_tags_case_insensitively() {
        let idx = sample_index();
        assert_eq!(search_index(&idx, "STRIPE").len(), 1);
        assert_eq!(search_index(&idx, "graphql").len(), 1);
        assert_eq!(search_index(&idx, "démonstration").len(), 1);
        assert_eq!(search_index(&idx, "project-management").len(), 1);
        assert_eq!(search_index(&idx, "aucune-correspondance-possible").len(), 0);
    }

    #[test]
    fn search_empty_query_matches_everything() {
        assert_eq!(search_index(&sample_index(), "").len(), 2);
    }

    #[test]
    fn registry_urls_use_env_override_when_set() {
        std::env::set_var("ILOC_PROVIDER_REGISTRY_URL", "https://custom.example/index.json");
        assert_eq!(registry_index_url(), "https://custom.example/index.json");
        std::env::remove_var("ILOC_PROVIDER_REGISTRY_URL");
        assert_eq!(registry_index_url(), DEFAULT_REGISTRY_INDEX_URL);
    }

    #[test]
    fn require_https_or_localhost_accepts_https_and_local_rejects_rest() {
        let https: hyper::Uri = "https://example.com/index.json".parse().unwrap();
        let local: hyper::Uri = "http://127.0.0.1:8080/index.json".parse().unwrap();
        let named_local: hyper::Uri = "http://localhost:8080/index.json".parse().unwrap();
        let remote_http: hyper::Uri = "http://example.com/index.json".parse().unwrap();

        assert!(require_https_or_localhost(&https, "x").is_ok());
        assert!(require_https_or_localhost(&local, "x").is_ok());
        assert!(require_https_or_localhost(&named_local, "x").is_ok());
        assert!(require_https_or_localhost(&remote_http, "x").is_err());
    }

    // ── Bout-en-bout contre un vrai serveur HTTP local ────────────────
    // Même infrastructure minimale que provider_engine.rs::spawn_test_server
    // (réécrite ici, privée à ce module — pas de dépendance croisée entre
    // modules de test).

    async fn spawn_registry_test_server<F>(handler: F) -> (String, tokio::task::JoinHandle<()>)
    where
        F: Fn(&str) -> (u16, Vec<u8>) + Send + Sync + 'static,
    {
        use hyper::service::{make_service_fn, service_fn};
        use hyper::{Body, Response, Server};
        use std::convert::Infallible;
        use std::sync::Arc;

        let handler = Arc::new(handler);
        let make_svc = make_service_fn(move |_conn| {
            let handler = handler.clone();
            async move {
                Ok::<_, Infallible>(service_fn(move |req: hyper::Request<Body>| {
                    let handler = handler.clone();
                    async move {
                        let (status, body) = handler(req.uri().path());
                        Ok::<_, Infallible>(
                            Response::builder().status(status).body(Body::from(body)).unwrap(),
                        )
                    }
                }))
            }
        });
        let addr: std::net::SocketAddr = ([127, 0, 0, 1], 0).into();
        let server = Server::bind(&addr).serve(make_svc);
        let bound_addr = server.local_addr();
        let handle = tokio::spawn(async move {
            let _ = server.await;
        });
        (format!("http://{}", bound_addr), handle)
    }

    #[tokio::test]
    async fn fetch_index_end_to_end_against_real_local_server() {
        let (base_url, _h) = spawn_registry_test_server(|_path| {
            (200, br#"{"version": 1, "providers": [{"slug": "x", "name": "X", "description": "d", "author": "a", "version": "1.0.0", "manifest_url": "https://e/x.toml", "manifest_sha256": "abc", "tags": []}]}"#.to_vec())
        }).await;
        std::env::set_var("ILOC_PROVIDER_REGISTRY_URL", format!("{}/index.json", base_url));
        let idx = fetch_index().await.unwrap();
        std::env::remove_var("ILOC_PROVIDER_REGISTRY_URL");
        assert_eq!(idx.providers.len(), 1);
        assert_eq!(idx.providers[0].slug, "x");
    }

    #[tokio::test]
    async fn fetch_manifest_bytes_accepts_matching_sha256() {
        let content = b"contenu de manifeste de test";
        let mut hasher = Sha256::new();
        hasher.update(content);
        let sha256 = hex::encode(hasher.finalize());

        let (base_url, _h) = spawn_registry_test_server({
            let content = content.to_vec();
            move |_path| (200, content.clone())
        }).await;

        let entry = RegistryEntry {
            slug: "x".into(), name: "X".into(), description: "d".into(), author: "a".into(),
            version: "1.0.0".into(), manifest_url: format!("{}/x.toml", base_url),
            manifest_sha256: sha256, tags: vec![],
        };
        let bytes = fetch_manifest_bytes(&entry).await.unwrap();
        assert_eq!(bytes, content);
    }

    #[tokio::test]
    async fn fetch_manifest_bytes_rejects_sha256_mismatch() {
        let (base_url, _h) = spawn_registry_test_server(|_path| (200, b"contenu reel".to_vec())).await;
        let entry = RegistryEntry {
            slug: "x".into(), name: "X".into(), description: "d".into(), author: "a".into(),
            version: "1.0.0".into(), manifest_url: format!("{}/x.toml", base_url),
            manifest_sha256: "0".repeat(64), tags: vec![],
        };
        let err = fetch_manifest_bytes(&entry).await.unwrap_err();
        assert!(err.to_string().contains("Intégrité invalide"), "erreur inattendue : {err}");
    }

    #[tokio::test]
    async fn fetch_manifest_bytes_rejects_non_https_non_local_url() {
        let entry = RegistryEntry {
            slug: "x".into(), name: "X".into(), description: "d".into(), author: "a".into(),
            version: "1.0.0".into(), manifest_url: "http://evil.example/x.toml".into(),
            manifest_sha256: "0".repeat(64), tags: vec![],
        };
        assert!(fetch_manifest_bytes(&entry).await.is_err());
    }
}
