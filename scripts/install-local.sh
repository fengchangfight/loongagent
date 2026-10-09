#!/usr/bin/env bash
#
# Install both binaries where PATH finds them, and say which commit each one is.
#
# The trap this exists to close: `~/.cargo/bin` holds a copy from whenever you last
# ran this, while `target/release` holds what you just built. Three times now a stale
# copy quietly ran old rules — publishing with the wrong issuer, not knowing a
# subcommand, refusing a pack the new code accepts. `--version` names the commit, so
# running this after a change is the cheap way to keep the two in step.
set -euo pipefail
cd "$(dirname "$0")/.."

export PATH="$HOME/.cargo/bin:$PATH"
cargo install --path crates/cli --locked --force
cargo install --path crates/control --locked --force
echo
echo "installed:"
"$HOME/.cargo/bin/loongagent" --version | sed 's/^/  /'
"$HOME/.cargo/bin/loongagent-control" --version | sed 's/^/  /'
echo
echo "repository HEAD: $(git rev-parse --short HEAD)"
