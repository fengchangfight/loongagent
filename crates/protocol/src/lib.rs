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
pub struct Request {
    pub jsonrpc: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl Request {
    pub fn new(id: u64, method: &str, params: Value) -> Self {
        Self {
            jsonrpc: JSONRPC.into(),
            id: Some(Value::from(id)),
            method: method.into(),
            params: Some(params),
        }
    }

    pub fn to_line(&self) -> String {
        serde_json::to_string(self).expect("request serializes")
    }

    /// Parse one line. On failure returns the error [`Response`] to send back, so
    /// the caller never has to invent a code.
    pub fn parse(line: &str) -> Result<Self, Response> {
        let value: Value = serde_json::from_str(line).map_err(|e| {
            Response::err(Value::Null, codes::PARSE_ERROR, format!("invalid JSON: {e}"))
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
pub struct Response {
    pub jsonrpc: String,
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl Response {
    pub fn ok(id: Value, result: Value) -> Self {
        Self { jsonrpc: JSONRPC.into(), id, result: Some(result), error: None }
    }

    pub fn err(id: Value, code: i64, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: JSONRPC.into(),
            id,
            result: None,
            error: Some(RpcError { code, message: message.into(), data: None }),
        }
    }

    pub fn to_line(&self) -> String {
        serde_json::to_string(self).expect("response serializes")
    }

    pub fn parse(line: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(line)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Notification {
    pub jsonrpc: String,
    pub method: String,
    pub params: Value,
}

impl Notification {
    pub fn new(method: &str, params: Value) -> Self {
        Self { jsonrpc: JSONRPC.into(), method: method.into(), params }
    }

    pub fn event(envelope: &Value) -> Self {
        Self::new(EVENT_NOTIFICATION, envelope.clone())
    }

    pub fn to_line(&self) -> String {
        serde_json::to_string(self).expect("notification serializes")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_request_round_trips() {
        let req = Request::new(1, "prompt.submit", json!({"session_id": "s", "text": "hi"}));
        let line = req.to_line();
        assert!(!line.contains('\n'));
        assert_eq!(Request::parse(&line).unwrap(), req);
    }

    #[test]
    fn garbage_becomes_a_parse_error_response() {
        let err = Request::parse("{not json").unwrap_err();
        assert_eq!(err.error.unwrap().code, codes::PARSE_ERROR);
        assert_eq!(err.id, Value::Null);
    }

    #[test]
    fn a_wrong_version_becomes_an_invalid_request() {
        let err = Request::parse(r#"{"jsonrpc":"1.0","id":7,"method":"x"}"#).unwrap_err();
        assert_eq!(err.error.unwrap().code, codes::INVALID_REQUEST);
        assert_eq!(err.id, json!(7));
    }

    #[test]
    fn bad_params_name_the_method() {
        let req = Request::parse(
            r#"{"jsonrpc":"2.0","id":1,"method":"prompt.submit","params":{"nope":1}}"#,
        )
        .unwrap();
        let err = req.params_as::<loong_schema::generated::PromptSubmitParams>().unwrap_err();
        let resp = err.error.unwrap();
        assert_eq!(resp.code, codes::INVALID_PARAMS);
        assert!(resp.message.contains("prompt.submit"));
    }

    #[test]
    fn responses_and_notifications_have_the_declared_shape() {
        let ok = Response::ok(json!(1), json!({"accepted": true}));
        let value: Value = serde_json::from_str(&ok.to_line()).unwrap();
        assert_eq!(value["jsonrpc"], "2.0");
        assert_eq!(value["id"], 1);
        assert!(value.get("error").is_none(), "absent when there is no error");
        let note = Notification::event(&json!({"seq": 1, "kind": "message.appended"}));
        let value: Value = serde_json::from_str(&note.to_line()).unwrap();
        assert_eq!(value["method"], "event");
        assert_eq!(value["params"]["kind"], "message.appended");
    }
}
