# Extending loongagent

- **Up:** [extending/README.md](extending/README.md)
- **Authority:** [ADR-0004](decisions/0004-microkernel-extension-model.md) · [ADR-0007](decisions/0007-permission-sandbox-engine.md)
- **Code:** `crates/tools/src/lib.rs`, `crates/schema/protocol.toml`, `crates/hooks/src/run.rs`, `crates/config/src/profile.rs`
- **Verify:** `cargo test -p loong-tools -p loong-schema -p loong-hooks -p loong-config` · `./scripts/check.sh`

This is the maintainer's entry: how to change the machinery rather than run it. Each
procedure has one file under [`docs/extending/`](extending/README.md), named after the
thing being added. Every procedure names the file that owns the mechanism, the order to
touch things in, and one command that proves the change took effect. Symbols are named
rather than quoted and ADRs are linked rather than restated, because a copy drifts from
the thing it was copied out of. The hook procedure alone also passes a per-repository
trust gate: having a hook file is not running it.

## Where each procedure moved

| Old section | Where it moved |
|---|---|
| Adding a built-in tool — the `Tool` trait, action classes, registration, the permission declaration, the test | [extending/builtin-tool.md](extending/builtin-tool.md) |
| Adding an event — `protocol.toml`, `gen-schema.py`, the generated files, `--check`, the `ignorable` rule | [extending/event.md](extending/event.md) |
| Attaching a hook — `hooks.toml`, verdict combination in `crates/hooks/src/run.rs`, the trust gate | [extending/hook.md](extending/hook.md) |
| How routing picks a model — ADR-0005, `Router`/`RouterRule`/`parse_condition`, the provider layer | [extending/routing.md](extending/routing.md) |
| The four procedures as a layer — what it is, the trust/verification rule, links | [extending/README.md](extending/README.md) |
