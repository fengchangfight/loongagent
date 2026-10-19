//! Purpose: Define the JSON-RPC 2.0 envelopes and one-line framing that carry schema messages between surfaces and the server.
//!
//! # Contract
//! - **Serves:** a thin client that sends intents and receives the event stream, holding no agent logic (ADR-0009).
//! - **Design:** `docs/design/events-storage.md` (ADR-0002).
//! - **Upstream:** `loong-server`'s request loop, `loong-tui`'s client and `loong-mcp`'s client.
//! - **Downstream:** `Request`, `Response` and `Notification`, over `loong_schema::generated::RpcError`.
//! - **Verify:** `cargo test -p loong-protocol` · `./scripts/check.sh`
//!
//! One line in, one line out — newline-delimited JSON over stdio or a unix
//! socket. Clients are thin (ADR-0009): they send intents, they receive the
//! event stream, and they hold no agent logic.
//! JSON-RPC 2.0 envelopes and framing (ADR-0002).
//!
//! One line in, one line out — newline-delimited JSON over stdio or a unix
//! socket. Clients are thin (ADR-0009): they send intents, they receive the
//! event stream, and they hold no agent logic.

use loong_schema::generated::RpcError;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const JSONRPC: &str = "2.0";

/// Standard JSON-RPC error codes.
pub mod codes {
    pub const PARSE_ERROR: i64 = -32700;
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    pub const INTERNAL_ERROR: i64 = -32603;
}

/// Server → client notification carrying one event (ADR-0003).
pub const EVENT_NOTIFICATION: &str = "event";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
/// Purpose: One JSON-RPC 2.0 request: a method call with an id, params and the protocol version.
///
/// Serves: intents travelling one line from a surface to the server (ADR-0009).
/// Design: `docs/design/events-storage.md` (ADR-0002).
/// Up: surfaces and clients that send a method call, such as `loong-tui` and `loong-mcp`.
/// Down: `Request::parse` and `params_as`, which decode the fields this struct carries.
/// Verify: `cargo test -p loong-protocol`.
pub struct Request {
    pub jsonrpc: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl Request {
    /// Purpose: Build a request with the protocol version and a numeric id filled in.
    ///
    /// Serves: a caller that sends an intent without hand-writing the envelope.
    /// Design: `docs/design/events-storage.md` (ADR-0002).
    /// Up: `loong-tui`'s client and `loong-mcp`'s client when they send a method call.
    /// Down: `JSONRPC` and the `Request` fields.
    /// Verify: `cargo test -p loong-protocol`.
    pub fn new(id: u64, method: &str, params: Value) -> Self {
        Self {
            jsonrpc: JSONRPC.into(),
            id: Some(Value::from(id)),
            method: method.into(),
            params: Some(params),
        }
    }

    /// Purpose: Serialize the request to the single line the transport writes.
    ///
    /// Serves: one JSON object per line, which is the wire framing (events-storage.md §2).
    /// Design: `docs/design/events-storage.md` (ADR-0002).
    /// Up: `loong-tui` and `loong-mcp` when they frame a request.
    /// Down: `serde_json::to_string` over the `Request` fields.
    /// Verify: `cargo test -p loong-protocol`.
    pub fn to_line(&self) -> String {
        serde_json::to_string(self).expect("request serializes")
    }

    /// Parse one line. On failure returns the error [`Response`] to send back, so
    /// the caller never has to invent a code.
    /// Purpose: Parse one line into a request, returning the response to send when it is invalid.
    ///
    /// Serves: a server that never has to invent a JSON-RPC error code (events-storage.md §2).
    /// Design: `docs/design/events-storage.md` (ADR-0002).
    /// Up: `loong-server`'s request loop.
    /// Down: `serde_json` decoding, `Response::err` and `codes`.
    /// Verify: `cargo test -p loong-protocol`.
    pub fn parse(line: &str) -> Result<Self, Response> {
        let value: Value = serde_json::from_str(line).map_err(|e| {
            Response::err(
                Value::Null,
                codes::PARSE_ERROR,
                format!("invalid JSON: {e}"),
            )
        })?;
        let request: Request = serde_json::from_value(value.clone()).map_err(|e| {
            Response::err(
                value.get("id").cloned().unwrap_or(Value::Null),
                codes::INVALID_REQUEST,
                format!("not a JSON-RPC request: {e}"),
            )
        })?;
        if request.jsonrpc != JSONRPC {
            return Err(Response::err(
                request.id.clone().unwrap_or(Value::Null),
                codes::INVALID_REQUEST,
                format!("jsonrpc must be {JSONRPC:?}"),
            ));
        }
        Ok(request)
    }

    /// Purpose: Decode the request params into a typed schema struct, naming the method on failure.
    ///
    /// Serves: a server method arm that receives the params type the schema declares.
    /// Design: `docs/design/events-storage.md` (ADR-0002).
    /// Up: `loong-server`'s method arms after `Request::parse`.
    /// Down: `serde_json::from_value` and `Response::err` with `codes::INVALID_PARAMS`.
    /// Verify: `cargo test -p loong-protocol`.
    pub fn params_as<T: DeserializeOwned>(&self) -> Result<T, Response> {
        let value = self.params.clone().unwrap_or(Value::Null);
        serde_json::from_value(value).map_err(|e| {
            Response::err(
                self.id.clone().unwrap_or(Value::Null),
                codes::INVALID_PARAMS,
                format!("invalid params for {}: {e}", self.method),
            )
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
/// Purpose: One JSON-RPC 2.0 response: exactly one of a result or an error, plus the request id.
///
/// Serves: an answer a client can match to its request (ADR-0009).
/// Design: `docs/design/events-storage.md` (ADR-0002).
/// Up: `loong-server` when it answers a method, and `loong-tui`/`loong-mcp` when they read one.
/// Down: `RpcError` from the schema and the `Response` constructors.
/// Verify: `cargo test -p loong-protocol`.
pub struct Response {
    pub jsonrpc: String,
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl Response {
    /// Purpose: Build a successful response carrying the method's result.
    ///
    /// Serves: a server answer with no error field on the wire.
    /// Design: `docs/design/events-storage.md` (ADR-0002).
    /// Up: `loong-server`'s successful method arms.
    /// Down: `JSONRPC` and the `Response` fields.
    /// Verify: `cargo test -p loong-protocol`.
    pub fn ok(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: JSONRPC.into(),
            id,
            result: Some(result),
            error: None,
        }
    }

    /// Purpose: Build an error response from a code and a message.
    ///
    /// Serves: a parse or params failure the client can read as JSON-RPC rather than a dead line.
    /// Design: `docs/design/events-storage.md` (ADR-0002).
    /// Up: `Request::parse`, `params_as` and `loong-server`'s error arms.
    /// Down: `RpcError` and the `Response` fields.
    /// Verify: `cargo test -p loong-protocol`.
    pub fn err(id: Value, code: i64, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: JSONRPC.into(),
            id,
            result: None,
            error: Some(RpcError {
                code,
                message: message.into(),
                data: None,
            }),
        }
    }

    /// Purpose: Serialize the response to the single line the transport writes.
    ///
    /// Serves: one JSON object per line, which is the wire framing (events-storage.md §2).
    /// Design: `docs/design/events-storage.md` (ADR-0002).
    /// Up: `loong-server` when it writes an answer.
    /// Down: `serde_json::to_string` over the `Response` fields.
    /// Verify: `cargo test -p loong-protocol`.
    pub fn to_line(&self) -> String {
        serde_json::to_string(self).expect("response serializes")
    }

    /// Purpose: Parse one response line back into its typed envelope.
    ///
    /// Serves: a client matching a server answer to the request it sent.
    /// Design: `docs/design/events-storage.md` (ADR-0002).
    /// Up: `loong-tui`'s client and `loong-mcp`'s client.
    /// Down: `serde_json::from_str` over the `Response` fields.
    /// Verify: `cargo test -p loong-protocol`.
    pub fn parse(line: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(line)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
/// Purpose: One JSON-RPC 2.0 notification: a method and params with no id and no reply.
///
/// Serves: the server-to-client event push, which is never answered (ADR-0003).
/// Design: `docs/design/events-storage.md` (ADR-0002).
/// Up: `loong-server`'s event sink, and `loong-tui`/`loong-mcp` when they read one.
/// Down: `Notification::event` and `EVENT_NOTIFICATION`.
/// Verify: `cargo test -p loong-protocol`.
pub struct Notification {
    pub jsonrpc: String,
    pub method: String,
    pub params: Value,
}

impl Notification {
    /// Purpose: Build a notification from a method name and its params.
    ///
    /// Serves: any server-to-client push that is not the standard event envelope.
    /// Design: `docs/design/events-storage.md` (ADR-0002).
    /// Up: `loong-server` for transient pushes such as `model.delta`.
    /// Down: `JSONRPC` and the `Notification` fields.
    /// Verify: `cargo test -p loong-protocol`.
    pub fn new(method: &str, params: Value) -> Self {
        Self {
            jsonrpc: JSONRPC.into(),
            method: method.into(),
            params,
        }
    }

    /// Purpose: Wrap one event envelope in the standard `event` notification.
    ///
    /// Serves: the surface event stream, which carries the record after it is written (events-storage.md §2).
    /// Design: `docs/design/events-storage.md` (ADR-0002, ADR-0003).
    /// Up: `loong-server`'s event sink and replay path.
    /// Down: `Notification::new` and `EVENT_NOTIFICATION`.
    /// Verify: `cargo test -p loong-protocol`.
    pub fn event(envelope: &Value) -> Self {
        Self::new(EVENT_NOTIFICATION, envelope.clone())
    }

    /// Purpose: Serialize the notification to the single line the transport writes.
    ///
    /// Serves: one JSON object per line, which is the wire framing (events-storage.md §2).
    /// Design: `docs/design/events-storage.md` (ADR-0002).
    /// Up: `loong-server` when it pushes an event or delta.
    /// Down: `serde_json::to_string` over the `Notification` fields.
    /// Verify: `cargo test -p loong-protocol`.
    pub fn to_line(&self) -> String {
        serde_json::to_string(self).expect("notification serializes")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    /// Purpose: Pins that a request serializes to one line and parses back unchanged.
    fn a_request_round_trips() {
        let req = Request::new(1, "prompt.submit", json!({"session_id": "s", "text": "hi"}));
        let line = req.to_line();
        assert!(!line.contains('\n'));
        assert_eq!(Request::parse(&line).unwrap(), req);
    }

    #[test]
    /// Purpose: Pins that invalid JSON becomes a `-32700` response with a null id.
    fn garbage_becomes_a_parse_error_response() {
        let err = Request::parse("{not json").unwrap_err();
        assert_eq!(err.error.unwrap().code, codes::PARSE_ERROR);
        assert_eq!(err.id, Value::Null);
    }

    #[test]
    /// Purpose: Pins that a wrong `jsonrpc` version is a `-32600` response that keeps the id.
    fn a_wrong_version_becomes_an_invalid_request() {
        let err = Request::parse(r#"{"jsonrpc":"1.0","id":7,"method":"x"}"#).unwrap_err();
        assert_eq!(err.error.unwrap().code, codes::INVALID_REQUEST);
        assert_eq!(err.id, json!(7));
    }

    #[test]
    /// Purpose: Pins that bad params become `-32602` and name the method.
    fn bad_params_name_the_method() {
        let req = Request::parse(
            r#"{"jsonrpc":"2.0","id":1,"method":"prompt.submit","params":{"nope":1}}"#,
        )
        .unwrap();
        let err = req
            .params_as::<loong_schema::generated::PromptSubmitParams>()
            .unwrap_err();
        let resp = err.error.unwrap();
        assert_eq!(resp.code, codes::INVALID_PARAMS);
        assert!(resp.message.contains("prompt.submit"));
    }

    #[test]
    /// Purpose: Pins the serialized shape of a response and an event notification.
    fn responses_and_notifications_have_the_declared_shape() {
        let ok = Response::ok(json!(1), json!({"accepted": true}));
        let value: Value = serde_json::from_str(&ok.to_line()).unwrap();
        assert_eq!(value["jsonrpc"], "2.0");
        assert_eq!(value["id"], 1);
        assert!(
            value.get("error").is_none(),
            "absent when there is no error"
        );
        let note = Notification::event(&json!({"seq": 1, "kind": "message.appended"}));
        let value: Value = serde_json::from_str(&note.to_line()).unwrap();
        assert_eq!(value["method"], "event");
        assert_eq!(value["params"]["kind"], "message.appended");
    }
}
