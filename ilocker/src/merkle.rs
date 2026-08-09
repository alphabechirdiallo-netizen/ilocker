// ============================================================
//  merkle.rs — Arbre de Merkle pour routage chirurgical Hyperscale
//
//  Chaque nœud de l'arbre possède une signature (hash) unique
//  qui représente le contenu de son sous-arbre. Cela permet :
//    1. L'export chirurgical d'un sous-dossier précis sans
//       exposer le reste du projet.
//    2. La déduplication inter-snapshots au niveau dossier.
//    3. La vérification d'intégrité distribuée : un seul hash
//       de racine suffit pour valider tout le projet.
//
//  Structure d'un nœud
//  ──────────────────────────────────────────────────────────
//  MerkleNode {
//    path:     chemin relatif (ex: "src/services/gmail")
//    hash:     SHA-256(hash_enfant_1 || hash_enfant_2 || ...)
//              pour les feuilles: SHA-256 du contenu du fichier
//    children: Vec<MerkleNode> (vide pour les feuilles)
//    is_leaf:  true si fichier, false si dossier
//    size:     taille totale en octets du sous-arbre
//  }
// ============================================================

use anyhow::Result;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

// ── Types publics ─────────────────────────────────────────────

/// Un nœud dans l'arbre de Merkle du projet.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MerkleNode {
    /// Chemin relatif depuis la racine du projet.
    pub path: String,
    /// Hash SHA-256 de ce nœud (contenu pour feuilles, hash des enfants pour dossiers).
    pub hash: String,
    /// Enfants (vide si feuille).
    pub children: Vec<MerkleNode>,
    /// true = fichier, false = dossier
    pub is_leaf: bool,
    /// Taille totale du sous-arbre en octets.
    pub size_bytes: u64,
    /// Nombre de fichiers dans ce sous-arbre.
    pub file_count: u64,
}

impl MerkleNode {
    /// Crée un nœud feuille (fichier).
    pub fn leaf(path: String, file_hash: String, size_bytes: u64) -> Self {
        MerkleNode {
            path,
            hash: file_hash,
            children: Vec::new(),
            is_leaf: true,
            size_bytes,
            file_count: 1,
        }
    }

    /// Crée un nœud interne (dossier) depuis ses enfants.
    pub fn internal(path: String, mut children: Vec<MerkleNode>) -> Self {
        // Tri déterministe pour que le hash soit reproductible
        children.sort_by(|a, b| a.path.cmp(&b.path));

        // Hash = SHA-256 de la concaténation des hashes des enfants
        let mut hasher = Sha256::new();
        for child in &children {
            hasher.update(child.hash.as_bytes());
            hasher.update(b"|");
        }
        let hash = hex::encode(hasher.finalize());

        let size_bytes: u64 = children.iter().map(|c| c.size_bytes).sum();
        let file_count: u64 = children.iter().map(|c| c.file_count).sum();

        MerkleNode {
            path,
            hash,
            children,
            is_leaf: false,
            size_bytes,
            file_count,
        }
    }

    /// Retourne vrai si ce nœud ou l'un de ses descendants correspond au chemin donné.
    pub fn contains_path(&self, target: &str) -> bool {
        if self.path == target || target.starts_with(&format!("{}/", self.path)) {
            return true;
        }
        self.children.iter().any(|c| c.contains_path(target))
    }

    /// Trouve un nœud par son chemin exact.
    pub fn find(&self, target: &str) -> Option<&MerkleNode> {
        if self.path == target {
            return Some(self);
        }
        for child in &self.children {
            if let Some(found) = child.find(target) {
                return Some(found);
            }
        }
        None
    }

    /// Collecte tous les hashes de chunks (feuilles) sous ce nœud.
    pub fn collect_leaf_hashes(&self) -> Vec<String> {
        if self.is_leaf {
            return vec![self.hash.clone()];
        }
        self.children
            .iter()
            .flat_map(|c| c.collect_leaf_hashes())
            .collect()
    }

    /// Collecte tous les chemins de fichiers sous ce nœud.
    pub fn collect_file_paths(&self) -> Vec<String> {
        if self.is_leaf {
            return vec![self.path.clone()];
        }
        self.children
            .iter()
            .flat_map(|c| c.collect_file_paths())
            .collect()
    }
}

// ── Construction de l'arbre depuis un manifest ────────────────

/// Construit l'arbre de Merkle depuis une liste de (rel_path, file_hash, size_bytes).
pub fn build_from_files(files: &[(String, String, u64)]) -> MerkleNode {
    // Organise les fichiers par hiérarchie de dossiers
    let mut tree: HashMap<String, Vec<(String, String, u64)>> = HashMap::new();

    for (path, hash, size) in files {
        // Détermine le dossier parent
        let parent = match path.rfind('/') {
            Some(idx) => path[..idx].to_string(),
            None => String::new(), // Racine
        };
        tree.entry(parent).or_default().push((path.clone(), hash.clone(), *size));
    }

    build_node("", &tree)
}

fn build_node(
    prefix: &str,
    tree: &HashMap<String, Vec<(String, String, u64)>>,
) -> MerkleNode {
    let mut children: Vec<MerkleNode> = Vec::new();

    // Feuilles directes dans ce dossier
    if let Some(direct_files) = tree.get(prefix) {
        for (path, hash, size) in direct_files {
            children.push(MerkleNode::leaf(path.clone(), hash.clone(), *size));
        }
    }

    // Sous-dossiers : dérivés du PREMIER segment de chemin restant après
    // le préfixe, pour toute clé de `tree` commençant par ce préfixe.
    //
    // Important : un dossier intermédiaire (ex: "src/services") peut ne
    // contenir AUCUN fichier directement — seulement des sous-dossiers
    // ("gmail", "drive"). Il n'existe alors jamais comme clé littérale
    // dans `tree` (qui n'indexe que les dossiers CONTENANT un fichier).
    // On reconstruit donc le chemin de l'enfant directement à partir du
    // premier segment, plutôt que d'exiger une correspondance exacte —
    // sinon tout le sous-arbre "services/*" est silencieusement perdu.
    let sub_prefix = if prefix.is_empty() {
        String::new()
    } else {
        format!("{}/", prefix)
    };

    let mut seen_dirs: std::collections::HashSet<String> = std::collections::HashSet::new();
    for key in tree.keys() {
        if key.is_empty() || key == prefix {
            continue;
        }

        let rest = match key.strip_prefix(sub_prefix.as_str()) {
            Some(r) if !r.is_empty() => r,
            _ => continue,
        };

        let first_segment = match rest.find('/') {
            Some(idx) => &rest[..idx],
            None => rest,
        };

        let child_path = if prefix.is_empty() {
            first_segment.to_string()
        } else {
            format!("{}/{}", prefix, first_segment)
        };

        if seen_dirs.insert(child_path.clone()) {
            children.push(build_node(&child_path, tree));
        }
    }

    MerkleNode::internal(prefix.to_string(), children)
}

// ── Export chirurgical ────────────────────────────────────────

/// Résultat d'un export chirurgical d'un sous-dossier.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SurgicalExport {
    /// Chemin du module exporté (ex: "src/services/gmail")
    pub module_path: String,
    /// Hash racine du sous-arbre exporté (preuve d'intégrité)
    pub root_hash: String,
    /// Liste des chemins de fichiers inclus
    pub file_paths: Vec<String>,
    /// Taille totale en octets
    pub total_size_bytes: u64,
    /// Nombre de fichiers
    pub file_count: u64,
}

/// Extrait les métadonnées d'un sous-module pour export chirurgical.
pub fn surgical_export(root: &MerkleNode, module_path: &str) -> Result<SurgicalExport> {
    let node = root.find(module_path).ok_or_else(|| {
        anyhow::anyhow!(
            "Module '{}' non trouvé dans l'arbre de Merkle du projet.",
            module_path
        )
    })?;

    Ok(SurgicalExport {
        module_path: module_path.to_string(),
        root_hash: node.hash.clone(),
        file_paths: node.collect_file_paths(),
        total_size_bytes: node.size_bytes,
        file_count: node.file_count,
    })
}

// ── Tests ─────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_files() -> Vec<(String, String, u64)> {
        vec![
            ("src/main.rs".to_string(), "aaaa".to_string(), 1000),
            ("src/lib.rs".to_string(), "bbbb".to_string(), 500),
            ("src/services/gmail/auth.rs".to_string(), "cccc".to_string(), 2000),
            ("src/services/gmail/smtp.rs".to_string(), "dddd".to_string(), 3000),
            ("src/services/drive/api.rs".to_string(), "eeee".to_string(), 1500),
            ("README.md".to_string(), "ffff".to_string(), 200),
        ]
    }

    #[test]
    fn build_and_find() {
        let files = sample_files();
        let root = build_from_files(&files);
        assert!(!root.hash.is_empty());

        // Le hash doit être déterministe
        let root2 = build_from_files(&files);
        assert_eq!(root.hash, root2.hash);
    }

    #[test]
    fn surgical_export_works() {
        let files = sample_files();
        let root = build_from_files(&files);
        let export = surgical_export(&root, "src/services/gmail").unwrap();
        assert_eq!(export.file_count, 2);
        assert!(export.file_paths.contains(&"src/services/gmail/auth.rs".to_string()));
    }
}
