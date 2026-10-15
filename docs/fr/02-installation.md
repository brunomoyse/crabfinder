# 2. Installation

À faire une fois, puis seulement quand BeSt publie un nouveau dump (l'index est figé sur le dump utilisé à la construction).

## Pré-requis matériels

- **Rust 1.90 ou plus récent** (installé via `rustup`, voir plus bas).
- **~3 Go d'espace disque libre** pour l'index Tantivy (~2 Go) + side-tables (~50 Mo) + marge.
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
    --notability-whitelist data/notability-whitelist-v1.yaml
```

Sortie typique :

```
INFO crabfinder-indexer starting; output="./index-belgium"
INFO regions selected; regions=[Brussels, Flanders, Wallonia]
INFO === ingesting region === Brussels
...
====== indexing summary ======
   Brussels: pass1=  450ms  pass2=  18430ms  parsed=234567 emitted=233834 ...
   Flanders: pass1=  890ms  pass2=  85420ms  ...
   Wallonia: pass1=  720ms  pass2=  42100ms  ...
      TOTAL: parsed=6864321 emitted=6841382 ...
   commit: 14820ms
   wall-clock total: 162145ms (2.7 min)
   index size on disk: 2123456789 bytes (2024.6 MB / 1.98 GB)
```

Le dossier `./index-belgium/` contient maintenant :

- Les fichiers du segment tree Tantivy (`*.idx`, `*.pos`, `*.term`, etc.).
- Un sidecar `.crab-meta.json` qui identifie la version du schéma.
- Un fichier `mid-street-v1.bin` (~300-500 Ko) utilisé par le fallback médian batch.

**Le dump BeSt utilisé est figé dans l'index.** Quand un nouveau dump est publié, tu refais l'étape 4 (remplacer les ZIP) puis l'étape 5 (reconstruire). Ce n'est pas une migration incrémentale.

### Options utiles

| Flag | Quand l'utiliser |
|---|---|
| `--region brussels` (ou `flanders` / `wallonia`) | Tests, ou besoin régional seulement |
| `--limit 1000` | Smoke test rapide (1000 adresses par région) |
| `--print-samples 5` | Afficher 5 adresses parsées avant indexation pour valider visuellement |

Sans `--notability-whitelist`, tout marche pareil mais les toponymes iconiques (Grand-Place, Atomium) ne sont plus boostés au classement. Le fichier est livré dans `data/`, donc autant le garder.

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
INFO starting crabfinder-service; index="./index-belgium" port=8080
INFO index sidecar OK; schema_version=7 tokenizer="crab"
INFO spatial index built; points=6841382 elapsed_ms=3420
INFO address-count index built; ...
INFO listening at http://127.0.0.1:8080
```

Test rapide depuis un autre terminal :

```bash
curl "http://127.0.0.1:8080/health"
# {"status":"ok","index_path":"./index-belgium","index_docs":6841382,"uptime_secs":12}

curl "http://127.0.0.1:8080/geocode?q=Grand-Place+1000&limit=2"
```

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

Les flags `--index`, `--host`, `--port`, `--abbreviation-dict` se passent aussi via variables d'environnement, pratique pour un déploiement systemd ou Docker :

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

Dans l'image, le service écoute sur `0.0.0.0` (`CRABFINDER_HOST`) et le dictionnaire d'abréviations est préconfiguré.

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
