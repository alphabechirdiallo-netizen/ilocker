# Contribuer un provider

## Critères d'acceptation

- `iloc provider validate <fichier>` doit afficher **"Prêt pour la
  publication publique aussi"** — sinon la pull request est rejetée par
  la CI avant toute revue humaine.
- `provider.slug` dans le manifeste doit correspondre exactement au nom
  du fichier (`ilocker-registry/providers/<slug>.toml`) et au champ
  `slug` de l'entrée `index.json`.
- `manifest_sha256` dans `index.json` doit être le sha256 exact du
  fichier soumis (`sha256sum ilocker-registry/providers/<slug>.toml`) —
  la CI le recalcule et rejette toute divergence.
- `manifest_url` doit pointer vers le fichier tel qu'il sera servi une
  fois la PR fusionnée sur `main` du dépôt `ilocker` :
  `https://raw.githubusercontent.com/alphabechirdiallo-netizen/ilocker/main/ilocker-registry/providers/<slug>.toml`
- Un slug ne peut être revendiqué qu'une fois. Une mise à jour d'un
  provider existant modifie l'entrée en place (nouveau sha256, nouvelle
  version) plutôt que d'en créer une seconde.
- Aucun manifeste ne doit déclarer un `api.base_url` ou un
  `auth.token_url` avec exception localhost (`iloc provider
  validate`/`publish` refusent déjà ceci automatiquement — la CI ne fait
  que le revérifier).
- Les modifications touchant `ilocker-registry/**` n'affectent jamais le
  code du CLI lui-même : une PR de provider ne doit contenir AUCUN
  changement sous `ilocker/src/`.

## Ce que la revue humaine vérifie en plus de la CI

- Le provider fait raisonnablement ce que sa `description` annonce.
- Les opérations `danger = "destructive"` sont correctement identifiées
  comme telles (suppression, révocation, tout ce qui n'est pas
  réversible depuis l'interface de l'API elle-même).
- Pas de nom trompeur (un manifeste nommé "stripe" doit réellement cibler
  l'API Stripe officielle, pas un service tiers du même nom).

## Mettre à jour un provider existant

Même processus qu'une nouvelle soumission : modifiez
`ilocker-registry/providers/<slug>.toml`, recalculez son sha256 via
`iloc provider publish --file ilocker-registry/providers/<slug>.toml`,
mettez à jour l'entrée correspondante dans `ilocker-registry/index.json`
(version + sha256), ouvrez une PR.

## Signaler un provider problématique

Ouvrez une issue plutôt qu'une PR — la suppression d'une entrée
`index.json` est traitée comme n'importe quelle autre modification du
registre, mais mérite une discussion avant d'être appliquée.
