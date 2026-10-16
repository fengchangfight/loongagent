#!/usr/bin/env bash
#
# A release artifact someone else can verify and install (not "whatever was in my
# working tree when I ran cargo build").
#
#   ./scripts/release.sh
#
# Produces, in dist/:
#   loongagent-<version>-<target>.tar.gz   the binary, the README, and BUILD
#   SHA256SUMS                             checksums over everything above
#
# BUILD names the three things needed to say what this artifact *is*: the version,
# the commit, and the exact toolchain. Bit-for-bit reproducibility is not claimed —
# it needs pinned paths, timestamps and a controlled environment; what is claimed is
# that the artifact can be traced to a commit and a compiler.
set -euo pipefail

cd "$(dirname "$0")/.."

if ! git diff --quiet || ! git diff --cached --quiet; then
  echo "refusing to release from a dirty tree: commit or stash first" >&2
  git status --short >&2
  exit 1
fi

version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
commit="$(git rev-parse --short HEAD)"
target="$(rustc -vV | sed -n 's/^host: //p')"
toolchain="$(rustc --version)"
rustfmt_version="$(rustfmt --version || echo 'rustfmt: not installed')"
name="loongagent-${version}-${target}"
dist="dist"
stage="${dist}/${name}"

echo "== building ${name} (locked, release)"
cargo build --release --locked

echo "== staging ${stage}"
rm -rf "${stage}"
mkdir -p "${stage}/bin"
cp target/release/loongagent "${stage}/bin/loongagent"
cp README.md "${stage}/README.md"
cat > "${stage}/BUILD" <<EOF
name:      loongagent
version:   ${version}
commit:    ${commit}
target:    ${target}
built at:  $(date -u +%Y-%m-%dT%H:%M:%SZ)
toolchain: ${toolchain}
formatter: ${rustfmt_version}

install:
  tar -xzf ${name}.tar.gz -C /opt
  ln -sfn /opt/${name} /opt/loongagent/current   # or wherever you keep versions
  ln -sfn /opt/loongagent/current/bin/loongagent /usr/local/bin/loongagent

This artifact is not signed. The configuration layer is protected by the pack
envelope instead: it is signed and verified against a pinned key. What is not
signed is the local org file an administrator owns -- that is the thing that
must not be called enforced.
EOF

echo "== packing"
tar -czf "${dist}/${name}.tar.gz" -C "${dist}" "${name}"
rm -rf "${stage}"

echo "== checksums"
( cd "${dist}" && sha256sum "${name}.tar.gz" > SHA256SUMS && cat SHA256SUMS )

echo
echo "artifacts in ${dist}/:"
ls -la "${dist}" | sed 's/^/  /'
echo
echo "The binary is dynamically linked against glibc (check: ldd target/release/loongagent),"
echo "so it runs on distributions at least as new as the build host. For an older fleet,"
echo "build in a container pinned to that glibc, or add a musl target."
