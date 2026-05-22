# 6. FAQ et dépannage

Réponses aux questions et erreurs les plus fréquentes.

## Erreurs au démarrage du service

### « index at … has schema_version=X, but this binary expects schema_version=Y »

Tu as construit l'index avec une version antérieure du binaire `crabfinder-indexer`, ou tu as mis à jour le code sans reconstruire l'index. Solution : reconstruire l'index avec le binaire courant.

```bash
./target/release/crabfinder-indexer \
    --best-dir ./best-full-latest \
    --output ./index-belgium \
    --notability-whitelist data/notability-whitelist-v1.yaml
```

L'opération écrase l'index existant. Compter ~2-3 min sur un dump complet.

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

### « Avenu Louise » (faute de frappe) ne trouve rien

crabfinder a un fallback fuzzy mais il n'est pas omnipotent. La passe stricte exige que **chaque token** de la requête matche au moins un token indexé. Si la faute est sur un token court (« Avenu » au lieu de « Avenue »), l'analyzer ne les rapproche pas — pas de stemming.

Solutions :
- Côté requête : retirer le token fautif, laisser la passe stricte trouver via les autres (`Louise 32 1050` matche).
- Vérifier l'orthographe en amont si tu géocodes en batch.
- Le score `fuzzy_street` quand la passe fuzzy retombe sur une rue marche bien sur des fautes mineures (« Etterbeek » vs « Étterbeek », gérée par l'AsciiFoldingFilter).

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
- Requête ambiguë (« Rue de la Gare ») → préciser CP ou région.
- Toponyme local non boosté en notabilité (Whitelist ne couvre pas tout). Tu peux ajouter une entrée dans `data/notability-whitelist-v1.yaml` puis ré-indexer.
- Faute de frappe : la passe fuzzy peut remonter un mauvais candidat.

Inspecter `parsed.street_tokens`, `parsed.house_number`, `parsed.postal_code` dans la réponse `/geocode` : si le parser a mal coupé la requête, le reste suit. Augmenter `limit` pour voir si la bonne réponse est dans le top-3 ou top-5.

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

Tu as beaucoup de lignes sans `house_number`, c'est attendu. Sur l'eval Tier A, ~38% des lignes basculent en `median_street`. Si c'est un comportement non voulu (tes données ont des numéros que crabfinder ne reconnaît pas), inspecter quelques lignes :

```bash
awk -F, '$11=="house_number_out_of_range" {print}' geocodees.csv | head -20
```

Cas typiques : numéros avec lettres de boîte intégrées (« 32 bte 1 ») non séparés, suffixes inhabituels, numéros à plus de 4 chiffres.

### Désactiver le fallback médian

Si tu préfères que les lignes sans numéro exact ressortent `not_found` plutôt que `median_street` :

```bash
# CLI
./target/release/crabfinder-batch ... --no-mid-street ...

# HTTP
curl -X POST .../batch/geocode -H "Content-Type: application/json" \
     -d '{"addresses":[...],"options":{"mid_street_fallback":false}}'
```

## Multi-process et partage d'index

### Puis-je faire tourner le service et la CLI en même temps sur le même index ?

**Oui.** Tantivy utilise `MmapDirectory` en lecture, qui supporte le partage entre processus. Tu peux avoir `crabfinder-service` en background qui sert `/geocode` interactif *et* `crabfinder-batch` lancé à côté qui traite un CSV de 100k lignes, tous les deux sur `./index-belgium`. Aucun lock à gérer.

### Puis-je faire tourner plusieurs instances de `crabfinder-batch` en parallèle ?

Oui, même raison. Utile si tu veux paralléliser sur plusieurs machines partageant un FS commun (NFS).

### Puis-je ré-indexer pendant que le service tourne ?

**Non, à éviter.** L'indexer ouvre l'index en écriture exclusive ; un service qui lit en parallèle peut servir des résultats incohérents le temps que les nouveaux segments soient committés. Workflow propre :

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

Pas de notification automatique. BeSt publie sur [opendata.bosa.be](https://opendata.bosa.be/) à un rythme typiquement mensuel. Le sidecar `.crab-meta.json` de ton index contient `built_at_unix_secs` qui te dit quand tu as indexé pour la dernière fois.

### Comment je sais quel dump BeSt est dans mon index ?

Le sidecar contient un timestamp d'**indexation**, pas la date du dump source. Si tu as besoin de tracer ça, garde une convention de nommage des archives BeSt et/ou note la date dans ta procédure de refresh.

### Mise à jour incrémentale ?

Non. crabfinder reconstruit l'index intégralement à chaque refresh. ~2-3 min sur dump complet. Pas de delta.

## Mémoire et empreinte disque

### Combien de RAM consomme le service en régime ?

Au démarrage : pic ~3-4 Go (construction du R-tree spatial + adresses count). En régime : ~500 Mo à 1 Go (mmap de l'index, ne compte pas vraiment comme « consommée » — c'est du cache OS). Le R-tree à 6,8M points ajoute ~400 Mo de heap effectif.

### Combien de RAM consomme la CLI batch ?

Variable selon `--parallel N` et la taille du CSV chargé en RAM. À 8 cœurs et 100k lignes : pic ~500 Mo. À 1M de lignes : pic ~2-3 Go. Si la RAM est tendue, utiliser `--parallel 4` ou `--parallel 2`.

### Combien de disque ?

Index Tantivy : ~2 Go pour la Belgique complète. Side-tables (mid_street) : < 1 Mo. Sidecar : quelques octets. Au total : compter 2,5 Go généreusement.

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

## Tu n'as pas trouvé ta réponse

Vérifie :
- Que tu as bien lu [`02-installation.md`](02-installation.md) si c'est un souci de setup.
- Que ton index est en `schema_version=7` (`cat ./index-belgium/.crab-meta.json`).
- Que les binaires sont en release (`./target/release/...`, pas `./target/debug/...`).

Ensuite, capturer :
- La version (`./target/release/crabfinder-service --version`).
- La commande exacte lancée.
- Le message d'erreur complet.
- Le contenu du sidecar (`cat ./index-belgium/.crab-meta.json`).

Et reporter sur le dépôt ou demander à un développeur de l'équipe.
