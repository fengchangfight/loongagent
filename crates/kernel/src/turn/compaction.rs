//! Purpose: Keep the prompt inside its ceiling: clear stale tool output first, drop whole exchanges only with the engine off, and explain what the request will carry.
//!
//! Serves: a long session that keeps working without silently losing the prompt being
//! answered or the map that says where it stands (ADR-0006, ADR-0017).
//!
//! # Contract
//! - **Design:** `docs/design/context-ledger.md` §3–§4, §6.
//! - **Upstream:** `Kernel::run_inner` calls `refresh_memory`、`age_tool_outputs`、`compact_if_needed` and, once per turn, `seat_map`; `crates/server` reads `context_explain`.
//! - **Downstream:** `Ledger::clear_tool_outputs`, `Ledger::forget`,
//!   `ContextEngine::refresh` and `ContextEngine::map_text`.
//! - **Verify:** `cargo test -p loong-kernel turn::tests::compaction_clears_stale_tool_output_before_dropping_anything` · `./scripts/check.sh`.

use super::*;

/// Purpose: The budget a model gets when nobody set one: a quarter of its window, floored at 32k and capped at 100k.
///
/// Serves: `Kernel::effective_budget`, so a million-token window is not a licence to grow to a million.
/// Design: `docs/superpowers/specs/2026-10-28-token-efficiency-design.md` §4.1.
/// Up: `Kernel::effective_budget` and the turn tests.
/// Down: no callee; it returns a number.
/// Verify: `cargo test -p loong-kernel turn::tests::a_default_budget_is_derived_from_the_window`.
pub(super) fn default_budget(window: u64) -> u64 {
    (window / 4).clamp(32_000, 100_000)
}

/// Purpose: How many of the newest tokens compaction keeps verbatim; `None` when no window can reach the ceiling.
///
/// Serves: `Kernel::compact_if_needed`, choosing a window a pass can actually land under.
/// Design: `docs/design/flows/context-compaction.md` §1.
/// Up: `Kernel::compact_if_needed` and the turn tests.
/// Down: no callee; it returns a number.
/// Verify: `cargo test -p loong-kernel turn::tests::the_window_shrinks_to_half_the_room_the_history_leaves`.
///
/// The configured `keep_newest_tokens` is a wish, not a right. A session whose
/// prompt, assistant text and tool-call arguments already fill most of the ceiling
/// has less than that to spend on verbatim tool output, and a window fixed above
/// what is left puts the *floor* of clearing over the ceiling: every pass would
/// clear a sliver, stay over, and the thrash guard would stop the turn three
/// passes later — and a fresh turn starts from the same context. `room` is what
/// the ceiling leaves above the history that cannot be cleared, and half of it is
/// kept so a pass leaves headroom for the steps that follow. When the room is
/// gone, no window fits and the caller must not clear at all.
pub(super) fn keep_window(
    configured: u64,
    ceiling: u64,
    total: u64,
    clearable: u64,
) -> Option<u64> {
    let room = ceiling.saturating_sub(total.saturating_sub(clearable));
    if room < 2 {
        return None;
    }
    Some(configured.min(ceiling / 2).min(room / 2))
}

/// Purpose: The one marker text both clearing paths write into a cleared tool message.
///
/// Serves: `Kernel::age_tool_outputs` and `Kernel::splice_cleared`, so message and ledger never disagree.
/// Design: `docs/design/context-ledger.md` §4.
/// Up: `Kernel::age_tool_outputs`, `Kernel::splice_cleared` and `Kernel::compact_if_needed`.
/// Down: no callee; it returns the text.
/// Verify: `cargo test -p loong-kernel turn::tests::compaction_clears_stale_tool_output_before_dropping_anything`.
fn marker_for_cleared_output(entry: &crate::ledger::Entry) -> String {
    format!(
        "[output of {} cleared to save context — re-run the command if you need it again]",
        entry.source
    )
}

impl Kernel {
    /// Purpose: The wire view of what the next request will carry, and why.
    ///
    /// Serves: a context panel that answers tokens, categories, entries, tools and memory state without changing anything.
    /// Design: `docs/design/context-ledger.md` §6.
    /// Up: `crates/server`'s `context.explain` handler; the turn tests.
    /// Down: `Ledger::{total_tokens, budget, tokens_by_category, entries}`, `Kernel::tool_specs`, `Index::kinds` and `ContextEngine::memory_status`.
    /// Verify: `cargo test -p loong-kernel turn::tests::the_context_view_says_what_the_next_request_will_carry`.
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
            budget_tokens: self.effective_budget().unwrap_or(0),
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

    /// Purpose: Clear tool output older than the keep window before the request grows, not only at the ceiling.
    ///
    /// Serves: a steady-state context instead of one that climbs until compaction fires.
    /// Design: `docs/superpowers/specs/2026-10-28-token-efficiency-design.md` §4.2.
    /// Up: `Kernel::run_inner`, once per step before `compact_if_needed`.
    /// Down: `Ledger::tool_output_tokens`, `Ledger::clear_tool_outputs` and `Kernel::append`.
    /// Verify: `cargo test -p loong-kernel turn::tests::tool_output_ages_before_any_ceiling_is_crossed` · `cargo test -p loong-kernel turn::tests::aging_leaves_headroom_below_the_line_that_triggered_it`.
    pub(super) fn age_tool_outputs(
        &mut self,
        events: &mut Vec<u64>,
    ) -> Result<Option<(u64, u64)>, KernelError> {
        let keep = self.config.keep_newest_tokens;
        if self.ledger.tool_output_tokens() <= keep {
            return Ok(None);
        }
        let before = self.ledger.total_tokens();
        // Clear to half the line that triggered this pass, never to the line
        // itself. A pass that stops at the line leaves no headroom: the next tool
        // result crosses it again, so one pass runs per read. That costs more than
        // the tokens it frees, because a pass rewrites tool messages in the middle
        // of the conversation, and the provider's cache is a prefix matching
        // byte for byte — everything after the first rewritten message is re-read
        // at full price. A recorded session shows the shape: eight passes, most of
        // them clearing one or two outputs, and every request that followed one of
        // them matched only the system prompt. Halving the target gives the next
        // reads room before the line is crossed again, so the same history costs
        // one cache miss per two reads instead of one per read.
        let target = keep / 2;
        let cleared = self
            .ledger
            .clear_tool_outputs(target, marker_for_cleared_output);
        if cleared.cleared.is_empty() {
            return Ok(None);
        }
        self.splice_cleared(&cleared);
        let after = self.ledger.total_tokens();
        events.push(self.append(
            gen::event::COMPACTION_APPLIED,
            &gen::CompactionApplied {
                tokens_before: before,
                tokens_after: after,
                // The window this pass actually left verbatim, which is what a
                // replay must reproduce; it is no longer the line it crossed.
                kept_tokens: target,
                cleared_tool_outputs: cleared.cleared.len() as u64,
                dropped_messages: 0,
                summary: format!(
                    "aged {} tool output(s) older than the newest {target} tokens: {before} -> {after} tokens",
                    cleared.cleared.len()
                ),
            },
        )?);
        Ok(Some((before, after)))
    }

    /// Purpose: Put the cleared markers into the live conversation, matching the ledger's entries by id.
    ///
    /// Serves: both clearing paths, which must leave the same text in the message and the ledger.
    /// Design: `docs/design/context-ledger.md` §4.
    /// Up: `Kernel::compact_if_needed` and `Kernel::age_tool_outputs`.
    /// Down: `Kernel::entry_of` and `ChatMessage::Tool`.
    /// Verify: `cargo test -p loong-kernel turn::tests::compaction_clears_stale_tool_output_before_dropping_anything`.
    fn splice_cleared(&mut self, cleared: &crate::ledger::ClearReport) {
        for entry in &cleared.cleared {
            if let Some(index) = self.entry_of.iter().position(|id| *id == Some(entry.id)) {
                let call_id = match &self.messages[index] {
                    ChatMessage::Tool { call_id, .. } => call_id.clone(),
                    _ => continue,
                };
                self.messages[index] = ChatMessage::Tool {
                    call_id,
                    output: marker_for_cleared_output(entry),
                };
            }
        }
    }

    /// Purpose: Refresh the layered memory's deterministic layers; seating the map is `seat_map`'s job.
    ///
    /// Serves: `Kernel::run_inner`, which keeps the memory current without moving the prompt prefix.
    /// Design: `docs/superpowers/specs/2026-10-28-token-efficiency-design.md` §4.3.
    /// Up: `Kernel::run_inner`, once per step and once before the loop.
    /// Down: `ContextEngine::refresh`.
    /// Verify: `cargo test -p loong-kernel turn::tests::the_request_prefix_is_append_only_within_a_turn`.
    pub(super) fn refresh_memory(&mut self) {
        if let Some(engine) = self.context_engine.as_mut() {
            engine.refresh();
        }
    }

    /// Purpose: Seat the pinned L4 map once per turn and after a clearing pass, so the request prefix does not move between steps.
    ///
    /// Serves: a stable prefix for the provider's cache without giving up the map.
    /// Design: `docs/superpowers/specs/2026-10-28-token-efficiency-design.md` §4.3.
    /// Up: `Kernel::run_inner`, before the first request and after a clearing pass.
    /// Down: `ContextEngine::map_text`, `Ledger::replace` and `Ledger::add`.
    /// Verify: `cargo test -p loong-kernel turn::tests::the_request_prefix_is_append_only_within_a_turn`.
    pub(super) fn seat_map(&mut self) {
        let Some(engine) = self.context_engine.as_mut() else {
            return;
        };
        let Some(text) = engine.map_text() else {
            return;
        };
        match (self.map_entry, self.map_index) {
            (Some(entry), Some(index)) if index < self.messages.len() => {
                // An unchanged map is not rewritten: the block sits before the
                // conversation, so a write with the same text would still discard the
                // provider's cache for no gain.
                if let ChatMessage::System { text: current } = &self.messages[index] {
                    if current == &text {
                        return;
                    }
                }
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
                // After all leading system blocks (prompt, instructions), before the
                // conversation.
                let at = self
                    .messages
                    .iter()
                    .take_while(|m| matches!(m, ChatMessage::System { .. }))
                    .count();
                self.messages.insert(at, ChatMessage::System { text });
                self.entry_of.insert(at, Some(entry));
                self.map_entry = Some(entry);
                self.map_index = Some(at);
            }
        }
    }

    /// Purpose: The budget the next request is measured against: the person's, or the window's quarter.
    ///
    /// Serves: `input_ceiling` and `context_explain`, which must agree on one number.
    /// Design: `docs/superpowers/specs/2026-10-28-token-efficiency-design.md` §4.1.
    /// Up: `Kernel::input_ceiling` and `Kernel::context_explain`.
    /// Down: `Ledger::budget`, `Kernel::current_window` and `default_budget`.
    /// Verify: `cargo test -p loong-kernel turn::tests::an_explicit_budget_wins_over_the_derived_one`.
    fn effective_budget(&self) -> Option<u64> {
        self.ledger
            .budget()
            .or_else(|| self.current_window().map(default_budget))
    }

    /// Purpose: How much input one request may carry now: the stricter of the budget and the model window.
    ///
    /// Serves: compaction firing before a provider refuses the prompt.
    /// Design: `docs/design/context-ledger.md` §3.
    /// Up: `Kernel::compact_if_needed`.
    /// Down: `Ledger::budget`, `current_window` and `KernelConfig.compact_at_percent` / `reserved_context_size`.
    /// Verify: `cargo test -p loong-kernel turn::tests::the_models_window_bounds_the_context_when_no_budget_does`.
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
    pub(super) fn input_ceiling(&self) -> Option<u64> {
        let from_budget = self
            .effective_budget()
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

    /// Purpose: The selected model's context window, or `None` when it declares none.
    ///
    /// Contract: `turn/compaction.rs` ceiling.
    /// The window of the model this session is on. Routing is picked per step and the
    /// selection is recorded before the call, so a switch is picked up on the next step;
    /// the reserve makes one stale step harmless.
    fn current_window(&self) -> Option<u64> {
        self.models
            .get(&self.selected_model)
            .and_then(|model| model.context_window())
    }

    /// Purpose: Clear stale tool output, then drop whole exchanges only when the engine is off.
    ///
    /// Serves: a long session that frees context without losing a tool pair or the prompt being answered.
    /// Design: `docs/design/context-ledger.md` §4.
    /// Up: `Kernel::run_inner` at every step boundary.
    /// Down: `input_ceiling`, `Ledger::clear_tool_outputs`, `drop_oldest_exchange` and `Kernel::append` (`compaction.applied`).
    /// Verify: `cargo test -p loong-kernel turn::tests::compaction_clears_stale_tool_output_before_dropping_anything`.
    /// ADR-0006 §4: compaction, in a fixed order, and visible.
    ///
    /// Clearing comes first because stale tool output is the bulk of a long
    /// session's context and the least valuable part of it — the model can re-run
    /// a command, but it cannot re-read a decision. Only then are the oldest whole
    /// exchanges dropped, and never across a tool pair: an assistant's tool call
    /// keeps its answer, because a conversation that splits them is invalid to
    /// every provider.
    pub(super) fn compact_if_needed(
        &mut self,
        events: &mut Vec<u64>,
    ) -> Result<Option<(u64, u64)>, KernelError> {
        let Some(ceiling) = self.input_ceiling() else {
            return Ok(None);
        };
        let before = self.ledger.total_tokens();
        if before <= ceiling {
            return Ok(None);
        }
        // Phase 1: clear stale tool output, oldest first, outside the new window.
        // The window has to fit under the ceiling as well as the config: clearing
        // cannot reach a ceiling the history alone already fills, and a pass that
        // rewrote tool bodies to still land over it would cost a full cache miss
        // and free nothing (`docs/design/flows/context-compaction.md` §1). Phase 2
        // is still allowed to try — dropping an exchange is the one lever left.
        let keep = keep_window(
            self.config.keep_newest_tokens,
            ceiling,
            before,
            self.ledger.tool_output_tokens(),
        );
        let cleared = match keep {
            Some(keep) => {
                let cleared = self
                    .ledger
                    .clear_tool_outputs(keep, marker_for_cleared_output);
                self.splice_cleared(&cleared);
                cleared
            }
            None => crate::ledger::ClearReport::default(),
        };

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
            return Ok(None);
        }

        let after = self.ledger.total_tokens();
        // A pass that cleared nothing records the whole context as its window, so a
        // replay clears nothing either.
        let kept_tokens = keep.unwrap_or(before);
        let summary = match keep {
            Some(keep) => format!(
                "cleared {} tool output(s) older than the newest {keep} tokens and dropped {dropped_messages} oldest message(s): {before} -> {after} tokens",
                cleared.cleared.len()
            ),
            None => format!(
                "cleared no tool output: the history alone fills the ceiling, so all {before} tokens were kept and {dropped_messages} oldest message(s) dropped: {before} -> {after} tokens"
            ),
        };
        events.push(self.append(
            gen::event::COMPACTION_APPLIED,
            &gen::CompactionApplied {
                tokens_before: before,
                tokens_after: after,
                kept_tokens,
                cleared_tool_outputs: cleared.cleared.len() as u64,
                dropped_messages: dropped_messages as u64,
                summary,
            },
        )?);
        Ok(Some((before, after)))
    }

    /// Purpose: Drop the oldest exchange and forget its ledger entries, returning how many messages went.
    ///
    /// Serves: a well-formed conversation after phase 2, with the current prompt kept.
    /// Design: `docs/design/context-ledger.md` §4.
    /// Up: `Kernel::compact_if_needed` and `Kernel::replay_compaction`.
    /// Down: `Ledger::forget` and the `messages`/`entry_of` vectors.
    /// Verify: `cargo test -p loong-kernel turn::tests::compaction_drops_the_oldest_exchange_when_clearing_is_not_enough`.
    /// Drop the oldest exchange (a user message and everything up to the next
    /// user message) so the conversation stays well-formed. Returns how many
    /// messages went.
    pub(super) fn drop_oldest_exchange(&mut self) -> usize {
        if self.messages.len() < 2 {
            return 0;
        }
        // Every leading System message is a pinned block, not an exchange: the
        // operator's prompt, the workspace's instructions, and the cognitive map.
        let start = self
            .messages
            .iter()
            .take_while(|m| matches!(m, ChatMessage::System { .. }))
            .count();
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
