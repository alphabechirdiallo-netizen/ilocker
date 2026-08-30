// ============================================================
//  commands/cloud_share.rs — Phase 5B / v1.7.0 Security Fix
//  Two-channel Signal/Wormhole model for cloud share links.
//  Azure SAS tokens now fully supported.
// ============================================================

use crate::chunker::{self, chunk_dir, FileManifest, SnapshotManifest};
use crate::cloud_crypto::CloudCrypto;
use crate::cloud_share_token::{self, CloudSharePayload};
use crate::cloud_store;
use crate::db;
use crate::presigned::{self, PresignedUrlGenerator, AzureSasGenerator};
use crate::cloud_backend::CloudBackend;
use crate::utils::{db_path, human_bytes};
use anyhow::{bail, Context, Result};
use chrono::Utc;
use colored::Colorize;
use indicatif::{ProgressBar, ProgressStyle};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::Semaphore;

pub const DEFAULT_TTL_HOURS: u64 = 2;
pub const MAX_TTL_HOURS:     u64 = 168;

// ── iloc share --cloud ────────────────────────────────────────

pub async fn run_share_cloud(ttl_hours: u64, files: Vec<String>, profile: Option<String>) -> Result<()> {
    let creds = cloud_store::require_credentials(profile.as_deref())?;

    let cwd         = std::env::current_dir()?;
    let ilocker_dir = cwd.join(".ilocker");
    if !ilocker_dir.exists() { bail!("Not an ilocker project. Run `iloc init` first."); }

    let project_key = load_project_key(&ilocker_dir)?;
    let db_file     = db_path(&ilocker_dir);
    let conn        = db::open(&db_file)?;
    let snap        = db::latest_snapshot(&conn)?
        .ok_or_else(|| anyhow::anyhow!("No snapshots. Run `iloc save` then `iloc push`."))?;

    let selective = !files.is_empty();
    println!();
    if selective {
        println!(
            "{} Generating cloud share link for {} selected file(s) from \"{}\"",
            "◎".cyan().bold(), files.len().to_string().yellow(), snap.message.bold()
        );
    } else {
        println!(
            "{} Generating cloud share link for {} — \"{}\"",
            "◎".cyan().bold(), snap.id.cyan(), snap.message.bold()
        );
    }

    let ttl_secs = ttl_hours.min(MAX_TTL_HOURS) * 3600;
    let expires_at = SystemTime::now()
        .duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() + ttl_secs;

    // Verify snapshot pushed to cloud
    let backend = CloudBackend::from_creds(&creds)?;
    if !backend.exists(&crate::commands::cloud::manifest_key(&snap.id)).await.unwrap_or(false) {
        bail!(
            "Snapshot not pushed to cloud yet.\nRun `iloc push` first, then retry."
        );
    }

    // Chunk all files (ou uniquement la sélection demandée)
    let all_records = db::files_for_snapshot(&conn, &snap.id)?;
    let (records, not_found) = db::select_records(&all_records, &files);
    for f in &not_found {
        println!("  {} '{}' introuvable dans ce snapshot — ignoré", "⚠".yellow(), f);
    }
    if selective && records.is_empty() {
        bail!("Aucun des fichiers demandés n'existe dans le dernier snapshot.");
    }
    let chunk_store = chunk_dir(&ilocker_dir);
    std::fs::create_dir_all(&chunk_store)?;

    let pb = progress_bar(records.len() as u64, "preparing");
    let mut all_manifests: Vec<FileManifest> = Vec::new();
    for rec in &records {
        let snap_src = crate::vault::snapshots_dir(&ilocker_dir).join(&snap.id).join(&rec.rel_path);
        let src = if snap_src.exists() { snap_src } else { cwd.join(&rec.rel_path) };
        if !src.exists() { pb.inc(1); continue; }
        all_manifests.push(chunker::chunk_file(&src, &rec.rel_path, &chunk_store)?);
        pb.inc(1);
    }
    pb.finish_and_clear();

    let all_sha256s: Vec<String> = {
        let mut seen = std::collections::HashSet::new();
        all_manifests.iter()
            .flat_map(|m| m.chunks.iter().map(|c| c.sha256.clone()))
            .filter(|s| seen.insert(s.clone()))
            .collect()
    };
    let total_bytes: u64 = all_manifests.iter().map(|m| m.total_size).sum();

    // ── Pre-sign URLs selon le provider ──────────────────────
    let is_azure = creds.provider == crate::cloud_store::CloudProvider::Azure;

    let pb = progress_bar(all_sha256s.len() as u64, "signing URLs");

    let (chunk_urls, manifest_url) = if is_azure {
        // ── Azure : SAS tokens ───────────────────────────────
        let sas = AzureSasGenerator::from_creds(&creds)
            .context("Impossible d'initialiser le générateur SAS Azure — vérifiez votre clé de compte")?;

        let chunk_urls = presigned::presign_chunks_azure(&sas, &all_sha256s, ttl_secs)?;
        pb.finish_and_clear();

        let manifest_url = if selective {
            let crypto   = CloudCrypto::from_project_key(&project_key);
            let share_id = format!("{}-share-{}", snap.id, uuid::Uuid::new_v4());
            let scoped_manifest = SnapshotManifest {
                snapshot_id: snap.id.clone(),
                project_key: project_key.clone(),
                files:       all_manifests.clone(),
                created_at:  Utc::now().to_rfc3339(),
                expires_at:  Some(expires_at),
            };
            let json = serde_json::to_vec(&scoped_manifest)?;
            let enc  = crypto.encrypt(&json, &share_id)?;
            backend.put_raw(&crate::commands::cloud::manifest_key(&share_id), &enc).await
                .context("Échec de l'upload du manifest de partage scellé (Azure)")?;
            presigned::presign_manifest_azure(&sas, &share_id, ttl_secs)?
        } else {
            presigned::presign_manifest_azure(&sas, &snap.id, ttl_secs)?
        };

        (chunk_urls, manifest_url)
    } else {
        // ── S3-compatible : SigV4 pré-signées ────────────────
        let gen = PresignedUrlGenerator::from_creds(&creds);

        let chunk_urls = presigned::presign_chunks(&gen, &all_sha256s, ttl_secs)?;
        pb.finish_and_clear();

        let manifest_url = if selective {
            let crypto   = CloudCrypto::from_project_key(&project_key);
            let share_id = format!("{}-share-{}", snap.id, uuid::Uuid::new_v4());
            let scoped_manifest = SnapshotManifest {
                snapshot_id: snap.id.clone(),
                project_key: project_key.clone(),
                files:       all_manifests.clone(),
                created_at:  Utc::now().to_rfc3339(),
                expires_at:  Some(expires_at),
            };
            let json = serde_json::to_vec(&scoped_manifest)?;
            let enc  = crypto.encrypt(&json, &share_id)?;
            backend.put_raw(&crate::commands::cloud::manifest_key(&share_id), &enc).await
                .context("Échec de l'upload du manifest de partage scellé")?;
            presigned::presign_manifest(&gen, &share_id, ttl_secs)?
        } else {
            presigned::presign_manifest(&gen, &snap.id, ttl_secs)?
        };

        (chunk_urls, manifest_url)
    };

    // Build payload
    let payload = CloudSharePayload {
        v:            2,
        project_key:  project_key.clone(),
        snapshot_id:  snap.id.clone(),
        manifest_url,
        chunks:       chunk_urls,
        expires_at,
        provider:     creds.provider.label().to_string(),
        file_count:   all_manifests.len(),
        total_bytes,
    };

    // Encrypt with project_key (Signal model)
    let magic_link = cloud_share_token::encode(&payload)?;

    // ── Display ───────────────────────────────────────────────
    println!();
    println!("{} Cloud share ready", "✓".green().bold());
    println!("  {} {} files · {}", "content:".dimmed(), all_manifests.len(), human_bytes(total_bytes));
    println!("  {} {} hours", "expires:".dimmed(), ttl_hours);
    if is_azure {
        println!("  {} Azure Blob Storage (SAS token)", "provider:".dimmed());
    }
    println!();

    // ── TWO-CHANNEL DISPLAY ───────────────────────────────────
    println!("{}", "══════════════════════════════════════════════════════".dimmed());
    println!("{}", "  CHANNEL 1 — Share this link (e.g. email, Slack):".bold());
    println!("{}", "══════════════════════════════════════════════════════".dimmed());
    println!();
    println!("  {}", magic_link.green());
    println!();
    println!("{}", "══════════════════════════════════════════════════════".dimmed());
    println!("{}", "  CHANNEL 2 — Share this key via a SEPARATE secure channel".bold());
    println!("{}", "  (Signal, encrypted email — NEVER the same channel as the link):".yellow());
    println!("{}", "══════════════════════════════════════════════════════".dimmed());
    println!();
    println!("  {}", project_key.yellow().bold());
    println!();
    println!("{}", "  ⚠  SECURITY: Either value alone is useless.".yellow().bold());
    println!("{}", "     An attacker needs BOTH to access your data.".yellow());
    println!();
    println!("{}", "  Peer runs:".bold());
    println!(
        "    {} {} {}",
        "iloc clone".cyan().bold(),
        "<link>".green(),
        format!("--key-secret {}", project_key).yellow()
    );
    println!();

    Ok(())
}

// ── iloc clone iloc://cloud-share-... --key-secret iloc://... ────

pub async fn run_clone_cloud(token_str: &str, project_key: &str, dest_dir: Option<PathBuf>) -> Result<()> {
    // Decode using the separately-provided project_key
    let payload = cloud_share_token::decode(token_str, project_key)
        .map_err(|e| anyhow::anyhow!("Cannot decrypt share link: {}\n\nMake sure you are using the correct --key-secret.", e))?;

    cloud_share_token::describe(&payload);

    // Prepare destination
    let project_id = payload.project_key.replace("iloc://", "");
    let dest = dest_dir.unwrap_or_else(|| {
        std::env::current_dir().unwrap()
            .join(format!("iloc-cloud-{}", &project_id[..8]))
    });
    if !dest.exists() { std::fs::create_dir_all(&dest)?; }

    let ilocker_dir = dest.join(".ilocker");
    std::fs::create_dir_all(&ilocker_dir)?;

    let vault_cfg = crate::vault::VaultConfig {
        mode: crate::vault::VaultMode::Sibling,
        vault_path: crate::vault::resolve_default_path(
            crate::vault::VaultMode::Sibling, &dest, &project_id, None,
        )?,
        project_id: project_id.clone(),
        mirrors: Vec::new(),
        cloud_backup_enabled: false,
        cloud_backup_profile: None,
        hyperscale_backup_enabled: false,
        auto_patch_gitignore: true,
    };
    crate::vault::provision(&ilocker_dir, &vault_cfg)?;
    let _ = crate::vault::patch_gitignore(&dest);

    let db_file = db_path(&ilocker_dir);
    db::init(&db_file)?;
    let chunk_store = chunk_dir(&ilocker_dir);
    let crypto      = CloudCrypto::from_project_key(&payload.project_key);

    // Download manifest
    println!("{}", "  Downloading snapshot manifest…".dimmed());
    let manifest_enc  = download_url(&payload.manifest_url).await
        .context("Failed to download manifest — link may have expired")?;
    let manifest_json = crypto.decrypt(&manifest_enc)
        .context("Failed to decrypt manifest")?;
    let manifest: SnapshotManifest = serde_json::from_slice(&manifest_json)?;

    let total_chunks: usize = manifest.files.iter().map(|f| f.chunks.len()).sum();
    println!(
        "  {} manifest: {} files · {} chunks",
        "✓".green(), manifest.files.len().to_string().yellow(), total_chunks.to_string().yellow()
    );

    // Deduplication check
    let mut missing: Vec<String> = Vec::new();
    for fm in &manifest.files {
        for ci in chunker::missing_chunks(fm, &chunk_store) {
            if payload.chunks.contains_key(&ci.sha256) {
                missing.push(ci.sha256.clone());
            }
        }
    }
    missing.sort(); missing.dedup();

    println!(
        "  {} {} local · {} to download",
        "⚡".yellow(),
        (total_chunks - missing.len()).to_string().green(),
        missing.len().to_string().yellow()
    );

    // Parallel download (16 workers)
    if !missing.is_empty() {
        let pb  = progress_bar(missing.len() as u64, "downloading");
        let sem = Arc::new(Semaphore::new(16));
        let mut handles = Vec::new();

        for sha256 in &missing {
            let url = match payload.chunks.get(sha256) {
                Some(u) => u.clone(),
                None    => { pb.inc(1); continue; }
            };
            let sha256       = sha256.clone();
            let chunk_store2 = chunk_store.clone();
            let crypto2      = CloudCrypto::from_project_key(&payload.project_key);
            let permit       = sem.clone().acquire_owned().await?;
            let pb2          = pb.clone();

            handles.push(tokio::spawn(async move {
                let _permit = permit;
                let result  = download_and_store(&url, &sha256, &chunk_store2, &crypto2).await;
                pb2.inc(1);
                result
            }));
        }

        let mut failed = 0usize;
        for h in handles {
            match h.await { Ok(Ok(_)) => {} Ok(Err(_)) | Err(_) => { failed += 1; } }
        }
        pb.finish_and_clear();
        if failed > 0 { println!("  {} {} chunk(s) failed", "⚠".yellow(), failed); }
        else           { println!("  {} All chunks downloaded", "✓".green()); }
    }

    // Reassemble
    let pb = progress_bar(manifest.files.len() as u64, "reassembling");
    for fm in &manifest.files {
        chunker::reassemble_file(fm, &chunk_store, &dest.join(&fm.rel_path))?;
        pb.inc(1);
    }
    pb.finish_and_clear();

    // Rebuild index
    rebuild_index(&db_file, &manifest)?;

    // Write config.json
    std::fs::write(
        ilocker_dir.join("config.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "version": "1.7.0",
            "project_id": project_id,
            "key": payload.project_key,
            "cloned_from": "cloud-share",
            "cloned_at": Utc::now().to_rfc3339(),
        }))?,
    )?;

    println!();
    println!("{} Clone complete → {}", "✓".green().bold(), dest.display().to_string().cyan().bold());
    println!("  {} {} files", "●".green(), manifest.files.len());

    rehydrate(&dest);
    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────

async fn download_url(url: &str) -> Result<Vec<u8>> {
    use hyper_rustls::HttpsConnectorBuilder;
    let uri:      hyper::Uri = url.parse()?;
    // with_native_roots() — pas with_webpki_roots() (même correctif que
    // s3_client.rs/azure_client.rs/github_client.rs/vercel_client.rs/
    // supabase_client.rs/updater.rs/provider_engine.rs).
    let connector = HttpsConnectorBuilder::new()
        .with_native_roots().https_or_http().enable_http1().build();
    let client: hyper::Client<_, hyper::Body> = hyper::Client::builder().build(connector);
    let resp = tokio::time::timeout(Duration::from_secs(30), client.get(uri)).await
        .map_err(|_| anyhow::anyhow!("Download timeout"))??;
    if !resp.status().is_success() { bail!("HTTP {} from cloud", resp.status()); }
    Ok(hyper::body::to_bytes(resp.into_body()).await?.to_vec())
}

async fn download_and_store(url: &str, sha256: &str, chunk_store: &Path, crypto: &CloudCrypto) -> Result<()> {
    use sha2::{Sha256 as SHA, Digest};
    let enc   = download_url(url).await?;
    let plain = crypto.decrypt(&enc)?;
    let actual = hex::encode(SHA::digest(&plain));
    if actual != sha256 { bail!("Integrity check failed for {}", &sha256[..12]); }
    let dir = chunk_store.join(&sha256[..2]);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(sha256);
    if !path.exists() { std::fs::write(path, &plain)?; }
    Ok(())
}

fn rebuild_index(db_file: &Path, manifest: &SnapshotManifest) -> Result<()> {
    let mut conn   = db::open(db_file)?;
    let created_at = Utc::now().to_rfc3339();
    db::insert_snapshot(&conn, &manifest.snapshot_id, "cloud-share clone", None, &created_at)?;
    let records: Vec<db::FileRecord> = manifest.files.iter().map(|fm| db::FileRecord {
        snapshot_id: manifest.snapshot_id.clone(),
        rel_path:    fm.rel_path.clone(),
        sha256:      fm.file_sha256.clone(),
        size_bytes:  fm.total_size as i64,
        modified_at: created_at.clone(),
        inode:       None,
    }).collect();
    db::insert_file_records(&mut conn, &records)?;
    db::update_snapshot_stats(&conn, &manifest.snapshot_id,
        records.len() as i64,
        manifest.files.iter().map(|f| f.total_size as i64).sum())?;
    Ok(())
}

fn load_project_key(ilocker_dir: &Path) -> Result<String> {
    let raw = std::fs::read_to_string(ilocker_dir.join("config.json"))?;
    let v: serde_json::Value = serde_json::from_str(&raw)?;
    v["key"].as_str().map(|s| s.to_string())
        .ok_or_else(|| anyhow::anyhow!("config.json missing 'key'"))
}

fn progress_bar(total: u64, msg: &'static str) -> ProgressBar {
    let pb = ProgressBar::new(total);
    pb.set_style(ProgressStyle::with_template(
        "  {spinner:.cyan} [{bar:40.cyan/blue}] {pos}/{len}  {msg}"
    ).unwrap().progress_chars("█▉▊▋▌▍▎▏  "));
    pb.set_message(msg); pb
}

fn rehydrate(root: &Path) {
    use crate::utils::ProjectType;
    for kind in ProjectType::detect(root) {
        match kind {
            ProjectType::NodeJs => run_cmd(root, "npm",    &["install"],          "Node.js"),
            ProjectType::Python => run_cmd(root, "python3",&["-m","venv",".venv"],"Python"),
            ProjectType::Rust   => run_cmd(root, "cargo",  &["build"],            "Rust"),
            ProjectType::Go     => run_cmd(root, "go",     &["mod","download"],   "Go"),
            _                   => {}
        }
    }
}

fn run_cmd(root: &Path, cmd: &str, args: &[&str], label: &str) {
    println!("  {} {}…", "⚙".cyan(), label.bold());
    match std::process::Command::new(cmd).args(args).current_dir(root).status() {
        Ok(s) if s.success() => println!("  {} {}", "✓".green(), label),
        Ok(s) => println!("  {} {} (exit {})", "⚠".yellow(), label, s),
        Err(e) => println!("  {} {}: {}", "⚠".yellow(), cmd, e),
    }
}
