// ============================================================
//  hyperscale_config.rs — Configuration Hyperscale
//
//  v1.13.0 — Simplifié : les credentials des providers cloud
//  sont désormais gérés exclusivement par cloud_store.rs
//  (multi-profils, keyring OS). HyperscaleConfig ne contient
//  plus que les paramètres d'architecture erasure et de nœud.
// ============================================================

use crate::cloud_store;
use crate::erasure::{self, DEFAULT_DATA_SHARDS, DEFAULT_PARITY_SHARDS};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const CONFIG_FILE: &str = "hyperscale.json";

// ── Configuration principale ──────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HyperscaleConfig {
    /// Identifiant de l'organisation (UUID, clé projet, ou nom libre).
    pub org_id: String,
    /// Nombre de fragments de données (k).
    pub data_shards: usize,
    /// Nombre de fragments de parité (m).
    pub parity_shards: usize,
    /// Espace alloué pour le nœud de partage en Go.
    #[serde(default = "default_peer_allocation")]
    pub peer_allocation_gb: u64,
    /// Mode silencieux du nœud (contribution sans notification).
    #[serde(default)]
    pub silent_peer_enabled: bool,
}

fn default_peer_allocation() -> u64 { 10 }

impl Default for HyperscaleConfig {
    fn default() -> Self {
        Self {
            org_id:               uuid::Uuid::new_v4().to_string(),
            data_shards:          DEFAULT_DATA_SHARDS,
            parity_shards:        DEFAULT_PARITY_SHARDS,
            peer_allocation_gb:   10,
            silent_peer_enabled:  false,
        }
    }
}

impl HyperscaleConfig {
    // ── Persistance ────────────────────────────────────────────

    pub fn load(hs_dir: &Path) -> Result<Self> {
        let path = hs_dir.join(CONFIG_FILE);
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!(
                "Hyperscale non configuré — lancez `iloc hyperscale config init` d'abord. ({})",
                path.display()
            ))?;
        serde_json::from_str(&raw).context("Fichier de configuration Hyperscale corrompu")
    }

    pub fn save(&self, hs_dir: &Path) -> Result<()> {
        std::fs::create_dir_all(hs_dir)?;
        std::fs::write(
            hs_dir.join(CONFIG_FILE),
            serde_json::to_string_pretty(self)?,
        )?;
        Ok(())
    }

    // ── Validation ─────────────────────────────────────────────

    pub fn validate(&self) -> Result<()> {
        if self.data_shards == 0 {
            bail!("data_shards doit être ≥ 1");
        }
        if self.parity_shards == 0 {
            bail!("parity_shards doit être ≥ 1");
        }

        let profiles = cloud_store::list_profiles()?;
        let n = profiles.profiles.len();

        if n == 0 {
            bail!(
                "Aucun profil cloud configuré pour Hyperscale.\n\
                 Lancez `iloc config cloud add` pour connecter votre propre AWS/GCS/DO/Supabase/Backblaze/R2/Wasabi/MinIO.\n\
                 Vous avez besoin d'au moins {} profil(s) pour k={}, m={}.",
                (self.data_shards + self.parity_shards + 2) / 3,
                self.data_shards, self.parity_shards
            );
        }

        // Recommandation de sécurité : aucun cloud unique ne devrait
        // pouvoir reconstruire les données seul (il lui faudrait k shards).
        // Exception : le mode "miroir" (data_shards == 1) donne PAR DÉFINITION
        // une copie complète à chaque cloud — c'est précisément son intérêt
        // (redondance simple façon RAID-1), donc ce garde-fou ne s'applique
        // qu'à partir de k ≥ 2 (schémas à fragmentation réelle).
        if self.data_shards >= 2 {
            let shards_per_provider = (self.data_shards + self.parity_shards + n - 1) / n;
            if n > 1 && shards_per_provider >= self.data_shards {
                bail!(
                    "Configuration non sécurisée : avec {} cloud(s), chacun recevrait ~{} shards ≥ k={}.\n\
                     Un seul cloud compromis pourrait reconstruire vos données.\n\
                     Ajoutez plus de profils cloud ou réduisez k (data_shards).",
                    n, shards_per_provider, self.data_shards
                );
            }
        }

        Ok(())
    }

    // ── Accesseurs ─────────────────────────────────────────────

    /// Liste des noms de profils cloud actifs, triés de façon
    /// déterministe pour une distribution reproductible.
    pub fn active_provider_names(&self) -> Vec<String> {
        cloud_store::list_profiles()
            .map(|cfg| {
                let mut names: Vec<String> = cfg.profiles.iter()
                    .map(|p| p.name.clone())
                    .collect();
                names.sort();
                names
            })
            .unwrap_or_default()
    }

    /// Suggère (data_shards, parity_shards) selon le nombre de clouds
    /// configurés — appelé automatiquement si l'utilisateur ne
    /// renseigne pas ces valeurs manuellement.
    pub fn auto_shard_params(&self) -> (usize, usize) {
        let n = self.active_provider_names().len();
        erasure::suggest_shard_params(n)
    }
}

// ── Chemin de stockage local des shards (cache téléchargé) ───

pub fn local_shard_dir(ilocker_dir: &Path) -> PathBuf {
    ilocker_dir.join("hyperscale").join("shard-cache")
}

/// Clé S3 d'un shard sur le cloud d'un profil donné.
/// Format : `hyperscale/shards/<chunk_sha256>/<shard_index>`
pub fn remote_shard_key(chunk_sha256: &str, shard_index: usize) -> String {
    format!("hyperscale/shards/{}/{}", chunk_sha256, shard_index)
}

/// Clé S3 du manifest Hyperscale d'un snapshot.
pub fn remote_manifest_key(snapshot_id: &str) -> String {
    format!("hyperscale/manifests/{}.enc.json", snapshot_id)
}
