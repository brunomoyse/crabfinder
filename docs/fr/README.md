# Documentation utilisateur — crabfinder (FR)

Bienvenue. Cette documentation est écrite pour un utilisateur qui veut **utiliser** crabfinder, pas le développer. Profil cible : un géomaticien à l'aise avec une ligne de commande, qui sait lire un CSV, qui n'a pas besoin de connaître Rust ni Tantivy.

> Version anglaise : voir [`../en/`](../en/README.md).

## crabfinder en une phrase

Un service de géocodage belge basé sur les données fédérales **BeSt Address** (6,9 millions d'adresses Bruxelles + Flandre + Wallonie). Deux usages :

1. **Recherche en ligne** — une requête, un top-N classé. Pour intégrer dans une application interactive (formulaire, carte).
2. **Géocodage batch** — un CSV en entrée, un CSV ou du GeoJSON enrichi en sortie. Pour traiter une liste d'adresses d'un coup.

Tout tourne en local. Pas d'appel externe. Index unique d'environ 2,2 Go sur disque.

**BeSt Address est la seule source.** Chaque adresse, identifiant, coordonnée et label renvoyé par crabfinder vient de BeSt. OpenStreetMap est une aide facultative, désactivée par défaut, pour comprendre les requêtes qui nomment un hameau (`--places`, indexeur) ou un point d'intérêt (`--pois`, service) : il sert seulement à décider où chercher. Il n'ajoute ni ne modifie jamais une adresse et n'est jamais renvoyé comme résultat — la réponse reste l'adresse BeSt la plus proche, signalée par `confidence: locality` / `landmark`.

## Quickstart

```bash
# 1. Construire les binaires (release impératif)
cargo build --release

# 2. Construire l'index (~1,5-3 min, ~2,2 Go disque)
./target/release/crabfinder-indexer \
    --best-dir ./best-full-latest \
    --output ./index-belgium \
    --notability-whitelist data/notability-whitelist-v1.yaml

# 3a. Recherche en ligne — démarrer le service
./target/release/crabfinder-service \
    --index ./index-belgium \
    --abbreviation-dict data/street-type-abbreviations-v1.yaml &

curl "http://127.0.0.1:8080/geocode?q=Avenue+Louise+32+1050"

# 3b. Batch — depuis un CSV, sans démarrer le service
./target/release/crabfinder-batch \
    --index ./index-belgium \
    --abbreviation-dict data/street-type-abbreviations-v1.yaml \
    --input adresses.csv \
    --output adresses-geocodees.csv
```

Si l'une de ces commandes ne tourne pas chez toi, va directement à [`02-installation.md`](02-installation.md).

## Table des matières

| Fichier | Contenu | Lire quand |
|---|---|---|
| [01-introduction.md](01-introduction.md) | Ce que fait crabfinder, ce qu'il ne fait pas | Avant de l'installer |
| [02-installation.md](02-installation.md) | Installer Rust, récupérer BeSt, construire l'index, options du service, mise à jour | Au premier setup |
| [03-batch.md](03-batch.md) | Géocoder un CSV (CLI + endpoint HTTP) | Cas d'usage principal pour traitement de listes |
| [04-recherche-en-ligne.md](04-recherche-en-ligne.md) | API HTTP : `/geocode`, `/reverse`, `/building/{id}/units`, `/metrics` | Pour intégrer crabfinder dans une appli |
| [05-comprendre-resultats.md](05-comprendre-resultats.md) | Lire les sorties : `confidence`, `fallback_reason`, `position_source`, `ambiguous`, `score` | Avant d'exploiter un CSV géocodé |
| [06-faq.md](06-faq.md) | Erreurs courantes, performance, dépannage | En cas de pépin |

## Conventions de cette doc

- Toutes les commandes sont à lancer depuis la racine du dépôt `crabfinder/`.
- Les chemins commencent par `./` quand ils sont relatifs à la racine.
- Les chiffres de performance (temps d'indexation, latence, etc.) sont indicatifs sur un laptop récent (M-series ou x86_64 multi-cœurs avec SSD).
- Quand une commande propose plusieurs options équivalentes (`--no-mid-street` côté CLI, `mid_street_fallback=false` côté HTTP), les deux sont documentées dans la même section.

## Licence

Code crabfinder : MIT OR Apache-2.0. Données BeSt Address : © autorités fédérales belges, licence open data (à redistribuer selon leurs conditions). Aides OpenStreetMap facultatives `data/osm-places-be-v1.csv` et `data/osm-pois-be-v1.csv` : © les contributeurs d'OpenStreetMap, ODbL 1.0.
