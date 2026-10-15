# Data files

| File | Used by | Source / licence |
|---|---|---|
| `notability-whitelist-v1.yaml` | indexer `--notability-whitelist` (index time) | Hand-curated; regenerate with `scripts/build_whitelist.py`. |
| `street-type-abbreviations-v1.yaml` | service `--abbreviation-dict` (query time) | Hand-curated. |
| `osm-places-be-v1.csv` | indexer `--places` (index time, schema v8) | © OpenStreetMap contributors, [ODbL 1.0](https://opendatacommons.org/licenses/odbl/1-0/). Regenerate with `scripts/build_osm_places.py`. |

## OpenStreetMap places

`osm-places-be-v1.csv` lists every Belgian OSM node tagged
`place=village|hamlet|locality|isolated_dwelling` with a `name`:

```
osm_id,place,name,name_fr,name_nl,name_de,lat,lon
```

The indexer attaches to each address the nearest place within a radius
that depends on its type (village 3 km, hamlet 1.5 km, locality and
isolated dwelling 1 km), unless the place only repeats the address's
municipality, part-of-municipality or postal name. Attached names become
searchable ("Maredsous", "Frahan"), and every place is listed in the
index's `localities-v1.json` side-file.

The CSV is a derived database of OpenStreetMap data and is distributed
under the Open Database License. Attribution: "© OpenStreetMap
contributors" (https://www.openstreetmap.org/copyright).
