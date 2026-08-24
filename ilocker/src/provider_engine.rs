// ============================================================
//  provider_engine.rs — Moteur d'exécution générique des providers
//  déclaratifs.
//
//  Ce fichier est écrit UNE fois et compilé UNE fois dans le
//  binaire ilocker. Chaque nouveau provider (Linear, Stripe,
//  GitLab, un outil interne d'entreprise…) ne touche JAMAIS ce
//  fichier — il ajoute seulement un manifeste TOML (voir
//  provider_manifest.rs) qui est interprété ici au runtime.
//
//  Deux responsabilités distinctes :
//
//  1. Construction dynamique de l'arbre `clap::Command` depuis les
//     opérations du manifeste, via l'API "builder" de clap (PAS le
//     macro `#[derive(Subcommand)]` utilisé partout ailleurs dans
//     ilocker, qui lui reste 100% statique et inchangé). C'est ce
//     qui permet `iloc <slug> <opération> [args]` avec la même
//     aide générée, la même validation de types, la même UX que
//     n'importe quelle commande native — sans qu'une seule ligne
//     n'ait été ajoutée à main.rs pour CE provider précis.
//
//  2. Exécution HTTP générique : construit une requête depuis une
//     Operation + les valeurs d'arguments résolues + les
//     identifiants du profil actif, l'envoie, gère les erreurs.
//     Même squelette que github_client.rs (hyper 0.14 +
//     hyper-rustls, timeout 30s, extraction générique de message
//     d'erreur) mais piloté par la donnée du manifeste au lieu
//     d'être codé en dur pour un service précis.
// ============================================================

use crate::commands::studio_docs::Danger;
use crate::provider_manifest::{ArgLocation, AuthType, BodyEncoding, Operation, Pagination, PaginationStyle, ProviderManifest};
use crate::provider_store;
use anyhow::{bail, Context, Result};
use base64::Engine;
use clap::{Arg, ArgAction, Command};
use hyper::{body::to_bytes, client::HttpConnector, Body, Client, Method, Request};
use hyper_rustls::HttpsConnector;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

type HyperClient = Client<HttpsConnector<HttpConnector>>;

/// clap 4.4.18 (version épinglée de ce projet) exige des `&'static str`
/// pour `Command::new` / `Arg::new` / `.long(...)` — cette version ne
/// fournit pas encore `impl From<String>` sur ces points d'entrée
/// (seulement `From<&'static str>`). On "leake" volontairement ces
/// petites chaînes issues du manifeste : borné par MAX_OPERATIONS et
/// la taille maximale du manifeste (quelques Ko au pire), et pour la
/// durée du PROCESSUS uniquement — `iloc` traite une commande puis se
/// termine, ce n'est jamais un serveur longue durée. C'est le
/// contournement standard pour construire un CLI clap dynamique
/// depuis des données runtime avec cette version de la crate.
fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

// ═══════════════════════════════════════════════════════════════
//  Emplacement disque des manifestes installés
// ═══════════════════════════════════════════════════════════════

fn config_dir() -> Result<PathBuf> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .context("Cannot determine home directory")?;
    Ok(PathBuf::from(home).join(".config").join("ilocker"))
}

pub fn providers_root() -> Result<PathBuf> {
    Ok(config_dir()?.join("providers"))
}

pub fn manifest_path(slug: &str) -> Result<PathBuf> {
    Ok(providers_root()?.join(slug).join("manifest.toml"))
}

/// Charge le manifeste installé pour `slug`, ou `None` si aucun
/// provider de ce nom n'est installé localement.
pub fn load_installed(slug: &str) -> Result<Option<ProviderManifest>> {
    let path = manifest_path(slug)?;
    if !path.exists() {
        return Ok(None);
    }
    Ok(Some(crate::provider_manifest::parse_file(&path)?))
}

/// Liste les slugs de tous les providers installés localement, en
/// balayant `~/.config/ilocker/providers/*/manifest.toml`. Utilisé
/// par `main()` pour décider — AVANT tout parsing clap statique —
/// si le premier argument de la ligne de commande doit être dérouté
/// vers ce moteur dynamique plutôt que vers l'arbre de commandes
/// compilé.
pub fn installed_slugs() -> Vec<String> {
    let Ok(root) = providers_root() else { return Vec::new() };
    let Ok(entries) = std::fs::read_dir(&root) else { return Vec::new() };
    entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .filter(|e| e.path().join("manifest.toml").exists())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect()
}

/// `true` si `slug` correspond à un provider installé — c'est
/// l'unique point de décision utilisé par `main()` pour intercepter
/// une commande dynamique avant `Cli::parse()`. Les slugs réservés
/// sont déjà refusés à l'installation (provider_manifest.rs), donc
/// aucune collision possible avec une commande native.
pub fn is_installed_provider(slug: &str) -> bool {
    manifest_path(slug).map(|p| p.exists()).unwrap_or(false)
}

// ═══════════════════════════════════════════════════════════════
//  Construction dynamique de l'arbre clap::Command
// ═══════════════════════════════════════════════════════════════

#[derive(Default)]
struct TreeNode {
    children: BTreeMap<String, TreeNode>,
    operation: Option<Operation>,
}

fn build_tree(operations: &[Operation]) -> TreeNode {
    let mut root = TreeNode::default();
    for op in operations {
        let mut node = &mut root;
        for segment in &op.path {
            node = node.children.entry(segment.clone()).or_default();
        }
        node.operation = Some(op.clone());
    }
    root
}

fn arg_to_clap(a: &crate::provider_manifest::OperationArg) -> Arg {
    let mut arg = Arg::new(leak(a.id.clone())).help(leak(a.help.clone()));
    if a.positional {
        // Sans .long()/.short(), clap traite l'Arg comme positionnel.
        arg = arg.required(a.required);
    } else {
        arg = arg
            .long(leak(a.long.clone().unwrap_or_else(|| a.id.clone())))
            .required(a.required)
            .action(ArgAction::Set);
    }
    arg
}

fn node_to_command(name: &str, node: &TreeNode) -> Command {
    let mut cmd = Command::new(leak(name.to_string()));
    if let Some(op) = &node.operation {
        cmd = cmd.about(leak(op.summary.clone()));
        if let Some(ex) = &op.example {
            cmd = cmd.after_help(leak(format!("Exemple :\n  {}", ex)));
        }
        for a in &op.args {
            cmd = cmd.arg(arg_to_clap(a));
        }
    }
    if !node.children.is_empty() {
        for (child_name, child_node) in &node.children {
            cmd = cmd.subcommand(node_to_command(child_name, child_node));
        }
        cmd = cmd.subcommand_required(true).arg_required_else_help(true);
    }
    cmd
}

/// Construit l'arbre `clap::Command` complet pour un provider —
/// racine nommée par le slug, sous-commandes imbriquées reflétant
/// exactement les `path` des opérations du manifeste.
pub fn build_command(manifest: &ProviderManifest) -> Command {
    let tree = build_tree(&manifest.operations);
    let mut root = Command::new(leak(manifest.provider.slug.clone()))
        .about(leak(manifest.provider.description.clone()))
        .arg(
            Arg::new("__profile")
                .long("profile")
                .help("Profil à utiliser (sinon le profil actif)")
                .action(ArgAction::Set)
                .global(true),
        )
        .arg(
            Arg::new("__yes")
                .long("yes")
                .short('y')
                .help("Confirme automatiquement les opérations sensibles")
                .action(ArgAction::SetTrue)
                .global(true),
        );

    for (name, node) in &tree.children {
        root = root.subcommand(node_to_command(name, node));
    }
    root.subcommand_required(true).arg_required_else_help(true)
}

/// Redescend l'arbre de sous-commandes retenu par clap jusqu'à la
/// feuille exécutée, et retourne l'Operation correspondante avec les
/// valeurs d'arguments résolues (id → valeur telle que saisie).
fn resolve_invocation<'a>(
    manifest: &'a ProviderManifest,
    matches: &clap::ArgMatches,
) -> Result<(&'a Operation, HashMap<String, String>)> {
    let mut path_taken: Vec<String> = Vec::new();
    let mut current = matches;
    loop {
        match current.subcommand() {
            Some((name, sub)) => {
                path_taken.push(name.to_string());
                current = sub;
            }
            None => break,
        }
    }

    let op = manifest
        .operations
        .iter()
        .find(|o| o.path == path_taken)
        .ok_or_else(|| anyhow::anyhow!("Commande interne introuvable : {}", path_taken.join(".")))?;

    let mut values = HashMap::new();
    for a in &op.args {
        if let Some(v) = current.get_one::<String>(&a.id) {
            values.insert(a.id.clone(), v.clone());
        }
    }
    Ok((op, values))
}

// ═══════════════════════════════════════════════════════════════
//  Client HTTP générique
// ═══════════════════════════════════════════════════════════════

pub struct GenericClient {
    http: HyperClient,
    base_url: String,
    auth_type: AuthType,
    auth_header: Option<String>,
    auth_prefix: String,
    fields: HashMap<String, String>,
    /// Slug + compte : nécessaires uniquement pour les deux variantes
    /// OAuth2 (clé du cache de jeton persistant, voir provider_store.rs) —
    /// inutilisés et sans coût pour les autres types d'authentification.
    slug: String,
    account: String,
    oauth_token_url: Option<String>,
    oauth_scope: Option<String>,
    oauth_jwt_audience: Option<String>,
}

impl GenericClient {
    pub fn new(manifest: &ProviderManifest, creds: &provider_store::ResolvedProviderCredentials) -> Self {
        let connector = hyper_rustls::HttpsConnectorBuilder::new()
            .with_native_roots()
            .https_or_http()
            .enable_http1()
            .build();
        Self {
            http: Client::builder().build(connector),
            base_url: creds.api_url.trim_end_matches('/').to_string(),
            auth_type: manifest.auth.auth_type,
            auth_header: manifest.auth.header.clone(),
            auth_prefix: manifest.auth.value_prefix.clone(),
            fields: creds.fields.clone(),
            slug: manifest.provider.slug.clone(),
            account: creds.account.clone(),
            oauth_token_url: manifest.auth.token_url.clone(),
            oauth_scope: manifest.auth.scope.clone(),
            oauth_jwt_audience: manifest.auth.jwt_audience.clone(),
        }
    }

    /// Construit la valeur du header d'authentification. C'est le
    /// SEUL endroit du moteur qui a le droit d'ajouter un header
    /// piloté par des données utilisateur — jamais un argument
    /// d'opération ne peut en ajouter un autre (voir
    /// provider_manifest.rs : ArgLocation n'a pas de variante
    /// "header", délibérément).
    fn auth_header_pair(&self) -> Result<Option<(String, String)>> {
        match self.auth_type {
            AuthType::None => Ok(None),
            AuthType::BearerToken | AuthType::ApiKey => {
                let token = self.fields.get("token").context("Champ 'token' manquant dans les identifiants")?;
                let header = self.auth_header.clone().unwrap_or_else(|| "Authorization".to_string());
                Ok(Some((header, format!("{}{}", self.auth_prefix, token))))
            }
            AuthType::Basic => {
                let user = self.fields.get("username").context("Champ 'username' manquant")?;
                let pass = self.fields.get("password").context("Champ 'password' manquant")?;
                let raw = format!("{}:{}", user, pass);
                let encoded = base64::engine::general_purpose::STANDARD.encode(raw.as_bytes());
                Ok(Some(("Authorization".to_string(), format!("Basic {}", encoded))))
            }
            AuthType::OAuth2ClientCredentials | AuthType::OAuth2ServiceAccount => {
                // Ces deux variantes exigent un échange de jeton réseau
                // (async) — jamais gérées ici. Tout appelant doit passer
                // par resolve_auth_header(), qui dispatche vers
                // ensure_oauth2_token() pour ces cas précis. Une erreur
                // explicite plutôt qu'un unreachable!() : plus sûr si un
                // futur appel direct venait à contourner resolve_auth_header
                // par erreur (échec propre au lieu d'un panic).
                bail!("Erreur interne : OAuth2 doit passer par resolve_auth_header(), pas auth_header_pair()")
            }
        }
    }

    /// Point d'entrée UNIQUE utilisé par verify()/execute() pour obtenir le
    /// header d'authentification — dispatche vers le chemin synchrone
    /// existant (auth_header_pair, inchangé) pour les types classiques, ou
    /// vers l'échange/rafraîchissement OAuth2 (asynchrone, réseau) pour les
    /// deux nouvelles variantes. Même garantie de sécurité que
    /// auth_header_pair() : c'est encore le SEUL endroit qui ajoute un
    /// header piloté par des données utilisateur/secrets.
    async fn resolve_auth_header(&self) -> Result<Option<(String, String)>> {
        match self.auth_type {
            AuthType::OAuth2ClientCredentials | AuthType::OAuth2ServiceAccount => {
                let token = self.ensure_oauth2_token().await?;
                Ok(Some(("Authorization".to_string(), format!("Bearer {}", token))))
            }
            _ => self.auth_header_pair(),
        }
    }

    /// Renvoie un jeton OAuth2 valide, en réutilisant le cache persistant
    /// (provider_store) tant qu'il n'expire pas dans moins de 60s, sinon en
    /// l'échangeant/le renouvelant via le flux approprié à `auth_type`.
    async fn ensure_oauth2_token(&self) -> Result<String> {
        const EXPIRY_SAFETY_MARGIN_SECS: i64 = 60;
        let now = now_unix()?;

        if let Ok(Some(cached)) = provider_store::load_oauth_cache(&self.slug, &self.account) {
            if cached.expires_at - EXPIRY_SAFETY_MARGIN_SECS > now {
                return Ok(cached.access_token);
            }
        }

        let (access_token, expires_in) = match self.auth_type {
            AuthType::OAuth2ClientCredentials => self.fetch_oauth2_client_credentials_token().await?,
            AuthType::OAuth2ServiceAccount => self.fetch_oauth2_service_account_token().await?,
            _ => bail!("ensure_oauth2_token appelé pour un auth_type non-OAuth2 (bug interne)"),
        };

        let cache = provider_store::OAuthTokenCache {
            access_token: access_token.clone(),
            expires_at: now + expires_in,
        };
        // Best-effort : si le cache ne peut pas être écrit (trousseau ET
        // repli indisponibles), on continue quand même avec le jeton
        // fraîchement obtenu plutôt que d'échouer toute l'opération pour
        // un problème de cache pur.
        let _ = provider_store::save_oauth_cache(&self.slug, &self.account, &cache);

        Ok(access_token)
    }

    /// RFC 6749 §4.4 — grant_type=client_credentials. Toujours en
    /// application/x-www-form-urlencoded : c'est le format imposé par la
    /// spec OAuth2 pour le endpoint de token, indépendamment de
    /// body_encoding (qui ne s'applique qu'aux opérations de l'API elle-même).
    async fn fetch_oauth2_client_credentials_token(&self) -> Result<(String, i64)> {
        let token_url = self.oauth_token_url.as_deref()
            .context("auth.token_url manquant dans le manifeste (devrait être rejeté à la validation)")?;
        let client_id = self.fields.get("client_id").context("Champ 'client_id' manquant dans les identifiants")?;
        let client_secret = self.fields.get("client_secret").context("Champ 'client_secret' manquant dans les identifiants")?;

        let mut form = format!(
            "grant_type=client_credentials&client_id={}&client_secret={}",
            url_encode(client_id), url_encode(client_secret)
        );
        if let Some(scope) = &self.oauth_scope {
            form.push_str(&format!("&scope={}", url_encode(scope)));
        }

        let req = Request::builder()
            .method(Method::POST)
            .uri(token_url)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .header("Accept", "application/json")
            .header("User-Agent", "ilocker-provider-engine/1.0")
            .body(Body::from(form))
            .context("Construction de la requête de token OAuth2")?;

        let (status, body) = self.send(req).await?;
        let v = parse_json_response(status, &body, "POST", "oauth2/token (client_credentials)")?;
        extract_token_response(&v)
    }

    /// RFC 7523 — JWT-bearer avec compte de service (flux GCP). Le JSON de
    /// compte de service fournit client_email/private_key/token_uri ; le
    /// moteur construit et signe (RS256) le JWT lui-même, sans jamais faire
    /// quitter le processus à la clé privée.
    async fn fetch_oauth2_service_account_token(&self) -> Result<(String, i64)> {
        let sa_json = self.fields.get("service_account_json")
            .context("Champ 'service_account_json' manquant dans les identifiants")?;
        let sa: Value = serde_json::from_str(sa_json)
            .context("service_account_json n'est pas un JSON valide")?;

        let client_email = sa.get("client_email").and_then(|x| x.as_str())
            .context("Champ 'client_email' manquant dans service_account_json")?;
        let private_key_pem = sa.get("private_key").and_then(|x| x.as_str())
            .context("Champ 'private_key' manquant dans service_account_json")?;
        let token_uri = sa.get("token_uri").and_then(|x| x.as_str())
            .unwrap_or("https://oauth2.googleapis.com/token");

        let now = now_unix()?;
        let aud = self.oauth_jwt_audience.clone().unwrap_or_else(|| token_uri.to_string());
        let scope = self.oauth_scope.clone().unwrap_or_default();

        let jwt = build_and_sign_jwt(client_email, &scope, &aud, now, private_key_pem)
            .context("Construction/signature du JWT de compte de service")?;

        let form = format!(
            "grant_type={}&assertion={}",
            url_encode("urn:ietf:params:oauth:grant-type:jwt-bearer"),
            url_encode(&jwt),
        );

        let req = Request::builder()
            .method(Method::POST)
            .uri(token_uri)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .header("Accept", "application/json")
            .header("User-Agent", "ilocker-provider-engine/1.0")
            .body(Body::from(form))
            .context("Construction de la requête de token JWT-bearer")?;

        let (status, body) = self.send(req).await?;
        let v = parse_json_response(status, &body, "POST", "oauth2/token (jwt-bearer)")?;
        extract_token_response(&v)
    }

    fn url_for(&self, endpoint: &str) -> String {
        if endpoint.starts_with("http://") || endpoint.starts_with("https://") {
            endpoint.to_string()
        } else {
            format!("{}/{}", self.base_url, endpoint.trim_start_matches('/'))
        }
    }

    async fn send(&self, req: Request<Body>) -> Result<(u16, Vec<u8>)> {
        let resp = tokio::time::timeout(std::time::Duration::from_secs(30), self.http.request(req))
            .await
            .context("Délai dépassé (30s) — le service ne répond pas")?
            .context("Requête échouée")?;
        let status = resp.status().as_u16();
        let body = to_bytes(resp.into_body()).await.context("Lecture du corps de réponse")?.to_vec();
        Ok((status, body))
    }

    /// Appelle `endpoint` (GET, sans authentification requise dans le
    /// cas de `verify_endpoint` — l'auth EST cependant appliquée si le
    /// manifeste en déclare une, exactement comme un appel normal) et
    /// retourne le JSON de réponse. Utilisé par `iloc connect` pour
    /// valider immédiatement des identifiants fraîchement saisis.
    pub async fn verify(&self, endpoint: &str) -> Result<Value> {
        let url = self.url_for(endpoint);
        let mut builder = Request::builder().method(Method::GET).uri(&url);
        if let Some((h, v)) = self.resolve_auth_header().await? {
            builder = builder.header(h, v);
        }
        let req = builder.body(Body::empty()).context("Construction requête de vérification")?;
        let (status, body) = self.send(req).await?;
        parse_json_response(status, &body, "GET", endpoint)
    }

    /// Exécute une opération complète : construit l'URL (substitution
    /// des `{id}` path), les query params, le corps JSON, injecte
    /// l'auth, envoie, parse la réponse.
    pub async fn execute(&self, op: &Operation, values: &HashMap<String, String>) -> Result<Value> {
        match &op.pagination {
            None => self.execute_single_page(op, values).await,
            Some(pagination) => self.execute_paginated(op, values, pagination).await,
        }
    }

    /// Pagination automatique — pilote un argument déjà déclaré (voir
    /// Pagination dans provider_manifest.rs) à travers toutes les pages
    /// disponibles, en réutilisant execute_single_page() SANS AUCUN
    /// changement à sa logique de construction de requête : le curseur/
    /// offset est simplement injecté dans la même carte `values` que
    /// n'importe quel argument fourni par l'utilisateur, exactement comme
    /// s'il l'avait tapé lui-même. Zéro risque pour les opérations qui ne
    /// déclarent pas de pagination : elles ne passent jamais par cette
    /// fonction (voir execute() ci-dessus).
    async fn execute_paginated(
        &self,
        op: &Operation,
        values: &HashMap<String, String>,
        pagination: &Pagination,
    ) -> Result<Value> {
        let mut all_items: Vec<Value> = Vec::new();
        let mut current_values = values.clone();
        let mut pages_fetched: u32 = 0;
        let mut truncated = false;

        // Offset : démarre à la valeur déjà fournie par l'utilisateur pour
        // cet argument si elle est numérique, sinon 0 — permet de composer
        // avec un utilisateur qui voudrait reprendre à un offset précis.
        let mut offset: u64 = match pagination.style {
            PaginationStyle::Offset => {
                let arg_id = pagination.offset_arg.as_deref().unwrap_or("");
                values.get(arg_id).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0)
            }
            PaginationStyle::Cursor => 0,
        };

        loop {
            pages_fetched += 1;
            let page = self.execute_single_page(op, &current_values).await?;

            let items = get_json_path(&page, &pagination.items_field)
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let items_len = items.len();
            all_items.extend(items);

            if pages_fetched >= pagination.max_pages {
                // On ne peut pas savoir s'il restait réellement une suite —
                // on le signale honnêtement via `truncated` plutôt que de
                // laisser croire que la liste est complète.
                truncated = has_more_after(&page, pagination, items_len);
                break;
            }

            match pagination.style {
                PaginationStyle::Cursor => {
                    let arg_id = pagination.cursor_arg.as_deref().unwrap_or("");
                    let next_cursor = pagination
                        .next_cursor_field
                        .as_deref()
                        .and_then(|path| get_json_path(&page, path))
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty());

                    let has_more = match &pagination.has_more_field {
                        Some(path) => get_json_path(&page, path).and_then(|v| v.as_bool()).unwrap_or(false),
                        None => next_cursor.is_some(),
                    };

                    match (has_more, next_cursor) {
                        (true, Some(cursor)) => {
                            current_values.insert(arg_id.to_string(), cursor.to_string());
                        }
                        _ => break,
                    }
                }
                PaginationStyle::Offset => {
                    let page_size = pagination.page_size.unwrap_or(0) as usize;
                    if items_len < page_size {
                        break; // page incomplète = dernière page
                    }
                    offset += page_size as u64;
                    let arg_id = pagination.offset_arg.as_deref().unwrap_or("");
                    current_values.insert(arg_id.to_string(), offset.to_string());
                }
            }
        }

        Ok(serde_json::json!({
            "items": all_items,
            "pages_fetched": pages_fetched,
            "truncated": truncated,
        }))
    }

    async fn execute_single_page(&self, op: &Operation, values: &HashMap<String, String>) -> Result<Value> {
        let mut endpoint = op.endpoint.clone();
        let mut query_pairs: Vec<(String, String)> = Vec::new();
        let mut body_map = serde_json::Map::new();

        for a in &op.args {
            let Some(v) = values.get(&a.id) else { continue };
            match a.location {
                ArgLocation::Path => {
                    endpoint = endpoint.replace(&format!("{{{}}}", a.id), v);
                }
                ArgLocation::Query => {
                    query_pairs.push((a.field_name().to_string(), v.clone()));
                }
                ArgLocation::Body => {
                    body_map.insert(a.field_name().to_string(), Value::String(v.clone()));
                }
            }
        }

        let mut url = self.url_for(&endpoint);
        if !query_pairs.is_empty() {
            let sep = if url.contains('?') { '&' } else { '?' };
            let qs: Vec<String> = query_pairs
                .iter()
                .map(|(k, v)| format!("{}={}", url_encode(k), url_encode(v)))
                .collect();
            url = format!("{}{}{}", url, sep, qs.join("&"));
        }

        let method = match op.method.to_uppercase().as_str() {
            "GET" => Method::GET,
            "POST" => Method::POST,
            "PUT" => Method::PUT,
            "PATCH" => Method::PATCH,
            "DELETE" => Method::DELETE,
            other => bail!("Méthode HTTP non supportée : {}", other),
        };

        let mut builder = Request::builder()
            .method(method.clone())
            .uri(&url)
            .header("User-Agent", "ilocker-provider-engine/1.0")
            .header("Accept", "application/json");

        if let Some((h, v)) = self.resolve_auth_header().await? {
            builder = builder.header(h, v);
        }

        // En-têtes statiques du manifeste — valeurs littérales uniquement,
        // jamais de substitution (voir StaticHeader dans provider_manifest.rs).
        for h in &op.extra_headers {
            builder = builder.header(h.name.as_str(), h.value.as_str());
        }
        // Clé d'idempotence générée par le MOTEUR (UUID v4) — jamais par le
        // manifeste ni par une valeur d'argument utilisateur.
        if let Some(header_name) = &op.idempotency_header {
            builder = builder.header(header_name.as_str(), generate_uuid_v4());
        }

        let body = if op.graphql_query.is_some() {
            builder = builder.header("Content-Type", "application/json");
            let payload = serde_json::json!({
                "query": op.graphql_query.as_deref().unwrap_or_default(),
                "variables": Value::Object(body_map),
            });
            Body::from(serde_json::to_vec(&payload)?)
        } else if matches!(method, Method::POST | Method::PUT | Method::PATCH) {
            match op.body_encoding {
                BodyEncoding::Json => {
                    builder = builder.header("Content-Type", "application/json");
                    Body::from(serde_json::to_vec(&Value::Object(body_map))?)
                }
                BodyEncoding::Form => {
                    builder = builder.header("Content-Type", "application/x-www-form-urlencoded");
                    let form: Vec<String> = body_map
                        .iter()
                        .map(|(k, v)| format!("{}={}", url_encode(k), url_encode(&compact(v))))
                        .collect();
                    Body::from(form.join("&"))
                }
            }
        } else {
            Body::empty()
        };

        let req = builder.body(body).context("Construction de la requête")?;
        let (status, resp_body) = self.send(req).await?;
        parse_json_response(status, &resp_body, op.method.as_str(), &op.path.join("."))
    }
}

/// Extraction générique de message d'erreur — essaie les clés JSON
/// les plus communes à travers les APIs REST (`message`, `error`,
/// `detail`, `errors[0].message`) plutôt qu'un format figé pour un
/// seul service, puisque ce client sert n'importe quel provider.
fn extract_error_message(v: &Value) -> String {
    if let Some(s) = v.get("message").and_then(|x| x.as_str()) {
        return s.to_string();
    }
    if let Some(s) = v.get("error").and_then(|x| x.as_str()) {
        return s.to_string();
    }
    if let Some(s) = v.get("detail").and_then(|x| x.as_str()) {
        return s.to_string();
    }
    if let Some(arr) = v.get("errors").and_then(|x| x.as_array()) {
        if let Some(first) = arr.first() {
            if let Some(s) = first.as_str() {
                return s.to_string();
            }
            if let Some(s) = first.get("message").and_then(|x| x.as_str()) {
                return s.to_string();
            }
        }
    }
    v.to_string()
}

fn parse_json_response(status: u16, body: &[u8], method: &str, path: &str) -> Result<Value> {
    if body.is_empty() && (status == 204 || status == 201 || status == 202) {
        return Ok(Value::Null);
    }
    let v: Value =
        serde_json::from_slice(body).unwrap_or_else(|_| Value::String(String::from_utf8_lossy(body).to_string()));

    if status >= 400 {
        let msg = extract_error_message(&v);
        let hint = match status {
            401 => " — identifiants invalides ou expirés (relancez `iloc connect`)",
            403 => " — permissions insuffisantes",
            404 => " — ressource introuvable",
            409 => " — conflit (la ressource existe peut-être déjà)",
            422 => " — données invalides",
            429 => " — trop de requêtes, réessayez plus tard",
            _ => "",
        };
        bail!("{} {} : HTTP {} — {}{}", method, path, status, msg, hint);
    }
    Ok(v)
}

fn url_encode(s: &str) -> String {
    s.bytes().fold(String::new(), |mut acc, b| {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => acc.push(b as char),
            _ => acc.push_str(&format!("%{:02X}", b)),
        }
        acc
    })
}

/// Navigue un `Value` JSON via un chemin pointé simple (ex:
/// "page_info.end_cursor") — utilisé exclusivement par la pagination
/// automatique pour localiser items_field/next_cursor_field/has_more_field
/// où que le manifeste les ait placés dans la réponse. Volontairement
/// minimal (pas de support de tableau indexé type "a[0].b") : les champs
/// de pagination pointent presque toujours vers un objet, jamais un
/// élément de tableau précis.
fn get_json_path<'a>(v: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').try_fold(v, |acc, seg| acc.get(seg))
}

/// Détermine, après avoir atteint max_pages, si l'API annonçait
/// probablement encore de la donnée au-delà — pour renseigner
/// honnêtement `truncated` dans le résultat plutôt que de laisser croire
/// à une liste complète alors que la limite de sécurité a coupé court.
fn has_more_after(page: &Value, pagination: &Pagination, last_page_items_len: usize) -> bool {
    match pagination.style {
        PaginationStyle::Cursor => match &pagination.has_more_field {
            Some(path) => get_json_path(page, path).and_then(|v| v.as_bool()).unwrap_or(false),
            None => pagination
                .next_cursor_field
                .as_deref()
                .and_then(|path| get_json_path(page, path))
                .and_then(|v| v.as_str())
                .map(|s| !s.is_empty())
                .unwrap_or(false),
        },
        PaginationStyle::Offset => {
            last_page_items_len >= pagination.page_size.unwrap_or(0) as usize
        }
    }
}

/// Secondes Unix courantes, en i64 (signé : les calculs d'expiration font
/// des soustractions qui pourraient sinon sous-débocher en u64).
fn now_unix() -> Result<i64> {
    Ok(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .context("horloge système antérieure à 1970")?
        .as_secs() as i64)
}

/// UUID v4 (RFC 4122) généré avec le même CSPRNG (OsRng) que le reste du
/// projet (credential_vault.rs, cloud_share_token.rs, commands/deploy.rs).
/// Utilisé UNIQUEMENT pour les en-têtes d'idempotence — jamais une donnée
/// secrète, mais doit rester imprévisible pour remplir son rôle (un
/// attaquant capable de prédire la valeur pourrait rejouer une requête
/// sous une clé d'idempotence différente).
fn generate_uuid_v4() -> String {
    use chacha20poly1305::aead::{rand_core::RngCore, OsRng};
    let mut b = [0u8; 16];
    OsRng.fill_bytes(&mut b);
    b[6] = (b[6] & 0x0F) | 0x40; // version 4
    b[8] = (b[8] & 0x3F) | 0x80; // variant RFC 4122
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]
    )
}

/// Extrait (access_token, expires_in) d'une réponse de endpoint de token
/// OAuth2 — commun aux deux flux (client_credentials et jwt-bearer).
fn extract_token_response(v: &Value) -> Result<(String, i64)> {
    let access_token = v
        .get("access_token")
        .and_then(|x| x.as_str())
        .context("Réponse du serveur OAuth2 sans champ 'access_token'")?
        .to_string();
    let expires_in = v.get("expires_in").and_then(|x| x.as_i64()).unwrap_or(3600);
    Ok((access_token, expires_in))
}

/// Encodage Base64URL sans padding — format exigé par la spec JWT (RFC
/// 7519 §3) pour les trois segments header/claims/signature.
fn b64url(data: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(data)
}

/// Extrait les octets DER du corps base64 d'un bloc PEM (`-----BEGIN
/// ...-----` / `-----END ...-----`), en ignorant les retours à la ligne
/// internes — implémentation minimale suffisante pour une clé PKCS#8,
/// sans dépendance PEM supplémentaire (juste le crate `base64` déjà présent).
fn pem_to_der(pem: &str) -> Result<Vec<u8>> {
    let mut b64 = String::new();
    let mut in_block = false;
    for line in pem.lines() {
        let line = line.trim();
        if line.starts_with("-----BEGIN") {
            in_block = true;
            continue;
        }
        if line.starts_with("-----END") {
            break;
        }
        if in_block {
            b64.push_str(line);
        }
    }
    if b64.is_empty() {
        bail!("private_key : format PEM invalide (aucun bloc -----BEGIN/-----END trouvé)");
    }
    base64::engine::general_purpose::STANDARD
        .decode(&b64)
        .context("private_key : échec du décodage base64 du bloc PEM")
}

/// Construit et signe (RS256) un JWT d'assertion pour le flux
/// jwt-bearer (RFC 7523) d'un compte de service GCP. `private_key_pem`
/// doit être au format PKCS#8 (c'est le format exact du champ
/// `private_key` d'un fichier de compte de service GCP téléchargé depuis
/// la console).
fn build_and_sign_jwt(
    issuer_email: &str,
    scope: &str,
    audience: &str,
    now_secs: i64,
    private_key_pem: &str,
) -> Result<String> {
    let header = serde_json::json!({ "alg": "RS256", "typ": "JWT" });
    let claims = serde_json::json!({
        "iss": issuer_email,
        "scope": scope,
        "aud": audience,
        "iat": now_secs,
        "exp": now_secs + 3600,
    });

    let signing_input = format!(
        "{}.{}",
        b64url(&serde_json::to_vec(&header)?),
        b64url(&serde_json::to_vec(&claims)?),
    );

    let der = pem_to_der(private_key_pem)?;
    let key_pair = ring::signature::RsaKeyPair::from_pkcs8(&der)
        .map_err(|_| anyhow::anyhow!("private_key : clé RSA PKCS#8 invalide ou illisible"))?;

    let mut signature = vec![0u8; key_pair.public().modulus_len()];
    let rng = ring::rand::SystemRandom::new();
    key_pair
        .sign(&ring::signature::RSA_PKCS1_SHA256, &rng, signing_input.as_bytes(), &mut signature)
        .map_err(|_| anyhow::anyhow!("Échec de la signature RS256 du JWT"))?;

    Ok(format!("{}.{}", signing_input, b64url(&signature)))
}

// ═══════════════════════════════════════════════════════════════
//  Formatage de sortie
// ═══════════════════════════════════════════════════════════════

pub fn print_response(op: &Operation, v: &Value) {
    if v.is_null() {
        println!("  ✓ Opération effectuée (aucun contenu renvoyé).");
        return;
    }
    if !op.response_fields.is_empty() {
        if let Value::Object(map) = v {
            for field in &op.response_fields {
                if let Some(val) = map.get(field) {
                    println!("  {:<16} {}", format!("{}:", field), compact(val));
                }
            }
            return;
        }
    }
    println!("{}", serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string()));
}

fn compact(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

// ═══════════════════════════════════════════════════════════════
//  Confirmation pour les opérations dangereuses — même pattern que
//  commands/github.rs::confirm() (bypass ILOC_AUTO_CONFIRM=1 / --yes)
// ═══════════════════════════════════════════════════════════════

pub fn confirm_if_destructive(op: &Operation, auto_yes: bool) -> Result<bool> {
    match op.danger {
        Danger::Destructive => {
            if auto_yes || std::env::var("ILOC_AUTO_CONFIRM").as_deref() == Ok("1") {
                return Ok(true);
            }
            use std::io::Write;
            print!("  ⚠ '{}' est une opération destructrice. Confirmer ? [y/N] ", op.path.join(" "));
            std::io::stdout().flush().ok();
            let mut ans = String::new();
            std::io::stdin().read_line(&mut ans)?;
            Ok(matches!(ans.trim().to_lowercase().as_str(), "y" | "yes" | "o" | "oui"))
        }
        _ => Ok(true),
    }
}

// ═══════════════════════════════════════════════════════════════
//  Dispatch principal — appelé depuis main() pour toute commande
//  dont le premier argument correspond à un slug installé.
// ═══════════════════════════════════════════════════════════════

pub async fn dispatch(raw_args: &[String]) -> Result<()> {
    let slug = raw_args.get(1).context("Slug de provider manquant")?;
    let manifest = load_installed(slug)?
        .ok_or_else(|| anyhow::anyhow!("Provider '{}' non installé. Voir `iloc provider install {}`.", slug, slug))?;

    let cmd = build_command(&manifest);
    let matches = match cmd.try_get_matches_from(&raw_args[1..]) {
        Ok(m) => m,
        Err(e) => {
            // Comportement standard clap : affiche l'aide/l'erreur et
            // quitte avec le bon code de sortie — identique à ce que
            // ferait n'importe quelle commande statique existante.
            e.exit();
        }
    };

    let (op, values) = resolve_invocation(&manifest, &matches)?;
    let profile_name = matches.get_one::<String>("__profile").map(|s| s.as_str());
    let auto_yes = matches.get_flag("__yes");

    if !confirm_if_destructive(op, auto_yes)? {
        println!("  Annulé.");
        return Ok(());
    }

    for a in &op.args {
        if a.required && !values.contains_key(&a.id) {
            bail!("Argument requis manquant : --{}", a.long.clone().unwrap_or_else(|| a.id.clone()));
        }
    }

    let creds = provider_store::require_credentials(slug, &manifest, profile_name)?;
    let client = GenericClient::new(&manifest, &creds);
    let result = client.execute(op, &values).await?;
    print_response(op, &result);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider_manifest::{ApiConfig, AuthSchema, BodyEncoding, OperationArg, ProviderIdentity, StaticHeader};

    fn sample_manifest() -> ProviderManifest {
        ProviderManifest {
            provider: ProviderIdentity {
                slug: "testco".into(), name: "TestCo".into(), description: "desc".into(),
                author: "a".into(), version: "0.1.0".into(), manifest_version: 1,
            },
            auth: AuthSchema {
                auth_type: AuthType::BearerToken,
                fields: vec![],
                header: Some("Authorization".into()),
                value_prefix: "Bearer ".into(),
                verify_endpoint: None, verify_field: None, help_url: None,
                token_url: None, scope: None, jwt_audience: None,
            },
            api: ApiConfig { base_url: "https://api.testco.example".into() },
            operations: vec![
                Operation {
                    path: vec!["item".into(), "list".into()],
                    method: "GET".into(), endpoint: "/items".into(),
                    summary: "Liste".into(), danger: Danger::Safe,
                    args: vec![], example: None, response_fields: vec![],
                    body_encoding: BodyEncoding::Json, graphql_query: None,
                    extra_headers: vec![], idempotency_header: None, pagination: None,
                },
                Operation {
                    path: vec!["item".into(), "view".into()],
                    method: "GET".into(), endpoint: "/items/{id}".into(),
                    summary: "Voir".into(), danger: Danger::Safe,
                    args: vec![OperationArg {
                        id: "id".into(), long: None, positional: true, required: true,
                        help: "id".into(), location: ArgLocation::Path, field: None,
                    }],
                    example: None, response_fields: vec![],
                    body_encoding: BodyEncoding::Json, graphql_query: None,
                    extra_headers: vec![], idempotency_header: None, pagination: None,
                },
                Operation {
                    path: vec!["item".into(), "delete".into()],
                    method: "DELETE".into(), endpoint: "/items/{id}".into(),
                    summary: "Supprime".into(), danger: Danger::Destructive,
                    args: vec![OperationArg {
                        id: "id".into(), long: None, positional: true, required: true,
                        help: "id".into(), location: ArgLocation::Path, field: None,
                    }],
                    example: None, response_fields: vec![],
                    body_encoding: BodyEncoding::Json, graphql_query: None,
                    extra_headers: vec![], idempotency_header: None, pagination: None,
                },
            ],
        }
    }

    #[test]
    fn builds_command_tree_matching_operations() {
        let m = sample_manifest();
        let cmd = build_command(&m);
        assert_eq!(cmd.get_name(), "testco");
        let item_sub = cmd.find_subcommand("item").expect("groupe 'item' attendu");
        assert!(item_sub.find_subcommand("list").is_some());
        assert!(item_sub.find_subcommand("view").is_some());
        assert!(item_sub.find_subcommand("delete").is_some());
    }

    #[test]
    fn parses_positional_path_arg_correctly() {
        let m = sample_manifest();
        let cmd = build_command(&m);
        let matches = cmd
            .try_get_matches_from(vec!["testco", "item", "view", "abc123"])
            .expect("doit parser un id positionnel");
        let (op, values) = resolve_invocation(&m, &matches).unwrap();
        assert_eq!(op.path, vec!["item", "view"]);
        assert_eq!(values.get("id").unwrap(), "abc123");
    }

    #[test]
    fn missing_required_positional_fails_parsing() {
        let m = sample_manifest();
        let cmd = build_command(&m);
        let result = cmd.try_get_matches_from(vec!["testco", "item", "view"]);
        assert!(result.is_err(), "clap doit exiger l'argument positionnel requis");
    }

    #[test]
    fn unknown_subcommand_fails_parsing() {
        let m = sample_manifest();
        let cmd = build_command(&m);
        let result = cmd.try_get_matches_from(vec!["testco", "bogus"]);
        assert!(result.is_err());
    }

    #[test]
    fn url_substitution_replaces_path_placeholder() {
        let m = sample_manifest();
        let creds = provider_store::ResolvedProviderCredentials {
            profile_name: "default".into(),
            account: "default-test".into(),
            api_url: "https://api.testco.example".into(),
            fields: HashMap::from([("token".to_string(), "tok".to_string())]),
        };
        let client = GenericClient::new(&m, &creds);
        let op = m.operations.iter().find(|o| o.path == ["item", "view"]).unwrap();
        let mut values = HashMap::new();
        values.insert("id".to_string(), "xyz".to_string());
        // On ne peut pas envoyer de vraie requête ici (pas de réseau
        // dans un test unitaire) — on vérifie seulement la construction
        // d'URL via url_for(), le comportement réseau étant couvert par
        // les tests d'intégration contre le serveur de test HTTP.
        let endpoint = op.endpoint.replace("{id}", &values["id"]);
        assert_eq!(client.url_for(&endpoint), "https://api.testco.example/items/xyz");
    }

    #[test]
    fn destructive_operation_auto_confirmed_with_yes_flag() {
        let m = sample_manifest();
        let op = m.operations.iter().find(|o| o.path == ["item", "delete"]).unwrap();
        assert!(confirm_if_destructive(op, true).unwrap());
    }

    #[test]
    fn safe_operation_never_prompts() {
        let m = sample_manifest();
        let op = m.operations.iter().find(|o| o.path == ["item", "list"]).unwrap();
        // danger=Safe → confirm_if_destructive doit retourner true
        // immédiatement, sans jamais lire stdin (sinon ce test bloquerait).
        assert!(confirm_if_destructive(op, false).unwrap());
    }

    #[test]
    fn error_message_extraction_tries_common_keys() {
        assert_eq!(extract_error_message(&serde_json::json!({"message": "boom"})), "boom");
        assert_eq!(extract_error_message(&serde_json::json!({"error": "nope"})), "nope");
        assert_eq!(extract_error_message(&serde_json::json!({"detail": "bad"})), "bad");
        assert_eq!(
            extract_error_message(&serde_json::json!({"errors": [{"message": "deep"}]})),
            "deep"
        );
        assert_eq!(extract_error_message(&serde_json::json!({"errors": ["flat"]})), "flat");
    }

    #[test]
    fn parse_json_response_maps_status_codes_to_hints() {
        let err = parse_json_response(401, br#"{"message":"bad token"}"#, "GET", "x").unwrap_err();
        assert!(err.to_string().contains("identifiants invalides"));
        let err = parse_json_response(404, b"{}", "GET", "x").unwrap_err();
        assert!(err.to_string().contains("introuvable"));
    }

    #[test]
    fn parse_json_response_empty_204_is_null() {
        let v = parse_json_response(204, b"", "DELETE", "x").unwrap();
        assert!(v.is_null());
    }

    #[test]
    fn is_installed_provider_false_for_unknown_slug() {
        std::env::set_var("HOME", std::env::temp_dir().join("iloc_engine_test_empty"));
        assert!(!is_installed_provider("definitely-not-installed-xyz"));
    }

    // ═══════════════════════════════════════════════════════════
    //  OAuth2 / GraphQL / form-encoding / headers — nouvelles
    //  capacités génériques (session août 2026)
    // ═══════════════════════════════════════════════════════════

    // Clé RSA de test (2048 bits, PKCS#8) — générée exclusivement pour
    // cette suite de tests, jamais utilisée ailleurs, aucun secret réel.
    const TEST_PRIVATE_KEY_PEM: &str = "-----BEGIN PRIVATE KEY-----\nMIIEvAIBADANBgkqhkiG9w0BAQEFAASCBKYwggSiAgEAAoIBAQDl7UKkWvaMiKbg\nQlI6UFY/d+pP8R9Gydbc3sGhk9zwqnudMOwdnHEVIfrwBS8jUlQr+pgiP+oq02V1\nZ6sWqr2Z2GIkiRlnMBiOk6A1rGhINoCPGcB0bxILiTd1x7Njz7U7w9AxQ6VLlh+P\n76DQn9K6kh9dlEz2KCBpIn3HkIY0qLihEx8Wf6T/aY2ZoZRk9PGYWGrHa2fvT+11\nW2mrfnlQACundQYh+tMVKZtEBrd1wpcsBnMgIpcn/GCQ9sKE4uWaXGJzKgybhm9H\n7CIEP46dTKRtmKRGehPT9g/T8R2BNAZhq3eCHZ4cR7Vr6XYTn+shZ9miyg5CSxEq\nt5GXO4E5AgMBAAECggEAPcoA+sInN6URk3q/NkSYqP3Ezi7yRMfBIiIKzy05VsO5\n7IhVK6/7A77Z/N6nyEo7rIXvlGSwvUmKHn75j2HbChkIZuEhHoXiU46Ao2vtqlpb\nOhmliqS+qLL7YH+GSfBrt9/rdxHCvgld+gRfpzEMJG9YVoGgHRazfw1x18uTBVs/\nu1z0iKgY+6JGq/TT/Ixo34nlvw1HVkCiELdz8lvip2+wwMcsfW+40TOGAqM5lHOn\nhcqY+lXfQI6AJJgEEznoncjC9s1yE+/8xk/A66mNypRh0de7Jy0faF/z9tSZSEW/\nNeohEcRWN2jBeSpc0spYueqmy66/roYyu3oX8MOQcQKBgQD0sLQ7LQ2UbpJ0mP/2\nqLfSgjEDoY7EMp2fB8ECrFgMls1WDeBc8x0ka52J+vcrxb2/qMgC/xVkkW+ouicc\nAA6SPlkgKPbGRVww+pZiQKo01NVXnr59OsfMSOqs6GJPBhJ8+xe6LEP21ITBPUrv\nTUuSj5An3uCg16295X78IFJmZwKBgQDwjd4qPZFwCgTwNlypDZLrKpRmnF6rPodG\nyrmOkK6x+woh0Ugve41pzvxZ/WZZSHMjTx4g++pY7TbU+0OThHOjMW7AwWLDnOTe\nkRAbHST/uAnQAKhPaYirlYT07s488D5TGc9Zn37zrs3uDJvDzqfnPyxlrm9WjE9H\nu+CQp8HXXwKBgGjgnD+I7fsi8Y8cTQmyAygtOUjvJDwf3cNeFXJJ4Gt074nk5Ley\nVFlZ7upHMU4HsW7GrwPpxYeXdp6BO2Ya+CPiqVzJcgxFimBL580xHkMKvm6R0d/n\nI+ABmOSHritk1OPQ07iuZGsVZ9lTphyvqqak9grA0tLd3tA335e9WtQdAoGAAYFB\ntI3yDPtjEIWmisA0/RelGgc8aGHZws2d35B0J1TkuVVv2CwztEfBOGbnbwOPBNeH\n3rj0vF2vjCGOSKv5dTnn8XjEP2kJ3YKW0TSbeKYUGaMHaofEfR5QWJ/t1l/CZA6z\nR2JCDxA25ZhamRz/2+h/RJuUwrvZ+x7nxr/l7I0CgYADDkixkjzB5WfL7qoK1KH1\nzd3Zg9qLN5iv9jwL5JhRG5EXZhK2fbS15GGtsqwOnMlEgLya5bLjZ4XxF0sf7fdk\nzdyoJ6aLWolKCTZafUCyQt5XcxXWLPJkjkRJLTGFNtQi8IuV4jVoNG+SDc1Lwlp1\nPcLX34NaKazrAWKhMgAc7Q==\n-----END PRIVATE KEY-----";

    const TEST_PUBLIC_KEY_PEM: &str = r#"-----BEGIN PUBLIC KEY-----
MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEA5e1CpFr2jIim4EJSOlBW
P3fqT/EfRsnW3N7BoZPc8Kp7nTDsHZxxFSH68AUvI1JUK/qYIj/qKtNldWerFqq9
mdhiJIkZZzAYjpOgNaxoSDaAjxnAdG8SC4k3dcezY8+1O8PQMUOlS5Yfj++g0J/S
upIfXZRM9iggaSJ9x5CGNKi4oRMfFn+k/2mNmaGUZPTxmFhqx2tn70/tdVtpq355
UAArp3UGIfrTFSmbRAa3dcKXLAZzICKXJ/xgkPbChOLlmlxicyoMm4ZvR+wiBD+O
nUykbZikRnoT0/YP0/EdgTQGYat3gh2eHEe1a+l2E5/rIWfZosoOQksRKreRlzuB
OQIDAQAB
-----END PUBLIC KEY-----"#;

    fn sample_service_account_json(token_uri: &str) -> String {
        serde_json::json!({
            "client_email": "test-sa@example-project.iam.gserviceaccount.com",
            "private_key": TEST_PRIVATE_KEY_PEM,
            "token_uri": token_uri,
        }).to_string()
    }

    // ── pem_to_der / b64url / build_and_sign_jwt — unitaires, sans réseau ──

    #[test]
    fn pem_to_der_extracts_base64_body() {
        let der = pem_to_der(TEST_PRIVATE_KEY_PEM).unwrap();
        assert!(!der.is_empty());
        // Une clé RSA 2048 bits PKCS#8 fait autour de 1200 octets en DER.
        assert!(der.len() > 1000 && der.len() < 1600, "taille DER suspecte: {}", der.len());
    }

    #[test]
    fn pem_to_der_rejects_garbage() {
        assert!(pem_to_der("pas du tout du PEM").is_err());
        assert!(pem_to_der("").is_err());
    }

    #[test]
    fn b64url_has_no_padding_and_uses_url_safe_alphabet() {
        let out = b64url(&[0xFF, 0xFE, 0xFD, 0x00, 0x01]);
        assert!(!out.contains('='), "le base64url JWT ne doit jamais avoir de padding");
        assert!(!out.contains('+') && !out.contains('/'), "alphabet non URL-safe détecté");
    }

    #[test]
    fn jwt_has_three_dot_separated_segments_with_correct_claims() {
        let jwt = build_and_sign_jwt(
            "test-sa@example-project.iam.gserviceaccount.com",
            "https://www.googleapis.com/auth/cloud-platform",
            "https://oauth2.googleapis.com/token",
            1_700_000_000,
            TEST_PRIVATE_KEY_PEM,
        ).unwrap();

        let parts: Vec<&str> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3, "un JWT doit avoir exactement 3 segments");

        let header_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(parts[0]).unwrap();
        let header: Value = serde_json::from_slice(&header_bytes).unwrap();
        assert_eq!(header["alg"], "RS256");
        assert_eq!(header["typ"], "JWT");

        let claims_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(parts[1]).unwrap();
        let claims: Value = serde_json::from_slice(&claims_bytes).unwrap();
        assert_eq!(claims["iss"], "test-sa@example-project.iam.gserviceaccount.com");
        assert_eq!(claims["aud"], "https://oauth2.googleapis.com/token");
        assert_eq!(claims["iat"], 1_700_000_000);
        assert_eq!(claims["exp"], 1_700_003_600);
    }

    #[test]
    fn jwt_signature_verifies_against_the_matching_public_key() {
        // Le test le plus important de tout ce bloc : signe avec la clé
        // privée, vérifie avec la clé PUBLIQUE correspondante via ring —
        // exactement ce qu'un vrai serveur OAuth2 (Google inclus) ferait
        // pour valider l'assertion JWT-bearer.
        let jwt = build_and_sign_jwt("iss@example.com", "scope", "aud", 1_700_000_000, TEST_PRIVATE_KEY_PEM).unwrap();
        let parts: Vec<&str> = jwt.split('.').collect();
        let signing_input = format!("{}.{}", parts[0], parts[1]);
        let signature = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(parts[2]).unwrap();

        let pub_der = pem_to_der(TEST_PUBLIC_KEY_PEM).unwrap();
        // SubjectPublicKeyInfo (SPKI) DER — ring veut la clé publique nue ;
        // on utilise UnparsedPublicKey avec RSA_PKCS1_2048_8192_SHA256 qui
        // accepte directement le format SPKI produit par `openssl rsa -pubout`.
        use ring::signature::{UnparsedPublicKey, RSA_PKCS1_2048_8192_SHA256};
        // Extrait la clé publique brute (BIT STRING) du SPKI — ring exige la
        // clé RSA nue, pas l'enveloppe SPKI complète. Solution robuste : on
        // redérive la clé publique depuis la paire de clés PRIVÉE elle-même
        // (ring expose modulus/exponent via RsaKeyPair::public), ce qui
        // élimine tout risque de parsing ASN.1 fragile dans ce test.
        let der = pem_to_der(TEST_PRIVATE_KEY_PEM).unwrap();
        let key_pair = ring::signature::RsaKeyPair::from_pkcs8(&der).unwrap();
        let public_key = UnparsedPublicKey::new(&RSA_PKCS1_2048_8192_SHA256, key_pair.public().as_ref());
        let _ = pub_der; // conservé pour documenter l'alternative SPKI ci-dessus

        public_key
            .verify(signing_input.as_bytes(), &signature)
            .expect("la signature RS256 doit être valide pour la clé publique correspondante");
    }

    #[test]
    fn jwt_signature_fails_verification_with_a_different_signing_input() {
        // Contre-épreuve : falsifier ne serait-ce qu'un octet du texte signé
        // doit faire échouer la vérification — sinon le test précédent ne
        // prouverait rien.
        let jwt = build_and_sign_jwt("iss@example.com", "scope", "aud", 1_700_000_000, TEST_PRIVATE_KEY_PEM).unwrap();
        let parts: Vec<&str> = jwt.split('.').collect();
        let signature = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(parts[2]).unwrap();

        let der = pem_to_der(TEST_PRIVATE_KEY_PEM).unwrap();
        let key_pair = ring::signature::RsaKeyPair::from_pkcs8(&der).unwrap();
        use ring::signature::{UnparsedPublicKey, RSA_PKCS1_2048_8192_SHA256};
        let public_key = UnparsedPublicKey::new(&RSA_PKCS1_2048_8192_SHA256, key_pair.public().as_ref());

        let tampered = b"ceci.nestpaslebonsigninginput";
        assert!(public_key.verify(tampered, &signature).is_err());
    }

    #[test]
    fn uuid_v4_has_correct_version_and_variant_and_is_unique() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..100 {
            let u = generate_uuid_v4();
            assert_eq!(u.len(), 36);
            assert_eq!(u.chars().nth(14), Some('4'), "le 13e octet doit encoder version=4");
            let variant_char = u.chars().nth(19).unwrap();
            assert!(matches!(variant_char, '8' | '9' | 'a' | 'b'), "variant RFC4122 attendu, reçu {variant_char}");
            assert!(seen.insert(u), "collision d'UUID détectée sur seulement 100 générations");
        }
    }

    // ── Serveur HTTP local minimal pour les tests d'intégration ──────
    // Capture chaque requête reçue (méthode, chemin, en-têtes, corps) et
    // répond via une closure fournie par le test — permet de vérifier
    // exactement ce que GenericClient envoie sur le réseau, sans jamais
    // contacter un vrai service tiers.

    #[derive(Debug, Clone)]
    struct CapturedRequest {
        method: String,
        path: String,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    }

    impl CapturedRequest {
        fn header(&self, name: &str) -> Option<&str> {
            self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
        }
        fn body_str(&self) -> String {
            String::from_utf8_lossy(&self.body).to_string()
        }
    }

    /// Démarre un serveur HTTP réel sur 127.0.0.1 (port choisi par l'OS) et
    /// retourne son URL de base, la liste (partagée, mutable) des requêtes
    /// reçues, et le handle de tâche associé. `handler` décide de la
    /// réponse à renvoyer pour chaque requête capturée.
    async fn spawn_test_server<F>(
        handler: F,
    ) -> (String, std::sync::Arc<std::sync::Mutex<Vec<CapturedRequest>>>, tokio::task::JoinHandle<()>)
    where
        F: Fn(&CapturedRequest) -> (u16, Vec<u8>) + Send + Sync + 'static,
    {
        use hyper::service::{make_service_fn, service_fn};
        use hyper::{Response, Server};
        use std::convert::Infallible;
        use std::sync::{Arc, Mutex};

        let captured: Arc<Mutex<Vec<CapturedRequest>>> = Arc::new(Mutex::new(Vec::new()));
        let captured_for_svc = captured.clone();
        let handler = Arc::new(handler);

        let make_svc = make_service_fn(move |_conn| {
            let captured = captured_for_svc.clone();
            let handler = handler.clone();
            async move {
                Ok::<_, Infallible>(service_fn(move |req: Request<Body>| {
                    let captured = captured.clone();
                    let handler = handler.clone();
                    async move {
                        let method = req.method().to_string();
                        // path_and_query (pas juste .path()) : inclut la query
                        // string, nécessaire pour les tests de pagination qui
                        // vérifient l'offset/curseur envoyé sur chaque page.
                        // Sans query string, identique à .path() — ne change
                        // rien pour les tests existants qui n'en ont jamais.
                        let path = req.uri().path_and_query().map(|pq| pq.to_string()).unwrap_or_default();
                        let headers: Vec<(String, String)> = req
                            .headers()
                            .iter()
                            .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
                            .collect();
                        let body = to_bytes(req.into_body()).await.unwrap_or_default().to_vec();
                        let cap = CapturedRequest { method, path, headers, body };
                        let (status, resp_body) = handler(&cap);
                        captured.lock().unwrap().push(cap);
                        Ok::<_, Infallible>(
                            Response::builder()
                                .status(status)
                                .header("Content-Type", "application/json")
                                .body(Body::from(resp_body))
                                .unwrap(),
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
        (format!("http://{}", bound_addr), captured, handle)
    }

    /// Isole chaque test réseau dans son propre HOME temporaire — même
    /// principe que provider_store.rs::use_temp_home(), nécessaire ici
    /// aussi car ces tests écrivent de vrais fichiers de repli chiffrés
    /// (cache de jeton OAuth2) et tournent en parallèle.
    fn use_temp_home_for_engine_test(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "iloc_engine_test_{tag}_{}",
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("HOME", &dir);
        std::env::set_var("USERPROFILE", &dir);
        dir
    }

    fn manifest_with_auth(slug: &str, base_url: &str, auth_type: AuthType, extra: Vec<OperationArg>, op: Operation) -> ProviderManifest {
        let _ = extra;
        ProviderManifest {
            provider: ProviderIdentity {
                slug: slug.into(), name: "Test".into(), description: "d".into(),
                author: "a".into(), version: "0.1.0".into(), manifest_version: 1,
            },
            auth: AuthSchema {
                auth_type,
                fields: vec![],
                header: Some("Authorization".into()),
                value_prefix: "Bearer ".into(),
                verify_endpoint: None, verify_field: None, help_url: None,
                token_url: Some(format!("{}/oauth/token", base_url)),
                scope: Some("read write".into()),
                jwt_audience: None,
            },
            api: ApiConfig { base_url: base_url.to_string() },
            operations: vec![op],
        }
    }

    #[tokio::test]
    async fn graphql_operation_sends_query_and_variables_as_json_body() {
        let (base_url, captured, _h) = spawn_test_server(|_req| {
            (200, br#"{"data": {"issue": {"id": "ISSUE-1"}}}"#.to_vec())
        }).await;

        let op = Operation {
            path: vec!["issue".into(), "view".into()],
            method: "POST".into(), endpoint: "/graphql".into(),
            summary: "s".into(), danger: Danger::Safe,
            args: vec![OperationArg {
                id: "issue_id".into(), long: None, positional: true, required: true,
                help: "id".into(), location: ArgLocation::Body, field: None,
            }],
            example: None, response_fields: vec![],
            body_encoding: BodyEncoding::Json,
            graphql_query: Some("query($issue_id: String!) { issue(id: $issue_id) { id } }".into()),
            extra_headers: vec![], idempotency_header: None, pagination: None,
        };
        let manifest = manifest_with_auth("gqltest", &base_url, AuthType::None, vec![], op.clone());
        let creds = provider_store::ResolvedProviderCredentials {
            profile_name: "default".into(), account: "gqltest-acc".into(),
            api_url: base_url.clone(), fields: HashMap::new(),
        };
        let client = GenericClient::new(&manifest, &creds);
        let mut values = HashMap::new();
        values.insert("issue_id".to_string(), "ISSUE-1".to_string());

        let result = client.execute(&op, &values).await.unwrap();
        assert_eq!(result["data"]["issue"]["id"], "ISSUE-1");

        let reqs = captured.lock().unwrap();
        assert_eq!(reqs.len(), 1);
        let body: Value = serde_json::from_slice(&reqs[0].body).unwrap();
        assert_eq!(body["query"], "query($issue_id: String!) { issue(id: $issue_id) { id } }");
        assert_eq!(body["variables"]["issue_id"], "ISSUE-1");
        assert_eq!(reqs[0].header("content-type"), Some("application/json"));
    }

    #[tokio::test]
    async fn form_encoding_sends_url_encoded_body_with_correct_content_type() {
        let (base_url, captured, _h) = spawn_test_server(|_req| (200, b"{}".to_vec())).await;

        let op = Operation {
            path: vec!["charge".into(), "create".into()],
            method: "POST".into(), endpoint: "/v1/charges".into(),
            summary: "s".into(), danger: Danger::Safe,
            args: vec![
                OperationArg { id: "amount".into(), long: None, positional: false, required: true,
                    help: "h".into(), location: ArgLocation::Body, field: None },
                OperationArg { id: "currency".into(), long: None, positional: false, required: true,
                    help: "h".into(), location: ArgLocation::Body, field: None },
            ],
            example: None, response_fields: vec![],
            body_encoding: BodyEncoding::Form,
            graphql_query: None, extra_headers: vec![], idempotency_header: None, pagination: None,
        };
        let manifest = manifest_with_auth("stripetest", &base_url, AuthType::None, vec![], op.clone());
        let creds = provider_store::ResolvedProviderCredentials {
            profile_name: "default".into(), account: "stripetest-acc".into(),
            api_url: base_url.clone(), fields: HashMap::new(),
        };
        let client = GenericClient::new(&manifest, &creds);
        let mut values = HashMap::new();
        values.insert("amount".to_string(), "2000".to_string());
        values.insert("currency".to_string(), "usd".to_string());

        client.execute(&op, &values).await.unwrap();

        let reqs = captured.lock().unwrap();
        assert_eq!(reqs[0].header("content-type"), Some("application/x-www-form-urlencoded"));
        let body = reqs[0].body_str();
        assert!(body.contains("amount=2000"), "corps reçu: {body}");
        assert!(body.contains("currency=usd"), "corps reçu: {body}");
        assert!(!body.trim_start().starts_with('{'), "ne doit PAS être du JSON: {body}");
    }

    #[tokio::test]
    async fn extra_headers_and_idempotency_key_are_sent_and_vary_per_call() {
        let (base_url, captured, _h) = spawn_test_server(|_req| (200, b"{}".to_vec())).await;

        let op = Operation {
            path: vec!["charge".into(), "create".into()],
            method: "POST".into(), endpoint: "/v1/charges".into(),
            summary: "s".into(), danger: Danger::Safe,
            args: vec![], example: None, response_fields: vec![],
            body_encoding: BodyEncoding::Json, graphql_query: None,
            extra_headers: vec![StaticHeader { name: "Stripe-Version".into(), value: "2024-06-20".into() }],
            idempotency_header: Some("Idempotency-Key".into()),
            pagination: None,
        };
        let manifest = manifest_with_auth("hdrtest", &base_url, AuthType::None, vec![], op.clone());
        let creds = provider_store::ResolvedProviderCredentials {
            profile_name: "default".into(), account: "hdrtest-acc".into(),
            api_url: base_url.clone(), fields: HashMap::new(),
        };
        let client = GenericClient::new(&manifest, &creds);
        client.execute(&op, &HashMap::new()).await.unwrap();
        client.execute(&op, &HashMap::new()).await.unwrap();

        let reqs = captured.lock().unwrap();
        assert_eq!(reqs.len(), 2);
        for r in reqs.iter() {
            assert_eq!(r.header("stripe-version"), Some("2024-06-20"));
            assert!(r.header("idempotency-key").is_some());
            assert_eq!(r.header("idempotency-key").unwrap().len(), 36);
        }
        assert_ne!(
            reqs[0].header("idempotency-key"), reqs[1].header("idempotency-key"),
            "deux appels distincts doivent avoir des clés d'idempotence différentes"
        );
    }

    #[tokio::test]
    async fn oauth2_client_credentials_end_to_end_with_cache_reuse() {
        let _home = use_temp_home_for_engine_test("cc");
        let (base_url, captured, _h) = spawn_test_server(|req| {
            if req.path == "/oauth/token" {
                (200, br#"{"access_token": "tok-abc123", "expires_in": 3600}"#.to_vec())
            } else {
                (200, br#"{"ok": true}"#.to_vec())
            }
        }).await;

        let op = Operation {
            path: vec!["vm".into(), "list".into()],
            method: "GET".into(), endpoint: "/vms".into(),
            summary: "s".into(), danger: Danger::Safe,
            args: vec![], example: None, response_fields: vec![],
            body_encoding: BodyEncoding::Json, graphql_query: None,
            extra_headers: vec![], idempotency_header: None, pagination: None,
        };
        let manifest = manifest_with_auth("azuretest", &base_url, AuthType::OAuth2ClientCredentials, vec![], op.clone());
        let mut fields = HashMap::new();
        fields.insert("client_id".to_string(), "my-client-id".to_string());
        fields.insert("client_secret".to_string(), "my-client-secret".to_string());
        let creds = provider_store::ResolvedProviderCredentials {
            profile_name: "default".into(), account: "azuretest-acc".into(),
            api_url: base_url.clone(), fields,
        };
        let client = GenericClient::new(&manifest, &creds);

        // Premier appel : doit échanger un jeton PUIS appeler l'API.
        client.execute(&op, &HashMap::new()).await.unwrap();
        {
            let reqs = captured.lock().unwrap();
            assert_eq!(reqs.len(), 2, "attendu : 1 appel token + 1 appel API");
            let token_req = reqs.iter().find(|r| r.path == "/oauth/token").unwrap();
            assert_eq!(token_req.header("content-type"), Some("application/x-www-form-urlencoded"));
            let form = token_req.body_str();
            assert!(form.contains("grant_type=client_credentials"), "{form}");
            assert!(form.contains("client_id=my-client-id"), "{form}");
            assert!(form.contains("client_secret=my-client-secret"), "{form}");
            let api_req = reqs.iter().find(|r| r.path == "/vms").unwrap();
            assert_eq!(api_req.header("authorization"), Some("Bearer tok-abc123"));
        }

        // Deuxième appel : le jeton en cache est encore valide (3600s) —
        // AUCUN nouvel appel au endpoint de token ne doit avoir lieu.
        client.execute(&op, &HashMap::new()).await.unwrap();
        {
            let reqs = captured.lock().unwrap();
            assert_eq!(reqs.len(), 3, "attendu : +1 appel API seulement, jeton réutilisé du cache");
            let token_calls = reqs.iter().filter(|r| r.path == "/oauth/token").count();
            assert_eq!(token_calls, 1, "le endpoint de token ne doit être appelé qu'une seule fois");
        }
    }

    #[tokio::test]
    async fn oauth2_service_account_jwt_bearer_end_to_end_with_signature_verification() {
        let _home = use_temp_home_for_engine_test("sa");
        let (base_url, captured, _h) = spawn_test_server(|req| {
            if req.path == "/oauth/token" {
                // Vérifie RÉELLEMENT la signature du JWT reçu — pas seulement
                // sa forme — exactement ce qu'un vrai serveur Google ferait.
                let form = req.body_str();
                let assertion = form.split('&')
                    .find_map(|kv| kv.strip_prefix("assertion="))
                    .expect("paramètre assertion manquant");
                let jwt = url_decode_for_test(assertion);
                let parts: Vec<&str> = jwt.split('.').collect();
                assert_eq!(parts.len(), 3);
                let signing_input = format!("{}.{}", parts[0], parts[1]);
                let signature = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(parts[2]).unwrap();
                let der = pem_to_der(TEST_PRIVATE_KEY_PEM).unwrap();
                let key_pair = ring::signature::RsaKeyPair::from_pkcs8(&der).unwrap();
                use ring::signature::{UnparsedPublicKey, RSA_PKCS1_2048_8192_SHA256};
                let public_key = UnparsedPublicKey::new(&RSA_PKCS1_2048_8192_SHA256, key_pair.public().as_ref());
                public_key.verify(signing_input.as_bytes(), &signature)
                    .expect("signature JWT invalide reçue par le faux serveur de token");
                assert!(form.contains("grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer"));
                (200, br#"{"access_token": "gcp-tok-xyz", "expires_in": 3600}"#.to_vec())
            } else {
                (200, br#"{"instances": []}"#.to_vec())
            }
        }).await;

        let op = Operation {
            path: vec!["instance".into(), "list".into()],
            method: "GET".into(), endpoint: "/instances".into(),
            summary: "s".into(), danger: Danger::Safe,
            args: vec![], example: None, response_fields: vec![],
            body_encoding: BodyEncoding::Json, graphql_query: None,
            extra_headers: vec![], idempotency_header: None, pagination: None,
        };
        let manifest = manifest_with_auth("gcptest", &base_url, AuthType::OAuth2ServiceAccount, vec![], op.clone());
        let mut fields = HashMap::new();
        fields.insert("service_account_json".to_string(), sample_service_account_json(&format!("{}/oauth/token", base_url)));
        let creds = provider_store::ResolvedProviderCredentials {
            profile_name: "default".into(), account: "gcptest-acc".into(),
            api_url: base_url.clone(), fields,
        };
        let client = GenericClient::new(&manifest, &creds);

        let result = client.execute(&op, &HashMap::new()).await.unwrap();
        assert!(result["instances"].is_array());

        let reqs = captured.lock().unwrap();
        let api_req = reqs.iter().find(|r| r.path == "/instances").unwrap();
        assert_eq!(api_req.header("authorization"), Some("Bearer gcp-tok-xyz"));
    }

    /// Décodage percent-encoding minimal — juste assez pour relire dans un
    /// test le paramètre `assertion=` encodé par url_encode() du moteur
    /// (jamais utilisé en dehors des tests : le moteur n'a besoin que
    /// d'encoder, jamais de décoder ses propres requêtes sortantes).
    fn url_decode_for_test(s: &str) -> String {
        let bytes = s.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'%' && i + 2 < bytes.len() {
                if let Ok(byte) = u8::from_str_radix(std::str::from_utf8(&bytes[i+1..i+3]).unwrap_or(""), 16) {
                    out.push(byte);
                    i += 3;
                    continue;
                }
            }
            out.push(bytes[i]);
            i += 1;
        }
        String::from_utf8_lossy(&out).to_string()
    }

    // ── Pagination automatique : bout-en-bout contre un vrai serveur ──

    #[test]
    fn get_json_path_navigates_nested_objects() {
        let v = serde_json::json!({"page_info": {"end_cursor": "abc", "has_next": true}});
        assert_eq!(get_json_path(&v, "page_info.end_cursor").unwrap(), "abc");
        assert_eq!(get_json_path(&v, "page_info.has_next").unwrap(), true);
        assert!(get_json_path(&v, "page_info.absent").is_none());
        assert!(get_json_path(&v, "absent.x").is_none());
    }

    #[test]
    fn get_json_path_single_segment_reads_top_level() {
        let v = serde_json::json!({"data": [1, 2, 3]});
        assert_eq!(get_json_path(&v, "data").unwrap().as_array().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn cursor_pagination_fetches_all_pages_and_stops_on_missing_cursor() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let call_count = std::sync::Arc::new(AtomicUsize::new(0));
        let call_count_srv = call_count.clone();

        let (base_url, captured, _h) = spawn_test_server(move |_req| {
            let n = call_count_srv.fetch_add(1, Ordering::SeqCst);
            match n {
                0 => (200, br#"{"data": ["a", "b"], "next_cursor": "page2"}"#.to_vec()),
                1 => (200, br#"{"data": ["c", "d"], "next_cursor": "page3"}"#.to_vec()),
                _ => (200, br#"{"data": ["e"], "next_cursor": null}"#.to_vec()),
            }
        }).await;

        let op = Operation {
            path: vec!["item".into(), "list".into()],
            method: "GET".into(), endpoint: "/items".into(),
            summary: "s".into(), danger: Danger::Safe,
            args: vec![OperationArg {
                id: "cursor".into(), long: Some("cursor".into()), positional: false, required: false,
                help: "h".into(), location: ArgLocation::Query, field: None,
            }],
            example: None, response_fields: vec![],
            body_encoding: BodyEncoding::Json, graphql_query: None,
            extra_headers: vec![], idempotency_header: None,
            pagination: Some(Pagination {
                style: PaginationStyle::Cursor,
                cursor_arg: Some("cursor".into()),
                next_cursor_field: Some("next_cursor".into()),
                has_more_field: None,
                offset_arg: None, page_size: None,
                items_field: "data".into(),
                max_pages: 10,
            }),
        };
        let manifest = manifest_with_auth("pagtest1", &base_url, AuthType::None, vec![], op.clone());
        let creds = provider_store::ResolvedProviderCredentials {
            profile_name: "default".into(), account: "pagtest1-acc".into(),
            api_url: base_url.clone(), fields: HashMap::new(),
        };
        let client = GenericClient::new(&manifest, &creds);

        let result = client.execute(&op, &HashMap::new()).await.unwrap();
        let items: Vec<&str> = result["items"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
        assert_eq!(items, vec!["a", "b", "c", "d", "e"]);
        assert_eq!(result["pages_fetched"], 3);
        assert_eq!(result["truncated"], false);

        let reqs = captured.lock().unwrap();
        assert_eq!(reqs.len(), 3);
        assert!(!reqs[0].path.contains('?') || !reqs[0].path.contains("cursor="), "1re requête ne doit pas avoir de curseur");
    }

    #[tokio::test]
    async fn cursor_pagination_respects_max_pages_and_reports_truncated() {
        let (base_url, _captured, _h) = spawn_test_server(|_req| {
            // Annonce TOUJOURS une page suivante — sans le garde-fou
            // max_pages, cette boucle ne s'arrêterait jamais.
            (200, br#"{"data": ["x"], "next_cursor": "toujours-une-suite"}"#.to_vec())
        }).await;

        let op = Operation {
            path: vec!["item".into(), "list".into()],
            method: "GET".into(), endpoint: "/items".into(),
            summary: "s".into(), danger: Danger::Safe,
            args: vec![OperationArg {
                id: "cursor".into(), long: Some("cursor".into()), positional: false, required: false,
                help: "h".into(), location: ArgLocation::Query, field: None,
            }],
            example: None, response_fields: vec![],
            body_encoding: BodyEncoding::Json, graphql_query: None,
            extra_headers: vec![], idempotency_header: None,
            pagination: Some(Pagination {
                style: PaginationStyle::Cursor,
                cursor_arg: Some("cursor".into()),
                next_cursor_field: Some("next_cursor".into()),
                has_more_field: None,
                offset_arg: None, page_size: None,
                items_field: "data".into(),
                max_pages: 4,
            }),
        };
        let manifest = manifest_with_auth("pagtest2", &base_url, AuthType::None, vec![], op.clone());
        let creds = provider_store::ResolvedProviderCredentials {
            profile_name: "default".into(), account: "pagtest2-acc".into(),
            api_url: base_url.clone(), fields: HashMap::new(),
        };
        let client = GenericClient::new(&manifest, &creds);

        let result = client.execute(&op, &HashMap::new()).await.unwrap();
        assert_eq!(result["pages_fetched"], 4, "doit s'arrêter pile à max_pages");
        assert_eq!(result["items"].as_array().unwrap().len(), 4);
        assert_eq!(result["truncated"], true, "doit signaler qu'il restait probablement de la donnée");
    }

    #[tokio::test]
    async fn offset_pagination_stops_on_short_page() {
        let (base_url, captured, _h) = spawn_test_server(|req| {
            if req.path.contains("offset=20") {
                (200, br#"{"data": ["k1"]}"#.to_vec()) // page courte = dernière page (< page_size=10)
            } else {
                (200, br#"{"data": ["a","b","c","d","e","f","g","h","i","j"]}"#.to_vec())
            }
        }).await;

        let op = Operation {
            path: vec!["item".into(), "list".into()],
            method: "GET".into(), endpoint: "/items".into(),
            summary: "s".into(), danger: Danger::Safe,
            args: vec![OperationArg {
                id: "offset".into(), long: Some("offset".into()), positional: false, required: false,
                help: "h".into(), location: ArgLocation::Query, field: None,
            }],
            example: None, response_fields: vec![],
            body_encoding: BodyEncoding::Json, graphql_query: None,
            extra_headers: vec![], idempotency_header: None,
            pagination: Some(Pagination {
                style: PaginationStyle::Offset,
                cursor_arg: None, next_cursor_field: None, has_more_field: None,
                offset_arg: Some("offset".into()),
                page_size: Some(10),
                items_field: "data".into(),
                max_pages: 10,
            }),
        };
        let manifest = manifest_with_auth("pagtest3", &base_url, AuthType::None, vec![], op.clone());
        let creds = provider_store::ResolvedProviderCredentials {
            profile_name: "default".into(), account: "pagtest3-acc".into(),
            api_url: base_url.clone(), fields: HashMap::new(),
        };
        let client = GenericClient::new(&manifest, &creds);

        let result = client.execute(&op, &HashMap::new()).await.unwrap();
        assert_eq!(result["items"].as_array().unwrap().len(), 21, "10 + 10 + 1 (page courte)");
        assert_eq!(result["pages_fetched"], 3);
        assert_eq!(result["truncated"], false);

        let reqs = captured.lock().unwrap();
        assert_eq!(reqs.len(), 3);
        assert!(reqs[0].path == "/items" || reqs[0].path.contains("offset=0"), "1re page: {}", reqs[0].path);
        assert!(reqs[1].path.contains("offset=10"), "2e page: {}", reqs[1].path);
        assert!(reqs[2].path.contains("offset=20"), "3e page: {}", reqs[2].path);
    }

    #[tokio::test]
    async fn graphql_cursor_pagination_uses_variables_not_query_string() {
        // Compose GraphQL + pagination cursor (style Relay : pageInfo /
        // endCursor) — le curseur doit être injecté comme variable
        // GraphQL, pas en query string, puisque l'arg est location="body".
        use std::sync::atomic::{AtomicUsize, Ordering};
        let call_count = std::sync::Arc::new(AtomicUsize::new(0));
        let call_count_srv = call_count.clone();

        let (base_url, captured, _h) = spawn_test_server(move |_req| {
            let n = call_count_srv.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                (200, br#"{"data": {"items": ["a", "b"]}, "page_info": {"end_cursor": "cur2", "has_next": true}}"#.to_vec())
            } else {
                (200, br#"{"data": {"items": ["c"]}, "page_info": {"end_cursor": null, "has_next": false}}"#.to_vec())
            }
        }).await;

        let op = Operation {
            path: vec!["issue".into(), "list".into()],
            method: "POST".into(), endpoint: "/graphql".into(),
            summary: "s".into(), danger: Danger::Safe,
            args: vec![OperationArg {
                id: "after".into(), long: None, positional: false, required: false,
                help: "h".into(), location: ArgLocation::Body, field: None,
            }],
            example: None, response_fields: vec![],
            body_encoding: BodyEncoding::Json,
            graphql_query: Some("query($after: String) { issues(after: $after) { items } }".into()),
            extra_headers: vec![], idempotency_header: None,
            pagination: Some(Pagination {
                style: PaginationStyle::Cursor,
                cursor_arg: Some("after".into()),
                next_cursor_field: Some("page_info.end_cursor".into()),
                has_more_field: Some("page_info.has_next".into()),
                offset_arg: None, page_size: None,
                items_field: "data.items".into(),
                max_pages: 10,
            }),
        };
        let manifest = manifest_with_auth("pagtest4", &base_url, AuthType::None, vec![], op.clone());
        let creds = provider_store::ResolvedProviderCredentials {
            profile_name: "default".into(), account: "pagtest4-acc".into(),
            api_url: base_url.clone(), fields: HashMap::new(),
        };
        let client = GenericClient::new(&manifest, &creds);

        let result = client.execute(&op, &HashMap::new()).await.unwrap();
        let items: Vec<&str> = result["items"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
        assert_eq!(items, vec!["a", "b", "c"]);
        assert_eq!(result["pages_fetched"], 2);

        let reqs = captured.lock().unwrap();
        let second_body: Value = serde_json::from_slice(&reqs[1].body).unwrap();
        assert_eq!(second_body["variables"]["after"], "cur2", "le curseur doit être passé en variable GraphQL");
    }
}
