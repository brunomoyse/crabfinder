#!/usr/bin/env python3
"""
Count the <com:status> distribution of address records in a BeSt Address
XML stream.

This is the script that 11-best-data-verified.md §9.3 *should* have been
derived from in the first place. The original §9.3 numbers were produced by
ad-hoc shell commands during a chat session, never versioned, and used the
`head -c <bytes>` shortcut to take a "sample" — which under-counted retired
addresses because BeSt's XML appears to stream current records first.

Two modes:

  --mode head-first --bytes N      Stream-decode the first N bytes of XML.
                                   Reproduces the original biased shortcut.
                                   Use this only to demonstrate the bias.

  --mode exhaustive                Stream-decode the whole XML, count every
                                   record. This is the authoritative method.
                                   Slow (8 GB XML for Flanders) but exact.

Usage:

  scripts/status_share.py --zip best-full-latest/FlandersAddress20260514L72.zip \\
                          --mode head-first --bytes 100000000

  scripts/status_share.py --zip best-full-latest/FlandersAddress20260514L72.zip \\
                          --mode exhaustive

The latter is what should be used to publish percentages in
11-best-data-verified.md §9.3. As of Phase 5, the crabfinder-indexer
already produces the equivalent number as part of its normal run (see the
Step 4 report) — prefer that over running this script if a full indexer
pass is available.
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from collections import Counter
from typing import Iterator

# We stream from `unzip -p` rather than using Python's zipfile module
# because the XML inside is multi-gigabyte uncompressed and we want to
# pipe-decode without buffering the whole thing.
STATUS_RE = re.compile(rb"<com:status>(\w+)</com:status>")
ADDR_TAG = rb"<tns:address"


def iter_status_in_addresses(stream: Iterator[bytes]) -> Iterator[str]:
    """
    Yield the status string for each <tns:address> record encountered.
    Skips status tags inside <com:streetNameStatus> / etc. by tracking
    whether we are currently inside an <tns:address> block.
    """
    buf = b""
    in_addr = False
    # Read in fixed-size chunks so memory stays bounded regardless of
    # input size.
    for chunk in stream:
        buf += chunk
        while True:
            if not in_addr:
                start = buf.find(ADDR_TAG)
                if start == -1:
                    # Keep the tail in case the open tag straddles the chunk
                    # boundary.
                    if len(buf) > 64:
                        buf = buf[-64:]
                    break
                buf = buf[start:]
                in_addr = True
            # in_addr == True; look for the close tag.
            close = buf.find(b"</tns:address>")
            if close == -1:
                break
            record = buf[: close + len(b"</tns:address>")]
            m = STATUS_RE.search(record)
            if m:
                yield m.group(1).decode("ascii", errors="replace")
            buf = buf[close + len(b"</tns:address>") :]
            in_addr = False


def chunked_stream(proc: subprocess.Popen, byte_limit: int | None) -> Iterator[bytes]:
    read = 0
    while True:
        chunk = proc.stdout.read(1 << 20)  # 1 MB
        if not chunk:
            return
        if byte_limit is not None:
            remaining = byte_limit - read
            if remaining <= 0:
                return
            if len(chunk) > remaining:
                yield chunk[:remaining]
                return
        read += len(chunk)
        yield chunk


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--zip", required=True, help="Path to *Address*.zip")
    ap.add_argument(
        "--mode",
        choices=["head-first", "exhaustive"],
        required=True,
        help="head-first reproduces the §9.3 bias; exhaustive is authoritative.",
    )
    ap.add_argument(
        "--bytes",
        type=int,
        default=None,
        help="Required for --mode head-first; number of bytes to read.",
    )
    ap.add_argument(
        "--by-decile",
        action="store_true",
        help="With --mode exhaustive, also print status distribution by stream-position decile. Reveals whether the stream is non-uniform.",
    )
    args = ap.parse_args()

    if args.mode == "head-first" and args.bytes is None:
        ap.error("--bytes is required with --mode head-first")

    proc = subprocess.Popen(["unzip", "-p", args.zip], stdout=subprocess.PIPE)
    assert proc.stdout is not None
    counter: Counter[str] = Counter()
    n_total = 0
    statuses_in_order: list[str] = [] if args.by_decile else []
    try:
        stream = chunked_stream(proc, args.bytes if args.mode == "head-first" else None)
        for status in iter_status_in_addresses(stream):
            counter[status] += 1
            n_total += 1
            if args.by_decile:
                statuses_in_order.append(status)
    finally:
        try:
            proc.stdout.close()
        except Exception:
            pass
        proc.wait()

    print(f"mode={args.mode} zip={args.zip}")
    if args.bytes:
        print(f"bytes_consumed_cap={args.bytes:,}")
    print(f"records_seen={n_total:,}")
    print(f"distribution:")
    for status, n in sorted(counter.items(), key=lambda x: -x[1]):
        pct = 100.0 * n / n_total if n_total else 0.0
        print(f"  {status:>10}: {n:>10,}  ({pct:5.2f}%)")
    if args.by_decile and n_total:
        print()
        print(f"by stream-position decile (each = ~{n_total // 10:,} records):")
        print(f"  decile   current   proposed   retired   reserved   total")
        per_decile = n_total // 10
        for d in range(10):
            lo = d * per_decile
            hi = (d + 1) * per_decile if d < 9 else n_total
            slab = statuses_in_order[lo:hi]
            c = Counter(slab)
            print(
                f"  {d * 10:>3d}–{(d + 1) * 10 if d < 9 else 100:<3d}%"
                f"  {c.get('current', 0):>7,}"
                f"  {c.get('proposed', 0):>8,}"
                f"  {c.get('retired', 0):>8,}"
                f"  {c.get('reserved', 0):>8,}"
                f"  {hi - lo:>6,}"
            )
    return 0


if __name__ == "__main__":
    sys.exit(main())
