#!/usr/bin/env python3
"""
Phase 5 Step 5 smoke battery.

Runs the per-round queries against a running crabfinder-service, captures
latency + top-feature summary, and prints per-round tables. Versioned so
re-running it on a future BeSt dump (or a refactored service) produces a
comparable report.

Usage:
    ./scripts/smoke_step5.py --base http://127.0.0.1:8080

Outputs to stdout; non-zero exit on transport errors.
"""

from __future__ import annotations

import argparse
import json
import sys
import time
import urllib.parse
import urllib.request


def call(base: str, path: str, params: dict | None = None) -> tuple[dict, float]:
    """GET base+path?params. Returns (json, latency_ms)."""
    url = base.rstrip("/") + path
    if params:
        url += "?" + urllib.parse.urlencode(params, safe="+")
    t0 = time.perf_counter()
    with urllib.request.urlopen(url, timeout=10) as resp:
        body = resp.read().decode("utf-8")
    dt_ms = (time.perf_counter() - t0) * 1000.0
    return json.loads(body), dt_ms


def feature_brief(feat: dict) -> dict:
    p = feat.get("properties", {}) or {}
    sn = p.get("street_name") or {}
    mn = p.get("municipality_name") or {}
    return {
        "fr": sn.get("fr"),
        "nl": sn.get("nl"),
        "de": sn.get("de"),
        "muni_fr": mn.get("fr"),
        "muni_nl": mn.get("nl"),
        "house": p.get("house_number"),
        "postal": p.get("postal_code"),
        "region": p.get("region"),
        "street_type": p.get("street_type"),
        "match_type": feat.get("match_type"),
        "score": feat.get("score"),
        "units": feat.get("unit_count"),
        "distance_m": feat.get("distance_m"),
    }


def top_label(feat: dict) -> str:
    b = feature_brief(feat)
    name = b["fr"] or b["nl"] or b["de"] or "?"
    muni = b["muni_fr"] or b["muni_nl"] or "?"
    extras = []
    if b["house"]:
        extras.append(f"#{b['house']}")
    if b["postal"]:
        extras.append(b["postal"])
    extras.append(f"({b['region']})")
    if b["units"] and b["units"] > 1:
        extras.append(f"×{b['units']}")
    if b["distance_m"] is not None:
        extras.append(f"@{b['distance_m']:.1f}m")
    return f"{name}, {muni} " + " ".join(extras)


def fwd(base: str, q: str, **extra) -> dict:
    params = {"q": q, **{k: v for k, v in extra.items() if v is not None}}
    body, ms = call(base, "/geocode", params)
    return {
        "q": q,
        "extras": {k: v for k, v in extra.items() if v is not None},
        "count": body["count"],
        "raw_hits": body["raw_hits"],
        "match_type": body["match_type"],
        "parsed": body["query"].get("parsed", {}),
        "top": top_label(body["features"][0]) if body["features"] else None,
        "regions_seen": sorted(
            {(f.get("properties", {}) or {}).get("region", "?") for f in body["features"]}
        ),
        "latency_ms": ms,
        "all_features": [feature_brief(f) for f in body["features"][:5]],
    }


def rev(base: str, lat: float, lng: float, **extra) -> dict:
    params = {"lat": lat, "lng": lng, **{k: v for k, v in extra.items() if v is not None}}
    body, ms = call(base, "/reverse", params)
    return {
        "lat": lat,
        "lng": lng,
        "extras": {k: v for k, v in extra.items() if v is not None},
        "count": body["count"],
        "raw_hits": body["raw_hits"],
        "top": top_label(body["features"][0]) if body["features"] else None,
        "regions_seen": sorted(
            {(f.get("properties", {}) or {}).get("region", "?") for f in body["features"]}
        ),
        "latency_ms": ms,
    }


def print_round(title: str, rows: list[dict], kind: str = "fwd") -> None:
    print()
    print(f"========== {title} ==========")
    if kind == "fwd":
        for r in rows:
            extras = (
                "  extras=" + json.dumps(r["extras"], separators=(",", ":"))
                if r["extras"]
                else ""
            )
            print(
                f"  q={r['q']!r:<42} {extras}"
            )
            parsed = r["parsed"]
            print(
                f"    parsed: tokens={parsed.get('street_tokens')!r}  "
                f"hn={parsed.get('house_number')!r}  pc={parsed.get('postal_code')!r}"
            )
            print(
                f"    result: count={r['count']:>3}  raw={r['raw_hits']:>5}  "
                f"mt={r['match_type']:<6}  latency={r['latency_ms']:>6.1f}ms  "
                f"regions={r['regions_seen']}"
            )
            print(f"    top:    {r['top']}")
            print()
    elif kind == "rev":
        for r in rows:
            extras = (
                "  extras=" + json.dumps(r["extras"], separators=(",", ":"))
                if r["extras"]
                else ""
            )
            print(
                f"  lat={r['lat']}  lng={r['lng']}{extras}"
            )
            print(
                f"    result: count={r['count']:>3}  raw={r['raw_hits']:>5}  "
                f"latency={r['latency_ms']:>6.1f}ms  regions={r['regions_seen']}"
            )
            print(f"    top:    {r['top']}")
            print()


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--base", default="http://127.0.0.1:8080")
    args = ap.parse_args()

    base = args.base

    # ---- Round 1: Brussels regressions (Phases 1–4 must still work) ----
    r1 = [
        fwd(base, "Avenue Louise 32"),
        fwd(base, "Grand-Place"),
    ]
    print_round("Round 1 — Brussels regression", r1)

    # ---- Round 2: Flanders / NL native ----
    r2 = [
        fwd(base, "Antwerpen Grote Markt"),
        fwd(base, "Gent Korenmarkt"),
        fwd(base, "Meir"),
    ]
    print_round("Round 2 — Flanders / NL native", r2)

    # ---- Round 3: Wallonia / FR + DE ----
    r3 = [
        fwd(base, "Liège Place Saint-Lambert"),
        fwd(base, "Rue Neuve Charleroi"),
        fwd(base, "Eupen Hauptstrasse"),
    ]
    print_round("Round 3 — Wallonia / FR + DE", r3)

    # ---- Round 4: cross-language muni aliases (highest-info) ----
    r4 = [
        fwd(base, "Anvers Grote Markt"),       # FR muni alias + NL street → should hit
        fwd(base, "Anvers Grand Place"),       # FR muni + FR street (no alias in data) → expected gap
        fwd(base, "Luik Place Saint-Lambert"), # NL muni alias for Liège + FR street
        fwd(base, "Bergen Grand-Place"),       # NL muni alias for Mons + FR street
    ]
    print_round("Round 4 — Cross-language muni aliases", r4)

    # ---- Round 5: disambiguation by postal code ----
    r5 = [
        fwd(base, "Grand-Place"),
        fwd(base, "Grand-Place 7000"),  # Mons
        fwd(base, "Grand-Place 4000"),  # Liège
    ]
    print_round("Round 5 — Postal-code disambiguation", r5)

    # ---- Round 6: fuzzy / latency probes ----
    r6 = [
        fwd(base, "Louise quetzal"),  # the never-matches probe (strict zero → fuzzy fallback)
        fwd(base, "rue saint"),       # common-token-OR probe (the actual worst case)
    ]
    print_round("Round 6 — Latency probes", r6)

    # ---- Reverse geocoding ----
    rev_rows = [
        rev(base, 51.2194, 4.4025),  # Antwerp Grote Markt area
        rev(base, 51.0543, 3.7174),  # Gent center
        rev(base, 50.6325, 5.5797),  # Liège center
        rev(base, 50.6308, 6.0353),  # Eupen center (DE area)
        rev(base, 50.4543, 3.9522),  # Mons center
        rev(base, 50.7470, 4.3580),  # Sint-Genesius-Rode (facilités, Brussels edge)
        rev(base, 50.6056, 3.3878),  # Tournai (deep FR/NL border)
    ]
    print_round("Reverse geocoding", rev_rows, kind="rev")

    return 0


if __name__ == "__main__":
    sys.exit(main())
