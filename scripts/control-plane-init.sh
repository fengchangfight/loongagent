#!/usr/bin/env bash
#
# Bring up a control plane on this machine: layout, signing key, tokens, and a
# starter pack for the administrator to keep in their configuration repository.
# Run it on the server that will serve profiles, as the user that will run
# `loongagent-control serve` (root, in the usual deployment).
#
#   ./scripts/control-plane-init.sh --issuer acme-platform
#   ./scripts/control-plane-init.sh --issuer acme \
#       --dir /etc/loongagent-control --state /var/lib/loongagent-control \
#       --starter ~/config/loongagent/profiles.toml
#
# Two roots, because they hold two different kinds of thing (FHS):
#
#   --dir    /etc/loongagent-control        what an administrator authors and what the
#                                           service must read at boot: the signing key
#                                           and the tokens, mode 0600.
#   --state  /var/lib/loongagent-control    what the service *writes*: the signed pack
#                                           it serves. Regenerable by republishing.
#
# No draft is written under /etc: a pack is versioned, PR-reviewable configuration
# (ADR-0008 §6), so its home is the administrator's repository and it reaches the
# server only through `publish --pack`.
set -euo pipefail

cd "$(dirname "$0")/.."

dir="/etc/loongagent-control"
state="/var/lib/loongagent-control"
issuer=""
port="8738"
bind="127.0.0.1"
starter=""

while [ $# -gt 0 ]; do
  case "$1" in
    --dir) dir="$2"; shift 2 ;;
    --state) state="$2"; shift 2 ;;
    --issuer) issuer="$2"; shift 2 ;;
    --port) port="$2"; shift 2 ;;
    --bind) bind="$2"; shift 2 ;;
    --starter) starter="$2"; shift 2 ;;
    -h|--help) sed -n '2,22p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

if [ -z "$issuer" ]; then
  echo "usage: $0 --issuer NAME [--dir DIR] [--state DIR] [--port N] [--bind IP] [--starter PATH]" >&2
  echo "  NAME is who signs the packs; installs show it (e.g. acme-platform)." >&2
  echo "  --starter writes a draft pack to PATH (keep it in your configuration repo)." >&2
  exit 2
fi

binary="target/release/loongagent-control"
if [ ! -x "$binary" ]; then
  echo "== building $binary"
  cargo build --release -p loong-control
fi

# The signing key must never be committed: a key in a repository is an off-site
# ability to mint packs every install accepts.
work_tree=""
probe="$(cd "$dir" 2>/dev/null && pwd || echo "$dir")"
while [ -n "$probe" ] && [ "$probe" != "/" ]; do
  if [ -e "$probe/.git" ]; then work_tree="$probe"; break; fi
  probe="$(dirname "$probe")"
done
if [ -n "$work_tree" ] && [ "${FORCE:-0}" = "1" ]; then
  echo "warning: creating private material inside the git work tree at $work_tree" >&2
  echo "  add these lines to .gitignore first:" >&2
  echo "    control-key.pkcs8" >&2
  echo "    client.token" >&2
  echo "    admin.token" >&2
fi
if [ -n "$work_tree" ] && [ "${FORCE:-0}" != "1" ]; then
  echo "refusing to create private material inside the git work tree at $work_tree" >&2
  echo "  use --dir /etc/loongagent-control, or FORCE=1 if this repository exists to" >&2
  echo "  hold secrets and ignores them (add: control-key.pkcs8, client.token, admin.token)." >&2
  exit 1
fi

if [ -e "$dir/control-key.pkcs8" ]; then
  echo "refusing to touch an existing control plane: $dir/control-key.pkcs8 exists" >&2
  echo "  a new key would invalidate every install that pinned the old one." >&2
  exit 1
fi

echo "== creating $dir (mode 0700) and $state"
mkdir -p "$dir" "$state"
chmod 700 "$dir"

echo "== generating the signing key (private half stays here, mode 0600)"
"$binary" keygen --out "$dir" > "$dir/keygen.log"
public_key="$(grep -A1 'public key' "$dir/keygen.log" | tail -1 | tr -d ' ')"
key_id="$(grep 'key id' "$dir/keygen.log" | awk '{print $3}')"

echo "== generating tokens (mode 0600); the client token cannot publish"
umask 077
head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n' > "$dir/client.token"
head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n' > "$dir/admin.token"
printf '\n' >> "$dir/client.token"
printf '\n' >> "$dir/admin.token"
chmod 600 "$dir/client.token" "$dir/admin.token"
rm -f "$dir/keygen.log"

if [ -n "$starter" ]; then
  if [ -e "$starter" ]; then
    echo "refusing to overwrite the existing starter $starter" >&2
    exit 1
  fi
  mkdir -p "$(dirname "$starter")"
  cat > "$starter" <<'TOML'
# A loongagent profile pack. Keep this file in a repository: it is configuration
# that reviewers read and that a control plane signs, not state.
#
# Engineers never see it. They see each `name` and `description` below (T-07), so
# write a description about cost and behaviour, not about vendors.
#
# Publish it (the control plane validates and signs it):
#   loongagent-control publish --url http://127.0.0.1:8738 --pack profiles.toml \
#     --token-file /etc/loongagent-control/admin.token --version 1
default_profile = "default"

[[profiles]]
name = "default"
description = "Balanced: the everyday profile. Replace this sentence."
  [[profiles.models]]
  name = "main"
  kind = "openai-compat"
  base_url = "http://127.0.0.1:8000/v1"      # provider, your gateway, or a local server
  model = "REPLACE-WITH-A-MODEL-ID"
  api_key = "env:DEEPSEEK_API_KEY"           # a *placeholder*: resolved on the client,
                                             # never a literal. This file is committed, so
                                             # it must not contain the secret itself.
  budget_tokens = 128000
  [profiles.router]
  default = "main"
TOML
  echo "== wrote a starter draft: $starter"
fi

echo
echo "control plane ready."
echo "  configuration: $dir            (key and tokens, 0600, root)"
echo "  served pack:   $state/profiles.json"
echo "  issuer:        $issuer"
echo "  key id:        $key_id"
echo "  public key:    $public_key"
echo
echo "1) start it (foreground first; a systemd unit is in the runbook):"
echo "     $binary serve --dir $state --key $dir/control-key.pkcs8 --issuer $issuer \\"
echo "       --client-token-file $dir/client.token --admin-token-file $dir/admin.token \\"
echo "       --bind $bind --port $port"
echo
echo "2) publish the draft — from your configuration repository, not from /etc:"
echo "     $binary publish --url http://127.0.0.1:$port --pack ./profiles.toml \\"
echo "       --token-file $dir/admin.token --version 1"
echo
echo "3) check what it is serving:"
echo "     $binary show --url http://127.0.0.1:$port --token-file $dir/client.token \\"
echo "       --public-key $public_key"
echo
echo "4) point an install at it. Address and key belong to the machine:"
echo "     sudo loongagent setup --system --url http://127.0.0.1:$port \\"
echo "       --public-key $public_key --token file:$dir/client.token"
echo "   or per user (each install keeps its own copy of the read token):"
echo "     loongagent setup --url http://127.0.0.1:$port --public-key $public_key \\"
echo "       --token file:\$HOME/.config/loongagent/client.token"
echo
echo "Keep $dir off any backup that leaves the machine: it holds the signing key."
