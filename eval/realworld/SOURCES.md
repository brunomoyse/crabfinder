# Real-world eval set — sources

`eval/realworld-be-v1.yaml` samples addresses from these open-data lists
(downloaded by `scripts/build_realworld_eval.py`, cached in `eval/realworld/sources/`,
which is gitignored). Only the address and the published point of each sampled
record are kept.

| key | dataset | publisher | licence | usable | sampled |
|---|---|---|---|---|---|
| `fwb-schools-brussels` | Fichier signalétique des établissements d'enseignement (Brussels sites) | Fédération Wallonie-Bruxelles (via ODWB) | CC BY 4.0 | 652 | 300 |
| `fwb-schools-wallonia` | Fichier signalétique des établissements d'enseignement (Walloon sites) | Fédération Wallonie-Bruxelles (via ODWB) | CC BY 4.0 | 3518 | 400 |
| `brussels-bpost` | Points postaux (bpost) — Ville de Bruxelles | Ville de Bruxelles (opendata.brussels) | CC BY 4.0 | 151 | 100 |
| `brussels-pharmacies` | Pharmacies — Ville de Bruxelles | Ville de Bruxelles (opendata.brussels) | CC0 1.0 | 105 | 60 |
| `namur-commerces` | Commerces namurois | Ville de Namur (via ODWB) | CC BY 4.0 | 1903 | 150 |
| `flanders-schools` | POI-dienst — thema Onderwijs | Vlaams Ministerie van Onderwijs en Vorming / Digitaal Vlaanderen | Modellicentie Gratis Hergebruik | 5054 | 250 |
| `flanders-care` | POI-dienst — thema Welzijn, gezondheid en gezin | Agencies named per record (AUTEUR) / Digitaal Vlaanderen | Modellicentie Gratis Hergebruik | 9533 | 250 |

Download URLs:

- `fwb-schools-brussels`: <https://www.odwb.be/api/explore/v2.1/catalog/datasets/fwb-age-fichier-signaletique-des-etablissements-d-enseignement-de-la-federation-/exports/csv>
- `brussels-bpost`: <https://opendata.brussels.be/api/explore/v2.1/catalog/datasets/points-postaux0/exports/csv>
- `brussels-pharmacies`: <https://opendata.brussels.be/api/explore/v2.1/catalog/datasets/pharmacies0/exports/csv>
- `namur-commerces`: <https://www.odwb.be/api/explore/v2.1/catalog/datasets/commerces-namurois/exports/csv>
- `flanders-schools`: <https://geo.api.vlaanderen.be/POI/ogc/features/v1/collections/POI/items (filter THEMA='Onderwijs')>
- `flanders-care`: <https://geo.api.vlaanderen.be/POI/ogc/features/v1/collections/POI/items (filter THEMA='Welzijn, gezondheid en gezin')>

## Attribution

- CC BY 4.0 data: © the publisher named above, <https://creativecommons.org/licenses/by/4.0/>.
- Flemish POI data: "Bron: Vlaams Ministerie van Onderwijs en Vorming" (schools) and
  the agency named in each record's `AUTEUR` (care), under the Flemish
  [Modellicentie Gratis Hergebruik](https://www.vlaanderen.be/digitaal-vlaanderen/onze-oplossingen/open-data/voorwaarden-voor-het-hergebruik-van-overheidsinformatie).
- CC0 data needs no attribution.

## Caveats

- A published point is a building, an entrance or a parcel centroid, not the
  BeSt address point: 10–50 m disagreements are normal, hence the 100 m radius.
- Flemish POI entries say where their point comes from (`notes: "point: …"`):
  `basisregister` points are the address register's own, so they are not
  independent of BeSt; `self-defined` ones are the publisher's.
- Combined street + number fields are split with a regular expression;
  unparseable rows, rows without a plain house number and duplicates
  (one per address across all sources) are dropped.
