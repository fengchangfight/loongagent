# Flow: one turn

- **Up:** [kernel-loop.md](../kernel-loop.md) · [design/README.md](../README.md)
- **Authority:** [ADR-0004](../../decisions/0004-microkernel-extension-model.md) · [ADR-0006](../../decisions/0006-context-engine.md)
- **Code:** `crates/kernel/src/turn/loop.rs`, `crates/kernel/src/turn.rs`
- **Verify:** `cargo test -p loong-kernel` · `./scripts/check.sh`

A turn is one `Kernel::submit_with` call. This page keeps the end-to-end sequence and
the event or function at each hop. The step loop, the endings and the approval seam
are owned by [kernel-loop.md](../kernel-loop.md); the tool gate by
[tool-approval.md](tool-approval.md). Event kinds are in `crates/schema/protocol.toml`.

## 1. The sequence in brief

1. **Intent.** `submit_with` clears the stop flag, refuses a pending approval
   (`KernelError::OutOfOrder`), expands a `/name` command via `SkillSet::invoke`
   (appending `skill.invoked`; a failed expansion is `TurnOutcome.refused`), writes
   the user `message.appended` through `Kernel::record_user`, and runs the
   `user_prompt_submit` hooks (`hook.fired`). A tightening hook refuses the turn
   before any model call.
2. **Context.** At each step `run_inner` checks the stop flag (`turn.interrupted`),
   calls `refresh_map` when the engine is on (no event), then `compact_if_needed`
   (`compaction.applied` only when it changed something).
3. **Model.** `Router::select(Facts { context_tokens, step })` may write
   `model.selected`; `Model::complete_streaming` answers; `record_cost` writes
   `cost.recorded`.
4. **Proposal.** A text-only answer takes the one nudge or becomes the assistant
   `message.appended`. Tool calls record the assistant message once and go through
   `Kernel::dispatch`; the gate is [tool-approval.md](tool-approval.md).
5. **Result and next step.** The results go back into the conversation and the loop
   takes another step. An `Ask` parks the turn (`approval.requested`); a failed or
   denied call is an ordinary `tool.result` with `ok: false`.
6. **End.** Answered, asked, stopped, step limit, refused or error. The six endings
   and their `TurnOutcome` fields are [kernel-loop.md](../kernel-loop.md) §6.

The one nudge detail the sequence needs: the correction is pushed into
`Kernel.messages`, is not a `message.appended` event and adds no ledger entry, and it
consumes a step — so an answer after a nudge comes from a later completion, not from
the reply that triggered it.

## 2. One turn, in sequence

```mermaid
sequenceDiagram
    autonumber
    participant S as Surface
    participant K as Kernel
    participant H as HookRunner
    participant E as ContextEngine
    participant R as Router
    participant M as Model
    participant P as Pdp
    participant T as Tool
    participant L as SessionLog

    S->>K: submit_with(text, on_delta)
    K->>K: parse_invocation / SkillSet::invoke
    K->>L: skill.invoked (command path)
    K->>L: message.appended (user)
    K->>H: fire_hooks(user_prompt_submit)
    H-->>K: allow, or tighten
    K->>L: hook.fired per answer
    Note over K: tighten -> TurnOutcome.refused, no model call
    K->>E: set_foreground_busy(true)
    loop each step, up to max_steps
        alt stopped
            K->>L: turn.interrupted
            K-->>S: TurnOutcome { interrupted: true }
        end
        K->>E: refresh_map (engine on)
        K->>L: compaction.applied (only if changed)
        K->>R: select(Facts { context_tokens, step })
        K->>L: model.selected (only if the model changed)
        K->>M: complete_streaming(request, on_delta)
        M-->>K: text and/or tool_calls
        K->>L: cost.recorded
        alt no tool calls
            alt nudge fires (once)
                K->>K: correction in messages; continue loop
            else answer stands
                K->>L: message.appended (assistant)
                K-->>S: TurnOutcome { final_text }
            end
        else tool calls
            K->>L: message.appended (assistant, once)
            loop each call in order
                K->>P: decide(action_class)
                K->>L: tool.call
                K->>H: pre_tool
                K->>L: hook.fired, then policy.decided
                alt allow
                    K->>T: execute(args, ExecContext)
                    K->>L: tool.result
                    K->>H: post_tool
                    K->>L: hook.fired
                else ask
                    K->>L: permission_requested hook.fired, approval.requested
                    K-->>S: TurnOutcome { pending_approval }
                else deny
                    K->>L: tool.result (ok = false)
                end
            end
        end
    end
    Note over S,K: a parked turn resumes with respond_to_approval:<br/>approval.decided, run or refuse, then run_inner again
```

`Kernel::run` clears the foreground-busy flag on every return path. The surface answers
a parked turn with `approval.respond`; the kernel resumes the same turn.

## 3. Where to read next

- [kernel-loop.md](../kernel-loop.md) §2, §5 and §6 — the mechanism behind each hop.
- [tool-approval.md](tool-approval.md) — the gate and its resume in detail.
- [context-ledger.md](../context-ledger.md) §4 — what `compact_if_needed` changes.
- [events-storage.md](../events-storage.md) — the envelope every event is written in.
- `crates/kernel/src/turn/tests.rs` — the endings above, pinned.
