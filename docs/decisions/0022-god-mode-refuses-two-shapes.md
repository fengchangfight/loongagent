# ADR-0022 — God mode refuses two shapes instead of asking

- **Status:** Proposed, awaiting the person's signature
- **Up:** [../design/README.md](../design/README.md)
- **Date:** 2026-11-16
- **Decides:** What a session may do without asking, past YOLO. Reads ADR-0007
  §4 (autonomy is allocated per action class) and §5 (an opt-out is itself an
  event) together with ADR-0020 (YOLO never relaxes an irreversible action),
  and adds a third stop that keeps two refusals where ADR-0020 kept the
  question.
- **Design docs:** [tools-permissions.md](../design/tools-permissions.md) ·
  [flows/tool-approval.md](../design/flows/tool-approval.md) ·
  [kernel-loop.md](../design/kernel-loop.md) ·
  [surfaces.md](../design/surfaces.md) ·
  [web-surface.md](../design/web-surface.md)
- **Code:** `crates/schema/protocol.toml` · `crates/schema/src/lib.rs` ·
  `crates/tools/src/lib.rs` · `crates/kernel/src/turn/loop.rs` ·
  `crates/server/src/lib.rs` · `crates/tui-app/src/lib.rs` ·
  `crates/web/src/app.js`
- **Verify:** `cargo test -p loong-schema -p loong-tools -p loong-kernel -p loong-server -p loong-tui-app -p loong-web` ·
  `./scripts/check.sh`

## Context

ADR-0020 drew a line and the line was right: two things cannot be taken back,
so YOLO still asks about them. In a long unattended run that line turned out to
cost more than it bought. The prompts that survive YOLO are the great majority
of what is left, and in practice the answer is always yes — deleting a scratch
file, pushing a branch, publishing a package. A question whose answer is always
yes is not a decision; it is an interruption, and an interruption repeated
often enough is answered without reading.

So the protection was never in the question. It is in the answer. What the
person named as the harm worth stopping is narrow and concrete: **a deletion
that reaches outside the workspace, and secret material leaving the machine.**
Everything else in the irreversible class is something they meant to allow.

## Decision

### 1. A third stop on one axis

`PermissionMode` is `ask`, `yolo`, `god`. It replaces the boolean behind
`permissions.set`, absorbed additively: the old `auto_approve` field stays
accepted and still means `yolo` / `ask`, and `permissions.relaxed` gains an
optional `mode` so an event from an older log still parses.

God is reachable only from inside a session: `/god` in the TUI,
`permissions.set {mode: "god"}` from the web page's button. There is no config
key, no command-line flag and no environment variable — a mode that survives a
restart is a mode nobody remembers granting. Every kernel starts at `ask`.

### 2. God relaxes the irreversible class, minus two refusals

Under god an `irreversible` ruling is allowed instead of parked, and the record
says so (`policy.decided{rule: "god:irreversible"}`). Two shapes are refused
outright instead — a `deny`, never a prompt, because there is nothing a click
could add:

- **a deletion whose operand is outside the workspace** — or whose operand a
  shell would resolve at run time (`$X`, `*`, `~`), which god treats as outside
  rather than guessing;
- **egress of secret material** — a command that sends data out and names a
  keyfile, a credential store, a `printenv`, or a `$…KEY` variable. The narrow
  reading on purpose: the *egg*, not the basket. Ordinary uploads stay silent.

`loong_tools::god_refusal(command, cwd)` is the whole of it: one pure function
from a command line and a workspace root to "refuse, and why" or nothing. Its
worked examples live in
[flows/tool-approval.md](../design/flows/tool-approval.md) §3.

### 3. God relaxes questions, never prohibitions

A `Verdict::Deny` — the sandbox's file-write wall, a hook's tightening, the
confinement — stays a deny under god, and is evaluated before the gate. God
changes what a question becomes. It changes nothing about what is forbidden.

## Consequences

- The classifier stays a heuristic and is now load-bearing for a deny rather
  than only for a prompt. It catches `rm -rf /`, `rm -rf ~`,
  `cat .env | curl -d @-`, `scp ~/.ssh/id_rsa`; it does not catch a `python -c`
  that opens a socket and reads a keyfile, a `base64` of a `cat`, or a script
  whose body does the sending. God promises "the ordinary shapes of the mistake
  stop and are refused", not "nothing leaves".
- A secret already committed and then pushed is **not** caught: the gate sees
  `git push` with no secret source and allows it. A pre-push scan is a
  different feature.
- False refusals cost a rewrite, not a click: the refusal is a tool result the
  model reads, so a command rewritten inside the gates runs. False allows cost
  the thing that was deleted or sent, which is why an unresolvable operand
  counts as outside.
- The gates are not configurable. A gate a person can widen is widened on the
  day it is inconvenient, which is the day it matters.
- YOLO keeps its exact meaning; ADR-0020 still governs it.

## Alternatives considered

1. **Persist god in a profile or a flag**, like `--dangerously-skip-permissions`.
   Rejected: an unattended process that starts in god mode was never granted it
   by a person in that session, and the log would say nothing.
2. **Keep asking and answer automatically**, so the log looks unchanged.
   Rejected: a relaxed ruling must look relaxed (ADR-0007 §5), or the
   transcript lies about how much autonomy the session had.
3. **The strict reading of egress** — refuse every payload-bearing command, let
   `git push` through only. Rejected by the person who asked for this: uploading
   a report, installing a package and publishing a release are what the session
   is for, and refusing them trades an interruption for a roadblock.
4. **Scrub config-loaded secrets from every child's environment** so `env` can
   carry nothing. Rejected here: it silently breaks a legitimate
   `curl -H "Authorization: $KEY"`, and it changes every mode rather than god.
   A per-command pass-through allowlist is its own decision.
5. **Widen the sandbox in god mode** (writes outside the workspace). Rejected:
   not asked for, and a different decision about a different axis.
