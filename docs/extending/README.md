# Extending loongagent

- **Up:** [design/extension-model.md](../design/extension-model.md)
- **Authority:** [ADR-0004](../decisions/0004-microkernel-extension-model.md) · [ADR-0007](../decisions/0007-permission-sandbox-engine.md) · [ADR-0016](../decisions/0016-pack-distribution.md) · [ADR-0018](../decisions/0018-plugin-install-and-bootstrap.md)
- **Code:** `crates/tools/src/lib.rs`, `crates/schema/protocol.toml`, `crates/hooks/src/run.rs`, `crates/config/src/profile.rs`
- **Verify:** `cargo test -p loong-tools -p loong-schema -p loong-hooks -p loong-config` · `./scripts/check.sh`

The repository explains itself in layers, and this is the layer that was missing.
`README.md` is for someone installing it. [DEVELOPMENT.md](../../DEVELOPMENT.md) is for
running it, and says what is built today. `docs/decisions/` is *why* it has this shape.
This document is for changing it: each section names the file that owns a mechanism, the
order to touch things in, and one command that proves the change took effect. Symbols
are named rather than quoted, and the ADRs are linked rather than restated, for the same
reason — a copy drifts from the thing it was copied out of.

The four procedures each have one file:

| Procedure | File | Proof |
|---|---|---|
| Adding a built-in tool | [builtin-tool.md](builtin-tool.md) | `cargo test -p loong-tools registry_exposes_specs_for_the_model` |
| Adding an event | [event.md](event.md) | `./scripts/gen-schema.py --check` |
| Attaching a hook | [hook.md](hook.md) | `cargo test -p loong-hooks` |
| How routing picks a model | [routing.md](routing.md) | `cargo test -p loong-config the_router_takes_the_first_matching_rule` |

**The rule is the same in all four.** Name the file that owns the mechanism, give
the order to touch things in, and end with one command that proves the change
took effect. The ADRs are linked rather than restated, because a copy drifts from
the thing it was copied out of.

**Trust is a separate question from verification.** A hook is repository
configuration, not a crate: having a hook file is not running it. `HookSet::active`
returns nothing until the manifest and every script it names match the per-repository
trust record, and editing either re-arms the question. The other three procedures
extend code or protocol, so the gate and the compiler are the trust boundary; the
hook file is the one input a running session can gain without a rebuild, which is
why it gets a byte-level trust gate of its own
([ADR-0007 §7](../decisions/0007-permission-sandbox-engine.md)).
