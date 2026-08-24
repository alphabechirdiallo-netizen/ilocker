# Registre communautaire de providers ilocker

Ce dossier est l'index public des providers [ilocker](https://github.com/alphabechirdiallo-netizen/ilocker)
partagés par la communauté. Un provider est un manifeste TOML déclaratif
qui transforme n'importe quelle API HTTP/GraphQL en commandes `iloc`
natives — sans jamais recompiler ilocker.

Il vit comme sous-dossier du dépôt principal (pas dans un dépôt séparé) :
plus simple à maintenir, et cohérent avec le fait qu'ilocker lui-même
est désormais public. Aucun serveur dédié : ce dossier **est** le
registre. `iloc provider search`/`install` lisent simplement
`ilocker-registry/index.json` via `raw.githubusercontent.com`,
exactement comme `iloc update` lit déjà les releases GitHub d'ilocker.

## Utiliser un provider publié

```bash
iloc provider search stripe          # cherche par nom/description/tag
iloc provider install stripe         # télécharge, vérifie le sha256, installe
iloc connect stripe                  # fournit vos identifiants (jamais envoyés ici)
```

L'intégrité de chaque téléchargement est vérifiée : le sha256 du fichier
reçu doit correspondre exactement à celui déclaré dans `index.json`, sinon
l'installation est refusée.

## Publier un provider

1. Écrivez et testez votre manifeste localement :
   ```bash
   iloc provider init mon-provider
   # éditez mon-provider.provider.toml
   iloc provider validate mon-provider.provider.toml
   iloc provider test mon-provider.provider.toml
   ```
2. Préparez la publication :
   ```bash
   iloc provider publish --file mon-provider.provider.toml
   ```
   Cette commande applique les règles strictes de publication (HTTPS pur,
   description et exemple obligatoires sur chaque opération), calcule le
   sha256, et affiche l'entrée exacte à ajouter à `index.json`.
3. Forkez le dépôt `ilocker`, ajoutez :
   - `ilocker-registry/providers/<slug>.toml` — votre manifeste
   - une entrée dans `ilocker-registry/index.json`, avec le sha256 affiché
     à l'étape 2
4. Ouvrez une pull request contre `ilocker`. Le workflow CI
   (`.github/workflows/validate-registry.yml`, à la racine du dépôt)
   compile ilocker depuis les sources de la PR et relance automatiquement
   `iloc provider validate` sur votre manifeste avant toute fusion humaine.

Voir [CONTRIBUTING.md](./CONTRIBUTING.md) pour les critères d'acceptation.

## Sécurité

- Un manifeste est une déclaration TOML pure, jamais du code exécutable.
- Le moteur d'ilocker impose déjà des garde-fous structurels (HTTPS
  obligatoire, endpoints bornés au host déclaré, un seul header
  d'authentification possible, headers additionnels limités à des
  littéraux figés) — voir `ilocker/src/provider_manifest.rs` et
  `ilocker/src/provider_engine.rs` pour le détail exact.
- Vos identifiants (clé API, client_secret, JSON de compte de service…)
  ne transitent jamais par ce registre : ils restent chiffrés localement
  sur votre machine, fournis uniquement à l'API tierce elle-même.
- Toute publication passe par une revue humaine en pull request — ce
  registre n'accepte aucune écriture automatisée non revue.
