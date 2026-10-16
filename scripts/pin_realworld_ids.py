#!/usr/bin/env python3
"""Pin BeSt addresses on Brussels entries of eval/realworld-be-v1.yaml
(v3 Phase 11.1), so they are scored at address level.

The id comes from the Brussels regional geocoder (Paradigm, which publishes
BeSt Brussels): independent of crabfinder, so pinning doesn't grade
crabfinder against its own answers. An entry is pinned when that
geocoder's top-1 carries the entry's exact house number and lies within
100 m of the published point. Reads a benchmark run's response cache and
detail file, asks nothing:

    crabfinder-eval --benchmark --eval-set eval/realworld-be-v1.yaml \\
        --geocoder brussels --output /tmp/b.json
    scripts/pin_realworld_ids.py --detail /tmp/b-detail.json.gz --max 200
"""

import argparse
import glob
import gzip
import json
import random
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def top_address(body):
    """Highest-score address of a Brussels geocoder answer."""
    hits = []
    for rl in body.get("resultsList") or []:
        for r in rl.get("result") or []:
            if "address" in r:
                hits.append((r.get("score") or 0, r["address"]))
    hits.sort(key=lambda h: -h[0])
    return hits[0][1] if hits else None


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--set", type=Path, default=ROOT / "eval" / "realworld-be-v1.yaml")
    ap.add_argument("--cache", type=Path, default=ROOT / "eval" / "bench-cache" / "brussels")
    ap.add_argument("--detail", type=Path, required=True, help="benchmark -detail.json.gz")
    ap.add_argument("--geocoder", default="brussels", help="label of the Brussels geocoder in the run")
    ap.add_argument("--max", type=int, default=200)
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--within-m", type=float, default=100.0)
    args = ap.parse_args()

    # (street, number, postal code) → top address, from structured requests.
    answers = {}
    for f in glob.glob(str(args.cache / "*.json")):
        e = json.load(open(f))
        url, _, query = e["key"].partition("&")
        if not url.endswith("/searchAddress") or e.get("status") != 200:
            continue
        # The key is "url&k=v&k=v" with raw (unescaped) values: split on
        # the known parameter names.
        p = dict(re.findall(r"(?:^|&)(searchLanguage|streetName|policeNumber|model|postalCode|municipality)=([^&]*)", query))
        a = top_address(json.loads(e["body"]))
        if a is None:
            continue
        k = (p.get("streetName"), p.get("policeNumber"), p.get("postalCode"))
        answers[k] = a

    detail = json.load(gzip.open(args.detail))
    near = {
        r["id"]
        for r in detail
        if r["geocoder"] == args.geocoder and r["error_m"] is not None and r["error_m"] <= args.within_m
    }

    lines = args.set.read_text().split("\n")
    # Parse entries: id line index, fields.
    entries, cur = [], None
    for i, l in enumerate(lines):
        m = re.match(r"  - id: (\S+)", l)
        if m:
            cur = {"id": m.group(1), "line": i}
            entries.append(cur)
        elif cur is not None:
            m = re.match(r"    (\w+): (.*)", l)
            if m:
                cur[m.group(1)] = m.group(2)
                cur["last"] = i
    candidates = []
    for e in entries:
        if "expected_best_id" in e or json.loads(e.get("region", '""')) != "brussels" or e["id"] not in near:
            continue
        k = tuple(json.loads(e.get(f, "null")) for f in ("street", "house_number", "postal_code"))
        a = answers.get(k)
        if not a or (a.get("houseNumber") or "").upper() != (k[1] or "").upper():
            continue
        c, s = a.get("code") or {}, a.get("hasStreetName") or {}
        if "namespace" not in c or "objectIdentifier" not in c:
            continue
        if "namespace" not in s or "objectIdentifier" not in s:
            s = None
        best = f"{c['namespace'].rstrip('/')}/{c['objectIdentifier']}/{c.get('versionIdentifier', '')}"
        bgid = f"{s['namespace'].rstrip('/')}|{s['objectIdentifier']}|{a['houseNumber'].upper()}" if s else None
        candidates.append((e, best, bgid))

    random.Random(args.seed).shuffle(candidates)
    picked = sorted(candidates[: args.max], key=lambda c: -c[0]["last"])
    for e, best, bgid in picked:
        add = [f"    expected_best_id: {json.dumps(best)}"]
        if bgid:
            add.append(f"    expected_building_group_id: {json.dumps(bgid)}")
        lines[e["last"] + 1 : e["last"] + 1] = add
    args.set.write_text("\n".join(lines))
    print(f"{len(candidates)} pinnable, {len(picked)} pinned", file=sys.stderr)


if __name__ == "__main__":
    main()
