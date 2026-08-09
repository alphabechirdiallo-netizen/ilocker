// ============================================================
//  commands/account.rs — account management commands
//
//  iloc login    — interactive email/password → JWT + CLI token
//  iloc logout   — revoke CLI token server-side + delete local file
//  iloc whoami   — display current user from local auth file + /me
// ============================================================

use crate::api_client::ApiClient;
use crate::auth_store::{self, AuthFile};
use anyhow::{Context, Result};
use colored::Colorize;

// ── Default server URL ────────────────────────────────────────
// Override with ILOC_SERVER env var for self-hosted or dev usage.
const DEFAULT_SERVER: &str = "http://localhost:4000";

fn server_url() -> String {
    std::env::var("ILOC_SERVER").unwrap_or_else(|_| DEFAULT_SERVER.to_string())
}

// ── iloc login ────────────────────────────────────────────────

pub async fn run_login() -> Result<()> {
    println!();
    println!("{}", "ilocker Cloud — Login".bold());
    println!(
        "  {} {}",
        "server:".dimmed(),
        server_url().cyan()
    );
    println!();

    // Check if already logged in
    if let Ok(Some(existing)) = auth_store::load() {
        println!(
            "  {} You are already logged in as {}",
            "!".yellow(),
            existing.email.bold()
        );
        println!(
            "  {} Run `iloc logout` first to switch accounts.",
            " ".dimmed()
        );
        println!();

        // Ask to continue anyway
        print!("  Login as a different account? [y/N] ");
        use std::io::Write;
        std::io::stdout().flush()?;
        let mut ans = String::new();
        std::io::stdin().read_line(&mut ans)?;
        if !ans.trim().eq_ignore_ascii_case("y") {
            println!("  Aborted.");
            return Ok(());
        }
        println!();
    }

    // ── Collect credentials interactively ─────────────────────
    let email = prompt_line("  Email: ")?;
    let email = email.trim().to_string();

    if email.is_empty() {
        anyhow::bail!("Email cannot be empty.");
    }

    // rpassword hides input so the password is never visible in the terminal
    let password = rpassword::prompt_password("  Password: ")
        .context("Failed to read password from terminal")?;

    if password.is_empty() {
        anyhow::bail!("Password cannot be empty.");
    }

    println!();
    println!("{}", "  Authenticating…".dimmed());

    // ── Call the API ──────────────────────────────────────────
    let client = ApiClient::new(&server_url());
    let resp   = client.login(&email, &password).await
        .with_context(|| format!(
            "Login failed — cannot reach {}. Is the server running?",
            server_url()
        ))?;

    // ── Persist credentials locally ───────────────────────────
    let auth = AuthFile {
        server_url:   server_url(),
        email:        resp.user.email.clone(),
        cli_token:    resp.cli_token.clone(),
        jwt:          resp.jwt.clone(),
        logged_in_at: chrono::Utc::now().to_rfc3339(),
    };
    auth_store::save(&auth)?;

    let auth_path = auth_store::auth_file_path()?;

    // ── Success output ────────────────────────────────────────
    println!("{} Logged in successfully", "✓".green().bold());
    println!("  {} {}", "account:".dimmed(),  resp.user.email.bold());
    println!("  {} {}", "plan:".dimmed(),     plan_label(&resp.user.plan).bold());
    println!("  {} {}", "token saved:".dimmed(), auth_path.display().to_string().dimmed());
    println!();
    println!(
        "{}",
        "  Credentials stored with 0600 permissions (owner-only).".dimmed()
    );

    Ok(())
}

// ── iloc logout ───────────────────────────────────────────────

pub async fn run_logout() -> Result<()> {
    let auth = auth_store::require_auth()?;

    println!();
    println!("{}", "  Revoking token on server…".dimmed());

    let client = ApiClient::new(&auth.server_url);
    match client.logout(&auth.cli_token).await {
        Ok(_) => {}
        Err(e) => {
            // Still delete locally even if server is unreachable
            println!(
                "  {} Could not revoke token on server: {}",
                "⚠".yellow(), e
            );
            println!("  Deleting local credentials anyway.");
        }
    }

    auth_store::remove()?;

    println!("{} Logged out ({})", "✓".green().bold(), auth.email.bold());
    println!();

    Ok(())
}

// ── iloc whoami ───────────────────────────────────────────────

pub async fn run_whoami() -> Result<()> {
    let auth = auth_store::require_auth()?;

    println!();
    println!("{}", "ilocker Cloud — Account".bold());
    println!();

    // Fast path: show what we know locally without hitting the network
    println!("  {} {}", "email:".dimmed(),       auth.email.bold());
    println!("  {} {}", "server:".dimmed(),       auth.server_url.cyan());
    println!("  {} {}", "logged in at:".dimmed(), auth.logged_in_at.dimmed());

    // Then fetch fresh data from /me
    println!();
    println!("{}", "  Fetching account details…".dimmed());

    let client = ApiClient::new(&auth.server_url);
    match client.me(&auth.cli_token).await {
        Ok(resp) => {
            let user = resp.user;
            let plan = user.subscription.as_ref()
                .map(|s| s.plan.as_str())
                .unwrap_or("FREE");

            println!();
            println!("  {} {}", "display name:".dimmed(),
                user.display_name.as_deref().unwrap_or("(not set)").bold());
            println!("  {} {}", "plan:".dimmed(), plan_label(plan).bold());
            println!("  {} {}", "member since:".dimmed(), &user.created_at[..10]);

            if let Some(sub) = &user.subscription {
                if let Some(end) = &sub.current_period_end {
                    println!("  {} {}", "renews:".dimmed(), &end[..10]);
                }
            }
        }
        Err(e) => {
            println!(
                "  {} Could not fetch live account data: {}",
                "⚠".yellow(), e
            );
            println!("  Showing cached credentials only.");
        }
    }

    println!();

    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────

fn prompt_line(prompt: &str) -> Result<String> {
    use std::io::Write;
    print!("{}", prompt);
    std::io::stdout().flush()?;
    let mut buf = String::new();
    std::io::stdin().read_line(&mut buf)?;
    Ok(buf.trim_end_matches('\n').trim_end_matches('\r').to_string())
}

fn plan_label(_plan: &str) -> &str {
    // ilocker est 100% gratuit — il n'y a plus de notion de plan payant.
    // (paramètre conservé pour compatibilité d'appel, volontairement ignoré)
    "Gratuit"
}
