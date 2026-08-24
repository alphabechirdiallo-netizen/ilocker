# ilocker — Architecture Standalone v1.9.0

## Vision

**Un seul binaire. Zéro serveur. Distribution universelle.**

`iloc` est un binaire Rust statique qui s'installe dans le PATH et fonctionne
globalement. Il se distribue par n'importe quel moyen — USB, Xender,
Bluetooth, email — et se met à jour automatiquement via GitHub Releases.

Au-delà des snapshots locaux et du cloud BYOC, `iloc` sait aussi **déployer**
directement vers GitHub, Vercel et Supabase (`iloc deploy`), en scannant le
projet courant pour détecter ce qui doit être créé/configuré.

---

## Distribution offline — Flux complet

```
┌─────────────────────────────────────────────────────────────────────┐
│                   Développeur A (publie)                             │
│                                                                      │
│  git tag v1.9.0 && git push --tags                                  │
│       ↓                                                              │
│  GitHub Actions compile 5 binaires statiques                        │
│  → iloc-linux-x86_64                                                │
│  → iloc-linux-aarch64                                               │
│  → iloc-macos-x86_64                                                │
│  → iloc-macos-aarch64                                               │
│  → iloc-windows-x86_64.exe                                          │
│  → + SHA256SUMS + install.sh + install.ps1                          │
│  → publié sur GitHub Releases (dépôt privé — accès via token)       │
└───────────────────────────────┬─────────────────────────────────────┘
                                │
              ┌─────────────────┼──────────────────────┐
              │                 │                      │
              ▼                 ▼                      ▼
         Via internet       Via Xender            Via USB/BT
    curl .../install.sh  iloc-linux-x86_64      iloc-macos-aarch64
              │                 │                      │
              └─────────────────┴──────────────────────┘
                                │
                    ./iloc-linux-x86_64 selfinstall
                    (ou install.sh --local ./iloc-*)
                                │
                                ▼
                    /usr/local/bin/iloc  ✓
                    (commande globale)
```

---

## Installation

Dépôt : **github.com/alphabechirdiallo-netizen/ilocker** (privé).
Un dépôt privé signifie que les téléchargements directs (`curl` nu) sur les
assets de release nécessitent un token GitHub — voir la note ci-dessous.

### Linux / macOS — Online (repo public, ou avec token pour repo privé)

```bash
# Repo public :
curl -fsSL https://github.com/alphabechirdiallo-netizen/ilocker/releases/latest/download/install.sh | sh

# Repo privé — export du token avant l'appel, ou --token en argument :
export GITHUB_TOKEN="ghp_xxx"
curl -fsSL -H "Authorization: token $GITHUB_TOKEN" \
  https://raw.githubusercontent.com/alphabechirdiallo-netizen/ilocker/main/ilocker-deploy/install.sh | sh
```

`install.sh` détecte automatiquement `$GITHUB_TOKEN` (ou `--token <tok>`) et
bascule sur l'API GitHub authentifiée si le téléchargement direct échoue —
c'est le cas normal sur un dépôt privé.

### Linux / macOS — Offline (Xender, USB, Bluetooth, email)

```bash
chmod +x ./iloc-linux-x86_64
./iloc-linux-x86_64 selfinstall
```

### Windows — Online (PowerShell)

```powershell
# Repo public :
irm https://github.com/alphabechirdiallo-netizen/ilocker/releases/latest/download/install.ps1 | iex

# Repo privé :
$env:GITHUB_TOKEN = "ghp_xxx"
irm https://github.com/alphabechirdiallo-netizen/ilocker/releases/latest/download/install.ps1 | iex
```

### Windows — Offline

```powershell
.\iloc-windows-x86_64.exe selfinstall
```

### Vérification d'intégrité SHA-256

```bash
# Linux / macOS
curl -fsSL .../releases/latest/download/SHA256SUMS -o SHA256SUMS
sha256sum -c SHA256SUMS --ignore-missing

# Windows (PowerShell)
(Get-FileHash iloc-windows-x86_64.exe -Algorithm SHA256).Hash
```

`SHA256SUMS` couvre désormais aussi `install.sh` et `install.ps1` eux-mêmes,
pas seulement les 5 binaires.

---

## Mise à jour automatique

```bash
iloc update          # vérifie + télécharge + swap atomique
iloc update --check  # vérifie seulement, n'installe pas
```

Source : GitHub Releases uniquement (`api.github.com/repos/alphabechirdiallo-netizen/ilocker`)
— aucun serveur ilocker requis pour cette fonctionnalité.

---

## Centre de commandes — extension VS Code « ilocker Studio »

Pour ne jamais avoir à mémoriser la syntaxe des 211 commandes, une extension
compagnon s'installe dans VS Code (et les forks compatibles : Cursor,
Windsurf).

```bash
code --install-extension ilocker-studio-0.1.0.vsix
```

Ou depuis VS Code : palette de commandes → *Extensions: Install from VSIX…*

Deux vues, une seule source de vérité :

- **Centre de commandes** (`iloc studio open`, ou icône ilocker dans la barre
  d'activité → *Ouvrir le centre de commandes*) — plein écran, 4 onglets :
  - *Commandes* : les 211 commandes catégorisées, recherche instantanée,
    clic pour lancer directement (ou formulaire généré si des arguments
    sont nécessaires — jamais de commande incomplète envoyée au terminal)
  - *Snapshots* : historique visuel de `iloc log`
  - *Déploiement* : état des liaisons GitHub/Vercel/Supabase et dernier
    déploiement connu
  - *Activité* : les commandes réellement lancées depuis l'extension
- **Assistant docké** (icône ilocker dans la barre latérale) — rétractable
  nativement par VS Code, affiche l'état du projet courant et suggère la
  prochaine action logique, sans jamais dupliquer les 211 commandes dans
  un espace trop petit pour les afficher utilement.

Toute donnée provient de `iloc studio manifest/snapshots/deploy-status/
project-status` (JSON, introspection directe du CLI) — l'extension ne
maintient aucune copie de la liste des commandes : structurellement
impossible de diverger du binaire réellement installé.

---

## Providers de déploiement — GitHub, Vercel, Supabase

`iloc` peut créer et configurer directement l'infrastructure d'un projet, en
plus de simplement le sauvegarder. Chaque provider a ses propres commandes,
et un orchestrateur (`iloc deploy`) les enchaîne automatiquement.

### Connexion aux comptes

```bash
iloc connect github      # device flow OAuth GitHub
iloc connect vercel       # token API Vercel
iloc connect supabase     # token API Supabase

iloc github status        # compte(s) connecté(s), profil actif
iloc vercel status
iloc supabase status
```

### Commandes par provider

```bash
iloc github <sous-commande>     # ex : création de repo, gestion de secrets Actions
iloc vercel <sous-commande>     # ex : lien de projet, variables d'environnement
iloc supabase <sous-commande>   # ex : création de projet, migrations
```

### Orchestrateur `iloc deploy`

```bash
iloc deploy              # scanne le projet, propose un plan, demande confirmation
iloc deploy --yes        # exécute sans confirmation interactive
iloc deploy --dry-run    # affiche le plan sans rien exécuter (lecture seule)
```

`iloc deploy` fonctionne en deux phases : un `scanner.rs` détecte le
framework (Next.js, React, etc.), le remote Git existant, et la présence de
migrations Supabase — puis un plan est construit (`build_plan`, lecture
seule) avant toute exécution (`execute_plan`). Le `--dry-run` s'arrête après
la première phase.

### Stockage des credentials

Les tokens GitHub/Vercel/Supabase sont d'abord tentés dans le trousseau du
système d'exploitation. Sur Linux, en l'absence d'un trousseau système actif
(serveurs headless, conteneurs — le cas le plus courant en production),
`iloc` bascule automatiquement sur un fichier local **chiffré** :

- Primitive : ChaCha20-Poly1305 (même mécanisme que le chiffrement cloud)
- Clé aléatoire dédiée, générée une fois, stockée séparément (`~/.config/ilocker/.vault-key`, permissions 0600)
- Fichiers chiffrés : `~/.config/ilocker/credentials/<compte>.<provider>.vault`
- Un avertissement explicite s'affiche à chaque bascule sur ce mécanisme —
  jamais de perte silencieuse

**Migration automatique :** les installations mises à jour depuis une
version antérieure au chiffrement du fallback (fichiers `.json` en clair)
sont migrées de façon transparente à la première lecture — aucune commande
`iloc connect` à refaire après une mise à jour. Le fichier en clair est
supprimé dès que la migration réussit.

---

## Providers déclaratifs — créez vos propres intégrations

À distinguer des trois providers ci-dessus, qui sont **natifs** (code Rust
dédié, compilé dans le binaire). Un provider **déclaratif** couvre n'importe
quel service tiers avec une API REST (Linear, Stripe, GitLab, un outil
interne d'entreprise…) sans recompiler ilocker : il se définit entièrement
par un manifeste TOML — jamais de code — installé localement.

```bash
iloc provider init linear                    # scaffold commenté
iloc provider validate linear.provider.toml  # schéma + garde-fous sécurité
iloc provider test linear.provider.toml      # vrais appels API, rien stocké
iloc provider install --file linear.provider.toml

iloc connect linear                          # identifiants de l'utilisateur, locaux
iloc linear issue list                       # devient une commande de premier niveau
```

**Sécurité — le manifeste ne peut décrire que des données, jamais du code :**
un seul header d'authentification possible (celui déclaré dans `[auth]`),
chaque endpoint doit rester sous le host de `api.base_url` (aucune
redirection vers un domaine tiers), HTTPS obligatoire sauf `127.0.0.1`/
`localhost` pour le développement local. Les identifiants restent partagés
par le même mécanisme trousseau OS + repli chiffré que GitHub/Vercel/
Supabase (voir *Sécurité des credentials* plus bas) — ni l'auteur du
manifeste ni ilocker ne les voient jamais.

La publication vers un registre public partagé n'est pas encore disponible
— seule l'installation depuis un fichier local (`--file`) l'est aujourd'hui.

---

## Toutes les commandes disponibles (213)

Générées directement depuis la structure réelle du CLI (`iloc studio manifest`) et son contenu éditorial associé — cette liste ne peut pas diverger du binaire : un test automatique (`cargo test`) échoue si une commande existe sans description ici, ou si une description mentionne un argument qui n'existe pas réellement.

Légende : ⚠️ modifie un état externe (pas trivialement annulable) · 🔴 destructif ou difficilement réversible.

### Noyau local (snapshots, historique, restauration)

- `iloc init [--vault-mode <val>] [--vault-dir <val>] [--mirror <val>] [--cloud-backup] [--hyperscale-backup] [--no-gitignore-patch] [--no-sentinel]`
  Initialise ilocker dans le dossier courant : crée le coffre-fort et la base de suivi des snapshots.
- `iloc save <MESSAGE>`
  Crée un snapshot de l'état actuel du projet — l'équivalent d'une sauvegarde instantanée et complète.
- `iloc undo [ID] [--file <val>]` ⚠️
  Restaure le projet (ou des fichiers précis) depuis un snapshot antérieur.
- `iloc log [--ids]`
  Affiche l'historique des snapshots, du plus récent au plus ancien.
- `iloc status [--health]`
  Compare l'état actuel du projet au dernier snapshot : quels fichiers ont changé.
- `iloc dashboard`
  Ouvre un tableau de bord interactif dans le terminal — navigation au clavier entre projets et snapshots.

### Vault & sauvegarde 3-2-1

- `iloc vault backup disable-cloud`
  Désactive l'envoi automatique vers le cloud personnel après chaque snapshot.
- `iloc vault backup disable-hyperscale`
  Désactive la sauvegarde Hyperscale automatique après chaque snapshot.
- `iloc vault backup enable-cloud [--profile <val>]`
  Active l'envoi automatique de chaque nouveau snapshot vers votre cloud personnel (Tier 3a).
- `iloc vault backup enable-hyperscale`
  Active la sauvegarde Hyperscale automatique après chaque snapshot.
- `iloc vault migrate [--mode <val>] [--dir <val>]` ⚠️
  Déplace le coffre-fort vers un autre emplacement ou mode de stockage.
- `iloc vault mirror add <PATH>`
  Ajoute un miroir local (Tier 2 de la stratégie 3-2-1) : une copie synchronisée sur un autre disque ou NAS.
- `iloc vault mirror remove <PATH>`
  Retire un miroir local précédemment configuré.
- `iloc vault mirror sync`
  Force une synchronisation immédiate de tous les miroirs pour le dernier snapshot.
- `iloc vault status`
  Affiche l'état complet du coffre-fort : emplacement, santé, taille, et niveaux de sauvegarde actifs.
- `iloc vault verify`
  Vérifie l'intégrité de tous les snapshots en recalculant leur empreinte SHA-256.

### Cloud BYOC (votre propre cloud)

- `iloc cloud doctor [--profile <val>]`
  Teste la connectivité réelle avec votre cloud personnel : écriture, lecture, puis suppression d'un fichier de test.
- `iloc cloud gc [--profile <val>] [--yes]` 🔴
  Nettoie les chunks orphelins et les liens de partage expirés sur votre cloud personnel.
- `iloc cloud usage [--profile <val>]`
  Affiche l'espace utilisé sur votre cloud personnel (chunks + manifests).
- `iloc cloud verify [--profile <val>]`
  Vérifie l'intégrité de tous les snapshots présents sur votre cloud personnel (recalcul SHA-256).
- `iloc config cloud add [--name <val>] [--activate]`
  Configure un nouveau profil cloud personnel (assistant interactif).
- `iloc config cloud list`
  Liste vos profils cloud personnels configurés.
- `iloc config cloud remove <NAME>` ⚠️
  Supprime un profil cloud personnel configuré.
- `iloc config cloud use <NAME>`
  Change le profil cloud personnel actif par défaut.
- `iloc push [--file <val>] [--profile <val>]`
  Sauvegarde le dernier snapshot vers votre cloud personnel (Cloud BYOC), chiffré et dédupliqué.
- `iloc pull [--id <val>] [--dest <val>] [--file <val>] [--profile <val>]`
  Restaure des fichiers depuis votre cloud personnel.

### Partage P2P

- `iloc share [--port <val>] [--relay <val>] [--cloud] [--ttl <val>] [--file <val>] [--profile <val>]`
  Partage le dernier snapshot directement avec un pair, en P2P ou via un lien cloud chiffré.
- `iloc clone <KEY> [--key-secret <val>] [--host <val>] [--relay <val>] [--port <val>] [--dest <val>]`
  Clone un projet ilocker partagé — détecte automatiquement s'il s'agit d'un lien cloud ou d'une clé P2P classique.
- `iloc transfer-status`
  Affiche l'état d'un transfert P2P (share/clone) en cours ou interrompu.

### Hyperscale (multi-cloud, Erasure Coding)

- `iloc hyperscale clone <URL> [--dest <val>]`
  Reconstitue un snapshot distribué via Hyperscale, en récupérant les shards depuis les clouds configurés.
- `iloc hyperscale config init [--org <val>]`
  Initialise la configuration Hyperscale pour le projet courant.
- `iloc hyperscale config show`
  Affiche la configuration Hyperscale actuelle et suggère des paramètres k/m adaptés au nombre de clouds connectés.
- `iloc hyperscale config validate`
  Vérifie que la configuration Hyperscale actuelle est cohérente et exploitable.
- `iloc hyperscale export <PATH> [--target <val>]`
  Exporte un sous-module précis du projet vers Hyperscale, avec une clé de ciblage optionnelle.
- `iloc hyperscale node start`
  Démarre ce nœud comme contributeur du réseau de stockage Hyperscale (partage de l'espace disque alloué).
- `iloc hyperscale node status`
  Affiche si un nœud de stockage Hyperscale est réellement actif, y compris depuis un autre terminal que celui qui l'a démarré.
- `iloc hyperscale node stop`
  Arrête le nœud de stockage local, y compris depuis un autre terminal que celui qui l'a démarré.
- `iloc hyperscale push [--path <val>] [--file <val>]`
  Distribue le dernier snapshot sur plusieurs clouds simultanément, avec tolérance de panne (Reed-Solomon).
- `iloc hyperscale status`
  Affiche l'état complet de la configuration Hyperscale du projet : organisation, schéma erasure coding, clouds connectés, nœud local.

### GitHub

- `iloc connect <SERVICE> [--name <val>] [--token <val>] [--api-url <val>]`
  Connecte un compte GitHub à ilocker via un Personal Access Token.
- `iloc github actions cancel <RUN_ID> [OWNER_REPO] [--profile <val>] [--yes]` ⚠️
  Annule un run de workflow en cours d'exécution.
- `iloc github actions list [OWNER_REPO] [--profile <val>]`
  Liste les workflows GitHub Actions configurés sur un repository.
- `iloc github actions rerun <RUN_ID> [OWNER_REPO] [--profile <val>]`
  Relance un run de workflow terminé (par exemple après un échec).
- `iloc github actions run <WORKFLOW> [OWNER_REPO] [--branch <val>] [--input <val>] [--profile <val>]` ⚠️
  Déclenche manuellement l'exécution d'un workflow (workflow_dispatch).
- `iloc github actions status [OWNER_REPO] [--workflow <val>] [--branch <val>] [--limit <val>] [--profile <val>]`
  Affiche les exécutions récentes des workflows (runs), avec leur état.
- `iloc github branch create <NAME> [--from <val>] [OWNER_REPO] [--profile <val>]`
  Crée une nouvelle branche à partir d'une branche existante.
- `iloc github branch default <NAME> [OWNER_REPO] [--profile <val>]` ⚠️
  Change la branche par défaut du repository (celle affichée en premier, ciblée par les PR sans base explicite).
- `iloc github branch delete <NAME> [OWNER_REPO] [--profile <val>] [--yes]` 🔴
  Supprime une branche distante sur GitHub.
- `iloc github branch list [OWNER_REPO] [--profile <val>]`
  Liste les branches d'un repository, en signalant la branche par défaut et celles protégées.
- `iloc github branch protect <NAME> [OWNER_REPO] [--check <val>] [--require-pr] [--min-reviews <val>] [--enforce-admins] [--linear] [--allow-force-pushes] [--allow-deletions] [--profile <val>]` ⚠️
  Active des règles de protection sur une branche : reviews obligatoires, status checks requis, etc.
- `iloc github branch unprotect <NAME> [OWNER_REPO] [--profile <val>] [--yes]` ⚠️
  Retire toutes les règles de protection d'une branche.
- `iloc github collab add <USERNAME> [OWNER_REPO] [--permission <val>] [--profile <val>] [--yes]` ⚠️
  Invite un utilisateur comme collaborateur sur un repository.
- `iloc github collab list [OWNER_REPO] [--profile <val>]`
  Liste les collaborateurs d'un repository et leur niveau de permission.
- `iloc github collab remove <USERNAME> [OWNER_REPO] [--profile <val>] [--yes]` 🔴
  Retire un collaborateur d'un repository.
- `iloc github issue assign <NUMBER> [USERS] [--repo <val>] [--unassign] [--profile <val>]`
  Assigne ou retire des utilisateurs d'une issue.
- `iloc github issue close <NUMBER> [OWNER_REPO] [--reason <val>] [--profile <val>]`
  Ferme une issue.
- `iloc github issue comment <NUMBER> [OWNER_REPO] [--body <val>] [--profile <val>]`
  Ajoute un commentaire à une issue.
- `iloc github issue create [OWNER_REPO] [--title <val>] [--body <val>] [--label <val>] [--assignee <val>] [--milestone <val>] [--profile <val>]`
  Crée une nouvelle issue sur un repository.
- `iloc github issue label <NUMBER> [OWNER_REPO] [--add <val>] [--remove <val>] [--profile <val>]`
  Ajoute ou retire des labels d'une issue.
- `iloc github issue list [OWNER_REPO] [--state <val>] [--label <val>] [--assignee <val>] [--limit <val>] [--profile <val>]`
  Liste les issues d'un repository, filtrables par état, labels et assigné.
- `iloc github issue reopen <NUMBER> [OWNER_REPO] [--profile <val>]`
  Rouvre une issue précédemment fermée.
- `iloc github issue view <NUMBER> [OWNER_REPO] [--profile <val>]`
  Affiche le détail complet d'une issue : titre, état, labels, assignés, description.
- `iloc github list`
  Liste les comptes GitHub connectés à ilocker.
- `iloc github pr checkout <NUMBER> [OWNER_REPO] [--profile <val>]`
  Checkout localement la branche source d'une PR pour la tester ou la modifier.
- `iloc github pr close <NUMBER> [OWNER_REPO] [--profile <val>] [--yes]` ⚠️
  Ferme une PR sans la merger.
- `iloc github pr create [OWNER_REPO] [--title <val>] [--body <val>] [--head <val>] [--base <val>] [--draft] [--profile <val>]`
  Crée une Pull Request.
- `iloc github pr list [OWNER_REPO] [--state <val>] [--base <val>] [--limit <val>] [--profile <val>]`
  Liste les Pull Requests d'un repository, filtrables par état et branche cible.
- `iloc github pr merge <NUMBER> [OWNER_REPO] [--method <val>] [--title <val>] [--message <val>] [--profile <val>] [--yes]` ⚠️
  Merge une Pull Request.
- `iloc github pr ready <NUMBER> [OWNER_REPO] [--draft] [--profile <val>]`
  Marque une PR brouillon (draft) comme prête pour review, ou l'inverse.
- `iloc github pr review <NUMBER> [OWNER_REPO] [--reviewer <val>] [--team <val>] [--profile <val>]`
  Demande une review sur une PR, à des utilisateurs et/ou des équipes.
- `iloc github pr update-branch <NUMBER> [OWNER_REPO] [--profile <val>]`
  Met à jour la branche d'une PR avec les derniers changements de sa branche cible.
- `iloc github pr view <NUMBER> [OWNER_REPO] [--profile <val>]`
  Affiche le détail complet d'une PR : état (open/draft/merged/closed), branches, mergeabilité, description.
- `iloc github release create [OWNER_REPO] [--tag <val>] [--name <val>] [--body <val>] [--draft] [--prerelease] [--target <val>] [--generate-notes] [--profile <val>] [--yes]`
  Crée une release à partir d'un tag Git.
- `iloc github release delete <TAG> [OWNER_REPO] [--profile <val>] [--yes]` 🔴
  Supprime une release.
- `iloc github release list [OWNER_REPO] [--limit <val>] [--profile <val>]`
  Liste les releases d'un repository.
- `iloc github release upload <TAG> <FILE> [OWNER_REPO] [--name <val>] [--content-type <val>] [--profile <val>]`
  Attache un fichier (binaire, archive...) à une release existante.
- `iloc github remove <NAME> [--yes]` ⚠️
  Déconnecte un compte GitHub d'ilocker et supprime son token du trousseau.
- `iloc github repo archive [OWNER_REPO] [--unarchive] [--profile <val>] [--yes]` ⚠️
  Archive (lecture seule) ou désarchive un repository.
- `iloc github repo create [NAME] [--description <val>] [--private] [--public] [--org <val>] [--auto-init] [--topic <val>] [--license <val>] [--gitignore <val>] [--profile <val>] [--yes]`
  Crée un nouveau repository GitHub, sur votre compte ou une organisation.
- `iloc github repo delete [OWNER_REPO] [--profile <val>] [--yes]` 🔴
  Supprime définitivement un repository GitHub — action irréversible.
- `iloc github repo fork <OWNER_REPO> [--org <val>] [--profile <val>]`
  Fork un repository dans votre compte ou une organisation.
- `iloc github repo list [--org <val>] [--private] [--public] [--fork] [--limit <val>] [--profile <val>]`
  Liste vos repositories GitHub, avec filtres de visibilité et d'organisation.
- `iloc github repo rename <NEW_NAME> [OWNER_REPO] [--profile <val>] [--yes]` ⚠️
  Renomme un repository GitHub.
- `iloc github repo topics [OWNER_REPO] [--add <val>] [--remove <val>] [--set <val>] [--profile <val>]`
  Ajoute, retire, ou remplace entièrement les topics (mots-clés) d'un repository.
- `iloc github repo transfer <NEW_OWNER> [OWNER_REPO] [--profile <val>] [--yes]` 🔴
  Transfère la propriété d'un repository vers un autre compte ou une organisation.
- `iloc github repo view [OWNER_REPO] [--profile <val>]`
  Affiche les détails complets d'un repository : stars, forks, issues ouvertes, langage, topics, URLs de clone.
- `iloc github repo visibility [OWNER_REPO] [--private] [--public] [--profile <val>] [--yes]` 🔴
  Change la visibilité d'un repository entre privé et public.
- `iloc github search repos <QUERY> [--limit <val>] [--profile <val>]`
  Recherche des repositories sur GitHub par mots-clés, avec la syntaxe de recherche GitHub complète.
- `iloc github secret delete <NAME> [OWNER_REPO] [--profile <val>] [--yes]` 🔴
  Supprime un secret GitHub Actions.
- `iloc github secret list [OWNER_REPO] [--profile <val>]`
  Liste les noms des secrets GitHub Actions configurés sur un repository.
- `iloc github secret set <NAME> [OWNER_REPO] [--value <val>] [--profile <val>]` ⚠️
  Crée ou met à jour un secret GitHub Actions.
- `iloc github status [--profile <val>]`
  Affiche le compte GitHub actuellement connecté et vérifie que le token est toujours valide.
- `iloc github use <NAME>`
  Change le compte GitHub actif parmi ceux déjà connectés.
- `iloc github webhook create <URL> [OWNER_REPO] [--event <val>] [--content-type <val>] [--secret <val>] [--inactive] [--profile <val>]` ⚠️
  Crée un webhook qui notifiera une URL externe lors d'événements du repository.
- `iloc github webhook delete <HOOK_ID> [OWNER_REPO] [--profile <val>] [--yes]` 🔴
  Supprime un webhook.
- `iloc github webhook list [OWNER_REPO] [--profile <val>]`
  Liste les webhooks configurés sur un repository, avec leur URL et statut (actif/inactif).
- `iloc github webhook ping <HOOK_ID> [OWNER_REPO] [--profile <val>]`
  Envoie un événement de test (ping) à un webhook pour vérifier qu'il fonctionne.

### Vercel

- `iloc vercel alias assign <DEPLOYMENT_ID> <ALIAS> [--redirect <val>] [--profile <val>]`
  Assigne un alias (URL personnalisée) à un déploiement précis.
- `iloc vercel alias delete <ALIAS> [--profile <val>] [--yes]` 🔴
  Supprime un alias.
- `iloc vercel alias list [--project <val>] [--limit <val>] [--profile <val>]`
  Liste les alias (URLs personnalisées pointant vers des déploiements).
- `iloc vercel check create <DEPLOYMENT_ID> <NAME> [--detached] [--blocking] [--profile <val>]`
  Crée un nouveau check sur un déploiement.
- `iloc vercel check list <DEPLOYMENT_ID> [--profile <val>]`
  Liste les checks (contrôles CI/qualité) d'un déploiement.
- `iloc vercel check update <DEPLOYMENT_ID> <CHECK_ID> --status <val> [--conclusion <val>] [--profile <val>]`
  Met à jour le statut ou la conclusion d'un check existant.
- `iloc vercel deploy [--prod] [--force] [--wait] [--project <val>] [--branch <val>] [--sha <val>] [--timeout <val>] [--profile <val>] [--yes]` ⚠️
  Déclenche un déploiement Vercel du projet lié.
- `iloc vercel deployment cancel <ID> [--profile <val>] [--yes]` ⚠️
  Annule un déploiement en cours de build.
- `iloc vercel deployment delete <ID> [--profile <val>] [--yes]` 🔴
  Supprime définitivement un déploiement.
- `iloc vercel deployment files <ID> [--profile <val>]`
  Affiche l'arborescence des fichiers déployés dans un déploiement.
- `iloc vercel deployment list [--project <val>] [--target <val>] [--state <val>] [--limit <val>] [--profile <val>]`
  Liste les déploiements, du projet lié ou d'un projet précisé.
- `iloc vercel deployment logs <ID> [--profile <val>]`
  Affiche les logs de build et d'exécution d'un déploiement.
- `iloc vercel deployment promote <ID> [--project <val>] [--profile <val>] [--yes]` ⚠️
  Promeut un déploiement preview au rang de déploiement de production.
- `iloc vercel deployment redeploy <ID> [--target <val>] [--profile <val>] [--yes]` ⚠️
  Relance un nouveau déploiement à partir d'un déploiement existant (même code source).
- `iloc vercel deployment view <ID> [--profile <val>]`
  Affiche le détail complet d'un déploiement précis.
- `iloc vercel domain add <DOMAIN> [--project <val>] [--git-branch <val>] [--redirect <val>] [--profile <val>]`
  Ajoute un domaine à votre compte, ou le lie à un projet précis.
- `iloc vercel domain check <DOMAIN> [--profile <val>]`
  Vérifie si un nom de domaine est disponible à l'achat via Vercel.
- `iloc vercel domain dns add <DOMAIN> <NAME> --type <val> <VALUE> [--ttl <val>] [--priority <val>] [--profile <val>]` ⚠️
  Ajoute un enregistrement DNS à un domaine géré par Vercel.
- `iloc vercel domain dns list <DOMAIN> [--profile <val>]`
  Liste les enregistrements DNS d'un domaine géré par Vercel.
- `iloc vercel domain dns remove <DOMAIN> <RECORD_ID> [--profile <val>] [--yes]` 🔴
  Supprime un enregistrement DNS.
- `iloc vercel domain inspect <DOMAIN> [--project <val>] [--profile <val>]`
  Vérifie la configuration DNS d'un domaine et affiche les éventuelles erreurs.
- `iloc vercel domain list [--project <val>] [--limit <val>] [--profile <val>]`
  Liste vos domaines Vercel, ou ceux liés à un projet précis.
- `iloc vercel domain remove <DOMAIN> [--project <val>] [--profile <val>] [--yes]` 🔴
  Retire un domaine du compte ou d'un projet.
- `iloc vercel edge create <SLUG> [--profile <val>]`
  Crée un nouvel Edge Config.
- `iloc vercel edge delete <ID> [--profile <val>] [--yes]` 🔴
  Supprime un Edge Config entier.
- `iloc vercel edge items <ID> [--profile <val>]`
  Liste les clés/valeurs stockées dans un Edge Config.
- `iloc vercel edge list [--profile <val>]`
  Liste les Edge Config (stockage clé-valeur à très faible latence, lu depuis les fonctions Edge).
- `iloc vercel edge update <ID> [--item <val>] [--profile <val>]` ⚠️
  Ajoute, modifie, ou supprime des clés dans un Edge Config.
- `iloc vercel env add <KEY> [VALUE] [--target <val>] [--env-type <val>] [--git-branch <val>] [--project <val>] [--profile <val>]` ⚠️
  Ajoute ou met à jour une variable d'environnement.
- `iloc vercel env list [--project <val>] [--profile <val>]`
  Liste les variables d'environnement d'un projet.
- `iloc vercel env pull [OUTPUT] [--target <val>] [--project <val>] [--profile <val>] [--yes]` ⚠️
  Télécharge les variables d'environnement d'un projet dans un fichier .env local.
- `iloc vercel env push [INPUT] [--target <val>] [--project <val>] [--profile <val>] [--yes]` ⚠️
  Envoie les variables d'un fichier .env local vers un projet Vercel.
- `iloc vercel env remove <KEY> [--target <val>] [--project <val>] [--profile <val>] [--yes]` 🔴
  Supprime une variable d'environnement.
- `iloc vercel inspect [--profile <val>]`
  Affiche le dernier déploiement prêt (READY) du projet lié dans le dossier courant.
- `iloc vercel list`
  Liste les comptes Vercel connectés.
- `iloc vercel project create [NAME] [--framework <val>] [--root <val>] [--build <val>] [--output <val>] [--install <val>] [--git-repo <val>] [--git-branch <val>] [--link] [--profile <val>] [--yes]`
  Crée un nouveau projet Vercel.
- `iloc vercel project delete [PROJECT] [--profile <val>] [--yes]` 🔴
  Supprime définitivement un projet Vercel.
- `iloc vercel project link [PROJECT] [--profile <val>]`
  Lie le dossier courant à un projet Vercel existant (crée .vercel/project.json).
- `iloc vercel project list [--limit <val>] [--profile <val>]`
  Liste vos projets Vercel.
- `iloc vercel project unlink [--yes]`
  Retire la liaison locale entre le dossier courant et un projet Vercel.
- `iloc vercel project update [PROJECT] [--name <val>] [--framework <val>] [--root <val>] [--build <val>] [--output <val>] [--install <val>] [--node <val>] [--prod-branch <val>] [--profile <val>]` ⚠️
  Met à jour la configuration d'un projet Vercel existant.
- `iloc vercel project view [PROJECT] [--profile <val>]`
  Affiche le détail complet d'un projet : framework, configuration de build, dépôt Git lié, derniers déploiements.
- `iloc vercel remove <NAME> [--yes]` ⚠️
  Déconnecte un compte Vercel d'ilocker.
- `iloc vercel secret add <NAME> [VALUE] [--profile <val>]` ⚠️
  Crée un secret Vercel legacy.
- `iloc vercel secret delete <NAME> [--profile <val>] [--yes]` 🔴
  Supprime un secret Vercel legacy.
- `iloc vercel secret list [--profile <val>]`
  Liste les secrets Vercel legacy (mécanisme historique, distinct des variables d'environnement).
- `iloc vercel secret rename <NAME> <NEW_NAME> [--profile <val>]`
  Renomme un secret Vercel legacy.
- `iloc vercel status [--profile <val>]`
  Affiche le compte Vercel connecté et vérifie que le token est valide.
- `iloc vercel team list [--profile <val>]`
  Liste les teams Vercel accessibles avec votre compte, et votre rôle dans chacune.
- `iloc vercel team switch <SLUG> [--profile <val>]`
  Change la team par défaut pour le profil actif.
- `iloc vercel use <NAME>`
  Change le compte Vercel actif.
- `iloc vercel webhook create <URL> [--event <val>] [--profile <val>]` ⚠️
  Crée un webhook Vercel qui notifiera une URL lors d'événements de déploiement.
- `iloc vercel webhook delete <ID> [--profile <val>] [--yes]` 🔴
  Supprime un webhook Vercel.
- `iloc vercel webhook list [--profile <val>]`
  Liste les webhooks Vercel configurés au niveau du compte.

### Supabase

- `iloc supabase advisor performance [PROJECT_REF] [--profile <val>]`
  Lance une analyse de performance automatique (index manquants, requêtes lentes détectées, etc.).
- `iloc supabase advisor security [PROJECT_REF] [--profile <val>]`
  Lance une analyse de sécurité automatique du projet (RLS manquantes, politiques trop permissives, etc.).
- `iloc supabase branch create <NAME> [PROJECT_REF] [--profile <val>]`
  Crée une nouvelle branche de preview.
- `iloc supabase branch delete <BRANCH_ID> [--profile <val>] [--yes]` 🔴
  Supprime une branche de preview et sa base de données associée.
- `iloc supabase branch list [PROJECT_REF] [--profile <val>]`
  Liste les branches (environnements de preview isolés, chacun avec sa propre base de données) d'un projet.
- `iloc supabase branch merge <BRANCH_ID> [--profile <val>] [--yes]` ⚠️
  Merge les changements d'une branche de preview vers la production.
- `iloc supabase branch rebase <BRANCH_ID> [--profile <val>] [--yes]` ⚠️
  Rebase une branche de preview sur l'état actuel de la production.
- `iloc supabase branch reset <BRANCH_ID> [--migration-version <val>] [--profile <val>] [--yes]` 🔴
  Réinitialise une branche de preview, en perdant les changements non trackés dans des migrations.
- `iloc supabase extension list [PROJECT_REF] [--installed-only] [--profile <val>]`
  Liste les extensions PostgreSQL disponibles et installées sur le projet.
- `iloc supabase function delete <SLUG> [PROJECT_REF] [--profile <val>] [--yes]` 🔴
  Supprime une Edge Function.
- `iloc supabase function deploy <SLUG> <FILE> [PROJECT_REF] [--no-verify-jwt] [--profile <val>] [--yes]` ⚠️
  Déploie (ou met à jour) une Edge Function à partir d'un fichier source local.
- `iloc supabase function list [PROJECT_REF] [--profile <val>]`
  Liste les Edge Functions déployées sur un projet.
- `iloc supabase function view <SLUG> [PROJECT_REF] [--profile <val>]`
  Affiche le détail d'une Edge Function : version, statut, vérification JWT, URL d'appel.
- `iloc supabase keys [PROJECT_REF] [--reveal] [--profile <val>]` ⚠️
  Affiche les clés API d'un projet (anon, service_role).
- `iloc supabase list`
  Liste les comptes Supabase connectés.
- `iloc supabase migration list [PROJECT_REF] [--profile <val>]`
  Liste les migrations déjà appliquées sur le projet distant.
- `iloc supabase migration push [PROJECT_REF] [--dir <val>] [--profile <val>] [--yes]` ⚠️
  Applique les migrations locales absentes du serveur — idempotent par construction.
- `iloc supabase migration status [PROJECT_REF] [--dir <val>] [--profile <val>]`
  Compare les migrations locales (dossier ./supabase/migrations par défaut) à celles déjà appliquées sur le serveur.
- `iloc supabase org list [--profile <val>]`
  Liste les organisations Supabase accessibles avec votre compte.
- `iloc supabase project create [NAME] [--org <val>] [--region <val>] [--db-pass <val>] [--link] [--profile <val>] [--yes]` ⚠️
  Crée un nouveau projet Supabase (base de données PostgreSQL + services associés).
- `iloc supabase project delete [PROJECT_REF] [--profile <val>] [--yes]` 🔴
  Supprime définitivement un projet Supabase — action irréversible.
- `iloc supabase project list [--profile <val>]`
  Liste vos projets Supabase.
- `iloc supabase project pause [PROJECT_REF] [--profile <val>] [--yes]` ⚠️
  Met en pause un projet Supabase (arrête la facturation compute, la base de données reste intacte).
- `iloc supabase project restore [PROJECT_REF] [--profile <val>] [--yes]`
  Sort un projet Supabase de pause.
- `iloc supabase project url [PROJECT_REF] [--profile <val>]`
  Affiche l'URL du dashboard web d'un projet.
- `iloc supabase project view [PROJECT_REF] [--profile <val>]`
  Affiche le détail d'un projet : statut, région, URL, date de création.
- `iloc supabase remove <NAME> [--yes]` ⚠️
  Déconnecte un compte Supabase d'ilocker.
- `iloc supabase secret delete <KEY> [PROJECT_REF] [--profile <val>] [--yes]` 🔴
  Supprime un secret.
- `iloc supabase secret list [PROJECT_REF] [--profile <val>]`
  Liste les noms des secrets configurés pour les Edge Functions d'un projet.
- `iloc supabase secret set <KEY> [VALUE] [PROJECT_REF] [--profile <val>]` ⚠️
  Crée ou met à jour un secret accessible depuis les Edge Functions.
- `iloc supabase sql <QUERY> [PROJECT_REF] [--profile <val>] [--yes]` ⚠️
  Exécute une requête SQL directement sur la base de données du projet.
- `iloc supabase status [--profile <val>]`
  Affiche le compte Supabase connecté et vérifie que le token est valide.
- `iloc supabase table list [PROJECT_REF] [--schema <val>] [--profile <val>]`
  Liste les tables d'un schéma, avec le nombre de lignes et l'état de la sécurité au niveau ligne (RLS).
- `iloc supabase use <NAME>`
  Change le compte Supabase actif.

### Providers déclaratifs (tiers)

- `iloc provider init <SLUG>`
  Crée un nouveau manifeste de provider tiers (fichier TOML commenté prêt à éditer).
- `iloc provider install [NAME] [--file <val>]`
  Installe un manifeste de provider — depuis le registre communautaire (nom), ou localement (--file).
- `iloc provider search <QUERY>`
  Cherche des providers publiés dans le registre communautaire.
- `iloc provider publish --file <val>`
  Valide un manifeste selon les règles de publication puis prépare sa soumission au registre communautaire.
- `iloc provider list`
  Liste les providers tiers installés localement.
- `iloc provider profile list <SLUG>`
  Liste les profils (comptes) connectés pour un provider tiers.
- `iloc provider profile remove <SLUG> <NAME> [--yes]` 🔴
  Supprime un profil précis d'un provider tiers, sans affecter ses autres profils.
- `iloc provider profile use <SLUG> <NAME>`
  Change le profil actif utilisé par défaut pour un provider tiers.
- `iloc provider remove <SLUG> [--yes]` 🔴
  Désinstalle un provider tiers : supprime son manifeste et purge tous ses identifiants stockés.
- `iloc provider test <PATH>` ⚠️
  Teste un manifeste de provider avec de vrais appels API, sans jamais stocker les identifiants.
- `iloc provider validate <PATH>`
  Valide un manifeste de provider : schéma et garde-fous de sécurité.

### Déploiement orchestré

- `iloc deploy [--yes] [--dry-run] [--force-new] [--skip-github] [--skip-vercel] [--skip-supabase] [--github-profile <val>] [--vercel-profile <val>] [--supabase-profile <val>] [--org <val>] [--team <val>]` ⚠️
  Orchestrateur intelligent : détecte, lie, ou crée automatiquement GitHub, Vercel et Supabase pour le projet courant, sans jamais dupliquer une ressource existante.

### Sentinel (protection auto-save)

- `iloc sentinel disable`
  Désactive le Sentinel dans les fichiers de configuration shell, sans désinstaller les hooks.
- `iloc sentinel enable`
  Active la protection automatique avant les commandes destructrices (rm -rf, git reset --hard, docker system prune, etc.).
- `iloc sentinel init`
  Installe les hooks Sentinel sans les activer immédiatement dans le shell courant.
- `iloc sentinel status`
  Affiche l'état du Sentinel : hooks installés, activation par shell, et si actif dans la session courante.
- `iloc sentinel uninstall` ⚠️
  Désinstalle complètement le Sentinel : retire les blocs de configuration ET supprime les scripts de hook.

### Studio (centre de commandes)

- `iloc studio open`
  Ouvre le centre de commandes ilocker dans VS Code.

### Réseau de pairs (mesh) & compte ilocker Cloud

- `iloc login`
  Connecte votre compte ilocker Cloud (optionnel — pour le réseau de pairs et les futures fonctionnalités liées au compte).
- `iloc logout`
  Déconnecte votre compte ilocker Cloud.
- `iloc whoami`
  Affiche le compte ilocker Cloud actuellement connecté.
- `iloc node join`
  Rejoint le réseau de pairs ilocker en tant que nœud STUN volontaire (aide les autres à se connecter en P2P direct).
- `iloc node leave`
  Quitte le réseau de pairs — exerce le droit RGPD à l'effacement (votre jeton de nœud est supprimé du serveur).
- `iloc node start`
  Démarre le nœud STUN local (nécessite d'avoir rejoint le réseau au préalable).
- `iloc node status`
  Affiche si vous participez au réseau de pairs et la configuration actuelle (port, bande passante allouée).

### Auto-gestion & installation

- `iloc selfinstall [--dir <val>] [--check]`
  Installe le binaire courant dans le PATH du système.
- `iloc update [--check]`
  Vérifie et installe la dernière version d'ilocker depuis GitHub Releases.
- `iloc completion <SHELL> [--setup]`
  Génère un script de complétion shell (Tab pour compléter les commandes et options).

## Sécurité des credentials — vue d'ensemble

Trois familles de secrets locaux, toutes protégées par le même principe :
trousseau système en premier essai, puis repli chiffré sur disque si
indisponible — jamais de perte silencieuse, jamais de texte en clair.

| Secret | Mécanisme primaire | Repli si indisponible | Fichier de repli |
|---|---|---|---|
| Tokens GitHub/Vercel/Supabase | Trousseau OS (`keyring`) | Fichier chiffré ChaCha20-Poly1305 | `~/.config/ilocker/credentials/<compte>.<provider>.vault` |
| Identifiants providers déclaratifs (tiers) | Trousseau OS (`keyring`) | Fichier chiffré ChaCha20-Poly1305 | `~/.config/ilocker/credentials/<compte>.provider-<slug>.vault` |
| Credentials cloud BYOC (AWS, etc.) | Trousseau OS (`keyring`) | Fichier chiffré ChaCha20-Poly1305 | `~/.config/ilocker/credentials/<compte>.vault` |
| Session ilocker Cloud (`iloc login`) | — (pas de trousseau, direct) | Fichier chiffré ChaCha20-Poly1305 | `~/.config/ilocker/auth.vault` |

Toutes les clés de chiffrement de repli partagent le même fichier de clé
locale (`~/.config/ilocker/.vault-key`, générée une seule fois, permissions
0600) — un seul mécanisme à auditer, pas un par provider.

**Modèle de menace couvert :** un tiers qui obtient uniquement un fichier de
credentials (backup partiel, export incomplet, erreur de partage) ne peut
rien en tirer sans le fichier de clé, stocké séparément. Ça ne protège pas
contre un accès root complet à la machine — rien ne le peut, sur une machine
où le processus tourne et déchiffre activement.

**Sur Linux, pourquoi un fichier plutôt que le trousseau du noyau
(`linux-keyutils`) ?** Ce backend a été évalué et écarté : il vide tout son
contenu à chaque redémarrage machine, ce qui casserait silencieusement
l'authentification après toute mise à jour de sécurité automatique sur un
serveur de production. Le fichier chiffré, lui, survit aux redémarrages.

---

## Architecture du binaire

```
iloc (binaire statique musl/MSVC ~9 MB)
├── Core local
│   ├── engine.rs          — CoW snapshots (APFS clonefile / Linux FICLONE / NTFS copy)
│   ├── db.rs              — SQLite bundled (WAL mode)
│   ├── snapshot.rs        — scan parallèle adaptatif (Small/Medium/Large tiers)
│   ├── vault.rs           — vault externalisé (Sibling/System/Custom/InProject)
│   ├── merkle.rs          — intégrité des snapshots
│   ├── crypto.rs          — chiffrement P2P (AES-GCM)
│   ├── cloud_crypto.rs    — chiffrement cloud (ChaCha20-Poly1305, RFC 8439)
│   ├── credential_vault.rs — chiffrement du fallback credentials (même primitive)
│   └── chunker.rs         — découpage 4 MiB pour P2P + cloud
│
├── P2P
│   ├── commands/share.rs  — serveur TCP (direct + relay)
│   ├── commands/clone.rs  — client TCP + auto-rehydrate
│   ├── commands/cloud_share.rs — liens cloud pré-signés (modèle Signal 2 canaux)
│   ├── relay_client.rs    — NAT traversal (STUN-like)
│   ├── protocol.rs        — protocole de transfert bincode
│   └── transfer_state.rs  — reprises de transfert (resumable)
│
├── Cloud BYOC
│   ├── s3_client.rs       — SigV4 générique (AWS/Backblaze/MinIO/DO/Supabase/GCS/R2/Wasabi)
│   ├── azure_client.rs    — Shared Key auth Azure Blob Storage
│   ├── cloud_backend.rs   — abstraction S3Client | AzureClient
│   ├── cloud_store.rs     — profils multi-cloud + keyring OS + fallback chiffré
│   ├── presigned.rs       — URLs pré-signées (liens de partage cloud)
│   └── commands/cloud.rs  — push / pull / usage / gc / doctor / verify
│
├── Providers de déploiement
│   ├── github_client.rs   — appels API GitHub (device flow OAuth, repos, secrets Actions)
│   ├── github_store.rs    — credentials GitHub (keyring OS + fallback chiffré)
│   ├── vercel_client.rs   — appels API Vercel
│   ├── vercel_store.rs    — credentials Vercel (keyring OS + fallback chiffré)
│   ├── supabase_client.rs — appels API Supabase
│   ├── supabase_store.rs  — credentials Supabase (keyring OS + fallback chiffré)
│   ├── scanner.rs         — détection framework/remote Git/migrations Supabase
│   ├── deploy_state.rs    — état persistant du déploiement (.ilocker/deploy.toml)
│   └── commands/{github,vercel,supabase,deploy}.rs — commandes CLI + orchestrateur
│
├── Providers déclaratifs (tiers)
│   ├── provider_manifest.rs — schéma TOML + validation (sécurité : HTTPS, anti-SSRF, slugs réservés)
│   ├── provider_store.rs  — credentials multi-profils (keyring OS + fallback chiffré, isolé par slug)
│   ├── provider_engine.rs — client HTTP générique + arbre clap construit dynamiquement au runtime
│   └── commands/provider.rs — init / validate / test / install / list / remove / profile
│
├── Hyperscale
│   ├── erasure.rs         — Reed-Solomon GF(2^8) (crate reed-solomon-erasure)
│   ├── dht.rs             — table DHT locale
│   ├── hyperscale_config.rs — configuration k/m auto selon nb de clouds
│   ├── mesh_node.rs       — nœud de stockage contributeur
│   └── commands/hyperscale.rs — push / clone / export / status / config / node
│
├── Intelligence
│   ├── health_score.rs    — score 0–100 (fréquence, efficacité delta, hygiène binaires)
│   ├── intel_client.rs    — patterns communautaires + déduplication globale
│   └── commands/intelligence.rs — iloc status --health, iloc node
│
├── Auto-gestion
│   ├── updater.rs         — check GitHub Releases + swap atomique
│   └── commands/
│       ├── update.rs      — iloc update
│       └── selfinstall.rs — iloc selfinstall
│
├── Authentification ilocker Cloud (optionnel)
│   └── auth_store.rs      — session iloc login (fallback chiffré, pas de keyring)
│
├── Sentinel
│   └── commands/sentinel.rs — hooks Bash/Zsh globaux (~/.ilocker/hooks/)
│
└── TUI
    ├── commands/dashboard.rs — TUI crossterm (projets + snapshots)
    └── commands/completion.rs — scripts Bash/Zsh
```

---

## Déploiement GitHub Releases — Guide rapide

Le dépôt est déjà configuré pour **github.com/alphabechirdiallo-netizen/ilocker**
(privé) — `updater.rs`, `install.sh` et `install.ps1` pointent tous dessus.
Il ne reste que la mécanique Git.

### 1. Placer les fichiers au bon endroit dans le repo

```
.github/workflows/release.yml    ← workflow CI/CD
ilocker-deploy/install.sh        ← script Linux/macOS
ilocker-deploy/install.ps1       ← script Windows
ilocker-deploy/STANDALONE.md     ← ce document
ilocker/                         ← code source Rust (Cargo.toml + Cargo.lock committés)
```

> **Important :** `ilocker/Cargo.lock` doit être committé et ne jamais être
> supprimé du repo. Il fige les versions de dépendances validées avec
> rustc 1.75 — sans lui, une dépendance transitive publiant une version qui
> exige une édition Rust plus récente peut casser la compilation en CI sans
> prévenir. Le workflow utilise `cargo build --locked`, qui échoue
> explicitement si `Cargo.toml` et `Cargo.lock` divergent, plutôt que de
> dériver silencieusement.

### 2. Publier une release

```bash
git add .
git commit -m "feat: v1.9.0 standalone + providers GitHub/Vercel/Supabase"
git tag v1.9.0
git push origin main --tags
```

GitHub Actions compile automatiquement (5 binaires), génère `SHA256SUMS`
(binaires + scripts d'installation), publie sur GitHub Releases, puis lance
un smoke-test sur les 3 plateformes via `gh release download` (authentifié
— nécessaire pour un dépôt privé).

### 3. Partager l'installation

```bash
# Repo privé : le token est requis pour tout téléchargement direct
export GITHUB_TOKEN="ghp_xxx"
curl -fsSL -H "Authorization: token $GITHUB_TOKEN" \
  https://raw.githubusercontent.com/alphabechirdiallo-netizen/ilocker/main/ilocker-deploy/install.sh | sh

# Windows :
$env:GITHUB_TOKEN = "ghp_xxx"
irm https://github.com/alphabechirdiallo-netizen/ilocker/releases/latest/download/install.ps1 | iex
```

---

## FAQ

**Q : iloc a-t-il besoin d'internet pour fonctionner ?**
R : Non pour `iloc init`, `save`, `undo`, `log`, `status`, ainsi que `share`/`clone` en
mode P2P direct (sans `--relay` ni `--cloud`). Oui pour `iloc update`, les commandes
cloud BYOC, les providers de déploiement (GitHub/Vercel/Supabase), les providers
déclaratifs tiers une fois connectés, et **`iloc hyperscale push`/`clone`** (le
multi-cloud erasure coding nécessite le réseau par nature — seuls
`hyperscale status`/`config show`/`config validate` restent locaux).

**Q : Comment partager iloc sans internet (Xender, Bluetooth) ?**
R : Envoyer le bon binaire pour la plateforme cible. Le destinataire lance
`chmod +x ./iloc-linux-x86_64 && ./iloc-linux-x86_64 selfinstall`. C'est tout.

**Q : Comment les mises à jour fonctionnent-elles ?**
R : `iloc update` interroge `api.github.com`, télécharge le bon binaire (vérifié par SHA-256),
le swap atomiquement. Sur un dépôt privé, une authentification est nécessaire : `iloc update`
utilise automatiquement la variable `GITHUB_TOKEN` si elle est définie, sinon réutilise un
compte déjà connecté via `iloc connect github` — sans configuration supplémentaire dans
ce second cas.

**Q : Mes tokens GitHub/Vercel/Supabase sont-ils en clair sur le disque ?**
R : Non. Le trousseau système est tenté en premier. S'il est indisponible (le cas normal sur
un serveur Linux headless), `iloc` bascule sur un fichier chiffré ChaCha20-Poly1305 — jamais
de texte en clair, et un avertissement s'affiche à chaque bascule.

**Q : Je viens de mettre à jour iloc — dois-je refaire `iloc connect github` ?**
R : Non. Les fichiers de credentials créés par une version antérieure (format JSON en clair)
sont migrés automatiquement vers le nouveau format chiffré dès la première lecture qui suit
la mise à jour. Aucune action requise ; l'ancien fichier en clair est supprimé après migration.

**Q : Le Sentinel fonctionne-t-il sur Windows ?**
R : Oui, nativement — support PowerShell (5.1+ et 7+) via PSReadLine, en plus de Bash et
Zsh. `iloc sentinel enable` détecte et configure automatiquement le profil PowerShell actif,
que ce soit sur Windows natif ou via `pwsh` cross-platform (macOS/Linux).

**Q : Fish shell est-il supporté ?**
R : Oui — `iloc selfinstall` configure le PATH de manière native pour Fish.
Sur Fish ≥ 3.2, `fish_add_path` est utilisé (idempotent, actif immédiatement).
Sur les versions antérieures, `set -gx PATH` est écrit dans `~/.config/fish/config.fish`
(syntaxe fish native. `export PATH=...` existe comme fonction de compatibilité POSIX
sous Fish, mais son usage avec des listes comme PATH est documenté comme pouvant
corrompre le PATH selon la version — `iloc selfinstall` l'évite délibérément).
Le Sentinel (hooks shell) et les complétions `iloc completion` ne couvrent pas encore
Fish — Bash, Zsh et PowerShell sont supportés pour ces fonctionnalités.

**Q : Azure Blob Storage est-il supporté pour les liens de partage cloud ?**
R : Oui. `iloc share --cloud` prend désormais en charge Azure Blob Storage via les
SAS tokens (Shared Access Signature, version 2020-12-06) — le mécanisme natif d'Azure,
distinct des URLs pré-signées S3. Les chunks et le manifest sont signés avec la clé
Shared Key du compte de stockage ; aucune credential supplémentaire n'est requise.
Les tokens SAS accordent uniquement la lecture (`sp=r`) et expirent selon le TTL
demandé (défaut 2 h, maximum 7 jours).

**Q : Comment créer mon propre provider pour un service non couvert nativement ?**
R : `iloc provider init <slug>` génère un manifeste TOML commenté — aucun code à
écrire, juste décrire l'authentification et les opérations (endpoint, méthode,
arguments). `iloc provider validate` puis `iloc provider test` avant
`iloc provider install --file`. Le manifeste ne peut jamais accéder à un domaine
hors de celui déclaré, ni ajouter un header d'authentification autre que celui
défini — la sécurité vient du format, pas d'une revue de code au cas par cas.
Voir la section *Providers déclaratifs* plus haut pour le détail du modèle de
sécurité.

**Q : ilocker est-il gratuit ?**
R : Oui, entièrement. Il n'y a aucun plan payant, aucune limite de fonctionnalité,
aucun compte requis pour les fonctionnalités core, et aucune publicité. L'infrastructure
cloud (bucket S3, Azure…) et les comptes GitHub/Vercel/Supabase appartiennent à
l'utilisateur.

**Q : Le serveur ilocker-server est-il encore supporté ?**
R : Il reste fonctionnel pour les commandes `iloc login` / `iloc logout` / `iloc whoami`
et pour rejoindre le réseau de pairs (`iloc node join`, qui nécessite `iloc login` au
préalable). Aucune autre commande ne le contacte : ni les fonctionnalités core, ni les
providers de déploiement GitHub/Vercel/Supabase (authentification directe auprès de
chaque plateforme, indépendamment d'ilocker Cloud), ni les providers déclaratifs tiers
(mêmes principes), ni `iloc save`/`undo`/`push`/`pull`/`hyperscale` (aucun appel réseau
caché vers ilocker Cloud dans ces commandes).
