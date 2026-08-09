// ============================================================
//  api_client.rs — ilocker Cloud API HTTP client  (v1.2.0)
//  Covers: auth (Phase 1) + billing (Phase 2)
// ============================================================

use anyhow::{bail, Context, Result};
use hyper::{Body, Client, Method, Request, Uri};
use hyper::header::{AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize};

// ── Response types ────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct LoginResponse {
    pub message:   String,
    pub jwt:       String,
    pub cli_token: String,
    pub user:      UserInfo,
}

#[derive(Debug, Deserialize)]
pub struct UserInfo {
    pub id:    String,
    pub email: String,
    pub plan:  String,
}

#[derive(Debug, Deserialize)]
pub struct MeResponse {
    pub user: MeUser,
}

#[derive(Debug, Deserialize)]
pub struct MeUser {
    pub id:           String,
    pub email:        String,
    pub display_name: Option<String>,
    pub created_at:   String,
    pub subscription: Option<SubInfo>,
}

#[derive(Debug, Deserialize)]
pub struct SubInfo {
    pub plan:               String,
    pub current_period_end: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ApiError {
    error: String,
}

// ── Client ────────────────────────────────────────────────────

pub struct ApiClient {
    base_url: String,
}

impl ApiClient {
    pub fn new(base_url: &str) -> Self {
        Self { base_url: base_url.trim_end_matches('/').to_string() }
    }

    // ── Auth endpoints (Phase 1) ──────────────────────────────

    /// POST /api/auth/login
    pub async fn login(&self, email: &str, password: &str) -> Result<LoginResponse> {
        let body = serde_json::json!({
            "email":      email,
            "password":   password,
            "token_label": format!("CLI · {}", gethostname_safe()),
        });
        self.post_json("/api/auth/login", &body, None).await
    }

    /// POST /api/auth/logout
    pub async fn logout(&self, cli_token: &str) -> Result<serde_json::Value> {
        self.post_json("/api/auth/logout", &serde_json::json!({}), Some(cli_token)).await
    }

    /// GET /api/auth/me
    pub async fn me(&self, cli_token: &str) -> Result<MeResponse> {
        self.get_json("/api/auth/me", cli_token).await
    }

    // ── Billing endpoints (Phase 2) ───────────────────────────

    /// POST /api/billing/upgrade → { checkoutUrl, sessionId }
    pub async fn create_checkout_session(
        &self,
        plan:   &str,
        bearer: &str,
    ) -> Result<serde_json::Value> {
        let body = serde_json::json!({ "plan": plan });
        self.post_json("/api/billing/upgrade", &body, Some(bearer)).await
    }

    /// POST /api/billing/portal → { portalUrl }
    pub async fn create_portal_session(&self, bearer: &str) -> Result<serde_json::Value> {
        self.post_json("/api/billing/portal", &serde_json::json!({}), Some(bearer)).await
    }

    /// GET /api/billing/status → { plan, currentPeriodEnd, cancelAtPeriodEnd }
    pub async fn billing_status(&self, bearer: &str) -> Result<serde_json::Value> {
        self.get_json("/api/billing/status", bearer).await
    }


    /// GET /api/cloud/verify — confirms PRO plan for cloud features
    pub async fn verify_cloud_access(&self, bearer: &str) -> Result<serde_json::Value> {
        self.get_json("/api/cloud/verify", bearer).await
    }

    // ── Internal helpers ──────────────────────────────────────

    async fn post_json<Req, Res>(&self, path: &str, body: &Req, bearer: Option<&str>) -> Result<Res>
    where
        Req: Serialize,
        Res: for<'de> Deserialize<'de>,
    {
        let url: Uri = format!("{}{}", self.base_url, path)
            .parse()
            .context("Invalid API URL")?;

        let json_bytes = serde_json::to_vec(body)?;

        let mut builder = Request::builder()
            .method(Method::POST)
            .uri(url)
            .header(CONTENT_TYPE, "application/json");

        if let Some(token) = bearer {
            builder = builder.header(AUTHORIZATION, format!("Bearer {}", token));
        }

        let req = builder
            .body(Body::from(json_bytes))
            .context("Failed to build HTTP request")?;

        self.execute(req).await
    }

    async fn get_json<Res>(&self, path: &str, bearer: &str) -> Result<Res>
    where
        Res: for<'de> Deserialize<'de>,
    {
        let url: Uri = format!("{}{}", self.base_url, path)
            .parse()
            .context("Invalid API URL")?;

        let req = Request::builder()
            .method(Method::GET)
            .uri(url)
            .header(AUTHORIZATION, format!("Bearer {}", bearer))
            .body(Body::empty())
            .context("Failed to build HTTP request")?;

        self.execute(req).await
    }

    async fn execute<Res>(&self, req: Request<Body>) -> Result<Res>
    where
        Res: for<'de> Deserialize<'de>,
    {
        let client = Client::new();
        let resp   = client.request(req).await
            .context("Cannot reach the ilocker Cloud API — check your connection")?;

        let status = resp.status();
        let bytes  = hyper::body::to_bytes(resp.into_body()).await
            .context("Failed to read API response body")?;

        if status.is_success() {
            serde_json::from_slice::<Res>(&bytes)
                .context("Unexpected API response format")
        } else {
            let msg = serde_json::from_slice::<ApiError>(&bytes)
                .map(|e| e.error)
                .unwrap_or_else(|_| format!("HTTP {}", status));
            bail!("{}", msg)
        }
    }
}

fn gethostname_safe() -> String {
    std::env::var("HOSTNAME")
        .or_else(|_| std::fs::read_to_string("/etc/hostname").map(|s| s.trim().to_string()))
        .unwrap_or_else(|_| "unknown-host".to_string())
}
