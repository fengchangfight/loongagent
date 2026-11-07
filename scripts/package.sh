#!/usr/bin/env bash
#
# One-command package for end users, on Linux/macOS:
#
#   ./scripts/package.sh
#
# Produces, in release/ (git-ignored):
#   loongagent-<version>/
#     loongagent                     the release binary for this machine
#     loongagent-vscode-<v>.vsix     the VS Code extension
#     manual.html                    single-file quick-start manual (images inlined)
#
# The package is for people who will never see this repository: a binary, the
# extension, and a manual. Windows builds use scripts/package.ps1 instead; this
# script compiles only for the machine it runs on.
set -euo pipefail

cd "$(dirname "$0")/.."

version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
out="release/loongagent-${version}"
vscode_dir="clients/vscode"
manual_src="clients/manual/index.html"

echo "== [1/4] building loongagent ${version} (release, locked)"
cargo build --release --locked -p loongagent

echo "== [2/4] packaging the VS Code extension"
(
  cd "${vscode_dir}"
  if [ ! -d node_modules ]; then
    echo "   installing extension dependencies (npm ci)"
    npm ci
  fi
  npm run package:vsix
)

echo "== [3/4] staging ${out}"
rm -rf "${out}"
mkdir -p "${out}"
cp target/release/loongagent "${out}/loongagent"
cp "${manual_src}" "${out}/manual.html"
vsix="$(ls -t "${vscode_dir}"/*.vsix | head -1)"
cp "${vsix}" "${out}/"

echo "== [4/4] done"
echo
echo "package ready at ${out}/:"
ls -la "${out}" | sed 's/^/  /'
echo
echo "Hand the whole folder to a user: put the binary on PATH, open manual.html,"
echo "and (optionally) install the .vsix in VS Code."
