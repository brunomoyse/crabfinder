#!/usr/bin/env python3
"""Extract Belgian place names from OpenStreetMap into a CSV for the indexer.

Queries the Overpass API for `place=hamlet|village|locality|isolated_dwelling`
nodes inside Belgium and writes `data/osm-places-be-v1.csv`:

    osm_id,place,name,name_fr,name_nl,name_de,lat,lon

The indexer (`--places`, schema v8) attaches the nearest place to each
address so hamlet names ("Falaën", "Wéris") become searchable.

Data © OpenStreetMap contributors, ODbL 1.0
(https://www.openstreetmap.org/copyright). See data/README.md.

Usage:
    python3 scripts/build_osm_places.py [--output data/osm-places-be-v1.csv]
        [--endpoint https://overpass-api.de/api/interpreter]
"""

import argparse
import csv
import json
import sys
import urllib.parse
import urllib.request

PLACE_TYPES = ("hamlet", "village", "locality", "isolated_dwelling")

QUERY = """
[out:json][timeout:300];
area["ISO3166-1"="BE"][admin_level=2]->.be;
node["place"~"^({types})$"]["name"](area.be);
out body;
""".strip()


def fetch(endpoint: str) -> list[dict]:
    body = urllib.parse.urlencode(
        {"data": QUERY.format(types="|".join(PLACE_TYPES))}
    ).encode()
    req = urllib.request.Request(
        endpoint,
        data=body,
        headers={"User-Agent": "crabfinder-build-osm-places/1"},
    )
    with urllib.request.urlopen(req, timeout=360) as resp:
        return json.load(resp)["elements"]


def rows(elements: list[dict]):
    for el in sorted(elements, key=lambda e: e["id"]):
        tags = el.get("tags", {})
        yield [
            el["id"],
            tags.get("place", ""),
            tags.get("name", ""),
            tags.get("name:fr", ""),
            tags.get("name:nl", ""),
            tags.get("name:de", ""),
            f'{el["lat"]:.7f}',
            f'{el["lon"]:.7f}',
        ]


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--output", default="data/osm-places-be-v1.csv")
    ap.add_argument(
        "--endpoint", default="https://overpass-api.de/api/interpreter"
    )
    args = ap.parse_args()

    elements = fetch(args.endpoint)
    n = 0
    with open(args.output, "w", newline="", encoding="utf-8") as f:
        w = csv.writer(f)
        w.writerow(
            ["osm_id", "place", "name", "name_fr", "name_nl", "name_de", "lat", "lon"]
        )
        for r in rows(elements):
            w.writerow(r)
            n += 1
    by_type: dict[str, int] = {}
    for el in elements:
        t = el.get("tags", {}).get("place", "?")
        by_type[t] = by_type.get(t, 0) + 1
    print(f"wrote {n} places to {args.output}: {by_type}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
