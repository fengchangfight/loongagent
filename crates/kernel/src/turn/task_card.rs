//! Purpose: Seat a small pinned card that says what the task is and what the person forbade, so neither can be compacted away.
//!
//! Serves: the one thing a summary cannot reconstruct — intent. A reasoning step can
//! be redone from the record, but "do not touch `b.txt`" is a preference, and once it
//! is compacted into a digest it is gone for good
//! (`docs/superpowers/specs/2026-11-07-finite-memory-unbounded-conversation-design.md` §4).
//!
//! # Contract
//! - **Design:** `docs/design/context-ledger.md` §4; the finite-memory spec §4 ("A 常驻").
//! - **Upstream:** `Kernel::run_inner` seats it at the turn's start and again at each
//!   budget renewal; `Kernel::seat_task_card` reads `self.messages` and the ledger.
//! - **Downstream:** `Ledger::{add, replace}`, the pinned `Category::Rules` entry and
//!   the leading `ChatMessage::System` block it rewrites in place.
//! - **Verify:** `cargo test -p loong-kernel turn::tests::the_task_card_survives_every_compaction_pass` · `./scripts/check.sh`.

use super::*;

/// The card's hard ceiling, in tokens. It sits in front of every request, so it is
/// paid for on every step: 800 tokens is about the most a goal and a handful of
/// constraints are worth, and the overflow rule below keeps it there.
pub(super) const TASK_CARD_MAX_TOKENS: u64 = 800;

/// How much of the goal the card keeps before it starts to be a transcript.
const GOAL_CHARS: usize = 200;

/// How many constraints the card tries to keep.
const MAX_CONSTRAINTS: usize = 8;

/// How many completed items the card shows before trimming them first.
const MAX_DONE: usize = 4;

/// Purpose: The deterministic facts a card is made of.
///
/// Serves: `render_card` taking facts rather than reading the kernel, so the overflow
/// rule is testable without a session.
/// Design: `docs/superpowers/specs/2026-11-07-finite-memory-unbounded-conversation-design.md` §4.
/// Up: `Kernel::card_facts`.
/// Down: `render_card` and the turn tests.
/// Verify: `cargo test -p loong-kernel turn::tests::an_overflowing_task_card_is_merged_not_evicted`.
pub(super) struct CardFacts {
    /// The person's opening request: what the session is for.
    pub goal: Option<String>,
    /// Sentences where the person said what not to do.
    pub constraints: Vec<String>,
    /// What recent calls completed, newest last.
    pub done: Vec<String>,
    /// The last command that verified something.
    pub verification: Option<String>,
}

/// Purpose: Render the card from facts, trimming detail before intent.
///
/// Serves: a card that always fits, with the goal never the thing that is cut.
/// Design: `docs/superpowers/specs/2026-11-07-finite-memory-unbounded-conversation-design.md` §4.
/// Up: `Kernel::seat_task_card`; the turn tests.
/// Down: `crate::ledger::estimate_tokens`.
/// Verify: `cargo test -p loong-kernel turn::tests::an_overflowing_task_card_is_merged_not_evicted`.
///
/// The order is deliberate: completed detail first, then the verification line, and
/// only then are constraints folded into a reference. The goal is never touched —
/// a card without it is a card that cannot say why the turn exists.
pub(super) fn render_card(facts: &CardFacts) -> String {
    let goal = facts
        .goal
        .as_deref()
        .map(|goal| goal.chars().take(GOAL_CHARS).collect::<String>())
        .unwrap_or_else(|| "(the person has not stated a goal yet)".to_string());
    let constraints: Vec<&String> = facts.constraints.iter().take(MAX_CONSTRAINTS).collect();
    let done: Vec<&String> = facts.done.iter().rev().take(MAX_DONE).collect();

    let render = |constraints: &[&String], done: &[&String], verification: Option<&str>| {
        let mut out = format!("任务卡（常驻，不参与压缩）\n目标：{goal}\n");
        if constraints.is_empty() {
            out.push_str("约束：尚未明说\n");
        } else {
            out.push_str("约束：\n");
            for constraint in constraints {
                out.push_str(&format!("- {constraint}\n"));
            }
        }
        if !done.is_empty() {
            out.push_str("最近完成：\n");
            for item in done {
                out.push_str(&format!("- {item}\n"));
            }
        }
        if let Some(verification) = verification {
            out.push_str(&format!("最近验证：{verification}\n"));
        }
        out.push_str("更早的细节不在卡上：存根里有 seq 区间，用 recall 查。\n");
        out
    };

    let over = |text: &str| crate::ledger::estimate_tokens(text) > TASK_CARD_MAX_TOKENS;
    let mut text = render(&constraints, &done, facts.verification.as_deref());
    // 1. Completed detail is the first thing that goes: it is reproducible by doing
    //    the work again, unlike a stated preference.
    let mut trimmed = done.clone();
    while over(&text) && !trimmed.is_empty() {
        trimmed.pop();
        text = render(&constraints, &trimmed, facts.verification.as_deref());
    }
    // 2. Then the verification line, which the log already records.
    if over(&text) {
        text = render(&constraints, &[], None);
    }
    // 3. Only now are constraints folded: the first one stays, the rest become a
    //    reference. The goal is still never cut.
    if over(&text) && constraints.len() > 1 {
        let first: Vec<&String> = constraints.iter().take(1).copied().collect();
        let mut folded = render(&first, &[], None);
        folded.push_str(&format!(
            "其余 {} 条约束已并成引用：用 recall 查「不要 / 必须 / 禁止」的原文。\n",
            constraints.len() - 1
        ));
        text = folded;
    }
    text
}

impl Kernel {
    /// Purpose: The card's facts, read from the conversation and the ledger, with no model call.
    ///
    /// Serves: a card that says only what the record already says.
    /// Design: `docs/superpowers/specs/2026-11-07-finite-memory-unbounded-conversation-design.md` §4.
    /// Up: `Kernel::seat_task_card`.
    /// Down: `self.messages`, `statement_is_a_constraint` and `is_failed_output`.
    /// Verify: `cargo test -p loong-kernel turn::tests::the_task_card_survives_every_compaction_pass`.
    pub(super) fn card_facts(&self) -> CardFacts {
        let start = self
            .messages
            .iter()
            .take_while(|m| matches!(m, ChatMessage::System { .. }))
            .count();
        // The goal and the constraints are **kept**, not re-derived: once their
        // messages become stubs, re-deriving replaces the task with the latest
        // "continue" and loses the veto for good.
        let mut constraints = self.card_constraints.clone();
        if self.card_goal.is_none() {
            for message in &self.messages[start..] {
                let ChatMessage::User { text } = message else {
                    continue;
                };
                for sentence in text.split(['\n', '。', '；', ';', '.', '！', '!']) {
                    let sentence = sentence.trim();
                    if sentence.is_empty() || !statement_is_a_constraint(sentence) {
                        continue;
                    }
                    let sentence = first_sentence(sentence);
                    if !constraints.iter().any(|known| known == &sentence) {
                        constraints.push(sentence);
                    }
                }
            }
        }
        let mut tools: std::collections::BTreeMap<String, String> =
            std::collections::BTreeMap::new();
        let mut done: Vec<String> = Vec::new();
        let mut verification: Option<String> = None;
        for message in &self.messages[start..] {
            match message {
                ChatMessage::Assistant { tool_calls, .. } => {
                    for call in tool_calls {
                        tools.insert(call.call_id.clone(), call.tool.clone());
                        if call.tool == "run_command" {
                            if let Some(command) = call.args.get("command").and_then(|v| v.as_str())
                            {
                                verification = Some(command.to_string());
                            }
                        }
                    }
                }
                ChatMessage::Tool { call_id, output } => {
                    let tool = tools.get(call_id).cloned().unwrap_or_default();
                    if tool.is_empty() || is_failed_output(output) {
                        continue;
                    }
                    let what = output.lines().next().unwrap_or_default();
                    let what: String = what.chars().take(60).collect();
                    done.push(format!("{tool}: {what}"));
                }
                _ => {}
            }
        }
        CardFacts {
            goal: self.card_goal.clone(),
            constraints,
            done,
            verification,
        }
    }

    /// Purpose: Seat the pinned task card once per turn, and after a renewal.
    ///
    /// Serves: a goal and its constraints that survive every compaction pass.
    /// Design: `docs/superpowers/specs/2026-11-07-finite-memory-unbounded-conversation-design.md` §4.
    /// Up: `Kernel::run_inner`, before the first request and at each budget renewal.
    /// Down: `Kernel::card_facts`, `render_card`, `Ledger::{add, replace}`.
    /// Verify: `cargo test -p loong-kernel turn::tests::the_task_card_survives_every_compaction_pass`.
    ///
    /// Seating copies `seat_map`: one leading `System` block, rewritten only when the
    /// text really changed. The card sits before the conversation, so a write with the
    /// same bytes would still discard the provider's cache for no gain.
    pub(super) fn seat_task_card(&mut self) {
        self.learn_card_facts();
        let facts = self.card_facts();
        // A card with nothing on it but the goal is a step-by-step cost that holds
        // nothing back: the goal is in the foreground anyway, and it can be re-read
        // from a stub. It is the stated constraint — the one thing a summary cannot
        // reconstruct — that earns a pinned block.
        if facts.constraints.is_empty() {
            return;
        }
        let text = render_card(&facts);
        match (self.card_entry, self.card_index) {
            (Some(entry), Some(index)) if index < self.messages.len() => {
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
                    Category::Rules,
                    "task-card",
                    &text,
                    Lifetime::Session,
                    // Pinned: a card that can be evicted disappears exactly when the
                    // context is tight, which is when the constraint matters most.
                    true,
                );
                // After the map, before the conversation: both are pinned blocks and
                // both must stay in front of the messages they summarise.
                let at = self
                    .messages
                    .iter()
                    .take_while(|m| matches!(m, ChatMessage::System { .. }))
                    .count();
                self.messages.insert(at, ChatMessage::System { text });
                self.entry_of.insert(at, Some(entry));
                self.card_entry = Some(entry);
                self.card_index = Some(at);
            }
        }
    }
}

impl Kernel {
    /// Purpose: Take the goal and the constraints out of the conversation once, and keep them.
    ///
    /// Serves: a card that still knows the task after the message that stated it became a stub.
    /// Design: `docs/superpowers/specs/2026-11-07-finite-memory-unbounded-conversation-design.md` §4.
    /// Up: `Kernel::seat_task_card`.
    /// Down: `Kernel.card_goal`, `Kernel.card_constraints`, `first_sentence` and `statement_is_a_constraint`.
    /// Verify: `cargo test -p loong-kernel turn::tests::the_task_card_survives_every_compaction_pass`.
    fn learn_card_facts(&mut self) {
        let start = self
            .messages
            .iter()
            .take_while(|m| matches!(m, ChatMessage::System { .. }))
            .count();
        // The goal is the newest request that states one. "继续" is not a new
        // task, and re-deriving the goal from it would quietly replace the task
        // with the word "continue".
        let newest = self.messages[start..].iter().rev().find_map(|m| match m {
            ChatMessage::User { text } => Some(text),
            _ => None,
        });
        if let Some(text) = newest {
            if !text.trim().is_empty() && !is_continuation(text) {
                self.card_goal = Some(first_sentence(text));
            }
        }
        for message in &self.messages[start..] {
            let ChatMessage::User { text } = message else {
                continue;
            };
            for sentence in text.split(['\n', '。', '；', ';', '.', '！', '!']) {
                let sentence = sentence.trim();
                if sentence.is_empty() || !statement_is_a_constraint(sentence) {
                    continue;
                }
                let sentence = first_sentence(sentence);
                if !self.card_constraints.iter().any(|known| known == &sentence) {
                    self.card_constraints.push(sentence);
                }
            }
        }
    }
}

/// Purpose: Whether one sentence is the person saying what must or must not happen.
///
/// Serves: reading constraints out of the record without inventing any.
/// Design: `docs/superpowers/specs/2026-11-07-finite-memory-unbounded-conversation-design.md` §4.
/// Up: `Kernel::card_facts`.
/// Down: no callee; it matches marker words.
/// Verify: `cargo test -p loong-kernel turn::tests::the_task_card_survives_every_compaction_pass`.
///
/// Deliberately a marker list and nothing else. There is no `/constraint` command in
/// this tree, so a sentence without a marker is left out rather than guessed at: a
/// constraint the model invented is worse than one it did not see.
fn statement_is_a_constraint(sentence: &str) -> bool {
    const MARKERS: [&str; 10] = [
        "不要", "别", "必须", "不能", "禁止", "不得", "do not", "don't", "must not", "never",
    ];
    let lower = sentence.to_lowercase();
    MARKERS.iter().any(|marker| lower.contains(marker))
}

/// Purpose: Whether a prompt is "keep going" rather than a new task.
///
/// Serves: `Kernel::learn_card_facts`, so a renewal or a "continue" turn does not
/// replace the goal with the word that was typed to resume it.
/// Design: `docs/superpowers/specs/2026-11-07-finite-memory-unbounded-conversation-design.md` §4.
/// Up: `Kernel::learn_card_facts`.
/// Down: no callee; it compares short marker words.
/// Verify: `cargo test -p loong-kernel turn::tests::the_task_card_survives_every_compaction_pass`.
fn is_continuation(text: &str) -> bool {
    const MARKERS: [&str; 8] = [
        "继续",
        "接着",
        "往下",
        "go on",
        "continue",
        "keep going",
        "ok",
        "好的",
    ];
    let text = text.trim().to_lowercase();
    MARKERS.iter().any(|marker| {
        text == *marker
            || text.starts_with(&format!("{marker}，"))
            || text.starts_with(&format!("{marker} "))
    })
}

/// Purpose: The first sentence-ish run of one message, trimmed.
///
/// Contract: `turn/task_card.rs` text helpers.
fn first_sentence(text: &str) -> String {
    let text = text.trim();
    let cut = text.find(['\n', '。', '；', ';']).unwrap_or(text.len());
    text[..cut].trim().to_string()
}

/// Purpose: Whether a tool output reads as a failure, so it is not counted as completed work.
///
/// Contract: `turn/task_card.rs` text helpers.
fn is_failed_output(output: &str) -> bool {
    output.contains("FAILED") || output.contains("error[") || output.contains("error:")
}
