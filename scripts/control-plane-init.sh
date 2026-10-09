#!/usr/bin/env bash
#
# Bring up a control plane on this machine: layout, signing key, tokens, and a
# starter profile pack. Run it on the server that will serve profiles, as the user
# that will run `loongagent-control serve` (root, in the usual deployment).
#
#   ./scripts/control-plane-init.sh --issuer acme-platform
#   ./scripts/control-plane-init.sh --issuer acme --dir /etc/loongagent/control \
#       --state /var/lib/loongagent/control
#
# It is deliberately a *setup* script, not a daemon: it writes files, prints the
# public key to pin, and tells you the two commands that come next (publish, serve).
set -euo pipefail

cd "$(dirname "$0")/.."

dir="/etc/loongagent/control"
state="/var/lib/loongagent/control"
issuer=""
port="8738"
bind="127.0.0.1"

while [ $# -gt 0 ]; do
  case "$1" in
    --dir) dir="$2"; shift 2 ;;
    --state) state="$2"; shift 2 ;;
    --issuer) issuer="$2"; shift 2 ;;
    --port) port="$2"; shift 2 ;;
    --bind) bind="$2"; shift 2 ;;
    -h|--help) sed -n '2,14p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

if [ -z "$issuer" ]; then
  echo "usage: $0 --issuer NAME [--dir DIR] [--state DIR] [--port N] [--bind IP]" >&2
  echo "  NAME is who signs the packs; installs show it (e.g. acme-platform)." >&2
  exit 2
fi

binary="target/release/loongagent-control"
if [ ! -x "$binary" ]; then
  echo "== building $binary"
  cargo build --release -p loong-control
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

echo "== writing a starter pack: $dir/profiles.toml"
cat > "$dir/profiles.toml" <<'TOML'
# The pack a profile administrator edits and publishes. Engineers only ever see
# `name` and `description`; endpoints and keys live here, on the control plane's
# side of the line (T-07).
#
#   publish:  loongagent-control publish --url http://127.0.0.1:8738 \
#               --pack /etc/loongagent/control/profiles.toml \
#               --token-file /etc/loongagent/control/admin.token --version 1
#
# `base_url` is wherever the model actually is: a provider, your own gateway, or a
# local server (vLLM, LM Studio, Ollama) on http://127.0.0.1:8000/v1.
# `api_key = "env:NAME"` is resolved *by the client*, so the secret stays on each
# machine; `gateway:` is not wired yet (ADR-0008 §5).
default_profile = "default"

[[profiles]]
name = "default"
description = "Balanced: the everyday profile. Edit this description — it is what engineers read."
  [[profiles.models]]
  name = "main"
  kind = "openai-compat"
  base_url = "http://127.0.0.1:8000/v1"
  model = "REPLACE-WITH-A-MODEL-ID"
  api_key = "env:LOONGAGENT_API_KEY"
  budget_tokens = 200000
  [profiles.router]
  default = "main"
TOML

echo
echo "control plane ready."
echo "  directory:     $dir"
echo "  served pack:   $state/profiles.json"
echo "  issuer:        $issuer"
echo "  key id:        $key_id"
echo "  public key:    $public_key"
echo
echo "1) edit the starter pack (base_url, model id, description):"
echo "     \$EDITOR $dir/profiles.toml"
echo
echo "2) start it (foreground first; a systemd unit is in the runbook):"
echo "     $binary serve --dir $state --key $dir/control-key.pkcs8 --issuer $issuer \\"
echo "       --client-token-file $dir/client.token --admin-token-file $dir/admin.token \\"
echo "       --bind $bind --port $port"
echo
echo "3) publish the draft (the control plane signs it with the key above):"
echo "     $binary publish --url http://127.0.0.1:$port --pack $dir/profiles.toml \\"
echo "       --token-file $dir/admin.token --version 1"
echo
echo "4) check what it is serving:"
echo "     $binary show --url http://127.0.0.1:$port --token-file $dir/client.token \\"
echo "       --public-key $public_key"
echo
echo "5) point an install at it (only the address, the pinned key, and a token):"
echo "     loongagent setup --url http://127.0.0.1:$port \\"
echo "       --public-key $public_key --token file:$dir/client.token"
echo
echo "Keep $dir off any backup that leaves the machine: it holds the signing key."
