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
    /// OAuth2 "client credentials" (RFC 6749 §4.4) — machine-à-machine,
    /// sans utilisateur final. Couvre Azure AD (app-only), et la plupart
    /// des APIs SaaS proposant un flux M2M par client_id/client_secret.
    /// Le moteur obtient, met en cache et renouvelle le jeton lui-même —
    /// le manifeste ne voit jamais le jeton, seulement client_id/secret.
    /// Renommage explicite requis : rename_all="snake_case" convertirait
    /// sinon "OAuth2ClientCredentials" en "o_auth2_client_credentials"
    /// (majuscules O/A consécutives traitées comme une frontière de mot) —
    /// vérifié empiriquement, piège classique de la conversion de casse
    /// automatique sur un identifiant contenant un acronyme.
    #[serde(rename = "oauth2_client_credentials")]
    OAuth2ClientCredentials,
    /// OAuth2 JWT-bearer avec compte de service (RFC 7523), tel qu'utilisé
    /// par Google Cloud : l'utilisateur fournit le JSON de compte de
    /// service téléchargé depuis la console GCP (un seul champ secret),
    /// le moteur en extrait client_email/private_key/token_uri, construit
    /// et signe (RS256) un JWT, puis l'échange contre un jeton d'accès.
    /// Même raison de renommage explicite que OAuth2ClientCredentials.
    #[serde(rename = "oauth2_service_account")]
    OAuth2ServiceAccount,
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
    /// URL du endpoint de token OAuth2 — requis pour
    /// oauth2_client_credentials. Ignoré pour oauth2_service_account
    /// (le `token_uri` du JSON de compte de service fait foi). Peut être
    /// sur un host DIFFÉRENT de api.base_url (ex: login.microsoftonline.com
    /// vs graph.microsoft.com) — c'est un fonctionnement OAuth2 normal,
    /// pas une exception au garde-fou anti-SSRF : ce dernier protège les
    /// *endpoints d'opération* contre l'exfiltration vers un tiers ; ici,
    /// la même transparence s'applique déjà à api.base_url lui-même — un
    /// manifeste malveillant pourrait de toute façon y pointer un host
    /// arbitraire, le TOML restant lisible et auditable avant toute
    /// confiance accordée par `iloc connect`.
    #[serde(default)]
    pub token_url: Option<String>,
    /// Scope(s) OAuth2 optionnel(s), envoyé(s) tel quel au endpoint de
    /// token (chaîne séparée par des espaces, convention OAuth2 standard).
    #[serde(default)]
    pub scope: Option<String>,
    /// oauth2_service_account uniquement : valeur du claim JWT `aud` si
    /// différente du `token_uri` du fichier de compte de service (rare).
    #[serde(default)]
    pub jwt_audience: Option<String>,
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

/// Encodage du corps pour POST/PUT/PATCH. JSON par défaut ; certaines
/// APIs historiques ou minimalistes (Stripe, entre autres) exigent
/// `application/x-www-form-urlencoded` — jamais les deux mélangés dans
/// la même opération.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BodyEncoding {
    #[default]
    Json,
    Form,
}

/// En-tête HTTP additionnel STATIQUE — `value` est un littéral figé au
/// moment de l'écriture du manifeste, JAMAIS un template substitué avec
/// un argument ou un secret (vérifié à la validation : `value` ne peut
/// contenir aucune accolade). C'est ce qui préserve intact l'invariant de
/// sécurité du fichier : le SEUL header piloté par une donnée utilisateur
/// reste le header d'authentification unique construit dans
/// provider_engine.rs::auth_header_pair(). `extra_headers` ne fait
/// qu'ajouter des constantes (ex: "Stripe-Version: 2024-06-20").
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct StaticHeader {
    pub name: String,
    pub value: String,
}

/// Stratégie de pagination automatique. Dans les deux cas, le moteur
/// pilote lui-même un argument DÉJÀ déclaré dans `operations.args` (voir
/// `cursor_arg`/`offset_arg`) — aucun mécanisme séparé de placement de
/// valeur n'est nécessaire : la substitution query/body/GraphQL déjà en
/// place pour cet argument s'applique automatiquement à chaque page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PaginationStyle {
    /// Curseur opaque renvoyé par l'API elle-même (Stripe, GraphQL Relay
    /// via pageInfo.endCursor, la plupart des APIs modernes).
    Cursor,
    /// Page/offset numérique incrémenté par le moteur (APIs REST plus
    /// anciennes ou plus simples).
    Offset,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Pagination {
    pub style: PaginationStyle,
    /// id d'un argument DÉJÀ déclaré dans operations.args, que le moteur
    /// pilote automatiquement (requis pour style="cursor").
    #[serde(default)]
    pub cursor_arg: Option<String>,
    /// Chemin pointé (ex: "page_info.end_cursor") dans la réponse JSON où
    /// trouver le curseur de la page suivante (requis pour style="cursor").
    #[serde(default)]
    pub next_cursor_field: Option<String>,
    /// Chemin pointé optionnel vers un booléen indiquant s'il reste des
    /// pages — en son absence, le moteur s'arrête dès que
    /// next_cursor_field est absent/null/vide.
    #[serde(default)]
    pub has_more_field: Option<String>,
    /// id d'un argument DÉJÀ déclaré dans operations.args, incrémenté par
    /// le moteur de `page_size` à chaque page (requis pour style="offset").
    #[serde(default)]
    pub offset_arg: Option<String>,
    /// Nombre d'éléments par page (requis pour style="offset") — le moteur
    /// s'arrête dès qu'une page renvoie moins d'éléments que cette valeur.
    #[serde(default)]
    pub page_size: Option<u32>,
    /// Chemin pointé vers le tableau d'éléments à concaténer entre les
    /// pages (requis, les deux styles).
    pub items_field: String,
    /// Garde-fou obligatoire contre une pagination sans fin (API buguée,
    /// curseur qui ne progresse jamais, etc.) — jamais de valeur par
    /// défaut implicite, un manifeste doit choisir cette limite en
    /// connaissance de cause.
    pub max_pages: u32,
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
    /// Encodage du corps — voir BodyEncoding. Sans effet sur GET/DELETE.
    #[serde(default)]
    pub body_encoding: BodyEncoding,
    /// Si renseigné, cette opération est un appel GraphQL plutôt que REST :
    /// `endpoint` est l'URL unique du endpoint GraphQL, `method` doit être
    /// "POST", et tous les arguments doivent avoir location="body" — ils
    /// deviennent les `variables` GraphQL (nommées par leur `field`/`id`),
    /// jamais interpolés dans le texte de la requête elle-même. Le texte
    /// ici DOIT utiliser des variables nommées GraphQL (`$nom`), jamais
    /// de valeurs littérales injectées — c'est le moteur qui envoie
    /// {"query": ..., "variables": {...}} en un seul appel JSON.
    #[serde(default)]
    pub graphql_query: Option<String>,
    /// En-têtes HTTP additionnels, valeurs figées — voir StaticHeader.
    #[serde(default)]
    pub extra_headers: Vec<StaticHeader>,
    /// Nom du header à remplir avec un UUID v4 généré par le moteur à
    /// chaque invocation (ex: "Idempotency-Key" pour Stripe). La valeur
    /// n'est JAMAIS fournie par le manifeste ni par l'utilisateur.
    #[serde(default)]
    pub idempotency_header: Option<String>,
    /// Pagination automatique — si renseignée, `iloc <slug> <op>` page
    /// lui-même à travers TOUTES les pages disponibles (jusqu'à max_pages)
    /// et renvoie un résultat déjà concaténé, sans que l'utilisateur ait
    /// besoin de connaître le mécanisme de pagination de l'API sous-jacente.
    #[serde(default)]
    pub pagination: Option<Pagination>,
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
        AuthType::OAuth2ClientCredentials => {
            let ids: Vec<&str> = auth.fields.iter().map(|f| f.id.as_str()).collect();
            if ids != ["client_id", "client_secret"] {
                bail!(
                    "auth.type = \"oauth2_client_credentials\" exige exactement deux champs, \
                     dans l'ordre : id=\"client_id\" puis id=\"client_secret\" (reçu : {:?})",
                    ids
                );
            }
            match &auth.token_url {
                Some(u) if !u.trim().is_empty() => require_https_or_localhost(u, "auth.token_url")?,
                _ => bail!("auth.token_url est requis pour auth.type = \"oauth2_client_credentials\""),
            }
        }
        AuthType::OAuth2ServiceAccount => {
            let ids: Vec<&str> = auth.fields.iter().map(|f| f.id.as_str()).collect();
            if ids != ["service_account_json"] {
                bail!(
                    "auth.type = \"oauth2_service_account\" exige exactement un champ : \
                     id=\"service_account_json\" (reçu : {:?})",
                    ids
                );
            }
            // Pas de validation de auth.token_url ici : le token_uri du JSON
            // de compte de service fait foi au runtime (voir provider_engine.rs)
            // — un token_url de manifeste serait de toute façon ignoré.
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

/// Même règle HTTPS-ou-local que validate_base_url, mais sans extraction
/// de host (pas besoin de comparaison inter-host pour un token_url —
/// voir le commentaire sur AuthSchema::token_url). Délibérément une
/// fonction séparée plutôt qu'une factorisation avec validate_base_url :
/// cette dernière est déjà couverte par des tests qui verrouillent son
/// comportement exact, la retoucher pour la partager ajouterait un risque
/// de régression pour un gain de lisibilité marginal.
fn require_https_or_localhost(url: &str, field_name: &str) -> Result<()> {
    let is_https = url.starts_with("https://");
    let is_local_http = url.starts_with("http://127.0.0.1") || url.starts_with("http://localhost");
    if !is_https && !is_local_http {
        bail!(
            "{} doit être en HTTPS (reçu : '{}'). \
             Seule exception : http://127.0.0.1 ou http://localhost, pour le développement local.",
            field_name, url
        );
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

    // ── GraphQL : method=POST imposé, tous les args en location="body" ──
    // (ils deviennent des variables GraphQL, pas des query/path params —
    // voir Operation::graphql_query dans ce fichier).
    if op.graphql_query.is_some() {
        if method != "POST" {
            bail!(
                "L'opération '{}' déclare graphql_query mais method=\"{}\" — GraphQL exige POST",
                op.path.join("."), op.method
            );
        }
        for arg in &op.args {
            if arg.location != ArgLocation::Body {
                bail!(
                    "L'opération '{}' est GraphQL : l'argument '{}' doit avoir location=\"body\" \
                     (il devient une variable GraphQL), pas \"{:?}\"",
                    op.path.join("."), arg.id, arg.location
                );
            }
        }
    }

    // ── extra_headers : littéraux figés uniquement, jamais substitués ──
    // (voir StaticHeader) — deux garde-fous complémentaires : (1) aucune
    // accolade dans la valeur, pour bannir toute tentative de template
    // qui contournerait auth_header_pair()/resolve_auth_header() comme
    // seul point d'entrée pour un header piloté par une donnée
    // utilisateur/secrète ; (2) une liste noire de noms de headers déjà
    // gérés ailleurs par le moteur, qu'un manifeste ne doit jamais
    // pouvoir écraser.
    const RESERVED_HEADER_NAMES: &[&str] = &["authorization", "host", "content-length"];
    for h in &op.extra_headers {
        if h.name.trim().is_empty() {
            bail!("L'opération '{}' a un extra_headers avec un nom vide", op.path.join("."));
        }
        if RESERVED_HEADER_NAMES.contains(&h.name.to_ascii_lowercase().as_str()) {
            bail!(
                "L'opération '{}' déclare extra_headers avec le nom réservé '{}' — \
                 géré exclusivement par le moteur, jamais par un manifeste",
                op.path.join("."), h.name
            );
        }
        if h.value.contains('{') || h.value.contains('}') {
            bail!(
                "L'opération '{}' a un extra_header '{}' dont la valeur contient une accolade \
                 ('{}') — les valeurs de extra_headers doivent être des littéraux figés, \
                 jamais un template substitué avec un argument ou un secret",
                op.path.join("."), h.name, h.value
            );
        }
    }
    if let Some(header_name) = &op.idempotency_header {
        if header_name.trim().is_empty() {
            bail!("L'opération '{}' a idempotency_header vide", op.path.join("."));
        }
        if RESERVED_HEADER_NAMES.contains(&header_name.to_ascii_lowercase().as_str()) {
            bail!(
                "L'opération '{}' déclare idempotency_header sur le nom réservé '{}'",
                op.path.join("."), header_name
            );
        }
    }

    // ── Pagination automatique ────────────────────────────────────────
    if let Some(p) = &op.pagination {
        if p.items_field.trim().is_empty() {
            bail!("L'opération '{}' a pagination.items_field vide", op.path.join("."));
        }
        if p.max_pages == 0 {
            bail!(
                "L'opération '{}' a pagination.max_pages = 0 — doit être au moins 1",
                op.path.join(".")
            );
        }
        if p.max_pages > 500 {
            bail!(
                "L'opération '{}' a pagination.max_pages = {} — plafonné à 500 par prudence \
                 (une API qui a réellement besoin de plus de 500 pages a probablement un \
                 problème de curseur/offset qui ne progresse pas)",
                op.path.join("."), p.max_pages
            );
        }
        match p.style {
            PaginationStyle::Cursor => {
                let arg_id = p.cursor_arg.as_deref().unwrap_or("");
                if arg_id.is_empty() {
                    bail!(
                        "L'opération '{}' a pagination.style=\"cursor\" mais pas de cursor_arg",
                        op.path.join(".")
                    );
                }
                if !op.args.iter().any(|a| a.id == arg_id) {
                    bail!(
                        "L'opération '{}' a pagination.cursor_arg=\"{}\" mais aucun argument \
                         de ce id n'est déclaré dans operations.args",
                        op.path.join("."), arg_id
                    );
                }
                if p.next_cursor_field.as_deref().unwrap_or("").trim().is_empty() {
                    bail!(
                        "L'opération '{}' a pagination.style=\"cursor\" mais pas de next_cursor_field",
                        op.path.join(".")
                    );
                }
            }
            PaginationStyle::Offset => {
                let arg_id = p.offset_arg.as_deref().unwrap_or("");
                if arg_id.is_empty() {
                    bail!(
                        "L'opération '{}' a pagination.style=\"offset\" mais pas de offset_arg",
                        op.path.join(".")
                    );
                }
                if !op.args.iter().any(|a| a.id == arg_id) {
                    bail!(
                        "L'opération '{}' a pagination.offset_arg=\"{}\" mais aucun argument \
                         de ce id n'est déclaré dans operations.args",
                        op.path.join("."), arg_id
                    );
                }
                match p.page_size {
                    Some(0) | None => bail!(
                        "L'opération '{}' a pagination.style=\"offset\" mais page_size est \
                         absent ou nul (doit être au moins 1)",
                        op.path.join(".")
                    ),
                    _ => {}
                }
            }
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

    // ── OAuth2 / GraphQL / body_encoding / extra_headers (session août 2026) ──

    fn oauth2_client_credentials_toml() -> String {
        r#"
[provider]
slug = "azuretest"
name = "AzureTest"
description = "Un provider de test OAuth2"
author = "Test Author"
manifest_version = 1

[auth]
type = "oauth2_client_credentials"
token_url = "https://login.microsoftonline.com/common/oauth2/v2.0/token"
scope = "https://management.azure.com/.default"
prompt_label = "Identifiants Azure AD"
[[auth.fields]]
id = "client_id"
label = "Client ID"
[[auth.fields]]
id = "client_secret"
label = "Client Secret"

[api]
base_url = "https://management.azure.com"

[[operations]]
path = ["vm", "list"]
method = "GET"
endpoint = "/vms"
summary = "Liste les VMs"
danger = "safe"
"#.to_string()
    }

    #[test]
    fn oauth2_client_credentials_parses_when_well_formed() {
        let m = parse(oauth2_client_credentials_toml().as_bytes()).expect("doit parser");
        assert_eq!(m.auth.auth_type, AuthType::OAuth2ClientCredentials);
        assert_eq!(m.auth.token_url.as_deref(), Some("https://login.microsoftonline.com/common/oauth2/v2.0/token"));
    }

    #[test]
    fn oauth2_client_credentials_rejects_wrong_field_names() {
        let toml = oauth2_client_credentials_toml().replace("id = \"client_secret\"", "id = \"secret\"");
        let err = parse(toml.as_bytes()).unwrap_err();
        assert!(err.to_string().contains("client_id"), "erreur inattendue : {err}");
    }

    #[test]
    fn oauth2_client_credentials_rejects_single_field() {
        let toml = oauth2_client_credentials_toml().replace(
            "[[auth.fields]]\nid = \"client_secret\"\nlabel = \"Client Secret\"\n", ""
        );
        assert!(parse(toml.as_bytes()).is_err());
    }

    #[test]
    fn oauth2_client_credentials_requires_token_url() {
        let toml = oauth2_client_credentials_toml()
            .replace("token_url = \"https://login.microsoftonline.com/common/oauth2/v2.0/token\"\n", "");
        let err = parse(toml.as_bytes()).unwrap_err();
        assert!(err.to_string().contains("token_url"), "erreur inattendue : {err}");
    }

    #[test]
    fn oauth2_client_credentials_rejects_non_https_token_url() {
        let toml = oauth2_client_credentials_toml().replace(
            "https://login.microsoftonline.com/common/oauth2/v2.0/token",
            "http://login.microsoftonline.com/common/oauth2/v2.0/token",
        );
        let err = parse(toml.as_bytes()).unwrap_err();
        assert!(err.to_string().contains("HTTPS"), "erreur inattendue : {err}");
    }

    #[test]
    fn oauth2_client_credentials_allows_localhost_token_url_for_dev() {
        let toml = oauth2_client_credentials_toml().replace(
            "https://login.microsoftonline.com/common/oauth2/v2.0/token",
            "http://127.0.0.1:9999/token",
        );
        assert!(parse(toml.as_bytes()).is_ok());
    }

    fn oauth2_service_account_toml() -> String {
        r#"
[provider]
slug = "gcptest"
name = "GcpTest"
description = "Un provider de test GCP"
author = "Test Author"
manifest_version = 1

[auth]
type = "oauth2_service_account"
prompt_label = "JSON de compte de service GCP"
[[auth.fields]]
id = "service_account_json"
label = "Contenu du fichier JSON"

[api]
base_url = "https://compute.googleapis.com"

[[operations]]
path = ["instance", "list"]
method = "GET"
endpoint = "/instances"
summary = "Liste les instances"
danger = "safe"
"#.to_string()
    }

    #[test]
    fn oauth2_service_account_parses_when_well_formed() {
        let m = parse(oauth2_service_account_toml().as_bytes()).expect("doit parser");
        assert_eq!(m.auth.auth_type, AuthType::OAuth2ServiceAccount);
    }

    #[test]
    fn oauth2_service_account_rejects_wrong_field_name() {
        let toml = oauth2_service_account_toml().replace("id = \"service_account_json\"", "id = \"json_key\"");
        let err = parse(toml.as_bytes()).unwrap_err();
        assert!(err.to_string().contains("service_account_json"), "erreur inattendue : {err}");
    }

    #[test]
    fn oauth2_service_account_rejects_two_fields() {
        let toml = oauth2_service_account_toml().replace(
            "[[auth.fields]]\nid = \"service_account_json\"\nlabel = \"Contenu du fichier JSON\"\n",
            "[[auth.fields]]\nid = \"service_account_json\"\nlabel = \"x\"\n[[auth.fields]]\nid = \"extra\"\nlabel = \"y\"\n",
        );
        assert!(parse(toml.as_bytes()).is_err());
    }

    #[test]
    fn graphql_operation_requires_post_method() {
        let toml = minimal_valid_toml().replace(
            "method = \"GET\"\nendpoint = \"/items\"\nsummary = \"Liste les items\"\ndanger = \"safe\"",
            "method = \"GET\"\nendpoint = \"/graphql\"\nsummary = \"Requête GraphQL\"\ndanger = \"safe\"\ngraphql_query = \"query { items { id } }\"",
        );
        let err = parse(toml.as_bytes()).unwrap_err();
        assert!(err.to_string().contains("POST"), "erreur inattendue : {err}");
    }

    #[test]
    fn graphql_operation_requires_body_location_args() {
        let toml = minimal_valid_toml().replace(
            "method = \"GET\"\nendpoint = \"/items\"\nsummary = \"Liste les items\"\ndanger = \"safe\"",
            "method = \"POST\"\nendpoint = \"/graphql\"\nsummary = \"Requête GraphQL\"\ndanger = \"safe\"\n\
             graphql_query = \"query($id: String!) { item(id: $id) { id } }\"\n\n[[operations.args]]\n\
             id = \"id\"\npositional = true\nrequired = true\nhelp = \"id\"\nlocation = \"query\"",
        );
        let err = parse(toml.as_bytes()).unwrap_err();
        assert!(err.to_string().contains("GraphQL"), "erreur inattendue : {err}");
    }

    #[test]
    fn graphql_operation_with_body_args_and_post_is_valid() {
        let toml = minimal_valid_toml().replace(
            "method = \"GET\"\nendpoint = \"/items\"\nsummary = \"Liste les items\"\ndanger = \"safe\"",
            "method = \"POST\"\nendpoint = \"/graphql\"\nsummary = \"Requête GraphQL\"\ndanger = \"safe\"\n\
             graphql_query = \"query($id: String!) { item(id: $id) { id } }\"\n\n[[operations.args]]\n\
             id = \"id\"\npositional = true\nrequired = true\nhelp = \"id\"\nlocation = \"body\"",
        );
        let m = parse(toml.as_bytes()).expect("doit parser");
        assert!(m.operations[0].graphql_query.is_some());
    }

    #[test]
    fn body_encoding_defaults_to_json_when_absent() {
        let m = parse(minimal_valid_toml().as_bytes()).unwrap();
        assert_eq!(m.operations[0].body_encoding, BodyEncoding::Json);
    }

    #[test]
    fn body_encoding_form_parses_correctly() {
        let toml = minimal_valid_toml().replace(
            "danger = \"safe\"",
            "danger = \"safe\"\nbody_encoding = \"form\"",
        );
        let m = parse(toml.as_bytes()).unwrap();
        assert_eq!(m.operations[0].body_encoding, BodyEncoding::Form);
    }

    #[test]
    fn extra_header_with_braces_in_value_is_rejected() {
        let toml = minimal_valid_toml().replace(
            "danger = \"safe\"",
            "danger = \"safe\"\n\n[[operations.extra_headers]]\nname = \"X-Custom\"\nvalue = \"{token}\"",
        );
        let err = parse(toml.as_bytes()).unwrap_err();
        assert!(err.to_string().contains("accolade"), "erreur inattendue : {err}");
    }

    #[test]
    fn extra_header_with_reserved_name_is_rejected() {
        let toml = minimal_valid_toml().replace(
            "danger = \"safe\"",
            "danger = \"safe\"\n\n[[operations.extra_headers]]\nname = \"Authorization\"\nvalue = \"whatever\"",
        );
        let err = parse(toml.as_bytes()).unwrap_err();
        assert!(err.to_string().contains("réservé"), "erreur inattendue : {err}");
    }

    #[test]
    fn extra_header_literal_value_is_accepted() {
        let toml = minimal_valid_toml().replace(
            "danger = \"safe\"",
            "danger = \"safe\"\n\n[[operations.extra_headers]]\nname = \"Stripe-Version\"\nvalue = \"2024-06-20\"",
        );
        let m = parse(toml.as_bytes()).expect("doit parser");
        assert_eq!(m.operations[0].extra_headers[0].value, "2024-06-20");
    }

    #[test]
    fn idempotency_header_with_reserved_name_is_rejected() {
        let toml = minimal_valid_toml().replace(
            "danger = \"safe\"",
            "danger = \"safe\"\nidempotency_header = \"Host\"",
        );
        assert!(parse(toml.as_bytes()).is_err());
    }

    #[test]
    fn idempotency_header_valid_name_is_accepted() {
        let toml = minimal_valid_toml().replace(
            "danger = \"safe\"",
            "danger = \"safe\"\nidempotency_header = \"Idempotency-Key\"",
        );
        let m = parse(toml.as_bytes()).expect("doit parser");
        assert_eq!(m.operations[0].idempotency_header.as_deref(), Some("Idempotency-Key"));
    }

    // ── Pagination automatique (session août 2026) ────────────────────

    fn toml_with_cursor_arg_and_pagination(pagination_block: &str) -> String {
        minimal_valid_toml().replace(
            "danger = \"safe\"",
            &format!(
                "danger = \"safe\"\n\n[[operations.args]]\nid = \"cursor\"\nlong = \"cursor\"\n\
                 required = false\nhelp = \"curseur\"\nlocation = \"query\"\n\n{}",
                pagination_block
            ),
        )
    }

    #[test]
    fn cursor_pagination_valid_manifest_parses() {
        let toml = toml_with_cursor_arg_and_pagination(
            "[operations.pagination]\nstyle = \"cursor\"\ncursor_arg = \"cursor\"\n\
             next_cursor_field = \"next_cursor\"\nitems_field = \"data\"\nmax_pages = 20",
        );
        let m = parse(toml.as_bytes()).expect("doit parser");
        assert!(m.operations[0].pagination.is_some());
    }

    #[test]
    fn cursor_pagination_requires_cursor_arg_to_reference_declared_arg() {
        let toml = toml_with_cursor_arg_and_pagination(
            "[operations.pagination]\nstyle = \"cursor\"\ncursor_arg = \"nexiste_pas\"\n\
             next_cursor_field = \"next_cursor\"\nitems_field = \"data\"\nmax_pages = 20",
        );
        let err = parse(toml.as_bytes()).unwrap_err();
        assert!(err.to_string().contains("aucun argument"), "erreur inattendue : {err}");
    }

    #[test]
    fn cursor_pagination_requires_next_cursor_field() {
        let toml = toml_with_cursor_arg_and_pagination(
            "[operations.pagination]\nstyle = \"cursor\"\ncursor_arg = \"cursor\"\n\
             items_field = \"data\"\nmax_pages = 20",
        );
        let err = parse(toml.as_bytes()).unwrap_err();
        assert!(err.to_string().contains("next_cursor_field"), "erreur inattendue : {err}");
    }

    #[test]
    fn offset_pagination_requires_page_size() {
        let toml = minimal_valid_toml().replace(
            "danger = \"safe\"",
            "danger = \"safe\"\n\n[[operations.args]]\nid = \"offset\"\nlong = \"offset\"\n\
             required = false\nhelp = \"offset\"\nlocation = \"query\"\n\n\
             [operations.pagination]\nstyle = \"offset\"\noffset_arg = \"offset\"\n\
             items_field = \"data\"\nmax_pages = 20",
        );
        let err = parse(toml.as_bytes()).unwrap_err();
        assert!(err.to_string().contains("page_size"), "erreur inattendue : {err}");
    }

    #[test]
    fn offset_pagination_valid_manifest_parses() {
        let toml = minimal_valid_toml().replace(
            "danger = \"safe\"",
            "danger = \"safe\"\n\n[[operations.args]]\nid = \"offset\"\nlong = \"offset\"\n\
             required = false\nhelp = \"offset\"\nlocation = \"query\"\n\n\
             [operations.pagination]\nstyle = \"offset\"\noffset_arg = \"offset\"\n\
             page_size = 50\nitems_field = \"data\"\nmax_pages = 20",
        );
        let m = parse(toml.as_bytes()).expect("doit parser");
        assert_eq!(m.operations[0].pagination.as_ref().unwrap().page_size, Some(50));
    }

    #[test]
    fn pagination_max_pages_zero_is_rejected() {
        let toml = toml_with_cursor_arg_and_pagination(
            "[operations.pagination]\nstyle = \"cursor\"\ncursor_arg = \"cursor\"\n\
             next_cursor_field = \"next_cursor\"\nitems_field = \"data\"\nmax_pages = 0",
        );
        let err = parse(toml.as_bytes()).unwrap_err();
        assert!(err.to_string().contains("max_pages"), "erreur inattendue : {err}");
    }

    #[test]
    fn pagination_max_pages_above_500_is_rejected() {
        let toml = toml_with_cursor_arg_and_pagination(
            "[operations.pagination]\nstyle = \"cursor\"\ncursor_arg = \"cursor\"\n\
             next_cursor_field = \"next_cursor\"\nitems_field = \"data\"\nmax_pages = 501",
        );
        assert!(parse(toml.as_bytes()).is_err());
    }

    #[test]
    fn pagination_missing_items_field_is_rejected() {
        let toml = toml_with_cursor_arg_and_pagination(
            "[operations.pagination]\nstyle = \"cursor\"\ncursor_arg = \"cursor\"\n\
             next_cursor_field = \"next_cursor\"\nmax_pages = 20",
        );
        assert!(parse(toml.as_bytes()).is_err());
    }
}
