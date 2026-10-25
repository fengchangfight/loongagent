//! Purpose: Run one turn: record the prompt, call the model, dispatch tools
//! through the PDP, and stop at an answer, an approval question, a stop, or the
//! step budget.
//!
//! # Contract
//! - **Serves:** a turn a person can follow and audit: every step is an event
//!   (ADR-0003) and every gated registry tool call has a recorded verdict
//!   (ADR-0007).
//! - **Design:** `docs/design/kernel-loop.md`; ADR-0004 fixes this loop, and
//!   ADR-0006 owns the ledger it feeds.
//! - **Upstream:** `Server`'s `prompt.submit` and `approval.respond` handlers, and
//!   `Kernel::submit` for callers that do not stream.
//! - **Downstream:** `Kernel::append`, `Kernel::record_*`, `Ledger`, `Pdp`,
//!   `Registry`, `Model`, `HookRunner`, and the `Kernel::execute` PEP.
//! - **Verify:** `cargo test -p loong-kernel turn::` · `./scripts/check.sh`

use super::*;

use super::hooks::tighten;
use super::nudge::{leaked_tool_call, looks_like_pasted_file};
use super::recall::RECALL_TOOL;

/// Purpose: What a dispatched tool call did: finish, or park the turn for a human.
///
/// Serves: one assistant message whose every call gets an answer, either by
/// running it or by saying it did not run.
/// Design: `docs/design/kernel-loop.md` (the approval seam).
/// Up: `Kernel::dispatch` returns it; `Kernel::run_inner` matches it.
/// Down: `PendingCall`, `Kernel::respond_to_approval`.
/// Verify: `cargo test -p loong-kernel turn::tests::an_out_of_bounds_write_asks_then_resumes_on_the_answer`.
enum Dispatch {
    /// The call finished; every sequence it appended is already in `events`.
    Done,
    Pending(PendingCall),
}
impl Kernel {
    /// Purpose: The approval a surface must answer, if the last turn parked one.
    ///
    /// Serves: a caller that needs to know whether the session is waiting on a
    /// human before it may start another turn.
    /// Design: `docs/design/kernel-loop.md` §5.
    /// Up: a surface or status reader (no in-tree caller; `Kernel::submit_with`
    /// checks `self.pending` directly).
    /// Down: `PendingCall`, `Kernel::respond_to_approval`.
    /// Verify: `cargo test -p loong-kernel turn::tests::an_out_of_bounds_write_asks_then_resumes_on_the_answer`.
    pub fn pending(&self) -> Option<&PendingCall> {
        self.pending.as_ref()
    }

    /// Submit a user turn and run the loop until it finishes, asks, or hits the
    /// step limit.
    ///
    /// Purpose: The no-preview entry point: one user turn, run to an ending.
    /// Serves: a caller that has nowhere to stream model deltas to.
    /// Design: `docs/design/kernel-loop.md` §2 and §6.
    /// Up: callers that do not stream (in-tree `turn::tests`); the server's
    /// `prompt.submit` uses `Kernel::submit_with`.
    /// Down: `Kernel::submit_with`.
    /// Verify: `cargo test -p loong-kernel turn::tests::a_tool_runs_and_the_turn_finishes`.
    pub fn submit(&mut self, text: &str) -> Result<TurnOutcome, KernelError> {
        self.submit_with(text, &mut |_| {})
    }

    /// As [`Kernel::submit`], but reporting the model's text **as it is written**.
    ///
    /// Deltas are a preview, never the record: they are not logged, not added to the
    /// ledger, and do not change the outcome. The final `message.appended` is the
    /// answer, which is why a client that misses every delta has missed nothing.
    ///
    /// Purpose: Run one user turn, forward preview deltas, and return its ending.
    /// Serves: the interactive surface: it can show the answer forming without
    /// turning a preview into a second source of truth.
    /// Design: `docs/design/kernel-loop.md` §1–§3.
    /// Up: `Server`'s `prompt.submit` handler and `Kernel::submit`.
    /// Down: `Kernel::record_user`, `Kernel::fire_hooks`, `Kernel::run`,
    /// `Kernel::append`, and `Kernel::fetch_mcp_prompt` on the command path.
    /// Verify: `cargo test -p loong-kernel turn::tests::deltas_are_a_preview_and_never_the_record`.
    pub fn submit_with(
        &mut self,
        text: &str,
        on_delta: &mut dyn FnMut(&str),
    ) -> Result<TurnOutcome, KernelError> {
        // A stop belongs to the turn it was asked for: a person who interrupts once and
        // then asks something new has not asked for that to stop too. Cleared here rather
        // than in the loop, because the loop also resumes a turn after an approval — which
        // is the same turn, and its stop still stands.
        self.interrupted
            .store(false, std::sync::atomic::Ordering::SeqCst);
        if let Some(p) = &self.pending {
            return Err(KernelError::OutOfOrder(format!(
                "approval {} is still pending",
                p.request_id
            )));
        }
        // `/name args` is an invocation (T-18). A sentence that merely starts with
        // a path (`/etc/hosts is wrong`) is a sentence, and `parse_invocation` is
        // strict about the difference.
        let (prompt, invocation) = match parse_invocation(text) {
            Some((name, args)) => match self.skills.invoke(&name, &args) {
                Some(expansion) => {
                    let source = expansion.source.clone();
                    // An MCP prompt has no local body: fetch it from the server that
                    // owns it, and refuse the turn if that is not possible rather
                    // than sending the model a command name it cannot resolve.
                    let fetched = match expansion.prompt {
                        Some(prompt) => Ok((prompt, expansion.tokens)),
                        None => self.fetch_mcp_prompt(&expansion.name, &args),
                    };
                    match fetched {
                        Ok((prompt, tokens)) => (
                            prompt,
                            Some(gen::SkillInvoked {
                                name: expansion.name,
                                source,
                                args,
                                tokens,
                                error: None,
                            }),
                        ),
                        Err(e) => {
                            let event = self.append(
                                gen::event::SKILL_INVOKED,
                                &gen::SkillInvoked {
                                    name: expansion.name.clone(),
                                    source,
                                    args,
                                    tokens: 0,
                                    error: Some(e.clone()),
                                },
                            )?;
                            return Ok(TurnOutcome {
                                steps: 0,
                                final_text: None,
                                pending_approval: None,
                                refused: Some(format!(
                                    "/{} could not be loaded: {e}",
                                    expansion.name
                                )),
                                events: vec![event],
                                interrupted: false,
                            });
                        }
                    }
                }
                None => {
                    // An unknown command is refused rather than sent to the model as
                    // a prompt: guessing what `/reveiw` meant is not a service.
                    let event = self.append(
                        gen::event::SKILL_INVOKED,
                        &gen::SkillInvoked {
                            name: name.clone(),
                            source: "<none>".to_string(),
                            args,
                            tokens: 0,
                            error: Some(format!(
                                "no skill named {name:?}; /help lists what exists"
                            )),
                        },
                    )?;
                    return Ok(TurnOutcome {
                        steps: 0,
                        final_text: None,
                        pending_approval: None,
                        refused: Some(format!(
                            "unknown command /{name} — /help lists the skills and commands available here"
                        )),
                        events: vec![event],
                        interrupted: false,
                    });
                }
            },
            None => (text.to_string(), None),
        };
        let mut events = Vec::new();
        if let Some(invocation) = invocation {
            events.push(self.append(gen::event::SKILL_INVOKED, &invocation)?);
        }
        events.push(self.record_user(&prompt)?);
        let report = self.fire_hooks(
            HookEvent::UserPromptSubmit,
            json!({"text": text, "profile": self.config.profile}),
            &mut events,
        )?;
        if report.tightening() {
            // There is no approval seam for a prompt, so "ask" has no answerer
            // here and must not be read as consent (ADR-0007: unanswered asks
            // fail closed).
            let why = report.reason();
            return Ok(TurnOutcome {
                steps: 0,
                final_text: None,
                pending_approval: None,
                refused: Some(format!("refused by hook: {why}")),
                events,
                interrupted: false,
            });
        }
        self.run(events, on_delta)
    }

    /// Answer a pending approval; the decision is an event either way.
    ///
    /// Purpose: Record the human's decision on the parked call and resume the same
    /// turn.
    /// Serves: a turn that stopped instead of blocking, now able to finish after the
    /// person answers.
    /// Design: `docs/design/kernel-loop.md` §5.
    /// Up: `Server`'s `approval.respond` handler; `Kernel::pending` is the state it
    /// consumes.
    /// Down: `Kernel::append` (`approval.decided`), `Kernel::fire_hooks` for
    /// `permission_decided`, `Kernel::execute`, `Kernel::record_tool_result`, and
    /// `Kernel::run`.
    /// Verify: `cargo test -p loong-kernel turn::tests::a_granted_approval_still_cannot_escape_the_workspace`.
    pub fn respond_to_approval(
        &mut self,
        request_id: &str,
        granted: bool,
        scope: ApprovalScope,
        reason: Option<&str>,
    ) -> Result<TurnOutcome, KernelError> {
        let Some(pending) = self.pending.take() else {
            return Err(KernelError::OutOfOrder("no approval is pending".into()));
        };
        if pending.request_id != request_id {
            let want = pending.request_id.clone();
            self.pending = Some(pending);
            return Err(KernelError::OutOfOrder(format!(
                "{request_id} is not pending (waiting on {want})"
            )));
        }
        let mut events = vec![self.append(
            gen::event::APPROVAL_DECIDED,
            &gen::ApprovalDecided {
                request_id: request_id.to_string(),
                granted,
                scope,
                reason: reason.map(|r| r.to_string()),
            },
        )?];
        self.fire_hooks(
            HookEvent::PermissionDecided,
            json!({
                "request_id": request_id,
                "tool": pending.tool,
                "granted": granted,
                "scope": scope,
            }),
            &mut events,
        )?;
        if granted {
            let (ok, output) = self.execute(&pending.tool, &pending.args);
            events.push(self.record_tool_result(&pending.call_id, ok, &output)?);
            let tool_name = pending.tool.clone();
            let call_id = pending.call_id.clone();
            // The same seam as the un-gated path: a tool that ran after an
            // approval is still a tool that ran.
            self.fire_hooks(
                HookEvent::PostTool,
                json!({"tool": tool_name, "call_id": call_id, "ok": ok, "output": output}),
                &mut events,
            )?;
        } else {
            let output = match reason {
                Some(r) => format!("denied by the user: {r}"),
                None => "denied by the user".to_string(),
            };
            events.push(self.record_tool_result(&pending.call_id, false, &output)?);
        }
        // Resuming after an approval has no observer: the surface that answered the
        // question already has the flow, and a new one would have to attach first.
        self.run(events, &mut |_| {})
    }

    // ------------------------------------------------------------- the loop

    /// Run one turn while telling the context engine that the foreground is busy.
    ///
    /// The busy flag is what keeps background enrichment off the critical path
    /// (ADR-0017 §4.6): the worker's own thread reads it and never opens a window
    /// while a turn is running. Every path through [`Kernel::run_inner`] — an
    /// answer, a stop, an approval question — clears it, including the error paths,
    /// because a flag left set would starve the background for the session's life.
    ///
    /// Purpose: Run one turn with the context engine told the foreground is busy.
    /// Serves: a background worker that must not enrich context in the middle of a
    /// turn.
    /// Design: `docs/design/kernel-loop.md` §2.
    /// Up: `Kernel::submit_with` and `Kernel::respond_to_approval`.
    /// Down: `Kernel::run_inner`, `ContextEngine::set_foreground_busy`.
    /// Verify: `cargo test -p loong-kernel turn::`.
    fn run(
        &mut self,
        events: Vec<u64>,
        on_delta: &mut dyn FnMut(&str),
    ) -> Result<TurnOutcome, KernelError> {
        let engine = self.context_engine.as_ref();
        if let Some(engine) = engine {
            engine.set_foreground_busy(true);
        }
        let result = self.run_inner(events, on_delta);
        if let Some(engine) = self.context_engine.as_ref() {
            engine.set_foreground_busy(false);
        }
        result
    }

    /// Purpose: The step loop: stop check, context preparation, routing, one model
    /// call, and the tool fan-out.
    ///
    /// Serves: one turn that advances only by complete steps, so a stop or an
    /// approval leaves no half-executed state.
    /// Design: `docs/design/kernel-loop.md` §2, §4 and §6.
    /// Up: `Kernel::run`.
    /// Down: `Kernel::interrupted`, `Kernel::refresh_map`,
    /// `Kernel::compact_if_needed`, `Kernel::tool_specs`, `Router::select`,
    /// `Model::complete_streaming`, `Kernel::record_model_selected`,
    /// `Kernel::record_cost`, `Kernel::record_assistant`, `Kernel::dispatch`,
    /// `leaked_tool_call`, `looks_like_pasted_file`.
    /// Verify: `cargo test -p loong-kernel turn::tests::the_step_limit_stops_a_spinning_model`.
    fn run_inner(
        &mut self,
        mut events: Vec<u64>,
        on_delta: &mut dyn FnMut(&str),
    ) -> Result<TurnOutcome, KernelError> {
        // Which tools have actually run this turn, and whether we have already objected
        // to a reply that only *described* the change. Both exist for one measured
        // failure: a local model asked to write a file pasted the file into the reply
        // instead, and the turn ended as if it had succeeded. The names (not a bool)
        // matter for the *printed call* variant: a model that read a file and then
        // printed `write_file` must still be corrected, while one that wrote the file and
        // then quoted the call in its summary must not be asked to write it again.
        let mut ran: Vec<String> = Vec::new();
        let mut nudged = false;
        for step in 1..=self.config.max_steps {
            // A permissions change asked for from outside is applied here, before the
            // step's model call and its tool decisions: the question this step would
            // have asked must never be asked after the person already said yes.
            self.apply_requested_permissions()?;
            // A stop asked for between steps stops here: after the last tool finished,
            // before the next model call. Nothing half-done is left behind.
            if self.interrupted() {
                events.push(self.append(
                    gen::event::TURN_INTERRUPTED,
                    &gen::TurnInterrupted {
                        reason: "interrupted".to_string(),
                    },
                )?);
                return Ok(TurnOutcome {
                    steps: step - 1,
                    final_text: None,
                    pending_approval: None,
                    refused: None,
                    events,
                    interrupted: true,
                });
            }
            self.refresh_map();
            self.compact_if_needed(&mut events)?;
            let request = CompletionRequest {
                messages: self.messages.clone(),
                tools: self.tool_specs(),
            };
            // Routing happens here, per call, from facts the session can explain —
            // and a change of model is an event, not a hidden optimisation.
            let facts = loong_config::Facts {
                context_tokens: self.ledger.total_tokens(),
                step: step as u64,
            };
            let (chosen, rule) = self.router.select(&facts);
            let chosen = chosen
                .filter(|name| self.models.contains_key(*name))
                .unwrap_or(self.selected_model.as_str())
                .to_string();
            if chosen != self.selected_model {
                events.push(self.record_model_selected(
                    &chosen,
                    rule.map(|r| r.condition.render()),
                    &facts,
                )?);
            }
            let response = self
                .models
                .get_mut(&chosen)
                .expect("the chosen model exists")
                .complete_streaming(&request, on_delta)
                .map_err(KernelError::Model)?;
            events.push(self.record_cost(&response.usage)?);

            if response.tool_calls.is_empty() {
                let text = response.text.unwrap_or_default();
                // A reply that pastes a file, or prints a tool call, and never *calls* a
                // tool is not a finished task. Object once, in as many words, and give the
                // model a turn to act; if it does it again, accept the answer and let the
                // person see what happened rather than looping.
                if !nudged {
                    // Two shapes of the same failure, and they need different words. A
                    // printed call (measured with `qwen2.5:7b-instruct` on Ollama's
                    // OpenAI shim: `{"name":"write_file",…}` inside `content`) named the
                    // tool it wanted, so the correction can name it too; a pasted file
                    // names nothing and the correction has to say `write_file` itself.
                    let printed = leaked_tool_call(&text).filter(|tool| {
                        // Printing a call for a tool that already ran this turn is a
                        // summary of what happened, not an attempt that failed.
                        !ran.iter().any(|done| done == tool)
                    });
                    // A pasted file is only suspect before anything has run: afterwards a
                    // reply may quote code freely, which is the guard this keeps.
                    let pasted =
                        printed.is_none() && ran.is_empty() && looks_like_pasted_file(&text);
                    if printed.is_some() || pasted {
                        nudged = true;
                        let correction = match &printed {
                            Some(tool) => format!(
                                "You wrote the `{tool}` call as text — a `<tool_call>` block or \
                                 JSON in your reply — instead of emitting a real tool call, so \
                                 nothing ran and the workspace is unchanged. Emit `{tool}` now \
                                 as an actual tool call with the arguments you intended. Do not \
                                 print the call and do not wrap it in `<tool_call>`: printing it \
                                 is what made it do nothing."
                            ),
                            None => "You answered with the file's contents in chat and did not \
                                     call a tool, so nothing was written to the workspace. Call \
                                     `write_file` now with the full content in the `content` \
                                     argument, then tell me the path you wrote."
                                .to_string(),
                        };
                        self.messages.push(ChatMessage::User { text: correction });
                        // Not recorded as a message event: this is the harness
                        // course-correcting, not something the person said. The retry's
                        // assistant message is what the log will show, which is the answer
                        // that actually mattered.
                        continue;
                    }
                }
                events.push(self.record_assistant(Some(&text), &[])?);
                return Ok(TurnOutcome {
                    steps: step,
                    final_text: Some(text),
                    pending_approval: None,
                    refused: None,
                    events,
                    interrupted: false,
                });
            }

            for call in &response.tool_calls {
                ran.push(call.tool.clone());
            }
            events.push(self.record_assistant(response.text.as_deref(), &response.tool_calls)?);
            for (at, call) in response.tool_calls.iter().enumerate() {
                match self.dispatch(call, &mut events)? {
                    Dispatch::Done => {}
                    Dispatch::Pending(pending) => {
                        // The rest of this message was never dispatched, and the provider
                        // requires an answer for **every** call it was told about: a request
                        // carrying one assistant message with three calls and one result is
                        // rejected outright, which is how a model that batches its calls — and
                        // asks for permission on the first of them — breaks its own session.
                        // Say they did not run; the model re-issues what it still wants.
                        for skipped in &response.tool_calls[at + 1..] {
                            events.push(self.record_tool_result(
                                &skipped.call_id,
                                false,
                                &format!(
                                    "not run: the turn stopped to ask about {} first",
                                    pending.request_id
                                ),
                            )?);
                        }
                        self.pending = Some(pending.clone());
                        return Ok(TurnOutcome {
                            steps: step,
                            final_text: None,
                            pending_approval: Some(pending),
                            refused: None,
                            events,
                            interrupted: false,
                        });
                    }
                }
            }
        }
        let note = format!(
            "stopped after {} steps without finishing (step limit) — reply to continue, or \
             raise `max_steps` in the configuration",
            self.config.max_steps
        );
        events.push(self.record_assistant(Some(&note), &[])?);
        Ok(TurnOutcome {
            steps: self.config.max_steps,
            final_text: Some(note),
            pending_approval: None,
            refused: None,
            events,
            interrupted: false,
        })
    }

    /// Purpose: Turn one model tool call into a recorded verdict and, unless it
    /// asks or is denied, an execution.
    ///
    /// Serves: the single tool chokepoint: the PDP decides here, and the decision
    /// plus the rule that produced it become events.
    /// Design: `docs/design/kernel-loop.md` §3; ADR-0007 owns the allow/ask/deny
    /// ladder.
    /// Up: `Kernel::run_inner`, once per call in the assistant message.
    /// Down: `Kernel::recall_tool`, `Pdp::decide`, `tighten`,
    /// `Kernel::fire_hooks`, `Kernel::append`, `Kernel::execute`,
    /// `Kernel::record_tool_result`.
    /// Verify: `cargo test -p loong-kernel turn::tests::a_tightened_policy_denies_without_asking`.
    fn dispatch(
        &mut self,
        call: &ToolCallRequest,
        events: &mut Vec<u64>,
    ) -> Result<Dispatch, KernelError> {
        // The last boundary before a verdict: a request that arrived while the model was
        // returning this call applies now, so the call is allowed rather than asked about.
        self.apply_requested_permissions()?;
        // `recall` is answered from this session's derived memory rather than from
        // the filesystem, so it is not in the registry — but it is still a tool the
        // model may call. It bypasses the registry *and* the PDP, on purpose: it is
        // read-only over the session's own record, which is data the model has
        // already been given in full, so there is nothing to gate. Doing this before
        // the registry lookup is what makes the tool exist at all; without it the
        // model would be told the tool is unknown.
        if call.tool == RECALL_TOOL {
            events.push(self.append(
                gen::event::TOOL_CALL,
                &gen::ToolCall {
                    call_id: call.call_id.clone(),
                    tool: RECALL_TOOL.to_string(),
                    args: call.args.clone(),
                    action_class: ActionClass::Investigate,
                },
            )?);
            let (ok, output) = self.recall_tool(&call.args);
            events.push(self.record_tool_result(&call.call_id, ok, &output)?);
            return Ok(Dispatch::Done);
        }
        let Some(tool) = self.tools.get(&call.tool) else {
            events.push(self.record_tool_result(
                &call.call_id,
                false,
                &format!("unknown tool {:?}", call.tool),
            )?);
            return Ok(Dispatch::Done);
        };
        let tool_name = tool.name().to_string();
        let class = tool.action_class(&call.args, &self.config.cwd);
        let mut ruling = self.pdp.decide(class, &tool_name);

        // The attempted call, then the hooks' answers, then the verdict, then
        // whatever happened.
        events.push(self.append(
            gen::event::TOOL_CALL,
            &gen::ToolCall {
                call_id: call.call_id.clone(),
                tool: tool_name.clone(),
                args: call.args.clone(),
                action_class: class,
            },
        )?);
        let report = self.fire_hooks(
            HookEvent::PreTool,
            json!({
                "tool": tool_name,
                "call_id": call.call_id,
                "args": call.args,
                "action_class": class,
                "policy_verdict": ruling.verdict,
                "policy_rule": ruling.rule,
            }),
            events,
        )?;
        // Hooks tighten, never widen (ADR-0008): their verdict replaces the
        // policy's only when it is stricter.
        if let Some(tighter) = tighten(&ruling, &report) {
            ruling = tighter;
        }
        events.push(self.append(
            gen::event::POLICY_DECIDED,
            &gen::PolicyDecided {
                action_class: class,
                verdict: ruling.verdict,
                rule: ruling.rule.clone(),
            },
        )?);

        match ruling.verdict {
            Verdict::Allow => {
                let (ok, output) = self.execute(&call.tool, &call.args);
                events.push(self.record_tool_result(&call.call_id, ok, &output)?);
                // Post hooks cannot undo anything; their answers are recorded so
                // the guard's behaviour stays auditable.
                self.fire_hooks(
                    HookEvent::PostTool,
                    json!({"tool": tool_name, "call_id": call.call_id, "ok": ok, "output": output}),
                    events,
                )?;
                Ok(Dispatch::Done)
            }
            // Opted out: the ruling is allowed instead of asking, and the event says the
            // autonomy was relaxed rather than that the policy changed.
            // Opted out: the ruling is allowed instead of asking, and the event says the
            // autonomy was relaxed rather than that the policy changed (ADR-0007 §5).
            Verdict::Ask if self.auto_approve => {
                events.push(self.append(
                    gen::event::POLICY_DECIDED,
                    &json!({
                        "verdict": Verdict::Allow,
                        "rule": format!("yolo:{class:?}"),
                        "tool": tool_name,
                    }),
                )?);
                events.push(self.append(
                    gen::event::TOOL_CALL,
                    &gen::ToolCall {
                        call_id: call.call_id.clone(),
                        tool: tool_name.clone(),
                        args: call.args.clone(),
                        action_class: class,
                    },
                )?);
                let (ok, output) = self.execute(&call.tool, &call.args);
                events.push(self.record_tool_result(&call.call_id, ok, &output)?);
                self.fire_hooks(
                    HookEvent::PostTool,
                    json!({"tool": tool_name, "call_id": call.call_id, "ok": ok, "output": output}),
                    events,
                )?;
                Ok(Dispatch::Done)
            }
            Verdict::Ask => {
                let request_id = format!("appr-{}", self.approvals + 1);
                self.approvals += 1;
                self.fire_hooks(
                    HookEvent::PermissionRequested,
                    json!({
                        "request_id": request_id,
                        "tool": tool_name,
                        "action_class": class,
                        "reason": ruling.reason,
                    }),
                    events,
                )?;
                events.push(self.append(
                    gen::event::APPROVAL_REQUESTED,
                    &gen::ApprovalRequested {
                        request_id: request_id.clone(),
                        action_class: class,
                        reason: ruling.reason.clone(),
                    },
                )?);
                Ok(Dispatch::Pending(PendingCall {
                    request_id,
                    call_id: call.call_id.clone(),
                    tool: tool_name,
                    args: call.args.clone(),
                    class,
                    rule: ruling.rule,
                    reason: ruling.reason,
                }))
            }
            Verdict::Deny => {
                let output = format!("denied by policy ({}): {}", ruling.rule, ruling.reason);
                events.push(self.record_tool_result(&call.call_id, false, &output)?);
                Ok(Dispatch::Done)
            }
        }
    }

    /// The PEPs: the tool runner (which refuses to write outside the workspace)
    /// and whatever confinement the kernel was given, applied to each child.
    ///
    /// Purpose: Run a tool through the registry and the confinement policy, and
    /// report success or the tool's error.
    /// Serves: a tool call that has been allowed, or granted after asking, actually
    /// runs with the same PEP on both paths.
    /// Design: `docs/design/kernel-loop.md` §3; ADR-0007 owns enforcement.
    /// Up: `Kernel::dispatch` (allow and `/yolo` arms) and
    /// `Kernel::respond_to_approval` (granted).
    /// Down: `Registry::get`, `Tool::execute`, `ExecContext::new`,
    /// `Kernel::recall_tool`.
    /// Verify: `cargo test -p loong-kernel turn::tests::a_granted_approval_still_cannot_escape_the_workspace`.
    fn execute(&self, tool: &str, args: &Json) -> (bool, String) {
        // `recall` is not in the registry: it answers from this session's derived
        // memory rather than from the filesystem, so it needs state the registry
        // deliberately has none of. It is still a tool from the model's side, and it
        // is still reported the same way — as a `tool.result` event.
        //
        // It deliberately does **not** add a `Category::Retrieval` ledger entry: the
        // answer becomes a tool-result message, which the ledger already counts. A
        // second entry for the same bytes would double-count them against the
        // context budget, which is the one number a person uses to decide whether
        // the session is affordable.
        if tool == RECALL_TOOL {
            return self.recall_tool(args);
        }
        match self.tools.get(tool) {
            Some(t) => {
                let ctx = ExecContext::new(&self.config.cwd, self.confinement.as_ref());
                match t.execute(args, &ctx) {
                    Ok(out) => (true, out),
                    Err(e) => (false, e.to_string()),
                }
            }
            None => (false, format!("unknown tool {tool:?}")),
        }
    }

    /// Turn approval prompts off or on. Recorded, because a session that never asks is a
    /// session whose log has to say why (ADR-0007 §5).
    ///
    /// Purpose: Set the session's auto-approve flag and keep the log honest about it.
    /// Serves: a person who asks for `/yolo` and can still see afterwards that the
    /// ask step was relaxed.
    /// Design: `docs/design/kernel-loop.md` §5.
    /// Up: `Server`'s `permissions.set` handler.
    /// Down: `Kernel::append` (`permissions.relaxed`), `Kernel::auto_approve`.
    /// Verify: `cargo test -p loong-kernel turn::tests::opting_out_of_prompts_allows_the_action_and_says_so_in_the_log`.
    pub fn set_auto_approve(&mut self, on: bool) -> Result<(), KernelError> {
        if self.auto_approve == on {
            return Ok(());
        }
        self.auto_approve = on;
        self.append(
            gen::event::PERMISSIONS_RELAXED,
            &gen::PermissionsRelaxed {
                auto_approve: on,
                reason: "the person asked for it with /yolo".to_string(),
            },
        )?;
        Ok(())
    }

    /// Purpose: Whether an `ask` ruling is being treated as an allow.
    ///
    /// Serves: a status surface that must show the session is in `/yolo` mode.
    /// Design: `docs/design/kernel-loop.md` §5.
    /// Up: a surface or status reader (no in-tree caller; `Kernel::dispatch`
    /// reads `self.auto_approve` directly).
    /// Down: `Kernel::set_auto_approve` is the only setter.
    /// Verify: `cargo test -p loong-kernel turn::tests::opting_out_of_prompts_allows_the_action_and_says_so_in_the_log`.
    pub fn auto_approve(&self) -> bool {
        self.auto_approve
    }

    /// Purpose: Hand out the slot a control channel writes a permissions request into.
    /// Serves: `Server::with_permissions`, whose reader thread receives the surface's
    /// `/yolo` while a turn owns the connection.
    /// Design: `docs/design/kernel-loop.md` §5; `docs/design/flows/tool-approval.md`.
    /// Up: `Server::with_permissions` and the server's `session.new` handler.
    /// Down: `Kernel::permissions_request`, read by `apply_requested_permissions`.
    /// Verify: `cargo test -p loong-kernel turn::tests::a_permissions_request_is_applied_at_the_next_decision`.
    pub fn permissions_requestor(&self) -> std::sync::Arc<std::sync::Mutex<Option<bool>>> {
        std::sync::Arc::clone(&self.permissions_request)
    }

    /// Purpose: Adopt a process-owned permissions request slot, so `/new` does not leave the
    /// session with an unreachable one.
    /// Serves: a server that owns one control channel for every kernel it installs.
    /// Design: `docs/design/kernel-loop.md` §5.
    /// Up: `Server::with_permissions` and the server's `session.new` handler.
    /// Down: `Kernel::permissions_request`.
    /// Verify: `cargo test -p loong-kernel turn::tests::a_permissions_request_is_applied_at_the_next_decision`.
    pub fn use_permissions_requester(
        &mut self,
        slot: std::sync::Arc<std::sync::Mutex<Option<bool>>>,
    ) {
        self.permissions_request = slot;
    }

    /// Purpose: Apply a permissions request that arrived from outside the turn, at the next
    /// boundary where no work is half-done.
    /// Serves: `/yolo` reaching a running turn instead of waiting for it to end.
    /// Design: `docs/design/kernel-loop.md` §5; `docs/design/flows/tool-approval.md`.
    /// Up: `Kernel::run_inner`, `Kernel::dispatch` and `Server::dispatch`.
    /// Down: `Kernel::set_auto_approve`.
    /// Verify: `cargo test -p loong-kernel turn::tests::a_permissions_request_is_applied_at_the_next_decision`.
    pub fn apply_requested_permissions(&mut self) -> Result<(), KernelError> {
        let asked = self
            .permissions_request
            .lock()
            .ok()
            .and_then(|mut slot| slot.take());
        if let Some(on) = asked {
            self.set_auto_approve(on)?;
        }
        Ok(())
    }

    /// The flag a signal handler sets to stop the running turn. Handing it out is how the
    /// server lets a person interrupt without a protocol message: the process is ours, and
    /// Ctrl-C already means "stop what you are doing".
    ///
    /// Purpose: Hand out the stop flag so a process-wide signal handler can set it.
    /// Serves: Ctrl-C stopping the turn without a protocol message or a second
    /// thread inside the kernel.
    /// Design: `docs/design/kernel-loop.md` §5.
    /// Up: `Server::catch_interrupts`.
    /// Down: `Kernel::interrupted`, `std::sync::atomic::AtomicBool`.
    /// Verify: `cargo test -p loong-kernel turn::tests::a_turn_stops_at_the_next_boundary_and_the_log_says_who_stopped_it`.
    pub fn interruptor(&self) -> std::sync::Arc<std::sync::atomic::AtomicBool> {
        std::sync::Arc::clone(&self.interrupted)
    }

    /// Use a flag someone else made. The server needs this: a signal handler can only
    /// reach a value that lives for the process, so the process owns it and the kernel
    /// borrows it.
    ///
    /// Purpose: Adopt a process-owned stop flag, so `/new` does not leave the session
    /// with an unreachable one.
    /// Serves: a server that owns one interrupt flag for every kernel it installs.
    /// Design: `docs/design/kernel-loop.md` §5.
    /// Up: `Server::catch_interrupts` and the server's `session.new` handler.
    /// Down: `Kernel::interrupted`.
    /// Verify: `cargo test -p loong-kernel turn::tests::a_turn_stops_at_the_next_boundary_and_the_log_says_who_stopped_it`.
    pub fn use_interruptor(&mut self, flag: std::sync::Arc<std::sync::atomic::AtomicBool>) {
        self.interrupted = flag;
    }

    /// Purpose: Whether a stop has been asked for.
    ///
    /// Serves: the loop's only interruption check, at a step boundary.
    /// Design: `docs/design/kernel-loop.md` §5.
    /// Up: `Kernel::run_inner` reads it once per step.
    /// Down: `Kernel.interrupted`, set through `Kernel::interruptor` or
    /// `Kernel::use_interruptor`.
    /// Verify: `cargo test -p loong-kernel turn::tests::a_turn_stops_at_the_next_boundary_and_the_log_says_who_stopped_it`.
    pub(super) fn interrupted(&self) -> bool {
        self.interrupted.load(std::sync::atomic::Ordering::SeqCst)
    }
}
