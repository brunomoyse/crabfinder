# 3. Géocodage batch

Cas d'usage : tu as une liste d'adresses (CSV, export Excel, requête SQL) et tu veux en sortir un fichier enrichi avec lat / lng et une indication de qualité par ligne.

Deux façons d'attaquer ce besoin, **avec exactement la même logique de résolution derrière** (le même résolveur que `/geocode`) :

- **CLI `crabfinder-batch`**: pas de serveur, pas de port, tu lances un binaire, tu lui donnes un CSV, tu récupères un CSV (ou du GeoJSON). Privilégié pour les pipelines de data, les scripts cron, les exports ponctuels.
- **Endpoint HTTP `POST /batch/geocode`**: le service tourne déjà, tu pousses un CSV par requête HTTP, tu reçois un CSV (ou du GeoJSON) streamé. Privilégié pour les intégrations applicatives (un backend qui géocode à la demande).

Si tu hésites : la CLI est plus simple à démarrer. Va à la section [CLI](#cli-crabfinder-batch).

---

## Format d'entrée

CSV avec une ligne d'en-tête. Les colonnes sont reconnues **par leur nom**, dans n'importe quel ordre ; les colonnes inconnues sont ignorées. Deux formes sont acceptées, prends celle que tes données ont déjà.

### Colonnes séparées

```csv
id,street,house_number,postal_code,box_number
1,Avenue Louise,32,1050,
2,Rue Belliard,71,1040,
3,Grand-Place,,1000,
4,Avenue Louise,33,1050,
5,Some Nonexistent Street,99,1000,
```

### Texte libre : une colonne `address`

```csv
id;address;postal_code
1;Av. Louise 32, B-1050 Ixelles;
2;Rue de la Loi 16 bte 3, 1000 Bruxelles;
3;Grote Markt 1 Antwerpen;
4;Spontin;
5;Chaussée de Waterloo 1357 1180 Uccle;
6;Rue Neuve 1;1000
```

Chaque `address` est découpée exactement comme `/geocode?q=` (code postal, numéro, boîte, formats belges, voir [`04-recherche-en-ligne.md`](04-recherche-en-ligne.md#ce-que-le-parseur-comprend)). Une ligne avec une `address` non vide ignore ses colonnes `street` / `house_number` / `box_number`. Une colonne `postal_code` l'emporte quand même sur le code postal lu dans le texte (ligne 6 ci-dessus). Les deux formes peuvent cohabiter dans un même fichier : les lignes dont `address` est vide utilisent les colonnes séparées.

### Colonnes

| Colonne | Obligatoire | Notes |
|---|---|---|
| `id` | oui | Identifiant libre. Recopié tel quel en sortie pour que tu puisses rejoindre tes données d'origine. |
| `street` | `street` **ou** `address` | Nom de rue. Accents, tirets, plusieurs mots, l'une ou l'autre langue : aucune normalisation requise. |
| `address` | `street` **ou** `address` | L'adresse complète en texte libre. |
| `house_number` | non | Numéro du bâtiment. Les suffixes alphabétiques (`32A`, `12b`) sont acceptés. |
| `postal_code` | non | Code postal belge à 4 chiffres. Fortement recommandé : sans lui, les rues homonymes (« Rue de la Gare », « Kerkstraat ») ne peuvent pas être départagées. |
| `city` | non | Nom de commune ou de localité, traité comme les mots de la rue. Ignoré sur les lignes en texte libre. Comparé à l'adresse par `--corrections`. |
| `box_number` | non | Boîte / unité. Ne filtre jamais : si le bâtiment a une unité avec cette boîte (écrite comme dans BeSt, par ex. `b002`), c'est elle qui donne le `best_id` et le `label` de la ligne (`Avenue Louise 32 bte b002, 1050 Ixelles`) ; une boîte inconnue est simplement ignorée. |
| `best_id` | non | Un identifiant d'adresse BeSt conservé d'un traitement précédent, avec ou sans sa version. Recherché avant tout le reste, anciens identifiants compris : voir [Anciens identifiants BeSt](#anciens-identifiants-best--best_id). |

L'en-tête doit contenir `street`, `address` ou `best_id` ; sinon la CLI s'arrête avec ``csv header has none of the `street`, `address` and `best_id` columns`` (HTTP : `400`).

**Encodage** : UTF-8. Si ton CSV est en `windows-1252` (export Excel français/néerlandais typique), convertis-le d'abord :

```bash
iconv -f WINDOWS-1252 -t UTF-8 adresses-export.csv > adresses.csv
```

**Délimiteur** : virgule (`,`) ou point-virgule (`;`). Détecté automatiquement sur la première ligne ; force-le avec `--delimiter ';'` côté CLI en cas de doute.

**Lignes vides** : ignorées. **Lignes malformées** : la CLI s'arrête avec une erreur indiquant le numéro de ligne. L'endpoint HTTP renvoie un 400 avec le même message.

## Format de sortie

### CSV

19 colonnes, toujours dans cet ordre. Les 15 premières sont celles de la v1 ; les 4 dernières ont été ajoutées en v2 à la fin, pour que les scripts qui lisent les colonnes par position continuent de fonctionner.

```csv
id,street,house_number,postal_code,matched_street,matched_house_number,matched_postal_code,lat,lng,confidence,fallback_reason,house_number_delta,street_match_score,building_group_id,best_id,address,label,position_source,ambiguous
```

| Colonne | Signification |
|---|---|
| `id`, `street`, `house_number`, `postal_code` | **Recopiés de l'entrée**, pour l'audit (`street` vide sur les lignes en texte libre). |
| `matched_street`, `matched_house_number`, `matched_postal_code` | Ce que crabfinder a réellement trouvé dans BeSt. Montre les écarts (faute corrigée, numéro voisin…). |
| `lat`, `lng` | WGS84 (EPSG:4326). Vides si `not_found`. |
| `confidence` | `exact`, `nearest_same_side`, `nearest_any_side`, `median_street`, `fuzzy_street`, `adjacent_commune`, `locality`, `landmark`, `id` (trouvé par la colonne `best_id`) ou `not_found`. Détail dans [`05-comprendre-resultats.md`](05-comprendre-resultats.md). |
| `fallback_reason` | Vide si `confidence == exact`. Sinon `house_number_missing`, `house_number_out_of_range`, `street_spelling_fuzzy`, `postal_code_mismatch`, `street_missing` ou `no_match`. |
| `house_number_delta` | Numéro trouvé moins numéro demandé : `0` quand le numéro demandé existe, par ex. `26` quand on demande le 33 et que le 59 est renvoyé. Vide pour les résultats au niveau de la rue ou d'un lieu. |
| `street_match_score` | Score BM25 brut. Indicateur de qualité du match textuel. Plus haut = meilleur match. Vide si `not_found`. |
| `building_group_id` | Identifiant de bâtiment de crabfinder (`{identifiant de rue}|{numéro}`). Stable d'un dump à l'autre tant que l'identifiant de rue BeSt ne change pas. |
| `best_id` | Identifiant BeSt officiel de l'adresse retenue (`namespace/object_identifier/version`). |
| `address` | Le texte libre d'entrée, recopié (vide sur les lignes en colonnes séparées). |
| `label` | L'adresse trouvée sur une ligne, par ex. `Avenue Louise 32, 1050 Ixelles`, dans la langue de l'adresse ou celle demandée avec `--lang` / `lang`. |
| `position_source` | D'où viennent les coordonnées : `measured` (publiées par BeSt) ou une imputation (`same_number`, `interpolated`, `street`, `locality`). Voir [`05-comprendre-resultats.md`](05-comprendre-resultats.md#doù-viennent-les-coordonnées--position_source). |
| `ambiguous` | `true` quand une autre réponse presque aussi bonne se trouve dans une autre rue ou une autre commune. À vérifier. |

Sortie réelle pour l'exemple en colonnes séparées ci-dessus (`street_match_score` et identifiants raccourcis) :

```csv
id,street,house_number,postal_code,matched_street,matched_house_number,matched_postal_code,lat,lng,confidence,fallback_reason,house_number_delta,...,label,position_source,ambiguous
1,Avenue Louise,32,1050,Avenue Louise,32,1050,50.83451422165412,4.356695401146948,exact,,0,...,"Avenue Louise 32, 1050 Ixelles",measured,false
2,Rue Belliard,71,1040,Rue Belliard,71,1040,50.84121570632554,4.374046595229909,exact,,0,...,"Rue Belliard 71, 1040 Bruxelles",measured,false
3,Grand-Place,,1000,Grand-Place,2,1000,50.846228509839484,4.353010703511631,median_street,house_number_missing,,...,"Grand-Place 2, 1000 Bruxelles",measured,false
4,Avenue Louise,33,1050,Avenue Louise,59,1050,50.83310464460224,4.357281635827657,nearest_same_side,house_number_out_of_range,26,...,"Avenue Louise 59, 1050 Bruxelles",measured,false
5,Some Nonexistent Street,99,1000,,,,,,not_found,no_match,,...,,,false
```

Ligne 4 : le 33 n'existe pas en 1050, crabfinder renvoie donc le numéro impair le plus proche sur cette rue (`nearest_same_side`, `house_number_delta=26`).

**Lignes de lieu** : une ligne qui ne nomme qu'un lieu (`Spontin`, `Han-sur-Lesse 5580` ; un hameau comme `Maredsous` demande l'index facultatif `--places`) ressort comme **l'adresse BeSt la plus proche** de ce lieu, à sa propre position BeSt : rue, numéro, `best_id`, `building_group_id` et label viennent tous de BeSt (`Spontin` → par ex. `Chaussée de Dinant 47, 5530 Yvoir`). `confidence=locality` et `fallback_reason=street_missing` t'indiquent que cette adresse n'a pas été demandée par sa rue.

**Lignes de point d'intérêt** (avec l'option facultative `--pois`) : une ligne qui ne nomme qu'un monument ou un lieu connu (`Manneken Pis`, `Gare du Midi`, `Gravensteen Gent`) ressort de la même façon : l'adresse BeSt la plus proche (par ex. `Rue du Chêne 2, 1000 Bruxelles` pour `Manneken Pis`), avec `confidence=landmark`. Le nom et la position du point d'intérêt lui-même ne sont jamais renvoyés.

**Garantie 1-in-1-out** : même nombre de lignes, même ordre en entrée et en sortie. Une ligne non géocodée sort avec `confidence=not_found` et des `lat,lng,matched_*` vides, et n'est **pas** retirée du fichier.

**Déduplication interne** : si ton CSV contient deux fois la même adresse, crabfinder la résout une seule fois et recopie le résultat. Optimisation transparente, sans effet sur la sortie.

### GeoJSON

Avec `--format geojson` (CLI) ou `format=geojson` (HTTP), la sortie est une `FeatureCollection` GeoJSON : un feature `Point` par ligne d'entrée, dans l'ordre d'entrée (`geometry: null` pour les lignes `not_found`), dont les `properties` sont les 19 colonnes du CSV.

```json
{
  "type": "FeatureCollection",
  "features": [
    {
      "type": "Feature",
      "geometry": { "type": "Point", "coordinates": [4.356695401146948, 50.83451422165412] },
      "properties": {
        "id": "1",
        "matched_street": "Avenue Louise",
        "matched_house_number": "32",
        "confidence": "exact",
        "house_number_delta": 0,
        "label": "Avenue Louise 32, 1050 Ixelles",
        "position_source": "measured",
        "ambiguous": false
      }
    }
  ]
}
```

(Propriétés raccourcies.) Les coordonnées sont en `[lng, lat]`, selon la convention GeoJSON. Le fichier s'ouvre directement dans QGIS.

---

## CLI: `crabfinder-batch`

### Invocation minimale

```bash
./target/release/crabfinder-batch \
    --index ./index-belgium \
    --abbreviation-dict data/street-type-abbreviations-v1.yaml \
    --input adresses.csv \
    --output adresses-geocodees.csv
```

Le binaire ouvre l'index Tantivy en lecture seule, parcourt le CSV, écrit le résultat. Pas de port HTTP. Pas de service à démarrer. Le `crabfinder-service` peut tourner en parallèle sur le même index (Tantivy supporte les lectures multi-processus).

### Tous les flags

| Flag | Défaut | Description |
|---|---|---|
| `--index CHEMIN` | n/a (obligatoire) | Dossier de l'index Tantivy. Variable d'environnement : `CRABFINDER_INDEX`. |
| `--abbreviation-dict CHEMIN` | (aucun) | YAML des abréviations. Sans lui, « Av. Louise » n'est pas étendu en « Avenue Louise ». **Recommandé** : `data/street-type-abbreviations-v1.yaml`. |
| `--pois CHEMIN` | (aucun) | Aide OpenStreetMap facultative (`data/osm-pois-be-v1.csv`) : comprendre les lignes qui nomment un point d'intérêt ; elles reçoivent l'adresse BeSt la plus proche, `confidence=landmark`. Variable d'environnement : `CRABFINDER_POIS`. |
| `-i, --input CHEMIN` | `stdin` | CSV d'entrée. `-` = stdin. |
| `-o, --output CHEMIN` | `stdout` | Fichier de sortie. `-` = stdout. |
| `--delimiter ,` ou `;` | auto | Force le délimiteur d'entrée (sinon détecté). |
| `--format csv` ou `geojson` | `csv` | Format de sortie. |
| `--lang fr`, `nl` ou `de` | langue de chaque adresse | Langue de la colonne `label`. Une adresse sans nom dans cette langue garde la sienne. |
| `--no-mid-street` | désactivé | Désactive les replis au niveau de la rue (voir plus bas). |
| `--corrections` | désactivé | Ajoute les colonnes `status` et `corrections` (voir [Vérifier des adresses](#vérifier-des-adresses--corrections)). |
| `--alternatives` | désactivé | Ajoute `score_gap` et `alternatives` : les communes des meilleures réponses, pour les lignes qui ne disent pas laquelle (`Charleroi (6000); La Louvière (7100)`). |
| `--parallel N` | tous les cœurs | Threads de travail. `--parallel 1` pour un profilage déterministe. |

### Exemples concrets

#### Géocoder un export Excel français (point-virgule, latin-1)

```bash
iconv -f WINDOWS-1252 -t UTF-8 export-excel.csv \
  | ./target/release/crabfinder-batch \
        --index ./index-belgium \
        --abbreviation-dict data/street-type-abbreviations-v1.yaml \
        --delimiter ';' \
        --output geocode.csv
```

#### Adresses en texte libre, labels en néerlandais

```bash
./target/release/crabfinder-batch \
    --index ./index-belgium \
    --abbreviation-dict data/street-type-abbreviations-v1.yaml \
    --input texte-libre.csv --lang nl
```

Sur l'exemple en texte libre ci-dessus, la ligne 1 (`Av. Louise 32, B-1050 Ixelles`) sort `exact` avec le label `Louizalaan 32, 1050 Elsene`, et la ligne 2 (`Rue de la Loi 16 bte 3, 1000 Bruxelles`) avec `Wetstraat 16, 1000 Brussel`.

#### Directement dans QGIS

```bash
./target/release/crabfinder-batch --index ./index-belgium \
    --abbreviation-dict data/street-type-abbreviations-v1.yaml \
    --input adresses.csv --format geojson --output adresses.geojson
```

#### En streaming, sans fichier intermédiaire

```bash
psql -c "COPY (SELECT id, rue AS street, num AS house_number, cp AS postal_code FROM clients) TO STDOUT CSV HEADER" \
  | ./target/release/crabfinder-batch \
        --index ./index-belgium \
        --abbreviation-dict data/street-type-abbreviations-v1.yaml \
  > clients-geocodes.csv
```

Ou, si ta table n'a qu'une colonne d'adresse : `SELECT id, adresse_complete AS address FROM clients`.

#### Estimer un débit

```bash
time ./target/release/crabfinder-batch \
    --index ./index-belgium \
    --abbreviation-dict data/street-type-abbreviations-v1.yaml \
    --input adresses-100k.csv \
    --output /tmp/out.csv
```

Ordre de grandeur : ~1500 lignes/s par cœur en lecture in-process sur un portable de développement. Pour 100k lignes sur 8 cœurs : ~10 s.

### Codes de retour

- `0` : tout s'est bien passé.
- différent de `0` : erreur (CSV invalide, pas de colonne `street`/`address`, index absent, mauvaise version du sidecar…). Le message d'erreur part sur `stderr` ; `stdout` reste propre (utile dans un pipe).

---

## Endpoint HTTP: `POST /batch/geocode`

Prérequis : `crabfinder-service` tourne (voir [`02-installation.md`](02-installation.md) §6).

### Trois variantes de body

Le handler choisit selon le `Content-Type`. Les trois acceptent les deux formes d'entrée (colonnes séparées ou `address`).

#### Variante 1: multipart/form-data (recommandé pour les gros fichiers)

```bash
curl -X POST http://127.0.0.1:8080/batch/geocode \
     -F file=@adresses.csv \
     -F 'options={"mid_street_fallback":true,"format":"csv"}' \
     -o adresses-geocodees.csv
```

Capacité : jusqu'à **50 Mo de body** par requête. À ~500 octets par ligne, ça fait ~100k lignes.

#### Variante 2: application/json (recommandé pour les petits lots, intégrations applicatives)

```bash
curl -X POST http://127.0.0.1:8080/batch/geocode \
     -H "Content-Type: application/json" \
     -d '{
       "addresses": [
         {"id": "1", "street": "Avenue Louise", "house_number": "32", "postal_code": "1050"},
         {"id": "2", "address": "Grand-Place 1, 1000 Bruxelles"}
       ],
       "options": {"mid_street_fallback": true, "lang": "nl"}
     }' \
     -o lot.csv
```

Capacité : jusqu'à **10 000 adresses** par requête (HTTP 413 au-delà, passe au multipart).

#### Variante 3: body CSV brut (Content-Type quelconque, simple à câbler)

```bash
curl -X POST "http://127.0.0.1:8080/batch/geocode?format=geojson&lang=nl" \
     -H "Content-Type: text/csv" \
     --data-binary @adresses.csv \
     -o adresses.geojson
```

C'est le chemin le plus léger, pratique pour tester avec un client minimal.

### Options

Les options vont dans le body JSON / la partie multipart `options`, **ou dans la query string** (`?format=geojson&lang=nl&mid_street_fallback=false`), pratique pour la variante CSV brut. La query string l'emporte quand les deux sont fournies.

| Option | Type | Défaut | Effet |
|---|---|---|---|
| `mid_street_fallback` | bool | `true` | À `false`, désactive les replis au niveau de la rue (même effet que `--no-mid-street` côté CLI). |
| `corrections` | bool | `false` | À `true`, ajoute les colonnes `status` et `corrections` (même effet que `--corrections`). |
| `alternatives` | bool | `false` | À `true`, ajoute les colonnes `score_gap` et `alternatives` (même effet que `--alternatives`). |
| `format` | string | `"csv"` | `"csv"` ou `"geojson"`. Toute autre valeur donne un 400. |
| `lang` | string | langue de chaque adresse | `"fr"`, `"nl"` ou `"de"` : langue de la colonne `label`. Toute autre valeur donne un 400. |

### Réponse

`text/csv; charset=utf-8` (ou `application/geo+json`), **streamé**. Les premiers octets partent dès que les premières lignes sont résolues, ce qui compte sur les gros fichiers.

### Codes HTTP

- `200` : réponse CSV / GeoJSON (une entrée réduite à l'en-tête donne un corps CSV vide, ou une FeatureCollection vide).
- `400` : CSV ou JSON malformé, pas de colonne `street`/`address`, `format` ou `lang` inconnu. Message en texte brut, par ex. `unknown format 'xml' (expected csv or geojson)`.
- `413` : payload JSON > 10 000 adresses.
- `503` : la réponse n'a pas commencé dans le délai `--batch-timeout-secs` (600 s par défaut). Corps JSON `{"error": "..."}`.
- `500` : erreur interne (à signaler ; ne devrait pas arriver).

---

## Désactiver les replis au niveau de la rue

Par défaut, une ligne dont le numéro ne peut pas être trouvé reçoit quand même des coordonnées sur sa rue (`median_street`, ou un `fuzzy_street` au niveau de la rue). Si tu préfères des coordonnées vides plutôt qu'une approximation à la rue, utilise `--no-mid-street` (CLI) ou `mid_street_fallback=false` (HTTP) : ces lignes sortent alors en `not_found`.

```bash
./target/release/crabfinder-batch --index ./index-belgium -i adresses.csv --no-mid-street
```

Sur l'exemple en colonnes séparées, la ligne 3 (`Grand-Place`, sans numéro) devient `not_found, no_match`. La ligne 4 (`Avenue Louise 33`) reste `nearest_same_side` : le numéro voisin est une vraie adresse, pas une approximation à la rue, il est donc conservé.

## Vérifier des adresses : `corrections`

Pour savoir, ligne par ligne, si l'adresse existe telle qu'écrite et ce qui a été corrigé, ajoute `--corrections` (CLI) ou `corrections=true` (HTTP). Deux colonnes s'ajoutent après les colonnes habituelles : `status` et `corrections`, le même verdict que [`/validate`](04-recherche-en-ligne.md#get-validate--vérifier-une-adresse).

```bash
./target/release/crabfinder-batch --index ./index-belgium \
    --abbreviation-dict data/street-type-abbreviations-v1.yaml \
    -i clients.csv -o verifie.csv --corrections
```

```
id,street,house_number,postal_code,city,…,status,corrections
1,Avenue Louise,32,1050,,…,valid,
2,Av Louise,32,1000,Ixelles,…,corrected,abbreviation_expanded:street:av->avenue; postal_code_changed:postal_code:1000->1050
3,Avenue Louise,999,1050,,…,corrected,house_number_approximated:house_number:999->589
```

`status` vaut `valid`, `corrected`, `ambiguous`, `incomplete` ou `not_found`. Chaque correction se lit `kind:champ:entrée->officiel`, séparées par `; `. Sans l'option, les colonnes de sortie ne changent pas.

## Anciens identifiants BeSt : `best_id`

Un fichier de `best_id` enregistrés il y a des mois n'a pas besoin d'être regéocodé. BeSt versionne ses identifiants et retire des adresses, donc certains ont changé depuis. Mets-les dans une colonne `best_id`. Chaque ligne est alors d'abord recherchée par identifiant, comme le fait [`/lookup`](04-recherche-en-ligne.md#get-lookup--adresses-par-identifiant-best), et la sortie gagne deux dernières colonnes : `id_status` et `successor_best_id`.

```
id,best_id,street,house_number,postal_code
1,https://data.vlaanderen.be/id/adres/200001,,,
2,https://data.vlaanderen.be/id/adres/200068/2023-11-01T16:15:58+01:00,,,
3,nope/1/1,Avenue Louise,45,1050
```

```
id,…,confidence,…,best_id,…,id_status,successor_best_id
1,…,id,…,…/adres/200001/2023-11-01T14:53:09+01:00,…,current,
2,…,id,…,…/adres/<successeur>,…,retired,…/adres/<successeur>
3,…,exact,…,…,…,unknown,
```

- `current` / `outdated_version` : l'adresse elle-même, avec son `best_id` d'aujourd'hui (`confidence=id`).
- `retired` / `reserved` / `rejected` : l'adresse qui l'a remplacée, si BeSt en a une. C'est une adresse en vigueur dans la même rue, au même numéro et à la même boîte, sinon au même numéro, sinon à moins de 5 m de l'emplacement de l'ancienne. `successor_best_id` est cette adresse. Sans successeur, la ligne est `not_found`.
- `unknown` : l'identifiant n'est pas dans ce dump BeSt.

Quand l'identifiant ne mène à aucune adresse en vigueur et que la ligne a aussi des colonnes d'adresse, celles-ci sont géocodées normalement (ligne 3). Les lignes sans `best_id` ont des colonnes d'identifiant vides. Avec `--corrections`, un identifiant remplacé se lit `id_replaced:best_id:<ancien>-><nouveau>`.

## Cas particuliers

### Adresses sans code postal

Ça fonctionne, mais la qualité baisse nettement. crabfinder ne peut pas filtrer par CP, donc les rues homonymes (« Kerkstraat » existe dans des centaines de communes) reviennent dans un ordre non déterminé, et ces lignes sont en général marquées `ambiguous=true`. **Fournis toujours le CP quand tu l'as.**

### Mauvais code postal

Un code postal qui ne contient pas l'adresse n'est pas rédhibitoire : quand rien n'y correspond (ou que la rue y est mais pas le numéro, et que le numéro exact existe juste de l'autre côté de la limite), crabfinder cherche la rue dans les 20 km autour du centre de ce code postal et renvoie `adjacent_commune` / `postal_code_mismatch`. `matched_postal_code` montre alors le vrai :

```
id,street,house_number,postal_code,matched_street,matched_house_number,matched_postal_code,lat,lng,confidence,fallback_reason,house_number_delta
1,Avenue Louise,54,1000,Avenue Louise,54,1050,50.83340378514029,4.358146544863564,adjacent_commune,postal_code_mismatch,0
```

Voir [`05-comprendre-resultats.md`](05-comprendre-resultats.md#adjacent_commune--trouvé-à-côté).

### Adresses avec seulement un nom de lieu (pas de rue)

Supporté, sous les deux formes : `address=Spontin`, ou `street=Maredsous` avec un `house_number` vide. Quand tout le texte est un nom de lieu connu (une commune, section de commune ou zone postale BeSt, ou, avec l'index facultatif `--places`, un village ou hameau OpenStreetMap comme `Maredsous`), tu obtiens une ligne `locality` : l'adresse BeSt la plus proche du lieu. Avec l'option facultative `--pois`, un nom de point d'intérêt (`Manneken Pis`, `Gare du Midi`, éventuellement suivi de sa localité : `Gravensteen Gent`) donne une ligne `landmark`, là aussi l'adresse BeSt la plus proche, sauf si une rue porte ce nom (`Atomium` → Avenue de l'Atomium, `median_street`).

### Numéros de boîte

Utilise la colonne `box_number`, ou écris la boîte avec le numéro, dans `house_number` (`32 bte b002`, `32/b002`) ou dans le texte libre (`bte 3`, `bus 3`, `box 3`, `12/3`). La boîte n'empêche jamais un match : le bâtiment est trouvé dans tous les cas, la boîte décide seulement laquelle de ses unités apparaît dans `best_id` / `label`. Écris la boîte comme BeSt l'écrit (Bruxelles utilise souvent des formes comme `b002`) ; une boîte qui ne correspond à aucune unité est ignorée.

### Doublons dans l'entrée

Conservés en sortie (1-in-1-out). En interne, crabfinder ne résout que les adresses uniques et propage le résultat à chaque occurrence.

### Taille recommandée d'un lot

- CLI : pas de limite. 1M de lignes passe (pic mémoire de quelques centaines de Mo).
- HTTP multipart : plafond de 50 Mo (~100k lignes).
- HTTP JSON : plafond de 10 000 adresses.

Pour aller au-delà en HTTP, découpe côté client. Ou passe à la CLI.

## Et après ?

- Comprendre les colonnes `confidence` / `fallback_reason` / `position_source` → [`05-comprendre-resultats.md`](05-comprendre-resultats.md).
- Un comportement étrange → [`06-faq.md`](06-faq.md).
