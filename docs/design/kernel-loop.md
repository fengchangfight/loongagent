# Kernel loop: one turn

- **Up:** [design/README.md](README.md) · [04 System overview](../04-system-overview.md)
- **Authority:** [ADR-0004](../decisions/0004-microkernel-extension-model.md) · [ADR-0006](../decisions/0006-context-engine.md)
- **Code:** `crates/kernel/src/turn.rs`, `crates/kernel/src/turn/loop.rs`, `crates/kernel/src/turn/model.rs`
- **Verify:** `cargo test -p loong-kernel` · `./scripts/check.sh`

A **turn** is one `Kernel::submit_with` call. It records the user's message, asks
the model for a completion, runs the tools the model asks for, feeds their output
back, and repeats. It ends when the model answers without a tool call, when a tool
call needs a human decision, when the person stops it, or when the step budget is
spent. Every step and every tool call is an event ([ADR-0003](../decisions/0003-jsonl-truth-sqlite-index.md)).
The ledger is the accounting path for everything the loop records; the one message
that bypasses it, the nudge correction, is named in §4 ([ADR-0006](../decisions/0006-context-engine.md)).

## 1. What a turn takes in, and what it leaves behind

`Kernel::submit_with` (`crates/kernel/src/turn/loop.rs`) takes the text the person
typed and an `on_delta` callback. `Kernel::submit` is the same path without the
callback.

| Input | Where it comes from |
|---|---|
| The prompt | Either plain text, or `/name args` expanded by `SkillSet::invoke`; an MCP prompt with no local body is fetched by `Kernel::fetch_mcp_prompt`. |
| Conversation | `Kernel.messages`: the system prompt seated once by `Kernel::seat_system_prompt`, then the history rebuilt by `Kernel::replay_history`. |
| Limits | `KernelConfig` (`max_steps`, budget, compaction thresholds), narrowed when the kernel is built. |
| Capabilities | `Kernel.tools` (`Registry`), `Kernel.pdp`, `Kernel.models` + `Kernel.router`, `Kernel.hooks`, `Kernel.skills`. |
| Preview stream | `on_delta`; the provider calls it, and it changes no record. |

| Output | Meaning |
|---|---|
| `TurnOutcome.events` | The sequence numbers this turn appended, in order. |
| `TurnOutcome.steps` | Model completions taken. On a stop it is the number *finished*, so an interrupt before the first call reports 0. |
| `TurnOutcome.final_text` | The assistant's answer, the step-limit note, or `None`. |
| `TurnOutcome.pending_approval` | The call parked for a human decision, if any. |
| `TurnOutcome.refused` | Why a hook or an unresolvable command refused the turn before any model call. |
| `TurnOutcome.interrupted` | Whether the stop flag ended the turn. |
| Side effects | Events in the JSONL log and its derived index; ledger entries and `Kernel.messages`; whatever a tool did to the workspace; `on_delta` previews. |

## 2. The step loop

`Kernel::run` is a thin wrapper: it marks the foreground busy on the context
engine (`ContextEngine::set_foreground_busy`), calls `Kernel::run_inner`, and
clears the flag on every return path. `Kernel::run_inner` is the loop itself.

One iteration is **one model completion**, counted by
`for step in 1..=self.config.max_steps`:

1. **Stop check.** If `Kernel::interrupted` is set, append `turn.interrupted` and
   return at once. The check is at the step boundary, not inside a tool, so a tool
   that has begun finishes.
2. **Context preparation.** `Kernel::refresh_map` refreshes the layered L4 map in
   place when that feature is on. `Kernel::compact_if_needed` clears stale tool
   output; when the layered engine is off it also drops the oldest exchanges. It
   appends `compaction.applied` only when something changed.
3. **Request.** `CompletionRequest { messages: self.messages.clone(), tools:
   self.tool_specs() }`. `Kernel::tool_specs` adds `recall` only when there is a
   memory behind it.
4. **Route.** `Router::select` receives `Facts { context_tokens:
   self.ledger.total_tokens(), step }`. A rule may choose a model; a name this
   session does not have falls back to `selected_model`. A change appends
   `model.selected` through `Kernel::record_model_selected`.
5. **Model call.** `Model::complete_streaming(&request, on_delta)`. A provider
   error becomes `KernelError::Model` and ends the turn. On success,
   `Kernel::record_cost` appends `cost.recorded`.
6. **No tool calls.** If the reply is text only, the loop first offers one nudge
   (§4). Otherwise `Kernel::record_assistant` appends `message.appended` and the
   turn returns the text.
7. **Tool calls.** The loop records the assistant message once, then calls
   `Kernel::dispatch` for each call in order. A dispatch that parks the turn returns
   `Dispatch::Pending`; the remaining calls in the same message are answered with
   `tool.result` events saying they did not run, because a provider requires one
   result for every call it was shown.

If the loop completes every step without an answer, it appends one assistant
`message.appended` with a "stopped after N steps" note and returns with
`steps = max_steps`. That is the step-limit ending, not a stop.

## 3. Where the four writes happen

| Write | Code | What it does |
|---|---|---|
| **Model call** | `Kernel::run_inner` → `Model::complete_streaming` | Sends `Kernel.messages` plus the tool specs. Streaming deltas are a preview: they are not logged, not ledgered, and do not change the outcome. |
| **Tool call** | `Kernel::dispatch` → `Pdp::decide` → `Kernel::execute` | The PDP returns allow / ask / deny plus the rule. `Kernel::execute` is the PEP: it builds an `ExecContext` from `Kernel.config.cwd` and `Kernel.confinement`, then calls the `Tool` in `Kernel.tools`. |
| **Ledger append** | `Kernel::record_user`, `Kernel::record_assistant`, `Kernel::record_tool_result` | Each adds a `Ledger` entry *and* pushes the matching `ChatMessage`. That lockstep is what makes `Kernel.messages` the ledger's view — with the one exception in §4. The system prompt and the L4 map are seated the same way by `Kernel::seat_system_prompt` and `Kernel::refresh_map`. `Kernel::compact_if_needed` is the only code that drops messages or clears tool output; the system prompt and the map are pinned, and the map is refreshed in place. |
| **Event append** | `Kernel::append` | Writes to `SessionLog`, mirrors the event into the derived `Index` (a failure marks `index_stale` rather than failing the turn), and calls the live `event_sink`. Every event the loop emits goes through it. |

Inside one registry tool call the events normally run: `tool.call`, any
`hook.fired` from a `pre_tool` hook, `policy.decided`, then `tool.result` and any
`post_tool` `hook.fired`. The PDP decides before `tool.call`; the hooks may tighten
the verdict, so `policy.decided` records the stricter one. An ask fires the
`permission_requested` hooks and then writes `approval.requested` in place of the
result. A `/yolo` ask records the relaxation as a second `policy.decided` and a
second `tool.call` before it executes. A `recall` call is the exception: it is
answered from the session's own record and skips both the registry and the PDP, so
it writes `tool.call` then `tool.result`.

Hooks can tighten a ruling but never widen one (`tighten` in `turn/hooks.rs`), so
the verdict in `policy.decided` is the stricter of the PDP's and the hooks'.

## 4. The step budget and the nudge

A step is a model completion, not a tool call: read, edit, run, fix is four steps.
`DEFAULT_MAX_STEPS` is 100. `KernelConfig.max_steps` is capped by the
configuration's `max_steps` with a plain `min` when the kernel is built, and
`steps_or_unlimited` maps a configured `0` to `usize::MAX`, meaning no limit. The
step limit is a guard against a model that spins; it is not the cost limit —
compaction and the token budget are.

The **nudge** exists because a model sometimes answers with a file pasted into
chat, or with a tool call printed as text. In the no-tool-call branch, before
accepting the answer:

- `leaked_tool_call` recognises a printed call and names the tool. `run_inner`
  drops a name that already ran this turn (`ran`): quoting a call you made is a
  summary, not a missed action.
- `looks_like_pasted_file` recognises a fenced whole file. `run_inner` only asks
  it when `ran.is_empty()`: after real work, code quotes are safe.

If either fires, once per turn, the loop pushes a `ChatMessage::User` correction
into `Kernel.messages` and continues. The correction is not a `message.appended`
event (the person did not say it) and adds no ledger entry. It consumes a step,
and a second offence is accepted rather than argued with. Because the correction
is in `Kernel.messages` but not the ledger, the next request carries it while
`ledger.total_tokens()` does not count it: compaction can undercount the prompt
by that message.

## 5. Interruption, approval and resume

**Interruption.** `Kernel.interrupted` is an `AtomicBool`. `Kernel::interruptor`
hands it to the server's signal handler; `Kernel::use_interruptor` lets the server
adopt a process-wide flag again after `/new`. The loop reads it only at a step
boundary. `Kernel::submit_with` clears it at the start of every new turn: a stop
asked once belongs to that turn. `Kernel::respond_to_approval` deliberately does
*not* clear it, because an approval resumes the same turn, so a stop asked while
the question was open still stands. A stopped turn appends `turn.interrupted` and
returns `interrupted: true` with no answer.

**Approval.** A `Verdict::Ask` parks the call instead of blocking: `dispatch`
appends `approval.requested` with an id (`appr-N`) and returns `Dispatch::Pending`;
`run_inner` stores the `PendingCall` in `self.pending` and puts it in
`TurnOutcome.pending_approval`. The surface asks the human.
`Kernel::respond_to_approval` checks the id, appends `approval.decided`, fires the
`permission_decided` hooks, and then either runs the call and records `tool.result`
(plus the `post_tool` hooks) or records a failed result saying the user denied it.
It then calls `Kernel::run` again, so the model sees the decision as ordinary
history. An id mismatch, or an answer with nothing pending, is
`KernelError::OutOfOrder`.

**`/yolo`.** `Kernel::set_auto_approve` records `permissions.relaxed` and flips
`Kernel.auto_approve`. The ask arm then executes instead of parking and records
that the ruling was relaxed; the sandbox is a different axis and still applies.

**Session resume is not approval resume.** `Kernel::open` rebuilds the
conversation with `Kernel::replay_history`, so the next turn sees the history it
had. The parked `PendingCall` lives only in memory (`Kernel.pending`), so a
restarted process does not resume an unanswered approval; the person starts a new
turn. The end-to-end flow is the subject of `docs/design/flows/turn-lifecycle.md`
(written in a later task; this document owns the loop, not the whole lifecycle).

## 6. How a turn ends

| Ending | What the log shows | `TurnOutcome` |
|---|---|---|
| **Answered** | The final assistant `message.appended` | `final_text`, `steps` = the step that answered |
| **Asked** | `approval.requested`, plus `tool.result` "not run" for the rest of that assistant message | `pending_approval`, no `final_text` |
| **Stopped** | `turn.interrupted` | `interrupted: true`, no `final_text`, `steps` = calls already finished |
| **Step limit** | An assistant `message.appended` with the limit note | `final_text` = the note, `steps` = `max_steps` |
| **Refused before the loop** | `skill.invoked` with an `error`, or a tightening `hook.fired` from `user_prompt_submit` | `refused`, `steps` = 0 |
| **Error** | The events written before the failure are still in the log | `Err(KernelError)`: `Model`, `Log`, `Encode`, or `OutOfOrder` |

A tool that fails is not a failed turn: its `tool.result` has `ok: false`, the
model reads the error, and the loop continues. A policy `Deny` is the same shape.
`Kernel::append` treats an index write failure as a stale index, not as a lost
event. What ends a turn with an error is the model transport, a log write, an
encoding failure, or a caller using the API out of order.

## 7. One turn, in sequence

```mermaid
sequenceDiagram
    autonumber
    participant S as Surface
    participant K as Kernel
    participant H as HookRunner
    participant R as Router
    participant M as Model
    participant P as Pdp
    participant T as Tool
    participant L as SessionLog and Ledger

    S->>K: submit_with(text, on_delta)
    K->>L: message.appended (user)
    K->>H: user_prompt_submit
    H-->>K: allow, or tighten

    loop each step, up to max_steps
        Note over K: interrupted? -> turn.interrupted and return
        K->>K: refresh_map, compact_if_needed
        K->>R: select(Facts)
        R-->>K: chosen model and rule
        K->>M: complete_streaming(request, on_delta)
        M-->>K: text and/or tool_calls
        K->>L: cost.recorded
        alt no tool calls
            alt nudge fires (once)
                K->>K: correction in messages; continue loop
            else answer stands
                K->>L: message.appended (assistant answer)
                K-->>S: TurnOutcome { final_text }
            end
        else tool calls
            K->>L: message.appended (assistant)
            loop each registry call in order
                K->>P: decide(action_class)
                P-->>K: allow / ask / deny
                K->>L: tool.call
                K->>H: pre_tool
                H-->>K: allow, or tighten
                K->>L: policy.decided (the stricter verdict)
                alt allow
                    K->>T: execute(args)
                    T-->>K: output
                    K->>L: tool.result
                    K->>H: post_tool
                else ask
                    K->>H: permission_requested
                    K->>L: approval.requested
                    K-->>S: TurnOutcome { pending_approval }
                else deny
                    K->>L: tool.result (ok = false)
                end
            end
        end
    end
```

The surface answers a parked turn with `respond_to_approval`, which writes
`approval.decided`, runs or refuses the call, and calls the same loop again; §5
gives the conditions around that resume.

## 8. Where to read next

- [ADR-0004](../decisions/0004-microkernel-extension-model.md) — why the loop is
  fixed and the edges are not.
- [ADR-0006](../decisions/0006-context-engine.md) — why the ledger, not the
  messages, is the write path to the model.
- [04 System overview](../04-system-overview.md) §3 — the four kernel-owned things.
- `crates/kernel/src/turn/tests.rs` — the behaviour pinned here; run
  `cargo test -p loong-kernel`.
