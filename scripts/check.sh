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
#   code contracts  a Rust file with no design anchor, or an item with no doc block
#   human-written   the human's notebook: an agent edit there is a failure, not a
#                   formatting nit (AGENTS.md, scripts/check-human-written.sh)
#   documents       a relative link that rotted, a document without its header,
#                   one over its line budget, or one with no checked path back
#                   to the design (the documents are also what nothing else in
#                   this list reads)
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

bold "code contracts"
python3 scripts/check-modules.py

bold "the human's notebook is untouched"
./scripts/check-human-written.sh

bold "documents"
python3 scripts/check-docs.py

bold "tests"
# The counts check judges the test totals README.md states against the totals
# the suite *reports*, so the run is teed into a scratch file outside the
# repository. pipefail is set above, so a failing test still fails the gate
# here; the trap removes the scratch file however this script leaves.
test_capture="$(mktemp "${TMPDIR:-/tmp}/loongcoder-tests.XXXXXX")"
trap 'rm -f "$test_capture"' EXIT
cargo test --workspace --locked | tee "$test_capture"
python3 scripts/check-docs.py --check counts --test-counts "$test_capture"

if [ "${FAST:-0}" = "1" ]; then
  bold "release build"
  echo "skipped (FAST=1)"
else
  bold "release build"
  cargo build --workspace --release --locked
fi

bold "all gates passed"
