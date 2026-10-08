//! Model providers behind one `Model` interface (ADR-0004).
//!
//! Adapters for the handful of wire protocols live here as *edges*: the kernel
//! only ever sees this trait, so swapping a provider never touches the loop.
//! `ScriptedModel` is the test double the kernel's loop tests use — no network,
//! no API keys, fully deterministic.

use loong_schema::generated::Json;
use serde::{Deserialize, Serialize};

pub mod openai_compat;

pub use openai_compat::OpenAiCompat;

/// A delta for a caller that wants to watch an answer arrive. Kept as a plain
/// `&str` callback in the trait: the provider assembles tool calls itself, so only
/// text is interesting to a surface.
pub type DeltaSink<'a> = &'a mut dyn FnMut(&str);

/// A tool as the model sees it (name + description + JSON Schema parameters).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: Json,
}

/// One message in the model's conversation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum ChatMessage {
    System {
        text: String,
    },
    User {
        text: String,
    },
    Assistant {
        #[serde(skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        #[serde(default)]
        tool_calls: Vec<ToolCallRequest>,
    },
    Tool {
        call_id: String,
        output: String,
    },
}

/// A tool call the model wants the kernel to make.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCallRequest {
    pub call_id: String,
    pub tool: String,
    pub args: Json,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub tokens_in: u64,
    pub tokens_out: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompletionRequest {
    pub messages: Vec<ChatMessage>,
    pub tools: Vec<ToolSpec>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompletionResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default)]
    pub tool_calls: Vec<ToolCallRequest>,
    #[serde(default)]
    pub usage: Usage,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelError {
    /// The transport failed (network, process, socket).
    Transport(String),
    /// The provider answered something we cannot use.
    Protocol(String),
}

impl std::fmt::Display for ModelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ModelError::Transport(m) => write!(f, "transport: {m}"),
            ModelError::Protocol(m) => write!(f, "protocol: {m}"),
        }
    }
}

impl std::error::Error for ModelError {}

/// The one interface the kernel knows (ADR-0004: provider adapters are edges).
pub trait Model: Send {
    fn complete(&mut self, req: &CompletionRequest) -> Result<CompletionResponse, ModelError>;

    /// The same call, reporting text as it is written.
    ///
    /// Streaming is a **preview, never the record**: the deltas are neither logged
    /// nor consumed as the answer — [`CompletionResponse`] is, exactly as before. The
    /// default delivers the whole text in one delta, so a model that cannot stream
    /// still works behind every surface.
    fn complete_streaming(
        &mut self,
        req: &CompletionRequest,
        on_delta: &mut dyn FnMut(&str),
    ) -> Result<CompletionResponse, ModelError> {
        let response = self.complete(req)?;
        if let Some(text) = &response.text {
            if !text.is_empty() {
                on_delta(text);
            }
        }
        Ok(response)
    }
}

/// A scripted model for tests and for the first vertical slice: it returns the
/// next canned response, and a plain "done" once the script is exhausted.
#[derive(Debug, Default)]
pub struct ScriptedModel {
    responses: Vec<CompletionResponse>,
    cursor: usize,
}

impl ScriptedModel {
    pub fn new(responses: Vec<CompletionResponse>) -> Self {
        Self { responses, cursor: 0 }
    }

    /// A model that immediately answers with text and no tool calls.
    pub fn saying(text: &str) -> Self {
        Self::new(vec![CompletionResponse {
            text: Some(text.to_string()),
            tool_calls: Vec::new(),
            usage: Usage { tokens_in: 1, tokens_out: 1 },
        }])
    }

    /// Number of completions requested so far — lets a test assert that the loop
    /// stopped instead of spinning.
    pub fn calls(&self) -> usize {
        self.cursor
    }
}

impl Model for ScriptedModel {
    fn complete(&mut self, _req: &CompletionRequest) -> Result<CompletionResponse, ModelError> {
        let response = self.responses.get(self.cursor).cloned().unwrap_or(CompletionResponse {
            text: Some("done".to_string()),
            tool_calls: Vec::new(),
            usage: Usage::default(),
        });
        self.cursor += 1;
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn scripted_model_plays_its_script_then_stops() {
        let mut m = ScriptedModel::new(vec![CompletionResponse {
            text: None,
            tool_calls: vec![ToolCallRequest {
                call_id: "c1".into(),
                tool: "read_file".into(),
                args: json!({"path": "a.txt"}),
            }],
            usage: Usage { tokens_in: 10, tokens_out: 2 },
        }]);
        let req = CompletionRequest { messages: vec![], tools: vec![] };
        let first = m.complete(&req).unwrap();
        assert_eq!(first.tool_calls.len(), 1);
        assert_eq!(first.tool_calls[0].tool, "read_file");
        assert_eq!(first.usage.tokens_in, 10);
        let second = m.complete(&req).unwrap();
        assert!(second.tool_calls.is_empty(), "the script is exhausted");
        assert_eq!(m.calls(), 2);
    }

    #[test]
    fn chat_messages_are_tagged_on_the_wire() {
        let wire =
            serde_json::to_value(ChatMessage::Tool { call_id: "c1".into(), output: "ok".into() })
                .unwrap();
        assert_eq!(wire["role"], "tool");
        let assistant = serde_json::to_value(ChatMessage::Assistant {
            text: Some("hi".into()),
            tool_calls: vec![],
        })
        .unwrap();
        assert_eq!(assistant["role"], "assistant");
    }
}
