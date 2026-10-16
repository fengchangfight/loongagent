use super::*;

impl Kernel {
    /// Rebuild the conversation the log already holds, plus any ancestor prefix.
    ///
    /// The log is the record (ADR-0003), so a kernel that starts with empty `messages`
    /// continues the session *and forgets it*: the model is asked to carry on a
    /// conversation it cannot see. This is the fix, and it is also what ADR-0015's
    /// lineage needs — a child's context is its ancestors' prefix stitched to its own
    /// events, and both are replayed by exactly this code.
    pub(super) fn replay_history(&mut self) -> Result<(), KernelError> {
        let chain = self.lineage()?;
        let depth = chain.len();
        for (index, (path, upto)) in chain.into_iter().enumerate() {
            // History that came from an ancestor says so (ADR-0015): the panel can then
            // answer "where did this come from?" for a session that inherited it.
            let from = (index + 1 < depth).then(|| {
                path.file_stem()
                    .map(|stem| {
                        let stem = stem.to_string_lossy().to_string();
                        if stem == "session" {
                            "default".to_string()
                        } else {
                            stem
                        }
                    })
                    .unwrap_or_else(|| "ancestor".to_string())
            });
            let replay = loong_eventlog::replay_lenient(&path)?;
            // A turn with several tool calls writes **one** assistant message and then
            // each call's events in order (call, verdict, result, call, verdict, result…),
            // so calls and results both accumulate here and are flushed together when the
            // tool phase ends. Flushing at the first result would strand the second call.
            type Pending = (
                Option<String>,
                Vec<loong_providers::ToolCallRequest>,
                Vec<(String, String)>,
            );
            let mut pending: Option<Pending> = None;
            for event in replay.events {
                if event.seq > upto {
                    break;
                }
                match event.kind.as_str() {
                    gen::event::SESSION_FORKED => {}
                    gen::event::MESSAGE_APPENDED => {
                        // The assistant message and its tool calls are recorded as
                        // separate events (the call payloads carry the args), so an
                        // assistant turn is held back until its calls are known — the
                        // alternative is a tool result with no matching call, which is
                        // not a conversation a provider will accept.
                        self.flush_pending(&mut pending)?;
                        let appended: gen::MessageAppended =
                            serde_json::from_value(event.data.clone()).map_err(|e| {
                                KernelError::Session(format!(
                                    "{} at seq {} does not match its kind: {e}",
                                    event.kind, event.seq
                                ))
                            })?;
                        match appended.role {
                            gen::Role::User => {
                                self.messages.push(ChatMessage::User {
                                    text: appended.text.clone(),
                                });
                                let source = match &from {
                                    Some(ancestor) => format!("user from {ancestor}"),
                                    None => "user".to_string(),
                                };
                                let entry = self.ledger.add(
                                    Category::History,
                                    &source,
                                    &appended.text,
                                    Lifetime::Session,
                                    false,
                                );
                                self.entry_of.push(Some(entry));
                            }
                            gen::Role::Assistant => {
                                pending = Some((
                                    (!appended.text.is_empty()).then(|| appended.text.clone()),
                                    Vec::new(),
                                    Vec::new(),
                                ));
                            }
                            // The log records only what the conversation holds: a user
                            // turn and an assistant turn. Anything else here is a
                            // schema change this replay has not been taught.
                            other => {
                                return Err(KernelError::Session(format!(
                                    "message.appended at seq {} has role {other:?}, which is \
                                     not part of a conversation",
                                    event.seq
                                )));
                            }
                        }
                    }
                    gen::event::TOOL_CALL => {
                        let call: gen::ToolCall = serde_json::from_value(event.data.clone())
                            .map_err(|e| {
                                KernelError::Session(format!(
                                    "tool.call at seq {} is malformed: {e}",
                                    event.seq
                                ))
                            })?;
                        // Attach it to the assistant turn it belongs to. A call with no
                        // assistant message in front of it still happened — an older
                        // writer, or a hand-edited log — so it gets the turn it needs
                        // rather than making the session unopenable.
                        let slot = pending.get_or_insert_with(|| (None, Vec::new(), Vec::new()));
                        slot.1.push(loong_providers::ToolCallRequest {
                            call_id: call.call_id.clone(),
                            tool: call.tool.clone(),
                            args: call.args.clone(),
                        });
                    }
                    gen::event::TOOL_RESULT => {
                        let result: gen::ToolResult = serde_json::from_value(event.data.clone())
                            .map_err(|e| {
                                KernelError::Session(format!(
                                    "{} at seq {} does not match its kind: {e}",
                                    event.kind, event.seq
                                ))
                            })?;
                        // Held back until the tool phase ends: the assistant message
                        // comes first in the conversation, and it must carry every call
                        // this turn made — not just the first.
                        if let Some(slot) = pending.as_mut() {
                            slot.2.push((result.call_id.clone(), result.output.clone()));
                        }
                    }
                    gen::event::COMPACTION_APPLIED => {
                        // Compaction clears tool output, so the state it acts on has to be
                        // complete before it is replayed.
                        self.flush_pending(&mut pending)?;
                        let applied: gen::CompactionApplied =
                            serde_json::from_value(event.data.clone()).map_err(|e| {
                                KernelError::Session(format!(
                                    "{} at seq {} does not match its kind: {e}",
                                    event.kind, event.seq
                                ))
                            })?;
                        self.replay_compaction(&applied);
                    }
                    // Provenance, cost, approvals, hooks: records of what happened
                    // around the conversation, not parts of it.
                    _ => {}
                }
            }
            self.flush_pending(&mut pending)?;
        }
        Ok(())
    }

    /// Push the assistant turn that was being held, now that its calls are known.
    fn flush_pending(
        &mut self,
        pending: &mut Option<(
            Option<String>,
            Vec<loong_providers::ToolCallRequest>,
            Vec<(String, String)>,
        )>,
    ) -> Result<(), KernelError> {
        if let Some((text, calls, results)) = pending.take() {
            self.push_assistant(None, text, &calls);
            for (call_id, output) in results {
                self.push_tool_result(None, &call_id, &output);
            }
        }
        Ok(())
    }

    /// The same ledger accounting, message push and index bookkeeping the live turn
    /// does — one implementation, so a replayed session and a live one cannot drift.
    fn push_assistant(
        &mut self,
        from: Option<&str>,
        text: Option<String>,
        calls: &[loong_providers::ToolCallRequest],
    ) {
        let accounting = match (text.as_deref(), calls.first()) {
            (Some(t), None) => t.to_string(),
            (Some(t), Some(_)) => {
                format!("{t}{}", serde_json::to_string(calls).unwrap_or_default())
            }
            (None, Some(_)) => serde_json::to_string(calls).unwrap_or_default(),
            (None, None) => String::new(),
        };
        let source = match from {
            Some(ancestor) => format!("assistant from {ancestor}"),
            None => "assistant".to_string(),
        };
        let entry = self.ledger.add(
            Category::History,
            &source,
            &accounting,
            Lifetime::Session,
            false,
        );
        self.entry_of.push(Some(entry));
        self.messages.push(ChatMessage::Assistant {
            text,
            tool_calls: calls.to_vec(),
        });
    }

    fn push_tool_result(&mut self, from: Option<&str>, call_id: &str, output: &str) {
        let source = match from {
            Some(ancestor) => format!("tool:{call_id} from {ancestor}"),
            None => format!("tool:{call_id}"),
        };
        let entry = self
            .ledger
            .add(Category::History, &source, output, Lifetime::Turn, false);
        self.entry_of.push(Some(entry));
        self.messages.push(ChatMessage::Tool {
            call_id: call_id.to_string(),
            output: output.to_string(),
        });
    }

    /// Reproduce a compaction exactly: the event records how many tool outputs were
    /// cleared and how many messages dropped, so a replay does not have to guess at a
    /// policy that may since have changed.
    fn replay_compaction(&mut self, applied: &gen::CompactionApplied) {
        let marker = |entry: &crate::ledger::Entry| {
            format!(
                "[output of {} cleared to save context — re-run the command if you need it again]",
                entry.source
            )
        };
        let cleared = self.ledger.clear_tool_outputs(applied.kept_tokens, marker);
        for entry in cleared
            .cleared
            .iter()
            .take(applied.cleared_tool_outputs as usize)
        {
            if let Some(index) = self.entry_of.iter().position(|id| *id == Some(entry.id)) {
                let call_id = match &self.messages[index] {
                    ChatMessage::Tool { call_id, .. } => call_id.clone(),
                    _ => continue,
                };
                self.messages[index] = ChatMessage::Tool {
                    call_id,
                    output: format!(
                        "[output of {} cleared to save context — re-run the command if you need it again]",
                        entry.source
                    ),
                };
            }
        }
        for _ in 0..applied.dropped_messages {
            if self.drop_oldest_exchange() == 0 {
                break;
            }
        }
    }

    /// Where this session came from, oldest ancestor first. A child stores the
    /// reference (ADR-0015), so the chain is read from the child's own first event and
    /// each ancestor's own first event after it. A missing ancestor is fatal and named:
    /// without its prefix the session is not merely unexplained, it is inoperable.
    pub(super) fn lineage(&self) -> Result<Vec<(std::path::PathBuf, u64)>, KernelError> {
        let mut chain: Vec<(std::path::PathBuf, u64)> = Vec::new();
        let mut path = self.log.path().to_path_buf();
        let mut seen: Vec<String> = Vec::new();
        loop {
            let replay = loong_eventlog::replay_lenient(&path)?;
            let first = replay.events.first();
            let Some(first) = first else { break };
            if first.kind != gen::event::SESSION_FORKED {
                break;
            }
            let forked: gen::SessionForked = serde_json::from_value(first.data.clone())
                .map_err(|e| KernelError::Session(format!("session.forked is malformed: {e}")))?;
            if seen.contains(&forked.parent_session) {
                return Err(KernelError::Session(format!(
                    "session lineage is cyclic: {} appears twice",
                    forked.parent_session
                )));
            }
            seen.push(forked.parent_session.clone());
            let dir = path.parent().unwrap_or_else(|| std::path::Path::new("."));
            // A child lives beside its parent, which is what makes the reference a file
            // name rather than a path that could go stale.
            let parent = if forked.parent_session == "default" {
                dir.join("session.jsonl")
            } else {
                dir.join(format!("{}.jsonl", forked.parent_session))
            };
            if !parent.exists() {
                let mut named: Vec<String> = seen.clone();
                named.reverse();
                return Err(KernelError::Session(format!(
                    "{} is a child of {}, which is not in {}: the chain is {}{} — export the \
                     ancestors with it (ADR-0014)",
                    path.file_name().unwrap_or_default().to_string_lossy(),
                    forked.parent_session,
                    dir.display(),
                    named.join(" -> "),
                    format!(
                        " -> {}",
                        path.file_name().unwrap_or_default().to_string_lossy()
                    )
                )));
            }
            chain.push((parent.clone(), forked.parent_seq));
            path = parent;
        }
        // Oldest first: the prefix has to be in place before what came after it.
        chain.reverse();
        chain.push((self.log.path().to_path_buf(), u64::MAX));
        Ok(chain)
    }
}
