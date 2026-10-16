# Design: batch geocoding pour crabfinder

> **Statut (2026-10-16)** : document de décision d'origine, conservé tel quel (les sections §1 à §5 sont citées par le code). Le périmètre v1 a été livré, avec des écarts par rapport au texte ci-dessous :
>
> - Le cœur `resolve()` vit dans `crates/crabfinder-service/src/resolver.rs` (avec `search()`, `resolve_with`, `resolve_with_alternatives`, `resolve_by_id`) ; `/geocode` utilise `search()` directement (option A du §1.3). `Confidence` compte désormais dix valeurs : `exact`, `nearest_same_side`, `nearest_any_side`, `median_street`, `fuzzy_street`, `adjacent_commune`, `locality`, `landmark`, `id`, `not_found` (`landmark` n'existe qu'avec `--pois`, aide OSM optionnelle qui ne fait qu'interpréter la requête : tout résultat reste une adresse BeSt).
> - Le CLI est le crate `crabfinder-batch` (lib + `main.rs` mince), pair du service, pas un wrapper. Flags : `--index`, `--abbreviation-dict`, `--pois`, `-i/--input`, `-o/--output`, `--delimiter`, `--no-mid-street`, `--parallel`, `--format csv|geojson`, `--lang`, `--corrections`, `--alternatives`. Il peut tourner à côté d'un service sur le même index (lecture mmap partagée).
> - L'endpoint HTTP `POST /batch/geocode` accepte JSON (max 10 000 adresses, sinon 413), multipart ou CSV brut ; corps limité à 50 MiB. La réponse n'est **pas** encore streamée incrémentalement : le premier octet part une fois le lot résolu (le streaming et la correction du délai `--batch-timeout-secs`, Phase 11.6, restent à faire). Les jobs batch asynchrones ne sont pas faits non plus.
> - Entrées au-delà de la v1 : `id,address` (free text), `box_number`, `city`, `best_id` ; sorties : GeoJSON, colonnes `address`, `label`, `position_source`, `ambiguous`, `status`/`corrections` (Phase 11.2), `score_gap`/`alternatives` (Phase 11.3), `id_status`/`successor_best_id` (Phase 11.4). `street_match_score` est le score BM25 brut, pas le bucket 0 à 3 du §3.3 ; la calibration des seuils de confiance est écrite par `crabfinder-eval --batch-mode` (`crates/crabfinder-eval/data/confidence-thresholds-v1.json`).
> - Le mid-street est une side-file `mid-street-v1.bin` (pas un champ FAST). Le schéma d'index est à la version **12** (`crates/crabfinder-indexer/src/meta.rs`) ; les « v6/v7 » cités plus bas sont historiques. Nearest-number (parité) et commune adjacente (post-v1 au §5) sont livrés. Voir `docs/architecture.md`, `docs/api.md` et `docs/en/` / `docs/fr/` pour l'état courant.

Document de décision. Prérequis : avoir lu [`comparison-phacochr.md`](./comparison-phacochr.md), beaucoup de choix ici se justifient par référence à ce qu'on y a appris.

Objectif : faire entrer un deuxième cas d'usage (batch d'une liste d'adresses → résultat enrichi) dans crabfinder, sans casser le cas d'usage actuel (recherche en ligne, query libre), sans introduire deux produits parallèles, et sans surcharger la v1.

Trois questions produit étaient ouvertes, ce document propose des réponses :
1. Quel(s) format(s) d'entrée batch accepter ?
2. Quelles interfaces exposer (HTTP, CLI, les deux) ?
3. Comment séparer batch et recherche tout en partageant la logique ?

Réponses, dans l'ordre où elles deviennent les moins discutables : §1 (architecture cœur partagé) d'abord, parce qu'il rend les trois questions ci-dessus largement cosmétiques et réversibles. Puis §2 (formats), §3 (sortie), §4 (interfaces), §5 (scope v1).

## 1. Architecture du cœur partagé: contrainte non négociable

**Recommandation** : extraire un module pur `crabfinder_resolver` (intra-crate `crabfinder-service` ou nouveau crate `crabfinder-resolver`) qui prend une adresse **structurée** en entrée et rend une **décision unique** + un score de confiance décomposé. Aucune dépendance HTTP, aucune dépendance CLI, aucune connaissance de Tantivy au-delà de ce qui est strictement nécessaire pour exécuter la requête (et on rend ces accès paramétrables).

Recherche en ligne (`/geocode`) et batch deviennent **deux consommateurs** de ce cœur :
- `/geocode` ajoute le parsing free-text amont (`crabfinder-parser`) et la sérialisation GeoJSON aval, façade HTTP.
- Le batch alimente le cœur avec des lignes déjà structurées + sérialise en CSV/GeoJSON.

C'est exactement le même pattern que `dedup.rs` et `spatial.rs` aujourd'hui : pure logic, signatures non-IO, testable en isolation. La preuve qu'on a déjà raison sur ce pattern est dans le code actuel, `dedup.rs:70` (`group_by_building`) est cité dans la doc d'architecture et fonctionne sans Tantivy ni HTTP.

### 1.1 Signature proposée du cœur

```rust
// crates/crabfinder-service/src/resolver.rs (ou crate dédié)

/// Entrée structurée : tout ce que le parser libre extrait, plus ce qu'un batch
/// peut fournir directement.
pub struct AddressQuery {
    pub street_tokens: Vec<String>,    // déjà tokenisé/normalisé
    pub house_number: Option<String>,  // "32", "32B", "32-34" → "32"
    pub postal_code: Option<String>,   // "1050"
    pub region_hint: Option<Region>,   // optionnel, filtre dur si fourni
    pub focus_point: Option<(f64, f64)>, // optionnel, pour décay distance (recherche)
}

/// Sortie : UNE décision par requête, pas un top-N. C'est la différence
/// fondamentale recherche vs batch : la recherche veut un top-N ranké pour
/// laisser l'utilisateur choisir ; le batch veut LA réponse.
pub struct ResolvedAddress {
    pub address: Option<Address>,           // None si zéro hit
    pub confidence: Confidence,             // taxonomie ci-dessous
    pub house_number_delta: Option<i32>,    // 0 = exact, n = approximé de n, None = mid_street ou not_found
    pub street_match_score: f32,            // BM25 du strict pass (0 si fuzzy), à interpréter via Confidence
    pub fallback_reason: Option<FallbackReason>, // pourquoi on n'a pas le bâtiment exact
}

#[derive(Clone, Copy, PartialEq)]
pub enum Confidence {
    Exact,                  // strict match, numéro exact
    NearestSameSide,        // numéro approximé, même parité
    NearestAnySide,         // numéro approximé, parité différente
    MedianStreet,           // pas de numéro / numéro inconnu → coordonnées centroïdes rue
    FuzzyStreet,            // fallback fuzzy AND→OR sur la rue, numéro exact
    AdjacentCommune,        // rue trouvée dans une commune limitrophe (post-v1)
    NotFound,               // aucun match
}

pub enum FallbackReason {
    HouseNumberMissing,        // input avait un numéro, BeSt ne l'a pas
    HouseNumberOutOfRange,     // approximation > seuil
    StreetSpellingFuzzy,       // strict a échoué
    PostalCodeMismatch,        // (post-v1) rue trouvée mais au mauvais CP
    NoMatch,
}

/// Entrée principale du cœur. Pas de fonction async ici si on peut l'éviter,
/// le cœur reste synchrone, les façades gèrent l'async.
pub fn resolve(
    query: &AddressQuery,
    ctx: &ResolverContext,
) -> ResolvedAddress;

/// Le contexte porte les ressources que le cœur ne possède pas (index, abrev dict,
/// éventuel cache numéro médian). Passé par référence : zéro mutation, zéro lock.
pub struct ResolverContext<'a> {
    pub searcher: &'a Searcher,         // Tantivy searcher
    pub schema: &'a CrabSchema,
    pub abbrev: &'a AbbreviationMap,
    pub addr_counts: &'a AddressCountIndex,
    pub mid_street_lookup: Option<&'a MidStreetIndex>, // §5, v1
}
```

### 1.2 Contrat

- **In** : une requête structurée. Pour la recherche, `crabfinder-parser::parse(q)` produit aujourd'hui un `ParsedQuery` qui se mappe trivialement vers `AddressQuery` (deux ou trois champs identiques). Pour le batch, le caller construit directement l'`AddressQuery` (rue + CP en colonnes séparées → rien à parser).
- **Out** : une `ResolvedAddress`. **Toujours une** : pas de `Vec`, pas de `Option<ResolvedAddress>`. Si zéro hit, on rend `Confidence::NotFound` + `address: None`. Cela évite à chaque consommateur de gérer la branche « rien trouvé » différemment.
- **Pas de surcharge** : le cœur ne sait pas s'il sert un humain ou un CSV. Il ne fait pas de logging structuré HTTP. Il ne sait pas envoyer une réponse 200 ou 404, c'est la façade qui mappe `Confidence::NotFound` vers le code HTTP qu'elle veut (200 avec liste vide pour recherche, ligne enrichie avec flag pour batch).

### 1.3 Comment `/geocode` (recherche) consomme ce cœur

La recherche **a en plus** un besoin que le batch n'a pas : top-N ranké. Donc `/geocode` reste un peu plus complexe. Trois options :

**Option A (recommandée)** : `/geocode` n'utilise PAS `resolve()` directement. Il garde son pipeline actuel (build_strict_query → top_docs → dedup → top-N). Le cœur `resolve()` n'est consommé que par le batch. **Mais** les briques pures utilisées par `resolve()` (build_strict_query, build_fuzzy_query, dedup, focus_point decay, hydrate) sont **factorisées** dans des fonctions pures pour que `/geocode` et `resolve()` les appellent toutes les deux.

C'est l'option **réaliste**. Elle évite de réécrire `/geocode`. Elle place le cœur batch comme une **composition différente** des mêmes briques, sans casser le flux recherche.

**Option B** : `/geocode` appelle `resolve()` puis ré-élargit à un top-N via une variante `resolve_topn()`. Plus puriste, mais double l'API et duplique le travail (résoudre une fois pour rendre le best, exécuter à nouveau pour rendre le top-N).

**Option C (anti-pattern à écarter)** : le batch boucle sur `/geocode` en HTTP local. Coût latence par appel (~5 ms HTTP loopback) × 100k lignes = 500 s. Plus le payload JSON parsé puis re-sérialisé. Plus la perte de partage d'état (searcher cloné par appel, abbrev dict relu, etc.). **À refuser explicitement** : on n'optimise pas plus tard, on évite dès le départ.

### 1.4 Coût

Refactor existant : ~modéré. Ce qui est déjà pur (cf. inventaire crabfinder du briefing : `dedup.rs`, `spatial.rs` queries, `abbreviation.rs::expand`) reste tel quel. Ce qu'il faut sortir du handler `/geocode` :
- `build_strict_query` / `build_fuzzy_query` → module `query_builder.rs` (pure construction, retourne `Box<dyn Query>`).
- Décay focus_point → fonction pure `apply_focus_decay(hits: &mut [RawHit], focus: (f64, f64))`.
- Hydrate (lookup doc → Address) → reste contre Tantivy mais déplacé dans `resolver.rs` pour que les deux façades l'appellent.

Une fois ça fait, `resolve()` enchaîne : strict query → execute → si zéro fallback fuzzy → dedup (déjà pur) → prendre rep[0] → hydrate → mapper sur `Confidence` → si nécessaire, fallback house_number approx / mid_street → renvoyer.

Effort estimé : **2–4 jours** de refactor sans changer la sémantique externe. La grosse partie est de prouver via les tests/eval existants que `/geocode` n'a pas régressé après extraction.

### 1.5 Pourquoi cette frontière rend « même produit / deux produits » réversible

Une fois `resolve()` posé comme cœur pur :
- On peut commencer par un **endpoint HTTP batch** dans le même binaire `crabfinder-service` (façade très mince : reçoit JSON, appelle `resolve()` en boucle, renvoie CSV).
- Si plus tard on veut un binaire batch séparé (pas de port HTTP, juste un CLI pur), on factorise le cœur dans un crate (déjà fait via le module), on ajoute un `crabfinder-batch` bin qui dépend du cœur, ne dépend pas du service HTTP.
- Si on veut séparer les deux produits (deux index distincts, deux schemas), on duplique l'index, mais le cœur ne bouge pas.

Aucune de ces évolutions n'oblige à refactorer `resolve()`. Le coût de réversibilité est faible parce que la frontière est tracée à l'endroit qui sépare réellement le domaine (résoudre une adresse) de la livraison (HTTP, CSV, mémoire).

## 2. Formats d'entrée: recommandation priorisée

Le coût réel observé dans PhacochR : ~300 lignes de regex en `phaco_geocode.R:437-760` rien que pour le pré-nettoyage orthographique, plus ~200 lignes de regex de parsing pour les formats 2/3/5 (numéro-rue-CP intégrés). C'est un sous-projet en soi, et le code R est commenté comme « À FAIRE EN NL » à plusieurs endroits, même eux n'ont pas fini.

### 2.1 Recommandation

**v1 batch** : un seul format d'entrée, **colonnes propres séparées**.

```csv
id,street,house_number,postal_code
1,Avenue Louise,32,1050
2,Rue Belliard,71,1040
3,Grand-Place,,1000
4,Stationsstraat,5,2000
```

Colonnes : `id` (libre, conservé tel quel dans la sortie), `street` (string libre, tokenisée par crabfinder côté résolveur), `house_number` (optionnel), `postal_code` (string 4 chiffres). Encodage UTF-8. Délimiteur : virgule par défaut, semicolon supporté (les exports BE Excel par défaut). Le header est obligatoire.

C'est le format « idéal » selon PhacochR (« Situation idéale ! »), eux l'ont adopté comme la cible principale, et leurs quatre autres formats sont des **best-effort de parsing** vers cette cible.

**v1.1 batch (à valider après v1)** : ajouter le support « rue+num+CP intégrés en un champ ».

```csv
id,address
1,Avenue Louise 32 1050 Ixelles
```

C'est exactement l'entrée que `/geocode` reçoit aujourd'hui. Le résolveur batch peut alors :
- pour chaque ligne, appeler `crabfinder_parser::parse(address)` → ParsedQuery
- mapper vers AddressQuery
- appeler `resolve()`

**Aucun nouveau code de parsing requis**. C'est le free-text path qui existe déjà.

**v1.2 batch (peut-être jamais)** : les formats 2, 3, 5 de PhacochR (rue+num séparés mais avec boîtes parasites, ou rue+CP intégrés). C'est ce qui coûte cher chez PhacochR : pre-strip de « bte 5 », « n°71 », « BP 12 », commune en clair, etc. Beaucoup de cas edge documentés comme échouant (cf. tableaux 2/3/5 du README PhacochR avec leurs `:x:`).

### 2.2 Justification

Trois raisons de ne pas attaquer les colonnes sales en v1 :

1. **Le destinataire batch typique a accès à ses données structurées**. Le cas « j'ai un export crasseux et je veux qu'il devine » est réel mais minoritaire chez les utilisateurs sérieux d'analyse spatiale (typiquement leurs données viennent d'un SI qui sépare déjà num/rue/CP, ou ils peuvent les pré-séparer eux-mêmes).
2. **Le pre-strip est mal défini**. Lire `phaco_geocode.R:437-760` est une expérience : il y a des cas où PhacochR perd (« bte 5 - rue Belliard, 71 » donne 5 comme numéro de rue). Reproduire ce comportement et ses limites en Rust est gros et pas un net win.
3. **On a déjà un path free-text qui marche pour le cas v1.1**. Une ligne `"Avenue Louise 32 1050"` est traitée par `crabfinder-parser::parse` aujourd'hui ; en exposer le pipeline au batch est trivial.

### 2.3 Alternative écartée

« Tout supporter dès la v1 (5 formats) ». Rejeté pour : (a) double le périmètre v1 avec un travail dont la qualité dépend de cas-tests qu'on n'a pas, (b) c'est ce qui a fait grossir PhacochR de 800 à 1343 lignes pour des gains incrémentaux modestes, leur fiabilité reste sur le format 1, le reste est best-effort, (c) on peut ajouter v1.1 + v1.2 plus tard sans casser v1.

### 2.4 Coût v1 (format unique)

Sur le résolveur, **zéro effort de parsing** : on a déjà tout. Construire l'`AddressQuery` est mécanique :

```rust
let q = AddressQuery {
    street_tokens: tokenize(&row.street, &index),  // tokenizer Tantivy existant
    house_number: row.house_number.filter(|s| !s.is_empty()),
    postal_code: row.postal_code.filter(|s| valid_belgian_postal(s)),
    region_hint: None,
    focus_point: None,
};
```

Le seul vrai travail est : (a) lecture CSV (crate `csv` ou `serde_csv`), (b) validation par ligne avec accumulation des erreurs (pas de fail-fast), (c) écriture CSV en sortie. ~1–2 jours.

## 3. Contrat de sortie batch

### 3.1 Une décision par ligne

Différence fondamentale avec la recherche : pas de top-N. **Exactement une ligne de sortie par ligne d'entrée**, indépendamment du résultat (trouvé / approximé / pas trouvé). C'est le contrat PhacochR (`phaco_geocode.R:1148-1154` : `MISSING` ré-injecté pour garantir rowcount input == rowcount output), pas une coïncidence, c'est ce qui rend la sortie utilisable pour un join SQL ou un mapping spatial.

### 3.2 Schéma de sortie CSV

```csv
id,street,house_number,postal_code,                   # colonnes d'entrée, conservées
matched_street,matched_house_number,matched_postal_code,  # ce qui a été trouvé dans BeSt
lat,lng,                                              # WGS84
confidence,fallback_reason,house_number_delta,street_match_score,  # taxonomie
building_group_id,best_id                             # IDs pour traçabilité / dédup aval
```

Détails :

- `id` : passé tel quel. Indispensable pour que l'utilisateur réassocie avec son DB d'origine.
- `street/house_number/postal_code` : recopiés de l'entrée (audit trail).
- `matched_*` : ce que crabfinder a effectivement matché. Permet d'auditer les divergences (CP réécrit en cas d'élargissement commune adjacente, pour quand on l'implémentera ; rue détectée différente de la rue d'entrée, pour les cas fuzzy).
- `lat,lng` : WGS84. Pas de Lambert 72 par défaut (différent de PhacochR qui sort `x_31370/y_31370`). On peut ajouter une option `--coords lambert72|wgs84` plus tard. Belge ou pas, WGS84 reste la convention web.
- `confidence` : une des sept valeurs énumérées en §1.1, en lowercase string (`exact`, `nearest_same_side`, …, `not_found`).
- `fallback_reason` : optionnel, vide si `confidence == exact`. Sinon une étiquette courte (`house_number_missing`, `street_spelling_fuzzy`, etc.).
- `house_number_delta` : entier signé. 0 = numéro exact. ±N = approximation (négatif si numéro trouvé est plus bas, positif si plus haut). Vide si pas applicable.
- `street_match_score` : score BM25 normalisé (ou bucketisé, voir §3.3). Vide si `not_found`.
- `building_group_id` : pour l'utilisateur qui veut dédupliquer côté aval ou faire son propre groupement.
- `best_id` : l'identifiant officiel BeSt de l'adresse retenue (`namespace/oid/version`). Pour audit et liens vers OpenData BeSt.

### 3.3 Quel score de confiance émettre: discussion

PhacochR émet `dist_fuzzy` (entier 0–error_max, sémantique LCS edit-distance). Lisible mais discret et propre à leur méthode. crabfinder a BM25 (réel, échelle non bornée, dépend de la longueur du document).

Trois options :

**A. Émettre BM25 brut**. Lisible par personne. Rejeté.

**B. Émettre BM25 normalisé contre une référence**. Calculer en background un BM25 moyen sur l'eval set et exprimer chaque score comme un ratio. Coûteux à calibrer, sensible à la dérive des données.

**C. Bucketiser**. Sortir un entier ∈ {0, 1, 2, 3} où 0 = strict-match-top, 3 = fuzzy-bottom. Seuils empiriques calibrés sur l'eval crabfinder existante (`crabfinder-eval`). Simple à expliquer (« mon CSV a 12k lignes avec confidence=3, je les inspecte »).

**Recommandation : C**. La valeur de la confiance batch n'est pas qu'elle soit théoriquement informative, c'est qu'elle soit **actionnable par un humain qui trie un CSV**. Quatre buckets suffisent. Les seuils sont une donnée externe (config), pas une décision figée, on peut les retoucher post-déploiement sans casser le schéma.

### 3.4 Comportement sur zéro hit

**Recommandation** : émettre une ligne avec `lat/lng/matched_* = vide`, `confidence = not_found`, `fallback_reason = no_match`. Pas de best-effort numéro médian dans ce cas (la rue n'a pas matché → on ne sait pas quelle rue prendre comme médiane).

Cas distinct : la rue matche mais ni le numéro exact ni un voisin n'est trouvé. Si `mid_street` est activé (par défaut activé en v1), `confidence = median_street`, `fallback_reason = house_number_out_of_range` ou `house_number_missing`, `lat/lng = centroïde médian de la rue`. Si `mid_street` est désactivé via flag CLI/HTTP, comporte comme not_found mais conserve `matched_street`.

### 3.5 Déduplication d'adresses identiques avant résolution

**Recommandation** : oui. Si le CSV d'entrée a 50k lignes mais seulement 12k adresses uniques (`street, house_number, postal_code` triplet), résoudre 12k fois et propager. Win mesurable sur les CSV réels.

Implémentation : index `(street_lower, house_number, postal_code) → ResolvedAddress` calculé une fois, joint au flux d'entrée. Ne casse pas le contrat 1-in-1-out (chaque ligne d'entrée sort avec sa propre `id`).

### 3.6 Format(s) de sortie

**v1** : CSV uniquement. C'est ce que les utilisateurs d'analyse spatiale consomment (ouvrent dans QGIS, ArcGIS, Python pandas).

**v1.1** : option `--format geojson` pour sortir une FeatureCollection. Coût faible (~½ jour) ; à ajouter quand un utilisateur le demande.

**Pas en v1** : Parquet, Excel. Le premier n'apporte rien qu'un consommateur ne puisse faire lui-même via `csv → parquet` ; le second n'est pas un format standard et alourdit la dépendance.

## 4. Les deux interfaces: HTTP et CLI

Le briefing dit « endpoint HTTP ET CLI sont tous deux voulus ». OK. Ce sont **deux façades minces sur le cœur §1**. Aucune logique de domaine spécifique à l'une ou à l'autre.

### 4.1 Endpoint HTTP

```
POST /batch/geocode
Content-Type: multipart/form-data | application/json

# Multipart form :
file: <CSV file>
options: { "format": "csv", "include_score": true, "mid_street_fallback": true }

# Ou JSON :
{
  "addresses": [
    { "id": "1", "street": "Avenue Louise", "house_number": "32", "postal_code": "1050" },
    ...
  ],
  "options": { "format": "csv", ... }
}
```

Réponse : CSV (ou GeoJSON en v1.1) en `Content-Type: text/csv`, streamé ligne par ligne.

**Important : streaming**. La réponse n'attend pas que tout soit traité pour commencer à émettre. Pour 100k lignes ça change la perception utilisateur (premier byte ~quelques secondes vs 1–2 minutes), et ça évite de tenir 100k résultats en RAM côté serveur.

### 4.2 CLI

```
$ crabfinder-batch \
    --index ./index-belgium \
    --abbreviation-dict data/street-type-abbreviations-v1.yaml \
    --input addresses.csv \
    --output addresses-geocoded.csv \
    --format csv
```

Pas de port HTTP. Le binaire ouvre l'index Tantivy en lecture seule, instancie le résolveur, parcourt le CSV, écrit le résultat. Une commande, un fichier, un résultat.

Subtilité : le CLI **ouvre l'index lui-même**, il ne contacte pas un service HTTP qui tournerait à côté. Conséquence : `crabfinder-batch` peut tourner sans `crabfinder-service` actif, mais ne peut pas tourner si `crabfinder-service` détient un lock exclusif sur l'index. (Tantivy supporte la lecture multi-process sur le même index, ce devrait être OK, à vérifier sur la version en usage.)

Avantage : pas de couche réseau, pas d'auth, pas de cap port. Idéal pour les pipelines de data engineering.

### 4.3 Le même cœur derrière les deux

Les deux façades font littéralement :
```
1. Initialiser : ouvrir l'index, charger abbreviation dict, charger mid_street_lookup.
2. Lire les lignes d'entrée (HTTP stream / CSV file).
3. Pour chaque ligne : construire AddressQuery, appeler resolve(), formatter.
4. Émettre la ligne de sortie (HTTP stream / CSV file).
```

L'étape 3 est strictement identique. Les étapes 1, 2, 4 diffèrent uniquement par la source/destination des bytes, c'est l'ABC d'une façade.

### 4.4 Streaming vs lots: seuil

**Question** : pour 100k lignes, faut-il bash tout en mémoire, batcher par paquets, ou streamer ligne à ligne ?

**Recommandation** : streaming ligne à ligne au-dehors, avec parallélisme borné à l'intérieur. Le résolveur `resolve()` est thread-safe (lecture pure sur Tantivy + structures immuables), donc :

```rust
// Pseudo-code de la boucle batch
input
  .lines_stream()
  .map(parse_row)
  .buffered_unordered(N_WORKERS)         // typiquement N = num_cpus
  .map(|row| resolve(&row, &ctx))
  .for_each(|res| output.write_line(res))
  .await
```

Coût mémoire : O(N_WORKERS) résultats en vol, pas O(input_size). Latence du premier byte : ~quelques ms.

**Seuil de basculement « batch petit / batch gros »** : pas de bascule. Le pattern streaming marche pour 10 lignes comme pour 1M. Pas de mode séparé à concevoir.

(Cas particulier : si le CSV d'entrée fait > quelques GB, la HTTP request peut timeout côté reverse-proxy. Pour ce cas, le CLI est l'outil, pas HTTP. C'est la division naturelle des deux façades.)

### 4.5 Coût

- HTTP endpoint : ~2 jours (handler Axum + multipart parsing + CSV stream writer + intégration au résolveur).
- CLI : ~1 jour (clap + boucle stream + CSV in/out).
- Tests : ~1 jour (eval batch sur un dataset de 1k–10k lignes, comparer à l'eval recherche existante pour s'assurer qu'on ne régresse pas).

## 5. Périmètre v1 vs post-v1

Recommandation franche, classée par ratio bénéfice / effort. Plus c'est haut, plus c'est en v1.

### v1 (cible 2–3 sprints)

1. **Cœur `resolve()` pur** (§1). C'est la précondition. Sans ça, tout le reste est un patch.
2. **Format d'entrée colonnes propres séparées** (§2.1). Pas de pipeline de parsing crasseux.
3. **Endpoint HTTP `POST /batch/geocode` + CLI `crabfinder-batch`** (§4). Les deux façades sur le même cœur.
4. **Sortie CSV avec taxonomie de confiance** (§3.2 + §3.3). Sept valeurs `confidence`, `fallback_reason`, `house_number_delta`, `street_match_score` bucketisé.
5. **Fallback numéro médian de rue** (`Confidence::MedianStreet`, §3.4). PhacochR §2.1.
   - Implémentation : ajout d'un champ FAST `street_mid_lat/lng` dans le schéma (v7), précomputé à l'indexation par groupement `(street_id, postal_code)` → coordonnées du numéro médian par ordre statistique. Ou alternativement, side-file `mid-street.bin` mappé en mémoire au démarrage.
   - **C'est le gain qualité le plus net pour le batch.** À ne pas reporter.
6. **Déduplication des adresses identiques** (§3.5).

Effort total v1 : ~10–14 jours homme. Le post du résolveur (§1) est la part lourde.

### post-v1 (par ordre de priorité)

7. **Format d'entrée free-text intégré** (§2.1 v1.1). Réutilise `crabfinder-parser`. ~2 jours.
8. **Sortie GeoJSON optionnelle** (§3.6 v1.1). ~½ jour.
9. **Nearest-number-same-side (parité)** (PhacochR §2.2). Précomputer un side-index ou requête Tantivy filtrée par `street_id` au moment du fallback. ~3–5 jours selon implémentation. À évaluer en regardant si le mid_street + le fuzzy existant suffisent déjà sur des données réelles.
10. **Élargissement aux communes adjacentes** (PhacochR §2.3). Table d'adjacence + nouvelle passe résolveur quand strict + fuzzy ont échoué. ~3–5 jours + livraison de la table.

### Probablement jamais en v1 ou v2

11. **Enrichissement administratif Statbel/Urbis** (PhacochR §2.6). Nouvelle source de données, nouvelles dépendances spatiales, nouveau champ schéma. Effort plurisemaine, valeur ajoutée segmentée (utilisateurs ayant un usage analyse spatiale Statbel ; le reste est indifférent). À garder en backlog mais ne pas s'y engager.
12. **Parsing de formats sales (PhacochR formats 2/3/5)** (§2.1 v1.2). Cf. §2.2 : effort élevé, gain marginal après v1.1. Si jamais demandé, considérer une approche LLM légère plutôt que regex à 300 lignes.
13. **Anonymisation au centroïde de secteur statistique** (PhacochR `anonymous=TRUE`). Cas d'usage de niche (recherche académique sur données sensibles). Garder en backlog.

### Justification du choix v1

Le but de la v1 est de **livrer un produit batch utilisable**, pas un clone de PhacochR. Utilisable = :
- Format d'entrée stable et documentable.
- 1 ligne in → 1 ligne out avec lat/lng + une confiance compréhensible.
- Au moins un fallback significatif (median_street) pour ne pas perdre les lignes sans numéro.
- Deux interfaces (HTTP + CLI) pour deux contextes (intégration applicative, pipeline data).

C'est strictement moins que PhacochR fournit. C'est aussi strictement plus que ce que crabfinder fournit aujourd'hui (zéro batch). Et c'est tractable en quelques sprints sans déstabiliser le cas recherche en ligne, parce que le cœur partagé §1 garantit que l'on ne réécrit ni l'index, ni le parser, ni le ranking.

Le reste est de l'amélioration incrémentale, à prioriser à la lumière de retours d'utilisateurs réels du batch v1.
