# 2. Installation

À faire une fois, puis seulement quand BeSt publie un nouveau dump (l'index est figé sur le dump utilisé à la construction). Pour les mises à jour, `scripts/refresh_best.sh` automatise les étapes 4 et 5, voir [Mettre à jour les données](#mettre-à-jour-les-données).

## Pré-requis matériels

- **Rust 1.90 ou plus récent** (installé via `rustup`, voir plus bas).
- **~3 Go d'espace disque libre** pour l'index Tantivy (~2,2 Go) + fichiers annexes (~13 Mo) + marge. Une mise à jour garde l'ancien index à côté du nouveau jusqu'à ce que tu le supprimes : compte ~5 Go si tu utilises le script de mise à jour.
- **~4 Go de RAM libre** pendant l'indexation (essentiellement pour la jointure des dictionnaires en mémoire). Le service tournant en régime stable consomme moins.
- Un système **Linux, macOS ou Windows** (Linux et macOS sont les chemins testés ; sur Windows, utiliser WSL2 simplifie la vie).

## Étape 1: Installer Rust

Si `cargo --version` ne renvoie rien :

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
# Suivre les instructions par défaut, puis recharger le shell :
source "$HOME/.cargo/env"
cargo --version    # doit afficher "cargo 1.90.x" ou plus récent
```

Sur Windows sans WSL : passer par `https://www.rust-lang.org/tools/install` puis redémarrer le terminal.

## Étape 2: Récupérer le code

```bash
git clone https://github.com/brunomoyse/crabfinder.git
cd crabfinder
```

(Si on t'a remis une archive `.tar.gz` au lieu d'un dépôt git, extraire et entrer dans le dossier décompressé.)

## Étape 3: Construire les binaires

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

## Étape 4: Télécharger les données BeSt Address

BeSt publie un bundle ZIP par région. Tu as besoin des trois pour couvrir la Belgique.

### Sources officielles

Page de téléchargement : [https://opendata.bosa.be/](https://opendata.bosa.be/), chercher « BeSt Address » dans le catalogue.

Trois archives à récupérer :
- `BrusselsAddress_*.zip`
- `FlandersAddress_*.zip`
- `WalloniaAddress_*.zip`

Plus, **dans la même page**, les archives sœurs :
- `Municipality_*.zip`
- `PostalInfo_*.zip`
- `StreetName_*.zip`
- `PartOfMunicipality_*.zip` (Wallonie uniquement : Bruxelles et la Flandre n'en publient pas)

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

## Étape 5: Construire l'index

```bash
./target/release/crabfinder-indexer \
    --best-dir ./best-full-latest \
    --output ./index-belgium \
    --notability-whitelist data/notability-whitelist-v1.yaml
```

Sortie typique (dump du 2026-10-15, portable Apple Silicon) :

```
====== indexing summary ======
  Brussels: pass1=   49ms  pass2=   9772ms  parsed=848825 emitted=844058 skipped_status=4767
  Flanders: pass1=  265ms  pass2=  52383ms  parsed=4218617 emitted=3933743 skipped_status=284874
  Wallonia: pass1=  192ms  pass2=  25818ms  parsed=2107430 emitted=2107430 skipped_status=0 ... missing_position=140263 ...
     TOTAL: parsed=7174872 emitted=6885231 skipped_status=289641 skipped_unresolved=0 missing_position=140263 ...
  mid_street entries: 145123
  imputed positions: same_number=44909 interpolated=87293 street=0 locality=8061 dropped=0
  history records: 289641
  localities: 3065 (OSM places loaded: 0)
  commit: 856ms
  wall-clock total: 93063ms (1.6 min)
  index size on disk: 2394290026 bytes (2283.4 MB / 2.23 GB)
```

Comment la lire :

- `skipped_status` : adresses retirées, réservées et rejetées, écartées de la recherche (les adresses en vigueur et en projet sont indexées). Elles sont gardées dans le fichier d'historique (`history records`) pour que les anciens identifiants restent retrouvables.
- `missing_position` / `imputed positions` : BeSt publie certaines adresses wallonnes (140 263 dans ce dump) sans position (Lambert 72 `0 0`, qui tomberait en France). L'indexeur leur attribue une position à partir de leurs voisines, et note comment dans `position_source`, voir [`05-comprendre-resultats.md`](05-comprendre-resultats.md#doù-viennent-les-coordonnées--position_source).
- `localities` : les noms de lieux que le service comprend dans une recherche de lieu : communes, sections de commune et zones postales BeSt (« Spontin », « Han-sur-Lesse »), plus les villages et hameaux OpenStreetMap si l'index est construit avec `--places` (`OSM places loaded` est alors non nul).

Le dossier `./index-belgium/` contient maintenant :

- Les fichiers de segments Tantivy (`*.idx`, `*.pos`, `*.term`, etc.).
- Un sidecar `.crab-meta.json` avec la version du schéma (actuellement **12** ; un index construit par une version plus ancienne doit être reconstruit) et la date de construction.
- `mid-street-v1.bin` (~10 Mo) : les points de rue utilisés par le repli `median_street`.
- `localities-v1.json` (~3 Mo) : les noms de lieux servant à comprendre les recherches de lieu.
- `history-v1.json.gz` (~10 Mo) : les adresses retirées, réservées et rejetées, pour que `/lookup` et la colonne batch `best_id` retrouvent encore les anciens identifiants.

**Le dump BeSt utilisé est figé dans l'index.** Quand un nouveau dump est publié, tu refais l'étape 4 (remplacer les ZIP) puis l'étape 5 (reconstruire). Ce n'est pas une migration incrémentale. L'indexeur refuse un dossier `--output` qui contient déjà un index (sinon il ajouterait une deuxième copie de chaque adresse) : supprime-le d'abord, ou passe `--overwrite`.

### Options utiles

| Flag | Quand l'utiliser |
|---|---|
| `--region brussels` (ou `flanders` / `wallonia`) | Tests, ou besoin régional seulement |
| `--limit 1000` | Smoke test rapide (1000 adresses par région) |
| `--print-samples 5` | Afficher 5 adresses parsées avant indexation pour valider visuellement |
| `--overwrite` | Remplacer un index déjà présent dans `--output` (vérifié seulement une fois toutes les entrées chargées : un mauvais paramètre ne te coûte jamais l'ancien index) |
| `--notability-whitelist CHEMIN` | Boost de classement des lieux emblématiques (voir plus bas) |
| `--places CHEMIN` | Aide OpenStreetMap facultative : noms de villages et de hameaux (voir plus bas) |

La whitelist est livrée dans `data/`, donc autant s'en servir : sans `--notability-whitelist`, tout marche pareil mais les toponymes iconiques (Grand-Place, Atomium…) ne sont plus boostés au classement.

### Aides OpenStreetMap (facultatives)

BeSt Address est la seule source de ce que renvoie crabfinder. Deux fichiers OpenStreetMap facultatifs, désactivés par défaut, l'aident seulement à *comprendre* des requêtes que BeSt seul ne sait pas situer :

| Aide | Flag | Permet de comprendre |
|---|---|---|
| `data/osm-places-be-v1.csv` (villages, hameaux) | indexeur `--places` | « Maredsous », un hameau sans entité BeSt propre (rattaché à Anhée) |
| `data/osm-pois-be-v1.csv` (points d'intérêt) | service / batch `--pois` (étape 6) | « Manneken Pis », « Gare du Midi », « Gravensteen Gent » |

Dans les deux cas, la réponse est l'adresse BeSt la plus proche (`confidence: locality` / `landmark`) ; aucun nom ni position OSM n'est jamais renvoyé, et aucune adresse n'est ajoutée ni modifiée. Sans elles, seules les communes, sections de commune et zones postales BeSt sont reconnues comme lieux.

Pour les lieux, ajoute `--places data/osm-places-be-v1.csv` à la commande d'indexation ci-dessus. Les points d'intérêt sont chargés par le service au démarrage : mettre ce fichier à jour ne demande jamais de réindexer. Les deux CSV sont dérivés d'OpenStreetMap (© les contributeurs d'OpenStreetMap, ODbL 1.0, voir `data/README.md`).

## Étape 6: Démarrer le service HTTP (pour le mode recherche)

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
INFO index sidecar OK schema_version=12 tokenizer=crab
INFO spatial index built points=6878576 elapsed_ms=1605
INFO abbreviation dictionary loaded path="data/street-type-abbreviations-v1.yaml" n_entries=17 ...
INFO address-count index built elapsed_ms=20048 n_docs=6885231 n_streets=144205 max_count=5649
INFO mid_street side-file loaded path=./index-belgium/mid-street-v1.bin entries=145123
INFO localities loaded entries=…
INFO listening at http://127.0.0.1:8080
```

Le service n'ouvre son port qu'une fois tout chargé (~20 s sur un portable) : la première requête voit donc toujours un index prêt. Ensuite, chaque requête est journalisée avec son statut et sa latence (`RUST_LOG=warn` pour les faire taire).

Test rapide depuis un autre terminal :

```bash
curl "http://127.0.0.1:8080/health"
# {"status":"ok","index_path":"./index-belgium","index_docs":6885231,"uptime_secs":12,
#  "schema_version":12,"index_built_at_unix_secs":1792106077,"version":"0.1.0"}

curl "http://127.0.0.1:8080/geocode?q=Grand-Place+1000&limit=2"
```

### Options du service

| Flag | Défaut | Effet |
|---|---|---|
| `--index CHEMIN` | n/a (obligatoire) | Dossier de l'index. |
| `--host IP` | `127.0.0.1` | Adresse d'écoute. `0.0.0.0` pour écouter sur toutes les interfaces (conteneurs). |
| `--port N` | `8080` | Port TCP. |
| `--abbreviation-dict CHEMIN` | (aucun) | Expansion `av` → `avenue`. **Recommandé** : `data/street-type-abbreviations-v1.yaml`. |
| `--pois CHEMIN` | (aucun) | Aide OpenStreetMap facultative (`data/osm-pois-be-v1.csv`) : comprendre les noms de points d'intérêt (« Manneken Pis », « Gare du Midi ») ; la réponse est l'adresse BeSt la plus proche, `confidence: landmark`. Sans lui, ces noms ne sont pas reconnus. |
| `--request-timeout-ms N` | `10000` | Délai maximal d'une requête, attente d'une place libre comprise. Au-delà, le service répond `503` avec une erreur JSON. |
| `--batch-timeout-secs N` | `600` | Idem pour `POST /batch/geocode`, jusqu'au début de la réponse. |
| `--max-concurrency N` | `256` | Requêtes traitées en même temps ; les autres attendent une place (dans leur délai). |
| `--cors-origin URL` | toutes (`*`) | Origine navigateur autorisée. Répéter le flag (ou séparer par des virgules) pour en mettre plusieurs. |
| `--no-metrics` | désactivé | Ne pas servir les métriques Prometheus sur `/metrics`. |
| `--no-notability-boost` | désactivé | Classer sans le boost de notoriété (tests A/B). |
| `--no-autocomplete` | désactivé | Ne pas construire l'index de suggestions de `/autocomplete` (environ 0,8 s de démarrage et 80 Mo en moins) ; la route répond alors `503`. |

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

Idem pour `crabfinder-batch` (`CRABFINDER_INDEX`, `CRABFINDER_ABBREVIATION_DICT`, et `CRABFINDER_POIS` pour l'aide facultative aux points d'intérêt).

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

Dans l'image, le service écoute sur `0.0.0.0` (`CRABFINDER_HOST`) et le dictionnaire d'abréviations est préconfiguré. Les aides OpenStreetMap facultatives ne sont pas incluses. Pour indexer les noms de hameaux, monter `data/` et ajouter `--places` :

```bash
docker run --rm -v ./best-full-latest:/best:ro -v ./data:/data:ro -v crabfinder-index:/index crabfinder \
    crabfinder-indexer --best-dir /best --output /index \
    --notability-whitelist /usr/share/crabfinder/notability-whitelist-v1.yaml \
    --places /data/osm-places-be-v1.csv
```

De même pour l'aide facultative aux points d'intérêt, que le service charge au démarrage : monter `data/` sur le conteneur qui sert et passer `--pois` :

```bash
docker run -d --name crabfinder -p 8080:8080 -v crabfinder-index:/index -v ./data:/data:ro crabfinder \
    crabfinder-service --pois /data/osm-pois-be-v1.csv
```

## Mettre à jour les données

`scripts/refresh_best.sh` fait une mise à jour complète d'une traite :

1. télécharge le bundle BeSt (`best-full-latest.zip` sur opendata.bosa.be) dans `best-<date>/` et le décompresse ;
2. compile les binaires release et indexe dans `index-<date>/` (avec la whitelist ; BeSt seul, sauf lancé avec `WITH_OSM=1`, qui ajoute `--places data/osm-places-be-v1.csv`) ;
3. démarre un service temporaire sur le port 18080 et lance `crabfinder-eval --validate-eval-set` : chaque adresse attendue par le jeu d'évaluation doit toujours exister (le script s'arrête si plus de 5 % manquent) ;
4. fait pointer le lien symbolique `index-current` vers le nouvel index, de façon atomique.

```bash
scripts/refresh_best.sh
# [refresh] index-current -> index-2026-10-15
# [refresh] restart the service with: crabfinder-service --index ./index-current
```

Le service en cours continue de servir l'ancien index jusqu'à ce que tu le redémarres sur `index-current`. Options (variables d'environnement) : `DATA_DIR=/srv/crab` pour travailler ailleurs qu'à la racine du dépôt, `SKIP_DOWNLOAD=1` pour réutiliser le `best-<date>/` du jour, `SKIP_VALIDATE=1` pour sauter l'étape 3, `WITH_OSM=1` pour indexer les lieux OSM facultatifs, `BEST_URL=…` pour une autre URL de téléchargement, `STAMP=2026-10-15` pour nommer les dossiers autrement que par la date du jour (un `index-<stamp>/` existant est supprimé puis reconstruit), `VALIDATE_PORT=N` pour changer le port 18080 du service de validation, `EVAL_SETS="a.yaml b.json"` pour valider d'autres jeux, `RESTART_CMD="…"` pour redémarrer le service après la bascule. Les anciens dossiers `best-*` / `index-*` ne sont pas supprimés : fais-le toi-même une fois le nouvel index en service.

### Planifier la mise à jour

BeSt est republié régulièrement ; une exécution hebdomadaire est un rythme raisonnable. Le script se termine en erreur (et laisse `index-current` intact) si le téléchargement, la construction ou la validation échoue : un planificateur peut donc alerter en cas d'échec. Avec cron :

```cron
# Chaque lundi à 03:00 ; le service redémarre sur le nouvel index si tout s'est bien passé.
0 3 * * 1  cd /srv/crabfinder && DATA_DIR=/srv/crab RESTART_CMD="systemctl restart crabfinder" scripts/refresh_best.sh >> /var/log/crabfinder-refresh.log 2>&1
```

Avec systemd, un service oneshot et un timer :

```ini
# /etc/systemd/system/crabfinder-refresh.service
[Service]
Type=oneshot
WorkingDirectory=/srv/crabfinder
Environment=DATA_DIR=/srv/crab
Environment="RESTART_CMD=systemctl restart crabfinder"
ExecStart=/srv/crabfinder/scripts/refresh_best.sh

# /etc/systemd/system/crabfinder-refresh.timer
[Timer]
OnCalendar=Mon *-*-* 03:00
Persistent=true
[Install]
WantedBy=timers.target
```

Chaque exécution demande ~3 Go libres pour les nouveaux `best-<date>/` et `index-<date>/` ; supprime les anciens (par exemple en gardant les deux derniers) dans la même tâche.

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
