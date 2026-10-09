# Control plane runbook

How to stand up the control plane, publish a profile pack, and point a local
install at it. Read ADR-0012 for what the control plane is *for* and ADR-0016 for
how packs are signed; this file is only the doing.

**Roles.** Three of them, separated by token and by which machine holds what:

| Role | Holds | Does |
|---|---|---|
| profile administrator | the admin token | writes `profiles.toml`, publishes drafts |
| control plane | the **signing key**, both tokens, the served pack | validates, signs, serves |
| every install (engineer) | a pinned public key + the client token | reads, verifies, runs a profile |

The signing key lives on the control plane (owner decision, 2026-10-09), so an
administrator needs no key material at all — only the admin token.

---

## 0. Build

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo build --release            # or: cargo install --path crates/control
```

## 1. Bootstrap

Run on the server that will serve profiles. Default layout follows FHS:

```bash
./scripts/control-plane-init.sh --issuer acme-platform
```

| Path | Mode | What |
|---|---|---|
| `/etc/loongagent/control/control-key.pkcs8` | `0600` | the signing key — this machine's most sensitive file |
| `/etc/loongagent/control/client.token` | `0600` | read token; every install gets a copy |
| `/etc/loongagent/control/admin.token` | `0600` | publish token; **only administrators** |
| `/etc/loongagent/control/profiles.toml` | `0644` | the draft pack the administrator edits |
| `/var/lib/loongagent/control/profiles.json` | `0644` | the signed pack being served |

The script prints the **public key** (to pin in every install) and refuses to run if
a key already exists — overwriting one would invalidate every install that pinned it.
It does not start anything.

## 2. Write the pack

Edit `/etc/loongagent/control/profiles.toml`. Engineers will see only `name` and
`description`; everything else stays here.

```toml
default_profile = "default"

[[profiles]]
name = "default"
description = "Balanced: the everyday profile; escalates to a long-context model past 150k tokens."
  [[profiles.models]]
  name = "main"
  kind = "openai-compat"
  base_url = "http://127.0.0.1:8000/v1"        # provider, your gateway, or a local server
  model = "llama-3.1-8b-instruct"              # the id your endpoint expects
  api_key = "env:LOONGAGENT_API_KEY"           # resolved on the *client*: the secret stays there
  budget_tokens = 200000
  [[profiles.models]]
  name = "long"
  kind = "openai-compat"
  base_url = "http://127.0.0.1:8000/v1"
  model = "llama-3.1-70b-instruct"
  api_key = "env:LOONGAGENT_API_KEY"
  [profiles.router]
  default = "main"
  [[profiles.router.rules]]                    # first match wins; the rule that fired is logged
  when = "context_tokens > 150000"
  use = "long"
```

Notes that save a round trip:

- `base_url` is the OpenAI-compatible base; `model` is whatever id that endpoint
  wants. A local server (vLLM, LM Studio, Ollama) is `http://127.0.0.1:8000/v1`.
- `api_key` must be a **reference** (`env:NAME`, `file:PATH`, `file:PATH#KEY`,
  `keychain:`, `gateway:`); a literal is refused. The client resolves it, so each
  machine keeps its own secret. `keychain:`/`gateway:` are recognised and refused
  with a reason — no credential store is wired yet (ADR-0008 §5).
- `description` is the only thing an engineer reads before choosing. Write it as a
  sentence about cost and behaviour, not about vendors.
- A name may not be a vendor (`fast`, `default`, `supersmart`) — that is the point
  of the layer (T-07).

## 3. Start it

Foreground first, so you see the startup line:

```bash
/usr/local/bin/loongagent-control serve \
  --dir /var/lib/loongagent/control \
  --key /etc/loongagent/control/control-key.pkcs8 \
  --issuer acme-platform \
  --client-token-file /etc/loongagent/control/client.token \
  --admin-token-file  /etc/loongagent/control/admin.token \
  --bind 127.0.0.1 --port 8738
```

```
loongagent-control serving 127.0.0.1:8738
  directory: /var/lib/loongagent/control
  issuer:    acme-platform
  key id:    684e44a30c89c781
  pack:      nothing published yet
  signing:   yes (drafts published to this plane are signed here)
```

`--key` is required: a control plane without a signing key can distribute but cannot
accept drafts, and it says so in `GET /v1/health` (`can_sign`). Passing
`--public-key` as well is optional and is a safety check — if it does not match the
key, the plane refuses to start rather than serving packs nobody could verify.

### Exposing it beyond loopback

The control plane speaks **plain HTTP**; there is no TLS listener in it yet. Clients
therefore refuse a non-loopback `http://` address (a pack decides where prompts go),
so pick one of:

- **loopback + SSH tunnel** (simplest for a first deployment):
  `ssh -N -L 8738:127.0.0.1:8738 cp.acme.internal`, then clients use
  `http://127.0.0.1:8738`;
- **a reverse proxy** terminating TLS (nginx/Caddy) in front of `127.0.0.1:8738`,
  with clients using `https://control.acme.internal`.

Do not bind `0.0.0.0` without one of those: the tokens would travel in the clear.

### As a service

```ini
# /etc/systemd/system/loongagent-control.service
[Unit]
Description=loongagent control plane (profile pack distribution)
After=network.target

[Service]
ExecStart=/usr/local/bin/loongagent-control serve \
  --dir /var/lib/loongagent/control \
  --key /etc/loongagent/control/control-key.pkcs8 \
  --issuer acme-platform \
  --client-token-file /etc/loongagent/control/client.token \
  --admin-token-file  /etc/loongagent/control/admin.token \
  --bind 127.0.0.1 --port 8738
User=loongagent
Group=loongagent
Restart=on-failure
NoNewPrivileges=true
ProtectSystem=strict
ReadWritePaths=/var/lib/loongagent/control
ProtectHome=true
PrivateTmp=true

[Install]
WantedBy=multi-user.target
```

The service user needs read access to the key and the tokens
(`chown loongagent:loongagent` + keep the `0600` modes) and write access to
`--dir`.

## 4. Publish, then verify

```bash
# the draft goes to the control plane, which validates and signs it
loongagent-control publish --url http://127.0.0.1:8738 \
  --pack /etc/loongagent/control/profiles.toml \
  --token-file /etc/loongagent/control/admin.token \
  --version 2026.10.09-1
#   published acme-platform 2026.10.09-1 (key 684e44a30c89c781)
#     profiles: default

# check what it is serving, verified against the key you will pin
loongagent-control show --url http://127.0.0.1:8738 \
  --token-file /etc/loongagent/control/client.token \
  --public-key <public key from step 1>
#   signature: verified under the key you gave
#   default profile: default
#     default — Balanced: the everyday profile; …
#         model main → llama-3.1-8b-instruct (http://127.0.0.1:8000/v1)
#         router default: main
```

`health` needs no token and answers the two questions worth asking in a monitoring
check: is a pack being served, and can this plane sign?

```bash
loongagent-control health --url http://127.0.0.1:8738
#   { "ok": true, "issuer": "acme-platform", "key_id": "684e…", "has_pack": true, "can_sign": true }
```

## 5. Point an install at it

On each machine — this is the **entire** configuration an engineer owns:

```bash
loongagent setup --url http://127.0.0.1:8738 \
  --public-key <public key from step 1> \
  --token file:/etc/loongagent/control/client.token
```

```
wrote ~/.config/loongagent/control.toml
verified pack: acme-platform 2026.10.09-1 (key 684e44a30c89c781), 1 profiles
  default
      Balanced: the everyday profile; …
      models: main
choose one with:  loongagent use <name>
```

```toml
# ~/.config/loongagent/control.toml — no models, no endpoints, no secrets
url = "http://127.0.0.1:8738"
public_key = "4c02c0415cb2715abf891d6f5981aa4f2d250924a030391ec89f4590ef032ade"
token = "file:/etc/loongagent/control/client.token"
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

## 6. Day two

**Change a profile**: edit `profiles.toml`, publish again with a higher `--version`.
Installs pick it up on their next start; a session already running keeps the pack it
verified. Verified behaviour: republishing `2026.10.09-2` made the next run use the
new model id with no client-side change.

**Installs do not expire packs** (ADR-0016 §7): a machine that never reconnects keeps
running the last pack it verified. There is no revocation with teeth against an
offline machine, and no `--force-expiry`. That is a decision, not an omission.

**Rotate the signing key**: `keygen` writes a new pair, installs must re-pin the new
public key (`loongagent setup` again). There is no automatic rotation, no overlap
window, and no revocation list yet — the envelope carries `key_id` and `version` for
exactly this, and the policy is unwritten.

**Backups**: back up `profiles.toml`; think twice about `control-key.pkcs8`. An
off-site copy of that file is an off-site ability to mint packs. The served
`profiles.json` is derived and can always be rebuilt by republishing.

**What leaves the machine**: nothing. The client reads `/v1/pack/profiles` and the
uplink (audit/statistics) is not built.

## 7. Troubleshooting

| Message | Meaning |
|---|---|
| `the admin token is required to publish` (HTTP 401) | the publish used the client token, or a wrong/absent one |
| `a client token is required` (HTTP 401) | the install's `token` reference resolved to nothing — check the file's path and mode |
| `the control plane has no profiles published yet` (409) | it is up, but nothing was published — step 4 |
| `the public key must be a 32-byte Ed25519 key in hex (N bytes given)` | the wrong value was pinned; the keygen output is 64 hex characters |
| `… is plain HTTP to another machine; … must travel over TLS` | non-loopback address without TLS — see “Exposing it beyond loopback” |
| `the pack is signed by key X, but this install pinned Y` | the signing key changed, or something else is answering that URL; re-pin deliberately or investigate |
| `this control plane has no signing key — start it with --key` | started without `--key`; it can serve, not author |
| `--public-key pins X but … is key Y — refusing to serve` | two different keys were configured; the served pack would be unverifiable |
| `the control plane is unreachable: …` then the run continues | the last verified pack was used — check `from_cache` in the log and the `[control]` line |
| TUI shows `not connected to a control plane` | no `control.toml` on this machine — run `loongagent setup` |
| `no model is configured yet` | a pack is configured but nothing selected, or no pack at all — `loongagent profiles`, then `use` |

## 8. What this does not protect against

Stated plainly so nobody infers more than the design claims:

- **A compromised control plane can mint packs** — it holds the signing key. That is
  the accepted cost of administrators not carrying key material.
- **A local administrator can edit anything on a machine**: the binary, the cached
  pack, `control.toml`. The signature protects the *wire* and the repository, not a
  root user with physical access.
- **The client token is a shared secret**, not identity: every install has the same
  one. Identity/SSO is ADR-0012's job and is not built.
- **No TLS in the control plane**: HTTPS is a reverse proxy or a tunnel today.
- **No revocation, no expiry, no rotation policy** (see day two).

## 9. Not built

Identity/SSO, the model gateway (central keys, metering, shared prompt cache), rollup
dashboards, the audit/statistics uplink, per-team scopes, push updates. Profile
distribution is the whole of it, on purpose (ADR-0016, ADR-0012 §5).
