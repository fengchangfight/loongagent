//! The optional small model: the semantic fields a deterministic pass cannot fill.
//!
//! Everything else in this crate computes. This module is the one place that asks
//! a model for an opinion, and the shape of the asking is the design:
//!
//! * **The model is handed facts, not events.** [`DigestInput`] is the rendered
//!   `D`, with confirmed facts on labelled lines. Measured on this machine, a 0.6B
//!   model handed a raw event list copies the list into `rejected[]`; handed
//!   labelled facts and asked one question, it answers correctly. Input quality
//!   decides the outcome far more than model size does.
//! * **It answers only what cannot be computed.** `rejected`, `open_questions`,
//!   `why`, `conclusion`. Status, files, commands, test results and citations are
//!   already decided — the design note records a 3B model calling a committed,
//!   passing task `open`, so those fields are never delegated again.
//! * **Additive, never authoritative.** A synthesis is merged *into* the digest; it
//!   cannot overwrite a fact or remove a citation. Worst case it adds a claim, which
//!   the citation structure lets a reader check.
//! * **Structured output is enforced by the runtime**, not requested in prose:
//!   the Ollama request carries a JSON schema, so "please answer in JSON" is not a
//!   hope.
//! * **Thinking is off.** Measured: `/v1/chat/completions` ignores `think:false` and
//!   spends the whole budget on a reasoning trace, leaving half a sentence. The
//!   native `/api/chat` honours it.
//! * **Its cost is counted.** Every call is charged against a budget, because the
//!   design note requires the background spend to be queryable.

use crate::digest::{Digest, TestRecord};
use serde_json::{json, Value};
use std::time::Duration;

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
    pub fn is_empty(&self) -> bool {
        self.rejected.is_empty()
            && self.open_questions.is_empty()
            && self.why.is_empty()
            && self.conclusion.is_none()
    }

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
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SummarizerError::Transport(m) => write!(f, "transport: {m}"),
            SummarizerError::Malformed(m) => write!(f, "malformed answer: {m}"),
            SummarizerError::Unavailable(m) => write!(f, "unavailable: {m}"),
        }
    }
}

impl std::error::Error for SummarizerError {}

/// The one thing this crate asks of a model.
///
/// `Send` because the enrichment worker runs on its own thread: a summariser that
/// could not be moved there would put the model call back in the foreground's
/// critical path, which is the one thing ADR-0017 §4.6 forbids.
pub trait Summarizer: Send {
    /// Name, for `producer` provenance: a summary has to say *which* model wrote
    /// the parts a model wrote.
    fn name(&self) -> String;

    fn summarize(&mut self, input: &DigestInput) -> Result<Synthesis, SummarizerError>;
}

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
    pub fn new(base: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            base: base.into().trim_end_matches('/').to_string(),
            model: model.into(),
            timeout: Duration::from_secs(60),
            max_tokens: 400,
            calls: 0,
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn with_max_tokens(mut self, max_tokens: u64) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    pub fn calls(&self) -> u64 {
        self.calls
    }

    pub fn endpoint(&self) -> String {
        format!("{}/api/chat", self.base)
    }

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
    fn name(&self) -> String {
        format!("ollama:{}", self.model)
    }

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

/// A summarizer that is deliberately not there. Used when the user has not
/// installed a model: the session keeps working in R mode, and the absence is a
/// fact rather than an error.
#[derive(Debug, Default, Clone)]
pub struct NoSummarizer;

impl Summarizer for NoSummarizer {
    fn name(&self) -> String {
        "none".to_string()
    }

    fn summarize(&mut self, _input: &DigestInput) -> Result<Synthesis, SummarizerError> {
        Err(SummarizerError::Unavailable(
            "no summarizer is configured".to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn a_model_that_admits_it_cannot_tell_says_nothing() {
        // "无法判断" is an honest answer, and turning it into a `why` would be
        // inventing a reason on the model's behalf.
        let value = json!({"rejected": [], "why": "无法判断", "conclusion": "改完了"});
        let synthesis = Synthesis::from_json(&value).unwrap();
        assert!(synthesis.why.is_empty());
        assert_eq!(synthesis.conclusion.as_deref(), Some("改完了"));
    }

    #[test]
    fn an_answer_with_none_of_the_fields_is_refused() {
        let value = json!({"summary": "我总结了一下"});
        assert!(matches!(
            Synthesis::from_json(&value),
            Err(SummarizerError::Malformed(_))
        ));
    }

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

    #[test]
    fn a_prose_answer_is_malformed_rather_than_guessed_at() {
        let body = json!({"message": {"content": "我总结了以下三点：第一……"}}).to_string();
        assert!(matches!(
            OllamaSummarizer::parse_response(&body),
            Err(SummarizerError::Malformed(_))
        ));
    }
}
