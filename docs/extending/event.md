# Adding an event

- **Up:** [README.md](README.md)
- **Authority:** [ADR-0002](../decisions/0002-json-rpc-wire-protocol.md) · [ADR-0003](../decisions/0003-jsonl-truth-sqlite-index.md)
- **Code:** `crates/schema/protocol.toml`, `scripts/gen-schema.py`, `crates/schema/src/generated.rs`, `crates/schema/tests/generated_roundtrip.rs`, `crates/eventlog/src/lib.rs`, `crates/tui/src/lib.rs`, `crates/web/src/index.html`
- **Verify:** `./scripts/gen-schema.py --check` · `cargo test -p loong-schema` · `./scripts/check.sh`

The event set is the protocol
([ADR-0002](../decisions/0002-json-rpc-wire-protocol.md),
[ADR-0003](../decisions/0003-jsonl-truth-sqlite-index.md)), so an event starts in
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
