# Runbooks

- **Up:** [design/control-plane.md](../design/control-plane.md)
- **Authority:** [ADR-0012](../decisions/0012-control-plane.md) · [ADR-0016](../decisions/0016-pack-distribution.md)
- **Code:** `crates/control/src/`, `crates/pack/src/lib.rs`, `scripts/control-plane-init.sh`, `scripts/build-windows.ps1`
- **Verify:** `./scripts/check.sh`

Doing, not deciding: one line per runbook, and the document that owns the
decision or the mechanism is linked from each.

| Runbook | When you need it |
|---|---|
| [control-plane.md](control-plane.md) | The entry: where the runbook's files live, the three roles, and which of the next three to open. |
| [control-plane-bootstrap.md](control-plane-bootstrap.md) | Stand the control plane up: build, key and tokens, write the pack, start it, point installs at it, migrate an old layout. |
| [control-plane-operate.md](control-plane-operate.md) | Run it day to day: expose it, run it as a service, publish, refresh, day two and troubleshooting. |
| [control-plane-security.md](control-plane-security.md) | Decide what is trusted: the signing key, pack secrets, what the signature does not protect. |
| [windows-build.md](windows-build.md) | Build and run the agent on Windows, with the parity table and the real gaps. |
