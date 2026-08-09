// ============================================================
//  commands/studio_docs.rs — Contenu éditorial des commandes
//
//  Principe : ce module ne contient AUCUNE logique de commande.
//  Il enrichit le squelette structurel auto-généré par studio.rs
//  (issu de l'introspection clap — noms, arguments, requis/optionnel)
//  avec du contenu rédigé à la main : description longue, exemple
//  concret, prérequis, niveau de risque.
//
//  Le contenu vit dans un fichier JSON séparé (content/commands.json),
//  embarqué dans le binaire à la compilation via include_str! — donc
//  toujours livré avec le binaire, sans dépendance externe à l'exécution,
//  mais modifiable sans toucher à un seul fichier de logique métier.
//
//  Garde-fou anti-dérive : un test (voir tests ci-dessous) échoue si
//  une commande RÉELLE (présente dans le manifeste clap) n'a pas
//  d'entrée correspondante ici. Impossible d'ajouter une commande au
//  CLI sans que la suite de tests exige sa documentation.
// ============================================================

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct ArgDoc {
    /// Description de l'argument, une phrase complète
    pub description: String,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "lowercase")]
pub enum Danger {
    /// Aucun risque — lecture seule ou réversible via `iloc undo`
    Safe,
    /// Modifie un état externe (crée une ressource, appelle une API) —
    /// pas destructeur mais pas trivialement annulable
    Caution,
    /// Supprime ou écrase des données de façon difficilement réversible
    Destructive,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct CommandDoc {
    /// Résumé d'une ligne (peut reprendre/affiner le `about` clap)
    pub summary: String,
    /// Explication complète : ce que fait la commande, pourquoi/quand
    /// s'en servir, effets de bord notables
    pub details: String,
    /// Un exemple d'invocation réel et directement copiable
    pub example: String,
    /// Commandes à avoir lancées avant celle-ci, le cas échéant
    #[serde(default)]
    pub prerequisites: Vec<String>,
    pub danger: Danger,
    /// Descriptions par nom d'argument (clé = `id` clap)
    #[serde(default)]
    pub args: HashMap<String, ArgDoc>,
}

/// Contenu éditorial embarqué dans le binaire à la compilation.
/// Aucune lecture disque à l'exécution — le JSON fait partie du binaire.
const CONTENT_JSON: &str = include_str!("../../content/commands.json");

/// Charge et parse le contenu éditorial. Panique volontairement si le
/// JSON embarqué est malformé : une erreur ici est un bug de build,
/// pas une situation runtime à gérer gracieusement — elle doit être
/// impossible à livrer (attrapée par `cargo test` avant toute release).
pub fn load() -> HashMap<String, CommandDoc> {
    serde_json::from_str(CONTENT_JSON)
        .expect("content/commands.json est malformé — ceci est un bug de build, pas une erreur runtime")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::studio;

    /// LE test anti-dérive : toute commande *feuille* (qui exécute
    /// réellement quelque chose) réellement présente dans le CLI doit
    /// avoir une entrée de documentation correspondante. Ce test
    /// échoue à la première commande ajoutée au CLI et oubliée ici —
    /// impossible de la manquer en CI.
    #[test]
    fn every_leaf_command_is_documented() {
        let manifest = studio::generate();
        let docs = load();

        let missing: Vec<String> = manifest
            .iter()
            .filter(|e| e.is_leaf)
            .map(|e| e.path.join("."))
            .filter(|key| !docs.contains_key(key))
            .collect();

        assert!(
            missing.is_empty(),
            "{} commande(s) sans documentation dans content/commands.json :\n  {}",
            missing.len(),
            missing.join("\n  ")
        );
    }

    /// Vérifie l'inverse : pas d'entrée de documentation qui ne
    /// correspond à aucune commande réelle (contenu orphelin après un
    /// renommage/suppression de commande — signale un JSON à nettoyer).
    #[test]
    fn no_orphan_documentation_entries() {
        let manifest = studio::generate();
        let real_paths: std::collections::HashSet<String> =
            manifest.iter().filter(|e| e.is_leaf).map(|e| e.path.join(".")).collect();

        let docs = load();
        let orphans: Vec<&String> = docs.keys().filter(|k| !real_paths.contains(*k)).collect();

        assert!(
            orphans.is_empty(),
            "{} entrée(s) de documentation orpheline(s) (commande renommée/supprimée) :\n  {:?}",
            orphans.len(),
            orphans
        );
    }

    /// Chaque argument documenté doit exister réellement sur la commande.
    #[test]
    fn documented_args_exist_on_real_commands() {
        let manifest = studio::generate();
        let docs = load();

        for entry in manifest.iter().filter(|e| e.is_leaf) {
            let key = entry.path.join(".");
            if let Some(doc) = docs.get(&key) {
                let real_arg_ids: std::collections::HashSet<&str> =
                    entry.args.iter().map(|a| a.id.as_str()).collect();
                for documented_arg in doc.args.keys() {
                    assert!(
                        real_arg_ids.contains(documented_arg.as_str()),
                        "'{}' documente un argument '{}' qui n'existe pas sur la commande réelle",
                        key, documented_arg
                    );
                }
            }
        }
    }
}
