#!/usr/bin/env python3
"""
Probe the running service to extract real bgids/best_ids for the
adversarial eval set (file 14 §3 — 100 entries).

For each anchor query, capture the top hit's (best_id, building_group_id,
muni_label, street_label) so the YAML can be generated with verified IDs.

For NOTABILITY entries, we query with a postal_code to anchor the muni,
then collect ALL bgids from the top-N where street_name matches the
landmark name — that goes into expected_building_group_id_set.

For KNOWN_FAILURE entries, no anchor query is needed; the entry just
documents expected_behavior.
"""

from __future__ import annotations

import json
import unicodedata
import urllib.parse
import urllib.request


def fold(s: str) -> str:
    """Strip diacritics + lowercase for substring matching that tolerates
    'Évêché' vs 'Evêché' style inconsistency in the BeSt data."""
    return "".join(
        c for c in unicodedata.normalize("NFD", s) if unicodedata.category(c) != "Mn"
    ).lower()


BASE = "http://127.0.0.1:8080"


def call(q: str, limit: int = 10) -> dict:
    url = BASE + "/geocode?" + urllib.parse.urlencode({"q": q, "limit": limit}, safe="+")
    with urllib.request.urlopen(url, timeout=10) as r:
        return json.loads(r.read())


def first_feature(body: dict):
    feats = body.get("features") or []
    return feats[0] if feats else None


def extract_ids(f: dict) -> tuple[str, str]:
    """Return (best_id, building_group_id) for one feature.
    best_id reassembled into the canonical "namespace/objid/verid" form
    (matches BestId::Display in crabfinder-core)."""
    props = f.get("properties", {}) or {}
    id_obj = props.get("id") or {}
    ns = (id_obj.get("namespace") or "").rstrip("/")
    oid = id_obj.get("object_identifier") or ""
    vid = id_obj.get("version_identifier") or ""
    best_id = f"{ns}/{oid}/{vid}" if ns else ""
    return (best_id, str(f.get("building_group_id", "")))


def extract_labels(f: dict) -> tuple[str, str]:
    props = f.get("properties", {}) or {}
    sn = props.get("street_name") or {}
    mn = props.get("municipality_name") or {}
    return (
        sn.get("nl") or sn.get("fr") or sn.get("de") or "?",
        mn.get("fr") or mn.get("nl") or mn.get("de") or "?",
    )


# ---------------------------------------------------------------- categories


# Hamlets (Wallonia parts of municipality). Anchor query targets a likely
# unique street in the hamlet so the top hit lands there.
HAMLETS = [
    # (subcategory, anchor_query)
    ("hives-laroche", "rue de la barrière hives"),
    ("beausaint-laroche", "beausaint la roche"),
    ("maredsous-anhee", "maredsous"),
    ("han-sur-lesse-rochefort", "han sur lesse 5580"),
    ("spontin-yvoir", "spontin yvoir"),
    ("crupet-assesse", "crupet"),
    ("sosoye-anhee", "sosoye"),
    ("falaen-onhaye", "falaën"),
    ("mozet-gesves", "mozet"),
    ("soulme-doische", "soulme"),
    ("laforet-vresse", "laforêt vresse"),
    ("chassepierre-florenville", "chassepierre"),
    ("torgny-rouvroy", "torgny"),
    ("mirwart-saint-hubert", "mirwart"),
    ("weris-durbuy", "wéris"),
    ("olloy-viroinval", "olloy-sur-viroin"),
    ("vierves-viroinval", "vierves-sur-viroin"),
    ("treignes-viroinval", "treignes"),
    ("roly-philippeville", "roly philippeville"),
    ("bellevaux-bouillon", "bellevaux bouillon"),
    ("frahan-bouillon", "frahan"),
    ("wibrin-houffalize", "wibrin"),
    ("lompret-chimay", "lompret"),
    ("aubechies-beloeil", "aubechies"),
    ("ny-hotton", "ny hotton"),
    ("ave-namur", "ave namur"),
    ("celles-houyet", "celles houyet"),
    ("furfooz-dinant", "furfooz"),
    ("limbourg-dolhain", "dolhain limbourg"),
    ("xhignesse-hamoir", "xhignesse"),
]


# Belgian German-speaking communes — NIS codes per file 11.
# Pick one anchor query per commune (the central square or town hall area).
DE_ZONE = [
    ("eupen-63023", "Rathausplatz Eupen 4700"),
    ("kelmis-63040", "Maxstraße Kelmis"),
    ("lontzen-63048", "Aachener Straße Lontzen"),
    ("raeren-63061", "Hauptstraße Raeren"),
    ("amel-63001", "Hauptstraße Amel"),
    ("buellingen-63012", "Bahnhofstraße Büllingen"),
    ("burg-reuland-63013", "Burg-Reuland 4790"),
    ("buetgenbach-63087", "Bahnhofstraße Bütgenbach"),
    ("sankt-vith-63067", "Hauptstraße Sankt Vith"),
]


# Six Brussels-periphery facilités communes.
FACILITES = [
    ("drogenbos-23098", "Grote Baan Drogenbos"),
    ("kraainem-23099", "Wezembeeklaan Kraainem"),
    ("linkebeek-23100", "Hollebeekstraat Linkebeek"),
    ("sint-genesius-rode-23101", "Linkebeekstraat Sint-Genesius-Rode"),
    ("wemmel-23102", "Steenweg Wemmel"),
    ("wezembeek-oppem-23103", "Astridlaan Wezembeek-Oppem"),
]


# Symmetric muni-alias pairs. Anchor: the canonical (often-iconic) address.
CROSS_LANGUAGE = [
    ("antwerpen-grote-markt", "Grote Markt 1, 2000 Antwerpen"),
    ("anvers-grote-markt-fr-muni", "Grote Markt 1 2000 Anvers"),
    ("liege-place-saint-lambert", "Place Saint-Lambert 1, 4000 Liège"),
    ("luik-place-saint-lambert", "Place Saint-Lambert 1 4000 Luik"),
    ("mons-grand-place", "Grand-Place 1, 7000 Mons"),
    ("bergen-grand-place", "Grand-Place 1 7000 Bergen"),
    ("tournai-grand-place", "Grand'Place 1, 7500 Tournai"),
    ("doornik-grand-place", "Grand'Place 1 7500 Doornik"),
    ("namur-rue-de-fer", "Rue de Fer 1, 5000 Namur"),
    ("namen-rue-de-fer", "Rue de Fer 1 5000 Namen"),
]


# Streets with special characters.
SPECIAL_CHARS = [
    ("place-eglise-apostrophe", "Place de l'Église 1 6000 Charleroi"),
    ("rue-saint-gery-accent", "Rue Saint-Géry 1 1000 Bruxelles"),
    ("rue-saint-gilles-hyphen", "Rue Saint-Gilles 1 4000 Liège"),
    ("place-saint-lambert-hyphen", "Place Saint-Lambert 1 4000 Liège"),
    ("grand-place-hyphen", "Grand-Place 1 1000 Bruxelles"),
    ("grand-place-apostrophe", "Grand'Place 1 7500 Tournai"),
    ("avenue-louise-no-special", "Avenue Louise 32 1050 Ixelles"),
    ("rue-de-la-loi-de-article", "Rue de la Loi 16 1000 Bruxelles"),
    ("place-poelaert-accent", "Place Poelaert 1 1000 Bruxelles"),
    ("rue-neuve-1", "Rue Neuve 1 1000 Bruxelles"),
]


# Notability whitelist — file 13 §4. expected_building_group_id_set with
# all bgids matching this iconic name in the canonical commune.
# Format: (subcategory, q_unqualified, anchor_query_with_postal,
#          street_name_substring, regression_marker)
NOTABILITY = [
    ("grand-place-bxl", "Grand-Place", "Grand-Place 1000", "grand-place", True),
    ("meir-anvers", "Meir", "Meir 2000", "meir", True),
    ("grote-markt-anvers", "Grote Markt", "Grote Markt 2000 Antwerpen", "grote markt", True),
    ("markt-bruges", "Markt", "Markt 8000 Brugge", "markt", True),
    ("burg-bruges", "Burg", "Burg 8000 Brugge", "burg", True),
    ("korenmarkt-gand", "Korenmarkt", "Korenmarkt 9000 Gent", "korenmarkt", True),
    ("vrijdagmarkt-gand", "Vrijdagmarkt", "Vrijdagmarkt 9000 Gent", "vrijdagmarkt", True),
    ("place-saint-lambert-liege", "Place Saint-Lambert", "Place Saint-Lambert 4000 Liège", "place saint-lambert", True),
    ("grand-place-mons", "Grand-Place Mons", "Grand-Place 7000 Mons", "grand-place", False),
    ("grand-place-tournai", "Grand-Place Tournai", "Grand'Place 7500 Tournai", "grand", False),
    ("sablon-bxl", "Sablon", "Place du Grand Sablon 1000", "sablon", False),
    ("place-de-brouckere-bxl", "Place de Brouckère", "Place de Brouckère 1000 Bruxelles", "brouckère", False),
    ("place-st-gery-bxl", "Place Saint-Géry", "Place Saint-Géry 1000", "saint-géry", False),
    ("place-jeu-de-balle-bxl", "Place du Jeu de Balle", "Place du Jeu de Balle 1000", "jeu de balle", False),
    ("kouter-gand", "Kouter", "Kouter 9000 Gent", "kouter", False),
    ("groenplaats-anvers", "Groenplaats", "Groenplaats 2000 Antwerpen", "groenplaats", False),
    ("steen-anvers", "Steenplein", "Steenplein 2000 Antwerpen", "steenplein", False),
    ("place-pol-mons", "Place Léopold Mons", "Place Léopold 7000 Mons", "léopold", False),
    ("rue-de-la-loi-bxl", "Rue de la Loi", "Rue de la Loi 1000 Bruxelles", "rue de la loi", True),
    ("avenue-louise-bxl", "Avenue Louise", "Avenue Louise 1050 Ixelles", "avenue louise", True),
    ("rue-haute-bxl", "Rue Haute", "Rue Haute 1000 Bruxelles", "rue haute", False),
    ("place-poelaert-bxl", "Place Poelaert", "Place Poelaert 1000 Bruxelles", "poelaert", False),
    ("place-flagey-ixelles", "Place Flagey", "Place Flagey 1050", "flagey", False),
    ("place-jourdan-bxl", "Place Jourdan", "Place Jourdan 1040", "jourdan", False),
    ("place-luxembourg-bxl", "Place du Luxembourg", "Place du Luxembourg 1050", "luxembourg", False),
    ("avenue-de-tervueren-bxl", "Avenue de Tervueren", "Avenue de Tervueren 1040", "tervueren", False),
    ("boulevard-anspach-bxl", "Boulevard Anspach", "Boulevard Anspach 1000", "anspach", False),
    ("dam-anvers", "Dam Antwerpen", "Dam 2060 Antwerpen", "dam", False),
    ("place-eveche-tournai", "Place de l'Évêché Tournai", "Place de l'Évêché 7500 Tournai", "évêché", False),
    ("place-grand-sablon-bxl", "Grand Sablon", "Grand Sablon 1000 Bruxelles", "sablon", False),
]


# Known-failure entries — surfaced in earlier phases, document don't fix.
KNOWN_FAILURE = [
    (
        "rue-de-la-gare-brussels",
        "Rue de la Gare",
        "Returns zero Brussels matches in top-100; file 13 §1. Top results are Wallonia "
        "(Villers-la-Ville, La Roche-en-Ardenne, La Louvière). Tracked for Phase 7 "
        "autocomplete design.",
        "2026-05-21",
        "airmail-analysis/13-phase6-3-1-design.md §1",
        "2026-08-21",
    ),
    (
        "place-de-l-eglise-ambiguous",
        "Place de l'Église",
        "36+ munis have a Place de l'Église. No notability prior should pick a 'right' "
        "answer until §3.1 adds whitelist scoring or postal context. Tracked as a "
        "Class-2 ambiguous query per file 13 §1.",
        "2026-05-21",
        "airmail-analysis/13-phase6-3-1-design.md §1",
        "2026-08-21",
    ),
    (
        "anvers-grand-place-cross-region",
        "Anvers Grand Place",
        "Anvers has no FR street 'Grand Place'. Expected behavior is fuzzy fallback "
        "within the Antwerpen/Anvers region, but currently the fuzzy OR returns "
        "brussels+wallonia noise. Cross-region leakage on an underspecified query. "
        "Tracked as Phase 6.5b region_leakage_rate input.",
        "2026-05-21",
        "airmail-analysis/12-phase5-outcomes.md §3.2",
        "2026-08-21",
    ),
]


# -------------------------------------------------------------- probe + emit


def probe_one(anchor: str, n: int = 5) -> list[dict]:
    """Return the top-N feature shapes with the fields we need."""
    body = call(anchor, limit=n)
    out = []
    for f in body.get("features", []):
        best_id, bgid = extract_ids(f)
        street, muni = extract_labels(f)
        out.append({
            "best_id": best_id,
            "bgid": bgid,
            "street": street,
            "muni": muni,
            "match_type": f.get("match_type"),
        })
    return out


def render(idx_counter, category, subcategory, query, top1, notes=None, regression_marker=False):
    yaml_lines = [
        f"  - id: adv-{category}-{idx_counter:03d}",
        f"    category: {category}",
        f"    subcategory: {subcategory}",
        f"    query: {json.dumps(query)}",
    ]
    if top1["best_id"]:
        yaml_lines.append(f"    expected_best_id: {json.dumps(top1['best_id'])}")
    yaml_lines.append(f"    expected_building_group_id: {json.dumps(top1['bgid'])}")
    yaml_lines.append(
        f'    notes: "{top1.get("street", "?")} — {top1.get("muni", "?")} ({top1.get("match_type", "?")})"'
    )
    if regression_marker:
        yaml_lines.append("    regression_marker: true")
    return "\n".join(yaml_lines)


def render_notability(idx, subcategory, q_unqualified, anchor, street_substr, regression_marker):
    """For notability: collect ALL bgids in top-10 whose street matches substr
    in ANY language slot (Brussels streets are bilingual — Grand-Place has
    NL form 'Grote Markt')."""
    body = call(anchor, limit=10)
    bgids = []
    best_ids = []
    seen_muni = "?"
    needle = fold(street_substr)
    for f in body.get("features", []):
        bid, bgid = extract_ids(f)
        _, muni = extract_labels(f)
        sn = (f.get("properties", {}) or {}).get("street_name") or {}
        all_lang_streets = [fold(sn.get(k) or "") for k in ("fr", "nl", "de")]
        if any(needle in s for s in all_lang_streets):
            bgids.append(bgid)
            best_ids.append(bid)
            seen_muni = muni
    yaml_lines = [
        f"  - id: adv-notability-{idx:03d}",
        f"    category: notability",
        f"    subcategory: {subcategory}",
        f"    query: {json.dumps(q_unqualified)}",
    ]
    if best_ids:
        yaml_lines.append(f"    expected_best_id: {json.dumps(best_ids[0])}")
    yaml_lines.append("    expected_building_group_id_set:")
    for bgid in bgids:
        yaml_lines.append(f"      - {json.dumps(bgid)}")
    if not bgids:
        yaml_lines.append('      []  # no matches found at anchor — REVIEW MANUALLY')
    note_body = f"{seen_muni} — anchor={anchor!r}; street match={street_substr!r}"
    yaml_lines.append(f"    notes: {json.dumps(note_body)}")
    if regression_marker:
        yaml_lines.append("    regression_marker: true")
    return "\n".join(yaml_lines)


def main():
    out = ["version: adversarial-belgium-v1", "", "# Header notes:",
           "# - expected_building_group_id_set (list) used for notability entries:",
           "#   queries like 'Grand-Place' can resolve to any of ~36 distinct buildings",
           "#   on Brussels' Grand-Place, all equivalent from the user's perspective.",
           "# - known_failure entries are tracked, NOT aggregated. Quarterly review",
           "#   enforced via acceptable_until.",
           "# - All best_id/bgid identifiers pinned to the live v3 index at",
           "#   generation time (2026-05-19 dump). Re-validate after BeSt refresh.",
           "",
           "queries:"]

    # 1. Hamlets
    out.append("\n# ---------- Hamlet (Wallonia parts of municipality) — 30 entries")
    for i, (subcat, anchor) in enumerate(HAMLETS, 1):
        top = probe_one(anchor)
        if not top:
            out.append(f"  # SKIPPED: {subcat} ({anchor!r}) returned no features")
            continue
        out.append(render(i, "hamlet", subcat, anchor, top[0]))

    # 2. DE zone
    out.append("\n# ---------- DE zone (9 German-speaking communes) — 9 entries")
    for i, (subcat, anchor) in enumerate(DE_ZONE, 1):
        top = probe_one(anchor)
        if not top:
            out.append(f"  # SKIPPED: {subcat} ({anchor!r}) returned no features")
            continue
        out.append(render(i, "de_zone", subcat, anchor, top[0]))

    # 3. Facilités
    out.append("\n# ---------- Facilités (Brussels periphery) — 6 entries")
    for i, (subcat, anchor) in enumerate(FACILITES, 1):
        top = probe_one(anchor)
        if not top:
            out.append(f"  # SKIPPED: {subcat} ({anchor!r}) returned no features")
            continue
        out.append(render(i, "facilites", subcat, anchor, top[0]))

    # 4. Cross_language
    out.append("\n# ---------- Cross_language muni-alias pairs — 10 entries (the §3.2 contract)")
    for i, (subcat, anchor) in enumerate(CROSS_LANGUAGE, 1):
        top = probe_one(anchor)
        if not top:
            out.append(f"  # SKIPPED: {subcat} ({anchor!r}) returned no features")
            continue
        out.append(render(i, "cross_language", subcat, anchor, top[0]))

    # 5. Special chars
    out.append("\n# ---------- Special chars (apostrophe/hyphen/accent) — 10 entries")
    for i, (subcat, anchor) in enumerate(SPECIAL_CHARS, 1):
        top = probe_one(anchor)
        if not top:
            out.append(f"  # SKIPPED: {subcat} ({anchor!r}) returned no features")
            continue
        out.append(render(i, "special_chars", subcat, anchor, top[0]))

    # 6. Notability (file 13 §4 whitelist)
    out.append("\n# ---------- Notability (file 13 §4 whitelist) — 30 entries — §3.1 target metric")
    for i, (subcat, q, anchor, street_substr, regression_marker) in enumerate(NOTABILITY, 1):
        out.append(render_notability(i, subcat, q, anchor, street_substr, regression_marker))

    # 7. Known_failure (3, no bgid lookup needed)
    out.append("\n# ---------- Known failures — 3 entries — tracked, NOT aggregated")
    for i, (subcat, q, behavior, introduced, ref, acceptable) in enumerate(KNOWN_FAILURE, 1):
        out.append(f"  - id: adv-known-{i:03d}")
        out.append(f"    category: known_failure")
        out.append(f"    subcategory: {subcat}")
        out.append(f"    query: {json.dumps(q)}")
        out.append(f"    expected_behavior: |")
        for line in behavior.split("\n"):
            out.append(f"      {line}")
        out.append("    expected_to_fail: true")
        out.append(f"    introduced: {json.dumps(introduced)}")
        out.append(f"    reference: {json.dumps(ref)}")
        out.append(f"    acceptable_until: {json.dumps(acceptable)}")

    print("\n".join(out))


if __name__ == "__main__":
    main()
