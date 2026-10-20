//! Purpose: Model providers behind one `Model` interface, so the kernel never learns which vendor answered (ADR-0004).
//!
//! # Contract
//! - **Serves:** the kernel's turn, which holds a `Box<dyn Model>`, and every surface that previews an answer.
//! - **Design:** `docs/design/config-routing.md` §8.
//! - **Upstream:** `crates/cli/src/server.rs` builds an implementation; `crates/kernel/src/turn.rs` holds the `Box<dyn Model>`.
//! - **Downstream:** the real adapter `openai_compat`, the probe, and `loong_schema`'s JSON type.
//! - **Verify:** `cargo test -p loong-providers` · `./scripts/check.sh`
//!
//! Model providers behind one `Model` interface (ADR-0004).
//!
//! Adapters for the handful of wire protocols live here as *edges*: the kernel
//! only ever sees this trait, so swapping a provider never touches the loop.
//! `ScriptedModel` is the test double the kernel's loop tests use — no network,
//! no API keys, fully deterministic.

use loong_schema::generated::Json;
use serde::{Deserialize, Serialize};

pub mod openai_compat;
pub mod probe;

pub use openai_compat::OpenAiCompat;

/// A delta for a caller that wants to watch an answer arrive. Kept as a plain
/// `&str` callback in the trait: the provider assembles tool calls itself, so only
/// text is interesting to a surface.
/// Purpose: The callback a streaming caller gives to watch text arrive.
/// Serves: `Model::complete_streaming` and the surfaces that show a preview.
/// Design: `docs/design/config-routing.md` §8.
/// Up: no in-tree caller; `complete_streaming` spells the callback out instead of naming this alias.
/// Down: the `on_delta` argument it names.
/// Verify: `cargo test -p loong-providers`.
pub type DeltaSink<'a> = &'a mut dyn FnMut(&str);

/// A tool as the model sees it (name + description + JSON Schema parameters).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
/// Purpose: A tool as the model sees it: name, description and JSON Schema.
/// Serves: `CompletionRequest`; the kernel's `tool_specs` and `OpenAiCompat::wire_tools`.
/// Design: `docs/design/config-routing.md` §8.
/// Up: `Kernel::tool_specs` builds it from the registry.
/// Down: `OpenAiCompat::wire_tools` serializes it onto the wire.
/// Verify: `cargo test -p loong-providers`.
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: Json,
}

/// One message in the model's conversation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
/// Purpose: One conversation message, tagged by role on the wire.
/// Serves: `CompletionRequest` and both provider adapters.
/// Design: `docs/design/config-routing.md` §8.
/// Up: the kernel's turn builds them from its ledger entries.
/// Down: `OpenAiCompat::wire_messages` maps each variant; `ScriptedModel` ignores messages.
/// Verify: `cargo test -p loong-providers`.
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
/// Purpose: A tool call the model asked the kernel to make.
/// Serves: `CompletionResponse`, `ChatMessage::Assistant` and the kernel's dispatch.
/// Design: `docs/design/config-routing.md` §8.
/// Up: `OpenAiCompat` parses it from a reply; tests construct one for `ScriptedModel`.
/// Down: `Kernel::execute` resolves its `tool` and `args`.
/// Verify: `cargo test -p loong-providers`.
pub struct ToolCallRequest {
    pub call_id: String,
    pub tool: String,
    pub args: Json,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
/// Purpose: Token counts a provider reported for one exchange.
/// Serves: `CompletionResponse` and the ledger's accounting.
/// Design: `docs/design/config-routing.md` §8.
/// Up: `OpenAiCompat` parses it from the reply's `usage`; tests construct it.
/// Down: the kernel's token totals.
/// Verify: `cargo test -p loong-providers`.
pub struct Usage {
    pub tokens_in: u64,
    pub tokens_out: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
/// Purpose: One request to a model: the messages plus the tools it may call.
/// Serves: `Model::complete` and `Model::complete_streaming`.
/// Design: `docs/design/config-routing.md` §8.
/// Up: the kernel's turn assembles it at every step.
/// Down: `OpenAiCompat::body` serializes it; `ScriptedModel` ignores it.
/// Verify: `cargo test -p loong-providers`.
pub struct CompletionRequest {
    pub messages: Vec<ChatMessage>,
    pub tools: Vec<ToolSpec>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
/// Purpose: What a model answered: text, tool calls and usage.
/// Serves: the kernel's loop and the streaming preview.
/// Design: `docs/design/config-routing.md` §8.
/// Up: both adapters and `ScriptedModel` return it.
/// Down: the kernel appends it to the ledger and dispatches its calls.
/// Verify: `cargo test -p loong-providers`.
pub struct CompletionResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default)]
    pub tool_calls: Vec<ToolCallRequest>,
    #[serde(default)]
    pub usage: Usage,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Purpose: Why a model call failed: the transport or the protocol.
/// Serves: every `Model` implementation and the kernel's turn error path.
/// Design: `docs/design/config-routing.md` §8.
/// Up: `OpenAiCompat` constructs it from a transport or protocol failure.
/// Down: `Model::complete` returns it, ending the turn.
/// Verify: `cargo test -p loong-providers`.
pub enum ModelError {
    /// The transport failed (network, process, socket).
    Transport(String),
    /// The provider answered something we cannot use.
    Protocol(String),
}

impl std::fmt::Display for ModelError {
    /// Purpose: Render the error with the transport or protocol prefix the log carries.
    /// Contract: `crates/providers/src/lib.rs`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ModelError::Transport(m) => write!(f, "transport: {m}"),
            ModelError::Protocol(m) => write!(f, "protocol: {m}"),
        }
    }
}

impl std::error::Error for ModelError {}

/// The one interface the kernel knows (ADR-0004: provider adapters are edges).
/// Purpose: The one model interface the kernel knows.
/// Serves: the kernel's turn and every surface; adapters are edges behind it.
/// Design: `docs/design/config-routing.md` §8.
/// Up: `Kernel` holds a `Box<dyn Model>`.
/// Down: `ScriptedModel` and `OpenAiCompat` implement it.
/// Verify: `cargo test -p loong-providers`.
pub trait Model: Send {
    /// Purpose: Ask the model for one complete answer.
    fn complete(&mut self, req: &CompletionRequest) -> Result<CompletionResponse, ModelError>;

    /// The same call, reporting text as it is written.
    ///
    /// Streaming is a **preview, never the record**: the deltas are neither logged
    /// nor consumed as the answer — [`CompletionResponse`] is, exactly as before. The
    /// default delivers the whole text in one delta, so a model that cannot stream
    /// still works behind every surface.
    /// Purpose: The same call, reporting text as it is written.
    /// Serves: the kernel's step loop, which forwards deltas as transient notifications; the returned response is still the record.
    /// Design: `docs/design/config-routing.md` §8.
    /// Up: `crates/kernel/src/turn/loop.rs`; surfaces see deltas through the protocol, not by calling this.
    /// Down: `Model::complete` by default; `OpenAiCompat` overrides it.
    /// Verify: `cargo test -p loong-providers`.
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

    /// How many tokens this model can hold in one request — prompt **and** answer
    /// together.
    ///
    /// `None` means the adapter does not know, which is a better answer than a guess: told
    /// too much, the provider rejects the request; told too little, the kernel throws away
    /// context the model could have used. The kernel compacts against this, and against
    /// what a person configured, whichever is stricter.
    /// Purpose: How many tokens the model can hold in one request, prompt and answer together.
    /// Serves: the kernel's compaction threshold.
    /// Design: `docs/design/config-routing.md` §8.
    /// Up: the kernel's compaction check.
    /// Down: `None` by default; both models report their configured value.
    /// Verify: `cargo test -p loong-providers`.
    fn context_window(&self) -> Option<u64> {
        None
    }
}

/// A scripted model for tests and for the first vertical slice: it returns the
/// next canned response, and a plain "done" once the script is exhausted.
#[derive(Debug, Default)]
/// Purpose: The deterministic double that plays canned responses.
/// Serves: the kernel's loop tests and the CLI's demo providers.
/// Design: `docs/design/config-routing.md` §8.
/// Up: `Kernel` tests and `crates/cli/src/server.rs`.
/// Down: `Model::complete` and `Model::context_window`.
/// Verify: `cargo test -p loong-providers`.
pub struct ScriptedModel {
    responses: Vec<CompletionResponse>,
    cursor: usize,
    /// A window for tests that exercise compaction; `None` means "does not know".
    context_window: Option<u64>,
}

impl ScriptedModel {
    /// Purpose: Build the double from the responses it will play in order.
    /// Serves: test fixtures and the CLI's demo provider.
    /// Design: `docs/design/config-routing.md` §8.
    /// Up: `ScriptedModel::saying` and the crate's tests.
    /// Down: the `responses` and `cursor` fields.
    /// Verify: `cargo test -p loong-providers`.
    pub fn new(responses: Vec<CompletionResponse>) -> Self {
        Self {
            responses,
            cursor: 0,
            context_window: None,
        }
    }

    /// Declare a context window, so a test can watch the kernel compact against it.
    /// Purpose: Declare a window so a test can watch compaction fire.
    /// Serves: the crate's own window test; no other caller in the tree.
    /// Design: `docs/design/config-routing.md` §8.
    /// Up: `a_model_can_declare_how_much_it_holds` in this file.
    /// Down: `Model::context_window`.
    /// Verify: `cargo test -p loong-providers`.
    pub fn with_context_window(mut self, tokens: u64) -> Self {
        self.context_window = Some(tokens);
        self
    }

    /// A model that immediately answers with text and no tool calls.
    /// Purpose: A double that answers with text and no tool calls.
    /// Serves: the provider, kernel-turn and server tests that need one plain answer.
    /// Design: `docs/design/config-routing.md` §8.
    /// Up: `crates/providers`, `crates/kernel/src/turn/tests.rs` and `crates/server/src/lib.rs` tests.
    /// Down: `ScriptedModel::new`.
    /// Verify: `cargo test -p loong-providers`.
    pub fn saying(text: &str) -> Self {
        Self::new(vec![CompletionResponse {
            text: Some(text.to_string()),
            tool_calls: Vec::new(),
            usage: Usage {
                tokens_in: 1,
                tokens_out: 1,
            },
        }])
    }

    /// Number of completions requested so far — lets a test assert that the loop
    /// stopped instead of spinning.
    /// Purpose: How many completions were requested so far.
    /// Serves: the crate's own script test; no other caller in the tree.
    /// Design: `docs/design/config-routing.md` §8.
    /// Up: `scripted_model_plays_its_script_then_stops` in this file.
    /// Down: the `cursor` field.
    /// Verify: `cargo test -p loong-providers`.
    pub fn calls(&self) -> usize {
        self.cursor
    }
}

impl Model for ScriptedModel {
    /// Purpose: Report the window the test double was given.
    /// Contract: `crates/providers/src/lib.rs`.
    fn context_window(&self) -> Option<u64> {
        self.context_window
    }

    /// Purpose: Play the next canned response, then a plain "done".
    /// Serves: `Model::complete` on the test double.
    /// Design: `docs/design/config-routing.md` §8.
    /// Up: kernel loop tests and the CLI demo.
    /// Down: the `responses` vector and `cursor`.
    /// Verify: `cargo test -p loong-providers`.
    fn complete(&mut self, _req: &CompletionRequest) -> Result<CompletionResponse, ModelError> {
        let response = self
            .responses
            .get(self.cursor)
            .cloned()
            .unwrap_or(CompletionResponse {
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
    /// Purpose: Pin that an undeclared context window is `None` and a declared one is reported.
    fn a_model_can_declare_how_much_it_holds() {
        // `None` is the honest default: an adapter that does not know must not invent a
        // window, because the kernel compacts against whatever it is told.
        assert_eq!(ScriptedModel::saying("hi").context_window(), None);
        assert_eq!(
            ScriptedModel::saying("hi")
                .with_context_window(131_072)
                .context_window(),
            Some(131_072)
        );
    }

    #[test]
    /// Purpose: Pin that the script plays in order and then answers with a plain "done".
    fn scripted_model_plays_its_script_then_stops() {
        let mut m = ScriptedModel::new(vec![CompletionResponse {
            text: None,
            tool_calls: vec![ToolCallRequest {
                call_id: "c1".into(),
                tool: "read_file".into(),
                args: json!({"path": "a.txt"}),
            }],
            usage: Usage {
                tokens_in: 10,
                tokens_out: 2,
            },
        }]);
        let req = CompletionRequest {
            messages: vec![],
            tools: vec![],
        };
        let first = m.complete(&req).unwrap();
        assert_eq!(first.tool_calls.len(), 1);
        assert_eq!(first.tool_calls[0].tool, "read_file");
        assert_eq!(first.usage.tokens_in, 10);
        let second = m.complete(&req).unwrap();
        assert!(second.tool_calls.is_empty(), "the script is exhausted");
        assert_eq!(m.calls(), 2);
    }

    #[test]
    /// Purpose: Pin that `ChatMessage` serializes with its role tag.
    fn chat_messages_are_tagged_on_the_wire() {
        let wire = serde_json::to_value(ChatMessage::Tool {
            call_id: "c1".into(),
            output: "ok".into(),
        })
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
