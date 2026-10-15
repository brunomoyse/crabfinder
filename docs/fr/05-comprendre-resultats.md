# 5. Comprendre les résultats

À lire avant d'exploiter un CSV géocodé ou de prendre une décision automatique à partir d'une réponse `/geocode`. Cette section explique ce que signifient `confidence`, `fallback_reason`, `house_number_delta`, `position_source`, `ambiguous` et `score`, et comment s'en servir pour trier ou filtrer. Le vocabulaire est le même en batch (colonnes CSV / GeoJSON) et en ligne (champs des features `/geocode`).

## La règle d'or

**Chaque ligne de sortie a une coordonnée *et* un niveau de confiance**, sauf `not_found` qui n'a ni l'un ni l'autre. La qualité varie. C'est à toi de décider ce qui est acceptable pour ton cas.

Seuils d'acceptation typiques :

| Usage | Acceptable | À vérifier | À rejeter |
|---|---|---|---|
| Carte avec un marqueur par bâtiment | `exact` | `fuzzy_street`, `nearest_*` | `median_street`, `locality`, `not_found` |
| Agrégation par quartier / commune | `exact`, `nearest_*`, `median_street`, `fuzzy_street` | `locality` | `not_found` |
| Jointure spatiale avec des polygones (densité, secteur) | tout sauf `not_found` | `locality` | `not_found` |
| Adresse de facturation | `exact` uniquement | tout le reste | tout le reste |

Ajoute deux contrôles à chacun de ces cas : les lignes avec `ambiguous=true` méritent un coup d'œil quelle que soit leur confiance, et `position_source` dit si le point d'une ligne `exact` est bien le bâtiment lui-même (voir [plus bas](#doù-viennent-les-coordonnées--position_source)).

## La taxonomie `confidence`

Huit valeurs, de la plus forte à la plus faible.

| `confidence` | Précision spatiale | En bref |
|---|---|---|
| `exact` | bâtiment | Le numéro demandé existe dans cette rue. |
| `nearest_same_side` | bâtiment (un voisin) | Numéro absent ; numéro le plus proche du même côté de la rue. |
| `nearest_any_side` | bâtiment (un voisin) | Numéro absent ; ce côté n'a pas de numéro assez proche, le plus proche de l'autre côté. |
| `median_street` | rue | Pas de numéro, ou un numéro trop loin de tout numéro existant. |
| `fuzzy_street` | bâtiment ou rue | Nom de rue mal orthographié ou trouvé partiellement. |
| `locality` | village / commune | La requête ne nomme qu'un lieu. |
| `adjacent_commune` | — | Réservé, jamais émis pour l'instant. |
| `not_found` | aucune | Rien de plausible. |

### `exact` — match parfait

Le numéro exact a été trouvé dans BeSt, dans la rue donnée, au code postal donné.

```
entrée :  Avenue Louise 32 1050
trouvé :  Avenue Louise 32, 1050 Ixelles   lat=50.83451 lng=4.35670
          confidence = exact
          fallback_reason = (vide)
          house_number_delta = 0
```

**Action** : à utiliser tel quel.

### `nearest_same_side` / `nearest_any_side` — un numéro voisin

La rue a été trouvée mais le numéro demandé n'y existe pas (à ce code postal). crabfinder renvoie le **numéro existant le plus proche du même côté de la rue** (même parité : impair avec impair, pair avec pair) — comme le ferait un facteur. Si ce côté n'a pas de numéro assez proche, il prend le plus proche de l'autre côté (`nearest_any_side`). En cas d'égalité, le plus petit numéro gagne. Les suffixes alphabétiques sont ignorés pour la distance (`34B` compte comme 34).

```
entrée :  Avenue Louise 33 1050
trouvé :  Avenue Louise 59, 1050 Bruxelles   lat=50.83310 lng=4.35728
          confidence = nearest_same_side
          fallback_reason = house_number_out_of_range
          house_number_delta = 26          (59 − 33)
```

`house_number_delta` est le numéro trouvé moins le numéro demandé : plus il est grand, plus le point peut être loin de l'adresse réelle. Un voisin n'est accepté qu'à `max(50, numéro demandé / 2)` numéros près ; au-delà (par ex. `Avenue Louise 9999`), le résultat se replie en `median_street`.

**Action** : coordonnées au bâtiment d'une vraie adresse voisine. Convient pour une carte quand `|house_number_delta|` est petit ; vérifie les grands écarts.

### `median_street` — centroïde de rue

Pas de numéro utilisable : absent de l'entrée, ou trop loin de tous les numéros existants. crabfinder renvoie un **point représentatif de la rue** : le bâtiment dont le numéro se situe à la médiane des numéros existants pour cette rue et ce code postal.

```
entrée :  Grand-Place 1000     (pas de house_number)
trouvé :  Grand-Place 2, 1000 Bruxelles   lat=50.84623 lng=4.35301
          confidence = median_street
          fallback_reason = house_number_missing
          house_number_delta = (vide)
```

**Précision spatiale** : **à la rue**, pas au bâtiment. L'écart peut être de quelques mètres sur une rue courte, ou de plusieurs centaines de mètres sur une rue longue. Pour une agrégation par quartier, généralement acceptable. Pour placer précisément un marqueur sur une carte, c'est trompeur — au minimum, affiche-le avec une autre icône.

Deux `fallback_reason` possibles :

- `house_number_missing` : l'entrée n'avait pas de numéro.
- `house_number_out_of_range` : un numéro était fourni mais aucun numéro existant de cette rue n'est assez proche.

**Action** : utilisable pour des agrégats spatiaux, à éviter pour une analyse au bâtiment. En batch, si tu préfères ne pas avoir de point du tout, tu peux désactiver ce repli (`--no-mid-street` / `mid_street_fallback=false`) ; la ligne devient alors `not_found`.

### `fuzzy_street` — rue trouvée approximativement

Le nom de rue n'a pas été trouvé tel quel. Deux mécanismes le produisent :

- **Correction de faute** (`match_type=typo`) : une ou deux lettres fausses dans un mot de rue (« Avenue Loiuse », « Rue Beliard »). En général juste.
- **Match partiel** (`match_type=fuzzy`) : tous les mots n'ont pas trouvé de correspondance ; crabfinder a gardé la rue qui partage le plus de mots avec ta requête. Plus risqué.

Dans les deux cas, le résultat peut être le bâtiment demandé dans cette rue (si le numéro existe : `house_number_delta=0`, précision au bâtiment) ou le point représentatif de la rue (`house_number_delta` vide, précision à la rue).

```
entrée :  Avenue Loiuse 32 1050     (faute sur « Louise »)
trouvé :  Avenue Louise 32, 1050 Ixelles   lat=50.83451 lng=4.35670
          confidence = fuzzy_street
          fallback_reason = street_spelling_fuzzy
          match_type = typo
```

**Action** : à vérifier, surtout en `match_type=fuzzy`. Compare `matched_street` avec ton entrée ; un `street_match_score` bas (sous ~10) suggère un faux positif.

### `locality` — un lieu, pas une adresse

La requête ne nommait qu'un lieu — une commune, une section de commune, une zone postale, ou un village ou hameau OpenStreetMap (« Spontin », « Maredsous », « Han-sur-Lesse 5580 »). Il n'y a pas de rue où placer le point.

- Batch : la ligne reçoit le **centre du lieu**, le label `5530 Spontin`, le code postal ; `matched_street`, `building_group_id` et `best_id` restent vides.
- `/geocode` : les features sont les adresses les plus proches du lieu (`match_type=spatial`), et l'objet `locality` de premier niveau décrit le lieu lui-même (type, noms, codes postaux, centre).

```
entrée :  Spontin
trouvé :  5530 Spontin   lat=50.32262 lng=5.00695
          confidence = locality
          fallback_reason = street_missing
```

**Action** : précision au village (de quelques centaines de mètres à quelques km). Bien pour savoir « quelle commune », pas pour une livraison.

### `not_found` — aucune coordonnée

Rien de plausible n'a été trouvé. La ligne sort avec des `lat/lng/matched_*` vides, `confidence = not_found`, `fallback_reason = no_match`.

```
entrée :  Some Nonexistent Street 99 1000
trouvé :  (rien)
          confidence = not_found
          fallback_reason = no_match
```

**Action** : enquêter ligne par ligne. Causes possibles :
- Une faute trop lourde pour être corrigée (plus de 2 lettres, ou dans un mot court).
- Mauvais code postal (la rue n'existe pas à ce CP).
- Adresse récente pas encore dans le dump BeSt (mise à jour nécessaire).
- Adresse hors de Belgique.
- Adresse retirée (seules les adresses BeSt en vigueur sont indexées).

### `adjacent_commune` — réservé

Défini dans le schéma de sortie (rue trouvée dans une commune voisine, mauvais CP en entrée) mais **pas encore émis**. Tes scripts peuvent l'ignorer pour l'instant.

## La taxonomie `fallback_reason`

Plus fine que `confidence`. Quand `confidence != exact`, elle dit **pourquoi** :

| `fallback_reason` | Signification | Avec |
|---|---|---|
| `house_number_missing` | L'entrée n'avait pas de numéro. | `median_street`, `fuzzy_street` |
| `house_number_out_of_range` | Un numéro était fourni mais n'existe pas pour cette rue dans BeSt. | `nearest_*`, `median_street` |
| `street_spelling_fuzzy` | La rue n'a été trouvée qu'après correction de faute ou partiellement. | `fuzzy_street` |
| `street_missing` | La requête nomme un lieu mais pas de rue. | `locality` |
| `postal_code_mismatch` | Réservé (futur repli sur une commune voisine). | — |
| `no_match` | Rien trouvé du tout. | `not_found` |

Cas combiné : une ligne avec `confidence=fuzzy_street, fallback_reason=house_number_missing` signifie « j'ai approximé la rue *et* tu n'as pas fourni de numéro ». C'est une double dégradation — à vérifier en priorité par rapport aux dégradations simples.

## Le champ `house_number_delta`

Numéro trouvé moins numéro demandé, rempli chaque fois que le numéro demandé a été respecté :

- `0` : le numéro demandé a été trouvé (`exact`, et `fuzzy_street` sur le bon bâtiment).
- non nul : un voisin a été renvoyé (`nearest_*`) ; `26` = 26 numéros plus loin dans la rue, `-2` = 2 numéros plus bas.
- vide : résultats à la rue et de lieu, `not_found`.

## D'où viennent les coordonnées : `position_source`

BeSt publie une position pour presque toutes les adresses, mais pas toutes : dans le dump du 2026-10-15, **140 263 adresses wallonnes** n'avaient pas encore de position (publiées en Lambert 72 `0 0`, ce qui tomberait en France). crabfinder leur attribue une position à partir de leurs voisines, et le signale :

| `position_source` | Comment le point a été obtenu | Précision | Dump 2026-10-15 |
|---|---|---|---|
| `measured` | Publié par BeSt. | bâtiment | 6 746 374 |
| `same_number` | Une autre unité au même numéro, dans la même rue et au même code postal, a une position : partagée. | bâtiment | 44 909 |
| `interpolated` | Entre les voisins numérotés les plus proches, même rue et même code postal, du même côté quand c'est possible. | à peu près le bâtiment (quelques dizaines de mètres) | 87 293 |
| `street` | La rue a des adresses positionnées mais aucune pour interpoler : le point représentatif de la rue. | rue | 0 |
| `locality` | Rien dans la rue n'a de position : le centre de la section de commune de l'adresse (sinon de la commune). | village | 8 061 |

Exemples réels : `Rue Longue 205, 1320 Beauvechain` est `interpolated`, `Stoquoit 9Bis, 7863 Lessines` est `same_number`, `Chemin du Bois Robert 1B, 4860 Pepinster` est `locality`.

Où la trouver :

- Batch : la colonne `position_source`, toujours remplie.
- `/geocode`, `/building` : `properties.position_source`, présent seulement quand la position a été imputée (absent = `measured`).

Attention : `confidence` porte sur le **match textuel**, `position_source` sur le **point**. Une ligne `exact` peut très bien porter une position `locality` — la bonne adresse, placée au centre de son village. Pour un usage au bâtiment, accepte `measured`, `same_number` et `interpolated`, et traite `street` / `locality` comme des résultats `median_street` / `locality`. `/reverse` ne renvoie jamais d'adresse dont la position est `street` ou `locality`.

## Ambiguïté : `ambiguous` et `score_gap`

`ambiguous=true` signifie que le deuxième candidat atteint au moins 90 % du score de la réponse retenue **et** se trouve dans une autre rue ou une autre commune. Cas typique : un nom de rue courant sans code postal (« Kerkstraat 1 », « Rue de la Station 1 ») — crabfinder a choisi un candidat parmi beaucoup d'autres tout aussi plausibles.

- Batch : la colonne `ambiguous` (`true` / `false`).
- `/geocode` : `ambiguous` au premier niveau, plus `score_gap` = `1 − score₂/score₁` quand il y a au moins deux résultats (`0` = égalité).

**Action** : considère une ligne ambiguë comme non confirmée, quelle que soit sa confiance. Ajouter le code postal la résout presque toujours.

## Le score `street_match_score`

C'est le **score BM25 brut** de la recherche textuelle sur le champ `searchable` (toutes les variantes multilingues du nom de rue + commune + noms postaux). Pas un score normalisé. Pas un pourcentage. Une grandeur continue qui dépend :

- de la fréquence des mots dans la requête,
- de la fréquence des mots dans le corpus (les mots rares pèsent plus),
- d'un éventuel boost de notoriété (Grand-Place, Atomium, etc.).

Sur l'éval Tier A (sur les hits `exact`) :

| Quartile | Valeur BM25 |
|---|---|
| q25 | ~25,8 |
| q50 (médiane) | ~32,8 |
| q75 | ~101,1 |
| q95 | ~144,4 |

**À retenir** : un score sous q25 sur un hit `exact` est *suspect* (match textuel faible, peut-être un nom de rue très court). Un score au-dessus de q75 est un `exact` solide. Pour comparer deux résultats, le score est utilisable ; pour des décisions absolues, **utilise `confidence`, pas le score**.

Pour `fuzzy_street`, les scores sont en général plus bas. Pour `not_found`, le champ est vide.

## Les identifiants `building_group_id` et `best_id`

Deux références exploitables en aval :

- `building_group_id` : identifiant de bâtiment de crabfinder, `{identifiant de rue}|{numéro}` (par ex. `https://databrussels.be/id/streetname|3919|32`). Stable d'un dump BeSt à l'autre tant que l'identifiant de rue et le numéro le sont. Toutes les boîtes d'un bâtiment le partagent — utile pour regrouper de ton côté, ou pour `/building/{id}/units`.
- `best_id` : identifiant BeSt officiel sous la forme `namespace/object_identifier/version`. Permet de remonter à la ligne BeSt source (audit, jointures avec d'autres données BeSt). Quand tu donnes un numéro de boîte connu de BeSt, c'est l'identifiant de cette unité.

## Mini-recette de tri en aval

Tu peux trier ton CSV géocodé par confiance + score pour traiter d'abord les cas sûrs et vérifier ensuite les douteux, par exemple avec `awk` :

```bash
# Lister les lignes par qualité décroissante (exact d'abord, not_found en queue)
awk -F, 'NR==1 {print; next} {
  rank = ($10=="exact") ? 1 :
         ($10 ~ /^nearest_/) ? 2 :
         ($10=="fuzzy_street") ? 3 :
         ($10=="median_street") ? 4 :
         ($10=="locality") ? 5 : 9
  print rank","$0
}' geocode.csv | sort -t, -k1n | cut -d, -f2-
```

(Découper sur `,` n'est sûr que tant qu'aucun champ ne contient de virgule ; la colonne `label` en contient, donc préfère la voie SQL ou un vrai outil CSV comme `xsv`/`qsv` pour un usage sérieux.)

Ou en SQL si tu charges le CSV dans une table :

```sql
SELECT id, street, postal_code, lat, lng, confidence, fallback_reason, position_source, ambiguous
FROM adresses_geocodees
ORDER BY CASE confidence
           WHEN 'exact'             THEN 1
           WHEN 'nearest_same_side' THEN 2
           WHEN 'nearest_any_side'  THEN 2
           WHEN 'fuzzy_street'      THEN 3
           WHEN 'median_street'     THEN 4
           WHEN 'locality'          THEN 5
           WHEN 'not_found'         THEN 9
           ELSE 6
         END,
         ambiguous,
         street_match_score DESC NULLS LAST;
```

## Et après ?

- Comportement inattendu, performance, erreur au démarrage → [`06-faq.md`](06-faq.md).
- Retour au mode batch → [`03-batch.md`](03-batch.md).
- Retour à l'API en ligne → [`04-recherche-en-ligne.md`](04-recherche-en-ligne.md).
