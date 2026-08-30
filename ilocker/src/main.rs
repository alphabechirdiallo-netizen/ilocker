// ============================================================
//  ilocker (iloc) — v1.10.8  (Standalone Edition)
//
//  Architecture : binaire autonome, zéro serveur requis.
//  Distribution : USB, Xender, Bluetooth, email, n'importe quoi.
//  Mise à jour  : iloc update  (via GitHub Releases, aucun VPS)
//  Installation : iloc selfinstall  (se loge dans le PATH système)
// ============================================================

mod api_client;
mod auth_store;
mod chunker;
mod cloud_crypto;
mod credential_vault; // ← nouveau : chiffrement du fallback fichier
mod cloud_share_token;
mod cloud_backend;
mod cloud_store;
mod commands;
mod crypto;
mod db;
mod dht;
mod engine;
mod erasure;
mod github_client;   // ← nouveau
mod github_store;    // ← nouveau
mod vercel_client;   // ← nouveau
mod vercel_store;    // ← nouveau
mod supabase_client; // ← nouveau
mod supabase_store;  // ← nouveau
mod scanner;         // ← nouveau
mod deploy_state;    // ← nouveau
mod health_score;
mod hyperscale_config;
mod intel_client;
mod logo;
mod merkle;
mod mesh_node;
mod presigned;
mod protocol;
mod provider_manifest;
mod provider_store;
mod provider_engine;
mod provider_registry;
mod relay_client;
mod s3_client;
mod snapshot;
mod transfer_state;
mod updater;
mod utils;
mod vault;
mod azure_client;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name    = "iloc",
    version = "1.10.8",
    about   = "ilocker — instant snapshot, Zero-Knowledge P2P & partage universel",
    long_about = "ilocker v1.10.8 — Standalone Edition\n\
                  \n\
                  Aucun serveur requis. Distribuez iloc par USB, Xender,\n\
                  Bluetooth, email — la commande s'installe dans le système.\n\
                  \n\
                  Premiers pas :\n\
                    iloc selfinstall   # installe dans le PATH\n\
                    iloc init          # initialise un projet\n\
                    iloc save \"msg\"    # snapshot\n\
                    iloc update        # mise à jour automatique",
)]
pub struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    // ── Local (aucun réseau requis) ──────────────────────────
    /// Initialise ilocker dans le projet courant
    Init {
        /// Emplacement du coffre-fort : in-project | sibling | system | custom
        #[arg(long)]
        vault_mode: Option<String>,
        /// Chemin explicite du vault (requis si --vault-mode custom)
        #[arg(long)]
        vault_dir: Option<std::path::PathBuf>,
        /// Ajoute un miroir local de sauvegarde (Tier 2) — répétable
        #[arg(long)]
        mirror: Vec<std::path::PathBuf>,
        /// Active la sauvegarde Cloud BYOC après chaque save (Tier 3a)
        #[arg(long)]
        cloud_backup: bool,
        /// Active la sauvegarde Hyperscale après chaque save (Tier 3b)
        #[arg(long)]
        hyperscale_backup: bool,
        /// Désactive l'ajout automatique de `.ilocker/` au .gitignore
        #[arg(long)]
        no_gitignore_patch: bool,
        /// Désactive l'activation automatique du Sentinel
        #[arg(long)]
        no_sentinel: bool,
    },
    /// Crée un snapshot du projet
    Save { message: String },
    /// Revient à un snapshot précédent
    Undo {
        id: Option<String>,
        #[arg(long = "file")]
        file: Vec<String>,
    },
    /// Affiche l'historique des snapshots
    Log {
        #[arg(long)]
        ids: bool,
    },
    /// Affiche le statut du projet
    Status {
        #[arg(long)]
        health: bool,
    },
    /// Surveillance continue (Sentinel)
    Sentinel {
        #[command(subcommand)]
        action: SentinelAction,
    },

    // ── Installation & mise à jour ───────────────────────────
    #[command(name = "selfinstall")]
    SelfInstall {
        #[arg(long)]
        dir: Option<std::path::PathBuf>,
        #[arg(long)]
        check: bool,
        /// Remplace sans demander de confirmation (scripts/CI, ou
        /// contexte non-interactif type "run" depuis un gestionnaire
        /// de fichiers, où une invite [Y/n] pourrait bloquer indéfiniment)
        #[arg(long, short)]
        yes: bool,
    },
    /// Met à jour iloc vers la dernière version
    Update {
        #[arg(long)]
        check: bool,
    },

    // ── P2P ──────────────────────────────────────────────────
    /// Partage un projet en P2P (direct ou via relay)
    Share {
        #[arg(short, long, default_value_t = protocol::DEFAULT_PORT)]
        port: u16,
        #[arg(long)]
        relay: Option<String>,
        #[arg(long)]
        cloud: bool,
        #[arg(long, default_value_t = commands::cloud_share::DEFAULT_TTL_HOURS)]
        ttl: u64,
        #[arg(long = "file")]
        file: Vec<String>,
        #[arg(long)]
        profile: Option<String>,
    },
    /// Clone un projet (P2P, relay, ou lien cloud)
    Clone {
        key: String,
        #[arg(long)]
        key_secret: Option<String>,
        #[arg(long)]
        host: Option<String>,
        #[arg(long)]
        relay: Option<String>,
        #[arg(short, long, default_value_t = protocol::DEFAULT_PORT)]
        port: u16,
        #[arg(short, long)]
        dest: Option<std::path::PathBuf>,
    },

    TransferStatus,
    Dashboard,
    Completion {
        #[arg(value_enum)]
        shell: CompletionShell,
        #[arg(long)]
        setup: bool,
    },

    // ── Studio (centre de commandes dans l'éditeur) ──────────
    /// Centre de commandes ilocker — s'ouvre dans VS Code
    #[command(name = "studio")]
    Studio {
        #[command(subcommand)]
        action: StudioAction,
    },

    // ── Hyperscale (Enterprise Multi-Cloud) ──────────────────
    #[command(name = "hyperscale")]
    Hyperscale {
        #[command(subcommand)]
        action: commands::hyperscale::HyperscaleCommand,
    },

    // ── Vault externalisé & sauvegarde 3-2-1 ─────────────────
    Vault {
        #[command(subcommand)]
        action: VaultAction,
    },

    // ── Mesh node ─────────────────────────────────────────────
    Node {
        #[command(subcommand)]
        action: NodeAction,
    },

    Login,
    Logout,
    Whoami,

    /// Configuration (cloud BYOC, etc.)
    #[command(name = "config")]
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// Push vers VOTRE cloud personnel (BYOC)
    Push {
        #[arg(long = "file")]
        file: Vec<String>,
        #[arg(long)]
        profile: Option<String>,
    },
    /// Restaure depuis VOTRE cloud personnel (BYOC)
    Pull {
        #[arg(short, long)]
        id: Option<String>,
        #[arg(short, long)]
        dest: Option<std::path::PathBuf>,
        #[arg(long = "file")]
        file: Vec<String>,
        #[arg(long)]
        profile: Option<String>,
    },
    /// Outils sur votre cloud personnel (usage, nettoyage, diagnostic, intégrité)
    Cloud {
        #[command(subcommand)]
        action: CloudAction,
    },

    // ── GitHub / Vercel ───────────────────────────────────────
    /// Connexion à un service externe (github, vercel, ou tout
    /// provider déclaratif installé via `iloc provider install`)
    Connect {
        /// Service à connecter : github, vercel, supabase, ou le slug
        /// de tout provider installé (voir `iloc provider list`)
        service: String,
        /// Nom du profil (optionnel)
        #[arg(long)]
        name: Option<String>,
        /// Token fourni directement (évite le prompt interactif — utile
        /// pour CI/CD et scripts : iloc connect vercel --token $VERCEL_TOKEN)
        #[arg(long)]
        token: Option<String>,
        /// URL de l'API GitHub Enterprise (GitHub uniquement — ignoré pour Vercel).
        /// Pour un provider déclaratif, sert d'override self-hosted.
        #[arg(long)]
        api_url: Option<String>,
    },

    /// Providers déclaratifs tiers — créer, valider, tester,
    /// installer et gérer des intégrations sans recompiler ilocker
    #[command(name = "provider")]
    Provider {
        #[command(subcommand)]
        action: ProviderAction,
    },

    /// Commandes GitHub (repos, issues, PRs, branches, releases, CI…)
    #[command(name = "github")]
    GitHub {
        #[command(subcommand)]
        action: GitHubAction,
    },

    /// Commandes Vercel (projets, déploiements, env, domaines, aliases…)
    #[command(name = "vercel")]
    Vercel {
        #[command(subcommand)]
        action: VercelAction,
    },

    /// Commandes Supabase (projets, migrations, edge functions, branches…)
    #[command(name = "supabase")]
    Supabase {
        #[command(subcommand)]
        action: SupabaseAction,
    },

    /// Orchestrateur intelligent : détecte, lie ou crée GitHub/Vercel/
    /// Supabase, applique les migrations en attente, synchronise les
    /// variables d'environnement, et déploie — sans jamais dupliquer
    /// une ressource déjà existante.
    Deploy {
        #[arg(long)] yes: bool,
        #[arg(long)] dry_run: bool,
        #[arg(long)] force_new: bool,
        #[arg(long)] skip_github: bool,
        #[arg(long)] skip_vercel: bool,
        #[arg(long)] skip_supabase: bool,
        #[arg(long)] github_profile: Option<String>,
        #[arg(long)] vercel_profile: Option<String>,
        #[arg(long)] supabase_profile: Option<String>,
        #[arg(long)] org: Option<String>,
        #[arg(long)] team: Option<String>,
    },
}

#[derive(Subcommand)]
enum ProviderAction {
    /// Crée un nouveau manifeste de provider (scaffold commenté)
    Init {
        /// Identifiant unique (minuscules, chiffres, tirets — ex: "linear")
        slug: String,
    },
    /// Valide un manifeste : schéma + garde-fous de sécurité
    Validate {
        /// Chemin du fichier manifeste (.toml)
        path: std::path::PathBuf,
    },
    /// Teste un manifeste avec de vrais appels API — identifiants de
    /// test saisis interactivement, jamais stockés ni transmis
    Test {
        path: std::path::PathBuf,
    },
    /// Installe un manifeste — depuis le registre communautaire par défaut
    /// (nom), ou localement avec --file (chemin)
    Install {
        /// Nom du provider dans le registre (ex: "linear")
        name: Option<String>,
        /// Chemin d'un manifeste local à installer directement
        #[arg(long)]
        file: Option<std::path::PathBuf>,
    },
    /// Cherche des providers dans le registre communautaire
    Search {
        /// Terme de recherche (nom, description, tags)
        query: String,
    },
    /// Valide un manifeste pour publication puis prépare sa soumission au
    /// registre communautaire (aucune donnée envoyée sans confirmation —
    /// ouvre une pull request pré-remplie sur GitHub)
    Publish {
        /// Chemin du manifeste à publier
        #[arg(long)]
        file: std::path::PathBuf,
    },
    /// Liste les providers installés localement
    List,
    /// Désinstalle un provider : supprime son manifeste et purge
    /// tous les identifiants stockés pour tous ses profils
    Remove {
        slug: String,
        #[arg(long, short)]
        yes: bool,
    },
    /// Gestion des profils (comptes multiples) d'un provider installé
    Profile {
        #[command(subcommand)]
        action: ProfileAction,
    },
}

#[derive(Subcommand)]
enum ProfileAction {
    /// Liste les profils connectés pour un provider
    List { slug: String },
    /// Change le profil actif utilisé par défaut
    Use { slug: String, name: String },
    /// Supprime un profil précis (les autres profils du même
    /// provider ne sont pas affectés)
    Remove {
        slug: String,
        name: String,
        #[arg(long, short)]
        yes: bool,
    },
}

// ── GitHub subcommands ────────────────────────────────────────

#[derive(Subcommand)]
enum GitHubAction {
    /// Liste les comptes GitHub connectés
    List,
    /// Change le compte GitHub actif
    Use { name: String },
    /// Affiche le compte connecté et son statut
    Status {
        #[arg(long)]
        profile: Option<String>,
    },
    /// Déconnecte un compte GitHub
    Remove {
        name: String,
        #[arg(long)]
        yes: bool,
    },

    /// Gestion des repositories
    Repo {
        #[command(subcommand)]
        action: GitHubRepoAction,
    },

    /// Gestion des branches
    Branch {
        #[command(subcommand)]
        action: GitHubBranchAction,
    },

    /// Gestion des issues
    Issue {
        #[command(subcommand)]
        action: GitHubIssueAction,
    },

    /// Gestion des pull requests
    #[command(name = "pr")]
    Pr {
        #[command(subcommand)]
        action: GitHubPrAction,
    },

    /// Gestion des releases
    Release {
        #[command(subcommand)]
        action: GitHubReleaseAction,
    },

    /// GitHub Actions / CI
    Actions {
        #[command(subcommand)]
        action: GitHubActionsAction,
    },

    /// Secrets Actions
    Secret {
        #[command(subcommand)]
        action: GitHubSecretAction,
    },

    /// Collaborateurs
    Collab {
        #[command(subcommand)]
        action: GitHubCollabAction,
    },

    /// Webhooks
    Webhook {
        #[command(subcommand)]
        action: GitHubWebhookAction,
    },

    /// Recherche GitHub
    Search {
        #[command(subcommand)]
        action: GitHubSearchAction,
    },
}

// ── Repo ─────────────────────────────────────────────────────

#[derive(Subcommand)]
enum GitHubRepoAction {
    /// Crée un repo
    Create {
        name: Option<String>,
        #[arg(short, long)]
        description: Option<String>,
        #[arg(long)]
        private: bool,
        #[arg(long)]
        public: bool,
        #[arg(long)]
        org: Option<String>,
        #[arg(long)]
        auto_init: bool,
        #[arg(long = "topic")]
        topics: Vec<String>,
        #[arg(long)]
        license: Option<String>,
        #[arg(long)]
        gitignore: Option<String>,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        yes: bool,
    },
    /// Liste vos repos
    List {
        #[arg(long)]
        org: Option<String>,
        #[arg(long)]
        private: bool,
        #[arg(long)]
        public: bool,
        #[arg(long)]
        fork: bool,
        #[arg(long, default_value_t = 30)]
        limit: usize,
        #[arg(long)]
        profile: Option<String>,
    },
    /// Détails d'un repo
    View {
        /// owner/repo (auto-détecté depuis git remote origin si omis)
        owner_repo: Option<String>,
        #[arg(long)]
        profile: Option<String>,
    },
    /// Supprime un repo (irréversible)
    Delete {
        owner_repo: Option<String>,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        yes: bool,
    },
    /// Archive (ou désarchive) un repo
    Archive {
        owner_repo: Option<String>,
        #[arg(long)]
        unarchive: bool,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        yes: bool,
    },
    /// Fork un repo
    Fork {
        /// owner/repo à forker
        owner_repo: String,
        #[arg(long)]
        org: Option<String>,
        #[arg(long)]
        profile: Option<String>,
    },
    /// Transfère un repo vers un autre owner/org
    Transfer {
        /// Nouveau propriétaire (owner ou org)
        new_owner: String,
        /// owner/repo (auto-détecté depuis git remote origin si omis)
        owner_repo: Option<String>,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        yes: bool,
    },
    /// Gère les topics d'un repo
    Topics {
        owner_repo: Option<String>,
        #[arg(long)]
        add: Vec<String>,
        #[arg(long)]
        remove: Vec<String>,
        #[arg(long)]
        set: Vec<String>,
        #[arg(long)]
        profile: Option<String>,
    },
    /// Change la visibilité (public/privé)
    Visibility {
        owner_repo: Option<String>,
        #[arg(long)]
        private: bool,
        #[arg(long)]
        public: bool,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        yes: bool,
    },
    /// Renomme un repo
    Rename {
        /// Nouveau nom du repo
        new_name: String,
        /// owner/repo (auto-détecté depuis git remote origin si omis)
        owner_repo: Option<String>,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        yes: bool,
    },
}

// ── Branch ───────────────────────────────────────────────────

#[derive(Subcommand)]
enum GitHubBranchAction {
    /// Liste les branches
    List {
        owner_repo: Option<String>,
        #[arg(long)]
        profile: Option<String>,
    },
    /// Crée une branche
    Create {
        name: String,
        #[arg(long)]
        from: Option<String>,
        owner_repo: Option<String>,
        #[arg(long)]
        profile: Option<String>,
    },
    /// Supprime une branche
    Delete {
        name: String,
        owner_repo: Option<String>,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        yes: bool,
    },
    /// Protège une branche
    Protect {
        name: String,
        owner_repo: Option<String>,
        #[arg(long = "check")]
        checks: Vec<String>,
        #[arg(long)]
        require_pr: bool,
        #[arg(long, default_value_t = 1)]
        min_reviews: u32,
        #[arg(long)]
        enforce_admins: bool,
        #[arg(long)]
        linear: bool,
        #[arg(long)]
        allow_force_pushes: bool,
        #[arg(long)]
        allow_deletions: bool,
        #[arg(long)]
        profile: Option<String>,
    },
    /// Retire la protection d'une branche
    Unprotect {
        name: String,
        owner_repo: Option<String>,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        yes: bool,
    },
    /// Change la branche par défaut
    Default {
        name: String,
        owner_repo: Option<String>,
        #[arg(long)]
        profile: Option<String>,
    },
}

// ── Issue ─────────────────────────────────────────────────────

#[derive(Subcommand)]
enum GitHubIssueAction {
    /// Liste les issues
    List {
        owner_repo: Option<String>,
        #[arg(long, default_value = "open")]
        state: String,
        #[arg(long = "label")]
        labels: Vec<String>,
        #[arg(long)]
        assignee: Option<String>,
        #[arg(long, default_value_t = 30)]
        limit: usize,
        #[arg(long)]
        profile: Option<String>,
    },
    /// Crée une issue
    Create {
        owner_repo: Option<String>,
        #[arg(short, long)]
        title: Option<String>,
        #[arg(short, long)]
        body: Option<String>,
        #[arg(long = "label")]
        labels: Vec<String>,
        #[arg(long = "assignee")]
        assignees: Vec<String>,
        #[arg(long)]
        milestone: Option<u64>,
        #[arg(long)]
        profile: Option<String>,
    },
    /// Affiche le détail d'une issue
    View {
        number: u64,
        owner_repo: Option<String>,
        #[arg(long)]
        profile: Option<String>,
    },
    /// Ferme une issue
    Close {
        number: u64,
        owner_repo: Option<String>,
        #[arg(long)]
        reason: Option<String>,
        #[arg(long)]
        profile: Option<String>,
    },
    /// Rouvre une issue
    Reopen {
        number: u64,
        owner_repo: Option<String>,
        #[arg(long)]
        profile: Option<String>,
    },
    /// Ajoute un commentaire
    Comment {
        number: u64,
        owner_repo: Option<String>,
        #[arg(short, long)]
        body: Option<String>,
        #[arg(long)]
        profile: Option<String>,
    },
    /// Assigne/désassigne des utilisateurs
    Assign {
        number: u64,
        /// Utilisateurs à (dés)assigner
        users: Vec<String>,
        /// owner/repo (auto-détecté depuis git remote origin si omis).
        /// Doit être --repo (pas positionnel) : sans ça, "owner/repo" serait
        /// interprété comme un utilisateur supplémentaire, ce qui est ambigu
        /// aussi bien pour clap que pour un humain lisant la commande.
        #[arg(long = "repo")]
        owner_repo: Option<String>,
        #[arg(long)]
        unassign: bool,
        #[arg(long)]
        profile: Option<String>,
    },
    /// Ajoute/retire des labels
    Label {
        number: u64,
        owner_repo: Option<String>,
        #[arg(long)]
        add: Vec<String>,
        #[arg(long)]
        remove: Vec<String>,
        #[arg(long)]
        profile: Option<String>,
    },
}

// ── PR ────────────────────────────────────────────────────────

#[derive(Subcommand)]
enum GitHubPrAction {
    /// Liste les PRs
    List {
        owner_repo: Option<String>,
        #[arg(long, default_value = "open")]
        state: String,
        #[arg(long)]
        base: Option<String>,
        #[arg(long, default_value_t = 20)]
        limit: usize,
        #[arg(long)]
        profile: Option<String>,
    },
    /// Crée une PR
    Create {
        owner_repo: Option<String>,
        #[arg(short, long)]
        title: Option<String>,
        #[arg(short, long)]
        body: Option<String>,
        #[arg(long)]
        head: Option<String>,
        #[arg(long)]
        base: Option<String>,
        #[arg(long)]
        draft: bool,
        #[arg(long)]
        profile: Option<String>,
    },
    /// Affiche le détail d'une PR
    View {
        number: u64,
        owner_repo: Option<String>,
        #[arg(long)]
        profile: Option<String>,
    },
    /// Merge une PR
    Merge {
        number: u64,
        owner_repo: Option<String>,
        #[arg(long, default_value = "merge")]
        method: String,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        message: Option<String>,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        yes: bool,
    },
    /// Demande une review
    Review {
        number: u64,
        owner_repo: Option<String>,
        #[arg(long = "reviewer")]
        reviewers: Vec<String>,
        #[arg(long = "team")]
        teams: Vec<String>,
        #[arg(long)]
        profile: Option<String>,
    },
    /// Checkout la branche d'une PR
    Checkout {
        number: u64,
        owner_repo: Option<String>,
        #[arg(long)]
        profile: Option<String>,
    },
    /// Ferme une PR sans merger
    Close {
        number: u64,
        owner_repo: Option<String>,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        yes: bool,
    },
    /// Marque une PR comme prête (retire le draft)
    Ready {
        number: u64,
        owner_repo: Option<String>,
        #[arg(long)]
        draft: bool,
        #[arg(long)]
        profile: Option<String>,
    },
    /// Met à jour la branche d'une PR avec la base
    UpdateBranch {
        number: u64,
        owner_repo: Option<String>,
        #[arg(long)]
        profile: Option<String>,
    },
}

// ── Release ───────────────────────────────────────────────────

#[derive(Subcommand)]
enum GitHubReleaseAction {
    /// Liste les releases
    List {
        owner_repo: Option<String>,
        #[arg(long, default_value_t = 10)]
        limit: usize,
        #[arg(long)]
        profile: Option<String>,
    },
    /// Crée une release
    Create {
        owner_repo: Option<String>,
        #[arg(short, long)]
        tag: Option<String>,
        #[arg(short, long)]
        name: Option<String>,
        #[arg(short, long)]
        body: Option<String>,
        #[arg(long)]
        draft: bool,
        #[arg(long)]
        prerelease: bool,
        #[arg(long)]
        target: Option<String>,
        #[arg(long)]
        generate_notes: bool,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        yes: bool,
    },
    /// Supprime une release
    Delete {
        tag: String,
        owner_repo: Option<String>,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        yes: bool,
    },
    /// Upload un fichier comme asset d'une release
    Upload {
        tag: String,
        file: std::path::PathBuf,
        owner_repo: Option<String>,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        content_type: Option<String>,
        #[arg(long)]
        profile: Option<String>,
    },
}

// ── Actions ───────────────────────────────────────────────────

#[derive(Subcommand)]
enum GitHubActionsAction {
    /// Liste les workflows
    List {
        owner_repo: Option<String>,
        #[arg(long)]
        profile: Option<String>,
    },
    /// Déclenche un workflow (workflow_dispatch)
    Run {
        workflow: String,
        owner_repo: Option<String>,
        #[arg(long)]
        branch: Option<String>,
        /// Inputs KEY=VALUE — répétable
        #[arg(long = "input")]
        inputs: Vec<String>,
        #[arg(long)]
        profile: Option<String>,
    },
    /// Affiche les runs récents
    Status {
        owner_repo: Option<String>,
        #[arg(long)]
        workflow: Option<String>,
        #[arg(long)]
        branch: Option<String>,
        #[arg(long, default_value_t = 10)]
        limit: usize,
        #[arg(long)]
        profile: Option<String>,
    },
    /// Annule un run
    Cancel {
        run_id: u64,
        owner_repo: Option<String>,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        yes: bool,
    },
    /// Relance un run
    Rerun {
        run_id: u64,
        owner_repo: Option<String>,
        #[arg(long)]
        profile: Option<String>,
    },
}

// ── Secret ────────────────────────────────────────────────────

#[derive(Subcommand)]
enum GitHubSecretAction {
    /// Liste les secrets Actions (noms uniquement)
    List {
        owner_repo: Option<String>,
        #[arg(long)]
        profile: Option<String>,
    },
    /// Crée ou met à jour un secret Actions
    Set {
        name: String,
        owner_repo: Option<String>,
        /// Valeur (si omise, demandée interactivement masquée)
        #[arg(long)]
        value: Option<String>,
        #[arg(long)]
        profile: Option<String>,
    },
    /// Supprime un secret Actions
    Delete {
        name: String,
        owner_repo: Option<String>,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        yes: bool,
    },
}

// ── Collab ────────────────────────────────────────────────────

#[derive(Subcommand)]
enum GitHubCollabAction {
    /// Liste les collaborateurs
    List {
        owner_repo: Option<String>,
        #[arg(long)]
        profile: Option<String>,
    },
    /// Ajoute un collaborateur
    Add {
        username: String,
        owner_repo: Option<String>,
        #[arg(long, default_value = "push")]
        permission: String,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        yes: bool,
    },
    /// Retire un collaborateur
    Remove {
        username: String,
        owner_repo: Option<String>,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        yes: bool,
    },
}

// ── Webhook ───────────────────────────────────────────────────

#[derive(Subcommand)]
enum GitHubWebhookAction {
    /// Liste les webhooks
    List {
        owner_repo: Option<String>,
        #[arg(long)]
        profile: Option<String>,
    },
    /// Crée un webhook
    Create {
        url: String,
        owner_repo: Option<String>,
        #[arg(long = "event", default_value = "push")]
        events: Vec<String>,
        #[arg(long)]
        content_type: Option<String>,
        #[arg(long)]
        secret: Option<String>,
        #[arg(long)]
        inactive: bool,
        #[arg(long)]
        profile: Option<String>,
    },
    /// Supprime un webhook
    Delete {
        hook_id: u64,
        owner_repo: Option<String>,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        yes: bool,
    },
    /// Ping un webhook
    Ping {
        hook_id: u64,
        owner_repo: Option<String>,
        #[arg(long)]
        profile: Option<String>,
    },
}

// ── Search ────────────────────────────────────────────────────

#[derive(Subcommand)]
enum GitHubSearchAction {
    /// Recherche des repositories
    Repos {
        query: String,
        #[arg(long, default_value_t = 10)]
        limit: usize,
        #[arg(long)]
        profile: Option<String>,
    },
}

// ── Sous-commandes existantes (inchangées) ────────────────────

#[derive(Subcommand)]
enum CloudAction {
    Usage   { #[arg(long)] profile: Option<String> },
    Gc      { #[arg(long)] profile: Option<String>, #[arg(long)] yes: bool },
    Doctor  { #[arg(long)] profile: Option<String> },
    Verify  { #[arg(long)] profile: Option<String> },
}

#[derive(Subcommand)]
enum NodeAction { Join, Leave, Start, Status }

#[derive(Subcommand)]
enum ConfigAction {
    Cloud {
        #[command(subcommand)]
        action: CloudConfigAction,
    },
}

#[derive(Subcommand)]
enum CloudConfigAction {
    Add    { #[arg(long)] name: Option<String>, #[arg(long)] activate: bool },
    List,
    Use    { name: String },
    Remove { name: String },
}

#[derive(Subcommand)]
enum SentinelAction { Init, Enable, Disable, Status, Uninstall }

#[derive(Debug, Clone, clap::ValueEnum)]
enum CompletionShell {
    Bash,
    Zsh,
    #[value(name = "powershell")]
    PowerShell,
}

#[derive(Subcommand)]
enum StudioAction {
    /// Ouvre le centre de commandes dans VS Code (installe l'extension si besoin)
    Open,
    /// Génère le manifeste JSON des commandes (introspection clap — usage interne, consommé par l'extension VS Code)
    #[command(hide = true)]
    Manifest {
        /// Écrit dans un fichier au lieu de stdout
        #[arg(long)]
        output: Option<std::path::PathBuf>,
    },
    /// Historique des snapshots en JSON (usage interne, extension VS Code)
    #[command(hide = true)]
    Snapshots {
        #[arg(long)]
        limit: Option<usize>,
    },
    /// État de déploiement en JSON (usage interne, extension VS Code)
    #[command(hide = true, name = "deploy-status")]
    DeployStatus,
    /// Vue d'ensemble du projet en JSON (usage interne, extension VS Code)
    #[command(hide = true, name = "project-status")]
    ProjectStatus,
}

#[derive(Subcommand)]
enum VaultAction {
    Status,
    Migrate { #[arg(long)] mode: Option<String>, #[arg(long)] dir: Option<std::path::PathBuf> },
    Mirror  { #[command(subcommand)] action: MirrorAction },
    Backup  { #[command(subcommand)] action: BackupAction },
    Verify,
}

#[derive(Subcommand)]
enum MirrorAction {
    Add    { path: std::path::PathBuf },
    Remove { path: std::path::PathBuf },
    Sync,
}

#[derive(Subcommand)]
enum BackupAction {
    EnableCloud    { #[arg(long)] profile: Option<String> },
    DisableCloud,
    EnableHyperscale,
    DisableHyperscale,
}



// ── Vercel subcommands ────────────────────────────────────────

#[derive(Subcommand)]
enum VercelAction {
    List,
    Use { name: String },
    Status { #[arg(long)] profile: Option<String> },
    Remove { name: String, #[arg(long)] yes: bool },
    Deploy {
        #[arg(long)] prod: bool,
        #[arg(long)] force: bool,
        #[arg(long)] wait: bool,
        #[arg(long)] project: Option<String>,
        #[arg(long)] branch: Option<String>,
        #[arg(long)] sha: Option<String>,
        #[arg(long, default_value_t = 300)] timeout: u64,
        #[arg(long)] profile: Option<String>,
        #[arg(long)] yes: bool,
    },
    Inspect { #[arg(long)] profile: Option<String> },
    Project { #[command(subcommand)] action: VercelProjectAction },
    Deployment { #[command(subcommand)] action: VercelDeploymentAction },
    Env { #[command(subcommand)] action: VercelEnvAction },
    Domain { #[command(subcommand)] action: VercelDomainAction },
    Alias { #[command(subcommand)] action: VercelAliasAction },
    Secret { #[command(subcommand)] action: VercelSecretAction },
    Edge { #[command(subcommand)] action: VercelEdgeAction },
    Webhook { #[command(subcommand)] action: VercelWebhookAction },
    Check { #[command(subcommand)] action: VercelCheckAction },
    Team { #[command(subcommand)] action: VercelTeamAction },
}

#[derive(Subcommand)]
enum VercelProjectAction {
    List { #[arg(long, default_value_t = 20)] limit: usize, #[arg(long)] profile: Option<String> },
    Create {
        name: Option<String>,
        #[arg(long)] framework: Option<String>,
        #[arg(long)] root: Option<String>,
        #[arg(long)] build: Option<String>,
        #[arg(long)] output: Option<String>,
        #[arg(long)] install: Option<String>,
        #[arg(long)] git_repo: Option<String>,
        #[arg(long)] git_branch: Option<String>,
        #[arg(long)] link: bool,
        #[arg(long)] profile: Option<String>,
        #[arg(long)] yes: bool,
    },
    View { project: Option<String>, #[arg(long)] profile: Option<String> },
    Update {
        project: Option<String>,
        #[arg(long)] name: Option<String>,
        #[arg(long)] framework: Option<String>,
        #[arg(long)] root: Option<String>,
        #[arg(long)] build: Option<String>,
        #[arg(long)] output: Option<String>,
        #[arg(long)] install: Option<String>,
        #[arg(long)] node: Option<String>,
        #[arg(long)] prod_branch: Option<String>,
        #[arg(long)] profile: Option<String>,
    },
    Delete { project: Option<String>, #[arg(long)] profile: Option<String>, #[arg(long)] yes: bool },
    Link { project: Option<String>, #[arg(long)] profile: Option<String> },
    Unlink { #[arg(long)] yes: bool },
}

#[derive(Subcommand)]
enum VercelDeploymentAction {
    List {
        #[arg(long)] project: Option<String>,
        #[arg(long)] target: Option<String>,
        #[arg(long)] state: Option<String>,
        #[arg(long, default_value_t = 20)] limit: usize,
        #[arg(long)] profile: Option<String>,
    },
    View { id: String, #[arg(long)] profile: Option<String> },
    Cancel { id: String, #[arg(long)] profile: Option<String>, #[arg(long)] yes: bool },
    Delete { id: String, #[arg(long)] profile: Option<String>, #[arg(long)] yes: bool },
    Redeploy { id: String, #[arg(long)] target: Option<String>, #[arg(long)] profile: Option<String>, #[arg(long)] yes: bool },
    Promote { id: String, #[arg(long)] project: Option<String>, #[arg(long)] profile: Option<String>, #[arg(long)] yes: bool },
    Logs { id: String, #[arg(long)] profile: Option<String> },
    Files { id: String, #[arg(long)] profile: Option<String> },
}

#[derive(Subcommand)]
enum VercelEnvAction {
    List { #[arg(long)] project: Option<String>, #[arg(long)] profile: Option<String> },
    Add {
        key: String,
        value: Option<String>,
        #[arg(long)] target: Vec<String>,
        #[arg(long)] env_type: Option<String>,
        #[arg(long)] git_branch: Option<String>,
        #[arg(long)] project: Option<String>,
        #[arg(long)] profile: Option<String>,
    },
    Remove { key: String, #[arg(long)] target: Option<String>, #[arg(long)] project: Option<String>, #[arg(long)] profile: Option<String>, #[arg(long)] yes: bool },
    Pull {
        output: Option<std::path::PathBuf>,
        #[arg(long)] target: Vec<String>,
        #[arg(long)] project: Option<String>,
        #[arg(long)] profile: Option<String>,
        #[arg(long)] yes: bool,
    },
    Push {
        input: Option<std::path::PathBuf>,
        #[arg(long)] target: Vec<String>,
        #[arg(long)] project: Option<String>,
        #[arg(long)] profile: Option<String>,
        #[arg(long)] yes: bool,
    },
}

#[derive(Subcommand)]
enum VercelDomainAction {
    List { #[arg(long)] project: Option<String>, #[arg(long, default_value_t = 20)] limit: usize, #[arg(long)] profile: Option<String> },
    Add { domain: String, #[arg(long)] project: Option<String>, #[arg(long)] git_branch: Option<String>, #[arg(long)] redirect: Option<String>, #[arg(long)] profile: Option<String> },
    Remove { domain: String, #[arg(long)] project: Option<String>, #[arg(long)] profile: Option<String>, #[arg(long)] yes: bool },
    Inspect { domain: String, #[arg(long)] project: Option<String>, #[arg(long)] profile: Option<String> },
    Check { domain: String, #[arg(long)] profile: Option<String> },
    Dns { #[command(subcommand)] action: VercelDnsAction },
}

#[derive(Subcommand)]
enum VercelDnsAction {
    List { domain: String, #[arg(long)] profile: Option<String> },
    Add {
        domain: String,
        name: String,
        #[arg(long = "type")] rec_type: String,
        value: String,
        #[arg(long)] ttl: Option<u64>,
        #[arg(long)] priority: Option<u64>,
        #[arg(long)] profile: Option<String>,
    },
    Remove { domain: String, record_id: String, #[arg(long)] profile: Option<String>, #[arg(long)] yes: bool },
}

#[derive(Subcommand)]
enum VercelAliasAction {
    List { #[arg(long)] project: Option<String>, #[arg(long, default_value_t = 20)] limit: usize, #[arg(long)] profile: Option<String> },
    Assign { deployment_id: String, alias: String, #[arg(long)] redirect: Option<String>, #[arg(long)] profile: Option<String> },
    Delete { alias: String, #[arg(long)] profile: Option<String>, #[arg(long)] yes: bool },
}

#[derive(Subcommand)]
enum VercelSecretAction {
    List { #[arg(long)] profile: Option<String> },
    Add { name: String, value: Option<String>, #[arg(long)] profile: Option<String> },
    Rename { name: String, new_name: String, #[arg(long)] profile: Option<String> },
    Delete { name: String, #[arg(long)] profile: Option<String>, #[arg(long)] yes: bool },
}

#[derive(Subcommand)]
enum VercelEdgeAction {
    List { #[arg(long)] profile: Option<String> },
    Create { slug: String, #[arg(long)] profile: Option<String> },
    Items { id: String, #[arg(long)] profile: Option<String> },
    Update { id: String, #[arg(long = "item")] items: Vec<String>, #[arg(long)] profile: Option<String> },
    Delete { id: String, #[arg(long)] profile: Option<String>, #[arg(long)] yes: bool },
}

#[derive(Subcommand)]
enum VercelWebhookAction {
    List { #[arg(long)] profile: Option<String> },
    Create { url: String, #[arg(long = "event")] events: Vec<String>, #[arg(long)] profile: Option<String> },
    Delete { id: String, #[arg(long)] profile: Option<String>, #[arg(long)] yes: bool },
}

#[derive(Subcommand)]
enum VercelCheckAction {
    List { deployment_id: String, #[arg(long)] profile: Option<String> },
    Create { deployment_id: String, name: String, #[arg(long)] detached: bool, #[arg(long)] blocking: bool, #[arg(long)] profile: Option<String> },
    Update { deployment_id: String, check_id: String, #[arg(long)] status: String, #[arg(long)] conclusion: Option<String>, #[arg(long)] profile: Option<String> },
}

#[derive(Subcommand)]
enum VercelTeamAction {
    List { #[arg(long)] profile: Option<String> },
    Switch { slug: String, #[arg(long)] profile: Option<String> },
}


// ── Supabase subcommands ──────────────────────────────────────

#[derive(Subcommand)]
enum SupabaseAction {
    List,
    Use { name: String },
    Status { #[arg(long)] profile: Option<String> },
    Remove { name: String, #[arg(long)] yes: bool },
    Org { #[command(subcommand)] action: SupabaseOrgAction },
    Project { #[command(subcommand)] action: SupabaseProjectAction },
    Keys {
        project_ref: Option<String>,
        #[arg(long)] reveal: bool,
        #[arg(long)] profile: Option<String>,
    },
    Sql {
        query: String,
        project_ref: Option<String>,
        #[arg(long)] profile: Option<String>,
        #[arg(long)] yes: bool,
    },
    Table { #[command(subcommand)] action: SupabaseTableAction },
    Extension { #[command(subcommand)] action: SupabaseExtensionAction },
    Migration { #[command(subcommand)] action: SupabaseMigrationAction },
    Function { #[command(subcommand)] action: SupabaseFunctionAction },
    Secret { #[command(subcommand)] action: SupabaseSecretAction },
    Branch { #[command(subcommand)] action: SupabaseBranchAction },
    Advisor { #[command(subcommand)] action: SupabaseAdvisorAction },
}

#[derive(Subcommand)]
enum SupabaseOrgAction {
    List { #[arg(long)] profile: Option<String> },
}

#[derive(Subcommand)]
enum SupabaseProjectAction {
    Create {
        name: Option<String>,
        #[arg(long)] org: Option<String>,
        #[arg(long)] region: Option<String>,
        #[arg(long)] db_pass: Option<String>,
        #[arg(long)] link: bool,
        #[arg(long)] profile: Option<String>,
        #[arg(long)] yes: bool,
    },
    List { #[arg(long)] profile: Option<String> },
    View { project_ref: Option<String>, #[arg(long)] profile: Option<String> },
    Delete { project_ref: Option<String>, #[arg(long)] profile: Option<String>, #[arg(long)] yes: bool },
    Pause { project_ref: Option<String>, #[arg(long)] profile: Option<String>, #[arg(long)] yes: bool },
    Restore { project_ref: Option<String>, #[arg(long)] profile: Option<String>, #[arg(long)] yes: bool },
    Url { project_ref: Option<String>, #[arg(long)] profile: Option<String> },
}

#[derive(Subcommand)]
enum SupabaseTableAction {
    List {
        project_ref: Option<String>,
        #[arg(long, default_value = "public")] schema: String,
        #[arg(long)] profile: Option<String>,
    },
}

#[derive(Subcommand)]
enum SupabaseExtensionAction {
    List {
        project_ref: Option<String>,
        #[arg(long)] installed_only: bool,
        #[arg(long)] profile: Option<String>,
    },
}

#[derive(Subcommand)]
enum SupabaseMigrationAction {
    List { project_ref: Option<String>, #[arg(long)] profile: Option<String> },
    Status {
        project_ref: Option<String>,
        #[arg(long, default_value = "supabase/migrations")] dir: std::path::PathBuf,
        #[arg(long)] profile: Option<String>,
    },
    Push {
        project_ref: Option<String>,
        #[arg(long, default_value = "supabase/migrations")] dir: std::path::PathBuf,
        #[arg(long)] profile: Option<String>,
        #[arg(long)] yes: bool,
    },
}

#[derive(Subcommand)]
enum SupabaseFunctionAction {
    List { project_ref: Option<String>, #[arg(long)] profile: Option<String> },
    View { slug: String, project_ref: Option<String>, #[arg(long)] profile: Option<String> },
    Deploy {
        slug: String,
        file: std::path::PathBuf,
        project_ref: Option<String>,
        #[arg(long)] no_verify_jwt: bool,
        #[arg(long)] profile: Option<String>,
        #[arg(long)] yes: bool,
    },
    Delete { slug: String, project_ref: Option<String>, #[arg(long)] profile: Option<String>, #[arg(long)] yes: bool },
}

#[derive(Subcommand)]
enum SupabaseSecretAction {
    List { project_ref: Option<String>, #[arg(long)] profile: Option<String> },
    Set {
        key: String,
        value: Option<String>,
        project_ref: Option<String>,
        #[arg(long)] profile: Option<String>,
    },
    Delete { key: String, project_ref: Option<String>, #[arg(long)] profile: Option<String>, #[arg(long)] yes: bool },
}

#[derive(Subcommand)]
enum SupabaseBranchAction {
    List { project_ref: Option<String>, #[arg(long)] profile: Option<String> },
    Create { name: String, project_ref: Option<String>, #[arg(long)] profile: Option<String> },
    Delete { branch_id: String, #[arg(long)] profile: Option<String>, #[arg(long)] yes: bool },
    Merge { branch_id: String, #[arg(long)] profile: Option<String>, #[arg(long)] yes: bool },
    Reset {
        branch_id: String,
        #[arg(long)] migration_version: Option<String>,
        #[arg(long)] profile: Option<String>,
        #[arg(long)] yes: bool,
    },
    Rebase { branch_id: String, #[arg(long)] profile: Option<String>, #[arg(long)] yes: bool },
}

#[derive(Subcommand)]
enum SupabaseAdvisorAction {
    Security    { project_ref: Option<String>, #[arg(long)] profile: Option<String> },
    Performance { project_ref: Option<String>, #[arg(long)] profile: Option<String> },
}

// ═════════════════════════════════════════════════════════════
//  main()
// ═════════════════════════════════════════════════════════════

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        // Erreur utilisateur normale (slug invalide, checksum, réseau, etc.) :
        // un message clair, jamais un stack backtrace Rust brut — celui-ci
        // n'apporte rien à un développeur qui a juste fait une faute de
        // frappe ou tapé un slug déjà pris, et RUST_BACKTRACE=1 est courant
        // chez les développeurs Rust (donc pas qu'un artefact de test).
        eprintln!("Error: {e:#}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    // ── Interception des commandes de providers dynamiques ────
    // AVANT tout parsing clap statique : si le premier argument
    // correspond à un provider installé via `iloc provider install`,
    // on déroute vers le moteur dynamique (provider_engine.rs) sans
    // jamais appeler Cli::parse() — qui, lui, ne connaît que l'arbre
    // de commandes compilé et échouerait sur un slug qu'il ignore.
    // Pour TOUTE autre commande, ce bloc ne fait rien : le flux normal
    // ci-dessous s'exécute exactement comme avant, sans exception.
    let raw_args: Vec<String> = std::env::args().collect();
    if let Some(first) = raw_args.get(1) {
        if provider_engine::is_installed_provider(first) {
            return provider_engine::dispatch(&raw_args).await;
        }
    }

    let cli = Cli::parse();

    match cli.command {

        // ── Local ─────────────────────────────────────────────
        Commands::Init {
            vault_mode, vault_dir, mirror,
            cloud_backup, hyperscale_backup,
            no_gitignore_patch, no_sentinel,
        } => {
            let opts = commands::init::InitOptions {
                vault_mode: vault_mode.as_deref().map(vault::VaultMode::parse).transpose()?,
                vault_dir,
                mirrors: mirror,
                cloud_backup,
                hyperscale_backup,
                no_gitignore_patch,
                sentinel: !no_sentinel,
            };
            commands::init::run(opts)?;
        }
        Commands::Save { message }  => commands::save::run(&message)?,
        Commands::Undo { id, file } => commands::undo::run(id, file)?,
        Commands::Log  { ids }      => commands::log::run(ids)?,
        Commands::Dashboard         => commands::dashboard::run()?,

        Commands::Status { health } => {
            if health { commands::intelligence::run_health()?; }
            else      { commands::status::run()?; }
        }

        Commands::Sentinel { action } => match action {
            SentinelAction::Init      => commands::sentinel::run_init()?,
            SentinelAction::Enable    => commands::sentinel::run_enable()?,
            SentinelAction::Disable   => commands::sentinel::run_disable()?,
            SentinelAction::Status    => commands::sentinel::run_status()?,
            SentinelAction::Uninstall => commands::sentinel::run_uninstall()?,
        },

        Commands::Completion { shell, setup } => {
            let s = match shell {
                CompletionShell::Bash       => commands::completion::Shell::Bash,
                CompletionShell::Zsh        => commands::completion::Shell::Zsh,
                CompletionShell::PowerShell => commands::completion::Shell::PowerShell,
            };
            if setup { commands::completion::run_setup(s)?; }
            else     { commands::completion::run(s)?; }
        }

        // ── Installation & mise à jour ─────────────────────────
        Commands::SelfInstall { dir, check, yes } => {
            commands::selfinstall::run(dir, check, yes)?;
        }
        Commands::Update { check } => {
            commands::update::run(check).await?;
        }

        // ── P2P ───────────────────────────────────────────────
        Commands::Share { port, relay, cloud, ttl, file, profile } => {
            if cloud {
                commands::cloud_share::run_share_cloud(ttl, file, profile).await?;
            } else {
                commands::share::run(port, relay, file).await?;
            }
        }
        Commands::Clone { key, key_secret, host, relay, port, dest } => {
            if cloud_share_token::is_cloud_share_token(&key) {
                let secret = key_secret.ok_or_else(|| anyhow::anyhow!(
                    "Les liens cloud nécessitent la clé de déchiffrement.\n\
                     Lancez: iloc clone <lien> --key-secret iloc://<project-key>"
                ))?;
                commands::cloud_share::run_clone_cloud(&key, &secret, dest).await?;
            } else {
                commands::clone::run(&key, host, relay, port, dest).await?;
            }
        }

        Commands::TransferStatus => run_transfer_status()?,

        // ── Studio (centre de commandes dans l'éditeur) ──────────
        Commands::Studio { action } => match action {
            StudioAction::Manifest { output } => {
                commands::studio::run_manifest(output)?;
            }
            StudioAction::Open => {
                commands::studio::run_open().await?;
            }
            StudioAction::Snapshots { limit } => {
                commands::studio::run_snapshots(limit)?;
            }
            StudioAction::DeployStatus => {
                commands::studio::run_deploy_status()?;
            }
            StudioAction::ProjectStatus => {
                commands::studio::run_project_status()?;
            }
        },

        // ── Hyperscale ────────────────────────────────────────
        Commands::Hyperscale { action } => {
            commands::hyperscale::dispatch(action).await?;
        }

        // ── Mesh node ─────────────────────────────────────────
        Commands::Node { action } => match action {
            NodeAction::Join   => commands::intelligence::run_node_join().await?,
            NodeAction::Leave  => commands::intelligence::run_node_leave().await?,
            NodeAction::Start  => commands::intelligence::run_node_start().await?,
            NodeAction::Status => commands::intelligence::run_node_status()?,
        },

        // ── Vault externalisé & sauvegarde 3-2-1 ──────────────
        Commands::Vault { action } => match action {
            VaultAction::Status        => commands::vault::run_status()?,
            VaultAction::Migrate { mode, dir } => commands::vault::run_migrate(mode, dir)?,
            VaultAction::Verify        => commands::vault::run_verify()?,
            VaultAction::Mirror { action } => match action {
                MirrorAction::Add { path }    => commands::vault::run_mirror_add(path)?,
                MirrorAction::Remove { path } => commands::vault::run_mirror_remove(path)?,
                MirrorAction::Sync            => commands::vault::run_mirror_sync()?,
            },
            VaultAction::Backup { action } => match action {
                BackupAction::EnableCloud { profile }  =>
                    commands::vault::run_backup_toggle(commands::vault::BackupTarget::Cloud, true, profile)?,
                BackupAction::DisableCloud             =>
                    commands::vault::run_backup_toggle(commands::vault::BackupTarget::Cloud, false, None)?,
                BackupAction::EnableHyperscale         =>
                    commands::vault::run_backup_toggle(commands::vault::BackupTarget::Hyperscale, true, None)?,
                BackupAction::DisableHyperscale        =>
                    commands::vault::run_backup_toggle(commands::vault::BackupTarget::Hyperscale, false, None)?,
            },
        },

        // ── Mesh (identité réseau optionnelle) ─────────────────
        Commands::Login   => commands::account::run_login().await?,
        Commands::Logout  => commands::account::run_logout().await?,
        Commands::Whoami  => commands::account::run_whoami().await?,

        // ── BYOC : le cloud personnel de l'utilisateur ─────────
        Commands::Config { action } => match action {
            ConfigAction::Cloud { action } => match action {
                CloudConfigAction::Add { name, activate } =>
                    commands::cloud::run_config_cloud_add(name, activate).await?,
                CloudConfigAction::List         => commands::cloud::run_config_cloud_list()?,
                CloudConfigAction::Use { name } => commands::cloud::run_config_cloud_use(name)?,
                CloudConfigAction::Remove { name } =>
                    commands::cloud::run_config_cloud_remove(name)?,
            },
        },
        Commands::Push { file, profile }     => commands::cloud::run_push(file, profile).await?,
        Commands::Pull { id, dest, file, profile } =>
            commands::cloud::run_pull(id, dest, file, profile).await?,
        Commands::Cloud { action } => match action {
            CloudAction::Usage { profile }   => commands::cloud::run_usage(profile).await?,
            CloudAction::Gc { profile, yes } => commands::cloud::run_gc(profile, yes).await?,
            CloudAction::Doctor { profile }  => commands::cloud::run_doctor(profile).await?,
            CloudAction::Verify { profile }  => commands::cloud::run_verify(profile).await?,
        },

        // ── GitHub / Vercel ───────────────────────────────────
        Commands::Connect { service, name, token, api_url } => match service.to_lowercase().as_str() {
            "github" => commands::github::run_connect(name, token, api_url).await?,
            "vercel"   => commands::vercel::run_connect(name, token).await?,
            "supabase" => commands::supabase::run_connect(name, token).await?,
            other => {
                if provider_engine::is_installed_provider(other) {
                    commands::provider::run_connect(other, name, api_url, token).await?
                } else {
                    anyhow::bail!(
                        "Service '{}' non reconnu. Services disponibles : github, vercel, supabase, \
                         ou un provider installé (voir `iloc provider list`)",
                        other
                    )
                }
            }
        },

        Commands::Provider { action } => match action {
            ProviderAction::Init { slug } => commands::provider::run_init(slug)?,
            ProviderAction::Validate { path } => commands::provider::run_validate(path)?,
            ProviderAction::Test { path } => commands::provider::run_test(path).await?,
            ProviderAction::Install { name, file } => commands::provider::run_install(name, file).await?,
            ProviderAction::Search { query } => commands::provider::run_search(&query).await?,
            ProviderAction::Publish { file } => commands::provider::run_publish(file).await?,
            ProviderAction::List => commands::provider::run_list()?,
            ProviderAction::Remove { slug, yes } => commands::provider::run_remove(slug, yes)?,
            ProviderAction::Profile { action } => match action {
                ProfileAction::List { slug } => commands::provider::run_profile_list(slug)?,
                ProfileAction::Use { slug, name } => commands::provider::run_profile_use(slug, name)?,
                ProfileAction::Remove { slug, name, yes } =>
                    commands::provider::run_profile_remove(slug, name, yes)?,
            },
        },

        Commands::GitHub { action } => match action {

            // Profils
            GitHubAction::List              => commands::github::run_list_profiles()?,
            GitHubAction::Use { name }      => commands::github::run_use_profile(name)?,
            GitHubAction::Status { profile } => commands::github::run_status(profile).await?,
            GitHubAction::Remove { name, yes } =>
                commands::github::run_remove_profile(name, yes)?,

            // Repos
            GitHubAction::Repo { action } => match action {
                GitHubRepoAction::Create {
                    name, description, private, public, org,
                    auto_init, topics, license, gitignore, profile, yes,
                } => commands::github::run_repo_create(
                    name, description, private, public, org,
                    auto_init, topics, license, gitignore, profile, yes,
                ).await?,
                GitHubRepoAction::List { org, private, public, fork, limit, profile } =>
                    commands::github::run_repo_list(org, private, public, fork, limit, profile).await?,
                GitHubRepoAction::View { owner_repo, profile } =>
                    commands::github::run_repo_view(owner_repo, profile).await?,
                GitHubRepoAction::Delete { owner_repo, profile, yes } =>
                    commands::github::run_repo_delete(owner_repo, profile, yes).await?,
                GitHubRepoAction::Archive { owner_repo, unarchive, profile, yes } =>
                    commands::github::run_repo_archive(owner_repo, unarchive, profile, yes).await?,
                GitHubRepoAction::Fork { owner_repo, org, profile } =>
                    commands::github::run_repo_fork(&owner_repo, org, profile).await?,
                GitHubRepoAction::Transfer { owner_repo, new_owner, profile, yes } =>
                    commands::github::run_repo_transfer(owner_repo, new_owner, profile, yes).await?,
                GitHubRepoAction::Topics { owner_repo, add, remove, set, profile } =>
                    commands::github::run_repo_topics(owner_repo, add, remove, set, profile).await?,
                GitHubRepoAction::Visibility { owner_repo, private, public, profile, yes } =>
                    commands::github::run_repo_visibility(owner_repo, private, public, profile, yes).await?,
                GitHubRepoAction::Rename { new_name, owner_repo, profile, yes } =>
                    commands::github::run_repo_rename(owner_repo, new_name, profile, yes).await?,
            },

            // Branches
            GitHubAction::Branch { action } => match action {
                GitHubBranchAction::List { owner_repo, profile } =>
                    commands::github::run_branch_list(owner_repo, profile).await?,
                GitHubBranchAction::Create { name, from, owner_repo, profile } =>
                    commands::github::run_branch_create(owner_repo, name, from, profile).await?,
                GitHubBranchAction::Delete { name, owner_repo, profile, yes } =>
                    commands::github::run_branch_delete(owner_repo, name, profile, yes).await?,
                GitHubBranchAction::Protect {
                    name, owner_repo, checks, require_pr, min_reviews,
                    enforce_admins, linear, allow_force_pushes, allow_deletions, profile,
                } => commands::github::run_branch_protect(
                    owner_repo, name, checks, require_pr, min_reviews,
                    enforce_admins, linear, allow_force_pushes, allow_deletions, profile,
                ).await?,
                GitHubBranchAction::Unprotect { name, owner_repo, profile, yes } =>
                    commands::github::run_branch_unprotect(owner_repo, name, profile, yes).await?,
                GitHubBranchAction::Default { name, owner_repo, profile } =>
                    commands::github::run_branch_default(owner_repo, name, profile).await?,
            },

            // Issues
            GitHubAction::Issue { action } => match action {
                GitHubIssueAction::List { owner_repo, state, labels, assignee, limit, profile } =>
                    commands::github::run_issue_list(owner_repo, state, labels, assignee, limit, profile).await?,
                GitHubIssueAction::Create { owner_repo, title, body, labels, assignees, milestone, profile } =>
                    commands::github::run_issue_create(owner_repo, title, body, labels, assignees, milestone, profile).await?,
                GitHubIssueAction::View { number, owner_repo, profile } =>
                    commands::github::run_issue_view(owner_repo, number, profile).await?,
                GitHubIssueAction::Close { number, owner_repo, reason, profile } =>
                    commands::github::run_issue_close(owner_repo, number, reason, profile).await?,
                GitHubIssueAction::Reopen { number, owner_repo, profile } =>
                    commands::github::run_issue_reopen(owner_repo, number, profile).await?,
                GitHubIssueAction::Comment { number, owner_repo, body, profile } =>
                    commands::github::run_issue_comment(owner_repo, number, body, profile).await?,
                GitHubIssueAction::Assign { number, users, owner_repo, unassign, profile } =>
                    commands::github::run_issue_assign(owner_repo, number, users, unassign, profile).await?,
                GitHubIssueAction::Label { number, owner_repo, add, remove, profile } =>
                    commands::github::run_issue_label(owner_repo, number, add, remove, profile).await?,
            },

            // Pull Requests
            GitHubAction::Pr { action } => match action {
                GitHubPrAction::List { owner_repo, state, base, limit, profile } =>
                    commands::github::run_pr_list(owner_repo, state, base, limit, profile).await?,
                GitHubPrAction::Create { owner_repo, title, body, head, base, draft, profile } =>
                    commands::github::run_pr_create(owner_repo, title, body, head, base, draft, profile).await?,
                GitHubPrAction::View { number, owner_repo, profile } =>
                    commands::github::run_pr_view(owner_repo, number, profile).await?,
                GitHubPrAction::Merge { number, owner_repo, method, title, message, profile, yes } =>
                    commands::github::run_pr_merge(owner_repo, number, method, title, message, profile, yes).await?,
                GitHubPrAction::Review { number, owner_repo, reviewers, teams, profile } =>
                    commands::github::run_pr_review(owner_repo, number, reviewers, teams, profile).await?,
                GitHubPrAction::Checkout { number, owner_repo, profile } =>
                    commands::github::run_pr_checkout(owner_repo, number, profile).await?,
                GitHubPrAction::Close { number, owner_repo, profile, yes } =>
                    commands::github::run_pr_close(owner_repo, number, profile, yes).await?,
                GitHubPrAction::Ready { number, owner_repo, draft, profile } =>
                    commands::github::run_pr_ready(owner_repo, number, draft, profile).await?,
                GitHubPrAction::UpdateBranch { number, owner_repo, profile } =>
                    commands::github::run_pr_update_branch(owner_repo, number, profile).await?,
            },

            // Releases
            GitHubAction::Release { action } => match action {
                GitHubReleaseAction::List { owner_repo, limit, profile } =>
                    commands::github::run_release_list(owner_repo, limit, profile).await?,
                GitHubReleaseAction::Create {
                    owner_repo, tag, name, body, draft, prerelease,
                    target, generate_notes, profile, yes,
                } => commands::github::run_release_create(
                    owner_repo, tag, name, body, draft, prerelease,
                    target, generate_notes, profile, yes,
                ).await?,
                GitHubReleaseAction::Delete { tag, owner_repo, profile, yes } =>
                    commands::github::run_release_delete(owner_repo, tag, profile, yes).await?,
                GitHubReleaseAction::Upload { tag, file, owner_repo, name, content_type, profile } =>
                    commands::github::run_release_upload(owner_repo, tag, file, name, content_type, profile).await?,
            },

            // GitHub Actions
            GitHubAction::Actions { action } => match action {
                GitHubActionsAction::List { owner_repo, profile } =>
                    commands::github::run_actions_list(owner_repo, profile).await?,
                GitHubActionsAction::Run { workflow, owner_repo, branch, inputs, profile } =>
                    commands::github::run_actions_run(owner_repo, workflow, branch, inputs, profile).await?,
                GitHubActionsAction::Status { owner_repo, workflow, branch, limit, profile } =>
                    commands::github::run_actions_status(owner_repo, workflow, branch, limit, profile).await?,
                GitHubActionsAction::Cancel { run_id, owner_repo, profile, yes } =>
                    commands::github::run_actions_cancel(owner_repo, run_id, profile, yes).await?,
                GitHubActionsAction::Rerun { run_id, owner_repo, profile } =>
                    commands::github::run_actions_rerun(owner_repo, run_id, profile).await?,
            },

            // Secrets
            GitHubAction::Secret { action } => match action {
                GitHubSecretAction::List { owner_repo, profile } =>
                    commands::github::run_secret_list(owner_repo, profile).await?,
                GitHubSecretAction::Set { name, owner_repo, value, profile } =>
                    commands::github::run_secret_set(owner_repo, name, value, profile).await?,
                GitHubSecretAction::Delete { name, owner_repo, profile, yes } =>
                    commands::github::run_secret_delete(owner_repo, name, profile, yes).await?,
            },

            // Collaborateurs
            GitHubAction::Collab { action } => match action {
                GitHubCollabAction::List { owner_repo, profile } =>
                    commands::github::run_collab_list(owner_repo, profile).await?,
                GitHubCollabAction::Add { username, owner_repo, permission, profile, yes } =>
                    commands::github::run_collab_add(owner_repo, username, permission, profile, yes).await?,
                GitHubCollabAction::Remove { username, owner_repo, profile, yes } =>
                    commands::github::run_collab_remove(owner_repo, username, profile, yes).await?,
            },

            // Webhooks
            GitHubAction::Webhook { action } => match action {
                GitHubWebhookAction::List { owner_repo, profile } =>
                    commands::github::run_webhook_list(owner_repo, profile).await?,
                GitHubWebhookAction::Create {
                    url, owner_repo, events, content_type, secret, inactive, profile,
                } => commands::github::run_webhook_create(
                    owner_repo, url, events, content_type, secret, inactive, profile,
                ).await?,
                GitHubWebhookAction::Delete { hook_id, owner_repo, profile, yes } =>
                    commands::github::run_webhook_delete(owner_repo, hook_id, profile, yes).await?,
                GitHubWebhookAction::Ping { hook_id, owner_repo, profile } =>
                    commands::github::run_webhook_ping(owner_repo, hook_id, profile).await?,
            },

            // Search
            GitHubAction::Search { action } => match action {
                GitHubSearchAction::Repos { query, limit, profile } =>
                    commands::github::run_search_repos(query, limit, profile).await?,
            },
        },



        // ── Vercel ────────────────────────────────────────────
        Commands::Vercel { action } => match action {
            VercelAction::List              => commands::vercel::run_list_profiles()?,
            VercelAction::Use { name }      => commands::vercel::run_use_profile(name)?,
            VercelAction::Status { profile } => commands::vercel::run_status(profile).await?,
            VercelAction::Remove { name, yes } => commands::vercel::run_remove_profile(name, yes)?,
            VercelAction::Deploy { prod, force, wait, project, branch, sha, timeout, profile, yes } =>
                commands::vercel::run_deploy(prod, force, wait, project, branch, sha, timeout, profile, yes).await?,
            VercelAction::Inspect { profile } => commands::vercel::run_inspect(profile).await?,
            VercelAction::Project { action } => match action {
                VercelProjectAction::List { limit, profile } => commands::vercel::run_project_list(limit, profile).await?,
                VercelProjectAction::Create { name, framework, root, build, output, install, git_repo, git_branch, link, profile, yes } =>
                    commands::vercel::run_project_create(name, framework, root, build, output, install, git_repo, git_branch, link, profile, yes).await?,
                VercelProjectAction::View { project, profile } => commands::vercel::run_project_view(project, profile).await?,
                VercelProjectAction::Update { project, name, framework, root, build, output, install, node, prod_branch, profile } =>
                    commands::vercel::run_project_update(project, name, framework, root, build, output, install, node, prod_branch, profile).await?,
                VercelProjectAction::Delete { project, profile, yes } => commands::vercel::run_project_delete(project, profile, yes).await?,
                VercelProjectAction::Link { project, profile } => commands::vercel::run_project_link(project, profile).await?,
                VercelProjectAction::Unlink { yes } => commands::vercel::run_project_unlink(yes)?,
            },
            VercelAction::Deployment { action } => match action {
                VercelDeploymentAction::List { project, target, state, limit, profile } =>
                    commands::vercel::run_deployment_list(project, target, state, limit, profile).await?,
                VercelDeploymentAction::View { id, profile } => commands::vercel::run_deployment_view(id, profile).await?,
                VercelDeploymentAction::Cancel { id, profile, yes } => commands::vercel::run_deployment_cancel(id, profile, yes).await?,
                VercelDeploymentAction::Delete { id, profile, yes } => commands::vercel::run_deployment_delete(id, profile, yes).await?,
                VercelDeploymentAction::Redeploy { id, target, profile, yes } => commands::vercel::run_deployment_redeploy(id, target, profile, yes).await?,
                VercelDeploymentAction::Promote { id, project, profile, yes } => commands::vercel::run_deployment_promote(id, project, profile, yes).await?,
                VercelDeploymentAction::Logs { id, profile } => commands::vercel::run_deployment_logs(id, profile).await?,
                VercelDeploymentAction::Files { id, profile } => commands::vercel::run_deployment_files(id, profile).await?,
            },
            VercelAction::Env { action } => match action {
                VercelEnvAction::List { project, profile } => commands::vercel::run_env_list(project, profile).await?,
                VercelEnvAction::Add { key, value, target, env_type, git_branch, project, profile } =>
                    commands::vercel::run_env_add(key, value, target, env_type, git_branch, project, profile).await?,
                VercelEnvAction::Remove { key, target, project, profile, yes } =>
                    commands::vercel::run_env_remove(key, target, project, profile, yes).await?,
                VercelEnvAction::Pull { output, target, project, profile, yes } =>
                    commands::vercel::run_env_pull(output, project, target, profile, yes).await?,
                VercelEnvAction::Push { input, target, project, profile, yes } =>
                    commands::vercel::run_env_push(input, target, project, profile, yes).await?,
            },
            VercelAction::Domain { action } => match action {
                VercelDomainAction::List { project, limit, profile } => commands::vercel::run_domain_list(project, limit, profile).await?,
                VercelDomainAction::Add { domain, project, git_branch, redirect, profile } =>
                    commands::vercel::run_domain_add(domain, project, git_branch, redirect, profile).await?,
                VercelDomainAction::Remove { domain, project, profile, yes } =>
                    commands::vercel::run_domain_remove(domain, project, profile, yes).await?,
                VercelDomainAction::Inspect { domain, project, profile } =>
                    commands::vercel::run_domain_inspect(domain, project, profile).await?,
                VercelDomainAction::Check { domain, profile } => commands::vercel::run_domain_check(domain, profile).await?,
                VercelDomainAction::Dns { action } => match action {
                    VercelDnsAction::List { domain, profile } => commands::vercel::run_dns_list(domain, profile).await?,
                    VercelDnsAction::Add { domain, name, rec_type, value, ttl, priority, profile } =>
                        commands::vercel::run_dns_add(domain, name, rec_type, value, ttl, priority, profile).await?,
                    VercelDnsAction::Remove { domain, record_id, profile, yes } =>
                        commands::vercel::run_dns_remove(domain, record_id, profile, yes).await?,
                },
            },
            VercelAction::Alias { action } => match action {
                VercelAliasAction::List { project, limit, profile } => commands::vercel::run_alias_list(project, limit, profile).await?,
                VercelAliasAction::Assign { deployment_id, alias, redirect, profile } =>
                    commands::vercel::run_alias_assign(deployment_id, alias, redirect, profile).await?,
                VercelAliasAction::Delete { alias, profile, yes } => commands::vercel::run_alias_delete(alias, profile, yes).await?,
            },
            VercelAction::Secret { action } => match action {
                VercelSecretAction::List { profile } => commands::vercel::run_secret_list(profile).await?,
                VercelSecretAction::Add { name, value, profile } => commands::vercel::run_secret_add(name, value, profile).await?,
                VercelSecretAction::Rename { name, new_name, profile } => commands::vercel::run_secret_rename(name, new_name, profile).await?,
                VercelSecretAction::Delete { name, profile, yes } => commands::vercel::run_secret_delete(name, profile, yes).await?,
            },
            VercelAction::Edge { action } => match action {
                VercelEdgeAction::List { profile } => commands::vercel::run_edge_list(profile).await?,
                VercelEdgeAction::Create { slug, profile } => commands::vercel::run_edge_create(slug, profile).await?,
                VercelEdgeAction::Items { id, profile } => commands::vercel::run_edge_items(id, profile).await?,
                VercelEdgeAction::Update { id, items, profile } => commands::vercel::run_edge_update(id, items, profile).await?,
                VercelEdgeAction::Delete { id, profile, yes } => commands::vercel::run_edge_delete(id, profile, yes).await?,
            },
            VercelAction::Webhook { action } => match action {
                VercelWebhookAction::List { profile } => commands::vercel::run_webhook_list(profile).await?,
                VercelWebhookAction::Create { url, events, profile } => commands::vercel::run_webhook_create(url, events, profile).await?,
                VercelWebhookAction::Delete { id, profile, yes } => commands::vercel::run_webhook_delete(id, profile, yes).await?,
            },
            VercelAction::Check { action } => match action {
                VercelCheckAction::List { deployment_id, profile } => commands::vercel::run_check_list(deployment_id, profile).await?,
                VercelCheckAction::Create { deployment_id, name, detached, blocking, profile } =>
                    commands::vercel::run_check_create(deployment_id, name, detached, blocking, profile).await?,
                VercelCheckAction::Update { deployment_id, check_id, status, conclusion, profile } =>
                    commands::vercel::run_check_update(deployment_id, check_id, status, conclusion, profile).await?,
            },
            VercelAction::Team { action } => match action {
                VercelTeamAction::List { profile } => commands::vercel::run_team_list(profile).await?,
                VercelTeamAction::Switch { slug, profile } => commands::vercel::run_team_switch(slug, profile).await?,
            },
        },

        // ── Supabase ──────────────────────────────────────────
        Commands::Supabase { action } => match action {
            SupabaseAction::List              => commands::supabase::run_list_profiles()?,
            SupabaseAction::Use { name }      => commands::supabase::run_use_profile(name)?,
            SupabaseAction::Status { profile } => commands::supabase::run_status(profile).await?,
            SupabaseAction::Remove { name, yes } => commands::supabase::run_remove_profile(name, yes)?,

            SupabaseAction::Org { action } => match action {
                SupabaseOrgAction::List { profile } => commands::supabase::run_org_list(profile).await?,
            },

            SupabaseAction::Project { action } => match action {
                SupabaseProjectAction::Create { name, org, region, db_pass, link, profile, yes } =>
                    commands::supabase::run_project_create(name, org, region, db_pass, link, profile, yes).await?,
                SupabaseProjectAction::List { profile } => commands::supabase::run_project_list(profile).await?,
                SupabaseProjectAction::View { project_ref, profile } =>
                    commands::supabase::run_project_view(project_ref, profile).await?,
                SupabaseProjectAction::Delete { project_ref, profile, yes } =>
                    commands::supabase::run_project_delete(project_ref, profile, yes).await?,
                SupabaseProjectAction::Pause { project_ref, profile, yes } =>
                    commands::supabase::run_project_pause(project_ref, profile, yes).await?,
                SupabaseProjectAction::Restore { project_ref, profile, yes } =>
                    commands::supabase::run_project_restore(project_ref, profile, yes).await?,
                SupabaseProjectAction::Url { project_ref, profile } =>
                    commands::supabase::run_project_url(project_ref, profile).await?,
            },

            SupabaseAction::Keys { project_ref, reveal, profile } =>
                commands::supabase::run_keys_show(project_ref, profile, reveal).await?,

            SupabaseAction::Sql { project_ref, query, profile, yes } =>
                commands::supabase::run_sql(project_ref, query, profile, yes).await?,

            SupabaseAction::Table { action } => match action {
                SupabaseTableAction::List { project_ref, schema, profile } =>
                    commands::supabase::run_table_list(project_ref, schema, profile).await?,
            },

            SupabaseAction::Extension { action } => match action {
                SupabaseExtensionAction::List { project_ref, installed_only, profile } =>
                    commands::supabase::run_extension_list(project_ref, installed_only, profile).await?,
            },

            SupabaseAction::Migration { action } => match action {
                SupabaseMigrationAction::List { project_ref, profile } =>
                    commands::supabase::run_migration_list(project_ref, profile).await?,
                SupabaseMigrationAction::Status { project_ref, dir, profile } =>
                    commands::supabase::run_migration_status(project_ref, dir, profile).await?,
                SupabaseMigrationAction::Push { project_ref, dir, profile, yes } =>
                    commands::supabase::run_migration_push(project_ref, dir, profile, yes).await?,
            },

            SupabaseAction::Function { action } => match action {
                SupabaseFunctionAction::List { project_ref, profile } =>
                    commands::supabase::run_function_list(project_ref, profile).await?,
                SupabaseFunctionAction::View { project_ref, slug, profile } =>
                    commands::supabase::run_function_view(project_ref, slug, profile).await?,
                SupabaseFunctionAction::Deploy { project_ref, slug, file, no_verify_jwt, profile, yes } =>
                    commands::supabase::run_function_deploy(project_ref, slug, file, no_verify_jwt, profile, yes).await?,
                SupabaseFunctionAction::Delete { project_ref, slug, profile, yes } =>
                    commands::supabase::run_function_delete(project_ref, slug, profile, yes).await?,
            },

            SupabaseAction::Secret { action } => match action {
                SupabaseSecretAction::List { project_ref, profile } =>
                    commands::supabase::run_secret_list(project_ref, profile).await?,
                SupabaseSecretAction::Set { project_ref, key, value, profile } =>
                    commands::supabase::run_secret_set(project_ref, key, value, profile).await?,
                SupabaseSecretAction::Delete { project_ref, key, profile, yes } =>
                    commands::supabase::run_secret_delete(project_ref, key, profile, yes).await?,
            },

            SupabaseAction::Branch { action } => match action {
                SupabaseBranchAction::List { project_ref, profile } =>
                    commands::supabase::run_branch_list(project_ref, profile).await?,
                SupabaseBranchAction::Create { project_ref, name, profile } =>
                    commands::supabase::run_branch_create(project_ref, name, profile).await?,
                SupabaseBranchAction::Delete { branch_id, profile, yes } =>
                    commands::supabase::run_branch_delete(branch_id, profile, yes).await?,
                SupabaseBranchAction::Merge { branch_id, profile, yes } =>
                    commands::supabase::run_branch_merge(branch_id, profile, yes).await?,
                SupabaseBranchAction::Reset { branch_id, migration_version, profile, yes } =>
                    commands::supabase::run_branch_reset(branch_id, migration_version, profile, yes).await?,
                SupabaseBranchAction::Rebase { branch_id, profile, yes } =>
                    commands::supabase::run_branch_rebase(branch_id, profile, yes).await?,
            },

            SupabaseAction::Advisor { action } => match action {
                SupabaseAdvisorAction::Security { project_ref, profile } =>
                    commands::supabase::run_advisor_show(project_ref, "security".to_string(), profile).await?,
                SupabaseAdvisorAction::Performance { project_ref, profile } =>
                    commands::supabase::run_advisor_show(project_ref, "performance".to_string(), profile).await?,
            },
        },

        // ── Deploy (orchestrateur GitHub + Vercel + Supabase) ──
        Commands::Deploy {
            yes, dry_run, force_new, skip_github, skip_vercel, skip_supabase,
            github_profile, vercel_profile, supabase_profile, org, team,
        } => {
            let cwd = std::env::current_dir()?;
            let ctx = commands::deploy::DeployContext {
                yes, dry_run, force_new, skip_github, skip_vercel, skip_supabase,
                github_profile, vercel_profile, supabase_profile, org, team,
            };
            commands::deploy::run(&cwd, ctx).await?;
        }

    }

    Ok(())
}

// ── run_transfer_status (inchangé) ───────────────────────────

fn run_transfer_status() -> Result<()> {
    use colored::Colorize;
    let cwd         = std::env::current_dir()?;
    let ilocker_dir = cwd.join(".ilocker");
    if !ilocker_dir.exists() { println!("Not an ilocker project."); return Ok(()); }
    match transfer_state::load(&ilocker_dir)? {
        None => println!("{}", "\n  No in-progress transfer found.".dimmed()),
        Some(xfr) => {
            println!();
            println!("{}", "  In-progress transfer".bold());
            println!("  {} {:?}", "direction:".dimmed(), xfr.direction);
            println!("  {} {}", "snapshot:".dimmed(),  xfr.snapshot_id.cyan());
            println!("  {} {}/{} chunks ({:.0}%)",
                "progress:".dimmed(),
                xfr.completed_chunks.len(), xfr.total_chunks, xfr.progress_pct());
            println!();
            println!("{}", "  Re-run iloc share / iloc clone to resume.".dimmed());
        }
    }
    Ok(())
}
