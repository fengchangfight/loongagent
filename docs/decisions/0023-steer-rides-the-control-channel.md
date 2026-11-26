# ADR-0023 — A second mid-turn intent: the control channel carries a steer too

- **Status:** Accepted
- **Up:** [../design/README.md](../design/README.md)
- **Date:** 2026-11-26
- **Decides:** Whether a *steer* — a message a person hands to the turn that is running —
  may travel the private control channel ADR-0019 created, and what keeps that channel
  narrow now that it carries two intents. Extends ADR-0019 §1 (one intent, and nothing
  else) and ADR-0015 (steering changes delivery, not the tree).
- **Design docs:** [sessions-lineage.md](../design/sessions-lineage.md) §5 ·
  [kernel-loop.md](../design/kernel-loop.md) §5 · [surfaces.md](../design/surfaces.md) §3 ·
  [spec](../superpowers/specs/2026-11-26-steer-design.md)

## Context

ADR-0019 §1 wrote its own boundary down: the channel "carries exactly one intent —
approval prompts on or off — and nothing else", a second mid-turn intent "is not a
reason to reuse it silently: that would be a general control plane and needs its own
decision". That sentence anticipated this case, and steering is the case.

The need is the same one that produced ADR-0019. A turn owns the stdio connection until
it ends or parks, so a word typed while the agent works cannot ride `prompt.submit`; and
the message is exactly what has to reach the turn that is running. Left unsolved, a
person's only mid-turn control is a stop: they kill the work and start over.

The options are each wrong in a specific way:

1. **Reuse the channel silently.** Forbidden by ADR-0019 §1, and rightly: the channel
   stops being able to say what it is for.
2. **A second socketpair.** Two descriptors, two reader threads, two inheritance paths,
   for two intents a surface always holds together. The Windows parity debt would be
   paid twice, and the second channel would be indistinguishable from the first in
   purpose.
3. **A general control plane** — one process-control RPC with a method space. This is
   the shape ADR-0019 refused, and it would invite each new intent without a decision.
4. **A closed list of two intents**, each with its own typed slot and its own point of
   application.

## Decision

### 1. The channel carries two intents, and the list is closed by decision

The first word of a line names the intent: `mode <word>` (ADR-0019) and
`steer "<json string>"` (this ADR). A third intent needs a new ADR — the narrowness is
now a rule with a number, not a comment. An unknown first word is ignored, and a
malformed payload is ignored without ending the reader, so one bad line cannot take
steering away for the rest of the session.

### 2. Each intent has its own slot and its own point of application

The reader thread only fills slots; the kernel applies them, and the two apply points
are deliberately different. A permissions request is applied before a verdict
(`Kernel::apply_requested_permissions`, at the top of a step and in `dispatch`). A steer
is applied `Kernel::apply_requested_steer` **only** at a step boundary after the stop
and stall checks, because that is the only point where no tool call is waiting for its
result — a user message between an assistant's `tool_calls` and its `role: tool` results
is a shape every provider rejects. Neither method may borrow the other's point.

### 3. A steer is content, not process control

The channel is the envelope, and only because the turn owns the connection. What
travels is a person's words, recorded as an ordinary user `message.appended` and gated by
`user_prompt_submit` like any submitted prompt. The channel still cannot change policy,
answer a question, or grant anything: a steer is history, and history is what the log
already holds (ADR-0015 — the tree does not grow a new path for it).

### 4. ADR-0019's invariants still hold

Only the turn writes the log. The write end never leaves the surface process, the child's
read end stays `FD_CLOEXEC`, and a tool can neither read nor send on it (ADR-0019 §2,
§5). The reader thread is still the only reader, and it still writes nothing but slots.

### 5. A message survives the gap; a command does not

A steer can arrive a moment after its turn ended. One mutex-guarded `pop_front` decides
where a text goes — applied inside the turn, it is that turn's interjection; taken by
`Kernel::submit_with` before the next prompt, it is spoken first. Exactly-once either
way, and nothing is dropped. A permission request cannot be treated this way (it is a
command about a call that may not exist any more); a message can, because it is a
message.

### 6. Platform scope, stated again

Unix only, for the same reason as ADR-0019 §6: no descriptor inheritance on Windows, so
`alt+enter` queues the text there and the screen says so. The gap lives in
[surfaces.md](../design/surfaces.md) §3, where a person will meet it.

## Consequences

**Won.** One transport and one reader thread carry both mid-turn behaviours; the screen
can promote a queued prompt into the running turn with one key; the log keeps one writer
and one order; the protocol grows nothing.

**Paid.** The channel is wider than ADR-0019 left it, so its reader now decodes a
payload and the malformed case needs a test; two apply points with different rules must
stay separate in review; and the surface's steer can be lost only to its own failure to
write, which it reports and turns into a queued prompt rather than a silent loss.

**Verified.** The kernel delivers a steer inside the running turn
(`a_steer_reaches_the_model_inside_the_running_turn`), speaks a steer that missed its
turn before the next prompt without repeating it
(`a_steer_that_missed_its_turn_opens_the_next_one`), and refuses one a
`user_prompt_submit` hook refuses (`a_steer_a_hook_refuses_never_reaches_the_model`); the
server decodes the line, keeps a newline inside one message and survives a malformed one
(`the_control_channel_leaves_a_steer_for_the_kernel`); the launcher writes the line the
server reads (`a_steer_writes_one_line_the_server_can_decode`); the screen queues a
prompt instead of refusing it, steers with `alt+enter` and `/steer`, flushes one queued
prompt per turn and only when nothing is pending, and queues the words rather than losing
them when the channel is absent or fails (`a_prompt_typed_during_a_turn_waits_in_the_queue`,
`an_alt_enter_steers_the_running_turn`,
`an_alt_enter_with_an_empty_prompt_steers_the_queue_head`,
`an_alt_enter_without_a_channel_queues_instead`, `a_failed_steer_is_queued_not_lost`,
`the_queue_moves_when_the_turn_ends`, `a_steer_command_takes_the_text_after_it`).

**Would reopen it:** a third mid-turn intent; a steer that must preempt a tool already
running (a different promise than this one, and it collides with "no half-finished
work"); or Windows parity landing, which makes §6 obsolete rather than wrong.
