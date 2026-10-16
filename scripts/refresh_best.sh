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

log() { printf '\033[1m[refresh]\033[0m %s\n' "$*" >&2; }

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

# 2. Build the binaries and the index.
log "building release binaries"
cargo build --release -p crabfinder-indexer -p crabfinder-service -p crabfinder-eval
log "indexing into $index_dir"
rm -rf "$index_dir"
places_args=()
if [[ -n "${WITH_OSM:-}" ]]; then
    places_args=(--places data/osm-places-be-v1.csv)
fi
./target/release/crabfinder-indexer --best-dir "$best_dir" --output "$index_dir" \
    --notability-whitelist data/notability-whitelist-v1.yaml ${places_args[@]+"${places_args[@]}"}

# 3. Validate: every expected id in the eval sets must still resolve.
if [[ -z "${SKIP_VALIDATE:-}" ]]; then
    log "starting a validation service on 127.0.0.1:$port"
    ./target/release/crabfinder-service --index "$index_dir" --port "$port" \
        --abbreviation-dict data/street-type-abbreviations-v1.yaml >/dev/null 2>&1 &
    svc=$!
    trap 'kill $svc 2>/dev/null || true' EXIT
    for _ in $(seq 1 120); do
        curl -fs "http://127.0.0.1:$port/health" >/dev/null 2>&1 && break
        sleep 1
    done
    args=()
    for s in "${eval_sets[@]}"; do args+=(--eval-set "$s"); done
    ./target/release/crabfinder-eval --validate-eval-set "${args[@]}" \
        --service "http://127.0.0.1:$port"
    kill "$svc" 2>/dev/null || true
    trap - EXIT
fi

# 4. Swap the symlink atomically (rename(2) over the old link).
link="$data_dir/index-current"
python3 -c 'import os, sys; t, l = sys.argv[1:]; os.symlink(t, l + ".tmp"); os.replace(l + ".tmp", l)' \
    "$(basename "$index_dir")" "$link"
log "index-current -> $(basename "$index_dir")"
if [[ -n "${RESTART_CMD:-}" ]]; then
    log "restarting: $RESTART_CMD"
    bash -c "$RESTART_CMD"
else
    log "restart the service with: crabfinder-service --index $link"
fi
