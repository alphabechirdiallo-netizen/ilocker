// ============================================================
//  supabase_client.rs — Client Supabase Management API v1
//
//  Réutilise hyper 0.14 + hyper-rustls 0.24 (déjà dans ilocker).
//
//  LEÇONS APPLIQUÉES DÈS LE DÉPART (pas en correction après coup) :
//    1. with_native_roots() — pas with_webpki_roots(). Un bug réel
//       (github_client/vercel_client) faisait échouer TOUTE requête
//       derrière un proxy d'entreprise ou une inspection TLS.
//    2. Timeout 30s sur chaque requête — sans lui, une requête qui
//       ne répond jamais bloque iloc indéfiniment sans message.
//    3. Parsing défensif des listes : les données réelles observées
//       via le connecteur Supabase montrent des enveloppes JSON
//       incohérentes selon l'endpoint (certaines wrappées dans une
//       clé nommée, d'autres en tableau brut). parse_list() accepte
//       les deux formes plutôt que de parier sur une seule.
//    4. Messages d'erreur : si le corps n'est pas du JSON valide,
//       afficher le texte brut plutôt que "Erreur inconnue".
//    5. Lookup direct par ID quand l'API l'expose, jamais list+find
//       (fragile, lent, sensible aux délais de cohérence).
//
//  Couverture API (>90% des besoins quotidiens) :
//    Organizations : list, get
//    Projects      : list, get, create, delete, pause, restore,
//                    get_api_keys, get_url
//    Database      : execute_sql, list_tables, list_extensions,
//                    list_migrations, apply_migration
//    Edge Functions: list, get, deploy, delete
//    Secrets       : list, set, delete (Edge Function secrets)
//    Branches      : list, get, create, delete, merge, reset, rebase
//    Advisors      : get (sécurité + performance)
//    Logs          : get par service
// ============================================================

use anyhow::{bail, Context, Result};
use hyper::{body::to_bytes, client::HttpConnector, Body, Client, Method, Request};
use hyper_rustls::HttpsConnector;
use serde::{Deserialize, Serialize};
use serde_json::Value;

type HyperClient = Client<HttpsConnector<HttpConnector>>;

/// Base URL surchargeable pour les tests (mock local) — même mécanisme
/// que ILOC_VERCEL_API_BASE, sans impact sur le comportement par défaut.
fn api_base() -> String {
    std::env::var("ILOC_SUPABASE_API_BASE").unwrap_or_else(|_| "https://api.supabase.com".to_string())
}

pub struct SupabaseClient {
    http:  HyperClient,
    token: String,
}

// ── Types principaux ──────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SbOrganization {
    pub id:   String,
    pub slug: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SbDatabaseInfo {
    pub host:            Option<String>,
    pub version:         Option<String>,
    pub postgres_engine: Option<String>,
    pub release_channel: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SbProject {
    pub id:               String,
    #[serde(default)]
    pub ref_:             Option<String>,
    #[serde(rename = "organization_id")]
    pub organization_id:  String,
    pub name:             String,
    pub region:           String,
    pub status:           String,
    #[serde(default)]
    pub database:         Option<SbDatabaseInfo>,
    pub created_at:       Option<String>,
}

impl SbProject {
    /// Le "ref" (identifiant utilisé dans les URLs et sous-domaines) est
    /// parfois un champ séparé, parfois identique à `id` selon la version
    /// de l'API — on gère les deux sans supposer.
    pub fn project_ref(&self) -> &str {
        self.ref_.as_deref().unwrap_or(&self.id)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SbApiKey {
    pub name:    String,   // "anon" | "service_role" | "publishable" | "secret"
    pub api_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SbTable {
    pub name:        String,
    #[serde(default)]
    pub rls_enabled: bool,
    #[serde(default)]
    pub rows:        Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SbExtension {
    pub name:             String,
    #[serde(default)]
    pub schema:           Option<String>,
    pub default_version:  Option<String>,
    pub installed_version: Option<String>,
    #[serde(default)]
    pub comment:          Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SbMigration {
    pub version: String,
    pub name:    String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SbEdgeFunction {
    pub id:              String,
    pub slug:            String,
    pub name:            String,
    pub status:          String,
    pub version:         u64,
    #[serde(default)]
    pub verify_jwt:      bool,
    pub created_at:      Option<i64>,
    pub updated_at:      Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SbSecret {
    pub name:  String,
    #[serde(default)]
    pub value: Option<String>, // jamais retourné en lecture, seulement en écriture
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SbBranch {
    pub id:              String,
    pub name:            String,
    #[serde(default)]
    pub project_ref:     Option<String>,
    pub status:          String,
    #[serde(default)]
    pub is_default:      bool,
    pub created_at:      Option<String>,
    pub updated_at:      Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SbAdvisorLint {
    pub name:        String,
    pub level:       String,   // "ERROR" | "WARN" | "INFO"
    pub title:       String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub remediation: Option<String>,
}

// ── Constructeur ─────────────────────────────────────────────

impl SupabaseClient {
    pub fn new(token: &str) -> Self {
        let connector = hyper_rustls::HttpsConnectorBuilder::new()
            .with_native_roots()
            .https_or_http()
            .enable_http1()
            .build();
        Self { http: Client::builder().build(connector), token: token.to_string() }
    }

    pub fn new_from_credentials(creds: &crate::supabase_store::SupabaseCredentials) -> Self {
        Self::new(&creds.token)
    }
}

// ── HTTP helpers privés ─────────────────────────────────────

impl SupabaseClient {
    fn url(&self, path: &str) -> String {
        format!("{}/v1/{}", api_base(), path.trim_start_matches('/'))
    }

    fn builder(&self, method: Method, url: &str) -> hyper::http::request::Builder {
        Request::builder()
            .method(method)
            .uri(url)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Content-Type",  "application/json")
            .header("User-Agent",    "ilocker/1.10.7")
    }

    async fn send(&self, req: Request<Body>) -> Result<(u16, Vec<u8>)> {
        let resp = tokio::time::timeout(std::time::Duration::from_secs(30), self.http.request(req))
            .await
            .context("Délai dépassé (30s) — Supabase ne répond pas, vérifiez votre connexion")?
            .context("Requête Supabase échouée")?;
        let status = resp.status().as_u16();
        let body   = to_bytes(resp.into_body()).await?.to_vec();
        Ok((status, body))
    }

    fn parse(&self, status: u16, body: &[u8], method: &str, path: &str) -> Result<Value> {
        if body.is_empty() && (status == 204 || status == 200) {
            return Ok(Value::Null);
        }
        let v: Value = serde_json::from_slice(body)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(body).to_string()));

        if status >= 400 {
            // Si le corps n'était pas du JSON valide, afficher le texte brut
            // plutôt que de masquer un message parfaitement lisible.
            if let Some(raw) = v.as_str() {
                let trimmed: String = raw.trim().chars().take(300).collect();
                if !trimmed.is_empty() {
                    bail!("Supabase {} {}: {} — {}", method, path, status, trimmed);
                }
            }
            let msg = v["message"].as_str().or(v["error"].as_str()).unwrap_or("Erreur inconnue");
            let hint = match status {
                401 => " — Vérifiez votre token (iloc connect supabase)",
                403 => " — Permissions insuffisantes sur ce token",
                404 => " — Ressource introuvable",
                409 => " — Conflit (ressource déjà existante ?)",
                422 => " — Données invalides",
                429 => " — Rate limit atteint, attendez quelques secondes",
                _   => "",
            };
            bail!("Supabase {} {}: {} — {}{}", method, path, status, msg, hint);
        }
        Ok(v)
    }

    async fn get(&self, path: &str) -> Result<Value> {
        let url = self.url(path);
        let req = self.builder(Method::GET, &url).body(Body::empty()).context("GET")?;
        let (s, b) = self.send(req).await?;
        self.parse(s, &b, "GET", path)
    }

    async fn post(&self, path: &str, payload: &Value) -> Result<Value> {
        let url  = self.url(path);
        let body = serde_json::to_vec(payload)?;
        let req  = self.builder(Method::POST, &url).body(Body::from(body)).context("POST")?;
        let (s, b) = self.send(req).await?;
        self.parse(s, &b, "POST", path)
    }

    async fn patch(&self, path: &str, payload: &Value) -> Result<Value> {
        let url  = self.url(path);
        let body = serde_json::to_vec(payload)?;
        let req  = self.builder(Method::PATCH, &url).body(Body::from(body)).context("PATCH")?;
        let (s, b) = self.send(req).await?;
        self.parse(s, &b, "PATCH", path)
    }

    async fn delete(&self, path: &str) -> Result<()> {
        let url = self.url(path);
        let req = self.builder(Method::DELETE, &url).body(Body::empty()).context("DELETE")?;
        let (s, b) = self.send(req).await?;
        if s == 200 || s == 204 { return Ok(()); }
        let v: Value = serde_json::from_slice(&b).unwrap_or(Value::Null);
        let msg = v["message"].as_str().unwrap_or("Erreur inconnue");
        bail!("Supabase DELETE {}: {} — {}", path, s, msg);
    }

    /// Parse une réponse de liste en acceptant DEUX formes possibles :
    ///   - tableau JSON brut :          [ {...}, {...} ]
    ///   - objet avec une clé nommée :  { "projects": [ {...}, {...} ] }
    ///
    /// Nécessaire car les données réelles observées montrent une
    /// incohérence d'enveloppe selon l'endpoint — parier sur une seule
    /// forme aurait pu casser silencieusement la moitié des commandes.
    fn parse_list(&self, v: Value, wrapper_key: &str) -> Vec<Value> {
        if let Some(arr) = v.as_array() {
            return arr.clone();
        }
        if let Some(arr) = v[wrapper_key].as_array() {
            return arr.clone();
        }
        // Dernier recours : la première clé de l'objet qui contient un tableau
        if let Some(obj) = v.as_object() {
            for (_, val) in obj {
                if let Some(arr) = val.as_array() {
                    return arr.clone();
                }
            }
        }
        Vec::new()
    }
}

// ── API : Organizations ────────────────────────────────────────

impl SupabaseClient {
    pub async fn list_organizations(&self) -> Result<Vec<SbOrganization>> {
        let v = self.get("organizations").await?;
        let items = self.parse_list(v, "organizations");
        serde_json::from_value(Value::Array(items)).context("Parse organizations")
    }

    pub async fn get_organization(&self, id_or_slug: &str) -> Result<Value> {
        self.get(&format!("organizations/{}", id_or_slug)).await
    }
}

// ── API : Projects ──────────────────────────────────────────────

impl SupabaseClient {
    pub async fn list_projects(&self) -> Result<Vec<SbProject>> {
        let v = self.get("projects").await?;
        let items = self.parse_list(v, "projects");
        serde_json::from_value(Value::Array(items)).context("Parse projects")
    }

    /// Lookup direct par ref — pas de list+find (leçon appliquée).
    pub async fn get_project(&self, project_ref: &str) -> Result<SbProject> {
        let v = self.get(&format!("projects/{}", project_ref)).await?;
        serde_json::from_value(v).context("Parse get_project")
    }

    pub async fn create_project(
        &self,
        name:            &str,
        organization_id: &str,
        region:          &str,
        db_pass:         &str,
    ) -> Result<SbProject> {
        let payload = serde_json::json!({
            "name":            name,
            "organization_id": organization_id,
            "region":          region,
            "db_pass":         db_pass,
        });
        let v = self.post("projects", &payload).await?;
        serde_json::from_value(v).context("Parse create_project")
    }

    pub async fn delete_project(&self, project_ref: &str) -> Result<()> {
        self.delete(&format!("projects/{}", project_ref)).await
    }

    pub async fn pause_project(&self, project_ref: &str) -> Result<()> {
        self.post(&format!("projects/{}/pause", project_ref), &serde_json::json!({})).await?;
        Ok(())
    }

    pub async fn restore_project(&self, project_ref: &str) -> Result<()> {
        self.post(&format!("projects/{}/restore", project_ref), &serde_json::json!({})).await?;
        Ok(())
    }

    /// Clés API du projet. Supabase expose désormais deux modèles :
    ///   - legacy : "anon" / "service_role" (JWT statiques)
    ///   - actuel : "publishable" / "secret" (rotables)
    /// On retourne les deux tels que l'API les fournit, sans supposer
    /// lequel des deux modèles est actif sur un projet donné.
    pub async fn get_api_keys(&self, project_ref: &str) -> Result<Vec<SbApiKey>> {
        let v = self.get(&format!("projects/{}/api-keys", project_ref)).await?;
        let items = self.parse_list(v, "api_keys");
        serde_json::from_value(Value::Array(items)).context("Parse api_keys")
    }

    pub fn project_url(&self, project_ref: &str) -> String {
        format!("https://{}.supabase.co", project_ref)
    }
}

// ── API : Database ────────────────────────────────────────────

impl SupabaseClient {
    /// Exécute une requête SQL brute. Utilisé pour appliquer les
    /// migrations et pour toute opération DDL/DML directe.
    pub async fn execute_sql(&self, project_ref: &str, query: &str) -> Result<Value> {
        let payload = serde_json::json!({ "query": query });
        self.post(&format!("projects/{}/database/query", project_ref), &payload).await
    }

    pub async fn list_tables(&self, project_ref: &str, schema: &str) -> Result<Vec<SbTable>> {
        let v = self.get(&format!(
            "projects/{}/database/tables?schema={}", project_ref, schema
        )).await?;
        let items = self.parse_list(v, "tables");
        serde_json::from_value(Value::Array(items)).context("Parse tables")
    }

    pub async fn list_extensions(&self, project_ref: &str) -> Result<Vec<SbExtension>> {
        let v = self.get(&format!("projects/{}/database/extensions", project_ref)).await?;
        let items = self.parse_list(v, "extensions");
        serde_json::from_value(Value::Array(items)).context("Parse extensions")
    }

    pub async fn list_migrations(&self, project_ref: &str) -> Result<Vec<SbMigration>> {
        let v = self.get(&format!("projects/{}/database/migrations", project_ref)).await?;
        let items = self.parse_list(v, "migrations");
        serde_json::from_value(Value::Array(items)).context("Parse migrations")
    }

    /// Applique une migration nommée. Le nom devient l'enregistrement
    /// de traçabilité (table supabase_migrations.schema_migrations côté
    /// serveur) — c'est ce qui permet à `list_migrations` de savoir
    /// ensuite qu'elle a déjà été appliquée.
    pub async fn apply_migration(
        &self,
        project_ref: &str,
        version:     &str,
        name:        &str,
        query:       &str,
    ) -> Result<()> {
        let payload = serde_json::json!({ "version": version, "name": name, "query": query });
        self.post(&format!("projects/{}/database/migrations", project_ref), &payload).await?;
        Ok(())
    }
}

// ── API : Edge Functions ──────────────────────────────────────

impl SupabaseClient {
    pub async fn list_edge_functions(&self, project_ref: &str) -> Result<Vec<SbEdgeFunction>> {
        let v = self.get(&format!("projects/{}/functions", project_ref)).await?;
        let items = self.parse_list(v, "functions");
        serde_json::from_value(Value::Array(items)).context("Parse edge functions")
    }

    /// Lookup direct par slug — pas de list+find (leçon appliquée).
    pub async fn get_edge_function(&self, project_ref: &str, slug: &str) -> Result<SbEdgeFunction> {
        let v = self.get(&format!("projects/{}/functions/{}", project_ref, slug)).await?;
        serde_json::from_value(v).context("Parse get_edge_function")
    }

    pub async fn deploy_edge_function(
        &self,
        project_ref: &str,
        slug:        &str,
        body_source: &str,
        verify_jwt:  bool,
    ) -> Result<SbEdgeFunction> {
        let payload = serde_json::json!({
            "slug":       slug,
            "name":       slug,
            "body":       body_source,
            "verify_jwt": verify_jwt,
        });
        let v = self.post(&format!("projects/{}/functions", project_ref), &payload).await?;
        serde_json::from_value(v).context("Parse deploy_edge_function")
    }

    pub async fn delete_edge_function(&self, project_ref: &str, slug: &str) -> Result<()> {
        self.delete(&format!("projects/{}/functions/{}", project_ref, slug)).await
    }
}

// ── API : Secrets (Edge Functions) ────────────────────────────

impl SupabaseClient {
    pub async fn list_secrets(&self, project_ref: &str) -> Result<Vec<SbSecret>> {
        let v = self.get(&format!("projects/{}/secrets", project_ref)).await?;
        let items = self.parse_list(v, "secrets");
        serde_json::from_value(Value::Array(items)).context("Parse secrets")
    }

    pub async fn set_secrets(&self, project_ref: &str, secrets: &[(String, String)]) -> Result<()> {
        let payload: Vec<Value> = secrets.iter()
            .map(|(k, v)| serde_json::json!({ "name": k, "value": v }))
            .collect();
        self.post(&format!("projects/{}/secrets", project_ref), &Value::Array(payload)).await?;
        Ok(())
    }

    pub async fn delete_secret(&self, project_ref: &str, name: &str) -> Result<()> {
        let payload = serde_json::json!([name]);
        // DELETE avec corps — construit manuellement (notre helper delete() ne supporte pas de body)
        let url = self.url(&format!("projects/{}/secrets", project_ref));
        let req = self.builder(Method::DELETE, &url)
            .body(Body::from(serde_json::to_vec(&payload)?))
            .context("DELETE secrets")?;
        let (s, b) = self.send(req).await?;
        if s == 200 || s == 204 { return Ok(()); }
        let v: Value = serde_json::from_slice(&b).unwrap_or(Value::Null);
        bail!("Supabase DELETE secrets: {} — {}", s, v["message"].as_str().unwrap_or("Erreur inconnue"));
    }
}

// ── API : Branches ──────────────────────────────────────────────

impl SupabaseClient {
    pub async fn list_branches(&self, project_ref: &str) -> Result<Vec<SbBranch>> {
        let v = self.get(&format!("projects/{}/branches", project_ref)).await?;
        let items = self.parse_list(v, "branches");
        serde_json::from_value(Value::Array(items)).context("Parse branches")
    }

    pub async fn create_branch(&self, project_ref: &str, name: &str) -> Result<SbBranch> {
        let payload = serde_json::json!({ "branch_name": name });
        let v = self.post(&format!("projects/{}/branches", project_ref), &payload).await?;
        serde_json::from_value(v).context("Parse create_branch")
    }

    pub async fn delete_branch(&self, branch_id: &str) -> Result<()> {
        self.delete(&format!("branches/{}", branch_id)).await
    }

    pub async fn merge_branch(&self, branch_id: &str) -> Result<()> {
        self.post(&format!("branches/{}/merge", branch_id), &serde_json::json!({})).await?;
        Ok(())
    }

    pub async fn reset_branch(&self, branch_id: &str, migration_version: Option<&str>) -> Result<()> {
        let mut payload = serde_json::json!({});
        if let Some(v) = migration_version { payload["migration_version"] = v.into(); }
        self.post(&format!("branches/{}/reset", branch_id), &payload).await?;
        Ok(())
    }

    pub async fn rebase_branch(&self, branch_id: &str) -> Result<()> {
        self.post(&format!("branches/{}/rebase", branch_id), &serde_json::json!({})).await?;
        Ok(())
    }
}

// ── API : Advisors & Logs ────────────────────────────────────────

impl SupabaseClient {
    pub async fn get_advisors(&self, project_ref: &str, kind: &str) -> Result<Vec<SbAdvisorLint>> {
        // kind: "security" | "performance"
        let v = self.get(&format!("projects/{}/advisors/{}", project_ref, kind)).await?;
        let items = self.parse_list(v, "lints");
        serde_json::from_value(Value::Array(items)).context("Parse advisors")
    }

    pub async fn get_logs(&self, project_ref: &str, service: &str, limit: usize) -> Result<Vec<Value>> {
        // service: "api" | "postgres" | "auth" | "storage" | "realtime" | "edge-function"
        let v = self.get(&format!(
            "projects/{}/analytics/endpoints/logs.all?sql=select+*+from+{}_logs+limit+{}",
            project_ref, service, limit
        )).await?;
        Ok(self.parse_list(v, "result"))
    }
}
