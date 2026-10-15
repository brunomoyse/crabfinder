# 1. Introduction

## Ce que crabfinder fait

crabfinder transforme des adresses belges en coordonnées géographiques (latitude / longitude WGS84) et inversement. Il utilise les données officielles **BeSt Address**, qui couvrent les trois régions du pays et représentent environ 6,9 millions d'adresses (état dump 2026-10). Tout est local : pas d'appel à un service tiers, pas de quota, pas de fuite de données.

Il y a deux façons concrètes de l'utiliser :

### Mode 1 — recherche en ligne (interactif)

Un utilisateur tape une adresse dans un formulaire. Le service répond avec un top-N classé par pertinence, en GeoJSON. Latence typique de quelques millisecondes (p95 ~7 ms).

```text
Question : « Grand-Place 1 1000 Bruxelles »
Réponse  : top 10 adresses candidates, la Grand-Place de Bruxelles en première
           position, avec lat/lng et toutes les métadonnées (nom de rue FR/NL,
           commune, code postal, identifiants BeSt).
```

C'est le mode idéal pour un champ de recherche ou une carte interactive (recherche à la validation : crabfinder ne complète pas les mots à moitié tapés). Voir [`04-recherche-en-ligne.md`](04-recherche-en-ligne.md).

### Mode 2 — géocodage batch (traitement de liste)

Un fichier CSV en entrée, soit en colonnes séparées (`id`, `street`, `house_number`, `postal_code`), soit avec une seule colonne `address` en texte libre. Un fichier CSV (ou GeoJSON) enrichi en sortie avec lat/lng et indicateurs de confiance pour chaque ligne, **dans le même ordre, avec le même nombre de lignes** que l'entrée.

```text
adresses.csv (3 lignes)        →  adresses-geocodees.csv (3 lignes)
id,street,house_number,postal_code   id,...,lat,lng,confidence,fallback_reason,...
1,Avenue Louise,32,1050              1,...,50.832,4.367,exact,,...
2,Grand-Place,,1000                  2,...,50.847,4.353,median_street,house_number_missing,...
3,Rue Inexistante,99,1000            3,...,,,not_found,no_match,...
```

C'est le mode pertinent pour enrichir une base de données existante, géocoder un export Excel, ou alimenter un croisement spatial. Voir [`03-batch.md`](03-batch.md).

## Ce que crabfinder **ne fait pas**

À savoir avant de l'adopter, pour éviter les surprises :

- **Pas d'autres pays que la Belgique.** L'index est figé sur BeSt Address. Pas de France, pas de Luxembourg, pas de Pays-Bas même limitrophes.
- **Pas de POI.** Pas de noms de gare, pas de commerces, pas de noms de monuments. Si tu cherches « Atomium », ça matchera l'adresse postale officielle de l'Atomium si elle est dans BeSt — pas l'objet « Atomium » en tant que tel. Les noms de lieux, eux, *sont* compris : communes, sections de commune, zones postales, et villages et hameaux OpenStreetMap (« Maredsous », « Spontin »).
- **Pas d'API publique en ligne.** crabfinder est un binaire que tu installes et fais tourner toi-même. Pas de SaaS.
- **Pas d'authentification.** Le service écoute sur `127.0.0.1:8080` par défaut. Si tu veux l'exposer sur Internet, mets un reverse-proxy + auth devant. Pour un usage local ou intra-LAN, c'est sans souci.
- **Pas de mise à jour automatique des données.** Quand BeSt publie un nouveau dump, tu lances `scripts/refresh_best.sh` (téléchargement, reconstruction ~1,5-3 min, vérification, bascule) et tu redémarres le service.

## Précision spatiale : ce que tu obtiens vraiment

Selon le cas, crabfinder retourne des coordonnées à plusieurs niveaux de précision. Voir [`05-comprendre-resultats.md`](05-comprendre-resultats.md) pour le détail.

| Niveau | Quand | Champ `confidence` |
|---|---|---|
| Bâtiment exact | Numéro de rue présent dans BeSt | `exact` |
| Bâtiment voisin | Numéro absent de BeSt, un numéro proche existe | `nearest_same_side`, `nearest_any_side` |
| Centroïde de rue (médian) | Pas de numéro, ou aucun assez proche | `median_street` |
| Bâtiment ou rue dans une commune voisine | Rien dans le code postal donné, la même rue juste à côté | `adjacent_commune` |
| Bâtiment exact, rue approchée | Faute de frappe ou match partiel sur le nom de rue | `fuzzy_street` |
| Centre du village / de la commune | La requête ne nomme qu'un lieu | `locality` |
| Pas de coordonnées | Rien de plausible | `not_found` |

Le destinataire d'un CSV géocodé peut donc trier ses lignes par confiance et inspecter manuellement la queue. Deux indicateurs complètent le tableau : `ambiguous` (une autre réponse presque aussi bonne existe ailleurs) et `position_source` (le point a-t-il été publié par BeSt ou estimé à partir des voisines, pour les ~140k adresses wallonnes que BeSt publie sans position).

## Couverture multilingue

Les adresses bilingues (Bruxelles, communes à facilités) sont indexées dans **toutes leurs variantes** (FR, NL, DE quand disponible). Tu peux taper indifféremment « Avenue Louise 1050 » ou « Louizalaan 1050 » — la même adresse remonte. Les communes germanophones (Eupen, Saint-Vith, …) sont également couvertes.

## Et après ?

- Premier setup → [`02-installation.md`](02-installation.md).
- Tu as déjà installé, tu veux géocoder un CSV → [`03-batch.md`](03-batch.md).
- Tu veux une API REST → [`04-recherche-en-ligne.md`](04-recherche-en-ligne.md).
