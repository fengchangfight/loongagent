//! Purpose: L2 episode digests, built deterministically from atoms so the record survives even when no model is installed.
//!
//! Serves: "what was asked, changed, run and verified" as computed facts, each traceable to a `seq` (ADR-0006, ADR-0017).
//!
//! # Contract
//! - **Design:** `docs/context/pipeline.md` §6.2–§6.3.
//! - **Upstream:** `Store::digest`, `Store::digest_pending` and `Store::synthesize_window` build segments and digests.
//! - **Downstream:** `crate::summaries::tail_summary` stores the result, and `render` produces the text `Store::recall`/`expand` return.
//! - **Verify:** `cargo test -p loong-context --test summaries` · `./scripts/check.sh`.

use crate::materialize::EntityKind;
use crate::summaries::AtomView;
use std::collections::{BTreeMap, BTreeSet};

/// A new user message only starts an episode once the current one has this much
/// material, so a back-and-forth that refines one request stays one episode.
pub const SEGMENT_MIN_EVENTS: usize = 4;
/// …or once this much wall-clock time has passed, whichever comes first.
pub const SEGMENT_GAP_MS: i64 = 10 * 60 * 1000;
/// Tool names (or argument shapes) that count as changing a file.

/// Purpose: One contiguous span of events that reads as a single unit of work.
/// Serves: a stable episode identity that survives the open tail growing, so recall can cite a boundary rather than a guess.
/// Design: `docs/context/pipeline.md` §6.3.
/// Up: `segment` builds it; `Store::digest` and `Store::digest_pending` consume it.
/// Down: `Segment::episode_id` and `Digest.segment`.
/// Verify: `cargo test -p loong-context --test summaries episodes_split`.
/// One contiguous span of events that reads as a single unit of work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    /// `seq` of its first event — the episode's stable seed.
    pub seq_from: u64,
    pub seq_to: u64,
    pub events: usize,
    pub first_time_ms: i64,
    pub last_time_ms: i64,
}

impl Segment {
    /// Purpose: The episode's id: the seed `seq` plus the end `seq`.
    /// Serves: an id that stays the same while the last episode grows, and cannot collide with another segmentation.
    /// Design: `docs/context/pipeline.md` §6.3.
    /// Up: `Digest::build`; no other caller.
    /// Down: Formats the two `seq` values; no callee.
    /// Verify: `cargo test -p loong-context --test summaries a_digest_is_deterministic`.
    /// The episode's id: derived from the seed and the end, so growing the last
    /// episode keeps its id while a different segmentation would not collide.
    pub fn episode_id(&self) -> String {
        format!("ep-{:08}-{:08}", self.seq_from, self.seq_to)
    }
}

/// Purpose: What a digest says about one test: how often it failed and passed.
/// Serves: keeping the `failed→ok` transition, the evidence that a fix was actually verified.
/// Design: `docs/context/pipeline.md` §6.2.
/// Up: `Digest::build` fills it from `tool.result` atoms; `render` and `DigestInput::prompt` read it.
/// Down: `TestRecord::result`.
/// Verify: `cargo test -p loong-context --test summaries a_test_that_failed_then_passed`.
/// What a digest says about a test: it may have failed first and passed later,
/// and that *transition* is the evidence that a fix was verified. Collapsing it
/// to "ok" would throw away the most valuable fact in the episode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestRecord {
    pub name: String,
    pub failures: u32,
    pub passes: u32,
}

impl TestRecord {
    /// Purpose: The `failed→ok`/`failed`/`ok` label for a test record.
    /// Serves: one wording shared by the rendered digest and the model prompt.
    /// Design: `docs/context/pipeline.md` §6.2.
    /// Up: `render` and `DigestInput::prompt`.
    /// Down: Matches on `failures`/`passes`; no callee.
    /// Verify: `cargo test -p loong-context --test summaries a_test_that_failed_then_passed`.
    pub fn result(&self) -> &'static str {
        match (self.failures > 0, self.passes > 0) {
            (true, true) => "failed→ok",
            (true, false) => "failed",
            (false, true) => "ok",
            (false, false) => "unknown",
        }
    }
}

/// Purpose: One attempt at changing a file, with the verification that followed it.
/// Serves: the sequence that lets a model tell an abandoned approach from an adopted one, instead of inventing one.
/// Design: `docs/context/pipeline.md` §6.2.
/// Up: `Digest::build` fills it from write calls and the next test result.
/// Down: `Digest.edits`, `render` and `DigestInput::prompt`.
/// Verify: `cargo test -p loong-context --test summaries touched_files`.
/// One attempt at changing a file, with whatever verification followed it.
///
/// This exists because of a measured failure. A digest carrying only *final state*
/// ("changed merge.rs", "layer_merge: failed→ok") gives a model no way to know what
/// was tried and abandoned — and a 7B model asked "which approaches were rejected?"
/// with that input **invented** one ("尝试直接修改merge函数"). The same model, given
/// the same episode with its attempts and their outcomes, answered correctly. The
/// lesson is the design note's §4.4 in its sharpest form: the model cannot infer
/// structure that was never written down, so the harness has to write it down.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub seq: u64,
    pub file: Option<String>,
    /// What the attempt changed, in a few words. The part that makes one attempt
    /// distinguishable from another — without it "which approach was rejected?" has
    /// no answer in the input, and a model asked anyway will invent one.
    pub attempt: Option<String>,
    /// The result of the *next* verification in the episode, when there is one.
    pub outcome: Option<bool>,
    pub outcome_seq: Option<u64>,
}

/// Purpose: The deterministic half of an L2 summary: rendered facts plus citations.
/// Serves: a summary that exists without a model and whose computed fields cannot be overwritten by one.
/// Design: `docs/context/pipeline.md` §6.2.
/// Up: `Digest::build` returns it; `Store::digest_pending` re-derives it.
/// Down: `Digest::rendered_tokens`, `crate::summaries::tail_summary` and `DigestInput::from_digest`.
/// Verify: `cargo test -p loong-context --test summaries`.
/// The deterministic half of an L2 summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Digest {
    pub episode_id: String,
    pub segment: Segment,
    pub title: String,
    /// The rendered body. Human-readable on purpose: this is what a person or a
    /// model reads when it expands the summary.
    pub content: String,
    /// Computed, never generated: see the field-ownership table in ADR-0017 §4.
    pub status: String,
    pub touched_files: Vec<String>,
    pub commands: Vec<String>,
    pub modules: Vec<String>,
    pub tests: Vec<TestRecord>,
    pub tools: Vec<String>,
    /// Attempts in order, each tied to the verification that followed it. This is
    /// what lets a model tell an abandoned approach from an adopted one.
    pub edits: Vec<Edit>,
    /// `seq` values that contributed to the digest, ascending.
    pub contributors: Vec<u64>,
    /// Merged `(seq_from, seq_to)` runs over `contributors`.
    pub citations: Vec<(u64, u64)>,
    pub tokens: u64,
}

impl Digest {
    /// Purpose: The token cost of a digest, title included.
    /// Serves: charging a summary for everything a reader actually receives, not only its body.
    /// Design: `docs/context/pipeline.md` §6.2.
    /// Up: `Digest::build` sets `Digest.tokens`.
    /// Down: `estimate_tokens` twice; no other callee.
    /// Verify: `cargo test -p loong-context --test summaries a_digest_is_deterministic`.
    /// Rendered text plus the title, which is what a token count must cover.
    pub fn rendered_tokens(&self) -> u64 {
        estimate_tokens(&self.title) + estimate_tokens(&self.content)
    }
}

/// Purpose: The crate's four-characters-per-token estimate.
/// Serves: one conservative counter for digests, windows and the L4 map, so budgets agree.
/// Design: `docs/design/context-ledger.md` §2.
/// Up: `Digest::build`, `Store::synthesize_window`, `global_map`, `l3_summary` and `Synthesis::estimated_tokens`.
/// Down: Counts characters and rounds up; no callee.
/// Verify: `cargo test -p loong-context --test summaries`.
/// The same ~4-chars-per-token rule the context ledger uses, deliberately
/// conservative so a digest is never under-counted when it competes for budget.
pub fn estimate_tokens(text: &str) -> u64 {
    (text.chars().count() as u64 + 3) / 4
}

/// Purpose: Split atoms into episodes at provable boundaries only.
/// Serves: a pure segmenter: the same atoms always give the same spans, and the open tail is always left open.
/// Design: `docs/context/pipeline.md` §6.3.
/// Up: `Store::digest` and `Store::digest_pending`.
/// Down: `span_of`.
/// Verify: `cargo test -p loong-context --test summaries episodes_split`.
/// Split atoms into episodes. Pure: the same atoms always give the same spans.
pub fn segment(events: &[(u64, i64, String, Option<String>)]) -> Vec<Segment> {
    let mut segments: Vec<Segment> = Vec::new();
    let mut current: Vec<usize> = Vec::new();

    for (index, (_, time_ms, kind, role)) in events.iter().enumerate() {
        let is_user = kind == "message.appended" && role.as_deref() == Some("user");
        if is_user && !current.is_empty() {
            let first = current[0];
            let elapsed = time_ms.saturating_sub(events[first].1);
            let enough_material = current.len() >= SEGMENT_MIN_EVENTS;
            let long_gap = elapsed >= SEGMENT_GAP_MS;
            if enough_material || long_gap {
                // The span starts at its own first event, not at the message that
                // closed it. Getting this wrong makes the range run backwards and
                // the episode id collide with the next episode's.
                segments.push(span_of(events, &current, events[current[0]].0));
                current.clear();
            }
        }
        current.push(index);
    }

    if !current.is_empty() {
        let first = current[0];
        segments.push(span_of(events, &current, events[first].0));
    }
    segments
}

/// Purpose: Build one `Segment` from a list of event indices.
/// Serves: the span arithmetic lives in one place, so a boundary cannot run backwards.
/// Design: `docs/context/pipeline.md` §6.3.
/// Up: `segment`.
/// Down: Constructs a `Segment`; no other callee.
/// Verify: `cargo test -p loong-context --test summaries episodes_split`.
fn span_of(
    events: &[(u64, i64, String, Option<String>)],
    indices: &[usize],
    seq_from: u64,
) -> Segment {
    let first = indices[0];
    let last = *indices.last().unwrap();
    Segment {
        seq_from,
        seq_to: events[last].0,
        events: indices.len(),
        first_time_ms: events[first].1,
        last_time_ms: events[last].1,
    }
}

/// Purpose: Build the digest for one segment from its atoms.
/// Serves: computing files, commands, tests, attempts, status and citations instead of asking a model to infer them.
/// Design: `docs/context/pipeline.md` §6.2 and `docs/context/data-model.md` §5.3.
/// Up: `Store::digest`, `Store::digest_pending` and `Store::synthesize_window`.
/// Down: `names_of`, `derive_status`, `title_from`, `render`, `merge_runs` and `estimate_tokens`.
/// Verify: `cargo test -p loong-context --test summaries`.
/// Build the digest for one segment. `atoms` must be that segment's atoms, in
/// `seq` order; `ok`/`tool` come from each atom.
pub fn build(segment: Segment, atoms: &[AtomView]) -> Digest {
    let mut touched_files = BTreeSet::new();
    let mut commands = BTreeSet::new();
    let mut modules = BTreeSet::new();
    let mut tools = BTreeSet::new();
    let mut tests: BTreeMap<String, TestRecord> = BTreeMap::new();
    let mut contributors = std::collections::BTreeSet::new();
    let mut asks: Vec<String> = Vec::new();
    let mut last_assistant: Option<String> = None;
    let mut pending_approval = false;
    // A tool result does not carry the tool's arguments, so whether it wrote a
    // file has to be looked up on the call it answers. Without this, an `edit`
    // call's file would be recorded and its *outcome* would not be attributable.
    let mut write_calls: BTreeMap<String, bool> = BTreeMap::new();

    for (position, atom) in atoms.iter().enumerate() {
        if !atom.tool.is_empty() {
            tools.insert(atom.tool.clone());
        }
        for name in names_of(atom, EntityKind::Command) {
            commands.insert(name);
        }
        for name in names_of(atom, EntityKind::Module) {
            modules.insert(name);
        }
        if atom.kind == "tool.call" {
            if let Some(call_id) = &atom.call_id {
                write_calls.insert(call_id.clone(), atom.is_write);
            }
        }
        let answer_to_write = atom
            .call_id
            .as_ref()
            .and_then(|call_id| write_calls.get(call_id))
            .copied()
            .unwrap_or(false);
        if atom.is_write || answer_to_write {
            // The file is named by the *call*'s arguments; the result only says
            // whether it worked.
            if atom.kind == "tool.call" {
                for name in names_of(atom, EntityKind::Path) {
                    touched_files.insert(name);
                }
            }
            contributors.insert(atom.seq);
        }
        if atom.kind == "tool.result" {
            // Tests this output mentions, plus tests named in the command that
            // produced it — `cargo test layer_merge` names one before it runs.
            let mut mentioned = atom.tests.clone();
            let names_in_text = names_of(atom, EntityKind::Test);
            mentioned.extend(names_in_text);
            mentioned.sort();
            mentioned.dedup();
            for name in &mentioned {
                let record = tests.entry(name.clone()).or_insert_with(|| TestRecord {
                    name: name.clone(),
                    failures: 0,
                    passes: 0,
                });
                if atom.ok == Some(true) {
                    record.passes += 1;
                } else {
                    record.failures += 1;
                }
            }
            // A failing result is exactly the evidence worth citing, and a
            // passing one is the evidence that a fix worked.
            if atom.ok == Some(false) || !mentioned.is_empty() {
                contributors.insert(atom.seq);
            }
            if atom.text.contains("pending approval") || atom.text.contains("等待批准") {
                pending_approval = true;
            }
        }
        if atom.kind == "message.appended" {
            match atom.role.as_str() {
                "user" => {
                    asks.push(atom.text.clone());
                    contributors.insert(atom.seq);
                }
                "assistant" => {
                    last_assistant = Some(atom.text.clone());
                    // The final assistant message of an episode is where the
                    // conclusion lives, so the last one is always cited.
                    if position + 1 == atoms.len() {
                        contributors.insert(atom.seq);
                    }
                }
                _ => {}
            }
        }
    }

    // The opening request is what the episode is *about*: it must be citable even
    // when the segment has little else.
    if let Some(first) = atoms
        .iter()
        .find(|a| a.kind == "message.appended" && a.role == "user")
    {
        contributors.insert(first.seq);
    }

    // Attempts in order, each paired with the first verification after it: an edit
    // followed by a failing test is a discarded approach, and the *pair* is the
    // fact. Either half alone is not evidence of anything.
    let mut edits: Vec<Edit> = Vec::new();
    for (position, atom) in atoms.iter().enumerate() {
        let is_attempt = atom.is_write
            || atom
                .call_id
                .as_ref()
                .and_then(|call_id| write_calls.get(call_id))
                .copied()
                .unwrap_or(false);
        if !is_attempt || atom.kind != "tool.call" {
            continue;
        }
        let file = names_of(atom, EntityKind::Path).into_iter().next();
        // The first verification at or after this attempt, within the episode.
        let outcome = atoms
            .iter()
            .skip(position)
            .find(|later| later.kind == "tool.result" && !later.tests.is_empty())
            .map(|later| (later.ok, later.seq));
        edits.push(Edit {
            seq: atom.seq,
            file,
            attempt: atom.write_attempt.clone(),
            outcome: outcome.and_then(|(ok, _)| ok),
            outcome_seq: outcome.map(|(_, at)| at),
        });
    }

    let status = derive_status(atoms, &tests, &touched_files, pending_approval);
    // The request is remembered with its role marker stripped for display.
    asks = asks
        .into_iter()
        .map(|ask| ask.trim_start_matches("[user] ").to_string())
        .collect();
    let title = title_from(asks.first(), &touched_files);
    let contributors: Vec<u64> = contributors.into_iter().collect();
    let citations = merge_runs(&contributors);

    let touched_files: Vec<String> = touched_files.into_iter().collect();
    let commands: Vec<String> = commands.into_iter().collect();
    let modules: Vec<String> = modules.into_iter().collect();
    let tools: Vec<String> = tools.into_iter().collect();
    let tests: Vec<TestRecord> = tests.into_values().collect();

    let content = render(
        2,
        &title,
        &status,
        asks.first().map(String::as_str),
        &touched_files,
        &commands,
        &tests,
        &edits,
        last_assistant.as_deref(),
        &citations,
    );

    let mut digest = Digest {
        episode_id: segment.episode_id(),
        segment,
        title,
        content,
        status,
        touched_files,
        commands,
        modules,
        tests,
        tools,
        edits,
        contributors,
        citations,
        tokens: 0,
    };
    digest.tokens = digest.rendered_tokens();
    digest
}

/// Purpose: The episode status computed from evidence: blocked, open, done or unknown.
/// Serves: the field a small model was measured getting wrong; it is derived here, never generated.
/// Design: `docs/context/pipeline.md` §6.2.
/// Up: `Digest::build`.
/// Down: Reads `TestRecord`, touched files and the approval marker; no callee.
/// Verify: `cargo test -p loong-context --test summaries status_is_computed_from_evidence`.
/// What an episode looks like as a fact. Computed from evidence, never guessed —
/// the measured failure mode was a 3B model calling a committed, passing task
/// `open`.
pub fn derive_status(
    atoms: &[AtomView],
    tests: &BTreeMap<String, TestRecord>,
    touched_files: &BTreeSet<String>,
    pending_approval: bool,
) -> String {
    if pending_approval {
        return "blocked".to_string();
    }
    let last_test_outcome = atoms
        .iter()
        .filter(|a| a.kind == "tool.result" && !a.tests.is_empty())
        .last()
        .and_then(|a| a.ok);
    if last_test_outcome == Some(false) {
        return "open".to_string();
    }
    if tests.values().any(|t| t.failures > 0) && last_test_outcome == Some(true) {
        return "done".to_string();
    }
    if tests.values().any(|t| t.passes > 0) && last_test_outcome.is_none() {
        return "done".to_string();
    }
    if !touched_files.is_empty() {
        return "open".to_string();
    }
    "unknown".to_string()
}

/// Purpose: The human-readable title: the first request plus the first touched file.
/// Serves: a title a person or a model can recognise without expanding the summary.
/// Design: `docs/context/pipeline.md` §6.2.
/// Up: `Digest::build`.
/// Down: Truncates the first ask; no callee.
/// Verify: `cargo test -p loong-context --test summaries episodes_split`.
fn title_from(first_ask: Option<&String>, touched_files: &BTreeSet<String>) -> String {
    let base = match first_ask {
        Some(ask) if !ask.is_empty() => {
            let trimmed = ask.trim();
            let head: String = trimmed.chars().take(40).collect();
            if trimmed.chars().count() > 40 {
                format!("{head}…")
            } else {
                head
            }
        }
        _ => "（无用户消息）".to_string(),
    };
    match touched_files.iter().next() {
        Some(file) => format!("{base} [{file}]"),
        None => base,
    }
}

/// Purpose: Render a digest as labelled plain text.
/// Serves: one format that both the model prompt and `recall`'s expanded answer read.
/// Design: `docs/context/pipeline.md` §6.2 and `docs/context/recall.md` §7.2.
/// Up: `Digest::build`.
/// Down: Writes the labelled lines; no callee.
/// Verify: `cargo test -p loong-context --test summaries`.
/// Render the digest. Deliberately plain: a model reads this in P5 as its input,
/// and a person reads it in `recall`. Both do better with labelled facts than
/// with prose.
#[allow(clippy::too_many_arguments)]
pub fn render(
    level: u32,
    title: &str,
    status: &str,
    ask: Option<&str>,
    touched_files: &[String],
    commands: &[String],
    tests: &[TestRecord],
    edits: &[Edit],
    conclusion: Option<&str>,
    citations: &[(u64, u64)],
) -> String {
    let mut out = String::new();
    out.push_str(&format!("L{level} · {title}\n"));
    out.push_str(&format!("状态：{status}\n"));
    if let Some(ask) = ask {
        out.push_str(&format!("请求：{ask}\n"));
    }
    if !touched_files.is_empty() {
        out.push_str(&format!("改动文件：{}\n", touched_files.join(", ")));
    }
    if !commands.is_empty() {
        out.push_str(&format!("命令：{}\n", commands.join(", ")));
    }
    if !tests.is_empty() {
        let rendered: Vec<String> = tests
            .iter()
            .map(|t| format!("{} ({})", t.name, t.result()))
            .collect();
        out.push_str(&format!("测试：{}\n", rendered.join(", ")));
    }
    if !edits.is_empty() {
        out.push_str("经过（按顺序，含每次尝试后的验证结果）：\n");
        for edit in edits {
            let file = edit.file.as_deref().unwrap_or("(未知文件)");
            let verdict = match edit.outcome {
                Some(true) => "随后验证通过",
                Some(false) => "随后验证失败",
                None => "随后没有验证",
            };
            let what = edit
                .attempt
                .as_deref()
                .map(|a| format!("：{a}"))
                .unwrap_or_default();
            out.push_str(&format!("  - 改 {file}{what}（{verdict}）\n"));
        }
    }
    // The abandoned attempts, computed rather than asked for. A model asked to copy
    // them out of a list it can already read does it unreliably — measured, a 7B
    // model quoted the right attempt on one case and *invented* a failing test on a
    // case whose test never failed. Since a failed attempt is a fact the harness
    // holds, deriving it here removes a whole failure mode and leaves the model only
    // what genuinely needs understanding: the reason, the open questions, the
    // conclusion.
    let abandoned: Vec<String> = edits
        .iter()
        .filter(|edit| edit.outcome == Some(false))
        .map(|edit| match (&edit.attempt, &edit.file) {
            (Some(attempt), Some(file)) => format!("改 {file}（{attempt}）"),
            (Some(attempt), None) => attempt.clone(),
            (None, Some(file)) => format!("改 {file}"),
            (None, None) => format!("第 {} 步", edit.seq),
        })
        .collect();
    if !abandoned.is_empty() {
        out.push_str(&format!(
            "被否决（随后验证失败）：{}\n",
            abandoned.join("；")
        ));
    }
    if let Some(conclusion) = conclusion {
        let trimmed = conclusion.trim();
        if !trimmed.is_empty() {
            out.push_str(&format!("最后结论：{trimmed}\n"));
        }
    }
    if !citations.is_empty() {
        let ranges: Vec<String> = citations
            .iter()
            .map(|(from, to)| {
                if from == to {
                    format!("{from}")
                } else {
                    format!("{from}-{to}")
                }
            })
            .collect();
        out.push_str(&format!("引用：{}\n", ranges.join(", ")));
    }
    out
}

/// Purpose: Append a model's answer to a rendered digest, strictly additively.
/// Serves: the model may add a reason, never remove or rewrite a computed fact.
/// Design: `docs/context/pipeline.md` §6.2.
/// Up: `Store::synthesize_window`.
/// Down: Reads `Synthesis`'s fields and extends the text; no callee.
/// Verify: `cargo test -p loong-context --test semantic a_models_answer_is_added`.
/// Append a model's answer to a rendered digest.
///
/// Strictly additive: no deterministic line is edited or removed, because those
/// lines are computed from evidence and a model's opinion does not outrank a test
/// result. If the model's `conclusion` disagrees with the computed status, both
/// remain visible — the disagreement is information, and silently preferring the
/// model is how a wrong summary becomes an invisible one.
pub fn merge_synthesis(content: &str, synthesis: &crate::summarizer::Synthesis) -> String {
    let mut out = content.trim_end().to_string();
    if !synthesis.rejected.is_empty() {
        out.push_str(&format!("被否决：{}\n", synthesis.rejected.join("；")));
    }
    if !synthesis.why.is_empty() {
        out.push_str(&format!("原因：{}\n", synthesis.why.join("；")));
    }
    if !synthesis.open_questions.is_empty() {
        out.push_str(&format!(
            "未决问题：{}\n",
            synthesis.open_questions.join("；")
        ));
    }
    if let Some(conclusion) = &synthesis.conclusion {
        out.push_str(&format!("模型结论：{conclusion}\n"));
    }
    out
}

/// Purpose: Collapse ascending `seq` values into inclusive runs.
/// Serves: a citation list that stays readable instead of one row per event.
/// Design: `docs/context/recall.md` §7.2.
/// Up: `Digest::build`.
/// Down: Groups the input; no callee.
/// Verify: `cargo test -p loong-context --test summaries every_citation_resolves`.
/// Collapse ascending `seq` values into inclusive runs, so a citation list stays
/// readable instead of turning into one row per event.
pub fn merge_runs(seqs: &[u64]) -> Vec<(u64, u64)> {
    let mut runs: Vec<(u64, u64)> = Vec::new();
    for seq in seqs {
        match runs.last_mut() {
            Some((_, to)) if *to + 1 == *seq => *to = *seq,
            _ => runs.push((*seq, *seq)),
        }
    }
    runs
}

/// Purpose: Entity names of one kind, owned so the caller can mutate its own collections.
/// Contract: `digest.rs` helpers.
/// Entity names of one kind, owned so the caller can hold them while it mutates
/// its own collections.
fn names_of(atom: &AtomView, kind: EntityKind) -> Vec<String> {
    atom.entities
        .iter()
        .filter(|(k, _)| *k == kind)
        .map(|(_, name)| name.clone())
        .collect()
}
