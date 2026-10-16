# crabfinder vs PhacochR

> **Statut (2026-10-16)** : l'analyse (§2 à §6) date de l'avant-batch et lit le code PhacochR v0.9.1.14 ; elle est conservée. Le tableau du §1 et la liste du §3.7 décrivaient crabfinder à l'époque du schéma v6 ; la mise à jour est ci-dessous (schéma d'index **v12**, BeSt seul).
>
> **Ce que crabfinder fait aujourd'hui des points que PhacochR avait en plus** :
>
> | Point (section) | État actuel |
> |---|---|
> | Numéro médian de rue (§2.1) | Livré : `Confidence::MedianStreet`, side-file `mid-street-v1.bin` |
> | Numéro voisin, parité (§2.2) | Livré : `nearest_same_side` puis `nearest_any_side` |
> | Communes adjacentes (§2.3) | Livré : `adjacent_commune` (relaxation du code postal, la plus proche d'abord) |
> | Taxonomie de confiance (§2.4) | Livré : dix valeurs `confidence`, `fallback_reason`, `house_number_delta`, `street_match_score`, colonnes batch dédiées |
> | Entrée en colonnes (§3.7) | Livré : `id,street,house_number,postal_code`, ou `id,address`, ou `best_id` (CSV `,` ou `;`) |
> | 1 ligne in, 1 ligne out (§3.7) | Livré : `POST /batch/geocode` et CLI `crabfinder-batch` |
> | Enrichissement Statbel / Urbis (§2.6) | Non fait, et hors périmètre : BeSt reste la seule source de résultats |
>
> **Ce que crabfinder a en plus depuis** : `/validate` et les colonnes batch `status` / `corrections` (ce qui diffère entre la saisie et l'adresse officielle) ; `alternatives` groupées par commune pour les saisies ambiguës ; `/lookup` et `best_id` avec recherche d'adresses retirées et successeur (`history-v1.json.gz`, ids indépendants de la version) ; interprétation des lieux-dits (et, avec `--pois`, des monuments) par aide OSM optionnelle, désactivée par défaut, qui ne fait qu'orienter la recherche (chaque résultat est une adresse BeSt) ; imputation des ~140 000 adresses wallonnes sans position.
>
> **Mesure** (`reports/benchmark-2026-10-16.md`, 1 510 adresses réelles issues de listes open data, point de référence publié par l'éditeur, succès si à 100 m ou moins) : crabfinder **0,929** contre PhacochR **0,907** (médiane 5 m contre 7 m, p90 72 m contre 77 m, 100 % trouvées contre 97,9 %) ; NGI 0,836, Bruxelles 0,925 (496 requêtes bruxelloises), Wallonie 0,847 (550), Flandre 0,505 (960). Les jeux synthétique et adversarial favorisent crabfinder (recall@1 0,965) et sont moins parlants. PhacochR traite le lot en une passe R (19,8 ms par requête en moyenne), crabfinder répond en 2,1 ms (p50) en local.

Analyse comparative basée sur la lecture du code de [phacochr](https://github.com/phacochr/phacochr) v0.9.1.14 (clone GPLv3 hors arbre, `/tmp/phacochr`) et du code crabfinder à la racine de ce repo. Toutes les références « file:line » renvoient au code réel, pas aux documents marketing.

PhacochR est, à notre connaissance, le seul géocodeur belge libre qui adresse spécifiquement le batch sur données réelles et sales (16 ans de pratique académique, dataset Statbel/BeSt, 18 bases de production internes utilisées en validation). Il est donc la référence pertinente pour informer la conception batch de crabfinder, pas pour comparaison concurrentielle (ce n'est pas le même produit, voir §3) mais comme spec de domaine.

## 1. Positionnement factuel

| Critère | PhacochR | crabfinder |
|---|---|---|
| Modèle d'usage | Lib R, batch en mémoire | Service HTTP long-lived, requête en ligne |
| Forme d'entrée | DataFrame R, 5 formats (colonnes propres / mixées) | Chaîne libre `q=...` parsée |
| Sortie | DataFrame enrichi (1 ligne / 1 décision), + objet `sf`, + `summary` | GeoJSON `FeatureCollection`, top-N ranké |
| Matching rue | Fuzzy join LCS via `stringdist` (`error_max=4`), tiebreak Jaro-Winkler, ex aequo par tirage aléatoire | BM25 Tantivy phrase + AND-of-OR (forms abréviation), fallback OR-of-tokens |
| Index | CSV preprocessés shardés par arrondissement (43 fichiers), chargés en RAM par requête | Index Tantivy v6 persistant (6.84M docs, ~2 GB) + R-tree spatial reconstruit au démarrage |
| Langues | FR / NL / DE (filtre `lang_encoded`) | Multilingual, un seul champ `searchable` (collapse FR/NL/DE après ascii-folding) |
| Données source | BeSt Address + Statbel (secteurs stats) + Urbis (quartiers BXL monitoring) + STATBEL prénoms (pour abréviations) | BeSt Address seul (+ whitelist notabilité curée) |
| Fallback numéro absent | Numéro médian de la rue *au sein du même CP* (précomputé) | Aucun, renvoie le top-N de la rue, ligne ouverte au consommateur |
| Fallback numéro inconnu | Nearest-number-same-side (parité ±, jusqu'à ±50 par défaut), puis any-side | Aucun |
| Élargissement géo | Communes adjacentes via table précomputée, `error_max/2` durci | Aucun explicite, recherche directement sur tout l'index |
| Métadonnées produites | `dist_fuzzy`, `type_geocoding`, `approx_num`, `nom_propre_abv`, `recode`, +~25 colonnes administratives (secteurs, quartiers, INS, refnis) | `match_type ∈ {strict, fuzzy, spatial}`, `score` BM25, `building_group_id` |
| Perf revendiquée | 0.4–0.8 s / 100 adresses à partir de n=2000 (overhead chargement amorti) | p99 `/geocode` ~20 ms HTTP loopback, recall@1 ≈ 0.93 (Tier A) |
| Parallélisation | doParallel/PSOCK, `foreach` sur les codes postaux uniques | Multi-thread Tantivy implicite, R-tree mono-thread |
| Confidentialité | Tout local (R) | Tout local (binaire) |
| Licence | GPL ≥ 3 | MIT OR Apache-2.0 |
| Statut | v0.9.1.14, « pré-1.0 », 1343 lignes pour le cœur géocodage | v6 du schéma, eval continue |

## 2. Avantages réels de PhacochR transposables à crabfinder

On liste ici uniquement ce que PhacochR fait, que crabfinder ne fait pas, et qui a du sens dans notre modèle (service Rust/Tantivy, futur batch). Pour chaque point : référence code, ce que ça apporte, transposabilité.

### 2.1 Fallback numéro médian de rue (`mid_street`): directement transposable

**Code** : `inst/scripts/phaco_best_data_update.R:509-521` (preprocessing) et `R/phaco_geocode.R:1124-1134` (lookup au géocodage).

```r
# Preprocessing : pour chaque (street_id_phaco, postal_id), on retient le numéro
# qui correspond à l'ordre statistique du milieu (type=3 = discret, arrondi bas).
num_mid <- belgium_street_abv %>%
  inner_join(openaddress_be, by = "street_id_phaco" ) %>%
  group_by(street_id_phaco, postal_id) %>%
  filter(house_number_sans_lettre == as.numeric(quantile(house_number_sans_lettre, p = 0.5, type = 3, na.rm=T)))
```

**Ce que ça fait** : pour chaque couple (rue, code postal), précompute les coordonnées d'un « bâtiment représentatif », celui dont le numéro est à la médiane des numéros existants. Lookup O(1) au géocodage. Au runtime, si le numéro demandé n'existe pas dans BeSt (adresse défunte, faute de frappe, ou juste pas saisi), le programme retourne ces coordonnées avec `type_geocoding = "mid_street"`.

**Pourquoi c'est important pour le batch** : sur listes d'adresses réelles, un volume significatif a un numéro absent ou faux. Renvoyer « rien » est inutilisable pour de l'analyse spatiale (la ligne est perdue). Renvoyer le milieu de la rue avec un flag explicite est généralement acceptable pour de l'agrégation par quartier ou secteur. C'est exactement le compromis qu'attend le destinataire batch.

**Transposabilité** : **directe**. Le calcul se ferait dans `crabfinder-indexer`, soit comme champs FAST supplémentaires sur le doc « bâtiment représentatif », soit comme un side-table `street_id × postal_code → repr_lat/lng`. Choix : implémentation propre côté Rust (l'idée, pas le code GPL).

### 2.2 Nearest-number-same-side avec parité: à adapter

**Code** : `R/phaco_geocode.R:1036-1097`. Étape « APPROXIMATION NUMÉRO ».

```r
APPROX_1 <- FULL_GEOCODING_APPROX %>%
  inner_join(openaddress_be, by="street_id_phaco") %>%
  filter(num_rue_clean%%2 == house_number_sans_lettre%%2)   # même côté (parité)
# ... puis sélectionne le numéro avec min(abs(num_rue_clean - house_number_sans_lettre))
# puis filter(approx_num <= approx_num_max*2)
```

**Ce que ça fait** : si la rue est trouvée mais que le numéro précis n'existe pas, prend le plus proche numéro de la **même parité** (= même côté de rue, par convention européenne). Si la passe « même parité » échoue, fallback sur n'importe quel côté (`APPROX_2`). Le seuil par défaut `approx_num_max=50` borne la dérive (`<= 50*2`, soit ±50 numéros, la `*2` vient du fait que sauter d'un même côté à un autre saute 2 unités).

**Limite** : la parité n'est qu'un proxy. Dans certaines rues belges la numérotation n'est pas strictement pair-impair par côté (rues en boucle, immeubles à numéros multiples, etc.). PhacochR accepte cette imperfection, `sample_n(1)` quand plusieurs candidats sont à égale distance. La doc PhacochR ne le revendique pas comme exact, juste comme « approximation utile ».

**Transposabilité** : **à adapter**. L'idée est saine. L'implémentation crabfinder peut s'en inspirer mais on n'a pas de notion native « tous les numéros d'une rue d'un coup » dans l'index Tantivy actuel (un doc = un numéro). Il faudra soit :
- précomputer un side-index `street_id × postal_code → [(num, lat, lng), ...]` dans l'indexer ;
- soit faire une requête Tantivy filtrée par `street_id` (déjà indexé) au moment du fallback, et trier les hits par |Δnum| en Rust.

La seconde voie est plus simple et n'ajoute pas de structure persistée nouvelle.

### 2.3 Élargissement aux communes adjacentes: partiellement transposable

**Code** : `R/phaco_geocode.R:892-985` et table `data/BeST/PREPROCESSED/table_commune_adjacentes.csv` (générée au setup, voir `phaco_best_data_update.R`).

```r
# Pour chaque commune i, on récupère la liste précomputée des voisines
com_adj_i <- table_commune_adjacentes$cd_munty_refnis_voisin[table_commune_adjacentes$cd_munty_refnis == i]
# Et on relance le fuzzy join élargi
stringdist_left_join(..., max_dist = error_max/2)  # tolérance durcie (de 4 à 2)
# Si trouvé : code_postal_to_geocode = postal_id (réécrit au CP où la rue a été trouvée)
```

**Ce que ça fait** : si la rue n'est pas trouvée dans le CP indiqué (faute de frappe sur le CP, ou rue à cheval entre deux communes), élargit la recherche aux **communes limitrophes uniquement**, avec une tolérance fuzzy **divisée par deux** pour limiter les faux positifs. La table d'adjacence est précomputée à l'installation (pas de calcul géométrique à chaque requête). Le CP de sortie est celui où la rue a été effectivement trouvée, donc la ligne sort avec un CP éventuellement différent de l'input.

**Comment l'adjacence est déterminée** : via la table CSV livrée avec les données ; cette table est produite à partir de jointures sur communes adjacentes (le code de génération vit dans `phaco_best_data_update.R` mais utilise des shapefiles Statbel, c'est un calcul géométrique fait *une fois* au setup, pas à la volée). Pas de calcul de polygone à l'exécution.

**Transposabilité** : **à adapter, mais en v2**. Le besoin est réel (le CP d'entrée peut être faux). Mais :
- Le bénéfice qualitatif n'est démontré que sur la slice « rue introuvable au CP indiqué », déjà un cas marginal après les fallbacks principaux.
- Cela ajoute une table secondaire (~600 communes × adjacents) à embarquer.
- crabfinder a déjà une porte de sortie naturelle : retirer le filtre postal et regarder les top hits sur l'index complet. La perte de précision peut être compensée par le ranking BM25 et la notabilité.

À garder dans la liste post-v1.

### 2.4 Taxonomie de confiance par résultat: directement transposable, fort intérêt

**Code** : sortie de `phaco_geocode()`, colonnes décrites lignes 380-432 du README et émises dans `R/phaco_geocode.R:1140-1235`.

| Colonne | Sémantique | Valeurs |
|---|---|---|
| `dist_fuzzy` | Nombre d'éditions LCS pour matcher la rue | entier 0–`error_max` (par défaut 0–4) ; NA si rue non trouvée |
| `type_geocoding` | Nature du géocodage final | "" (numéro exact), "elargissement_adj" (commune voisine), "mid_street" (numéro médian), ou les deux concaténés " ; " |
| `approx_num` | Δ absolu entre numéro demandé et numéro trouvé | 0 (exact), entier (approximé), NA (fallback médian / non trouvé) |
| `nom_propre_abv` | A nécessité l'index des prénoms abrégés (« B. Smith ») | 1 / NA |
| `recode` | Quelles règles de normalisation ortho ont déclenché | string composite (« virgule ; boite ; avenue ») |

**Pourquoi c'est important** : c'est *l'avantage majeur* de PhacochR sur la concurrence, l'utilisateur peut **trier ses adresses par qualité** et décider lui-même quoi accepter. crabfinder émet un `match_type ∈ {strict, fuzzy, spatial}` à 3 valeurs : c'est trop pauvre pour piloter une décision batch. Pour de la recherche en ligne ça suffit (l'utilisateur retape s'il n'est pas content) ; pour du batch ça ne suffit pas (le destinataire d'un CSV enrichi doit pouvoir trier ses 100k lignes par confiance et inspecter les 200 douteuses).

**Transposabilité** : **directe** (l'idée). Mais la taxonomie crabfinder doit être adaptée à *notre* stack, pas copiée :
- `dist_fuzzy` (édit-distance entière) n'a pas de sens dans BM25, on a un `score` continu (réel). On peut soit l'émettre tel quel (peu lisible), soit le bucketiser en `{exact, fuzzy_close, fuzzy_far}` selon un seuil empirique.
- `type_geocoding` est directement émettable : `exact | nearest_number_same_side | nearest_number_any_side | median_number | adjacent_commune | not_found`.
- `approx_num` est calculable trivialement quand on sort un numéro approximé.

À retenir : **émettre un score décomposé, pas juste une note finale**. C'est ce que PhacochR fait bien et que crabfinder pour le batch doit reproduire.

### 2.5 Correction orthographique FR/NL pré-matching: pas pertinent, déjà couvert autrement

**Code** : `R/phaco_geocode.R:437-760`. ~30 substitutions regex en pipeline, sur une colonne `rue_recoded`.

```r
rue_recoded_avenue = str_detect(rue_recoded, regex("(^av[.](\s|)|^av(e|)\s)", ignore_case = TRUE)),
rue_recoded = ifelse(rue_recoded_avenue, str_replace(..., "Avenue "), rue_recoded),
# ... pareil pour ch./chs., stwg, burg., kon., prof., dr., bvd., rte, pl., sq.,
# st./ste/sint → Saint/Sainte/Sint, et beaucoup de FR (de la → Rue de la, etc.)
```

C'est **hand-coded**, pas un dictionnaire. Quelques règles sont conditionnées par la présence de mots NL (`if str_detect(NL_marker) FALSE else ...`). La section NL est commentée comme **incomplète** (« À FAIRE EN NL: boulevard => blv, straat => str »).

**Pourquoi ce n'est pas un avantage pour nous** : crabfinder a déjà deux mécanismes qui couvrent ce besoin par construction :
1. `data/street-type-abbreviations-v1.yaml` (~100 règles, expansion bidirectionnelle à la requête) couvre le cœur (av, ch, bvd, stwg, dr, prof, etc.).
2. Le pipeline analyzer `LowerCaser → AsciiFoldingFilter` + fallback fuzzy AND→OR couvre les coquilles ponctuelles que les regex PhacochR rattrapent par force brute.

La partie qui reste **non couverte** chez crabfinder :
- Ponctuation parasite (boîte 5, n°, BP/CP), ces tokens passent dans `street_tokens` aujourd'hui et polluent la requête. Le parser `crabfinder-parser` ne sait pas les déstaffer.
- Abréviations de **prénoms** dans noms propres de rue (« Bd. M. Lemonnier »). Ni notre dictionnaire ni le parser ne traitent ce cas.
- Préfixes français isolés (« d' », « de la », « de l' », « du », « des ») qui ailleurs sont des stop-words mais qui ici font partie du nom officiel.

**Transposabilité** : **partielle, et seulement la partie « strip »**. Ajouter au parser une étape de pré-nettoyage qui drop `bte XX`, `boîte X`, `n° X`, `BP X`, `CP X` avant la tokenisation. Ne PAS reproduire le pipeline de 30 regex, c'est de l'over-engineering qui n'a payé chez PhacochR que parce qu'ils n'avaient pas d'analyzer ni d'index. Voir §4.

### 2.6 Enrichissement administratif (secteurs Statbel, quartiers monitoring Urbis): pertinent mais coûteux

**Code** : `R/phaco_geocode.R:1180-1183` (lookup) ; `phaco_best_data_update.R` (preprocessing avec jointures spatiales sf::st_intersects sur les shapefiles Statbel).

```r
# Les secteurs statistiques sont déjà attachés à chaque adresse au preprocessing.
# Au géocodage : simple left_join (lookup O(1)).
FULL_GEOCODING <- FULL_GEOCODING %>%
  left_join(table_secteurs_prov_commune_quartier, by = "cd_sector")
```

**Ce que ça fait** : à chaque adresse géocodée, attache `cd_sector` (secteur statistique Statbel, granularité ~quartier IRIS) puis joint via une table de référence pour récupérer : nom du secteur FR/NL, sous-commune, code Refnis commune, arrondissement, province, région, et pour Bruxelles le code MDRC (quartier monitoring Urbis). ~25 colonnes administratives au total.

**Pourquoi c'est important pour le batch** : un utilisateur batch typique géocode pour faire de l'analyse spatiale en aval (densité, démographie, croisement avec Statbel). Lui livrer juste lat/lng le force à refaire le point-in-polygon lui-même contre les shapefiles Statbel, un sous-projet en soi.

**D'où ça vient** : Statbel (secteurs statistiques shapefile, table de hiérarchie administrative, prénoms pour la détection nom abrégé) et Urbis (quartiers monitoring Bruxelles seulement). Sources publiques, libres mais nouvelles pour crabfinder. Le preprocessing PhacochR fait `st_intersects(adresse, polygone_secteur)` une fois pour toujours au setup ; il n'y a *pas* de calcul géométrique au géocodage.

**Transposabilité** : **à adapter, gros morceau, post-v1**. Nécessite :
- Embarquer ou télécharger les shapefiles Statbel/Urbis.
- Point-in-polygon dans `crabfinder-indexer` (ou pré-attachement via lookup à partir d'une table externe).
- Schéma index v7 avec nouveaux champs FAST (cd_sector, cd_munty_refnis, cd_prov_refnis, etc.) ou hydratés depuis le payload.

L'effort est non trivial : nouvelles sources de données, nouvelle dépendance géo (`geo` crate ou similaire), nouvelles colonnes index. À garder pour une v1.1+ batch.

## 3. Ce que crabfinder fait mieux / différemment

Symétrie : où PhacochR ne va pas, ou choisit autrement.

### 3.1 Service en ligne, faible latence

PhacochR est un package R chargé à chaque session. Le coût fixe (chargement des CSV, fuzzy join initial) est amorti sur le batch, d'où la perf décroissante en 1/n affichée dans `vignettes/performances.Rmd` (2 adresses : 16 s ; 20 000 adresses : 140 s). Pour la recherche en ligne, c'est **inutilisable**.

crabfinder fait l'inverse : un binaire long-lived, index Tantivy persisté, R-tree spatial reconstruit une fois au démarrage. p99 `/geocode` ~20 ms en HTTP loopback. Permet l'usage interactif (un humain qui tape une adresse) que PhacochR ne sert tout simplement pas.

### 3.2 Query libre (free-text) vs colonnes structurées

PhacochR demande 5 formats explicites de colonnes. C'est puissant en batch mais inutilisable pour la recherche, où on n'a qu'un champ « tape ton adresse ». crabfinder-parser fait le travail inverse : il prend une chaîne arbitraire et en extrait `(street_tokens, house_number, postal_code)` heuristiquement (`crates/crabfinder-parser/src/lib.rs:44`). C'est moins fiable que des colonnes propres, mais c'est ce que le cas « recherche » exige.

### 3.3 Ranking continu BM25 + notabilité, vs match exact / fuzzy strict

PhacochR est binaire : la rue est matchée (avec une distance d'édition <= 4) ou pas. Il n'y a pas de notion de « cette rue est plus probable que cette autre » au-delà du Jaro-Winkler de tiebreak. Pour de la recherche où l'utilisateur attend un top-N pertinent (« Grand-Place 1000 » → Grand-Place de Bruxelles avant Grand-Place de Mons), BM25 + tweak notabilité de crabfinder donnent un classement par pertinence que PhacochR ne propose pas.

### 3.4 Dédup multi-unité par bâtiment (Bruxelles)

`crabfinder-service/src/dedup.rs:70` (`group_by_building`) collapse les boîtes multiples d'un même bâtiment via `building_group_id` synthétique (street_id + house_number). PhacochR ne fait rien de tel : si la rue a 12 boîtes au n° 71, la jointure renverra 12 lignes, sauf que PhacochR n'utilise pas le numéro de boîte, donc en pratique il fait 1 ligne par adresse postale unique. Légèrement différent : nous on émet 1 par bâtiment au top-N, eux 1 par numéro de rue. Pour le batch, **leur** modèle est probablement plus juste (une adresse postale = un point).

### 3.5 Reverse geocoding

crabfinder a `/reverse?lat=...&lng=...` (R-tree, Haversine final, multi-langue). PhacochR ne fait pas de reverse, c'est explicitement hors-scope (R sert le forward batch).

### 3.6 Matching fuzzy intégré à l'index, pas recalculé par appel

PhacochR refait un `stringdist_left_join` complet à chaque appel sur les rues de l'arrondissement chargé en RAM. crabfinder a son matching « tolérance fautes » baked-in : analyzer ascii-folding pré-applique pour les diacritiques, fallback OR-of-tokens rattrape les fautes de tokens. C'est moins flexible que stringdist en R (on ne paramètre pas la méthode), mais plusieurs ordres de grandeur plus rapide.

### 3.7 Ce que crabfinder ne fait PAS et que PhacochR fait

Récap honnête, pour situer ce qu'il manque côté batch :
- Pas de fallback numéro médian (cf. §2.1).
- Pas de fallback numéro voisin (cf. §2.2).
- Pas d'élargissement commune adjacente (cf. §2.3).
- Pas de taxonomie de confiance riche (cf. §2.4).
- Pas d'enrichissement administratif Statbel/Urbis (cf. §2.6).
- Pas d'API d'entrée acceptant des colonnes structurées (rue / num / CP séparés).
- Pas de sortie 1-ligne-1-décision : on retourne un top-N. Pour du batch il faut « réduire » à 1.

## 4. Idées de domaine pour le batch: pont vers le livrable 2

Sur tout ce qui précède, voici les trois priorités pour bâtir un bon batch crabfinder, classées par ratio bénéfice / effort. Le design détaillé est dans [`design-batch-geocoding.md`](./design-batch-geocoding.md).

1. **Numéro médian de rue (mid_street)** : **v1**. Donnée précomputable à l'indexation (un champ par street_id × postal_code), lookup O(1), gain qualité énorme sur les lignes sans numéro ou numéro inconnu. Coût d'implémentation : faible.

2. **Taxonomie de confiance par ligne** : **v1**. Émettre `type_geocoding ∈ {exact, nearest_same_side, nearest_any_side, median_street, fuzzy_street, adjacent_commune, not_found}` + `street_match_score` (BM25 normalisé ou bucketisé) + `house_number_delta`. Sans cela, un CSV de 100k lignes enrichi n'est pas trillable par qualité par le destinataire. Coût : faible (purement additif sur la sortie).

3. **Formats d'entrée multiples** : **v1 limité, v1.1 complet**. Au minimum, accepter `(street, house_number, postal_code)` en colonnes séparées dans le batch (le cas idéal selon README PhacochR). Le parsing de champs sales (rue+num+CP intégrés) est tentant mais c'est l'essentiel des 1343 lignes de `phaco_geocode.R` (au moins 300 dédiées au parsing regex). À ne pas embarquer en v1, voir design.

Trois autres priorités à garder en post-v1 :
- Nearest-number-same-side (parité)
- Élargissement commune adjacente
- Enrichissement administratif Statbel

## 5. Vérifications du README vs code

- ✅ « 0,4–0,8 s / 100 adresses », confirmé dans `vignettes/performances.Rmd:21`, mais à partir de **n ≥ 2000**. Pour 2 adresses : 16 s. C'est une perf de batch amorti, pas de requête unitaire.
- ✅ « Médiane de 97% adresses trouvées », `vignettes/performances.Rmd:28`. Mesuré sur 18 bases internes, pas un benchmark public. Non reproductible en externe.
- ✅ Fallback numéro médian, confirmé (`phaco_best_data_update.R:509-521` au preprocessing ; `phaco_geocode.R:1124` au runtime).
- ✅ Approximation parité même côté, confirmée (`phaco_geocode.R:1041`), mais avec fallback `APPROX_2` any-side si parité ne donne rien, la doc README ne mentionne pas cette deuxième passe.
- ✅ Élargissement aux communes adjacentes, confirmé, avec `error_max/2` (le README ne dit pas que la tolérance est durcie ; c'est un détail important).
- ✅ Tiebreak Jaro-Winkler, confirmé (`phaco_geocode.R:873`). Le README dit « le programme choisit la rue la plus proche ». Code dit : LCS minimum d'abord, puis JW pour départager les ex aequo, puis `sample_n(1)` aléatoire si encore ex aequo. Le tirage aléatoire n'est pas dans le README.
- ✅ 5 formats d'entrée, confirmés, code en `phaco_geocode.R:255-308`.
- ⚠️ « Correction orthographique en FR et NL », le README sous-vend : c'est ~30 substitutions regex hand-coded essentiellement FR + une fraction NL avec un TODO explicite (`# A FAIRE EN NL` ligne ~755). Pas un dictionnaire bilingue symétrique.
- ⚠️ Niveau de précision « bâtiment », vrai *quand le numéro matche*. Avec le fallback mid_street, on est au niveau **rue**. Avec le fallback approx_num, on est à un numéro voisin mais le même côté. Le README mélange un peu les trois niveaux dans son discours marketing.
- ✅ Confidentialité « tout local », confirmé, aucun appel réseau dans le code de géocodage (réseau seulement dans `phaco_best_data_update.R` pour télécharger les sources).

Pas de divergence majeure découverte. Le code fait bien ce que le README annonce ; il fait aussi plus (tirage aléatoire pour les ex aequo restants, durcissement de la tolérance à l'élargissement, fallback any-side après échec de la passe parité) que ce que le README détaille.

## 6. Licence

PhacochR est sous **GPL ≥ 3** (`DESCRIPTION:7`). crabfinder est sous **MIT OR Apache-2.0** (`README.md:80`). Conséquence pratique pour ce design : on peut s'inspirer librement des **idées algorithmiques** de PhacochR (médian, nearest-number, taxonomie, élargissement commune) ; on ne peut pas **traduire mot-à-mot** leur R en Rust dans crabfinder. Les pages « lookup precomputed median » ou « parity filter » sont des idées de domaine génériques (et publiques par ailleurs : c'est de la pratique standard en géocodage adresse), donc sûres ; mais le **pipeline exact de 30 regex de normalisation orthographique** est une œuvre originale du projet, et il faut soit s'en abstenir, soit le réécrire « à partir de zéro » à partir d'observations sur les BeSt, pas en regardant leur fichier R.

Formulation des recos qui suivront : « voici l'idée du fallback médian par street × postal ; réimplémente proprement en Rust ».
