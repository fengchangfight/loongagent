# Control plane runbook

- **Up:** [design/control-plane.md](../design/control-plane.md)
- **Authority:** [ADR-0012](../decisions/0012-control-plane.md) · [ADR-0016](../decisions/0016-pack-distribution.md)
- **Code:** `crates/control/src/`, `crates/pack/src/lib.rs`, `crates/config/src/lib.rs`, `scripts/control-plane-init.sh`
- **Verify:** `cargo test -p loong-control -p loong-pack` · `./scripts/check.sh`

How to stand up the control plane, publish a profile pack, and point a local
install at it. ADR-0012 says what the control plane is *for*, ADR-0016 how packs
are signed; these files are only the doing.

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

## Read it in this order

| When | File |
|---|---|
| Stand it up: build, key and tokens, write the pack, start it, point installs at it, migrate an old layout | [control-plane-bootstrap.md](control-plane-bootstrap.md) |
| Run it day to day: expose it, run it as a service, publish, refresh, recover | [control-plane-operate.md](control-plane-operate.md) |
| Decide what is trusted: the signing key, pack secrets, the security limits | [control-plane-security.md](control-plane-security.md) |

`scripts/control-plane-init.sh` writes the layout and prints the next commands.
