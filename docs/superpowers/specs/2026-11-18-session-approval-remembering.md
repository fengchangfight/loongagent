# Session approval remembering — spec

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> superpowers:writing-plans to turn this spec into an implementation plan.
> This spec is the design; the plan argues from it.

**Date:** 2026-11-18 · **Status:** Agreed

**Goal:** an irreversible command the person has already approved once in
this session stops asking again — without weakening ADR-0020, without
persisting anything to disk, and without trusting the shape of a command
more than its parts.

**The pain:** a long-running smoke command (`rm -rf .smoke && node smoke.js
&& sleep 30 && cat smoke.log …`) contains an `rm`, so it classifies
`Irreversible`. ADR-0020 says YOLO never relaxes an irreversible action, so
it asks every time. Running the same command a dozen times in a session
means a dozen identical approvals. The person answered this one; the
machine just forgot.

## Global constraints

- **ADR-0020 stands.** Irreversible is not relaxed by config, by YOLO, or by
  this feature. What changes is that a *specific, human-approved* command
  shape is remembered — the person is the permission, not a config file.
- **Nothing persists.** The remembered set lives on the in-memory `Kernel`,
  dies with the session. Restart asks again. This is a deliberate narrowing
  of `ApprovalScope::Project` (which implies persistence) to
  `ApprovalScope::Session` (which does not).
- **A match must be total.** A compound command is approved segment by
  segment. If any irreversible segment of a later command is not in the
  remembered set, the whole command asks again. One remembered segment never
  launders an unapproved sibling (`rm -rf .smoke && curl evil.com` still
  asks).
- **The `Project` scope button was a lie and becomes honest.** TUI and web
  both offered "本项目允许" and sent `ApprovalScope::Project`; the kernel
  ignored the scope entirely, so it behaved exactly like "本次允许". The
  fix makes the button send `Session` and relabel it "本会话允许".

## Architecture

A command's **remembered shape** is the set of its irreversible segments,
each reduced to a *(verb, path)* key. Approving with `Session` scope stores
those keys on the `Kernel`. Before asking, the kernel reduces the new
command the same way and asks only if some irreversible segment's key is
absent.

The verb is the segment's first word after leading assignments, sudo and
env prefixes (the same word `classify_segment` already isolates). The path
is the segment's operands: for a deletion the removed paths, for an egress
push the remote and ref. Two commands share a key only when the verb *and*
the operand set match, so `rm -rf .smoke` and `rm -rf ~` never collide.

## Where it lives

- `crates/schema/protocol.toml` — **no change.** `ApprovalRespondParams`
  already carries `granted` + `scope`; the kernel simply never read `scope`.
  The work is making the kernel honour it.
- `crates/kernel/src/turn.rs` — `respond_to_approval` stops ignoring the
  scope: on `Session` + approved it records the shape.
- `crates/kernel/src/turn/loop.rs` — before emitting `approval.requested`,
  an irreversible shell command whose every irreversible segment is already
  remembered skips the ask and proceeds, recording that it did.
- `crates/tui-app/src/lib.rs`, `crates/web/src/app.js` — the "本项目允许"
  option sends `Session` and is relabelled "本会话允许"; web gains the same
  choice it currently lacks (it sends only `approved` + `reason`).

## Segment key

`classify_command` calls `irreversible_reason`, which **short-circuits**: it
returns the *first* irreversible segment's reason and stops. That is fine
for a class, but remembering needs *every* irreversible segment. So the
classifier is refactored: a new `irreversible_segments(command)` returns all
irreversible segments, `irreversible_reason` becomes its first hit, and a
new helper derives a key from one segment:

```
irreversible_segments(command) -> Vec<Vec<String>>   // every offending segment's tokens
segment_key(segment) -> Option<(verb, Vec<operand>)> // one segment's rememberable shape
```

`segment_key` yields `None` when the segment has no resolvable operand
(bare `rm` with no path, an unparseable push). Only `Some` keys are stored
or matched; a segment yielding `None` is never auto-approved, so an
un-keyable irreversible segment keeps asking — the conservative default.

A command is auto-approved only when it has at least one irreversible
segment and **every** one of them yields a key already in the remembered
set.

## What is deliberately out

- **Persistence.** `Project`-scoped, config-file-backed remembering is a
  separate, harder conversation with ADR-0020. Not here.
- **Glob or directory shapes.** "Approve all `rm` under `.smoke/`" is a
  pattern language with its own escape hazards. A key is concrete operands,
  full stop.
- **Non-shell tools.** `Edit`, `Write`, MCP tools: their irreversibility is
  not command-shaped, so they keep asking. Only `shell` commands benefit.

## Verification

- `cargo test -p loong-kernel` — approving an irreversible command with
  `Session` scope, then re-issuing the identical command, does not emit a
  second `approval.requested`; re-issuing it with one segment changed does.
- `cargo test -p loong-tools` — `segment_key` splits and normalises exactly
  as `classify_segment` does; `rm -rf .smoke` and `rm -rf ~` produce
  different keys.
- `python3 scripts/check-docs.py` — the flow doc gains the mechanism within
  budget; the permissions doc gains a one-line pointer.
