#!/usr/bin/env python3
"""Extract notable Belgian landmarks from OpenStreetMap into a CSV.

Queries the Overpass API for named nodes, ways and relations inside Belgium
that carry a `wikidata` tag (the notability filter) and one of the landmark
tags below, and writes `data/osm-pois-be-v1.csv`:

    osm_type,osm_id,kind,name,name_fr,name_nl,name_de,name_en,lat,lon

Ways and relations are placed at their Overpass `center`. The service
(`--pois`, v2 Phase 10) answers a query naming a landmark ("Atomium",
"Manneken Pis", "Gare du Midi") with the buildings around it, when no
street matches.

Data © OpenStreetMap contributors, ODbL 1.0
(https://www.openstreetmap.org/copyright). See data/README.md.

Usage:
    python3 scripts/build_osm_pois.py [--output data/osm-pois-be-v1.csv]
        [--endpoint URL ...]
"""

import argparse
import csv
import json
import sys
import time
import urllib.error
import urllib.parse
import urllib.request

# (tag, value regex) pairs; the first that matches names the `kind`.
LANDMARK_TAGS = [
    ("tourism", "attraction|museum|zoo|theme_park|aquarium|gallery"),
    ("historic", "castle|monument|memorial|fort|citadel|archaeological_site|city_gate|tower|ruins"),
    ("amenity", "townhall|university|place_of_worship|theatre|concert_hall|hospital"),
    ("railway", "station"),
    ("aeroway", "aerodrome"),
    ("leisure", "stadium|park"),
    ("building", "cathedral|basilica|palace"),
    ("man_made", "tower|lighthouse"),
]

QUERY = """
[out:json][timeout:600];
area["ISO3166-1"="BE"][admin_level=2]->.be;
(
{clauses}
);
out center tags;
""".strip()


def query() -> str:
    clauses = "\n".join(
        f'  nwr["{k}"~"^({v})$"]["name"]["wikidata"](area.be);'
        for k, v in LANDMARK_TAGS
    )
    return QUERY.format(clauses=clauses)


ENDPOINTS = [
    "https://overpass-api.de/api/interpreter",
    "https://overpass.kumi.systems/api/interpreter",
    "https://maps.mail.ru/osm/tools/overpass/api/interpreter",
]


def fetch(endpoints: list[str]) -> list[dict]:
    body = urllib.parse.urlencode({"data": query()}).encode()
    for attempt in range(8):
        # Busy public instances: back off and rotate through the mirrors.
        endpoint = endpoints[attempt % len(endpoints)]
        req = urllib.request.Request(
            endpoint,
            data=body,
            headers={"User-Agent": "crabfinder-build-osm-pois/1"},
        )
        try:
            with urllib.request.urlopen(req, timeout=660) as resp:
                data = json.load(resp)
        except (urllib.error.URLError, TimeoutError) as e:
            code = getattr(e, "code", None)
            if (code is not None and code not in (429, 500, 502, 503, 504)) or attempt == 7:
                raise
            wait = 30 * (attempt + 1)
            print(f"{code or e} from {endpoint}, retrying in {wait}s", file=sys.stderr)
            time.sleep(wait)
            continue
        # A runtime error (timeout, memory) comes back as a 200 with a
        # `remark` and partial elements: never write a partial file.
        if "remark" in data:
            raise SystemExit(f"overpass error: {data['remark']}")
        return data["elements"]
    raise AssertionError("unreachable")


def kind_of(tags: dict) -> str:
    import re

    for k, v in LANDMARK_TAGS:
        if k in tags and re.fullmatch(v, tags[k]):
            return f"{k}={tags[k]}"
    return ""


def rows(elements: list[dict]):
    seen = set()
    for el in sorted(elements, key=lambda e: (e["type"], e["id"])):
        key = (el["type"], el["id"])
        if key in seen:
            continue
        seen.add(key)
        tags = el.get("tags", {})
        if "lat" in el:
            lat, lon = el["lat"], el["lon"]
        elif "center" in el:
            lat, lon = el["center"]["lat"], el["center"]["lon"]
        else:
            continue
        kind = kind_of(tags)
        if not kind:
            continue
        yield [
            el["type"],
            el["id"],
            kind,
            tags.get("name", ""),
            tags.get("name:fr", ""),
            tags.get("name:nl", ""),
            tags.get("name:de", ""),
            tags.get("name:en", ""),
            f"{lat:.7f}",
            f"{lon:.7f}",
        ]


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--output", default="data/osm-pois-be-v1.csv")
    ap.add_argument(
        "--endpoint",
        action="append",
        help="Overpass endpoint (repeatable); default: the public mirrors",
    )
    args = ap.parse_args()

    elements = fetch(args.endpoint or ENDPOINTS)
    n = 0
    with open(args.output, "w", newline="", encoding="utf-8") as f:
        w = csv.writer(f)
        w.writerow(
            [
                "osm_type", "osm_id", "kind", "name", "name_fr", "name_nl",
                "name_de", "name_en", "lat", "lon",
            ]
        )
        for r in rows(elements):
            w.writerow(r)
            n += 1
    print(f"wrote {n} landmarks to {args.output}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
