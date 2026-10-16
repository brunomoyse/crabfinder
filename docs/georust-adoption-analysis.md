# GeoRust adoption analysis

> **Statut (2026-10-16)** : analyse du 2026-05-22, conclusions toujours valables : `geo` (Haversine) et `proj4rs` (Lambert 72) sont adoptés, `rstar` conservé, le reste écarté ou en réserve. Écarts constatés depuis dans le code :
>
> - Versions : `rstar` **0.13** (workspace), `proj4rs` **0.2.1** (indexer), `geo` 0.33 ; les numéros 0.12.2 / 0.1.10 ci-dessous sont ceux d'alors.
> - Les numéros de ligne `main.rs:NNN` sont périmés : `main.rs` est un wrapper mince. Les handlers sont dans `crates/crabfinder-service/src/http/` (`geocode.rs`, `reverse.rs`, `building.rs`, `lookup.rs`, `validate.rs`), `parse_focus_point` dans `focus.rs`, `haversine_m` dans `spatial.rs` (désormais via `geo::Haversine`). Le contrat GeoJSON de sortie reste sérialisé via `serde_json`, sans la crate `geojson`.
> - Le fichier `reproject.rs` (indexer) délègue bien à `proj4rs` avec la proj-string `+towgs84` IGN. Le schéma d'index est en v12 ; rien dans cette analyse n'en dépend.
> - OSM : `osmpbf` reste non pertinent. L'OSM d'aujourd'hui est un CSV optionnel (`--places` / `--pois`, désactivé par défaut) qui ne sert qu'à interpréter des requêtes ; il n'est jamais renvoyé ni stocké dans une adresse, BeSt étant l'unique source de résultats.

Audit du code géospatial maison de crabfinder face à l'écosystème GeoRust
(<https://georust.org/>). Objectif : remplacer ce qui est réinventé là où c'est
un gain net (correction, robustesse, maintenance), garder le reste maison là
où l'adoption coûte plus qu'elle ne rapporte.

État vérifié des crates au 2026-05-22 via l'API crates.io (versions et dates
de dernière release). Lambert 72, distance, et l'enveloppe GeoJSON de sortie
sont les trois zones où le code maison est le plus exposé ; le reste est
soit déjà via une crate GeoRust (rstar), soit hors-scope.

---

## 1. Cartographie de l'existant

### 1.1 Reprojection Lambert 72 → WGS84 (EPSG:31370 → EPSG:4326)

- **Fichier** : `crates/crabfinder-indexer/src/reproject.rs`.
- **Implémentation actuelle** (depuis cette analyse) : délègue à
  **`proj4rs` 0.1.10** avec une proj-string explicite portant les paramètres
  IGN `+towgs84=-106.869,52.2978,-103.724,0.3366,-0.4570,1.8422,-1.2747`.
- **Implémentation précédente** : LCC 2SP inverse (Snyder 1987, ellipsoïde
  Hayford International 1924) → géodésique→géocentrique → Helmert
  7-paramètres (BD72 ↔ WGS84) → géocentrique→géodésique (Bowring 1985).
  Environ 200 lignes de code maison.
- **Diagnostic corrigé** : le commentaire d'origine
  (« proj4rs produces results ~200 m off because the 7-parameter Helmert is
  not fully applied ») était **inexact**. Une enquête a démontré que :
  - avec `Proj::from_user_string("EPSG:31370")` (chaîne fournie par
    `crs-definitions` 0.4.0), proj4rs est à **~107 m** du résultat correct ;
  - **avec la même proj-string augmentée de `+towgs84=...`**, proj4rs et le
    code maison s'accordaient à **<1 mm** sur 4 points de référence
    (epsg.io publié, adresse réelle Anderlecht, baseline Brussels, Anvers) ;
  - le bug attribué à proj4rs était donc un manque de **paramètres** dans
    la chaîne EPSG embarquée, pas dans l'algorithme de proj4rs.
- **Tests** : 3 tests d'intégration (paire de référence epsg.io,
  Anderlecht, propagation 100 m est), inchangés, tous verts après la
  migration.
- **Usage** : appelée une seule fois par adresse au moment de l'indexation,
  dans `crates/crabfinder-indexer/src/pipeline.rs:102` et `:180`. Jamais à la
  requête. Le service ne reprojette rien, les coordonnées sont WGS84 à la
  lecture (`crabfinder-indexer/src/index.rs:200-202` refuse explicitement le
  cas Lambert 72 résiduel comme un bug pipeline).

### 1.2 Calcul de distance grand-cercle (Haversine)

- **Fichier** : `crates/crabfinder-service/src/spatial.rs:201-209`,
  fonction `haversine_m`.
- **Implémentation** : Haversine sous forme `atan2`, numériquement stable
  pour les points antipodaux.
- **Rayon terrestre** : `EARTH_RADIUS_M = 6_371_000.0` (`spatial.rs:29`),
  arrondi délibérément à 6 371 000 m « for round-trip stability with other
  geocoder vendors ». Le rayon IUGG/géo crate canonique est 6 371 008.7714 m.
  L'écart est ~1,37 ppm (≈ 1,4 m sur 10 km).
- **Tests** : 3 tests (`spatial.rs:227-248`), Londres↔Paris (~343,5 km),
  identité à 0, courte distance ~111 m. Robustes mais peu nombreux ; pas de
  test antipodal direct (le commentaire revendique la robustesse, le test ne
  l'exerce pas).
- **Usage** :
  1. `/reverse`, distance reportée au client (`main.rs:912-939`,
     `:953`).
  2. R-tree → tri final (`spatial.rs:169-194`).
  3. `/geocode` focus_point, exp(−d/25 km) (`main.rs:298`, `:648`).

### 1.3 Index spatial (R-tree)

- **Fichier** : `crates/crabfinder-service/src/spatial.rs:39-198`.
- **Implémentation** : **`rstar` 0.12.2** (déjà une crate GeoRust).
  `RTreeObject` + `PointDistance` implémentés à la main avec une distance
  planaire `(lon, lat)` au carré. Construit par `bulk_load` une fois au boot
  (`spatial.rs:113`, ~838 k points aujourd'hui, ~6,8 M visés au national).
- **Particularité** : le R-tree classe par distance planaire (lon×lat
  euclidien) ; à 50°N le ratio lat/lon ≈ 1,56 introduit du bruit d'ordre.
  Crabfinder compense par **oversearch 2×** puis re-tri Haversine
  (`spatial.rs:152-197`). C'est juste, calibré, et documenté.
- **Tests** : 9 tests (`spatial.rs:226-348`) couvrant near-N, radius,
  out-of-region, radius=0, query vide.

### 1.4 Sortie GeoJSON

- **Fichier** : `crates/crabfinder-service/src/main.rs:323-401`,
  `:735-755`, `:825-832`, `:966-980`, `:1186-1199`.
- **Implémentation** : structures Rust **sérialisées via `serde_json`**, pas
  via la crate `geojson`. Trois `FeatureCollection` distincts :
  `/geocode` (`main.rs:323`), `/building/{id}/units` (JSON ad-hoc via
  `serde_json::json!` à `main.rs:825`), `/reverse` (`main.rs:861`).
- **Conformité RFC 7946** : `geometry.type = "Point"`, coordonnées
  `[lon, lat]` (`main.rs:399-400`), `Feature.type = "Feature"`,
  `FeatureCollection.type = "FeatureCollection"`. Champs `properties`,
  `score`, `match_type`, `unit_count`, `building_group_id`,
  `distance_m` sont passés à plat dans le Feature ou en top-level, la
  RFC autorise « foreign members » mais conventionnellement on les met
  sous `properties`. Le placement actuel mélange members spec (`type`,
  `geometry`, `properties`) et foreign members (`score`, `match_type`,
  `unit_count`, `building_group_id`, `distance_m`) au même niveau, ce qui
  est non-conforme strict mais largement toléré par les consommateurs.

### 1.5 Autre code « géométrique »

- **Bounding box Belgique** : `WgsCoord::is_in_belgium`
  (`crates/crabfinder-core/src/coordinates.rs:31-33`), `lat ∈ [49.4, 51.6]`,
  `lon ∈ [2.5, 6.5]`. Trivial, pas de centroïde, pas de polygone, pas de
  parité de côté de rue, pas d'union, pas d'intersection.
- **Sanity focus_point** : `parse_focus_point` (`main.rs:277-291`) :
  bbox NW Europe `[49,52]×[2,7]`, du même tonneau.
- **Pas d'utilisation** : aucune géométrie polygonale, aucun centroïde,
  aucune simplification, aucun calcul d'aire, aucune ligne/polyligne,
  aucun WKB/WKT, aucune projection autre que Lambert 72.

### 1.6 Dépendances actuelles (Cargo.lock vérifié)

- **Effectivement liées (après cette analyse)** :
  - `rstar` 0.12.2, index spatial.
  - `geo` 0.33.1, `Haversine.distance(...)` (service).
  - `proj4rs` 0.1.10, `Lambert72 → WGS84` (indexer).
- **Nettoyé** : `geo-types` n'est plus listé directement dans
  `[workspace.dependencies]` (encore présent en transitif via `geo`).

### 1.7 État des crates GeoRust pertinentes (vérifié 2026-05-22)

| Crate              | Version    | Dernière release | Téléchargements | Maturité |
|--------------------|------------|------------------|-----------------|----------|
| geo                | 0.33.1     | 2026-04-20       | 16,7 M          | Mature, actif |
| geo-types          | 0.7.19     | 2026-04-14       | 19,7 M          | Mature, actif |
| rstar              | 0.12.2     | 2024-11-05       | 26,6 M          | Mature, stable |
| geo-index          | 0.3.4      | 2026-02-27       | 65 k            | Jeune mais actif |
| geojson            | 1.0.0      | 2026-03-16       | 8,6 M           | **Stable 1.0** |
| geozero            | 0.15.1     | 2025-12-11       | 942 k           | Mature, actif |
| geohash            | 0.13.1     | 2024-03-13       | 2,5 M           | Mature, peu de churn |
| proj               | 0.31.0     | 2025-08-29       | 594 k           | Wrap C libproj |
| proj4rs            | 0.1.10     | 2026-03-06       | 448 k           | Pre-1.0, pure Rust |
| geographiclib-rs   | 0.2.7      | 2026-02-17       | 14,2 M          | Mature, pure Rust |
| geo-postgis        | 0.2.2      | 2022-08-04       | 31 k            | Quasi-inactif |
| geo-svg            | 0.8.0      | 2025-07-14       | 37 k            | Niche mais maintenu |
| osmpbf             | 0.3.8      | 2025-10-24       | 137 k           | Mature |
| wkb                | 0.9.2      | 2025-11-21       | 686 k           | Mature |
| wkt                | 0.14.0     | 2025-05-14       | 6,7 M           | Mature |
| polyline           | 0.11.0     | 2024-05-13       | 378 k           | Mature, stable |
| geos               | 11.1.1     | 2026-03-03       | 596 k           | Wrap C libgeos |
| gdal               | 0.19.0     | 2025-12-23       | 3,6 M           | Wrap C libgdal |
| ogcapi             | 0.3.1      | 2025-04-05       | 4 k             | Très jeune |

---

## 2. Revue crate par crate

### 2.1 `rstar` 0.12.2: index spatial R*-tree

- **Recommandation** : **GARDER** (déjà adoptée).
- **Justification** : `spatial.rs` l'utilise correctement avec un correctif
  Haversine documenté. Pas de meilleure alternative pour un index in-memory
  généraliste. C'est l'exemple-type de « code maison qui marche, on garde ».
- **Coût** : nul (déjà en build).

### 2.2 `geo` 0.33.1: primitives + algorithmes (Haversine, Geodesic, …)

- **Une phrase** : primitives géométriques (Point, LineString) et
  algorithmes (distance Haversine/Geodesic/Vincenty, centroïde, convex
  hull, simplification, …). Pure Rust.
- **Recommandation** : **ADOPTER pour `haversine_m` ET RIEN D'AUTRE**.
  Le seul gain mesurable est de remplacer la fonction `haversine_m`
  (`spatial.rs:201-209`, 9 lignes) par `Haversine.distance(p1, p2)`. Le
  reste de la crate ne sert à rien à crabfinder aujourd'hui.
- **Justification** : Haversine est un classique réimplémenté ; même si la
  formule est juste, elle a des coins (atan2 pour antipodal, choix du
  rayon). Adopter `geo` :
  - aligne le rayon sur IUGG 6 371 008.7714 m (au lieu de 6 371 000.0
    arrondi pour interop), **changement de comportement de ~+1,4 m sur
    10 km** ;
  - donne accès gratuit à `Geodesic` (Karney 2013, exact à mieux que
    1 mm sur l'ellipsoïde WGS84) si un jour on veut une distance plus
    précise que Haversine en Belgique (où Haversine se trompe d'environ
    0,3 % vs géodésique parce que la Terre est aplatie aux pôles) ;
  - retire 9 lignes de code maison qu'il faudra continuer à maintenir.
- **Coût** : pure Rust, pas de dépendance C. Build : `geo` tire `geo-types`,
  `num-traits`, `robust`, `rstar` (déjà là), `serde`, `spade` ; ~10
  sous-crates supplémentaires, compile time modeste (<10s sur cold).
  Risque : le rayon canonique diffère du nôtre. Pour préserver la stabilité
  externe annoncée dans le commentaire `spatial.rs:27-29`, soit on
  enveloppe `Haversine` dans une fonction qui re-scale le résultat par
  `6_371_000.0 / 6_371_008.7714`, soit on accepte le ~1,4 m/10 km d'écart et
  on met à jour le commentaire. La 2e voie est plus honnête.
- **Esquisse** :

  ```rust
  use geo::{Haversine, Distance, point};
  pub fn haversine_m(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
      Haversine.distance(point!(x: lon1, y: lat1), point!(x: lon2, y: lat2))
  }
  ```

  Tests existants (`spatial.rs:227-248`) passeront : la tolérance
  Londres↔Paris est ±1 000 m, on bouge de ~0,5 m.

### 2.3 `geo-types` 0.7.19: Point, LineString, Coord (types nus)

- **Recommandation** : **NON PERTINENT** seule, **TIRÉ INDIRECTEMENT** par
  `geo` si on adopte 2.2. Crabfinder n'a pas de besoin direct de
  `Point<f64>`, `lat`/`lon` en `f64` séparés (struct `WgsCoord`) suffisent
  partout. Migrer toutes les API à `geo_types::Point` est de la cosmétique
  qui ne corrige aucun bug.
- **Coût** : nul si on prend `geo`, mais aucun gain à le faire seul.

### 2.4 `geojson` 1.0.0: sérialisation/désérialisation GeoJSON

- **Une phrase** : types `Feature` / `FeatureCollection` / `Geometry`
  conformes RFC 7946, avec sérialisation via serde.
- **Recommandation** : **NE PAS ADOPTER**. Garder en réserve si on
  ajoute un endpoint qui accepte du GeoJSON en entrée.
- **Justification** : c'est précisément le cas-type « ça marche et c'est
  testé, le gain est faible ». La sortie actuelle est `Feature` /
  `FeatureCollection` à plat via `serde_json` (`main.rs:374-401`,
  `:735-755`), bien testée par les tests d'intégration (les tests Phase 3/4
  inspectent chaque champ via `serde_json::Value`). Trois écueils
  réels néanmoins, qu'adopter `geojson` corrigerait :
  1. **Foreign members au mauvais niveau** : `score`, `match_type`,
     `unit_count`, `building_group_id`, `distance_m` sont au niveau Feature,
     alors que RFC 7946 §6 (foreign members) les tolère mais préfère sous
     `properties`. La crate `geojson` met les foreign members exactement où
     la spec les autorise, adopter forcerait à choisir consciemment.
     **Mais** : le format actuel est un contrat externe ; le casser pour
     « se conformer à la spec » est un breaking change client. À ne pas
     faire à la légère.
  2. **`/building/{id}/units` mélange ad-hoc JSON et Feature**
     (`main.rs:825-832`), `geojson::FeatureCollection` permettrait de
     mettre `building_group_id` et `_note` en foreign members propres ;
     mais c'est du polish, pas un bug.
  3. **Coordinates `[lon, lat]`**, déjà correctes (`main.rs:399-400`),
     `geojson` ne le ferait pas mieux.
- **Coût** : pure Rust. Migration : ~150 lignes touchées dans `main.rs`,
  re-validation contre la suite de tests Phase 3/4 + clients HTTP existants.
  Risque non-nul de régression sur les payloads externes.
- **Verdict** : refactor cosmétique. Si on touche les types de sortie
  pour autre chose, on en profitera ; sinon le code maison passe les
  tests et le coût de migration n'est pas justifié.

### 2.5 `geohash` 0.13.1: encodage geohash

- **Une phrase** : encode/decode geohash (préfixe alphanumérique
  emboîtable d'une boîte WGS84).
- **Recommandation** : **GARDER EN RÉSERVE** (post-v1, conditionnel à
  un signal RAM/scale).
- **Justification** : option (c) écartée en Phase 4, au lieu d'un R-tree
  in-memory, indexer un champ `geohash` dans Tantivy puis prefix-query
  pour le proximity search. Aujourd'hui le R-tree consomme 24 octets/point
  × ~840 k pts = ~20 MB ; à l'échelle nationale (6,8 M docs) on est à
  ~160 MB. C'est encore confortable. Le jour où l'empreinte mémoire au
  démarrage devient un problème (containers serrés, plusieurs index
  collocalisés, etc.), `geohash` débloque la voie « pas de structure
  in-memory du tout, tout passe par Tantivy ».
- **Coût** : pure Rust, micro-crate. Mais c'est une refonte majeure de
  `/reverse`, `O(log n)` du R-tree → prefix-scan + filtre Haversine sur
  une boîte qui peut être large. Ne pas s'engager avant le signal.

### 2.6 `proj` 0.31.0: bindings libproj

- **Une phrase** : wrap C libproj (le standard de fait pour les
  transformations CRS).
- **Recommandation** : **NE PAS ADOPTER**.
- **Justification** : `proj4rs` 0.1.10 fait déjà le job en pure Rust avec
  une équivalence numérique <1 mm vs le code maison historique sur les
  4 points de référence (cf. § 2.7). Adopter `proj` :
  - **introduit une dépendance C** (libproj ≥ 9.x avec ses datums, sa
    base SQLite proj.db, ses fichiers de grilles NTv2) → casse le « single
    binary, no system deps » qui est une propriété d'architecture
    explicite (`README.md`, `docs/architecture.md`) ;
  - apporte la précision et la généralité dont crabfinder n'a aucun
    besoin (toute la Belgique, un seul CRS source).
- **Coût** : énorme côté déploiement (paquets système libproj + assets).
  Gain : nul vs `proj4rs` une fois la chaîne `+towgs84` correctement
  passée.

### 2.7 `proj4rs` 0.1.10: réimplémentation pure-Rust de Proj4

- **Recommandation** : **ADOPTÉ** (depuis cette analyse, en remplacement
  de `reproject.rs` maison).
- **Justification** : enquête menée sur 4 points de référence Belgique
  (epsg.io publié, adresse réelle Anderlecht, baseline Brussels, Anvers) :
  - avec la chaîne EPSG:31370 par défaut (via `crs-definitions`), proj4rs
    est **~107 m** off, c'est exactement le bug que le commentaire
    original imputait à proj4rs ;
  - avec une proj-string explicite portant les paramètres IGN
    `+towgs84=-106.869,52.2978,-103.724,0.3366,-0.4570,1.8422,-1.2747`,
    proj4rs et le code maison s'accordent à **<1 mm**. L'algorithme
    proj4rs **est correct** ; c'était la définition embarquée qui omettait
    le datum shift.
  - Conséquence : on supprime ~200 lignes d'algorithmes maison
    (LCC 2SP + Helmert + Bowring), on délègue à une crate maintenue
    (mainteneur 3liz, dernière release mars 2026), et on garde les 3 tests
    de référence existants comme garde-fou (paire epsg.io ±1e-4°,
    Anderlecht en bbox, propagation 100 m).
- **Coût** : pure Rust → binaire autonome préservé. +`crs-definitions` en
  transitif (lookup table EPSG, ~quelques centaines de Ko de strings).
  Risque : si proj4rs publie une 0.2 breaking, on bouge nos 5 lignes
  d'usage. Pas pire que n'importe quelle autre dep.

### 2.8 `geographiclib-rs` 0.2.7: algorithmes géodésiques Karney

- **Une phrase** : port Rust pur de GeographicLib (géodésique exacte sur
  l'ellipsoïde, formules Karney 2013).
- **Recommandation** : **NON PERTINENT** seule. Sa fonctionnalité utile
  (distance géodésique exacte) est exposée par `geo::Geodesic`, qui est
  déjà sous-jacente à `geographiclib-rs`. Si on a un jour besoin d'une
  distance exacte, on passe par `geo::Geodesic` (cf. 2.2), pas par
  `geographiclib-rs` directement.
- **Coût** : pure Rust, mais redondant avec `geo`.

### 2.9 `geo-index` 0.3.4: index spatial immutable zero-copy

- **Une phrase** : index spatial (R-tree ou KD-tree) packé,
  immuable, zero-copy mmap-able.
- **Recommandation** : **NON PERTINENT** à court terme, **GARDER EN
  RÉSERVE** si la construction du R-tree au démarrage devient un goulot
  d'étranglement.
- **Justification** : aujourd'hui `rstar::bulk_load` reconstruit l'index
  à chaque démarrage du service en RAM (`spatial.rs:113`). C'est un coût
  acceptable (quelques secondes pour ~840 k points). Si à 6,8 M points
  ce coût ou l'empreinte mémoire devenait problématique, `geo-index`
  permettrait de **persister** un index packé à côté de Tantivy et de le
  charger en mmap, démarrage instantané. Mais : crate jeune (0.3.x,
  ~65 k downloads), API moins stable que `rstar`.
- **Coût** : remplace `rstar` au runtime, double l'artéfact disque
  (index Tantivy + index spatial), refonte de `SpatialIndex::build_from_tantivy`.

### 2.10 `geo-postgis` 0.2.2

- **Recommandation** : **NON PERTINENT**.
- **Justification** : crabfinder n'a pas de PostGIS. Index = Tantivy.
  Crate inactive depuis 2022 par ailleurs.

### 2.11 `geo-svg` 0.8.0

- **Recommandation** : **GARDER EN RÉSERVE** (debug/validation).
- **Justification** : l'analogue Rust de `mapsf` de PhacochR.
  Outil de validation visuelle (rendre une page SVG avec les hits du jour,
  les BBox de cantons, etc.) qu'un futur outil de debug pourrait utiliser
  en hors-ligne. Pas un besoin v1. Si jamais on veut auditer
  visuellement des résultats de `/reverse` ou des erreurs de
  reprojection, c'est la crate à atteindre.
- **Coût** : pure Rust, niche. Aucun coût si on ne l'utilise pas.

### 2.12 OSM (`osmpbf` 0.3.8 / `osmpbfreader` 0.19.1)

- **Recommandation** : **NON PERTINENT**.
- **Justification** : crabfinder n'ingère que des données BeSt
  (XML zippé), pas d'OSM PBF. Si un jour on ajoute un fallback OSM pour
  combler les trous de BeSt (rare en Belgique), `osmpbf` est la voie.

### 2.13 `wkb` 0.9.2 / `wkt` 0.14.0

- **Recommandation** : **NON PERTINENT**.
- **Justification** : aucune entrée/sortie binaire ou texte de
  géométries (le service consomme XML BeSt et émet GeoJSON). Si on
  exposait un /export pour des outils GIS desktop, `wkt` deviendrait
  pertinent, pas un besoin v1.

### 2.14 `polyline` 0.11.0

- **Recommandation** : **NON PERTINENT**.
- **Justification** : pas de polylignes (pas de routing, pas de tracé).
  Pure géocodage ponctuel.

### 2.15 `geos` 11.1.1

- **Recommandation** : **NON PERTINENT**.
- **Justification** : wrap C libgeos. Géométrie avancée
  (union, intersection, buffer), non utilisée et **introduirait une
  dépendance système C** comme `proj`.

### 2.16 `gdal` 0.19.0

- **Recommandation** : **NON PERTINENT**.
- **Justification** : wrap C libgdal. Raster/vector I/O multiformat.
  Crabfinder n'écrit ni ne lit de Shapefile, GeoPackage, GeoTIFF, etc.
  Dépendance système C massive.

### 2.17 `ogcapi` 0.3.1

- **Recommandation** : **NON PERTINENT**.
- **Justification** : building blocks OGC API (Features, Tiles, Maps).
  Crabfinder expose une API REST custom autour de GeoJSON, pas une OGC
  API Features. Et la crate est très jeune (~4 k downloads).

### 2.18 `geozero` 0.15.1

- **Recommandation** : **NON PERTINENT**.
- **Justification** : abstraction zero-copy entre formats géospatiaux.
  Utile pour les pipelines multi-format. Crabfinder a UN format en entrée
  (BeSt XML) et UN format en sortie (GeoJSON). L'abstraction n'achète
  rien.

### 2.19 Crates raster / nuage de points / autres

- `geotiff`, `netcdf`, `pdal`, `world-file`, `transit`, `tilejson`,
  `topojson`, `gpx`, `kml`, `earcut`, `robust`, `geocoding` :
  **NON PERTINENT, raster/point-cloud/format-tiers hors scope géocodage
  vecteur d'adresses ponctuelles**. `geocoding` en particulier est un
  client vers Mapbox/OpenCage/etc., l'opposé de ce qu'on construit.

---

## 3. Synthèse priorisée

Trié par rapport gain/coût décroissant. La conclusion finale, après
enquête : **deux adoptions sérieuses** (`geo` pour Haversine,
`proj4rs` pour la reprojection), tout le reste reste maison ou en
réserve.

| Crate                | Recommandation             | Gain                                                                 | Coût                                                                                   | Effort | Priorité  |
|----------------------|----------------------------|----------------------------------------------------------------------|----------------------------------------------------------------------------------------|--------|-----------|
| **`geo`** (Haversine seul) | **ADOPTÉ**           | Supprime 9 lignes de Haversine maison, aligne sur IUGG, donne accès à `Geodesic` pour le futur. | Pure Rust, +1,4 m/10 km de drift, ~10 sous-crates au build. | ~1h | **fait**  |
| **`proj4rs`** (Lambert 72) | **ADOPTÉ**           | Supprime ~200 lignes de LCC + Helmert + Bowring maison. Équivalence <1 mm prouvée sur 4 points de référence. | Pure Rust → binaire autonome préservé. Nécessite une proj-string explicite avec `+towgs84` (la chaîne EPSG par défaut omet le datum shift). | ~2h | **fait**  |
| `rstar` (déjà là)    | GARDER                     | n/a | n/a | 0      | déjà fait |
| `geojson`            | NE PAS ADOPTER             | Foreign-members rangés selon RFC 7946, code de sortie plus déclaratif. | Refactor ~150 lignes, risque de breaking change du contrat HTTP, gain de correction faible. | ~1j   | non       |
| `geohash`            | **GARDER EN RÉSERVE**      | Permet de remplacer le R-tree in-memory par un prefix-scan Tantivy. | Refonte majeure de `/reverse`.                                                          | ~3-5j | post-v1, conditionnel |
| `geo-index`          | GARDER EN RÉSERVE          | Index spatial persistant, démarrage instantané.                      | Crate jeune, double artéfact disque.                                                   | ~2-3j | post-v1, conditionnel |
| `geo-svg`            | GARDER EN RÉSERVE          | Outil de debug visuel.                                              | Zéro tant qu'inutilisée.                                                                | 0     | nice-to-have |
| `proj`               | NE PAS ADOPTER             | Précision théorique, multi-CRS.                                     | **Casse le binaire autonome** (libproj C + proj.db). Redondant avec proj4rs sur notre seule transformation. | n/a    | non       |
| `geographiclib-rs`   | NE PAS ADOPTER             | Redondant avec `geo::Geodesic`.                                     | n/a | n/a    | non       |
| `geos`, `gdal`       | NE PAS ADOPTER             | n/a | **Dépendances C** lourdes, fonctionnalités non utilisées.                              | n/a    | non       |
| `geo-types` (seul)   | NE PAS ADOPTER             | Cosmétique.                                                          | n/a | n/a    | non       |
| `geozero`            | NE PAS ADOPTER             | n/a | n/a | n/a    | non       |
| `geo-postgis`        | NE PAS ADOPTER             | n/a | Inactive, hors-scope.                                                                  | n/a    | non       |
| `wkb`, `wkt`, `polyline`, `ogcapi` | NE PAS ADOPTER | n/a | n/a | n/a    | non       |
| `osmpbf`             | NE PAS ADOPTER             | n/a | Hors-scope (pas d'OSM).                                                                | n/a    | non       |
| Raster / pt-cloud (`geotiff`, `netcdf`, `pdal`, `world-file`) | NE PAS ADOPTER | n/a | Hors-scope géocodage vecteur. | n/a | non |

---

## 4. Réponse à la question à trancher

> Parmi tout le code géospatial maison de crabfinder, lequel est le plus
> à risque (algorithme classique réimplémenté, faiblement testé, ou
> subtilement faux) et donc le meilleur candidat au remplacement par
> une crate testée ?

**Le candidat est `haversine_m` dans `spatial.rs:201-209`, adopter
`geo::Haversine`.** Argumentation par le risque réel :

- **Algorithme classique** : Haversine est un grand classique avec des
  cas-bord connus (instabilité numérique sur les antipodes, choix du
  rayon, perte de précision à courte distance). La version maison
  utilise la forme `atan2` qui adresse l'antipodal, mais aucun test
  ne l'exerce, le commentaire `spatial.rs:207` revendique la
  robustesse, les tests `spatial.rs:227-248` valident Londres↔Paris,
  identité et 111 m, point.
- **Choix du rayon non-standard** : 6 371 000.0 m vs IUGG 6 371 008.7714 m
  (`spatial.rs:29`). La justification (« round-trip stability with other
  geocoder vendors ») est plausible mais non vérifiée, quels vendors,
  exactement ? Si c'est juste de la coïncidence numérique sur des seuils
  de tests, ça coûte ~1,4 m/10 km de drift permanent. Aligner sur le
  standard est plus défendable que de figer un arrondi historique.
- **Volume d'usage** : trois call-sites (`spatial.rs:169`, `main.rs:648`,
  re-tri R-tree). Toute imprécision se propage à `/reverse` (la valeur
  reportée au client), à `/geocode` focus_point (le re-scoring),
  et à la calibration de l'oversearch R-tree.
- **Coût de remplacement** : minimal. La signature de `geo::Haversine`
  s'enveloppe en 3 lignes ; le rayon différent change le résultat de
  ~1,4 m sur 10 km, invisible pour `/reverse` (la résolution BeSt est
  pire que ça) et pour le focus_point (half-life 25 km, le delta est
  noise).

**Addendum après enquête sur la reprojection** :

L'analyse initiale écartait `proj4rs` en s'appuyant sur le commentaire
historique de `reproject.rs` (« Helmert pas pleinement appliqué,
décalage ~200 m »). En reprenant la mesure sur 4 points de référence,
ce commentaire s'est révélé **inexact** : le bug venait de la chaîne
EPSG:31370 par défaut (qui omet `+towgs84`), pas de l'algorithme de
proj4rs. Avec une proj-string explicite portant les paramètres IGN,
proj4rs et le code maison s'accordent à <1 mm. C'est la deuxième
adoption justifiée : ~200 lignes d'algorithmes classiques
(LCC 2SP + Helmert + Bowring) supprimées, mêmes tests, même précision,
toujours en pure Rust → binaire autonome conservé.

**Adoptions finales** : `geo::Haversine` (runtime) + `proj4rs`
(indexation). Tout le reste reste maison ou en réserve documentée
(`geohash`, `geo-index`, `geo-svg`). La leçon générale, dans le cas de
la reprojection : un commentaire historique justifiant un rejet doit
être re-mesuré périodiquement, surtout quand la crate a continué à
publier (proj4rs 0.1.8/0.1.9/0.1.10 entre temps).
