// ============================================================
//  commands/deploy.rs — iloc deploy, l'orchestrateur multi-provider
//
//  Architecture en deux phases strictement séparées :
//    build_plan()   — LECTURE SEULE. Scanne, interroge les APIs en
//                     GET uniquement, décide adopter/créer pour
//                     chaque provider. Ne mute jamais rien. C'est
//                     ce que `--dry-run` utilise et qui s'arrête là.
//    execute_plan() — Les actions réelles : création/adoption,
//                     migrations, sync env, déploiement, écriture
//                     de l'état.
//
//  LEÇON STRUCTURELLE APPLIQUÉE : le bug "--yes oublie un prompt"
//  s'est répété 3 fois (github repo create, vercel project create,
//  7 commandes supabase) malgré avoir été corrigé et documenté à
//  chaque fois. Ici, un seul point d'entrée `confirm()` reçoit le
//  DeployContext et c'est la SEULE fonction autorisée à lire
//  stdin dans tout ce fichier — structurellement impossible
//  d'oublier un prompt parce qu'il n'y a qu'un seul endroit où
//  on peut en écrire un.
// ============================================================

use crate::deploy_state::{self, DeployState, GithubLink, LastDeploy, SupabaseLink, VercelLink};
use crate::github_client::{GhRepo, GitHubClient};
use crate::github_store;
use crate::scanner::{self, ProjectScan};
use crate::supabase_client::{SbProject, SupabaseClient};
use crate::supabase_store;
use crate::vercel_client::{VcProject, VercelClient};
use crate::vercel_store;

use anyhow::{bail, Context, Result};
use colored::Colorize;
use indicatif::{ProgressBar, ProgressStyle};
use std::path::Path;

// ═════════════════════════════════════════════════════════════
//  Contexte partagé — le SEUL chemin par lequel `yes` circule
// ═════════════════════════════════════════════════════════════

pub struct DeployContext {
    pub yes:              bool,
    pub force_new:         bool,
    pub dry_run:           bool,
    pub skip_github:       bool,
    pub skip_vercel:       bool,
    pub skip_supabase:     bool,
    pub github_profile:    Option<String>,
    pub vercel_profile:    Option<String>,
    pub supabase_profile:  Option<String>,
    pub org:               Option<String>,
    pub team:              Option<String>,
}

/// SEUL point d'entrée pour demander une confirmation dans tout ce
/// fichier. Toute nouvelle étape de l'orchestrateur qui a besoin
/// d'un accord utilisateur DOIT passer par ici — jamais un
/// `prompt()` direct ailleurs.
fn confirm(ctx: &DeployContext, question: &str) -> Result<bool> {
    if ctx.yes { return Ok(true); }
    if std::env::var("ILOC_AUTO_CONFIRM").as_deref() == Ok("1") { return Ok(true); }
    use std::io::Write;
    print!("  {} [y/N] ", question);
    std::io::stdout().flush()?;
    let mut buf = String::new();
    std::io::stdin().read_line(&mut buf)?;
    let ans = buf.trim();
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

// ═════════════════════════════════════════════════════════════
//  Résolutions par provider — le résultat de la logique adopter/créer
// ═════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AdoptSource {
    /// Trouvé dans .ilocker/deploy.toml et vérifié encore existant.
    State,
    /// Trouvé via un fichier de liaison natif (.git, .vercel, .supabase).
    LocalLink,
    /// Aucune trace locale — trouvé par recherche de nom à distance.
    NameMatch,
}

impl AdoptSource {
    fn label(self) -> &'static str {
        match self {
            AdoptSource::State     => "état ilocker",
            AdoptSource::LocalLink => "liaison locale",
            AdoptSource::NameMatch => "trouvé par nom",
        }
    }
}

pub enum GithubResolution {
    Adopt(Box<GhRepo>, AdoptSource),
    Create,
    NotConnected,
    Skipped,
}

pub enum VercelResolution {
    Adopt(Box<VcProject>, AdoptSource),
    Create,
    NotConnected,
    Skipped,
}

pub enum SupabaseResolution {
    Adopt(Box<SbProject>, AdoptSource),
    Create,
    /// Plusieurs projets Supabase partagent ce nom — Supabase ne
    /// garantit pas l'unicité du nom dans une org, contrairement
    /// à GitHub et Vercel. On refuse de deviner.
    Ambiguous(Vec<SbProject>),
    NotConnected,
    Skipped,
}

// ═════════════════════════════════════════════════════════════
//  Le plan complet — produit de build_plan(), lu par execute_plan()
// ═════════════════════════════════════════════════════════════

pub struct DeployPlan {
    pub scan:               ProjectScan,
    pub state:               DeployState,
    pub github:               GithubResolution,
    pub vercel:               VercelResolution,
    pub supabase:             SupabaseResolution,
    /// (version, name) des migrations locales absentes du serveur —
    /// jamais le contenu SQL ici, juste de quoi les afficher.
    pub pending_migrations:   Vec<(String, String)>,
    /// Clés des variables d'env à synchroniser (jamais les valeurs).
    pub env_vars_to_sync:     Vec<String>,
    /// true si rien n'a changé depuis le dernier déploiement connu
    /// (même SHA git, pas de migration en attente, pas d'env modifié).
    pub nothing_to_deploy:    bool,
}

// ═════════════════════════════════════════════════════════════
//  PHASE 1 — build_plan() : lecture seule, jamais de mutation
// ═════════════════════════════════════════════════════════════

pub async fn build_plan(dir: &Path, ctx: &DeployContext) -> Result<DeployPlan> {
    let scan  = scanner::scan_project(dir)?;
    let state = deploy_state::load_state(dir)?;

    let github = if ctx.skip_github {
        GithubResolution::Skipped
    } else {
        resolve_github(&scan, &state, ctx).await?
    };

    let supabase = if ctx.skip_supabase || (!scan.uses_supabase && state.supabase.is_none()) {
        SupabaseResolution::Skipped
    } else {
        resolve_supabase(&scan, &state, ctx).await?
    };

    let vercel = if ctx.skip_vercel {
        VercelResolution::Skipped
    } else {
        resolve_vercel(&scan, &state, ctx).await?
    };

    // Migrations en attente — seulement calculable si on a un projet
    // Supabase résolu (adopté) ET un dossier de migrations local.
    let pending_migrations = match (&supabase, &scan.supabase_migrations_dir) {
        (SupabaseResolution::Adopt(proj, _), Some(mig_dir)) => {
            compute_pending_migrations(dir, proj.project_ref(), mig_dir, ctx).await.unwrap_or_default()
        }
        _ => Vec::new(),
    };

    // Variables d'env à synchroniser : celles du fichier .env local
    // dont le hash diffère de ce qui est enregistré dans l'état.
    let env_vars_to_sync = if let Some(env_file) = &scan.env_file {
        compute_env_diff(env_file, &state, "vercel").unwrap_or_default()
    } else {
        Vec::new()
    };

    // "Rien à déployer" : même SHA git que le dernier déploiement
    // connu, aucune migration en attente, aucune variable modifiée.
    let current_sha = current_git_sha(dir);
    let nothing_to_deploy = matches!(&vercel, VercelResolution::Adopt(_, _))
        && pending_migrations.is_empty()
        && env_vars_to_sync.is_empty()
        && state.last_deploy.as_ref()
            .and_then(|d| d.git_sha.as_ref())
            .zip(current_sha.as_ref())
            .map(|(a, b)| a == b)
            .unwrap_or(false);

    Ok(DeployPlan {
        scan, state, github, vercel, supabase,
        pending_migrations, env_vars_to_sync, nothing_to_deploy,
    })
}

// ── Réconciliation GitHub ─────────────────────────────────────────

async fn resolve_github(scan: &ProjectScan, state: &DeployState, ctx: &DeployContext) -> Result<GithubResolution> {
    let creds = match github_store::require_credentials(ctx.github_profile.as_deref()) {
        Ok(c) => c,
        Err(_) => return Ok(GithubResolution::NotConnected),
    };
    let client = GitHubClient::new(&creds.token, &creds.api_url);

    // 1. État ilocker
    if let Some(link) = &state.github {
        if let Ok(repo) = client.get_repo(&link.owner, &link.repo).await {
            return Ok(GithubResolution::Adopt(Box::new(repo), AdoptSource::State));
        }
        // Enregistré mais introuvable à distance — on ne s'arrête pas
        // là, on retente les niveaux suivants plutôt que d'échouer.
    }

    // 2. Liaison native (.git remote)
    if let Some((owner, repo)) = &scan.git_remote {
        if let Ok(r) = client.get_repo(owner, repo).await {
            return Ok(GithubResolution::Adopt(Box::new(r), AdoptSource::LocalLink));
        }
    }

    // 3. Recherche par nom sous le compte connecté
    if !ctx.force_new {
        if let Ok(r) = client.get_repo(&creds.login, &scan.project_name).await {
            return Ok(GithubResolution::Adopt(Box::new(r), AdoptSource::NameMatch));
        }
    }

    // 4. Rien trouvé — création
    Ok(GithubResolution::Create)
}

// ── Réconciliation Vercel ─────────────────────────────────────────

async fn resolve_vercel(scan: &ProjectScan, state: &DeployState, ctx: &DeployContext) -> Result<VercelResolution> {
    let creds = match vercel_store::require_credentials(ctx.vercel_profile.as_deref()) {
        Ok(c) => c,
        Err(_) => return Ok(VercelResolution::NotConnected),
    };
    let client = VercelClient::new_from_credentials(&creds);

    // 1. État ilocker
    if let Some(link) = &state.vercel {
        if let Ok(p) = client.get_project(&link.project_id).await {
            return Ok(VercelResolution::Adopt(Box::new(p), AdoptSource::State));
        }
    }

    // 2. Liaison native (.vercel/project.json)
    if let Some(pid) = &scan.vercel_linked {
        if let Ok(p) = client.get_project(pid).await {
            return Ok(VercelResolution::Adopt(Box::new(p), AdoptSource::LocalLink));
        }
    }

    // 3. Recherche par nom — Vercel accepte un lookup direct id OU nom
    if !ctx.force_new {
        if let Ok(p) = client.get_project(&scan.project_name).await {
            return Ok(VercelResolution::Adopt(Box::new(p), AdoptSource::NameMatch));
        }
    }

    Ok(VercelResolution::Create)
}

// ── Réconciliation Supabase (avec gestion de l'ambiguïté de nom) ──

async fn resolve_supabase(scan: &ProjectScan, state: &DeployState, ctx: &DeployContext) -> Result<SupabaseResolution> {
    let creds = match supabase_store::require_credentials(ctx.supabase_profile.as_deref()) {
        Ok(c) => c,
        Err(_) => return Ok(SupabaseResolution::NotConnected),
    };
    let client = SupabaseClient::new_from_credentials(&creds);

    // 1. État ilocker
    if let Some(link) = &state.supabase {
        if let Ok(p) = client.get_project(&link.project_ref).await {
            return Ok(SupabaseResolution::Adopt(Box::new(p), AdoptSource::State));
        }
    }

    // 2. Liaison native (.supabase/project.json)
    if let Some(pref) = &scan.supabase_linked {
        if let Ok(p) = client.get_project(pref).await {
            return Ok(SupabaseResolution::Adopt(Box::new(p), AdoptSource::LocalLink));
        }
    }

    // 3. Recherche par nom — Supabase N'A PAS de lookup direct par nom,
    // et NE GARANTIT PAS l'unicité du nom dans une org (contrairement
    // à GitHub et Vercel). On liste et on filtre, et si plusieurs
    // projets partagent ce nom, on refuse de deviner lequel adopter.
    if !ctx.force_new {
        if let Ok(projects) = client.list_projects().await {
            let matches: Vec<SbProject> = projects.into_iter()
                .filter(|p| p.name == scan.project_name)
                .collect();
            match matches.len() {
                0 => {}
                1 => return Ok(SupabaseResolution::Adopt(Box::new(matches.into_iter().next().unwrap()), AdoptSource::NameMatch)),
                _ => return Ok(SupabaseResolution::Ambiguous(matches)),
            }
        }
    }

    Ok(SupabaseResolution::Create)
}

// ── Migrations en attente (réutilise la logique idempotente déjà
//    testée dans commands/supabase.rs, dupliquée ici en version
//    lecture-seule car celle-ci ne doit produire aucune sortie
//    console — c'est le plan qui décide de l'affichage) ──

async fn compute_pending_migrations(
    _project_dir: &Path,
    project_ref: &str,
    migrations_dir: &Path,
    ctx: &DeployContext,
) -> Result<Vec<(String, String)>> {
    let creds  = supabase_store::require_credentials(ctx.supabase_profile.as_deref())?;
    let client = SupabaseClient::new_from_credentials(&creds);

    let mut entries: Vec<_> = std::fs::read_dir(migrations_dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("sql"))
        .collect();
    entries.sort_by_key(|e| e.file_name());

    let mut local = Vec::new();
    for entry in entries {
        let filename = entry.file_name().to_string_lossy().to_string();
        let stem = filename.trim_end_matches(".sql");
        let (version, name) = match stem.split_once('_') {
            Some((v, n)) if v.chars().all(|c| c.is_ascii_digit()) => (v.to_string(), n.to_string()),
            _ => (stem.to_string(), stem.to_string()),
        };
        local.push((version, name));
    }

    let remote = client.list_migrations(project_ref).await?;
    let remote_versions: std::collections::HashSet<String> = remote.iter().map(|m| m.version.clone()).collect();

    Ok(local.into_iter().filter(|(v, _)| !remote_versions.contains(v)).collect())
}

// ── Diff des variables d'environnement (par hash, jamais par valeur
//    affichée) ──

fn compute_env_diff(env_file: &Path, state: &DeployState, provider: &str) -> Result<Vec<String>> {
    let content = std::fs::read_to_string(env_file)?;
    let mut changed = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') { continue; }
        if let Some((k, v)) = line.split_once('=') {
            let key = k.trim();
            let val = v.trim().trim_matches('"').trim_matches('\'');
            if key.is_empty() { continue; }
            let hash_key = deploy_state::env_hash_key(provider, key);
            let current_hash = deploy_state::hash_value(val);
            let matches_stored = state.env_hashes.get(&hash_key) == Some(&current_hash);
            if !matches_stored {
                changed.push(key.to_string());
            }
        }
    }
    Ok(changed)
}

fn current_git_sha(dir: &Path) -> Option<String> {
    std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(dir)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

// ═════════════════════════════════════════════════════════════
//  Affichage du plan
// ═════════════════════════════════════════════════════════════

fn print_plan(plan: &DeployPlan) {
    println!();
    println!("{}", "  ilocker deploy — plan".bold());
    println!();

    let mut fw = String::new();
    if let Some(f) = &plan.scan.framework { fw.push_str(f); } else { fw.push_str("non détecté"); }
    println!("  {} {}  ·  {} {}", "projet:".dimmed(), plan.scan.project_name.bold(), "framework:".dimmed(), fw.cyan());
    println!();

    print_resolution_line("GitHub", match &plan.github {
        GithubResolution::Adopt(r, src) => format!("adopter {} ({})", r.full_name.bold(), src.label().dimmed()),
        GithubResolution::Create         => "créer un nouveau repo".to_string(),
        GithubResolution::NotConnected   => "non connecté — `iloc connect github`".yellow().to_string(),
        GithubResolution::Skipped        => "ignoré (--skip-github)".dimmed().to_string(),
    });

    print_resolution_line("Supabase", match &plan.supabase {
        SupabaseResolution::Adopt(p, src) => format!("adopter {} ({})", p.name.bold(), src.label().dimmed()),
        SupabaseResolution::Create          => "créer un nouveau projet".to_string(),
        SupabaseResolution::Ambiguous(list) => format!(
            "{} — {} projets partagent ce nom, précisez --supabase-ref",
            "ambigu".red(), list.len()
        ),
        SupabaseResolution::NotConnected    => "non connecté — `iloc connect supabase`".yellow().to_string(),
        SupabaseResolution::Skipped         => "ignoré".dimmed().to_string(),
    });

    if !plan.pending_migrations.is_empty() {
        println!("    {} {} migration(s) en attente :", "→".dimmed(), plan.pending_migrations.len());
        for (version, name) in &plan.pending_migrations {
            println!("      {} {} — {}", "○".yellow(), version.dimmed(), name);
        }
    }

    print_resolution_line("Vercel", match &plan.vercel {
        VercelResolution::Adopt(p, src) => format!("adopter {} ({})", p.name.bold(), src.label().dimmed()),
        VercelResolution::Create         => "créer un nouveau projet".to_string(),
        VercelResolution::NotConnected   => "non connecté — `iloc connect vercel`".yellow().to_string(),
        VercelResolution::Skipped        => "ignoré (--skip-vercel)".dimmed().to_string(),
    });

    if !plan.env_vars_to_sync.is_empty() {
        println!(
            "    {} {} variable(s) d'environnement à synchroniser : {}",
            "→".dimmed(), plan.env_vars_to_sync.len(), plan.env_vars_to_sync.join(", ").dimmed()
        );
    }

    println!();
    if plan.nothing_to_deploy {
        println!("  {} Rien à déployer — tout est déjà à jour.", "✓".green().bold());
    }
    println!();
}

fn print_resolution_line(provider: &str, detail: String) {
    println!("  {} {}", format!("{}:", provider).cyan().bold(), detail);
}

// ═════════════════════════════════════════════════════════════
//  PHASE 2 — execute_plan() : les actions réelles
// ═════════════════════════════════════════════════════════════

pub async fn execute_plan(dir: &Path, mut plan: DeployPlan, ctx: &DeployContext) -> Result<()> {
    let mut state = plan.state.clone_for_update();

    // ── GitHub ──────────────────────────────────────────────
    let github_link = match &plan.github {
        GithubResolution::Adopt(repo, _) => {
            let link = GithubLink {
                owner: repo.full_name.split('/').next().unwrap_or_default().to_string(),
                repo:  repo.name.clone(),
                linked_at: state.github.as_ref().map(|g| g.linked_at.clone())
                    .unwrap_or_else(|| chrono::Utc::now().to_rfc3339()),
            };
            ensure_local_git(dir, &repo.ssh_url, &repo.default_branch, ctx)?;
            Some((link, repo.clone_url.clone(), repo.default_branch.clone()))
        }
        GithubResolution::Create => {
            let creds  = github_store::require_credentials(ctx.github_profile.as_deref())?;
            let client = GitHubClient::new(&creds.token, &creds.api_url);
            let sp     = spinner(&format!("Création du repo GitHub '{}'…", plan.scan.project_name));
            let repo   = client.create_repo(
                &plan.scan.project_name, None, true, true, None, None, None, None,
            ).await?;
            sp.finish_and_clear();
            println!("  {} Repo GitHub créé : {}", "✓".green(), repo.html_url.cyan());
            let link = GithubLink {
                owner: creds.login.clone(), repo: repo.name.clone(),
                linked_at: chrono::Utc::now().to_rfc3339(),
            };
            ensure_local_git(dir, &repo.ssh_url, &repo.default_branch, ctx)?;
            Some((link, repo.clone_url.clone(), repo.default_branch.clone()))
        }
        GithubResolution::NotConnected | GithubResolution::Skipped => None,
    };
    if let Some((link, _, _)) = &github_link {
        state.github = Some(link.clone());
    }

    // ── Supabase ────────────────────────────────────────────
    let supabase_ref = match &plan.supabase {
        SupabaseResolution::Adopt(proj, _) => {
            state.supabase = Some(SupabaseLink {
                project_ref: proj.project_ref().to_string(),
                org_id:      proj.organization_id.clone(),
                linked_at:   state.supabase.as_ref().map(|s| s.linked_at.clone())
                    .unwrap_or_else(|| chrono::Utc::now().to_rfc3339()),
            });
            Some(proj.project_ref().to_string())
        }
        SupabaseResolution::Create => {
            let creds  = supabase_store::require_credentials(ctx.supabase_profile.as_deref())?;
            let client = SupabaseClient::new_from_credentials(&creds);
            let org_id = ctx.org.clone()
                .or_else(|| creds.default_org_id.clone())
                .context("Aucune organisation Supabase déterminable — précisez --org")?;
            let sp = spinner(&format!("Création du projet Supabase '{}' (peut prendre 1-2 min)…", plan.scan.project_name));
            let password = generate_password();
            let proj = client.create_project(&plan.scan.project_name, &org_id, "eu-west-1", &password).await?;
            sp.finish_and_clear();
            println!("  {} Projet Supabase créé : {}", "✓".green(), proj.project_ref().cyan());
            println!("    {} mot de passe DB (conservez-le) : {}", "🔑".to_string(), password.yellow());
            state.supabase = Some(SupabaseLink {
                project_ref: proj.project_ref().to_string(), org_id,
                linked_at: chrono::Utc::now().to_rfc3339(),
            });
            Some(proj.project_ref().to_string())
        }
        SupabaseResolution::Ambiguous(_) => {
            bail!("Plusieurs projets Supabase partagent ce nom — précisez le projet à utiliser manuellement.");
        }
        SupabaseResolution::NotConnected | SupabaseResolution::Skipped => None,
    };

    // Migrations — jamais réappliquées, uniquement les nouvelles.
    //
    // Note : sur une création fraîche (SupabaseResolution::Create),
    // plan.pending_migrations est toujours vide car calculé AVANT la
    // création du projet (aucun serveur à interroger à ce moment-là).
    // Plutôt que d'attendre un second `iloc deploy` pour les pousser,
    // on relance systématiquement la détection ici — apply_pending_
    // migrations interroge le serveur en temps réel et ne réapplique
    // jamais une migration déjà passée, donc c'est sûr de l'appeler
    // à chaque fois qu'un dossier de migrations existe, sans risque
    // de duplication même si plan.pending_migrations était déjà à jour.
    if let Some(pref) = &supabase_ref {
        if let Some(mig_dir) = &plan.scan.supabase_migrations_dir {
            let just_created = matches!(&plan.supabase, SupabaseResolution::Create);
            let has_known_pending = !plan.pending_migrations.is_empty();

            if just_created || has_known_pending {
                let count_hint = if just_created {
                    "toutes les migrations locales (nouveau projet)".to_string()
                } else {
                    format!("{} migration(s) en attente", plan.pending_migrations.len())
                };
                if confirm(ctx, &format!("Appliquer {} ?", count_hint))? {
                    let _ = mig_dir;
                    apply_pending_migrations(dir, pref, &plan.scan, ctx).await?;
                }
            }
        }
    }

    // ── Vercel ──────────────────────────────────────────────
    match &plan.vercel {
        VercelResolution::Adopt(proj, _) => {
            state.vercel = Some(VercelLink {
                project_id: proj.id.clone(), team_id: ctx.team.clone(),
                linked_at: state.vercel.as_ref().map(|v| v.linked_at.clone())
                    .unwrap_or_else(|| chrono::Utc::now().to_rfc3339()),
            });
        }
        VercelResolution::Create => {
            let creds  = vercel_store::require_credentials(ctx.vercel_profile.as_deref())?;
            let client = VercelClient::new_from_credentials(&creds);
            let sp = spinner(&format!("Création du projet Vercel '{}'…", plan.scan.project_name));

            let (git_repo, git_provider, git_branch) = match &github_link {
                Some((link, _, branch)) => (
                    Some(format!("{}/{}", link.owner, link.repo)),
                    Some("github"),
                    Some(branch.as_str()),
                ),
                None => (None, None, None),
            };

            let proj = client.create_project(
                &plan.scan.project_name,
                plan.scan.framework.as_deref(),
                None, None, None, None, None,
                git_repo.as_deref(), git_provider, git_branch,
            ).await?;
            sp.finish_and_clear();
            println!("  {} Projet Vercel créé : {}", "✓".green(), proj.name.cyan());
            if git_repo.is_some() {
                println!("    {} Lié à GitHub — les futurs push déclencheront un déploiement automatique.", "ℹ".cyan());
            }
            state.vercel = Some(VercelLink {
                project_id: proj.id.clone(), team_id: ctx.team.clone(),
                linked_at: chrono::Utc::now().to_rfc3339(),
            });
        }
        VercelResolution::NotConnected | VercelResolution::Skipped => {}
    }

    // Sync des variables d'environnement Vercel (diff par hash)
    let vercel_resolved = matches!(&plan.vercel, VercelResolution::Adopt(_, _) | VercelResolution::Create);
    if vercel_resolved {
        if let Some(env_file) = &plan.scan.env_file {
            let project_id = match &plan.vercel {
                VercelResolution::Adopt(p, _) => p.id.clone(),
                _ => state.vercel.as_ref().map(|v| v.project_id.clone()).unwrap_or_default(),
            };
            if !plan.env_vars_to_sync.is_empty() {
                sync_env_to_vercel(&project_id, env_file, &mut state, ctx).await?;
            }
        }
    }

    // Déploiement Vercel (seulement si un repo git est lié — voir
    // note honnête ci-dessous sur la limite connue de cette v1)
    if !plan.nothing_to_deploy {
        if let (Some(project_id), Some((_, _, branch))) = (
            state.vercel.as_ref().map(|v| v.project_id.clone()),
            &github_link,
        ) {
            if confirm(ctx, "Déclencher un déploiement Vercel ?")? {
                trigger_vercel_deploy(&project_id, branch, ctx).await?;
            }
        } else if state.vercel.is_some() && github_link.is_none() {
            println!();
            println!(
                "  {} Projet Vercel prêt mais aucun repo GitHub lié dans cette exécution.",
                "ℹ".cyan()
            );
            println!(
                "    Si ce projet Vercel n'est pas déjà connecté à un repo Git, lancez `iloc vercel deploy` manuellement une première fois."
            );
        }
    }

    state.last_deploy = Some(LastDeploy {
        git_sha: current_git_sha(dir),
        vercel_deployment_id: None,
        deployed_at: chrono::Utc::now().to_rfc3339(),
    });
    deploy_state::save_state(dir, &state)?;

    println!();
    println!("{} Terminé.", "✓".green().bold());
    if let Some((link, _, _)) = &github_link {
        println!("  {} https://github.com/{}/{}", "github:".dimmed(), link.owner, link.repo);
    }
    if let Some(pref) = &supabase_ref {
        println!("  {} https://{}.supabase.co", "supabase:".dimmed(), pref);
    }
    println!();
    Ok(())
}

// ── git init / remote / commit / push ──────────────────────────

fn ensure_local_git(dir: &Path, ssh_url: &str, default_branch: &str, ctx: &DeployContext) -> Result<()> {
    let git = |args: &[&str]| -> Result<std::process::Output> {
        std::process::Command::new("git").args(args).current_dir(dir).output()
            .context("git non disponible dans le PATH")
    };

    if !dir.join(".git").exists() {
        git(&["init", "-q"])?;
        println!("  {} dépôt git local initialisé", "✓".green());
    }

    let has_origin = git(&["remote", "get-url", "origin"])?.status.success();
    if !has_origin {
        git(&["remote", "add", "origin", ssh_url])?;
    } else {
        git(&["remote", "set-url", "origin", ssh_url])?;
    }

    let has_commits = git(&["rev-parse", "HEAD"])?.status.success();
    if !has_commits {
        if !confirm(ctx, "Aucun commit local — créer un commit initial et pousser ?")? {
            return Ok(());
        }
        git(&["add", "-A"])?;
        let commit = git(&["commit", "-q", "-m", "Initial commit (via ilocker)"])?;
        if !commit.status.success() {
            // Cause la plus fréquente : identité git non configurée sur
            // cette machine (git commit échoue avec le code 128 et un
            // message explicite, mais SANS cette vérification, .output()
            // ne remonte pas d'erreur Rust — la commande "réussit" du
            // point de vue du process, seul le code de sortie dit le
            // contraire). Sans ce contrôle, le push suivant échoue avec
            // un message confus ("src refspec HEAD does not match any")
            // qui ne pointe jamais vers la vraie cause.
            let stderr = String::from_utf8_lossy(&commit.stderr);
            println!(
                "  {} Le commit initial a échoué — probablement une identité git non configurée :",
                "⚠".yellow()
            );
            for line in stderr.lines().take(3) {
                println!("      {}", line.dimmed());
            }
            println!("    Configurez git puis relancez :");
            println!("      {}", "git config --global user.email \"vous@exemple.com\"".cyan());
            println!("      {}", "git config --global user.name \"Votre Nom\"".cyan());
            return Ok(()); // on s'arrête ici — inutile de tenter un push qui échouera aussi
        }
    }

    let push = git(&["push", "-u", "origin", &format!("HEAD:{}", default_branch)])?;
    if push.status.success() {
        println!("  {} poussé vers origin/{}", "✓".green(), default_branch);
    } else {
        println!(
            "  {} push automatique impossible ({}). Poussez manuellement : git push -u origin {}",
            "⚠".yellow(),
            String::from_utf8_lossy(&push.stderr).lines().next().unwrap_or("erreur inconnue").trim(),
            default_branch
        );
    }
    Ok(())
}

// ── Migrations : applique uniquement les manquantes ────────────

async fn apply_pending_migrations(
    _dir: &Path,
    project_ref: &str,
    scan: &ProjectScan,
    ctx: &DeployContext,
) -> Result<()> {
    let Some(mig_dir) = &scan.supabase_migrations_dir else { return Ok(()) };
    let creds  = supabase_store::require_credentials(ctx.supabase_profile.as_deref())?;
    let client = SupabaseClient::new_from_credentials(&creds);

    let mut entries: Vec<_> = std::fs::read_dir(mig_dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("sql"))
        .collect();
    entries.sort_by_key(|e| e.file_name());

    let remote = client.list_migrations(project_ref).await?;
    let remote_versions: std::collections::HashSet<String> = remote.iter().map(|m| m.version.clone()).collect();

    for entry in entries {
        let filename = entry.file_name().to_string_lossy().to_string();
        let stem = filename.trim_end_matches(".sql");
        let (version, name) = match stem.split_once('_') {
            Some((v, n)) if v.chars().all(|c| c.is_ascii_digit()) => (v.to_string(), n.to_string()),
            _ => (stem.to_string(), stem.to_string()),
        };
        if remote_versions.contains(&version) { continue; }

        let sql = std::fs::read_to_string(entry.path())?;
        let sp  = spinner(&format!("Migration {} ({})…", version, name));
        client.apply_migration(project_ref, &version, &name, &sql).await
            .with_context(|| format!("Échec de la migration {} — les précédentes restent appliquées", version))?;
        sp.finish_and_clear();
        println!("  {} {} — {}", "✓".green(), version.dimmed(), name);
    }
    Ok(())
}

// ── Sync env vars Vercel (diff par hash) ────────────────────────

async fn sync_env_to_vercel(
    project_id: &str,
    env_file:    &Path,
    state:       &mut DeployState,
    ctx:         &DeployContext,
) -> Result<()> {
    let creds  = vercel_store::require_credentials(ctx.vercel_profile.as_deref())?;
    let client = VercelClient::new_from_credentials(&creds);

    let content = std::fs::read_to_string(env_file)?;
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') { continue; }
        let Some((key, val)) = line.split_once('=') else { continue };
        let key = key.trim();
        let val = val.trim().trim_matches('"').trim_matches('\'');
        if key.is_empty() { continue; }

        let hash_key = deploy_state::env_hash_key("vercel", key);
        let current_hash = deploy_state::hash_value(val);
        if state.env_hashes.get(&hash_key) == Some(&current_hash) {
            continue; // inchangé depuis le dernier push — on ne fait rien
        }

        client.upsert_env(
            project_id, key, val,
            &["production", "preview", "development"], "encrypted", None,
        ).await.with_context(|| format!("Échec push de la variable '{}'", key))?;
        state.env_hashes.insert(hash_key, current_hash);
        println!("  {} '{}' synchronisée vers Vercel", "✓".green(), key.bold());
    }
    Ok(())
}

// ── Déclenchement du déploiement ─────────────────────────────────

async fn trigger_vercel_deploy(project_id: &str, branch: &str, ctx: &DeployContext) -> Result<()> {
    let creds  = vercel_store::require_credentials(ctx.vercel_profile.as_deref())?;
    let client = VercelClient::new_from_credentials(&creds);
    let sp = spinner("Déclenchement du déploiement Vercel…");
    let d  = client.create_deployment_from_git(project_id, None, Some(branch), Some("production"), false).await?;
    sp.finish_and_clear();
    println!("  {} déploiement lancé : {}", "✓".green(), format!("https://{}", d.url).cyan());
    Ok(())
}

fn generate_password() -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let mut state = seed as u64 ^ 0x9E3779B97F4A7C15;
    let mut out = String::with_capacity(32);
    for _ in 0..32 {
        state ^= state << 13; state ^= state >> 7; state ^= state << 17;
        out.push(CHARS[(state as usize) % CHARS.len()] as char);
    }
    out
}

// ═════════════════════════════════════════════════════════════
//  Point d'entrée principal
// ═════════════════════════════════════════════════════════════

pub async fn run(dir: &Path, ctx: DeployContext) -> Result<()> {
    let plan = build_plan(dir, &ctx).await?;
    print_plan(&plan);

    if ctx.dry_run {
        println!("{}", "  --dry-run : aucune action exécutée.".dimmed());
        return Ok(());
    }

    if plan.nothing_to_deploy {
        return Ok(());
    }

    if !confirm(&ctx, "Exécuter ce plan ?")? {
        println!("  Annulé.");
        return Ok(());
    }

    execute_plan(dir, plan, &ctx).await
}

// Petite extension pour permettre de partir de l'état chargé en
// lecture (dans le plan) sans avoir à le recharger depuis le disque
// avant la phase d'exécution — évite une lecture disque redondante
// et une fenêtre de race (fichier modifié entre plan et exécution).
impl DeployState {
    fn clone_for_update(&self) -> DeployState {
        self.clone()
    }
}
