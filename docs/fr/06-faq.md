# 6. FAQ et dépannage

Réponses aux questions et erreurs les plus fréquentes.

## Erreurs au démarrage du service

### « index at … has schema_version=X, but this binary expects schema_version=Y »

Tu as construit l'index avec une version antérieure du binaire `crabfinder-indexer`, ou tu as mis à jour le code sans reconstruire l'index. Solution : reconstruire l'index avec le binaire courant.

```bash
./target/release/crabfinder-indexer \
    --best-dir ./best-full-latest \
    --output ./index-belgium \
    --notability-whitelist data/notability-whitelist-v1.yaml \
    --places data/osm-places-be-v1.csv \
    --overwrite
```

`--overwrite` supprime l'ancien index de `--output` avant la construction (sans lui, l'indexeur refuse un dossier qui en contient déjà un). Compter ~1,5-3 min sur un dump complet. Ou lance `scripts/refresh_best.sh`, qui télécharge aussi un dump à jour (voir [`02-installation.md`](02-installation.md#mettre-à-jour-les-données)).

### « index at … is missing .crab-meta.json »

Le dossier ne contient pas de sidecar de version. Soit l'indexation a échoué avant la fin, soit ce n'est pas un dossier d'index crabfinder. Vérifie qu'il contient des fichiers Tantivy (`*.idx`, `*.term`…) et un `.crab-meta.json`. Sinon, refais l'étape 5 de l'installation.

### « opening Tantivy index at ./index-belgium: No such file or directory »

Le chemin passé à `--index` n'existe pas. `ls ./index-belgium` doit lister des fichiers. Si c'est un chemin relatif, vérifie que tu lances la commande depuis la racine du dépôt.

### « Address already in use (os error 48) » au démarrage

Le port (8080 par défaut) est déjà pris par un autre processus. Soit un autre `crabfinder-service` tourne déjà :

```bash
pgrep -fa crabfinder-service
# tuer si besoin :
kill <PID>
```

Soit un autre service occupe le port :

```bash
lsof -i :8080
```

Changer de port via `--port 8081` ou `CRABFINDER_PORT=8081`.

### « connect: connection refused » sur localhost mais pas sur 127.0.0.1

Le service écoute en IPv4 seulement. Sur certains systèmes, `localhost` résout en IPv6 (`::1`) d'abord. Utilise toujours `127.0.0.1` dans tes curls.

## Erreurs côté indexation

### « … already holds an index; pass --overwrite to replace it »

Le dossier `--output` contient déjà un index. L'indexeur refuse d'écrire dedans : indexer par-dessus un index existant stockerait chaque adresse en double. Soit tu pointes `--output` vers un nouveau dossier (ce que fait `scripts/refresh_best.sh`, avec des dossiers datés `index-<date>`), soit tu ajoutes `--overwrite` pour supprimer d'abord l'ancien index. N'utilise pas `--overwrite` sur le dossier que lit un service en marche : construis ailleurs puis bascule (voir [plus bas](#puis-je-ré-indexer-pendant-que-le-service-tourne-)).

### « no ZIP found for region Brussels (Entity::Address) in ./best-full-latest »

Une archive BeSt attendue est absente. Refaire l'étape 4 de l'installation et vérifier que les ~10 archives sont bien dans `--best-dir` (Address × 3 régions, Municipality × 3, PostalInfo × 3, StreetName × 3, PartOfMunicipality pour la Wallonie). Ne pas décompresser : l'indexeur lit les ZIP directement.

### « status-skip share drifted from the file 11 §9.3 baseline »

Avertissement informatif uniquement, **pas une erreur**. crabfinder a détecté que la proportion d'adresses rejetées (statut non-current) dans le dump BeSt courant diffère des baselines de référence. Ça arrive quand BeSt fait évoluer sa politique de statut entre deux dumps. L'indexation se termine normalement. À investiguer si la dérive est > 2 points (typiquement une décision côté BeSt qu'il faut acter en mettant à jour la baseline).

### L'indexation prend beaucoup plus que 2-3 minutes

Possibles raisons :
- **Tu n'as pas compilé en `--release`**. Le binaire debug est 10× plus lent. `cargo build --release` puis `./target/release/crabfinder-indexer`.
- **Disque saturé ou lent**. Les écritures Tantivy sont I/O intensives. Sur disque dur classique ou clé USB, prévoir 10-15 min.
- **Peu de RAM**. La pass 1 charge les dictionnaires en mémoire (~1 Go). Sur une VM 2 Go, ça swap et ralentit énormément.

## Recherche : résultats étranges

### Une rue mal orthographiée n'est pas trouvée

Les fautes de frappe sont corrigées : un mot de rue qui n'existe pas dans l'index est rapproché du mot existant le plus proche, à 1 lettre près pour les mots de 5 à 8 lettres et 2 lettres pour les plus longs. « Avenu Louise 32 1050 », « Avenue Loiuse 32 1050 » et « Rue Beliard 71 1040 » reviennent tous justes, en `fuzzy_street` / `match_type=typo`. Les accents et la casse ne comptent jamais comme des fautes.

Ce qui n'est pas corrigé :
- Une faute dans un mot de 4 lettres ou moins (« Rua ») : les mots courts doivent être bien écrits. Les autres mots peuvent quand même trouver la rue (« Rua de la Loi 16 1000 » revient juste, mais en `match_type=fuzzy`, moins fiable).
- Plus de 2 lettres fausses dans un mot, ou deux mots collés (« AvenueLouise 32 1050 » ne trouve rien).

Solutions : retirer le mot cassé et laisser les autres faire le travail (`Louise 32 1050` matche), ou vérifier l'orthographe en amont si tu géocodes en batch.

### Les recherches « Avenue Louise » sans CP retournent du bruit

Sans `postal_code`, crabfinder ne peut pas désambiguïser les rues homonymes. « Avenue Louise » existe à Bruxelles, mais aussi à Mons, Charleroi, Tournai, etc. Le tri se fait par BM25 + notabilité, donc l'Avenue Louise bruxelloise remonte typiquement en premier (boost notabilité), mais c'est une heuristique, pas une garantie.

Solutions :
- Toujours fournir `postal_code` quand tu l'as.
- Utiliser `region=brussels` quand le contexte le permet.
- Utiliser `focus_point=lat,lng` pour favoriser une zone géographique.

### Les abréviations « Av. Louise » ne sont pas reconnues

Tu as oublié le flag `--abbreviation-dict` au démarrage du service ou de la CLI. Sans ce flag, les abréviations passent telles quelles à l'index et ne matchent pas la forme canonique (« avenue »).

Solution :

```bash
./target/release/crabfinder-service \
    --index ./index-belgium \
    --abbreviation-dict data/street-type-abbreviations-v1.yaml \
    ...
```

(Idem pour `crabfinder-batch`.)

### Le top-1 n'est pas la bonne réponse

C'est rare mais ça arrive. Causes typiques :
- Requête ambiguë (« Kerkstraat 1 » existe dans des dizaines de communes) → la réponse indique `ambiguous: true` ; préciser CP, `region` ou `focus_point`.
- Toponyme local non boosté en notabilité (Whitelist ne couvre pas tout). Tu peux ajouter une entrée dans `data/notability-whitelist-v1.yaml` puis ré-indexer.
- Faute de frappe : la passe fuzzy peut remonter un mauvais candidat.
- Un nom de rue court qui fait aussi partie de noms plus longs : la rue qui s'appelle exactement comme ta saisie passe en premier. « Markt » donne le Markt de Bruges, pas l'un des nombreux « Grote Markt » ; « Grand-Place » donne Bruxelles. Si tu visais un nom plus long, tape-le en entier ou ajoute le code postal.

Inspecter `query.parsed` (`street_tokens`, `house_number`, `box_number`, `postal_code`) et `query.reading` dans la réponse `/geocode` : si le parser a mal coupé la requête, le reste suit. Augmenter `limit` pour voir si la bonne réponse est dans le top-3 ou top-5.

## Batch : performance et qualité

### Combien de lignes par seconde puis-je traiter ?

Ordre de grandeur sur laptop dev (M1 8-cœurs ou x86_64 8-cœurs, SSD) :
- CLI in-process : ~1500 lignes/s par cœur, donc ~10k-12k lignes/s à 8 cœurs.
- HTTP : ~5-6k lignes/s (sérialisation + déserialisation HTTP en plus).

Pour 100k lignes : ~10 s en CLI, ~20 s en HTTP. Pour 1M de lignes : ~2-3 min en CLI.

### Mon taux `not_found` est élevé

Vérifier dans l'ordre :
1. **Encodage** : un CSV en `windows-1252` indexé comme UTF-8 produit des accents corrompus → la passe stricte rate, le fuzzy aussi. Convertir avec `iconv`.
2. **Délimiteur** : si la sniffing s'est trompée (rare), force avec `--delimiter ';'`.
3. **Code postal manquant** : sans CP, beaucoup de rues homonymes sortent dans un ordre indéterminé, et certaines lignes peuvent tomber sur `not_found` si BM25 ne ramène rien d'évident.
4. **Adresses hors Belgique** : crabfinder ne couvre que la Belgique. Les adresses étrangères sortent en `not_found`.

### `median_street` est très fréquent dans mon résultat

Tu as beaucoup de lignes sans `house_number`, c'est attendu. Si c'est un comportement non voulu (tes données ont des numéros que crabfinder ne reconnaît pas), inspecter quelques lignes :

```bash
grep ',house_number_out_of_range,' geocodees.csv | head -20
```

Un numéro qui n'existe pas revient en général en `nearest_same_side` / `nearest_any_side` (un numéro voisin) ; `median_street` avec `house_number_out_of_range` signifie qu'aucun numéro existant n'était assez proche (`Avenue Louise 9999`), ou que la rue n'a pas d'adresse numérotée à ce code postal. Les boîtes écrites dans le numéro (`32 bte 1`, `32/1`) ne posent pas de problème : elles sont séparées automatiquement.

### Désactiver les replis au niveau de la rue

Si tu préfères que les lignes sans numéro utilisable ressortent `not_found` plutôt que `median_street` (ou un `fuzzy_street` au niveau de la rue) :

```bash
# CLI
./target/release/crabfinder-batch ... --no-mid-street ...

# HTTP
curl -X POST .../batch/geocode -H "Content-Type: application/json" \
     -d '{"addresses":[...],"options":{"mid_street_fallback":false}}'
```

Les résultats au numéro voisin (`nearest_*`) sont conservés : ce sont de vraies adresses voisines, pas des approximations à la rue.

### Mes codes postaux sont parfois faux

Fréquent dans les vrais fichiers : l'adresse est pile sur une limite de commune, ou quelqu'un a tapé le code postal de la commune principale (1000 au lieu de 1050). Quand rien ne correspond dans le code postal donné, crabfinder cherche la même rue dans les communes autour (dans un rayon de ~20 km) et la renvoie avec `confidence=adjacent_commune` et `fallback_reason=postal_code_mismatch` :

```
1,Avenue Louise,54,1000,Avenue Louise,54,1050,50.83340378514029,4.358146544863564,adjacent_commune,postal_code_mismatch,0
```

Le code postal réellement trouvé est dans `matched_postal_code` ; compare-le à ton entrée pour repérer les lignes à corriger à la source. Un match exact dans le code postal donné l'emporte toujours sur la commune voisine. Si tu ne veux jamais sortir de ton code postal (`/geocode` uniquement), passe-le en filtre `postal_code=` plutôt que dans la requête : ce filtre est strict et jamais relâché. Détails dans [`05-comprendre-resultats.md`](05-comprendre-resultats.md#adjacent_commune--trouvé-à-côté).

## Multi-process et partage d'index

### Puis-je faire tourner le service et la CLI en même temps sur le même index ?

**Oui.** Tantivy utilise `MmapDirectory` en lecture, qui supporte le partage entre processus. Tu peux avoir `crabfinder-service` en background qui sert `/geocode` interactif *et* `crabfinder-batch` lancé à côté qui traite un CSV de 100k lignes, tous les deux sur `./index-belgium`. Aucun lock à gérer.

### Puis-je faire tourner plusieurs instances de `crabfinder-batch` en parallèle ?

Oui, même raison. Utile si tu veux paralléliser sur plusieurs machines partageant un FS commun (NFS).

### Puis-je ré-indexer pendant que le service tourne ?

**Non, à éviter.** L'indexer ouvre l'index en écriture exclusive ; un service qui lit en parallèle peut servir des résultats incohérents le temps que les nouveaux segments soient committés. Construis le nouvel index dans un autre dossier : `scripts/refresh_best.sh` fait exactement ça (`index-<date>/`, puis bascule du lien `index-current` ; redémarre ensuite le service sur `index-current`). À la main :

```bash
# 1. Construire le nouvel index dans un autre dossier
./target/release/crabfinder-indexer \
    --best-dir ./best-full-latest \
    --output ./index-belgium-new \
    --notability-whitelist data/notability-whitelist-v1.yaml

# 2. Arrêter le service, basculer, redémarrer
kill $(cat crabfinder.pid)
mv ./index-belgium ./index-belgium-old
mv ./index-belgium-new ./index-belgium
./target/release/crabfinder-service ... &

# 3. Optionnel : supprimer l'ancien
rm -rf ./index-belgium-old
```

## Mise à jour des données

### Comment je sais si un nouveau dump BeSt est disponible ?

Pas de notification automatique. BeSt publie sur [opendata.bosa.be](https://opendata.bosa.be/) à un rythme typiquement mensuel. Le sidecar `.crab-meta.json` de ton index contient `built_at_unix_secs` qui te dit quand tu as indexé pour la dernière fois ; un service en marche l'affiche dans `/health` (`index_built_at_unix_secs`). Un cron mensuel de `scripts/refresh_best.sh` te garde à jour.

### Comment je sais quel dump BeSt est dans mon index ?

Le sidecar contient un timestamp d'**indexation**, pas la date du dump source. Le script de mise à jour nomme les dossiers d'après la date de téléchargement (`best-2026-10-15/`, `index-2026-10-15/`), et `readlink index-current` te dit lequel est en service.

### Mise à jour incrémentale ?

Non. crabfinder reconstruit l'index intégralement à chaque refresh. ~1,5-3 min sur dump complet. Pas de delta.

## Mémoire et empreinte disque

### Combien de RAM consomme le service en régime ?

Au démarrage : pic ~3-4 Go (construction du R-tree spatial + adresses count). En régime : ~500 Mo à 1 Go (mmap de l'index, ne compte pas vraiment comme « consommée » — c'est du cache OS). Le R-tree à 6,9M points ajoute ~400 Mo de heap effectif.

### Combien de RAM consomme la CLI batch ?

Variable selon `--parallel N` et la taille du CSV chargé en RAM. À 8 cœurs et 100k lignes : pic ~500 Mo. À 1M de lignes : pic ~2-3 Go. Si la RAM est tendue, utiliser `--parallel 4` ou `--parallel 2`.

### Combien de disque ?

Index Tantivy : ~2,2 Go pour la Belgique complète. Fichiers annexes (`mid-street-v1.bin`, `localities-v1.json`) : ~13 Mo. Sidecar : quelques octets. Au total : compter 2,5 Go généreusement, le double pendant une mise à jour (ancien et nouvel index côte à côte).

## Logs et observabilité

### Activer des logs détaillés

Les logs sont contrôlés via `RUST_LOG`. Niveaux : `error`, `warn`, `info` (défaut), `debug`, `trace`.

```bash
RUST_LOG=debug ./target/release/crabfinder-service ...
RUST_LOG=crabfinder_service=debug,tantivy=warn ./target/release/crabfinder-service ...
```

Pour la CLI batch :

```bash
RUST_LOG=debug ./target/release/crabfinder-batch ... 2> batch.log
```

(La CLI envoie ses logs sur `stderr` pour ne pas polluer la sortie CSV.)

### Format JSON pour ingestion centralisée

Le service utilise `tracing-subscriber` avec un format text par défaut. Pour activer le format JSON :

```bash
RUST_LOG=info ./target/release/crabfinder-service ... 2>&1 \
  | jq -R '. as $line | try fromjson catch {raw: $line}'
```

(Pas de flag explicite pour le moment ; le format JSON peut être ajouté via une variable d'env si besoin — ouvrir une issue.)

### Monitoring

Chaque requête est journalisée au niveau `info` avec son statut et sa latence. Pour des tableaux de bord et des alertes, scrape `GET /metrics` (format Prometheus) : nombre de requêtes et latence par route, `503`, et le résultat de chaque recherche par `confidence` — voir [`04-recherche-en-ligne.md`](04-recherche-en-ligne.md#métriques). `--no-metrics` désactive l'endpoint.

### Le service répond `503`

Une requête a attendu plus que `--request-timeout-ms` (10 s par défaut), attente d'une place comprise : le service est surchargé (plus de `--max-concurrency` requêtes simultanées, 256 par défaut) ou la machine swappe. Augmente les limites, ajoute de la RAM, ou répartis la charge (plusieurs services peuvent partager un même index).

## Tu n'as pas trouvé ta réponse

Vérifie :
- Que tu as bien lu [`02-installation.md`](02-installation.md) si c'est un souci de setup.
- Que ton index est en `schema_version=10` (`cat ./index-belgium/.crab-meta.json`, ou `/health`).
- Que les binaires sont en release (`./target/release/...`, pas `./target/debug/...`).

Ensuite, capturer :
- La version (`./target/release/crabfinder-service --version`).
- La commande exacte lancée.
- Le message d'erreur complet.
- Le contenu du sidecar (`cat ./index-belgium/.crab-meta.json`).

Et reporter sur le dépôt ou demander à un développeur de l'équipe.
