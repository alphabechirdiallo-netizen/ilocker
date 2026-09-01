// ============================================================
//  commands/provider.rs — Commandes CLI statiques du système de
//  providers déclaratifs : init / validate / test / install /
//  list / remove / connect / profile.
//
//  Ces commandes SONT déclarées statiquement dans main.rs (comme
//  toute autre commande ilocker) — seule l'EXÉCUTION d'un provider
//  une fois installé (`iloc <slug> <opération>`) est dynamique
//  (voir provider_engine.rs::dispatch, interceptée avant Cli::parse
//  dans main()).
// ============================================================

use crate::provider_engine;
use crate::provider_manifest::{self, AuthType, ProviderManifest};
use crate::provider_registry;
use crate::provider_store::{self, ProviderProfile};
use crate::commands::studio_docs::Danger;
use anyhow::{bail, Context, Result};
use colored::Colorize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::PathBuf;

// ═══════════════════════════════════════════════════════════════
//  iloc provider init <slug>
// ═══════════════════════════════════════════════════════════════

pub fn run_init(slug: String) -> Result<()> {
    if let Err(e) = provider_manifest::validate(&placeholder_manifest_for_slug_check(&slug)) {
        // On ne valide ici QUE la forme du slug (via un manifeste
        // minimal jetable) pour donner un message d'erreur immédiat
        // et clair avant même d'écrire le fichier.
        let msg = e.to_string();
        if msg.contains("slug") || msg.contains("réservé") {
            bail!("{}", msg);
        }
    }

    let filename = format!("{}.provider.toml", slug);
    let path = PathBuf::from(&filename);
    if path.exists() {
        bail!("'{}' existe déjà — supprimez-le ou choisissez un autre slug.", filename);
    }

    std::fs::write(&path, scaffold_toml(&slug))?;
    println!();
    println!("{} Manifeste créé : {}", "✓".green().bold(), filename.cyan());
    println!();
    println!("  Prochaines étapes :");
    println!("    1. Éditez {} — déclarez votre auth et vos opérations", filename.cyan());
    println!("    2. {}", format!("iloc provider validate {}", filename).cyan());
    println!("    3. {}", format!("iloc provider test {}", filename).cyan());
    println!("    4. {}", format!("iloc provider install --file {}", filename).cyan());
    println!();
    Ok(())
}

fn placeholder_manifest_for_slug_check(slug: &str) -> ProviderManifest {
    use crate::provider_manifest::*;
    ProviderManifest {
        provider: ProviderIdentity {
            slug: slug.to_string(), name: "x".into(), description: "x".into(),
            author: "x".into(), version: "0.1.0".into(), manifest_version: 1,
        },
        auth: AuthSchema {
            auth_type: AuthType::None, fields: vec![], header: None,
            value_prefix: String::new(), verify_endpoint: None, verify_field: None, help_url: None,
            token_url: None, scope: None, jwt_audience: None,
        },
        api: ApiConfig { base_url: "http://127.0.0.1".into() },
        operations: vec![Operation {
            path: vec!["x".into()], method: "GET".into(), endpoint: "/x".into(),
            summary: "x".into(), danger: Danger::Safe, args: vec![], example: None, response_fields: vec![],
            body_encoding: BodyEncoding::Json, graphql_query: None, extra_headers: vec![], idempotency_header: None, pagination: None,
        }],
    }
}

fn scaffold_toml(slug: &str) -> String {
    format!(
        r#"# ══════════════════════════════════════════════════════════
#  Manifeste de provider ilocker — {slug}
#
#  Ce fichier décrit intégralement un provider : IL NE CONTIENT
#  JAMAIS DE CODE. Chaque opération devient une commande CLI réelle
#  une fois installé : `iloc {slug} <chemin> [args]`.
#
#  Valider :  iloc provider validate {slug}.provider.toml
#  Tester :   iloc provider test {slug}.provider.toml
#  Installer :iloc provider install --file {slug}.provider.toml
# ══════════════════════════════════════════════════════════

[provider]
slug = "{slug}"
name = "Nom affiché"
description = "Ce que fait ce provider, en une phrase"
author = "Votre nom <email@exemple.com>"
version = "0.1.0"
manifest_version = 1

[auth]
# type = "bearer_token" | "api_key" | "basic" | "none"
type = "bearer_token"
header = "Authorization"
value_prefix = "Bearer "
prompt_label = "Clé API"
# Endpoint GET (relatif à base_url) appelé pour vérifier la connexion
verify_endpoint = "/me"
verify_field = "email"
help_url = "https://exemple.com/settings/api"

[[auth.fields]]
id = "token"
label = "Clé API"
secret = true

[api]
base_url = "https://api.exemple.com"

# ── Une entrée [[operations]] par commande exposée ────────────
# path      → segments de la commande : ["item","list"] devient
#             `iloc {slug} item list`
# method    → GET | POST | PUT | PATCH | DELETE
# endpoint  → relatif à base_url, ou URL absolue SOUS LE MÊME HOST
# danger    → "safe" | "caution" | "destructive" (confirmation
#             interactive automatique pour "destructive")

[[operations]]
path = ["item", "list"]
method = "GET"
endpoint = "/items"
summary = "Liste les items"
danger = "safe"
example = "iloc {slug} item list"

[[operations]]
path = ["item", "view"]
method = "GET"
endpoint = "/items/{{id}}"
summary = "Affiche un item"
danger = "safe"
example = "iloc {slug} item view abc123"
[[operations.args]]
id = "id"
positional = true
required = true
help = "Identifiant de l'item"
location = "path"

[[operations]]
path = ["item", "create"]
method = "POST"
endpoint = "/items"
summary = "Crée un item"
danger = "caution"
example = "iloc {slug} item create --title \"Mon item\""
[[operations.args]]
id = "title"
long = "title"
required = true
help = "Titre de l'item"
location = "body"

[[operations]]
path = ["item", "delete"]
method = "DELETE"
endpoint = "/items/{{id}}"
summary = "Supprime un item définitivement"
danger = "destructive"
example = "iloc {slug} item delete abc123"
[[operations.args]]
id = "id"
positional = true
required = true
help = "Identifiant de l'item à supprimer"
location = "path"
"#,
        slug = slug
    )
}

// ═══════════════════════════════════════════════════════════════
//  iloc provider validate <path>
// ═══════════════════════════════════════════════════════════════

pub fn run_validate(path: PathBuf) -> Result<()> {
    match provider_manifest::parse_file(&path) {
        Ok(m) => {
            println!();
            println!("{} Manifeste valide", "✓".green().bold());
            println!("  {} {}", "provider:".dimmed(), m.provider.name);
            println!("  {} {}", "slug:".dimmed(), m.provider.slug);
            println!("  {} {}", "opérations:".dimmed(), m.operations.len());
            for op in &m.operations {
                println!("    · iloc {} {}", m.provider.slug, op.path.join(" "));
            }
            println!();

            match provider_manifest::validate_for_publish(&m) {
                Ok(()) => println!("  {} Prêt pour la publication publique aussi.", "✓".green()),
                Err(e) => println!("  {} Usage privé uniquement — {}", "ℹ".cyan(), e),
            }
            println!();
            Ok(())
        }
        Err(e) => {
            println!();
            println!("{} Manifeste invalide", "✗".red().bold());
            println!("  {}", e);
            println!();
            Err(e)
        }
    }
}

// ═══════════════════════════════════════════════════════════════
//  iloc provider test <path>  — exécution réelle, sans stockage
// ═══════════════════════════════════════════════════════════════

pub async fn run_test(path: PathBuf) -> Result<()> {
    let manifest = provider_manifest::parse_file(&path)?;
    println!();
    println!("{} sur {}", "Test".bold(), manifest.provider.name.cyan());
    println!();
    println!("  Opérations disponibles :");
    for (i, op) in manifest.operations.iter().enumerate() {
        println!("    [{}] {} — {}", i + 1, op.path.join(" "), op.summary.dimmed());
    }
    println!();

    let choice = prompt("  Numéro de l'opération à tester : ")?;
    let idx: usize = choice.trim().parse().context("Numéro invalide")?;
    let op = manifest
        .operations
        .get(idx.saturating_sub(1))
        .context("Numéro hors plage")?
        .clone();

    println!();
    println!("  {} — identifiants de test (jamais stockés) :", "Connexion".bold());
    let mut fields = HashMap::new();
    for f in &manifest.auth.fields {
        let v = if f.secret {
            rpassword::prompt_password(format!("    {} (masqué) : ", f.label))?
        } else {
            prompt(&format!("    {} : ", f.label))?
        };
        fields.insert(f.id.clone(), v.trim().to_string());
    }

    let mut values = HashMap::new();
    if !op.args.is_empty() {
        println!();
        println!("  {} pour '{}' :", "Arguments".bold(), op.path.join(" "));
        for a in &op.args {
            let suffix = if a.required { "" } else { " (optionnel, Entrée pour ignorer)" };
            let v = prompt(&format!("    {}{} : ", a.help, suffix))?;
            let v = v.trim();
            if !v.is_empty() {
                values.insert(a.id.clone(), v.to_string());
            } else if a.required {
                bail!("'{}' est requis", a.id);
            }
        }
    }

    let creds = provider_store::ResolvedProviderCredentials {
        profile_name: "test".to_string(),
        // Compte factice mais STABLE (pas aléatoire) : permet au cache de
        // jeton OAuth2 de survivre entre plusieurs `iloc provider test`
        // successifs pendant qu'un développeur itère sur son manifeste,
        // au lieu de re-échanger un jeton à chaque appel.
        account: "__test_ephemeral__".to_string(),
        api_url: manifest.api.base_url.clone(),
        fields,
    };
    let client = provider_engine::GenericClient::new(&manifest, &creds);

    println!();
    println!("  {} {} {}…", "→".cyan(), op.method, op.path.join(" "));
    let result = client.execute(&op, &values).await?;
    println!();
    println!("{} Réponse complète :", "✓".green().bold());
    println!("{}", serde_json::to_string_pretty(&result)?);
    println!();
    println!("  {} le manifeste fonctionne. Vous pouvez maintenant :", "✓".green());
    println!("    {}", format!("iloc provider install --file {}", path.display()).cyan());
    println!();
    Ok(())
}

// ═══════════════════════════════════════════════════════════════
//  iloc provider install --file <path>
// ═══════════════════════════════════════════════════════════════

pub fn run_install_file(path: PathBuf) -> Result<()> {
    let manifest = provider_manifest::parse_file(&path)
        .context("Le manifeste doit être valide avant installation — voir `iloc provider validate`")?;

    let dest = provider_engine::manifest_path(&manifest.provider.slug)?;
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(&path, &dest)?;

    println!();
    println!("{} '{}' installé.", "✓".green().bold(), manifest.provider.name);
    println!("  {} iloc {} <commande>", "Utilisation :".dimmed(), manifest.provider.slug);
    println!("  {} iloc connect {}", "Connexion :  ".dimmed(), manifest.provider.slug);
    println!();
    Ok(())
}

// ═══════════════════════════════════════════════════════════════
//  iloc provider install <name> / search / publish — registre
//  communautaire (session août 2026)
// ═══════════════════════════════════════════════════════════════

/// Dispatch : `--file <chemin>` installe un manifeste local (inchangé,
/// voir run_install_file) ; un nom seul l'installe depuis le registre
/// communautaire. Les deux exclusifs, l'un des deux requis.
pub async fn run_install(name: Option<String>, file: Option<PathBuf>) -> Result<()> {
    match (name, file) {
        (Some(_), Some(_)) => {
            bail!("Précisez soit un nom de registre, soit --file <chemin> — pas les deux.")
        }
        (None, None) => {
            bail!(
                "Précisez un nom de provider à installer depuis le registre \
                 (ex: `iloc provider install linear`), ou --file <chemin> pour un manifeste local."
            )
        }
        (None, Some(path)) => run_install_file(path),
        (Some(name), None) => run_install_from_registry(&name).await,
    }
}

async fn run_install_from_registry(name: &str) -> Result<()> {
    println!();
    println!("{} Recherche de '{}' dans le registre communautaire...", "→".cyan(), name);

    let index = provider_registry::fetch_index()
        .await
        .context("Impossible de contacter le registre communautaire")?;
    let entry = index.providers.iter().find(|p| p.slug == name).with_context(|| {
        format!(
            "Aucun provider nommé '{}' dans le registre. Essayez `iloc provider search {}`.",
            name, name
        )
    })?;

    println!("  {} {} — {}", entry.name.bold(), format!("v{}", entry.version).dimmed(), entry.description);
    println!("  {} téléchargement et vérification d'intégrité...", "→".cyan());

    let bytes = provider_registry::fetch_manifest_bytes(entry).await?;
    let manifest = provider_manifest::parse(&bytes)
        .context("Le manifeste téléchargé depuis le registre a échoué la validation locale")?;

    if manifest.provider.slug != entry.slug {
        bail!(
            "Incohérence du registre : l'entrée annonce le slug '{}' mais le manifeste \
             déclare '{}'. Installation refusée par prudence.",
            entry.slug, manifest.provider.slug
        );
    }

    let dest = provider_engine::manifest_path(&manifest.provider.slug)?;
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&dest, &bytes).with_context(|| format!("Écriture de {}", dest.display()))?;

    println!();
    println!(
        "{} '{}' installé depuis le registre (intégrité sha256 vérifiée).",
        "✓".green().bold(),
        manifest.provider.name
    );
    println!("  {} iloc {} <commande>", "Utilisation :".dimmed(), manifest.provider.slug);
    println!("  {} iloc connect {}", "Connexion :  ".dimmed(), manifest.provider.slug);
    println!();
    Ok(())
}

pub async fn run_search(query: &str) -> Result<()> {
    println!();
    println!("{} Recherche '{}' dans le registre communautaire...", "→".cyan(), query);

    let index = provider_registry::fetch_index()
        .await
        .context("Impossible de contacter le registre communautaire")?;
    let results = provider_registry::search_index(&index, query);

    println!();
    if results.is_empty() {
        println!("  Aucun résultat pour '{}'.", query);
        println!();
        return Ok(());
    }

    for r in &results {
        println!("  {} {} — {}", r.slug.cyan().bold(), format!("v{}", r.version).dimmed(), r.description);
        if !r.tags.is_empty() {
            println!("    {} {}", "tags:".dimmed(), r.tags.join(", ").dimmed());
        }
    }
    println!();
    println!("  {} iloc provider install <nom>", "Installer :".dimmed());
    println!();
    Ok(())
}

pub async fn run_publish(path: PathBuf) -> Result<()> {
    use std::io::Write;
    let bytes = std::fs::read(&path).with_context(|| format!("Lecture de {}", path.display()))?;
    let manifest = provider_manifest::parse(&bytes)
        .context("Le manifeste doit d'abord passer `iloc provider validate` sans erreur")?;

    provider_manifest::validate_for_publish(&manifest)
        .context("Le manifeste ne respecte pas les règles de publication publique")?;

    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let sha256 = hex::encode(hasher.finalize());

    println!();
    println!("{} '{}' est valide et prêt pour publication.", "✓".green().bold(), manifest.provider.name);
    println!();
    println!("  {} {}", "slug:   ".dimmed(), manifest.provider.slug);
    println!("  {} {}", "version:".dimmed(), manifest.provider.version);
    println!("  {} {}", "sha256: ".dimmed(), sha256);
    println!();
    println!("{}", "Prochaine étape — soumettre une pull request :".bold());
    println!("  1. Forkez {}", provider_registry::registry_repo_url());
    println!("  2. Ajoutez votre fichier : ilocker-registry/providers/{}.toml", manifest.provider.slug);
    println!("  3. Ajoutez cette entrée à index.json (\"providers\": [...]) :");
    println!();
    let entry_json = serde_json::json!({
        "slug": manifest.provider.slug,
        "name": manifest.provider.name,
        "description": manifest.provider.description,
        "author": manifest.provider.author,
        "version": manifest.provider.version,
        "manifest_url": format!(
            "https://raw.githubusercontent.com/<votre-fork>/main/ilocker-registry/providers/{}.toml",
            manifest.provider.slug
        ),
        "manifest_sha256": sha256,
        "tags": Vec::<String>::new(),
    });
    println!("{}", serde_json::to_string_pretty(&entry_json)?);
    println!();
    println!("  4. Ouvrez la pull request — un workflow CI y relance automatiquement");
    println!("     `iloc provider validate` sur votre manifeste avant toute fusion.");
    println!();

    print!("  Ouvrir la page du dépôt dans le navigateur maintenant ? [O/n] ");
    std::io::stdout().flush()?;
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    if !input.trim().eq_ignore_ascii_case("n") {
        let _ = opener::open(provider_registry::registry_repo_url());
    }
    println!();
    Ok(())
}

// ═══════════════════════════════════════════════════════════════
//  iloc provider list / remove
// ═══════════════════════════════════════════════════════════════

pub fn run_list() -> Result<()> {
    let slugs = provider_engine::installed_slugs();
    println!();
    if slugs.is_empty() {
        println!("  Aucun provider installé. Voir `iloc provider install --file <manifeste>`.");
        println!();
        return Ok(());
    }
    println!("{}", "Providers installés :".bold());
    for slug in slugs {
        match provider_engine::load_installed(&slug) {
            Ok(Some(m)) => {
                let profiles = provider_store::list_profiles(&slug).unwrap_or_default();
                let connected = if profiles.profiles.is_empty() { "non connecté".dimmed() } else {
                    format!("{} profil(s)", profiles.profiles.len()).green()
                };
                println!(
                    "  {:<16} {} — {} opération(s), {}",
                    slug.cyan(), m.provider.name, m.operations.len(), connected
                );
            }
            _ => println!("  {:<16} {}", slug.cyan(), "(manifeste illisible)".red()),
        }
    }
    println!();
    Ok(())
}

pub fn run_remove(slug: String, yes: bool) -> Result<()> {
    if !provider_engine::is_installed_provider(&slug) {
        bail!("'{}' n'est pas installé.", slug);
    }
    if !yes {
        print!("  Supprimer '{}' et tous ses identifiants stockés ? [y/N] ", slug);
        use std::io::Write;
        std::io::stdout().flush().ok();
        let mut ans = String::new();
        std::io::stdin().read_line(&mut ans)?;
        if !matches!(ans.trim().to_lowercase().as_str(), "y" | "yes") {
            println!("  Annulé.");
            return Ok(());
        }
    }
    provider_store::purge_all(&slug)?;
    let manifest_dir = provider_engine::manifest_path(&slug)?;
    if let Some(dir) = manifest_dir.parent() {
        let _ = std::fs::remove_dir_all(dir);
    }
    println!("  {} '{}' désinstallé (manifeste + identifiants supprimés).", "✓".green(), slug);
    Ok(())
}

// ═══════════════════════════════════════════════════════════════
//  iloc connect <slug>  (pour un provider dynamique installé)
// ═══════════════════════════════════════════════════════════════

pub async fn run_connect(
    slug: &str,
    profile_name: Option<String>,
    api_url_override: Option<String>,
    token_flag: Option<String>,
) -> Result<()> {
    let manifest = provider_engine::load_installed(slug)?
        .ok_or_else(|| anyhow::anyhow!("'{}' n'est pas installé. Voir `iloc provider install --file <manifeste>`.", slug))?;

    println!();
    println!("{} {}", "Connexion à".bold(), manifest.provider.name.cyan());
    if let Some(url) = &manifest.auth.help_url {
        println!("  {} {}", "Obtenir des identifiants :".dimmed(), url);
    }
    println!();

    let mut fields = HashMap::new();
    // --token ne peut couvrir directement qu'un schéma à un seul champ
    // (bearer_token / api_key) — utile pour CI/CD, comme
    // `iloc connect github --token $VAR`. Le basic auth (deux champs)
    // reste toujours interactif : un seul flag ne peut pas porter
    // username ET password sans ambiguïté.
    let single_field_token = token_flag
        .filter(|_| manifest.auth.fields.len() == 1 && matches!(manifest.auth.auth_type, AuthType::BearerToken | AuthType::ApiKey));

    for f in &manifest.auth.fields {
        if let Some(t) = &single_field_token {
            fields.insert(f.id.clone(), t.clone());
            continue;
        }
        let v = if f.secret {
            rpassword::prompt_password(format!("  {} (masqué) : ", f.label))?
        } else {
            prompt(&format!("  {} : ", f.label))?
        };
        fields.insert(f.id.clone(), v.trim().to_string());
    }

    let api_url = api_url_override.clone().unwrap_or_else(|| manifest.api.base_url.clone());
    // Généré ICI (avant creds/verify), pas après : si le manifeste utilise
    // OAuth2, la vérification ci-dessous doit déjà pouvoir mettre en cache
    // son jeton sous une clé stable — voir ResolvedProviderCredentials::account.
    let account = format!("{}-{}", profile_name.clone().unwrap_or_else(|| "default".to_string()), short_random());
    let creds = provider_store::ResolvedProviderCredentials {
        profile_name: profile_name.clone().unwrap_or_else(|| "default".to_string()),
        account: account.clone(),
        api_url: api_url.clone(),
        fields: fields.clone(),
    };

    let mut identity_label: Option<String> = None;
    if let Some(verify_ep) = &manifest.auth.verify_endpoint {
        println!();
        println!("  {} …", "Vérification".dimmed());
        let client = provider_engine::GenericClient::new(&manifest, &creds);
        match client.verify(verify_ep).await {
            Ok(v) => {
                if let Some(field) = &manifest.auth.verify_field {
                    identity_label = v.get(field).and_then(|x| x.as_str()).map(|s| s.to_string());
                }
                println!(
                    "  {} Connecté{}",
                    "✓".green().bold(),
                    identity_label.as_deref().map(|l| format!(" en tant que {}", l)).unwrap_or_default()
                );
            }
            Err(e) => {
                bail!(
                    "Échec de la vérification : {}\n\n{}",
                    e,
                    manifest.auth.help_url.as_deref().unwrap_or("Vérifiez vos identifiants et réessayez.")
                );
            }
        }
    }

    let profile = ProviderProfile {
        name: profile_name.unwrap_or_else(|| "default".to_string()),
        identity_label,
        api_url_override: api_url_override,
        account,
        connected_at: chrono::Utc::now().to_rfc3339(),
    };
    provider_store::upsert_profile(slug, profile, &fields, true)?;

    println!();
    println!("  {} iloc {} <commande>", "Prêt :".dimmed(), slug);
    println!();
    Ok(())
}

fn short_random() -> String {
    format!(
        "{:x}",
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos() % 0xFFFFFF
    )
}

// ═══════════════════════════════════════════════════════════════
//  iloc provider profile list / use / remove <slug> …
// ═══════════════════════════════════════════════════════════════

pub fn run_profile_list(slug: String) -> Result<()> {
    let cfg = provider_store::list_profiles(&slug)?;
    println!();
    if cfg.profiles.is_empty() {
        println!("  Aucun profil pour '{}'. Voir `iloc connect {}`.", slug, slug);
        println!();
        return Ok(());
    }
    println!("{} pour '{}' :", "Profils".bold(), slug);
    for p in &cfg.profiles {
        let marker = if cfg.active.as_deref() == Some(&p.name) { "●".green() } else { "○".dimmed() };
        let label = p.identity_label.as_deref().unwrap_or("(non vérifié)");
        println!("  {} {:<16} {}", marker, p.name, label.dimmed());
    }
    println!();
    Ok(())
}

pub fn run_profile_use(slug: String, name: String) -> Result<()> {
    provider_store::set_active(&slug, &name)?;
    println!("  {} Profil actif pour '{}' : {}", "✓".green(), slug, name);
    Ok(())
}

pub fn run_profile_remove(slug: String, name: String, yes: bool) -> Result<()> {
    if !yes {
        print!("  Supprimer le profil '{}' de '{}' ? [y/N] ", name, slug);
        use std::io::Write;
        std::io::stdout().flush().ok();
        let mut ans = String::new();
        std::io::stdin().read_line(&mut ans)?;
        if !matches!(ans.trim().to_lowercase().as_str(), "y" | "yes") {
            println!("  Annulé.");
            return Ok(());
        }
    }
    if provider_store::remove_profile(&slug, &name)? {
        println!("  {} Profil '{}' supprimé pour '{}'.", "✓".green(), name, slug);
    } else {
        bail!("Profil '{}' introuvable pour '{}'.", name, slug);
    }
    Ok(())
}

// ═══════════════════════════════════════════════════════════════
//  Helpers
// ═══════════════════════════════════════════════════════════════

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
    std::io::stdout().flush().ok();
    let mut s = String::new();
    std::io::stdin().read_line(&mut s)?;
    Ok(s.trim().to_string())
}
