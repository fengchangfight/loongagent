//! The context ledger (ADR-0006) — the **only** write path to the model.
//!
//! Every block that reaches the model is an entry here, with a category, a
//! source, a token count and a lifetime. Two promises come out of that
//! mechanically rather than by convention:
//!
//! * **"Why is this in my context?"** is a query ([`Ledger::explain`]), not an
//!   inference — and the Cursor-style context ring falls out of the same data.
//! * **Budget pressure is visible and ordered** ([`Ledger::evict_to_budget`]):
//!   stale tool output goes before prose (ADR-0006 §4), pinned entries never go,
//!   and the report says what left and why.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Lifetime {
    Turn,
    Session,
    Persistent,
}

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

/// One row of "why is this in my context?".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExplainRow {
    pub id: u64,
    pub category: &'static str,
    pub source: String,
    pub tokens: u64,
    pub pinned: bool,
}

/// Rough accounting until a real tokenizer is wired in: ~4 chars per token.
/// Deliberately conservative (rounds up) so budgets are not overshot.
pub fn estimate_tokens(text: &str) -> u64 {
    (text.chars().count() as u64 + 3) / 4
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct ClearReport {
    pub cleared: Vec<Entry>,
    pub tokens_freed: u64,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct EvictionReport {
    pub removed: Vec<Entry>,
    pub removed_tokens: u64,
    /// True when the budget is still exceeded after eviction — i.e. only pinned
    /// or protected entries remain. The caller must then compact, not evict.
    pub still_over_budget: bool,
}

#[derive(Debug, Default)]
pub struct Ledger {
    entries: Vec<Entry>,
    next_id: u64,
    budget: Option<u64>,
}

impl Ledger {
    pub fn new(budget: Option<u64>) -> Self {
        Self {
            entries: Vec::new(),
            next_id: 1,
            budget,
        }
    }

    pub fn budget(&self) -> Option<u64> {
        self.budget
    }

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

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

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

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn total_tokens(&self) -> u64 {
        self.entries.iter().map(|e| e.tokens).sum()
    }

    pub fn tokens_by_category(&self) -> BTreeMap<&'static str, u64> {
        let mut out = BTreeMap::new();
        for e in &self.entries {
            *out.entry(e.category.as_str()).or_insert(0) += e.tokens;
        }
        out
    }

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

    /// Drop non-pinned entries by eviction priority until the budget is met.
    /// Whole entries only — a half-evicted message is worse than a missing one.
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
