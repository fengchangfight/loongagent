# Control plane runbook

How to stand up the control plane, publish a profile pack, and point a local
install at it. ADR-0012 says what the control plane is *for*, ADR-0016 how packs
are signed; this file is only the doing.

**Two programs, two roots.** The local agent and the control plane share a
repository and a protocol, not a directory (ADR-0012 §1 — "the same code, two
binaries"). Each keeps its files where its kind of file belongs:

| Root | Program | Contents |
|---|---|---|
| `/etc/loongagent/` | **agent** (client) | `profile.toml` (the org layer), `control.toml` (address + pinned key, machine-wide) |
| `~/.loongagent/` | **agent** | one root: files directly in it are configuration (`profile.toml`, `control.toml`, `env`, `client.token`), its subdirectories are state (`projects/<project>/session.jsonl`, `control/<origin>/profiles.json`). `$LOONGAGENT_HOME` moves the whole thing |
| `<repository>/.loongagent/` | **agent** | project layer: `profile.toml`, `hooks.toml`, `mcp.toml`, `*.trusted`, `skills/` |
| `/etc/loongagent-control/` | **control plane** | `control-key.pkcs8` (signing key), `client.token`, `admin.token` — all `0600` |
| `/var/lib/loongagent-control/` | **control plane** | `profiles.json` — the signed pack it serves |

The split inside the control plane is FHS doing its job: `/etc` is what an
administrator authors and the service must read at boot; `/var/lib` is what the
service *writes* and can rebuild by republishing. A **draft pack is neither**: it is
versioned, PR-reviewable configuration (ADR-0008 §6), so it lives in the
administrator's repository and reaches the server only through `publish --pack`.

**Roles.** Separated by token and by which machine holds what:

| Role | Holds | Does |
|---|---|---|
| profile administrator | the admin token | writes `profiles.toml`, publishes drafts |
| control plane | the **signing key**, both tokens, the served pack | validates, signs, serves |
| every install | a pinned public key + the client token | reads, verifies, runs a profile |

The signing key lives on the control plane (owner decision, 2026-10-09), so an
administrator needs no key material — only the admin token. Do treat that token as
sensitive: whoever holds it can make the control plane sign a pack.

---

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

### The signing key must not be in a repository

A key in a repository is an off-site ability to mint packs that every install accepts,
so the tools check rather than trust:

- `keygen` **refuses to write a key inside a git work tree**, and names the layout to
  use instead. `--force` writes it and prints the `.gitignore` lines you now need.
- `scripts/control-plane-init.sh` refuses the same way (`FORCE=1` overrides) — it also
  writes the two tokens.
- `publish --pack` **refuses while a secret is tracked** in the repository holding the
  draft (`.env`, `client.token`, `*.pkcs8`, …), and prints the fix: `git rm --cached`,
  add it to `.gitignore`, and rotate it. A secret that is merely *untracked and
  unignored* gets a warning, because that is one `git add -A` from a leak.
- A **pack itself can only carry references** — a literal `api_key` is refused at
  publish time and again at load time — so the draft is safe to commit. That is the
  point of the split: the draft is configuration, the value is machine state.

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
  api_key = "env:DEEPSEEK_API_KEY"             # resolved on the *client*: the secret stays there
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

Notes that save a round trip:

- `base_url` is the OpenAI-compatible base, `model` is the id that endpoint wants. A
  local server (vLLM, LM Studio, Ollama) is `http://127.0.0.1:8000/v1`.
- `api_key` must be a **reference** — `env:NAME`, `file:PATH`, `file:PATH#KEY`,
  `keychain:`, `gateway:`. A literal is refused at the administrator's own
  `publish --pack`, again at the control plane, and again when a client loads the
  pack (ADR-0008 §5). The **name** is checked as well: `env:` must be followed by a
  variable name (`UPPER_SNAKE`), so pasting the secret after the prefix — which would
  otherwise travel inside the signed pack — is refused with the mistake spelled out. The reference is resolved **on the client**, so the secret
  never reaches the control plane and never enters a signed pack.

### The draft is committed; the secret is not

This file is configuration: it goes in a repository, it gets reviewed, and a pack is
what reviewers approve. So it carries a **placeholder**, and the value lives in an
environment file on each machine:

```toml
api_key = "env:DEEPSEEK_API_KEY"        # committed — a name, not a secret
```

The client reads environment files before resolving any reference, in this order:

| Order | File | Notes |
|---|---|---|
| 1 | whatever the shell already exported | always wins; a `.env` never overrides it |
| 2 | `~/.loongagent/env` | the recommended home for a secret; mode `0600` |
| 3 | `<workspace>/.env` | convenient when you work inside the repository that has one |

Both accept `NAME=value`, `NAME: value`, `export NAME=value` and quoted values. A
missing file is not an error. `file:PATH#KEY` remains available when a machine keeps
its secret somewhere else entirely — but prefer a name over a path in anything you
commit.

`.gitignore` in this repository ignores `.env` and `.env.*`; if you keep the secret in
a repository of your own, make sure the same is true there. **A pack never contains
the value**: `api_key` travels as the reference string, so `profiles.json` on the
control plane and in a client's cache holds `env:DEEPSEEK_API_KEY`, not a key.
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

### Exposing it beyond loopback

The control plane speaks **plain HTTP**; there is no TLS listener in it yet. Clients
refuse a non-loopback `http://` address (a pack decides where prompts go), so pick
one of:

- **loopback + SSH tunnel**: `ssh -N -L 8738:127.0.0.1:8738 cp.acme.internal`, then
  clients use `http://127.0.0.1:8738`;
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
  --dir /var/lib/loongagent-control \
  --key /etc/loongagent-control/control-key.pkcs8 \
  --issuer acme-platform \
  --client-token-file /etc/loongagent-control/client.token \
  --admin-token-file  /etc/loongagent-control/admin.token \
  --bind 127.0.0.1 --port 8738
User=loongagent
Group=loongagent
Restart=on-failure
NoNewPrivileges=true
ProtectSystem=strict
ReadWritePaths=/var/lib/loongagent-control
ProtectHome=true
PrivateTmp=true

[Install]
WantedBy=multi-user.target
```

The service user needs read access to the key and the tokens
(`chown loongagent:loongagent`, modes stay `0600`) and write access to `--dir`.

## 4. Publish, then verify

```bash
# from your configuration repository; the control plane validates and signs it
loongagent-control publish --url http://127.0.0.1:8738 \
  --pack ~/config/loongagent/profiles.toml \
  --token-file /etc/loongagent-control/admin.token \
  --version 2026.10.09-1
#   published acme-platform 2026.10.09-1 (key 684e44a30c89c781)
#     profiles: default

# what is it serving, verified against the key you will pin?
loongagent-control show --url http://127.0.0.1:8738 \
  --token-file /etc/loongagent-control/client.token \
  --public-key <public key from step 1>
#   signature: verified under the key you gave
```

`health` needs no token and answers the two questions worth monitoring:

```bash
loongagent-control health --url http://127.0.0.1:8738
#   { "ok": true, "issuer": "acme-platform", "key_id": "684e…", "has_pack": true, "can_sign": true }
```

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

## 5b. When the first run already happened

`loongagent control status` answers "what is this install talking to, and what has it
verified?" — the machine file, the user file, the cached pack and whether it still
verifies. `loongagent control sync` refreshes the cache without starting a session.

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

## 7. Day two

**Change a profile**: edit the pack in your repository, publish again with a higher
`--version`. Installs pick it up on their next start; a session already running keeps
the pack it verified.

**Installs do not expire packs** (ADR-0016 §7): a machine that never reconnects keeps
running the last pack it verified. There is no revocation with teeth against an
offline machine, and no `--force-expiry`. That is a decision, not an omission.

**Rotate the signing key**: `keygen` writes a new pair; every install must re-pin the
new public key. There is no automatic rotation, no overlap window, and no revocation
list yet — the envelope carries `key_id` and `version` for exactly this, and the
policy is unwritten. Rotating a **token** is cheap by comparison: replace the file
and restart the service.

**Backups**: back up the pack in your repository. Think twice about
`/etc/loongagent-control/control-key.pkcs8` — an off-site copy of that file is an
off-site ability to mint packs. The served `profiles.json` is derived; republishing
rebuilds it.

**What leaves the machine**: nothing. The client reads `/v1/pack/profiles` and the
uplink (audit/statistics) is not built.

**Where an `api_key` reference points**: a pack may say
`file:/home/alice/project/.env#DEEPSEEK_API_KEY` and the file exists per machine.
Keep that file `0600`: it is a secret, and its directory listing does not make it one
(`umask 002` gives `-rw-rw-r--`).

## 8. Troubleshooting

| Message | Meaning |
|---|---|
| `the admin token is required to publish` (HTTP 401) | the publish used the client token, or a wrong/absent one |
| `a client token is required` (HTTP 401) | the install's `token` reference resolved to nothing |
| `cannot read …: Permission denied` | the token reference points into a root-only directory; copy the client token out (step 5) |
| `the control plane has no profiles published yet` (409) | it is up, but nothing was published — step 4 |
| `the public key must be a 32-byte Ed25519 key in hex (N bytes given)` | the wrong value was pinned; keygen prints 64 hex characters |
| `… is plain HTTP to another machine; … must travel over TLS` | non-loopback address without TLS — see “Exposing it beyond loopback” |
| `the pack is signed by key X, but this install pinned Y` | the signing key changed, or something else is answering that URL |
| `the machine-wide file needs root: try with sudo` | `setup --system` without root; drop the flag for a per-user file |
| `this control plane has no signing key — start it with --key` | started without `--key`; it can serve, not author |
| `--public-key pins X but … is key Y — refusing to serve` | two different keys configured; the served pack would be unverifiable |
| `the control plane is unreachable: …` then the run continues | the last verified pack was used — check `from_cache` in the log |
| TUI shows `not connected to a control plane` | no `control.toml` on this machine — run `loongagent setup` |
| `no model is configured yet` | a pack is configured but nothing selected, or no pack at all — `loongagent profiles`, then `use` |

## 9. What this does not protect against

- **A compromised control plane can mint packs** — it holds the signing key. So can
  anyone holding the admin token. That is the accepted cost of administrators not
  carrying key material.
- **A local administrator can edit anything on a machine**: the binary, the cached
  pack, `control.toml`. The signature protects the *wire* and the repository, not a
  root user with physical access.
- **The client token is a shared secret**, not identity: every install has the same
  one. Identity/SSO is ADR-0012's job and is not built.
- **No TLS in the control plane**: HTTPS is a reverse proxy or a tunnel today.
- **No revocation, no expiry, no rotation policy** (see day two).

## 10. Not built

Identity/SSO, the model gateway (central keys, metering, shared prompt cache), rollup
dashboards, the audit/statistics uplink, per-team scopes, push updates. Profile
distribution is the whole of it, on purpose (ADR-0016, ADR-0012 §5).
