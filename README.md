# ilocker

**Un seul binaire. Zéro serveur.** Snapshots instantanés, coffre-fort chiffré, partage P2P
Zero-Knowledge, sauvegarde cloud BYOC et déploiement GitHub/Vercel/Supabase — le tout dans
`iloc`, un exécutable Rust statique qui s'installe en une commande et se distribue par
n'importe quel moyen (USB, Xender, Bluetooth, email).

[![Release](https://github.com/alphabechirdiallo-netizen/ilocker/actions/workflows/release.yml/badge.svg)](https://github.com/alphabechirdiallo-netizen/ilocker/actions/workflows/release.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](./LICENSE)

## Installation

```bash
# Linux / macOS
curl -fsSL https://github.com/alphabechirdiallo-netizen/ilocker/releases/latest/download/install.sh | sh

# Windows (PowerShell)
irm https://github.com/alphabechirdiallo-netizen/ilocker/releases/latest/download/install.ps1 | iex

# Depuis un binaire reçu hors-ligne (USB, Xender, Bluetooth, email…)
chmod +x ./iloc-linux-x86_64 && ./iloc-linux-x86_64 selfinstall
```

Binaires disponibles pour Linux (x86_64/aarch64), macOS (x86_64/aarch64) et Windows, avec
somme de contrôle SHA-256 — voir la page [Releases](https://github.com/alphabechirdiallo-netizen/ilocker/releases/latest).

## Démarrage rapide

```bash
iloc init          # initialise un projet (vault externalisé par défaut)
iloc save "msg"     # snapshot instantané (copy-on-write : APFS / Btrfs / ext4 / NTFS)
iloc undo           # restaure le dernier snapshot
iloc update         # met à jour iloc lui-même
```

## Fonctionnalités

| | |
|---|---|
| **Snapshots locaux** | `iloc save` / `undo` / `log` / `status` — historique instantané en copy-on-write, sans jamais dupliquer les dossiers de dépendances (`node_modules`, `.venv`, `target`…) |
| **Vault 3-2-1** | miroir local (Tier 2), sauvegarde cloud (Tier 3a) ou hyperscale multi-cloud (Tier 3b) |
| **Cloud BYOC** | AWS S3, Backblaze, MinIO, DigitalOcean, Supabase, GCS, R2, Wasabi, Azure — chiffrement ChaCha20-Poly1305 avant envoi, identifiants dans le trousseau OS |
| **Déploiement** | `iloc deploy` orchestre GitHub, Vercel et Supabase en un seul appel |
| **Partage P2P** | `iloc share` / `clone` — direct ou relais NAT, chiffré de bout en bout |
| **Providers déclaratifs** | connectez n'importe quelle API HTTP/GraphQL via un manifeste TOML, sans recompiler ilocker — voir [ilocker-registry](./ilocker-registry) |
| **Hyperscale** | erasure coding Reed-Solomon (GF(2^8)) multi-cloud |
| **Sentinel** | snapshot automatique avant les commandes destructrices |

La référence complète des commandes est documentée dans
[`ilocker-deploy/STANDALONE.md`](./ilocker-deploy/STANDALONE.md), et consultable de façon
interactive via l'extension VS Code ci-dessous.

## Registre de providers

[`ilocker-registry/`](./ilocker-registry) est l'index communautaire des providers publiés :

```bash
iloc provider search stripe
iloc provider install stripe
iloc connect stripe
```

Voir [CONTRIBUTING.md](./ilocker-registry/CONTRIBUTING.md) pour publier le vôtre.

## Extension VS Code

[`ilocker-studio-vscode/`](./ilocker-studio-vscode) ajoute un centre de commandes visuel
directement dans l'éditeur. Le `.vsix` est publié sur chaque
[release](https://github.com/alphabechirdiallo-netizen/ilocker/releases/latest) :

```bash
code --install-extension ilocker-studio.vsix
```

## Compiler depuis les sources

```bash
cd ilocker
cargo build --release --locked   # rustc 1.75+, sans dépendance runtime (musl / MSVC)
```

## Contribuer

Les pull requests sont bienvenues. Pour un provider du registre, voir
[ilocker-registry/CONTRIBUTING.md](./ilocker-registry/CONTRIBUTING.md) — les changements
de providers n'affectent jamais le code du CLI (`ilocker/src/`) et passent par une CI dédiée
avant toute revue humaine.

## Licence

[MIT](./LICENSE)
