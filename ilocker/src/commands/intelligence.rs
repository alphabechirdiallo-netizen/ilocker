#![allow(dead_code)]
// ============================================================
//  commands/intelligence.rs — Phase 5A CLI commands
//
//  Enhanced `iloc status --health`   → health score report
//  `iloc node join/leave/start/status` → mesh STUN node
//  Updated `iloc init` with predictive .ilockerignore
// ============================================================

use crate::auth_store;
use crate::health_score;
use crate::intel_client::IntelClient;
use crate::mesh_node;
use crate::utils::{db_path};
use anyhow::Result;
use colored::Colorize;
use std::path::Path;

// ── Health score (iloc status --health) ───────────────────────

pub fn run_health() -> Result<()> {
    let cwd         = std::env::current_dir()?;
    let ilocker_dir = cwd.join(".ilocker");

    if !ilocker_dir.exists() {
        anyhow::bail!("Not an ilocker project. Run `iloc init` first.");
    }

    let db_file = db_path(&ilocker_dir);
    let report  = health_score::compute(&db_file)?;
    health_score::print_health_report(&report);

    Ok(())
}

// ── Mesh node commands ────────────────────────────────────────

fn server_url() -> String {
    std::env::var("ILOC_SERVER")
        .unwrap_or_else(|_| "http://localhost:4000".to_string())
}

pub async fn run_node_join() -> Result<()> {
    let auth = auth_store::require_auth()?;

    // Derive project key from current project if available
    let cwd = std::env::current_dir()?;
    let project_key = load_project_key_opt(&cwd)
        .unwrap_or_else(|| auth.email.clone());

    mesh_node::run_join(
        &project_key,
        &auth.server_url,
        &auth.cli_token,
        None,
    ).await
}

pub async fn run_node_leave() -> Result<()> {
    let auth = auth_store::require_auth()?;
    mesh_node::run_leave(&auth.server_url, &auth.cli_token).await
}

pub async fn run_node_start() -> Result<()> {
    mesh_node::run_start().await
}

pub fn run_node_status() -> Result<()> {
    mesh_node::run_status()
}

// ── Predictive .ilockerignore (iloc init upgrade) ─────────────

/// Fetch community ignore patterns and merge with local defaults.
/// Called by `iloc init` after detecting the project type.
pub async fn fetch_predictive_ignore(
    project_type: &str,
    server_url:   &str,
) -> Vec<String> {
    let intel  = IntelClient::new(server_url, None);
    match intel.get_ignore_patterns(project_type).await {
        Ok(resp) => {
            let mut patterns: Vec<String> = resp.baseline.clone();

            // Add community patterns with enough votes (≥5)
            for p in resp.patterns.iter().filter(|p| p.vote_count >= 5) {
                if !patterns.contains(&p.pattern) {
                    patterns.push(p.pattern.clone());
                }
            }

            println!(
                "  {} Fetched {} community patterns for {} projects",
                "✓".green(),
                patterns.len(),
                project_type.bold()
            );

            patterns
        }
        Err(e) => {
            // Offline or server error — silently fall back to hardcoded defaults
            eprintln!("  {} Could not fetch community patterns: {}", "⚠".yellow(), e);
            vec![]
        }
    }
}

/// Generate the .ilockerignore file content from a pattern list.
pub fn render_ilockerignore(project_type: &str, patterns: &[String]) -> String {
    let mut lines = vec![
        format!(
            "# ilocker ignore — generated for {} projects",
            project_type
        ),
        format!("# Community-sourced patterns — edit freely"),
        format!("# Updated: {}", chrono::Utc::now().format("%Y-%m-%d")),
        String::new(),
        "# ── Auto-detected ────────────────────────────────────".to_string(),
    ];

    for pattern in patterns {
        lines.push(pattern.clone());
    }

    lines.push(String::new());
    lines.push("# ── ilocker itself ────────────────────────────────────".to_string());
    lines.push(".ilocker/".to_string());

    lines.join("\n")
}

// ── Global chunk deduplication (iloc push integration) ────────

/// Check which chunks in a set are universally known (can skip upload).
/// Returns the set of SHA-256 hashes that are globally available.
pub async fn check_global_dedup(
    chunk_hashes: &[String],
    server_url:   &str,
) -> std::collections::HashSet<String> {
    if chunk_hashes.is_empty() {
        return std::collections::HashSet::new();
    }

    let intel = IntelClient::new(server_url, None);
    match intel.check_universal_chunks(chunk_hashes).await {
        Ok(resp) => {
            if !resp.universal.is_empty() {
                println!(
                    "  {} {} chunks globally deduplicated — {} saved",
                    "⚡".yellow(),
                    resp.universal.len(),
                    crate::utils::human_bytes(resp.bytes_saved)
                );
            }
            resp.universal.into_iter().collect()
        }
        Err(_) => std::collections::HashSet::new(),
    }
}

/// Report chunks to the global registry after a successful push.
pub async fn report_chunks_to_registry(
    chunk_data:   &[(String, u64, String)], // (sha256, size, category)
    server_url:   &str,
    cli_token:    &str,
) {
    let intel = IntelClient::new(server_url, Some(cli_token));
    let reports: Vec<crate::intel_client::ChunkReport> = chunk_data.iter()
        .map(|(sha, size, cat)| crate::intel_client::ChunkReport {
            sha256:     sha.clone(),
            size_bytes: *size,
            category:   cat.clone(),
        })
        .collect();

    let _ = intel.report_chunks(&reports).await;
}

// ── Mesh node discovery for P2P (share/clone enhancement) ────

/// Discover nearby mesh nodes to attempt STUN before using central relay.
/// Returns a list of (public_endpoint, node_token) pairs sorted by proximity.
pub async fn discover_nearby_nodes(
    server_url:    &str,
    country_hint:  Option<&str>,
) -> Vec<crate::intel_client::MeshNode> {
    let intel = IntelClient::new(server_url, None);
    match intel.get_mesh_nodes(country_hint).await {
        Ok(resp) => {
            if !resp.nodes.is_empty() {
                println!(
                    "  {} {} mesh node(s) available nearby",
                    "◎".cyan(), resp.nodes.len()
                );
            }
            resp.nodes
        }
        Err(_) => vec![],
    }
}

// ── Helpers ───────────────────────────────────────────────────

fn load_project_key_opt(dir: &Path) -> Option<String> {
    let config_path = dir.join(".ilocker").join("config.json");
    if !config_path.exists() { return None; }
    let raw = std::fs::read_to_string(config_path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    v["key"].as_str().map(|s| s.to_string())
}
