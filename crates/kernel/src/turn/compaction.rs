use super::*;

impl Kernel {
    pub fn context_explain(&self) -> gen::ContextExplain {
        let by_category = self.ledger.tokens_by_category();
        let mut categories: Vec<gen::ContextCategory> = by_category
            .into_iter()
            .map(|(category, tokens)| gen::ContextCategory {
                category: category.to_string(),
                tokens,
            })
            .collect();
        // Largest first: the panel's job is to show where the budget went.
        categories.sort_by(|a, b| b.tokens.cmp(&a.tokens).then(a.category.cmp(&b.category)));
        // The memory's own state, reported rather than inferred. With no engine the
        // mode is empty and the worker state is `off` — the surface must never have
        // to guess "off" from "the ledger happens to hold no cognitive map yet",
        // because a semantic session's first turns look exactly like that.
        let (memory_mode, memory_summarizer, memory_degraded, memory_layers, memory_worker) =
            match self.context_engine.as_ref() {
                None => (
                    String::new(),
                    String::new(),
                    String::new(),
                    Vec::new(),
                    gen::ContextWorker {
                        state: "off".to_string(),
                        windows: 0,
                        model_calls: 0,
                        enriched: 0,
                        failed: 0,
                        pending: 0,
                        deferrals: 0,
                        last_error: String::new(),
                    },
                ),
                Some(engine) => {
                    let status = engine.memory_status();
                    let worker = status.worker;
                    (
                        status.mode,
                        status.summarizer,
                        status.degraded,
                        status
                            .layers
                            .into_iter()
                            .map(|layer| gen::ContextLayer {
                                level: layer.level,
                                scope: layer.scope.to_string(),
                                count: layer.count,
                                tokens: layer.tokens,
                            })
                            .collect(),
                        gen::ContextWorker {
                            state: worker.state.to_string(),
                            windows: worker.windows,
                            model_calls: worker.model_calls,
                            enriched: worker.enriched,
                            failed: worker.failed,
                            pending: worker.pending,
                            deferrals: worker.deferrals,
                            last_error: worker.last_error,
                        },
                    )
                }
            };
        gen::ContextExplain {
            context_tokens: self.ledger.total_tokens(),
            budget_tokens: self.ledger.budget().unwrap_or(0),
            messages: self.messages.len() as u64,
            tools: self.tool_specs().len() as u64,
            // Read from the derived index rather than a counter, so a resumed session
            // reports its whole history.
            compactions: self
                .index
                .kinds()
                .map(|kinds| {
                    kinds
                        .iter()
                        .filter(|k| k.kind == gen::event::COMPACTION_APPLIED)
                        .map(|k| k.count)
                        .sum()
                })
                .unwrap_or(0),
            entries: self
                .ledger
                .entries()
                .iter()
                .map(|entry| gen::ContextEntry {
                    id: entry.id,
                    category: entry.category.as_str().to_string(),
                    source: entry.source.clone(),
                    tokens: entry.tokens,
                    lifetime: match entry.lifetime {
                        Lifetime::Turn => "turn".to_string(),
                        Lifetime::Session => "session".to_string(),
                        Lifetime::Persistent => "persistent".to_string(),
                    },
                    pinned: entry.pinned,
                    cleared: entry.cleared,
                })
                .collect(),
            categories,
            memory_mode,
            memory_summarizer,
            memory_degraded,
            memory_layers,
            memory_worker,
        }
    }

    /// Put the current L4 map in front of the model, refreshing it in place.
    ///
    /// Placed as its own message rather than folded into the system prompt: the
    /// system prompt is the operator's text and should not be rewritten on every
    /// step, while the map is expected to change. The ledger entry is created once
    /// and then refreshed with [`Ledger::replace`], so a long session has exactly
    /// one map entry rather than one per step — "why is this in my context?" has one
    /// answer, not a growing pile of them.
    ///
    /// Does nothing when the engine is off, which is the rollback path.
    pub(super) fn refresh_map(&mut self) {
        let Some(engine) = self.context_engine.as_mut() else {
            return;
        };
        engine.refresh();
        let Some(text) = engine.map_text() else {
            return;
        };
        match (self.map_entry, self.map_index) {
            (Some(entry), Some(index)) if index < self.messages.len() => {
                self.messages[index] = ChatMessage::System { text: text.clone() };
                self.ledger.replace(entry, &text);
            }
            _ => {
                let entry = self.ledger.add(
                    Category::CognitiveMap,
                    "context-engine:L4",
                    &text,
                    Lifetime::Session,
                    // Pinned: the map is the one block whose value is that it is
                    // always there. An evictable map is a map that disappears
                    // exactly when the context is under pressure, which is when it
                    // is worth the most.
                    true,
                );
                // After the system prompt, before the conversation.
                let at = usize::from(matches!(
                    self.messages.first(),
                    Some(ChatMessage::System { .. })
                ));
                self.messages.insert(at, ChatMessage::System { text });
                self.entry_of.insert(at, Some(entry));
                self.map_entry = Some(entry);
                self.map_index = Some(at);
            }
        }
    }

    /// ADR-0006 §4: compaction, in a fixed order, and visible.
    ///
    /// Clearing comes first because stale tool output is the bulk of a long
    /// session's context and the least valuable part of it — the model can re-run
    /// a command, but it cannot re-read a decision. Only then are the oldest whole
    /// exchanges dropped, and never across a tool pair: an assistant's tool call
    /// keeps its answer, because a conversation that splits them is invalid to
    /// every provider.
    /// How much input one request may carry, right now.
    ///
    /// Two ceilings, and the stricter wins because either one overflowing ends the same
    /// way. The **budget** is what a person or a pack chose: a cap on the input of **one
    /// request**, which it reaches at `compact_at_percent` of itself. The **window** is what
    /// the model can physically hold, and it is reached when the room left for the answer
    /// drops below the reserve. `None` from both means nothing bounds the context — which is
    /// a thing the CLI says out loud, because it means compaction never runs.
    ///
    /// Neither is a cumulative quota. What is measured is `ledger.total_tokens()`: the size
    /// of the prompt about to be sent, kept history included. Twenty calls in one turn are
    /// twenty measurements of a context, not twenty terms in a sum — a session is free to
    /// spend far more than the ceiling in total, and nothing meters that.
    fn input_ceiling(&self) -> Option<u64> {
        let from_budget = self
            .ledger
            .budget()
            .map(|budget| budget.saturating_mul(self.config.compact_at_percent) / 100);
        let from_window = self.current_window().map(|window| {
            // A reserve larger than the window would hold back everything; half the window
            // is as much as it can sensibly keep for the answer.
            let reserve = self.config.reserved_context_size.min(window / 2);
            window.saturating_sub(reserve)
        });
        match (from_budget, from_window) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// The window of the model this session is on. Routing is picked per step and the
    /// selection is recorded before the call, so a switch is picked up on the next step;
    /// the reserve makes one stale step harmless.
    fn current_window(&self) -> Option<u64> {
        self.models
            .get(&self.selected_model)
            .and_then(|model| model.context_window())
    }

    pub(super) fn compact_if_needed(&mut self, events: &mut Vec<u64>) -> Result<(), KernelError> {
        let Some(ceiling) = self.input_ceiling() else {
            return Ok(());
        };
        let before = self.ledger.total_tokens();
        if before <= ceiling {
            return Ok(());
        }
        let keep = self.config.keep_newest_tokens.min(ceiling / 2);

        // Phase 1: clear stale tool output, oldest first, outside the new window.
        let cleared = self.ledger.clear_tool_outputs(keep, |entry| {
            format!(
                "[output of {} cleared to save context — re-run the command if you need it again]",
                entry.source
            )
        });
        for entry in &cleared.cleared {
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

        // Phase 2: only if still over, drop the oldest whole exchanges — unless the
        // layered engine is on, in which case the exchange must survive: the whole
        // claim of ADR-0017 is that the original stays reachable, and a dropped
        // message is exactly the lossy step it exists to remove. Stale tool output
        // was already cleared above, which is where the bulk of the saving is.
        let mut dropped_messages = 0;
        let may_drop = self.context_engine.is_none();
        while may_drop && self.ledger.total_tokens() > ceiling {
            let removed = self.drop_oldest_exchange();
            if removed == 0 {
                break; // nothing safe left to drop (a single long turn)
            }
            dropped_messages += removed;
        }

        // A compaction that could not free anything (a single long turn, nothing
        // stale) is not an event: `compaction.applied` means the context changed.
        if cleared.cleared.is_empty() && dropped_messages == 0 {
            return Ok(());
        }

        let after = self.ledger.total_tokens();
        events.push(self.append(
            gen::event::COMPACTION_APPLIED,
            &gen::CompactionApplied {
                tokens_before: before,
                tokens_after: after,
                kept_tokens: keep,
                cleared_tool_outputs: cleared.cleared.len() as u64,
                dropped_messages: dropped_messages as u64,
                summary: format!(
                    "cleared {} tool output(s) older than the newest {keep} tokens and dropped                      {dropped_messages} oldest message(s): {before} -> {after} tokens",
                    cleared.cleared.len()
                ),
            },
        )?);
        Ok(())
    }

    /// Drop the oldest exchange (a user message and everything up to the next
    /// user message) so the conversation stays well-formed. Returns how many
    /// messages went.
    pub(super) fn drop_oldest_exchange(&mut self) -> usize {
        if self.messages.len() < 2 {
            return 0;
        }
        // The system prompt is not an exchange: it stays for the session's life.
        let start = usize::from(matches!(
            self.messages.first(),
            Some(ChatMessage::System { .. })
        ));
        let end = self
            .messages
            .iter()
            .enumerate()
            .skip(start + 1)
            .find(|(_, m)| matches!(m, ChatMessage::User { .. }))
            .map(|(index, _)| index)
            .unwrap_or(0);
        // `end == 0` means the whole conversation is one turn: dropping it would
        // throw away the prompt we are answering.
        if end <= start || end >= self.messages.len() {
            return 0;
        }
        self.messages.drain(start..end);
        let ids: Vec<u64> = self.entry_of.drain(start..end).flatten().collect();
        let _ = self.ledger.forget(&ids);
        end - start
    }
}
