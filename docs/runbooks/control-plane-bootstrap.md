# Control plane — bootstrap

- **Up:** [README.md](README.md)
- **Authority:** [ADR-0012](../decisions/0012-control-plane.md) · [ADR-0016](../decisions/0016-pack-distribution.md)
- **Code:** `crates/control/src/`, `crates/pack/src/lib.rs`, `crates/config/src/lib.rs`
- **Verify:** `cargo test -p loong-control -p loong-pack` · `./scripts/check.sh`

This is the stand-it-up half of the [control-plane runbook](control-plane.md): one
machine serves a signed profile pack, and each engineer's install points at it.
Do this once, in order. Day-to-day serving, republishing and troubleshooting are in
[control-plane-operate.md](control-plane-operate.md); the signing key, the pack's
secret handling and the trust limits are in
[control-plane-security.md](control-plane-security.md).

## From nothing to a synced profile

The whole sequence, with the paths this runbook uses. Steps 1–4 are on the machine
that serves profiles; step 5 is on each engineer's machine.

```bash
# 1. the control plane: key, tokens, and a starter draft where you keep configuration
./scripts/control-plane-init.sh --issuer acme-platform \
  --dir /etc/loongagent-control --state /var/lib/loongagent-control \
  --starter ~/config/loongagent/profiles.toml
#   prints the public key to pin — keep that line

# 2. edit the draft: base_url, the model id from your provider's docs, a description,
#    and `api_key = "env:NAME"` (a placeholder: the value lives on each client)

# 3. start it
sudo /usr/local/bin/loongagent-control serve \
  --dir /var/lib/loongagent-control \
  --key /etc/loongagent-control/control-key.pkcs8 --issuer acme-platform \
  --client-token-file /etc/loongagent-control/client.token \
  --admin-token-file  /etc/loongagent-control/admin.token --port 8738

# 4. publish, and check what it is serving
loongagent-control publish --url http://127.0.0.1:8738 \
  --pack ~/config/loongagent/profiles.toml \
  --token-file /etc/loongagent-control/admin.token --version 1
loongagent-control show --url http://127.0.0.1:8738 \
  --token-file /etc/loongagent-control/client.token --public-key <hex>
```

Then on an engineer's machine there are two ways, and they do the same thing:

```bash
# 5a. just start the UI: the first run asks for the address, the key and a token
loongagent tui

# 5b. or configure it non-interactively (an image, MDM, a provisioning script)
loongagent setup --url http://127.0.0.1:8738 --public-key <hex> \
  --token file:/etc/loongagent-control/client.token

# then, either way:
loongagent profiles          # what the control plane offers, and what is selected
loongagent use default       # the one profile-related choice a user makes
loongagent tui               # no flags: the pack decides the model
```

The first run of `tui` draws a three-field form (address, public key, token
reference) in the terminal, verifies a pack before saving anything, and writes
`~/.loongagent/control.toml`. Press Esc to skip it: a session without a control plane
is a legitimate state.

## 0. Build

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo build --release            # or: cargo install --path crates/control
```

## 1. Bootstrap

```bash
./scripts/control-plane-init.sh --issuer acme-platform \
  --starter ~/config/loongagent/profiles.toml
```

| Path | Mode | What |
|---|---|---|
| `/etc/loongagent-control/control-key.pkcs8` | `0600` | the signing key — the most sensitive file here |
| `/etc/loongagent-control/client.token` | `0600` | read token; every install gets a copy |
| `/etc/loongagent-control/admin.token` | `0600` | publish token; **only administrators** |
| `~/config/loongagent/profiles.toml` | — | the draft, which you keep in git |

The script prints the **public key** (to pin in every install) and refuses to run if
a key already exists — overwriting one would invalidate every install that pinned it.
It does not start anything.

## 2. Write the pack

Edit the starter (`~/config/loongagent/profiles.toml`). Engineers see only `name`
and `description`; everything else stays here.

```toml
default_profile = "default"

[[profiles]]
name = "default"
description = "Balanced: the everyday profile; escalates to a long-context model past 150k tokens."
  [[profiles.models]]
  name = "main"
  kind = "openai-compat"
  base_url = "https://api.deepseek.com/v1"     # provider, your gateway, or a local server
  model = "deepseek-flash"                     # the id that endpoint expects
  api_key = "env:DEEPSEEK_API_KEY"             # interregnum: resolved on the client — see Mode check
  budget_tokens = 128000
  [[profiles.models]]
  name = "long"
  kind = "openai-compat"
  base_url = "https://api.deepseek.com/v1"
  model = "deepseek-v4-pro"
  api_key = "env:DEEPSEEK_API_KEY"
  [profiles.router]
  default = "main"
  [[profiles.router.rules]]                    # first match wins; the rule that fired is logged
  when = "context_tokens > 150000"
  use = "long"
```

**Mode check (ADR-0005).** In **gateway mode** a profile's models
point at the gateway and carry no provider key on the client
(`api_key = "gateway:…"`); the gateway picks the model, meters, and refuses over
budget. The direct-provider example above — `base_url` at the provider, key
resolved locally — is the pre-gateway interregnum form: what the pack path
supports today, visible, and what ADR-0005 retires. **Local mode** never reads a
pack; it uses the user's own key with no control plane.

Notes that save a round trip:

- `base_url` is the OpenAI-compatible base, `model` is the id that endpoint wants. A
  local server (vLLM, LM Studio, Ollama) is `http://127.0.0.1:8000/v1`.
- `api_key` must be a **reference** — `env:NAME`, `file:PATH`, `file:PATH#KEY`,
  `keychain:`, `gateway:`. A literal is refused at the administrator's own
  `publish --pack`, again at the control plane, and again when a client loads the
  pack (ADR-0008 §5). The **name** is checked as well: `env:` must be followed by a
  variable name (`UPPER_SNAKE`), so pasting the secret after the prefix — which would
  otherwise travel inside the signed pack — is refused with the mistake spelled out. The reference is resolved **on the client**, so the secret
  never reaches the control plane and never enters a signed pack. That is the
  pre-gateway form; in gateway mode a `gateway:` reference is resolved **at the
  gateway** and the client holds nothing (ADR-0005).

- `description` is the only thing an engineer reads before choosing. Write it about
  cost and behaviour.
- Name profiles by capability (`default`, `fast`, `supersmart`), never by vendor —
  that is the point of the layer (T-07).

## 3. Start it

Foreground first, so you see the startup line:

```bash
/usr/local/bin/loongagent-control serve \
  --dir /var/lib/loongagent-control \
  --key /etc/loongagent-control/control-key.pkcs8 \
  --issuer acme-platform \
  --client-token-file /etc/loongagent-control/client.token \
  --admin-token-file  /etc/loongagent-control/admin.token \
  --bind 127.0.0.1 --port 8738
```

```
loongagent-control serving 127.0.0.1:8738
  directory: /var/lib/loongagent-control
  issuer:    acme-platform
  key id:    684e44a30c89c781
  pack:      nothing published yet
  signing:   yes (drafts published to this plane are signed here)
```

`--key` is required: a plane without a signing key can distribute but cannot accept
drafts, and says so in `GET /v1/health` (`can_sign`). Passing `--public-key` as well
is optional and is a safety check — if it does not match the key, the plane refuses
to start rather than serving packs nobody could verify.

## 5. Point an install at it

Two shapes, and they compose:

```bash
# machine-wide (root): the address and the pinned key, delivered with the binary by
# an image or configuration management. The token reference points into the
# root-only directory, which is right when every user reads the same token.
sudo loongagent setup --system --url http://127.0.0.1:8738 \
  --public-key <public key from step 1> \
  --token file:/etc/loongagent-control/client.token

# per user: each install keeps its own copy of the read token (mode 0600)
install -m 0600 /etc/loongagent-control/client.token ~/.loongagent/client.token
loongagent setup --url http://127.0.0.1:8738 \
  --public-key <public key from step 1> \
  --token file:~/.loongagent/client.token
```

When both files exist, **the machine file decides the address and the key, and the
user file may only supply the token** — a user who could re-pin the key would make
the pin advisory; a user who could not supply a token would need to read a root-only
file. `loongagent control status` shows which files are in play.

The result is the **entire** configuration an engineer owns:

```toml
# ~/.loongagent/control.toml — no models, no endpoints, no secrets
url = "http://127.0.0.1:8738"
public_key = "4c02c0415cb2715abf891d6f5981aa4f2d250924a030391ec89f4590ef032ade"
token = "file:/home/me/.loongagent/client.token"
```

Then:

```bash
loongagent profiles        # the bundles, with descriptions; the selected one is marked
loongagent use default     # the one profile-related choice a user makes
loongagent tui             # or: web | repl | run "…"
```

The session says where its policy came from, and the log records it:

```
[control] acme-platform 2026.10.09-1 (key 684e44a30c89c781) — profile: default — Balanced: …
event control.synced    {"issuer":"acme-platform","version":"2026.10.09-1","key_id":"684e…","profile":"default","from_cache":false}
event profile.selected  {"profile":"default","description":"Balanced: …","models":"main"}
```

## 6. Migration

Layouts before 2026-10-09 put the control plane under `/etc/loongagent/control` (and
the served pack under `/var/lib/loongagent/control`), sharing `/etc/loongagent` with
the agent, with the draft kept as a system file. Moving is one `mv` per root:

A layout from before 2026-10-09 put the control plane under `/etc/loongagent/control`
(sharing `/etc/loongagent` with the agent, draft included as a system file), and the
client's own files under `~/.config/loongagent` and `~/.local/state/loongagent`.

```bash
# --- the control plane, on the server ---
sudo install -d -m 0700 /etc/loongagent-control /var/lib/loongagent-control
sudo mv /etc/loongagent/control/control-key.pkcs8 \
        /etc/loongagent/control/client.token \
        /etc/loongagent/control/admin.token /etc/loongagent-control/
sudo mv /var/lib/loongagent/control/profiles.json /var/lib/loongagent-control/
# the draft belongs in your configuration repository, not in /etc:
sudo install -o "$USER" -m 0644 /etc/loongagent/control/profiles.toml \
  ~/config/loongagent/profiles.toml
sudo rmdir /etc/loongagent/control /var/lib/loongagent/control

# --- the client, on each machine: one root now ---
mkdir -p ~/.loongagent
mv ~/.config/loongagent/{profile.toml,control.toml,env,client.token} ~/.loongagent/ 2>/dev/null
mv ~/.local/state/loongagent/projects ~/.loongagent/ 2>/dev/null
mv ~/.local/state/loongagent/control  ~/.loongagent/ 2>/dev/null
rmdir ~/.config/loongagent ~/.local/state/loongagent 2>/dev/null

# then start it with the new paths (step 3) — the key is unchanged, so no install
# needs to re-pin anything.
```

Nothing else changes: the pack format, the key, and the client configuration are all
unaffected by where the server keeps its files.
