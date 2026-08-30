// ============================================================
//  vercel_client.rs — Client Vercel API v9
//
//  Réutilise hyper 0.14 + hyper-rustls 0.24 (déjà dans ilocker).
//
//  Couverture API :
//    Deployments : create, list, get, cancel, delete, redeploy,
//                  promote (alias), list_files, get_file
//    Projects    : create, list, get, update, delete,
//                  list_env, add_env, update_env, remove_env,
//                  get_domain, add_domain, remove_domain,
//                  list_domains
//    Domains     : check, buy, transfer, list, get, delete,
//                  add_record, list_records, delete_record
//    Teams       : list, get, create, invite, remove_member
//    Aliases     : list, get, assign, delete
//    Secrets     : list, create, rename, delete
//    Edge Config : create, list, get, update_items, delete
//    Logs        : get deployment logs
//    User        : get authenticated
//    Webhooks    : create, list, delete
//    Checks      : create, list, update (CI gates)
//    Git         : connect_provider, list_repos, disconnect
//    Analytics   : get web analytics for a project
// ============================================================

use anyhow::{bail, Context, Result};
use hyper::{body::to_bytes, client::HttpConnector, Body, Client, Method, Request};
use hyper_rustls::HttpsConnector;
use serde::{Deserialize, Serialize};
use serde_json::Value;

type HyperClient = Client<HttpsConnector<HttpConnector>>;

/// Base URL de l'API Vercel. Surchageable via la variable d'environnement
/// ILOC_VERCEL_API_BASE pour pointer vers un serveur de test local.
/// Sans cette variable, comportement strictement identique : l'API Vercel
/// réelle. Utile pour les tests automatisés et le développement de nouveaux
/// providers sans jamais toucher un vrai compte Vercel.
fn api_base() -> String {
    std::env::var("ILOC_VERCEL_API_BASE").unwrap_or_else(|_| "https://api.vercel.com".to_string())
}

pub struct VercelClient {
    http:     HyperClient,
    token:    String,
    team_id:  Option<String>,
}

// ── Types principaux ──────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VcDeployment {
    pub id:          String,
    pub name:        String,
    pub url:         String,
    pub state:       Option<String>,
    pub target:      Option<String>,
    #[serde(rename = "readyState")]
    pub ready_state: Option<String>,
    pub created:     Option<u64>,
    pub ready:       Option<u64>,
    #[serde(rename = "buildingAt")]
    pub building_at: Option<u64>,
    pub creator:     Option<VcCreator>,
    pub meta:        Option<Value>,
    #[serde(rename = "gitSource")]
    pub git_source:  Option<VcGitSource>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VcCreator {
    pub uid:      Option<String>,
    pub email:    Option<String>,
    pub username: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VcGitSource {
    #[serde(rename = "type")]
    pub source_type: Option<String>,
    pub repo:        Option<String>,
    pub ref_:        Option<String>,
    pub sha:         Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VcProject {
    pub id:                  String,
    pub name:                String,
    pub framework:           Option<String>,
    #[serde(rename = "latestDeployments")]
    pub latest_deployments:  Option<Vec<VcDeployment>>,
    #[serde(rename = "rootDirectory")]
    pub root_directory:      Option<String>,
    #[serde(rename = "buildCommand")]
    pub build_command:       Option<String>,
    #[serde(rename = "outputDirectory")]
    pub output_directory:    Option<String>,
    #[serde(rename = "installCommand")]
    pub install_command:     Option<String>,
    #[serde(rename = "devCommand")]
    pub dev_command:         Option<String>,
    pub link:                Option<VcProjectLink>,
    pub targets:             Option<Value>,
    #[serde(rename = "nodeVersion")]
    pub node_version:        Option<String>,
    #[serde(rename = "createdAt")]
    pub created_at:          Option<u64>,
    #[serde(rename = "updatedAt")]
    pub updated_at:          Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VcProjectLink {
    #[serde(rename = "type")]
    pub link_type: Option<String>,
    pub org:       Option<String>,
    pub repo:      Option<String>,
    #[serde(rename = "repoId")]
    pub repo_id:   Option<u64>,
    #[serde(rename = "productionBranch")]
    pub production_branch: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VcEnvVar {
    pub id:         Option<String>,
    pub key:        String,
    pub value:      Option<String>,
    #[serde(rename = "type")]
    pub env_type:   Option<String>,   // "plain" | "secret" | "encrypted"
    pub target:     Option<Vec<String>>, // ["production","preview","development"]
    #[serde(rename = "gitBranch")]
    pub git_branch: Option<String>,
    #[serde(rename = "createdAt")]
    pub created_at: Option<u64>,
    #[serde(rename = "updatedAt")]
    pub updated_at: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VcDomain {
    pub name:          String,
    pub verified:      Option<bool>,
    #[serde(rename = "createdAt")]
    pub created_at:    Option<u64>,
    #[serde(rename = "expiresAt")]
    pub expires_at:    Option<u64>,
    pub cdn:           Option<bool>,
    pub nameservers:   Option<Vec<String>>,
    #[serde(rename = "intendedNameservers")]
    pub intended_ns:   Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VcDnsRecord {
    pub id:      Option<String>,
    pub name:    String,
    #[serde(rename = "type")]
    pub rec_type: String,
    pub value:   String,
    pub ttl:     Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VcAlias {
    pub uid:          Option<String>,
    pub alias:        String,
    pub created:      Option<u64>,
    pub deployment:   Option<VcAliasDeployment>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VcAliasDeployment {
    pub id:  Option<String>,
    pub url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VcTeam {
    pub id:          String,
    pub slug:        String,
    pub name:        String,
    #[serde(rename = "createdAt")]
    pub created_at:  Option<u64>,
    pub membership:  Option<VcMembership>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VcMembership {
    pub role:  Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VcUser {
    pub id:       Option<String>,
    pub email:    String,
    pub username: String,
    pub name:     Option<String>,
    #[serde(rename = "createdAt")]
    pub created_at: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VcSecret {
    pub uid:         String,
    pub name:        String,
    pub created:     Option<u64>,
    #[serde(rename = "projectId")]
    pub project_id:  Option<String>,
    #[serde(rename = "teamId")]
    pub team_id:     Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VcWebhook {
    pub id:        String,
    pub url:       String,
    pub events:    Vec<String>,
    #[serde(rename = "createdAt")]
    pub created_at: Option<u64>,
    #[serde(rename = "teamId")]
    pub team_id:   Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VcEdgeConfig {
    pub id:          String,
    pub slug:        Option<String>,
    pub digest:      Option<String>,
    #[serde(rename = "itemCount")]
    pub item_count:  Option<u64>,
    #[serde(rename = "createdAt")]
    pub created_at:  Option<u64>,
    #[serde(rename = "updatedAt")]
    pub updated_at:  Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VcCheck {
    pub id:          String,
    pub name:        String,
    pub status:      String,
    pub conclusion:  Option<String>,
    #[serde(rename = "deploymentId")]
    pub deployment_id: String,
    #[serde(rename = "createdAt")]
    pub created_at:  Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VcFile {
    pub name: String,
    #[serde(rename = "type")]
    pub file_type: Option<String>,
    pub uid:  Option<String>,
    pub mode: Option<u32>,
    pub children: Option<Vec<VcFile>>,
}

// ── Constructeur ─────────────────────────────────────────────

impl VercelClient {
    pub fn new(token: &str, team_id: Option<String>) -> Self {
        let connector = hyper_rustls::HttpsConnectorBuilder::new()
            .with_native_roots()
            .https_or_http()
            .enable_http1()
            .build();
        Self {
            http:    Client::builder().build(connector),
            token:   token.to_string(),
            team_id,
        }
    }

    pub fn new_from_credentials(creds: &crate::vercel_store::VercelCredentials) -> Self {
        Self::new(&creds.token, creds.default_team_id.clone())
    }
}

// ── HTTP helpers privés ───────────────────────────────────────

impl VercelClient {
    /// Ajoute ?teamId=xxx si un team est configuré
    fn url(&self, path: &str) -> String {
        let base = format!("{}/{}", api_base(), path.trim_start_matches('/'));
        match &self.team_id {
            Some(tid) => {
                if base.contains('?') {
                    format!("{}&teamId={}", base, tid)
                } else {
                    format!("{}?teamId={}", base, tid)
                }
            }
            None => base,
        }
    }

    fn builder(&self, method: Method, url: &str) -> hyper::http::request::Builder {
        Request::builder()
            .method(method)
            .uri(url)
            .header("Authorization",  format!("Bearer {}", self.token))
            .header("Content-Type",   "application/json")
            .header("User-Agent",     "ilocker/1.10.8")
    }

    async fn send(&self, req: Request<Body>) -> Result<(u16, Vec<u8>)> {
        let resp = tokio::time::timeout(std::time::Duration::from_secs(30), self.http.request(req))
            .await
            .context("Délai dépassé (30s) — Vercel ne répond pas, vérifiez votre connexion")?
            .context("Requête Vercel échouée")?;
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
            // Si le corps n'était pas du JSON valide (page d'erreur d'un
            // proxy, HTML, etc.), afficher le texte brut plutôt que de
            // masquer un message parfaitement lisible derrière "Erreur inconnue".
            if let Some(raw) = v.as_str() {
                let trimmed: String = raw.trim().chars().take(300).collect();
                if !trimmed.is_empty() {
                    bail!("Vercel {} {}: {} — {}", method, path, status, trimmed);
                }
            }
            let msg  = v["error"]["message"].as_str()
                .or(v["message"].as_str())
                .unwrap_or("Erreur inconnue");
            let code = v["error"]["code"].as_str().unwrap_or("");
            let hint = match status {
                401 => " — Vérifiez votre token (iloc connect vercel)",
                 403 => " — Permissions insuffisantes",
                404 => " — Ressource introuvable",
                409 => " — Conflit (ressource déjà existante ?)",
                422 => " — Données invalides",
                429 => " — Rate limit atteint, attendez quelques secondes",
                _   => "",
            };
            let code_str = if code.is_empty() { String::new() } else { format!(" [{}]", code) };
            bail!("Vercel {} {}: {} — {}{}{}", method, path, status, msg, code_str, hint);
        }
        Ok(v)
    }

    async fn get(&self, path: &str) -> Result<Value> {
        let url = self.url(path);
        let req = self.builder(Method::GET, &url)
            .body(Body::empty()).context("GET")?;
        let (s, b) = self.send(req).await?;
        self.parse(s, &b, "GET", path)
    }

    async fn post(&self, path: &str, payload: &Value) -> Result<Value> {
        let url  = self.url(path);
        let body = serde_json::to_vec(payload)?;
        let req  = self.builder(Method::POST, &url)
            .body(Body::from(body)).context("POST")?;
        let (s, b) = self.send(req).await?;
        self.parse(s, &b, "POST", path)
    }

    async fn patch(&self, path: &str, payload: &Value) -> Result<Value> {
        let url  = self.url(path);
        let body = serde_json::to_vec(payload)?;
        let req  = self.builder(Method::PATCH, &url)
            .body(Body::from(body)).context("PATCH")?;
        let (s, b) = self.send(req).await?;
        self.parse(s, &b, "PATCH", path)
    }

    async fn put(&self, path: &str, payload: &Value) -> Result<Value> {
        let url  = self.url(path);
        let body = serde_json::to_vec(payload)?;
        let req  = self.builder(Method::PUT, &url)
            .body(Body::from(body)).context("PUT")?;
        let (s, b) = self.send(req).await?;
        self.parse(s, &b, "PUT", path)
    }

    async fn delete(&self, path: &str) -> Result<()> {
        let url = self.url(path);
        let req = self.builder(Method::DELETE, &url)
            .body(Body::empty()).context("DELETE")?;
        let (s, b) = self.send(req).await?;
        if s == 200 || s == 204 { return Ok(()); }
        let v: Value = serde_json::from_slice(&b).unwrap_or(Value::Null);
        let msg = v["error"]["message"].as_str().unwrap_or("Erreur inconnue");
        bail!("Vercel DELETE {}: {} — {}", path, s, msg);
    }

    /// Pagination automatique via next cursor
    async fn get_all(&self, path: &str, items_key: &str, mut limit: usize) -> Result<Vec<Value>>
    {
        let mut results = Vec::new();
        let mut cursor: Option<String> = None;
        let page_size = limit.min(100);

        loop {
            let url = match &cursor {
                Some(c) => format!("{}&until={}&limit={}", self.url(path), c, page_size),
                None    => format!("{}&limit={}", self.url(path), page_size),
            };
            let req = self.builder(Method::GET, &url)
                .body(Body::empty()).context("GET paginated")?;
            let (s, b) = self.send(req).await?;
            let v = self.parse(s, &b, "GET", path)?;

            if let Some(items) = v[items_key].as_array() {
                let take = items.len().min(limit);
                results.extend(items[..take].iter().cloned());
                limit = limit.saturating_sub(take);
            }

            if limit == 0 { break; }

            // Vercel retourne pagination.next (timestamp) ou null
            match v["pagination"]["next"].as_u64() {
                Some(next) => cursor = Some(next.to_string()),
                None       => break,
            }
        }
        Ok(results)
    }
}

// ── API : User ────────────────────────────────────────────────

impl VercelClient {
    pub async fn get_user(&self) -> Result<VcUser> {
        let v = self.get("v2/user").await?;
        serde_json::from_value(v["user"].clone()).context("Parse VcUser")
    }

    pub async fn list_teams(&self) -> Result<Vec<VcTeam>> {
        let v = self.get("v2/teams").await?;
        serde_json::from_value(if v["teams"].is_null() { Value::Array(vec![]) } else { v["teams"].clone() })
            .context("Parse teams")
    }

    pub async fn get_team(&self, team_id: &str) -> Result<VcTeam> {
        let v = self.get(&format!("v2/teams/{}", team_id)).await?;
        serde_json::from_value(v).context("Parse VcTeam")
    }
}

// ── API : Projects ────────────────────────────────────────────

impl VercelClient {
    pub async fn create_project(
        &self,
        name:             &str,
        framework:        Option<&str>,
        root_directory:   Option<&str>,
        build_command:    Option<&str>,
        output_directory: Option<&str>,
        install_command:  Option<&str>,
        dev_command:      Option<&str>,
        git_repo:         Option<&str>,   // "owner/repo"
        git_provider:     Option<&str>,   // "github" | "gitlab" | "bitbucket"
        git_branch:       Option<&str>,
    ) -> Result<VcProject> {
        let mut payload = serde_json::json!({ "name": name });
        if let Some(f)  = framework        { payload["framework"]       = f.into(); }
        if let Some(r)  = root_directory   { payload["rootDirectory"]   = r.into(); }
        if let Some(b)  = build_command    { payload["buildCommand"]    = b.into(); }
        if let Some(o)  = output_directory { payload["outputDirectory"] = o.into(); }
        if let Some(i)  = install_command  { payload["installCommand"]  = i.into(); }
        if let Some(d)  = dev_command      { payload["devCommand"]      = d.into(); }

        if let (Some(repo), Some(provider)) = (git_repo, git_provider) {
            payload["gitRepository"] = serde_json::json!({
                "type": provider,
                "repo": repo,
            });
        }
        if let Some(b) = git_branch { payload["productionBranch"] = b.into(); }

        let v = self.post("v9/projects", &payload).await?;
        serde_json::from_value(v).context("Parse VcProject create")
    }

    pub async fn list_projects(&self, limit: usize) -> Result<Vec<VcProject>> {
        let items = self.get_all("v9/projects?", "projects", limit).await?;
        serde_json::from_value(Value::Array(items)).context("Parse projects list")
    }

    pub async fn get_project(&self, id_or_name: &str) -> Result<VcProject> {
        let v = self.get(&format!("v9/projects/{}", id_or_name)).await?;
        serde_json::from_value(v).context("Parse VcProject get")
    }

    pub async fn update_project(
        &self,
        id_or_name:       &str,
        name:             Option<&str>,
        framework:        Option<&str>,
        root_directory:   Option<&str>,
        build_command:    Option<&str>,
        output_directory: Option<&str>,
        install_command:  Option<&str>,
        dev_command:      Option<&str>,
        node_version:     Option<&str>,
        production_branch: Option<&str>,
    ) -> Result<VcProject> {
        let mut payload = serde_json::json!({});
        if let Some(n) = name             { payload["name"]             = n.into(); }
        if let Some(f) = framework        { payload["framework"]        = f.into(); }
        if let Some(r) = root_directory   { payload["rootDirectory"]    = r.into(); }
        if let Some(b) = build_command    { payload["buildCommand"]     = b.into(); }
        if let Some(o) = output_directory { payload["outputDirectory"]  = o.into(); }
        if let Some(i) = install_command  { payload["installCommand"]   = i.into(); }
        if let Some(d) = dev_command      { payload["devCommand"]       = d.into(); }
        if let Some(v) = node_version     { payload["nodeVersion"]      = v.into(); }
        if let Some(b) = production_branch { payload["productionBranch"] = b.into(); }
        let v = self.patch(&format!("v9/projects/{}", id_or_name), &payload).await?;
        serde_json::from_value(v).context("Parse VcProject update")
    }

    pub async fn delete_project(&self, id_or_name: &str) -> Result<()> {
        self.delete(&format!("v9/projects/{}", id_or_name)).await
    }

    // ── Env vars ──────────────────────────────────────────────

    pub async fn list_env(&self, project_id: &str) -> Result<Vec<VcEnvVar>> {
        let v = self.get(&format!("v9/projects/{}/env?decrypt=false", project_id)).await?;
        let envs = v["envs"].as_array().cloned().unwrap_or_default();
        serde_json::from_value(Value::Array(envs)).context("Parse env vars")
    }

    pub async fn get_env_decrypted(&self, project_id: &str, env_id: &str) -> Result<VcEnvVar> {
        let v = self.get(&format!("v9/projects/{}/env/{}?decrypt=true", project_id, env_id)).await?;
        serde_json::from_value(v).context("Parse env var decrypt")
    }

    /// Crée ou met à jour une variable d'environnement.
    pub async fn upsert_env(
        &self,
        project_id:  &str,
        key:         &str,
        value:       &str,
        targets:     &[&str],   // ["production","preview","development"]
        env_type:    &str,      // "encrypted" (recommandé) | "plain"
        git_branch:  Option<&str>,
    ) -> Result<VcEnvVar> {
        let mut payload = serde_json::json!({
            "key":    key,
            "value":  value,
            "type":   env_type,
            "target": targets,
        });
        if let Some(b) = git_branch { payload["gitBranch"] = b.into(); }

        let v = self.post(&format!("v9/projects/{}/env", project_id), &payload).await?;
        // L'API retourne soit l'env créé soit un tableau (si key existait déjà)
        if v.is_array() {
            serde_json::from_value(v[0].clone()).context("Parse upsert env (array)")
        } else {
            serde_json::from_value(v).context("Parse upsert env")
        }
    }

    /// Supprime une variable d'environnement par son ID.
    pub async fn delete_env(&self, project_id: &str, env_id: &str) -> Result<()> {
        self.delete(&format!("v9/projects/{}/env/{}", project_id, env_id)).await
    }

    /// Pull toutes les env vars d'un projet (avec valeurs déchiffrées).
    pub async fn pull_env_all(&self, project_id: &str) -> Result<Vec<(String, String, Vec<String>)>> {
        let envs = self.list_env(project_id).await?;
        let mut result = Vec::new();
        for env in envs {
            if let Some(id) = &env.id {
                if let Ok(decrypted) = self.get_env_decrypted(project_id, id).await {
                    let value   = decrypted.value.unwrap_or_default();
                    let targets = decrypted.target.unwrap_or_default();
                    result.push((env.key, value, targets));
                }
            }
        }
        Ok(result)
    }

    // ── Domains du projet ──────────────────────────────────────

    pub async fn list_project_domains(&self, project_id: &str) -> Result<Vec<Value>> {
        let v = self.get(&format!("v9/projects/{}/domains", project_id)).await?;
        Ok(v["domains"].as_array().cloned().unwrap_or_default())
    }

    pub async fn add_project_domain(
        &self,
        project_id: &str,
        domain:     &str,
        git_branch: Option<&str>,
        redirect:   Option<&str>,
    ) -> Result<Value> {
        let mut payload = serde_json::json!({ "name": domain });
        if let Some(b) = git_branch { payload["gitBranch"] = b.into(); }
        if let Some(r) = redirect   { payload["redirect"]  = r.into(); }
        self.post(&format!("v9/projects/{}/domains", project_id), &payload).await
    }

    pub async fn remove_project_domain(&self, project_id: &str, domain: &str) -> Result<()> {
        self.delete(&format!("v9/projects/{}/domains/{}", project_id, domain)).await
    }

    pub async fn verify_project_domain(&self, project_id: &str, domain: &str) -> Result<Value> {
        self.post(
            &format!("v9/projects/{}/domains/{}/verify", project_id, domain),
            &serde_json::json!({}),
        ).await
    }
}

// ── API : Deployments ─────────────────────────────────────────

impl VercelClient {
    pub async fn list_deployments(
        &self,
        project_id: Option<&str>,
        target:     Option<&str>,   // "production" | "preview"
        state:      Option<&str>,   // "READY" | "ERROR" | "BUILDING" | "CANCELED"
        limit:      usize,
    ) -> Result<Vec<VcDeployment>> {
        let mut qs = String::from("v6/deployments?");
        if let Some(p) = project_id { qs.push_str(&format!("projectId={}&", p)); }
        if let Some(t) = target     { qs.push_str(&format!("target={}&", t)); }
        if let Some(s) = state      { qs.push_str(&format!("state={}&", s)); }
        let items = self.get_all(&qs, "deployments", limit).await?;
        serde_json::from_value(Value::Array(items)).context("Parse deployments")
    }

    pub async fn get_deployment(&self, id_or_url: &str) -> Result<VcDeployment> {
        let v = self.get(&format!("v13/deployments/{}", id_or_url)).await?;
        serde_json::from_value(v).context("Parse VcDeployment get")
    }

    pub async fn cancel_deployment(&self, id: &str) -> Result<VcDeployment> {
        let v = self.patch(
            &format!("v12/deployments/{}/cancel", id),
            &serde_json::json!({}),
        ).await?;
        serde_json::from_value(v).context("Parse cancel deployment")
    }

    pub async fn delete_deployment(&self, id: &str) -> Result<()> {
        self.delete(&format!("v13/deployments/{}", id)).await
    }

    pub async fn redeploy(&self, id: &str, target: Option<&str>) -> Result<VcDeployment> {
        let mut payload = serde_json::json!({ "deploymentId": id });
        if let Some(t) = target { payload["target"] = t.into(); }
        let v = self.post("v13/deployments", &payload).await?;
        serde_json::from_value(v).context("Parse redeploy")
    }

    /// Liste les fichiers d'un déploiement.
    pub async fn list_deployment_files(&self, id: &str) -> Result<Vec<VcFile>> {
        let v = self.get(&format!("v6/deployments/{}/files", id)).await?;
        serde_json::from_value(v).context("Parse deployment files")
    }

    /// Récupère les logs d'un déploiement (build logs).
    pub async fn get_deployment_logs(&self, id: &str) -> Result<Vec<Value>> {
        let v = self.get(&format!("v2/deployments/{}/events", id)).await?;
        Ok(v.as_array().cloned().unwrap_or_default())
    }

    /// Crée un déploiement depuis une source Git (trigger).
    pub async fn create_deployment_from_git(
        &self,
        project_name_or_id: &str,
        git_sha:            Option<&str>,
        git_branch:         Option<&str>,
        target:             Option<&str>,
        force:              bool,
    ) -> Result<VcDeployment> {
        let mut payload = serde_json::json!({
            "name":   project_name_or_id,
            "target": target.unwrap_or("production"),
            "source": "import",
        });
        if let Some(sha) = git_sha    { payload["meta"]["githubCommitSha"] = sha.into(); }
        if let Some(b)   = git_branch { payload["meta"]["githubCommitRef"] = b.into(); }
        if force { payload["forceNew"] = true.into(); }

        let v = self.post("v13/deployments?forceNew=1", &payload).await?;
        serde_json::from_value(v).context("Parse create deployment")
    }

    /// Promote un déploiement en production (alias swap).
    pub async fn promote_deployment(
        &self,
        project_id:    &str,
        deployment_id: &str,
    ) -> Result<()> {
        self.post(
            &format!("v10/projects/{}/promote/{}", project_id, deployment_id),
            &serde_json::json!({}),
        ).await?;
        Ok(())
    }
}

// ── API : Aliases ─────────────────────────────────────────────

impl VercelClient {
    pub async fn list_aliases(
        &self,
        project_id: Option<&str>,
        limit:      usize,
    ) -> Result<Vec<VcAlias>> {
        let path = match project_id {
            Some(p) => format!("v4/aliases?projectId={}&", p),
            None    => "v4/aliases?".to_string(),
        };
        let items = self.get_all(&path, "aliases", limit).await?;
        serde_json::from_value(Value::Array(items)).context("Parse aliases")
    }

    pub async fn assign_alias(
        &self,
        deployment_id: &str,
        alias:         &str,
        redirect:      Option<&str>,
    ) -> Result<Value> {
        let mut payload = serde_json::json!({ "alias": alias });
        if let Some(r) = redirect { payload["redirect"] = r.into(); }
        self.post(
            &format!("v2/deployments/{}/aliases", deployment_id),
            &payload,
        ).await
    }

    pub async fn delete_alias(&self, alias_or_id: &str) -> Result<()> {
        self.delete(&format!("v2/aliases/{}", alias_or_id)).await
    }
}

// ── API : Domains ─────────────────────────────────────────────

impl VercelClient {
    pub async fn list_domains(&self, limit: usize) -> Result<Vec<VcDomain>> {
        let items = self.get_all("v5/domains?", "domains", limit).await?;
        serde_json::from_value(Value::Array(items)).context("Parse domains")
    }

    pub async fn get_domain(&self, domain: &str) -> Result<VcDomain> {
        let v = self.get(&format!("v5/domains/{}", domain)).await?;
        serde_json::from_value(v["domain"].clone()).context("Parse domain")
    }

    pub async fn check_domain(&self, domain: &str) -> Result<Value> {
        self.get(&format!("v4/domains/status?name={}", domain)).await
    }

    pub async fn add_domain(&self, domain: &str) -> Result<VcDomain> {
        let payload = serde_json::json!({ "name": domain });
        let v = self.post("v5/domains", &payload).await?;
        serde_json::from_value(if v["domain"].is_null() { v.clone() } else { v["domain"].clone() }).context("Parse add domain")
    }

    pub async fn delete_domain(&self, domain: &str) -> Result<()> {
        self.delete(&format!("v6/domains/{}", domain)).await
    }

    // ── DNS Records ───────────────────────────────────────────

    pub async fn list_dns_records(&self, domain: &str) -> Result<Vec<VcDnsRecord>> {
        let v = self.get(&format!("v4/domains/{}/records", domain)).await?;
        serde_json::from_value(if v["records"].is_null() { Value::Array(vec![]) } else { v["records"].clone() })
            .context("Parse DNS records")
    }

    pub async fn add_dns_record(
        &self,
        domain:   &str,
        name:     &str,
        rec_type: &str,
        value:    &str,
        ttl:      Option<u64>,
        priority: Option<u64>,
    ) -> Result<VcDnsRecord> {
        let mut payload = serde_json::json!({
            "name":  name,
            "type":  rec_type,
            "value": value,
        });
        if let Some(t) = ttl      { payload["ttl"]      = t.into(); }
        if let Some(p) = priority { payload["mxPriority"] = p.into(); }
        let v = self.post(&format!("v2/domains/{}/records", domain), &payload).await?;
        serde_json::from_value(v).context("Parse add DNS record")
    }

    pub async fn delete_dns_record(&self, domain: &str, record_id: &str) -> Result<()> {
        self.delete(&format!("v2/domains/{}/records/{}", domain, record_id)).await
    }
}

// ── API : Secrets ─────────────────────────────────────────────

impl VercelClient {
    pub async fn list_secrets(&self) -> Result<Vec<VcSecret>> {
        let v = self.get("v3/secrets").await?;
        serde_json::from_value(if v["secrets"].is_null() { Value::Array(vec![]) } else { v["secrets"].clone() })
            .context("Parse secrets")
    }

    pub async fn create_secret(&self, name: &str, value: &str, decryptable: bool) -> Result<VcSecret> {
        let payload = serde_json::json!({
            "name":        name,
            "value":       value,
            "decryptable": decryptable,
        });
        let v = self.post("v2/secrets", &payload).await?;
        serde_json::from_value(v).context("Parse create secret")
    }

    pub async fn rename_secret(&self, name_or_id: &str, new_name: &str) -> Result<VcSecret> {
        let payload = serde_json::json!({ "name": new_name });
        let v = self.patch(&format!("v2/secrets/{}", name_or_id), &payload).await?;
        serde_json::from_value(v).context("Parse rename secret")
    }

    pub async fn delete_secret(&self, name_or_id: &str) -> Result<()> {
        self.delete(&format!("v2/secrets/{}", name_or_id)).await
    }
}

// ── API : Webhooks ────────────────────────────────────────────

impl VercelClient {
    pub async fn list_webhooks(&self) -> Result<Vec<VcWebhook>> {
        let v = self.get("v1/webhooks").await?;
        serde_json::from_value(v.as_array().map(|a| Value::Array(a.clone())).unwrap_or(Value::Array(vec![])))
            .context("Parse webhooks")
    }

    pub async fn create_webhook(
        &self,
        url:    &str,
        events: &[&str],
    ) -> Result<VcWebhook> {
        let payload = serde_json::json!({ "url": url, "events": events });
        let v = self.post("v1/webhooks", &payload).await?;
        serde_json::from_value(v).context("Parse create webhook")
    }

    pub async fn delete_webhook(&self, id: &str) -> Result<()> {
        self.delete(&format!("v1/webhooks/{}", id)).await
    }
}

// ── API : Edge Config ─────────────────────────────────────────

impl VercelClient {
    pub async fn list_edge_configs(&self) -> Result<Vec<VcEdgeConfig>> {
        let v = self.get("v1/edge-config").await?;
        serde_json::from_value(v.as_array().map(|a| Value::Array(a.clone())).unwrap_or(Value::Array(vec![])))
            .context("Parse edge configs")
    }

    pub async fn create_edge_config(&self, slug: &str) -> Result<VcEdgeConfig> {
        let payload = serde_json::json!({ "slug": slug });
        let v = self.post("v1/edge-config", &payload).await?;
        serde_json::from_value(v).context("Parse create edge config")
    }

    pub async fn get_edge_config_items(&self, id: &str) -> Result<Vec<Value>> {
        let v = self.get(&format!("v1/edge-config/{}/items", id)).await?;
        Ok(v.as_array().cloned().unwrap_or_default())
    }

    pub async fn update_edge_config_items(
        &self,
        id:    &str,
        items: &[(String, Value, &str)], // (key, value, operation: "create"|"update"|"delete")
    ) -> Result<()> {
        let operations: Vec<Value> = items.iter().map(|(k, v, op)| serde_json::json!({
            "operation": op,
            "key":       k,
            "value":     v,
        })).collect();
        let payload = serde_json::json!({ "items": operations });
        self.patch(&format!("v1/edge-config/{}/items", id), &payload).await?;
        Ok(())
    }

    pub async fn delete_edge_config(&self, id: &str) -> Result<()> {
        self.delete(&format!("v1/edge-config/{}", id)).await
    }
}

// ── API : Checks (CI gates) ───────────────────────────────────

impl VercelClient {
    pub async fn list_checks(&self, deployment_id: &str) -> Result<Vec<VcCheck>> {
        let v = self.get(&format!("v1/deployments/{}/checks", deployment_id)).await?;
        serde_json::from_value(if v["checks"].is_null() { Value::Array(vec![]) } else { v["checks"].clone() })
            .context("Parse checks")
    }

    pub async fn create_check(
        &self,
        deployment_id: &str,
        name:          &str,
        detached:      bool,
        blocking:      bool,
    ) -> Result<VcCheck> {
        let payload = serde_json::json!({
            "name":     name,
            "detached": detached,
            "blocking": blocking,
        });
        let v = self.post(&format!("v1/deployments/{}/checks", deployment_id), &payload).await?;
        serde_json::from_value(v).context("Parse create check")
    }

    pub async fn update_check(
        &self,
        deployment_id: &str,
        check_id:      &str,
        status:        &str,       // "running" | "completed"
        conclusion:    Option<&str>, // "succeeded" | "failed" | "canceled" | "skipped"
        output:        Option<Value>,
    ) -> Result<VcCheck> {
        let mut payload = serde_json::json!({ "status": status });
        if let Some(c) = conclusion { payload["conclusion"] = c.into(); }
        if let Some(o) = output     { payload["output"]     = o; }
        let v = self.patch(
            &format!("v1/deployments/{}/checks/{}", deployment_id, check_id),
            &payload,
        ).await?;
        serde_json::from_value(v).context("Parse update check")
    }
}

// ── Helpers ───────────────────────────────────────────────────

impl VercelClient {
    /// Résout un nom de projet en ID.
    pub async fn resolve_project_id(&self, name_or_id: &str) -> Result<String> {
        let project = self.get_project(name_or_id).await?;
        Ok(project.id)
    }

    /// Polling du statut d'un déploiement jusqu'à READY ou ERROR.
    /// Retourne le déploiement final.
    pub async fn wait_deployment_ready(
        &self,
        deployment_id: &str,
        timeout_secs:  u64,
        tick_cb:       impl Fn(&VcDeployment),
    ) -> Result<VcDeployment> {
        let start = std::time::Instant::now();
        let limit = std::time::Duration::from_secs(timeout_secs);
        loop {
            let d = self.get_deployment(deployment_id).await?;
            let state = d.ready_state.as_deref().unwrap_or("");
            tick_cb(&d);
            if state == "READY" || state == "ERROR" || state == "CANCELED" {
                return Ok(d);
            }
            if start.elapsed() > limit {
                bail!(
                    "Timeout : le déploiement {} n'est pas prêt après {}s (état: {}).",
                    deployment_id, timeout_secs, state
                );
            }
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        }
    }
}

// ── Helpers d'affichage ───────────────────────────────────────

pub fn deployment_state_icon(state: &str) -> &'static str {
    match state {
        "READY"     => "✓",
        "ERROR"     => "✗",
        "BUILDING"  => "↻",
        "QUEUED"    => "○",
        "CANCELED"  => "○",
        "INITIALIZING" => "…",
        _           => "?",
    }
}

pub fn format_ts(ts_ms: Option<u64>) -> String {
    match ts_ms {
        None => "-".to_string(),
        Some(ms) => {
            let secs = ms / 1000;
            let now  = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let diff = now.saturating_sub(secs);
            if diff < 60 { format!("{}s ago", diff) }
            else if diff < 3600 { format!("{}m ago", diff / 60) }
            else if diff < 86400 { format!("{}h ago", diff / 3600) }
            else { format!("{}d ago", diff / 86400) }
        }
    }
}
