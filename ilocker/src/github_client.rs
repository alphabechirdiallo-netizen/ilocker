// ============================================================
//  github_client.rs — Client GitHub API v3 (REST) + GraphQL
//
//  Réutilise les crates déjà présentes dans ilocker :
//    hyper 0.14  +  hyper-rustls 0.24  (identique à s3_client.rs)
//
//  Principe :
//    - Un seul client instancié par appel de commande
//    - Toutes les erreurs GitHub sont parsées et remontées
//      avec le message exact de l'API (pas de messages génériques)
//    - Rate limit détecté et affiché avec le reset time
//    - Pagination automatique via Link header (liste complète)
//    - Support GitHub Enterprise (api_url custom)
//
//  Méthodes couvertes :
//    Repos  : create, get, list, delete, update, fork, transfer,
//             topics, visibility, default_branch, archive
//    Issues : create, list, get, update, close, add_labels,
//             remove_labels, assign, unassign, lock, unlock
//    PRs    : create, list, get, merge, review, request_review,
//             update_branch, draft toggle
//    Branches: create, delete, list, protect, unprotect, rename
//    Releases: create, list, get, delete, upload_asset, latest
//    Actions : list_workflows, trigger, list_runs, get_run,
//              cancel_run, rerun, list_secrets, set_secret,
//              delete_secret
//    Secrets : set_repo_secret, delete_repo_secret,
//              set_org_secret, list_repo_secrets
//    Webhooks: create, list, delete, ping
//    Teams   : list, add_member, remove_member, add_repo
//    Orgs    : list_repos, list_members, get_org
//    User    : get_authenticated, list_orgs, list_repos
//    Collaborators: add, remove, list
//    Git objects: create_ref, delete_ref, get_commit, create_tag
//    Search  : repos, code, issues, users
//    Gists   : create, list
// ============================================================

use anyhow::{bail, Context, Result};
use hyper::{
    body::to_bytes,
    client::HttpConnector,
    Body, Client, Method, Request, Uri,
};
use hyper_rustls::HttpsConnector;
use serde::{Deserialize, Serialize};
use serde_json::Value;

// ── Types de base ─────────────────────────────────────────────

type HyperClient = Client<HttpsConnector<HttpConnector>>;

pub struct GitHubClient {
    http:    HyperClient,
    token:   String,
    api_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GhRepo {
    pub id:               u64,
    pub node_id:          String,
    pub name:             String,
    pub full_name:        String,
    pub private:          bool,
    pub description:      Option<String>,
    pub html_url:         String,
    pub clone_url:        String,
    pub ssh_url:          String,
    pub default_branch:   String,
    pub visibility:       Option<String>,
    pub fork:             bool,
    pub archived:         bool,
    pub topics:           Option<Vec<String>>,
    pub open_issues_count: Option<u64>,
    pub stargazers_count: Option<u64>,
    pub forks_count:      Option<u64>,
    pub language:         Option<String>,
    pub created_at:       Option<String>,
    pub updated_at:       Option<String>,
    pub pushed_at:        Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GhIssue {
    pub number:     u64,
    pub title:      String,
    pub state:      String,
    pub html_url:   String,
    pub body:       Option<String>,
    pub user:       GhUser,
    pub labels:     Vec<GhLabel>,
    pub assignees:  Vec<GhUser>,
    pub created_at: String,
    pub updated_at: String,
    pub closed_at:  Option<String>,
    pub pull_request: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GhPullRequest {
    pub number:     u64,
    /// Identifiant global GraphQL (ex: "PR_kwDOA...") — requis par set_pr_draft(),
    /// distinct de `number` (REST) et de `head.label` (ex: "owner:branche").
    pub node_id:    String,
    pub title:      String,
    pub state:      String,
    pub html_url:   String,
    pub body:       Option<String>,
    pub user:       GhUser,
    pub head:       GhBranchRef,
    pub base:       GhBranchRef,
    pub draft:      bool,
    pub mergeable:  Option<bool>,
    pub merged:     Option<bool>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GhBranchRef {
    pub label: String,
    #[serde(rename = "ref")]
    pub ref_:  String,
    pub sha:   String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GhBranch {
    pub name:      String,
    pub protected: bool,
    pub commit:    GhCommitRef,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GhCommitRef {
    pub sha: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GhRelease {
    pub id:          u64,
    pub tag_name:    String,
    pub name:        Option<String>,
    pub body:        Option<String>,
    pub draft:       bool,
    pub prerelease:  bool,
    pub html_url:    String,
    pub assets:      Vec<GhReleaseAsset>,
    pub created_at:  String,
    pub published_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GhReleaseAsset {
    pub id:           u64,
    pub name:         String,
    pub size:         u64,
    pub download_count: u64,
    pub browser_download_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GhUser {
    pub login:      String,
    pub avatar_url: Option<String>,
    pub html_url:   Option<String>,
    #[serde(rename = "type")]
    pub user_type:  Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GhLabel {
    pub name:  String,
    pub color: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GhWorkflow {
    pub id:    u64,
    pub name:  String,
    pub state: String,
    pub path:  String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GhWorkflowRun {
    pub id:          u64,
    pub name:        Option<String>,
    pub status:      String,
    pub conclusion:  Option<String>,
    pub html_url:    String,
    pub created_at:  String,
    pub updated_at:  String,
    pub head_branch: Option<String>,
    pub head_sha:    String,
    pub run_number:  u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GhOrg {
    pub login:       String,
    pub description: Option<String>,
    pub avatar_url:  Option<String>,
    pub html_url:    Option<String>,
    pub public_repos: Option<u64>,
    pub total_private_repos: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GhSearchResult<T> {
    pub total_count:        u64,
    pub incomplete_results: bool,
    pub items:              Vec<T>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GhCollaborator {
    pub login:       String,
    pub permissions: GhPermissions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GhPermissions {
    pub admin: bool,
    pub push:  bool,
    pub pull:  bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GhWebhook {
    pub id:     u64,
    pub config: GhWebhookConfig,
    pub events: Vec<String>,
    pub active: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GhWebhookConfig {
    pub url:          String,
    pub content_type: Option<String>,
    pub insecure_ssl: Option<String>,
}

// ── Constructeur ─────────────────────────────────────────────

impl GitHubClient {
    pub fn new(token: &str, api_url: &str) -> Self {
        let connector = hyper_rustls::HttpsConnectorBuilder::new()
            .with_native_roots()
            .https_or_http()
            .enable_http1()
            .build();
        let http = Client::builder().build(connector);
        Self {
            http,
            token:   token.to_string(),
            api_url: api_url.trim_end_matches('/').to_string(),
        }
    }

    pub fn new_from_credentials(creds: &crate::github_store::GitHubCredentials) -> Self {
        Self::new(&creds.token, &creds.api_url)
    }
}

// ── HTTP helpers privés ───────────────────────────────────────

impl GitHubClient {
    fn url(&self, path: &str) -> String {
        if path.starts_with("https://") || path.starts_with("http://") {
            path.to_string()
        } else {
            format!("{}/{}", self.api_url, path.trim_start_matches('/'))
        }
    }

    fn base_request(&self, method: Method, url: &str) -> hyper::http::request::Builder {
        Request::builder()
            .method(method)
            .uri(url)
            .header("Authorization",  format!("Bearer {}", self.token))
            .header("Accept",         "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("User-Agent",     "ilocker/1.10.6")
            .header("Content-Type",   "application/json")
    }

    async fn send(&self, req: Request<Body>) -> Result<(u16, Vec<u8>, Vec<(String, String)>)> {
        // Timeout explicite : sans lui, hyper attend indéfiniment une réponse
        // (aucun timeout par défaut dans hyper 0.14). Une requête qui ne
        // répond jamais (réseau capricieux, service qui traîne) bloquerait
        // sinon iloc pour toujours, sans aucun message à l'utilisateur.
        let resp = tokio::time::timeout(std::time::Duration::from_secs(30), self.http.request(req))
            .await
            .context("Délai dépassé (30s) — GitHub ne répond pas, vérifiez votre connexion")?
            .context("Requête GitHub échouée")?;
        let status = resp.status().as_u16();

        // Collecter les headers importants
        let mut headers: Vec<(String, String)> = Vec::new();
        for (k, v) in resp.headers() {
            let key = k.as_str().to_lowercase();
            if matches!(key.as_str(),
                "link" | "x-ratelimit-remaining" | "x-ratelimit-reset" |
                "x-ratelimit-limit" | "location" | "x-ratelimit-used"
            ) {
                if let Ok(val) = v.to_str() {
                    headers.push((key, val.to_string()));
                }
            }
        }

        let body = to_bytes(resp.into_body()).await.context("Lecture corps réponse")?.to_vec();
        Ok((status, body, headers))
    }

    async fn get(&self, path: &str) -> Result<Value> {
        let url = self.url(path);
        let req = self.base_request(Method::GET, &url)
            .body(Body::empty())
            .context("Construction requête GET")?;
        let (status, body, headers) = self.send(req).await?;
        self.check_rate_limit(&headers);
        self.parse_response(status, &body, "GET", path)
    }

    async fn post(&self, path: &str, payload: &Value) -> Result<Value> {
        let url  = self.url(path);
        let body = serde_json::to_vec(payload)?;
        let req  = self.base_request(Method::POST, &url)
            .body(Body::from(body))
            .context("Construction requête POST")?;
        let (status, resp_body, headers) = self.send(req).await?;
        self.check_rate_limit(&headers);
        self.parse_response(status, &resp_body, "POST", path)
    }

    async fn patch(&self, path: &str, payload: &Value) -> Result<Value> {
        let url  = self.url(path);
        let body = serde_json::to_vec(payload)?;
        let req  = self.base_request(Method::PATCH, &url)
            .body(Body::from(body))
            .context("Construction requête PATCH")?;
        let (status, resp_body, headers) = self.send(req).await?;
        self.check_rate_limit(&headers);
        self.parse_response(status, &resp_body, "PATCH", path)
    }

    async fn put(&self, path: &str, payload: &Value) -> Result<Value> {
        let url  = self.url(path);
        let body = serde_json::to_vec(payload)?;
        let req  = self.base_request(Method::PUT, &url)
            .body(Body::from(body))
            .context("Construction requête PUT")?;
        let (status, resp_body, headers) = self.send(req).await?;
        self.check_rate_limit(&headers);
        self.parse_response(status, &resp_body, "PUT", path)
    }

    async fn delete(&self, path: &str) -> Result<()> {
        let url = self.url(path);
        let req = self.base_request(Method::DELETE, &url)
            .body(Body::empty())
            .context("Construction requête DELETE")?;
        let (status, body, headers) = self.send(req).await?;
        self.check_rate_limit(&headers);
        if status == 204 || status == 200 { return Ok(()); }
        let msg = self.extract_error_message(&body);
        bail!("GitHub DELETE {}: {} — {}", path, status, msg);
    }

    // Upload binaire (pour les assets de release)
    async fn upload_binary(&self, url: &str, content_type: &str, data: Vec<u8>) -> Result<Value> {
        let req = Request::builder()
            .method(Method::POST)
            .uri(url)
            .header("Authorization",  format!("Bearer {}", self.token))
            .header("Accept",         "application/vnd.github+json")
            .header("Content-Type",   content_type)
            .header("User-Agent",     "ilocker/1.10.6")
            .header("Content-Length", data.len())
            .body(Body::from(data))
            .context("Construction requête upload")?;
        let (status, body, _) = self.send(req).await?;
        self.parse_response(status, &body, "POST", url)
    }

    fn parse_response(&self, status: u16, body: &[u8], method: &str, path: &str) -> Result<Value> {
        if body.is_empty() && (status == 204 || status == 201) {
            return Ok(Value::Null);
        }
        let v: Value = serde_json::from_slice(body)
            .unwrap_or(Value::String(String::from_utf8_lossy(body).to_string()));

        if status >= 400 {
            let msg = self.extract_error_message_from_value(&v);
            // Messages spécifiques selon le code
            let hint = match status {
                401 => " — Vérifiez votre token (iloc connect github)",
                403 => " — Permissions insuffisantes sur ce token",
                404 => " — Ressource introuvable (repo privé ou supprimé ?)",
                409 => " — Conflit (la ressource existe déjà ?)",
                422 => " — Données invalides",
                _   => "",
            };
            bail!("GitHub {} {}: {} — {}{}", method, path, status, msg, hint);
        }
        Ok(v)
    }

    fn extract_error_message(&self, body: &[u8]) -> String {
        if let Ok(v) = serde_json::from_slice::<Value>(body) {
            self.extract_error_message_from_value(&v)
        } else {
            String::from_utf8_lossy(body).to_string()
        }
    }

    fn extract_error_message_from_value(&self, v: &Value) -> String {
        // Si le corps n'était pas du JSON valide, parse_response() l'a
        // enveloppé en Value::String(texte_brut) — indexer par clé dessus
        // retourne toujours Null, donc "Erreur inconnue" masquerait un
        // message parfaitement lisible (page d'erreur d'un proxy, HTML, etc).
        if let Some(raw) = v.as_str() {
            let trimmed = raw.trim();
            if !trimmed.is_empty() {
                return trimmed.chars().take(300).collect();
            }
        }
        // GitHub retourne soit "message" soit "errors[].message"
        let main = v["message"].as_str().unwrap_or("Erreur inconnue");
        if let Some(errors) = v["errors"].as_array() {
            let detail: Vec<String> = errors.iter()
                .filter_map(|e| e["message"].as_str().or(e["code"].as_str()))
                .map(|s| s.to_string())
                .collect();
            if !detail.is_empty() {
                return format!("{} ({})", main, detail.join(", "));
            }
        }
        main.to_string()
    }

    fn check_rate_limit(&self, headers: &[(String, String)]) {
        let remaining = headers.iter()
            .find(|(k, _)| k == "x-ratelimit-remaining")
            .and_then(|(_, v)| v.parse::<u64>().ok())
            .unwrap_or(u64::MAX);
        if remaining < 10 {
            let reset = headers.iter()
                .find(|(k, _)| k == "x-ratelimit-reset")
                .and_then(|(_, v)| v.parse::<u64>().ok())
                .unwrap_or(0);
            let now  = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default().as_secs();
            let wait = reset.saturating_sub(now);
            eprintln!(
                "  {} Rate limit GitHub : {} requête(s) restante(s), reset dans {}s",
                "⚠".to_string(), remaining, wait
            );
        }
    }

    // Pagination automatique — récupère TOUTES les pages
    async fn get_all_pages(&self, path: &str) -> Result<Vec<Value>> {
        let mut results = Vec::new();
        let base = format!("{}/{}", self.api_url, path.trim_start_matches('/'));
        // `path` peut déjà contenir son propre "?...", auquel cas il faut
        // enchaîner avec "&", pas "?" (sinon URL avec deux points
        // d'interrogation — toléré par GitHub actuellement mais objectivement
        // malformé et non garanti sur GitHub Enterprise ou une version future).
        let sep = if base.contains('?') { '&' } else { '?' };
        let mut url = format!("{}{}per_page=100", base, sep);

        loop {
            let req = self.base_request(Method::GET, &url)
                .body(Body::empty())
                .context("Construction requête GET paginée")?;
            let (status, body, headers) = self.send(req).await?;
            self.check_rate_limit(&headers);

            let page = self.parse_response(status, &body, "GET", path)?;
            if let Some(items) = page.as_array() {
                results.extend(items.iter().cloned());
            }

            // Trouver le lien "next" dans le header Link
            let next_url = headers.iter()
                .find(|(k, _)| k == "link")
                .and_then(|(_, v)| parse_next_link(v));
            match next_url {
                Some(next) => url = next,
                None       => break,
            }
        }
        Ok(results)
    }
}

// ── Parse header Link ─────────────────────────────────────────

fn parse_next_link(link_header: &str) -> Option<String> {
    for part in link_header.split(',') {
        let segments: Vec<&str> = part.trim().split(';').collect();
        if segments.len() == 2 {
            let rel = segments[1].trim();
            if rel == "rel=\"next\"" {
                let url = segments[0].trim().trim_matches('<').trim_matches('>');
                return Some(url.to_string());
            }
        }
    }
    None
}

// ── API : Authentification & User ─────────────────────────────

impl GitHubClient {
    /// Valide le token et retourne l'utilisateur authentifié.
    pub async fn get_authenticated_user(&self) -> Result<GhUser> {
        let v = self.get("user").await?;
        serde_json::from_value(v).context("Parse GhUser")
    }

    /// Retourne les orgs auxquelles l'utilisateur appartient.
    pub async fn list_user_orgs(&self) -> Result<Vec<GhOrg>> {
        let v = self.get_all_pages("user/orgs").await?;
        serde_json::from_value(Value::Array(v)).context("Parse orgs")
    }

    /// Retourne les repos de l'utilisateur (avec pagination complète).
    pub async fn list_user_repos(&self, affiliation: &str) -> Result<Vec<GhRepo>> {
        let path = format!("user/repos?affiliation={}&sort=updated&per_page=100", affiliation);
        let v = self.get_all_pages(&path).await?;
        serde_json::from_value(Value::Array(v)).context("Parse user repos")
    }
}

// ── API : Repositories ────────────────────────────────────────

impl GitHubClient {
    /// Crée un repo (personnel ou dans une org).
    pub async fn create_repo(
        &self,
        name:        &str,
        description: Option<&str>,
        private:     bool,
        auto_init:   bool,
        org:         Option<&str>,
        homepage:    Option<&str>,
        license:     Option<&str>,
        gitignore:   Option<&str>,
    ) -> Result<GhRepo> {
        let mut payload = serde_json::json!({
            "name":         name,
            "private":      private,
            "auto_init":    auto_init,
            "has_issues":   true,
            "has_projects": true,
            "has_wiki":     true,
        });
        if let Some(d) = description { payload["description"]  = d.into(); }
        if let Some(h) = homepage    { payload["homepage"]     = h.into(); }
        if let Some(l) = license     { payload["license_template"] = l.into(); }
        if let Some(g) = gitignore   { payload["gitignore_template"] = g.into(); }

        let path = match org {
            Some(o) => format!("orgs/{}/repos", o),
            None    => "user/repos".to_string(),
        };
        let v = self.post(&path, &payload).await?;
        let repo: GhRepo = serde_json::from_value(v).context("Parse créer repo")?;
        Ok(repo)
    }

    /// Récupère un repo.
    pub async fn get_repo(&self, owner: &str, repo: &str) -> Result<GhRepo> {
        let v = self.get(&format!("repos/{}/{}", owner, repo)).await?;
        serde_json::from_value(v).context("Parse get_repo")
    }

    /// Met à jour les métadonnées d'un repo.
    pub async fn update_repo(
        &self,
        owner:       &str,
        repo:        &str,
        name:        Option<&str>,
        description: Option<&str>,
        homepage:    Option<&str>,
        private:     Option<bool>,
        archived:    Option<bool>,
        default_branch: Option<&str>,
        has_issues:  Option<bool>,
        has_wiki:    Option<bool>,
        has_projects: Option<bool>,
    ) -> Result<GhRepo> {
        let mut payload = serde_json::json!({});
        if let Some(n) = name            { payload["name"]            = n.into(); }
        if let Some(d) = description    { payload["description"]    = d.into(); }
        if let Some(h) = homepage       { payload["homepage"]       = h.into(); }
        if let Some(p) = private        { payload["private"]        = p.into(); }
        if let Some(a) = archived       { payload["archived"]       = a.into(); }
        if let Some(b) = default_branch { payload["default_branch"] = b.into(); }
        if let Some(i) = has_issues     { payload["has_issues"]     = i.into(); }
        if let Some(w) = has_wiki       { payload["has_wiki"]       = w.into(); }
        if let Some(p) = has_projects   { payload["has_projects"]   = p.into(); }
        let v = self.patch(&format!("repos/{}/{}", owner, repo), &payload).await?;
        serde_json::from_value(v).context("Parse update_repo")
    }

    /// Supprime un repo (irréversible — confirmation demandée côté commande).
    pub async fn delete_repo(&self, owner: &str, repo: &str) -> Result<()> {
        self.delete(&format!("repos/{}/{}", owner, repo)).await
    }

    /// Fork un repo dans le compte ou une org.
    pub async fn fork_repo(&self, owner: &str, repo: &str, org: Option<&str>) -> Result<GhRepo> {
        let mut payload = serde_json::json!({});
        if let Some(o) = org { payload["organization"] = o.into(); }
        let v = self.post(&format!("repos/{}/{}/forks", owner, repo), &payload).await?;
        serde_json::from_value(v).context("Parse fork_repo")
    }

    /// Remplace les topics d'un repo.
    pub async fn replace_topics(&self, full_name: &str, topics: &[String]) -> Result<()> {
        let payload = serde_json::json!({ "names": topics });
        self.put(&format!("repos/{}/topics", full_name), &payload).await?;
        Ok(())
    }

    /// Transfert un repo vers un autre owner/org.
    pub async fn transfer_repo(
        &self,
        owner: &str,
        repo:  &str,
        new_owner: &str,
        team_ids:  &[u64],
    ) -> Result<GhRepo> {
        let payload = serde_json::json!({
            "new_owner": new_owner,
            "team_ids":  team_ids,
        });
        let v = self.post(&format!("repos/{}/{}/transfer", owner, repo), &payload).await?;
        serde_json::from_value(v).context("Parse transfer_repo")
    }

    /// Liste les repos d'une org (pagination complète).
    pub async fn list_org_repos(&self, org: &str, repo_type: &str) -> Result<Vec<GhRepo>> {
        let path = format!("orgs/{}/repos?type={}&sort=updated", org, repo_type);
        let v = self.get_all_pages(&path).await?;
        serde_json::from_value(Value::Array(v)).context("Parse list_org_repos")
    }

    /// Recherche de repos.
    pub async fn search_repos(&self, query: &str) -> Result<GhSearchResult<GhRepo>> {
        let encoded = query.replace(' ', "+");
        let v = self.get(&format!("search/repositories?q={}&per_page=30", encoded)).await?;
        serde_json::from_value(v).context("Parse search_repos")
    }
}

// ── API : Branches ────────────────────────────────────────────

impl GitHubClient {
    /// Liste les branches (pagination complète).
    pub async fn list_branches(&self, owner: &str, repo: &str) -> Result<Vec<GhBranch>> {
        let v = self.get_all_pages(&format!("repos/{}/{}/branches", owner, repo)).await?;
        serde_json::from_value(Value::Array(v)).context("Parse branches")
    }

    /// Crée une branche depuis un SHA ou une branche existante.
    pub async fn create_branch(
        &self,
        owner: &str,
        repo:  &str,
        name:  &str,
        from:  &str,     // SHA ou "refs/heads/<nom>"
    ) -> Result<()> {
        // Résoudre le SHA si `from` est un nom de branche
        let sha = if from.len() == 40 && from.chars().all(|c| c.is_ascii_hexdigit()) {
            from.to_string()
        } else {
            let branch = self.get_branch(owner, repo, from).await?;
            branch.commit.sha
        };
        let payload = serde_json::json!({
            "ref": format!("refs/heads/{}", name),
            "sha": sha,
        });
        self.post(&format!("repos/{}/{}/git/refs", owner, repo), &payload).await?;
        Ok(())
    }

    /// Récupère une branche.
    pub async fn get_branch(&self, owner: &str, repo: &str, branch: &str) -> Result<GhBranch> {
        let v = self.get(&format!("repos/{}/{}/branches/{}", owner, repo, branch)).await?;
        serde_json::from_value(v).context("Parse get_branch")
    }

    /// Supprime une branche.
    pub async fn delete_branch(&self, owner: &str, repo: &str, branch: &str) -> Result<()> {
        self.delete(&format!("repos/{}/{}/git/refs/heads/{}", owner, repo, branch)).await
    }

    /// Renomme la branche par défaut.
    pub async fn rename_branch(
        &self,
        owner:    &str,
        repo:     &str,
        old_name: &str,
        new_name: &str,
    ) -> Result<GhBranch> {
        let payload = serde_json::json!({ "new_name": new_name });
        let v = self.post(
            &format!("repos/{}/{}/branches/{}/rename", owner, repo, old_name),
            &payload,
        ).await?;
        serde_json::from_value(v).context("Parse rename_branch")
    }

    /// Configure la protection d'une branche.
    pub async fn protect_branch(
        &self,
        owner:                    &str,
        repo:                     &str,
        branch:                   &str,
        required_status_checks:   &[&str],
        require_pr_reviews:       bool,
        required_approving_count: u32,
        enforce_admins:           bool,
        require_linear_history:   bool,
        allow_force_pushes:       bool,
        allow_deletions:          bool,
    ) -> Result<()> {
        let checks: Vec<Value> = required_status_checks.iter()
            .map(|c| serde_json::json!({ "context": c, "app_id": -1 }))
            .collect();

        let payload = serde_json::json!({
            "required_status_checks": if checks.is_empty() { Value::Null } else {
                serde_json::json!({ "strict": true, "checks": checks })
            },
            "enforce_admins": enforce_admins,
            "required_pull_request_reviews": if require_pr_reviews {
                serde_json::json!({
                    "required_approving_review_count": required_approving_count,
                    "dismiss_stale_reviews": true,
                })
            } else { Value::Null },
            "restrictions": Value::Null,
            "required_linear_history": require_linear_history,
            "allow_force_pushes":  allow_force_pushes,
            "allow_deletions":     allow_deletions,
        });
        self.put(
            &format!("repos/{}/{}/branches/{}/protection", owner, repo, branch),
            &payload,
        ).await?;
        Ok(())
    }

    /// Retire la protection d'une branche.
    pub async fn unprotect_branch(&self, owner: &str, repo: &str, branch: &str) -> Result<()> {
        self.delete(&format!("repos/{}/{}/branches/{}/protection", owner, repo, branch)).await
    }
}

// ── API : Issues ──────────────────────────────────────────────

impl GitHubClient {
    /// Crée une issue.
    pub async fn create_issue(
        &self,
        owner:     &str,
        repo:      &str,
        title:     &str,
        body:      Option<&str>,
        labels:    &[String],
        assignees: &[String],
        milestone: Option<u64>,
    ) -> Result<GhIssue> {
        let mut payload = serde_json::json!({
            "title":     title,
            "labels":    labels,
            "assignees": assignees,
        });
        if let Some(b) = body      { payload["body"]      = b.into(); }
        if let Some(m) = milestone { payload["milestone"] = m.into(); }
        let v = self.post(&format!("repos/{}/{}/issues", owner, repo), &payload).await?;
        serde_json::from_value(v).context("Parse create_issue")
    }

    /// Récupère une issue par son numéro — lookup direct (pas de liste
    /// complète à filtrer côté client, plus rapide et pas soumis au léger
    /// délai de cohérence que peut avoir l'endpoint de liste juste après
    /// une création).
    pub async fn get_issue(&self, owner: &str, repo: &str, number: u64) -> Result<GhIssue> {
        let v = self.get(&format!("repos/{}/{}/issues/{}", owner, repo, number)).await?;
        serde_json::from_value(v).context("Parse get_issue")
    }

    /// Liste les issues (filtres : state, labels, assignee, milestone).
    pub async fn list_issues(
        &self,
        owner:    &str,
        repo:     &str,
        state:    &str,         // "open" | "closed" | "all"
        labels:   &[String],
        assignee: Option<&str>,
    ) -> Result<Vec<GhIssue>> {
        let mut qs = format!("state={}", state);
        if !labels.is_empty() { qs.push_str(&format!("&labels={}", labels.join(","))); }
        if let Some(a) = assignee { qs.push_str(&format!("&assignee={}", a)); }
        let path = format!("repos/{}/{}/issues?{}&per_page=100", owner, repo, qs);
        let v = self.get_all_pages(&path).await?;
        let issues: Vec<GhIssue> = serde_json::from_value(Value::Array(v))?;
        // Filtrer les PRs (l'API issues retourne aussi les PRs)
        Ok(issues.into_iter().filter(|i| i.pull_request.is_none()).collect())
    }

    /// Met à jour une issue (titre, body, state, labels, assignees).
    pub async fn update_issue(
        &self,
        owner:     &str,
        repo:      &str,
        number:    u64,
        title:     Option<&str>,
        body:      Option<&str>,
        state:     Option<&str>,
        labels:    Option<&[String]>,
        assignees: Option<&[String]>,
        state_reason: Option<&str>,
    ) -> Result<GhIssue> {
        let mut payload = serde_json::json!({});
        if let Some(t) = title     { payload["title"]     = t.into(); }
        if let Some(b) = body      { payload["body"]      = b.into(); }
        if let Some(s) = state     { payload["state"]     = s.into(); }
        if let Some(l) = labels    { payload["labels"]    = serde_json::to_value(l)?; }
        if let Some(a) = assignees { payload["assignees"] = serde_json::to_value(a)?; }
        if let Some(r) = state_reason { payload["state_reason"] = r.into(); }
        let v = self.patch(&format!("repos/{}/{}/issues/{}", owner, repo, number), &payload).await?;
        serde_json::from_value(v).context("Parse update_issue")
    }

    /// Verrouille une issue.
    pub async fn lock_issue(&self, owner: &str, repo: &str, number: u64, reason: &str) -> Result<()> {
        let payload = serde_json::json!({ "lock_reason": reason });
        self.put(&format!("repos/{}/{}/issues/{}/lock", owner, repo, number), &payload).await?;
        Ok(())
    }

    /// Déverrouille une issue.
    pub async fn unlock_issue(&self, owner: &str, repo: &str, number: u64) -> Result<()> {
        self.delete(&format!("repos/{}/{}/issues/{}/lock", owner, repo, number)).await
    }

    /// Ajoute un commentaire sur une issue ou une PR.
    pub async fn add_comment(&self, owner: &str, repo: &str, number: u64, body: &str) -> Result<()> {
        let payload = serde_json::json!({ "body": body });
        self.post(&format!("repos/{}/{}/issues/{}/comments", owner, repo, number), &payload).await?;
        Ok(())
    }

    /// Liste les commentaires d'une issue.
    pub async fn list_comments(&self, owner: &str, repo: &str, number: u64) -> Result<Vec<Value>> {
        self.get_all_pages(&format!("repos/{}/{}/issues/{}/comments", owner, repo, number)).await
    }

    /// Crée des labels dans un repo.
    pub async fn create_label(
        &self,
        owner: &str,
        repo:  &str,
        name:  &str,
        color: &str,
        desc:  Option<&str>,
    ) -> Result<GhLabel> {
        let mut payload = serde_json::json!({ "name": name, "color": color });
        if let Some(d) = desc { payload["description"] = d.into(); }
        let v = self.post(&format!("repos/{}/{}/labels", owner, repo), &payload).await?;
        serde_json::from_value(v).context("Parse create_label")
    }

    /// Liste les labels d'un repo.
    pub async fn list_labels(&self, owner: &str, repo: &str) -> Result<Vec<GhLabel>> {
        let v = self.get_all_pages(&format!("repos/{}/{}/labels", owner, repo)).await?;
        serde_json::from_value(Value::Array(v)).context("Parse labels")
    }
}

// ── API : Pull Requests ───────────────────────────────────────

impl GitHubClient {
    /// Crée une PR.
    pub async fn create_pr(
        &self,
        owner:  &str,
        repo:   &str,
        title:  &str,
        body:   Option<&str>,
        head:   &str,           // "branch" ou "user:branch"
        base:   &str,
        draft:  bool,
    ) -> Result<GhPullRequest> {
        let mut payload = serde_json::json!({
            "title": title,
            "head":  head,
            "base":  base,
            "draft": draft,
        });
        if let Some(b) = body { payload["body"] = b.into(); }
        let v = self.post(&format!("repos/{}/{}/pulls", owner, repo), &payload).await?;
        serde_json::from_value(v).context("Parse create_pr")
    }

    /// Liste les PRs.
    pub async fn list_prs(
        &self,
        owner: &str,
        repo:  &str,
        state: &str,    // "open" | "closed" | "all"
        base:  Option<&str>,
    ) -> Result<Vec<GhPullRequest>> {
        let mut qs = format!("state={}&per_page=100", state);
        if let Some(b) = base { qs.push_str(&format!("&base={}", b)); }
        let v = self.get_all_pages(&format!("repos/{}/{}/pulls?{}", owner, repo, qs)).await?;
        serde_json::from_value(Value::Array(v)).context("Parse PRs")
    }

    /// Récupère une PR.
    pub async fn get_pr(&self, owner: &str, repo: &str, number: u64) -> Result<GhPullRequest> {
        let v = self.get(&format!("repos/{}/{}/pulls/{}", owner, repo, number)).await?;
        serde_json::from_value(v).context("Parse get_pr")
    }

    /// Merge une PR.
    pub async fn merge_pr(
        &self,
        owner:         &str,
        repo:          &str,
        number:        u64,
        commit_title:  Option<&str>,
        commit_msg:    Option<&str>,
        merge_method:  &str,    // "merge" | "squash" | "rebase"
    ) -> Result<()> {
        let mut payload = serde_json::json!({ "merge_method": merge_method });
        if let Some(t) = commit_title { payload["commit_title"]   = t.into(); }
        if let Some(m) = commit_msg   { payload["commit_message"] = m.into(); }
        self.put(&format!("repos/{}/{}/pulls/{}/merge", owner, repo, number), &payload).await?;
        Ok(())
    }

    /// Demande une review.
    pub async fn request_reviewers(
        &self,
        owner:    &str,
        repo:     &str,
        number:   u64,
        users:    &[String],
        teams:    &[String],
    ) -> Result<()> {
        let payload = serde_json::json!({
            "reviewers":      users,
            "team_reviewers": teams,
        });
        self.post(
            &format!("repos/{}/{}/pulls/{}/requested_reviewers", owner, repo, number),
            &payload,
        ).await?;
        Ok(())
    }

    /// Met à jour la branche d'une PR (merge base dans head).
    pub async fn update_pr_branch(&self, owner: &str, repo: &str, number: u64) -> Result<()> {
        let payload = serde_json::json!({});
        self.put(
            &format!("repos/{}/{}/pulls/{}/update-branch", owner, repo, number),
            &payload,
        ).await?;
        Ok(())
    }

    /// Convertit une PR en draft ou la sort du mode draft.
    pub async fn set_pr_draft(&self, pr_node_id: &str, draft: bool) -> Result<()> {
        // Nécessite GraphQL (l'API REST ne supporte pas le toggle draft)
        let mutation = if draft {
            format!(r#"mutation {{ convertPullRequestToDraft(input: {{pullRequestId: "{}"}}) {{ pullRequest {{ isDraft }} }} }}"#, pr_node_id)
        } else {
            format!(r#"mutation {{ markPullRequestReadyForReview(input: {{pullRequestId: "{}"}}) {{ pullRequest {{ isDraft }} }} }}"#, pr_node_id)
        };
        self.graphql(&mutation).await?;
        Ok(())
    }
}

// ── API : Releases ────────────────────────────────────────────

impl GitHubClient {
    /// Crée une release.
    pub async fn create_release(
        &self,
        owner:        &str,
        repo:         &str,
        tag:          &str,
        name:         Option<&str>,
        body:         Option<&str>,
        draft:        bool,
        prerelease:   bool,
        target:       Option<&str>,     // branch ou SHA
        generate_notes: bool,
    ) -> Result<GhRelease> {
        let mut payload = serde_json::json!({
            "tag_name":         tag,
            "draft":            draft,
            "prerelease":       prerelease,
            "generate_release_notes": generate_notes,
        });
        if let Some(n) = name   { payload["name"]              = n.into(); }
        if let Some(b) = body   { payload["body"]              = b.into(); }
        if let Some(t) = target { payload["target_commitish"]  = t.into(); }
        let v = self.post(&format!("repos/{}/{}/releases", owner, repo), &payload).await?;
        serde_json::from_value(v).context("Parse create_release")
    }

    /// Liste les releases (pagination complète).
    pub async fn list_releases(&self, owner: &str, repo: &str) -> Result<Vec<GhRelease>> {
        let v = self.get_all_pages(&format!("repos/{}/{}/releases", owner, repo)).await?;
        serde_json::from_value(Value::Array(v)).context("Parse releases")
    }

    /// Récupère la dernière release publiée.
    pub async fn latest_release(&self, owner: &str, repo: &str) -> Result<GhRelease> {
        let v = self.get(&format!("repos/{}/{}/releases/latest", owner, repo)).await?;
        serde_json::from_value(v).context("Parse latest_release")
    }

    /// Récupère une release par son tag — lookup direct (pas de liste
    /// complète à filtrer côté client). Même raison que get_issue : plus
    /// rapide, et pas soumis au léger délai de cohérence de l'endpoint de
    /// liste juste après une création.
    pub async fn get_release_by_tag(&self, owner: &str, repo: &str, tag: &str) -> Result<GhRelease> {
        let v = self.get(&format!("repos/{}/{}/releases/tags/{}", owner, repo, tag)).await?;
        serde_json::from_value(v).context("Parse get_release_by_tag")
    }

    /// Upload un fichier comme asset d'une release.
    pub async fn upload_release_asset(
        &self,
        upload_url:   &str,
        name:         &str,
        content_type: &str,
        data:         Vec<u8>,
    ) -> Result<GhReleaseAsset> {
        // L'upload URL contient {?name,label} — on la parse et on ajoute ?name=
        let base = upload_url.split('{').next().unwrap_or(upload_url);
        let url  = format!("{}?name={}", base.trim_end_matches('/'), name);
        let v    = self.upload_binary(&url, content_type, data).await?;
        serde_json::from_value(v).context("Parse release asset")
    }

    /// Supprime une release.
    pub async fn delete_release(&self, owner: &str, repo: &str, release_id: u64) -> Result<()> {
        self.delete(&format!("repos/{}/{}/releases/{}", owner, repo, release_id)).await
    }
}

// ── API : GitHub Actions ──────────────────────────────────────

impl GitHubClient {
    /// Liste les workflows d'un repo.
    pub async fn list_workflows(&self, owner: &str, repo: &str) -> Result<Vec<GhWorkflow>> {
        let v = self.get(&format!("repos/{}/{}/actions/workflows", owner, repo)).await?;
        let arr = v["workflows"].as_array().cloned().unwrap_or_default();
        serde_json::from_value(Value::Array(arr)).context("Parse workflows")
    }

    /// Déclenche un workflow (workflow_dispatch).
    pub async fn trigger_workflow(
        &self,
        owner:       &str,
        repo:        &str,
        workflow_id: &str,      // nom du fichier (ci.yml) ou ID
        branch:      &str,
        inputs:      &serde_json::Map<String, Value>,
    ) -> Result<()> {
        let payload = serde_json::json!({
            "ref":    branch,
            "inputs": inputs,
        });
        self.post(
            &format!("repos/{}/{}/actions/workflows/{}/dispatches", owner, repo, workflow_id),
            &payload,
        ).await?;
        Ok(())
    }

    /// Liste les runs d'un workflow.
    pub async fn list_workflow_runs(
        &self,
        owner:       &str,
        repo:        &str,
        workflow_id: Option<&str>,
        branch:      Option<&str>,
        status:      Option<&str>,
    ) -> Result<Vec<GhWorkflowRun>> {
        let base = match workflow_id {
            Some(id) => format!("repos/{}/{}/actions/workflows/{}/runs", owner, repo, id),
            None     => format!("repos/{}/{}/actions/runs", owner, repo),
        };
        let mut qs = "per_page=30".to_string();
        if let Some(b) = branch { qs.push_str(&format!("&branch={}", b)); }
        if let Some(s) = status { qs.push_str(&format!("&status={}", s)); }
        let v = self.get(&format!("{}?{}", base, qs)).await?;
        let arr = v["workflow_runs"].as_array().cloned().unwrap_or_default();
        serde_json::from_value(Value::Array(arr)).context("Parse runs")
    }

    /// Récupère un run par ID.
    pub async fn get_workflow_run(&self, owner: &str, repo: &str, run_id: u64) -> Result<GhWorkflowRun> {
        let v = self.get(&format!("repos/{}/{}/actions/runs/{}", owner, repo, run_id)).await?;
        serde_json::from_value(v).context("Parse get_run")
    }

    /// Annule un run.
    pub async fn cancel_workflow_run(&self, owner: &str, repo: &str, run_id: u64) -> Result<()> {
        self.post(
            &format!("repos/{}/{}/actions/runs/{}/cancel", owner, repo, run_id),
            &serde_json::json!({}),
        ).await?;
        Ok(())
    }

    /// Relance un run.
    pub async fn rerun_workflow(&self, owner: &str, repo: &str, run_id: u64) -> Result<()> {
        self.post(
            &format!("repos/{}/{}/actions/runs/{}/rerun", owner, repo, run_id),
            &serde_json::json!({}),
        ).await?;
        Ok(())
    }
}

// ── API : Secrets (Actions) ───────────────────────────────────

impl GitHubClient {
    /// Récupère la clé publique du repo (pour chiffrer les secrets).
    pub async fn get_repo_public_key(&self, owner: &str, repo: &str) -> Result<(String, String)> {
        let v = self.get(&format!("repos/{}/{}/actions/secrets/public-key", owner, repo)).await?;
        let key_id  = v["key_id"].as_str().unwrap_or("").to_string();
        let key_b64 = v["key"].as_str().unwrap_or("").to_string();
        Ok((key_id, key_b64))
    }

    /// Crée ou met à jour un secret Actions.
    /// Le chiffrement libsodium (sealed box) est géré côté commande.
    pub async fn set_repo_secret(
        &self,
        owner:           &str,
        repo:            &str,
        secret_name:     &str,
        encrypted_value: &str,
        key_id:          &str,
    ) -> Result<()> {
        let payload = serde_json::json!({
            "encrypted_value": encrypted_value,
            "key_id":          key_id,
        });
        self.put(
            &format!("repos/{}/{}/actions/secrets/{}", owner, repo, secret_name),
            &payload,
        ).await?;
        Ok(())
    }

    /// Supprime un secret.
    pub async fn delete_repo_secret(&self, owner: &str, repo: &str, name: &str) -> Result<()> {
        self.delete(&format!("repos/{}/{}/actions/secrets/{}", owner, repo, name)).await
    }

    /// Liste les secrets (noms seulement — valeurs jamais exposées par l'API).
    pub async fn list_repo_secrets(&self, owner: &str, repo: &str) -> Result<Vec<String>> {
        let v = self.get(&format!("repos/{}/{}/actions/secrets", owner, repo)).await?;
        let secrets = v["secrets"].as_array().cloned().unwrap_or_default();
        Ok(secrets.iter()
            .filter_map(|s| s["name"].as_str().map(|n| n.to_string()))
            .collect())
    }
}

// ── API : Collaborateurs ──────────────────────────────────────

impl GitHubClient {
    /// Ajoute un collaborateur.
    pub async fn add_collaborator(
        &self,
        owner:      &str,
        repo:       &str,
        username:   &str,
        permission: &str,   // "pull" | "push" | "admin" | "maintain" | "triage"
    ) -> Result<()> {
        let payload = serde_json::json!({ "permission": permission });
        self.put(
            &format!("repos/{}/{}/collaborators/{}", owner, repo, username),
            &payload,
        ).await?;
        Ok(())
    }

    /// Supprime un collaborateur.
    pub async fn remove_collaborator(&self, owner: &str, repo: &str, username: &str) -> Result<()> {
        self.delete(&format!("repos/{}/{}/collaborators/{}", owner, repo, username)).await
    }

    /// Liste les collaborateurs.
    pub async fn list_collaborators(&self, owner: &str, repo: &str) -> Result<Vec<GhCollaborator>> {
        let v = self.get_all_pages(&format!("repos/{}/{}/collaborators", owner, repo)).await?;
        serde_json::from_value(Value::Array(v)).context("Parse collaborators")
    }
}

// ── API : Webhooks ────────────────────────────────────────────

impl GitHubClient {
    /// Crée un webhook.
    pub async fn create_webhook(
        &self,
        owner:        &str,
        repo:         &str,
        url:          &str,
        events:       &[&str],
        content_type: &str,
        secret:       Option<&str>,
        active:       bool,
    ) -> Result<GhWebhook> {
        let mut config = serde_json::json!({
            "url":          url,
            "content_type": content_type,
            "insecure_ssl": "0",
        });
        if let Some(s) = secret { config["secret"] = s.into(); }
        let payload = serde_json::json!({
            "name":   "web",
            "active": active,
            "events": events,
            "config": config,
        });
        let v = self.post(&format!("repos/{}/{}/hooks", owner, repo), &payload).await?;
        serde_json::from_value(v).context("Parse create_webhook")
    }

    /// Liste les webhooks d'un repo.
    pub async fn list_webhooks(&self, owner: &str, repo: &str) -> Result<Vec<GhWebhook>> {
        let v = self.get_all_pages(&format!("repos/{}/{}/hooks", owner, repo)).await?;
        serde_json::from_value(Value::Array(v)).context("Parse webhooks")
    }

    /// Supprime un webhook.
    pub async fn delete_webhook(&self, owner: &str, repo: &str, hook_id: u64) -> Result<()> {
        self.delete(&format!("repos/{}/{}/hooks/{}", owner, repo, hook_id)).await
    }

    /// Envoie un ping à un webhook (test).
    pub async fn ping_webhook(&self, owner: &str, repo: &str, hook_id: u64) -> Result<()> {
        self.post(
            &format!("repos/{}/{}/hooks/{}/pings", owner, repo, hook_id),
            &serde_json::json!({}),
        ).await?;
        Ok(())
    }
}

// ── API : GraphQL (utilisé pour les features non disponibles en REST) ──

impl GitHubClient {
    pub async fn graphql(&self, query: &str) -> Result<Value> {
        let payload = serde_json::json!({ "query": query });
        let url     = "https://api.github.com/graphql";
        let body    = serde_json::to_vec(&payload)?;
        let req     = Request::builder()
            .method(Method::POST)
            .uri(url)
            .header("Authorization",  format!("Bearer {}", self.token))
            .header("Content-Type",   "application/json")
            .header("User-Agent",     "ilocker/1.10.6")
            .body(Body::from(body))
            .context("Construction requête GraphQL")?;
        let (status, resp_body, _) = self.send(req).await?;
        self.parse_response(status, &resp_body, "POST", "graphql")
    }
}

// ── Tests ──────────────────────────────────────────────────────
//
// parse_next_link est critique : un bug ici ne casse rien à la compilation
// ni au premier appel (une seule page suffit pour un petit repo de test),
// mais silencieusement tronquerait les résultats dès qu'un utilisateur a
// plus de 100 repos/issues/PRs. C'est exactement le genre de bug qui ne
// se voit qu'en conditions réelles à grande échelle — donc testé ici en
// isolation avec des headers Link réels tels que GitHub les produit.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_real_github_link_header_with_next() {
        // Format exact tel que retourné par l'API GitHub (RFC 5988)
        let header = r#"<https://api.github.com/user/repos?page=2>; rel="next", <https://api.github.com/user/repos?page=5>; rel="last""#;
        let next = parse_next_link(header);
        assert_eq!(next, Some("https://api.github.com/user/repos?page=2".to_string()));
    }

    #[test]
    fn parses_link_header_middle_page_has_prev_next_last_first() {
        let header = concat!(
            r#"<https://api.github.com/user/repos?page=3>; rel="prev", "#,
            r#"<https://api.github.com/user/repos?page=5>; rel="next", "#,
            r#"<https://api.github.com/user/repos?page=10>; rel="last", "#,
            r#"<https://api.github.com/user/repos?page=1>; rel="first""#,
        );
        let next = parse_next_link(header);
        assert_eq!(next, Some("https://api.github.com/user/repos?page=5".to_string()));
    }

    #[test]
    fn last_page_has_no_next_returns_none() {
        // Dernière page : seulement rel="prev" et rel="first", pas de "next"
        let header = r#"<https://api.github.com/user/repos?page=1>; rel="prev", <https://api.github.com/user/repos?page=1>; rel="first""#;
        assert_eq!(parse_next_link(header), None);
    }

    #[test]
    fn single_page_no_link_header_content() {
        assert_eq!(parse_next_link(""), None);
    }

    #[test]
    fn gh_pull_request_deserializes_node_id_from_real_api_shape() {
        // Réponse représentative de l'API REST GitHub pour une PR (champs réellement
        // renvoyés par GET /repos/{owner}/{repo}/pulls/{number}). Régression du bug
        // où set_pr_draft() recevait head.label ("owner:branche") au lieu du vrai
        // node_id GraphQL — la mutation GraphQL échouait systématiquement.
        let raw = r#"{
            "number": 42,
            "node_id": "PR_kwDOA1b2c3M4Nfa5",
            "title": "feat: ajoute le mode hors-ligne",
            "state": "open",
            "html_url": "https://github.com/alphabechirdiallo-netizen/ilocker/pull/42",
            "body": "Description de la PR",
            "user": {"login": "bechir", "avatar_url": null, "html_url": null, "type": "User"},
            "head": {"label": "bechir:feature-offline", "ref": "feature-offline", "sha": "abc123"},
            "base": {"label": "alphabechirdiallo-netizen:main", "ref": "main", "sha": "def456"},
            "draft": true,
            "mergeable": null,
            "merged": false,
            "created_at": "2026-08-01T10:00:00Z",
            "updated_at": "2026-08-02T10:00:00Z"
        }"#;

        let pr: GhPullRequest = serde_json::from_str(raw).expect("doit se désérialiser");
        assert_eq!(pr.node_id, "PR_kwDOA1b2c3M4Nfa5");
        // Le node_id doit être distinct de head.label : c'est exactement la confusion
        // à l'origine du bug (les deux sont des String, faciles à interchanger par erreur).
        assert_ne!(pr.node_id, pr.head.label);
    }
}

