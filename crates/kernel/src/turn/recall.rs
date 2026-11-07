//! Purpose: The read-only `recall` tool: search this session's derived memory and expand a summary into the events it cites.
//!
//! Serves: a thin foreground that can still answer "what did we do?" with evidence
//! rather than a derived summary taken on faith (`context-ledger.md` §5).
//!
//! # Contract
//! - **Design:** `docs/design/context-ledger.md` §5.
//! - **Upstream:** `Kernel::dispatch` answers `recall` before the registry;
//!   `Kernel::execute` has the matching arm; `Kernel::tool_specs` advertises it.
//! - **Downstream:** `loong_context::Store::{expand, recall_at, search}` and its
//!   `Summary`, `Hit` and `Expansion` views.
//! - **Verify:** `cargo test -p loong-kernel turn::tests::recall_finds_earlier_work_and_cites_it` · `./scripts/check.sh`.

use super::*;

impl Kernel {
    /// Purpose: Answer a `recall` call: expand one summary, or search the memory and cite the record.
    ///
    /// Serves: a read-only retrieval tool that never materialises or writes.
    /// Design: `docs/design/context-ledger.md` §5.
    /// Up: `Kernel::dispatch` (before the registry) and `Kernel::execute` (the matching arm); the turn tests.
    /// Down: `ContextEngine::store`, `Store::{expand, recall_at, search}`, `render_recall`, `render_expansion` and `cap_chars`.
    /// Verify: `cargo test -p loong-kernel turn::tests::recall_finds_earlier_work_and_cites_it`.
    /// The `recall` tool: search the derived memory, or expand one summary into the
    /// evidence beneath it.
    ///
    /// Read-only by construction — it never materialises, digests or writes; it asks
    /// questions of what is already there. That is why it needs no approval step:
    /// the answer is a *view* of the session's own record, which the model has
    /// already been given the whole of.
    pub(super) fn recall_tool(&self, args: &Json) -> (bool, String) {
        let Some(engine) = self.context_engine.as_ref() else {
            return (
                false,
                "no memory is available in this session (the context engine is off)".to_string(),
            );
        };
        if let Some(id) = args.get("expand").and_then(|v| v.as_str()) {
            // Expansion stands alone: mixing it with a search would make the result
            // shape depend on which keyword happened to be present.
            return match engine.store().expand(id, 24) {
                Ok(Some(expansion)) => (
                    true,
                    cap_chars(&render_expansion(&expansion), RECALL_MAX_CHARS),
                ),
                Ok(None) => (false, format!("no summary with id {id:?}")),
                Err(e) => (false, format!("expanding {id:?} failed: {e}")),
            };
        }

        let terms: Vec<String> = args
            .get("query")
            .and_then(|v| v.as_str())
            .map(split_terms)
            .unwrap_or_default();
        let entity = args.get("entity").and_then(|v| v.as_str());
        let tool = args.get("tool").and_then(|v| v.as_str());
        let level = args.get("level").and_then(|v| v.as_i64()).map(|l| l as u32);
        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(8)
            .clamp(1, 32) as usize;
        // One call carries evidence, so the model does not spend a second call (and a
        // second step) on `expand` just to see the words it matched.
        let excerpt_chars = args
            .get("excerpt_chars")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_EXCERPT_CHARS as u64)
            .clamp(1, MAX_EXCERPT_CHARS as u64) as usize;

        // A bare `entity` or `tool` with no words is a legitimate way to ask "what
        // touched src/config?" — it is not an error, so it is not reported as one.
        if terms.is_empty() && entity.is_none() && tool.is_none() {
            return (
                false,
                "give a `query`, an `entity`, a `tool`, or an `expand` id".to_string(),
            );
        }

        let search = loong_context::SearchQuery {
            terms: terms.clone(),
            limit,
            ..Default::default()
        };
        // Summaries first: they are the concentrated answer. Raw events are the
        // fallback, and they are what makes the memory complete rather than merely
        // convenient.
        let summaries = match engine.store().recall_at(level, &search) {
            Ok(found) => found,
            Err(e) => return (false, format!("searching summaries failed: {e}")),
        };
        let events = if summaries.is_empty() {
            let mut search = search.clone();
            search.kind = None;
            // Entity and tool filters apply to events; summaries have no tool.
            search.entity = entity.map(|e| e.to_string());
            search.tool = tool.map(|t| t.to_string());
            match engine.store().search(&search) {
                Ok(found) => found,
                Err(e) => return (false, format!("searching events failed: {e}")),
            }
        } else {
            Vec::new()
        };

        if summaries.is_empty() && events.is_empty() {
            let asked = terms.join(" ");
            return (
                true,
                format!(
                    "no match for: {asked}. That is a real answer, not an error: do not \
                     conclude from it that nothing happened — try different words, or expand \
                     an `ep` range directly."
                ),
            );
        }
        let rendered = self.render_recall(&summaries, &events, &terms, excerpt_chars);
        (true, cap_chars(&rendered, RECALL_MAX_CHARS))
    }

    /// Purpose: The best excerpt for a summary: the newest cited atom that contains a term.
    ///
    /// Serves: `Kernel::render_recall`, so a summary line carries the words it matched
    /// instead of a preview that starts before them.
    /// Design: `docs/design/context-ledger.md` §5.
    /// Up: `Kernel::render_recall`.
    /// Down: `ContextEngine::store`, `Store::atom_views` and `excerpt_around`.
    /// Verify: `cargo test -p loong-kernel turn::tests::recall_returns_an_evidence_excerpt_in_one_call`.
    fn summary_excerpt(
        &self,
        summary: &loong_context::Summary,
        terms: &[String],
        width: usize,
    ) -> Option<(u64, String)> {
        let store = self.context_engine.as_ref()?.store();
        let mut best: Option<(u64, String)> = None;
        for (from, to) in &summary.citations {
            let Ok(atoms) = store.atom_views(*from, *to) else {
                continue;
            };
            for atom in atoms {
                let Some(text) = excerpt_around(&atom.text, terms, width) else {
                    continue;
                };
                if best.as_ref().is_none_or(|(seq, _)| atom.seq > *seq) {
                    best = Some((atom.seq, text));
                }
            }
        }
        best
    }
}

/// The tool's name, in one place: it is used by `tool_specs`, by `dispatch` and by
/// the executor, and three spellings of a tool name is how a tool stops working.
pub(super) const RECALL_TOOL: &str = "recall";

/// The description the model reads. Written as an instruction because that is what
/// it is: the design's discipline ("recall before answering about the past, cite
/// evidence") only works if the tool tells the model how it is meant to be used.
pub(super) const RECALL_DESCRIPTION: &str = "Search this session's memory for earlier work. One call returns the original fragment around each match, with its [seq] citation — read it before asserting what was decided, verified or fixed. Pass \"expand\" with a summary id only when you want the whole summary together with the events it rests on. Summaries are a derived index, not the record: when they disagree with the evidence, the evidence is right.";

/// Purpose: The JSON schema the model is offered for `recall`.
///
/// Serves: a tool whose arguments match what `recall_tool` actually reads.
/// Design: `docs/design/context-ledger.md` §5.
/// Up: `Kernel::tool_specs` when memory exists.
/// Down: `json!`; no other callee.
/// Verify: `cargo test -p loong-kernel turn::tests::recall_is_offered_only_when_there_is_memory_behind_it`.
/// The tool's schema. Every field is optional except that `expand` stands alone.
pub(super) fn recall_parameters() -> Json {
    json!({
        "type": "object",
        "properties": {
            "query": {
                "type": "string",
                "description": "Words to look for. Chinese words of two characters work."
            },
            "entity": {
                "type": "string",
                "description": "Restrict to a file, module, tool, command, test or error, e.g. \"src/config\" or \"module:src/kernel\"."
            },
            "tool": {"type": "string", "description": "Restrict to one tool, e.g. \"edit_file\"."},
            "level": {
                "type": "integer",
                "description": "2 = task summary, 3 = module map, 4 = global map. Omit for all."
            },
            "expand": {
                "type": "string",
                "description": "A summary id from a previous recall; returns it with the original events it cites."
            },
            "excerpt_chars": {
                "type": "integer",
                "description": "How many characters of the original text to show around a match (default 240, capped at 800)."
            },
            "limit": {"type": "integer", "description": "Maximum results (default 8, capped at 32)."}
        }
    })
}

/// How many characters of the original text one hit shows around its match by default.
///
/// One sentence of a decision, or one clause of a constraint, is the unit a later turn
/// needs; a longer window would spend the context retrieval exists to save.
pub(super) const DEFAULT_EXCERPT_CHARS: usize = 240;

/// The most a caller may widen that window to.
pub(super) const MAX_EXCERPT_CHARS: usize = 800;

/// How many characters of retrieved memory one `recall` may put in front of the
/// model. The whole point of the thin foreground is that retrieval competes for
/// budget like everything else, so this is a ceiling rather than a hope.
///
/// When the ceiling is reached the renderer drops whole hits instead of cutting each
/// one: a cheap call must not become expensive because it matched a lot.
pub(super) const RECALL_MAX_CHARS: usize = 4_000;

/// Purpose: Split a query into search terms, keeping two-character Chinese words.
///
/// Serves: recall matching the words a person would recognise.
/// Design: `docs/design/context-ledger.md` §5.
/// Up: `Kernel::recall_tool`.
/// Down: `char::is_whitespace` and the punctuation list; no other callee.
/// Verify: `cargo test -p loong-kernel turn::tests::recall_finds_earlier_work_and_cites_it`.
/// Split a query into terms. Whitespace and the punctuation that would otherwise
/// glue words together; the two-character Chinese word is preserved, which is the
/// case a trigram index gets silently wrong.
fn split_terms(query: &str) -> Vec<String> {
    query
        .split(|c: char| {
            c.is_whitespace()
                || matches!(
                    c,
                    ',' | ';'
                        | '，'
                        | '；'
                        | '、'
                        | '。'
                        | '：'
                        | '（'
                        | '）'
                        | '('
                        | ')'
                        | '"'
                        | '\''
                        | '“'
                        | '”'
                        | '|'
                        | '/'
                )
        })
        .map(|term| term.trim())
        .filter(|term| !term.is_empty())
        .map(|term| term.to_string())
        .collect()
}

/// Purpose: Truncate a reply and say so, so a shortened answer never reads as the whole truth.
///
/// Contract: `turn/recall.rs` retrieval cap.
/// Truncate and say so. A silently shortened answer would read as "that is all
/// there was", which is the one thing a retrieval result must never imply.
fn cap_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let kept: String = text.chars().take(max).collect();
    format!("{kept}\n…[truncated — narrow the query for the rest]")
}

/// Purpose: The slice of one atom's text around the first query term, cut and marked.
///
/// Serves: the one-hop answer: the words the model matched, in their original sentence.
/// Design: `docs/design/context-ledger.md` §5.
/// Up: `Kernel::render_recall` and `Kernel::summary_excerpt`.
/// Down: `char::to_lowercase` matching over `str::find`; no other callee.
/// Verify: `cargo test -p loong-kernel turn::tests::recall_returns_an_evidence_excerpt_in_one_call`.
/// A window on the match, not the whole message. `None` when no term occurs, so a caller
/// can fall back instead of showing an arbitrary slice.
fn excerpt_around(text: &str, terms: &[String], width: usize) -> Option<String> {
    let lower = text.to_lowercase();
    let mut at: Option<usize> = None;
    for term in terms {
        let needle = term.trim().to_lowercase();
        if needle.is_empty() {
            continue;
        }
        if let Some(byte) = lower.find(&needle) {
            let ch = lower[..byte].chars().count();
            at = Some(at.map_or(ch, |best| best.min(ch)));
        }
    }
    let at = at?;
    let chars: Vec<char> = text.chars().collect();
    let width = width.max(1);
    let start = at.saturating_sub(width / 2);
    let end = (start + width).min(chars.len());
    let start = end.saturating_sub(width);
    let mut out = String::new();
    if start > 0 {
        out.push('…');
    }
    out.extend(&chars[start..end]);
    if end < chars.len() {
        out.push('…');
    }
    Some(out)
}

impl Kernel {
    /// Purpose: Render hits with a seq citation and the fragment around the match, within one budget.
    ///
    /// Serves: a reply the model can trace and read in one call, with the record distinguished from the index.
    /// Design: `docs/design/context-ledger.md` §5.
    /// Up: `Kernel::recall_tool`.
    /// Down: `Summary`, `Hit`, `Kernel::summary_excerpt`, `Store::atom_views` and `excerpt_around`.
    /// Verify: `cargo test -p loong-kernel turn::tests::recall_returns_an_evidence_excerpt_in_one_call` · `cargo test -p loong-kernel turn::tests::a_cleared_recall_result_can_be_recalled_again`.
    ///
    /// Render hits as an index the model can jump from. The citation is part of every line
    /// on purpose: a summary the model cannot trace is a claim, not memory.
    ///
    /// The budget is spent on whole hits, in order. When the next hit would cross it, the
    /// rest are counted instead of cut: a result that is cheap to ask for must not become
    /// expensive because the query matched often.
    fn render_recall(
        &self,
        summaries: &[loong_context::Summary],
        events: &[loong_context::Hit],
        terms: &[String],
        excerpt_chars: usize,
    ) -> String {
        let mut entries: Vec<String> = Vec::new();
        if !summaries.is_empty() {
            entries.push("memory (summaries; expand one before relying on it):\n".to_string());
            for summary in summaries {
                let ranges: Vec<String> = summary
                    .citations
                    .iter()
                    .map(|(from, to)| {
                        if from == to {
                            format!("{from}")
                        } else {
                            format!("{from}-{to}")
                        }
                    })
                    .collect();
                let mut entry = format!(
                    "- [{}] L{} {} ({}) seq[{}]\n",
                    summary.id,
                    summary.level,
                    summary.title,
                    summary.status,
                    ranges.join(",")
                );
                match self.summary_excerpt(summary, terms, excerpt_chars) {
                    Some((seq, text)) => {
                        entry.push_str(&format!("  evidence [seq {seq}]: {text}\n"))
                    }
                    None => {
                        // No cited event contains the words (a digest can carry them itself).
                        // Show the window on the summary, and say which of the two this is.
                        let text = excerpt_around(&summary.content, terms, excerpt_chars)
                            .unwrap_or_else(|| {
                                summary.content.chars().take(excerpt_chars).collect()
                            });
                        entry.push_str(&format!("  summary text: {text}\n"));
                    }
                }
                entries.push(entry);
            }
        }
        if !events.is_empty() {
            entries.push("memory (raw events; these are the record):\n".to_string());
            for hit in events {
                let who = hit
                    .tool
                    .clone()
                    .or_else(|| hit.role.clone())
                    .unwrap_or_else(|| hit.kind.clone());
                let text = self
                    .context_engine
                    .as_ref()
                    .and_then(|engine| engine.store().atom_views(hit.seq, hit.seq).ok())
                    .and_then(|atoms| {
                        atoms
                            .into_iter()
                            .find_map(|atom| excerpt_around(&atom.text, terms, excerpt_chars))
                    })
                    .unwrap_or_else(|| hit.preview.replace('\n', " "));
                entries.push(format!("- [{}] {} {}: {}\n", hit.seq, hit.kind, who, text));
            }
        }

        let budget = RECALL_MAX_CHARS.saturating_sub(80);
        let mut out = String::new();
        let mut used = 0usize;
        let mut omitted = 0usize;
        for entry in entries {
            let width = entry.chars().count();
            if used + width > budget {
                omitted += 1;
                continue;
            }
            used += width;
            out.push_str(&entry);
        }
        if omitted > 0 {
            out.push_str(&format!(
                "…[{omitted} more result(s) did not fit; narrow the query for the rest]\n"
            ));
        }
        out
    }
}

/// Purpose: Render one summary together with the original events it cites.
///
/// Serves: expansion returning evidence, not just a derived claim.
/// Design: `docs/design/context-ledger.md` §5.
/// Up: `Kernel::recall_tool`'s `expand` arm.
/// Down: `Expansion`, `Summary` and the evidence atoms; no other callee.
/// Verify: `cargo test -p loong-kernel turn::tests`; the expansion data is covered by `cargo test -p loong-context expand_returns_the_summary_and_the_raw_evidence_it_rests_on`.
/// Render a summary *with* the evidence it cites. The evidence is not a footnote:
/// returning it in the same response is the mechanism that stops a derived index
/// from being read as the record.
fn render_expansion(expansion: &loong_context::Expansion) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "summary [{}] L{} {} ({})\n{}\n",
        expansion.summary.id,
        expansion.summary.level,
        expansion.summary.title,
        expansion.summary.status,
        expansion.summary.content
    ));
    if expansion.evidence.is_empty() {
        out.push_str("(this summary cites no events)\n");
        return out;
    }
    out.push_str("evidence (the original events this summary rests on):\n");
    for atom in &expansion.evidence {
        let who = if !atom.tool.is_empty() {
            atom.tool.clone()
        } else if !atom.role.is_empty() {
            atom.role.clone()
        } else {
            atom.kind.clone()
        };
        let outcome = match atom.ok {
            Some(true) => " ok",
            Some(false) => " FAILED",
            None => "",
        };
        let text: String = atom.text.chars().take(300).collect();
        out.push_str(&format!(
            "[{}] {}{} {}: {}\n",
            atom.seq, atom.kind, outcome, who, text
        ));
    }
    out
}
