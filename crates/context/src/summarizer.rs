//! Purpose: The optional local model: the few semantic fields a deterministic pass cannot fill, and the budget around asking.
//!
//! Serves: the "additive, never authoritative" rule: a model may explain, but it cannot overwrite status, citations or boundaries.
//!
//! # Contract
//! - **Design:** `docs/context/pipeline.md` §6.2 and `docs/context/config.md` §11.
//! - **Upstream:** `loong-kernel`'s `ContextEngine` chooses an implementation; `Store::synthesize_window` calls it off the critical path.
//! - **Downstream:** `crate::digest::merge_synthesis` merges the answer; the `producer` column records which model (if any) wrote it.
//! - **Verify:** `cargo test -p loong-context` · `./scripts/check.sh`.

use crate::digest::{Digest, TestRecord};
use serde_json::{json, Value};
use std::time::Duration;

/// Purpose: A factual brief for the model: the digest's computed facts plus the question.
/// Serves: input quality deciding the answer, because the model is handed labelled facts rather than raw events.
/// Design: `docs/context/pipeline.md` §6.2.
/// Up: `DigestInput::from_digest` builds it from a digest; `Store::synthesize_window` asks with it.
/// Down: `DigestInput::prompt` and `DigestInput::schema`.
/// Verify: `cargo test -p loong-context summarizer::tests::the_request_disables_thinking_and_carries_the_schema`.
/// A factual brief for the model: the digest, plus the question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DigestInput {
    /// The episode's request, already trimmed by the digest.
    pub request: String,
    pub status: String,
    pub touched_files: Vec<String>,
    pub commands: Vec<String>,
    pub tests: Vec<TestRecord>,
    /// Attempts in order with the verification that followed each. The model needs
    /// this to tell an abandoned approach from an adopted one — measured: without
    /// it, a 7B model *invented* a rejected approach, and with it the same model
    /// answered correctly.
    pub edits: Vec<crate::digest::Edit>,
    /// The digest's own rendering, for context. Never re-parsed.
    pub rendered: String,
}

impl DigestInput {
    /// Purpose: Build the brief from a rendered digest.
    /// Serves: the model receiving the same computed facts the digest already established.
    /// Design: `docs/context/pipeline.md` §6.2.
    /// Up: `Store::synthesize_window`.
    /// Down: Reads `Digest`'s fields; no other callee.
    /// Verify: `cargo test -p loong-context summarizer::tests::the_request_disables_thinking_and_carries_the_schema`.
    pub fn from_digest(digest: &Digest) -> Self {
        Self {
            request: digest
                .content
                .lines()
                .find(|l| l.starts_with("请求："))
                .map(|l| l.trim_start_matches("请求：").to_string())
                .unwrap_or_else(|| digest.title.clone()),
            status: digest.status.clone(),
            touched_files: digest.touched_files.clone(),
            commands: digest.commands.clone(),
            tests: digest.tests.clone(),
            edits: digest.edits.clone(),
            rendered: digest.content.clone(),
        }
    }

    /// Purpose: The prompt: few labelled facts and one question.
    /// Serves: a small model answering correctly instead of copying a raw event list.
    /// Design: `docs/context/pipeline.md` §6.2.
    /// Up: `OllamaSummarizer::body`.
    /// Down: Reads the brief's fields; no callee.
    /// Verify: `cargo test -p loong-context summarizer::tests::the_request_disables_thinking_and_carries_the_schema`.
    /// The prompt. Short on purpose: a small model does better with few, labelled
    /// facts and one question than with a transcript and an essay of instructions.
    pub fn prompt(&self) -> String {
        let mut out = String::new();
        out.push_str("已完成的工作（这些是事实，不是候选方案）：\n");
        out.push_str(&format!("- 请求：{}\n", self.request));
        if !self.touched_files.is_empty() {
            out.push_str(&format!("- 改动文件：{}\n", self.touched_files.join(", ")));
        }
        if !self.commands.is_empty() {
            out.push_str(&format!("- 运行命令：{}\n", self.commands.join(", ")));
        }
        for test in &self.tests {
            out.push_str(&format!("- 测试 {}：{}\n", test.name, test.result()));
        }
        if !self.edits.is_empty() {
            // The sequence is the point: an edit followed by a failing test is a
            // discarded approach, and the pair is the fact. Without this the model
            // has only the final state and cannot know what was abandoned —
            // measured, it invents rather than abstains.
            out.push_str("- 操作顺序（每次尝试及其后的验证）：\n");
            for edit in &self.edits {
                let file = edit.file.as_deref().unwrap_or("未知文件");
                let verdict = match edit.outcome {
                    Some(true) => "随后验证通过",
                    Some(false) => "随后验证失败",
                    None => "随后没有验证",
                };
                let what = edit
                    .attempt
                    .as_deref()
                    .map(|a| format!("【{a}】"))
                    .unwrap_or_default();
                out.push_str(&format!("  {}. 改 {file}{what} → {verdict}\n", edit.seq));
            }
        }
        out.push_str(&format!("- 程序判定的状态：{}\n", self.status));
        out.push_str(
            // `rejected` is deliberately **not** asked for: it is computed from the
            // attempt sequence, which the harness already holds. Asking a model to
            // copy a list it can read was measured to fail both ways — right answers
            // on some cases, an invented failing test on another.
            "\n请用 JSON 回答：\n\
             1. why：最终采用的做法为什么可行，或这个问题的根因是什么。\n\
             2. open_questions：还没解决的问题；没有就填空数组。\n\
             3. conclusion：一句话结论。\n\
             不要复述上面的事实，不要输出 JSON 以外的内容。",
        );
        out
    }

    /// Purpose: The JSON schema handed to the runtime.
    /// Serves: structured output enforced rather than requested in prose.
    /// Design: `docs/context/pipeline.md` §6.2.
    /// Up: `OllamaSummarizer::body`.
    /// Down: Builds the schema value; no callee.
    /// Verify: `cargo test -p loong-context summarizer::tests::the_request_disables_thinking_and_carries_the_schema`.
    /// The JSON schema handed to the runtime, so the shape is enforced rather than
    /// hoped for.
    pub fn schema() -> Value {
        // Three fields, not four: `rejected` is computed, so it is not in the contract
        // the model is held to. A field a model can fill wrongly is a field that will
        // eventually be filled wrongly.
        json!({
            "type": "object",
            "properties": {
                "open_questions": {"type": "array", "items": {"type": "string"}},
                "why": {"type": "string"},
                "conclusion": {"type": "string"}
            },
            "required": ["why", "conclusion"]
        })
    }
}

/// Purpose: What the model is allowed to contribute.
/// Serves: a field a model can fill wrongly is not in the contract, so `rejected` and the facts stay computed.
/// Design: `docs/context/pipeline.md` §6.2.
/// Up: `Synthesis::from_json` builds it; `Summarizer::summarize` returns it.
/// Down: `crate::digest::merge_synthesis` and `Synthesis::is_empty`/`estimated_tokens`.
/// Verify: `cargo test -p loong-context summarizer::tests::a_schema_shaped_answer_becomes_a_synthesis`.
/// What the model is allowed to contribute. Everything else in a summary is
/// computed, and there is deliberately no field here for it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Synthesis {
    pub rejected: Vec<String>,
    pub open_questions: Vec<String>,
    pub why: Vec<String>,
    pub conclusion: Option<String>,
}

impl Synthesis {
    /// Purpose: Whether the model said nothing.
    /// Serves: an empty answer being recorded as asked instead of laundered into a confident one.
    /// Design: `docs/context/pipeline.md` §6.2.
    /// Up: `Synthesis::from_json` and `Store::synthesize_window`.
    /// Down: Reads the fields; no callee.
    /// Verify: `cargo test -p loong-context summarizer::tests::an_answer_with_none_of_the_fields_is_refused`.
    pub fn is_empty(&self) -> bool {
        self.rejected.is_empty()
            && self.open_questions.is_empty()
            && self.why.is_empty()
            && self.conclusion.is_none()
    }

    /// Purpose: The window cost of the fields a model is allowed to fill.
    /// Serves: charging the window for the model's addition, not for the digest it already had.
    /// Design: `docs/context/status.md` §11.1.
    /// Up: `Store::synthesize_window`.
    /// Down: `crate::digest::estimate_tokens`.
    /// Verify: `cargo test -p loong-context --test semantic a_window_stops_at_its_token_budget`.
    /// What this answer is likely to cost the window's token budget. The same
    /// estimate the rest of the crate uses, summed over the fields a model is
    /// allowed to fill — not the merged summary, which would charge the window for
    /// the deterministic digest it already had.
    pub fn estimated_tokens(&self) -> u64 {
        self.rejected
            .iter()
            .chain(self.open_questions.iter())
            .chain(self.why.iter())
            .chain(self.conclusion.iter())
            .map(|s| crate::digest::estimate_tokens(s))
            .sum()
    }

    /// Purpose: Accept a model's JSON answer, keeping only the fields it may fill.
    /// Serves: refusing an empty or off-schema answer instead of inventing confidence.
    /// Design: `docs/context/pipeline.md` §6.2.
    /// Up: `OllamaSummarizer::parse_response`.
    /// Down: Reads the JSON value; no callee.
    /// Verify: `cargo test -p loong-context summarizer::tests::a_schema_shaped_answer_becomes_a_synthesis`.
    /// Accept a model's answer, keeping only what it is allowed to say and
    /// refusing to launder an empty answer into a confident one.
    pub fn from_json(value: &Value) -> Result<Self, SummarizerError> {
        let strings = |key: &str| -> Vec<String> {
            value
                .get(key)
                .and_then(|v| v.as_array())
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|i| i.as_str())
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect()
                })
                .unwrap_or_default()
        };
        let why = value
            .get("why")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            // A model that says it cannot tell is more useful than one that
            // invents a reason, so this is kept rather than dropped.
            .filter(|s| s != "无法判断" && !s.eq_ignore_ascii_case("unknown"))
            .into_iter()
            .collect();
        let conclusion = value
            .get("conclusion")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let synthesis = Self {
            rejected: strings("rejected"),
            open_questions: strings("open_questions"),
            why,
            conclusion,
        };
        if synthesis.is_empty() && value.get("why").is_none() {
            return Err(SummarizerError::Malformed(
                "the answer carried none of the fields it was asked for".to_string(),
            ));
        }
        Ok(synthesis)
    }
}

/// Purpose: Why an enrichment call could not be used.
/// Serves: an absent model degrading to the digest while a broken one is reported.
/// Design: `docs/context/status.md` §11.1.
/// Up: `Summarizer::summarize` and the kernel's enrichment loop.
/// Down: The `Display` impl below.
/// Verify: `cargo test -p loong-context --test semantic a_model_that_breaks_mid_pass`.
#[derive(Debug)]
pub enum SummarizerError {
    /// The endpoint could not be reached or answered with a non-200.
    Transport(String),
    /// A 200 whose body was not the shape the schema asked for.
    Malformed(String),
    /// The model was not available. Distinct from `Transport` because the caller's
    /// response is different: an absent model degrades to the digest, a broken one
    /// is worth reporting.
    Unavailable(String),
}

impl std::fmt::Display for SummarizerError {
    /// Purpose: Render a summarizer error as the message recorded in `producer`.
    /// Serves: the reason an episode degraded travelling with the summary rather than being swallowed.
    /// Design: `docs/context/status.md` §11.1.
    /// Up: `SummarizerError`'s `Display` contract.
    /// Down: Writes each variant; no callee.
    /// Verify: `cargo test -p loong-context --test semantic a_model_that_breaks_mid_pass`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SummarizerError::Transport(m) => write!(f, "transport: {m}"),
            SummarizerError::Malformed(m) => write!(f, "malformed answer: {m}"),
            SummarizerError::Unavailable(m) => write!(f, "unavailable: {m}"),
        }
    }
}

impl std::error::Error for SummarizerError {}

/// Purpose: The one thing this crate asks of a model.
/// Serves: the enrichment thread being able to own the client, so no model call sits on the foreground path.
/// Design: `docs/context/pipeline.md` §6.6.
/// Up: `Store::synthesize_window` and the kernel's enrichment thread.
/// Down: `name` and `summarize` below.
/// Verify: `cargo test -p loong-context --test semantic`.
/// The one thing this crate asks of a model.
///
/// `Send` because the enrichment worker runs on its own thread: a summariser that
/// could not be moved there would put the model call back in the foreground's
/// critical path, which is the one thing ADR-0017 §4.6 forbids.
pub trait Summarizer: Send {
    /// Purpose: Name the model for `producer` provenance, and for the "different model, different question" rule.
    /// Name, for `producer` provenance: a summary has to say *which* model wrote
    /// the parts a model wrote.
    fn name(&self) -> String;

    /// Purpose: Answer the brief with the semantic fields the model is allowed to contribute.
    fn summarize(&mut self, input: &DigestInput) -> Result<Synthesis, SummarizerError>;
}

/// Purpose: The Ollama-native client, using `/api/chat` with `think: false`.
/// Serves: the measured endpoint that honours structured output and does not spend the budget on a reasoning trace.
/// Design: `docs/context/config.md` §11.
/// Up: The kernel's `ContextEngine` and the faithfulness/window-cost measurements.
/// Down: `new`, the builders, `body`, `parse_response` and the `Summarizer` impl.
/// Verify: `cargo test -p loong-context summarizer::tests`.
/// The Ollama-native client (ADR-0017: owner's choice, and the measured one).
///
/// `/api/chat`, not `/v1/chat/completions`: the OpenAI-compatible shim ignored
/// `think:false` on this machine and spent the entire token budget on a reasoning
/// trace, returning half a sentence in `content`. The native endpoint honours it,
/// and takes a JSON schema as `format`.
#[derive(Debug, Clone)]
pub struct OllamaSummarizer {
    base: String,
    model: String,
    timeout: Duration,
    /// Tokens to allow for the answer. Bounded: a background summariser has no
    /// business writing an essay.
    max_tokens: u64,
    /// Requests actually sent, so a budget can be enforced and a cost reported.
    calls: u64,
}

impl OllamaSummarizer {
    /// Purpose: A client for one endpoint and model.
    /// Serves: the measured defaults (`think: false`, a 60 s timeout, 400 answer tokens) in one place.
    /// Design: `docs/context/config.md` §11.
    /// Up: The kernel's `ContextEngine` and the faithfulness/window-cost tests.
    /// Down: `with_timeout`, `with_max_tokens` and the `Summarizer` impl.
    /// Verify: `cargo test -p loong-context summarizer::tests::the_request_disables_thinking_and_carries_the_schema`.
    pub fn new(base: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            base: base.into().trim_end_matches('/').to_string(),
            model: model.into(),
            timeout: Duration::from_secs(60),
            max_tokens: 400,
            calls: 0,
        }
    }

    /// Purpose: Override the request timeout.
    /// Serves: a slow local model being bounded without changing the rest of the client.
    /// Design: `docs/context/config.md` §11.
    /// Up: The kernel's `ContextEngine`.
    /// Down: Mutates the client; no other callee.
    /// Verify: `cargo test -p loong-context summarizer::tests`.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Purpose: Bound the answer length.
    /// Serves: a background summariser not writing an essay.
    /// Design: `docs/context/config.md` §11.
    /// Up: The faithfulness and window-cost measurements.
    /// Down: Mutates the client; no other callee.
    /// Verify: `cargo test -p loong-context --test window_cost`.
    pub fn with_max_tokens(mut self, max_tokens: u64) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    /// Purpose: How many requests this client has sent.
    /// Serves: the measurement reporting its own cost.
    /// Design: `docs/context/status.md` §11.1.
    /// Up: The faithfulness measurement; no caller in production code.
    /// Down: Reads the counter; no callee.
    /// Verify: `cargo test -p loong-context --test faithfulness`.
    pub fn calls(&self) -> u64 {
        self.calls
    }

    /// Purpose: The chat endpoint for this client.
    /// Serves: one place that builds the URL.
    /// Design: `docs/context/config.md` §11.
    /// Up: `OllamaSummarizer::summarize`.
    /// Down: Formats the base and path; no callee.
    /// Verify: `cargo test -p loong-context summarizer::tests`.
    pub fn endpoint(&self) -> String {
        format!("{}/api/chat", self.base)
    }

    /// Purpose: The request body: model, `think: false`, the schema, the prompt.
    /// Serves: the measured requirements of the local endpoint expressed as one value.
    /// Design: `docs/context/config.md` §11.
    /// Up: `OllamaSummarizer::summarize`.
    /// Down: `DigestInput::schema` and `DigestInput::prompt`.
    /// Verify: `cargo test -p loong-context summarizer::tests::the_request_disables_thinking_and_carries_the_schema`.
    pub fn body(&self, input: &DigestInput) -> Value {
        json!({
            "model": self.model,
            "stream": false,
            // The measured requirement: without this a thinking model spends the
            // whole budget before answering.
            "think": false,
            "format": DigestInput::schema(),
            "options": { "num_predict": self.max_tokens, "temperature": 0.1 },
            "messages": [
                {
                    "role": "system",
                    // One sentence of role, because the schema does the enforcing.
                    "content": "你在归纳一个编码会话。只输出 JSON，不要复述输入。"
                },
                { "role": "user", "content": input.prompt() }
            ]
        })
    }

    /// Purpose: Parse an Ollama response into a `Synthesis`, ignoring `thinking`.
    /// Serves: a prose or off-schema answer being rejected rather than guessed at.
    /// Design: `docs/context/config.md` §11.
    /// Up: `OllamaSummarizer::summarize`.
    /// Down: `Synthesis::from_json`.
    /// Verify: `cargo test -p loong-context summarizer::tests::the_response_parser_reads_content_and_ignores_thinking`.
    /// Pull the answer out of an Ollama chat response. `content` is what the model
    /// said; `thinking` is its reasoning and is deliberately not read.
    pub fn parse_response(body: &str) -> Result<Synthesis, SummarizerError> {
        let value: Value = serde_json::from_str(body)
            .map_err(|e| SummarizerError::Malformed(format!("the body is not JSON: {e}")))?;
        let content = value
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .ok_or_else(|| {
                SummarizerError::Malformed("no message.content in the response".to_string())
            })?;
        let parsed: Value = serde_json::from_str(content.trim())
            .map_err(|e| SummarizerError::Malformed(format!("content is not JSON: {e}")))?;
        Synthesis::from_json(&parsed)
    }
}

impl Summarizer for OllamaSummarizer {
    /// Purpose: The provenance name `ollama:<model>`.
    /// Contract: `summarizer.rs` trait impl.
    fn name(&self) -> String {
        format!("ollama:{}", self.model)
    }

    /// Purpose: Send the brief and parse one answer.
    /// Serves: the single network call this crate makes, charged before it is sent.
    /// Design: `docs/context/config.md` §11.
    /// Up: `Store::synthesize_window` through `Summarizer`.
    /// Down: `body`, `endpoint`, `parse_response` and `loong_http::post_json`.
    /// Verify: `cargo test -p loong-context --test window_cost`.
    fn summarize(&mut self, input: &DigestInput) -> Result<Synthesis, SummarizerError> {
        self.calls += 1;
        let body = self.body(input).to_string();
        let response = loong_http::post_json(&self.endpoint(), &body, None, self.timeout)
            .map_err(|e| SummarizerError::Unavailable(e.0))?;
        if response.status != 200 {
            let snippet: String = response.body.chars().take(200).collect();
            return Err(SummarizerError::Transport(format!(
                "HTTP {} from {}: {snippet}",
                response.status,
                self.endpoint()
            )));
        }
        Self::parse_response(&response.body)
    }
}

/// Purpose: The deliberate absence of a model.
/// Serves: R mode working with no installation, and the absence being a fact rather than an error.
/// Design: `docs/context/status.md` §11.1.
/// Up: The kernel's `ContextEngine` fallback and the faithfulness test.
/// Down: The `Summarizer` impl.
/// Verify: `cargo test -p loong-context --test faithfulness an_absent_summarizer_is_not_an_error`.
/// A summarizer that is deliberately not there. Used when the user has not
/// installed a model: the session keeps working in R mode, and the absence is a
/// fact rather than an error.
#[derive(Debug, Default, Clone)]
pub struct NoSummarizer;

impl Summarizer for NoSummarizer {
    /// Purpose: The provenance name `none`.
    /// Contract: `summarizer.rs` trait impl.
    fn name(&self) -> String {
        "none".to_string()
    }

    /// Purpose: Answer every call with `Unavailable`.
    /// Contract: `summarizer.rs` trait impl.
    fn summarize(&mut self, _input: &DigestInput) -> Result<Synthesis, SummarizerError> {
        Err(SummarizerError::Unavailable(
            "no summarizer is configured".to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Purpose: Pin that a schema-shaped answer becomes a `Synthesis` with the allowed fields.
    #[test]
    fn a_schema_shaped_answer_becomes_a_synthesis() {
        let value = json!({
            "rejected": ["先深拷贝再合并：测试仍失败", ""],
            "why": "层叠顺序不能依赖调用序",
            "open_questions": [],
            "conclusion": "改为按层号排序后合并"
        });
        let synthesis = Synthesis::from_json(&value).unwrap();
        assert_eq!(synthesis.rejected, vec!["先深拷贝再合并：测试仍失败"]);
        assert_eq!(synthesis.why, vec!["层叠顺序不能依赖调用序"]);
        assert_eq!(
            synthesis.conclusion.as_deref(),
            Some("改为按层号排序后合并")
        );
    }

    /// Purpose: Pin that "无法判断" is dropped instead of becoming an invented reason.
    #[test]
    fn a_model_that_admits_it_cannot_tell_says_nothing() {
        // "无法判断" is an honest answer, and turning it into a `why` would be
        // inventing a reason on the model's behalf.
        let value = json!({"rejected": [], "why": "无法判断", "conclusion": "改完了"});
        let synthesis = Synthesis::from_json(&value).unwrap();
        assert!(synthesis.why.is_empty());
        assert_eq!(synthesis.conclusion.as_deref(), Some("改完了"));
    }

    /// Purpose: Pin that an answer carrying none of the asked fields is refused.
    #[test]
    fn an_answer_with_none_of_the_fields_is_refused() {
        let value = json!({"summary": "我总结了一下"});
        assert!(matches!(
            Synthesis::from_json(&value),
            Err(SummarizerError::Malformed(_))
        ));
    }

    /// Purpose: Pin that the request carries `think: false`, the schema and the labelled facts.
    #[test]
    fn the_request_disables_thinking_and_carries_the_schema() {
        let input = DigestInput {
            request: "配置层叠失效".into(),
            status: "done".into(),
            touched_files: vec!["src/config/merge.rs".into()],
            commands: vec!["cargo".into()],
            tests: vec![TestRecord {
                name: "layer_merge".into(),
                failures: 1,
                passes: 1,
            }],
            edits: vec![crate::digest::Edit {
                seq: 6,
                file: Some("src/config/merge.rs".into()),
                attempt: Some("深拷贝后再合并".into()),
                outcome: Some(false),
                outcome_seq: Some(7),
            }],
            rendered: String::new(),
        };
        let summarizer = OllamaSummarizer::new("http://127.0.0.1:11434", "qwen3:0.6b");
        let body = summarizer.body(&input);
        // Measured on this machine: without `think:false` the OpenAI shim spends
        // the budget on a reasoning trace; the native endpoint honours it.
        assert_eq!(body["think"], json!(false));
        assert_eq!(body["stream"], json!(false));
        // `rejected` is deliberately absent from the schema: it is computed from the
        // attempt sequence, so the model is not asked for something it can only get
        // wrong.
        assert!(body["format"]["properties"]["rejected"].is_null());
        assert!(body["format"]["properties"]["why"].is_object());
        assert!(body["format"]["properties"]["open_questions"].is_object());
        assert_eq!(body["model"], json!("qwen3:0.6b"));
        let prompt = body["messages"][1]["content"].as_str().unwrap();
        // Handed facts, labelled — not a transcript.
        assert!(prompt.contains("这些是事实"), "{prompt}");
        assert!(prompt.contains("测试 layer_merge"), "{prompt}");
        assert!(prompt.contains("failed→ok"), "{prompt}");
        // The attempt and its outcome reach the model: measured, this is what lets it
        // explain *why* rather than guess.
        assert!(prompt.contains("操作顺序"), "{prompt}");
        assert!(prompt.contains("深拷贝后再合并"), "{prompt}");
    }

    /// Purpose: Pin that `content` is parsed and `thinking` is ignored.
    #[test]
    fn the_response_parser_reads_content_and_ignores_thinking() {
        let body = json!({
            "message": {
                "role": "assistant",
                "content": "{\"rejected\":[\"方案A\"],\"why\":\"根因是深拷贝\",\"conclusion\":\"方案B\"}",
                "thinking": "让我想想……用户说的是层叠失效，可能……"
            },
            "done": true
        })
        .to_string();
        let synthesis = OllamaSummarizer::parse_response(&body).unwrap();
        assert_eq!(synthesis.rejected, vec!["方案A"]);
        assert_eq!(synthesis.why, vec!["根因是深拷贝"]);
    }

    /// Purpose: Pin that a prose answer is malformed rather than guessed at.
    #[test]
    fn a_prose_answer_is_malformed_rather_than_guessed_at() {
        let body = json!({"message": {"content": "我总结了以下三点：第一……"}}).to_string();
        assert!(matches!(
            OllamaSummarizer::parse_response(&body),
            Err(SummarizerError::Malformed(_))
        ));
    }
}
