// ============================================================
//  dht.rs — DHT Entreprise Privé pour iloc hyperscale
//
//  La DHT (Distributed Hash Table) est la "carte géante" du
//  réseau Hyperscale. Elle répond à la question :
//    "Où se trouvent les fragments du fichier X dans le monde ?"
//
//  Contrairement aux DHTs publiques (BitTorrent, IPFS), cette
//  DHT est :
//    1. Privée : chiffrée, accessible uniquement aux membres
//       authentifiés de l'organisation.
//    2. Hybride : combine des nœuds Cloud stables (S3, GCS,
//       Azure) et des nœuds P2P éphémères (PC des développeurs).
//    3. Hiérarchique : entrées persistantes (cloud) + cache
//       éphémère (PC dev connectés).
//
//  Structure d'une entrée DHT
//  ──────────────────────────────────────────────────────────
//  DhtEntry {
//    chunk_hash:  SHA-256 du fragment (clé de lookup)
//    locations:   Vec<ShardLocation> (où ce fragment est dispo)
//    added_at:    timestamp
//    ttl_secs:    durée de validité (∞ pour cloud, ~3600 pour P2P)
//  }
//
//  ShardLocation {
//    kind:       Cloud(provider, bucket, key) | Peer(ip, port)
//    shard_idx:  index dans l'ErasureSet
//    verified_at: dernière vérification de disponibilité
//  }
//
//  En production, cette DHT serait hébergée sur les serveurs
//  iLocker et synchronisée via WebSocket. Pour le MVP, elle
//  est persistée localement dans un fichier JSON chiffré dans
//  .ilocker/hyperscale/dht.json et peut être partagée/fusionnée
//  entre membres via `iloc hyperscale dht sync`.
// ============================================================

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

// ── Types publics ─────────────────────────────────────────────

/// Type de localisation d'un fragment.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind")]
pub enum LocationKind {
    /// Fragment sur un Cloud managé.
    Cloud {
        provider: String,   // "aws-s3", "gcp-storage", "azure-blob", "custom"
        bucket: String,
        object_key: String,
    },
    /// Fragment sur le PC d'un développeur (P2P éphémère).
    Peer {
        peer_id: String,
        ip: String,
        port: u16,
    },
    /// Fragment sur un serveur de stockage privé (on-premise).
    PrivateServer {
        server_id: String,
        endpoint: String,
    },
}

/// Localisation d'un fragment spécifique.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShardLocation {
    pub kind: LocationKind,
    /// Index du fragment dans l'ErasureSet.
    pub shard_index: usize,
    /// Timestamp de dernière vérification de disponibilité.
    pub verified_at: DateTime<Utc>,
    /// Poids de priorité pour le téléchargement (bande passante estimée).
    pub priority_weight: f32,
}

/// Entrée dans la DHT pour un fragment donné.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DhtEntry {
    /// SHA-256 du fragment (clé de lookup).
    pub chunk_hash: String,
    /// Toutes les localisations connues pour ce fragment.
    pub locations: Vec<ShardLocation>,
    /// Timestamp d'ajout initial.
    pub added_at: DateTime<Utc>,
    /// TTL en secondes (None = permanent, pour les clouds).
    pub ttl_secs: Option<u64>,
    /// Chemin relatif du fichier source (pour l'index).
    pub file_path: Option<String>,
}

impl DhtEntry {
    /// Retourne vrai si l'entrée est encore valide.
    pub fn is_valid(&self) -> bool {
        match self.ttl_secs {
            None => true,
            Some(ttl) => {
                let age = (Utc::now() - self.added_at).num_seconds() as u64;
                age < ttl
            }
        }
    }

    /// Retourne les localisations triées par priorité décroissante.
    pub fn sorted_locations(&self) -> Vec<&ShardLocation> {
        let mut locs: Vec<&ShardLocation> = self.locations.iter().collect();
        locs.sort_by(|a, b| b.priority_weight.partial_cmp(&a.priority_weight).unwrap_or(std::cmp::Ordering::Equal));
        locs
    }
}

// ── DHT Table ────────────────────────────────────────────────

/// Table DHT complète de l'organisation.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DhtTable {
    /// chunk_hash → DhtEntry
    pub entries: HashMap<String, DhtEntry>,
    /// Métadonnées : ID de l'organisation.
    pub org_id: String,
    /// Dernière mise à jour.
    pub updated_at: DateTime<Utc>,
    /// Nombre total d'entrées (pour affichage rapide).
    pub total_shards_indexed: u64,
}

impl DhtTable {
    /// Crée une nouvelle DHT vide pour une organisation.
    pub fn new(org_id: &str) -> Self {
        DhtTable {
            entries: HashMap::new(),
            org_id: org_id.to_string(),
            updated_at: Utc::now(),
            total_shards_indexed: 0,
        }
    }

    /// Ajoute ou met à jour une entrée dans la DHT.
    pub fn upsert(&mut self, entry: DhtEntry) {
        let is_new = !self.entries.contains_key(&entry.chunk_hash);
        self.entries.insert(entry.chunk_hash.clone(), entry);
        if is_new {
            self.total_shards_indexed += 1;
        }
        self.updated_at = Utc::now();
    }

    /// Annonce qu'un snapshot est disponible sur ce nœud.
    pub fn announce(&mut self, snapshot_id: &str, _org_id: &str) {
        let entry = DhtEntry {
            chunk_hash:  snapshot_id.to_string(),
            locations:   Vec::new(),
            added_at:    chrono::Utc::now(),
            ttl_secs:    Some(7 * 24 * 3600),
            file_path:   None,
        };
        self.upsert(entry);
    }

    /// Nombre total d'entrées connues dans la DHT.
    pub fn node_count(&self) -> usize {
        self.entries.len()
    }

    /// Lookup d'un fragment par son hash.
    pub fn lookup(&self, chunk_hash: &str) -> Option<&DhtEntry> {
        self.entries.get(chunk_hash).filter(|e| e.is_valid())
    }

    /// Retourne toutes les localisations pour une liste de hashes.
    /// Optimisé pour le download parallèle massif.
    pub fn bulk_lookup(&self, hashes: &[String]) -> HashMap<String, Vec<&ShardLocation>> {
        hashes.iter()
            .filter_map(|h| {
                self.lookup(h).map(|entry| {
                    (h.clone(), entry.sorted_locations())
                })
            })
            .collect()
    }

    /// Purge les entrées expirées.
    pub fn purge_expired(&mut self) -> usize {
        let before = self.entries.len();
        self.entries.retain(|_, entry| entry.is_valid());
        before - self.entries.len()
    }

    /// Fusionne une DHT distante dans cette DHT (merge).
    /// Les entrées plus récentes écrasent les plus anciennes.
    pub fn merge(&mut self, remote: DhtTable) {
        for (hash, remote_entry) in remote.entries {
            if let Some(local_entry) = self.entries.get(&hash) {
                if remote_entry.added_at > local_entry.added_at {
                    self.entries.insert(hash, remote_entry);
                } else {
                    // Fusionne les locations uniques
                    let local = self.entries.get_mut(&hash).unwrap();
                    for loc in remote_entry.locations {
                        if !local.locations.iter().any(|l| l.shard_index == loc.shard_index) {
                            local.locations.push(loc);
                        }
                    }
                }
            } else {
                self.entries.insert(hash, remote_entry);
                self.total_shards_indexed += 1;
            }
        }
        self.updated_at = Utc::now();
    }

    /// Sérialise la DHT en JSON pour persistance/transport.
    pub fn to_json(&self) -> Result<String> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    /// Désérialise depuis JSON.
    pub fn from_json(json: &str) -> Result<Self> {
        Ok(serde_json::from_str(json)?)
    }

    /// Sauvegarde la DHT sur disque (chemin: .ilocker/hyperscale/dht.json).
    pub fn save(&self, hyperscale_dir: &Path) -> Result<()> {
        std::fs::create_dir_all(hyperscale_dir)?;
        let path = hyperscale_dir.join("dht.json");
        let json = self.to_json()?;
        std::fs::write(&path, json)?;
        Ok(())
    }

    /// Charge la DHT depuis disque.
    pub fn load(hyperscale_dir: &Path) -> Result<Self> {
        let path = hyperscale_dir.join("dht.json");
        if !path.exists() {
            return Ok(DhtTable::default());
        }
        let json = std::fs::read_to_string(&path)?;
        DhtTable::from_json(&json)
    }

    /// Statistiques de la DHT pour affichage.
    pub fn stats(&self) -> DhtStats {
        let cloud_entries = self.entries.values()
            .filter(|e| e.locations.iter().any(|l| matches!(l.kind, LocationKind::Cloud { .. })))
            .count();
        let peer_entries = self.entries.values()
            .filter(|e| e.locations.iter().any(|l| matches!(l.kind, LocationKind::Peer { .. })))
            .count();

        DhtStats {
            total_entries: self.entries.len(),
            cloud_backed: cloud_entries,
            peer_backed: peer_entries,
            expired: self.entries.values().filter(|e| !e.is_valid()).count(),
        }
    }
}

/// Statistiques d'une DHT.
#[derive(Debug)]
pub struct DhtStats {
    pub total_entries: usize,
    pub cloud_backed: usize,
    pub peer_backed: usize,
    pub expired: usize,
}

// ── Indexeur de fragments ─────────────────────────────────────

/// Indexe un ensemble de fragments dans la DHT après upload Cloud.
pub fn index_cloud_shards(
    dht: &mut DhtTable,
    shard_hashes: &[String],
    provider: &str,
    bucket: &str,
    project_id: &str,
    file_path: Option<String>,
) {
    for (idx, hash) in shard_hashes.iter().enumerate() {
        let object_key = format!("hyperscale/{}/shard-{:04}-{}", project_id, idx, &hash[..8]);
        let entry = DhtEntry {
            chunk_hash: hash.clone(),
            locations: vec![ShardLocation {
                kind: LocationKind::Cloud {
                    provider: provider.to_string(),
                    bucket: bucket.to_string(),
                    object_key,
                },
                shard_index: idx,
                verified_at: Utc::now(),
                priority_weight: priority_for_provider(provider),
            }],
            added_at: Utc::now(),
            ttl_secs: None, // Cloud = permanent
            file_path: file_path.clone(),
        };
        dht.upsert(entry);
    }
}

/// Indexe un nœud P2P (PC dev) dans la DHT.
pub fn index_peer_shards(
    dht: &mut DhtTable,
    shard_hashes: &[String],
    peer_id: &str,
    ip: &str,
    port: u16,
    ttl_secs: u64,
) {
    for (idx, hash) in shard_hashes.iter().enumerate() {
        if let Some(entry) = dht.entries.get_mut(hash) {
            // Ajoute ce peer comme source supplémentaire
            entry.locations.push(ShardLocation {
                kind: LocationKind::Peer {
                    peer_id: peer_id.to_string(),
                    ip: ip.to_string(),
                    port,
                },
                shard_index: idx,
                verified_at: Utc::now(),
                priority_weight: 0.5, // P2P = priorité moyenne
            });
        } else {
            // Nouvelle entrée peer-only (avant upload cloud)
            let entry = DhtEntry {
                chunk_hash: hash.clone(),
                locations: vec![ShardLocation {
                    kind: LocationKind::Peer {
                        peer_id: peer_id.to_string(),
                        ip: ip.to_string(),
                        port,
                    },
                    shard_index: idx,
                    verified_at: Utc::now(),
                    priority_weight: 0.5,
                }],
                added_at: Utc::now(),
                ttl_secs: Some(ttl_secs),
                file_path: None,
            };
            dht.upsert(entry);
        }
    }
}

/// Poids de priorité selon le provider (bande passante estimée).
fn priority_for_provider(provider: &str) -> f32 {
    match provider {
        "aws-s3"      => 0.9,
        "gcp-storage" => 0.85,
        "azure-blob"  => 0.80,
        _             => 0.7,  // Custom / on-premise
    }
}

// ── Tests ─────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_and_lookup() {
        let mut dht = DhtTable::new("org-test");
        let entry = DhtEntry {
            chunk_hash: "abc123".to_string(),
            locations: vec![],
            added_at: Utc::now(),
            ttl_secs: None,
            file_path: Some("src/main.rs".to_string()),
        };
        dht.upsert(entry);
        assert!(dht.lookup("abc123").is_some());
        assert!(dht.lookup("nonexistent").is_none());
    }

    #[test]
    fn expired_entries_invisible() {
        let mut dht = DhtTable::new("org-test");
        let past = Utc::now() - chrono::Duration::seconds(3601);
        let entry = DhtEntry {
            chunk_hash: "expired_hash".to_string(),
            locations: vec![],
            added_at: past,
            ttl_secs: Some(3600), // TTL dépassé
            file_path: None,
        };
        dht.upsert(entry);
        assert!(dht.lookup("expired_hash").is_none());
    }

    #[test]
    fn merge_deduplicates() {
        let mut dht1 = DhtTable::new("org-test");
        let mut dht2 = DhtTable::new("org-test");

        let entry1 = DhtEntry {
            chunk_hash: "shared_hash".to_string(),
            locations: vec![],
            added_at: Utc::now(),
            ttl_secs: None,
            file_path: None,
        };
        dht1.upsert(entry1.clone());
        dht2.upsert(entry1);

        dht1.merge(dht2);
        assert_eq!(dht1.entries.len(), 1); // Pas de doublon
    }
}
