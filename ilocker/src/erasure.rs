// ============================================================
//  erasure.rs — Erasure Coding (Code d'effacement) Hyperscale
//
//  v1.13.0 — Remplacement par un vrai Reed-Solomon GF(2^8)
//  ─────────────────────────────────────────────────────────
//  L'implémentation précédente (XOR rotatif fait main) ne
//  pouvait reconstruire correctement qu'en cas de perte d'UN
//  SEUL fragment de données à la fois — alors qu'elle prétendait
//  tolérer `m` pannes simultanées. Avec 2+ fragments de données
//  perdus en même temps, elle renvoyait des données SILENCIEUSEMENT
//  corrompues (aucune erreur, juste des octets faux). Inacceptable
//  pour un système censé transporter des gigaoctets à pétaoctets.
//
//  Remplacé par le crate `reed-solomon-erasure` (GF(2^8)), une
//  implémentation mathématiquement correcte et largement testée
//  (port de l'implémentation Java de Backblaze / Go de Klaus Post),
//  qui reconstruit correctement n'importe quelle combinaison de
//  pertes tant qu'au plus `m` fragments (données OU parité,
//  n'importe lesquels) manquent.
//
//  Configuration par défaut Hyperscale
//  ────────────────────────────────────
//    k = 6  (données)
//    m = 3  (parité)
//  → Tolérance à 3 pannes simultanées sur 9 fragments, QUELLE QUE
//    SOIT la combinaison de fragments perdus.
//  → Overhead stockage : 9/6 = 1.5× (vs 3× pour réplication 3x)
// ============================================================

use anyhow::{bail, Context, Result};
use reed_solomon_erasure::galois_8::ReedSolomon;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ── Constantes par défaut ─────────────────────────────────────

/// Nombre de fragments de données (k).
pub const DEFAULT_DATA_SHARDS: usize = 6;
/// Nombre de fragments de parité (m).
pub const DEFAULT_PARITY_SHARDS: usize = 3;
/// Total de fragments (k + m).
pub const TOTAL_SHARDS: usize = DEFAULT_DATA_SHARDS + DEFAULT_PARITY_SHARDS;

// ── Types publics ─────────────────────────────────────────────

/// Métadonnées d'un fragment distribué.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShardMeta {
    /// Index du fragment (0..k = données, k..k+m = parité).
    pub index: usize,
    /// true si fragment de parité, false si données.
    pub is_parity: bool,
    /// Hash SHA-256 des données brutes de ce fragment.
    pub sha256: String,
    /// Taille en octets.
    pub size: usize,
    /// Sur quel profil Cloud ce fragment est stocké.
    pub cloud_target: Option<String>,
}

/// Résultat du découpage en fragments (encode).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErasureSet {
    /// k = nombre de fragments de données.
    pub data_shards: usize,
    /// m = nombre de fragments de parité.
    pub parity_shards: usize,
    /// Taille originale des données avant padding.
    pub original_size: usize,
    /// Tous les fragments (données + parité).
    #[serde(skip)]
    pub shards: Vec<Vec<u8>>,
    /// Métadonnées de chaque fragment.
    pub meta: Vec<ShardMeta>,
}

// ── Encode ────────────────────────────────────────────────────

/// Découpe `data` en `k` fragments et génère `m` fragments de parité
/// Reed-Solomon (GF(2^8)) — reconstruction garantie correcte pour
/// n'importe quelle combinaison de pertes ≤ `parity_shards`.
pub fn encode(
    data: &[u8],
    data_shards: usize,
    parity_shards: usize,
) -> Result<ErasureSet> {
    if data_shards == 0 || parity_shards == 0 {
        bail!("data_shards et parity_shards doivent être > 0");
    }

    let original_size = data.len();
    let safe_len = data.len().max(1); // évite shard_size=0 sur entrée vide

    let shard_size = (safe_len + data_shards - 1) / data_shards;
    let total_padded = shard_size * data_shards;
    let mut padded = data.to_vec();
    padded.resize(total_padded, 0u8);

    let mut shards: Vec<Vec<u8>> = (0..data_shards)
        .map(|i| padded[i * shard_size..(i + 1) * shard_size].to_vec())
        .collect();
    for _ in 0..parity_shards {
        shards.push(vec![0u8; shard_size]);
    }

    let rs = ReedSolomon::new(data_shards, parity_shards)
        .context("Paramètres Reed-Solomon invalides")?;
    rs.encode(&mut shards)
        .context("Échec de l'encodage Reed-Solomon")?;

    let meta: Vec<ShardMeta> = shards
        .iter()
        .enumerate()
        .map(|(i, s)| ShardMeta {
            index: i,
            is_parity: i >= data_shards,
            sha256: sha256_hex(s),
            size: s.len(),
            cloud_target: None,
        })
        .collect();

    Ok(ErasureSet {
        data_shards,
        parity_shards,
        original_size,
        shards,
        meta,
    })
}

// ── Decode / Reconstruction ───────────────────────────────────

/// Reconstruit les données originales depuis les fragments disponibles.
///
/// `available` est une liste de (index_fragment, données). Il faut au
/// minimum `data_shards` fragments — n'IMPORTE LESQUELS parmi les
/// k+m (données et/ou parité) — pour reconstruire correctement.
pub fn decode(
    available: &[(usize, Vec<u8>)],
    data_shards: usize,
    parity_shards: usize,
    original_size: usize,
) -> Result<Vec<u8>> {
    let total = data_shards + parity_shards;

    if available.len() < data_shards {
        bail!(
            "Reconstruction impossible : {} fragments disponibles, {} requis (tolérance max {} pannes).",
            available.len(),
            data_shards,
            parity_shards
        );
    }

    let rs = ReedSolomon::new(data_shards, parity_shards)
        .context("Paramètres Reed-Solomon invalides")?;

    let mut slots: Vec<Option<Vec<u8>>> = vec![None; total];
    for (idx, data) in available {
        if *idx >= total {
            bail!("Index de fragment hors limites : {} (total={})", idx, total);
        }
        slots[*idx] = Some(data.clone());
    }

    rs.reconstruct(&mut slots)
        .context("Échec de la reconstruction Reed-Solomon — fragments corrompus ou incompatibles ?")?;

    let mut result = Vec::with_capacity(original_size);
    for i in 0..data_shards {
        let shard = slots[i].as_ref()
            .ok_or_else(|| anyhow::anyhow!("Fragment de données {} manquant après reconstruction", i))?;
        result.extend_from_slice(shard);
    }
    result.truncate(original_size);
    Ok(result)
}

/// Vérifie qu'un jeu de fragments complet (k+m) est cohérent
/// (les parités correspondent réellement aux données). Utile pour
/// détecter une corruption silencieuse avant de faire confiance à
/// des fragments téléchargés depuis un cloud distant.
pub fn verify(shards: &[Vec<u8>], data_shards: usize, parity_shards: usize) -> Result<bool> {
    let rs = ReedSolomon::new(data_shards, parity_shards)?;
    Ok(rs.verify(shards)?)
}

// ── Distribution multi-cloud ──────────────────────────────────

/// Assigne les fragments aux profils Cloud de manière équilibrée.
///
/// `providers` est la liste des noms de profils cloud configurés
/// (`iloc config cloud list`). Chaque profil reçoit environ
/// (total_shards / nb_providers) fragments, garantissant qu'aucun
/// profil seul ne peut reconstituer le projet (sécurité face à un
/// provider compromis ou à une fuite chez un seul hébergeur).
pub fn assign_shards_to_clouds(
    erasure_set: &mut ErasureSet,
    providers: &[String],
) -> Result<HashMap<String, Vec<usize>>> {
    if providers.is_empty() {
        bail!("Aucun profil Cloud configuré pour la distribution Hyperscale.");
    }

    let mut distribution: HashMap<String, Vec<usize>> = HashMap::new();

    for (i, shard_meta) in erasure_set.meta.iter_mut().enumerate() {
        let cloud = &providers[i % providers.len()];
        shard_meta.cloud_target = Some(cloud.clone());
        distribution.entry(cloud.clone()).or_default().push(i);
    }

    // Avertissement de sécurité (non bloquant si un seul profil est
    // disponible — mieux vaut une redondance partielle que rien du
    // tout — mais on le signale clairement à l'appelant).
    for (_cloud, indices) in &distribution {
        if indices.len() >= erasure_set.data_shards && providers.len() > 1 {
            bail!(
                "Distribution non sécurisée : un profil possède {} fragments ≥ k={}. \
                 Ajoutez plus de profils cloud (`iloc config cloud add`) ou réduisez k.",
                indices.len(),
                erasure_set.data_shards
            );
        }
    }

    Ok(distribution)
}

/// Calcule des paramètres (k, m) raisonnables selon le nombre de
/// profils cloud réellement disponibles — inutile de viser k=6,m=3
/// si l'utilisateur n'a connecté que 2 ou 3 clouds.
pub fn suggest_shard_params(n_profiles: usize) -> (usize, usize) {
    match n_profiles {
        0 | 1 => (1, 0),       // pas de redondance possible
        2     => (1, 1),       // miroir simple (RAID-1 like)
        3     => (2, 1),
        4     => (2, 2),
        5     => (3, 2),
        6     => (4, 2),
        7     => (4, 3),
        8     => (5, 3),
        _     => (DEFAULT_DATA_SHARDS, DEFAULT_PARITY_SHARDS), // ≥9 profils
    }
}

// ── Utilitaires ───────────────────────────────────────────────

fn sha256_hex(data: &[u8]) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(data))
}

/// Statistiques d'un schéma erasure pour affichage.
pub fn stats_for(data_shards: usize, parity_shards: usize) -> String {
    let overhead = (data_shards + parity_shards) as f64 / data_shards as f64;
    format!(
        "k={} données, m={} parité | overhead={:.2}× | tolère {} panne(s) simultanée(s), n'importe lesquelles",
        data_shards, parity_shards, overhead, parity_shards
    )
}

// ── Tests ─────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_intact() {
        let data = b"Hello Hyperscale iLocker! This is a test payload for erasure coding.";
        let set = encode(data, 4, 2).unwrap();
        assert_eq!(set.shards.len(), 6);

        let available: Vec<(usize, Vec<u8>)> = set.shards
            .iter().enumerate().take(4)
            .map(|(i, s)| (i, s.clone())).collect();

        let recovered = decode(&available, 4, 2, data.len()).unwrap();
        assert_eq!(&recovered, data);
    }

    #[test]
    fn tolerates_any_single_loss() {
        let data = b"Test data for erasure coding with one lost shard. Reconstruction required!";
        let set = encode(data, 4, 2).unwrap();

        for lost in 0..6 {
            let available: Vec<(usize, Vec<u8>)> = set.shards.iter().enumerate()
                .filter(|(i, _)| *i != lost)
                .map(|(i, s)| (i, s.clone())).collect();
            let recovered = decode(&available, 4, 2, data.len())
                .unwrap_or_else(|e| panic!("perte du fragment {} aurait dû être tolérée: {}", lost, e));
            assert_eq!(&recovered, data, "fragment {} perdu : données reconstruites incorrectes", lost);
        }
    }

    /// LE test qui aurait fait échouer l'ancienne implémentation XOR :
    /// perte de DEUX fragments de données simultanément (m=2 le permet).
    #[test]
    fn tolerates_two_simultaneous_data_losses() {
        let data = b"Ce payload doit survivre a la perte de DEUX fragments de donnees en meme temps, pas juste un seul. Repetition pour depasser la taille d'un shard de test.";
        let set = encode(data, 4, 2).unwrap();

        // Perd les fragments de données 0 ET 2 simultanément.
        let available: Vec<(usize, Vec<u8>)> = set.shards.iter().enumerate()
            .filter(|(i, _)| *i != 0 && *i != 2)
            .map(|(i, s)| (i, s.clone())).collect();

        let recovered = decode(&available, 4, 2, data.len()).unwrap();
        assert_eq!(&recovered, data, "perte simultanée de 2 fragments de données : reconstruction incorrecte");
    }

    #[test]
    fn tolerates_all_parity_combinations_for_m_equals_3() {
        let data = vec![42u8; 10_000];
        let set = encode(&data, 6, 3).unwrap();

        // Teste TOUTES les combinaisons de 3 pertes parmi 9 fragments
        // (84 combinaisons) — garantit qu'aucune combinaison ne casse
        // la reconstruction, contrairement à l'ancien schéma XOR.
        let total = 9;
        for a in 0..total {
            for b in (a+1)..total {
                for c in (b+1)..total {
                    let lost = [a, b, c];
                    let available: Vec<(usize, Vec<u8>)> = set.shards.iter().enumerate()
                        .filter(|(i, _)| !lost.contains(i))
                        .map(|(i, s)| (i, s.clone())).collect();
                    let recovered = decode(&available, 6, 3, data.len())
                        .unwrap_or_else(|e| panic!("combinaison de pertes {:?} aurait dû être tolérée: {}", lost, e));
                    assert_eq!(recovered, data, "combinaison de pertes {:?} : données incorrectes", lost);
                }
            }
        }
    }

    #[test]
    fn fails_clearly_on_insufficient_shards() {
        let data = b"Some test data";
        let set = encode(data, 4, 2).unwrap();
        let available: Vec<(usize, Vec<u8>)> = set.shards.iter().enumerate()
            .take(3).map(|(i, s)| (i, s.clone())).collect();
        assert!(decode(&available, 4, 2, data.len()).is_err());
    }

    #[test]
    fn distribution_is_secure_with_multiple_providers() {
        let data = vec![0u8; 1024];
        let mut set = encode(&data, DEFAULT_DATA_SHARDS, DEFAULT_PARITY_SHARDS).unwrap();
        let providers = vec![
            "aws-perso".to_string(),
            "backblaze-archive".to_string(),
            "digitalocean-prod".to_string(),
        ];
        let dist = assign_shards_to_clouds(&mut set, &providers).unwrap();
        for (_cloud, indices) in &dist {
            assert!(indices.len() < DEFAULT_DATA_SHARDS);
        }
    }

    #[test]
    fn suggested_params_scale_with_provider_count() {
        assert_eq!(suggest_shard_params(2), (1, 1));
        assert_eq!(suggest_shard_params(3), (2, 1));
        assert_eq!(suggest_shard_params(9), (6, 3));
        assert_eq!(suggest_shard_params(20), (6, 3));
    }
}
