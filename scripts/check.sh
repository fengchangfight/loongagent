#!/usr/bin/env bash
#
# The gates, in one command: the same set CI runs, so a push can be checked first.
#
#   ./scripts/check.sh          everything
#   FAST=1 ./scripts/check.sh   skip the release build (the slow one)
#
# Every gate here has caught something real in this repository:
#   formatting      a formatter sweep that also touched *generated* files, which is
#                   why the generator now writes formatted Rust
#   schema check    generated types drifting from crates/schema/protocol.toml
#   layering        a crate dependency pointing the wrong way (ADR-0004)
#   documents       a relative link to a file that no longer exists (the documents
#                   are also what nothing else in this list reads)
#   tests           including three end-to-end tests against the real binary
#   release build   code that compiles in debug and not with optimisations
set -euo pipefail

cd "$(dirname "$0")/.."

bold() { printf '\n\033[1m== %s\033[0m\n' "$1"; }

bold "formatting"
cargo fmt --all -- --check

bold "generated schema is current"
./scripts/gen-schema.py --check

bold "crate dependencies point down (ADR-0004)"
./scripts/check-layering.sh

bold "documents"
python3 scripts/check-docs.py

bold "tests"
cargo test --workspace --locked

if [ "${FAST:-0}" = "1" ]; then
  bold "release build"
  echo "skipped (FAST=1)"
else
  bold "release build"
  cargo build --workspace --release --locked
fi

bold "all gates passed"
