// ============================================================
//  commands/clone.rs — iloc clone <key> [--host <ip>|--relay <host>]
//  v1.4.0: relay integration + resumable transfers
// ============================================================

use crate::chunker::{self, chunk_dir, SnapshotManifest};
use crate::crypto::CryptoCtx;
use crate::db;
use crate::protocol::{recv_msg, send_msg, Message, PROTOCOL_VERSION};
use crate::relay_client;
use crate::transfer_state::{self, Direction};
use crate::utils::{db_path, human_bytes, ProjectType};
use anyhow::{bail, Result};
use colored::Colorize;
use indicatif::{ProgressBar, ProgressStyle};
use std::path::{Path, PathBuf};
use tokio::net::TcpStream;

pub async fn run(
    project_key: &str,
    host:        Option<String>,   // None = use relay
    relay_host:  Option<String>,
    port:        u16,
    dest_dir:    Option<PathBuf>,
) -> Result<()> {
    let project_id = crate::crypto::key_to_project_id(project_key)?;
    let ctx        = CryptoCtx::from_key_str(project_key)?;

    // ── Prepare destination ───────────────────────────────────
    let dest = dest_dir.unwrap_or_else(|| {
        std::env::current_dir().unwrap().join(format!("iloc-{}", &project_id[..8]))
    });

    let is_update = dest.exists()
        && dest.read_dir().map(|mut d| d.next().is_some()).unwrap_or(false);

    if is_update {
        println!(
            "  {} Destination exists — resumable update mode",
            "ℹ".cyan()
        );
    } else {
        std::fs::create_dir_all(&dest)?;
    }

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

    // ── Establish connection ──────────────────────────────────
    println!();
    let mut stream: TcpStream = if let Some(ref h) = host {
        // Direct connection
        let addr = format!("{}:{}", h, port);
        println!("{} Connecting to {}…", "◎".cyan().bold(), addr.bold());
        TcpStream::connect(&addr).await
            .map_err(|e| anyhow::anyhow!("Cannot connect to {}: {}", addr, e))?
    } else if let Some(ref relay) = relay_host {
        // Relay-assisted connection
        let session_key = &project_key.replace("iloc://", "")[..16];
        // Use a local port for hole-punch attempts
        let local_port = port.wrapping_add(1);
        relay_client::connect_as_cloner(session_key, local_port, relay).await?
    } else {
        bail!(
            "Provide either --host <ip> (direct) or --relay <host> (NAT traversal).\n\
             Example: iloc clone {} --relay relay.ilocker.dev",
            project_key
        );
    };

    println!("  {} connected", "✓".green());

    // ── Handshake ─────────────────────────────────────────────
    send_msg(&mut stream, &Message::Hello {
        project_key: project_key.to_string(),
        version:     PROTOCOL_VERSION,
    }).await?;

    // ── Receive manifest ──────────────────────────────────────
    let manifest: SnapshotManifest = match recv_msg(&mut stream).await? {
        Message::Manifest(m) => m,
        Message::Error(e)    => bail!("Sharer rejected: {}", e),
        other                => bail!("Unexpected: {:?}", other),
    };

    let total_files  = manifest.files.len();
    let total_chunks: usize = manifest.files.iter().map(|f| f.chunks.len()).sum();

    println!(
        "  {} manifest: {} files · {} chunks",
        "→".dimmed(), total_files.to_string().yellow(), total_chunks.to_string().yellow()
    );

    // ── Load / create transfer manifest (resumption) ──────────
    let peer_addr_str = host.as_deref()
        .unwrap_or(relay_host.as_deref().unwrap_or("relay"));

    let (mut xfr, is_resume) = transfer_state::load_or_create(
        &ilocker_dir, Direction::Receive,
        project_key, &manifest.snapshot_id,
        total_chunks, peer_addr_str,
    )?;

    if is_resume {
        println!(
            "  {} Resuming — {}/{} chunks already received ({:.0}%)",
            "↺".yellow(),
            xfr.completed_chunks.len(),
            total_chunks,
            xfr.progress_pct()
        );
    }

    // ── Compute missing chunks ────────────────────────────────
    let mut missing: Vec<String> = Vec::new();
    for fm in &manifest.files {
        for ci in chunker::missing_chunks(fm, &chunk_store) {
            if !xfr.completed_chunks.contains(&ci.sha256) {
                missing.push(ci.sha256.clone());
            }
        }
    }
    missing.sort();
    missing.dedup();

    let already_local = total_chunks.saturating_sub(missing.len());
    println!(
        "  {} {} chunks local · {} to download",
        "⚡".yellow(),
        already_local.to_string().green(),
        missing.len().to_string().yellow()
    );

    // ── Request missing chunks ────────────────────────────────
    if missing.is_empty() {
        send_msg(&mut stream, &Message::Done).await?;
    } else {
        send_msg(&mut stream, &Message::NeedChunks(missing.clone())).await?;
    }

    // ── Receive + store chunks ────────────────────────────────
    if !missing.is_empty() {
        let pb = progress_bar(missing.len() as u64, "downloading");
        let mut bytes_received: u64 = 0;

        loop {
            match recv_msg(&mut stream).await? {
                Message::ChunkData { sha256, rel_path: _, encrypted_bytes } => {
                    bytes_received += encrypted_bytes.len() as u64;
                    let plain = ctx.decrypt(&encrypted_bytes)?;
                    store_chunk(&chunk_store, &sha256, &plain)?;
                    xfr.mark_done(&sha256, &ilocker_dir)?;
                    pb.inc(1);
                }
                Message::Done => {
                    pb.finish_and_clear();
                    println!("  {} {}", "✓".green(), human_bytes(bytes_received));
                    break;
                }
                Message::Error(e) => bail!("Sharer error: {}", e),
                other             => bail!("Unexpected: {:?}", other),
            }
        }
        send_msg(&mut stream, &Message::Ack).await?;
    }

    // ── Reassemble files ──────────────────────────────────────
    println!("{}", "  Reassembling files…".dimmed());
    let pb = progress_bar(manifest.files.len() as u64, "reassembling");
    for fm in &manifest.files {
        let dest_file = dest.join(&fm.rel_path);
        chunker::reassemble_file(fm, &chunk_store, &dest_file)?;
        pb.inc(1);
    }
    pb.finish_and_clear();

    // ── Rebuild SQLite index ──────────────────────────────────
    rebuild_index(&db_file, &manifest)?;

    // ── Write config.json ─────────────────────────────────────
    let config = serde_json::json!({
        "version":    "1.4.0",
        "project_id": &project_id,
        "key":        project_key,
        "cloned_from": peer_addr_str,
        "cloned_at":  chrono::Utc::now().to_rfc3339(),
    });
    std::fs::write(
        ilocker_dir.join("config.json"),
        serde_json::to_string_pretty(&config)?,
    )?;

    // ── Clean up transfer manifest on success ─────────────────
    if xfr.is_complete() {
        transfer_state::remove(&ilocker_dir)?;
    }

    println!();
    println!(
        "{} Clone complete → {}",
        "✓".green().bold(), dest.display().to_string().cyan().bold()
    );
    println!("  {} {} files reconstructed", "●".green(), manifest.files.len());

    // ── Auto-rehydrate ────────────────────────────────────────
    rehydrate(&dest);

    Ok(())
}

fn store_chunk(chunk_dir: &Path, sha256: &str, data: &[u8]) -> Result<()> {
    let prefix  = &sha256[..2];
    let dir     = chunk_dir.join(prefix);
    let outpath = dir.join(sha256);
    if outpath.exists() { return Ok(()); }
    std::fs::create_dir_all(&dir)?;
    std::fs::write(&outpath, data)?;
    Ok(())
}

fn rebuild_index(db_file: &Path, manifest: &SnapshotManifest) -> Result<()> {
    let mut conn = db::open(db_file)?;
    let created_at = chrono::Utc::now().to_rfc3339();
    db::insert_snapshot(&conn, &manifest.snapshot_id, "cloned snapshot", None, &created_at)?;
    let records: Vec<db::FileRecord> = manifest.files.iter().map(|fm| db::FileRecord {
        snapshot_id: manifest.snapshot_id.clone(),
        rel_path:    fm.rel_path.clone(),
        sha256:      fm.file_sha256.clone(),
        size_bytes:  fm.total_size as i64,
        modified_at: created_at.clone(),
        inode:       None,
    }).collect();
    db::insert_file_records(&mut conn, &records)?;
    db::update_snapshot_stats(
        &conn, &manifest.snapshot_id,
        records.len() as i64,
        manifest.files.iter().map(|f| f.total_size as i64).sum(),
    )?;
    Ok(())
}

fn rehydrate(project_root: &Path) {
    let types = ProjectType::detect(project_root);
    if types.iter().all(|t| *t == ProjectType::Unknown) { return; }

    println!();
    println!("{}", "── Auto-rehydration ─────────────────────────────────────".dimmed());

    for kind in &types {
        match kind {
            ProjectType::NodeJs  => run_install(project_root, "npm",    &["install"],               "Node.js"),
            ProjectType::Python  => run_install(project_root, "python3",&["-m","venv",".venv"],     "Python (venv)"),
            ProjectType::Rust    => run_install(project_root, "cargo",  &["build"],                 "Rust"),
            ProjectType::Go      => run_install(project_root, "go",     &["mod","download"],         "Go"),
            _                    => {}
        }
    }
}

fn run_install(root: &Path, cmd: &str, args: &[&str], label: &str) {
    println!("  {} {}…", "⚙".cyan(), label.bold());
    let status = std::process::Command::new(cmd).args(args).current_dir(root).status();
    match status {
        Ok(s) if s.success() => println!("  {} {} ready", "✓".green(), label),
        Ok(s) => println!("  {} {} exited with {}", "⚠".yellow(), label, s),
        Err(e) => println!("  {} {} not found: {}", "⚠".yellow(), cmd, e),
    }
}

fn progress_bar(total: u64, msg: &'static str) -> ProgressBar {
    let pb = ProgressBar::new(total);
    pb.set_style(ProgressStyle::with_template(
        "  {spinner:.cyan} [{bar:40.cyan/blue}] {pos}/{len}  {msg}"
    ).unwrap().progress_chars("█▉▊▋▌▍▎▏  "));
    pb.set_message(msg);
    pb
}
