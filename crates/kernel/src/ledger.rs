//! Purpose: The context ledger: the account of every block the model is shown, with category, source, tokens, lifetime and pin.
//!
//! Serves: "why is this in my context?" answered from data the kernel owns, and a
//! budget that drops stale tool output before prose (ADR-0006).
//!
//! # Contract
//! - **Design:** `docs/design/context-ledger.md` §1–§4.
//! - **Upstream:** `Kernel::record_user`, `record_assistant`, `record_tool_result`,
//!   `seat_system_prompt` and `refresh_map` add entries; `compact_if_needed` clears
//!   and forgets.
//! - **Downstream:** `Kernel::context_explain`, `Kernel::compact_if_needed`,
//!   `Kernel::input_ceiling` and `Kernel::run_inner`'s routing facts.
//! - **Verify:** `cargo test -p loong-kernel ledger::` · `./scripts/check.sh`.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Purpose: Where a block came from, and therefore when it is evicted.
///
/// Serves: the eviction order that drops stale tool output before prose (ADR-0006 §4).
/// Design: `docs/design/context-ledger.md` §2.
/// Up: `Kernel::record_user`, `record_assistant` and `record_tool_result` tag history; `Kernel::seat_system_prompt` and `refresh_map` tag the pinned blocks.
/// Down: `Category::as_str`, `Category::eviction_priority` and `Entry.category`; no other callee.
/// Verify: `cargo test -p loong-kernel ledger::tests::retrieval_is_evicted_before_rules`.
/// Where a block came from. The eviction order is a property of the category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Category {
    SystemPrompt,
    Tools,
    Rules,
    Memory,
    /// The always-present global map (ADR-0017 §11): a few hundred tokens that
    /// say what the project is and where it stands. It sits between the rules and
    /// memory in eviction order — cheaper to lose than a rule the operator wrote,
    /// more valuable than a stale tool result.
    CognitiveMap,
    McpTools,
    Retrieval,
    History,
}

impl Category {
    /// Purpose: The wire name a context panel shows for this category.
    ///
    /// Serves: `context.explain` naming a category a client can render.
    /// Design: `docs/design/context-ledger.md` §2.
    /// Up: `Ledger::tokens_by_category`, `Ledger::explain` and `Kernel::context_explain`.
    /// Down: The `Category` variants; no callee.
    /// Verify: `cargo test -p loong-kernel turn::tests::the_context_view_says_what_the_next_request_will_carry`.
    pub fn as_str(self) -> &'static str {
        match self {
            Category::SystemPrompt => "system_prompt",
            Category::Tools => "tools",
            Category::Rules => "rules",
            Category::Memory => "memory",
            Category::CognitiveMap => "cognitive_map",
            Category::McpTools => "mcp_tools",
            Category::Retrieval => "retrieval",
            Category::History => "history",
        }
    }

    /// Purpose: The eviction order: a higher number leaves first.
    ///
    /// Serves: history before rules, and a system prompt last.
    /// Design: `docs/design/context-ledger.md` §3–§4.
    /// Up: `Ledger::evict_to_budget`.
    /// Down: The `Category` variants; category totals are not ordered by it.
    /// Verify: `cargo test -p loong-kernel ledger::tests::retrieval_is_evicted_before_rules`.
    /// Higher goes first. History (which is mostly tool output) is the cheapest
    /// to lose; the system prompt is the last thing to go.
    pub fn eviction_priority(self) -> u8 {
        match self {
            Category::History => 100,
            Category::Retrieval => 80,
            Category::McpTools => 70,
            Category::Memory => 50,
            Category::CognitiveMap => 45,
            Category::Rules => 40,
            Category::Tools => 30,
            Category::SystemPrompt => 0,
        }
    }
}

/// Purpose: How long a block is meant to live: this turn, the session, or the session's life.
///
/// Serves: the stated intent behind clearing: tool results are turn-scoped (ADR-0006 §4).
/// Design: `docs/design/context-ledger.md` §2.
/// Up: `Kernel::record_tool_result` (Turn), `Kernel::record_user` and `record_assistant` (Session), `Kernel::seat_system_prompt` (Persistent), and `Kernel::refresh_map` (Session).
/// Down: `Entry.lifetime` and `Kernel::context_explain`'s string view.
/// Verify: `cargo test -p loong-kernel turn::tests::the_ledger_and_the_conversation_stay_in_step`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Lifetime {
    Turn,
    Session,
    Persistent,
}

/// Purpose: One accounted block: provenance, cost and pin state, never the text itself.
///
/// Serves: the ledger answering 'why is this here?' without a second copy of the conversation.
/// Design: `docs/design/context-ledger.md` §1–§2.
/// Up: `Ledger::add` and `Ledger::clear_tool_outputs`.
/// Down: `Kernel::context_explain`, `Kernel::compact_if_needed` and the `entry_of` join.
/// Verify: `cargo test -p loong-kernel ledger::tests::tokens_are_counted_and_attributed`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub id: u64,
    pub category: Category,
    pub source: String,
    pub tokens: u64,
    pub lifetime: Lifetime,
    pub pinned: bool,
    /// Its content was replaced by a marker to free context (ADR-0006 §4).
    pub cleared: bool,
}

/// Purpose: One row of the explain query: entry id, category, source, tokens and pin.
///
/// Serves: the context panel's per-entry answer.
/// Design: `docs/design/context-ledger.md` §6.
/// Up: `Ledger::explain`.
/// Down: the ledger tests; `Kernel::context_explain` builds the wire view from `entries` instead.
/// Verify: `cargo test -p loong-kernel ledger::tests::explain_answers_why_each_entry_is_present`.
/// One row of "why is this in my context?".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExplainRow {
    pub id: u64,
    pub category: &'static str,
    pub source: String,
    pub tokens: u64,
    pub pinned: bool,
}

/// Purpose: Count roughly four characters per token, rounding up.
///
/// Serves: budgets that are not overshot before a real tokenizer exists.
/// Design: `docs/design/context-ledger.md` §2.
/// Up: `Ledger::add`, `replace` and `clear_tool_outputs`; re-exported from `crates/kernel/src/lib.rs`.
/// Down: no callee; it returns a count.
/// Verify: `cargo test -p loong-kernel ledger::tests::tokens_are_counted_and_attributed`.
/// Rough accounting until a real tokenizer is wired in: ~4 chars per token.
/// Deliberately conservative (rounds up) so budgets are not overshot.
pub fn estimate_tokens(text: &str) -> u64 {
    (text.chars().count() as u64 + 3) / 4
}

/// Purpose: What one clearing pass cleared and how many tokens it freed.
///
/// Serves: compaction reporting a real change rather than a guess.
/// Design: `docs/design/context-ledger.md` §4.
/// Up: `Ledger::clear_tool_outputs`.
/// Down: `Kernel::compact_if_needed` and `Kernel::replay_compaction`.
/// Verify: `cargo test -p loong-kernel turn::tests::compaction_clears_stale_tool_output_before_dropping_anything`.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ClearReport {
    pub cleared: Vec<Entry>,
    pub tokens_freed: u64,
}

/// Purpose: What eviction removed and whether only pinned entries remain.
///
/// Serves: the caller learning that eviction cannot meet the budget and compaction must run.
/// Design: `docs/design/context-ledger.md` §3.
/// Up: `Ledger::evict_to_budget`; no kernel path calls it today.
/// Down: the ledger tests, which check `still_over_budget`.
/// Verify: `cargo test -p loong-kernel ledger::tests::pinned_entries_are_never_evicted_and_the_report_says_so`.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct EvictionReport {
    pub removed: Vec<Entry>,
    pub removed_tokens: u64,
    /// True when the budget is still exceeded after eviction — i.e. only pinned
    /// or protected entries remain. The caller must then compact, not evict.
    pub still_over_budget: bool,
}

/// Purpose: The ordered account of every block the model is shown.
///
/// Serves: one place to add, clear, refresh and total the context.
/// Design: `docs/design/context-ledger.md` §1.
/// Up: held by `Kernel`; built in `Kernel::build`.
/// Down: `Entry` and the methods below; `Kernel::context_explain` reads the whole account.
/// Verify: `cargo test -p loong-kernel ledger::`.
#[derive(Debug, Default)]
pub struct Ledger {
    entries: Vec<Entry>,
    next_id: u64,
    budget: Option<u64>,
}

impl Ledger {
    /// Purpose: An empty ledger with an optional budget and ids starting at one.
    ///
    /// Serves: the kernel's one construction point.
    /// Design: `docs/design/context-ledger.md` §3.
    /// Up: `Kernel::build`; the ledger tests.
    /// Down: `Ledger::add`.
    /// Verify: `cargo test -p loong-kernel ledger::tests::tokens_are_counted_and_attributed`.
    pub fn new(budget: Option<u64>) -> Self {
        Self {
            entries: Vec::new(),
            next_id: 1,
            budget,
        }
    }

    /// Purpose: The configured ceiling, or `None` for unbounded.
    ///
    /// Serves: compaction deciding whether anything bounds the prompt.
    /// Design: `docs/design/context-ledger.md` §3.
    /// Up: `Kernel::input_ceiling` and `Kernel::context_explain`.
    /// Down: no callee; it reads a field.
    /// Verify: `cargo test -p loong-kernel turn::tests::narrow_only_numbers_are_the_strictest_of_the_two`.
    pub fn budget(&self) -> Option<u64> {
        self.budget
    }

    /// Purpose: Append one entry with a fresh id and return the id.
    ///
    /// Serves: the only append door, so an entry's provenance is decided once.
    /// Design: `docs/design/context-ledger.md` §1.
    /// Up: `Kernel::record_user`, `record_assistant`, `record_tool_result`, `seat_system_prompt` and `refresh_map`; session replay.
    /// Down: `estimate_tokens`; the `entries` vector.
    /// Verify: `cargo test -p loong-kernel ledger::tests::tokens_are_counted_and_attributed`.
    pub fn add(
        &mut self,
        category: Category,
        source: impl Into<String>,
        text: &str,
        lifetime: Lifetime,
        pinned: bool,
    ) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.entries.push(Entry {
            id,
            category,
            source: source.into(),
            tokens: estimate_tokens(text),
            lifetime,
            pinned,
            cleared: false,
        });
        id
    }

    /// Purpose: Every entry, in insertion order.
    ///
    /// Serves: the context panel and the token totals reading the whole account.
    /// Design: `docs/design/context-ledger.md` §6.
    /// Up: `Kernel::context_explain`; the ledger and turn tests.
    /// Down: no callee.
    /// Verify: `cargo test -p loong-kernel turn::tests::the_layered_engine_puts_a_map_in_front_of_the_model`.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Purpose: Rewrite one entry's text-derived token count, keeping its id, category, lifetime and pin.
    ///
    /// Serves: refreshing the L4 map in place instead of appending a copy each step (ADR-0017).
    /// Design: `docs/design/context-ledger.md` §7.
    /// Up: `Kernel::refresh_map`.
    /// Down: `estimate_tokens`; it also clears the `cleared` marker.
    /// Verify: `cargo test -p loong-kernel turn::tests::the_layered_engine_puts_a_map_in_front_of_the_model`.
    /// Rewrite the text behind one entry, keeping its id, category, lifetime and
    /// pin. Used for blocks whose *content* is refreshed rather than replaced —
    /// the cognitive map, which is re-derived as work proceeds.
    ///
    /// Deliberately not a general "edit the ledger" door: it cannot change what an
    /// entry *is*, only how large it currently is. A caller that could reclassify
    /// an entry could hide where context came from, which is the one thing the
    /// ledger exists to prevent (ADR-0006 §1).
    pub fn replace(&mut self, id: u64, text: &str) -> bool {
        match self.entries.iter_mut().find(|entry| entry.id == id) {
            Some(entry) => {
                entry.tokens = estimate_tokens(text);
                // A refreshed block is present again: it is not the marker of a
                // cleared tool output.
                entry.cleared = false;
                true
            }
            None => false,
        }
    }

    /// Purpose: How many entries the ledger holds.
    ///
    /// Serves: the lockstep assertion that every message has one entry.
    /// Design: `docs/design/context-ledger.md` §1.
    /// Up: the ledger and turn tests; no production caller.
    /// Down: no callee.
    /// Verify: `cargo test -p loong-kernel ledger::tests::pinned_entries_are_never_evicted_and_the_report_says_so`.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Purpose: Whether the ledger holds no entries at all.
    ///
    /// Serves: a caller that needs the empty case; no in-tree caller today.
    /// Design: `docs/design/context-ledger.md` §1.
    /// Up: no in-tree caller.
    /// Down: `len`.
    /// Verify: `cargo test -p loong-kernel ledger::`.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Purpose: The ledger's share of the prompt about to be sent.
    ///
    /// Serves: compaction and routing measuring the current request, not a cumulative spend.
    /// Design: `docs/design/context-ledger.md` §3.
    /// Up: `Kernel::input_ceiling`, `compact_if_needed`, `context_explain`, `select_model` and `run_inner`'s routing facts.
    /// Down: sums `Entry.tokens`.
    /// Verify: `cargo test -p loong-kernel turn::tests::the_ledger_and_the_conversation_stay_in_step`.
    pub fn total_tokens(&self) -> u64 {
        self.entries.iter().map(|e| e.tokens).sum()
    }

    /// Purpose: Token totals grouped by category name.
    ///
    /// Serves: the context panel showing where the budget went.
    /// Design: `docs/design/context-ledger.md` §6.
    /// Up: `Kernel::context_explain`.
    /// Down: `Category::as_str`.
    /// Verify: `cargo test -p loong-kernel ledger::tests::tokens_are_counted_and_attributed`.
    pub fn tokens_by_category(&self) -> BTreeMap<&'static str, u64> {
        let mut out = BTreeMap::new();
        for e in &self.entries {
            *out.entry(e.category.as_str()).or_insert(0) += e.tokens;
        }
        out
    }

    /// Purpose: One row per entry with its provenance, for 'why is this in my context?'.
    ///
    /// Serves: the explain query ADR-0006 promises.
    /// Design: `docs/design/context-ledger.md` §6.
    /// Up: the tests only; `Kernel::context_explain` builds the wire view from `entries`.
    /// Down: `Category::as_str`.
    /// Verify: `cargo test -p loong-kernel ledger::tests::explain_answers_why_each_entry_is_present`.
    /// "Why is this in my context?" — every entry with its provenance.
    pub fn explain(&self) -> Vec<ExplainRow> {
        self.entries
            .iter()
            .map(|e| ExplainRow {
                id: e.id,
                category: e.category.as_str(),
                source: e.source.clone(),
                tokens: e.tokens,
                pinned: e.pinned,
            })
            .collect()
    }

    /// Purpose: Replace stale tool bodies outside the newest `keep_newest_tokens` with a marker.
    ///
    /// Serves: phase 1 of compaction: clearing without dropping a tool pair.
    /// Design: `docs/design/context-ledger.md` §4.
    /// Up: `Kernel::compact_if_needed` and `Kernel::replay_compaction`.
    /// Down: `marker`, `estimate_tokens` and `ClearReport`.
    /// Verify: `cargo test -p loong-kernel turn::tests::compaction_clears_stale_tool_output_before_dropping_anything`.
    /// Phase 1 of compaction (ADR-0006 §4): clear the **stale tool output** first,
    /// because it is the bulk of a long session's context and the least valuable —
    /// the model can re-run a command, but it cannot re-read a decision.
    ///
    /// The newest `keep_newest_tokens` of context is left untouched, and pinned
    /// entries are never cleared. Nothing is removed here: a tool result stays in
    /// the conversation (so an assistant's tool call keeps its answer), only its
    /// body is replaced by what `marker` returns.
    pub fn clear_tool_outputs(
        &mut self,
        keep_newest_tokens: u64,
        marker: impl Fn(&Entry) -> String,
    ) -> ClearReport {
        // Walk backwards to find where the protected window starts.
        let mut protected_from = 0usize;
        let mut seen = 0u64;
        for (index, entry) in self.entries.iter().enumerate().rev() {
            seen += entry.tokens;
            if seen > keep_newest_tokens {
                protected_from = index + 1;
                break;
            }
        }
        // …but always keep the most recent entry, whatever the numbers say. A
        // single tool output bigger than the whole window would otherwise be
        // cleared the moment it arrived — exactly when the model needs it. A long
        // turn still gets its older steps cleared, which is the point.
        if !self.entries.is_empty() {
            protected_from = protected_from.min(self.entries.len() - 1);
        }

        let mut report = ClearReport::default();
        for entry in self.entries.iter_mut().take(protected_from) {
            if entry.pinned || entry.cleared {
                continue;
            }
            if entry.category != Category::History || !entry.source.starts_with("tool:") {
                continue;
            }
            let text = marker(entry);
            let before = entry.tokens;
            entry.tokens = estimate_tokens(&text);
            entry.cleared = true;
            report.tokens_freed += before.saturating_sub(entry.tokens);
            report.cleared.push(entry.clone());
        }
        report
    }

    /// Purpose: Remove entries by id after their messages were dropped, returning the tokens freed.
    ///
    /// Serves: keeping the ledger and conversation in step during phase 2.
    /// Design: `docs/design/context-ledger.md` §4.
    /// Up: `Kernel::drop_oldest_exchange`.
    /// Down: the `entries` vector; no callee.
    /// Verify: `cargo test -p loong-kernel turn::tests::compaction_drops_the_oldest_exchange_when_clearing_is_not_enough`.
    /// Forget entries entirely (phase 2, after their messages are dropped).
    pub fn forget(&mut self, ids: &[u64]) -> u64 {
        let mut freed = 0;
        self.entries.retain(|entry| {
            if ids.contains(&entry.id) {
                freed += entry.tokens;
                false
            } else {
                true
            }
        });
        freed
    }

    /// Purpose: Drop whole non-pinned entries by eviction priority until the budget holds.
    ///
    /// Serves: the ordered eviction ADR-0006 §3 describes.
    /// Design: `docs/design/context-ledger.md` §3.
    /// Up: no kernel path calls it; the ledger tests pin it.
    /// Down: `Category::eviction_priority` and `EvictionReport`.
    /// Verify: `cargo test -p loong-kernel ledger::tests::pinned_entries_are_never_evicted_and_the_report_says_so`.
    /// Whole entries only — a half-evicted message is worse than a missing one.
    pub fn evict_to_budget(&mut self) -> EvictionReport {
        let mut report = EvictionReport::default();
        let Some(budget) = self.budget else {
            return report;
        };
        while self.total_tokens() > budget {
            let victim = self
                .entries
                .iter()
                .enumerate()
                .filter(|(_, e)| !e.pinned)
                .max_by_key(|(i, e)| (e.category.eviction_priority(), *i))
                .map(|(i, _)| i);
            let Some(index) = victim else {
                report.still_over_budget = true;
                break;
            };
            let removed = self.entries.remove(index);
            report.removed_tokens += removed.tokens;
            report.removed.push(removed);
        }
        report
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Purpose: A ledger with a pinned system prompt, for eviction tests.
    ///
    /// Contract: `ledger.rs` tests.
    fn ledger_with_budget(b: u64) -> Ledger {
        let mut l = Ledger::new(Some(b));
        l.add(
            Category::SystemPrompt,
            "system",
            &"s".repeat(40),
            Lifetime::Session,
            true,
        );
        l
    }

    /// Purpose: Character counts become token totals and per-category attribution.
    #[test]
    fn tokens_are_counted_and_attributed() {
        let mut l = Ledger::new(None);
        l.add(
            Category::SystemPrompt,
            "system",
            &"a".repeat(40),
            Lifetime::Session,
            true,
        );
        l.add(
            Category::Rules,
            "AGENTS.md",
            &"b".repeat(8),
            Lifetime::Session,
            false,
        );
        assert_eq!(l.total_tokens(), 10 + 2);
        let by = l.tokens_by_category();
        assert_eq!(by["system_prompt"], 10);
        assert_eq!(by["rules"], 2);
    }

    /// Purpose: Eviction removes history before the pinned system prompt and says what left.
    #[test]
    fn eviction_drops_history_before_the_system_prompt() {
        let mut l = ledger_with_budget(40);
        l.add(
            Category::History,
            "tool:read_file",
            &"h".repeat(400),
            Lifetime::Session,
            false,
        );
        assert!(l.total_tokens() > 40);
        let report = l.evict_to_budget();
        assert_eq!(report.removed.len(), 1);
        assert_eq!(report.removed[0].category, Category::History);
        assert_eq!(report.removed[0].source, "tool:read_file");
        assert!(l.total_tokens() <= 40);
        assert!(!report.still_over_budget);
    }

    /// Purpose: A budget below the pinned prompt leaves eviction over budget instead of dropping the prompt.
    #[test]
    fn pinned_entries_are_never_evicted_and_the_report_says_so() {
        let mut l = ledger_with_budget(5); // the pinned system prompt alone is 10
        l.add(
            Category::History,
            "h",
            &"x".repeat(400),
            Lifetime::Session,
            false,
        );
        let report = l.evict_to_budget();
        assert!(report.still_over_budget, "only pinned entries remain");
        assert_eq!(l.len(), 1);
        assert_eq!(l.entries()[0].category, Category::SystemPrompt);
    }

    /// Purpose: Explain lists an entry's category, source, tokens and pin.
    #[test]
    fn explain_answers_why_each_entry_is_present() {
        let mut l = Ledger::new(None);
        l.add(
            Category::Retrieval,
            "repo-map",
            "abc",
            Lifetime::Turn,
            false,
        );
        let rows = l.explain();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].category, "retrieval");
        assert_eq!(rows[0].source, "repo-map");
        assert_eq!(rows[0].tokens, 1);
        assert!(!rows[0].pinned);
    }

    /// Purpose: Retrieval is evicted before rules when only one of them fits.
    #[test]
    fn retrieval_is_evicted_before_rules() {
        let mut l = Ledger::new(Some(4));
        l.add(
            Category::Rules,
            "rules",
            &"r".repeat(8),
            Lifetime::Session,
            false,
        ); // 2
        l.add(
            Category::Retrieval,
            "map",
            &"m".repeat(40),
            Lifetime::Turn,
            false,
        ); // 10
        let report = l.evict_to_budget();
        assert_eq!(report.removed.len(), 1);
        assert_eq!(report.removed[0].category, Category::Retrieval);
        assert_eq!(l.total_tokens(), 2);
    }
}
