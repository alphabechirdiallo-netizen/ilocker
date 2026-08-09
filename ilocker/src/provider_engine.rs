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
use crate::provider_manifest::{ArgLocation, AuthType, Operation, ProviderManifest};
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
        }
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
        if let Some((h, v)) = self.auth_header_pair()? {
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

        if let Some((h, v)) = self.auth_header_pair()? {
            builder = builder.header(h, v);
        }

        let body = if matches!(method, Method::POST | Method::PUT | Method::PATCH) {
            builder = builder.header("Content-Type", "application/json");
            Body::from(serde_json::to_vec(&Value::Object(body_map))?)
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
    use crate::provider_manifest::{ApiConfig, AuthSchema, OperationArg, ProviderIdentity};

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
            },
            api: ApiConfig { base_url: "https://api.testco.example".into() },
            operations: vec![
                Operation {
                    path: vec!["item".into(), "list".into()],
                    method: "GET".into(), endpoint: "/items".into(),
                    summary: "Liste".into(), danger: Danger::Safe,
                    args: vec![], example: None, response_fields: vec![],
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
}
