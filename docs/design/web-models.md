# The model panel

- **Up:** [web-surface.md](web-surface.md)
- **Authority:** [ADR-0009](../decisions/0009-surfaces.md) · [ADR-0002](../decisions/0002-json-rpc-wire-protocol.md)
- **Code:** `crates/web/src/app.js`, `crates/web/src/index.html`, `crates/web/src/app.rs`, `crates/cli/src/web.rs`, `crates/cli/src/local.rs`
- **Verify:** `cargo test -p loong-web` · `cargo test -p loongagent --test web_surface` · `./scripts/check.sh`

How a browser puts models on the bar of [web-surface.md](web-surface.md) §4 and takes
them off again. It is `loongagent local add` without a terminal: the same two files, the
same rules, one writer — the CLI (config-routing.md §3, §5, §8). Nothing about config is
decided in the page.

## 1. The warning comes first

A new install has no model, and the page is where its owner will be. `model.configured`
answers at boot from the resolved profile, before any tab or child exists. An empty list
puts a banner under the bar — *no model is configured yet* — with the button that fixes it:
a banner, not a dialog, because a page that cannot answer must still be readable, and one
that cannot answer at all says nothing.

## 2. The panel asks, and the list is the selection

`model.providers` lists the catalogue with what each entry needs (`needs_key`, `key_env`),
so the list is a convenience: leave it alone and the pasted key names its own provider, the
guess `local add` already makes.

`model.probe {provider, key}` proves the key and answers what the provider offers —
`{name, context_window, configured}` per model — and writes nothing. Those rows are drawn
with a tick box each, ticked only where the model is already configured: a pasted key says
something about credentials, not about wanting the three hundred models a provider has
published. A provider with many models would otherwise fill `local.toml` with blocks nobody
chose, and choosing one out of them is the thing the panel is for. `全选`, `清空` and the
filter box are one click each for those who do want them all.

`model.add {provider, key, models}` proves the key (when one is typed), writes it to `env`,
and writes **exactly the named** `[[models]]` blocks — behind `loong_web::ModelSetup`, so
one file keeps one writer (config-routing.md §3, §8). A refused key is not written: the
wizard can show a probe and a key field at once, a page cannot without one of them lying.
A page that sends no `models` at all is a broken page and is refused, because the empty
list means "none of them" and deleting on an accident is not a mistake a config file can
undo.

## 3. Editing is the same panel

`model.configured` carries each model's `provider` and `context_window`, so the page draws
the same rows for what is already in the file, with no network call. Unticking one and
saving is a `model.add` that omits it; every other provider's blocks are left alone.

Deselection removes a **model**, never a provider: `env` keeps the key, so `model.probe`
with an empty key answers from it (`key_on_file` says whether it can) and the model can be
ticked back on without pasting anything. A block that survives the write is kept verbatim,
so a comment above a kept model stays where it is (config-routing.md §3). A model with no
catalogue provider behind it — hand-written, or named after an endpoint — is listed and
not offered: it has no tick box, because rewriting it would mean inventing a provider for
it.

## 4. What it costs

A child resolves its config once, so a write leaves live children stale. The adopt drops
every idle child — a child reads the log, never owns it, so dropping one loses only what
replay restores — and the page re-opens its session. A child mid-turn is left alone and
returned as `waiting`; a workspace with a running turn refuses the write, since killing
that child drops the turn.
