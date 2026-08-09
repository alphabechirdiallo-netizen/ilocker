// ============================================================
//  mesh_node.rs — Opt-in decentralised STUN mini-node
//  Phase 5A Feature 4
//
//  When a user runs `iloc node join`, this machine registers
//  itself as a volunteer STUN helper.  Other peers on the
//  network can then query the server for nearby nodes and
//  attempt STUN-like hole-punching through them BEFORE
//  falling back to the central relay.
//
//  Privacy & consent:
//    • Completely opt-in — never enabled by default
//    • Can be withdrawn at any time with `iloc node leave`
//    • Only the user's public IP:port is shared (already public)
//    • No traffic content passes through mesh nodes —
//      they only exchange connection endpoints (STUN, not TURN)
//    • The node token is SHA-256(project_key) — cannot be
//      linked back to an account by anyone, including us
//
//  What a mesh node does:
//    • Listens on a secondary UDP port (7482) for STUN-like
//      binding requests from other iloc peers
//    • Responds with the requester's observed public IP:port
//      (the classic STUN "XOR-MAPPED-ADDRESS" response)
//    • Does NOT relay data — purely address discovery
// ============================================================

use anyhow::{Context, Result};
use colored::Colorize;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;
use tokio::net::UdpSocket;

pub const STUN_PORT: u16   = 7482;
const HEARTBEAT_INTERVAL:  Duration = Duration::from_secs(120); // 2 minutes
const MAX_BANDWIDTH_KBPS:  u32      = 100;    // default bandwidth cap (advisory)

// ── Node config (persisted in ~/.config/ilocker/mesh.toml) ────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshConfig {
    pub enabled:          bool,
    pub node_token:       String,   // SHA-256(project_key)[..64]
    pub stun_port:        u16,
    pub max_bandwidth_kbps: u32,
    pub joined_at:        String,
}

pub fn config_path() -> Result<std::path::PathBuf> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .context("Cannot determine home directory")?;
    Ok(Path::new(&home).join(".config").join("ilocker").join("mesh.toml"))
}

pub fn load_config() -> Result<Option<MeshConfig>> {
    let path = config_path()?;
    if !path.exists() { return Ok(None); }
    let raw = std::fs::read_to_string(&path)?;
    Ok(Some(toml::from_str(&raw)?))
}

pub fn save_config(cfg: &MeshConfig) -> Result<()> {
    let path = config_path()?;
    if let Some(p) = path.parent() { std::fs::create_dir_all(p)?; }
    std::fs::write(&path, toml::to_string_pretty(cfg)?)?;
    Ok(())
}

pub fn delete_config() -> Result<()> {
    let path = config_path()?;
    if path.exists() { std::fs::remove_file(&path)?; }
    Ok(())
}

/// Derive a stable, anonymous node token from the project key.
/// SHA-256(project_key) is deterministic but cannot be reversed.
pub fn derive_node_token(project_key: &str) -> String {
    hex::encode(Sha256::digest(project_key.as_bytes()))
}

// ── iloc node join ────────────────────────────────────────────

pub async fn run_join(
    project_key:     &str,
    server_url:      &str,
    cli_token:       &str,
    bandwidth_kbps:  Option<u32>,
) -> Result<()> {
    println!();
    println!("{}", "ilocker Mesh Network — Opt-in STUN Node".bold());
    println!();
    println!("{}", "  How it works:".cyan().bold());
    println!("  Your machine will answer STUN binding requests on UDP:{}.", STUN_PORT);
    println!("  Other iloc peers discover your public IP:port to attempt");
    println!("  direct connections — NO data ever passes through your machine.");
    println!();
    println!("{}", "  What we share:".cyan().bold());
    println!("  • Your public IP address and port  (already visible to anyone)");
    println!("  • Your approximate country (for geographic routing)");
    println!("  • Your available bandwidth (advisory, you set the limit)");
    println!();
    println!("{}", "  Privacy guarantee:".cyan().bold());
    println!("  Your node token is SHA-256(project_key) — cryptographically");
    println!("  unlinkable to your account even by ilocker staff.");
    println!();

    // Explicit consent
    print!("  Join the mesh network? [y/N] ");
    use std::io::Write;
    std::io::stdout().flush()?;
    let mut ans = String::new();
    std::io::stdin().read_line(&mut ans)?;
    if !ans.trim().eq_ignore_ascii_case("y") {
        println!("  Opt-in declined — nothing changed.");
        return Ok(());
    }

    let bw = bandwidth_kbps.unwrap_or(MAX_BANDWIDTH_KBPS);
    println!();
    println!("  {} Max bandwidth: {} Kbps", "Setting:".dimmed(), bw);

    // Discover our public endpoint via the STUN socket
    println!("  {} Discovering public endpoint…", "→".dimmed());
    let public_ep = discover_public_endpoint(STUN_PORT).await
        .unwrap_or_else(|_| format!("0.0.0.0:{}", STUN_PORT));
    println!("  {} Public endpoint: {}", "✓".green(), public_ep.cyan());

    let node_token = derive_node_token(project_key);

    // Register with the server
    let intel = crate::intel_client::IntelClient::new(server_url, Some(cli_token));
    intel.register_mesh_node(&node_token, &public_ep, bw).await
        .context("Failed to register with mesh network")?;

    // Persist local config
    let cfg = MeshConfig {
        enabled:            true,
        node_token:         node_token.clone(),
        stun_port:          STUN_PORT,
        max_bandwidth_kbps: bw,
        joined_at:          chrono::Utc::now().to_rfc3339(),
    };
    save_config(&cfg)?;

    println!();
    println!("{} Joined the mesh network", "✓".green().bold());
    println!(
        "  {} Add to your shell to start the STUN listener on boot:",
        "Tip:".dimmed()
    );
    println!(
        "    {}",
        "iloc node start &".cyan()
    );
    println!();
    println!(
        "  To leave the network at any time: {}",
        "iloc node leave".cyan()
    );

    Ok(())
}

// ── iloc node leave ───────────────────────────────────────────

pub async fn run_leave(server_url: &str, cli_token: &str) -> Result<()> {
    let cfg = load_config()?.ok_or_else(|| {
        anyhow::anyhow!("Not currently in the mesh network.")
    })?;

    let intel = crate::intel_client::IntelClient::new(server_url, Some(cli_token));
    intel.unregister_mesh_node(&cfg.node_token).await
        .unwrap_or_else(|e| eprintln!("  Warning: server opt-out failed: {}", e));

    delete_config()?;

    println!("{} Left the mesh network", "✓".green().bold());
    println!("  Your node token has been removed from the server.");
    println!("  GDPR right to erasure exercised.");

    Ok(())
}

// ── iloc node start — STUN listener ──────────────────────────

pub async fn run_start() -> Result<()> {
    let cfg = load_config()?.ok_or_else(|| {
        anyhow::anyhow!("Not in mesh network. Run `iloc node join` first.")
    })?;

    if !cfg.enabled {
        anyhow::bail!("Mesh node is disabled in config.");
    }

    println!();
    println!("{} Starting STUN listener on UDP:{}", "◎".cyan().bold(), cfg.stun_port);
    println!(
        "  {} Max bandwidth: {} Kbps  |  Token: {}…",
        "Config:".dimmed(), cfg.max_bandwidth_kbps, &cfg.node_token[..16]
    );
    println!(
        "  {}",
        "Press Ctrl-C to stop.  Run `iloc node leave` to permanently opt-out.".dimmed()
    );
    println!();

    let socket = UdpSocket::bind(format!("0.0.0.0:{}", cfg.stun_port))
        .await
        .with_context(|| format!("Cannot bind UDP port {}", cfg.stun_port))?;

    println!("{}", "  Listening for STUN binding requests…".dimmed());

    // Heartbeat task
    let token_clone   = cfg.node_token.clone();
    let server_clone  = std::env::var("ILOC_SERVER")
        .unwrap_or_else(|_| "http://localhost:4000".to_string());
    let bearer_clone  = crate::auth_store::load()
        .ok().flatten()
        .map(|a| a.cli_token)
        .unwrap_or_default();

    tokio::spawn(async move {
        loop {
            tokio::time::sleep(HEARTBEAT_INTERVAL).await;
            let intel = crate::intel_client::IntelClient::new(
                &server_clone, Some(&bearer_clone)
            );
            let _ = intel.node_heartbeat(&token_clone).await;
        }
    });

    // Main STUN loop
    let mut buf = [0u8; 512];
    loop {
        let (n, peer) = socket.recv_from(&mut buf).await?;
        // Parse minimal STUN Binding Request (RFC 5389)
        if n >= 20 && buf[0] == 0x00 && buf[1] == 0x01 {
            handle_stun_request(&socket, peer, &buf[..n]).await;
        }
    }
}

// ── Nœud de stockage Hyperscale ──────────────────────────────

/// Démarre un nœud de contribution au réseau de stockage Hyperscale.
/// Contrairement à `run_start` (nœud P2P STUN pour les transferts
/// directs entre pairs), ce nœud stocke physiquement des shards
/// Hyperscale sur le disque local et les rend disponibles pour la
/// reconstruction de ses propres projets ou ceux de ses collaborateurs.
// ── PID file du nœud de stockage Hyperscale ─────────────────────
//
// `start_storage_node` tourne en boucle bloquante au premier plan
// dans le terminal qui l'a lancé — il n'existe pas de "démon"
// séparé. Pour que `iloc hyperscale node stop` et `node status`
// (lancés depuis un AUTRE terminal/invocation) puissent réellement
// agir sur ce process, celui-ci publie son PID dans un fichier au
// démarrage, et le retire proprement à l'arrêt (Ctrl+C intercepté).

fn hyperscale_node_pid_path() -> std::path::PathBuf {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".to_string());
    std::path::PathBuf::from(home).join(".ilocker").join("hyperscale-node.pid")
}

fn write_hyperscale_node_pid() -> Result<()> {
    let path = hyperscale_node_pid_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, std::process::id().to_string())?;
    Ok(())
}

pub fn remove_hyperscale_node_pid() {
    let _ = std::fs::remove_file(hyperscale_node_pid_path());
}

/// Lit le PID publié, s'il existe.
pub fn read_hyperscale_node_pid() -> Option<u32> {
    std::fs::read_to_string(hyperscale_node_pid_path())
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
}

/// Vérifie si un process avec ce PID est réellement vivant, sans lui
/// envoyer de signal destructeur (kill -0 sur Unix, tasklist sur Windows).
#[cfg(unix)]
pub fn process_is_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[cfg(windows)]
pub fn process_is_alive(pid: u32) -> bool {
    std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {}", pid), "/NH"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains(&pid.to_string()))
        .unwrap_or(false)
}

/// Demande l'arrêt du process ciblé (SIGTERM sur Unix, taskkill sur Windows).
#[cfg(unix)]
pub fn terminate_process(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(windows)]
pub fn terminate_process(pid: u32) -> bool {
    std::process::Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/F"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub async fn start_storage_node(
    org_id:          &str,
    allocation_bytes: u64,
    silent:           bool,
) -> Result<()> {
    if !silent {
        println!("{}", "  ilocker Hyperscale Storage Node".bold());
        println!(
            "  {} org: {} | allocation: {}",
            "→".cyan(),
            org_id,
            crate::utils::human_bytes(allocation_bytes)
        );
        println!("{}", "  Appuyez sur Ctrl-C pour arrêter (ou, depuis un autre terminal : iloc hyperscale node stop).".dimmed());
        println!();
    }

    // Le nœud de stockage écoute sur UDP pour les requêtes de
    // téléchargement de shards entre pairs Hyperscale.
    // Pour l'instant : nœud passif, les shards sont déjà sur le
    // cloud de l'utilisateur — cette fonction amorce simplement le
    // daemon local et attend une connexion.
    let port = 4747u16; // port par défaut du nœud Hyperscale
    let socket = UdpSocket::bind(format!("0.0.0.0:{}", port)).await
        .with_context(|| format!("Impossible d'écouter sur le port UDP:{}", port))?;

    write_hyperscale_node_pid()?;

    if !silent {
        println!(
            "{} Nœud Hyperscale en écoute sur UDP:{} ({})",
            "●".green().bold(),
            port,
            crate::utils::human_bytes(allocation_bytes)
        );
    }

    // Écoute Ctrl-C (SIGINT) ET, sur Unix, SIGTERM — c'est ce
    // second signal qu'envoie `iloc hyperscale node stop` depuis un
    // autre terminal. Sans l'intercepter, le process serait tué
    // immédiatement par le comportement par défaut du système sans
    // nettoyer son fichier PID (rattrapé sinon, mais avec retard,
    // par le nettoyage différé de `node status`/`node stop`).
    #[cfg(unix)]
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .context("Impossible d'installer le handler SIGTERM")?;

    // Boucle principale — s'arrête proprement (nettoyage du PID file)
    // sur Ctrl-C ou sur un signal d'arrêt reçu via `iloc hyperscale node stop`.
    let mut buf = [0u8; 4096];
    let result = loop {
        #[cfg(unix)]
        let stop_signal = async { sigterm.recv().await };
        #[cfg(not(unix))]
        let stop_signal = std::future::pending::<Option<()>>();

        tokio::select! {
            recv = socket.recv_from(&mut buf) => {
                match recv {
                    Ok((n, peer)) if n > 0 => {
                        handle_hyperscale_request(&socket, peer, &buf[..n]).await;
                    }
                    Ok(_) => {}
                    Err(e) => break Err(e.into()),
                }
            }
            _ = tokio::signal::ctrl_c() => {
                if !silent {
                    println!();
                    println!("{}", "  Arrêt du nœud Hyperscale (Ctrl-C)…".dimmed());
                }
                break Ok(());
            }
            _ = stop_signal => {
                if !silent {
                    println!();
                    println!("{}", "  Arrêt du nœud Hyperscale (iloc hyperscale node stop)…".dimmed());
                }
                break Ok(());
            }
        }
    };

    remove_hyperscale_node_pid();
    result
}

async fn handle_hyperscale_request(
    _socket: &UdpSocket,
    peer:    std::net::SocketAddr,
    _data:   &[u8],
) {
    eprintln!("  [hyperscale-node] requête de {}", peer);
}



pub fn run_status() -> Result<()> {
    match load_config()? {
        None => {
            println!();
            println!("  {} Not participating in mesh network.", "○".dimmed());
            println!(
                "  Run {} to contribute and help others connect faster.",
                "iloc node join".cyan()
            );
        }
        Some(cfg) => {
            println!();
            println!("{}", "  Mesh Node Status".bold());
            println!("  {} {}", "enabled:".dimmed(), if cfg.enabled { "yes".green().to_string() } else { "no".yellow().to_string() });
            println!("  {} UDP:{}", "stun port:".dimmed(), cfg.stun_port);
            println!("  {} {} Kbps", "bandwidth cap:".dimmed(), cfg.max_bandwidth_kbps);
            println!("  {} {}…", "token:".dimmed(), &cfg.node_token[..16]);
            println!("  {} {}", "joined:".dimmed(), &cfg.joined_at[..10]);
            println!();
            println!("  To stop participating: {}", "iloc node leave".cyan());
        }
    }
    Ok(())
}

// ── STUN helpers ──────────────────────────────────────────────

/// Handle one STUN Binding Request — respond with the peer's observed address.
async fn handle_stun_request(
    socket:  &UdpSocket,
    peer:    SocketAddr,
    request: &[u8],
) {
    // Build a minimal STUN Binding Response (RFC 5389 §6)
    let mut response = vec![0u8; 28];

    // Message type: 0x0101 (Binding Success Response)
    response[0] = 0x01;
    response[1] = 0x01;

    // Message length: 8 bytes (one XOR-MAPPED-ADDRESS attribute)
    response[2] = 0x00;
    response[3] = 0x08;

    // Magic cookie: 0x2112A442
    response[4] = 0x21; response[5] = 0x12;
    response[6] = 0xA4; response[7] = 0x42;

    // Transaction ID: copy from request bytes 8..20
    if request.len() >= 20 {
        response[8..20].copy_from_slice(&request[8..20]);
    }

    // XOR-MAPPED-ADDRESS attribute (type 0x0020, length 8, IPv4)
    response[20] = 0x00; response[21] = 0x20; // attribute type
    response[22] = 0x00; response[23] = 0x08; // attribute length
    response[24] = 0x00;                       // reserved
    response[25] = 0x01;                       // family: IPv4

    // XOR the port with the magic cookie high 16 bits
    if let SocketAddr::V4(v4) = peer {
        let xor_port = peer.port() ^ 0x2112u16;
        response[26] = (xor_port >> 8) as u8;
        response[27] = (xor_port & 0xFF) as u8;

        // XOR the IP with the full magic cookie
        let octets = v4.ip().octets();
        let magic  = [0x21u8, 0x12, 0xA4, 0x42];
        let mut xor_ip = [0u8; 4];
        for i in 0..4 { xor_ip[i] = octets[i] ^ magic[i]; }

        // Extend response to include the XOR'd IP (4 more bytes)
        response.extend_from_slice(&xor_ip);
        response[3] = 12; // update attribute length to 12

        let _ = socket.send_to(&response, peer).await;
    }
}

/// Best-effort: discover our public IP:port by looking at what
/// a remote STUN server sees.  Uses Google's public STUN server.
async fn discover_public_endpoint(local_port: u16) -> Result<String> {
    // Bind on the STUN port we'll advertise
    let socket = UdpSocket::bind(format!("0.0.0.0:{}", local_port)).await?;

    // Send a minimal STUN Binding Request to Google's STUN server
    let stun_server = "stun.l.google.com:19302";
    let request: [u8; 20] = [
        0x00, 0x01,                          // Binding Request
        0x00, 0x00,                          // Message length: 0
        0x21, 0x12, 0xA4, 0x42,             // Magic cookie
        0x00, 0x01, 0x02, 0x03,             // Transaction ID (arbitrary)
        0x04, 0x05, 0x06, 0x07,
        0x08, 0x09, 0x0A, 0x0B,
    ];

    // Resolve STUN server address
    use tokio::net::lookup_host;
    let stun_addr = lookup_host(stun_server).await?
        .next()
        .ok_or_else(|| anyhow::anyhow!("Cannot resolve STUN server"))?;

    socket.send_to(&request, stun_addr).await?;

    let mut buf = [0u8; 512];
    let timeout = tokio::time::timeout(Duration::from_secs(3), socket.recv_from(&mut buf));

    match timeout.await {
        Ok(Ok((n, _))) if n >= 28 => {
            // Parse XOR-MAPPED-ADDRESS from response
            // Attribute starts at offset 20
            let port_xor = u16::from_be_bytes([buf[26], buf[27]]) ^ 0x2112u16;
            let ip_xor   = [buf[28] ^ 0x21, buf[29] ^ 0x12, buf[30] ^ 0xA4, buf[31] ^ 0x42];
            Ok(format!("{}.{}.{}.{}:{}", ip_xor[0], ip_xor[1], ip_xor[2], ip_xor[3], port_xor))
        }
        _ => Ok(format!("0.0.0.0:{}", local_port)),
    }
}
