// ============================================================
//  provider_manifest.rs — Format déclaratif des providers tiers
//
//  Un « provider » ilocker (Linear, Stripe, GitLab, un outil interne
//  d'entreprise, …) est décrit intégralement par un fichier TOML.
//  CE FICHIER NE CONTIENT JAMAIS DE CODE — ni script, ni eval, ni
//  WASM. C'est une décision de sécurité délibérée, pas une limite
//  technique provisoire : un manifeste ne peut décrire QUE des
//  appels HTTP vers des endpoints sous son propre `base_url`, avec
//  UN SEUL header d'authentification (celui déclaré dans [auth]) —
//  jamais des headers arbitraires pilotés par l'utilisateur. Ça
//  ferme, par construction, toute la classe d'attaques "exfiltration
//  de credentials via plugin tiers" : il n'y a rien à cacher dans
//  une donnée structurée qu'on peut valider intégralement avant
//  exécution.
//
//  Ce module ne fait QUE parser et valider. L'exécution réelle des
//  appels HTTP vit dans provider_engine.rs ; le stockage des
//  identifiants vit dans provider_store.rs.
//
//  Compatibilité : le niveau de danger d'une opération réutilise
//  EXACTEMENT `commands::studio_docs::Danger` (Safe/Caution/
//  Destructive) — même vocabulaire que les commandes natives,
//  déjà consommé par `iloc studio manifest` et l'extension VS Code.
// ============================================================

use crate::commands::studio_docs::Danger;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Limite haute du fichier manifeste lui-même — un manifeste de
/// provider est une poignée de kilo-octets ; 256 KiB est déjà
/// extrêmement généreux et empêche un fichier pathologique de
/// ralentir le chargement à chaque démarrage de `iloc`.
pub const MAX_MANIFEST_BYTES: usize = 256 * 1024;

/// Nombre maximal d'opérations dans un seul manifeste — protège la
/// construction de l'arbre `clap::Command` dynamique (voir
/// provider_engine.rs) contre un manifeste pathologique.
pub const MAX_OPERATIONS: usize = 200;

/// Noms de commandes déjà utilisés par ilocker lui-même, ou
/// susceptibles d'entrer en collision avec le vocabulaire du CLI.
/// Un manifeste dont le slug figure ici est refusé au chargement —
/// avant même d'atteindre `main.rs`, aucune commande native ne peut
/// jamais être masquée par un provider tiers.
const RESERVED_SLUGS: &[&str] = &[
    "init", "save", "undo", "log", "status", "sentinel", "selfinstall",
    "update", "share", "clone", "completion", "studio", "hyperscale",
    "vault", "node", "config", "push", "pull", "cloud", "connect",
    "github", "vercel", "supabase", "deploy", "provider",
    "help", "version", "login", "logout", "whoami",
];

// ═══════════════════════════════════════════════════════════════
//  Schéma
// ═══════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProviderManifest {
    pub provider: ProviderIdentity,
    pub auth: AuthSchema,
    pub api: ApiConfig,
    #[serde(default, rename = "operations")]
    pub operations: Vec<Operation>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProviderIdentity {
    pub slug: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub author: String,
    #[serde(default = "default_version")]
    pub version: String,
    /// Version du FORMAT de manifeste (pas du provider) — permet au
    /// moteur d'évoluer sans casser les manifestes déjà publiés.
    /// Seule la valeur 1 est comprise par cette version d'ilocker.
    pub manifest_version: u32,
}

fn default_version() -> String { "0.1.0".to_string() }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthType {
    BearerToken,
    ApiKey,
    Basic,
    /// Aucune authentification requise (APIs publiques en lecture).
    None,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AuthField {
    /// Identifiant interne (ex: "token", "username", "password").
    pub id: String,
    /// Libellé affiché à `iloc connect`.
    pub label: String,
    /// true → saisie masquée (comme un mot de passe), jamais affichée
    /// ni loggée en clair par le moteur.
    #[serde(default = "default_true")]
    pub secret: bool,
}

fn default_true() -> bool { true }

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AuthSchema {
    #[serde(rename = "type")]
    pub auth_type: AuthType,
    /// Champs à demander à l'utilisateur. Pour bearer_token/api_key :
    /// un seul champ (conventionnellement id="token"). Pour basic :
    /// deux champs (id="username", id="password").
    #[serde(default)]
    pub fields: Vec<AuthField>,
    /// Header HTTP où injecter la valeur (ignoré si type = "basic",
    /// qui utilise toujours "Authorization").
    #[serde(default)]
    pub header: Option<String>,
    /// Préfixe avant la valeur dans le header (ex: "Bearer ", "token ").
    /// Chaîne vide par défaut (clé API nue).
    #[serde(default)]
    pub value_prefix: String,
    /// Endpoint GET (relatif à base_url) appelé par `iloc connect`
    /// pour vérifier immédiatement que les identifiants sont valides.
    #[serde(default)]
    pub verify_endpoint: Option<String>,
    /// Champ du JSON de réponse de verify_endpoint affiché comme
    /// confirmation (ex: "email", "login", "name").
    #[serde(default)]
    pub verify_field: Option<String>,
    /// Où obtenir des identifiants — affiché si la connexion échoue.
    #[serde(default)]
    pub help_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ApiConfig {
    pub base_url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ArgLocation {
    Query,
    Body,
    Path,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OperationArg {
    pub id: String,
    /// Nom du flag long (ex: "team" → --team). `None` avec
    /// `positional = true` → argument positionnel.
    #[serde(default)]
    pub long: Option<String>,
    #[serde(default)]
    pub positional: bool,
    #[serde(default)]
    pub required: bool,
    pub help: String,
    #[serde(default = "default_location")]
    pub location: ArgLocation,
    /// Nom du paramètre côté API si différent de `id`.
    #[serde(default)]
    pub field: Option<String>,
}

fn default_location() -> ArgLocation { ArgLocation::Query }

impl OperationArg {
    pub fn field_name(&self) -> &str {
        self.field.as_deref().unwrap_or(&self.id)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Operation {
    /// Chemin CLI, ex: ["issue","create"] → `iloc <slug> issue create`.
    pub path: Vec<String>,
    pub method: String,
    /// Relatif à base_url, ou URL absolue (doit alors être sous le
    /// même host que base_url — vérifié à la validation).
    pub endpoint: String,
    pub summary: String,
    #[serde(default = "default_danger")]
    pub danger: Danger,
    #[serde(default)]
    pub args: Vec<OperationArg>,
    #[serde(default)]
    pub example: Option<String>,
    /// Champs du JSON de réponse à afficher (sinon JSON brut indenté).
    #[serde(default)]
    pub response_fields: Vec<String>,
}

fn default_danger() -> Danger { Danger::Caution }

// ═══════════════════════════════════════════════════════════════
//  Parsing
// ═══════════════════════════════════════════════════════════════

/// Charge et valide un manifeste depuis des octets bruts (fichier
/// disque ou téléchargement du registre — même chemin de validation
/// dans les deux cas, aucun traitement de confiance différencié).
pub fn parse(raw: &[u8]) -> Result<ProviderManifest> {
    if raw.len() > MAX_MANIFEST_BYTES {
        bail!(
            "Manifeste trop volumineux ({} octets, maximum {})",
            raw.len(), MAX_MANIFEST_BYTES
        );
    }
    let text = std::str::from_utf8(raw).context("Manifeste non-UTF8")?;
    let manifest: ProviderManifest = toml::from_str(text)
        .context("TOML de manifeste invalide")?;
    validate(&manifest)?;
    Ok(manifest)
}

pub fn parse_file(path: &std::path::Path) -> Result<ProviderManifest> {
    let raw = std::fs::read(path)
        .with_context(|| format!("Impossible de lire {}", path.display()))?;
    parse(&raw).with_context(|| format!("Manifeste invalide : {}", path.display()))
}

// ═══════════════════════════════════════════════════════════════
//  Validation — c'est ICI que vit la sécurité du système
// ═══════════════════════════════════════════════════════════════

pub fn validate(m: &ProviderManifest) -> Result<()> {
    validate_identity(&m.provider)?;
    validate_auth(&m.auth)?;
    let base_host = validate_base_url(&m.api.base_url)?;

    if m.operations.is_empty() {
        bail!("Le manifeste ne déclare aucune opération — au moins une est requise.");
    }
    if m.operations.len() > MAX_OPERATIONS {
        bail!(
            "{} opérations déclarées, maximum {}",
            m.operations.len(), MAX_OPERATIONS
        );
    }

    let mut seen_paths: HashSet<String> = HashSet::new();
    for op in &m.operations {
        validate_operation(op, &base_host)?;
        let key = op.path.join(".");
        if !seen_paths.insert(key.clone()) {
            bail!("Chemin de commande dupliqué : '{}'", key);
        }
    }

    // Aucun path ne doit être le préfixe strict d'un autre : sinon un
    // même nœud de l'arbre de commandes serait à la fois une opération
    // exécutable et un groupe de sous-commandes, ce que la construction
    // dynamique de clap::Command (provider_engine.rs) ne peut pas
    // représenter sans ambiguïté UX (`iloc x issue` ferait-il quelque
    // chose, ou faudrait-il toujours descendre plus loin ?).
    for a in &m.operations {
        for b in &m.operations {
            if a.path.len() < b.path.len() && b.path.starts_with(&a.path) {
                bail!(
                    "'{}' est un préfixe de '{}' — un chemin d'opération ne peut pas \
                     être à la fois une commande exécutable et un groupe",
                    a.path.join("."), b.path.join(".")
                );
            }
        }
    }

    Ok(())
}

fn validate_identity(id: &ProviderIdentity) -> Result<()> {
    if id.manifest_version != 1 {
        bail!(
            "manifest_version {} non supporté par cette version d'ilocker (seule la version 1 est comprise)",
            id.manifest_version
        );
    }

    let slug = &id.slug;
    if slug.len() < 2 || slug.len() > 32 {
        bail!("Le slug doit faire entre 2 et 32 caractères (reçu : '{}')", slug);
    }
    let mut chars = slug.chars();
    let first = chars.next().unwrap();
    if !first.is_ascii_lowercase() {
        bail!("Le slug doit commencer par une lettre minuscule (reçu : '{}')", slug);
    }
    if !slug.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
        bail!(
            "Le slug ne peut contenir que des minuscules, chiffres et tirets (reçu : '{}')",
            slug
        );
    }
    if RESERVED_SLUGS.contains(&slug.as_str()) {
        bail!(
            "'{}' est un nom de commande réservé par ilocker et ne peut pas être utilisé comme slug de provider",
            slug
        );
    }
    if id.name.trim().is_empty() {
        bail!("Le nom du provider ne peut pas être vide.");
    }
    Ok(())
}

fn validate_auth(auth: &AuthSchema) -> Result<()> {
    match auth.auth_type {
        AuthType::None => {
            if !auth.fields.is_empty() {
                bail!("auth.type = \"none\" ne doit déclarer aucun champ dans auth.fields");
            }
        }
        AuthType::Basic => {
            let ids: Vec<&str> = auth.fields.iter().map(|f| f.id.as_str()).collect();
            if ids != ["username", "password"] {
                bail!(
                    "auth.type = \"basic\" exige exactement deux champs, dans l'ordre : \
                     id=\"username\" puis id=\"password\" (reçu : {:?})",
                    ids
                );
            }
        }
        AuthType::BearerToken | AuthType::ApiKey => {
            if auth.fields.len() != 1 {
                bail!(
                    "auth.type = \"{:?}\" exige exactement un champ dans auth.fields (reçu : {})",
                    auth.auth_type, auth.fields.len()
                );
            }
            if auth.header.as_deref().unwrap_or("").trim().is_empty() {
                bail!("auth.header est requis pour auth.type = \"{:?}\"", auth.auth_type);
            }
        }
    }

    let mut seen_ids: HashSet<&str> = HashSet::new();
    for f in &auth.fields {
        if f.id.trim().is_empty() {
            bail!("Un champ auth.fields a un id vide");
        }
        if !seen_ids.insert(f.id.as_str()) {
            bail!("Champ auth.fields dupliqué : '{}'", f.id);
        }
        if f.label.trim().is_empty() {
            bail!("Le champ auth '{}' doit avoir un label non vide", f.id);
        }
    }

    Ok(())
}

/// Valide `base_url` et retourne le host normalisé (utilisé ensuite
/// pour vérifier que chaque endpoint absolu reste sous ce host).
///
/// Règle : HTTPS obligatoire, SAUF exception explicite et étroite
/// pour 127.0.0.1 / localhost — nécessaire à `iloc provider test`
/// contre un serveur de développement local, et cohérente avec des
/// standards établis (RFC 8252 autorise de même http://127.0.0.1
/// pour les redirections OAuth d'applications natives). Cette
/// exception ne s'appliquera JAMAIS à la publication sur le
/// registre public — voir provider_engine.rs::validate_for_publish.
fn validate_base_url(base_url: &str) -> Result<String> {
    let url = base_url.trim();
    if url.is_empty() {
        bail!("api.base_url ne peut pas être vide");
    }

    let is_https = url.starts_with("https://");
    let is_local_http = url.starts_with("http://127.0.0.1")
        || url.starts_with("http://localhost");

    if !is_https && !is_local_http {
        bail!(
            "api.base_url doit être en HTTPS (reçu : '{}'). \
             Seule exception : http://127.0.0.1 ou http://localhost, pour le développement local.",
            url
        );
    }

    extract_host(url)
}

/// Extrait `host[:port]` d'une URL, sans dépendance externe (pas de
/// crate `url` dans ce projet — extraction manuelle suffisante ici
/// car on ne valide qu'un host, jamais un chemin complexe).
fn extract_host(url: &str) -> Result<String> {
    let without_scheme = url
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    let host = without_scheme.split('/').next().unwrap_or(without_scheme);
    if host.is_empty() {
        bail!("URL sans host valide : '{}'", url);
    }
    Ok(host.to_lowercase())
}

fn validate_operation(op: &Operation, base_host: &str) -> Result<()> {
    if op.path.is_empty() {
        bail!("Une opération a un `path` vide");
    }
    for segment in &op.path {
        if segment.is_empty()
            || !segment.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            bail!(
                "Segment de path invalide : '{}' (minuscules, chiffres, tirets uniquement)",
                segment
            );
        }
    }

    let method = op.method.to_uppercase();
    if !["GET", "POST", "PUT", "PATCH", "DELETE"].contains(&method.as_str()) {
        bail!(
            "Méthode HTTP non supportée : '{}' (autorisées : GET, POST, PUT, PATCH, DELETE)",
            op.method
        );
    }

    if op.summary.trim().is_empty() {
        bail!("L'opération '{}' doit avoir un résumé (summary) non vide", op.path.join("."));
    }

    // ── Garde-fou anti-SSRF / anti-exfiltration ──────────────────
    // Un endpoint absolu doit rester sous le host de base_url. Sans
    // cette règle, un manifeste pourrait déclarer un endpoint pointant
    // vers un domaine tiers tout en gardant l'apparence d'un provider
    // légitime — c'est le seul vecteur d'exfiltration qu'un manifeste
    // purement déclaratif pourrait autrement offrir.
    if op.endpoint.starts_with("http://") || op.endpoint.starts_with("https://") {
        let endpoint_host = extract_host(&op.endpoint)?;
        if endpoint_host != base_host {
            bail!(
                "L'opération '{}' cible '{}', hors du host déclaré dans api.base_url ('{}'). \
                 Un endpoint absolu doit rester sous le même host.",
                op.path.join("."), endpoint_host, base_host
            );
        }
    }

    let mut seen_arg_ids: HashSet<&str> = HashSet::new();
    let mut positional_seen = false;
    for arg in &op.args {
        if arg.id.trim().is_empty() {
            bail!("L'opération '{}' a un argument avec un id vide", op.path.join("."));
        }
        if !seen_arg_ids.insert(arg.id.as_str()) {
            bail!(
                "Argument dupliqué '{}' dans l'opération '{}'",
                arg.id, op.path.join(".")
            );
        }
        if arg.positional {
            if arg.long.is_some() {
                bail!(
                    "L'argument '{}' de '{}' est positional mais déclare aussi `long` — les deux sont exclusifs",
                    arg.id, op.path.join(".")
                );
            }
            // Un seul argument positionnel par opération en v1 : la
            // construction dynamique de clap::Command (provider_engine.rs)
            // reste ainsi entièrement prévisible, sans les règles d'ordre
            // multi-positionnels de clap (tous requis sauf le dernier)
            // qu'il serait facile de mal reproduire dynamiquement.
            if positional_seen {
                bail!(
                    "L'opération '{}' déclare plusieurs arguments positional — un seul est autorisé par opération (v1)",
                    op.path.join(".")
                );
            }
            positional_seen = true;
        } else if arg.long.as_deref().unwrap_or("").trim().is_empty() {
            bail!(
                "L'argument '{}' de '{}' doit déclarer `long` (ou `positional = true`)",
                arg.id, op.path.join(".")
            );
        }

        if arg.location == ArgLocation::Path {
            let placeholder = format!("{{{}}}", arg.id);
            if !op.endpoint.contains(&placeholder) {
                bail!(
                    "L'argument '{}' de '{}' a location=\"path\" mais '{}' n'apparaît pas dans endpoint ('{}')",
                    arg.id, op.path.join("."), placeholder, op.endpoint
                );
            }
        }
    }
    let _ = positional_seen; // conservé pour lisibilité du flux ci-dessus

    // Tout placeholder {xxx} présent dans l'endpoint doit correspondre
    // à un argument déclaré avec location="path" — sinon l'opération
    // est invocable mais génèrera systématiquement une URL invalide.
    for placeholder in extract_placeholders(&op.endpoint) {
        let declared = op.args.iter().any(|a| a.location == ArgLocation::Path && a.id == placeholder);
        if !declared {
            bail!(
                "endpoint de '{}' référence '{{{}}}' mais aucun argument avec location=\"path\" et id=\"{}\" n'est déclaré",
                op.path.join("."), placeholder, placeholder
            );
        }
    }

    Ok(())
}

fn extract_placeholders(endpoint: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = endpoint;
    while let Some(start) = rest.find('{') {
        if let Some(end) = rest[start..].find('}') {
            out.push(rest[start + 1..start + end].to_string());
            rest = &rest[start + end + 1..];
        } else {
            break;
        }
    }
    out
}

/// Règles additionnelles, strictement plus sévères, appliquées
/// uniquement au moment de `iloc provider publish` (registre public).
/// `validate()` ci-dessus reste la seule barrière pour un usage privé
/// — la sécurité d'exécution ne dépend jamais du statut public/privé,
/// mais la qualité éditoriale minimale, elle, n'est exigée que pour
/// ce qui sera listé publiquement.
pub fn validate_for_publish(m: &ProviderManifest) -> Result<()> {
    validate(m)?;

    if !m.api.base_url.starts_with("https://") {
        bail!(
            "Publication refusée : api.base_url doit être en HTTPS pur (l'exception localhost \
             n'est valable que pour `iloc provider test` en local, jamais pour le registre public)."
        );
    }
    if m.provider.description.trim().is_empty() {
        bail!("Publication refusée : provider.description est requis pour le registre public.");
    }
    if m.provider.author.trim().is_empty() {
        bail!("Publication refusée : provider.author est requis pour le registre public.");
    }
    for op in &m.operations {
        if op.example.as_deref().unwrap_or("").trim().is_empty() {
            bail!(
                "Publication refusée : l'opération '{}' n'a pas d'exemple (`example`) — \
                 requis pour toute commande listée publiquement.",
                op.path.join(".")
            );
        }
    }
    Ok(())
}

// ═══════════════════════════════════════════════════════════════
//  Tests
// ═══════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_valid_toml() -> String {
        r#"
[provider]
slug = "testco"
name = "TestCo"
description = "Un provider de test"
author = "Test Author"
manifest_version = 1

[auth]
type = "bearer_token"
header = "Authorization"
value_prefix = "Bearer "
prompt_label = "Clé API TestCo"
[[auth.fields]]
id = "token"
label = "Clé API"

[api]
base_url = "https://api.testco.example"

[[operations]]
path = ["item", "list"]
method = "GET"
endpoint = "/items"
summary = "Liste les items"
danger = "safe"
"#.to_string()
    }

    #[test]
    fn parses_minimal_valid_manifest() {
        let m = parse(minimal_valid_toml().as_bytes()).expect("doit parser");
        assert_eq!(m.provider.slug, "testco");
        assert_eq!(m.operations.len(), 1);
        assert_eq!(m.operations[0].path, vec!["item", "list"]);
    }

    #[test]
    fn rejects_reserved_slug() {
        let toml = minimal_valid_toml().replace("slug = \"testco\"", "slug = \"github\"");
        let err = parse(toml.as_bytes()).unwrap_err();
        assert!(err.to_string().contains("réservé"), "erreur inattendue : {err}");
    }

    #[test]
    fn rejects_uppercase_slug() {
        let toml = minimal_valid_toml().replace("slug = \"testco\"", "slug = \"TestCo\"");
        assert!(parse(toml.as_bytes()).is_err());
    }

    #[test]
    fn rejects_http_base_url_non_local() {
        let toml = minimal_valid_toml()
            .replace("https://api.testco.example", "http://api.testco.example");
        let err = parse(toml.as_bytes()).unwrap_err();
        assert!(err.to_string().contains("HTTPS"), "erreur inattendue : {err}");
    }

    #[test]
    fn allows_http_localhost_for_dev() {
        let toml = minimal_valid_toml()
            .replace("https://api.testco.example", "http://127.0.0.1:8080");
        assert!(parse(toml.as_bytes()).is_ok(), "127.0.0.1 doit être autorisé en http");
    }

    #[test]
    fn allows_http_localhost_named() {
        let toml = minimal_valid_toml()
            .replace("https://api.testco.example", "http://localhost:8080");
        assert!(parse(toml.as_bytes()).is_ok());
    }

    #[test]
    fn rejects_endpoint_pointing_to_foreign_host() {
        let mut toml = minimal_valid_toml();
        toml.push_str(
            r#"
[[operations]]
path = ["evil"]
method = "GET"
endpoint = "https://attacker.example/steal"
summary = "Malveillant"
danger = "safe"
"#,
        );
        let err = parse(toml.as_bytes()).unwrap_err();
        assert!(err.to_string().contains("host"), "erreur inattendue : {err}");
    }

    #[test]
    fn accepts_absolute_endpoint_on_same_host() {
        let mut toml = minimal_valid_toml();
        toml.push_str(
            r#"
[[operations]]
path = ["item", "special"]
method = "GET"
endpoint = "https://api.testco.example/v2/special"
summary = "Cas particulier v2"
danger = "safe"
"#,
        );
        assert!(parse(toml.as_bytes()).is_ok());
    }

    #[test]
    fn rejects_duplicate_operation_path() {
        let mut toml = minimal_valid_toml();
        toml.push_str(
            r#"
[[operations]]
path = ["item", "list"]
method = "GET"
endpoint = "/items-v2"
summary = "Doublon"
danger = "safe"
"#,
        );
        let err = parse(toml.as_bytes()).unwrap_err();
        assert!(err.to_string().contains("dupliqué"), "erreur inattendue : {err}");
    }

    #[test]
    fn rejects_unsupported_http_method() {
        let toml = minimal_valid_toml().replace("method = \"GET\"", "method = \"TRACE\"");
        assert!(parse(toml.as_bytes()).is_err());
    }

    #[test]
    fn rejects_manifest_version_zero() {
        let toml = minimal_valid_toml().replace("manifest_version = 1", "manifest_version = 0");
        let err = parse(toml.as_bytes()).unwrap_err();
        assert!(err.to_string().contains("manifest_version"), "erreur inattendue : {err}");
    }

    #[test]
    fn rejects_path_arg_missing_placeholder() {
        // Argument déclaré location="path" (id="id") mais son
        // placeholder {id} n'apparaît PAS dans endpoint → doit être
        // rejeté (sans quoi l'opération produirait toujours une URL
        // qui ignore silencieusement la valeur fournie par l'utilisateur).
        let mut toml = minimal_valid_toml();
        toml.push_str(
            r#"
[[operations]]
path = ["item", "view"]
method = "GET"
endpoint = "/items"
summary = "Voir un item"
danger = "safe"
[[operations.args]]
id = "id"
positional = true
required = true
help = "Identifiant de l'item"
location = "path"
"#,
        );
        let err = parse(toml.as_bytes()).unwrap_err();
        assert!(err.to_string().contains("id"), "erreur inattendue : {err}");
    }

    #[test]
    fn rejects_placeholder_without_declared_arg() {
        let mut toml = minimal_valid_toml();
        toml.push_str(
            r#"
[[operations]]
path = ["item", "view"]
method = "GET"
endpoint = "/items/{id}"
summary = "Voir un item"
danger = "safe"
"#,
        );
        let err = parse(toml.as_bytes()).unwrap_err();
        assert!(err.to_string().contains("id"), "erreur inattendue : {err}");
    }

    #[test]
    fn accepts_valid_path_arg() {
        let mut toml = minimal_valid_toml();
        toml.push_str(
            r#"
[[operations]]
path = ["item", "view"]
method = "GET"
endpoint = "/items/{id}"
summary = "Voir un item"
danger = "safe"
[[operations.args]]
id = "id"
positional = true
required = true
help = "Identifiant de l'item"
location = "path"
"#,
        );
        assert!(parse(toml.as_bytes()).is_ok());
    }

    #[test]
    fn rejects_positional_with_long() {
        let mut toml = minimal_valid_toml();
        toml.push_str(
            r#"
[[operations]]
path = ["item", "bad"]
method = "GET"
endpoint = "/items"
summary = "Invalide"
danger = "safe"
[[operations.args]]
id = "x"
long = "x"
positional = true
required = true
help = "conflit"
location = "query"
"#,
        );
        assert!(parse(toml.as_bytes()).is_err());
    }

    #[test]
    fn basic_auth_requires_exact_fields() {
        let toml = minimal_valid_toml()
            .replace("type = \"bearer_token\"", "type = \"basic\"")
            .replace(
                "[[auth.fields]]\nid = \"token\"\nlabel = \"Clé API\"",
                "[[auth.fields]]\nid = \"token\"\nlabel = \"Clé API\"",
            );
        // basic avec un seul champ "token" (au lieu de username+password) doit échouer
        let err = parse(toml.as_bytes()).unwrap_err();
        assert!(err.to_string().contains("basic"), "erreur inattendue : {err}");
    }

    #[test]
    fn basic_auth_accepts_username_password() {
        let toml = minimal_valid_toml()
            .replace("type = \"bearer_token\"", "type = \"basic\"")
            .replace(
                "[[auth.fields]]\nid = \"token\"\nlabel = \"Clé API\"",
                "[[auth.fields]]\nid = \"username\"\nlabel = \"Utilisateur\"\n[[auth.fields]]\nid = \"password\"\nlabel = \"Mot de passe\"",
            );
        assert!(parse(toml.as_bytes()).is_ok());
    }

    #[test]
    fn rejects_manifest_exceeding_size_limit() {
        let huge = vec![b'#'; MAX_MANIFEST_BYTES + 1];
        let err = parse(&huge).unwrap_err();
        assert!(err.to_string().contains("volumineux"), "erreur inattendue : {err}");
    }

    #[test]
    fn publish_requires_https_even_if_local_test_passed() {
        let toml = minimal_valid_toml()
            .replace("https://api.testco.example", "http://127.0.0.1:9999");
        let m = parse(toml.as_bytes()).expect("doit passer la validation locale");
        let err = validate_for_publish(&m).unwrap_err();
        assert!(err.to_string().contains("HTTPS"), "erreur inattendue : {err}");
    }

    #[test]
    fn publish_requires_examples_on_every_operation() {
        let m = parse(minimal_valid_toml().as_bytes()).unwrap();
        let err = validate_for_publish(&m).unwrap_err();
        assert!(err.to_string().contains("exemple"), "erreur inattendue : {err}");
    }

    #[test]
    fn rejects_path_that_is_prefix_of_another() {
        let mut toml = minimal_valid_toml();
        toml.push_str(
            r#"
[[operations]]
path = ["item"]
method = "GET"
endpoint = "/items/default"
summary = "Ambigu : à la fois feuille et groupe"
danger = "safe"
"#,
        );
        // "item" (déjà utilisé comme groupe via "item.list" dans le
        // manifeste minimal) devient ici AUSSI une feuille exécutable —
        // ambiguïté qui doit être rejetée.
        let err = parse(toml.as_bytes()).unwrap_err();
        assert!(err.to_string().contains("préfixe"), "erreur inattendue : {err}");
    }

    #[test]
    fn rejects_two_positional_args_in_same_operation() {
        let mut toml = minimal_valid_toml();
        toml.push_str(
            r#"
[[operations]]
path = ["item", "move"]
method = "POST"
endpoint = "/items/move"
summary = "Déplace un item"
danger = "caution"
[[operations.args]]
id = "from"
positional = true
required = true
help = "Source"
location = "body"
[[operations.args]]
id = "to"
positional = true
required = true
help = "Destination"
location = "body"
"#,
        );
        let err = parse(toml.as_bytes()).unwrap_err();
        assert!(err.to_string().contains("positional"), "erreur inattendue : {err}");
    }

    #[test]
    fn extract_placeholders_finds_all() {
        assert_eq!(
            extract_placeholders("/orgs/{org}/repos/{repo}/issues/{number}"),
            vec!["org", "repo", "number"]
        );
        assert_eq!(extract_placeholders("/items"), Vec::<String>::new());
    }
}
