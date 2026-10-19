# Control plane — security

- **Up:** [README.md](README.md)
- **Authority:** [ADR-0012](../decisions/0012-control-plane.md) · [ADR-0016](../decisions/0016-pack-distribution.md)
- **Code:** `crates/control/src/`, `crates/pack/src/lib.rs`, `crates/config/src/lib.rs`
- **Verify:** `cargo test -p loong-control -p loong-pack` · `./scripts/check.sh`

This is the trust half of the [control-plane runbook](control-plane.md): where the
signing key lives, what a pack may carry, what the tools refuse, and what the
signature does not protect. Standing it up is
[control-plane-bootstrap.md](control-plane-bootstrap.md); running it is
[control-plane-operate.md](control-plane-operate.md).

## Keys and secrets

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

### Shipping the credential in the pack (temporary, visible, retired for gateway mode)

Before identity and the model gateway exist, a firm may choose to send the credential
with the pack so that an install needs no local secret at all. Put the value in the
draft:

```toml
api_key = "sk-…"                       # inside the signed pack, in the clear
# api_key = "env:DEEPSEEK_API_KEY"     # what this becomes once identity arrives
```

> **Retired by [ADR-0005](../decisions/0005-routing-philosophy.md) for gateway mode.** A provider key in the pack is the exact leak gateway mode exists
> to remove: it lands on every install, in plaintext. The remaining use is the
> pre-gateway interregnum that ADR-0005's *Transition* note names; do not use it
> once a gateway is deployed. **Local mode never reads a pack** — the user brings
> their own key.

Everything says so out loud:

- `publish` prints a warning and the response carries it (`"warnings": […]`);
- `GET /v1/health` reports `"plaintext_secrets": 1`, so a monitor can see it;
- every client prints `[control] warning: … carries the secret itself` on load;
- `check --strict` exits 1, which is what a pre-commit hook in a configuration
  repository wants.

It lands in exactly three places — the draft, the control plane's `profiles.json`, and
each install's `~/.loongagent/control/<origin>/profiles.json` — and **not** in the
session log, the index, or `control.toml`. All three are plaintext files, so treat
backups of them as secrets. Local configuration files (project, user, session) still
refuse a literal; only the pack may carry one.

### Validate before you commit, not before you publish

`publish` refuses a draft that carries a value, but by then a `git commit` may already
have recorded it. So the same check is available offline, and it is worth wiring into
the repository that holds the draft:

```bash
loongagent-control check --pack profiles.toml     # exits 1 with the reason
```

```sh
# .git/hooks/pre-commit  (or your hook manager's equivalent)
loongagent-control check --pack profiles.toml || exit 1
```

`check` never contacts a control plane and never signs anything: it runs the same
validation the server would, on your machine, before the value has a chance to enter
the history.

### The draft is committed; the secret is not

This file is configuration: the intention is that it goes in a repository, gets
reviewed, and that a pack is what reviewers approve. Whether or not it is in a
repository *today*, it carries a **placeholder**, and the value lives in an
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
