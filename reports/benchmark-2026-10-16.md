# Belgian geocoder benchmark — 2026-10-16

Phase 11.1. One run of `crabfinder-eval --benchmark` (see `docs/eval.md` §Benchmarking) over two query sets:

- **Real-world** (`eval/realworld-be-v1.yaml`): 1,510 addresses from 7 public open-data lists (schools, pharmacies, bpost offices, care facilities, shops; sources and licences in `eval/realworld/SOURCES.md`). The reference point is the coordinate the publisher gives, so it isn't exact; a hit means within 100 m.
- **Synthetic + adversarial** (`eval/belgium-2026-10-15-v3.json`, `eval/adversarial-belgium-v1.yaml`), with `--with-points`: each expected BeSt building gives the reference point. These sets come from BeSt and crabfinder's own failure history, so they **favour crabfinder**. The real-world set is the fairer comparison.

## Setup

| Geocoder | What | How it was asked |
|---|---|---|
| `crabfinder` | this repo at `1dbd3a0` (before the 11.1b ranking fixes), index schema v11 from BeSt 2026-10-15, BeSt-only | free text, `/geocode` |
| `crabfinder-osm` | same, index built with `--places`, service with `--pois` (the optional OSM aids) | free text |
| `brussels` | Brussels regional geocoder (Paradigm / UrbIS), `geoservices.irisnet.be` | free text with fr/nl fallback, Brussels queries only, 2 req/s |
| `vlaanderen` | Digitaal Vlaanderen Geolocation API v4 | free text, Flanders + Brussels queries, 2 req/s |
| `wallonie` | SPW `geoservices.wallonie.be/geocodeWS` (ICAR) | structured street/house/city when known, else free text; Wallonia only, 2 req/s |
| `phacochr` | PhacochR (R package over BeSt + UrbIS), local | one batch through `scripts/bench/phacochr.R` |

Competitor responses are cached in `eval/bench-cache/` (gitignored), so a rerun only queries crabfinder.

## Notes

- **NGI / IGN geocoder:** left out. It has no public endpoint we could query; the adapter fails with "no public endpoint".
- **BePelias:** pending. It is being set up on a separate machine and will be added in a later run.
- **Regional services** are asked only the queries in their region. Compare them within the `region:*` rows, not on the overall line.
- **vlaanderen in Brussels:** the Flemish API barely covers French-language Brussels addresses (found rate 0.095 on the real-world Brussels slice). Its Brussels numbers say little about Flanders, where it is close to crabfinder.
- **Latency:** HTTP services were measured from the benchmark machine over the internet. crabfinder ran on localhost. PhacochR's figure is the batch wall clock divided by the number of queries.
- **`recall@1`** needs BeSt ids. Only crabfinder and the Brussels geocoder (whose ids map back to BeSt) have it. Every geocoder gets the distance metrics.
- **Losses:** the "Queries crabfinder loses" lists are queries where another geocoder lands within 100 m and crabfinder doesn't. They feed the ranking backlog; Phase 11.1b fixes part of them.

## Real-world set (`eval/realworld-be-v1.yaml`)

- Queries: **1510** (1510 with a reference point)
- A top-1 within 100 m of the reference point counts as a hit in the head-to-head and the losses list.
- Each geocoder is only asked the queries it covers (a regional service, its region): compare them within a region row.
- recall@1 needs BeSt ids; it is `–` for geocoders that return none.

### Overall

| Geocoder | asked | errors | recall@1 | n | found | ≤25 m | ≤100 m | ≤250 m | median | p90 | p50 | p95 |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| crabfinder | 1510 | 0 | 0.923 | 1510 | 1.000 | 0.730 | 0.923 | 0.962 | 5 m | 78 m | 2.0 ms | 10.6 ms |
| crabfinder-osm | 1510 | 0 | 0.923 | 1510 | 1.000 | 0.730 | 0.923 | 0.962 | 5 m | 78 m | 2.8 ms | 25.3 ms |
| brussels | 496 | 0 | 0.925 | 496 | 0.980 | 0.780 | 0.925 | 0.942 | 7 m | 51 m | 111.2 ms | 504.3 ms |
| vlaanderen | 960 | 0 | – | 960 | 0.526 | 0.481 | 0.505 | 0.510 | 0 m | 13 m | 85.7 ms | 200.9 ms |
| wallonie | 550 | 0 | – | 550 | 0.989 | 0.500 | 0.847 | 0.911 | 24 m | 186 m | 126.6 ms | 308.0 ms |
| phacochr | 1510 | 0 | – | 1510 | 0.979 | 0.699 | 0.907 | 0.952 | 7 m | 77 m | 19.8 ms | 19.8 ms |

### By slice (distance to the reference point)

#### realworld

| Geocoder | n | found | ≤25 m | ≤100 m | ≤250 m | median | p90 |
|---|---|---|---|---|---|---|---|
| crabfinder | 1510 | 1.000 | 0.730 | 0.923 | 0.962 | 5 m | 78 m |
| crabfinder-osm | 1510 | 1.000 | 0.730 | 0.923 | 0.962 | 5 m | 78 m |
| brussels | 496 | 0.980 | 0.780 | 0.925 | 0.942 | 7 m | 51 m |
| vlaanderen | 960 | 0.526 | 0.481 | 0.505 | 0.510 | 0 m | 13 m |
| wallonie | 550 | 0.989 | 0.500 | 0.847 | 0.911 | 24 m | 186 m |
| phacochr | 1510 | 0.979 | 0.699 | 0.907 | 0.952 | 7 m | 77 m |

#### realworld:brussels-bpost

| Geocoder | n | found | ≤25 m | ≤100 m | ≤250 m | median | p90 |
|---|---|---|---|---|---|---|---|
| crabfinder | 100 | 1.000 | 0.720 | 0.890 | 0.960 | 12 m | 110 m |
| crabfinder-osm | 100 | 1.000 | 0.720 | 0.890 | 0.960 | 12 m | 110 m |
| brussels | 100 | 0.940 | 0.700 | 0.870 | 0.930 | 13 m | 72 m |
| vlaanderen | 100 | 0.080 | 0.050 | 0.070 | 0.080 | 9 m | 248 m |
| phacochr | 100 | 0.960 | 0.710 | 0.870 | 0.930 | 13 m | 82 m |

#### realworld:brussels-pharmacies

| Geocoder | n | found | ≤25 m | ≤100 m | ≤250 m | median | p90 |
|---|---|---|---|---|---|---|---|
| crabfinder | 60 | 1.000 | 1.000 | 1.000 | 1.000 | 0 m | 1 m |
| crabfinder-osm | 60 | 1.000 | 1.000 | 1.000 | 1.000 | 0 m | 1 m |
| brussels | 60 | 1.000 | 1.000 | 1.000 | 1.000 | 0 m | 1 m |
| vlaanderen | 60 | 0.000 | 0.000 | 0.000 | 0.000 | – | – |
| phacochr | 60 | 1.000 | 1.000 | 1.000 | 1.000 | 0 m | 3 m |

#### realworld:flanders-care

| Geocoder | n | found | ≤25 m | ≤100 m | ≤250 m | median | p90 |
|---|---|---|---|---|---|---|---|
| crabfinder | 250 | 1.000 | 0.860 | 0.928 | 0.940 | 0 m | 47 m |
| crabfinder-osm | 250 | 1.000 | 0.860 | 0.928 | 0.940 | 0 m | 47 m |
| brussels | 18 | 1.000 | 0.556 | 0.556 | 0.556 | 0 m | 2851 m |
| vlaanderen | 250 | 0.992 | 0.852 | 0.920 | 0.932 | 0 m | 49 m |
| phacochr | 250 | 1.000 | 0.828 | 0.904 | 0.940 | 0 m | 76 m |

#### realworld:flanders-schools

| Geocoder | n | found | ≤25 m | ≤100 m | ≤250 m | median | p90 |
|---|---|---|---|---|---|---|---|
| crabfinder | 250 | 1.000 | 0.980 | 0.992 | 1.000 | 0 m | 1 m |
| crabfinder-osm | 250 | 1.000 | 0.980 | 0.992 | 1.000 | 0 m | 1 m |
| brussels | 18 | 1.000 | 1.000 | 1.000 | 1.000 | 0 m | 1 m |
| vlaanderen | 250 | 0.976 | 0.960 | 0.972 | 0.976 | 0 m | 1 m |
| phacochr | 250 | 0.964 | 0.860 | 0.932 | 0.964 | 0 m | 34 m |

#### realworld:fwb-schools-brussels

| Geocoder | n | found | ≤25 m | ≤100 m | ≤250 m | median | p90 |
|---|---|---|---|---|---|---|---|
| crabfinder | 300 | 1.000 | 0.773 | 0.953 | 0.970 | 11 m | 53 m |
| crabfinder-osm | 300 | 1.000 | 0.773 | 0.953 | 0.970 | 11 m | 53 m |
| brussels | 300 | 0.987 | 0.763 | 0.947 | 0.953 | 11 m | 51 m |
| vlaanderen | 300 | 0.017 | 0.013 | 0.017 | 0.017 | 8 m | 44 m |
| phacochr | 300 | 0.993 | 0.757 | 0.960 | 0.973 | 11 m | 54 m |

#### realworld:fwb-schools-wallonia

| Geocoder | n | found | ≤25 m | ≤100 m | ≤250 m | median | p90 |
|---|---|---|---|---|---|---|---|
| crabfinder | 400 | 1.000 | 0.367 | 0.840 | 0.940 | 36 m | 152 m |
| crabfinder-osm | 400 | 1.000 | 0.367 | 0.840 | 0.940 | 36 m | 152 m |
| wallonie | 400 | 0.988 | 0.357 | 0.805 | 0.892 | 36 m | 234 m |
| phacochr | 400 | 0.960 | 0.350 | 0.818 | 0.917 | 36 m | 129 m |

#### realworld:namur-commerces

| Geocoder | n | found | ≤25 m | ≤100 m | ≤250 m | median | p90 |
|---|---|---|---|---|---|---|---|
| crabfinder | 150 | 1.000 | 0.880 | 0.953 | 0.960 | 4 m | 27 m |
| crabfinder-osm | 150 | 1.000 | 0.880 | 0.953 | 0.960 | 4 m | 27 m |
| wallonie | 150 | 0.993 | 0.880 | 0.960 | 0.960 | 4 m | 27 m |
| phacochr | 150 | 1.000 | 0.900 | 0.993 | 0.993 | 4 m | 24 m |

#### region:brussels

| Geocoder | n | found | ≤25 m | ≤100 m | ≤250 m | median | p90 |
|---|---|---|---|---|---|---|---|
| crabfinder | 496 | 1.000 | 0.790 | 0.933 | 0.958 | 7 m | 62 m |
| crabfinder-osm | 496 | 1.000 | 0.790 | 0.933 | 0.958 | 7 m | 62 m |
| brussels | 496 | 0.980 | 0.780 | 0.925 | 0.942 | 7 m | 51 m |
| vlaanderen | 496 | 0.095 | 0.071 | 0.077 | 0.079 | 1 m | 1310 m |
| phacochr | 496 | 0.986 | 0.776 | 0.931 | 0.952 | 9 m | 61 m |

#### region:flanders

| Geocoder | n | found | ≤25 m | ≤100 m | ≤250 m | median | p90 |
|---|---|---|---|---|---|---|---|
| crabfinder | 464 | 1.000 | 0.931 | 0.974 | 0.985 | 0 m | 11 m |
| crabfinder-osm | 464 | 1.000 | 0.931 | 0.974 | 0.985 | 0 m | 11 m |
| vlaanderen | 464 | 0.987 | 0.920 | 0.963 | 0.972 | 0 m | 10 m |
| phacochr | 464 | 0.983 | 0.851 | 0.931 | 0.968 | 0 m | 45 m |

#### region:wallonia

| Geocoder | n | found | ≤25 m | ≤100 m | ≤250 m | median | p90 |
|---|---|---|---|---|---|---|---|
| crabfinder | 550 | 1.000 | 0.507 | 0.871 | 0.945 | 24 m | 126 m |
| crabfinder-osm | 550 | 1.000 | 0.507 | 0.871 | 0.945 | 24 m | 126 m |
| wallonie | 550 | 0.989 | 0.500 | 0.847 | 0.911 | 24 m | 186 m |
| phacochr | 550 | 0.971 | 0.500 | 0.865 | 0.938 | 24 m | 104 m |

### Head to head (crabfinder, ≤ 100 m)

| vs | common | both | crabfinder only | other only | neither |
|---|---|---|---|---|---|
| crabfinder-osm | 1510 | 1394 | 0 | 0 | 116 |
| brussels | 496 | 454 | 9 | 5 | 28 |
| vlaanderen | 960 | 485 | 430 | 0 | 45 |
| wallonie | 550 | 458 | 21 | 8 | 63 |
| phacochr | 1510 | 1344 | 50 | 26 | 90 |

### Queries crabfinder loses (31)

| id | query | crabfinder error | got it |
|---|---|---|---|
| `rw-brussels-bpost-0072` | Rue Wiertz 60, 1000 Bruxelles | 110 m | brussels (22 m) |
| `rw-brussels-bpost-0073` | Rue Du Cloà®tre 7, 1020 Bruxelles | 1162 m | brussels (19 m), phacochr (19 m) |
| `rw-brussels-bpost-0079` | Schildknaapstraat 19, 1000 Bruxelles | 499 m | brussels (6 m), phacochr (6 m) |
| `rw-flanders-schools-0156` | Herestraat 49, 3000 Leuven | 148 m | phacochr (0 m) |
| `rw-fwb-schools-brussels-0109` | Avenue de la Libre Académie 1, 1070 Anderlecht | 108 m | brussels (12 m) |
| `rw-fwb-schools-brussels-0141` | Place du temps libre 6, 1200 Woluwe-Saint-Lambert | 931 m | phacochr (6 m) |
| `rw-fwb-schools-brussels-0148` | Avenue de L Astronomie 46, 1030 Schaerbeek | 1928 m | phacochr (10 m) |
| `rw-fwb-schools-brussels-0207` | Avenue Franklin Roosevelt 50, 1050 Ixelles | 111 m | phacochr (89 m) |
| `rw-fwb-schools-brussels-0223` | Rue Belliard 135A, 1040 Etterbeek | 156 m | brussels (0 m), phacochr (0 m) |
| `rw-fwb-schools-wallonia-0078` | Rue Verlaine 9, 5030 Gembloux | 133 m | wallonie (33 m) |
| `rw-fwb-schools-wallonia-0125` | Boulevard Président Kennedy 2, 7000 Mons | 243 m | phacochr (86 m) |
| `rw-fwb-schools-wallonia-0129` | Rue Albert Ier 17, 7611 Rumes | 437 m | wallonie (12 m), phacochr (12 m) |
| `rw-fwb-schools-wallonia-0144` | Passage des Déportés 2, 5030 Gembloux | 136 m | phacochr (0 m) |
| `rw-fwb-schools-wallonia-0153` | Place des Combattants et Déportés 1, 6534 Thuin | 2788 m | phacochr (26 m) |
| `rw-fwb-schools-wallonia-0159` | Rue de l'Avenir 5, 7160 Chapelle-lez-Herlaimont | 255 m | phacochr (54 m) |
| `rw-fwb-schools-wallonia-0182` | Rue Faubourg de Charleroi 14B, 6041 Charleroi | 1228 m | phacochr (25 m) |
| `rw-fwb-schools-wallonia-0206` | Borsu 4, 4560 Clavier | 812 m | phacochr (100 m) |
| `rw-fwb-schools-wallonia-0218` | Rue de Noailles 3, 6460 Chimay | 118 m | wallonie (71 m) |
| `rw-fwb-schools-wallonia-0257` | Rue du Moustier 3, 7050 Jurbise | 198 m | phacochr (13 m) |
| `rw-fwb-schools-wallonia-0279` | Place du Palais de Justice 8, 5500 Dinant | 112 m | phacochr (75 m) |
| `rw-fwb-schools-wallonia-0314` | Rue de la Pte Chenevière 40, 6001 Charleroi | 162 m | phacochr (46 m) |
| `rw-fwb-schools-wallonia-0330` | Chemin Gabrielle Petit 2, 7940 Brugelette | 231 m | phacochr (35 m) |
| `rw-fwb-schools-wallonia-0337` | Rue de Monz-lez-Liège 262, 4400 Flémalle | 1122 m | phacochr (18 m) |
| `rw-fwb-schools-wallonia-0348` | Rue des Cent Bonniers 68, 6150 Anderlues | 119 m | wallonie (97 m) |
| `rw-fwb-schools-wallonia-0376` | Rue de l'Aurore 223, 5100 Namur | 125 m | phacochr (56 m) |
| `rw-namur-commerces-0012` | Chaussée de Marche 797, 5100 Jambes | 4316 m | wallonie (0 m), phacochr (0 m) |
| `rw-namur-commerces-0018` | Chaussée de Marche 823, 5100 Jambes | 4698 m | wallonie (2 m), phacochr (2 m) |
| `rw-namur-commerces-0053` | Rue Rogier 44, 5000 Beez | 4004 m | phacochr (1 m) |
| `rw-namur-commerces-0059` | Rue du Réemploi 14, 5020 Suarlée | 196 m | wallonie (22 m), phacochr (23 m) |
| `rw-namur-commerces-0089` | Rue de Bruxelles 20, 5000 Beez | 4090 m | phacochr (10 m) |
| `rw-namur-commerces-0138` | Chaussée de Marche 654, 5100 Jambes | 1911 m | wallonie (7 m), phacochr (7 m) |

## Synthetic + adversarial sets (`--with-points`)

- Queries: **4368** (4350 with a reference point)
- A top-1 within 100 m of the reference point counts as a hit in the head-to-head and the losses list.
- Each geocoder is only asked the queries it covers (a regional service, its region): compare them within a region row.
- recall@1 needs BeSt ids; it is `–` for geocoders that return none.

### Overall

| Geocoder | asked | errors | recall@1 | n | found | ≤25 m | ≤100 m | ≤250 m | median | p90 | p50 | p95 |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| crabfinder | 4368 | 0 | 0.964 | 4350 | 0.998 | 0.952 | 0.956 | 0.961 | 0 m | 0 m | 2.4 ms | 14.1 ms |
| crabfinder-osm | 4368 | 0 | 0.970 | 4350 | 1.000 | 0.958 | 0.965 | 0.969 | 0 m | 0 m | 2.6 ms | 14.3 ms |
| brussels | 885 | 11 | 0.960 | 874 | 0.977 | 0.960 | 0.960 | 0.963 | 0 m | 0 m | 107.5 ms | 1202.2 ms |
| vlaanderen | 3079 | 0 | – | 3073 | 0.528 | 0.473 | 0.477 | 0.480 | 0 m | 56 m | 75.9 ms | 227.8 ms |
| wallonie | 1260 | 0 | – | 1251 | 0.763 | 0.500 | 0.514 | 0.520 | 0 m | 8732 m | 99.3 ms | 2120.5 ms |
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
| phacochr | 106 | 0.179 | 0.179 | 0.179 | 0.179 | 0 m | 4 m |

#### region:brussels

| Geocoder | n | found | ≤25 m | ≤100 m | ≤250 m | median | p90 |
|---|---|---|---|---|---|---|---|
| crabfinder | 863 | 1.000 | 0.988 | 0.990 | 0.990 | 0 m | 0 m |
| crabfinder-osm | 863 | 1.000 | 0.988 | 0.990 | 0.990 | 0 m | 0 m |
| brussels | 854 | 0.993 | 0.975 | 0.975 | 0.979 | 0 m | 0 m |
| vlaanderen | 863 | 0.572 | 0.530 | 0.530 | 0.533 | 0 m | 0 m |
| phacochr | 863 | 0.758 | 0.711 | 0.733 | 0.743 | 0 m | 4 m |

#### region:flanders

| Geocoder | n | found | ≤25 m | ≤100 m | ≤250 m | median | p90 |
|---|---|---|---|---|---|---|---|
| crabfinder | 2177 | 1.000 | 0.961 | 0.962 | 0.962 | 0 m | 0 m |
| crabfinder-osm | 2177 | 1.000 | 0.961 | 0.962 | 0.962 | 0 m | 0 m |
| vlaanderen | 2177 | 0.503 | 0.456 | 0.457 | 0.457 | 0 m | 0 m |
| phacochr | 2177 | 0.673 | 0.607 | 0.634 | 0.637 | 0 m | 25 m |

#### region:wallonia

| Geocoder | n | found | ≤25 m | ≤100 m | ≤250 m | median | p90 |
|---|---|---|---|---|---|---|---|
| crabfinder | 1204 | 1.000 | 0.970 | 0.970 | 0.970 | 0 m | 0 m |
| crabfinder-osm | 1204 | 1.000 | 0.970 | 0.970 | 0.970 | 0 m | 0 m |
| wallonie | 1204 | 0.777 | 0.510 | 0.522 | 0.528 | 0 m | 9172 m |
| phacochr | 1204 | 0.644 | 0.556 | 0.600 | 0.612 | 0 m | 54 m |

#### synthetic

| Geocoder | n | found | ≤25 m | ≤100 m | ≤250 m | median | p90 |
|---|---|---|---|---|---|---|---|
| crabfinder | 4244 | 1.000 | 0.969 | 0.970 | 0.970 | 0 m | 0 m |
| crabfinder-osm | 4244 | 1.000 | 0.969 | 0.970 | 0.970 | 0 m | 0 m |
| brussels | 854 | 0.993 | 0.975 | 0.975 | 0.979 | 0 m | 0 m |
| vlaanderen | 3040 | 0.523 | 0.477 | 0.477 | 0.479 | 0 m | 0 m |
| wallonie | 1204 | 0.777 | 0.510 | 0.522 | 0.528 | 0 m | 9172 m |
| phacochr | 4244 | 0.682 | 0.614 | 0.644 | 0.652 | 0 m | 25 m |

### Head to head (crabfinder, ≤ 100 m)

| vs | common | both | crabfinder only | other only | neither |
|---|---|---|---|---|---|
| crabfinder-osm | 4350 | 4157 | 1 | 39 | 153 |
| brussels | 884 | 832 | 32 | 7 | 13 |
| vlaanderen | 3073 | 1449 | 1515 | 17 | 92 |
| wallonie | 1251 | 641 | 551 | 2 | 57 |
| phacochr | 4350 | 2746 | 1412 | 8 | 184 |

### Queries crabfinder loses (71)

| id | query | crabfinder error | got it |
|---|---|---|---|
| `adv-hamlet-002` | beausaint la roche | 2320 m | crabfinder-osm (21 m) |
| `adv-hamlet-003` | maredsous | 4904 m | crabfinder-osm (44 m) |
| `adv-hamlet-004` | han sur lesse 5580 | 227 m | crabfinder-osm (31 m) |
| `adv-hamlet-005` | spontin yvoir | 436 m | crabfinder-osm (34 m) |
| `adv-hamlet-008` | falaën | 307 m | crabfinder-osm (29 m) |
| `adv-hamlet-010` | soulme | 158 m | crabfinder-osm (18 m) |
| `adv-hamlet-012` | chassepierre | 516 m | crabfinder-osm (12 m) |
| `adv-hamlet-013` | torgny | 135 m | crabfinder-osm (18 m) |
| `adv-hamlet-015` | wéris | 559 m | crabfinder-osm (21 m) |
| `adv-hamlet-016` | olloy-sur-viroin | 255 m | crabfinder-osm (10 m) |
| `adv-hamlet-017` | vierves-sur-viroin | 107 m | crabfinder-osm (17 m) |
| `adv-hamlet-018` | treignes | 185 m | crabfinder-osm (17 m) |
| `adv-hamlet-019` | roly philippeville | 2055 m | crabfinder-osm (18 m) |
| `adv-hamlet-020` | bellevaux bouillon | 156 m | crabfinder-osm (7 m) |
| `adv-hamlet-021` | frahan | 366 m | crabfinder-osm (11 m) |
| `adv-hamlet-022` | wibrin | 991 m | crabfinder-osm (51 m) |
| `adv-hamlet-023` | lompret | 627 m | crabfinder-osm (32 m) |
| `adv-hamlet-024` | aubechies | 245 m | crabfinder-osm (43 m) |
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
| `adv-landmark-010` | Palais Royal | 717 m | crabfinder-osm (12 m) |
| `adv-landmark-011` | Koninklijk Paleis | 5335 m | crabfinder-osm (12 m) |
| `adv-landmark-012` | Cathédrale Saints-Michel-et-Gudule | 88298 m | crabfinder-osm (49 m) |
| `adv-landmark-013` | Basilique du Sacré-Coeur | 2013 m | crabfinder-osm (69 m) |
| `adv-landmark-015` | Zoo Antwerpen | 1737 m | crabfinder-osm (12 m) |
| `adv-landmark-018` | Citadelle de Liège | 852 m | crabfinder-osm (93 m) |
| `adv-landmark-019` | Stadhuis Gent | 310 m | crabfinder-osm (27 m) |
| `adv-landmark-020` | Manneken Pis Bruxelles | 275 m | crabfinder-osm (3 m) |
| `adv-landmark-021` | Gravensteen | 50466 m | crabfinder-osm (38 m) |
| `adv-landmark-022` | Gravensteen Gent | 299 m | crabfinder-osm (38 m) |
| `adv-landmark-026` | Mini-Europe | 53587 m | crabfinder-osm (82 m) |
| `adv-notability-001` | Grand-Place | 126 m | vlaanderen (65 m) |
| `adv-notability-012` | Place de Brouckère | 104 m | vlaanderen (55 m) |
| `adv-notability-014` | Place du Jeu de Balle | 125 m | vlaanderen (73 m) |
| `adv-notability-016` | Groenplaats | 139 m | vlaanderen (97 m) |
| `adv-notability-022` | Place Poelaert | 141 m | vlaanderen (61 m) |
| `adv-notability-030` | Grand Sablon | 128 m | vlaanderen (56 m) |
| `mvp-v1-000016` | Wielewaalstraat 30 | 58464 m | brussels (0 m) |
| `mvp-v1-000321` | Edelweisslaan 7 | 18768 m | brussels (0 m) |
| `mvp-v1-000403` | Predikherenstraat 10 | 24321 m | brussels (0 m) |
| `mvp-v1-000480` | Hovenierstraat 41 | 36105 m | brussels (0 m) |
| `mvp-v1-000536` | Koninginnelaan 13 | 114063 m | brussels (0 m) |
| `mvp-v1-000591` | Juliaan 21, 1200 Sint-Lambrechts-Woluwe | 774 m | phacochr (0 m) |
| `mvp-v1-001131` | St. Jansstraat 20 | 132483 m | vlaanderen (0 m) |
| `mvp-v1-001152` | Neerveldstraat 4 | 14640 m | vlaanderen (0 m) |
| `mvp-v1-001227` | Grensstraat 142 | 62753 m | vlaanderen (0 m) |
| `mvp-v1-001255` | Klaproosstraat 27 | 93677 m | vlaanderen (0 m) |
| `mvp-v1-001843` | Stationstraat 49, 9400 Ninove | 1174 m | phacochr (1 m) |
| `mvp-v1-001963` | Pinguïnstraat 23 | 53440 m | vlaanderen (0 m) |
| `mvp-v1-002036` | Truwelstraat 100, 9100 Sint-Niklaas | 1800 m | phacochr (0 m) |
| `mvp-v1-002137` | Dwarsstraat 2 | 51454 m | vlaanderen (0 m) |
| `mvp-v1-002166` | Winkelstraat 20 | 21684 m | vlaanderen (0 m) |
| `mvp-v1-002240` | Gnzeplas 13, 9880 Aalter | 548 m | phacochr (55 m) |
| `mvp-v1-002243` | Julia Tulkenslaan 1 | 76116 m | vlaanderen (0 m) |
| `mvp-v1-002363` | Lochtstraat 9 | 65001 m | vlaanderen (0 m) |
| `mvp-v1-002384` | Rijkenhoek 60 | 21110 m | vlaanderen (0 m) |
| `mvp-v1-002468` | Moutstraat 18 | 44570 m | vlaanderen (0 m) |
| `mvp-v1-002516` | Rue de Warisoulo 3, 5080 La Bruyère | 557 m | phacochr (23 m) |
| `mvp-v1-002610` | Avenue des Muguet 73, 5190 Jemeppe-sur-Sambre | 1387 m | wallonie (0 m), phacochr (0 m) |
| `mvp-v1-003771` | Romeinsesteenweg 445 1853 | 399 m | brussels (0 m) |
| `mvp-v1-003827` | Sterstraat 11 1620 | 897 m | brussels (0 m) |
| `mvp-v1-004163` | Rue d'Harchies 29 7321 | 2878 m | phacochr (0 m) |
| `mvp-v1-004210` | Grand'Place 8 6000 | 1370 m | wallonie (0 m), phacochr (1 m) |
