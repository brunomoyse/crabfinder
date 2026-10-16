# Data files

BeSt Address is crabfinder's only source of results. The OpenStreetMap
files below are optional (off unless passed with `--places` / `--pois`)
and only help interpret queries that name a hamlet or a landmark; they
never add to or alter an address.

| File | Used by | Source / licence |
|---|---|---|
| `notability-whitelist-v1.yaml` | indexer `--notability-whitelist` (index time) | Hand-curated; regenerate with `scripts/build_whitelist.py`. |
| `street-type-abbreviations-v1.yaml` | service `--abbreviation-dict` (query time) | Hand-curated. |
| `osm-places-be-v1.csv` | indexer `--places` (index time, schema v8+) | © OpenStreetMap contributors, [ODbL 1.0](https://opendatacommons.org/licenses/odbl/1-0/). Regenerate with `scripts/build_osm_places.py`. |
| `osm-pois-be-v1.csv` | service / batch `--pois` (query time, no reindex) | © OpenStreetMap contributors, [ODbL 1.0](https://opendatacommons.org/licenses/odbl/1-0/). Regenerate with `scripts/build_osm_pois.py`. |

## OpenStreetMap places

`osm-places-be-v1.csv` lists every Belgian OSM node tagged
`place=village|hamlet|locality|isolated_dwelling` with a `name`:

```
osm_id,place,name,name_fr,name_nl,name_de,lat,lon
```

The indexer matches each address to the nearest place within a radius
that depends on its type (village 3 km, hamlet 1.5 km, locality and
isolated dwelling 1 km), to give each place its postal codes. Nothing is
added to the address (schema v11). Every place is listed in the index's
`localities-v1.json` side-file, which the service's locality step uses to
recognise "Maredsous" and search there; the answer is the nearest BeSt
address.

## OpenStreetMap landmarks

`osm-pois-be-v1.csv` lists Belgian OSM nodes, ways and relations (ways
and relations at their centre) with a `name` and a `wikidata` tag (the notability filter) and a landmark tag:
`tourism=attraction|museum|zoo|
theme_park|aquarium|gallery`, `historic=castle|monument|memorial|fort|
citadel|archaeological_site|city_gate|tower|ruins`, `amenity=townhall|
university|place_of_worship|theatre|concert_hall|hospital`,
`railway=station`, `aeroway=aerodrome`, `leisure=stadium|park`,
`building=cathedral|basilica|palace`, `man_made=tower|lighthouse`:

```
osm_type,osm_id,kind,name,name_fr,name_nl,name_de,name_en,lat,lon
```

The service's landmark step recognises "Manneken Pis" or "Gravensteen
Gent" when no street matches and answers with the BeSt addresses next to
the landmark; the landmark's own position is never returned.

Both CSVs are derived databases of OpenStreetMap data and are distributed
under the Open Database License. Attribution: "© OpenStreetMap
contributors" (https://www.openstreetmap.org/copyright).
