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

use super::recall::split_terms;
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

/// How many words a stub may carry. Enough to search with, few enough that the stub
/// is still a fraction of the exchange it replaces.
pub(super) const STUB_MAX_CLUES: usize = 12;

/// How much of the request a stub keeps as its title.
pub(super) const STUB_TITLE_CHARS: usize = 60;

/// How many changed files a stub names, newest first.
pub(super) const STUB_MAX_FILES: usize = 4;

/// How many verification commands a stub names.
pub(super) const STUB_MAX_COMMANDS: usize = 2;

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
        forced_ceiling: Option<u64>,
    ) -> Result<Option<(u64, u64)>, KernelError> {
        let ceiling = match forced_ceiling {
            Some(ceiling) => ceiling,
            None => match self.input_ceiling() {
                Some(ceiling) => ceiling,
                None => return Ok(None),
            },
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

        // Phase 1.5: with the layered engine on, old exchanges do not stay in the
        // foreground and are not dropped either — each becomes a stub with an address,
        // which is what stops the prompt from growing monotonically. This is the rung
        // that the `compaction thrash` stop was really asking for: without it, clearing
        // tool output is all a long session can do, and once every output is a marker,
        // every further pass frees nothing.
        let mut stubbed_messages = 0usize;
        if self.context_engine.is_some() {
            while self.ledger.total_tokens() > ceiling {
                let keep = self.rounds_that_fit(self.config.keep_newest_tokens);
                let moved = self.stub_old_exchanges(keep);
                if moved == 0 {
                    break;
                }
                stubbed_messages += moved;
            }
            if stubbed_messages > 0 {
                self.merge_adjacent_stubs();
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
        if cleared.cleared.is_empty() && dropped_messages == 0 && stubbed_messages == 0 {
            return Ok(None);
        }

        let after = self.ledger.total_tokens();
        // A pass that cleared nothing records the whole context as its window, so a
        // replay clears nothing either.
        let kept_tokens = keep.unwrap_or(before);
        let stubbed = if stubbed_messages > 0 {
            format!(", stubbed {stubbed_messages} message(s) into the stub layer")
        } else {
            String::new()
        };
        let summary = match keep {
            Some(keep) => format!(
                "cleared {} tool output(s) older than the newest {keep} tokens and dropped {dropped_messages} oldest message(s){stubbed}: {before} -> {after} tokens",
                cleared.cleared.len()
            ),
            None => format!(
                "cleared no tool output: the history alone fills the ceiling, so all {before} tokens were kept and {dropped_messages} oldest message(s) dropped{stubbed}: {before} -> {after} tokens"
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

    /// Purpose: Replace the oldest whole exchanges with one stub that keeps their address and their clues.
    ///
    /// Serves: a foreground that stops growing while the original record stays reachable —
    /// the change that makes a long session end as a degraded answer instead of a
    /// `compaction thrash` stop.
    /// Design: `docs/superpowers/specs/2026-11-07-finite-memory-unbounded-conversation-design.md` §4, §6.
    /// Up: `Kernel::compact_if_needed`, after clearing tool output was not enough.
    /// Down: `stub_text`, `Kernel::rounds_that_fit`, `Ledger::{forget, add, locate, set_clues}` and the `messages`/`entry_of` splices.
    /// Verify: `cargo test -p loong-kernel turn::tests::old_exchanges_become_stubs_with_an_address`.
    ///
    /// Nothing here calls a model: degradation is the moment latency is least affordable,
    /// and every field comes from a message this turn already holds. Deterministic
    /// extraction can be mechanical; inventing is what is not allowed.
    ///
    /// Returns how many messages left the foreground, or `0` when there is nothing safe
    /// to stub. The newest `keep_rounds` exchanges are never touched: that is the turn
    /// being answered, and it is the one thing a stub cannot stand in for.
    pub(super) fn stub_old_exchanges(&mut self, keep_rounds: usize) -> usize {
        let start = self
            .messages
            .iter()
            .take_while(|m| matches!(m, ChatMessage::System { .. }))
            .count();
        let rounds: Vec<usize> = (start..self.messages.len())
            .filter(|index| matches!(self.messages[*index], ChatMessage::User { .. }))
            .collect();
        if rounds.len() <= keep_rounds {
            return 0;
        }
        let cut = rounds[rounds.len() - keep_rounds];
        if cut <= start {
            return 0;
        }

        let span = self.span_of(start, cut);
        let (text, clues) = stub_text(&self.messages[start..cut], span);
        // The entries the stub stands in for are gone from the ledger: their text is
        // no longer in the prompt, and pretending otherwise would double-count it.
        let ids: Vec<u64> = self.entry_of[start..cut]
            .iter()
            .flatten()
            .copied()
            .collect();
        self.ledger.forget(&ids);
        let entry = self
            .ledger
            .add(Category::Stub, "stub", &text, Lifetime::Session, false);
        if let Some((from, to)) = span {
            self.ledger.locate(entry, from, to);
        }
        self.ledger.set_clues(entry, clues);
        self.messages
            .splice(start..cut, [ChatMessage::System { text }]);
        self.entry_of.splice(start..cut, [Some(entry)]);
        cut - start
    }

    /// Purpose: Fold adjacent stubs into one, so the stub layer is bounded as well.
    ///
    /// Serves: the "forgotten content can still be forgotten again" half of the design:
    /// a second compaction pass must not leave a growing row of stubs in the foreground.
    /// Design: `docs/superpowers/specs/2026-11-07-finite-memory-unbounded-conversation-design.md` §5.
    /// Up: `Kernel::compact_if_needed`, after `stub_old_exchanges` moved something.
    /// Down: `Ledger::{replace, set_clues, entries}` and the `messages`/`entry_of` vectors.
    /// Verify: `cargo test -p loong-kernel turn::tests::the_foreground_stops_growing_once_stubs_take_over`.
    ///
    /// Two stubs are adjacent when nothing was kept between them. Their ranges union
    /// and their clues union (capped at [`STUB_MAX_CLUES`]), so the merged stub is one
    /// address that covers both. Returns how many merges it made.
    pub(super) fn merge_adjacent_stubs(&mut self) -> usize {
        let mut merges = 0usize;
        let mut index = 0usize;
        while index + 1 < self.messages.len() {
            if !self.is_stub(index) || !self.is_stub(index + 1) {
                index += 1;
                continue;
            }
            let (left, right) = (self.stub_entry(index), self.stub_entry(index + 1));
            let (Some(left), Some(right)) = (left, right) else {
                index += 1;
                continue;
            };
            let left_clues = self.ledger.clues(left).to_vec();
            let right_clues = self.ledger.clues(right).to_vec();
            let mut clues: Vec<String> = Vec::new();
            for clue in left_clues.iter().chain(right_clues.iter()) {
                if !clues.iter().any(|known: &String| known == clue) {
                    clues.push(clue.clone());
                }
                if clues.len() == STUB_MAX_CLUES {
                    break;
                }
            }
            let span = match (self.entry_seq(left), self.entry_seq(right)) {
                (Some((lf, _)), Some((_, rt))) => Some((lf, rt)),
                (span, _) => span,
            };
            let title = match &self.messages[index] {
                ChatMessage::System { text } => text
                    .lines()
                    .next()
                    .and_then(|line| line.split_once("] ").map(|(_, rest)| rest.to_string()))
                    .unwrap_or_else(|| "earlier work".to_string()),
                _ => "earlier work".to_string(),
            };
            let text = format_stub(span, &title, 0, None, &[], &[], clues.clone(), true);
            self.messages[index] = ChatMessage::System { text: text.clone() };
            self.ledger.replace(left, &text);
            self.ledger.set_clues(left, clues);
            if let Some((from, to)) = span {
                self.ledger.locate(left, from, to);
            }
            // The right-hand stub and its entry leave together, so the lockstep holds.
            self.messages.remove(index + 1);
            self.entry_of.remove(index + 1);
            let _ = self.ledger.forget(&[right]);
            merges += 1;
        }
        merges
    }

    /// Purpose: Whether the message at one index is a stub, judged by its ledger entry.
    ///
    /// Serves: `Kernel::merge_adjacent_stubs` without parsing message text.
    /// Contract: `turn/compaction.rs` stub layer.
    fn is_stub(&self, index: usize) -> bool {
        self.stub_entry(index).is_some()
    }

    /// Purpose: The ledger entry id of the stub at one index, if that message is one.
    ///
    /// Contract: `turn/compaction.rs` stub layer.
    fn stub_entry(&self, index: usize) -> Option<u64> {
        let id = self.entry_of.get(index).copied().flatten()?;
        self.ledger
            .entries()
            .iter()
            .find(|entry| entry.id == id)
            .filter(|entry| entry.category == Category::Stub)
            .map(|entry| entry.id)
    }

    /// Purpose: The `seq` interval one ledger entry recorded, if any.
    ///
    /// Contract: `turn/compaction.rs` stub layer.
    fn entry_seq(&self, id: u64) -> Option<(u64, u64)> {
        self.ledger
            .entries()
            .iter()
            .find(|entry| entry.id == id)
            .and_then(|entry| entry.seq)
    }

    /// Purpose: How many of the newest exchanges fit in the verbatim window.
    ///
    /// Serves: `stub_old_exchanges` choosing what to keep instead of a fixed count.
    /// Design: `docs/superpowers/specs/2026-11-07-finite-memory-unbounded-conversation-design.md` §4.
    /// Up: `Kernel::compact_if_needed`.
    /// Down: `Kernel::entry_of` and the ledger's token counts.
    /// Verify: `cargo test -p loong-kernel turn::tests::old_exchanges_become_stubs_with_an_address`.
    ///
    /// At least one exchange is always kept: the newest one is the turn being answered,
    /// and stubbing it would answer a question with a note about itself.
    pub(super) fn rounds_that_fit(&self, budget: u64) -> usize {
        let start = self
            .messages
            .iter()
            .take_while(|m| matches!(m, ChatMessage::System { .. }))
            .count();
        let rounds: Vec<usize> = (start..self.messages.len())
            .filter(|index| matches!(self.messages[*index], ChatMessage::User { .. }))
            .collect();
        // One pass over the ledger, then a map: a linear scan per message would be
        // quadratic on exactly the long sessions this rule exists for.
        let cost_of: std::collections::BTreeMap<u64, u64> = self
            .ledger
            .entries()
            .iter()
            .map(|entry| (entry.id, entry.tokens))
            .collect();
        let tokens = |from: usize, to: usize| -> u64 {
            (from..to)
                .filter_map(|index| self.entry_of.get(index).copied().flatten())
                .filter_map(|id| cost_of.get(&id).copied())
                .sum()
        };
        let mut seen = 0u64;
        let mut keep = 0usize;
        for (position, begin) in rounds.iter().enumerate().rev() {
            let end = rounds
                .get(position + 1)
                .copied()
                .unwrap_or(self.messages.len());
            let cost = tokens(*begin, end);
            if seen + cost > budget {
                break;
            }
            seen += cost;
            keep += 1;
        }
        keep.max(1).min(rounds.len())
    }

    /// Purpose: The `seq` interval the messages in one range were recorded from.
    ///
    /// Serves: a stub naming exactly the record it stands in for.
    /// Design: `docs/superpowers/specs/2026-11-07-finite-memory-unbounded-conversation-design.md` §6.
    /// Up: `Kernel::stub_old_exchanges`.
    /// Down: `Kernel::entry_seq` and `Ledger::entries`.
    /// Verify: `cargo test -p loong-kernel turn::tests::old_exchanges_become_stubs_with_an_address`.
    /// The low end is the oldest known `seq` and the high end the newest, so events
    /// that recorded no message (a policy decision, a hook) still fall inside the
    /// range a recall over it reads back.
    fn span_of(&self, start: usize, cut: usize) -> Option<(u64, u64)> {
        let mut span: Option<(u64, u64)> = None;
        for entry in self.entry_of[start..cut].iter().flatten() {
            let Some((from, to)) = self.entry_seq(*entry) else {
                continue;
            };
            span = Some(match span {
                Some((lo, hi)) => (lo.min(from), hi.max(to)),
                None => (from, to),
            });
        }
        span
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

/// Purpose: Build one stub's text from the messages it replaces, deterministically.
///
/// Serves: an exchange leaving the foreground while its request, its changes, its
/// verification and the words that lead back to it stay behind.
/// Design: `docs/superpowers/specs/2026-11-07-finite-memory-unbounded-conversation-design.md` §6.
/// Up: `Kernel::stub_old_exchanges`.
/// Down: `split_terms`, `format_stub` and `ChatMessage`; no model call.
/// Verify: `cargo test -p loong-kernel turn::tests::a_rejected_plan_survives_as_a_stub`.
///
/// Every field is read from a message already in the conversation: the request from
/// the person's own words, the changed files and the commands from the model's calls,
/// the verdict from the matching tool output. Nothing is inferred — a field with no
/// evidence is left out rather than guessed.
fn stub_text(messages: &[ChatMessage], span: Option<(u64, u64)>) -> (String, Vec<String>) {
    let mut title = String::new();
    let mut steps = 0usize;
    let mut files: Vec<String> = Vec::new();
    let mut commands: Vec<(String, bool)> = Vec::new();
    let mut clues: Vec<String> = Vec::new();
    let mut outputs: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    for message in messages {
        if let ChatMessage::Tool { call_id, output } = message {
            outputs.insert(call_id.clone(), output.clone());
        }
    }
    let mut answered = false;
    for message in messages {
        match message {
            ChatMessage::User { text } => {
                if title.is_empty() {
                    title = text
                        .chars()
                        .take(STUB_TITLE_CHARS)
                        .map(|c| if c == '\n' { ' ' } else { c })
                        .collect();
                }
                for term in split_terms(text) {
                    push_clue(&mut clues, term);
                }
            }
            ChatMessage::Assistant { text, tool_calls } => {
                steps += 1;
                // A turn that ended with prose is a finished exchange; one that ended
                // with a call was interrupted by the budget. Saying which is honest.
                answered = tool_calls.is_empty()
                    && text
                        .as_deref()
                        .map(|text| !text.trim().is_empty())
                        .unwrap_or(false);
                for call in tool_calls {
                    push_clue(&mut clues, call.tool.clone());
                    if matches!(call.tool.as_str(), "edit_file" | "write_file") {
                        if let Some(path) = call.args.get("path").and_then(|v| v.as_str()) {
                            if files.len() < STUB_MAX_FILES && !files.iter().any(|f| f == path) {
                                files.push(path.to_string());
                            }
                        }
                    }
                    if call.tool == "run_command" {
                        if let Some(command) = call.args.get("command").and_then(|v| v.as_str()) {
                            if commands.len() < STUB_MAX_COMMANDS {
                                // A failed verification is recorded as failed: a
                                // re-run can reproduce a success, not a decision.
                                let ok = outputs
                                    .get(&call.call_id)
                                    .map(|output| {
                                        !(output.contains("FAILED")
                                            || output.contains("error[")
                                            || output.contains("error:"))
                                    })
                                    .unwrap_or(true);
                                commands.push((command.to_string(), ok));
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    let status = if answered { "完成" } else { "未完成" };
    let text = format_stub(
        span,
        &title,
        steps,
        Some(status),
        &files,
        &commands,
        clues.clone(),
        false,
    );
    (text, clues)
}

/// Purpose: Render one stub line plus its clues and its address.
///
/// Serves: the fixed shape a model can read at a glance and a person can audit.
/// Design: `docs/superpowers/specs/2026-11-07-finite-memory-unbounded-conversation-design.md` §6.
/// Up: `stub_text` and `Kernel::merge_adjacent_stubs`.
/// Down: no callee; it formats a string.
/// Verify: `cargo test -p loong-kernel turn::tests::old_exchanges_become_stubs_with_an_address`.
///
/// The address is the part that makes forgetting reversible: `recall seq a-b`
/// reads the record back in one call, and `merged` says how many earlier stubs
/// this one now stands for.
fn format_stub(
    span: Option<(u64, u64)>,
    title: &str,
    steps: usize,
    status: Option<&str>,
    files: &[String],
    commands: &[(String, bool)],
    clues: Vec<String>,
    merged: bool,
) -> String {
    let mut out = String::new();
    match span {
        Some((from, to)) => {
            out.push_str(&format!("[stub {from}-{to}] {title}"));
            let mut header: Vec<String> = Vec::new();
            if steps > 0 {
                header.push(format!("{steps} 步"));
            }
            if let Some(status) = status {
                header.push(status.to_string());
            }
            if !header.is_empty() {
                out.push_str(&format!("（{}）", header.join("，")));
            }
            if merged {
                out.push_str("（并段）");
            }
            out.push('\n');
            if !files.is_empty() {
                out.push_str(&format!("  改动: {}\n", files.join(", ")));
            }
            if !commands.is_empty() {
                let said: Vec<String> = commands
                    .iter()
                    .map(|(command, ok)| {
                        if *ok {
                            format!("{command} 通过")
                        } else {
                            format!("{command} 失败")
                        }
                    })
                    .collect();
                out.push_str(&format!("  验证: {}\n", said.join(" | ")));
            }
            if clues.is_empty() {
                out.push_str(&format!("  原文: recall seq {from}-{to}\n"));
            } else {
                out.push_str(&format!(
                    "  线索: {} | 原文: recall seq {from}-{to}\n",
                    clues.join(" ")
                ));
            }
        }
        None => {
            out.push_str(&format!("[stub] {title}"));
            if steps > 0 {
                out.push_str(&format!("（{steps} 步）"));
            }
            out.push('\n');
            if !files.is_empty() {
                out.push_str(&format!("  改动: {}\n", files.join(", ")));
            }
            if !clues.is_empty() {
                out.push_str(&format!("  线索: {}\n", clues.join(" ")));
            }
        }
    }
    out
}

/// Purpose: Add one clue word, keeping the order and the cap.
///
/// Contract: `turn/compaction.rs` stub layer.
fn push_clue(clues: &mut Vec<String>, clue: String) {
    let clue = clue.trim().to_string();
    if clue.is_empty() || clues.len() == STUB_MAX_CLUES {
        return;
    }
    if !clues.iter().any(|known| known == &clue) {
        clues.push(clue);
    }
}
