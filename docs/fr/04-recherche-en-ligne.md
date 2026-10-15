# 4. Recherche en ligne (API HTTP)

Cas d'usage : tu intègres crabfinder dans une application (formulaire, carte, outil de back-office) où un utilisateur tape une requête à la fois. Chaque appel HTTP est court (p50 ~1,5 ms, p95 ~7 ms sur le jeu d'évaluation) et le service tourne en continu.

> Tu cherches le mode batch ? Voir [`03-batch.md`](03-batch.md).

## Démarrage du service

Voir [`02-installation.md`](02-installation.md) §6. En résumé :

```bash
./target/release/crabfinder-service \
    --index ./index-belgium \
    --abbreviation-dict data/street-type-abbreviations-v1.yaml \
    --port 8080
```

Le service écoute **en IPv4 uniquement** sur `127.0.0.1:8080` par défaut. Interroge-le toujours via `127.0.0.1`, pas `localhost` (qui se résout d'abord en IPv6 sur certains systèmes).

## Endpoints disponibles

| Méthode | Chemin | Usage |
|---|---|---|
| `GET` | `/health` | État du service |
| `GET` | `/geocode` | Géocodage direct (adresse → coordonnées) |
| `GET` | `/reverse` | Géocodage inverse (coordonnées → adresse) |
| `GET` | `/building/{id}/units` | Lister toutes les unités d'un bâtiment |
| `GET` | `/lookup` | Adresses par identifiant BeSt |
| `POST` | `/batch/geocode` | Voir [`03-batch.md`](03-batch.md) |
| `GET` | `/metrics` | Métriques Prometheus (sauf si démarré avec `--no-metrics`) |

## `GET /health` — état du service

```bash
curl "http://127.0.0.1:8080/health"
```

```json
{
  "status": "ok",
  "index_path": "./index-belgium",
  "index_docs": 6886637,
  "uptime_secs": 1234,
  "schema_version": 10,
  "index_built_at_unix_secs": 1792106077,
  "version": "0.1.0"
}
```

À utiliser depuis un load balancer / un monitoring pour vérifier que le service répond. `index_docs` doit tourner autour de 6,9M pour un index Belgique complet — beaucoup moins, et l'indexation a partiellement échoué. `index_built_at_unix_secs` indique quand l'index a été construit (pratique pour repérer une mise à jour oubliée).

## `GET /geocode` — recherche d'adresse

L'endpoint principal. Une adresse en entrée — en texte libre **ou** en champs séparés — et un top-N classé en sortie, en **GeoJSON**.

### Paramètres

| Param | Type | Défaut | Effet |
|---|---|---|---|
| `q` | string | — | Requête en texte libre (« Avenue Louise 32 1050 », « grand place bruxelles »…). |
| `street`, `housenumber`, `box`, `postcode`, `city` | string | — | Requête structurée, à la place de `q` (voir plus bas). |
| `limit` | int | `10` | Nombre max de résultats. Min 1, max 100. |
| `lang` | string | langue de chaque adresse | `fr`, `nl` ou `de` : langue de `properties.label`. |
| `expand` | bool | `false` | À `true`, renvoie un feature par ligne BeSt, sans déduplication par bâtiment. Utile pour lister les boîtes d'un même bâtiment. |
| `region` | string | — | Filtre dur : `brussels`, `flanders`, `wallonia` (ou `BE-BRU`, `BE-VLG`, `BE-WAL`, ou les variantes `bruxelles`, `vlaanderen`, `wallonie`…). |
| `postal_code` | string | — | Filtre dur sur le code postal (4 chiffres). L'emporte sur tout code postal lu dans `q`, et ne se replie jamais sur un code postal voisin. |
| `bbox` | string | — | Filtre dur : `min_lon,min_lat,max_lon,max_lat` (WGS84, ordre GeoJSON). Seulement les adresses dans le rectangle. |
| `focus_point` | string | — | `"lat,lng"` WGS84 en Belgique. Re-classement doux : les candidats proches du point remontent. Demi-vie 25 km. |

Donne soit `q`, soit les champs structurés, pas les deux.

### Exemples

```bash
# Recherche basique
curl "http://127.0.0.1:8080/geocode?q=Avenue+Louise+32+1050&limit=3"

# Formats belges : boîte, préfixe B-, virgule
curl "http://127.0.0.1:8080/geocode?q=Rue+de+la+Loi+16+bte+3,+B-1000"

# Faute de frappe
curl "http://127.0.0.1:8080/geocode?q=Avenue+Loiuse+32+1050"

# Structurée, avec labels en néerlandais
curl "http://127.0.0.1:8080/geocode?street=Grote+Markt&housenumber=1&city=Antwerpen&lang=nl"

# Un hameau
curl "http://127.0.0.1:8080/geocode?q=Maredsous"

# Restriction à une région
curl "http://127.0.0.1:8080/geocode?q=Rue+de+la+Loi&region=wallonia&limit=5"

# Désambiguïsation géographique (centre de Gand)
curl "http://127.0.0.1:8080/geocode?q=Kerkstraat+1&focus_point=51.0543,3.7174"

# Cross-langue : la même adresse trouvée par son nom NL
curl "http://127.0.0.1:8080/geocode?q=Louizalaan+32+1050"
```

### Ce que le parseur comprend

`q` est découpé en mots de rue, numéro, boîte et code postal. Chaque ligne ci-dessous est un vrai résultat de `/geocode` :

| Tu tapes | Découpé en | Résultat |
|---|---|---|
| `Rue de la Loi 16 bte 3, B-1000` | rue `Rue de la Loi`, numéro `16`, boîte `3`, CP `1000` | Rue de la Loi 16, 1000 Bruxelles |
| `Louizalaan 32 bus 2 1050` | marqueurs de boîte `bte`, `boîte`, `bt`, `bus`, `box`, `b` | Avenue Louise 32, 1050 Ixelles |
| `Rue Belliard 71/2 1040` | `71/2` = numéro 71, boîte 2 | Rue Belliard 71, 1040 Bruxelles |
| `Avenue Louise 32-34 1050` | une plage garde son premier numéro | Avenue Louise 32, 1050 Ixelles |
| `1050-Ixelles Avenue Louise 32` | `1050-Ixelles` = CP + mot ; `B1050`, `BE-1050` marchent aussi | Avenue Louise 32, 1050 Ixelles |
| `Chaussée de Waterloo 1357 1180 Uccle` | deux nombres à 4 chiffres : le dernier est le CP, l'autre le numéro | Chaussée de Waterloo 1357, 1180 Uccle |
| `Rue du 11 Novembre 5 1040` | de plusieurs nombres, le **dernier** est le numéro ; `11` reste dans le nom de rue | mots de rue `Rue du 11 Novembre`, numéro `5` |

Quand une requête peut se lire de plusieurs façons, le parseur essaie aussi les autres lectures, dans l'ordre, et la réponse indique laquelle a gagné (`query.reading`, `0` = la première). Deux cas fréquents :

- `Rue du 11 Novembre` (pas d'autre nombre) : lu d'abord avec le numéro 11, puis avec `11` dans le nom de rue.
- `Han-sur-Lesse 5580`, `Grand-Place 1000` : un nombre isolé à 4 chiffres est lu d'abord comme code postal, puis comme numéro.

Les numéros ne tiennent pas compte de la casse (`32a` = `32A`). L'objet `query.parsed` de la réponse montre comment ta requête a été découpée — la première chose à regarder quand un résultat semble faux.

### Fautes de frappe

Un mot de rue qui n'existe pas dans l'index est rapproché des mots existants les plus proches : à 1 lettre près pour les mots de 5 à 8 lettres, 2 lettres pour 9 lettres et plus. Les mots de 4 lettres ou moins doivent être bien écrits. Un mot correctement orthographié n'est jamais « corrigé » en une autre rue.

```
q = Avenue Loiuse 32 1050
→ Avenue Louise 32, 1050 Ixelles   confidence=fuzzy_street  fallback_reason=street_spelling_fuzzy  match_type=typo
```

Les accents et la casse ne comptent jamais comme des fautes (`etterbeek` = `Etterbeek`, `chaussee` = `Chaussée`).

### Paramètres structurés

Quand tes données sont déjà découpées, envoie les champs plutôt que `q` :

| Param | Effet |
|---|---|
| `street` | Nom de rue, dans l'une ou l'autre langue. |
| `housenumber` | Numéro (`12`, `12A`). |
| `box` | Boîte / unité. Jamais un filtre : elle choisit quelle unité du bâtiment est renvoyée. |
| `postcode` | Code postal à 4 chiffres (toute autre valeur donne un 400). Fait partie de l'adresse, comme un code postal dans `q` : quand rien n'y correspond, la rue est cherchée à côté (`adjacent_commune`). |
| `city` | Nom de commune ou de localité, cherché avec les mots de rue. |

Il faut au moins `street`, `city` ou `postcode`.

```bash
curl "http://127.0.0.1:8080/geocode?street=Avenue+Louise&housenumber=32&box=b002&postcode=1050"
# → "label": "Avenue Louise 32 bte b002, 1050 Ixelles"
```

### Labels et langues

Chaque feature porte `properties.label`, l'adresse sur une ligne : `"{rue} {numéro}[ {marqueur} {boîte}], {code postal} {commune}"`, avec `bte` / `bus` / `Bfk.` comme marqueur de boîte en français / néerlandais / allemand.

Sans `lang`, chaque adresse est libellée dans sa propre langue : français en Wallonie, néerlandais en Flandre, français à Bruxelles, allemand dans les communes germanophones (codes postaux 4700–4791). Avec `lang=nl` (ou `fr`, `de`), le label utilise cette langue quand l'adresse a un nom dans cette langue, et sa propre langue sinon :

```
q=Rue de la Loi 16&lang=nl   → "Wetstraat 16, 1000 Brussel"
q=Klosterstraße 1 4700       → "Klosterstraße 1, 4700 Eupen"
q=Grand-Place 1000&lang=de   → "Grand-Place 2, 1000 Bruxelles"   (pas de nom allemand : langue propre)
```

`lang` ne change que le label. La recherche fonctionne toujours dans toutes les langues.

### Noms de lieux : villages, hameaux, communes

Une requête qui n'est qu'un nom de lieu — une commune, une section de commune, une zone postale, ou un village ou hameau OpenStreetMap — renvoie les adresses autour de ce lieu, plus un objet `locality` au premier niveau qui le décrit :

```bash
curl "http://127.0.0.1:8080/geocode?q=Maredsous&limit=2"
```

```jsonc
{
  "locality": {
    "kind": "hamlet",            // municipality, part_of_municipality, postal_area, village, hamlet, locality, isolated_dwelling
    "name": { "fr": "Maredsous", "nl": null, "de": null },
    "postal_codes": ["5537"],
    "coordinates": [4.769483, 50.304636]   // [lng, lat] du lieu
  },
  "features": [
    { "properties": { "label": "Rue des Abbayes 20, 5537 Anhée", ... },
      "match_type": "spatial", "confidence": "locality", "fallback_reason": "street_missing", ... },
    { "properties": { "label": "Rue des Abbayes 37, 5537 Anhée", ... }, ... }
  ]
}
```

Une rue suivie d'un nom de lieu marche aussi quand ce lieu n'est pas la commune officielle de l'adresse : `Rue de la Loi 17, 1040 Etterbeek` trouve `Rue de la Loi 17, 1040 Bruxelles` (ce tronçon du 1040 appartient à la Ville de Bruxelles) en `exact`.

### Forme de la réponse

```jsonc
{
  "type": "FeatureCollection",
  "query": {
    "q": "Avenue Louise 32 1050",
    "limit": 2,
    "expand": false,
    "parsed": {
      "street_tokens": ["Avenue", "Louise"],
      "house_number": "32",
      "box_number": null,
      "postal_code": "1050"
    },
    "reading": 0                    // quelle lecture de la requête a gagné (voir plus haut)
  },
  "count": 1,
  "ambiguous": false,               // voir « Requêtes ambiguës »
  // "score_gap": 0.42,             // présent quand il y a 2 résultats ou plus
  "match_type": "strict",           // "strict" si au moins un feature est strict ; sinon "fuzzy"
  "deduplicated": true,             // false quand expand=true
  "raw_hits": 11,                   // lignes BeSt considérées avant le regroupement par bâtiment
  "_note": "...",
  // "locality": { ... },           // seulement pour les recherches de lieu
  "features": [
    {
      "type": "Feature",
      "geometry": {
        "type": "Point",
        "coordinates": [4.356695401146948, 50.83451422165412]   // [lng, lat] selon la convention GeoJSON
      },
      "properties": {
        "label":         "Avenue Louise 32, 1050 Ixelles",
        "street_name":   { "fr": "Avenue Louise", "nl": "Louizalaan", "de": null },
        "house_number":  "32",
        "box_number":    null,
        "postal_code":   "1050",
        "municipality_name": { "fr": "Ixelles", "nl": "Elsene", "de": null },
        "region":        "brussels",
        "status":        "current",
        "coordinates":   { "crs": "EPSG:4326", "lat": 50.8345, "lon": 4.3567 },
        "id":            { "namespace": "...", "object_identifier": "...", "version_identifier": "..." }
        // ... (toutes les métadonnées BeSt ; "position_source" apparaît quand la position a été imputée)
      },
      "score": 186.87859,
      "match_type": "strict",
      "unit_count": 11,               // 11 lignes BeSt (boîtes) dans ce bâtiment
      "building_group_id": "https://databrussels.be/id/streetname|3919|32",
      "confidence": "exact",          // même vocabulaire que la colonne batch
      "house_number_delta": 0         // numéro trouvé moins numéro demandé, quand un numéro a été respecté
      // "fallback_reason": "...",    // présent quand la confiance n'est pas exact
    }
  ]
}
```

`confidence`, `fallback_reason` et `house_number_delta` ont exactement le même sens qu'en mode batch — voir [`05-comprendre-resultats.md`](05-comprendre-resultats.md). Demander `Avenue Louise 33 1050`, par exemple, renvoie `Avenue Louise 59` avec `confidence=nearest_same_side`, `house_number_delta=26`.

### Comprendre `match_type`

| Valeur | Signification |
|---|---|
| `strict` | Tous les mots de la requête ont trouvé une correspondance (le « chemin heureux »). |
| `typo` | Un mot de rue mal orthographié a été corrigé (voir [Fautes de frappe](#fautes-de-frappe)). |
| `fuzzy` | La passe stricte n'a rien donné ; le service s'est rabattu sur « n'importe lequel des mots ». Match plus lâche, à vérifier. |
| `spatial` | Trouvé par position, pas par le texte : `/reverse`, et les adresses autour d'un lieu pour une recherche de lieu. |
| `id` | Retrouvé par identifiant BeSt (`/lookup`). |

Le `match_type` de premier niveau vaut `strict` si **au moins un** feature renvoyé est strict, `fuzzy` sinon.

### Requêtes ambiguës

`ambiguous` vaut `true` quand le deuxième résultat atteint au moins 90 % du score du premier **et** se trouve dans une autre rue ou une autre commune — typiquement un nom de rue présent dans beaucoup de communes, sans code postal. `score_gap` (`1 − score₂/score₁`, présent dès qu'il y a au moins deux résultats) indique à quel point c'était serré : `0` = égalité.

```
q = Kerkstraat 1          → ambiguous=true,  score_gap=0.004
                            Kerkstraat 1, 8370 BLANKENBERGE/Uitkerke
                            Kerkstraat 1, 8420 DE HAAN/Klemskerke/Wenduine …
q = Kerkstraat 1 &focus_point=51.0543,3.7174
                          → ambiguous=false, score_gap=0.45
                            Kerkstraat 1, 9050 Gentbrugge/Ledeberg
```

Quand `ambiguous` vaut `true`, demande à l'utilisateur de choisir, ou ajoute un code postal, `region` ou `focus_point`.

### Désactiver la déduplication par bâtiment

Par défaut, les boîtes d'un même bâtiment sont regroupées en un seul feature avec `unit_count > 1`. Pour obtenir chaque boîte séparément :

```bash
curl "http://127.0.0.1:8080/geocode?q=Avenue+Louise+32+1050&expand=true"
```

Tu obtiens un feature par ligne BeSt (jusqu'à `limit`), sans regroupement.

Alternative plus propre : utiliser `/building/{id}/units` (plus bas) avec le `building_group_id` renvoyé par la première requête.

## `GET /reverse` — coordonnées → adresses

Cas d'usage : un utilisateur clique sur une carte, tu veux l'adresse la plus proche.

```bash
curl "http://127.0.0.1:8080/reverse?lat=50.8467&lng=4.3525&limit=1&lang=nl"
# → "label": "Grote Markt 29, 1000 Brussel", "distance_m": 37.3
```

### Paramètres

| Param | Type | Défaut | Effet |
|---|---|---|---|
| `lat` | float | obligatoire | Latitude WGS84 |
| `lng` | float | obligatoire | Longitude WGS84 |
| `limit` | int | `1` | Nombre max de résultats. Max 100. |
| `radius_m` | float | — | Si fourni, écarte les résultats au-delà de `radius_m` mètres. |
| `lang` | string | langue de chaque adresse | Langue de `properties.label`. |

### Réponse

Même enveloppe que `/geocode`, plus un champ `distance_m` (distance orthodromique en mètres) sur chaque feature, `match_type: "spatial"`. Tri par distance croissante, puis déduplication par bâtiment.

```jsonc
{
  "type": "FeatureCollection",
  "query": { "lat": 50.8467, "lng": 4.3525, "limit": 1 },
  "count": 1,
  "features": [
    {
      "type": "Feature",
      "geometry": { "type": "Point", "coordinates": [4.352720289567735, 50.84700502224868] },
      "properties": {
        "label": "Grote Markt 29, 1000 Brussel",
        "street_name": { "fr": "Grand-Place", "nl": "Grote Markt" },
        "house_number": "29",
        "postal_code": "1000"
        // ...
      },
      "distance_m": 37.28,
      "match_type": "spatial",
      "unit_count": 1,
      "building_group_id": "..."
    }
  ]
}
```

Seules les adresses avec une position précise au bâtiment sont candidates : les quelques milliers d'adresses qui n'ont reçu qu'une position de rue ou de village (voir [`05-comprendre-resultats.md`](05-comprendre-resultats.md#doù-viennent-les-coordonnées--position_source)) ne sortent jamais de `/reverse`.

### Limiter au rayon utile

Pour une géolocalisation mobile (~10-50 m de précision), filtrer avec `radius_m=50` évite de renvoyer une adresse à 200 m quand l'utilisateur est au milieu d'un parc :

```bash
curl "http://127.0.0.1:8080/reverse?lat=50.8466&lng=4.3525&radius_m=50&limit=3"
```

## `GET /building/{id}/units` — toutes les unités d'un bâtiment

Cas d'usage : tu as un `building_group_id` (issu d'un appel `/geocode`) et tu veux lister toutes les boîtes du bâtiment. Pratique quand un utilisateur clique sur un marqueur de bâtiment.

```bash
# Le building_group_id contient des / et des | qu'il faut URL-encoder.
BGID="https://databrussels.be/id/streetname|3919|32"
ENCODED=$(printf '%s' "$BGID" | jq -sRr @uri)

curl "http://127.0.0.1:8080/building/${ENCODED}/units"
```

Réponse : une `FeatureCollection` avec `building_group_id`, `count` et un feature par unité (chacun avec son `label`), triés par numéro de boîte (l'unité sans boîte en premier). Un identifiant inconnu renvoie `count: 0`.

## `GET /lookup` — adresses par identifiant BeSt

Tu as stocké des `best_id` (d'un batch ou de `properties.id`) et tu veux récupérer les adresses, sans chercher. Passe jusqu'à 50 identifiants séparés par des virgules ; `lang` fonctionne comme pour `/geocode`.

```bash
curl -G "http://127.0.0.1:8080/lookup" \
  --data-urlencode "id=https://databrussels.be/id/address/210225/1,nope/1/1" \
  --data lang=nl
```

Réponse : une `FeatureCollection` avec `count`, les features **dans l'ordre demandé** (`match_type: "id"`) et une liste `missing` des identifiants qui n'existent pas (par exemple retirés après une mise à jour BeSt) :

```
count = 1
missing = ["nope/1/1"]
features[0].properties.label = "Wetstraat 16, 1000 Brussel"
```

Un `id` vide donne un 400 (`id is empty`), plus de 50 aussi (`too many ids: 51 (max 50)`).

## Filtres et stratégies de désambiguïsation

### Quand utiliser `region`

Tu sais que toutes tes adresses sont flamandes : ajouter `region=flanders` réduit le pool de candidats et exclut d'office les rues wallonnes homonymes.

### Quand utiliser `postal_code` en filtre dur

Quand ton flux a déjà un CP propre et que tu veux interdire au service de chercher ailleurs. Le filtre dur `postal_code` remplace tout CP que le parseur a tiré de `q` :

```bash
# Même si q contient "1000", le filtre force 1050
curl "http://127.0.0.1:8080/geocode?q=Grand-Place+1000&postal_code=1050"
```

Sans lui, un code postal dans `q` (ou `postcode`) fait partie de l'adresse et peut être corrigé : `Avenue Louise 54 1000` renvoie `Avenue Louise 54, 1050` avec `confidence: adjacent_commune`, parce que le 54 est à 1050. Avec `q=Avenue Louise 54&postal_code=1000`, le service reste dans 1000 et n'y trouve qu'un match approximatif.

### Quand utiliser `bbox`

Ta carte montre une zone, ou tes données sont connues pour tenir dans un rectangle : `bbox=min_lon,min_lat,max_lon,max_lat` ne garde que les adresses à l'intérieur, à toutes les étapes.

```bash
curl -G "http://127.0.0.1:8080/geocode" --data-urlencode "q=Rue de la Loi 16" \
  --data-urlencode "bbox=4.36,50.84,4.37,50.85"
# → "Rue de la Loi 16, 1000 Bruxelles"
```

Une boîte malformée (pas 4 nombres, min ≥ max, hors bornes) donne un 400 : `invalid bbox '1,2,3' (expected min_lon,min_lat,max_lon,max_lat)`.

### Quand utiliser `focus_point`

Tu as la position de l'utilisateur (navigateur, GPS, IP) : passer `focus_point=lat,lng` fait remonter en douceur les candidats proches sans exclure les lointains. Demi-vie 25 km : un candidat à 25 km garde la moitié de son score.

**Limite** : `focus_point` re-classe les candidats trouvés par la recherche textuelle ; il ne cherche pas autour du point. Sans numéro, un nom de rue courant (« Rue de la Gare ») se replie au niveau de la rue la mieux notée, et le focus a peu de matière. Ajouter un numéro, `postal_code` ou `region` aide davantage.

## CORS

Le service répond avec `Access-Control-Allow-Origin: *` par défaut, donc un frontend JS dans un navigateur peut appeler l'API directement. Pour restreindre, démarre le service avec `--cors-origin https://app.example.org` (répétable). Pas d'authentification — pars toujours du principe que le service tourne en local ou derrière un reverse proxy de confiance.

## Délais et charge

Chaque requête a un délai maximal (`--request-timeout-ms`, 10 s par défaut), attente d'une place libre comprise ; au plus `--max-concurrency` requêtes (256 par défaut) sont traitées en même temps, les autres attendent. Une requête qui dépasse son délai reçoit un `503`. En pratique un géocodage prend quelques millisecondes : un `503` signifie que le service est surchargé.

## Métriques

`GET /metrics` sert le format texte Prometheus :

| Série | Contenu |
|---|---|
| `crabfinder_http_requests_total{route,method,status}` | Requêtes par route et par statut. |
| `crabfinder_http_request_duration_seconds{route}` | Histogramme de latence par route. |
| `crabfinder_http_rejected_total{route,reason}` | Requêtes ayant reçu un `503` pour dépassement de délai. |
| `crabfinder_search_total{tier,confidence,match_type}` | Une par recherche (chaque `/geocode`, chaque ligne batch), selon son résultat. |
| `crabfinder_search_capped_total` | Recherches avec tant de candidats que la fin du résultat peut manquer des bâtiments. |

La répartition par `confidence` est un indicateur de qualité peu coûteux : un bond des `not_found` ou des `fuzzy_street` signale en général un problème de données en amont.

## Erreurs

Tout corps d'erreur est du JSON : `{"error": "<message>"}`.

| Code | Cause |
|---|---|
| `400 Bad Request` | Paramètre manquant ou malformé : ni `q` ni champ structuré, `q` combiné avec des champs structurés, `postcode` qui n'a pas 4 chiffres, `region` ou `lang` inconnu, `lat`/`lng` hors bornes, `focus_point` ou `bbox` malformé, identifiants `/lookup` vides ou trop nombreux… |
| `404 Not Found` | Chemin inconnu (corps vide). |
| `503 Service Unavailable` | Délai dépassé (`request timed out after 10000 ms`). |

Exemples :

```
GET /geocode?q=x&street=y   → 400 {"error":"q cannot be combined with street, housenumber, box, postcode or city"}
GET /geocode?q=x&lang=en    → 400 {"error":"unknown lang 'en' (expected fr, nl or de)"}
GET /reverse?lat=95&lng=4   → 400 {"error":"lat out of range [-90, 90]: 95"}
```

Le service n'accepte les connexions qu'une fois l'index entièrement chargé : il n'y a donc pas d'erreur « démarrage en cours ».

## Et après ?

- Géocoder un CSV → [`03-batch.md`](03-batch.md).
- Comprendre les scores et les champs → [`05-comprendre-resultats.md`](05-comprendre-resultats.md).
- Le service ne démarre pas → [`06-faq.md`](06-faq.md).
