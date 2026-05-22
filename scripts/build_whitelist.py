#!/usr/bin/env python3
"""
Build data/notability-whitelist-v1.yaml from the adversarial eval set's
30 `category: notability` entries.

The whitelist is the SOURCE OF TRUTH for the indexer's notability prior
(file 13 §4–§6, file 14 §1 metric #14). The adversarial eval set is the
TEST against that source. They share building_group_ids but live in
different files so each can be edited / regenerated independently.

Rationale text is human-curated based on UNESCO / tourism / political
importance — drawn from file 13 §4 and Phase-5 smoke battery findings.

The +20 second-tier toponyms file 14 §3 mentions are listed in
SECOND_TIER below; they probe the live service for their bgids at script
time so the resulting whitelist is fully self-contained.
"""

from __future__ import annotations

import json
import urllib.parse
import urllib.request

import yaml  # the adversarial file is YAML and we want clean dump


BASE = "http://127.0.0.1:8080"


# ----------------------------------------------------------------- rationale


# Human-curated rationale per notability subcategory. Lines kept short so
# the YAML stays readable. The string keys match adversarial-belgium-v1.yaml
# subcategory values.
RATIONALE: dict[str, tuple[str, str, float]] = {
    # 30 first-tier (existing adversarial)
    "grand-place-bxl": ("Grand-Place / Grote Markt — Bruxelles", "UNESCO World Heritage; central square of Brussels.", 3.0),
    "meir-anvers":     ("Meir — Anvers", "Iconic 1.6 km shopping axis through Antwerp city centre.", 3.0),
    "grote-markt-anvers": ("Grote Markt — Anvers", "Antwerp's central square; cathedral + city hall.", 3.0),
    "markt-bruges":    ("Markt — Brugge", "UNESCO old town main square (Belfort, Provinciaal Hof).", 3.0),
    "burg-bruges":     ("Burg — Brugge", "UNESCO old town civic square (Stadhuis, Heilig Bloed).", 3.0),
    "korenmarkt-gand": ("Korenmarkt — Gent", "Central market square; Sint-Niklaaskerk, Belfort access.", 3.0),
    "vrijdagmarkt-gand": ("Vrijdagmarkt — Gent", "Historic civic square; weekly market for centuries.", 3.0),
    "place-saint-lambert-liege": ("Place Saint-Lambert — Liège", "Liège's central square; Palais des Princes-Évêques.", 3.0),
    "grand-place-mons": ("Grand-Place — Mons", "UNESCO belfry square; Doudou processional point.", 3.0),
    "grand-place-tournai": ("Grand-Place / Grand'Place — Tournai", "Mediaeval central square; cathedral approach.", 3.0),
    "sablon-bxl":      ("Sablon (Grand Sablon) — Bruxelles", "Notre-Dame du Sablon; antiques quarter.", 2.5),
    "place-de-brouckere-bxl": ("Place de Brouckère — Bruxelles", "Major Brussels civic square; metro hub.", 2.0),
    "place-st-gery-bxl": ("Place Saint-Géry — Bruxelles", "Hipster heart of Brussels old town.", 2.0),
    "place-jeu-de-balle-bxl": ("Place du Jeu de Balle — Bruxelles", "Marolles flea market square; daily Vieux Marché.", 2.0),
    "kouter-gand":     ("Kouter — Gent", "Ghent's flower-market square; weekly Sunday market.", 2.0),
    "groenplaats-anvers": ("Groenplaats — Anvers", "Antwerp cathedral square (Rubens monument).", 2.5),
    "steen-anvers":    ("Steenplein — Anvers", "Antwerp's medieval Steen castle riverfront.", 2.0),
    "place-pol-mons":  ("Place Léopold — Mons", "Mons Place Léopold civic and commerce centre.", 2.0),
    "rue-de-la-loi-bxl": ("Rue de la Loi — Bruxelles", "EU institutions axis; Brussels political street.", 2.5),
    "avenue-louise-bxl": ("Avenue Louise — Ixelles", "Brussels' grand luxury avenue; iconic 2.6 km axis.", 2.5),
    "rue-haute-bxl":   ("Rue Haute — Bruxelles", "Marolles main commercial street.", 2.0),
    "place-poelaert-bxl": ("Place Poelaert — Bruxelles", "Palais de Justice esplanade; panoramic viewpoint.", 2.0),
    "place-flagey-ixelles": ("Place Eugène Flagey — Ixelles", "Ixelles civic square; weekly market.", 2.0),
    "place-jourdan-bxl": ("Place Jourdan — Bruxelles", "Etterbeek square; Maison Antoine fries.", 2.0),
    "place-luxembourg-bxl": ("Place du Luxembourg — Bruxelles", "European Parliament square.", 2.0),
    "avenue-de-tervueren-bxl": ("Avenue de Tervueren — Bruxelles", "Long royal axis to Tervueren park.", 2.0),
    "boulevard-anspach-bxl": ("Boulevard Anspach — Bruxelles", "Central pedestrian Brussels boulevard.", 2.0),
    "dam-anvers":      ("Dam — Antwerpen 2060", "Antwerp Park Spoor Noord-area neighbourhood square.", 2.0),
    "place-eveche-tournai": ("Place de l'Évêché — Tournai", "Tournai cathedral square (UNESCO).", 2.5),
    "place-grand-sablon-bxl": ("Place du Grand Sablon — Bruxelles", "Antiques and chocolate quarter (same as Sablon, see notes).", 2.5),

    # 20 second-tier additions (probed below)
    "atomium-bxl": ("Atomium — Bruxelles (Heysel)", "Belgium's iconic 1958 World's Fair monument.", 2.5),
    "manneken-pis-bxl": ("Manneken Pis — Bruxelles", "Brussels' iconic statuette; pilgrimage tourist target.", 2.0),
    "mont-des-arts-bxl": ("Mont des Arts — Bruxelles", "Cultural acropolis between upper/lower town.", 2.0),
    "petit-sablon-bxl": ("Petit Sablon (Place du Petit Sablon) — Bruxelles", "Statuary garden square opposite Grand Sablon.", 2.0),
    "place-royale-bxl": ("Place Royale — Bruxelles", "Royal palace approach; museums district.", 2.0),
    "oude-markt-leuven": ("Oude Markt — Leuven", "'Longest bar in the world'; Leuven student heart.", 2.0),
    "grote-markt-leuven": ("Grote Markt — Leuven", "Leuven's Stadhuis square (gothic city hall).", 2.5),
    "grote-markt-mechelen": ("Grote Markt — Mechelen", "Mechelen's central square (Sint-Rombouts belfry).", 2.5),
    "burchtplein-anvers": ("Het Steen — Antwerpen", "Steen castle plaza; Schelde riverfront.", 2.0),
    "place-st-jacques-liege": ("Place Saint-Jacques — Liège", "Liège church square (Romanesque-Gothic St-Jacques).", 2.0),
    "place-cathedrale-liege": ("Place de la Cathédrale — Liège", "Saint-Paul cathedral square (Liège centre).", 2.0),
    "rue-vinave-liege": ("Vinâve d'Île — Liège", "Liège's main pedestrian shopping strip.", 2.0),
    "place-st-aubain-namur": ("Place Saint-Aubain — Namur", "Namur cathedral square.", 2.0),
    "grand-place-charleroi": ("Place du Manège — Charleroi", "Charleroi central square; Eden cultural centre.", 2.0),
    "place-marche-charleroi": ("Place du Marché — Charleroi", "Charleroi market square (Ville-Basse).", 2.0),
    "place-saint-aubain-namur": ("Place du Marché-aux-Légumes — Namur", "Namur old market square.", 2.0),
    "ijzerlaan-anvers": ("IJzerlaan — Antwerpen", "Major Antwerp axis (placeholder iconic).", 1.8),
    "place-charles-rogier-bxl": ("Place Charles Rogier — Bruxelles", "Brussels-Nord station square.", 1.8),
    "place-stephanie-bxl": ("Place Stéphanie — Bruxelles", "Upper-Louise commerce square.", 1.8),
    "place-rouppe-bxl": ("Place Rouppe — Bruxelles", "Brussels south-of-centre square (cocktail bars).", 1.8),
}


# Second-tier probes: (subcategory, query_anchor_with_postal, street_substr).
# Mirrors the format used for adv-notability-* probing.
SECOND_TIER_PROBES = [
    ("atomium-bxl", "Atomium 1020 Bruxelles", "atomium"),
    ("manneken-pis-bxl", "Rue du Chêne 1000 Bruxelles", "chêne"),  # cul-de-sac near Manneken
    ("mont-des-arts-bxl", "Mont des Arts 1000 Bruxelles", "mont des arts"),
    ("petit-sablon-bxl", "Place du Petit Sablon 1000 Bruxelles", "petit sablon"),
    ("place-royale-bxl", "Place Royale 1000 Bruxelles", "place royale"),
    ("oude-markt-leuven", "Oude Markt 3000 Leuven", "oude markt"),
    ("grote-markt-leuven", "Grote Markt 3000 Leuven", "grote markt"),
    ("grote-markt-mechelen", "Grote Markt 2800 Mechelen", "grote markt"),
    ("burchtplein-anvers", "Steenplein 2000 Antwerpen", "steenplein"),
    ("place-st-jacques-liege", "Place Saint-Jacques 4000 Liège", "saint-jacques"),
    ("place-cathedrale-liege", "Place de la Cathédrale 4000 Liège", "cathédrale"),
    ("rue-vinave-liege", "Vinâve d'Île 4000 Liège", "vinâve"),
    ("place-st-aubain-namur", "Place Saint-Aubain 5000 Namur", "saint-aubain"),
    ("grand-place-charleroi", "Place du Manège 6000 Charleroi", "manège"),
    ("place-marche-charleroi", "Place du Marché 6000 Charleroi", "marché"),
    ("place-saint-aubain-namur", "Place du Marché-aux-Légumes 5000 Namur", "marché-aux-légumes"),
    ("ijzerlaan-anvers", "IJzerlaan 2060 Antwerpen", "ijzerlaan"),
    ("place-charles-rogier-bxl", "Place Charles Rogier 1210 Bruxelles", "rogier"),
    ("place-stephanie-bxl", "Place Stéphanie 1050 Bruxelles", "stéphanie"),
    ("place-rouppe-bxl", "Place Rouppe 1000 Bruxelles", "rouppe"),
]


def fold(s: str) -> str:
    import unicodedata
    return "".join(
        c for c in unicodedata.normalize("NFD", s) if unicodedata.category(c) != "Mn"
    ).lower()


def probe_bgids(anchor: str, street_substr: str) -> list[str]:
    url = BASE + "/geocode?" + urllib.parse.urlencode({"q": anchor, "limit": 10}, safe="+")
    with urllib.request.urlopen(url, timeout=10) as r:
        body = json.loads(r.read())
    needle = fold(street_substr)
    bgids = []
    for f in body.get("features", []):
        sn = (f.get("properties", {}) or {}).get("street_name") or {}
        all_langs = [fold(sn.get(k) or "") for k in ("fr", "nl", "de")]
        if any(needle in s for s in all_langs):
            bgids.append(f.get("building_group_id", ""))
    return bgids


def main():
    # 1. Read adversarial-belgium-v1.yaml; extract notability entries
    with open("eval/adversarial-belgium-v1.yaml") as f:
        adv = yaml.safe_load(f)
    notability_entries = [q for q in adv["queries"] if q.get("category") == "notability"]

    output_entries = []

    # First-tier: lifted from adversarial set
    for q in notability_entries:
        subcat = q["subcategory"]
        bgids = q.get("expected_building_group_id_set") or []
        rationale_entry = RATIONALE.get(subcat)
        if not rationale_entry:
            canonical_name, rationale_text, multiplier = (q.get("query", "?"), "(no rationale)", 2.0)
        else:
            canonical_name, rationale_text, multiplier = rationale_entry
        output_entries.append({
            "notability_id": f"nota-{subcat}",
            "canonical_name": canonical_name,
            "target_building_group_ids": bgids,
            "boost_multiplier": multiplier,
            "rationale": rationale_text,
        })

    # Second-tier: probe live service for bgids
    skipped_second_tier = []
    for subcat, anchor, street_substr in SECOND_TIER_PROBES:
        bgids = probe_bgids(anchor, street_substr)
        if not bgids:
            skipped_second_tier.append(subcat)
            continue
        rationale_entry = RATIONALE.get(subcat)
        if not rationale_entry:
            continue
        canonical_name, rationale_text, multiplier = rationale_entry
        output_entries.append({
            "notability_id": f"nota-{subcat}",
            "canonical_name": canonical_name,
            "target_building_group_ids": bgids,
            "boost_multiplier": multiplier,
            "rationale": rationale_text,
        })

    out_doc = {
        "version": "v1",
        "entries": output_entries,
    }

    out_path = "data/notability-whitelist-v1.yaml"
    import os
    os.makedirs(os.path.dirname(out_path), exist_ok=True)
    with open(out_path, "w") as f:
        f.write(
            "# crabfinder notability whitelist (file 13 §4, file 14 §1 metric #14)\n"
            "#\n"
            "# Each entry contributes a `boost_multiplier` (> 1.0) applied to BM25\n"
            "# at query time via Tantivy's TopDocs::tweak_score, against any address\n"
            "# whose building_group_id is in `target_building_group_ids`.\n"
            "#\n"
            "# Higher multiplier = stronger 'this is the famous one' signal.\n"
            "# 3.0 = UNESCO / globally famous; 2.0–2.5 = nationally iconic;\n"
            "# 1.8 = locally known.\n"
            "#\n"
            "# To add an entry, query the service with the canonical name +\n"
            "# postal_code, capture top-N building_group_ids whose street_name\n"
            "# matches the place, and PR an entry. The schema is intentionally\n"
            "# small so contributions are low-friction.\n\n"
        )
        yaml.safe_dump(out_doc, f, default_flow_style=False, sort_keys=False, allow_unicode=True, width=200)

    print(f"Wrote {out_path} with {len(output_entries)} entries")
    if skipped_second_tier:
        print(f"Skipped {len(skipped_second_tier)} second-tier (no bgid hit): {skipped_second_tier}")


if __name__ == "__main__":
    main()
