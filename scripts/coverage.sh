#!/usr/bin/env bash
# Workspace line coverage via cargo-llvm-cov.
#
#   scripts/coverage.sh            # summary + enforce the 95 % line gate
#   scripts/coverage.sh --html     # also write target/llvm-cov/html
#   COVERAGE_MIN=0 scripts/coverage.sh   # report only, no gate
#
# Thin binary entry points (`crates/*/src/main.rs`) and the test-support
# crate are excluded: every line of real logic lives in a library.
#
# The profile tools must come from the *same* LLVM that compiled the tests.
# When rustup is installed but another rustc (e.g. Homebrew) shadows it on
# PATH, prepend the rustup toolchain's bin dir so cargo, rustc and
# llvm-profdata all agree.
set -euo pipefail
cd "$(dirname "$0")/.."

if command -v rustup >/dev/null 2>&1; then
    toolchain_bin="$(dirname "$(rustup which rustc)")"
    export PATH="$toolchain_bin:$HOME/.cargo/bin:$PATH"
fi

min="${COVERAGE_MIN:-95}"
ignore='(crates/[^/]+/src/main\.rs$|crabfinder-test-support/)'

cargo llvm-cov clean --workspace >/dev/null 2>&1 || true
cargo llvm-cov --workspace --all-targets --no-report
cargo llvm-cov report --summary-only --ignore-filename-regex "$ignore" --fail-under-lines "$min"
if [[ "${1:-}" == "--html" ]]; then
    cargo llvm-cov report --html --ignore-filename-regex "$ignore"
    echo "HTML report: target/llvm-cov/html/index.html"
fi
