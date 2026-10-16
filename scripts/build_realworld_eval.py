#!/usr/bin/env python3
"""Build eval/realworld-be-v1.yaml from public Belgian open-data lists
(v3 Phase 11.1).

Each source is an official list of places (schools, post offices,
pharmacies, shops, care facilities) with a postal address and published
coordinates. The address becomes the query, the published point the
ground truth (`expected_point`, scored within 100 m). Downloads are cached
in eval/realworld/sources/ (gitignored); sampling is seeded, so the same
downloads give the same set.

    scripts/build_realworld_eval.py                  # download (once) + build
    scripts/build_realworld_eval.py --seed 2 --per-source-scale 0.5

Licences and attribution: eval/realworld/SOURCES.md (written alongside).
Only the sampled addresses and points are kept in the eval set; no names,
phones or e-mails.
"""

import argparse
import csv
import io
import json
import random
import re
import sys
import time
import unicodedata
import urllib.parse
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CACHE = ROOT / "eval" / "realworld" / "sources"
OUT = ROOT / "eval" / "realworld-be-v1.yaml"
SOURCES_MD = ROOT / "eval" / "realworld" / "SOURCES.md"
UA = "crabfinder-eval/realworld (https://github.com/; benchmark set builder)"

ODWB = "https://www.odwb.be/api/explore/v2.1/catalog/datasets/{}/exports/csv"
BXL = "https://opendata.brussels.be/api/explore/v2.1/catalog/datasets/{}/exports/csv"
POI = "https://geo.api.vlaanderen.be/POI/ogc/features/v1/collections/POI/items"

# key, region(s), target count, title, publisher, licence, url
SOURCES = [
    {
        "key": "fwb-schools-brussels",
        "file": "fwb-schools.csv",
        "url": ODWB.format(
            "fwb-age-fichier-signaletique-des-etablissements-d-enseignement-de-la-federation-"
        ),
        "target": 300,
        "title": "Fichier signalétique des établissements d'enseignement (Brussels sites)",
        "publisher": "Fédération Wallonie-Bruxelles (via ODWB)",
        "licence": "CC BY 4.0",
    },
    {
        "key": "fwb-schools-wallonia",
        "file": "fwb-schools.csv",
        "url": None,  # same download as above
        "target": 400,
        "title": "Fichier signalétique des établissements d'enseignement (Walloon sites)",
        "publisher": "Fédération Wallonie-Bruxelles (via ODWB)",
        "licence": "CC BY 4.0",
    },
    {
        "key": "brussels-bpost",
        "file": "brussels-bpost.csv",
        "url": BXL.format("points-postaux0"),
        "target": 100,
        "title": "Points postaux (bpost) — Ville de Bruxelles",
        "publisher": "Ville de Bruxelles (opendata.brussels)",
        "licence": "CC BY 4.0",
    },
    {
        "key": "brussels-pharmacies",
        "file": "brussels-pharmacies.csv",
        "url": BXL.format("pharmacies0"),
        "target": 60,
        "title": "Pharmacies — Ville de Bruxelles",
        "publisher": "Ville de Bruxelles (opendata.brussels)",
        "licence": "CC0 1.0",
    },
    {
        "key": "namur-commerces",
        "file": "namur-commerces.csv",
        "url": ODWB.format("commerces-namurois"),
        "target": 150,
        "title": "Commerces namurois",
        "publisher": "Ville de Namur (via ODWB)",
        "licence": "CC BY 4.0",
    },
    {
        "key": "flanders-schools",
        "file": "flanders-onderwijs.json",
        "url": "THEMA='Onderwijs'",
        "target": 250,
        "title": "POI-dienst — thema Onderwijs",
        "publisher": "Vlaams Ministerie van Onderwijs en Vorming / Digitaal Vlaanderen",
        "licence": "Modellicentie Gratis Hergebruik",
    },
    {
        "key": "flanders-care",
        "file": "flanders-welzijn.json",
        "url": "THEMA='Welzijn, gezondheid en gezin'",
        "target": 250,
        "title": "POI-dienst — thema Welzijn, gezondheid en gezin",
        "publisher": "Agencies named per record (AUTEUR) / Digitaal Vlaanderen",
        "licence": "Modellicentie Gratis Hergebruik",
    },
]


def fetch(url, dest):
    if dest.exists() and dest.stat().st_size > 0:
        return
    dest.parent.mkdir(parents=True, exist_ok=True)
    print(f"downloading {url}", file=sys.stderr)
    req = urllib.request.Request(url, headers={"User-Agent": UA})
    with urllib.request.urlopen(req, timeout=300) as r:
        dest.write_bytes(r.read())


def fetch_poi(cql, dest):
    """Every POI feature matching a CQL filter, paged 5000 at a time."""
    if dest.exists() and dest.stat().st_size > 0:
        return
    feats, start = [], 0
    while True:
        q = urllib.parse.urlencode(
            {
                "f": "application/geo+json",
                "limit": 5000,
                "startIndex": start,
                "filter": cql,
                "filter-lang": "cql-text",
            }
        )
        print(f"downloading POI {cql} from {start}", file=sys.stderr)
        req = urllib.request.Request(f"{POI}?{q}", headers={"User-Agent": UA})
        with urllib.request.urlopen(req, timeout=300) as r:
            page = json.load(r)
        got = page.get("features", [])
        feats.extend(got)
        if len(got) < 5000:
            break
        start += 5000
        time.sleep(1)
    dest.parent.mkdir(parents=True, exist_ok=True)
    dest.write_text(json.dumps({"features": feats}))


def read_csv(path):
    text = path.read_text(encoding="utf-8-sig")
    delim = ";" if text.split("\n", 1)[0].count(";") > text.split("\n", 1)[0].count(",") else ","
    return list(csv.DictReader(io.StringIO(text), delimiter=delim))


# "Rue de la Loi 16", "Avenue du Port, 88", "Rue Blaes 182 B", "Kerkstraat 5 bus 2"
NUM = r"(\d+[A-Za-z]?(?:\s?[-/]\s?\d+[A-Za-z]?)?)"
ADDR_RE = re.compile(
    rf"^\s*(?P<street>.*?[^\d\s,])\s*,?\s+(?P<num>{NUM})(?:\s*(?:,|bte|bus|boîte|box|b\.)\s*\S*)?\s*$",
    re.IGNORECASE,
)


def split_address(s):
    """`(street, number)` of a combined street + number field, or None."""
    if not s:
        return None
    m = ADDR_RE.match(s.strip())
    if not m:
        return None
    street = re.sub(r"\s+", " ", m.group("street")).strip(" ,")
    num = re.sub(r"\s+", "", m.group("num")).upper()
    return (street, num) if street and num else None


def num_ok(n):
    return bool(n) and bool(re.match(r"^\d+[A-Z]?$", n.strip().upper()))


def tidy_city(c):
    """Title-case an all-caps commune ("SAINT-GILLES" → "Saint-Gilles")."""
    c = (c or "").strip()
    if c.isupper():
        c = "-".join(w.capitalize() for w in c.lower().split("-"))
        c = " ".join(w if w in ("de", "la", "le", "les", "sur", "en") else w[:1].upper() + w[1:] for w in c.split(" "))
    return c


def as_float(x):
    try:
        return float(str(x).replace(",", "."))
    except (TypeError, ValueError):
        return None


def point_pair(s):
    """"50.85, 4.35" → (50.85, 4.35)."""
    if not s:
        return None
    parts = [as_float(p) for p in str(s).split(",")]
    return tuple(parts) if len(parts) == 2 and None not in parts else None


def in_belgium(lat, lon):
    return lat is not None and lon is not None and 49.4 <= lat <= 51.6 and 2.5 <= lon <= 6.5


def entry(source, region, street, num, pc, city, lat, lon, note=None):
    if not (street and num_ok(num) and pc and re.match(r"^\d{4}$", pc) and in_belgium(lat, lon)):
        return None
    city = tidy_city(city)
    q = f"{street} {num}, {pc} {city}".strip()
    return {
        "source": source,
        "region": region,
        "query": q,
        "street": street,
        "house_number": num.upper(),
        "postal_code": pc,
        "city": city,
        "expected_point": [round(lat, 6), round(lon, 6)],
        "notes": note,
    }


def region_of_pc(pc):
    n = int(pc)
    if 1000 <= n <= 1299:
        return "brussels"
    if 1300 <= n <= 1499 or 4000 <= n <= 7999:
        return "wallonia"
    return "flanders"


def load_fwb():
    out = []
    for r in read_csv(CACHE / "fwb-schools.csv"):
        sp = split_address(r.get("adresse_de_l_implantation"))
        pc = (r.get("code_postal_de_l_implantation") or "").strip()
        if not sp or not pc.isdigit():
            continue
        region = region_of_pc(pc)
        if region == "flanders":
            continue
        key = "fwb-schools-brussels" if region == "brussels" else "fwb-schools-wallonia"
        e = entry(
            key,
            region,
            sp[0],
            sp[1],
            pc,
            r.get("commune_de_l_implantation") or r.get("localite_de_l_implantation"),
            as_float(r.get("latitude")),
            as_float(r.get("longitude")),
        )
        if e:
            out.append(e)
    return out


def load_bpost():
    out = []
    for r in read_csv(CACHE / "brussels-bpost.csv"):
        sp = split_address(r.get("rue_"))
        p = point_pair(r.get("geo_point_2d"))
        pc = (r.get("pccp") or "").strip()
        if sp and p:
            e = entry("brussels-bpost", "brussels", sp[0], sp[1], pc, r.get("commune"), p[0], p[1])
            if e:
                out.append(e)
    return out


def load_pharmacies():
    out = []
    for r in read_csv(CACHE / "brussels-pharmacies.csv"):
        e = entry(
            "brussels-pharmacies",
            "brussels",
            (r.get("rue") or "").strip(),
            (r.get("numero") or "").strip(),
            (r.get("code_postal") or "").strip(),
            r.get("commune"),
            as_float(r.get("latitude_wgs84")),
            as_float(r.get("longitude_wgs84")),
        )
        if e:
            out.append(e)
    return out


def load_namur():
    out = []
    for r in read_csv(CACHE / "namur-commerces.csv"):
        p = point_pair(r.get("geo_point_2d"))
        if not p:
            continue
        e = entry(
            "namur-commerces",
            "wallonia",
            (r.get("rue_nom") or "").strip(),
            (r.get("adr_nopol") or "").strip(),
            (r.get("cp") or "").strip(),
            r.get("localite"),
            p[0],
            p[1],
        )
        if e:
            out.append(e)
    return out


def load_poi(key, file):
    out = []
    for f in json.loads((CACHE / file).read_text())["features"]:
        pr = f.get("properties") or {}
        g = (f.get("geometry") or {}).get("coordinates") or [None, None]
        pc = (pr.get("POSTCODE") or "").strip()
        if not pc.isdigit():
            continue
        region = region_of_pc(pc)
        if region == "wallonia":
            continue
        e = entry(
            key,
            region,
            (pr.get("STRAAT") or "").strip(),
            (pr.get("HUISNUMMER") or "").strip(),
            pc,
            pr.get("GEMEENTE"),
            as_float(g[1]),
            as_float(g[0]),
            # "basisregister" points are the address register's own: not
            # independent of BeSt.
            note=f"point: {pr.get('PUNTKWAL') or '?'}",
        )
        if e:
            out.append(e)
    return out


def norm_key(e):
    s = unicodedata.normalize("NFKD", f"{e['street']} {e['house_number']} {e['postal_code']}")
    return re.sub(r"[^a-z0-9]", "", s.encode("ascii", "ignore").decode().lower())


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--per-source-scale", type=float, default=1.0)
    ap.add_argument("--output", type=Path, default=OUT)
    args = ap.parse_args()

    for s in SOURCES:
        if s["url"] is None:
            continue
        if s["file"].endswith(".json"):
            fetch_poi(s["url"], CACHE / s["file"])
        else:
            fetch(s["url"], CACHE / s["file"])

    pool = load_fwb() + load_bpost() + load_pharmacies() + load_namur()
    pool += load_poi("flanders-schools", "flanders-onderwijs.json")
    pool += load_poi("flanders-care", "flanders-welzijn.json")

    # One entry per address across all sources (many schools share a site).
    seen, uniq = set(), []
    for e in pool:
        k = norm_key(e)
        if k not in seen:
            seen.add(k)
            uniq.append(e)

    rng = random.Random(args.seed)
    picked = []
    for s in SOURCES:
        cands = [e for e in uniq if e["source"] == s["key"]]
        cands.sort(key=lambda e: (e["postal_code"], e["query"]))
        n = min(len(cands), round(s["target"] * args.per_source_scale))
        # Stratify by the first two postal digits so no one city dominates.
        by_area = {}
        for e in cands:
            by_area.setdefault(e["postal_code"][:2], []).append(e)
        for v in by_area.values():
            rng.shuffle(v)
        areas = sorted(by_area)
        chosen = []
        while len(chosen) < n:
            for a in areas:
                if by_area[a] and len(chosen) < n:
                    chosen.append(by_area[a].pop())
        s["available"] = len(cands)
        s["sampled"] = len(chosen)
        picked.extend(chosen)
        print(f"{s['key']}: {len(cands)} usable, {len(chosen)} sampled", file=sys.stderr)

    lines = [
        "version: realworld-be-v1",
        "kind: realworld",
        "",
        "# Addresses from public Belgian open-data lists with their published",
        "# coordinates (v3 Phase 11.1). Built by scripts/build_realworld_eval.py",
        f"# (seed {args.seed}); sources and licences in eval/realworld/SOURCES.md.",
        "# expected_point is the publisher's point: scored within 100 m by default.",
        "# Pin BeSt addresses afterwards with scripts/pin_realworld_ids.py.",
        "",
        "queries:",
    ]
    counters = {}
    for e in picked:
        counters[e["source"]] = counters.get(e["source"], 0) + 1
        e = {"id": f"rw-{e['source']}-{counters[e['source']]:04d}", **e}
        lines.append(f"  - id: {e['id']}")
        for k in ("source", "region", "query", "street", "house_number", "postal_code", "city"):
            lines.append(f"    {k}: {json.dumps(e[k], ensure_ascii=False)}")
        lines.append(f"    expected_point: [{e['expected_point'][0]}, {e['expected_point'][1]}]")
        if e.get("notes"):
            lines.append(f"    notes: {json.dumps(e['notes'], ensure_ascii=False)}")
    args.output.write_text("\n".join(lines) + "\n")

    md = [
        "# Real-world eval set — sources",
        "",
        f"`{args.output.relative_to(ROOT)}` samples addresses from these open-data lists",
        "(downloaded by `scripts/build_realworld_eval.py`, cached in `eval/realworld/sources/`,",
        "which is gitignored). Only the address and the published point of each sampled",
        "record are kept.",
        "",
        "| key | dataset | publisher | licence | usable | sampled |",
        "|---|---|---|---|---|---|",
    ]
    for s in SOURCES:
        md.append(
            f"| `{s['key']}` | {s['title']} | {s['publisher']} | {s['licence']} | "
            f"{s.get('available', 0)} | {s.get('sampled', 0)} |"
        )
    md += [
        "",
        "Download URLs:",
        "",
    ]
    for s in SOURCES:
        if s["url"] is None:
            continue
        url = s["url"] if s["url"].startswith("http") else f"{POI} (filter {s['url']})"
        md.append(f"- `{s['key']}`: <{url}>")
    md += [
        "",
        "## Attribution",
        "",
        "- CC BY 4.0 data: © the publisher named above, "
        "<https://creativecommons.org/licenses/by/4.0/>.",
        "- Flemish POI data: \"Bron: Vlaams Ministerie van Onderwijs en Vorming\" (schools) and",
        "  the agency named in each record's `AUTEUR` (care), under the Flemish",
        "  [Modellicentie Gratis Hergebruik](https://www.vlaanderen.be/digitaal-vlaanderen/onze-oplossingen/open-data/voorwaarden-voor-het-hergebruik-van-overheidsinformatie).",
        "- CC0 data needs no attribution.",
        "",
        "## Caveats",
        "",
        "- A published point is a building, an entrance or a parcel centroid, not the",
        "  BeSt address point: 10–50 m disagreements are normal, hence the 100 m radius.",
        "- Flemish POI entries say where their point comes from (`notes: \"point: …\"`):",
        "  `basisregister` points are the address register's own, so they are not",
        "  independent of BeSt; `self-defined` ones are the publisher's.",
        "- Combined street + number fields are split with a regular expression;",
        "  unparseable rows, rows without a plain house number and duplicates",
        "  (one per address across all sources) are dropped.",
    ]
    SOURCES_MD.parent.mkdir(parents=True, exist_ok=True)
    SOURCES_MD.write_text("\n".join(md) + "\n")
    print(f"wrote {len(picked)} queries to {args.output}", file=sys.stderr)


if __name__ == "__main__":
    main()
