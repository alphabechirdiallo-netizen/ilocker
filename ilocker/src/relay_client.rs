// ============================================================
//  relay_client.rs — NAT traversal & relay client  (Phase 4)
// ============================================================

use anyhow::{bail, Context, Result};
use colored::Colorize;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::time::{sleep, timeout};

#[allow(dead_code)]
pub const DEFAULT_RELAY: &str = "relay.ilocker.dev";
pub const SIGNAL_PORT:   u16  = 7480;
pub const TUNNEL_PORT:   u16  = 7481;

const HOLE_PUNCH_TIMEOUT: Duration = Duration::from_secs(6);
const SIGNAL_TIMEOUT:     Duration = Duration::from_secs(60);

#[derive(Debug)]
pub struct RelaySession {
    pub peer_public_addr: String,
    pub peer_listen_port: u16,
    pub tunnel_token:     Option<String>,
    pub relay_addr:       Option<String>,
}

// ── Public entry points ───────────────────────────────────────

pub async fn connect_as_sharer(
    session_key:  &str,
    listen_port:  u16,
    relay_host:   &str,
) -> Result<TcpStream> {
    println!("  {} Contacting relay {}…", "↔".cyan(), relay_host);

    let session = exchange_signal(session_key, "sharer", listen_port, relay_host).await?;
    attempt_direct_or_relay(session, listen_port, relay_host).await
}

pub async fn connect_as_cloner(
    session_key:  &str,
    listen_port:  u16,
    relay_host:   &str,
) -> Result<TcpStream> {
    println!("  {} Contacting relay {}…", "↔".cyan(), relay_host);

    let session = exchange_signal(session_key, "cloner", listen_port, relay_host).await?;
    attempt_direct_connect(session, relay_host).await
}

// ── Signal exchange ───────────────────────────────────────────

async fn exchange_signal(
    session_key: &str,
    role:        &str,
    listen_port: u16,
    relay_host:  &str,
) -> Result<RelaySession> {
    let addr = format!("{}:{}", relay_host, SIGNAL_PORT);

    let mut stream = timeout(Duration::from_secs(10), TcpStream::connect(&addr))
        .await
        .map_err(|_| anyhow::anyhow!("Relay {} unreachable (timeout)", addr))?
        .with_context(|| format!("Cannot connect to relay at {}", addr))?;

    // Send Register
    let register_msg = format!(
        "{}\n",
        serde_json::json!({
            "type":        "register",
            "session_key": session_key,
            "role":        role,
            "client_port": listen_port,
        })
    );
    stream.write_all(register_msg.as_bytes()).await?;

    let (reader, _writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();

    // Read Registered
    let line1 = read_line_timeout(&mut lines, SIGNAL_TIMEOUT, "Registered").await?;
    let registered: serde_json::Value = serde_json::from_str(&line1)
        .context("Invalid JSON from relay")?;

    if registered["type"] == "error" {
        bail!("Relay error: {}", registered["message"]);
    }

    let your_addr = registered["your_public_addr"]
        .as_str().unwrap_or("unknown").to_string();
    println!(
        "  {} Registered — your public address: {}",
        "✓".green(), your_addr.cyan()
    );
    println!("  {} Waiting for peer…", "…".dimmed());

    // Read PeerInfo
    let line2 = read_line_timeout(&mut lines, SIGNAL_TIMEOUT, "PeerInfo").await?;
    let peer_info: serde_json::Value = serde_json::from_str(&line2)
        .context("Invalid PeerInfo JSON")?;

    match peer_info["type"].as_str() {
        Some("peer_info") => {
            let peer_addr = peer_info["peer_public_addr"]
                .as_str().unwrap_or("").to_string();
            let peer_port = peer_info["peer_listen_port"]
                .as_u64().unwrap_or(7477) as u16;

            println!(
                "  {} Peer found at {}:{}",
                "✓".green(), peer_addr.cyan(), peer_port
            );

            Ok(RelaySession {
                peer_public_addr: peer_addr,
                peer_listen_port: peer_port,
                tunnel_token:     None,
                relay_addr:       None,
            })
        }
        Some("error") => bail!("Relay error: {}", peer_info["message"]),
        other          => bail!("Unexpected relay message type: {:?}", other),
    }
}

// ── Connection strategies ─────────────────────────────────────

async fn attempt_direct_or_relay(
    session:     RelaySession,
    listen_port: u16,
    relay_host:  &str,
) -> Result<TcpStream> {
    println!("  {} Attempting direct connection (hole-punch)…", "→".dimmed());

    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", listen_port))
        .await
        .with_context(|| format!("Cannot bind port {}", listen_port))?;

    match timeout(HOLE_PUNCH_TIMEOUT, listener.accept()).await {
        Ok(Ok((stream, peer))) => {
            println!("  {} Direct connection from {}", "✓".green().bold(), peer);
            Ok(stream)
        }
        _ => {
            println!("  {} Direct failed — using relay tunnel", "⚠".yellow());
            connect_via_relay(&session, relay_host).await
        }
    }
}

async fn attempt_direct_connect(
    session:     RelaySession,
    relay_host:  &str,
) -> Result<TcpStream> {
    let target = format!("{}:{}", session.peer_public_addr, session.peer_listen_port);
    println!("  {} Attempting direct connection to {}…", "→".dimmed(), target.cyan());

    for attempt in 1..=3u8 {
        match timeout(Duration::from_secs(2), TcpStream::connect(&target)).await {
            Ok(Ok(stream)) => {
                println!("  {} Direct connection established (attempt {})", "✓".green().bold(), attempt);
                return Ok(stream);
            }
            _ if attempt < 3 => sleep(Duration::from_millis(500)).await,
            _                => {}
        }
    }

    println!("  {} Direct failed — using relay tunnel", "⚠".yellow());
    connect_via_relay(&session, relay_host).await
}

async fn connect_via_relay(session: &RelaySession, relay_host: &str) -> Result<TcpStream> {
    let token = session.tunnel_token.clone()
        .unwrap_or_else(|| format!("{:0<32}", "relay"));

    let relay_addr = session.relay_addr.clone()
        .unwrap_or_else(|| format!("{}:{}", relay_host, TUNNEL_PORT));

    let mut stream = timeout(Duration::from_secs(10), TcpStream::connect(&relay_addr))
        .await
        .map_err(|_| anyhow::anyhow!("Relay tunnel {} unreachable", relay_addr))?
        .with_context(|| format!("Cannot connect to relay tunnel at {}", relay_addr))?;

    // Send 32-char token + newline as handshake
    let handshake = format!("{:0<32}\n", &token[..token.len().min(32)]);
    stream.write_all(handshake.as_bytes()).await?;

    println!("  {} Relay tunnel active ({})", "✓".green().bold(), relay_addr.dimmed());
    Ok(stream)
}

// ── Helpers ───────────────────────────────────────────────────

/// Read one non-empty line from an async lines stream with a timeout.
async fn read_line_timeout<S>(
    lines:   &mut tokio::io::Lines<S>,
    dur:     Duration,
    context: &'static str,
) -> Result<String>
where
    S: AsyncBufReadExt + Unpin,
{
    let result = timeout(dur, lines.next_line())
        .await
        .map_err(|_| anyhow::anyhow!("Timeout waiting for {} from relay", context))?;

    match result {
        Ok(Some(line)) => Ok(line),
        Ok(None)       => bail!("Relay closed connection before sending {}", context),
        Err(e)         => bail!("Read error waiting for {}: {}", context, e),
    }
}
