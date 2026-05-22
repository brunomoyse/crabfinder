#!/usr/bin/env python3
"""Rewrite adversarial eval entries to use `expected_building_group_id_set`
for queries that do not constrain a house number.

Phase 6 §3.1 follow-up: the per-HN `expected_building_group_id` is fragile
under reindexing — Tantivy assigns fresh segment UUIDs each rebuild, so
tied-score buildings on the same street swap top-1 positions arbitrarily.
For queries that don't pin an HN, ANY building on the canonical street is
semantically correct; the eval should express that as a set.

Scope (per file 12 §3.1.b policy):
  - Categories: hamlet, de_zone, facilites, special_chars
  - Only entries whose query string lacks an HN constraint
  - cross_language, notability, known_failure left untouched
  - special_chars entries that DO include an HN left untouched

Discovery: for each rewrite candidate, hit the live /geocode service with
expand=true, filter result bgids to those sharing the canonical street
prefix (everything before the last `|` of the original expected bgid),
and emit the sorted unique set.

Pre-conditions:
  - crabfinder-service running at SERVICE (boost mode irrelevant — we read
    candidates, not ranks)
  - eval/adversarial-belgium-v1.yaml exists at the project root
"""

from __future__ import annotations

import json
import re
import sys
import time
import urllib.parse
import urllib.request
from pathlib import Path

import yaml

SERVICE = "http://127.0.0.1:8080"
ADV_PATH = Path("eval/adversarial-belgium-v1.yaml")
BACKUP_PATH = Path("eval/adversarial-belgium-v1.yaml.pre-step7")
TARGET_CATS = {"hamlet", "de_zone", "facilites", "special_chars"}

HN_RE = re.compile(r"\b\d+[A-Z]?\b")


def has_house_number(query: str) -> bool:
    """True if the query contains a token that looks like an HN.
    Excludes bare 4-digit postal codes."""
    return any(not (len(m) == 4 and m.isdigit()) for m in HN_RE.findall(query))


def fetch_bgids(query: str, street_prefix: str) -> list[str]:
    """Query the service with expand=true and return all unique bgids
    whose canonical-street prefix matches."""
    url = (
        SERVICE
        + "/geocode?q="
        + urllib.parse.quote_plus(query)
        + "&expand=true&limit=100"
    )
    with urllib.request.urlopen(url, timeout=10) as r:
        data = json.loads(r.read())
    matched = {
        f["properties"]["building_group_id"]
        for f in data["features"]
        if f["properties"]["building_group_id"].startswith(street_prefix + "|")
    }
    return sorted(matched, key=_bgid_sort_key)


def _bgid_sort_key(bgid: str) -> tuple:
    """Sort bgids by HN suffix: numeric first, then alphanumeric (e.g.
    1, 2, 3, 14A) — gives readable diffs in the YAML."""
    hn = bgid.rsplit("|", 1)[-1]
    m = re.match(r"^(\d+)([A-Z]?)$", hn)
    if m:
        return (0, int(m.group(1)), m.group(2))
    return (1, hn)


def discover_all() -> dict[str, list[str]]:
    adv = yaml.safe_load(ADV_PATH.read_text())
    out: dict[str, list[str]] = {}
    warnings: list[str] = []
    for q in adv["queries"]:
        cat = q.get("category")
        if cat not in TARGET_CATS:
            continue
        if has_house_number(q["query"]):
            continue
        expected = q.get("expected_building_group_id")
        if not expected:
            warnings.append(f"  {q['id']}: no expected_building_group_id; skipping")
            continue
        prefix = expected.rsplit("|", 1)[0]
        try:
            bgids = fetch_bgids(q["query"], prefix)
        except Exception as e:
            warnings.append(f"  {q['id']}: service error {e}; skipping")
            continue
        if expected not in bgids:
            warnings.append(
                f"  {q['id']}: expected {expected} not in discovered set "
                f"(found {len(bgids)} bgids matching {prefix}|); keeping anyway"
            )
            # Force-include the expected so the test always passes for the
            # canonical entry. The discovery is a coverage *expansion*, not
            # a substitution.
            bgids = sorted({*bgids, expected}, key=_bgid_sort_key)
        if not bgids:
            warnings.append(f"  {q['id']}: 0 bgids discovered; keeping as-is")
            continue
        out[q["id"]] = bgids
        time.sleep(0.005)  # be polite to the service
    if warnings:
        print("WARNINGS:", file=sys.stderr)
        for w in warnings:
            print(w, file=sys.stderr)
    return out


def rewrite_yaml(rewrites: dict[str, list[str]]) -> str:
    """Line-based rewrite that preserves header comments, section headers,
    and untouched entries verbatim. Each block boundary is the next `  - id:`
    or section header / EOF.

    For each block in `rewrites`:
      - drop `expected_best_id:` line
      - replace `expected_building_group_id:` line with
        `expected_building_group_id_set:` followed by an indented list
    """
    text = ADV_PATH.read_text()
    lines = text.splitlines(keepends=True)

    # Find block ranges (start line index of each `  - id:`).
    block_starts: list[tuple[int, str]] = []
    for i, line in enumerate(lines):
        m = re.match(r"^  - id:\s*(\S+)", line)
        if m:
            block_starts.append((i, m.group(1)))
    # Add a sentinel for the file end.
    bounds = [(s, lines.__len__() if i + 1 == len(block_starts) else block_starts[i + 1][0])
              for i, (s, _) in enumerate(block_starts)]
    block_ids = [bid for _, bid in block_starts]

    # Rebuild output, emitting blocks verbatim unless in rewrites.
    out_chunks: list[str] = []
    cursor = 0
    for (start, end), block_id in zip(bounds, block_ids):
        out_chunks.append("".join(lines[cursor:start]))  # gap (comments etc.)
        block = lines[start:end]
        if block_id in rewrites:
            block = rewrite_block(block, rewrites[block_id])
        out_chunks.append("".join(block))
        cursor = end
    out_chunks.append("".join(lines[cursor:]))  # trailing content
    return "".join(out_chunks)


def rewrite_block(block: list[str], bgid_set: list[str]) -> list[str]:
    """Drop `expected_best_id`, replace `expected_building_group_id` with the
    set form. Other lines preserved verbatim."""
    new: list[str] = []
    set_written = False
    for line in block:
        if re.match(r"^    expected_best_id:", line):
            # drop
            continue
        if re.match(r"^    expected_building_group_id:", line):
            new.append("    expected_building_group_id_set:\n")
            for bg in bgid_set:
                # YAML-quote with double quotes; pipe-containing strings
                # MUST be quoted in flow context.
                new.append(f'      - "{bg}"\n')
            set_written = True
            continue
        new.append(line)
    if not set_written:
        # Defensive — block had no expected_building_group_id at all. Don't
        # silently add one; surface via stderr.
        print(
            f"WARNING: block had no expected_building_group_id; left as-is:\n"
            f"  {''.join(block).strip()[:200]}",
            file=sys.stderr,
        )
    return new


def main() -> int:
    if not ADV_PATH.exists():
        print(f"ERROR: {ADV_PATH} not found", file=sys.stderr)
        return 1
    print(f"Discovering bgid sets from {SERVICE}...")
    rewrites = discover_all()
    print(f"  rewrites to apply: {len(rewrites)}")
    sizes = sorted(len(v) for v in rewrites.values())
    if sizes:
        print(f"  bgid_set sizes: min={sizes[0]} median={sizes[len(sizes)//2]} max={sizes[-1]}")
    if not BACKUP_PATH.exists():
        BACKUP_PATH.write_text(ADV_PATH.read_text())
        print(f"  backup written to {BACKUP_PATH}")
    new_text = rewrite_yaml(rewrites)
    ADV_PATH.write_text(new_text)
    print(f"  wrote {ADV_PATH}")
    # Sanity parse
    parsed = yaml.safe_load(new_text)
    print(f"  re-parsed: {len(parsed['queries'])} queries OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
