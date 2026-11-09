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
# `cargo install` cannot inherit a toolchain file's override — rustup warns and falls back
# to the default instead — so name the toolchain the file pins, explicitly. Read rather than
# hard-code `stable`, so moving the pin in `rust-toolchain.toml` moves these commands too.
toolchain="$(sed -n 's/^channel *= *"\([^"]*\)".*/\1/p' rust-toolchain.toml | head -n 1)"
[ -n "$toolchain" ] || toolchain=stable
cargo "+$toolchain" install --path crates/cli --locked --force
cargo "+$toolchain" install --path crates/control --locked --force
# `loongagent reverse` starts this server from PATH (its manifest names the bare
# command), so a session without it opens the portal but has none of the reverse
# tools. Install it here or the reverse surface is a page with no engine behind it.
cargo "+$toolchain" install --path crates/reverse/mcp-server --locked --force
echo
echo "installed:"
"$HOME/.cargo/bin/loongagent" --version | sed 's/^/  /'
"$HOME/.cargo/bin/loongagent-control" --version | sed 's/^/  /'
ls -l "$HOME/.cargo/bin/loong-reverse-mcp" | sed 's/^/  /'
echo
echo "repository HEAD: $(git rev-parse --short HEAD)"
