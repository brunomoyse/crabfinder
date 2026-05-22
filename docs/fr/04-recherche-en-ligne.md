# 4. Recherche en ligne (API HTTP)

Cas d'usage : tu intègres crabfinder dans une application (formulaire, autocomplétion, carte) où un utilisateur final tape une requête à la fois. Chaque appel HTTP est court (~20 ms), le service est censé tourner en continu.

> Tu cherches le mode batch ? Va à [`03-batch.md`](03-batch.md).

## Démarrage du service

Voir [`02-installation.md`](02-installation.md) §6. En résumé :

```bash
./target/release/crabfinder-service \
    --index ./index-belgium \
    --abbreviation-dict data/street-type-abbreviations-v1.yaml \
    --port 8080
```

Le service écoute en **IPv4 seulement** sur `127.0.0.1:8080` par défaut. Toujours interroger via `127.0.0.1`, pas `localhost` (qui sur certains systèmes résout en IPv6 d'abord).

## Endpoints disponibles

| Méthode | Chemin | Usage |
|---|---|---|
| `GET` | `/health` | Vérification de l'état du service |
| `GET` | `/geocode` | Géocodage direct (adresse → coordonnées) |
| `GET` | `/reverse` | Géocodage inverse (coordonnées → adresse) |
| `GET` | `/building/{id}/units` | Lister tous les logements d'un bâtiment |
| `POST` | `/batch/geocode` | Voir [`03-batch.md`](03-batch.md) |

## `GET /health` — état du service

```bash
curl "http://127.0.0.1:8080/health"
```

```json
{
  "status": "ok",
  "index_path": "./index-belgium",
  "index_docs": 6841382,
  "uptime_secs": 1234
}
```

À utiliser par un load balancer / monitoring pour vérifier que le service répond. Le champ `index_docs` doit tomber autour de 6,8M pour un index Belgique complet — s'il est très inférieur, l'indexation a échoué partiellement.

## `GET /geocode` — recherche d'adresse

C'est l'endpoint principal. Texte libre en entrée, top-N classé en sortie au format **GeoJSON**.

### Paramètres

| Paramètre | Type | Défaut | Effet |
|---|---|---|---|
| `q` | string | **requis** | La requête utilisateur (« Avenue Louise 32 1050 », « grand place bruxelles »…) |
| `limit` | int | `10` | Nombre max de résultats. Min 1, max 100. |
| `expand` | bool | `false` | Si `true`, retourne une feature par ligne BeSt sans dédoublonner par bâtiment. Utile pour lister les boîtes d'un même immeuble. |
| `region` | string | — | Filtre dur : `brussels`, `flanders`, `wallonia` (ou `BE-BRU`, `BE-VLG`, `BE-WAL`, ou les variantes `bruxelles`, `vlaanderen`, `wallonie`...). |
| `postal_code` | string | — | Filtre dur sur le code postal (4 chiffres). Prioritaire sur un CP extrait du `q`. |
| `focus_point` | string | — | `"lat,lng"` WGS84. Re-rerank doux : les candidats proches du point remontent. Demi-vie 25 km. |

### Exemples

```bash
# Recherche basique
curl "http://127.0.0.1:8080/geocode?q=Avenue+Louise+32+1050&limit=3"

# Restriction à une région
curl "http://127.0.0.1:8080/geocode?q=Rue+de+la+Gare&region=flanders&limit=5"

# Désambiguïsation géographique (centre de Liège)
curl "http://127.0.0.1:8080/geocode?q=Rue+de+la+Gare&focus_point=50.6326,5.5797"

# Cross-langue : la même adresse trouvée par son nom NL
curl "http://127.0.0.1:8080/geocode?q=Louizalaan+32+1050"
```

### Forme de la réponse

```jsonc
{
  "type": "FeatureCollection",
  "query": {
    "q": "Avenue Louise 32 1050",
    "limit": 3,
    "expand": false,
    "parsed": {
      "street_tokens": ["Avenue", "Louise"],
      "house_number": "32",
      "postal_code": "1050"
    }
  },
  "count": 3,
  "match_type": "strict",          // "strict" si au moins un résultat strict ; sinon "fuzzy"
  "deduplicated": true,            // false quand expand=true
  "raw_hits": 24,                  // lignes BeSt candidates avant collapse par bâtiment
  "_note": "...",
  "features": [
    {
      "type": "Feature",
      "geometry": {
        "type": "Point",
        "coordinates": [4.371, 50.825]    // [lng, lat] selon convention GeoJSON
      },
      "properties": {
        "street_name":   { "fr": "Avenue Louise", "nl": "Louizalaan", "de": null },
        "house_number":  "32",
        "box_number":    null,
        "postal_code":   "1050",
        "municipality_name": { "fr": "Ixelles", "nl": "Elsene", "de": null },
        "region":        "brussels",
        "status":        "current",
        "coordinates":   { "crs": "EPSG:4326", "lat": 50.825, "lon": 4.371 },
        "id":            { "namespace": "...", "object_identifier": "...", "version_identifier": "..." }
        // ... (toutes les métadonnées BeSt)
      },
      "score": 30.04,                    // BM25 × notability × ln(addr_count)
      "match_type": "strict",            // "strict" ou "fuzzy"
      "unit_count": 4,                   // 4 boîtes dans ce bâtiment
      "building_group_id": "..."
    }
    // ... 2 autres features
  ]
}
```

### Comprendre `match_type`

| Valeur | Sens |
|---|---|
| `strict` | Tous les tokens de la requête ont matché via la passe AND principale (la « voie royale »). |
| `fuzzy` | La passe stricte a renvoyé zéro résultat, le service est retombé sur OR-of-tokens. Match plus lâche, à inspecter. |

Le champ `match_type` au niveau racine du document est `strict` dès qu'**au moins une** feature retournée est strict. Si toutes sont fuzzy, c'est `fuzzy`.

### Désactiver la déduplication par bâtiment

Par défaut, les boîtes multiples d'un même bâtiment sont collapsées en une feature avec `unit_count > 1`. Pour récupérer chaque boîte séparément (ex : tu veux les 12 boîtes du 32 Avenue Louise dans une carte) :

```bash
curl "http://127.0.0.1:8080/geocode?q=Avenue+Louise+32&expand=true"
```

Tu obtiens une feature par ligne BeSt (jusqu'à `limit`), sans collapse.

Alternative plus propre : utiliser `/building/{id}/units` (voir plus bas) avec le `building_group_id` retourné par la première requête.

## `GET /reverse` — coordonnées → adresses

Cas d'usage : un utilisateur clique sur une carte, tu veux l'adresse la plus proche.

```bash
curl "http://127.0.0.1:8080/reverse?lat=50.8466&lng=4.3525&limit=1"
```

### Paramètres

| Paramètre | Type | Défaut | Effet |
|---|---|---|---|
| `lat` | float | requis | Latitude WGS84 |
| `lng` | float | requis | Longitude WGS84 |
| `limit` | int | `1` | Nombre max de résultats. Max 100. |
| `radius_m` | float | — | Si fourni, exclut les résultats au-delà de `radius_m` mètres. |

### Réponse

Même enveloppe que `/geocode`, avec un champ `distance_m` (distance grand-cercle en mètres) sur chaque feature. Triées par distance croissante puis dédoublonnées par bâtiment.

```jsonc
{
  "type": "FeatureCollection",
  "query": { "lat": 50.8466, "lng": 4.3525, "limit": 1 },
  "count": 1,
  "features": [
    {
      "type": "Feature",
      "geometry": { "type": "Point", "coordinates": [4.3525, 50.8466] },
      "properties": {
        "street_name":   { "fr": "Grand-Place", "nl": "Grote Markt" },
        "house_number":  "1",
        "postal_code":   "1000",
        "municipality_name": { "fr": "Bruxelles", "nl": "Brussel" }
        // ...
      },
      "distance_m": 4.23,             // 4,23 m du point demandé
      "unit_count": 1,
      "building_group_id": "..."
    }
  ]
}
```

### Limiter au rayon utile

Pour de la géolocalisation depuis un téléphone (précision ~10-50 m), filtrer à `radius_m=50` évite de remonter l'adresse à 200 m si l'utilisateur est en plein parc :

```bash
curl "http://127.0.0.1:8080/reverse?lat=50.8466&lng=4.3525&radius_m=50&limit=3"
```

## `GET /building/{id}/units` — toutes les unités d'un bâtiment

Cas d'usage : tu as un `building_group_id` (depuis un appel `/geocode` précédent) et tu veux lister toutes les boîtes du bâtiment. Pratique pour afficher une liste d'unités quand l'utilisateur clique sur un immeuble.

```bash
# Le building_group_id contient des / et des | qu'il faut URL-encoder.
BGID="https://databrussels.be/id/streetname/3919/1|32"
ENCODED=$(printf '%s' "$BGID" | jq -sRr @uri)

curl "http://127.0.0.1:8080/building/${ENCODED}/units"
```

Réponse : une `FeatureCollection` plate (pas de top-N), une feature par boîte, triées par `house_number` puis `box_number` croissants.

## Filtres et stratégies de désambiguïsation

### Quand utiliser `region`

Tu sais que tes adresses sont toutes flamandes : ajouter `region=flanders` réduit le pool de candidats et écarte par construction les rues homonymes wallonnes.

### Quand utiliser `postal_code` en filtre dur

Quand ton flux a déjà le CP propre et que tu veux interdire au service de chercher ailleurs. Le `postal_code` en filtre dur écrase tout CP éventuellement extrait du `q` :

```bash
# Même si q contient "1000", le filtre force 1050
curl "http://127.0.0.1:8080/geocode?q=Grand-Place+1000&postal_code=1050"
```

### Quand utiliser `focus_point`

Tu as la localisation de l'utilisateur (depuis le navigateur, GPS, IP) : passer `focus_point=lat,lng` re-rankera doucement les candidats proches sans exclure les lointains. Demi-vie 25 km.

**Attention** : `focus_point` n'aide que si le pool de candidats contient déjà des adresses proches. Si tu cherches « Rue de la Gare » et qu'une grosse ville domine BM25 avec 200 résultats identiques, focus_point ne lèvera personne dans le pool. Dans ce cas, ajouter `region` ou `postal_code` aide davantage.

## CORS

Le service répond `Access-Control-Allow-Origin: *` par défaut, donc un front JavaScript dans un navigateur peut taper directement sur l'API. Pas d'auth — toujours assumer que le service tourne en local ou derrière un reverse-proxy de confiance.

## Erreurs

| Code | Cause |
|---|---|
| `400 Bad Request` | Paramètre manquant ou mal formé (`region` inconnu, `lat`/`lng` hors plage, `focus_point` mal écrit…) |
| `503 Service Unavailable` | Démarrage pas terminé (le R-tree spatial se construit ~3 s au démarrage) |

Le body d'erreur est `text/plain` (pas JSON). Si tu as besoin d'erreurs structurées, mets une gateway devant.

## Et après ?

- Géocoder un CSV → [`03-batch.md`](03-batch.md).
- Comprendre les scores et les champs → [`05-comprendre-resultats.md`](05-comprendre-resultats.md).
- Le service ne démarre pas → [`06-faq.md`](06-faq.md).
