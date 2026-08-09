// ============================================================
//  commands/share.rs — iloc share [--port <p>] [--relay <host>]
//  v1.4.0: relay integration + resumable transfers
// ============================================================

use crate::chunker::{self, chunk_dir, FileManifest, SnapshotManifest};
use crate::commands::init::assert_initialised;
use crate::crypto::CryptoCtx;
use crate::db;
use crate::protocol::{send_msg, recv_msg, Message};
use crate::relay_client;
use crate::transfer_state::{self, Direction};
use crate::utils::{db_path, human_bytes};
use anyhow::Result;
use chrono::Utc;
use colored::Colorize;
use indicatif::{ProgressBar, ProgressStyle};

use tokio::net::{TcpListener, TcpStream};

pub async fn run(port: u16, relay_host: Option<String>, files: Vec<String>) -> Result<()> {
    let ilocker_dir  = assert_initialised()?;
    let project_root = ilocker_dir.parent().unwrap().to_path_buf();
    let selective     = !files.is_empty();

    // ── 1. Load project config ────────────────────────────────
    let config      = load_config(&ilocker_dir)?;
    let project_key = config["key"].as_str()
        .ok_or_else(|| anyhow::anyhow!("Missing 'key' in config.json"))?.to_string();

    // ── 2. Build chunk manifest ───────────────────────────────
    let db_file = db_path(&ilocker_dir);
    let conn    = db::open(&db_file)?;
    let snap    = db::latest_snapshot(&conn)?
        .ok_or_else(|| anyhow::anyhow!("No snapshots. Run `iloc save` first."))?;

    let all_records = db::files_for_snapshot(&conn, &snap.id)?;
    let (records, not_found) = db::select_records(&all_records, &files);
    for f in &not_found {
        println!("  {} '{}' introuvable dans ce snapshot — ignoré", "⚠".yellow(), f);
    }
    if selective && records.is_empty() {
        anyhow::bail!("Aucun des fichiers demandés n'existe dans le dernier snapshot.");
    }
    let chunk_store = chunk_dir(&ilocker_dir);
    std::fs::create_dir_all(&chunk_store)?;

    println!();
    if selective {
        println!(
            "{} Building chunks for {} selected file(s) from \"{}\"",
            "◎".cyan().bold(), records.len().to_string().yellow(), snap.message.bold()
        );
    } else {
        println!(
            "{} Building chunks for snapshot — \"{}\"",
            "◎".cyan().bold(), snap.message.bold()
        );
    }

    let pb = progress_bar(records.len() as u64, "chunking");
    let mut file_manifests: Vec<FileManifest> = Vec::new();
    for rec in &records {
        let snap_src = crate::vault::snapshots_dir(&ilocker_dir).join(&snap.id).join(&rec.rel_path);
        let src = if snap_src.exists() { snap_src } else { project_root.join(&rec.rel_path) };
        if !src.exists() { pb.inc(1); continue; }
        file_manifests.push(chunker::chunk_file(&src, &rec.rel_path, &chunk_store)?);
        pb.inc(1);
    }
    pb.finish_and_clear();

    let total_chunks: usize = file_manifests.iter().map(|m| m.chunks.len()).sum();
    println!(
        "  {} {} files → {} chunks ready",
        "✓".green(), file_manifests.len(), total_chunks.to_string().yellow()
    );

    let snapshot_manifest = SnapshotManifest {
        snapshot_id: snap.id.clone(),
        project_key: project_key.clone(),
        files:       file_manifests,
        created_at:  Utc::now().to_rfc3339(),
        expires_at:  None,
    };

    // ── 3. Establish connection (direct or relay) ─────────────
    let mut stream: TcpStream;

    if let Some(ref relay) = relay_host {
        // ── Relay-assisted connection ─────────────────────────
        // session_key = first 16 chars of project_id (short but sufficient)
        let session_key = &project_key.replace("iloc://", "")[..16];
        println!();
        println!(
            "  {} Using relay: {}",
            "↔".cyan(), relay.bold()
        );
        println!(
            "  {}",
            "Share this key with your peer:".dimmed()
        );
        println!("    {}", project_key.cyan().bold());
        println!(
            "  {}",
            format!("They run: iloc clone {}  --relay {}", project_key, relay).green()
        );
        println!();

        stream = relay_client::connect_as_sharer(session_key, port, relay).await?;
    } else {
        // ── Direct listen ─────────────────────────────────────
        let addr = format!("0.0.0.0:{}", port);
        let listener = TcpListener::bind(&addr).await?;
        let display_ip = local_ip_hint();

        println!();
        println!("{} Listening on {}:{}", "▶".green().bold(), display_ip, port);
        println!("  {} {}", "key:".dimmed(), project_key.yellow().bold());
        println!();
        println!("  {}", "Peer command:".bold());
        println!(
            "    {}",
            format!("iloc clone {}  --host {}  --port {}", project_key, display_ip, port)
                .green().bold()
        );
        println!();
        println!("{}", "Waiting for connection…".dimmed());

        let (s, peer) = listener.accept().await?;
        println!("  {} connected from {}", "→".cyan(), peer);
        stream = s;
    }

    // ── 4. Handshake ──────────────────────────────────────────
    match recv_msg(&mut stream).await? {
        Message::Hello { project_key: recv_key, version: _ } => {
            if recv_key != project_key {
                send_msg(&mut stream, &Message::Error("Wrong project key".into())).await?;
                anyhow::bail!("Cloner sent wrong key");
            }
        }
        other => {
            send_msg(&mut stream, &Message::Error("Expected Hello".into())).await?;
            anyhow::bail!("Unexpected first message: {:?}", other);
        }
    }

    // ── 5. Send manifest ──────────────────────────────────────
    send_msg(&mut stream, &Message::Manifest(snapshot_manifest.clone())).await?;

    // ── 6. Receive needed chunks ──────────────────────────────
    let needed: Vec<String> = match recv_msg(&mut stream).await? {
        Message::NeedChunks(list) => list,
        Message::Done => {
            println!("{} Cloner already has all chunks.", "✓".green());
            return Ok(());
        }
        other => anyhow::bail!("Expected NeedChunks, got {:?}", other),
    };

    // ── 7. Load / create transfer manifest (resumption) ───────
    let peer_addr = "peer".to_string();
    let (mut xfr, is_resume) = transfer_state::load_or_create(
        &ilocker_dir, Direction::Send,
        &project_key, &snap.id,
        needed.len(), &peer_addr,
    )?;

    if is_resume {
        let done = xfr.completed_chunks.len();
        let pct  = xfr.progress_pct();
        println!(
            "  {} Resuming previous transfer ({} chunks done, {:.0}%)",
            "↺".yellow(), done, pct
        );
    }

    // ── 8. Stream chunks (skip already-sent ones) ─────────────
    let ctx = CryptoCtx::from_key_str(&project_key)?;

    // Build chunk → rel_path lookup
    let mut chunk_to_rel: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    for fm in &snapshot_manifest.files {
        for c in &fm.chunks {
            chunk_to_rel.entry(c.sha256.clone()).or_insert_with(|| fm.rel_path.clone());
        }
    }

    let to_send: Vec<&String> = needed.iter()
        .filter(|sha| !xfr.completed_chunks.contains(*sha))
        .collect();

    println!(
        "  {} {} / {} chunks to send",
        "→".dimmed(), to_send.len(), needed.len()
    );

    let pb = progress_bar(to_send.len() as u64, "streaming");
    let mut bytes_sent: u64 = 0;

    for sha256 in &to_send {
        let rel_path = chunk_to_rel.get(*sha256).map(|s| s.as_str()).unwrap_or("unknown");
        let raw      = chunker::load_chunk(&chunk_store, sha256)?;
        let enc      = ctx.encrypt(&raw, sha256, rel_path)?;
        bytes_sent  += enc.len() as u64;

        send_msg(&mut stream, &Message::ChunkData {
            sha256:          (*sha256).clone(),
            rel_path:        rel_path.to_string(),
            encrypted_bytes: enc,
        }).await?;

        xfr.mark_done(sha256, &ilocker_dir)?;
        pb.inc(1);
    }
    pb.finish_and_clear();

    send_msg(&mut stream, &Message::Done).await?;
    let _ = recv_msg(&mut stream).await;

    // ── 9. Clean up manifest on success ──────────────────────
    if xfr.is_complete() {
        transfer_state::remove(&ilocker_dir)?;
    }

    println!();
    println!("{} Transfer complete — {} sent", "✓".green().bold(), human_bytes(bytes_sent));
    if to_send.len() < needed.len() {
        println!(
            "  {} {} chunks skipped (already sent in previous session)",
            "⚡".yellow(), needed.len() - to_send.len()
        );
    }

    Ok(())
}

fn load_config(ilocker_dir: &std::path::Path) -> Result<serde_json::Value> {
    let raw = std::fs::read_to_string(ilocker_dir.join("config.json"))?;
    Ok(serde_json::from_str(&raw)?)
}

fn local_ip_hint() -> String {
    if let Ok(socket) = std::net::UdpSocket::bind("0.0.0.0:0") {
        if socket.connect("8.8.8.8:80").is_ok() {
            if let Ok(addr) = socket.local_addr() {
                return addr.ip().to_string();
            }
        }
    }
    "127.0.0.1".to_string()
}

fn progress_bar(total: u64, msg: &'static str) -> ProgressBar {
    let pb = ProgressBar::new(total);
    pb.set_style(ProgressStyle::with_template(
        "  {spinner:.green} [{bar:40.cyan/blue}] {pos}/{len}  {msg}"
    ).unwrap().progress_chars("█▉▊▋▌▍▎▏  "));
    pb.set_message(msg);
    pb
}
