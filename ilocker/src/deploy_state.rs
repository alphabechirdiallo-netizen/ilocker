// ============================================================
//  deploy_state.rs — État de déploiement (.ilocker/deploy.toml)
//
//  Contrairement à .vercel/project.json et .supabase/project.json
//  (gitignorés par convention des CLIs officiels), CE fichier est
//  fait pour être commité. Il ne contient jamais de secret — que
//  des identifiants publics (owner/repo, project_id, project_ref)
//  et des empreintes SHA-256 de valeurs d'env vars (jamais les
//  valeurs elles-mêmes). L'intérêt : un collègue qui clone le repo
//  sait immédiatement quelles ressources sont déjà liées, sans
//  qu'ilocker ne recrée jamais un doublon faute de fichiers de
//  liaison locaux absents sur sa machine.
// ============================================================

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DeployState {
    pub github:   Option<GithubLink>,
    pub vercel:   Option<VercelLink>,
    pub supabase: Option<SupabaseLink>,
    #[serde(default)]
    pub env_hashes: std::collections::BTreeMap<String, String>,
    pub last_deploy: Option<LastDeploy>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GithubLink {
    pub owner:     String,
    pub repo:      String,
    pub linked_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VercelLink {
    pub project_id: String,
    pub team_id:    Option<String>,
    pub linked_at:  String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SupabaseLink {
    pub project_ref: String,
    pub org_id:      String,
    pub linked_at:   String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LastDeploy {
    pub git_sha:              Option<String>,
    pub vercel_deployment_id: Option<String>,
    pub deployed_at:          String,
}

// ── Chemin ──────────────────────────────────────────────────────

pub fn state_path(project_dir: &Path) -> PathBuf {
    project_dir.join(".ilocker").join("deploy.toml")
}

// ── Chargement / sauvegarde ───────────────────────────────────────

pub fn load_state(project_dir: &Path) -> Result<DeployState> {
    let path = state_path(project_dir);
    if !path.exists() {
        return Ok(DeployState::default());
    }
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("Lecture de {}", path.display()))?;
    Ok(toml::from_str(&raw).unwrap_or_default())
}

pub fn save_state(project_dir: &Path, state: &DeployState) -> Result<()> {
    let path = state_path(project_dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let raw = toml::to_string_pretty(state)?;
    let header = "# Fichier d'état ilocker deploy — généré et maintenu automatiquement.\n\
                  # Ne contient AUCUN secret : uniquement des identifiants publics et des\n\
                  # empreintes de hachage. Ce fichier DOIT être commité dans git, pour que\n\
                  # toute l'équipe partage la même liaison de ressources et qu'iloc deploy\n\
                  # n'en recrée jamais en double.\n\n";
    std::fs::write(&path, format!("{}{}", header, raw))
        .with_context(|| format!("Écriture de {}", path.display()))?;
    Ok(())
}

// ── Hachage des valeurs d'environnement ──────────────────────────
//
// Utilisé pour la synchronisation idempotente des secrets qui ne
// peuvent jamais être relus (GitHub Actions secrets). On stocke
// sha256(valeur) et on compare avant de pousser — jamais la valeur.

pub fn hash_value(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Clé composite pour l'entrée de hash : "provider:nom_variable".
pub fn env_hash_key(provider: &str, var_name: &str) -> String {
    format!("{}:{}", provider, var_name)
}

// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("iloc_deploystate_test_{}", uuid_like()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn uuid_like() -> String {
        format!("{:x}", std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos())
    }

    #[test]
    fn load_on_missing_file_returns_default() {
        let dir = temp_dir();
        let state = load_state(&dir).unwrap();
        assert!(state.github.is_none());
        assert!(state.vercel.is_none());
        assert!(state.supabase.is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn round_trip_full_state() {
        let dir = temp_dir();
        let mut state = DeployState::default();
        state.github = Some(GithubLink {
            owner: "bechir".into(), repo: "mon-app".into(), linked_at: "2026-07-10T00:00:00Z".into(),
        });
        state.vercel = Some(VercelLink {
            project_id: "prj_abc".into(), team_id: Some("team_xyz".into()), linked_at: "2026-07-10T00:00:00Z".into(),
        });
        state.supabase = Some(SupabaseLink {
            project_ref: "abcdef123456".into(), org_id: "org_1".into(), linked_at: "2026-07-10T00:00:00Z".into(),
        });
        state.env_hashes.insert(env_hash_key("vercel", "DATABASE_URL"), hash_value("postgres://x"));
        state.last_deploy = Some(LastDeploy {
            git_sha: Some("a1b2c3".into()), vercel_deployment_id: Some("dpl_xyz".into()),
            deployed_at: "2026-07-10T01:00:00Z".into(),
        });

        save_state(&dir, &state).unwrap();
        let loaded = load_state(&dir).unwrap();

        assert_eq!(loaded.github.unwrap().repo, "mon-app");
        assert_eq!(loaded.vercel.unwrap().project_id, "prj_abc");
        assert_eq!(loaded.supabase.unwrap().project_ref, "abcdef123456");
        assert_eq!(loaded.env_hashes.get("vercel:DATABASE_URL"), Some(&hash_value("postgres://x")));
        assert_eq!(loaded.last_deploy.unwrap().git_sha, Some("a1b2c3".into()));

        // Vérifier qu'aucune valeur en clair ne fuite dans le fichier écrit
        let raw = std::fs::read_to_string(state_path(&dir)).unwrap();
        assert!(!raw.contains("postgres://x"), "le fichier d'état ne doit JAMAIS contenir de valeur en clair");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn round_trip_partial_state_no_null_literals() {
        // Cas réel le plus fréquent : seul GitHub est lié pour l'instant.
        let dir = temp_dir();
        let mut state = DeployState::default();
        state.github = Some(GithubLink {
            owner: "bechir".into(), repo: "mon-app".into(), linked_at: "t".into(),
        });
        save_state(&dir, &state).unwrap();

        let raw = std::fs::read_to_string(state_path(&dir)).unwrap();
        assert!(!raw.contains("null"), "TOML ne doit jamais contenir 'null' littéral : {}", raw);

        let loaded = load_state(&dir).unwrap();
        assert!(loaded.vercel.is_none());
        assert!(loaded.supabase.is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn hash_value_is_deterministic_and_sensitive_to_change() {
        let h1 = hash_value("secret123");
        let h2 = hash_value("secret123");
        let h3 = hash_value("secret124");
        assert_eq!(h1, h2);
        assert_ne!(h1, h3);
    }

    #[test]
    fn corrupt_toml_falls_back_to_default_instead_of_crashing() {
        let dir = temp_dir();
        std::fs::create_dir_all(dir.join(".ilocker")).unwrap();
        std::fs::write(state_path(&dir), "ceci n'est pas [[[ du toml valide").unwrap();
        let state = load_state(&dir).unwrap();
        assert!(state.github.is_none());
        std::fs::remove_dir_all(&dir).ok();
    }
}
