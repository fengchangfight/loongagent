# Attaching a hook

- **Up:** [README.md](README.md)
- **Authority:** [ADR-0004](../decisions/0004-microkernel-extension-model.md) · [ADR-0007](../decisions/0007-permission-sandbox-engine.md)
- **Code:** `crates/hooks/src/config.rs`, `crates/hooks/src/run.rs`, `crates/kernel/src/turn.rs`
- **Verify:** `cargo test -p loong-hooks` · `./scripts/check.sh`

A hook is repository configuration, not a crate: a manifest at
`<workspace>/.loongagent/hooks.toml` and, for a shell hook, a script beside it.
`crates/hooks/src/config.rs` owns the manifest (`HookSet::load`), the moments
(`HookEvent`) and the kinds (`HookKind`); `crates/hooks/src/run.rs` fires them. One
verdict contract covers every kind: a JSON object
`{"verdict": "allow" | "ask" | "deny", "reason": "…"}` on stdout, or a shell exit code —
zero allows, two denies with stderr as the reason. A hook that cannot answer falls back
to its own `on_error`, which defaults to `deny`; a guard that silently evaporates is the
outcome that default exists to prevent
([ADR-0007 §7](../decisions/0007-permission-sandbox-engine.md)).

Order:

1. Write `.loongagent/hooks.toml`. A shell script is named relative to the hooks
   directory and is read by the shell, so it need not be executable; an `http` hook is
   refused unless the manifest sets `allow_http_hooks = true`; an `mcp` hook names a
   tool on a server from `mcp.toml`.
2. `loongagent hooks list --cwd .` — it reports the set as INERT and names the files
   nobody has trusted yet.
3. Read those files, then run `loongagent hooks trust --cwd .`, which records their
   hashes in `.loongagent/hooks.trusted`.

**Having a hook file is not running it.** A hook is trust-gated per repository:
`HookSet::active` returns nothing unless the manifest and every script it names match the
trust record, and editing either re-arms the question, because the record is a change
detector bound to bytes rather than a signature. `hooks.trusted` is gitignored
(`.loongagent/*.trusted` in `.gitignore`) deliberately — a repository that could ship its
own trust record would defeat the gate it exists to be.

The proof that it ran, not merely that it was trusted: every answer is a `hook.fired`
event, whether or not it changed the outcome (`Kernel::fire_hooks`), so
`grep hook.fired <session>.jsonl` shows the hook, its verdict and its reason.

Rougher than a recipe suggests: only the manifest and the shell scripts it names are
fingerprinted (`sources_of`). An `http` or `mcp` hook has no local bytes to pin, so
"trusted" covers the configuration that points at the behaviour, not the behaviour.
Verdicts combine monotonically — deny over ask over allow (`strictest`) — and can only
tighten the PDP's ruling (`tighten`), so a hook may advise or veto but never *be* the PDP
([ADR-0007 §1](../decisions/0007-permission-sandbox-engine.md)).
