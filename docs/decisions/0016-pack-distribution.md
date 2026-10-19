# ADR-0016 — Pack distribution: one envelope, verified before it can be policy

- **Status:** Accepted
- **Up:** [../design/README.md](../design/README.md)
- **Date:** 2026-10-09
- **Decides:** ADR-0008 §6's deferred items (signature algorithm, key
  distribution, pack transfer) and ADR-0012 §1's "share the envelope and its
  sign/verify path"; also the client half of T-07 (profiles as named bundles
  that arrive from the control plane)
- **Builds on:** ADR-0005 (routing follows the keys: local mode has no router,
  gateway mode routes server-side), ADR-0008 (layers, merge kinds,
  signed packs), ADR-0012 (the control plane is small and pushes only packs)

## Context

ADR-0008 §6 fixed the *envelope* and the rule — a pack must verify before it can
occupy the org layer, and an unsigned artifact can never be org policy — and left
three things open: the signature algorithm, key distribution, and transport.
ADR-0012 §5 kept exactly one distribution job for the control plane: "policy &
profile pack distribution". T-07 decided that engineers pick a **named profile**
(`default`, `supersmart`), never a vendor or endpoint.

None of it was implemented. In its place the local configuration grew
`[[models]]`, which is precisely the thing T-07 exists to prevent: it puts
endpoint and key choices in a file a user edits.

## Decision

### 1. One envelope, one crate, two binaries

`crates/pack` holds the envelope and its sign/verify path, and is a normal
dependency of both the local agent and the control plane. ADR-0012 §1 makes this
sharing correctness-critical rather than tidiness: the two sides deploy
independently, so a duplicated verifier "rejects valid packs or accepts invalid
ones". Sharing the crate makes agreement a compiler-enforced property.

```jsonc
{
  "artifact":       "loongagent.profiles",   // what this is
  "payload_schema": "loongagent.profiles/1", // how to read the payload
  "version":        "2026.10.09-1",          // semantic, admin-authored
  "issuer":         "acme-platform",         // who signed it; shown to users
  "scope":          "org",                   // who it is for
  "issued_at_ms":   1791500000000,
  "key_id":         "9f2c1ab4e5d60718",      // sha256 of the public key, truncated
  "payload":        "…",                     // the artifact itself (TOML text)
  "signature":      "…"                      // ed25519 over the canonical bytes
}
```

The signed bytes are canonical JSON of every field **except** `signature`, in a
fixed order, with no insignificant whitespace. `signature` and `key_id` are
excluded from what they authenticate; everything else is covered, including the
version and the scope, so a pack cannot be replayed as another artifact.

### 2. Ed25519 (via `ring`), and the control plane holds the key

Ed25519 through `ring` — already in the tree for TLS, so no new cryptography family
arrives, and `ring` is the implementation we already trust for the transport. The
private key is PKCS#8 (`ring`'s native input), written 0600.

**Owner decision, 2026-10-09: the control plane holds the signing key.** An
administrator publishes a *draft* and the control plane validates and signs it, so
the private key never has to be copied to an administrator's workstation and no
signing step depends on one person's laptop. The alternative — an administrator
signing offline and uploading a finished envelope — was built first and remains
supported (`publish --file`, `sign`) for an air-gapped administrator, but it is not
the normal path: it puts the key wherever the administrator works.

The trade is named rather than hidden: **a compromised control plane can mint
packs**, where offline signing would have kept that power on a workstation. What
still holds: the key file is 0600 and never leaves the server; a control plane
started with `--public-key` refuses to run if the key does not derive it; and
clients refuse anything not signed by the key they pinned. `GET /v1/health` reports
`can_sign`, so "can this plane author?" is observable.

`key_id` is a truncated SHA-256 of the public key so rotation is possible without
breaking older clients.

### 3. Key distribution: pinned out of band, never learned from the wire

The org's **public key is pinned in the client's machine configuration**, by the
same channel that installs the binary (image, MDM, Ansible). A client may fetch a
server's key for convenience, but only through an explicit
`--trust-server-key` that records the fingerprint in the configuration, and a
later mismatch is a hard failure — never a silent re-pin. Auto-trust would make
the signature decorative: whoever can answer the URL could then sign.

### 4. Transport: HTTPS, or plain HTTP on loopback

The pack travels as `GET {base}/v1/pack/profiles` with `Authorization: Bearer`.
HTTPS (verified against the system roots, as the model adapters already do) is
required for any non-loopback address; `http://` is accepted for loopback, where
the traffic cannot leave the machine. Publishing is a `POST` to
`/v1/admin/publish` guarded by a **separate** admin token, so a client token -
which every install has - cannot change policy.

### 5. The verified pack is the control layer, and lower layers cannot rewrite it

A verified pack becomes a new layer, `control`, ranked **immediately above
`org`** so that a managed install's policy wins over a stale local org file, and
far above project/user/session.

Its keys are **locked**:

- a lower layer that tries to set a locked key is an **error naming the control
  plane** (issuer, version), not a silent override;
- a `narrow-only` key may still be narrowed (an org ceiling stays a ceiling);
- an `add-only` key may still be added to (extra skill directories, say).

This is what makes "engineers pick a profile, not a vendor" enforceable rather
than advisory: the endpoint and the routing table come from the pack, and the one
profile-related key the user owns is *which profile* they run. This is the
pre-gateway form; in gateway mode the gateway chooses the model and its routing
policy stays server-side (ADR-0005). T-07's escape hatches therefore stay a
profile-layer feature (an admin publishes a profile for it), not a config-layer
loophole.

### 6. A profile is a named bundle, and the payload may carry several

The pack payload is admin-authored TOML:

```toml
default_profile = "default"

[[profiles]]
name = "default"
description = "Balanced: a cheap model for the loop, escalating past 150k tokens."
  [[profiles.models]] …      # endpoints and key references live here
  [profiles.router] …        # the routing table travels with the profile (pre-gateway form)

[[profiles]]
name = "supersmart"
description = "Frontier model throughout; slower and several times the cost."
```

The client stores one local key — `control.profile` — and the resolver expands
the chosen bundle into the control layer. `description` is not decoration: it is
what the user is shown instead of a model list (T-07's "declare intent").

### 7. A pack does not expire

**Owner decision, 2026-10-09: no expiry, no maximum age.** A machine that never
reaches the control plane again keeps running the last pack it verified, for as long
as it exists. The consequence is recorded rather than mitigated: revocation has no
teeth against a machine that stays offline, and a rotated-out pack keeps working
there. That is acceptable for a firm-internal tool whose failure mode is "an
engineer's old laptop keeps using last quarter's profile", and it is the cost of
a pack that never expires — the machine that never reconnects is exactly the one
revocation cannot reach.

If that trade ever changes, the mechanism is already in the envelope (`issued_at_ms`,
`version`) — the decision, not the plumbing, is what would have to change.

### 8. Offline policy keeps working

The last verified pack is cached under the state directory and used when the
control plane is unreachable, so a disconnected machine still runs against its
last verified policy — in **gateway mode** the model call is the one thing that
needs the gateway (ADR-0005 §4), and local mode never reads a pack at all.
Because the cache is only ever written after verification, being offline cannot
lower the policy standard. Every session records `control.synced` (issuer,
version, key id, verified) as its first-class provenance, which is the local half
of ADR-0012's "policy is auditable".

## Consequences

- **Distribution exists, the rest of ADR-0012's job list does not.** Identity/SSO,
  the model gateway, and rollup dashboards remain unbuilt, and this ADR does not
  pretend otherwise; a solo install still runs with no control plane at all.
- **The control plane is deliberately one endpoint plus one admin endpoint.** No
  session data, no uplink: the client only ever *reads*, which keeps ADR-0012 §3's
  allow-list easy to defend.
- **A local `[[models]]` file remains possible** for unmanaged and development
  installs (an empty org layer is the v1 story, ADR-0012 Consequences). It is no
  longer the documented path, and `loongagent setup` no longer writes one: setup
  asks for a control-plane address.
- **Scope, reaffirmed 2026-10-09:** this ADR covers **profile distribution only**.
  Identity/SSO, the model gateway, rollups/dashboards and the uplink stay in
  ADR-0012's deferred list, to be built later and separately; the client continues to
  only ever *read*.
- **Deferred:** key rotation procedure, revocation, per-team scopes, transport
  cadence (poll vs push), the uplink (audit/statistics batches), and signed
  policy/context payloads — the envelope is built for them, the payload schemas are
  not written.
