#![allow(dead_code)]
// ============================================================
//  intel_client.rs — ilocker Collective Intelligence client
//  Phase 5A — communicates with /api/intel/* endpoints
// ============================================================

use anyhow::{Context, Result};
use hyper::{Body, Client, Method, Request, Uri};
use hyper::header::{AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize};

// ── Response types ────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ChunkCheckResponse {
    pub universal:       Vec<String>,
    pub universal_count: usize,
    pub checked_count:   usize,
    pub bytes_saved:     u64,
}

#[derive(Debug, Deserialize)]
pub struct IgnorePattern {
    pub pattern:        String,
    pub vote_count:     i64,
    #[serde(default)]
    pub avg_bytes_saved: u64,
}

#[derive(Debug, Deserialize)]
pub struct IgnoreResponse {
    pub project_type: String,
    pub patterns:     Vec<IgnorePattern>,
    pub baseline:     Vec<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct MeshNode {
    pub node_token:      String,
    pub public_endpoint: String,
    pub country_code:    Option<String>,
    pub bandwidth_kbps:  i64,
}

#[derive(Debug, Deserialize)]
pub struct NodesResponse {
    pub nodes: Vec<MeshNode>,
    pub count: usize,
}

#[derive(Debug, Deserialize)]
struct ApiError {
    error: String,
}

// ── Client ────────────────────────────────────────────────────

pub struct IntelClient {
    base_url: String,
    bearer:   Option<String>,
}

impl IntelClient {
    pub fn new(base_url: &str, bearer: Option<&str>) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            bearer:   bearer.map(|s| s.to_string()),
        }
    }

    // ── Feature 1: Global chunk deduplication ─────────────────

    /// Check which chunk hashes are universally known — can skip uploading those.
    /// Returns the set of SHA-256 hashes that are globally available.
    pub async fn check_universal_chunks(
        &self,
        hashes: &[String],
    ) -> Result<ChunkCheckResponse> {
        let body = serde_json::json!({ "hashes": hashes });
        self.post_json("/api/intel/chunks/check", &body).await
    }

    /// Report newly seen chunk hashes to grow the global registry.
    pub async fn report_chunks(
        &self,
        chunks: &[ChunkReport],
    ) -> Result<serde_json::Value> {
        let body = serde_json::json!({ "chunks": chunks });
        self.post_json_authed("/api/intel/chunks/report", &body).await
    }

    // ── Feature 2: Predictive .ilockerignore ──────────────────

    /// Fetch community ignore patterns for a project type.
    pub async fn get_ignore_patterns(
        &self,
        project_type: &str,
    ) -> Result<IgnoreResponse> {
        self.get_json(&format!("/api/intel/ignore/{}", project_type)).await
    }

    /// Submit effective ignore patterns back to the community registry.
    pub async fn report_ignore_patterns(
        &self,
        project_type: &str,
        patterns:     &[String],
        bytes_saved:  u64,
    ) -> Result<serde_json::Value> {
        let body = serde_json::json!({
            "projectType": project_type,
            "patterns":    patterns,
            "bytesSaved":  bytes_saved,
        });
        self.post_json_authed("/api/intel/ignore/report", &body).await
    }

    // ── Feature 4: Mesh node discovery ───────────────────────

    /// Discover nearby mesh nodes (optional country filter).
    pub async fn get_mesh_nodes(
        &self,
        country: Option<&str>,
    ) -> Result<NodesResponse> {
        let path = match country {
            Some(c) => format!("/api/intel/nodes?country={}&limit=10", c),
            None    => "/api/intel/nodes?limit=10".to_string(),
        };
        self.get_json(&path).await
    }

    /// Register this machine as a mesh node (explicit opt-in).
    pub async fn register_mesh_node(
        &self,
        node_token:      &str,
        public_endpoint: &str,
        bandwidth_kbps:  u32,
    ) -> Result<serde_json::Value> {
        let body = serde_json::json!({
            "nodeToken":       node_token,
            "publicEndpoint":  public_endpoint,
            "bandwidthKbps":   bandwidth_kbps,
            "consent":         true,
        });
        self.post_json_authed("/api/intel/nodes/register", &body).await
    }

    /// Send a heartbeat to keep the mesh node registration alive.
    pub async fn node_heartbeat(&self, node_token: &str) -> Result<()> {
        let body = serde_json::json!({ "nodeToken": node_token });
        let _: serde_json::Value = self.post_json("/api/intel/nodes/heartbeat", &body).await?;
        Ok(())
    }

    /// Opt-out of the mesh network (GDPR right to erasure).
    pub async fn unregister_mesh_node(&self, node_token: &str) -> Result<()> {
        let url: Uri = format!("{}/api/intel/nodes/{}", self.base_url, node_token)
            .parse().context("Invalid URL")?;
        let mut builder = Request::builder().method("DELETE").uri(url);
        if let Some(ref tok) = self.bearer {
            builder = builder.header(AUTHORIZATION, format!("Bearer {}", tok));
        }
        let req = builder.body(Body::empty())?;
        let client = Client::new();
        let resp = client.request(req).await
            .context("Cannot reach ilocker Cloud API")?;
        if !resp.status().is_success() {
            anyhow::bail!("Opt-out failed: HTTP {}", resp.status());
        }
        Ok(())
    }

    // ── HTTP helpers ──────────────────────────────────────────

    async fn get_json<R: for<'de> serde::Deserialize<'de>>(
        &self,
        path: &str,
    ) -> Result<R> {
        let url: Uri = format!("{}{}", self.base_url, path)
            .parse().context("Invalid API URL")?;
        let mut builder = Request::builder().method(Method::GET).uri(url);
        if let Some(ref tok) = self.bearer {
            builder = builder.header(AUTHORIZATION, format!("Bearer {}", tok));
        }
        let req = builder.body(Body::empty())?;
        self.execute(req).await
    }

    async fn post_json<B: Serialize, R: for<'de> serde::Deserialize<'de>>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<R> {
        self.post_json_impl(path, body, false).await
    }

    async fn post_json_authed<B: Serialize, R: for<'de> serde::Deserialize<'de>>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<R> {
        self.post_json_impl(path, body, true).await
    }

    async fn post_json_impl<B: Serialize, R: for<'de> serde::Deserialize<'de>>(
        &self,
        path:        &str,
        body:        &B,
        with_bearer: bool,
    ) -> Result<R> {
        let url: Uri = format!("{}{}", self.base_url, path)
            .parse().context("Invalid API URL")?;
        let json = serde_json::to_vec(body)?;
        let mut builder = Request::builder()
            .method(Method::POST)
            .uri(url)
            .header(CONTENT_TYPE, "application/json");
        if with_bearer {
            if let Some(ref tok) = self.bearer {
                builder = builder.header(AUTHORIZATION, format!("Bearer {}", tok));
            }
        }
        let req = builder.body(Body::from(json))?;
        self.execute(req).await
    }

    async fn execute<R: for<'de> serde::Deserialize<'de>>(
        &self,
        req: Request<Body>,
    ) -> Result<R> {
        let client = Client::new();
        let resp   = client.request(req).await
            .context("Cannot reach ilocker Cloud API")?;
        let status = resp.status();
        let bytes  = hyper::body::to_bytes(resp.into_body()).await
            .context("Failed to read API response")?;
        if status.is_success() {
            serde_json::from_slice(&bytes).context("Unexpected API response format")
        } else {
            let msg = serde_json::from_slice::<ApiError>(&bytes)
                .map(|e| e.error)
                .unwrap_or_else(|_| format!("HTTP {}", status));
            anyhow::bail!("{}", msg)
        }
    }
}

// ── Data types for reporting ──────────────────────────────────

#[derive(Debug, Serialize)]
pub struct ChunkReport {
    pub sha256:     String,
    #[serde(rename = "sizeBytes")]
    pub size_bytes: u64,
    pub category:   String,
}
