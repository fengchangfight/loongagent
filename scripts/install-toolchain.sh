#!/usr/bin/env bash
#
# Install the pinned toolchain, mirror first and upstream as the fallback.
#
# Why this is a script and not four lines of YAML: the same block appears in the
# `check` and `release` jobs of both `.github/workflows/ci.yml` and
# `.gitea/workflows/ci.yml`, and it was wrong in all four.
#
# The mirror (TUNA, see DEVELOPMENT.md) carries the rolling channels and the dated
# builds, but **not** the versioned `channel-rust-<version>.toml` manifests:
#
#   mirrors.tuna.tsinghua.edu.cn/rustup/dist/channel-rust-stable.toml    200
#   mirrors.tuna.tsinghua.edu.cn/rustup/dist/channel-rust-1.99.0.toml    404
#   static.rust-lang.org/dist/channel-rust-1.99.0.toml                   200
#
# so `rustup toolchain install 1.99.0` against the mirror fails, and CI was red at the
# *toolchain* step before it ever looked at the code. The mirror is still tried first —
# it is what makes a runner inside the network fast, and it is the only one that works
# from where this project is developed — and upstream is the fallback for the versions
# the mirror does not carry.
#
#   RUST_VERSION=1.99.0 ./scripts/install-toolchain.sh
set -euo pipefail

version="${RUST_VERSION:?set RUST_VERSION to the version to install}"
flags=(--profile minimal --component rustfmt --no-self-update)

upstream="https://static.rust-lang.org"

if rustup toolchain list | grep -q "^${version}-"; then
  echo "== toolchain ${version} is already installed"
else
  # The first attempt is *expected* to fail on a runner whose mirror lacks this
  # version, so the failure is handled here rather than left to `set -e`.
  if rustup toolchain install "$version" "${flags[@]}"; then
    :
  else
    echo
    echo "== ${RUSTUP_DIST_SERVER:-the default server} does not have ${version};"
    echo "   falling back to ${upstream} (a mirror carries the channels, not every version)"
    RUSTUP_DIST_SERVER="$upstream" RUSTUP_UPDATE_ROOT="$upstream/rustup" \
      rustup toolchain install "$version" "${flags[@]}"
  fi
fi

rustup default "$version"
rustc --version
