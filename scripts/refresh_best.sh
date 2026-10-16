#!/usr/bin/env bash
# Download the latest BeSt Address bundle, build a fresh index, validate it
# against the eval sets, then atomically point `index-current` at it.
#
#   scripts/refresh_best.sh                     # full refresh in the repo root
#   DATA_DIR=/srv/crab scripts/refresh_best.sh  # somewhere else
#   SKIP_DOWNLOAD=1 scripts/refresh_best.sh     # reuse today's best-<date>/
#   SKIP_VALIDATE=1 scripts/refresh_best.sh     # don't start a service to validate
#   WITH_OSM=1 scripts/refresh_best.sh          # also index the optional OSM places
#   EVAL_SETS="a.yaml b.json" scripts/refresh_best.sh   # sets to validate (space-separated)
#   RESTART_CMD="systemctl restart crabfinder" scripts/refresh_best.sh
#                                               # run after the swap (else: restart by hand)
#   REFRESH_IF_OLDER_THAN_DAYS=7 scripts/refresh_best.sh
#                                               # do nothing while index-current is younger
#   PRUNE=1 scripts/refresh_best.sh             # delete the download once indexed, and every
#                                               # index but the new one and the previous one
#   BIN_DIR=/usr/local/bin scripts/refresh_best.sh
#                                               # use these binaries instead of building them
#                                               # (the Docker image does this)
#
# Without crabfinder-eval next to the indexer (the Docker image), step 3
# only checks that the index holds at least MIN_DOCS addresses (default
# 6,000,000, the full country; set it lower for a --limit smoke run).
#
# BeSt Address is the only source of the index. With WITH_OSM=1 the
# OpenStreetMap places CSV (data/osm-places-be-v1.csv) is added as a
# query-interpretation aid for hamlet names; it never alters an address.
#
# The running service keeps serving the old index (it mmaps it at startup);
# restart it against `index-current` to pick up the new one.
set -euo pipefail
cd "$(dirname "$0")/.."

url="${BEST_URL:-https://opendata.bosa.be/download/best/best-full-latest.zip}"
data_dir="${DATA_DIR:-.}"
stamp="${STAMP:-$(date +%Y-%m-%d)}"
best_dir="$data_dir/best-$stamp"
index_dir="$data_dir/index-$stamp"
port="${VALIDATE_PORT:-18080}"
read -r -a eval_sets <<<"${EVAL_SETS:-eval/adversarial-belgium-v1.yaml}"
whitelist="${WHITELIST:-data/notability-whitelist-v1.yaml}"
abbrev="${ABBREV_DICT:-data/street-type-abbreviations-v1.yaml}"
bin="${BIN_DIR:-./target/release}"
link="$data_dir/index-current"

log() { printf '\033[1m[refresh]\033[0m %s\n' "$*" >&2; }

# 0. Nothing to do while the current index is recent enough.
if [[ -n "${REFRESH_IF_OLDER_THAN_DAYS:-}" && -e "$link" ]] \
    && [[ -n "$(find -L "$link" -maxdepth 0 -mtime "-$REFRESH_IF_OLDER_THAN_DAYS")" ]]; then
    log "index-current is less than $REFRESH_IF_OLDER_THAN_DAYS days old; nothing to do"
    exit 0
fi
previous="$(readlink "$link" 2>/dev/null || true)"
# Never rebuild over the index being served (a second refresh the same day).
if [[ -n "$previous" && "$(basename "$index_dir")" == "$previous" ]]; then
    index_dir="$index_dir-$(date +%H%M%S)"
fi

# 1. Download + unpack. The outer zip holds one zip per region × entity;
#    flatten them into $best_dir so `--best-dir` finds them.
if [[ -z "${SKIP_DOWNLOAD:-}" ]]; then
    mkdir -p "$best_dir"
    log "downloading $url"
    curl -fL --retry 3 --progress-bar -o "$best_dir/best-full.zip" "$url"
    log "unpacking into $best_dir"
    unzip -q -o "$best_dir/best-full.zip" -d "$best_dir/unpacked"
    find "$best_dir/unpacked" -name '*.zip' -exec mv {} "$best_dir/" \;
    rm -rf "$best_dir/unpacked" "$best_dir/best-full.zip"
fi
ls "$best_dir"/*Address*.zip >/dev/null 2>&1 || { log "no *Address*.zip in $best_dir"; exit 1; }

# 2. Build the binaries (unless BIN_DIR has them) and the index.
if [[ -z "${BIN_DIR:-}" ]]; then
    log "building release binaries"
    cargo build --release -p crabfinder-indexer -p crabfinder-service -p crabfinder-eval
fi
log "indexing into $index_dir"
rm -rf "$index_dir"
places_args=()
if [[ -n "${WITH_OSM:-}" ]]; then
    places_args=(--places data/osm-places-be-v1.csv)
fi
"$bin/crabfinder-indexer" --best-dir "$best_dir" --output "$index_dir" \
    --notability-whitelist "$whitelist" ${places_args[@]+"${places_args[@]}"}

# 3. Validate: every expected id in the eval sets must still resolve
#    (or, without crabfinder-eval, the index must be big enough).
if [[ -z "${SKIP_VALIDATE:-}" && ! -x "$bin/crabfinder-eval" ]]; then
    docs="$(grep -o '"max_doc": *[0-9]*' "$index_dir/meta.json" | awk -F: '{s += $2} END {print s + 0}')"
    min="${MIN_DOCS:-6000000}"
    if (( docs < min )); then
        log "the new index holds $docs addresses, fewer than MIN_DOCS=$min; keeping the current one"
        exit 1
    fi
    log "the new index holds $docs addresses"
elif [[ -z "${SKIP_VALIDATE:-}" ]]; then
    log "starting a validation service on 127.0.0.1:$port"
    "$bin/crabfinder-service" --index "$index_dir" --port "$port" \
        --abbreviation-dict "$abbrev" >/dev/null 2>&1 &
    svc=$!
    trap 'kill $svc 2>/dev/null || true' EXIT
    for _ in $(seq 1 120); do
        curl -fs "http://127.0.0.1:$port/health" >/dev/null 2>&1 && break
        sleep 1
    done
    args=()
    for s in "${eval_sets[@]}"; do args+=(--eval-set "$s"); done
    "$bin/crabfinder-eval" --validate-eval-set "${args[@]}" \
        --service "http://127.0.0.1:$port"
    kill "$svc" 2>/dev/null || true
    trap - EXIT
fi

# 4. Swap the symlink atomically (rename(2) over the old link): GNU
#    `mv -T`, else Python (macOS).
ln -sfn "$(basename "$index_dir")" "$link.tmp"
if mv --version >/dev/null 2>&1; then
    mv -T "$link.tmp" "$link"
else
    python3 -c 'import os, sys; os.replace(sys.argv[1], sys.argv[2])' "$link.tmp" "$link"
fi
log "index-current -> $(basename "$index_dir")"

# 5. Optionally free the disk: the download, and indexes other than the
#    new one and the one a running service may still have open.
if [[ -n "${PRUNE:-}" ]]; then
    rm -rf "$best_dir"
    for d in "$data_dir"/index-*; do
        name="$(basename "$d")"
        [[ -d "$d" && ! -L "$d" ]] || continue
        [[ "$name" == "$(basename "$index_dir")" || "$name" == "$previous" ]] && continue
        log "pruning $name"
        rm -rf "$d"
    done
fi
if [[ -n "${RESTART_CMD:-}" ]]; then
    log "restarting: $RESTART_CMD"
    bash -c "$RESTART_CMD"
else
    log "restart the service with: crabfinder-service --index $link"
fi
