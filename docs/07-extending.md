# Extending loongagent

- **Up:** [extending/README.md](extending/README.md)
- **Authority:** [ADR-0004](decisions/0004-microkernel-extension-model.md) · [ADR-0007](decisions/0007-permission-sandbox-engine.md)
- **Code:** `crates/tools/src/lib.rs`, `crates/schema/protocol.toml`, `crates/hooks/src/run.rs`, `crates/config/src/profile.rs`
- **Verify:** `cargo test -p loong-tools -p loong-schema -p loong-hooks -p loong-config` · `./scripts/check.sh`

To change the machinery rather than run it, start at
[`docs/extending/`](extending/README.md): one file per procedure, each naming the file
that owns the mechanism, the order to touch things in, and one command that proves the
change; a hook is additionally trust-gated per repository.

## Where each procedure moved

| Old section | Where it moved |
|---|---|
| Adding a built-in tool | [extending/builtin-tool.md](extending/builtin-tool.md) |
| Adding an event | [extending/event.md](extending/event.md) |
| Attaching a hook | [extending/hook.md](extending/hook.md) |
| How routing picks a model | [extending/routing.md](extending/routing.md) |
| The four procedures as a layer | [extending/README.md](extending/README.md) |
