# Paste, the busy prompt and approval ids

- **Date:** 2026-11-03
- **Status:** Approved in conversation; this document is the spec the implementation plan is written from
- **Scope:** three small repairs found by resuming a real session: a multi-line paste must arrive as one prompt, a prompt typed while a turn is running must not be queued into the turn's approval pause, and approval ids must continue across a resume. No schema change, no new configuration key.

## 1. Problem

Resuming `7c86phhd` and pasting a multi-line prompt produced
`-32602: out of order: approval appr-1 is still pending`:

1. The TUI has no bracketed paste. A pasted block is delivered as keystrokes,
   so its first newline **submits** the first line — the log's user message was
   only `继续。当前状态：` (8 characters). The rest of the block stayed in the
   input.
2. The screen allows enter while a turn is running, so the rest was submitted as
   a second prompt. The worker's command channel is sequential: the second
   `prompt.submit` waited until the turn **paused** on an approval, then arrived
   while `Kernel.pending` was set. `Kernel::submit_with` refuses it, which is
   correct — but the person sees a raw JSON-RPC error.
3. The approval id restarted at `appr-1` because `Kernel.approvals` is in-memory
   and begins at zero. The log already held `appr-1`…`appr-9`; the same file now
   has two different requests with the same id.

## 2. Goals and success criteria

| # | Criterion | How it is measured |
|---|---|---|
| 1 | A pasted block arrives as one input, not one submit per newline | `App::paste` test: internal newlines stay, a trailing newline is dropped, no `Action::Send` |
| 2 | Bracketed paste is on for the session and off on exit | `surface::run` uses `EnableBracketedPaste` / `DisableBracketedPaste`; `Event::Paste` reaches `App::paste` |
| 3 | Enter while a turn runs does not queue a prompt, but slash commands still work | key test: a plain prompt is refused with a line; `/yolo` still returns its action |
| 4 | Approval ids continue from the log | kernel test: create, ask, reopen, ask → `appr-2` |
| 5 | The pending-approval refusal reads as an instruction, not a protocol error | TUI turns the message into “还有一条操作在等审批 …” |

## 3. Non-goals

- **A real follow-up queue.** Refusing is enough; a queue of prompts that runs
  after a paused approval needs a lifecycle decision (does the follow-up join the
  paused turn or start a new one?) and is not this repair.
- **Replacing JSON-RPC errors with a structured code.** The TUI text is the fix
  a person sees; the protocol keeps returning the kernel error.
- **Multi-line editing keys.** Paste inserts newlines; `enter` still submits.

## 4. Design

### 4.1 Bracketed paste

`surface::run` enables bracketed paste with the alternate screen and disables it
on every exit path. The event loop matches `event::Event::Paste(text)` and calls
`App::paste(&text)`, which:

- normalises `\r\n` and `\r` to `\n`;
- drops one trailing newline, so a copied block does not leave an empty last line;
- inserts the rest at the caret and never submits.

The input already renders `\n` as a hard break (§3 of surfaces.md), so the
multi-line block is visible as a block and `enter` sends all of it.

### 4.2 Enter while a turn is running

The enter arm first offers the text to `App::surface_command`, so `/yolo`,
`/cost`, `/exit` and the rest keep working (and `/yolo` still uses its private
control channel while a turn owns the connection). Only a plain prompt that
reaches the submit path while `App::busy` is true is refused, with:

```
· 上一轮还在跑 — 等它结束，或 ctrl-c 停下
```

`pending` is not `busy`: when an approval is open, enter answers the menu, as
before.

### 4.3 Approval ids continue

`Kernel::build` seeds `approvals` from the derived index's count of
`approval.requested` events, so the next id is one past the last one in the log.
A fresh session still starts at `appr-1`; a resumed one starts where it stopped.
Replay still does not restore `Kernel.pending`: the parked call belongs to the
process that asked it.

### 4.4 The refusal reads like advice

The TUI's failure arm maps a message containing `is still pending` to:

```
! 还有一条操作在等审批 — 先回答它（y/n 或方向键 + 回车），再发新的提问
```

Other failures are unchanged. The kernel's own message gains the action it
wants (`— answer it (approve or deny) before sending another prompt`), because a
non-TUI surface shows that string directly.

## 5. Verification

| Check | Where |
|---|---|
| Paste stays one input, normalises line endings, drops the trailing newline | `cargo test -p loong-tui-app tests::a_pasted_block_stays_one_prompt` |
| Enter while busy refuses with the line; `/yolo` still acts | `cargo test -p loong-tui-app tests::` |
| Approval ids continue | `cargo test -p loong-kernel turn::tests::approval_ids_continue_after_a_resume` |
| The whole gate | `./scripts/check.sh` |

## 6. Risks

- **A terminal without bracketed-paste support** ignores the enable sequence and
  keeps the old behaviour. Nothing breaks; the paste is still line-by-line.
- **Refusing enter while busy removes the accidental queue.** Someone who wants
  to line up a second task now waits or presses `ctrl-c`; the message says so.
- **Seeding from the index** counts `approval.requested` events from this log
  only. A fork inherits no ancestor approval events into its own file; ids are
  unique where the process can see them, which is what `respond_to_approval`
  checks.
