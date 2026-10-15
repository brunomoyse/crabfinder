# 2. Installation

À faire une fois, puis seulement quand BeSt publie un nouveau dump (l'index est figé sur le dump utilisé à la construction). Pour les mises à jour, `scripts/refresh_best.sh` automatise les étapes 4 et 5 — voir [Mettre à jour les données](#mettre-à-jour-les-données).

## Pré-requis matériels

- **Rust 1.90 ou plus récent** (installé via `rustup`, voir plus bas).
- **~3 Go d'espace disque libre** pour l'index Tantivy (~2,2 Go) + fichiers annexes (~13 Mo) + marge. Une mise à jour garde l'ancien index à côté du nouveau jusqu'à ce que tu le supprimes : compte ~5 Go si tu utilises le script de mise à jour.
- **~4 Go de RAM libre** pendant l'indexation (essentiellement pour la jointure des dictionnaires en mémoire). Le service tournant en régime stable consomme moins.
- Un système **Linux, macOS ou Windows** (Linux et macOS sont les chemins testés ; sur Windows, utiliser WSL2 simplifie la vie).

## Étape 1 — Installer Rust

Si `cargo --version` ne renvoie rien :

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
# Suivre les instructions par défaut, puis recharger le shell :
source "$HOME/.cargo/env"
cargo --version    # doit afficher "cargo 1.90.x" ou plus récent
```

Sur Windows sans WSL : passer par `https://www.rust-lang.org/tools/install` puis redémarrer le terminal.

## Étape 2 — Récupérer le code

```bash
git clone https://github.com/brunomoyse/crabfinder.git
cd crabfinder
```

(Si on t'a remis une archive `.tar.gz` au lieu d'un dépôt git, extraire et entrer dans le dossier décompressé.)

## Étape 3 — Construire les binaires

```bash
cargo build --release
```

Cette commande compile **tous** les binaires : `crabfinder-indexer`, `crabfinder-service`, `crabfinder-batch`, `crabfinder-eval`. La première compilation dure 3-8 min selon la machine (Tantivy en dépendance). Les exécutables atterrissent dans `target/release/`.

**Toujours utiliser `--release`** : un binaire `cargo build` debug est 10× plus lent et l'indexation ne tient pas dans un temps raisonnable.

Vérification rapide :

```bash
./target/release/crabfinder-indexer --version
./target/release/crabfinder-service --version
./target/release/crabfinder-batch --version
```

## Étape 4 — Télécharger les données BeSt Address

BeSt publie un bundle ZIP par région. Tu as besoin des trois pour couvrir la Belgique.

### Sources officielles

Page de téléchargement : [https://opendata.bosa.be/](https://opendata.bosa.be/) — chercher « BeSt Address » dans le catalogue.

Trois archives à récupérer :
- `BrusselsAddress_*.zip`
- `FlandersAddress_*.zip`
- `WalloniaAddress_*.zip`

Plus, **dans la même page**, les archives sœurs :
- `Municipality_*.zip`
- `PostalInfo_*.zip`
- `StreetName_*.zip`
- `PartOfMunicipality_*.zip` (Wallonie uniquement — Bruxelles et la Flandre n'en publient pas)

### Mise en place

Crée un dossier `best-full-latest/` et **dépose toutes les archives ZIP dedans sans les décompresser**. L'indexeur lit directement les ZIP.

```bash
mkdir best-full-latest
mv ~/Downloads/*Address*.zip best-full-latest/
mv ~/Downloads/Municipality*.zip best-full-latest/
mv ~/Downloads/PostalInfo*.zip best-full-latest/
mv ~/Downloads/StreetName*.zip best-full-latest/
mv ~/Downloads/PartOfMunicipality*.zip best-full-latest/

ls best-full-latest/      # vérifier la présence des ~10 ZIP
```

## Étape 5 — Construire l'index

```bash
./target/release/crabfinder-indexer \
    --best-dir ./best-full-latest \
    --output ./index-belgium \
    --notability-whitelist data/notability-whitelist-v1.yaml \
    --places data/osm-places-be-v1.csv
```

Sortie typique (dump du 2026-10-15, portable Apple Silicon) :

```
====== indexing summary ======
  Brussels: pass1=   49ms  pass2=   9772ms  parsed=848825 emitted=845464 skipped_status=3361 ... places_attached=414804
  Flanders: pass1=  265ms  pass2=  52383ms  parsed=4218617 emitted=3933743 skipped_status=284874 ... places_attached=3014584
  Wallonia: pass1=  192ms  pass2=  25818ms  parsed=2107430 emitted=2107430 skipped_status=0 ... missing_position=140263 ...
     TOTAL: parsed=7174872 emitted=6886637 skipped_status=288235 skipped_unresolved=0 missing_position=140263 ...
  mid_street entries: 145123
  imputed positions: same_number=44909 interpolated=87293 street=0 locality=8061 dropped=0
  localities: 16045 (OSM places loaded: 12980)
  commit: 856ms
  wall-clock total: 91279ms (1.5 min)
  index size on disk: 2312810065 bytes (2205.7 MB / 2.15 GB)
```

Comment la lire :

- `skipped_status` : adresses retirées ou en projet, volontairement écartées (seules les adresses en vigueur sont indexées).
- `missing_position` / `imputed positions` : BeSt publie certaines adresses wallonnes (140 263 dans ce dump) sans position (Lambert 72 `0 0`, qui tomberait en France). L'indexeur leur attribue une position à partir de leurs voisines, et note comment dans `position_source` — voir [`05-comprendre-resultats.md`](05-comprendre-resultats.md#doù-viennent-les-coordonnées--position_source).
- `localities` : communes, sections de commune, zones postales et villages/hameaux OpenStreetMap pour lesquels le service sait répondre à une recherche de lieu (« Maredsous », « Spontin »).

Le dossier `./index-belgium/` contient maintenant :

- Les fichiers de segments Tantivy (`*.idx`, `*.pos`, `*.term`, etc.).
- Un sidecar `.crab-meta.json` avec la version du schéma (actuellement **9**) et la date de construction.
- `mid-street-v1.bin` (~10 Mo) : les points de rue utilisés par le repli `median_street`.
- `localities-v1.json` (~3 Mo) : les noms de lieux et leurs centres, pour les recherches de lieu.

**Le dump BeSt utilisé est figé dans l'index.** Quand un nouveau dump est publié, tu refais l'étape 4 (remplacer les ZIP) puis l'étape 5 (reconstruire). Ce n'est pas une migration incrémentale.

### Options utiles

| Flag | Quand l'utiliser |
|---|---|
| `--region brussels` (ou `flanders` / `wallonia`) | Tests, ou besoin régional seulement |
| `--limit 1000` | Smoke test rapide (1000 adresses par région) |
| `--print-samples 5` | Afficher 5 adresses parsées avant indexation pour valider visuellement |
| `--notability-whitelist CHEMIN` | Boost de classement des lieux emblématiques (voir plus bas) |
| `--places CHEMIN` | Villages et hameaux OpenStreetMap (voir plus bas) |

Les deux fichiers sont livrés dans `data/`, donc autant s'en servir :

- Sans `--notability-whitelist`, tout marche pareil mais les toponymes iconiques (Grand-Place, Atomium…) ne sont plus boostés au classement.
- Sans `--places`, les recherches de lieu ne connaissent que les communes, sections de commune et zones postales de BeSt : un hameau comme « Maredsous » (rattaché à Anhée, sans entité BeSt propre) n'est plus trouvé. Le CSV est dérivé d'OpenStreetMap (© les contributeurs d'OpenStreetMap, ODbL 1.0, voir `data/README.md`).

## Étape 6 — Démarrer le service HTTP (pour le mode recherche)

Si tu vas utiliser exclusivement la CLI batch, tu peux sauter cette étape.

```bash
./target/release/crabfinder-service \
    --index ./index-belgium \
    --abbreviation-dict data/street-type-abbreviations-v1.yaml \
    --port 8080
```

Sortie attendue :

```
INFO starting crabfinder-service index=./index-belgium port=8080
INFO index sidecar OK schema_version=9 tokenizer=crab
INFO spatial index built points=6878576 elapsed_ms=1605
INFO abbreviation dictionary loaded path="data/street-type-abbreviations-v1.yaml" n_entries=17 ...
INFO address-count index built elapsed_ms=20048 n_docs=6886637 n_streets=144205 max_count=5649
INFO mid_street side-file loaded path=./index-belgium/mid-street-v1.bin entries=145123
INFO localities loaded entries=16045
INFO listening at http://127.0.0.1:8080
```

Le service n'ouvre son port qu'une fois tout chargé (~20 s sur un portable) : la première requête voit donc toujours un index prêt. Ensuite, chaque requête est journalisée avec son statut et sa latence (`RUST_LOG=warn` pour les faire taire).

Test rapide depuis un autre terminal :

```bash
curl "http://127.0.0.1:8080/health"
# {"status":"ok","index_path":"./index-belgium","index_docs":6886637,"uptime_secs":12,
#  "schema_version":9,"index_built_at_unix_secs":1792100762,"version":"0.1.0"}

curl "http://127.0.0.1:8080/geocode?q=Grand-Place+1000&limit=2"
```

### Options du service

| Flag | Défaut | Effet |
|---|---|---|
| `--index CHEMIN` | — (obligatoire) | Dossier de l'index. |
| `--host IP` | `127.0.0.1` | Adresse d'écoute. `0.0.0.0` pour écouter sur toutes les interfaces (conteneurs). |
| `--port N` | `8080` | Port TCP. |
| `--abbreviation-dict CHEMIN` | (aucun) | Expansion `av` → `avenue`. **Recommandé** : `data/street-type-abbreviations-v1.yaml`. |
| `--request-timeout-ms N` | `10000` | Délai maximal d'une requête, attente d'une place libre comprise. Au-delà, le service répond `503` avec une erreur JSON. |
| `--batch-timeout-secs N` | `600` | Idem pour `POST /batch/geocode`, jusqu'au début de la réponse. |
| `--max-concurrency N` | `256` | Requêtes traitées en même temps ; les autres attendent une place (dans leur délai). |
| `--cors-origin URL` | toutes (`*`) | Origine navigateur autorisée. Répéter le flag (ou séparer par des virgules) pour en mettre plusieurs. |
| `--no-metrics` | désactivé | Ne pas servir les métriques Prometheus sur `/metrics`. |
| `--no-notability-boost` | désactivé | Classer sans le boost de notoriété (tests A/B). |

Pour arrêter le service : `Ctrl+C` dans le terminal où il tourne.

### Faire tourner en arrière-plan

```bash
nohup ./target/release/crabfinder-service \
    --index ./index-belgium \
    --abbreviation-dict data/street-type-abbreviations-v1.yaml \
    > crabfinder.log 2>&1 &
echo $! > crabfinder.pid
```

Puis pour arrêter : `kill $(cat crabfinder.pid)`.

## Variables d'environnement (alternative aux flags)

Chaque flag du service a son équivalent en variable d'environnement (`CRABFINDER_` + le nom du flag en majuscules : `CRABFINDER_REQUEST_TIMEOUT_MS`, `CRABFINDER_MAX_CONCURRENCY`, `CRABFINDER_CORS_ORIGIN`, `CRABFINDER_NO_METRICS`…), pratique pour un déploiement systemd ou Docker :

```bash
export CRABFINDER_INDEX=./index-belgium
export CRABFINDER_HOST=127.0.0.1      # défaut ; 0.0.0.0 pour écouter sur toutes les interfaces
export CRABFINDER_PORT=8080
export CRABFINDER_ABBREVIATION_DICT=./data/street-type-abbreviations-v1.yaml

./target/release/crabfinder-service     # plus besoin des flags
```

Idem pour `crabfinder-batch` (`CRABFINDER_INDEX`, `CRABFINDER_ABBREVIATION_DICT`).

## Docker (alternative aux étapes 1, 3, 5 et 6)

Le `Dockerfile` construit `crabfinder-service`, `crabfinder-indexer` et `crabfinder-batch` dans une seule image, avec les deux fichiers YAML inclus. Seules les données BeSt (étape 4) sont nécessaires sur la machine hôte. L'index vit dans un volume monté sur `/index`, qui doit être accessible en écriture (Tantivy pose un fichier de verrou même en lecture).

```bash
docker build -t crabfinder .

# Construire l'index dans un volume nommé (~2–3 min)
docker run --rm -v ./best-full-latest:/best:ro -v crabfinder-index:/index crabfinder \
    crabfinder-indexer --best-dir /best --output /index \
    --notability-whitelist /usr/share/crabfinder/notability-whitelist-v1.yaml

# Le servir
docker run -d --name crabfinder -p 8080:8080 -v crabfinder-index:/index crabfinder
curl "http://127.0.0.1:8080/health"

# Batch via stdin/stdout
docker run --rm -i -v crabfinder-index:/index crabfinder crabfinder-batch < input.csv > output.csv
```

Dans l'image, le service écoute sur `0.0.0.0` (`CRABFINDER_HOST`) et le dictionnaire d'abréviations est préconfiguré. Le CSV des lieux OpenStreetMap n'est pas inclus : pour indexer les hameaux, monter `data/` et ajouter `--places` :

```bash
docker run --rm -v ./best-full-latest:/best:ro -v ./data:/data:ro -v crabfinder-index:/index crabfinder \
    crabfinder-indexer --best-dir /best --output /index \
    --notability-whitelist /usr/share/crabfinder/notability-whitelist-v1.yaml \
    --places /data/osm-places-be-v1.csv
```

## Mettre à jour les données

`scripts/refresh_best.sh` fait une mise à jour complète d'une traite :

1. télécharge le bundle BeSt (`best-full-latest.zip` sur opendata.bosa.be) dans `best-<date>/` et le décompresse ;
2. compile les binaires release et indexe dans `index-<date>/` (avec la whitelist, et `--places` si `data/osm-places-be-v1.csv` est présent) ;
3. démarre un service temporaire sur le port 18080 et lance `crabfinder-eval --validate-eval-set` : chaque adresse attendue par le jeu d'évaluation doit toujours exister (le script s'arrête si plus de 5 % manquent) ;
4. fait pointer le lien symbolique `index-current` vers le nouvel index, de façon atomique.

```bash
scripts/refresh_best.sh
# [refresh] index-current -> index-2026-10-15
# [refresh] restart the service with: crabfinder-service --index ./index-current
```

Le service en cours continue de servir l'ancien index jusqu'à ce que tu le redémarres sur `index-current`. Options (variables d'environnement) : `DATA_DIR=/srv/crab` pour travailler ailleurs qu'à la racine du dépôt, `SKIP_DOWNLOAD=1` pour réutiliser le `best-<date>/` du jour, `SKIP_VALIDATE=1` pour sauter l'étape 3, `BEST_URL=…` pour une autre URL de téléchargement. Les anciens dossiers `best-*` / `index-*` ne sont pas supprimés : fais-le toi-même une fois le nouvel index en service.

## Désinstallation

Pas de fichiers en-dehors du dépôt. Pour tout retirer :

```bash
rm -rf crabfinder/    # le dépôt cloné
# Optionnel : désinstaller Rust si plus utilisé
rustup self uninstall
```

## Et après ?

- Géocoder un CSV → [`03-batch.md`](03-batch.md).
- Interroger le service en HTTP → [`04-recherche-en-ligne.md`](04-recherche-en-ligne.md).
- Un message d'erreur au démarrage ? → [`06-faq.md`](06-faq.md).
