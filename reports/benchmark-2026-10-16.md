# Belgian geocoder benchmark, 2026-10-16

Phase 11.1, refreshed after the Phase 11.1b ranking fixes, then rerun with the NGI national geocoder and BePelias added. One run of `crabfinder-eval --benchmark` (see `docs/eval.md` §Benchmarking) per query set:

- **Real-world** (`eval/realworld-be-v1.yaml`): 1,510 addresses from 7 public open-data lists (schools, pharmacies, bpost offices, care facilities, shops; sources and licences in `eval/realworld/SOURCES.md`). The reference point is the coordinate the publisher gives, so it isn't exact; a hit means within 100 m.
- **Synthetic + adversarial** (`eval/belgium-2026-10-15-v3.json`, `eval/adversarial-belgium-v1.yaml`), with `--with-points`: each expected BeSt building gives the reference point. These sets come from BeSt and crabfinder's own failure history, so they **favour crabfinder**. The real-world set is the fairer comparison.

## Setup

| Geocoder | What | How it was asked |
|---|---|---|
| `crabfinder` | this repo with the Phase 11.1b ranking fixes, index schema v12 from BeSt 2026-10-15, BeSt-only | free text, `/geocode` |
| `crabfinder-osm` | same, index built with `--places`, service with `--pois` (the optional OSM aids) | free text |
| `brussels` | Brussels regional geocoder (Paradigm / UrbIS), `geoservices.brussels` | free text with fr/nl fallback, Brussels queries only, 2 req/s |
| `vlaanderen` | Digitaal Vlaanderen Geolocation API v4 | free text, Flanders + Brussels queries, 2 req/s |
| `wallonie` | SPW `geoservices.wallonie.be/geocodeWS` (ICAR) | structured street/house/city when known, else free text; Wallonia only, 2 req/s |
| `ngi` | NGI-IGN national geocoder (GeoSearchEngine, built with Smals), `geosearch.ngi.be` | free text, `/search/text?source=ALL_SOURCES`, `FAST` then `FULL` mode, 2 req/s |
| `bepelias` | SmalsResearch BePelias (Pelias over BeSt), self-hosted from its Docker compose on a separate machine, fed from the same BeSt dump (2026-10-15) | structured street/number/postcode/city when known, else `/geocode/unstructured`, 10 req/s |
| `phacochr` | PhacochR (R package over BeSt + UrbIS), local | one batch through `scripts/bench/phacochr.R` |

Competitor responses are cached in `eval/bench-cache/` (gitignored), so a rerun only queries crabfinder.

## Notes

- **NGI:** the national geocoder announced at BeGeo 2024 as "search.geo.be" runs at `https://geosearch.ngi.be`, keyless, OpenAPI at `/v3/api-docs` (found 2026-10-16). Its addresses are BeSt's and carry BeSt ids, so it gets `recall@1`. What holds it back here:
  - the postal code doesn't filter: "Wollestraat 25, 8000 Brugge" returns a Wollestraat 25 elsewhere in the municipality of Brugge, 10 km away;
  - no abbreviations (`Wielewaalstr.`), no cross-language queries (`Gand Einde Were 15`), no neighbouring-commune relaxation (`wrong_postal` 0.018);
  - a street name without a street type and a postal code without a city (`Kolmen 28D 3980`) is rejected (HTTP 400, "House number provided without streetname"), 6 queries.

  Its field-by-field endpoint (`/search/field`) was tried on a sample of 60 real-world misses and fixed 1, so the adapter sends free text.
- **BePelias:** built from its repository's Docker compose with the 2026-10-15 BeSt dump, every container memory-capped. Its BeSt interpolation step failed (the feed script runs it in a directory the container user can't write to, then deletes its input CSVs), so for BeSt addresses without coordinates BePelias falls back to the generic Pelias interpolation data or the street centre. That costs it 2 real-world and 16 synthetic misses. A `(0, 0)` answer is counted as no position. 7 synthetic queries got HTTP 500 from BePelias. Its typo, abbreviation and messy-punctuation misses mostly come back as a city-level answer.
- **Regional services** are asked only the queries in their region. Compare them within the `region:*` rows, not on the overall line.
- **vlaanderen in Brussels:** the Flemish API barely covers French-language Brussels addresses (found rate 0.095 on the real-world Brussels slice). Its Brussels numbers say little about Flanders, where it is close to crabfinder.
- **Latency:** HTTP services were measured from the benchmark machine over the internet; BePelias over the local network, on a smaller machine (an Apple-silicon homelab under Asahi Linux, 15 GB RAM). crabfinder ran on localhost. PhacochR's figure is the batch wall clock divided by the number of queries.
- **`recall@1`** needs BeSt ids. crabfinder, NGI, BePelias and the Brussels geocoder (whose ids map back to BeSt) have it. Every geocoder gets the distance metrics.
- **Losses:** the "Queries crabfinder loses" lists are queries where another geocoder lands within 100 m and crabfinder doesn't. They feed the ranking backlog. Adding NGI added one real-world loss (`Rue du Monument 1, 5590 Ciney`, crabfinder 9.9 km off) and 9 synthetic ones; adding BePelias, 6 real-world (mostly 100 to 130 m cases where its point sits nearer the publisher's; the real miss is `Route de Perwez 3, 5310 Eghezée`, 1.6 km) and 17 synthetic ones.
- **Phase 11.1b** (village words after the street, a leading street type BeSt doesn't have, typos corrected inside the postal code, `1er`/`ier`): real-world ≤ 100 m 0.923 → 0.929 (Wallonia 0.871 → 0.885), losses 31 → 22; synthetic + adversarial recall@1 0.964 → 0.965. The first run, before these fixes, is in this file's git history. crabfinder's numbers are unchanged on the schema v12 index (Phase 11.4).
- **Remaining real-world losses:** most are 100–250 m off, the gap between the BeSt address point and the publisher's own point. Real misses left: `Pte` for Petite, "L Astronomie" without the apostrophe, mojibake in the source (`Cloà®tre`), a few square names whose BeSt name differs ("Place du temps libre", "Place des Combattants et Déportés"), a hamlet used as a street (`Borsu 4, 4560 Clavier`), `Rue du Monument 1, 5590 Ciney` and `Route de Perwez 3, 5310 Eghezée`.

## Real-world set (`eval/realworld-be-v1.yaml`)

- Queries: **1510** (1510 with a reference point)
- A top-1 within 100 m of the reference point counts as a hit in the head-to-head and the losses list.
- Each geocoder is only asked the queries it covers (a regional service, its region): compare them within a region row.
- recall@1 needs BeSt ids; it is `–` for geocoders that return none.

### Overall

| Geocoder | asked | errors | recall@1 | n | found | ≤25 m | ≤100 m | ≤250 m | median | p90 | p50 | p95 |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| crabfinder | 1510 | 0 | 0.929 | 1510 | 1.000 | 0.736 | 0.929 | 0.967 | 5 m | 72 m | 1.9 ms | 18.7 ms |
| crabfinder-osm | 1510 | 0 | 0.929 | 1510 | 1.000 | 0.736 | 0.929 | 0.967 | 5 m | 72 m | 1.9 ms | 18.8 ms |
| brussels | 496 | 0 | 0.925 | 496 | 0.980 | 0.780 | 0.925 | 0.942 | 7 m | 51 m | 111.2 ms | 504.3 ms |
| vlaanderen | 960 | 0 | – | 960 | 0.526 | 0.481 | 0.505 | 0.510 | 0 m | 13 m | 85.7 ms | 200.9 ms |
| wallonie | 550 | 0 | – | 550 | 0.989 | 0.500 | 0.847 | 0.911 | 24 m | 186 m | 126.6 ms | 308.0 ms |
| ngi | 1510 | 0 | 0.830 | 1510 | 0.991 | 0.678 | 0.836 | 0.885 | 7 m | 325 m | 55.0 ms | 111.8 ms |
| bepelias | 1510 | 0 | 0.908 | 1510 | 1.000 | 0.726 | 0.908 | 0.954 | 5 m | 89 m | 63.9 ms | 242.8 ms |
| phacochr | 1510 | 0 | – | 1510 | 0.979 | 0.699 | 0.907 | 0.952 | 7 m | 77 m | 19.8 ms | 19.8 ms |

### By slice (distance to the reference point)

#### realworld

| Geocoder | n | found | ≤25 m | ≤100 m | ≤250 m | median | p90 |
|---|---|---|---|---|---|---|---|
| crabfinder | 1510 | 1.000 | 0.736 | 0.929 | 0.967 | 5 m | 72 m |
| crabfinder-osm | 1510 | 1.000 | 0.736 | 0.929 | 0.967 | 5 m | 72 m |
| brussels | 496 | 0.980 | 0.780 | 0.925 | 0.942 | 7 m | 51 m |
| vlaanderen | 960 | 0.526 | 0.481 | 0.505 | 0.510 | 0 m | 13 m |
| wallonie | 550 | 0.989 | 0.500 | 0.847 | 0.911 | 24 m | 186 m |
| ngi | 1510 | 0.991 | 0.678 | 0.836 | 0.885 | 7 m | 325 m |
| bepelias | 1510 | 1.000 | 0.726 | 0.908 | 0.954 | 5 m | 89 m |
| phacochr | 1510 | 0.979 | 0.699 | 0.907 | 0.952 | 7 m | 77 m |

#### realworld:brussels-bpost

| Geocoder | n | found | ≤25 m | ≤100 m | ≤250 m | median | p90 |
|---|---|---|---|---|---|---|---|
| crabfinder | 100 | 1.000 | 0.730 | 0.900 | 0.970 | 12 m | 82 m |
| crabfinder-osm | 100 | 1.000 | 0.730 | 0.900 | 0.970 | 12 m | 82 m |
| brussels | 100 | 0.940 | 0.700 | 0.870 | 0.930 | 13 m | 72 m |
| vlaanderen | 100 | 0.080 | 0.050 | 0.070 | 0.080 | 9 m | 248 m |
| ngi | 100 | 1.000 | 0.670 | 0.820 | 0.880 | 14 m | 326 m |
| bepelias | 100 | 1.000 | 0.660 | 0.840 | 0.930 | 14 m | 155 m |
| phacochr | 100 | 0.960 | 0.710 | 0.870 | 0.930 | 13 m | 82 m |

#### realworld:brussels-pharmacies

| Geocoder | n | found | ≤25 m | ≤100 m | ≤250 m | median | p90 |
|---|---|---|---|---|---|---|---|
| crabfinder | 60 | 1.000 | 1.000 | 1.000 | 1.000 | 0 m | 1 m |
| crabfinder-osm | 60 | 1.000 | 1.000 | 1.000 | 1.000 | 0 m | 1 m |
| brussels | 60 | 1.000 | 1.000 | 1.000 | 1.000 | 0 m | 1 m |
| vlaanderen | 60 | 0.000 | 0.000 | 0.000 | 0.000 | – | – |
| ngi | 60 | 1.000 | 0.967 | 0.967 | 0.967 | 0 m | 2 m |
| bepelias | 60 | 1.000 | 1.000 | 1.000 | 1.000 | 1 m | 2 m |
| phacochr | 60 | 1.000 | 1.000 | 1.000 | 1.000 | 0 m | 3 m |

#### realworld:flanders-care

| Geocoder | n | found | ≤25 m | ≤100 m | ≤250 m | median | p90 |
|---|---|---|---|---|---|---|---|
| crabfinder | 250 | 1.000 | 0.860 | 0.928 | 0.940 | 0 m | 47 m |
| crabfinder-osm | 250 | 1.000 | 0.860 | 0.928 | 0.940 | 0 m | 47 m |
| brussels | 18 | 1.000 | 0.556 | 0.556 | 0.556 | 0 m | 2851 m |
| vlaanderen | 250 | 0.992 | 0.852 | 0.920 | 0.932 | 0 m | 49 m |
| ngi | 250 | 1.000 | 0.828 | 0.892 | 0.908 | 0 m | 123 m |
| bepelias | 250 | 1.000 | 0.856 | 0.924 | 0.940 | 1 m | 50 m |
| phacochr | 250 | 1.000 | 0.828 | 0.904 | 0.940 | 0 m | 76 m |

#### realworld:flanders-schools

| Geocoder | n | found | ≤25 m | ≤100 m | ≤250 m | median | p90 |
|---|---|---|---|---|---|---|---|
| crabfinder | 250 | 1.000 | 0.980 | 0.992 | 1.000 | 0 m | 1 m |
| crabfinder-osm | 250 | 1.000 | 0.980 | 0.992 | 1.000 | 0 m | 1 m |
| brussels | 18 | 1.000 | 1.000 | 1.000 | 1.000 | 0 m | 1 m |
| vlaanderen | 250 | 0.976 | 0.960 | 0.972 | 0.976 | 0 m | 1 m |
| ngi | 250 | 1.000 | 0.908 | 0.928 | 0.936 | 0 m | 9 m |
| bepelias | 250 | 1.000 | 0.956 | 0.984 | 0.992 | 1 m | 2 m |
| phacochr | 250 | 0.964 | 0.860 | 0.932 | 0.964 | 0 m | 34 m |

#### realworld:fwb-schools-brussels

| Geocoder | n | found | ≤25 m | ≤100 m | ≤250 m | median | p90 |
|---|---|---|---|---|---|---|---|
| crabfinder | 300 | 1.000 | 0.773 | 0.953 | 0.970 | 11 m | 53 m |
| crabfinder-osm | 300 | 1.000 | 0.773 | 0.953 | 0.970 | 11 m | 53 m |
| brussels | 300 | 0.987 | 0.763 | 0.947 | 0.953 | 11 m | 51 m |
| vlaanderen | 300 | 0.017 | 0.013 | 0.017 | 0.017 | 8 m | 44 m |
| ngi | 300 | 1.000 | 0.730 | 0.907 | 0.933 | 12 m | 98 m |
| bepelias | 300 | 1.000 | 0.770 | 0.943 | 0.960 | 11 m | 59 m |
| phacochr | 300 | 0.993 | 0.757 | 0.960 | 0.973 | 11 m | 54 m |

#### realworld:fwb-schools-wallonia

| Geocoder | n | found | ≤25 m | ≤100 m | ≤250 m | median | p90 |
|---|---|---|---|---|---|---|---|
| crabfinder | 400 | 1.000 | 0.370 | 0.845 | 0.945 | 35 m | 148 m |
| crabfinder-osm | 400 | 1.000 | 0.370 | 0.845 | 0.945 | 35 m | 148 m |
| wallonie | 400 | 0.988 | 0.357 | 0.805 | 0.892 | 36 m | 234 m |
| ngi | 400 | 0.970 | 0.300 | 0.650 | 0.782 | 48 m | 1243 m |
| bepelias | 400 | 1.000 | 0.375 | 0.802 | 0.922 | 36 m | 199 m |
| phacochr | 400 | 0.960 | 0.350 | 0.818 | 0.917 | 36 m | 129 m |

#### realworld:namur-commerces

| Geocoder | n | found | ≤25 m | ≤100 m | ≤250 m | median | p90 |
|---|---|---|---|---|---|---|---|
| crabfinder | 150 | 1.000 | 0.920 | 0.993 | 0.993 | 4 m | 19 m |
| crabfinder-osm | 150 | 1.000 | 0.920 | 0.993 | 0.993 | 4 m | 19 m |
| wallonie | 150 | 0.993 | 0.880 | 0.960 | 0.960 | 4 m | 27 m |
| ngi | 150 | 0.993 | 0.840 | 0.900 | 0.907 | 5 m | 53 m |
| bepelias | 150 | 1.000 | 0.913 | 0.973 | 0.980 | 4 m | 20 m |
| phacochr | 150 | 1.000 | 0.900 | 0.993 | 0.993 | 4 m | 24 m |

#### region:brussels

| Geocoder | n | found | ≤25 m | ≤100 m | ≤250 m | median | p90 |
|---|---|---|---|---|---|---|---|
| crabfinder | 496 | 1.000 | 0.792 | 0.935 | 0.960 | 7 m | 61 m |
| crabfinder-osm | 496 | 1.000 | 0.792 | 0.935 | 0.960 | 7 m | 61 m |
| brussels | 496 | 0.980 | 0.780 | 0.925 | 0.942 | 7 m | 51 m |
| vlaanderen | 496 | 0.095 | 0.071 | 0.077 | 0.079 | 1 m | 1310 m |
| ngi | 496 | 1.000 | 0.746 | 0.885 | 0.913 | 9 m | 153 m |
| bepelias | 496 | 1.000 | 0.774 | 0.915 | 0.946 | 8 m | 75 m |
| phacochr | 496 | 0.986 | 0.776 | 0.931 | 0.952 | 9 m | 61 m |

#### region:flanders

| Geocoder | n | found | ≤25 m | ≤100 m | ≤250 m | median | p90 |
|---|---|---|---|---|---|---|---|
| crabfinder | 464 | 1.000 | 0.931 | 0.974 | 0.985 | 0 m | 11 m |
| crabfinder-osm | 464 | 1.000 | 0.931 | 0.974 | 0.985 | 0 m | 11 m |
| vlaanderen | 464 | 0.987 | 0.920 | 0.963 | 0.972 | 0 m | 10 m |
| ngi | 464 | 1.000 | 0.879 | 0.922 | 0.935 | 0 m | 45 m |
| bepelias | 464 | 1.000 | 0.918 | 0.970 | 0.981 | 1 m | 14 m |
| phacochr | 464 | 0.983 | 0.851 | 0.931 | 0.968 | 0 m | 45 m |

#### region:wallonia

| Geocoder | n | found | ≤25 m | ≤100 m | ≤250 m | median | p90 |
|---|---|---|---|---|---|---|---|
| crabfinder | 550 | 1.000 | 0.520 | 0.885 | 0.958 | 23 m | 111 m |
| crabfinder-osm | 550 | 1.000 | 0.520 | 0.885 | 0.958 | 23 m | 111 m |
| wallonie | 550 | 0.989 | 0.500 | 0.847 | 0.911 | 24 m | 186 m |
| ngi | 550 | 0.976 | 0.447 | 0.718 | 0.816 | 32 m | 991 m |
| bepelias | 550 | 1.000 | 0.522 | 0.849 | 0.938 | 22 m | 161 m |
| phacochr | 550 | 0.971 | 0.500 | 0.865 | 0.938 | 24 m | 104 m |

### Head to head (crabfinder, ≤ 100 m)

| vs | common | both | crabfinder only | other only | neither |
|---|---|---|---|---|---|
| crabfinder-osm | 1510 | 1403 | 0 | 0 | 107 |
| brussels | 496 | 455 | 9 | 4 | 28 |
| vlaanderen | 960 | 485 | 431 | 0 | 44 |
| wallonie | 550 | 463 | 24 | 3 | 60 |
| ngi | 1510 | 1257 | 146 | 5 | 102 |
| bepelias | 1510 | 1361 | 42 | 10 | 97 |
| phacochr | 1510 | 1353 | 50 | 17 | 90 |

### Queries crabfinder loses (29)

| id | query | crabfinder error | got it |
|---|---|---|---|
| `rw-brussels-bpost-0072` | Rue Wiertz 60, 1000 Bruxelles | 110 m | brussels (22 m), ngi (60 m) |
| `rw-brussels-bpost-0073` | Rue Du Cloà®tre 7, 1020 Bruxelles | 1162 m | brussels (19 m), ngi (19 m), phacochr (19 m) |
| `rw-flanders-schools-0026` | Rummenweg 117, 3800 Sint-Truiden | 109 m | bepelias (11 m) |
| `rw-flanders-schools-0156` | Herestraat 49, 3000 Leuven | 148 m | phacochr (0 m) |
| `rw-fwb-schools-brussels-0109` | Avenue de la Libre Académie 1, 1070 Anderlecht | 108 m | brussels (12 m) |
| `rw-fwb-schools-brussels-0141` | Place du temps libre 6, 1200 Woluwe-Saint-Lambert | 931 m | phacochr (6 m) |
| `rw-fwb-schools-brussels-0148` | Avenue de L Astronomie 46, 1030 Schaerbeek | 1928 m | phacochr (10 m) |
| `rw-fwb-schools-brussels-0207` | Avenue Franklin Roosevelt 50, 1050 Ixelles | 111 m | phacochr (89 m) |
| `rw-fwb-schools-brussels-0223` | Rue Belliard 135A, 1040 Etterbeek | 156 m | brussels (0 m), ngi (0 m), bepelias (1 m), phacochr (0 m) |
| `rw-fwb-schools-brussels-0240` | Rue de la Grenouillette 2, 1130 Bruxelles | 116 m | bepelias (51 m) |
| `rw-fwb-schools-wallonia-0028` | Avenue de l'Enseignement 20, 7330 Saint-Ghislain | 104 m | bepelias (46 m) |
| `rw-fwb-schools-wallonia-0078` | Rue Verlaine 9, 5030 Gembloux | 133 m | wallonie (33 m), bepelias (18 m) |
| `rw-fwb-schools-wallonia-0125` | Boulevard Président Kennedy 2, 7000 Mons | 243 m | phacochr (86 m) |
| `rw-fwb-schools-wallonia-0144` | Passage des Déportés 2, 5030 Gembloux | 136 m | phacochr (0 m) |
| `rw-fwb-schools-wallonia-0153` | Place des Combattants et Déportés 1, 6534 Thuin | 2788 m | phacochr (26 m) |
| `rw-fwb-schools-wallonia-0159` | Rue de l'Avenir 5, 7160 Chapelle-lez-Herlaimont | 255 m | phacochr (54 m) |
| `rw-fwb-schools-wallonia-0206` | Borsu 4, 4560 Clavier | 812 m | phacochr (100 m) |
| `rw-fwb-schools-wallonia-0218` | Rue de Noailles 3, 6460 Chimay | 118 m | wallonie (71 m) |
| `rw-fwb-schools-wallonia-0252` | Rue Grignard 24, 6533 Thuin | 102 m | bepelias (85 m) |
| `rw-fwb-schools-wallonia-0257` | Rue du Moustier 3, 7050 Jurbise | 198 m | phacochr (13 m) |
| `rw-fwb-schools-wallonia-0279` | Place du Palais de Justice 8, 5500 Dinant | 112 m | phacochr (75 m) |
| `rw-fwb-schools-wallonia-0311` | Route de Perwez 3, 5310 Eghezée | 1599 m | bepelias (68 m) |
| `rw-fwb-schools-wallonia-0314` | Rue de la Pte Chenevière 40, 6001 Charleroi | 162 m | phacochr (46 m) |
| `rw-fwb-schools-wallonia-0330` | Chemin Gabrielle Petit 2, 7940 Brugelette | 231 m | phacochr (35 m) |
| `rw-fwb-schools-wallonia-0337` | Rue de Monz-lez-Liège 262, 4400 Flémalle | 1122 m | phacochr (18 m) |
| `rw-fwb-schools-wallonia-0348` | Rue des Cent Bonniers 68, 6150 Anderlues | 119 m | wallonie (97 m), bepelias (22 m) |
| `rw-fwb-schools-wallonia-0376` | Rue de l'Aurore 223, 5100 Namur | 125 m | ngi (55 m), bepelias (15 m), phacochr (56 m) |
| `rw-fwb-schools-wallonia-0378` | Rue du Monument 1, 5590 Ciney | 9886 m | ngi (19 m) |
| `rw-fwb-schools-wallonia-0381` | Boulevard du Midi 161, 6140 Fontaine-l'Evêque | 100 m | bepelias (64 m) |

## Synthetic + adversarial sets (`--with-points`)

- Queries: **4368** (4350 with a reference point)
- A top-1 within 100 m of the reference point counts as a hit in the head-to-head and the losses list.
- Each geocoder is only asked the queries it covers (a regional service, its region): compare them within a region row.
- recall@1 needs BeSt ids; it is `–` for geocoders that return none.

### Overall

| Geocoder | asked | errors | recall@1 | n | found | ≤25 m | ≤100 m | ≤250 m | median | p90 | p50 | p95 |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| crabfinder | 4368 | 0 | 0.965 | 4350 | 0.998 | 0.953 | 0.957 | 0.962 | 0 m | 0 m | 1.3 ms | 7.9 ms |
| crabfinder-osm | 4368 | 0 | 0.971 | 4350 | 1.000 | 0.959 | 0.966 | 0.970 | 0 m | 0 m | 1.4 ms | 8.0 ms |
| brussels | 885 | 11 | 0.960 | 874 | 0.977 | 0.960 | 0.960 | 0.963 | 0 m | 0 m | 107.5 ms | 1202.2 ms |
| vlaanderen | 3079 | 0 | – | 3073 | 0.528 | 0.473 | 0.477 | 0.480 | 0 m | 56 m | 75.9 ms | 227.8 ms |
| wallonie | 1260 | 0 | – | 1251 | 0.763 | 0.500 | 0.514 | 0.520 | 0 m | 8732 m | 99.3 ms | 2120.5 ms |
| ngi | 4368 | 6 | 0.591 | 4344 | 0.918 | 0.624 | 0.632 | 0.640 | 0 m | 4549 m | 46.7 ms | 97.8 ms |
| bepelias | 4368 | 7 | 0.614 | 4343 | 0.993 | 0.699 | 0.721 | 0.734 | 1 m | 4139 m | 101.3 ms | 601.1 ms |
| phacochr | 4368 | 0 | – | 4350 | 0.670 | 0.603 | 0.633 | 0.640 | 0 m | 25 m | 9.5 ms | 9.5 ms |

### By slice (distance to the reference point)

#### adversarial

| Geocoder | n | found | ≤25 m | ≤100 m | ≤250 m | median | p90 |
|---|---|---|---|---|---|---|---|
| crabfinder | 106 | 0.934 | 0.255 | 0.396 | 0.613 | 128 m | 2013 m |
| crabfinder-osm | 106 | 1.000 | 0.509 | 0.755 | 0.943 | 25 m | 190 m |
| brussels | 20 | 0.300 | 0.300 | 0.300 | 0.300 | 0 m | 0 m |
| vlaanderen | 33 | 0.970 | 0.091 | 0.455 | 0.576 | 109 m | 20996 m |
| wallonie | 47 | 0.404 | 0.255 | 0.319 | 0.319 | 0 m | 4360 m |
| ngi | 106 | 0.868 | 0.123 | 0.208 | 0.264 | 2571 m | 75112 m |
| bepelias | 105 | 0.857 | 0.210 | 0.390 | 0.467 | 129 m | 46309 m |
| phacochr | 106 | 0.179 | 0.179 | 0.179 | 0.179 | 0 m | 4 m |

#### region:brussels

| Geocoder | n | found | ≤25 m | ≤100 m | ≤250 m | median | p90 |
|---|---|---|---|---|---|---|---|
| crabfinder | 863 | 1.000 | 0.990 | 0.991 | 0.991 | 0 m | 0 m |
| crabfinder-osm | 863 | 1.000 | 0.990 | 0.991 | 0.991 | 0 m | 0 m |
| brussels | 854 | 0.993 | 0.975 | 0.975 | 0.979 | 0 m | 0 m |
| vlaanderen | 863 | 0.572 | 0.530 | 0.530 | 0.533 | 0 m | 0 m |
| ngi | 863 | 0.994 | 0.721 | 0.729 | 0.737 | 0 m | 2089 m |
| bepelias | 863 | 0.994 | 0.717 | 0.738 | 0.760 | 1 m | 1962 m |
| phacochr | 863 | 0.758 | 0.711 | 0.733 | 0.743 | 0 m | 4 m |

#### region:flanders

| Geocoder | n | found | ≤25 m | ≤100 m | ≤250 m | median | p90 |
|---|---|---|---|---|---|---|---|
| crabfinder | 2177 | 1.000 | 0.962 | 0.963 | 0.963 | 0 m | 0 m |
| crabfinder-osm | 2177 | 1.000 | 0.962 | 0.963 | 0.963 | 0 m | 0 m |
| vlaanderen | 2177 | 0.503 | 0.456 | 0.457 | 0.457 | 0 m | 0 m |
| ngi | 2171 | 0.913 | 0.632 | 0.637 | 0.643 | 0 m | 4901 m |
| bepelias | 2173 | 0.997 | 0.692 | 0.702 | 0.708 | 1 m | 5500 m |
| phacochr | 2177 | 0.673 | 0.607 | 0.634 | 0.637 | 0 m | 25 m |

#### region:wallonia

| Geocoder | n | found | ≤25 m | ≤100 m | ≤250 m | median | p90 |
|---|---|---|---|---|---|---|---|
| crabfinder | 1204 | 1.000 | 0.971 | 0.971 | 0.971 | 0 m | 0 m |
| crabfinder-osm | 1204 | 1.000 | 0.971 | 0.971 | 0.971 | 0 m | 0 m |
| wallonie | 1204 | 0.777 | 0.510 | 0.522 | 0.528 | 0 m | 9172 m |
| ngi | 1204 | 0.875 | 0.586 | 0.590 | 0.599 | 0 m | 4947 m |
| bepelias | 1202 | 0.997 | 0.740 | 0.770 | 0.787 | 1 m | 3121 m |
| phacochr | 1204 | 0.644 | 0.556 | 0.600 | 0.612 | 0 m | 54 m |

#### synthetic

| Geocoder | n | found | ≤25 m | ≤100 m | ≤250 m | median | p90 |
|---|---|---|---|---|---|---|---|
| crabfinder | 4244 | 1.000 | 0.970 | 0.971 | 0.971 | 0 m | 0 m |
| crabfinder-osm | 4244 | 1.000 | 0.970 | 0.971 | 0.971 | 0 m | 0 m |
| brussels | 854 | 0.993 | 0.975 | 0.975 | 0.979 | 0 m | 0 m |
| vlaanderen | 3040 | 0.523 | 0.477 | 0.477 | 0.479 | 0 m | 0 m |
| wallonie | 1204 | 0.777 | 0.510 | 0.522 | 0.528 | 0 m | 9172 m |
| ngi | 4238 | 0.919 | 0.637 | 0.642 | 0.650 | 0 m | 4312 m |
| bepelias | 4238 | 0.996 | 0.711 | 0.729 | 0.741 | 1 m | 3848 m |
| phacochr | 4244 | 0.682 | 0.614 | 0.644 | 0.652 | 0 m | 25 m |

### Head to head (crabfinder, ≤ 100 m)

| vs | common | both | crabfinder only | other only | neither |
|---|---|---|---|---|---|
| crabfinder-osm | 4350 | 4161 | 1 | 39 | 149 |
| brussels | 884 | 832 | 33 | 7 | 12 |
| vlaanderen | 3073 | 1449 | 1518 | 17 | 89 |
| wallonie | 1251 | 642 | 551 | 1 | 57 |
| ngi | 4350 | 2726 | 1436 | 18 | 170 |
| bepelias | 4350 | 3086 | 1076 | 44 | 144 |
| phacochr | 4350 | 2750 | 1412 | 4 | 184 |

### Queries crabfinder loses (93)

| id | query | crabfinder error | got it |
|---|---|---|---|
| `adv-hamlet-002` | beausaint la roche | 2320 m | crabfinder-osm (21 m) |
| `adv-hamlet-003` | maredsous | 4904 m | crabfinder-osm (44 m) |
| `adv-hamlet-004` | han sur lesse 5580 | 227 m | crabfinder-osm (31 m) |
| `adv-hamlet-005` | spontin yvoir | 436 m | crabfinder-osm (34 m) |
| `adv-hamlet-008` | falaën | 307 m | crabfinder-osm (29 m), bepelias (76 m) |
| `adv-hamlet-010` | soulme | 158 m | crabfinder-osm (18 m) |
| `adv-hamlet-012` | chassepierre | 516 m | crabfinder-osm (12 m) |
| `adv-hamlet-013` | torgny | 135 m | crabfinder-osm (18 m) |
| `adv-hamlet-015` | wéris | 559 m | crabfinder-osm (21 m), bepelias (37 m) |
| `adv-hamlet-016` | olloy-sur-viroin | 255 m | crabfinder-osm (10 m) |
| `adv-hamlet-017` | vierves-sur-viroin | 107 m | crabfinder-osm (17 m), bepelias (93 m) |
| `adv-hamlet-018` | treignes | 185 m | crabfinder-osm (17 m), bepelias (71 m) |
| `adv-hamlet-019` | roly philippeville | 2055 m | crabfinder-osm (18 m), ngi (77 m), bepelias (64 m) |
| `adv-hamlet-020` | bellevaux bouillon | 156 m | crabfinder-osm (7 m) |
| `adv-hamlet-021` | frahan | 366 m | crabfinder-osm (11 m) |
| `adv-hamlet-022` | wibrin | 991 m | crabfinder-osm (51 m) |
| `adv-hamlet-023` | lompret | 627 m | crabfinder-osm (32 m), bepelias (22 m) |
| `adv-hamlet-024` | aubechies | 245 m | crabfinder-osm (43 m), bepelias (39 m) |
| `adv-hamlet-026` | ave namur | 44254 m | crabfinder-osm (12 m) |
| `adv-hamlet-027` | celles houyet | 1081 m | crabfinder-osm (25 m) |
| `adv-hamlet-028` | furfooz | 125 m | crabfinder-osm (14 m) |
| `adv-hamlet-029` | dolhain limbourg | 1722 m | crabfinder-osm (25 m) |
| `adv-landmark-001` | Manneken Pis | no answer | crabfinder-osm (3 m) |
| `adv-landmark-005` | Brussel-Centraal | 399 m | crabfinder-osm (16 m) |
| `adv-landmark-006` | Gare Centrale | 44122 m | crabfinder-osm (20 m) |
| `adv-landmark-007` | Antwerpen-Centraal | 1583 m | crabfinder-osm (33 m) |
| `adv-landmark-008` | Butte du Lion | 1639 m | crabfinder-osm (99 m) |
| `adv-landmark-009` | Leeuwenheuvel | no answer | crabfinder-osm (99 m) |
| `adv-landmark-010` | Palais Royal | 719 m | crabfinder-osm (12 m) |
| `adv-landmark-011` | Koninklijk Paleis | 5335 m | crabfinder-osm (12 m) |
| `adv-landmark-012` | Cathédrale Saints-Michel-et-Gudule | 88298 m | crabfinder-osm (49 m) |
| `adv-landmark-013` | Basilique du Sacré-Coeur | 2013 m | crabfinder-osm (69 m), bepelias (68 m) |
| `adv-landmark-015` | Zoo Antwerpen | 1737 m | crabfinder-osm (12 m) |
| `adv-landmark-018` | Citadelle de Liège | 852 m | crabfinder-osm (93 m) |
| `adv-landmark-019` | Stadhuis Gent | 310 m | crabfinder-osm (27 m) |
| `adv-landmark-020` | Manneken Pis Bruxelles | 275 m | crabfinder-osm (3 m) |
| `adv-landmark-021` | Gravensteen | 50466 m | crabfinder-osm (38 m) |
| `adv-landmark-022` | Gravensteen Gent | 299 m | crabfinder-osm (38 m) |
| `adv-landmark-026` | Mini-Europe | 53587 m | crabfinder-osm (82 m) |
| `adv-notability-001` | Grand-Place | 126 m | vlaanderen (65 m), bepelias (2 m) |
| `adv-notability-012` | Place de Brouckère | 104 m | vlaanderen (55 m), bepelias (53 m) |
| `adv-notability-014` | Place du Jeu de Balle | 125 m | vlaanderen (73 m), bepelias (90 m) |
| `adv-notability-016` | Groenplaats | 139 m | vlaanderen (97 m) |
| `adv-notability-022` | Place Poelaert | 141 m | vlaanderen (61 m), ngi (91 m), bepelias (73 m) |
| `adv-notability-030` | Grand Sablon | 128 m | vlaanderen (56 m), ngi (51 m) |
| `mvp-v1-000016` | Wielewaalstraat 30 | 58464 m | brussels (0 m) |
| `mvp-v1-000321` | Edelweisslaan 7 | 18768 m | brussels (0 m), bepelias (1 m) |
| `mvp-v1-000403` | Predikherenstraat 10 | 24321 m | brussels (0 m), bepelias (1 m) |
| `mvp-v1-000480` | Hovenierstraat 41 | 36105 m | brussels (0 m) |
| `mvp-v1-000536` | Koninginnelaan 13 | 114063 m | brussels (0 m), bepelias (1 m) |
| `mvp-v1-000729` | Diestsesteenweg 387 | 16831 m | bepelias (1 m) |
| `mvp-v1-000750` | Berkenlaan 13 | 40982 m | ngi (0 m) |
| `mvp-v1-000894` | Oranjestraat 28 | 46932 m | ngi (0 m) |
| `mvp-v1-001051` | Nijverheidstraat 32 | 36292 m | bepelias (1 m) |
| `mvp-v1-001131` | St. Jansstraat 20 | 132483 m | vlaanderen (0 m) |
| `mvp-v1-001152` | Neerveldstraat 4 | 14640 m | vlaanderen (0 m), bepelias (1 m) |
| `mvp-v1-001200` | Pontweg 5 | 32333 m | ngi (0 m) |
| `mvp-v1-001227` | Grensstraat 142 | 62753 m | vlaanderen (0 m) |
| `mvp-v1-001255` | Klaproosstraat 27 | 93677 m | vlaanderen (0 m) |
| `mvp-v1-001400` | Industriepark 7 | 6277 m | ngi (0 m), bepelias (1 m) |
| `mvp-v1-001425` | Boonhof 82 | 60422 m | ngi (0 m), bepelias (1 m) |
| `mvp-v1-001846` | Otegemstraat 95 | 5014 m | bepelias (2 m) |
| `mvp-v1-001867` | Oude Truierbaan 31 | 12093 m | ngi (0 m) |
| `mvp-v1-001963` | Pinguïnstraat 23 | 53440 m | vlaanderen (0 m), ngi (0 m), bepelias (1 m) |
| `mvp-v1-002015` | Kapellestraat 171 | 80186 m | ngi (0 m) |
| `mvp-v1-002093` | Pastoriestraat 47 | 101112 m | bepelias (1 m) |
| `mvp-v1-002137` | Dwarsstraat 2 | 51454 m | vlaanderen (0 m) |
| `mvp-v1-002152` | Veldstraat 47 | 19676 m | bepelias (1 m) |
| `mvp-v1-002166` | Winkelstraat 20 | 21684 m | vlaanderen (0 m) |
| `mvp-v1-002187` | Steenkaai 35 | 91328 m | bepelias (0 m) |
| `mvp-v1-002240` | Gnzeplas 13, 9880 Aalter | 548 m | phacochr (55 m) |
| `mvp-v1-002243` | Julia Tulkenslaan 1 | 76116 m | vlaanderen (0 m), ngi (0 m), bepelias (1 m) |
| `mvp-v1-002363` | Lochtstraat 9 | 65001 m | vlaanderen (0 m), ngi (0 m) |
| `mvp-v1-002384` | Rijkenhoek 60 | 21110 m | vlaanderen (0 m), ngi (0 m), bepelias (1 m) |
| `mvp-v1-002405` | Serskampstraat 40 | 4711 m | bepelias (1 m) |
| `mvp-v1-002468` | Moutstraat 18 | 44570 m | vlaanderen (0 m), ngi (0 m), bepelias (1 m) |
| `mvp-v1-002516` | Rue de Warisoulo 3, 5080 La Bruyère | 557 m | ngi (0 m), bepelias (1 m), phacochr (23 m) |
| `mvp-v1-002538` | Veldenstraat 14 | 92010 m | bepelias (2 m) |
| `mvp-v1-002557` | Rue de Seneffe 48 | 5273 m | ngi (0 m), bepelias (1 m) |
| `mvp-v1-002661` | Rue des Fleurs 39 | 165531 m | bepelias (1 m) |
| `mvp-v1-002720` | Strijdersstraat 17 | 78281 m | bepelias (1 m) |
| `mvp-v1-002825` | Rue d'Espagne 42 | 45116 m | bepelias (1 m) |
| `mvp-v1-002874` | Rue Saint-Vincent 78 | 41508 m | ngi (0 m), bepelias (1 m) |
| `mvp-v1-002895` | Rue du Moulin 95 | 91617 m | bepelias (1 m) |
| `mvp-v1-002966` | Stoppelstraat 25 | 45298 m | bepelias (1 m) |
| `mvp-v1-002978` | Rue du Village 51 | 30451 m | bepelias (1 m) |
| `mvp-v1-003210` | Rue de la Piscine 7 | 143621 m | bepelias (1 m) |
| `mvp-v1-003359` | Rue du Château d'Eau 7 | 99803 m | bepelias (1 m) |
| `mvp-v1-003407` | Chaussée de Louvain 102 | 45854 m | bepelias (1 m) |
| `mvp-v1-003771` | Romeinsesteenweg 445 1853 | 399 m | brussels (0 m) |
| `mvp-v1-003827` | Sterstraat 11 1620 | 897 m | brussels (0 m) |
| `mvp-v1-004163` | Rue d'Harchies 29 7321 | 2878 m | bepelias (1 m), phacochr (0 m) |
| `mvp-v1-004210` | Grand'Place 8 6000 | 1370 m | wallonie (0 m), bepelias (1 m), phacochr (1 m) |
