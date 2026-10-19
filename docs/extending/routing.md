# How routing picks a model

- **Up:** [README.md](README.md)
- **Authority:** [ADR-0005](../decisions/0005-routing-philosophy.md)
- **Code:** `crates/config/src/profile.rs`, `crates/cli/src/server.rs`, `crates/providers/src/lib.rs`, `crates/kernel/src/turn/loop.rs`, `crates/cli/src/config_cmd.rs`
- **Verify:** `cargo test -p loong-config the_router_takes_the_first_matching_rule` · `./scripts/check.sh`

Routing happens once per model call, inside the turn loop in
`crates/kernel/src/turn/loop.rs`:
the kernel builds `loong_config::Facts` from the ledger's token estimate and the step
number, asks `Router::select`, and switches — and records — only when the answer differs
from the model already selected. The philosophy is
[ADR-0005](../decisions/0005-routing-philosophy.md); the mechanism is in
`crates/config/src/profile.rs`:

- `Facts` is the whole input to a decision, and `Fact` plus `parse_condition` is the
  whole grammar a rule may use, so a condition is parsed and validated where it is
  written rather than when it first fires;
- `Router::select` is first-match-wins over its `rules` (each a `RouterRule`), then
  `default`;
- the models a profile declares are built into adapters by `build_models`
  (`crates/cli/src/server.rs`), each one an implementation of `loong_providers::Model`
  (`crates/providers/src/lib.rs`); `OpenAiCompat` is the one real adapter, and the
  scripted models the demos use are doubles for the same trait;
- the choice is an event (`model.selected`, written by `record_model_selected`), so a
  change of model is never silent and "why this model?" is answerable from the log.

What to touch depends on what is changing:

| Change | Where |
|---|---|
| a model, the default, or a rule | the profile TOML only — `[[models]]` and `[router]`, the shape [README.md](../../README.md) shows. A rule naming a model the profile does not define is refused where the profile is read (`profiles_from_payload` for a pack in `crates/config/src/layer.rs`, `finish` for layered files in `crates/config/src/merge.rs`), so a rule that can never fire does not load |
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
`router.rules` prints the key, the value, its kind and the `set by` source — the path of
the file that set it, or `control plane: <issuer> <version>` for a value a signed pack
set, since the layer-file list printed above the table names only the file layers — and
read what actually fired with the `model.selected` grep above.
