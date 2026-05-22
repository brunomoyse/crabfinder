# 3. Géocodage batch

Cas d'usage : tu as une liste d'adresses (CSV, export Excel, requête SQL) et tu veux en sortir un fichier enrichi avec lat / lng et une indication de qualité par ligne.

Deux façons d'attaquer ce besoin, **avec exactement la même logique de résolution derrière** :

- **CLI `crabfinder-batch`** — pas de serveur, pas de port, tu lances un binaire, tu lui donnes un CSV, tu récupères un CSV. Privilégié pour les pipelines de data, les scripts cron, les exports ponctuels.
- **Endpoint HTTP `POST /batch/geocode`** — le service tourne déjà, tu pousses un CSV par requête HTTP, tu reçois un CSV streamé. Privilégié pour les intégrations applicatives (un backend qui géocode à la demande).

Si tu hésites : la CLI est plus simple à démarrer. Va à la section [CLI](#cli--crabfinder-batch).

---

## Format d'entrée

**Un seul format supporté en v1** : CSV avec colonnes nommées et séparées. Le header est obligatoire.

```csv
id,street,house_number,postal_code
1,Avenue Louise,32,1050
2,Rue Belliard,71,1040
3,Grand-Place,,1000
4,Stationsstraat,5,2000
5,Rue Inexistante du Test,99,1000
```

Règles :

| Colonne | Type | Obligatoire | Notes |
|---|---|---|---|
| `id` | chaîne | oui | Identifiant libre. Recopié tel quel en sortie pour permettre la jointure avec ta source de données. |
| `street` | chaîne | oui | Nom de rue. Peut contenir des accents, des tirets, plusieurs mots. Pas besoin de normaliser. |
| `house_number` | chaîne | non | Numéro de bâtiment. Vide accepté. Les suffixes alphabétiques (`32A`, `12B`) sont conservés. |
| `postal_code` | chaîne | non | Code postal belge à 4 chiffres. Vide accepté (mais fortement déconseillé : sans CP, les rues homonymes ne sont pas désambiguïsées). |

**Encodage** : UTF-8. Si ton CSV est en `windows-1252` (export Excel français/néerlandais), convertis-le d'abord :

```bash
iconv -f WINDOWS-1252 -t UTF-8 adresses-export.csv > adresses.csv
```

**Délimiteur** : virgule (`,`) ou point-virgule (`;`). Détection automatique sur la première ligne ; en cas de doute, force avec `--delimiter ';'` côté CLI.

**Lignes vides** : ignorées. **Lignes mal formées** : la CLI s'arrête avec un message d'erreur indiquant le numéro de ligne. L'endpoint HTTP renvoie un 400 avec le même message.

## Format de sortie

15 colonnes, dans cet ordre (toujours) :

```csv
id,street,house_number,postal_code,matched_street,matched_house_number,matched_postal_code,lat,lng,confidence,fallback_reason,house_number_delta,street_match_score,building_group_id,best_id
```

| Colonne | Sémantique |
|---|---|
| `id`, `street`, `house_number`, `postal_code` | **Recopiés de l'entrée**, audit trail. |
| `matched_street`, `matched_house_number`, `matched_postal_code` | Ce que crabfinder a effectivement trouvé dans BeSt. Permet de voir les divergences (fuzzy_street, etc.). |
| `lat`, `lng` | WGS84 (EPSG:4326). Vides en cas de `not_found`. |
| `confidence` | Une des sept étiquettes : `exact`, `median_street`, `fuzzy_street`, `nearest_same_side`, `nearest_any_side`, `adjacent_commune`, `not_found`. Détail dans [`05-comprendre-resultats.md`](05-comprendre-resultats.md). |
| `fallback_reason` | Vide si `confidence == exact`. Sinon une étiquette parmi `house_number_missing`, `house_number_out_of_range`, `street_spelling_fuzzy`, `no_match`. |
| `house_number_delta` | Vide pour `exact` (delta = 0 implicite) et pour les confiances rue-only. Rempli quand un numéro voisin a été retenu. |
| `street_match_score` | Score BM25 brut. Indication de qualité du match texte. Plus c'est élevé, plus la rue trouvée est probante. Vide si `not_found`. |
| `building_group_id` | Identifiant synthétique du bâtiment dans crabfinder. Stable d'un dump à l'autre tant que BeSt ne change pas le `street_id`. |
| `best_id` | Identifiant officiel BeSt de l'adresse retenue (`namespace/object_identifier/version`). Permet de remonter à la source authoritative. |

**Garantie 1-in-1-out** : autant de lignes en sortie qu'en entrée, dans le même ordre. Une ligne non géocodée sort avec `confidence=not_found` et les colonnes `lat,lng,matched_*` vides — **pas** supprimée du fichier.

**Déduplication interne** : si ton CSV contient deux fois la même adresse (même triplet `street, house_number, postal_code`), crabfinder résout une seule fois et propage le résultat. Optimisation transparente, ne change pas le format de sortie.

---

## CLI — `crabfinder-batch`

### Invocation minimale

```bash
./target/release/crabfinder-batch \
    --index ./index-belgium \
    --input adresses.csv \
    --output adresses-geocodees.csv
```

Le binaire ouvre l'index Tantivy en lecture seule, parcourt le CSV, écrit le résultat. Aucun port HTTP. Aucun service à démarrer. Le service `crabfinder-service` peut tourner en parallèle sur le même index — Tantivy supporte la lecture multi-process.

### Tous les flags

| Flag | Défaut | Description |
|---|---|---|
| `--index PATH` | — (obligatoire) | Dossier de l'index Tantivy. Variable d'env : `CRABFINDER_INDEX`. |
| `--abbreviation-dict PATH` | (aucun) | YAML d'abréviations. Sans, les requêtes type « Av. Louise » ne se déploient pas en « Avenue Louise ». **Recommandé** : `data/street-type-abbreviations-v1.yaml`. |
| `-i, --input PATH` | `stdin` | CSV d'entrée. `-` = stdin. |
| `-o, --output PATH` | `stdout` | CSV de sortie. `-` = stdout. |
| `--delimiter ,` ou `;` | auto | Force le délimiteur d'entrée (sinon détection auto). |
| `--no-mid-street` | désactivé (=> fallback médian actif) | Désactive le fallback `median_street`. Les lignes sans numéro retournent `not_found` au lieu d'un centroïde de rue. |
| `--parallel N` | nombre de cœurs | Threads de résolution. `--parallel 1` pour un profiling déterministe. |

### Exemples concrets

#### Géocoder un export Excel français (point-virgule, latin-1)

```bash
iconv -f WINDOWS-1252 -t UTF-8 export-excel.csv \
  | ./target/release/crabfinder-batch \
        --index ./index-belgium \
        --abbreviation-dict data/street-type-abbreviations-v1.yaml \
        --delimiter ';' \
        --output geocodees.csv
```

#### En streaming, sans fichier intermédiaire

```bash
psql -c "COPY (SELECT id, street, hn AS house_number, cp AS postal_code FROM clients) TO STDOUT CSV HEADER" \
  | ./target/release/crabfinder-batch \
        --index ./index-belgium \
        --abbreviation-dict data/street-type-abbreviations-v1.yaml \
  > clients-geocodees.csv
```

#### Estimer un débit

```bash
time ./target/release/crabfinder-batch \
    --index ./index-belgium \
    --abbreviation-dict data/street-type-abbreviations-v1.yaml \
    --input adresses-100k.csv \
    --output /tmp/out.csv
```

Ordre de grandeur : ~1500 lignes/s par cœur sur laptop dev en lecture in-process. Pour 100k lignes sur 8 cœurs : ~10 s.

### Codes de retour

- `0` : tout est passé.
- `non-0` : erreur (CSV invalide, index introuvable, sidecar de mauvaise version, ...). Le message d'erreur sort sur `stderr` ; `stdout` reste « propre » (utile en pipe).

---

## Endpoint HTTP — `POST /batch/geocode`

Prérequis : le service `crabfinder-service` tourne (voir [`02-installation.md`](02-installation.md) §6).

### Trois variantes de body

Le handler dispatche selon le `Content-Type`.

#### Variante 1 — multipart/form-data (recommandé pour les gros fichiers)

```bash
curl -X POST http://127.0.0.1:8080/batch/geocode \
     -F file=@adresses.csv \
     -F 'options={"mid_street_fallback":true,"format":"csv"}' \
     -o adresses-geocodees.csv
```

Capacité : jusqu'à **50 Mo de body** par requête (limite serveur par défaut). À ~500 octets/ligne, ça plafonne à ~100k lignes.

#### Variante 2 — application/json (recommandé pour les petits lots, intégrations applicatives)

```bash
curl -X POST http://127.0.0.1:8080/batch/geocode \
     -H "Content-Type: application/json" \
     -d '{
       "addresses": [
         {"id": "1", "street": "Avenue Louise", "house_number": "32", "postal_code": "1050"},
         {"id": "2", "street": "Grand-Place", "postal_code": "1000"}
       ],
       "options": {"mid_street_fallback": true}
     }' \
     -o lot.csv
```

Capacité : jusqu'à **10 000 adresses** par requête (HTTP 413 au-delà — passer en multipart).

#### Variante 3 — body CSV brut (Content-Type quelconque, simple à câbler)

```bash
curl -X POST http://127.0.0.1:8080/batch/geocode \
     -H "Content-Type: text/csv" \
     --data-binary @adresses.csv \
     -o adresses-geocodees.csv
```

C'est le chemin le plus dépouillé, utile pour tester avec un client minimal.

### Options du body JSON / multipart

| Option | Type | Défaut | Effet |
|---|---|---|---|
| `mid_street_fallback` | bool | `true` | Quand `false`, désactive le fallback `median_street` (mêmes effets que `--no-mid-street` côté CLI). |
| `format` | string | `"csv"` | Réservé pour de futurs formats (GeoJSON). Seul `"csv"` est supporté en v1. |

### Réponse

`Content-Type: text/csv`, **streamée ligne par ligne**. Le premier octet sort dès que les premiers résultats sont prêts, ce qui change la perception utilisateur sur les gros fichiers (TTFB en quelques centaines de ms au lieu de plusieurs secondes en mode bufferisé).

### Codes HTTP

- `200` : réponse CSV (peut être vide si le CSV d'entrée n'avait que le header).
- `400` : CSV mal formé, JSON mal formé, content-type incohérent.
- `413` : payload JSON > 10 000 adresses.
- `500` : erreur interne (à signaler ; pas censé arriver).

---

## Cas particuliers

### Adresses sans code postal

Fonctionne, mais qualité fortement dégradée. crabfinder ne peut plus filtrer sur le CP, donc les rues homonymes (« Rue de la Gare » présente dans des centaines de communes) reviennent dans un ordre indéterminé. **Toujours fournir le CP si tu l'as**.

### Adresses avec uniquement un nom de commune (pas de rue)

Non supporté en v1. Le champ `street` est obligatoire. Si tu veux juste géocoder « Bruxelles 1000 », passe par le mode recherche en ligne avec `q=Bruxelles 1000`.

### Doublons dans l'entrée

Préservés en sortie (1-in-1-out). En interne, crabfinder dédoublonne le travail : seules les adresses uniques sont résolues, le résultat est propagé sur toutes les occurrences. Aucune action de ta part nécessaire.

### Taille recommandée d'un lot

- CLI : pas de limite. 1M de lignes passent (mémoire ~quelques centaines de Mo en pic).
- HTTP multipart : 50 Mo max (~100k lignes).
- HTTP JSON : 10 000 adresses max.

Pour aller au-delà via HTTP, découpe en plusieurs requêtes côté client. Ou bascule sur la CLI.

## Et après ?

- Comprendre les colonnes `confidence` / `fallback_reason` → [`05-comprendre-resultats.md`](05-comprendre-resultats.md).
- Un comportement bizarre → [`06-faq.md`](06-faq.md).
