# Control plane — operate

- **Up:** [README.md](README.md)
- **Authority:** [ADR-0012](../decisions/0012-control-plane.md) · [ADR-0016](../decisions/0016-pack-distribution.md)
- **Code:** `crates/control/src/`, `crates/pack/src/lib.rs`, `crates/config/src/lib.rs`
- **Verify:** `cargo test -p loong-control -p loong-pack` · `./scripts/check.sh`

This is the day-to-day half of the [control-plane runbook](control-plane.md): serve
it, expose it safely, publish a new pack, refresh an install, and recover when a
message does not match what you expected. Standing it up is
[control-plane-bootstrap.md](control-plane-bootstrap.md); keys and what the
signature trusts are [control-plane-security.md](control-plane-security.md).

## Serving it

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

## 5b. When the first run already happened

`loongagent control status` answers "what is this install talking to, and what has it
verified?" — the machine file, the user file, the cached pack and whether it still
verifies. `loongagent control sync` refreshes the cache without starting a session.

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

**After an upgrade, restart it.** A running control plane holds the code it started
with, so new rules do not reach it until the process does. The repository ships the
installer that keeps the two copies of each binary in step, and prints the commit each
one was built from:

```bash
./scripts/install-local.sh          # installs both, prints both commits + repository HEAD
sudo systemctl restart loongagent-control
curl -s http://127.0.0.1:8738/v1/health | jq
```

If a restart seems to have no effect, check that it actually happened: a stale pid file
points at a process that is already gone. Ask the port who is listening —

```bash
ss -ltnp | grep 8738                # the pid that really owns it
```

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
