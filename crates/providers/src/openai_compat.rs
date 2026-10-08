//! An OpenAI-compatible provider adapter (ADR-0004: adapter as an edge).
//!
//! This is the first *real* adapter: it speaks the Chat Completions wire
//! protocol that vLLM, LM Studio, llama.cpp's server, Ollama's `/v1`, and the
//! hosted vendors all implement, and it translates to and from the `Model`
//! interface the kernel knows. Nothing about the kernel changes when a provider
//! does (that is the whole point of the edge).
//!
//! **Transport scope, stated honestly:** HTTP only, so it targets a local or
//! intranet endpoint. An `https://` base URL fails with a clear message rather
//! than pretending — wiring a TLS client is a deliberate follow-up (it is a
//! dependency decision, not an architecture one), and self-hosted endpoints are
//! exactly the firm-internal case.

use crate::{ChatMessage, CompletionRequest, CompletionResponse, Model, ModelError, ToolCallRequest, Usage};
use loong_schema::generated::Json;
use loong_http::{self as http, HttpError};
use serde_json::{json, Value};
use std::time::Duration;

const IO_TIMEOUT: Duration = Duration::from_secs(120);

pub struct OpenAiCompat {
    base: String,
    model: String,
    api_key: Option<String>,
}

impl OpenAiCompat {
    /// `base` is the API root, e.g. `http://127.0.0.1:8000/v1`.
    pub fn new(base: impl Into<String>, model: impl Into<String>, api_key: Option<String>) -> Self {
        Self { base: base.into().trim_end_matches('/').to_string(), model: model.into(), api_key }
    }

    /// Convenience for the common local setup: `LOONGAGENT_BASE_URL`,
    /// `LOONGAGENT_MODEL`, optional `LOONGAGENT_API_KEY`.
    pub fn from_env() -> Result<Self, ModelError> {
        let base = std::env::var("LOONGAGENT_BASE_URL").map_err(|_| {
            ModelError::Transport(
                "openai-compat needs LOONGAGENT_BASE_URL (e.g. http://127.0.0.1:8000/v1)".into(),
            )
        })?;
        let model = std::env::var("LOONGAGENT_MODEL").unwrap_or_else(|_| "local".to_string());
        Ok(Self::new(base, model, std::env::var("LOONGAGENT_API_KEY").ok()))
    }

    fn endpoint(&self) -> String {
        // Scheme, host, port and TLS policy belong to the shared client; this
        // adapter only knows the path its protocol lives at.
        format!("{}/chat/completions", self.base)
    }

    /// Our messages → the wire's messages.
    fn wire_messages(messages: &[ChatMessage]) -> Vec<Value> {
        messages
            .iter()
            .map(|m| match m {
                ChatMessage::System { text } => json!({"role": "system", "content": text}),
                ChatMessage::User { text } => json!({"role": "user", "content": text}),
                ChatMessage::Assistant { text, tool_calls } => {
                    let mut value = json!({"role": "assistant", "content": text});
                    if !tool_calls.is_empty() {
                        value["tool_calls"] = Value::Array(
                            tool_calls
                                .iter()
                                .map(|c| {
                                    json!({
                                        "id": c.call_id,
                                        "type": "function",
                                        "function": {
                                            "name": c.tool,
                                            // The wire carries arguments as a JSON *string*.
                                            "arguments": c.args.to_string(),
                                        }
                                    })
                                })
                                .collect(),
                        );
                    }
                    value
                }
                ChatMessage::Tool { call_id, output } => {
                    json!({"role": "tool", "tool_call_id": call_id, "content": output})
                }
            })
            .collect()
    }

    /// Our tools → the wire's tools.
    fn wire_tools(request: &CompletionRequest) -> Value {
        Value::Array(
            request
                .tools
                .iter()
                .map(|t| {
                    json!({
                        "type": "function",
                        "function": {
                            "name": t.name,
                            "description": t.description,
                            "parameters": t.parameters,
                        }
                    })
                })
                .collect(),
        )
    }

    fn body(&self, request: &CompletionRequest) -> String {
        let mut body = json!({
            "model": self.model,
            "messages": Self::wire_messages(&request.messages),
            "stream": false,
        });
        // Some servers reject an empty tools array, and none need it.
        if !request.tools.is_empty() {
            body["tools"] = Self::wire_tools(request);
            body["tool_choice"] = json!("auto");
        }
        body.to_string()
    }

    /// The wire's answer → our response.
    fn parse_response(body: &str) -> Result<CompletionResponse, ModelError> {
        let value: Value = serde_json::from_str(body)
            .map_err(|e| ModelError::Protocol(format!("response is not JSON: {e}")))?;
        let message = value
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .ok_or_else(|| ModelError::Protocol(format!("no choices[0].message in {body}")))?;

        let text = message.get("content").and_then(|c| c.as_str()).map(|s| s.to_string());

        let mut tool_calls = Vec::new();
        if let Some(calls) = message.get("tool_calls").and_then(|c| c.as_array()) {
            for call in calls {
                let call_id = call
                    .get("id")
                    .and_then(|i| i.as_str())
                    .ok_or_else(|| ModelError::Protocol("tool_call without id".into()))?
                    .to_string();
                let function = call
                    .get("function")
                    .ok_or_else(|| ModelError::Protocol("tool_call without function".into()))?;
                let tool = function
                    .get("name")
                    .and_then(|n| n.as_str())
                    .ok_or_else(|| ModelError::Protocol("tool_call without a name".into()))?
                    .to_string();
                let raw = function.get("arguments").and_then(|a| a.as_str()).unwrap_or("{}");
                let args: Json = serde_json::from_str(raw).map_err(|e| {
                    ModelError::Protocol(format!(
                        "tool {tool:?} sent arguments that are not JSON ({e}): {raw}"
                    ))
                })?;
                tool_calls.push(ToolCallRequest { call_id, tool, args });
            }
        }

        let usage = Usage {
            tokens_in: value
                .get("usage")
                .and_then(|u| u.get("prompt_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            tokens_out: value
                .get("usage")
                .and_then(|u| u.get("completion_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
        };

        Ok(CompletionResponse { text, tool_calls, usage })
    }
}

impl Model for OpenAiCompat {
    fn complete(&mut self, request: &CompletionRequest) -> Result<CompletionResponse, ModelError> {
        let endpoint = self.endpoint();
        let body = self.body(request);
        let response = http::post_json(&endpoint, &body, self.api_key.as_deref(), IO_TIMEOUT)
            .map_err(|HttpError(message)| ModelError::Transport(message))?;
        if response.status != 200 {
            let snippet: String = response.body.chars().take(400).collect();
            return Err(ModelError::Transport(format!(
                "HTTP {} from {endpoint}: {snippet}",
                response.status
            )));
        }
        Self::parse_response(&response.body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ToolSpec;
    use loong_http::testing::fake_server;

    fn split(request: &str) -> (String, Value) {
        let (head, body) = request.split_once("\r\n\r\n").expect("headers and body");
        (head.to_string(), serde_json::from_str(body).expect("the body is JSON"))
    }

    #[test]
    fn a_tool_call_round_trips_through_the_wire() {
        let (base, server) = fake_server(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n\
             {\"choices\":[{\"message\":{\"role\":\"assistant\",\"content\":null,\
             \"tool_calls\":[{\"id\":\"call_1\",\"type\":\"function\",\
             \"function\":{\"name\":\"read_file\",\"arguments\":\"{\\\"path\\\":\\\"a.txt\\\"}\"}}]},\
             \"finish_reason\":\"tool_calls\"}],\"usage\":{\"prompt_tokens\":31,\"completion_tokens\":7}}",
        );
        let mut model = OpenAiCompat::new(&base, "local-model", Some("secret".into()));
        let request = CompletionRequest {
            messages: vec![
                ChatMessage::System { text: "be brief".into() },
                ChatMessage::User { text: "read a.txt".into() },
            ],
            tools: vec![ToolSpec {
                name: "read_file".into(),
                description: "Read a file".into(),
                parameters: json!({"type": "object"}),
            }],
        };
        let response = model.complete(&request).unwrap();

        assert_eq!(response.text, None);
        assert_eq!(response.tool_calls.len(), 1);
        assert_eq!(response.tool_calls[0].call_id, "call_1");
        assert_eq!(response.tool_calls[0].tool, "read_file");
        assert_eq!(response.tool_calls[0].args["path"], "a.txt");
        assert_eq!(response.usage, Usage { tokens_in: 31, tokens_out: 7 });

        let sent = server.join().unwrap();
        let (head, body) = split(&sent);
        assert!(head.starts_with("POST /v1/chat/completions HTTP/1.1"), "{head}");
        assert!(head.contains("Authorization: Bearer secret"), "{head}");
        assert_eq!(body["model"], "local-model");
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][1]["content"], "read a.txt");
        assert_eq!(body["tools"][0]["function"]["name"], "read_file");
        assert_eq!(body["tool_choice"], "auto");
        assert_eq!(body["stream"], false);
    }

    #[test]
    fn an_earlier_tool_result_is_mapped_back_onto_the_wire() {
        let (base, server) = fake_server(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n\
             {\"choices\":[{\"message\":{\"role\":\"assistant\",\"content\":\"done\"}}],\"usage\":{}}",
        );
        let mut model = OpenAiCompat::new(&base, "m", None);
        let request = CompletionRequest {
            messages: vec![
                ChatMessage::Assistant {
                    text: None,
                    tool_calls: vec![ToolCallRequest {
                        call_id: "call_1".into(),
                        tool: "read_file".into(),
                        args: json!({"path": "a.txt"}),
                    }],
                },
                ChatMessage::Tool { call_id: "call_1".into(), output: "file body".into() },
            ],
            tools: vec![],
        };
        let response = model.complete(&request).unwrap();
        assert_eq!(response.text.as_deref(), Some("done"));

        let (_, body) = split(&server.join().unwrap());
        assert_eq!(body["messages"][0]["tool_calls"][0]["function"]["name"], "read_file");
        assert_eq!(
            body["messages"][0]["tool_calls"][0]["function"]["arguments"],
            "{\"path\":\"a.txt\"}",
            "arguments are a JSON string on the wire"
        );
        assert_eq!(body["messages"][1]["role"], "tool");
        assert_eq!(body["messages"][1]["tool_call_id"], "call_1");
    }

    #[test]
    fn a_non_200_is_a_transport_error_with_the_status_and_a_snippet() {
        let (base, server) = fake_server(
            "HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\n\r\n\
             {\"error\":{\"message\":\"no key\"}}",
        );
        let mut model = OpenAiCompat::new(&base, "m", None);
        let err = model.complete(&CompletionRequest { messages: vec![], tools: vec![] }).unwrap_err();
        let message = err.to_string();
        assert!(message.contains("401"), "{message}");
        assert!(message.contains("no key"), "{message}");
        server.join().unwrap();
    }

    #[test]
    fn an_https_endpoint_is_accepted_and_a_bad_host_is_a_transport_error() {
        // The scheme is no longer a reason to refuse: TLS is wired. A host that does
        // not resolve proves the adapter got as far as the network.
        let mut model = OpenAiCompat::new("https://api.example.invalid/v1", "m", None);
        let err = model.complete(&CompletionRequest { messages: vec![], tools: vec![] }).unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("resolving") || message.contains("connecting"),
            "a transport error, not a scheme refusal: {message}"
        );
        assert!(!message.contains("TLS is not wired"));
    }

    #[test]
    fn an_empty_tool_list_is_not_sent() {
        let model = OpenAiCompat::new("http://127.0.0.1:9/v1", "m", None);
        let body: Value = serde_json::from_str(
            &model.body(&CompletionRequest { messages: vec![], tools: vec![] }),
        )
        .unwrap();
        assert!(body.get("tools").is_none(), "no tools, no tools key: {body}");
        assert!(body.get("tool_choice").is_none());

        let with_tools = model.body(&CompletionRequest {
            messages: vec![],
            tools: vec![ToolSpec {
                name: "t".into(),
                description: "d".into(),
                parameters: json!({"type": "object"}),
            }],
        });
        let body: Value = serde_json::from_str(&with_tools).unwrap();
        assert_eq!(body["tools"][0]["function"]["name"], "t");
        assert_eq!(body["tool_choice"], "auto");
    }

    #[test]
    fn malformed_tool_arguments_are_a_protocol_error() {
        let body = "{\"choices\":[{\"message\":{\"tool_calls\":[{\"id\":\"c\",\"function\":\
                    {\"name\":\"t\",\"arguments\":\"not json\"}}]}}]}";
        let err = OpenAiCompat::parse_response(body).unwrap_err();
        assert!(err.to_string().contains("not JSON"), "{err}");
    }

    #[test]
    fn a_response_without_choices_is_rejected() {
        let err = OpenAiCompat::parse_response("{\"error\":\"nope\"}").unwrap_err();
        assert!(err.to_string().contains("no choices[0].message"), "{err}");
    }
}
