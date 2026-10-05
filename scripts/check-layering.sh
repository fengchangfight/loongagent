#!/usr/bin/env bash
# Assert the crate dependency direction that encodes the microkernel (ADR-0004).
# A dependency may only point *down*: edges are pluggable, the kernel is fixed,
# clients never reach back into the server.
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"

cargo metadata --format-version 1 --no-deps --quiet \
  | python3 scripts/layering_rules.py
