# Steer — spec

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:writing-plans to
> turn this spec into an implementation plan. This spec is the design; the plan
> argues from it.

**Date:** 2026-11-26 · **Status:** Proposed

**Goal:** a person typing during a running turn is never turned away. Their words
either go into the turn that is running (steer) or wait their turn in a visible
queue (follow-up), and the choice is theirs, one key apart.

**The pain:** enter during a turn prints `· 上一轮还在跑 — 等它结束，或 ctrl-c 停下`
and throws the text away. The only mid-turn control is `Ctrl-C`, which ends the
turn; the only way to say "no, do it this way" is to kill the work and start over.
Every other agent CLI lets a person interject, and the note itself admits the
missing feature (`docs/design/sessions-lineage.md` §5).

## 1. Two behaviours, one axis

| What the person does while a turn runs | What happens |
|---|---|
| `enter` | The text joins the **queue**. Nothing is sent now; it runs as the next turn when this one ends. The transcript says `· 已排队（第 2 条），这一轮结束后发出`. |
| `alt+enter` (or `/steer <text>`) | The text is handed to the **running turn** and the model sees it at its next step boundary. The transcript says `↪ 插话：…` and the event stream carries it as a user `message.appended`. |

`alt+enter` with an empty input steers the **head of the queue** instead, so a
queued line can be promoted without retyping it.

Both are standard: the queue is the follow-up every chat UI has, and the hand-off
is what "steering" means in the tools this one is measured against. Steering is
not a rewind — the turn keeps its step count, its budget and its history
(ADR-0015); only the delivery time of one more user message changes.

## 2. Where a steer can land: a step boundary

A steer is only ever applied at the boundary between two steps of `run_inner`,
after the tool results of the previous step are recorded and before the model is
asked again. That is deliberate, not an implementation detail:

- A user message inserted **between** an assistant `tool_calls` message and its
  `role: tool` results breaks the conversation shape every provider enforces.
  The boundary is the only place where no call is waiting for a result.
- The boundary is also where `interrupt` and the stall guard read their flags, so
  a steer cannot overtake a stop: `apply_requested_steer` runs after both. A turn
  that stops at that boundary leaves the steer queued, and the next turn delivers
  it before its own prompt — exactly-once either way.
- Consequence, stated plainly: a steer cannot interrupt a tool that is already
  running. A five-minute `cargo build` is not preempted; the words arrive when it
  returns. The repo's rule is that no turn leaves half-finished work, and
  preemption would.

`Kernel::submit_with` drains the same slot **before** recording the new prompt,
so a steer that missed its turn (the person pressed `alt+enter` as the turn was
ending) is spoken first and then the new prompt follows. One mutex-guarded
`pop_front` decides where each text goes, so a text is never delivered twice and
never dropped.

## 3. Transport: the control channel that already exists

`/yolo` typed during a turn cannot ride the wire either — the worker thread is
inside `client.submit_with`, and on stdio the server is not reading requests. The
launcher therefore gives the child a **private control channel**
([surfaces.md](../../design/surfaces.md) §3): a socketpair on unix, one reader thread on the
server, a slot the kernel drains. Steer reuses it:

- One line per message, `steer "<json string>"`. The payload is a JSON string
  literal so a multi-line steer survives; `mode yolo` keeps its bare word.
- The reader thread only writes to the slot. The turn stays the single writer of
  the log, which is the property that made the permissions path safe.
- Unix only. Windows has no fd inheritance, so there a steer is queued instead and
  the screen says so (`alt+enter` degrades to the same queue as `enter`). This
  matches how `/yolo` already degrades.

Reusing that channel is a decision, not a convenience: ADR-0019 §1 said the channel
carries one intent and that a second one needs its own decision, so
[ADR-0023](../../decisions/0023-steer-rides-the-control-channel.md) widens it to a closed
list of two intents with separate slots and separate points of application.

No protocol method is added. `web` and `vscode` therefore cannot steer mid-turn;
they can queue, which they already do by submitting after the turn ends. This is
the same gap `/yolo` has and is documented as a gap, not as a feature.

## 4. The steer passes the prompt gate

A steer is a prompt submission. `Kernel::apply_requested_steer` fires
`user_prompt_submit` for each text, with the same `{"text", "profile"}` payload a
submitted prompt sends, and a **tightening** verdict drops that text and records
an assistant note saying so. Otherwise a hook installed to screen prompts could be
walked around by pressing `alt+enter`.

The steer is recorded as a plain user `message.appended`; no new event kind, no
new schema field. Replay already handles a user message anywhere in the log, so a
resumed session shows the interjection where it happened.

## 5. What the screen shows

- The queue is visible: the hint bar carries `待发 2 · alt+enter 插话` while it is
  non-empty, and each queued line is echoed once as it is queued.
- A steer is echoed once, at the moment it is sent (`↪ 插话：…`), like a prompt.
  The worker already suppresses role-`user` `message.appended` because the
  person's own line is on screen; the steer relies on the same rule.
- The queue survives a turn that ends on its own, an error, or a stop... except
  an explicit stop. `Ctrl-C` while a turn runs **clears the queue** and says so:
  "stop" must not be followed by a turn starting itself. A steer already handed to
  the kernel cannot be recalled, and the screen says that too.

## 6. Out of scope

- Preempting a running tool (§2).
- Steering from `web`/`vscode`, or a `turn.steer` protocol method (§3).
- Editing or reordering queued lines; `alt+enter` promotes the head, which covers
  the "I want this one now" case without a second editor.
- A retry (`Alt+R`) of a steered turn: the steer is part of the running turn's
  log, so a resend simply includes it in the history.
