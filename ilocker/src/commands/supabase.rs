// ============================================================
//  commands/supabase.rs — iloc supabase <sous-commande>
//
//  Couvre >90% des besoins quotidiens d'un développeur Supabase :
//
//  CONNEXION
//    iloc connect supabase              assistant interactif (token)
//    iloc supabase list                 liste les profils
//    iloc supabase use <nom>            change le profil actif
//    iloc supabase remove <nom>         déconnecte un profil
//    iloc supabase status               affiche le compte connecté
//
//  ORGANISATIONS
//    iloc supabase org list             liste les organisations
//
//  PROJETS
//    iloc supabase project create       crée un projet
//    iloc supabase project list         liste les projets
//    iloc supabase project view <ref>   détails d'un projet
//    iloc supabase project delete <ref> supprime un projet
//    iloc supabase project pause <ref>  met en pause
//    iloc supabase project restore <ref> restaure un projet en pause
//    iloc supabase project url <ref>    affiche l'URL du projet
//
//  CLÉS API
//    iloc supabase keys show <ref>      affiche les clés API
//
//  BASE DE DONNÉES
//    iloc supabase sql <ref> <query>    exécute une requête SQL brute
//    iloc supabase table list <ref>     liste les tables
//    iloc supabase extension list <ref> liste les extensions
//
//  MIGRATIONS (idempotent — ne réapplique jamais une migration déjà passée)
//    iloc supabase migration list <ref>       liste les migrations appliquées
//    iloc supabase migration push <ref> <dir> applique les migrations en attente
//    iloc supabase migration status <ref> <dir> compare local vs distant
//
//  EDGE FUNCTIONS
//    iloc supabase function list <ref>
//    iloc supabase function view <ref> <slug>
//    iloc supabase function deploy <ref> <slug> <fichier>
//    iloc supabase function delete <ref> <slug>
//
//  SECRETS (Edge Functions)
//    iloc supabase secret list <ref>
//    iloc supabase secret set <ref> <key> <value>
//    iloc supabase secret delete <ref> <key>
//
//  BRANCHES (preview environments)
//    iloc supabase branch list <ref>
//    iloc supabase branch create <ref> <nom>
//    iloc supabase branch delete <id>
//    iloc supabase branch merge <id>
//    iloc supabase branch reset <id>
//    iloc supabase branch rebase <id>
//
//  ADVISORS
//    iloc supabase advisor security <ref>
//    iloc supabase advisor performance <ref>
//
// ============================================================

use crate::supabase_client::{SupabaseClient, SbProject};
use crate::supabase_store::{self, SupabaseCredentials, SupabaseProfile};
use anyhow::{bail, Context, Result};
use colored::Colorize;
use indicatif::{ProgressBar, ProgressStyle};
use std::path::{Path, PathBuf};

// ── Helpers partagés (mêmes noms/signatures que github.rs / vercel.rs) ──

fn prompt(label: &str) -> Result<String> {
    use std::io::Write;
    print!("{}", label);
    std::io::stdout().flush()?;
    let mut buf = String::new();
    std::io::stdin().read_line(&mut buf)?;
    Ok(buf.trim().to_string())
}

fn prompt_default(label: &str, default: &str) -> Result<String> {
    let v = prompt(&format!("{} [{}]: ", label, default))?;
    Ok(if v.is_empty() { default.to_string() } else { v })
}

fn confirm(question: &str, auto_yes: bool) -> Result<bool> {
    if auto_yes { return Ok(true); }
    if std::env::var("ILOC_AUTO_CONFIRM").as_deref() == Ok("1") { return Ok(true); }
    let ans = prompt(&format!("  {} [y/N] ", question))?;
    Ok(ans.eq_ignore_ascii_case("y") || ans.eq_ignore_ascii_case("yes"))
}

fn spinner(msg: &str) -> ProgressBar {
    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::with_template("  {spinner:.cyan} {msg}")
            .unwrap()
            .tick_strings(&["⠋","⠙","⠹","⠸","⠼","⠴","⠦","⠧","⠇","⠏"]),
    );
    pb.set_message(msg.to_string());
    pb.enable_steady_tick(std::time::Duration::from_millis(80));
    pb
}

fn sb_client(creds: &SupabaseCredentials) -> SupabaseClient {
    SupabaseClient::new_from_credentials(creds)
}

/// Génère un mot de passe Postgres aléatoire sûr pour la création de
/// projet (32 caractères, alphanumérique + symboles sûrs en URL).
fn generate_db_password() -> String {
    // OsRng (CSPRNG du système) — remplace un ancien PRNG maison
    // (xorshift64 seedé par l'horloge, non-cryptographique et prévisible)
    // utilisé ici pour un vrai secret (mot de passe de base de données).
    // Même source déjà utilisée ailleurs dans le projet pour du chiffrement
    // réel (credential_vault.rs, cloud_share_token.rs, commands/github.rs) —
    // aucune nouvelle dépendance nécessaire. Charset et longueur inchangés.
    use chacha20poly1305::aead::{rand_core::RngCore, OsRng};
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut random_bytes = [0u8; 32];
    OsRng.fill_bytes(&mut random_bytes);
    let mut out = String::with_capacity(32);
    for b in random_bytes {
        out.push(CHARS[(b as usize) % CHARS.len()] as char);
    }
    out
}

// ── Affichage commun ────────────────────────────────────────────

fn print_project(p: &SbProject) {
    let status_colored = match p.status.as_str() {
        "ACTIVE_HEALTHY" => p.status.green().to_string(),
        "INACTIVE"       => p.status.dimmed().to_string(),
        "PAUSED"         => p.status.yellow().to_string(),
        s if s.starts_with("COMING_UP") || s.starts_with("RESTORING") => p.status.cyan().to_string(),
        _                => p.status.red().to_string(),
    };
    println!("  {} {} [{}]", "●".cyan(), p.name.bold(), status_colored);
    println!("    {} {}  ·  {} {}", "ref:".dimmed(), p.project_ref().dimmed(), "région:".dimmed(), p.region.dimmed());
    if let Some(db) = &p.database {
        if let Some(v) = &db.version {
            println!("    {} Postgres {}", "db:".dimmed(), v.dimmed());
        }
    }
}

// ═════════════════════════════════════════════════════════════
//  CONNEXION
// ═════════════════════════════════════════════════════════════

pub async fn run_connect(
    profile_name: Option<String>,
    token_arg:    Option<String>,
) -> Result<()> {
    println!();
    println!("{}", "  ilocker — Connecter un compte Supabase".bold());
    println!();
    println!(
        "  {}",
        "Votre token est stocké dans le trousseau système — jamais en clair sur disque.".dimmed()
    );
    println!();
    println!("  {} Pour créer un Personal Access Token Supabase :", "ℹ".cyan());
    println!("    1. https://supabase.com/dashboard/account/tokens");
    println!("    2. Nom: ilocker · Aucune expiration nécessaire pour un usage CLI");
    println!("    3. Copiez le token (préfixe sbp_) et collez-le ci-dessous");
    println!();

    // Mode non-interactif dès que --token est fourni — AUCUN prompt ne
    // doit bloquer, quel qu'il soit (leçon appliquée dès le départ,
    // pas en correction après le bug trouvé sur github/vercel connect).
    let non_interactive = token_arg.is_some();

    let existing = supabase_store::list_profiles()?;
    let default_name = if existing.profiles.is_empty() {
        "perso".to_string()
    } else {
        format!("compte-{}", existing.profiles.len() + 1)
    };
    let name = match profile_name {
        Some(n) => n,
        None if non_interactive => default_name,
        None    => prompt_default("  Nom de ce profil", &default_name)?,
    };

    println!();
    let token = match token_arg {
        Some(t) => {
            println!("  {} Token fourni via --token", "ℹ".cyan());
            t
        }
        None => rpassword::prompt_password("  Personal Access Token (masqué): ")
            .context("Impossible de lire le token (non-interactif ? utilisez --token)")?,
    };
    if token.is_empty() { bail!("Le token ne peut pas être vide."); }

    // Validation
    let sp     = spinner("Validation du token…");
    let client = SupabaseClient::new(&token);
    let orgs   = client.list_organizations().await.map_err(|e| {
        sp.finish_and_clear();
        anyhow::anyhow!("Token invalide ou inaccessible : {}", e)
    })?;
    sp.finish_and_clear();
    println!("  {} token valide", "✓".green());

    let (default_org_slug, default_org_id) = if orgs.is_empty() {
        (None, None)
    } else if orgs.len() == 1 {
        println!("  {} organisation: {} ({})", "ℹ".cyan(), orgs[0].name.bold(), orgs[0].slug.dimmed());
        (Some(orgs[0].slug.clone()), Some(orgs[0].id.clone()))
    } else if non_interactive {
        (None, None)
    } else {
        println!();
        println!("  {} Organisations accessibles :", "ℹ".cyan());
        for o in &orgs {
            println!("    {} {} ({})", "○".dimmed(), o.name.bold(), o.slug.dimmed());
        }
        println!();
        let ans = prompt("  Organisation par défaut (slug, Entrée pour choisir à chaque fois) : ")?;
        if ans.is_empty() {
            (None, None)
        } else {
            match orgs.iter().find(|o| o.slug == ans) {
                Some(o) => (Some(o.slug.clone()), Some(o.id.clone())),
                None    => {
                    println!("  {} organisation '{}' introuvable — aucune par défaut.", "⚠".yellow(), ans);
                    (None, None)
                }
            }
        }
    };

    let account = format!("{}-{}", name, uuid::Uuid::new_v4().to_string().replace('-', ""));
    let profile = SupabaseProfile {
        name: name.clone(), default_org_slug: default_org_slug.clone(),
        default_org_id, account: account.clone(),
        connected_at: chrono::Utc::now().to_rfc3339(),
    };
    supabase_store::upsert_profile(profile, existing.profiles.is_empty())?;
    supabase_store::save_token(&account, &token)?;

    println!();
    println!("{} Profil '{}' connecté", "✓".green().bold(), name.bold());
    if let Some(org) = &default_org_slug {
        println!("  {} org par défaut: {}", "ℹ".cyan(), org.cyan());
    }
    println!();
    println!("  Essayez :");
    println!("    {} — lister vos projets", "iloc supabase project list".cyan());
    println!("    {} — créer un projet", "iloc supabase project create".cyan());
    println!();
    Ok(())
}

pub fn run_list_profiles() -> Result<()> {
    let cfg = supabase_store::list_profiles()?;
    println!();
    if cfg.profiles.is_empty() {
        println!("{}", "  Aucun compte Supabase configuré.".yellow());
        println!("  Lancez {} pour connecter votre compte.", "iloc connect supabase".cyan());
        println!();
        return Ok(());
    }
    println!("{}", "  Comptes Supabase connectés".bold());
    for p in &cfg.profiles {
        let active = cfg.active.as_deref() == Some(p.name.as_str());
        let marker = if active { "●".green() } else { "○".dimmed() };
        let org    = p.default_org_slug.as_deref().unwrap_or("(à choisir)");
        println!("  {} {} — org: {} — {}", marker, p.name.bold(), org, &p.connected_at[..10.min(p.connected_at.len())].dimmed());
    }
    println!();
    Ok(())
}

pub fn run_use_profile(name: String) -> Result<()> {
    supabase_store::set_active(&name)?;
    println!("{} compte Supabase actif : {}", "✓".green().bold(), name.bold());
    Ok(())
}

pub fn run_remove_profile(name: String, yes: bool) -> Result<()> {
    if !confirm(&format!("Déconnecter le profil Supabase '{}' ?", name), yes)? {
        println!("  Annulé."); return Ok(());
    }
    if supabase_store::remove_profile(&name)? {
        println!("{} profil '{}' déconnecté.", "✓".green().bold(), name);
    } else {
        println!("{} aucun profil nommé '{}'.", "⚠".yellow(), name);
    }
    Ok(())
}

pub async fn run_status(profile: Option<String>) -> Result<()> {
    let creds  = supabase_store::require_credentials(profile.as_deref())?;
    let client = sb_client(&creds);
    let sp     = spinner("Connexion à Supabase…");
    let orgs   = client.list_organizations().await?;
    sp.finish_and_clear();

    println!();
    println!("{}", "  Statut Supabase".bold());
    println!("  {} {}", "profil:".dimmed(), creds.profile_name);
    println!("  {} {}", "organisations:".dimmed(), orgs.len());
    if let Some(org) = &creds.default_org_slug {
        println!("  {} {}", "org défaut:".dimmed(), org.cyan());
    }
    println!("  {} {}", "token:".dimmed(), "valide ✓".green());
    println!();
    Ok(())
}

// ═════════════════════════════════════════════════════════════
//  ORGANISATIONS
// ═════════════════════════════════════════════════════════════

pub async fn run_org_list(profile: Option<String>) -> Result<()> {
    let creds  = supabase_store::require_credentials(profile.as_deref())?;
    let client = sb_client(&creds);
    let sp     = spinner("Chargement des organisations…");
    let orgs   = client.list_organizations().await?;
    sp.finish_and_clear();

    if orgs.is_empty() {
        println!("{}", "  Aucune organisation.".yellow());
        return Ok(());
    }
    println!();
    println!("  {} organisation(s)", orgs.len());
    println!();
    for o in &orgs {
        println!("  {} {} ({})", "●".cyan(), o.name.bold(), o.slug.dimmed());
    }
    println!();
    Ok(())
}

// ═════════════════════════════════════════════════════════════
//  PROJETS
// ═════════════════════════════════════════════════════════════

/// Résout l'org à utiliser : arg explicite > défaut du profil > prompt
/// (ou erreur claire en mode --yes si aucune des deux et plusieurs orgs).
async fn resolve_org(
    org_arg: Option<&str>,
    creds:   &SupabaseCredentials,
    client:  &SupabaseClient,
    yes:     bool,
) -> Result<String> {
    if let Some(o) = org_arg { return Ok(o.to_string()); }
    if let Some(o) = &creds.default_org_id { return Ok(o.clone()); }

    let orgs = client.list_organizations().await?;
    if orgs.len() == 1 { return Ok(orgs[0].id.clone()); }
    if orgs.is_empty() { bail!("Aucune organisation accessible avec ce token."); }

    if yes {
        bail!(
            "Plusieurs organisations disponibles et --org non précisé : {}. Utilisez --org <slug>.",
            orgs.iter().map(|o| o.slug.as_str()).collect::<Vec<_>>().join(", ")
        );
    }

    println!();
    println!("  Organisations disponibles :");
    for o in &orgs { println!("    {} {} ({})", "○".dimmed(), o.name.bold(), o.slug.dimmed()); }
    let ans = prompt("  Slug de l'organisation : ")?;
    orgs.iter().find(|o| o.slug == ans).map(|o| o.id.clone())
        .ok_or_else(|| anyhow::anyhow!("Organisation '{}' introuvable.", ans))
}

pub async fn run_project_create(
    name:    Option<String>,
    org:     Option<String>,
    region:  Option<String>,
    db_pass: Option<String>,
    link:    bool,
    profile: Option<String>,
    yes:     bool,
) -> Result<()> {
    let creds  = supabase_store::require_credentials(profile.as_deref())?;
    let client = sb_client(&creds);

    println!();
    println!("{}", "  ilocker — Créer un projet Supabase".bold());
    println!();

    let project_name = match name {
        Some(n) => n,
        None if yes => {
            std::env::current_dir().ok()
                .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
                .unwrap_or_else(|| "mon-projet".to_string())
        }
        None => {
            let cwd = std::env::current_dir().ok()
                .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
                .unwrap_or_else(|| "mon-projet".to_string());
            prompt_default("  Nom du projet", &cwd)?
        }
    };
    if project_name.is_empty() { bail!("Le nom du projet ne peut pas être vide."); }

    let org_id = resolve_org(org.as_deref(), &creds, &client, yes).await?;
    let region = region.unwrap_or_else(|| "eu-west-1".to_string());
    let password = db_pass.unwrap_or_else(generate_db_password);

    println!();
    println!("  {} créer '{}' dans l'org {}", "→".cyan(), project_name.bold(), org_id.dimmed());
    println!("  {} région: {}", "ℹ".cyan(), region.cyan());
    println!(
        "  {} Un projet Supabase peut avoir un coût selon le plan de votre organisation.",
        "⚠".yellow()
    );
    println!("  {} Consultez https://supabase.com/pricing pour le détail.", "ℹ".cyan());

    if !confirm("Confirmer la création ?", yes)? {
        println!("  Annulé."); return Ok(());
    }

    println!();
    let sp = spinner("Création du projet (peut prendre plusieurs minutes)…");
    let project = client.create_project(&project_name, &org_id, &region, &password).await?;
    sp.finish_and_clear();

    println!("{} Projet créé !", "✓".green().bold());
    println!("  {} {}", "ref:".dimmed(), project.project_ref().bold().cyan());
    println!("  {} {}", "url:".dimmed(), client.project_url(project.project_ref()).cyan());
    println!("  {} {}", "statut:".dimmed(), project.status.dimmed());
    println!();
    println!(
        "  {} Mot de passe base de données (conservez-le en lieu sûr, non récupérable) :",
        "🔑".to_string()
    );
    println!("    {}", password.yellow());
    println!();
    println!(
        "  {} Le projet peut prendre 1-2 minutes à devenir actif : {} pour vérifier.",
        "ℹ".cyan(), "iloc supabase project view".cyan()
    );

    if link {
        write_project_link(project.project_ref())?;
        println!("  {} .supabase/project.json créé (projet lié)", "✓".green());
    }

    println!();
    Ok(())
}

pub async fn run_project_list(profile: Option<String>) -> Result<()> {
    let creds    = supabase_store::require_credentials(profile.as_deref())?;
    let client   = sb_client(&creds);
    let sp       = spinner("Chargement des projets…");
    let projects = client.list_projects().await?;
    sp.finish_and_clear();

    if projects.is_empty() {
        println!("{}", "  Aucun projet trouvé.".yellow());
        return Ok(());
    }
    println!();
    println!("  {} projet(s)", projects.len());
    println!();
    for p in &projects { print_project(p); println!(); }
    Ok(())
}

pub async fn run_project_view(project_ref: Option<String>, profile: Option<String>) -> Result<()> {
    let creds = supabase_store::require_credentials(profile.as_deref())?;
    let client = sb_client(&creds);
    let pref  = resolve_project_ref(project_ref.as_deref())?;

    let sp = spinner("Chargement…");
    let p  = client.get_project(&pref).await?;
    sp.finish_and_clear();

    println!();
    print_project(&p);
    println!("  {} {}", "url:".dimmed(), client.project_url(p.project_ref()).cyan());
    if let Some(created) = &p.created_at {
        println!("  {} {}", "créé:".dimmed(), &created[..10.min(created.len())].dimmed());
    }
    println!();
    Ok(())
}

pub async fn run_project_delete(project_ref: Option<String>, profile: Option<String>, yes: bool) -> Result<()> {
    let creds = supabase_store::require_credentials(profile.as_deref())?;
    let pref  = resolve_project_ref(project_ref.as_deref())?;

    println!();
    println!("  {} Vous êtes sur le point de SUPPRIMER définitivement le projet {}", "⚠".red().bold(), pref.bold());
    println!("  {} Cette action est IRRÉVERSIBLE — toutes les données seront perdues.", "⚠".red());
    println!();

    if !yes {
        let confirmation = prompt(&format!("  Tapez '{}' pour confirmer : ", pref))?;
        if confirmation != pref {
            println!("  Confirmation invalide — suppression annulée.");
            return Ok(());
        }
    }
    if !confirm("Supprimer définitivement ?", yes)? {
        println!("  Annulé."); return Ok(());
    }

    let client = sb_client(&creds);
    let sp     = spinner("Suppression…");
    client.delete_project(&pref).await?;
    sp.finish_and_clear();

    println!("{} Projet '{}' supprimé.", "✓".green().bold(), pref);
    Ok(())
}

pub async fn run_project_pause(project_ref: Option<String>, profile: Option<String>, yes: bool) -> Result<()> {
    let creds = supabase_store::require_credentials(profile.as_deref())?;
    let pref  = resolve_project_ref(project_ref.as_deref())?;
    if !confirm(&format!("Mettre en pause le projet '{}' ?", pref), yes)? {
        println!("  Annulé."); return Ok(());
    }
    let client = sb_client(&creds);
    let sp     = spinner("Mise en pause…");
    client.pause_project(&pref).await?;
    sp.finish_and_clear();
    println!("{} Projet '{}' en pause.", "✓".green().bold(), pref);
    Ok(())
}

pub async fn run_project_restore(project_ref: Option<String>, profile: Option<String>, yes: bool) -> Result<()> {
    let creds = supabase_store::require_credentials(profile.as_deref())?;
    let pref  = resolve_project_ref(project_ref.as_deref())?;
    if !confirm(&format!("Restaurer le projet '{}' ?", pref), yes)? {
        println!("  Annulé."); return Ok(());
    }
    let client = sb_client(&creds);
    let sp     = spinner("Restauration…");
    client.restore_project(&pref).await?;
    sp.finish_and_clear();
    println!("{} Restauration de '{}' lancée.", "✓".green().bold(), pref);
    Ok(())
}

pub async fn run_project_url(project_ref: Option<String>, profile: Option<String>) -> Result<()> {
    let _creds = supabase_store::require_credentials(profile.as_deref())?;
    let pref = resolve_project_ref(project_ref.as_deref())?;
    let client = SupabaseClient::new("");
    println!("{}", client.project_url(&pref));
    Ok(())
}

// ── Liaison locale (.supabase/project.json) ────────────────────

fn write_project_link(project_ref: &str) -> Result<()> {
    std::fs::create_dir_all(".supabase")?;
    let content = serde_json::json!({ "projectRef": project_ref });
    std::fs::write(".supabase/project.json", serde_json::to_string_pretty(&content)?)?;
    if Path::new(".gitignore").exists() {
        let gi = std::fs::read_to_string(".gitignore").unwrap_or_default();
        if !gi.contains(".supabase") {
            let mut gi = gi;
            if !gi.ends_with('\n') { gi.push('\n'); }
            gi.push_str(".supabase\n");
            let _ = std::fs::write(".gitignore", gi);
        }
    }
    Ok(())
}

fn read_project_link() -> Option<String> {
    let raw = std::fs::read_to_string(PathBuf::from(".supabase").join("project.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    v["projectRef"].as_str().map(|s| s.to_string())
}

fn resolve_project_ref(arg: Option<&str>) -> Result<String> {
    if let Some(r) = arg { return Ok(r.to_string()); }
    read_project_link().ok_or_else(|| anyhow::anyhow!(
        "Aucun projet lié. Précisez le ref du projet ou liez-en un avec --link lors de la création."
    ))
}

// ═════════════════════════════════════════════════════════════
//  CLÉS API
// ═════════════════════════════════════════════════════════════

pub async fn run_keys_show(project_ref: Option<String>, profile: Option<String>, reveal: bool) -> Result<()> {
    let creds  = supabase_store::require_credentials(profile.as_deref())?;
    let client = sb_client(&creds);
    let pref   = resolve_project_ref(project_ref.as_deref())?;

    let sp   = spinner("Chargement des clés API…");
    let keys = client.get_api_keys(&pref).await?;
    sp.finish_and_clear();

    if keys.is_empty() {
        println!("{}", "  Aucune clé API trouvée.".yellow());
        return Ok(());
    }
    println!();
    println!("  {} — {}", "Clés API".bold(), pref.dimmed());
    println!();
    for k in &keys {
        let display = if reveal {
            k.api_key.clone()
        } else {
            let visible = k.api_key.chars().take(12).collect::<String>();
            format!("{}…", visible)
        };
        println!("  {} {}", format!("{}:", k.name).cyan().bold(), display.dimmed());
    }
    if !reveal {
        println!();
        println!("  {} Ajoutez --reveal pour afficher les clés en clair.", "ℹ".cyan());
    }
    println!();
    Ok(())
}

// ═════════════════════════════════════════════════════════════
//  BASE DE DONNÉES
// ═════════════════════════════════════════════════════════════

pub async fn run_sql(
    project_ref: Option<String>,
    query:       String,
    profile:     Option<String>,
    yes:         bool,
) -> Result<()> {
    let creds  = supabase_store::require_credentials(profile.as_deref())?;
    let pref   = resolve_project_ref(project_ref.as_deref())?;

    let is_mutating = {
        let q = query.trim_start().to_uppercase();
        q.starts_with("DROP") || q.starts_with("DELETE") || q.starts_with("TRUNCATE")
            || q.starts_with("ALTER") || q.starts_with("UPDATE")
    };
    if is_mutating {
        println!();
        println!("  {} Cette requête modifie ou supprime des données :", "⚠".yellow());
        println!("    {}", query.dimmed());
        if !confirm("Exécuter quand même ?", yes)? {
            println!("  Annulé."); return Ok(());
        }
    }

    let client = sb_client(&creds);
    let sp     = spinner("Exécution de la requête…");
    let result = client.execute_sql(&pref, &query).await?;
    sp.finish_and_clear();

    println!();
    println!("{}", serde_json::to_string_pretty(&result).unwrap_or_else(|_| result.to_string()));
    Ok(())
}

pub async fn run_table_list(project_ref: Option<String>, schema: String, profile: Option<String>) -> Result<()> {
    let creds  = supabase_store::require_credentials(profile.as_deref())?;
    let client = sb_client(&creds);
    let pref   = resolve_project_ref(project_ref.as_deref())?;

    let sp     = spinner("Chargement des tables…");
    let tables = client.list_tables(&pref, &schema).await?;
    sp.finish_and_clear();

    if tables.is_empty() {
        println!("{}", "  Aucune table trouvée.".yellow());
        return Ok(());
    }
    println!();
    println!("  {} table(s) — schéma {}", tables.len(), schema.cyan());
    println!();
    for t in &tables {
        let rls = if t.rls_enabled { "RLS ✓".green().to_string() } else { "RLS ✗".red().to_string() };
        let rows = t.rows.map(|r| r.to_string()).unwrap_or_else(|| "-".to_string());
        println!("  {} {} — {} lignes — {}", "●".cyan(), t.name.bold(), rows, rls);
    }
    println!();
    Ok(())
}

pub async fn run_extension_list(project_ref: Option<String>, installed_only: bool, profile: Option<String>) -> Result<()> {
    let creds  = supabase_store::require_credentials(profile.as_deref())?;
    let client = sb_client(&creds);
    let pref   = resolve_project_ref(project_ref.as_deref())?;

    let sp   = spinner("Chargement des extensions…");
    let mut exts = client.list_extensions(&pref).await?;
    sp.finish_and_clear();

    if installed_only {
        exts.retain(|e| e.installed_version.is_some());
    }

    println!();
    println!("  {} extension(s){}", exts.len(), if installed_only { " installées" } else { "" });
    println!();
    for e in &exts {
        let status = match &e.installed_version {
            Some(v) => format!("installée ({})", v).green().to_string(),
            None    => "disponible".dimmed().to_string(),
        };
        println!("  {} {} — {}", "●".cyan(), e.name.bold(), status);
    }
    println!();
    Ok(())
}

// ═════════════════════════════════════════════════════════════
//  MIGRATIONS — idempotentes par construction
// ═════════════════════════════════════════════════════════════

/// Lit les fichiers .sql d'un dossier de migrations, triés par nom.
/// Convention Supabase : <timestamp>_<nom>.sql (ex: 20260709113021_add_x.sql)
fn read_local_migrations(dir: &Path) -> Result<Vec<(String, String, String)>> {
    // (version, name, sql_content)
    let mut out = Vec::new();
    if !dir.exists() {
        bail!("Dossier de migrations introuvable : {}", dir.display());
    }
    let mut entries: Vec<_> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("sql"))
        .collect();
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let filename = entry.file_name().to_string_lossy().to_string();
        let stem = filename.trim_end_matches(".sql");
        let (version, name) = match stem.split_once('_') {
            Some((v, n)) if v.chars().all(|c| c.is_ascii_digit()) => (v.to_string(), n.to_string()),
            _ => (stem.to_string(), stem.to_string()),
        };
        let sql = std::fs::read_to_string(entry.path())
            .with_context(|| format!("Lecture de {}", filename))?;
        out.push((version, name, sql));
    }
    Ok(out)
}

pub async fn run_migration_list(project_ref: Option<String>, profile: Option<String>) -> Result<()> {
    let creds  = supabase_store::require_credentials(profile.as_deref())?;
    let client = sb_client(&creds);
    let pref   = resolve_project_ref(project_ref.as_deref())?;

    let sp = spinner("Chargement des migrations appliquées…");
    let migrations = client.list_migrations(&pref).await?;
    sp.finish_and_clear();

    if migrations.is_empty() {
        println!("{}", "  Aucune migration appliquée.".yellow());
        return Ok(());
    }
    println!();
    println!("  {} migration(s) appliquée(s)", migrations.len());
    println!();
    for m in &migrations {
        println!("  {} {} — {}", "✓".green(), m.version.dimmed(), m.name);
    }
    println!();
    Ok(())
}

pub async fn run_migration_status(
    project_ref: Option<String>,
    dir:         PathBuf,
    profile:     Option<String>,
) -> Result<()> {
    let creds  = supabase_store::require_credentials(profile.as_deref())?;
    let client = sb_client(&creds);
    let pref   = resolve_project_ref(project_ref.as_deref())?;

    let local  = read_local_migrations(&dir)?;
    let sp     = spinner("Comparaison avec le serveur…");
    let remote = client.list_migrations(&pref).await?;
    sp.finish_and_clear();

    let remote_versions: std::collections::HashSet<&str> =
        remote.iter().map(|m| m.version.as_str()).collect();

    println!();
    println!("  {} migration(s) locale(s) — {} appliquée(s) sur le serveur", local.len(), remote.len());
    println!();
    let mut pending = 0;
    for (version, name, _) in &local {
        if remote_versions.contains(version.as_str()) {
            println!("  {} {} — {}", "✓".green(), version.dimmed(), name);
        } else {
            println!("  {} {} — {} {}", "○".yellow(), version.dimmed(), name, "(en attente)".yellow());
            pending += 1;
        }
    }
    println!();
    if pending > 0 {
        println!("  {} {} migration(s) en attente — lancez `iloc supabase migration push`", "ℹ".cyan(), pending);
    } else {
        println!("  {} Tout est synchronisé.", "✓".green());
    }
    println!();
    Ok(())
}

/// Applique UNIQUEMENT les migrations locales absentes du serveur.
/// C'est le cœur de l'idempotence : on ne réapplique jamais une
/// migration déjà passée, même si `push` est relancé cent fois.
pub async fn run_migration_push(
    project_ref: Option<String>,
    dir:         PathBuf,
    profile:     Option<String>,
    yes:         bool,
) -> Result<()> {
    let creds  = supabase_store::require_credentials(profile.as_deref())?;
    let client = sb_client(&creds);
    let pref   = resolve_project_ref(project_ref.as_deref())?;

    let local = read_local_migrations(&dir)?;
    if local.is_empty() {
        println!("{}", "  Aucun fichier de migration trouvé.".yellow());
        return Ok(());
    }

    let sp     = spinner("Vérification des migrations déjà appliquées…");
    let remote = client.list_migrations(&pref).await?;
    sp.finish_and_clear();

    let remote_versions: std::collections::HashSet<String> =
        remote.iter().map(|m| m.version.clone()).collect();

    let pending: Vec<_> = local.into_iter()
        .filter(|(version, _, _)| !remote_versions.contains(version))
        .collect();

    if pending.is_empty() {
        println!("{} Aucune migration en attente — déjà à jour.", "✓".green().bold());
        return Ok(());
    }

    println!();
    println!("  {} migration(s) à appliquer :", pending.len());
    for (version, name, _) in &pending {
        println!("    {} {} — {}", "○".yellow(), version.dimmed(), name);
    }
    println!();

    if !confirm(&format!("Appliquer {} migration(s) sur '{}' ?", pending.len(), pref), yes)? {
        println!("  Annulé."); return Ok(());
    }

    println!();
    let mut ok = 0usize;
    for (version, name, sql) in &pending {
        let sp = spinner(&format!("Application de {} ({})…", version, name));
        match client.apply_migration(&pref, version, name, sql).await {
            Ok(())  => { sp.finish_and_clear(); println!("  {} {} — {}", "✓".green(), version, name); ok += 1; }
            Err(e)  => {
                sp.finish_and_clear();
                println!("  {} {} — {} : {}", "✗".red(), version, name, e);
                println!();
                println!("  {} Migration arrêtée — corrigez le fichier avant de relancer.", "⚠".yellow());
                println!("  {} Les migrations déjà appliquées ne seront pas rejouées.", "ℹ".cyan());
                bail!("Échec de la migration {}", version);
            }
        }
    }
    println!();
    println!("{} {} migration(s) appliquée(s).", "✓".green().bold(), ok);
    Ok(())
}

// ═════════════════════════════════════════════════════════════
//  EDGE FUNCTIONS
// ═════════════════════════════════════════════════════════════

pub async fn run_function_list(project_ref: Option<String>, profile: Option<String>) -> Result<()> {
    let creds  = supabase_store::require_credentials(profile.as_deref())?;
    let client = sb_client(&creds);
    let pref   = resolve_project_ref(project_ref.as_deref())?;

    let sp    = spinner("Chargement des Edge Functions…");
    let funcs = client.list_edge_functions(&pref).await?;
    sp.finish_and_clear();

    if funcs.is_empty() {
        println!("{}", "  Aucune Edge Function.".yellow());
        return Ok(());
    }
    println!();
    println!("  {} function(s)", funcs.len());
    println!();
    for f in &funcs {
        let status = if f.status == "ACTIVE" { f.status.green().to_string() } else { f.status.dimmed().to_string() };
        println!("  {} {} [{}] — v{}", "●".cyan(), f.slug.bold(), status, f.version);
    }
    println!();
    Ok(())
}

pub async fn run_function_view(project_ref: Option<String>, slug: String, profile: Option<String>) -> Result<()> {
    let creds  = supabase_store::require_credentials(profile.as_deref())?;
    let client = sb_client(&creds);
    let pref   = resolve_project_ref(project_ref.as_deref())?;

    let sp = spinner("Chargement…");
    let f  = client.get_edge_function(&pref, &slug).await?;
    sp.finish_and_clear();

    println!();
    println!("  {} [{}]", f.slug.bold(), f.status.dimmed());
    println!("  {} v{}", "version:".dimmed(), f.version);
    println!("  {} {}", "verify_jwt:".dimmed(), f.verify_jwt);
    println!("  {} {}", "url:".dimmed(), format!("https://{}.supabase.co/functions/v1/{}", pref, f.slug).cyan());
    println!();
    Ok(())
}

pub async fn run_function_deploy(
    project_ref: Option<String>,
    slug:        String,
    file:        PathBuf,
    no_verify_jwt: bool,
    profile:     Option<String>,
    yes:         bool,
) -> Result<()> {
    let creds  = supabase_store::require_credentials(profile.as_deref())?;
    let pref   = resolve_project_ref(project_ref.as_deref())?;

    if !file.exists() { bail!("Fichier '{}' introuvable.", file.display()); }
    let source = std::fs::read_to_string(&file)
        .with_context(|| format!("Lecture de {}", file.display()))?;

    if !confirm(&format!("Déployer '{}' vers le projet '{}' ?", slug, pref), yes)? {
        println!("  Annulé."); return Ok(());
    }

    let client = sb_client(&creds);
    let sp     = spinner(&format!("Déploiement de '{}'…", slug));
    let f      = client.deploy_edge_function(&pref, &slug, &source, !no_verify_jwt).await?;
    sp.finish_and_clear();

    println!("{} Fonction '{}' déployée (v{}).", "✓".green().bold(), f.slug.bold(), f.version);
    println!("  {}", format!("https://{}.supabase.co/functions/v1/{}", pref, f.slug).cyan());
    Ok(())
}

pub async fn run_function_delete(project_ref: Option<String>, slug: String, profile: Option<String>, yes: bool) -> Result<()> {
    let creds = supabase_store::require_credentials(profile.as_deref())?;
    let pref  = resolve_project_ref(project_ref.as_deref())?;
    if !confirm(&format!("Supprimer la fonction '{}' ?", slug), yes)? {
        println!("  Annulé."); return Ok(());
    }
    let client = sb_client(&creds);
    let sp     = spinner("Suppression…");
    client.delete_edge_function(&pref, &slug).await?;
    sp.finish_and_clear();
    println!("{} Fonction '{}' supprimée.", "✓".green().bold(), slug.bold());
    Ok(())
}

// ═════════════════════════════════════════════════════════════
//  SECRETS
// ═════════════════════════════════════════════════════════════

pub async fn run_secret_list(project_ref: Option<String>, profile: Option<String>) -> Result<()> {
    let creds   = supabase_store::require_credentials(profile.as_deref())?;
    let client  = sb_client(&creds);
    let pref    = resolve_project_ref(project_ref.as_deref())?;

    let sp      = spinner("Chargement des secrets…");
    let secrets = client.list_secrets(&pref).await?;
    sp.finish_and_clear();

    if secrets.is_empty() {
        println!("{}", "  Aucun secret configuré.".yellow());
        return Ok(());
    }
    println!();
    println!("  {} secret(s) — {}", secrets.len(), pref.dimmed());
    println!("  {} Les valeurs ne sont jamais exposées par l'API.", "ℹ".cyan());
    println!();
    for s in &secrets { println!("  {} {}", "🔑".to_string(), s.name.bold()); }
    println!();
    Ok(())
}

pub async fn run_secret_set(
    project_ref: Option<String>,
    key:         String,
    value:       Option<String>,
    profile:     Option<String>,
) -> Result<()> {
    let creds = supabase_store::require_credentials(profile.as_deref())?;
    let pref  = resolve_project_ref(project_ref.as_deref())?;

    let val = match value {
        Some(v) => v,
        None    => rpassword::prompt_password(&format!("  Valeur de '{}' (masquée): ", key))
            .context("Lecture de la valeur")?,
    };
    if val.is_empty() { bail!("La valeur ne peut pas être vide."); }

    let client = sb_client(&creds);
    let sp     = spinner(&format!("Enregistrement de '{}'…", key));
    client.set_secrets(&pref, &[(key.clone(), val)]).await?;
    sp.finish_and_clear();

    println!("{} Secret '{}' enregistré.", "✓".green().bold(), key.bold());
    Ok(())
}

pub async fn run_secret_delete(project_ref: Option<String>, key: String, profile: Option<String>, yes: bool) -> Result<()> {
    let creds = supabase_store::require_credentials(profile.as_deref())?;
    let pref  = resolve_project_ref(project_ref.as_deref())?;
    if !confirm(&format!("Supprimer le secret '{}' ?", key), yes)? {
        println!("  Annulé."); return Ok(());
    }
    let client = sb_client(&creds);
    let sp     = spinner("Suppression…");
    client.delete_secret(&pref, &key).await?;
    sp.finish_and_clear();
    println!("{} Secret '{}' supprimé.", "✓".green().bold(), key.bold());
    Ok(())
}

// ═════════════════════════════════════════════════════════════
//  BRANCHES (preview environments)
// ═════════════════════════════════════════════════════════════

pub async fn run_branch_list(project_ref: Option<String>, profile: Option<String>) -> Result<()> {
    let creds  = supabase_store::require_credentials(profile.as_deref())?;
    let client = sb_client(&creds);
    let pref   = resolve_project_ref(project_ref.as_deref())?;

    let sp       = spinner("Chargement des branches…");
    let branches = client.list_branches(&pref).await?;
    sp.finish_and_clear();

    if branches.is_empty() {
        println!("{}", "  Aucune branche.".yellow());
        return Ok(());
    }
    println!();
    println!("  {} branche(s)", branches.len());
    println!();
    for b in &branches {
        let default_tag = if b.is_default { " [défaut]".green().to_string() } else { String::new() };
        println!("  {} {}{} [{}] — {}", "🌿".to_string(), b.name.bold(), default_tag, b.status.dimmed(), b.id.dimmed());
    }
    println!();
    Ok(())
}

pub async fn run_branch_create(project_ref: Option<String>, name: String, profile: Option<String>) -> Result<()> {
    let creds  = supabase_store::require_credentials(profile.as_deref())?;
    let client = sb_client(&creds);
    let pref   = resolve_project_ref(project_ref.as_deref())?;

    let sp = spinner(&format!("Création de la branche '{}'…", name));
    let b  = client.create_branch(&pref, &name).await?;
    sp.finish_and_clear();

    println!("{} Branche '{}' créée (id: {}).", "✓".green().bold(), b.name.bold(), b.id.dimmed());
    Ok(())
}

pub async fn run_branch_delete(branch_id: String, profile: Option<String>, yes: bool) -> Result<()> {
    let creds = supabase_store::require_credentials(profile.as_deref())?;
    if !confirm(&format!("Supprimer la branche '{}' ?", branch_id), yes)? {
        println!("  Annulé."); return Ok(());
    }
    let client = sb_client(&creds);
    let sp     = spinner("Suppression…");
    client.delete_branch(&branch_id).await?;
    sp.finish_and_clear();
    println!("{} Branche '{}' supprimée.", "✓".green().bold(), branch_id.dimmed());
    Ok(())
}

pub async fn run_branch_merge(branch_id: String, profile: Option<String>, yes: bool) -> Result<()> {
    let creds = supabase_store::require_credentials(profile.as_deref())?;
    if !confirm(&format!("Merger la branche '{}' vers production ?", branch_id), yes)? {
        println!("  Annulé."); return Ok(());
    }
    let client = sb_client(&creds);
    let sp     = spinner("Merge en cours…");
    client.merge_branch(&branch_id).await?;
    sp.finish_and_clear();
    println!("{} Branche '{}' mergée.", "✓".green().bold(), branch_id.dimmed());
    Ok(())
}

pub async fn run_branch_reset(branch_id: String, migration_version: Option<String>, profile: Option<String>, yes: bool) -> Result<()> {
    let creds = supabase_store::require_credentials(profile.as_deref())?;
    if !confirm(&format!("Réinitialiser la branche '{}' ? (perte des changements non trackés)", branch_id), yes)? {
        println!("  Annulé."); return Ok(());
    }
    let client = sb_client(&creds);
    let sp     = spinner("Réinitialisation…");
    client.reset_branch(&branch_id, migration_version.as_deref()).await?;
    sp.finish_and_clear();
    println!("{} Branche '{}' réinitialisée.", "✓".green().bold(), branch_id.dimmed());
    Ok(())
}

pub async fn run_branch_rebase(branch_id: String, profile: Option<String>, yes: bool) -> Result<()> {
    let creds = supabase_store::require_credentials(profile.as_deref())?;
    if !confirm(&format!("Rebaser la branche '{}' sur production ?", branch_id), yes)? {
        println!("  Annulé."); return Ok(());
    }
    let client = sb_client(&creds);
    let sp     = spinner("Rebase en cours…");
    client.rebase_branch(&branch_id).await?;
    sp.finish_and_clear();
    println!("{} Branche '{}' rebasée.", "✓".green().bold(), branch_id.dimmed());
    Ok(())
}

// ═════════════════════════════════════════════════════════════
//  ADVISORS
// ═════════════════════════════════════════════════════════════

pub async fn run_advisor_show(project_ref: Option<String>, kind: String, profile: Option<String>) -> Result<()> {
    let creds  = supabase_store::require_credentials(profile.as_deref())?;
    let client = sb_client(&creds);
    let pref   = resolve_project_ref(project_ref.as_deref())?;

    let sp    = spinner(&format!("Analyse {} en cours…", kind));
    let lints = client.get_advisors(&pref, &kind).await?;
    sp.finish_and_clear();

    if lints.is_empty() {
        println!("{} Aucun problème détecté.", "✓".green().bold());
        return Ok(());
    }
    println!();
    println!("  {} problème(s) détecté(s) — {}", lints.len(), kind.cyan());
    println!();
    for l in &lints {
        let icon = match l.level.as_str() {
            "ERROR" => "✗".red(),
            "WARN"  => "⚠".yellow(),
            _       => "ℹ".cyan(),
        };
        println!("  {} {}", icon, l.title.bold());
        if let Some(d) = &l.description { println!("    {}", d.dimmed()); }
        if let Some(r) = &l.remediation { println!("    {} {}", "→".dimmed(), r.dimmed()); }
    }
    println!();
    Ok(())
}

#[cfg(test)]
mod generate_db_password_tests {
    use super::*;

    #[test]
    fn has_expected_length_and_charset() {
        const CHARS: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
        let pw = generate_db_password();
        assert_eq!(pw.len(), 32);
        assert!(pw.chars().all(|c| CHARS.contains(c)), "caractère hors charset: {pw}");
    }

    #[test]
    fn is_not_predictable_across_calls() {
        // Avec l'ancien PRNG maison (seed = horloge), deux appels rapprochés
        // pouvaient partager le même seed nanoseconde selon la résolution de
        // l'horloge, ou être trivialement dérivables l'un de l'autre. Avec
        // OsRng, une collision ou une relation prévisible sur 32 caractères
        // (~190 bits d'entropie) est écartée pour toute preuve pratique.
        let a = generate_db_password();
        let b = generate_db_password();
        let c = generate_db_password();
        assert_ne!(a, b);
        assert_ne!(b, c);
        assert_ne!(a, c);
    }

    #[test]
    fn many_generations_have_no_duplicates() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..200 {
            assert!(seen.insert(generate_db_password()), "mot de passe dupliqué détecté sur 200 générations");
        }
    }
}
