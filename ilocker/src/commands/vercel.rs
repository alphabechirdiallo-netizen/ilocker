// ============================================================
//  commands/vercel.rs — iloc vercel <sous-commande>
//
//  Couvre >90% des besoins quotidiens d'un développeur Vercel :
//
//  CONNEXION
//    iloc connect vercel               assistant interactif (token)
//    iloc vercel list                  liste les profils
//    iloc vercel use <nom>             change le profil actif
//    iloc vercel remove <nom>          déconnecte un profil
//    iloc vercel status                affiche le compte connecté
//
//  PROJETS
//    iloc vercel project list          liste les projets
//    iloc vercel project create        crée un projet
//    iloc vercel project view [nom]    détails d'un projet
//    iloc vercel project update        met à jour un projet
//    iloc vercel project delete        supprime un projet
//    iloc vercel project link          lie à un projet existant
//    iloc vercel project unlink        délie le projet courant
//
//  DÉPLOIEMENTS
//    iloc vercel deploy                déploie le projet courant
//    iloc vercel deploy --prod         déploiement production
//    iloc vercel deploy --force        force un nouveau déploiement
//    iloc vercel deploy --wait         attend que le déploiement soit prêt
//    iloc vercel deployment list       liste les déploiements
//    iloc vercel deployment view <id>  détails d'un déploiement
//    iloc vercel deployment cancel <id> annule un déploiement
//    iloc vercel deployment delete <id> supprime un déploiement
//    iloc vercel deployment redeploy <id> redéploie
//    iloc vercel deployment promote <id>  promu en production
//    iloc vercel deployment logs <id>    logs de build
//    iloc vercel deployment files <id>   fichiers d'un déploiement
//    iloc vercel inspect               inspecte le déploiement courant
//
//  VARIABLES D'ENVIRONNEMENT
//    iloc vercel env list              liste les env vars
//    iloc vercel env add <key> <val>   ajoute une env var
//    iloc vercel env remove <key>      supprime une env var
//    iloc vercel env pull [fichier]    exporte vers .env.local
//    iloc vercel env push [fichier]    importe depuis un .env
//    iloc vercel env update <key>      met à jour une env var
//
//  DOMAINES
//    iloc vercel domain list           liste les domaines
//    iloc vercel domain add <domain>   ajoute un domaine
//    iloc vercel domain remove <dom>   supprime un domaine
//    iloc vercel domain inspect <dom>  vérifie un domaine
//    iloc vercel domain check <dom>    disponibilité d'un domaine
//    iloc vercel domain dns list       liste les enregistrements DNS
//    iloc vercel domain dns add        ajoute un enregistrement DNS
//    iloc vercel domain dns remove <id> supprime un enregistrement
//
//  ALIASES
//    iloc vercel alias list            liste les aliases
//    iloc vercel alias assign <dep> <alias>  assigne un alias
//    iloc vercel alias delete <alias>  supprime un alias
//
//  SECRETS
//    iloc vercel secret list           liste les secrets (legacy)
//    iloc vercel secret add <name>     crée un secret
//    iloc vercel secret rename <n> <n2> renomme un secret
//    iloc vercel secret delete <name>  supprime un secret
//
//  EDGE CONFIG
//    iloc vercel edge list             liste les edge configs
//    iloc vercel edge create <slug>    crée un edge config
//    iloc vercel edge items <id>       liste les items
//    iloc vercel edge update <id>      met à jour des items
//    iloc vercel edge delete <id>      supprime un edge config
//
//  WEBHOOKS
//    iloc vercel webhook list          liste les webhooks
//    iloc vercel webhook create        crée un webhook
//    iloc vercel webhook delete <id>   supprime un webhook
//
//  CHECKS (CI gates)
//    iloc vercel check list <dep>      liste les checks
//    iloc vercel check create <dep>    crée un check
//    iloc vercel check update <dep> <id> met à jour un check
//
//  TEAMS
//    iloc vercel team list             liste les teams
//    iloc vercel team switch <slug>    change la team active
//
// ============================================================

use crate::vercel_client::{
    VercelClient, VcDeployment, VcProject, VcEnvVar,
    deployment_state_icon, format_ts,
};
use crate::vercel_store::{self, VercelCredentials, VercelProfile};
use anyhow::{bail, Context, Result};
use colored::Colorize;
use indicatif::{ProgressBar, ProgressStyle};
use std::path::{Path, PathBuf};

// ── Helpers partagés ──────────────────────────────────────────

fn prompt(label: &str) -> Result<String> {
    use std::io::{IsTerminal, Write};
    if !std::io::stdin().is_terminal() {
        bail!(
            "Entrée non-interactive détectée en attendant une réponse ({}).\n\
             Fournissez la valeur via les options de la commande (voir --help),\n\
             ou définissez ILOC_AUTO_CONFIRM=1 pour bypasser les confirmations.",
            label.trim()
        );
    }
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

fn vc_client(creds: &VercelCredentials) -> VercelClient {
    VercelClient::new_from_credentials(creds)
}

/// Lit le fichier .vercel/project.json pour récupérer le project ID / org
fn read_project_link() -> Option<(String, String)> {
    let path = PathBuf::from(".vercel").join("project.json");
    let raw  = std::fs::read_to_string(&path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let org_id  = v["orgId"].as_str()?.to_string();
    let proj_id = v["projectId"].as_str()?.to_string();
    Some((org_id, proj_id))
}

/// Écrit .vercel/project.json (liaison locale au projet)
fn write_project_link(org_id: &str, project_id: &str) -> Result<()> {
    std::fs::create_dir_all(".vercel")?;
    let content = serde_json::json!({
        "orgId":     org_id,
        "projectId": project_id,
    });
    std::fs::write(
        ".vercel/project.json",
        serde_json::to_string_pretty(&content)?,
    )?;
    // Ajouter .vercel à .gitignore si présent
    if Path::new(".gitignore").exists() {
        let gi = std::fs::read_to_string(".gitignore").unwrap_or_default();
        if !gi.contains(".vercel") {
            let mut gi = gi;
            if !gi.ends_with('\n') { gi.push('\n'); }
            gi.push_str(".vercel\n");
            let _ = std::fs::write(".gitignore", gi);
        }
    }
    Ok(())
}

fn delete_project_link() {
    let _ = std::fs::remove_file(".vercel/project.json");
}

/// Résout le project ID : depuis arg ou depuis .vercel/project.json
async fn resolve_project(
    project: Option<&str>,
    client:  &VercelClient,
) -> Result<String> {
    if let Some(p) = project {
        return Ok(p.to_string());
    }
    if let Some((_, proj_id)) = read_project_link() {
        return Ok(proj_id);
    }
    bail!(
        "Aucun projet lié. Précisez --project <nom> ou liez d'abord : iloc vercel project link"
    );
}

// ── Affichage commun ──────────────────────────────────────────

fn print_deployment(d: &VcDeployment) {
    let state  = d.ready_state.as_deref().unwrap_or(d.state.as_deref().unwrap_or("?"));
    let icon   = deployment_state_icon(state);
    let target = d.target.as_deref().unwrap_or("preview");

    let icon_colored = match state {
        "READY"    => icon.green().to_string(),
        "ERROR"    => icon.red().to_string(),
        "BUILDING" => icon.cyan().to_string(),
        "CANCELED" => icon.dimmed().to_string(),
        _          => icon.yellow().to_string(),
    };

    let target_colored = if target == "production" {
        target.green().bold().to_string()
    } else {
        target.dimmed().to_string()
    };

    println!(
        "  {} {} [{}] — {}",
        icon_colored,
        format!("https://{}", d.url).cyan().bold(),
        target_colored,
        format_ts(d.created).dimmed()
    );
    println!(
        "    {} {} · {} {}",
        "id:".dimmed(),    d.id.dimmed(),
        "état:".dimmed(),  state.dimmed()
    );
    if let Some(git) = &d.git_source {
        if let Some(repo) = &git.repo {
            let branch = git.ref_.as_deref().unwrap_or("-");
            println!("    {} {} @ {}", "git:".dimmed(), repo.dimmed(), branch.dimmed());
        }
    }
}

fn print_project(p: &VcProject) {
    let framework = p.framework.as_deref().unwrap_or("custom");
    println!("  {} {} [{}]", "●".cyan(), p.name.bold(), framework.dimmed());
    if let Some(link) = &p.link {
        if let Some(repo) = &link.repo {
            let branch = link.production_branch.as_deref().unwrap_or("main");
            println!("    {} {}", "repo:".dimmed(), format!("{} @ {}", repo, branch).dimmed());
        }
    }
    if let Some(deployments) = &p.latest_deployments {
        if let Some(last) = deployments.first() {
            let state = last.ready_state.as_deref().unwrap_or("?");
            println!(
                "    {} {} ({})",
                "dernier deploy:".dimmed(),
                format!("https://{}", last.url).cyan(),
                state.dimmed()
            );
        }
    }
    println!("    {} {}", "mis à jour:".dimmed(), format_ts(p.updated_at).dimmed());
}

fn print_env(e: &VcEnvVar) {
    let targets = e.target.as_deref().unwrap_or(&[]);
    let target_str = targets.join(",");
    let type_str = e.env_type.as_deref().unwrap_or("plain");
    println!(
        "  {} {} {} [{}]",
        "●".cyan(),
        e.key.bold(),
        target_str.dimmed(),
        type_str.dimmed()
    );
}

// ═════════════════════════════════════════════════════════════
//  CONNEXION
// ═════════════════════════════════════════════════════════════

pub async fn run_connect(profile_name: Option<String>, token_arg: Option<String>) -> Result<()> {
    println!();
    println!("{}", "  ilocker — Connecter un compte Vercel".bold());
    println!();
    println!(
        "  {}",
        "Votre token est stocké dans le trousseau système — jamais en clair sur disque.".dimmed()
    );
    println!();
    println!("  {} Pour créer un token Vercel :", "ℹ".cyan());
    println!("    1. https://vercel.com/account/tokens");
    println!("    2. Nom: ilocker · Scope: Full Account (ou Team)");
    println!("    3. Copiez le token et collez-le ci-dessous");
    println!();

    let existing = vercel_store::list_profiles()?;
    let default_name = if existing.profiles.is_empty() {
        "perso".to_string()
    } else {
        format!("compte-{}", existing.profiles.len() + 1)
    };

    // Mode non-interactif : dès que --token est fourni, aucun prompt ne doit
    // jamais bloquer en attente de stdin (usage CI/scripts).
    let non_interactive = token_arg.is_some();

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
        None => rpassword::prompt_password("  Token Vercel (masqué): ")
            .context("Impossible de lire le token (non-interactif ? utilisez --token)")?,
    };
    if token.is_empty() { bail!("Le token ne peut pas être vide."); }

    // Validation
    let sp     = spinner("Validation du token…");
    let client = VercelClient::new(&token, None);
    let user   = client.get_user().await.map_err(|e| {
        sp.finish_and_clear();
        anyhow::anyhow!("Token invalide ou inaccessible : {}", e)
    })?;
    sp.finish_and_clear();
    println!("  {} connecté : {} ({})", "✓".green(), user.username.bold().cyan(), user.email.dimmed());

    // Teams disponibles
    let teams  = client.list_teams().await.unwrap_or_default();
    let (default_team, default_team_id) = if teams.is_empty() || non_interactive {
        (None, None)
    } else {
        println!();
        println!("  {} Teams accessibles :", "ℹ".cyan());
        for t in &teams {
            println!("    {} {} ({})", "○".dimmed(), t.name.bold(), t.slug.dimmed());
        }
        println!();
        let ans = prompt("  Team par défaut (slug, Entrée pour compte personnel) : ")?;
        if ans.is_empty() {
            (None, None)
        } else {
            let found = teams.iter().find(|t| t.slug == ans);
            match found {
                Some(t) => {
                    println!("  {} Team '{}' sélectionnée.", "✓".green(), t.name.bold());
                    (Some(t.slug.clone()), Some(t.id.clone()))
                }
                None => {
                    println!("  {} Team '{}' introuvable — scope personnel utilisé.", "⚠".yellow(), ans);
                    (None, None)
                }
            }
        }
    };

    let account = format!("{}-{}", name, uuid::Uuid::new_v4().to_string().replace('-', ""));
    let profile = VercelProfile {
        name:            name.clone(),
        email:           user.email.clone(),
        username:        user.username.clone(),
        default_team:    default_team.clone(),
        default_team_id: default_team_id.clone(),
        account:         account.clone(),
        connected_at:    chrono::Utc::now().to_rfc3339(),
    };
    vercel_store::upsert_profile(profile, existing.profiles.is_empty())?;
    vercel_store::save_token(&account, &token)?;

    println!();
    println!("{} Profil '{}' connecté ({} / {})",
        "✓".green().bold(), name.bold(), user.username.cyan(), user.email.dimmed()
    );
    if let Some(team) = &default_team {
        println!("  {} team: {}", "ℹ".cyan(), team.cyan());
    }
    println!();
    println!("  Essayez :");
    println!("    {} — lister vos projets", "iloc vercel project list".cyan());
    println!("    {} — déployer le projet courant", "iloc vercel deploy".cyan());
    println!();
    Ok(())
}

pub fn run_list_profiles() -> Result<()> {
    let cfg = vercel_store::list_profiles()?;
    println!();
    if cfg.profiles.is_empty() {
        println!("{}", "  Aucun compte Vercel configuré.".yellow());
        println!("  Lancez {} pour connecter votre compte.", "iloc connect vercel".cyan());
        println!();
        return Ok(());
    }
    println!("{}", "  Comptes Vercel connectés".bold());
    for p in &cfg.profiles {
        let active = cfg.active.as_deref() == Some(p.name.as_str());
        let marker = if active { "●".green() } else { "○".dimmed() };
        let team   = p.default_team.as_deref().unwrap_or("(personnel)");
        println!(
            "  {} {} — {} — team: {} — {}",
            marker, p.name.bold(), p.username.cyan(), team, &p.connected_at[..10].dimmed()
        );
    }
    println!();
    Ok(())
}

pub fn run_use_profile(name: String) -> Result<()> {
    vercel_store::set_active(&name)?;
    println!("{} compte Vercel actif : {}", "✓".green().bold(), name.bold());
    Ok(())
}

pub fn run_remove_profile(name: String, yes: bool) -> Result<()> {
    if !confirm(&format!("Déconnecter le profil Vercel '{}' ?", name), yes)? {
        println!("  Annulé."); return Ok(());
    }
    if vercel_store::remove_profile(&name)? {
        println!("{} profil '{}' déconnecté.", "✓".green().bold(), name);
    } else {
        println!("{} aucun profil nommé '{}'.", "⚠".yellow(), name);
    }
    Ok(())
}

pub async fn run_status(profile: Option<String>) -> Result<()> {
    let creds  = vercel_store::require_credentials(profile.as_deref())?;
    let client = vc_client(&creds);
    let sp     = spinner("Connexion à Vercel…");
    let user   = client.get_user().await?;
    sp.finish_and_clear();

    println!();
    println!("{}", "  Statut Vercel".bold());
    println!("  {} {}", "compte:".dimmed(),  user.username.bold().cyan());
    println!("  {} {}", "email:".dimmed(),   user.email.dimmed());
    println!("  {} {}", "profil:".dimmed(),  creds.profile_name);
    if let Some(team) = &creds.default_team {
        println!("  {} {}", "team:".dimmed(), team.cyan());
    }
    println!("  {} {}", "token:".dimmed(), "valide ✓".green());
    println!();
    Ok(())
}

// ═════════════════════════════════════════════════════════════
//  PROJETS
// ═════════════════════════════════════════════════════════════

pub async fn run_project_list(
    limit:   usize,
    profile: Option<String>,
) -> Result<()> {
    let creds   = vercel_store::require_credentials(profile.as_deref())?;
    let client  = vc_client(&creds);
    let sp      = spinner("Chargement des projets…");
    let projects = client.list_projects(limit).await?;
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

pub async fn run_project_create(
    name:             Option<String>,
    framework:        Option<String>,
    root_directory:   Option<String>,
    build_command:    Option<String>,
    output_directory: Option<String>,
    install_command:  Option<String>,
    git_repo:         Option<String>,
    git_branch:       Option<String>,
    link:             bool,
    profile:          Option<String>,
    yes:              bool,
) -> Result<()> {
    let creds = vercel_store::require_credentials(profile.as_deref())?;

    println!();
    println!("{}", "  ilocker — Créer un projet Vercel".bold());
    println!();

    let project_name = match name {
        Some(n) => n,
        None if yes => {
            std::env::current_dir()
                .ok()
                .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
                .unwrap_or_else(|| "mon-projet".to_string())
        }
        None    => {
            let cwd = std::env::current_dir()
                .ok()
                .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
                .unwrap_or_else(|| "mon-projet".to_string());
            prompt_default("  Nom du projet", &cwd)?
        }
    };
    if project_name.is_empty() { bail!("Le nom du projet ne peut pas être vide."); }

    let fw = match framework {
        Some(f) => Some(f),
        None if yes => None,
        None    => {
            println!("  Frameworks : nextjs · react · vue · nuxt · svelte · angular · remix · astro · gatsby · none");
            let f = prompt("  Framework (Entrée pour auto-détect) : ")?;
            if f.is_empty() { None } else { Some(f) }
        }
    };

    // Résumé
    println!();
    println!("  {} créer '{}'", "→".cyan(), project_name.bold());
    if let Some(f)  = &fw              { println!("  {} {}", "framework:".dimmed(), f); }
    if let Some(r)  = &root_directory  { println!("  {} {}", "root:".dimmed(), r); }
    if let Some(g)  = &git_repo        { println!("  {} {}", "git repo:".dimmed(), g); }
    if let Some(b)  = &git_branch      { println!("  {} {}", "branche prod:".dimmed(), b); }

    if !confirm("Créer le projet ?", yes)? {
        println!("  Annulé."); return Ok(());
    }

    let client = vc_client(&creds);
    let sp     = spinner("Création du projet…");
    let project = client.create_project(
        &project_name,
        fw.as_deref(),
        root_directory.as_deref(),
        build_command.as_deref(),
        output_directory.as_deref(),
        install_command.as_deref(),
        None,
        git_repo.as_deref(),
        if git_repo.is_some() { Some("github") } else { None },
        git_branch.as_deref(),
    ).await?;
    sp.finish_and_clear();

    println!("{} Projet '{}' créé (id: {})", "✓".green().bold(), project.name.bold(), project.id.dimmed());

    // Lier localement si demandé
    if link {
        let org_id = creds.default_team_id.as_deref().unwrap_or(&creds.username);
        write_project_link(org_id, &project.id)?;
        println!("  {} .vercel/project.json créé (projet lié)", "✓".green());
    }

    println!();
    println!("  Prochaines étapes :");
    println!("    {} — déployer en production", "iloc vercel deploy --prod".cyan());
    println!("    {} — ajouter des variables d'env", "iloc vercel env add KEY value".cyan());
    println!();
    Ok(())
}

pub async fn run_project_view(
    project: Option<String>,
    profile: Option<String>,
) -> Result<()> {
    let creds  = vercel_store::require_credentials(profile.as_deref())?;
    let client = vc_client(&creds);
    let proj_id = resolve_project(project.as_deref(), &client).await?;

    let sp = spinner("Chargement…");
    let p  = client.get_project(&proj_id).await?;
    sp.finish_and_clear();

    println!();
    println!("  {}", p.name.bold().cyan());
    println!("  {} {}", "id:".dimmed(), p.id.dimmed());
    if let Some(f) = &p.framework { println!("  {} {}", "framework:".dimmed(), f); }
    if let Some(n) = &p.node_version { println!("  {} {}", "node:".dimmed(), n); }
    if let Some(r) = &p.root_directory { println!("  {} {}", "root:".dimmed(), r); }
    if let Some(b) = &p.build_command { println!("  {} {}", "build:".dimmed(), b); }
    if let Some(o) = &p.output_directory { println!("  {} {}", "output:".dimmed(), o); }
    if let Some(i) = &p.install_command { println!("  {} {}", "install:".dimmed(), i); }

    if let Some(link) = &p.link {
        if let Some(repo) = &link.repo {
            let branch = link.production_branch.as_deref().unwrap_or("main");
            println!("  {} {} @ {}", "git:".dimmed(), repo.cyan(), branch);
        }
    }

    if let Some(deployments) = &p.latest_deployments {
        if !deployments.is_empty() {
            println!();
            println!("  {}", "Derniers déploiements :".dimmed());
            for d in deployments.iter().take(3) { print_deployment(d); }
        }
    }

    println!("  {} {}", "créé:".dimmed(), format_ts(p.created_at).dimmed());
    println!();
    Ok(())
}

pub async fn run_project_update(
    project:          Option<String>,
    name:             Option<String>,
    framework:        Option<String>,
    root_directory:   Option<String>,
    build_command:    Option<String>,
    output_directory: Option<String>,
    install_command:  Option<String>,
    node_version:     Option<String>,
    production_branch: Option<String>,
    profile:          Option<String>,
) -> Result<()> {
    let creds  = vercel_store::require_credentials(profile.as_deref())?;
    let client = vc_client(&creds);
    let proj_id = resolve_project(project.as_deref(), &client).await?;

    let sp = spinner("Mise à jour du projet…");
    let p  = client.update_project(
        &proj_id,
        name.as_deref(),
        framework.as_deref(),
        root_directory.as_deref(),
        build_command.as_deref(),
        output_directory.as_deref(),
        install_command.as_deref(),
        None,
        node_version.as_deref(),
        production_branch.as_deref(),
    ).await?;
    sp.finish_and_clear();

    println!("{} Projet '{}' mis à jour.", "✓".green().bold(), p.name.bold());
    Ok(())
}

pub async fn run_project_delete(
    project: Option<String>,
    profile: Option<String>,
    yes:     bool,
) -> Result<()> {
    let creds  = vercel_store::require_credentials(profile.as_deref())?;
    let client = vc_client(&creds);
    let proj_id = resolve_project(project.as_deref(), &client).await?;

    println!();
    println!(
        "  {} Supprimer définitivement le projet {} ?",
        "⚠".red().bold(), proj_id.bold()
    );
    if !confirm("Cette action est irréversible. Confirmer ?", yes)? {
        println!("  Annulé."); return Ok(());
    }

    let sp = spinner("Suppression…");
    client.delete_project(&proj_id).await?;
    sp.finish_and_clear();

    delete_project_link();
    println!("{} Projet '{}' supprimé.", "✓".green().bold(), proj_id.bold());
    Ok(())
}

pub async fn run_project_link(
    project: Option<String>,
    profile: Option<String>,
) -> Result<()> {
    let creds  = vercel_store::require_credentials(profile.as_deref())?;
    let client = vc_client(&creds);

    let proj_name = match project {
        Some(p) => p,
        None    => {
            let sp       = spinner("Chargement des projets…");
            let projects = client.list_projects(50).await?;
            sp.finish_and_clear();

            if projects.is_empty() {
                bail!("Aucun projet trouvé. Créez-en un avec `iloc vercel project create`.");
            }
            println!();
            println!("  {} projet(s) disponibles :", projects.len());
            for (i, p) in projects.iter().enumerate() {
                println!("    {} {} — {}", format!("[{}]", i + 1).dimmed(), p.name.bold(), p.id.dimmed());
            }
            println!();
            let ans = prompt("  Numéro ou nom du projet : ")?;
            if let Ok(idx) = ans.parse::<usize>() {
                projects.get(idx - 1)
                    .map(|p| p.name.clone())
                    .ok_or_else(|| anyhow::anyhow!("Index invalide"))?
            } else {
                ans
            }
        }
    };

    let sp = spinner("Résolution du projet…");
    let p  = client.get_project(&proj_name).await?;
    sp.finish_and_clear();

    let org_id = creds.default_team_id.as_deref().unwrap_or(&creds.username);
    write_project_link(org_id, &p.id)?;

    println!(
        "{} Projet '{}' lié (id: {})",
        "✓".green().bold(), p.name.bold(), p.id.dimmed()
    );
    println!("  {} .vercel/project.json créé", "✓".green());
    Ok(())
}

pub fn run_project_unlink(yes: bool) -> Result<()> {
    if !read_project_link().is_some() {
        println!("{}", "  Aucun projet lié dans ce dossier.".yellow());
        return Ok(());
    }
    if !confirm("Supprimer la liaison locale (.vercel/project.json) ?", yes)? {
        println!("  Annulé."); return Ok(());
    }
    delete_project_link();
    println!("{} Liaison supprimée.", "✓".green().bold());
    Ok(())
}

// ═════════════════════════════════════════════════════════════
//  DÉPLOIEMENTS
// ═════════════════════════════════════════════════════════════

pub async fn run_deploy(
    production: bool,
    force:      bool,
    wait:       bool,
    project:    Option<String>,
    git_branch: Option<String>,
    git_sha:    Option<String>,
    timeout:    u64,
    profile:    Option<String>,
    yes:        bool,
) -> Result<()> {
    let creds  = vercel_store::require_credentials(profile.as_deref())?;
    let client = vc_client(&creds);

    let proj_id = resolve_project(project.as_deref(), &client).await?;

    let target = if production { "production" } else { "preview" };

    println!();
    println!(
        "  {} Déployer '{}' vers {}",
        "→".cyan(), proj_id.bold(),
        if production { "production".green().bold().to_string() }
        else          { "preview".dimmed().to_string() }
    );
    if let Some(b) = &git_branch { println!("  {} branche: {}", "ℹ".cyan(), b.cyan()); }
    if let Some(s) = &git_sha    { println!("  {} commit: {}", "ℹ".cyan(), &s[..8]); }
    if force { println!("  {} force: oui", "ℹ".cyan()); }
    println!();

    if !confirm("Lancer le déploiement ?", yes)? {
        println!("  Annulé."); return Ok(());
    }

    let sp = spinner("Déclenchement du déploiement…");
    let d  = client.create_deployment_from_git(
        &proj_id,
        git_sha.as_deref(),
        git_branch.as_deref(),
        Some(target),
        force,
    ).await?;
    sp.finish_and_clear();

    println!(
        "{} Déploiement déclenché : {}",
        "✓".green().bold(),
        format!("https://{}", d.url).cyan().bold()
    );
    println!("  {} {}", "id:".dimmed(), d.id.dimmed());

    if wait {
        println!();
        let sp2 = spinner("Attente du déploiement…");
        let final_d = client.wait_deployment_ready(&d.id, timeout, |dep| {
            let state = dep.ready_state.as_deref().unwrap_or("?");
            sp2.set_message(format!("état: {} …", state));
        }).await?;
        sp2.finish_and_clear();

        let state = final_d.ready_state.as_deref().unwrap_or("?");
        match state {
            "READY" => {
                println!("{} Déploiement prêt !", "✓".green().bold());
                println!("  → {}", format!("https://{}", final_d.url).cyan().bold());
            }
            "ERROR" => {
                println!("{} Déploiement en erreur.", "✗".red().bold());
                println!("  Consultez les logs : {} {}", "iloc vercel deployment logs".cyan(), d.id.dimmed());
            }
            other => println!("{} État final: {}", "⚠".yellow(), other),
        }
    } else {
        println!();
        println!("  {} suivre: {} {}", "ℹ".cyan(), "iloc vercel deployment logs".cyan(), d.id.dimmed());
        println!("  {} statut: {} {}", "ℹ".cyan(), "iloc vercel deployment view".cyan(), d.id.dimmed());
    }

    println!();
    Ok(())
}

pub async fn run_deployment_list(
    project: Option<String>,
    target:  Option<String>,
    state:   Option<String>,
    limit:   usize,
    profile: Option<String>,
) -> Result<()> {
    let creds  = vercel_store::require_credentials(profile.as_deref())?;
    let client = vc_client(&creds);
    let proj_id = if project.is_some() {
        resolve_project(project.as_deref(), &client).await.ok()
    } else {
        read_project_link().map(|(_, p)| p)
    };

    let sp = spinner("Chargement des déploiements…");
    let deployments = client.list_deployments(
        proj_id.as_deref(),
        target.as_deref(),
        state.as_deref(),
        limit,
    ).await?;
    sp.finish_and_clear();

    if deployments.is_empty() {
        println!("{}", "  Aucun déploiement trouvé.".yellow());
        return Ok(());
    }
    println!();
    println!("  {} déploiement(s)", deployments.len());
    println!();
    for d in &deployments { print_deployment(d); println!(); }
    Ok(())
}

pub async fn run_deployment_view(
    id_or_url: String,
    profile:   Option<String>,
) -> Result<()> {
    let creds  = vercel_store::require_credentials(profile.as_deref())?;
    let client = vc_client(&creds);

    let sp = spinner("Chargement…");
    let d  = client.get_deployment(&id_or_url).await?;
    sp.finish_and_clear();

    println!();
    print_deployment(&d);
    println!();
    Ok(())
}

pub async fn run_deployment_cancel(
    id:      String,
    profile: Option<String>,
    yes:     bool,
) -> Result<()> {
    if !confirm(&format!("Annuler le déploiement '{}' ?", id), yes)? {
        println!("  Annulé."); return Ok(());
    }
    let creds  = vercel_store::require_credentials(profile.as_deref())?;
    let client = vc_client(&creds);

    let sp = spinner("Annulation…");
    client.cancel_deployment(&id).await?;
    sp.finish_and_clear();
    println!("{} Déploiement '{}' annulé.", "✓".green().bold(), id.dimmed());
    Ok(())
}

pub async fn run_deployment_delete(
    id:      String,
    profile: Option<String>,
    yes:     bool,
) -> Result<()> {
    if !confirm(&format!("Supprimer définitivement le déploiement '{}' ?", id), yes)? {
        println!("  Annulé."); return Ok(());
    }
    let creds  = vercel_store::require_credentials(profile.as_deref())?;
    let client = vc_client(&creds);

    let sp = spinner("Suppression…");
    client.delete_deployment(&id).await?;
    sp.finish_and_clear();
    println!("{} Déploiement '{}' supprimé.", "✓".green().bold(), id.dimmed());
    Ok(())
}

pub async fn run_deployment_redeploy(
    id:      String,
    target:  Option<String>,
    profile: Option<String>,
    yes:     bool,
) -> Result<()> {
    if !confirm(&format!("Redéployer '{}' ?", id), yes)? {
        println!("  Annulé."); return Ok(());
    }
    let creds  = vercel_store::require_credentials(profile.as_deref())?;
    let client = vc_client(&creds);

    let sp = spinner("Redéploiement…");
    let d  = client.redeploy(&id, target.as_deref()).await?;
    sp.finish_and_clear();

    println!(
        "{} Redéploiement lancé : {}",
        "✓".green().bold(),
        format!("https://{}", d.url).cyan().bold()
    );
    Ok(())
}

pub async fn run_deployment_promote(
    id:      String,
    project: Option<String>,
    profile: Option<String>,
    yes:     bool,
) -> Result<()> {
    let creds  = vercel_store::require_credentials(profile.as_deref())?;
    let client = vc_client(&creds);
    let proj_id = resolve_project(project.as_deref(), &client).await?;

    if !confirm(&format!("Promouvoir '{}' en production ?", id), yes)? {
        println!("  Annulé."); return Ok(());
    }

    let sp = spinner("Promotion en production…");
    client.promote_deployment(&proj_id, &id).await?;
    sp.finish_and_clear();

    println!("{} Déploiement '{}' promu en production.", "✓".green().bold(), id.dimmed());
    Ok(())
}

pub async fn run_deployment_logs(
    id:      String,
    profile: Option<String>,
) -> Result<()> {
    let creds  = vercel_store::require_credentials(profile.as_deref())?;
    let client = vc_client(&creds);

    let sp   = spinner("Chargement des logs…");
    let logs = client.get_deployment_logs(&id).await?;
    sp.finish_and_clear();

    if logs.is_empty() {
        println!("{}", "  Aucun log disponible.".yellow());
        return Ok(());
    }
    println!();
    println!("  {} logs — {}", logs.len(), id.dimmed());
    println!();
    for entry in &logs {
        let text  = entry["text"].as_str().unwrap_or("");
        let level = entry["type"].as_str().unwrap_or("info");
        let line  = match level {
            "error"   => format!("  {} {}", "✗".red(), text),
            "warning" => format!("  {} {}", "⚠".yellow(), text),
            _         => format!("  {} {}", "·".dimmed(), text),
        };
        println!("{}", line);
    }
    println!();
    Ok(())
}

pub async fn run_deployment_files(
    id:      String,
    profile: Option<String>,
) -> Result<()> {
    let creds  = vercel_store::require_credentials(profile.as_deref())?;
    let client = vc_client(&creds);

    let sp    = spinner("Chargement des fichiers…");
    let files = client.list_deployment_files(&id).await?;
    sp.finish_and_clear();

    fn print_files(files: &[crate::vercel_client::VcFile], prefix: &str) {
        for f in files {
            let icon = if f.file_type.as_deref() == Some("directory") { "📁" } else { "📄" };
            println!("  {}{} {}", prefix, icon, f.name);
            if let Some(children) = &f.children {
                print_files(children, &format!("{}  ", prefix));
            }
        }
    }

    println!();
    println!("  Fichiers du déploiement {}", id.dimmed());
    println!();
    print_files(&files, "");
    println!();
    Ok(())
}

pub async fn run_inspect(profile: Option<String>) -> Result<()> {
    let (_, proj_id) = read_project_link()
        .ok_or_else(|| anyhow::anyhow!(
            "Aucun projet lié. Lancez `iloc vercel project link` d'abord."
        ))?;

    let creds  = vercel_store::require_credentials(profile.as_deref())?;
    let client = vc_client(&creds);

    let sp = spinner("Chargement du dernier déploiement…");
    let deployments = client.list_deployments(Some(&proj_id), None, Some("READY"), 1).await?;
    sp.finish_and_clear();

    if deployments.is_empty() {
        println!("{}", "  Aucun déploiement READY trouvé pour ce projet.".yellow());
        return Ok(());
    }
    let d = &deployments[0];
    println!();
    print_deployment(d);
    println!();
    Ok(())
}

// ═════════════════════════════════════════════════════════════
//  VARIABLES D'ENVIRONNEMENT
// ═════════════════════════════════════════════════════════════

pub async fn run_env_list(
    project: Option<String>,
    profile: Option<String>,
) -> Result<()> {
    let creds  = vercel_store::require_credentials(profile.as_deref())?;
    let client = vc_client(&creds);
    let proj_id = resolve_project(project.as_deref(), &client).await?;

    let sp   = spinner("Chargement des variables…");
    let envs = client.list_env(&proj_id).await?;
    sp.finish_and_clear();

    if envs.is_empty() {
        println!("{}", "  Aucune variable d'environnement configurée.".yellow());
        return Ok(());
    }

    println!();
    println!("  {} variable(s) — projet {}", envs.len(), proj_id.dimmed());
    println!();
    for e in &envs { print_env(e); }
    println!();
    Ok(())
}

pub async fn run_env_add(
    key:     String,
    value:   Option<String>,
    targets: Vec<String>,
    env_type: Option<String>,
    git_branch: Option<String>,
    project: Option<String>,
    profile: Option<String>,
) -> Result<()> {
    let creds  = vercel_store::require_credentials(profile.as_deref())?;
    let client = vc_client(&creds);
    let proj_id = resolve_project(project.as_deref(), &client).await?;

    let val = match value {
        Some(v) => v,
        None    => rpassword::prompt_password(&format!("  Valeur de '{}' (masquée): ", key))
            .context("Lecture de la valeur")?,
    };
    if val.is_empty() { bail!("La valeur ne peut pas être vide."); }

    let tgts: Vec<&str> = if targets.is_empty() {
        vec!["production", "preview", "development"]
    } else {
        targets.iter().map(|s| s.as_str()).collect()
    };

    let et = env_type.as_deref().unwrap_or("encrypted");

    let sp = spinner(&format!("Ajout de '{}'…", key));
    client.upsert_env(&proj_id, &key, &val, &tgts, et, git_branch.as_deref()).await?;
    sp.finish_and_clear();

    println!(
        "{} '{}' → {} [{}]",
        "✓".green().bold(), key.bold(), tgts.join(",").cyan(), et.dimmed()
    );
    Ok(())
}

pub async fn run_env_remove(
    key:     String,
    target:  Option<String>,
    project: Option<String>,
    profile: Option<String>,
    yes:     bool,
) -> Result<()> {
    let creds  = vercel_store::require_credentials(profile.as_deref())?;
    let client = vc_client(&creds);
    let proj_id = resolve_project(project.as_deref(), &client).await?;

    let envs = client.list_env(&proj_id).await?;
    let matching: Vec<&VcEnvVar> = envs.iter().filter(|e| {
        e.key == key && match (&target, &e.target) {
            (Some(t), Some(tgts)) => tgts.contains(t),
            (None,    _)          => true,
            _                     => false,
        }
    }).collect();

    if matching.is_empty() {
        bail!("Variable '{}' introuvable.", key);
    }

    if !confirm(&format!("Supprimer '{}' ({} entrée(s)) ?", key, matching.len()), yes)? {
        println!("  Annulé."); return Ok(());
    }

    let sp = spinner("Suppression…");
    for e in &matching {
        if let Some(id) = &e.id {
            client.delete_env(&proj_id, id).await?;
        }
    }
    sp.finish_and_clear();

    println!("{} '{}' supprimé.", "✓".green().bold(), key.bold());
    Ok(())
}

pub async fn run_env_pull(
    output:  Option<PathBuf>,
    project: Option<String>,
    targets: Vec<String>,
    profile: Option<String>,
    yes:     bool,
) -> Result<()> {
    let creds  = vercel_store::require_credentials(profile.as_deref())?;
    let client = vc_client(&creds);
    let proj_id = resolve_project(project.as_deref(), &client).await?;

    let out_path = output.unwrap_or_else(|| PathBuf::from(".env.local"));

    if out_path.exists() && !confirm(
        &format!("{} existe déjà. Écraser ?", out_path.display()), yes
    )? {
        println!("  Annulé."); return Ok(());
    }

    let filter_targets: Vec<String> = if targets.is_empty() {
        vec!["development".to_string()]
    } else {
        targets
    };

    let sp   = spinner("Récupération des variables d'environnement…");
    let vars = client.pull_env_all(&proj_id).await?;
    sp.finish_and_clear();

    let filtered: Vec<(String, String)> = vars.into_iter()
        .filter(|(_, _, tgts)| {
            tgts.iter().any(|t| filter_targets.contains(t))
        })
        .map(|(k, v, _)| (k, v))
        .collect();

    let mut content = format!(
        "# Généré par ilocker — iloc vercel env pull\n# Projet: {}\n# Targets: {}\n\n",
        proj_id, filter_targets.join(",")
    );
    for (k, v) in &filtered {
        // Échapper les valeurs multi-lignes
        if v.contains('\n') {
            content.push_str(&format!("{}=\"{}\"\n", k, v.replace('"', "\\\"")));
        } else {
            content.push_str(&format!("{}={}\n", k, v));
        }
    }

    std::fs::write(&out_path, &content)?;

    // Ajouter au .gitignore si c'est .env.local
    if out_path.to_string_lossy() == ".env.local" {
        if Path::new(".gitignore").exists() {
            let gi = std::fs::read_to_string(".gitignore").unwrap_or_default();
            if !gi.contains(".env.local") {
                let mut gi = gi;
                if !gi.ends_with('\n') { gi.push('\n'); }
                gi.push_str(".env.local\n");
                let _ = std::fs::write(".gitignore", gi);
                println!("  {} .env.local ajouté au .gitignore", "✓".green());
            }
        }
    }

    println!(
        "{} {} variable(s) exportées vers {}",
        "✓".green().bold(), filtered.len(), out_path.display().to_string().cyan()
    );
    Ok(())
}

pub async fn run_env_push(
    input:   Option<PathBuf>,
    targets: Vec<String>,
    project: Option<String>,
    profile: Option<String>,
    yes:     bool,
) -> Result<()> {
    let creds  = vercel_store::require_credentials(profile.as_deref())?;
    let client = vc_client(&creds);
    let proj_id = resolve_project(project.as_deref(), &client).await?;

    // Détecter le fichier .env à utiliser
    let env_file = match input {
        Some(p) => p,
        None    => {
            // Priorité : .env.local > .env.development > .env
            for candidate in &[".env.local", ".env.development", ".env"] {
                let p = PathBuf::from(candidate);
                if p.exists() { break; } // sera utilisé ci-dessous
            }
            [".env.local", ".env.development", ".env"]
                .iter()
                .map(PathBuf::from)
                .find(|p| p.exists())
                .ok_or_else(|| anyhow::anyhow!(
                    "Aucun fichier .env trouvé. Précisez le chemin : iloc vercel env push <fichier>."
                ))?
        }
    };

    let content = std::fs::read_to_string(&env_file)
        .with_context(|| format!("Impossible de lire {}", env_file.display()))?;

    // Parser le fichier .env
    let mut vars: Vec<(String, String)> = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') { continue; }
        if let Some((k, v)) = line.split_once('=') {
            let k = k.trim().to_string();
            let v = v.trim().trim_matches('"').trim_matches('\'').to_string();
            if !k.is_empty() { vars.push((k, v)); }
        }
    }

    if vars.is_empty() {
        bail!("Aucune variable trouvée dans {}", env_file.display());
    }

    let tgts: Vec<&str> = if targets.is_empty() {
        vec!["production", "preview", "development"]
    } else {
        targets.iter().map(|s| s.as_str()).collect()
    };

    println!();
    println!(
        "  {} Pousser {} variable(s) depuis {} → projet {} [{}]",
        "→".cyan(), vars.len(), env_file.display(), proj_id.bold(), tgts.join(",").cyan()
    );
    for (k, _) in &vars { println!("    {}", k.dimmed()); }
    println!();

    if !confirm("Confirmer le push des variables ?", yes)? {
        println!("  Annulé."); return Ok(());
    }

    let sp = spinner("Push des variables…");
    let mut ok = 0usize;
    let mut ko = 0usize;
    for (k, v) in &vars {
        match client.upsert_env(&proj_id, k, v, &tgts, "encrypted", None).await {
            Ok(_)  => ok += 1,
            Err(e) => {
                eprintln!("  {} {}: {}", "⚠".yellow(), k, e);
                ko += 1;
            }
        }
    }
    sp.finish_and_clear();

    println!(
        "{} {} variable(s) poussée(s){}",
        "✓".green().bold(), ok,
        if ko > 0 { format!(", {} erreur(s)", ko).red().to_string() } else { String::new() }
    );
    Ok(())
}

// ═════════════════════════════════════════════════════════════
//  DOMAINES
// ═════════════════════════════════════════════════════════════

pub async fn run_domain_list(
    project: Option<String>,
    limit:   usize,
    profile: Option<String>,
) -> Result<()> {
    let creds  = vercel_store::require_credentials(profile.as_deref())?;
    let client = vc_client(&creds);

    if let Some(proj) = project {
        let proj_id  = client.get_project(&proj).await?.id;
        let sp       = spinner("Chargement des domaines du projet…");
        let domains  = client.list_project_domains(&proj_id).await?;
        sp.finish_and_clear();
        println!();
        println!("  {} domaine(s) — {}", domains.len(), proj_id.dimmed());
        println!();
        for d in &domains {
            let name = d["name"].as_str().unwrap_or("-");
            let verified = d["verified"].as_bool().unwrap_or(false);
            println!("  {} {} {}", "●".cyan(), name.bold(),
                if verified { "✓".green().to_string() } else { "(non vérifié)".yellow().to_string() }
            );
        }
    } else {
        let sp      = spinner("Chargement des domaines…");
        let domains = client.list_domains(limit).await?;
        sp.finish_and_clear();
        println!();
        println!("  {} domaine(s)", domains.len());
        println!();
        for d in &domains {
            let verified = d.verified.unwrap_or(false);
            println!("  {} {} {}", "●".cyan(), d.name.bold(),
                if verified { "✓".green().to_string() } else { "(non vérifié)".yellow().to_string() }
            );
            if let Some(exp) = d.expires_at {
                println!("    {} {}", "expiration:".dimmed(), format_ts(Some(exp)).dimmed());
            }
        }
    }
    println!();
    Ok(())
}

pub async fn run_domain_add(
    domain:     String,
    project:    Option<String>,
    git_branch: Option<String>,
    redirect:   Option<String>,
    profile:    Option<String>,
) -> Result<()> {
    let creds  = vercel_store::require_credentials(profile.as_deref())?;
    let client = vc_client(&creds);

    let sp = spinner(&format!("Ajout du domaine '{}'…", domain));
    if let Some(proj) = project {
        let proj_id = client.get_project(&proj).await?.id;
        client.add_project_domain(&proj_id, &domain, git_branch.as_deref(), redirect.as_deref()).await?;
    } else {
        client.add_domain(&domain).await?;
    }
    sp.finish_and_clear();

    println!("{} Domaine '{}' ajouté.", "✓".green().bold(), domain.bold());
    println!("  {} Configurez vos DNS selon les instructions de Vercel.", "ℹ".cyan());
    Ok(())
}

pub async fn run_domain_remove(
    domain:  String,
    project: Option<String>,
    profile: Option<String>,
    yes:     bool,
) -> Result<()> {
    if !confirm(&format!("Supprimer le domaine '{}' ?", domain), yes)? {
        println!("  Annulé."); return Ok(());
    }
    let creds  = vercel_store::require_credentials(profile.as_deref())?;
    let client = vc_client(&creds);

    let sp = spinner("Suppression…");
    if let Some(proj) = project {
        let proj_id = client.get_project(&proj).await?.id;
        client.remove_project_domain(&proj_id, &domain).await?;
    } else {
        client.delete_domain(&domain).await?;
    }
    sp.finish_and_clear();
    println!("{} Domaine '{}' supprimé.", "✓".green().bold(), domain.bold());
    Ok(())
}

pub async fn run_domain_inspect(
    domain:  String,
    project: Option<String>,
    profile: Option<String>,
) -> Result<()> {
    let creds  = vercel_store::require_credentials(profile.as_deref())?;
    let client = vc_client(&creds);

    let sp = spinner("Vérification du domaine…");
    if let Some(proj) = project {
        let proj_id = client.get_project(&proj).await?.id;
        let result  = client.verify_project_domain(&proj_id, &domain).await?;
        sp.finish_and_clear();
        let verified = result["verified"].as_bool().unwrap_or(false);
        if verified {
            println!("{} Domaine '{}' vérifié.", "✓".green().bold(), domain.bold());
        } else {
            println!("{} Domaine '{}' non vérifié.", "⚠".yellow(), domain.bold());
            if let Some(errors) = result["errors"].as_array() {
                for e in errors {
                    println!("  {} {}", "✗".red(), e["message"].as_str().unwrap_or("?"));
                }
            }
        }
    } else {
        let d = client.get_domain(&domain).await?;
        sp.finish_and_clear();
        println!();
        println!("  {} {}", "domaine:".dimmed(), d.name.bold());
        println!("  {} {}", "vérifié:".dimmed(),
            if d.verified.unwrap_or(false) { "oui".green().to_string() } else { "non".red().to_string() }
        );
        if let Some(ns) = &d.nameservers {
            println!("  {} {}", "nameservers:".dimmed(), ns.join(", ").dimmed());
        }
        if let Some(ins) = &d.intended_ns {
            println!("  {} {}", "ns attendus:".dimmed(), ins.join(", ").cyan());
        }
    }
    println!();
    Ok(())
}

pub async fn run_domain_check(domain: String, profile: Option<String>) -> Result<()> {
    let creds  = vercel_store::require_credentials(profile.as_deref())?;
    let client = vc_client(&creds);

    let sp     = spinner(&format!("Vérification de la disponibilité de '{}'…", domain));
    let result = client.check_domain(&domain).await?;
    sp.finish_and_clear();

    let available = result["available"].as_bool().unwrap_or(false);
    let price     = result["price"].as_f64();
    if available {
        println!(
            "{} '{}' est disponible{}",
            "✓".green().bold(), domain.bold(),
            price.map(|p| format!(" (~${}/an)", p)).unwrap_or_default().dimmed().to_string()
        );
    } else {
        println!("{} '{}' n'est pas disponible.", "✗".red(), domain.bold());
    }
    Ok(())
}

pub async fn run_dns_list(domain: String, profile: Option<String>) -> Result<()> {
    let creds   = vercel_store::require_credentials(profile.as_deref())?;
    let client  = vc_client(&creds);
    let sp      = spinner("Chargement des enregistrements DNS…");
    let records = client.list_dns_records(&domain).await?;
    sp.finish_and_clear();

    if records.is_empty() {
        println!("{}", "  Aucun enregistrement DNS.".yellow());
        return Ok(());
    }
    println!();
    println!("  {} enregistrement(s) DNS — {}", records.len(), domain.bold());
    println!();
    for r in &records {
        let id = r.id.as_deref().unwrap_or("-");
        println!(
            "  {} {} {} {} {}",
            "●".cyan(), r.rec_type.bold(), r.name.cyan(), r.value.dimmed(),
            format!("[{}]", id).dimmed()
        );
    }
    println!();
    Ok(())
}

pub async fn run_dns_add(
    domain:   String,
    name:     String,
    rec_type: String,
    value:    String,
    ttl:      Option<u64>,
    priority: Option<u64>,
    profile:  Option<String>,
) -> Result<()> {
    let creds  = vercel_store::require_credentials(profile.as_deref())?;
    let client = vc_client(&creds);
    let sp     = spinner("Ajout de l'enregistrement DNS…");
    let r      = client.add_dns_record(&domain, &name, &rec_type, &value, ttl, priority).await?;
    sp.finish_and_clear();
    println!(
        "{} {} {} → {} ajouté (id: {})",
        "✓".green().bold(), rec_type.bold(), name.cyan(), value.dimmed(),
        r.id.as_deref().unwrap_or("-").dimmed()
    );
    Ok(())
}

pub async fn run_dns_remove(
    domain:    String,
    record_id: String,
    profile:   Option<String>,
    yes:       bool,
) -> Result<()> {
    if !confirm(&format!("Supprimer l'enregistrement DNS '{}' ?", record_id), yes)? {
        println!("  Annulé."); return Ok(());
    }
    let creds  = vercel_store::require_credentials(profile.as_deref())?;
    let client = vc_client(&creds);
    let sp     = spinner("Suppression…");
    client.delete_dns_record(&domain, &record_id).await?;
    sp.finish_and_clear();
    println!("{} Enregistrement DNS '{}' supprimé.", "✓".green().bold(), record_id.dimmed());
    Ok(())
}

// ═════════════════════════════════════════════════════════════
//  ALIASES
// ═════════════════════════════════════════════════════════════

pub async fn run_alias_list(
    project: Option<String>,
    limit:   usize,
    profile: Option<String>,
) -> Result<()> {
    let creds  = vercel_store::require_credentials(profile.as_deref())?;
    let client = vc_client(&creds);
    let proj_id = if project.is_some() {
        resolve_project(project.as_deref(), &client).await.ok()
    } else {
        read_project_link().map(|(_, p)| p)
    };
    let sp      = spinner("Chargement des aliases…");
    let aliases = client.list_aliases(proj_id.as_deref(), limit).await?;
    sp.finish_and_clear();

    if aliases.is_empty() {
        println!("{}", "  Aucun alias.".yellow());
        return Ok(());
    }
    println!();
    println!("  {} alias", aliases.len());
    println!();
    for a in &aliases {
        let dep_url = a.deployment.as_ref()
            .and_then(|d| d.url.as_deref())
            .unwrap_or("-");
        println!("  {} {} → {}", "●".cyan(), a.alias.bold(), dep_url.dimmed());
    }
    println!();
    Ok(())
}

pub async fn run_alias_assign(
    deployment_id: String,
    alias:         String,
    redirect:      Option<String>,
    profile:       Option<String>,
) -> Result<()> {
    let creds  = vercel_store::require_credentials(profile.as_deref())?;
    let client = vc_client(&creds);
    let sp     = spinner(&format!("Assignation de l'alias '{}'…", alias));
    client.assign_alias(&deployment_id, &alias, redirect.as_deref()).await?;
    sp.finish_and_clear();
    println!(
        "{} Alias '{}' → '{}'.",
        "✓".green().bold(), alias.bold(), deployment_id.dimmed()
    );
    Ok(())
}

pub async fn run_alias_delete(
    alias:   String,
    profile: Option<String>,
    yes:     bool,
) -> Result<()> {
    if !confirm(&format!("Supprimer l'alias '{}' ?", alias), yes)? {
        println!("  Annulé."); return Ok(());
    }
    let creds  = vercel_store::require_credentials(profile.as_deref())?;
    let client = vc_client(&creds);
    let sp     = spinner("Suppression…");
    client.delete_alias(&alias).await?;
    sp.finish_and_clear();
    println!("{} Alias '{}' supprimé.", "✓".green().bold(), alias.bold());
    Ok(())
}

// ═════════════════════════════════════════════════════════════
//  SECRETS (legacy Vercel secrets)
// ═════════════════════════════════════════════════════════════

pub async fn run_secret_list(profile: Option<String>) -> Result<()> {
    let creds   = vercel_store::require_credentials(profile.as_deref())?;
    let client  = vc_client(&creds);
    let sp      = spinner("Chargement des secrets…");
    let secrets = client.list_secrets().await?;
    sp.finish_and_clear();

    if secrets.is_empty() {
        println!("{}", "  Aucun secret.".yellow());
        println!("  {} Préférez les env vars chiffrées : {} ",
            "ℹ".cyan(), "iloc vercel env add".cyan()
        );
        return Ok(());
    }
    println!();
    for s in &secrets {
        println!("  {} @{} ({})", "🔑".to_string(), s.name.bold(), s.uid.dimmed());
    }
    println!();
    Ok(())
}

pub async fn run_secret_add(
    name:    String,
    value:   Option<String>,
    profile: Option<String>,
) -> Result<()> {
    let val = match value {
        Some(v) => v,
        None    => rpassword::prompt_password(&format!("  Valeur de '@{}' (masquée): ", name))?,
    };
    let creds  = vercel_store::require_credentials(profile.as_deref())?;
    let client = vc_client(&creds);
    let sp     = spinner("Création du secret…");
    let s      = client.create_secret(&name, &val, true).await?;
    sp.finish_and_clear();
    println!("{} Secret '@{}' créé (uid: {}).", "✓".green().bold(), name.bold(), s.uid.dimmed());
    Ok(())
}

pub async fn run_secret_rename(
    name:     String,
    new_name: String,
    profile:  Option<String>,
) -> Result<()> {
    let creds  = vercel_store::require_credentials(profile.as_deref())?;
    let client = vc_client(&creds);
    let sp     = spinner("Renommage du secret…");
    client.rename_secret(&name, &new_name).await?;
    sp.finish_and_clear();
    println!("{} Secret '@{}' → '@{}'.", "✓".green().bold(), name.bold(), new_name.bold());
    Ok(())
}

pub async fn run_secret_delete(
    name:    String,
    profile: Option<String>,
    yes:     bool,
) -> Result<()> {
    if !confirm(&format!("Supprimer le secret '@{}' ?", name), yes)? {
        println!("  Annulé."); return Ok(());
    }
    let creds  = vercel_store::require_credentials(profile.as_deref())?;
    let client = vc_client(&creds);
    let sp     = spinner("Suppression…");
    client.delete_secret(&name).await?;
    sp.finish_and_clear();
    println!("{} Secret '@{}' supprimé.", "✓".green().bold(), name.bold());
    Ok(())
}

// ═════════════════════════════════════════════════════════════
//  EDGE CONFIG
// ═════════════════════════════════════════════════════════════

pub async fn run_edge_list(profile: Option<String>) -> Result<()> {
    let creds   = vercel_store::require_credentials(profile.as_deref())?;
    let client  = vc_client(&creds);
    let sp      = spinner("Chargement des edge configs…");
    let configs = client.list_edge_configs().await?;
    sp.finish_and_clear();

    if configs.is_empty() {
        println!("{}", "  Aucun Edge Config.".yellow());
        return Ok(());
    }
    println!();
    for c in &configs {
        let slug  = c.slug.as_deref().unwrap_or("-");
        let count = c.item_count.unwrap_or(0);
        println!("  {} {} ({}) — {} item(s)", "●".cyan(), slug.bold(), c.id.dimmed(), count);
    }
    println!();
    Ok(())
}

pub async fn run_edge_create(slug: String, profile: Option<String>) -> Result<()> {
    let creds  = vercel_store::require_credentials(profile.as_deref())?;
    let client = vc_client(&creds);
    let sp     = spinner("Création de l'Edge Config…");
    let ec     = client.create_edge_config(&slug).await?;
    sp.finish_and_clear();
    println!("{} Edge Config '{}' créé (id: {}).", "✓".green().bold(), slug.bold(), ec.id.dimmed());
    Ok(())
}

pub async fn run_edge_items(id: String, profile: Option<String>) -> Result<()> {
    let creds  = vercel_store::require_credentials(profile.as_deref())?;
    let client = vc_client(&creds);
    let sp     = spinner("Chargement des items…");
    let items  = client.get_edge_config_items(&id).await?;
    sp.finish_and_clear();

    if items.is_empty() {
        println!("{}", "  Aucun item.".yellow());
        return Ok(());
    }
    println!();
    for item in &items {
        let key = item["key"].as_str().unwrap_or("?");
        println!("  {} {} → {}", "●".cyan(), key.bold(), item["value"]);
    }
    println!();
    Ok(())
}

pub async fn run_edge_update(
    id:      String,
    items:   Vec<String>,  // "key=value" ou "key:delete"
    profile: Option<String>,
) -> Result<()> {
    let creds  = vercel_store::require_credentials(profile.as_deref())?;
    let client = vc_client(&creds);

    let ops: Vec<(String, serde_json::Value, &str)> = items.iter().filter_map(|item| {
        if let Some(key) = item.strip_suffix(":delete") {
            Some((key.to_string(), serde_json::Value::Null, "delete"))
        } else if let Some((k, v)) = item.split_once('=') {
            let val = serde_json::json!(v);
            Some((k.to_string(), val, "create"))
        } else {
            None
        }
    }).collect();

    if ops.is_empty() {
        bail!("Format attendu : KEY=value ou KEY:delete");
    }

    let sp = spinner("Mise à jour de l'Edge Config…");
    client.update_edge_config_items(&id, &ops).await?;
    sp.finish_and_clear();
    println!("{} Edge Config '{}' mis à jour ({} opération(s)).", "✓".green().bold(), id.dimmed(), ops.len());
    Ok(())
}

pub async fn run_edge_delete(id: String, profile: Option<String>, yes: bool) -> Result<()> {
    if !confirm(&format!("Supprimer l'Edge Config '{}' ?", id), yes)? {
        println!("  Annulé."); return Ok(());
    }
    let creds  = vercel_store::require_credentials(profile.as_deref())?;
    let client = vc_client(&creds);
    let sp     = spinner("Suppression…");
    client.delete_edge_config(&id).await?;
    sp.finish_and_clear();
    println!("{} Edge Config '{}' supprimé.", "✓".green().bold(), id.dimmed());
    Ok(())
}

// ═════════════════════════════════════════════════════════════
//  WEBHOOKS
// ═════════════════════════════════════════════════════════════

pub async fn run_webhook_list(profile: Option<String>) -> Result<()> {
    let creds    = vercel_store::require_credentials(profile.as_deref())?;
    let client   = vc_client(&creds);
    let sp       = spinner("Chargement des webhooks…");
    let webhooks = client.list_webhooks().await?;
    sp.finish_and_clear();

    if webhooks.is_empty() {
        println!("{}", "  Aucun webhook.".yellow());
        return Ok(());
    }
    println!();
    for w in &webhooks {
        println!("  {} {} [{}]", "🔗".to_string(), w.url.bold(), w.id.dimmed());
        println!("    events: {}", w.events.join(", ").dimmed());
    }
    println!();
    Ok(())
}

pub async fn run_webhook_create(
    url:     String,
    events:  Vec<String>,
    profile: Option<String>,
) -> Result<()> {
    let creds  = vercel_store::require_credentials(profile.as_deref())?;
    let client = vc_client(&creds);

    let evts: Vec<&str> = if events.is_empty() {
        vec!["deployment.created", "deployment.ready", "deployment.error"]
    } else {
        events.iter().map(|s| s.as_str()).collect()
    };

    let sp = spinner("Création du webhook…");
    let w  = client.create_webhook(&url, &evts).await?;
    sp.finish_and_clear();
    println!("{} Webhook créé (id: {}).", "✓".green().bold(), w.id.dimmed());
    println!("  {} {}", "url:".dimmed(), w.url.cyan());
    println!("  {} {}", "events:".dimmed(), w.events.join(", ").dimmed());
    Ok(())
}

pub async fn run_webhook_delete(id: String, profile: Option<String>, yes: bool) -> Result<()> {
    if !confirm(&format!("Supprimer le webhook '{}' ?", id), yes)? {
        println!("  Annulé."); return Ok(());
    }
    let creds  = vercel_store::require_credentials(profile.as_deref())?;
    let client = vc_client(&creds);
    let sp     = spinner("Suppression…");
    client.delete_webhook(&id).await?;
    sp.finish_and_clear();
    println!("{} Webhook '{}' supprimé.", "✓".green().bold(), id.dimmed());
    Ok(())
}

// ═════════════════════════════════════════════════════════════
//  CHECKS (CI gates)
// ═════════════════════════════════════════════════════════════

pub async fn run_check_list(deployment_id: String, profile: Option<String>) -> Result<()> {
    let creds  = vercel_store::require_credentials(profile.as_deref())?;
    let client = vc_client(&creds);
    let sp     = spinner("Chargement des checks…");
    let checks = client.list_checks(&deployment_id).await?;
    sp.finish_and_clear();

    if checks.is_empty() {
        println!("{}", "  Aucun check.".yellow());
        return Ok(());
    }
    println!();
    for c in &checks {
        let icon = match c.conclusion.as_deref() {
            Some("succeeded") => "✓".green().to_string(),
            Some("failed")    => "✗".red().to_string(),
            Some("canceled")  => "○".dimmed().to_string(),
            None              => "↻".cyan().to_string(),
            _                 => "?".dimmed().to_string(),
        };
        println!("  {} {} [{}]", icon, c.name.bold(), c.status.dimmed());
    }
    println!();
    Ok(())
}

pub async fn run_check_create(
    deployment_id: String,
    name:          String,
    detached:      bool,
    blocking:      bool,
    profile:       Option<String>,
) -> Result<()> {
    let creds  = vercel_store::require_credentials(profile.as_deref())?;
    let client = vc_client(&creds);
    let sp     = spinner("Création du check…");
    let c      = client.create_check(&deployment_id, &name, detached, blocking).await?;
    sp.finish_and_clear();
    println!("{} Check '{}' créé (id: {}).", "✓".green().bold(), name.bold(), c.id.dimmed());
    Ok(())
}

pub async fn run_check_update(
    deployment_id: String,
    check_id:      String,
    status:        String,
    conclusion:    Option<String>,
    profile:       Option<String>,
) -> Result<()> {
    let creds  = vercel_store::require_credentials(profile.as_deref())?;
    let client = vc_client(&creds);
    let sp     = spinner("Mise à jour du check…");
    client.update_check(&deployment_id, &check_id, &status, conclusion.as_deref(), None).await?;
    sp.finish_and_clear();
    println!(
        "{} Check '{}' → {} {}",
        "✓".green().bold(), check_id.dimmed(), status.bold(),
        conclusion.as_deref().unwrap_or("").cyan().to_string()
    );
    Ok(())
}

// ═════════════════════════════════════════════════════════════
//  TEAMS
// ═════════════════════════════════════════════════════════════

pub async fn run_team_list(profile: Option<String>) -> Result<()> {
    let creds  = vercel_store::require_credentials(profile.as_deref())?;
    let client = vc_client(&creds);
    let sp     = spinner("Chargement des teams…");
    let teams  = client.list_teams().await?;
    sp.finish_and_clear();

    if teams.is_empty() {
        println!("{}", "  Aucune team.".yellow());
        return Ok(());
    }
    let active = creds.default_team.as_deref();
    println!();
    println!("  {} team(s)", teams.len());
    println!();
    for t in &teams {
        let marker = if active == Some(t.slug.as_str()) { "●".green() } else { "○".dimmed() };
        let role   = t.membership.as_ref()
            .and_then(|m| m.role.as_deref())
            .unwrap_or("-");
        println!("  {} {} ({}) — {}", marker, t.name.bold(), t.slug.dimmed(), role.cyan());
    }
    println!();
    Ok(())
}

pub async fn run_team_switch(slug: String, profile: Option<String>) -> Result<()> {
    let creds  = vercel_store::require_credentials(profile.as_deref())?;
    let client = vc_client(&creds);

    let sp    = spinner("Résolution de la team…");
    let teams = client.list_teams().await?;
    sp.finish_and_clear();

    let team = teams.iter().find(|t| t.slug == slug)
        .ok_or_else(|| anyhow::anyhow!("Team '{}' introuvable.", slug))?;

    let mut cfg = vercel_store::load_profiles()?;
    if let Some(p) = cfg.profiles.iter_mut().find(|p| p.name == creds.profile_name) {
        p.default_team    = Some(team.slug.clone());
        p.default_team_id = Some(team.id.clone());
    }
    vercel_store::save_profiles(&cfg)?;

    println!(
        "{} Team active : {} ({})",
        "✓".green().bold(), team.name.bold(), team.slug.dimmed()
    );
    Ok(())
}
