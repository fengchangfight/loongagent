# Extending loongagent

The repository explains itself in layers, and this is the layer that was missing.
`README.md` is for someone installing it. [DEVELOPMENT.md](../DEVELOPMENT.md) is for
running it, and says what is built today. `docs/decisions/` is *why* it has this shape.
This document is for changing it: each section names the file that owns a mechanism, the
order to touch things in, and one command that proves the change took effect. Symbols
are named rather than quoted, and the ADRs are linked rather than restated, for the same
reason — a copy drifts from the thing it was copied out of.

## Adding a built-in tool

A tool is one implementation of the `Tool` trait in `crates/tools/src/lib.rs`:

- `name`, `description` and `parameters` — what the model is shown; `parameters` is a
  JSON Schema;
- `action_class(args, cwd)` — the autonomy this *call* gets, computed from the call's
  own arguments rather than fixed on the tool
  ([ADR-0007 §4](decisions/0007-permission-sandbox-engine.md));
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

## Adding an event

The event set is the protocol
([ADR-0002](decisions/0002-json-rpc-wire-protocol.md),
[ADR-0003](decisions/0003-jsonl-truth-sqlite-index.md)), so an event starts in
`crates/schema/protocol.toml` and nowhere else:

1. Declare the payload as a `[[types]]` entry with `kind = "struct"`, one field per
   thing the record carries.
2. Declare the event as a `[[events]]` entry naming that payload and its `ignorable`
   marker. `ignorable = false` is the right answer for a kind a reader must understand;
   `true` is a promise that a reader which does not know the kind may skip the record
   and still show the session.
3. Run `./scripts/gen-schema.py`. It writes the files it owns: the Rust types, the
   `gen::event::*` name and the `EVENT_KINDS` table in
   `crates/schema/src/generated.rs`; the conformance test over generated samples in
   `crates/schema/tests/generated_roundtrip.rs`; and the TypeScript types and sample
   payloads under `clients/ts/`. None of them is edited by hand.
4. Write the event from the kernel through `Kernel::append`, naming the kind with the
   generated constant.

**A schema change is not finished until the generator has run.** `./scripts/check.sh`
runs `./scripts/gen-schema.py --check`, which compares those files byte for byte and
fails the gate when any is stale, so editing `protocol.toml` and skipping the generator
stops at a red gate whose message says to run the generator. The `cargo fmt --check`
gate is in the same script, which is why the generator formats the Rust it writes.

What the envelope then does is `crates/eventlog/src/lib.rs`'s job, not the caller's:
`append_with_parent` takes `ignorable` from the schema, so a writer cannot mislabel a
core event; a record whose marker contradicts the schema fails to read; an unknown
non-ignorable kind fails loudly under `replay`; an unknown ignorable kind is skipped and
reported under `replay_lenient`.

The proof: `./scripts/gen-schema.py --check` is silent, and `cargo test -p loong-schema`
runs the roundtrip test the generator just wrote.

Rougher than a recipe suggests: the record's `data` is `Json`, opaque to the type
system, so the declared struct and what `append` serialises can disagree with no compiler
error. `model.selected` is the standing example — the struct in `protocol.toml` carries
the model, the rule and the estimate, and the kernel also writes `step`, which the schema
does not declare. Whether anything notices depends on the kind. For one the kernel
replays — `message.appended`, `tool.call`, `tool.result`, `compaction.applied` — replay
reads the data back into the generated struct, so an omitted required field surfaces as a
session that will not reopen; the generator's own round-trip test cannot see it, because
it is generated in lockstep with the struct and never looks at what the kernel writes.
For `model.selected`, which replay ignores, nothing deserialises it back into its struct
at all, and the note above is the only warning there is. The event set is also not the display set: a new event
is not rendered specially until a surface handles its kind — `loong_tui::render_event`
(`crates/tui/src/lib.rs`) and the web client's switch in `crates/web/src/index.html`. An
unhandled kind is not dropped, which is the forward-compatibility envelope working; it is
shown as its bare name.

## Attaching a hook

A hook is repository configuration, not a crate: a manifest at
`<workspace>/.loongagent/hooks.toml` and, for a shell hook, a script beside it.
`crates/hooks/src/config.rs` owns the manifest (`HookSet::load`), the moments
(`HookEvent`) and the kinds (`HookKind`); `crates/hooks/src/run.rs` fires them. One
verdict contract covers every kind: a JSON object
`{"verdict": "allow" | "ask" | "deny", "reason": "…"}` on stdout, or a shell exit code —
zero allows, two denies with stderr as the reason. A hook that cannot answer falls back
to its own `on_error`, which defaults to `deny`; a guard that silently evaporates is the
outcome that default exists to prevent
([ADR-0007 §7](decisions/0007-permission-sandbox-engine.md)).

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
([ADR-0007 §1](decisions/0007-permission-sandbox-engine.md)).

## How routing picks a model

Routing happens once per model call, inside the turn loop in
`crates/kernel/src/turn/loop.rs`:
the kernel builds `loong_config::Facts` from the ledger's token estimate and the step
number, asks `Router::select`, and switches — and records — only when the answer differs
from the model already selected. The philosophy is
[ADR-0005](decisions/0005-routing-philosophy.md); the mechanism is in
`crates/config/src/profile.rs`:

- `Facts` is the whole input to a decision, and `Fact` plus `parse_condition` is the
  whole grammar a rule may use, so a condition is parsed and validated where it is
  written rather than when it first fires;
- `Router::select` is first-match-wins over `rules`, then `default`;
- the models a profile declares are built into adapters by `build_models`
  (`crates/cli/src/server.rs`), each one an implementation of `loong_providers::Model`
  (`crates/providers/src/lib.rs`); `OpenAiCompat` is the one real adapter, and the
  scripted models the demos use are doubles for the same trait;
- the choice is an event (`model.selected`, written by `record_model_selected`), so a
  change of model is never silent and "why this model?" is answerable from the log.

What to touch depends on what is changing:

| Change | Where |
|---|---|
| a model, the default, or a rule | the profile TOML only — `[[models]]` and `[router]`, the shape [DEVELOPMENT.md](../DEVELOPMENT.md) shows. A rule naming a model the profile does not define is refused where the profile is read (`profiles_from_payload` for a pack in `crates/config/src/layer.rs`, `finish` for layered files in `crates/config/src/merge.rs`), so a rule that can never fire does not load |
| a new adapter kind | implement `Model`, add the kind to `ADAPTER_KINDS` (`crates/config/src/layer.rs`), and build it in `build_models` **and** `build_model` (`crates/cli/src/server.rs`) |
| a new fact a rule may read | `Fact`, `Facts`, `Condition::matches`, `Condition::render` and `parse_condition`, all in `crates/config/src/profile.rs` |

The proof: `cargo test -p loong-config the_router_takes_the_first_matching_rule`, and in
a session with more than one model, `grep model.selected <session>.jsonl` prints the
model chosen, the one it replaced, the rule that fired and the estimate it read.

Rougher than a recipe suggests: a rule or a model is configuration and needs no code, but
a *fact* or an *adapter* is a several-place edit whose places do not reference each
other. The compiler catches part of a new fact — a variant added to `Fact` breaks the
`match` in `matches` and `render`, and a field added to `Facts` breaks the places that
build one — but `parse_condition` matches on a *string*, so a fact declared and not named
there is simply unparseable, and only a rule that tries to use it says so. For an adapter
the whitelist and the constructor are in different crates: update `ADAPTER_KINDS` alone
and loading names the kinds that exist; update the constructor alone and the
configuration is refused before the adapter is ever built. And a person's `/model` choice
rewrites `router.default` (`Kernel::select_model`), so it becomes the model a later turn
starts from, while rules still outrank it.

One inspection does not work, and it is worth knowing before reaching for it:
`loongagent config explain router.rules` fails with `unknown key "router"`, because
`config_explain` (`crates/cli/src/config_cmd.rs`) reduces a key to its first path segment
while `kind_of` (`crates/config/src/layer.rs`) knows only the dotted keys. Read the
effective rules with `loongagent config show`, whose row for `router.default` and
`router.rules` prints the key, the value, its kind and the file that set it — the layer
that file belongs to is the separate list of layer files printed above the table — and
read what actually fired with the `model.selected` grep above.
