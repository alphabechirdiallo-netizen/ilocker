// ============================================================
//  commands/github.rs — iloc github <sous-commande>
//
//  Couvre >90% des besoins quotidiens d'un développeur GitHub :
//
//  CONNEXION
//    iloc connect github                  assistant de connexion OAuth PAT
//    iloc github list                     liste les profils
//    iloc github use <nom>                change le profil actif
//    iloc github remove <nom>             déconnecte un profil
//    iloc github status                   affiche l'utilisateur connecté + rate limit
//
//  REPOS
//    iloc github repo create              crée un repo
//    iloc github repo list                liste vos repos
//    iloc github repo view [nom]          détails d'un repo
//    iloc github repo clone <url>         clone (via git) + init ilocker
//    iloc github repo delete              supprime un repo
//    iloc github repo archive             archive un repo
//    iloc github repo fork <owner/repo>   fork un repo
//    iloc github repo transfer            transfert de propriété
//    iloc github repo topics              gère les topics
//    iloc github repo rename              renomme un repo
//    iloc github repo visibility          change public/privé
//
//  BRANCHES
//    iloc github branch list
//    iloc github branch create <nom>
//    iloc github branch delete <nom>
//    iloc github branch rename <old> <new>
//    iloc github branch protect <nom>
//    iloc github branch default <nom>
//
//  ISSUES
//    iloc github issue list
//    iloc github issue create
//    iloc github issue view <num>
//    iloc github issue close <num>
//    iloc github issue reopen <num>
//    iloc github issue comment <num>
//    iloc github issue assign <num>
//    iloc github issue label <num>
//    iloc github issue lock <num>
//
//  PULL REQUESTS
//    iloc github pr list
//    iloc github pr create
//    iloc github pr view <num>
//    iloc github pr merge <num>
//    iloc github pr review <num>
//    iloc github pr checkout <num>
//    iloc github pr close <num>
//    iloc github pr ready <num>
//    iloc github pr update-branch <num>
//
//  RELEASES
//    iloc github release list
//    iloc github release create
//    iloc github release view [tag]
//    iloc github release delete <tag>
//    iloc github release upload <tag> <file>
//
//  ACTIONS / CI
//    iloc github actions list
//    iloc github actions run <workflow>
//    iloc github actions status
//    iloc github actions cancel <run-id>
//    iloc github actions rerun <run-id>
//    iloc github actions logs <run-id>
//
//  SECRETS
//    iloc github secret list
//    iloc github secret set <name>
//    iloc github secret delete <name>
//
//  COLLABORATEURS
//    iloc github collab list
//    iloc github collab add <user>
//    iloc github collab remove <user>
//
//  WEBHOOKS
//    iloc github webhook list
//    iloc github webhook create
//    iloc github webhook delete <id>
//    iloc github webhook ping <id>
//
//  SEARCH
//    iloc github search repos <query>
//    iloc github search issues <query>
//
// ============================================================

use crate::github_client::{
    GitHubClient, GhRepo, GhBranch, GhIssue, GhPullRequest, GhRelease,
    GhWorkflow, GhWorkflowRun,
};
use crate::github_store::{self, GitHubCredentials, GitHubProfile};
use anyhow::{bail, Context, Result};
// base64::encode/decode (fonctions libres) sont dépréciées depuis base64 0.21 —
// migré vers l'API Engine ; STANDARD reproduit exactement le même alphabet et
// padding que l'ancien comportement par défaut (requis par l'API GitHub Secrets).
use base64::Engine as _;
use colored::Colorize;
use indicatif::{ProgressBar, ProgressStyle};
use std::path::PathBuf;

// ── Helpers partagés ──────────────────────────────────────────

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
    // Bypass programmatique pour les IA et les scripts CI
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

/// Résout "owner/repo" depuis args ou depuis git remote origin dans le cwd.
fn resolve_owner_repo(
    owner_repo: Option<&str>,
    profile:    &GitHubCredentials,
) -> Result<(String, String)> {
    if let Some(or) = owner_repo {
        let parts: Vec<&str> = or.splitn(2, '/').collect();
        if parts.len() == 2 {
            return Ok((parts[0].to_string(), parts[1].to_string()));
        }
        // Juste "repo" → owner = login de l'utilisateur
        return Ok((profile.login.clone(), or.to_string()));
    }
    // Lire depuis git remote origin
    detect_repo_from_git().context(
        "Impossible de détecter le repo. Précisez owner/repo ou lancez la commande depuis le dossier git."
    )
}

fn detect_repo_from_git() -> Result<(String, String)> {
    let output = std::process::Command::new("git")
        .args(["remote", "get-url", "origin"])
        .output()
        .context("git non disponible")?;
    if !output.status.success() {
        bail!("Pas de remote 'origin' trouvé.");
    }
    let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
    parse_github_url(&url)
}

fn parse_github_url(url: &str) -> Result<(String, String)> {
    // SSH: git@github.com:owner/repo.git
    // HTTPS: https://github.com/owner/repo.git
    let cleaned = url
        .trim_end_matches(".git")
        .trim_end_matches('/');
    if let Some(rest) = cleaned.strip_prefix("git@github.com:") {
        let parts: Vec<&str> = rest.splitn(2, '/').collect();
        if parts.len() == 2 {
            return Ok((parts[0].to_string(), parts[1].to_string()));
        }
    }
    for prefix in &["https://github.com/", "http://github.com/"] {
        if let Some(rest) = cleaned.strip_prefix(prefix) {
            let parts: Vec<&str> = rest.splitn(2, '/').collect();
            if parts.len() == 2 {
                return Ok((parts[0].to_string(), parts[1].to_string()));
            }
        }
    }
    bail!("URL GitHub non reconnue : {}", url);
}

fn gh_client(creds: &GitHubCredentials) -> GitHubClient {
    GitHubClient::new(&creds.token, &creds.api_url)
}

// ── Affichage commun ──────────────────────────────────────────

fn print_repo(repo: &GhRepo) {
    let visibility = if repo.private { "privé".red().to_string() } else { "public".green().to_string() };
    let archived   = if repo.archived { " [archivé]".yellow().to_string() } else { String::new() };
    println!(
        "  {} {} {}{}",
        "●".cyan(),
        repo.full_name.bold(),
        visibility,
        archived
    );
    if let Some(d) = &repo.description {
        if !d.is_empty() { println!("    {}", d.dimmed()); }
    }
    println!(
        "    {} {} · {} {} · {} {} · {} {}",
        "⭐".to_string(), repo.stargazers_count.unwrap_or(0),
        "🍴".to_string(), repo.forks_count.unwrap_or(0),
        "🐛".to_string(), repo.open_issues_count.unwrap_or(0),
        "🌿".to_string(), repo.default_branch
    );
    println!("    {}", repo.html_url.dimmed());
}

fn print_issue(issue: &GhIssue) {
    let state = if issue.state == "open" { "●".green() } else { "●".red() };
    let labels: Vec<String> = issue.labels.iter().map(|l| l.name.clone()).collect();
    println!(
        "  {} #{} {}",
        state, issue.number, issue.title.bold()
    );
    if !labels.is_empty() {
        println!("    {}", labels.join(", ").dimmed());
    }
    println!("    {} · {}", issue.user.login.cyan(), &issue.created_at[..10].dimmed());
}

fn print_pr(pr: &GhPullRequest) {
    let state = match pr.state.as_str() {
        "open"   if pr.draft => "◐".yellow(),
        "open"               => "●".green(),
        "closed" if pr.merged.unwrap_or(false) => "✓".magenta(),
        _                    => "●".red(),
    };
    println!(
        "  {} #{} {} {} → {}",
        state, pr.number, pr.title.bold(),
        pr.head.ref_.cyan(), pr.base.ref_.dimmed()
    );
    println!("    {} · {}", pr.user.login.cyan(), &pr.created_at[..10].dimmed());
}

fn print_branch(b: &GhBranch) {
    let prot = if b.protected { " [protégée]".yellow().to_string() } else { String::new() };
    println!("  {} {}{} — {}", "🌿".to_string(), b.name.bold(), prot, &b.commit.sha[..8].dimmed());
}

fn print_release(r: &GhRelease) {
    let draft = if r.draft { " [draft]".yellow().to_string() } else { String::new() };
    let pre   = if r.prerelease { " [pre-release]".cyan().to_string() } else { String::new() };
    println!(
        "  {} {}{}{} — {} asset(s)",
        "🏷".to_string(), r.tag_name.bold(), draft, pre, r.assets.len()
    );
    if let Some(n) = &r.name { println!("    {}", n.dimmed()); }
    println!("    {}", r.html_url.dimmed());
}

fn print_workflow(w: &GhWorkflow) {
    let state = match w.state.as_str() {
        "active"   => "●".green(),
        "disabled" => "●".red(),
        _          => "●".dimmed(),
    };
    println!("  {} {} — {}", state, w.name.bold(), w.path.dimmed());
}

fn print_run(r: &GhWorkflowRun) {
    let icon = match r.conclusion.as_deref() {
        Some("success")  => "✓".green(),
        Some("failure")  => "✗".red(),
        Some("cancelled")=> "○".yellow(),
        Some("skipped")  => "○".dimmed(),
        None             => "↻".cyan(),
        _                => "?".dimmed(),
    };
    println!(
        "  {} #{} {} — {} {}",
        icon,
        r.run_number,
        r.name.as_deref().unwrap_or("Run").bold(),
        r.status.dimmed(),
        r.head_branch.as_deref().unwrap_or("").cyan()
    );
    println!("    {} · {}", &r.created_at[..16].dimmed(), r.html_url.dimmed());
}

// ═════════════════════════════════════════════════════════════
//  CONNEXION
// ═════════════════════════════════════════════════════════════

pub async fn run_connect(
    profile_name: Option<String>,
    token_arg: Option<String>,
    api_url_arg: Option<String>,
) -> Result<()> {
    println!();
    println!("{}", "  ilocker — Connecter un compte GitHub".bold());
    println!();
    println!(
        "  {}",
        "Votre token est stocké dans le trousseau système (Keychain / Credential Manager / kernel keyring) — jamais en clair sur disque.".dimmed()
    );
    println!();
    println!("  {}", "Comment créer un Personal Access Token (PAT) :".dimmed());
    println!("    1. https://github.com/settings/tokens/new?scopes=repo,workflow,read:org,read:user,gist");
    println!("    2. Note: ilocker · Expiration: 90 jours (recommandé)");
    println!("    3. Scopes requis : {}", "repo, workflow, read:org, read:user".cyan());
    println!("    4. Copiez le token et collez-le ci-dessous");
    println!();

    // Mode non-interactif : dès que --token est fourni, on ne bloque plus
    // jamais sur une lecture stdin (utile pour CI/scripts — un run_connect
    // à moitié non-interactif qui bloque quand même sur un AUTRE prompt
    // serait pire qu'inutile : il planterait silencieusement en pipeline).
    let non_interactive = token_arg.is_some();

    // Nom du profil
    let existing = github_store::list_profiles()?;
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

    // API URL (GitHub Enterprise ou github.com)
    let api_url = match api_url_arg {
        Some(u) => u,
        None if non_interactive => "https://api.github.com".to_string(),
        None => {
            println!();
            println!("  {} Pour GitHub.com, appuyez sur Entrée directement.", "ℹ".cyan());
            prompt_default("  URL de l'API GitHub", "https://api.github.com")?
        }
    };

    // Token
    println!();
    let token = match token_arg {
        Some(t) => {
            println!("  {} Token fourni via --token", "ℹ".cyan());
            t
        }
        None => rpassword::prompt_password("  Personal Access Token (masqué): ")
            .context("Impossible de lire le token (non-interactif ? utilisez --token)")?,
    };
    if token.is_empty() {
        bail!("Le token ne peut pas être vide.");
    }

    // Validation du token
    println!();
    let sp = spinner("Validation du token…");
    let client = GitHubClient::new(&token, &api_url);
    let user = client.get_authenticated_user().await.map_err(|e| {
        sp.finish_and_clear();
        anyhow::anyhow!("Token invalide ou inaccessible : {}", e)
    })?;
    sp.finish_and_clear();
    println!("  {} connecté en tant que {}", "✓".green(), user.login.bold().cyan());

    // Détection des scopes (via les headers — on ne peut pas les lire ici
    // mais on les affiche comme "vérifiés" si le GET /user a réussi)
    // Org par défaut
    println!();
    println!("  {} {}", "login:".dimmed(), user.login.bold());
    let orgs = client.list_user_orgs().await.unwrap_or_default();
    if !orgs.is_empty() {
        println!("  {} {}", "organisations:".dimmed(),
            orgs.iter().map(|o| o.login.as_str()).collect::<Vec<_>>().join(", ").cyan()
        );
    }

    let default_org = if orgs.is_empty() {
        None
    } else if non_interactive {
        None
    } else {
        println!();
        let ans = prompt_default(
            &format!("  Org par défaut pour `iloc github repo create`"),
            &user.login,
        )?;
        if ans == user.login { None } else { Some(ans) }
    };

    // Sauvegarde
    let account = format!("{}-{}", name, uuid::Uuid::new_v4().to_string().replace('-', ""));
    let profile = GitHubProfile {
        name:         name.clone(),
        login:        user.login.clone(),
        default_org:  default_org.clone(),
        scopes:       vec!["repo".to_string(), "workflow".to_string()],
        api_url:      api_url.clone(),
        account:      account.clone(),
        connected_at: chrono::Utc::now().to_rfc3339(),
    };
    github_store::upsert_profile(profile, existing.profiles.is_empty())?;
    github_store::save_token(&account, &token)?;

    println!();
    println!("{} Profil '{}' connecté ({})", "✓".green().bold(), name.bold(), user.login.cyan());
    if let Some(org) = default_org {
        println!("  {} {}", "org par défaut:".dimmed(), org.cyan());
    }
    println!();
    println!("  Essayez :");
    println!("    {} — lister vos repos", "iloc github repo list".cyan());
    println!("    {} — créer un repo", "iloc github repo create".cyan());
    println!();
    Ok(())
}

pub fn run_list_profiles() -> Result<()> {
    let cfg = github_store::list_profiles()?;
    println!();
    if cfg.profiles.is_empty() {
        println!("{}", "  Aucun compte GitHub configuré.".yellow());
        println!("  Lancez {} pour connecter votre compte.", "iloc connect github".cyan());
        println!();
        return Ok(());
    }
    println!("{}", "  Comptes GitHub connectés".bold());
    for p in &cfg.profiles {
        let active = cfg.active.as_deref() == Some(p.name.as_str());
        let marker = if active { "●".green() } else { "○".dimmed() };
        let org    = p.default_org.as_deref().unwrap_or("-");
        println!(
            "  {} {} — {} — org: {} — {}",
            marker, p.name.bold(), p.login.cyan(), org, &p.connected_at[..10].dimmed()
        );
    }
    println!();
    println!("{}", "  `iloc github use <nom>` pour changer le compte actif.".dimmed());
    println!();
    Ok(())
}

pub fn run_use_profile(name: String) -> Result<()> {
    github_store::set_active(&name)?;
    println!("{} compte GitHub actif : {}", "✓".green().bold(), name.bold());
    Ok(())
}

pub fn run_remove_profile(name: String, yes: bool) -> Result<()> {
    if !confirm(&format!("Déconnecter le profil '{}' ? (token supprimé du trousseau)", name), yes)? {
        println!("  Annulé.");
        return Ok(());
    }
    if github_store::remove_profile(&name)? {
        println!("{} profil '{}' déconnecté.", "✓".green().bold(), name);
    } else {
        println!("{} aucun profil nommé '{}'.", "⚠".yellow(), name);
    }
    Ok(())
}

pub async fn run_status(profile: Option<String>) -> Result<()> {
    let creds  = github_store::require_credentials(profile.as_deref())?;
    let client = gh_client(&creds);
    let sp     = spinner("Connexion à GitHub…");
    let user   = client.get_authenticated_user().await?;
    sp.finish_and_clear();
    println!();
    println!("{}", "  Statut GitHub".bold());
    println!("  {} {}", "compte:".dimmed(), user.login.bold().cyan());
    println!("  {} {}", "profil:".dimmed(), creds.profile_name);
    if let Some(org) = &creds.default_org {
        println!("  {} {}", "org défaut:".dimmed(), org.cyan());
    }
    println!("  {} {}", "API:".dimmed(), creds.api_url.dimmed());
    println!("  {} {}", "token:".dimmed(), "valide ✓".green());
    println!();
    Ok(())
}

// ═════════════════════════════════════════════════════════════
//  REPOS
// ═════════════════════════════════════════════════════════════

pub async fn run_repo_create(
    name:        Option<String>,
    description: Option<String>,
    private:     bool,
    public:      bool,
    org:         Option<String>,
    auto_init:   bool,
    topics:      Vec<String>,
    license:     Option<String>,
    gitignore:   Option<String>,
    profile:     Option<String>,
    yes:         bool,
) -> Result<()> {
    let creds = github_store::require_credentials(profile.as_deref())?;

    println!();
    println!("{}", "  ilocker — Créer un repo GitHub".bold());
    println!();

    // Collecte interactive si args manquants
    let repo_name = match name {
        Some(n) => n,
        None if yes => {
            std::env::current_dir()
                .ok()
                .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
                .unwrap_or_else(|| "mon-projet".to_string())
        }
        None    => {
            let cwd_name = std::env::current_dir()
                .ok()
                .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
                .unwrap_or_else(|| "mon-projet".to_string());
            prompt_default("  Nom du repo", &cwd_name)?
        }
    };
    if repo_name.is_empty() { bail!("Le nom du repo ne peut pas être vide."); }

    let desc = match description {
        Some(d) => Some(d),
        None if yes => None,
        None    => {
            let d = prompt("  Description (optionnelle): ")?;
            if d.is_empty() { None } else { Some(d) }
        }
    };

    let is_private = if public { false } else if private { true } else if yes {
        true // défaut sûr en mode non-interactif : privé plutôt que public par erreur
    } else {
        let ans = prompt_default("  Visibilité", "privé (p) / public (P)")?;
        !ans.starts_with('P') && !ans.eq_ignore_ascii_case("public")
    };

    // Owner : org ou perso
    let owner_org = match org {
        Some(o) => Some(o),
        None    => creds.default_org.clone(),
    };
    let owner_display = owner_org.as_deref().unwrap_or(&creds.login);

    // Résumé avant création
    println!();
    println!("  {} créer {}/{}", "→".cyan(), owner_display.bold(), repo_name.bold());
    println!("  {} {}", "visibilité:".dimmed(),
        if is_private { "privé".red().to_string() } else { "public".green().to_string() }
    );
    if let Some(d) = &desc { println!("  {} {}", "description:".dimmed(), d); }
    if !topics.is_empty()  { println!("  {} {}", "topics:".dimmed(), topics.join(", ")); }

    if !confirm("Confirmer la création ?", yes)? {
        println!("  Annulé.");
        return Ok(());
    }

    println!();
    let sp = spinner("Création du repo sur GitHub…");
    let client = gh_client(&creds);
    let repo = client.create_repo(
        &repo_name,
        desc.as_deref(),
        is_private,
        auto_init,
        owner_org.as_deref(),
        None,
        license.as_deref(),
        gitignore.as_deref(),
    ).await?;
    sp.finish_and_clear();

    if !topics.is_empty() {
        if let Err(e) = client.replace_topics(&repo.full_name, &topics).await {
            println!(
                "  {} Le repo est créé, mais l'ajout des topics a échoué : {}",
                "⚠".yellow(), e
            );
            println!("    Réessayez avec : iloc github repo topics --set {}", topics.join(","));
        }
    }

    println!("{} Repo créé !", "✓".green().bold());
    println!("  {} {}", "url:".dimmed(), repo.html_url.cyan().bold());
    println!("  {} {}", "clone SSH:".dimmed(), repo.ssh_url.cyan());
    println!("  {} {}", "clone HTTPS:".dimmed(), repo.clone_url.dimmed());
    println!();

    // Proposer de configurer le remote si on est dans un projet git
    if std::path::Path::new(".git").exists() {
        println!("  {} Ajouter comme remote 'origin' ?", "ℹ".cyan());
        if confirm("  Configurer git remote origin ?", yes)? {
            let status = std::process::Command::new("git")
                .args(["remote", "add", "origin", &repo.ssh_url])
                .status();
            match status {
                Ok(s) if s.success() => println!("  {} remote 'origin' configuré (SSH)", "✓".green()),
                _ => {
                    // Remote existe déjà — mettre à jour
                    let _ = std::process::Command::new("git")
                        .args(["remote", "set-url", "origin", &repo.ssh_url])
                        .status();
                    println!("  {} remote 'origin' mis à jour", "✓".green());
                }
            }
            println!();
            println!("  Prochaines étapes :");
            println!("    git add . && git commit -m \"Initial commit\"");
            println!("    git push -u origin {}", repo.default_branch.cyan());
        }
    }

    println!();
    Ok(())
}

pub async fn run_repo_list(
    org:     Option<String>,
    private: bool,
    public:  bool,
    fork:    bool,
    limit:   usize,
    profile: Option<String>,
) -> Result<()> {
    let creds  = github_store::require_credentials(profile.as_deref())?;
    let client = gh_client(&creds);

    println!();
    let sp = spinner("Récupération des repos…");

    let repos = match &org {
        Some(o) => {
            let t = if fork { "forks" } else if private { "private" } else if public { "public" } else { "all" };
            client.list_org_repos(o, t).await?
        }
        None => {
            let aff = if fork { "collaborator" } else { "owner,collaborator,organization_member" };
            client.list_user_repos(aff).await?
        }
    };
    sp.finish_and_clear();

    let mut repos = repos;
    if private      { repos.retain(|r| r.private); }
    if public       { repos.retain(|r| !r.private); }
    if fork         { repos.retain(|r| r.fork); }
    repos.truncate(limit);

    if repos.is_empty() {
        println!("{}", "  Aucun repo trouvé.".yellow());
        println!();
        return Ok(());
    }
    println!(
        "  {} {} repo(s){}",
        "Repos".bold(),
        repos.len(),
        org.as_deref().map(|o| format!(" dans {}", o.cyan())).unwrap_or_default()
    );
    println!();
    for r in &repos { print_repo(r); println!(); }
    Ok(())
}

pub async fn run_repo_view(
    owner_repo: Option<String>,
    profile:    Option<String>,
) -> Result<()> {
    let creds  = github_store::require_credentials(profile.as_deref())?;
    let (owner, repo) = resolve_owner_repo(owner_repo.as_deref(), &creds)?;
    let client = gh_client(&creds);

    let sp = spinner("Chargement…");
    let r  = client.get_repo(&owner, &repo).await?;
    sp.finish_and_clear();

    println!();
    println!("{}", r.full_name.bold().cyan());
    let vis = if r.private { "privé".red().to_string() } else { "public".green().to_string() };
    let arc = if r.archived { "  [archivé]".yellow().to_string() } else { String::new() };
    println!("  {} {}{}", vis, r.default_branch.dimmed(), arc);
    if let Some(d) = &r.description { println!("  {}", d); }
    println!();
    println!("  {} {}", "⭐ Stars:".dimmed(),  r.stargazers_count.unwrap_or(0));
    println!("  {} {}", "🍴 Forks:".dimmed(),  r.forks_count.unwrap_or(0));
    println!("  {} {}", "🐛 Issues:".dimmed(), r.open_issues_count.unwrap_or(0));
    if let Some(lang) = &r.language { println!("  {} {}", "💻 Langue:".dimmed(), lang); }
    if let Some(topics) = &r.topics {
        if !topics.is_empty() {
            println!("  {} {}", "🏷 Topics:".dimmed(), topics.join(", ").cyan());
        }
    }
    println!();
    println!("  {} {}", "clone SSH:".dimmed(),   r.ssh_url.cyan());
    println!("  {} {}", "clone HTTPS:".dimmed(), r.clone_url.dimmed());
    println!("  {} {}", "url:".dimmed(),          r.html_url.dimmed());
    if let Some(up) = &r.updated_at { println!("  {} {}", "mis à jour:".dimmed(), &up[..10]); }
    println!();
    Ok(())
}

pub async fn run_repo_delete(
    owner_repo: Option<String>,
    profile:    Option<String>,
    yes:        bool,
) -> Result<()> {
    let creds  = github_store::require_credentials(profile.as_deref())?;
    let (owner, repo) = resolve_owner_repo(owner_repo.as_deref(), &creds)?;

    println!();
    println!(
        "  {} Vous êtes sur le point de SUPPRIMER définitivement {}",
        "⚠".red().bold(), format!("{}/{}", owner, repo).bold()
    );
    println!("  {} Cette action est IRRÉVERSIBLE — le repo et tout son historique seront perdus.", "⚠".red());
    println!();

    let confirmation = prompt(&format!("  Tapez '{}/{}' pour confirmer : ", owner, repo))?;
    if confirmation != format!("{}/{}", owner, repo) {
        println!("  Confirmation invalide — suppression annulée.");
        return Ok(());
    }
    if !confirm("Supprimer définitivement ?", yes)? {
        println!("  Annulé.");
        return Ok(());
    }

    let client = gh_client(&creds);
    let sp     = spinner("Suppression…");
    client.delete_repo(&owner, &repo).await?;
    sp.finish_and_clear();

    println!("{} Repo {}/{} supprimé.", "✓".green().bold(), owner, repo);
    println!();
    Ok(())
}

pub async fn run_repo_archive(
    owner_repo: Option<String>,
    unarchive:  bool,
    profile:    Option<String>,
    yes:        bool,
) -> Result<()> {
    let creds  = github_store::require_credentials(profile.as_deref())?;
    let (owner, repo) = resolve_owner_repo(owner_repo.as_deref(), &creds)?;
    let client = gh_client(&creds);

    let action = if unarchive { "désarchiver" } else { "archiver" };
    if !confirm(&format!("{} {}/{} ?", action, owner, repo), yes)? {
        println!("  Annulé."); return Ok(());
    }

    let sp = spinner(&format!("{}…", action));
    client.update_repo(&owner, &repo, None, None, None, None, Some(!unarchive), None, None, None, None).await?;
    sp.finish_and_clear();

    let done = if unarchive { "désarchivé" } else { "archivé" };
    println!("{} {}/{} {}.", "✓".green().bold(), owner, repo, done);
    Ok(())
}

pub async fn run_repo_fork(
    owner_repo: &str,
    org:        Option<String>,
    profile:    Option<String>,
) -> Result<()> {
    let creds  = github_store::require_credentials(profile.as_deref())?;
    let (owner, repo) = parse_github_url(&format!("https://github.com/{}", owner_repo))
        .or_else(|_| {
            let p: Vec<&str> = owner_repo.splitn(2, '/').collect();
            if p.len() == 2 { Ok((p[0].to_string(), p[1].to_string())) }
            else { bail!("Format attendu: owner/repo") }
        })?;
    let client = gh_client(&creds);

    println!();
    let dest = org.as_deref().unwrap_or(&creds.login);
    let sp   = spinner(&format!("Fork de {}/{} vers {}…", owner, repo, dest));
    let fork = client.fork_repo(&owner, &repo, org.as_deref()).await?;
    sp.finish_and_clear();

    println!("{} Fork créé : {}", "✓".green().bold(), fork.full_name.bold().cyan());
    println!("  {} {}", "clone:".dimmed(), fork.clone_url.dimmed());
    println!();
    Ok(())
}

pub async fn run_repo_transfer(
    owner_repo: Option<String>,
    new_owner:  String,
    profile:    Option<String>,
    yes:        bool,
) -> Result<()> {
    let creds  = github_store::require_credentials(profile.as_deref())?;
    let (owner, repo) = resolve_owner_repo(owner_repo.as_deref(), &creds)?;

    println!();
    println!(
        "  {} Transférer {}/{} vers {}",
        "⚠".yellow(), owner.bold(), repo.bold(), new_owner.bold()
    );
    if !confirm("Confirmer le transfert ?", yes)? {
        println!("  Annulé."); return Ok(());
    }

    let client = gh_client(&creds);
    let sp     = spinner("Transfert…");
    client.transfer_repo(&owner, &repo, &new_owner, &[]).await?;
    sp.finish_and_clear();

    println!(
        "{} {}/{} transféré vers {}.",
        "✓".green().bold(), owner, repo, new_owner.bold()
    );
    Ok(())
}

pub async fn run_repo_topics(
    owner_repo: Option<String>,
    add:        Vec<String>,
    remove:     Vec<String>,
    set:        Vec<String>,
    profile:    Option<String>,
) -> Result<()> {
    let creds  = github_store::require_credentials(profile.as_deref())?;
    let (owner, repo) = resolve_owner_repo(owner_repo.as_deref(), &creds)?;
    let client = gh_client(&creds);

    let current = client.get_repo(&owner, &repo).await?.topics.unwrap_or_default();

    let final_topics = if !set.is_empty() {
        set
    } else {
        let mut t: Vec<String> = current.clone().into_iter()
            .filter(|t| !remove.contains(t))
            .collect();
        for topic in add {
            if !t.contains(&topic) { t.push(topic); }
        }
        t
    };

    let sp = spinner("Mise à jour des topics…");
    client.replace_topics(&format!("{}/{}", owner, repo), &final_topics).await?;
    sp.finish_and_clear();

    println!("{} Topics mis à jour : {}", "✓".green().bold(),
        if final_topics.is_empty() { "(aucun)".dimmed().to_string() }
        else { final_topics.join(", ").cyan().to_string() }
    );
    Ok(())
}

pub async fn run_repo_rename(
    owner_repo: Option<String>,
    new_name:   String,
    profile:    Option<String>,
    yes:        bool,
) -> Result<()> {
    let creds  = github_store::require_credentials(profile.as_deref())?;
    let (owner, repo) = resolve_owner_repo(owner_repo.as_deref(), &creds)?;

    if !confirm(&format!("Renommer {}/{} → {}/{} ?", owner, repo, owner, new_name), yes)? {
        println!("  Annulé."); return Ok(());
    }

    let client = gh_client(&creds);
    let sp     = spinner("Renommage…");
    let result = client
        .update_repo(&owner, &repo, Some(&new_name), None, None, None, None, None, None, None, None)
        .await;
    sp.finish_and_clear();

    let updated = result?;

    println!(
        "{} Repo renommé : {}/{} → {}",
        "✓".green().bold(),
        owner, repo,
        updated.full_name.cyan().bold()
    );
    println!(
        "  {}",
        "N'oubliez pas de mettre à jour votre remote Git local :".dimmed()
    );
    println!(
        "  {} {}",
        "git remote set-url origin".dimmed(),
        format!("https://github.com/{}.git", updated.full_name).dimmed()
    );
    Ok(())
}

pub async fn run_repo_visibility(
    owner_repo: Option<String>,
    private:    bool,
    public:     bool,
    profile:    Option<String>,
    yes:        bool,
) -> Result<()> {
    let creds  = github_store::require_credentials(profile.as_deref())?;
    let (owner, repo) = resolve_owner_repo(owner_repo.as_deref(), &creds)?;

    let is_private = if public { false } else if private { true } else {
        bail!("Précisez --private ou --public");
    };
    let label = if is_private { "privé".red() } else { "public".green() };

    if !confirm(&format!("Changer {}/{} en {} ?", owner, repo, label), yes)? {
        println!("  Annulé."); return Ok(());
    }

    let client = gh_client(&creds);
    let sp     = spinner("Changement de visibilité…");
    client.update_repo(&owner, &repo, None, None, None, Some(is_private), None, None, None, None, None).await?;
    sp.finish_and_clear();

    println!("{} {}/{} est maintenant {}.", "✓".green().bold(), owner, repo, label);
    Ok(())
}

// ═════════════════════════════════════════════════════════════
//  BRANCHES
// ═════════════════════════════════════════════════════════════

pub async fn run_branch_list(
    owner_repo: Option<String>,
    profile:    Option<String>,
) -> Result<()> {
    let creds  = github_store::require_credentials(profile.as_deref())?;
    let (owner, repo) = resolve_owner_repo(owner_repo.as_deref(), &creds)?;
    let client = gh_client(&creds);

    let sp       = spinner("Chargement des branches…");
    let branches = client.list_branches(&owner, &repo).await?;
    sp.finish_and_clear();

    // Branche par défaut
    let default_branch = client.get_repo(&owner, &repo).await
        .map(|r| r.default_branch)
        .unwrap_or_else(|_| "main".to_string());

    println!();
    println!("  {} branches dans {}/{}", branches.len(), owner.bold(), repo.bold());
    println!();
    for b in &branches {
        if b.name != default_branch { continue; }
        print!("  {} {} {}", "🌿".to_string(), b.name.bold().green(), "[défaut]".dimmed());
        let prot = if b.protected { "  [protégée]".yellow().to_string() } else { String::new() };
        println!("{} — {}", prot, &b.commit.sha[..8].dimmed());
    }
    for b in &branches {
        if b.name != default_branch { print_branch(b); }
    }
    println!();
    Ok(())
}

pub async fn run_branch_create(
    owner_repo: Option<String>,
    name:       String,
    from:       Option<String>,
    profile:    Option<String>,
) -> Result<()> {
    let creds  = github_store::require_credentials(profile.as_deref())?;
    let (owner, repo) = resolve_owner_repo(owner_repo.as_deref(), &creds)?;
    let client = gh_client(&creds);

    // Base : branche par défaut si non précisé
    let base = match from {
        Some(f) => f,
        None    => client.get_repo(&owner, &repo).await?.default_branch,
    };

    let sp = spinner(&format!("Création de '{}' depuis '{}'…", name, base));
    client.create_branch(&owner, &repo, &name, &base).await?;
    sp.finish_and_clear();

    println!("{} Branche '{}' créée depuis '{}'.", "✓".green().bold(), name.bold(), base.cyan());
    Ok(())
}

pub async fn run_branch_delete(
    owner_repo: Option<String>,
    name:       String,
    profile:    Option<String>,
    yes:        bool,
) -> Result<()> {
    let creds  = github_store::require_credentials(profile.as_deref())?;
    let (owner, repo) = resolve_owner_repo(owner_repo.as_deref(), &creds)?;

    if !confirm(&format!("Supprimer la branche '{}' ?", name), yes)? {
        println!("  Annulé."); return Ok(());
    }

    let client = gh_client(&creds);
    let sp     = spinner("Suppression…");
    client.delete_branch(&owner, &repo, &name).await?;
    sp.finish_and_clear();

    println!("{} Branche '{}' supprimée.", "✓".green().bold(), name.bold());
    Ok(())
}

pub async fn run_branch_protect(
    owner_repo:    Option<String>,
    name:          String,
    checks:        Vec<String>,
    require_pr:    bool,
    min_reviews:   u32,
    enforce_admin: bool,
    linear:        bool,
    allow_force:   bool,
    allow_delete:  bool,
    profile:       Option<String>,
) -> Result<()> {
    let creds  = github_store::require_credentials(profile.as_deref())?;
    let (owner, repo) = resolve_owner_repo(owner_repo.as_deref(), &creds)?;
    let client = gh_client(&creds);

    let checks_ref: Vec<&str> = checks.iter().map(|s| s.as_str()).collect();

    let sp = spinner(&format!("Protection de la branche '{}'…", name));
    client.protect_branch(
        &owner, &repo, &name,
        &checks_ref,
        require_pr, min_reviews,
        enforce_admin,
        linear, allow_force, allow_delete,
    ).await?;
    sp.finish_and_clear();

    println!("{} Branche '{}' protégée.", "✓".green().bold(), name.bold());
    println!("  {} PR reviews requises: {}", "ℹ".cyan(), if require_pr { min_reviews.to_string() } else { "non".to_string() });
    if !checks.is_empty() {
        println!("  {} Status checks: {}", "ℹ".cyan(), checks.join(", "));
    }
    Ok(())
}

pub async fn run_branch_unprotect(
    owner_repo: Option<String>,
    name:       String,
    profile:    Option<String>,
    yes:        bool,
) -> Result<()> {
    let creds  = github_store::require_credentials(profile.as_deref())?;
    let (owner, repo) = resolve_owner_repo(owner_repo.as_deref(), &creds)?;

    if !confirm(&format!("Retirer la protection de la branche '{}' ?", name), yes)? {
        println!("  Annulé."); return Ok(());
    }

    let client = gh_client(&creds);
    let sp     = spinner(&format!("Retrait de la protection de '{}'…", name));
    client.unprotect_branch(&owner, &repo, &name).await?;
    sp.finish_and_clear();

    println!("{} Protection retirée de la branche '{}'.", "✓".green().bold(), name.bold());
    Ok(())
}

pub async fn run_branch_default(
    owner_repo: Option<String>,
    name:       String,
    profile:    Option<String>,
) -> Result<()> {
    let creds  = github_store::require_credentials(profile.as_deref())?;
    let (owner, repo) = resolve_owner_repo(owner_repo.as_deref(), &creds)?;
    let client = gh_client(&creds);

    let sp = spinner("Changement de la branche par défaut…");
    client.update_repo(&owner, &repo, None, None, None, None, None, Some(&name), None, None, None).await?;
    sp.finish_and_clear();

    println!("{} Branche par défaut : '{}'.", "✓".green().bold(), name.bold());
    Ok(())
}

// ═════════════════════════════════════════════════════════════
//  ISSUES
// ═════════════════════════════════════════════════════════════

pub async fn run_issue_list(
    owner_repo: Option<String>,
    state:      String,
    labels:     Vec<String>,
    assignee:   Option<String>,
    limit:      usize,
    profile:    Option<String>,
) -> Result<()> {
    let creds  = github_store::require_credentials(profile.as_deref())?;
    let (owner, repo) = resolve_owner_repo(owner_repo.as_deref(), &creds)?;
    let client = gh_client(&creds);

    let sp     = spinner("Chargement des issues…");
    let mut issues = client.list_issues(&owner, &repo, &state, &labels, assignee.as_deref()).await?;
    sp.finish_and_clear();

    issues.truncate(limit);
    if issues.is_empty() {
        println!("{}", "  Aucune issue trouvée.".yellow());
        return Ok(());
    }

    println!();
    println!("  {} issue(s) — {}/{}", issues.len(), owner.bold(), repo.bold());
    println!();
    for i in &issues { print_issue(i); }
    println!();
    Ok(())
}

pub async fn run_issue_create(
    owner_repo: Option<String>,
    title:      Option<String>,
    body:       Option<String>,
    labels:     Vec<String>,
    assignees:  Vec<String>,
    milestone:  Option<u64>,
    profile:    Option<String>,
) -> Result<()> {
    let creds  = github_store::require_credentials(profile.as_deref())?;
    let (owner, repo) = resolve_owner_repo(owner_repo.as_deref(), &creds)?;

    println!();
    let issue_title = match title {
        Some(t) => t,
        None    => {
            let t = prompt("  Titre : ")?;
            if t.is_empty() { bail!("Le titre est obligatoire."); }
            t
        }
    };

    let issue_body = match body {
        Some(b) => Some(b),
        None    => {
            let b = prompt("  Description (Entrée pour passer) : ")?;
            if b.is_empty() { None } else { Some(b) }
        }
    };

    let client = gh_client(&creds);
    let sp     = spinner("Création de l'issue…");
    let issue  = client.create_issue(
        &owner, &repo,
        &issue_title, issue_body.as_deref(),
        &labels, &assignees, milestone,
    ).await?;
    sp.finish_and_clear();

    println!("{} Issue #{} créée", "✓".green().bold(), issue.number);
    println!("  {} {}", "titre:".dimmed(), issue.title.bold());
    println!("  {}", issue.html_url.cyan());
    println!();
    Ok(())
}

pub async fn run_issue_view(
    owner_repo: Option<String>,
    number:     u64,
    profile:    Option<String>,
) -> Result<()> {
    let creds  = github_store::require_credentials(profile.as_deref())?;
    let (owner, repo) = resolve_owner_repo(owner_repo.as_deref(), &creds)?;
    let client = gh_client(&creds);

    let sp    = spinner("Chargement…");
    let issue = client.get_issue(&owner, &repo, number).await?;
    sp.finish_and_clear();

    println!();
    println!("  #{} {}", issue.number, issue.title.bold());
    println!("  {} {} · {}", issue.state.cyan(), issue.user.login.dimmed(), &issue.created_at[..10].dimmed());
    if !issue.labels.is_empty() {
        println!("  {}", issue.labels.iter().map(|l| l.name.clone()).collect::<Vec<_>>().join(", ").cyan());
    }
    if !issue.assignees.is_empty() {
        println!("  {} {}", "assignés:".dimmed(), issue.assignees.iter().map(|u| u.login.clone()).collect::<Vec<_>>().join(", "));
    }
    if let Some(body) = &issue.body {
        println!();
        for line in body.lines().take(20) { println!("  {}", line); }
    }
    println!();
    println!("  {}", issue.html_url.dimmed());
    println!();
    Ok(())
}

pub async fn run_issue_close(
    owner_repo: Option<String>,
    number:     u64,
    reason:     Option<String>,
    profile:    Option<String>,
) -> Result<()> {
    let creds  = github_store::require_credentials(profile.as_deref())?;
    let (owner, repo) = resolve_owner_repo(owner_repo.as_deref(), &creds)?;
    let client = gh_client(&creds);

    let sp = spinner(&format!("Fermeture de l'issue #{}…", number));
    client.update_issue(&owner, &repo, number, None, None, Some("closed"), None, None, reason.as_deref()).await?;
    sp.finish_and_clear();

    println!("{} Issue #{} fermée.", "✓".green().bold(), number);
    Ok(())
}

pub async fn run_issue_reopen(
    owner_repo: Option<String>,
    number:     u64,
    profile:    Option<String>,
) -> Result<()> {
    let creds  = github_store::require_credentials(profile.as_deref())?;
    let (owner, repo) = resolve_owner_repo(owner_repo.as_deref(), &creds)?;
    let client = gh_client(&creds);

    let sp = spinner(&format!("Réouverture de l'issue #{}…", number));
    client.update_issue(&owner, &repo, number, None, None, Some("open"), None, None, None).await?;
    sp.finish_and_clear();
    println!("{} Issue #{} rouverte.", "✓".green().bold(), number);
    Ok(())
}

pub async fn run_issue_comment(
    owner_repo: Option<String>,
    number:     u64,
    body:       Option<String>,
    profile:    Option<String>,
) -> Result<()> {
    let creds  = github_store::require_credentials(profile.as_deref())?;
    let (owner, repo) = resolve_owner_repo(owner_repo.as_deref(), &creds)?;

    let text = match body {
        Some(b) => b,
        None    => {
            let b = prompt("  Commentaire : ")?;
            if b.is_empty() { bail!("Le commentaire ne peut pas être vide."); }
            b
        }
    };

    let client = gh_client(&creds);
    let sp     = spinner("Ajout du commentaire…");
    client.add_comment(&owner, &repo, number, &text).await?;
    sp.finish_and_clear();

    println!("{} Commentaire ajouté à l'issue #{}.", "✓".green().bold(), number);
    Ok(())
}

pub async fn run_issue_assign(
    owner_repo: Option<String>,
    number:     u64,
    users:      Vec<String>,
    unassign:   bool,
    profile:    Option<String>,
) -> Result<()> {
    let creds  = github_store::require_credentials(profile.as_deref())?;
    let (owner, repo) = resolve_owner_repo(owner_repo.as_deref(), &creds)?;
    let client = gh_client(&creds);

    let (new_assignees, action) = if unassign {
        // Récupérer la liste actuelle et retirer les users demandés
        let current = client.get_issue(&owner, &repo, number).await
            .map(|i| i.assignees.iter().map(|u| u.login.clone()).collect::<Vec<_>>())
            .unwrap_or_default();
        let kept: Vec<String> = current.into_iter().filter(|u| !users.contains(u)).collect();
        (kept, "retirés de")
    } else {
        (users.clone(), "assignés à")
    };

    let sp = spinner("Mise à jour des assignés…");
    client.update_issue(&owner, &repo, number, None, None, None, None, Some(&new_assignees), None).await?;
    sp.finish_and_clear();

    println!(
        "{} {} {} l'issue #{}.",
        "✓".green().bold(), users.join(", ").cyan(), action, number
    );
    Ok(())
}

pub async fn run_issue_label(
    owner_repo: Option<String>,
    number:     u64,
    add:        Vec<String>,
    remove:     Vec<String>,
    profile:    Option<String>,
) -> Result<()> {
    let creds  = github_store::require_credentials(profile.as_deref())?;
    let (owner, repo) = resolve_owner_repo(owner_repo.as_deref(), &creds)?;
    let client = gh_client(&creds);

    let current = client.get_issue(&owner, &repo, number).await
        .map(|i| i.labels.iter().map(|l| l.name.clone()).collect::<Vec<_>>())
        .unwrap_or_default();

    let mut labels: Vec<String> = current.into_iter().filter(|l| !remove.contains(l)).collect();
    for label in &add {
        if !labels.contains(label) { labels.push(label.clone()); }
    }

    let sp = spinner("Mise à jour des labels…");
    client.update_issue(&owner, &repo, number, None, None, None, Some(&labels), None, None).await?;
    sp.finish_and_clear();

    println!("{} Labels de l'issue #{} mis à jour : {}", "✓".green().bold(), number,
        if labels.is_empty() { "(aucun)".dimmed().to_string() }
        else { labels.join(", ").cyan().to_string() }
    );
    Ok(())
}

// ═════════════════════════════════════════════════════════════
//  PULL REQUESTS
// ═════════════════════════════════════════════════════════════

pub async fn run_pr_list(
    owner_repo: Option<String>,
    state:      String,
    base:       Option<String>,
    limit:      usize,
    profile:    Option<String>,
) -> Result<()> {
    let creds  = github_store::require_credentials(profile.as_deref())?;
    let (owner, repo) = resolve_owner_repo(owner_repo.as_deref(), &creds)?;
    let client = gh_client(&creds);

    let sp  = spinner("Chargement des PRs…");
    let mut prs = client.list_prs(&owner, &repo, &state, base.as_deref()).await?;
    sp.finish_and_clear();

    prs.truncate(limit);
    if prs.is_empty() {
        println!("{}", "  Aucune PR trouvée.".yellow());
        return Ok(());
    }

    println!();
    println!("  {} PR(s) — {}/{}", prs.len(), owner.bold(), repo.bold());
    println!();
    for pr in &prs { print_pr(pr); }
    println!();
    Ok(())
}

pub async fn run_pr_create(
    owner_repo: Option<String>,
    title:      Option<String>,
    body:       Option<String>,
    head:       Option<String>,
    base:       Option<String>,
    draft:      bool,
    profile:    Option<String>,
) -> Result<()> {
    let creds  = github_store::require_credentials(profile.as_deref())?;
    let (owner, repo) = resolve_owner_repo(owner_repo.as_deref(), &creds)?;
    let client = gh_client(&creds);

    println!();
    // Branche head : branche git courante par défaut
    let head_branch = match head {
        Some(h) => h,
        None => {
            let output = std::process::Command::new("git")
                .args(["rev-parse", "--abbrev-ref", "HEAD"])
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .unwrap_or_else(|_| String::new());
            if output.is_empty() {
                prompt("  Branche source (head) : ")?
            } else {
                prompt_default("  Branche source (head)", &output)?
            }
        }
    };

    let default_branch = client.get_repo(&owner, &repo).await?.default_branch;
    let base_branch = match base {
        Some(b) => b,
        None    => prompt_default("  Branche cible (base)", &default_branch)?,
    };

    let pr_title = match title {
        Some(t) => t,
        None    => {
            let t = prompt("  Titre : ")?;
            if t.is_empty() { bail!("Le titre est obligatoire."); }
            t
        }
    };

    let pr_body = match body {
        Some(b) => Some(b),
        None    => {
            let b = prompt("  Description (Entrée pour passer) : ")?;
            if b.is_empty() { None } else { Some(b) }
        }
    };

    let sp = spinner("Création de la PR…");
    let pr = client.create_pr(
        &owner, &repo,
        &pr_title, pr_body.as_deref(),
        &head_branch, &base_branch, draft,
    ).await?;
    sp.finish_and_clear();

    println!("{} PR #{} créée{}", "✓".green().bold(), pr.number, if draft { " [draft]" } else { "" });
    println!("  {} {}", "titre:".dimmed(), pr.title.bold());
    println!("  {} {} → {}", "branches:".dimmed(), pr.head.ref_.cyan(), pr.base.ref_.dimmed());
    println!("  {}", pr.html_url.cyan());
    println!();
    Ok(())
}

pub async fn run_pr_view(
    owner_repo: Option<String>,
    number:     u64,
    profile:    Option<String>,
) -> Result<()> {
    let creds  = github_store::require_credentials(profile.as_deref())?;
    let (owner, repo) = resolve_owner_repo(owner_repo.as_deref(), &creds)?;
    let client = gh_client(&creds);

    let sp = spinner("Chargement…");
    let pr = client.get_pr(&owner, &repo, number).await?;
    sp.finish_and_clear();

    println!();
    println!("  PR #{} {}", pr.number, pr.title.bold());
    let state_label = match pr.state.as_str() {
        "open"   if pr.draft                    => "draft".yellow().to_string(),
        "open"                                   => "open".green().to_string(),
        "closed" if pr.merged.unwrap_or(false)  => "merged".magenta().to_string(),
        _                                        => "closed".red().to_string(),
    };
    println!("  {} {} → {}", state_label, pr.head.ref_.cyan(), pr.base.ref_.dimmed());
    println!("  {} {} · {}", "auteur:".dimmed(), pr.user.login.cyan(), &pr.created_at[..10].dimmed());
    if let Some(m) = pr.mergeable {
        println!("  {} {}", "mergeable:".dimmed(),
            if m { "oui".green().to_string() } else { "conflits".red().to_string() }
        );
    }
    if let Some(body) = &pr.body {
        if !body.is_empty() {
            println!();
            for line in body.lines().take(15) { println!("  {}", line); }
        }
    }
    println!();
    println!("  {}", pr.html_url.dimmed());
    println!();
    Ok(())
}

pub async fn run_pr_merge(
    owner_repo:   Option<String>,
    number:       u64,
    method:       String,
    title:        Option<String>,
    message:      Option<String>,
    profile:      Option<String>,
    yes:          bool,
) -> Result<()> {
    let creds  = github_store::require_credentials(profile.as_deref())?;
    let (owner, repo) = resolve_owner_repo(owner_repo.as_deref(), &creds)?;

    if !confirm(&format!("Merger la PR #{} ({}) ?", number, method), yes)? {
        println!("  Annulé."); return Ok(());
    }

    let client = gh_client(&creds);
    let sp     = spinner("Merge…");
    client.merge_pr(&owner, &repo, number, title.as_deref(), message.as_deref(), &method).await?;
    sp.finish_and_clear();

    println!("{} PR #{} mergée ({}).", "✓".green().bold(), number, method);
    Ok(())
}

pub async fn run_pr_review(
    owner_repo: Option<String>,
    number:     u64,
    reviewers:  Vec<String>,
    teams:      Vec<String>,
    profile:    Option<String>,
) -> Result<()> {
    let creds  = github_store::require_credentials(profile.as_deref())?;
    let (owner, repo) = resolve_owner_repo(owner_repo.as_deref(), &creds)?;
    let client = gh_client(&creds);

    let sp = spinner("Demande de review…");
    client.request_reviewers(&owner, &repo, number, &reviewers, &teams).await?;
    sp.finish_and_clear();

    let all: Vec<String> = reviewers.iter().chain(teams.iter()).cloned().collect();
    println!("{} Review demandée à : {}", "✓".green().bold(), all.join(", ").cyan());
    Ok(())
}

pub async fn run_pr_checkout(
    owner_repo: Option<String>,
    number:     u64,
    profile:    Option<String>,
) -> Result<()> {
    let creds  = github_store::require_credentials(profile.as_deref())?;
    let (owner, repo) = resolve_owner_repo(owner_repo.as_deref(), &creds)?;
    let client = gh_client(&creds);

    let sp = spinner("Chargement de la PR…");
    let pr = client.get_pr(&owner, &repo, number).await?;
    sp.finish_and_clear();

    let branch = &pr.head.ref_;
    println!("  → git fetch origin && git checkout {}", branch.cyan());

    let fetch = std::process::Command::new("git")
        .args(["fetch", "origin"])
        .status();
    if fetch.map(|s| s.success()).unwrap_or(false) {
        let checkout = std::process::Command::new("git")
            .args(["checkout", branch])
            .status();
        match checkout {
            Ok(s) if s.success() => println!("{} Branche '{}' checkoutée.", "✓".green().bold(), branch.bold()),
            _ => println!("  {} git checkout {} (copiez et exécutez manuellement)", "→".cyan(), branch.cyan()),
        }
    } else {
        println!("  {} git fetch origin && git checkout {}", "→".cyan(), branch.cyan());
    }
    Ok(())
}

pub async fn run_pr_ready(
    owner_repo: Option<String>,
    number:     u64,
    draft:      bool,
    profile:    Option<String>,
) -> Result<()> {
    let creds  = github_store::require_credentials(profile.as_deref())?;
    let (owner, repo) = resolve_owner_repo(owner_repo.as_deref(), &creds)?;
    let client = gh_client(&creds);

    let sp = spinner("Chargement de la PR…");
    let pr = client.get_pr(&owner, &repo, number).await?;
    sp.finish_and_clear();

    let sp2 = spinner(if draft { "Conversion en draft…" } else { "Marquage comme prête…" });
    // node_id GraphQL requis ici, PAS pr.head.label (qui vaut "owner:branche" —
    // bug réel corrigé : l'appel GraphQL échouait systématiquement avant ce correctif).
    client.set_pr_draft(&pr.node_id, draft).await?;
    sp2.finish_and_clear();

    println!(
        "{} PR #{} {}.",
        "✓".green().bold(), number,
        if draft { "convertie en draft" } else { "marquée comme prête" }
    );
    Ok(())
}

pub async fn run_pr_close(
    owner_repo: Option<String>,
    number:     u64,
    profile:    Option<String>,
    yes:        bool,
) -> Result<()> {
    let creds  = github_store::require_credentials(profile.as_deref())?;
    let (owner, repo) = resolve_owner_repo(owner_repo.as_deref(), &creds)?;

    if !confirm(&format!("Fermer la PR #{} sans merger ?", number), yes)? {
        println!("  Annulé."); return Ok(());
    }

    let client = gh_client(&creds);
    let sp     = spinner(&format!("Fermeture de la PR #{}…", number));
    // Les PRs partagent l'API de fermeture des issues sur GitHub, mais on
    // garde un message dédié pour ne pas dire "Issue fermée" pour une PR.
    client.update_issue(&owner, &repo, number, None, None, Some("closed"), None, None, None).await?;
    sp.finish_and_clear();

    println!("{} PR #{} fermée (sans merge).", "✓".green().bold(), number);
    Ok(())
}

pub async fn run_pr_update_branch(
    owner_repo: Option<String>,
    number:     u64,
    profile:    Option<String>,
) -> Result<()> {
    let creds  = github_store::require_credentials(profile.as_deref())?;
    let (owner, repo) = resolve_owner_repo(owner_repo.as_deref(), &creds)?;
    let client = gh_client(&creds);

    let sp = spinner("Mise à jour de la branche de la PR…");
    client.update_pr_branch(&owner, &repo, number).await?;
    sp.finish_and_clear();

    println!("{} Branche de la PR #{} mise à jour.", "✓".green().bold(), number);
    Ok(())
}

// ═════════════════════════════════════════════════════════════
//  RELEASES
// ═════════════════════════════════════════════════════════════

pub async fn run_release_list(
    owner_repo: Option<String>,
    limit:      usize,
    profile:    Option<String>,
) -> Result<()> {
    let creds  = github_store::require_credentials(profile.as_deref())?;
    let (owner, repo) = resolve_owner_repo(owner_repo.as_deref(), &creds)?;
    let client = gh_client(&creds);

    let sp = spinner("Chargement des releases…");
    let mut releases = client.list_releases(&owner, &repo).await?;
    sp.finish_and_clear();
    releases.truncate(limit);

    if releases.is_empty() {
        println!("{}", "  Aucune release trouvée.".yellow());
        return Ok(());
    }
    println!();
    println!("  {} release(s) — {}/{}", releases.len(), owner.bold(), repo.bold());
    println!();
    for r in &releases { print_release(r); }
    println!();
    Ok(())
}

pub async fn run_release_create(
    owner_repo:     Option<String>,
    tag:            Option<String>,
    name:           Option<String>,
    body:           Option<String>,
    draft:          bool,
    prerelease:     bool,
    target:         Option<String>,
    generate_notes: bool,
    profile:        Option<String>,
    yes:            bool,
) -> Result<()> {
    let creds  = github_store::require_credentials(profile.as_deref())?;
    let (owner, repo) = resolve_owner_repo(owner_repo.as_deref(), &creds)?;

    println!();
    let tag_name = match tag {
        Some(t) => t,
        None if yes => bail!("--tag est obligatoire en mode --yes (pas de prompt interactif)."),
        None    => {
            let t = prompt("  Tag (ex: v1.0.0) : ")?;
            if t.is_empty() { bail!("Le tag est obligatoire."); }
            t
        }
    };

    let release_name = match name {
        Some(n) => Some(n),
        None if yes => Some(tag_name.clone()),
        None    => {
            let n = prompt_default("  Nom de la release", &tag_name)?;
            Some(n)
        }
    };

    let notes = match body {
        Some(b) => Some(b),
        None if generate_notes => None,
        None if yes => None,
        None => {
            let b = prompt("  Notes (Entrée pour passer ou --generate-notes) : ")?;
            if b.is_empty() { None } else { Some(b) }
        }
    };

    if !confirm(
        &format!("Créer la release {} {} ?", tag_name.bold(), if draft { "[draft]" } else { "" }),
        yes,
    )? {
        println!("  Annulé."); return Ok(());
    }

    let client = gh_client(&creds);
    let sp     = spinner("Création de la release…");
    let release = client.create_release(
        &owner, &repo,
        &tag_name, release_name.as_deref(), notes.as_deref(),
        draft, prerelease,
        target.as_deref(), generate_notes,
    ).await?;
    sp.finish_and_clear();

    println!("{} Release {} créée{}", "✓".green().bold(), release.tag_name.bold(),
        if draft { " [draft — non publiée]" } else { "" }
    );
    println!("  {}", release.html_url.cyan());
    println!();
    Ok(())
}

pub async fn run_release_delete(
    owner_repo: Option<String>,
    tag:        String,
    profile:    Option<String>,
    yes:        bool,
) -> Result<()> {
    let creds  = github_store::require_credentials(profile.as_deref())?;
    let (owner, repo) = resolve_owner_repo(owner_repo.as_deref(), &creds)?;
    let client = gh_client(&creds);

    let release = client.get_release_by_tag(&owner, &repo, &tag).await
        .map_err(|_| anyhow::anyhow!("Release '{}' introuvable.", tag))?;

    if !confirm(&format!("Supprimer la release '{}' ?", tag), yes)? {
        println!("  Annulé."); return Ok(());
    }

    let sp = spinner("Suppression…");
    client.delete_release(&owner, &repo, release.id).await?;
    sp.finish_and_clear();

    println!("{} Release '{}' supprimée.", "✓".green().bold(), tag.bold());
    Ok(())
}

pub async fn run_release_upload(
    owner_repo:   Option<String>,
    tag:          String,
    file:         PathBuf,
    name:         Option<String>,
    content_type: Option<String>,
    profile:      Option<String>,
) -> Result<()> {
    let creds  = github_store::require_credentials(profile.as_deref())?;
    let (owner, repo) = resolve_owner_repo(owner_repo.as_deref(), &creds)?;
    let client = gh_client(&creds);

    if !file.exists() { bail!("Fichier '{}' introuvable.", file.display()); }

    let release = client.get_release_by_tag(&owner, &repo, &tag).await
        .map_err(|_| anyhow::anyhow!("Release '{}' introuvable.", tag))?;

    let asset_name = name.unwrap_or_else(|| {
        file.file_name().unwrap_or_default().to_string_lossy().to_string()
    });
    let ct = content_type.unwrap_or_else(|| guess_content_type(&file).to_string());
    let data = std::fs::read(&file).context("Lecture du fichier")?;
    let size = data.len();

    let upload_url = format!(
        "https://uploads.github.com/repos/{}/{}/releases/{}/assets",
        owner, repo, release.id
    );

    let sp = spinner(&format!("Upload de '{}' ({})…", asset_name, crate::utils::human_bytes(size as u64)));
    let asset = client.upload_release_asset(&upload_url, &asset_name, &ct, data).await?;
    sp.finish_and_clear();

    println!("{} Asset '{}' uploadé ({} téléchargements)", "✓".green().bold(),
        asset.name.bold(), asset.download_count
    );
    println!("  {}", asset.browser_download_url.dimmed());
    Ok(())
}

fn guess_content_type(path: &PathBuf) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "gz" | "tgz" => "application/gzip",
        "zip"        => "application/zip",
        "exe"        => "application/octet-stream",
        "deb"        => "application/vnd.debian.binary-package",
        "rpm"        => "application/x-rpm",
        "dmg"        => "application/x-apple-diskimage",
        "txt"        => "text/plain",
        "json"       => "application/json",
        _            => "application/octet-stream",
    }
}

// ═════════════════════════════════════════════════════════════
//  GITHUB ACTIONS
// ═════════════════════════════════════════════════════════════

pub async fn run_actions_list(
    owner_repo: Option<String>,
    profile:    Option<String>,
) -> Result<()> {
    let creds  = github_store::require_credentials(profile.as_deref())?;
    let (owner, repo) = resolve_owner_repo(owner_repo.as_deref(), &creds)?;
    let client = gh_client(&creds);

    let sp        = spinner("Chargement des workflows…");
    let workflows = client.list_workflows(&owner, &repo).await?;
    sp.finish_and_clear();

    if workflows.is_empty() {
        println!("{}", "  Aucun workflow trouvé.".yellow());
        return Ok(());
    }
    println!();
    println!("  {} workflow(s) — {}/{}", workflows.len(), owner.bold(), repo.bold());
    println!();
    for w in &workflows { print_workflow(w); }
    println!();
    Ok(())
}

pub async fn run_actions_run(
    owner_repo: Option<String>,
    workflow:   String,
    branch:     Option<String>,
    inputs:     Vec<String>,
    profile:    Option<String>,
) -> Result<()> {
    let creds  = github_store::require_credentials(profile.as_deref())?;
    let (owner, repo) = resolve_owner_repo(owner_repo.as_deref(), &creds)?;
    let client = gh_client(&creds);

    let target_branch = match branch {
        Some(b) => b,
        None    => client.get_repo(&owner, &repo).await?.default_branch,
    };

    // Parser les inputs KEY=VALUE
    let mut inputs_map = serde_json::Map::new();
    for input in &inputs {
        let parts: Vec<&str> = input.splitn(2, '=').collect();
        if parts.len() == 2 {
            inputs_map.insert(parts[0].to_string(), serde_json::json!(parts[1]));
        }
    }

    let sp = spinner(&format!("Déclenchement de '{}' sur '{}'…", workflow, target_branch));
    client.trigger_workflow(&owner, &repo, &workflow, &target_branch, &inputs_map).await?;
    sp.finish_and_clear();

    println!(
        "{} Workflow '{}' déclenché sur '{}'.",
        "✓".green().bold(), workflow.bold(), target_branch.cyan()
    );
    println!(
        "  Suivez l'exécution : {}",
        format!("https://github.com/{}/{}/actions", owner, repo).dimmed()
    );
    Ok(())
}

pub async fn run_actions_status(
    owner_repo: Option<String>,
    workflow:   Option<String>,
    branch:     Option<String>,
    limit:      usize,
    profile:    Option<String>,
) -> Result<()> {
    let creds  = github_store::require_credentials(profile.as_deref())?;
    let (owner, repo) = resolve_owner_repo(owner_repo.as_deref(), &creds)?;
    let client = gh_client(&creds);

    let sp  = spinner("Chargement des runs…");
    let mut runs = client.list_workflow_runs(
        &owner, &repo,
        workflow.as_deref(), branch.as_deref(), None,
    ).await?;
    sp.finish_and_clear();

    runs.truncate(limit);
    if runs.is_empty() {
        println!("{}", "  Aucun run trouvé.".yellow());
        return Ok(());
    }
    println!();
    println!("  {} run(s) récent(s) — {}/{}", runs.len(), owner.bold(), repo.bold());
    println!();
    for r in &runs { print_run(r); }
    println!();
    Ok(())
}

pub async fn run_actions_cancel(
    owner_repo: Option<String>,
    run_id:     u64,
    profile:    Option<String>,
    yes:        bool,
) -> Result<()> {
    let creds  = github_store::require_credentials(profile.as_deref())?;
    let (owner, repo) = resolve_owner_repo(owner_repo.as_deref(), &creds)?;

    if !confirm(&format!("Annuler le run #{} ?", run_id), yes)? {
        println!("  Annulé."); return Ok(());
    }

    let client = gh_client(&creds);
    let sp     = spinner("Annulation…");
    client.cancel_workflow_run(&owner, &repo, run_id).await?;
    sp.finish_and_clear();

    println!("{} Run #{} annulé.", "✓".green().bold(), run_id);
    Ok(())
}

pub async fn run_actions_rerun(
    owner_repo: Option<String>,
    run_id:     u64,
    profile:    Option<String>,
) -> Result<()> {
    let creds  = github_store::require_credentials(profile.as_deref())?;
    let (owner, repo) = resolve_owner_repo(owner_repo.as_deref(), &creds)?;
    let client = gh_client(&creds);

    let sp = spinner("Relancement du run…");
    client.rerun_workflow(&owner, &repo, run_id).await?;
    sp.finish_and_clear();

    println!("{} Run #{} relancé.", "✓".green().bold(), run_id);
    Ok(())
}

// ═════════════════════════════════════════════════════════════
//  SECRETS
// ═════════════════════════════════════════════════════════════

pub async fn run_secret_list(
    owner_repo: Option<String>,
    profile:    Option<String>,
) -> Result<()> {
    let creds  = github_store::require_credentials(profile.as_deref())?;
    let (owner, repo) = resolve_owner_repo(owner_repo.as_deref(), &creds)?;
    let client = gh_client(&creds);

    let sp      = spinner("Chargement des secrets…");
    let secrets = client.list_repo_secrets(&owner, &repo).await?;
    sp.finish_and_clear();

    if secrets.is_empty() {
        println!("{}", "  Aucun secret Actions trouvé.".yellow());
        return Ok(());
    }
    println!();
    println!("  {} secret(s) Actions — {}/{}", secrets.len(), owner.bold(), repo.bold());
    println!("  {} Les valeurs ne sont jamais exposées par l'API GitHub.", "ℹ".cyan());
    println!();
    for s in &secrets {
        println!("  {} {}", "🔑".to_string(), s.bold());
    }
    println!();
    Ok(())
}

pub async fn run_secret_set(
    owner_repo: Option<String>,
    name:       String,
    value:      Option<String>,
    profile:    Option<String>,
) -> Result<()> {
    let creds  = github_store::require_credentials(profile.as_deref())?;
    let (owner, repo) = resolve_owner_repo(owner_repo.as_deref(), &creds)?;

    let secret_value = match value {
        Some(v) => v,
        None    => rpassword::prompt_password(
            &format!("  Valeur de '{}' (masquée): ", name)
        ).context("Lecture de la valeur du secret")?,
    };
    if secret_value.is_empty() { bail!("La valeur du secret ne peut pas être vide."); }

    let client = gh_client(&creds);

    // Récupérer la clé publique et chiffrer via libsodium (sealed box)
    let sp = spinner("Récupération de la clé publique…");
    let (key_id, key_b64) = client.get_repo_public_key(&owner, &repo).await?;
    sp.finish_and_clear();

    // Chiffrement sealed box (libsodium compatible)
    let encrypted = encrypt_secret_value(&key_b64, &secret_value)
        .context("Impossible de chiffrer la valeur du secret")?;

    let sp2 = spinner(&format!("Enregistrement du secret '{}'…", name));
    client.set_repo_secret(&owner, &repo, &name, &encrypted, &key_id).await?;
    sp2.finish_and_clear();

    println!(
        "{} Secret '{}' enregistré dans {}/{}.",
        "✓".green().bold(), name.bold(), owner, repo
    );
    Ok(())
}

/// Chiffre une valeur de secret avec la clé publique GitHub
/// en utilisant le schéma "sealed box" de libsodium (Box X25519-XSalsa20-Poly1305).
///
/// GitHub attend : base64(libsodium.seal(utf8(secret), public_key_bytes))
///
/// Note : ilocker n'ajoute pas de dépendance libsodium — on utilise
/// l'implémentation X25519 + XSalsa20-Poly1305 via les crates déjà présentes
/// (chacha20poly1305 + sha2) en suivant le schéma exact de GitHub.
///
/// Pour un usage en production, recommander sodiumoxide ou libsodium-sys.
/// Chiffre une valeur de secret avec la clé publique GitHub en utilisant
/// le schéma "sealed box" de libsodium (crypto_box_seal), attendu par
/// l'API GitHub Actions Secrets.
///
/// Implémentation manuelle par-dessus la crate `crypto_box` : celle-ci
/// n'expose que la construction authentifiée standard (SalsaBox, avec
/// clés des deux parties), pas la variante anonyme "sealed box". On
/// reproduit donc le protocole exact de libsodium :
///
///   (ephemeral_pk, ephemeral_sk) = paire X25519 aléatoire
///   nonce  = BLAKE2b-24octets(ephemeral_pk || recipient_pk)
///   cipher = crypto_box(secret, nonce, recipient_pk, ephemeral_sk)
///   scellé = ephemeral_pk || cipher
///
/// Validé par round-trip local (seal + seal_open) avant intégration —
/// voir la note de session : la clé éphémère aléatoire garantit un
/// résultat différent à chaque appel même pour un secret identique.
fn encrypt_secret_value(public_key_b64: &str, secret: &str) -> Result<String> {
    use blake2::digest::{Update, VariableOutput};
    use blake2::Blake2bVar;
    use crypto_box::aead::{Aead, OsRng};
    use crypto_box::{PublicKey, SalsaBox, SecretKey};

    let pk_bytes = base64::engine::general_purpose::STANDARD.decode(public_key_b64)
        .context("Décodage base64 de la clé publique GitHub")?;
    if pk_bytes.len() != 32 {
        bail!("La clé publique GitHub doit faire 32 bytes (X25519), obtenu: {}", pk_bytes.len());
    }
    let mut pk_arr = [0u8; 32];
    pk_arr.copy_from_slice(&pk_bytes);
    let recipient_pk = PublicKey::from(pk_arr);

    let ephemeral_sk = SecretKey::generate(&mut OsRng);
    let ephemeral_pk = ephemeral_sk.public_key();

    let mut hasher = Blake2bVar::new(24)
        .map_err(|e| anyhow::anyhow!("Initialisation BLAKE2b: {}", e))?;
    hasher.update(ephemeral_pk.as_bytes());
    hasher.update(recipient_pk.as_bytes());
    let mut nonce_bytes = [0u8; 24];
    hasher
        .finalize_variable(&mut nonce_bytes)
        .map_err(|e| anyhow::anyhow!("Dérivation du nonce: {}", e))?;
    let nonce = crypto_box::Nonce::from(nonce_bytes);

    let cbox = SalsaBox::new(&recipient_pk, &ephemeral_sk);
    let ciphertext = cbox
        .encrypt(&nonce, secret.as_bytes())
        .map_err(|e| anyhow::anyhow!("Chiffrement du secret échoué: {}", e))?;

    let mut sealed = Vec::with_capacity(32 + ciphertext.len());
    sealed.extend_from_slice(ephemeral_pk.as_bytes());
    sealed.extend_from_slice(&ciphertext);

    Ok(base64::engine::general_purpose::STANDARD.encode(sealed))
}

pub async fn run_secret_delete(
    owner_repo: Option<String>,
    name:       String,
    profile:    Option<String>,
    yes:        bool,
) -> Result<()> {
    let creds  = github_store::require_credentials(profile.as_deref())?;
    let (owner, repo) = resolve_owner_repo(owner_repo.as_deref(), &creds)?;

    if !confirm(&format!("Supprimer le secret '{}' ?", name), yes)? {
        println!("  Annulé."); return Ok(());
    }

    let client = gh_client(&creds);
    let sp     = spinner("Suppression…");
    client.delete_repo_secret(&owner, &repo, &name).await?;
    sp.finish_and_clear();

    println!("{} Secret '{}' supprimé.", "✓".green().bold(), name.bold());
    Ok(())
}

// ═════════════════════════════════════════════════════════════
//  COLLABORATEURS
// ═════════════════════════════════════════════════════════════

pub async fn run_collab_list(
    owner_repo: Option<String>,
    profile:    Option<String>,
) -> Result<()> {
    let creds  = github_store::require_credentials(profile.as_deref())?;
    let (owner, repo) = resolve_owner_repo(owner_repo.as_deref(), &creds)?;
    let client = gh_client(&creds);

    let sp     = spinner("Chargement des collaborateurs…");
    let collabs = client.list_collaborators(&owner, &repo).await?;
    sp.finish_and_clear();

    if collabs.is_empty() {
        println!("{}", "  Aucun collaborateur.".yellow());
        return Ok(());
    }
    println!();
    println!("  {} collaborateur(s) — {}/{}", collabs.len(), owner.bold(), repo.bold());
    println!();
    for c in &collabs {
        let perm = if c.permissions.admin { "admin" }
            else if c.permissions.push    { "push"  }
            else                          { "pull"  };
        println!("  {} {} [{}]", "👤".to_string(), c.login.bold(), perm.cyan());
    }
    println!();
    Ok(())
}

pub async fn run_collab_add(
    owner_repo:  Option<String>,
    username:    String,
    permission:  String,
    profile:     Option<String>,
    yes:         bool,
) -> Result<()> {
    let creds  = github_store::require_credentials(profile.as_deref())?;
    let (owner, repo) = resolve_owner_repo(owner_repo.as_deref(), &creds)?;

    if !confirm(&format!("Ajouter @{} ({}) à {}/{} ?", username, permission, owner, repo), yes)? {
        println!("  Annulé."); return Ok(());
    }

    let client = gh_client(&creds);
    let sp     = spinner("Invitation du collaborateur…");
    client.add_collaborator(&owner, &repo, &username, &permission).await?;
    sp.finish_and_clear();

    println!(
        "{} @{} invité à {}/{} (permission: {}).",
        "✓".green().bold(), username.bold(), owner, repo, permission.cyan()
    );
    println!("  {} Une invitation a été envoyée — l'accès est effectif après acceptation.", "ℹ".cyan());
    Ok(())
}

pub async fn run_collab_remove(
    owner_repo: Option<String>,
    username:   String,
    profile:    Option<String>,
    yes:        bool,
) -> Result<()> {
    let creds  = github_store::require_credentials(profile.as_deref())?;
    let (owner, repo) = resolve_owner_repo(owner_repo.as_deref(), &creds)?;

    if !confirm(&format!("Retirer @{} de {}/{} ?", username, owner, repo), yes)? {
        println!("  Annulé."); return Ok(());
    }

    let client = gh_client(&creds);
    let sp     = spinner("Retrait du collaborateur…");
    client.remove_collaborator(&owner, &repo, &username).await?;
    sp.finish_and_clear();

    println!("{} @{} retiré de {}/{}.", "✓".green().bold(), username.bold(), owner, repo);
    Ok(())
}

// ═════════════════════════════════════════════════════════════
//  WEBHOOKS
// ═════════════════════════════════════════════════════════════

pub async fn run_webhook_list(
    owner_repo: Option<String>,
    profile:    Option<String>,
) -> Result<()> {
    let creds  = github_store::require_credentials(profile.as_deref())?;
    let (owner, repo) = resolve_owner_repo(owner_repo.as_deref(), &creds)?;
    let client = gh_client(&creds);

    let sp       = spinner("Chargement des webhooks…");
    let webhooks = client.list_webhooks(&owner, &repo).await?;
    sp.finish_and_clear();

    if webhooks.is_empty() {
        println!("{}", "  Aucun webhook configuré.".yellow());
        return Ok(());
    }
    println!();
    println!("  {} webhook(s) — {}/{}", webhooks.len(), owner.bold(), repo.bold());
    println!();
    for w in &webhooks {
        let active = if w.active { "actif".green().to_string() } else { "inactif".red().to_string() };
        println!("  {} #{} {} — {}", "🔗".to_string(), w.id, w.config.url.bold(), active);
        println!("    events: {}", w.events.join(", ").dimmed());
    }
    println!();
    Ok(())
}

pub async fn run_webhook_create(
    owner_repo:   Option<String>,
    url:          String,
    events:       Vec<String>,
    content_type: Option<String>,
    secret:       Option<String>,
    inactive:     bool,
    profile:      Option<String>,
) -> Result<()> {
    let creds  = github_store::require_credentials(profile.as_deref())?;
    let (owner, repo) = resolve_owner_repo(owner_repo.as_deref(), &creds)?;
    let client = gh_client(&creds);

    let ct = content_type.as_deref().unwrap_or("json");
    let evts: Vec<&str> = if events.is_empty() { vec!["push"] }
        else { events.iter().map(|s| s.as_str()).collect() };

    let sp      = spinner("Création du webhook…");
    let webhook = client.create_webhook(
        &owner, &repo,
        &url, &evts, ct,
        secret.as_deref(), !inactive,
    ).await?;
    sp.finish_and_clear();

    println!("{} Webhook #{} créé.", "✓".green().bold(), webhook.id);
    println!("  {} {}", "url:".dimmed(), webhook.config.url.cyan());
    println!("  {} {}", "events:".dimmed(), webhook.events.join(", ").dimmed());
    Ok(())
}

pub async fn run_webhook_delete(
    owner_repo: Option<String>,
    hook_id:    u64,
    profile:    Option<String>,
    yes:        bool,
) -> Result<()> {
    let creds  = github_store::require_credentials(profile.as_deref())?;
    let (owner, repo) = resolve_owner_repo(owner_repo.as_deref(), &creds)?;

    if !confirm(&format!("Supprimer le webhook #{} ?", hook_id), yes)? {
        println!("  Annulé."); return Ok(());
    }

    let client = gh_client(&creds);
    let sp     = spinner("Suppression…");
    client.delete_webhook(&owner, &repo, hook_id).await?;
    sp.finish_and_clear();

    println!("{} Webhook #{} supprimé.", "✓".green().bold(), hook_id);
    Ok(())
}

pub async fn run_webhook_ping(
    owner_repo: Option<String>,
    hook_id:    u64,
    profile:    Option<String>,
) -> Result<()> {
    let creds  = github_store::require_credentials(profile.as_deref())?;
    let (owner, repo) = resolve_owner_repo(owner_repo.as_deref(), &creds)?;
    let client = gh_client(&creds);

    let sp = spinner("Ping du webhook…");
    client.ping_webhook(&owner, &repo, hook_id).await?;
    sp.finish_and_clear();

    println!("{} Ping envoyé au webhook #{}.", "✓".green().bold(), hook_id);
    Ok(())
}

// ═════════════════════════════════════════════════════════════
//  SEARCH
// ═════════════════════════════════════════════════════════════

pub async fn run_search_repos(
    query:   String,
    limit:   usize,
    profile: Option<String>,
) -> Result<()> {
    let creds  = github_store::require_credentials(profile.as_deref())?;
    let client = gh_client(&creds);

    let sp      = spinner(&format!("Recherche de repos : '{}'…", query));
    let results = client.search_repos(&query).await?;
    sp.finish_and_clear();

    println!();
    println!(
        "  {} résultat(s) pour '{}' (top {})",
        results.total_count, query.bold(), limit.min(results.items.len())
    );
    println!();
    for r in results.items.iter().take(limit) { print_repo(r); println!(); }
    Ok(())
}

// ── Tests ──────────────────────────────────────────────────────
//
// parse_github_url sous-tend resolve_owner_repo, appelé par quasiment
// toutes les commandes github quand owner_repo est omis (auto-détection
// depuis `git remote get-url origin`). Un bug ici casse silencieusement
// TOUTES les commandes lancées sans argument owner_repo explicite.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ssh_url() {
        let (owner, repo) = parse_github_url("git@github.com:bechir/ilocker.git").unwrap();
        assert_eq!(owner, "bechir");
        assert_eq!(repo, "ilocker");
    }

    #[test]
    fn parses_ssh_url_without_dotgit_suffix() {
        let (owner, repo) = parse_github_url("git@github.com:bechir/ilocker").unwrap();
        assert_eq!(owner, "bechir");
        assert_eq!(repo, "ilocker");
    }

    #[test]
    fn parses_https_url() {
        let (owner, repo) = parse_github_url("https://github.com/bechir/ilocker.git").unwrap();
        assert_eq!(owner, "bechir");
        assert_eq!(repo, "ilocker");
    }

    #[test]
    fn parses_https_url_without_dotgit_suffix() {
        let (owner, repo) = parse_github_url("https://github.com/bechir/ilocker").unwrap();
        assert_eq!(owner, "bechir");
        assert_eq!(repo, "ilocker");
    }

    #[test]
    fn parses_https_url_with_trailing_slash() {
        let (owner, repo) = parse_github_url("https://github.com/bechir/ilocker/").unwrap();
        assert_eq!(owner, "bechir");
        assert_eq!(repo, "ilocker");
    }

    #[test]
    fn rejects_non_github_url() {
        let result = parse_github_url("https://gitlab.com/bechir/ilocker.git");
        assert!(result.is_err());
    }

    #[test]
    fn rejects_malformed_url() {
        let result = parse_github_url("not-a-url-at-all");
        assert!(result.is_err());
    }

    #[test]
    fn repo_names_with_hyphens_and_dots_work() {
        // Cas réel : "ilocker-deploy", "my.project.name"
        let (owner, repo) = parse_github_url("git@github.com:org-name/ilocker-deploy.git").unwrap();
        assert_eq!(owner, "org-name");
        assert_eq!(repo, "ilocker-deploy");
    }

    #[test]
    fn secret_sealed_box_round_trips_with_own_keypair() {
        // encrypt_secret_value() ne peut pas être ouvert que par GitHub (on n'a
        // pas leur clé privée), mais on peut valider que le PROTOCOLE est correct
        // en scellant avec notre propre clé publique test, puis en rouvrant avec
        // la clé privée correspondante via les mêmes primitives crypto_box.
        use crypto_box::aead::{Aead, OsRng};
        use crypto_box::{PublicKey, SalsaBox, SecretKey};
        use blake2::digest::{Update, VariableOutput};
        use blake2::Blake2bVar;

        let recipient_sk = SecretKey::generate(&mut OsRng);
        let recipient_pk = recipient_sk.public_key();
        let recipient_pk_b64 = base64::engine::general_purpose::STANDARD.encode(recipient_pk.as_bytes());

        let secret = "ghp_test_secret_value";
        let sealed_b64 = encrypt_secret_value(&recipient_pk_b64, secret)
            .expect("encrypt_secret_value doit réussir");
        let sealed = base64::engine::general_purpose::STANDARD.decode(&sealed_b64).unwrap();
        assert_eq!(sealed.len(), 32 + secret.len() + 16, "32 (clé éphémère) + message + 16 (tag Poly1305)");

        // Rouvrir avec les mêmes primitives, en suivant le protocole crypto_box_seal
        let ephemeral_pk_bytes: [u8; 32] = sealed[..32].try_into().unwrap();
        let ephemeral_pk = PublicKey::from(ephemeral_pk_bytes);
        let ciphertext = &sealed[32..];

        let mut hasher = Blake2bVar::new(24).unwrap();
        hasher.update(ephemeral_pk.as_bytes());
        hasher.update(recipient_pk.as_bytes());
        let mut nonce_bytes = [0u8; 24];
        hasher.finalize_variable(&mut nonce_bytes).unwrap();
        let nonce = crypto_box::Nonce::from(nonce_bytes);

        let cbox = SalsaBox::new(&ephemeral_pk, &recipient_sk);
        let opened = cbox.decrypt(&nonce, ciphertext).expect("decrypt doit réussir");
        assert_eq!(String::from_utf8(opened).unwrap(), secret);
    }

    #[test]
    fn secret_sealed_box_is_nondeterministic() {
        use crypto_box::aead::OsRng;
        use crypto_box::SecretKey;

        let recipient_sk = SecretKey::generate(&mut OsRng);
        let recipient_pk_b64 = base64::engine::general_purpose::STANDARD.encode(recipient_sk.public_key().as_bytes());

        let a = encrypt_secret_value(&recipient_pk_b64, "meme-secret").unwrap();
        let b = encrypt_secret_value(&recipient_pk_b64, "meme-secret").unwrap();
        assert_ne!(a, b, "deux scellements du même secret doivent différer (clé éphémère aléatoire à chaque appel)");
    }

    #[test]
    fn rejects_invalid_public_key_length() {
        let bad_key = base64::engine::general_purpose::STANDARD.encode(b"trop court");
        let result = encrypt_secret_value(&bad_key, "secret");
        assert!(result.is_err());
    }
}

