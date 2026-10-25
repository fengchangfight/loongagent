# ADR-0019 — Permissions can change mid-turn: a private control channel, applied by the kernel

- **Status:** Accepted
- **Up:** [../design/README.md](../design/README.md)
- **Date:** 2026-10-20
- **Decides:** Whether a surface may change the session's approval policy while a turn
  owns the connection, and how that request reaches the kernel. Extends ADR-0007 §5
  (YOLO is a visible opt-out, and opting out is an event) and ADR-0009 §3 (a capability
  is implemented once, in the server; a surface never decides a permission).
- **Design docs:** [flows/tool-approval.md](../design/flows/tool-approval.md) ·
  [kernel-loop.md](../design/kernel-loop.md) · [surfaces.md](../design/surfaces.md)

## Context

Two ADRs already say what YOLO is. ADR-0007 §5 makes the opt-out an event
(`permissions.relaxed`) and requires the interface to show it; ADR-0009 §3 says a
capability lives once, behind the server, and a surface may only render and ask.

The transport says something else. There is one stdio connection and the server reads
one request at a time: `prompt.submit` owns the connection until the turn ends or parks
for an approval. A `permissions.set` sent while the agent is working therefore waits
for the turn — which is the one moment the command exists for. Reported from use: a
person reaches for `/yolo` *because* the agent is running, and it took effect only
after the work it was meant to stop interrupting.

The obvious options are each wrong in a specific way:

1. **Flip the state locally and never tell the server.** The surface would claim a
   security state it cannot enforce. ADR-0007 §5 asks the interface to show what is
   true; showing prompts off while they are still on is the same lie in the other
   direction.
2. **Serve `permissions.set` concurrently on the main connection.** That makes the
   server's single-reader loop and its single-threaded `Out` concurrent, and makes the
   client match responses by id. It changes the request semantics of every surface —
   the web page included — to solve one command.
3. **A signal, like the interrupt.** Windows has no user signals, and a signal carries
   no value (on/off) and no error.
4. **A loopback socket with a token.** The token would have to travel in the child's
   arguments or environment, which are readable by any same-user process — including
   the agent's own shell tool. That is a new path for a tool to grant itself
   permissions. Rejected on the permission boundary alone.

## Decision

### 1. A private control channel, not a protocol method

The surface that spawns the server opens a socketpair; the read end is inherited as a
fixed descriptor (descriptor 3 on Unix) and **the write end never leaves the surface
process**. The channel carries exactly one intent — approval prompts on or off — and
nothing else. Like the interrupt, it is process control between a surface and the
server it spawned ("the process is ours"), so the protocol does not grow a shape that
only one surface can produce.

### 2. The server records the request; the kernel applies it

A reader thread leaves the request in `Kernel.permissions_request`. The kernel applies
it at the next boundary where nothing is half-done: the top of a step, and
`Kernel::dispatch` before `Pdp::decide`, so the call that would have asked is allowed
instead. The server applies the same method before handling a request, which catches one
that arrived after the last boundary. **Only the turn writes `permissions.relaxed`** —
in order, before the call it relaxes (ADR-0003, ADR-0007).

### 3. It never answers a pending approval

The channel changes the policy for the next verdict. A question already on screen is
still the person's to answer; a surface deciding it would be the divergence ADR-0009 §3
forbids.

### 4. The screen may lead; the event is the truth

The surface may show the change at once — the owner chose "as fast as the keypress",
and a command that appears ignored is its own defect. `permissions.relaxed` is the
confirmation, and when it disagrees the event corrects the screen. The log records only
what the kernel applied.

### 5. The channel is not reachable from a tool

The write end is held only by the surface. The child's read end is marked
`FD_CLOEXEC`, so no tool child can even read (and swallow) a request. A tool can neither
grant itself permissions nor make a request disappear.

### 6. Platform scope, stated

Unix implements this now. Windows does not get this transport in this change — a
named pipe is the parity work — so the command keeps the queued path there and the
surface does not claim the change early. The gap is written down in
[surfaces.md](../design/surfaces.md) rather than left to be discovered.

## Consequences

**Won.** The command reaches the running turn at its next decision boundary; the log
keeps one writer and one order; the protocol is untouched; no tool gains a path to
permissions.

**Paid.** A second transport to maintain and test; the surface can lead the truth by at
most one decision boundary; Windows parity is pending; and the channel must stay narrow.
A second mid-turn intent is not a reason to reuse it silently: that would be a general
control plane and needs its own decision.

**Verified.** The kernel applies a request left before a turn and one left *while the
model is answering* (`a_permissions_request_is_applied_at_the_next_decision`,
`a_permissions_request_during_a_model_call_still_allows_the_call`); the server's reader
turns a line into a request and streams the relaxation before answering the request that
flushed it (`the_control_channel_leaves_a_permissions_request_for_the_kernel`,
`a_permissions_request_between_turns_is_streamed_before_the_response`); the TUI flips on
the keypress and does not queue (`yolo_during_a_turn_flips_at_once_and_tells_the_server`),
waits for the server where no channel exists
(`yolo_without_a_control_channel_still_goes_through_the_worker`), and records the
confirmation from the event (`a_permissions_event_during_a_turn_confirms_the_immediate_yolo`).
An end-to-end run of the real binary with a real control descriptor shows
`permissions.relaxed` before the call and no `approval.requested`.

**Would reopen it:** a second capability that must act mid-turn (then decide a general
control channel, not a private one), Windows parity landing, or evidence that the
surface's one-boundary lead misleads a person.
