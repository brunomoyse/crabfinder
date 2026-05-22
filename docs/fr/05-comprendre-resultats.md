# 5. Comprendre les résultats

À lire avant d'exploiter un CSV géocodé ou de prendre une décision automatisée à partir d'une réponse `/geocode`. Cette section explique ce que veulent dire `confidence`, `fallback_reason`, `score`, et comment les utiliser pour trier ou filtrer.

## La règle d'or

**Toute ligne de sortie a une coordonnée *et* un niveau de confiance**, sauf `not_found` qui n'a ni l'un ni l'autre. La qualité varie. C'est à toi de décider ce que tu acceptes selon ton cas d'usage.

Exemples typiques de seuils d'acceptation :

| Usage | Acceptable | À inspecter | À rejeter |
|---|---|---|---|
| Carte avec marqueurs par bâtiment | `exact` | `fuzzy_street`, `nearest_*` | `median_street`, `not_found` |
| Agrégation par quartier / commune | `exact`, `median_street`, `fuzzy_street` | `nearest_*`, `adjacent_commune` | `not_found` |
| Croisement spatial avec polygone (densité, secteur) | tout sauf `not_found` | — | `not_found` |
| Adresse de facturation | `exact` seulement | tout le reste | tout le reste |

## La taxonomie `confidence`

Sept valeurs possibles, dans l'ordre du plus fort au plus faible.

### `exact` — match parfait

Le numéro précis a été trouvé dans BeSt, à la rue indiquée, au code postal indiqué.

```
input :  Avenue Louise 32 1050
trouvé : Avenue Louise 32 1050 Ixelles, lat=50.832 lng=4.367
        confidence = exact
        fallback_reason = (vide)
```

**Action** : utiliser tel quel.

### `median_street` — centroïde de rue

Le numéro demandé n'existe pas dans BeSt (numéro manquant en entrée, ou présent mais inexistant dans le dataset officiel). crabfinder retourne un **point représentatif de la rue** : les coordonnées du bâtiment dont le numéro est à la médiane des numéros existants pour le couple `(rue, CP)`.

```
input :  Grand-Place 1000     (pas de house_number)
trouvé : Grand-Place 1000, lat=50.847 lng=4.353 (médiane des numéros 1 à 19)
        confidence = median_street
        fallback_reason = house_number_missing
        house_number_delta = (vide)
```

**Précision spatiale** : niveau **rue**, pas niveau bâtiment. La dérive peut être de quelques mètres dans une rue courte, ou de plusieurs centaines de mètres dans une rue longue. Pour de l'agrégation à la maille IRIS / quartier, c'est généralement acceptable. Pour mettre un marqueur précis sur une carte, c'est trompeur — afficher au minimum une icône différente.

**Action** : utilisable pour des agrégats spatiaux, à éviter pour de l'analyse au niveau bâtiment.

Deux `fallback_reason` possibles dans ce cas :

- `house_number_missing` : le champ `house_number` était vide en entrée.
- `house_number_out_of_range` : un numéro était fourni mais inexistant dans BeSt pour cette rue.

### `fuzzy_street` — rue trouvée approximativement

La passe stricte a échoué (rue mal orthographiée, abréviation non reconnue…). La passe fuzzy OR-of-tokens a trouvé une rue qui partage assez de tokens avec ta requête. Le résultat peut être :

- Le bâtiment exact à cette rue floue (si `house_number` matchait après coup) → coordonnées niveau bâtiment.
- Le centroïde de la rue floue (si `house_number` ne matchait pas non plus) → coordonnées niveau rue, `fallback_reason = house_number_missing` cumulé.

```
input :  Avenu Louise 32 1050     (faute de frappe sur "Avenue")
trouvé : Avenue Louise 32 1050, lat=50.832 lng=4.367
        confidence = fuzzy_street
        fallback_reason = street_spelling_fuzzy
        street_match_score = bas (typiquement < 10)
```

**Action** : à inspecter. Le score BM25 (`street_match_score`) est un bon indicateur de la qualité du match texte ; les valeurs basses (< 10 sur l'eval Tier A) suggèrent un faux positif.

### `not_found` — aucune coordonnée

Ni la passe stricte ni la passe fuzzy n'ont trouvé de candidat plausible. La ligne sort avec `lat/lng/matched_* = vide`, `confidence = not_found`, `fallback_reason = no_match`.

```
input :  Rue Inexistante du Test 99 1000
trouvé : (rien)
        confidence = not_found
        fallback_reason = no_match
```

**Action** : à investiguer ligne par ligne. Causes possibles :
- Faute de frappe sévère que le fuzzy n'arrive pas à rattraper.
- Code postal incorrect (la rue n'existe pas à ce CP).
- Adresse récente non encore dans le dump BeSt (refresh requis).
- Adresse hors Belgique.
- Adresse historiquement supprimée (le dump BeSt filtre les statuts non-actifs).

Sur l'eval Tier A (~2600 requêtes), le taux `not_found` est de l'ordre de 0,1%. Si ton fichier a un taux substantiellement plus élevé, il y a probablement un problème systématique d'entrée (encodage, CP manquants en masse, etc.) à investiguer.

### Confiances réservées aux versions futures

Ces trois valeurs sont définies dans le schéma de sortie mais **non émises en v1**. Elles deviendront actives quand la fonctionnalité correspondante sera implémentée :

- `nearest_same_side` — numéro le plus proche de la même parité (= même côté de rue). Inspiré de PhacochR §2.2.
- `nearest_any_side` — numéro le plus proche, n'importe quel côté.
- `adjacent_commune` — rue trouvée dans une commune limitrophe (CP d'entrée incorrect).

Tes scripts peuvent les ignorer pour l'instant ; ils ne sortiront jamais d'un crabfinder v1.

## La taxonomie `fallback_reason`

Plus fine que `confidence`. Quand `confidence != exact`, dit **pourquoi** :

| `fallback_reason` | Sens |
|---|---|
| `house_number_missing` | Le champ `house_number` était vide en entrée. |
| `house_number_out_of_range` | Un numéro était fourni mais inexistant pour cette rue dans BeSt. |
| `street_spelling_fuzzy` | La passe stricte n'a pas trouvé la rue, le fuzzy l'a rattrapée. |
| `postal_code_mismatch` | Réservé (futur fallback commune adjacente). |
| `no_match` | Rien trouvé du tout. Accompagne toujours `confidence=not_found`. |

Cas combiné : une ligne `confidence=fuzzy_street, fallback_reason=house_number_missing` signifie « j'ai approximé la rue *et* tu n'avais pas fourni de numéro ». C'est une double dégradation : à inspecter plus que les cas single-dégradation.

## Le score `street_match_score`

C'est le score **BM25 brut** de la passe Tantivy sur le champ `searchable` (qui contient toutes les variantes multilingues du nom de rue + nom de commune + CP). Pas un score normalisé. Pas un pourcentage. Une grandeur continue dépendante de :

- la fréquence des tokens dans la requête,
- la fréquence des tokens dans le corpus (les tokens rares pèsent plus),
- le boost de notabilité éventuel (Grand-Place, Atomium, etc.).

Sur l'eval Tier A, on a observé (sur les hits `exact`) :

| Quartile | Valeur BM25 |
|---|---|
| q25 | ~25.8 |
| q50 (médiane) | ~32.8 |
| q75 | ~101.1 |
| q95 | ~144.4 |

**À retenir** : un score sous le q25 sur un hit `exact` est *suspect* (match texte faible, peut-être un nom de rue très court). Un score au-delà du q75 est `exact` solide. Pour comparer deux résultats, le score est exploitable ; pour décider en absolu, **c'est `confidence` qui est le bon outil**, pas le score.

Pour `fuzzy_street`, le score est typiquement plus bas (5-15). Pour `median_street` et `not_found`, le champ est vide.

## Le champ `house_number_delta`

Rempli en v1 uniquement pour les confiances qui le justifient (à terme : `nearest_*`). En v1 actuelle, **toujours vide ou `0`**. Réservé.

## Les identifiants `building_group_id` et `best_id`

Deux références exploitables côté aval :

- `building_group_id` : identifiant synthétique crabfinder. Stable d'un dump BeSt à l'autre tant que le couple `(street_id, house_number)` ne change pas. Utile pour grouper côté toi (toutes les boîtes d'un même immeuble partagent le même `building_group_id`).
- `best_id` : identifiant officiel BeSt, sous forme `namespace/object_identifier/version`. Permet de retrouver la ligne BeSt source (utile pour audit ou pour rejoindre avec d'autres données BeSt).

## Mini-recette de tri en aval

Tu peux trier ton CSV géocodé par confiance + score pour traiter d'abord les cas certains puis inspecter les douteux, par exemple avec `awk` :

```bash
# Lister les lignes par qualité décroissante (exact d'abord, not_found en queue)
awk -F, 'NR==1 {print; next} {
  rank = ($10=="exact") ? 1 :
         ($10=="fuzzy_street") ? 2 :
         ($10=="median_street") ? 3 : 4
  print rank","$0
}' geocodees.csv | sort -t, -k1n | cut -d, -f2-
```

Ou en SQL si tu charges le CSV dans une table :

```sql
SELECT id, street, postal_code, lat, lng, confidence, fallback_reason
FROM adresses_geocodees
ORDER BY CASE confidence
           WHEN 'exact'          THEN 1
           WHEN 'fuzzy_street'   THEN 2
           WHEN 'median_street'  THEN 3
           WHEN 'not_found'      THEN 9
           ELSE 5
         END,
         street_match_score DESC NULLS LAST;
```

## Et après ?

- Comportement inattendu, performance, erreur de démarrage → [`06-faq.md`](06-faq.md).
- Revenir au mode batch → [`03-batch.md`](03-batch.md).
- Revenir à l'API en ligne → [`04-recherche-en-ligne.md`](04-recherche-en-ligne.md).
