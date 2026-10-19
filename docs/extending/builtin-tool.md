# Adding a built-in tool

- **Up:** [README.md](README.md)
- **Authority:** [ADR-0004](../decisions/0004-microkernel-extension-model.md) · [ADR-0007](../decisions/0007-permission-sandbox-engine.md)
- **Code:** `crates/tools/src/lib.rs`, `crates/kernel/src/turn/build.rs`, `crates/kernel/src/turn/loop.rs`, `crates/sandbox/src/lib.rs`, `crates/schema/protocol.toml`
- **Verify:** `cargo test -p loong-tools registry_exposes_specs_for_the_model` · `./scripts/check.sh`

A tool is one implementation of the `Tool` trait in `crates/tools/src/lib.rs`:

- `name`, `description` and `parameters` — what the model is shown; `parameters` is a
  JSON Schema;
- `action_class(args, cwd)` — the autonomy this *call* gets, computed from the call's
  own arguments rather than fixed on the tool
  ([ADR-0007 §4](../decisions/0007-permission-sandbox-engine.md));
- `execute(args, ctx)` — the work, with the workspace and the process confinement in
  `ExecContext`.

Order:

1. Implement `Tool` beside the existing ones — `crates/tools/src/lib.rs`, or
   `crates/tools/src/search.rs` when it wants the shared search helpers.
2. Register it in `Registry::with_builtins`. For a tool that acts on the workspace or
   the shell, that is the one list it is added to: `Kernel::build`
   (`crates/kernel/src/turn/build.rs`) starts from `with_builtins` and then adds the tools
   that come from elsewhere (`McpTool`, the skills tool). `recall` is the exception, and
   the roughness paragraph below says why it is a real second path rather than a
   workaround.
3. Choose the `ActionClass` it declares. `Kernel::dispatch` (`crates/kernel/src/turn/loop.rs`)
   reads that declaration and asks `Pdp::decide` (`crates/sandbox/src/lib.rs`) for a
   verdict; the classes themselves are declared in `crates/schema/protocol.toml`.
4. If it writes, refuse an out-of-workspace target inside `execute` as well — see
   `WriteFile::execute`. The PDP decides and the tool refuses; neither replaces the
   other.

The proof: `cargo test -p loong-tools registry_exposes_specs_for_the_model`. That test
pins the catalogue's names, so a new tool fails it until the expectation is updated; the
failure is the reminder that the catalogue is a fixed surface rather than an accident.
`Kernel::tool_specs` is then what carries the catalogue into the request, which is the
point at which a registered tool becomes one the model can call.

Rougher than a recipe suggests. The first snag is the action class: it is code, not a
declaration, so nothing checks that it matches what the tool can do. A read-only tool
classified as a workspace write costs an approval it does not need; a write tool
classified as an investigation runs without one, and only a test or a live turn catches
either. `classify_command` in the same file is the precedent to read before writing one —
it is an autonomy heuristic, never a security boundary.

The second snag is `recall`, which lives outside the registry entirely. It needs session
state — a `Tool` impl gets only the workspace and the confinement in `ExecContext` — so
the kernel owns it: `tool_specs` appends its spec when a context engine exists, and
`dispatch` answers `RECALL_TOOL` *before* the registry lookup and *without asking the
PDP*. That bypass is deliberate, not an oversight: a recall is read-only over the
session's own record, which the model has already been given in full, so there is nothing
to gate. A tool that needs state the trait cannot carry is built the same way — answered
in `dispatch`, with its own `tool.call` and `tool.result` written by hand and a comment
saying why the registry and the PDP are the wrong gates. It is the wrong gate only for a
read-only view of data the session already holds; anything that touches the workspace, the
shell or the network still belongs in the registry and behind `Pdp::decide`.
