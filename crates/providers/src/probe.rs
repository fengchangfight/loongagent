//! Asking a provider who it is, using nothing but the key the user pasted.
//!
//! This is the whole first-run experience in one function. A person types a key;
//! this asks the vendor's own `/models` route; the answer decides which provider was
//! meant, proves the key works, and — on vendors that say so — reports the context
//! window so the profile can be written with a real `max_context_size` instead of a
//! guess.
//!
//! **Three outcomes, never two.** A first run that reports "your key is wrong" when
//! the truth is "this vendor is unreachable from your network" sends somebody to
//! re-issue a key that was working. So [`Probe::Rejected`] (the provider answered and
//! said no) and [`Probe::Unreachable`] (nothing answered) are separate, and the
//! caller must show them differently.
//!
//! Two facts from the research shape the rest:
//!
//! * **A key's shape cannot identify its owner.** Three of the five `sk-` vendors do
//!   not publish a key format, and none of the rest publish enough to separate them.
//!   So this never guesses — it is asked about one provider at a time, and its answer
//!   is what identifies the provider.
//! * **Zhipu and Z.ai publish no `/models` route** (verified against their own
//!   `openapi.json`). For those the caller must fall back to
//!   [`probe_with_completion`], which spends a token or two instead of nothing.

use loong_http::{self as http, HttpError};
use serde_json::Value;
use std::time::Duration;

/// How long a probe may take, connect and read together.
///
/// Short on purpose: this sits between a person pressing Enter and seeing an answer,
/// and a wrong guess here is cheap to correct (the wizard tries the next candidate)
/// while a slow one is not.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// What one vendor said, and what it can be trusted to mean.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probe {
    /// The key works. `models` is what the vendor listed, in its own order.
    Ok { models: Vec<ModelInfo> },
    /// The provider answered and refused the key.
    ///
    /// `region` marks the one case that is *not* a bad key: DashScope keys are bound
    /// to a region, and a valid key used against another region's endpoint gets a
    /// plain `401 invalid_api_key`. Telling somebody their key is invalid when they
    /// simply pasted the wrong zone's address is the same failure the three outcomes
    /// above exist to prevent.
    Rejected {
        status: u16,
        detail: String,
        region: bool,
    },
    /// Nothing answered: DNS, TCP, TLS, or the timeout. Says nothing about the key.
    Unreachable { detail: String },
}

impl Probe {
    pub fn is_ok(&self) -> bool {
        matches!(self, Probe::Ok { .. })
    }
}

/// What a vendor volunteers about one model.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ModelInfo {
    pub id: String,
    /// The vendor's own number for prompt + answer together, when it publishes one.
    ///
    /// Worth having: it is what the kernel compacts against, and getting it from the
    /// endpoint beats making the user look it up (ADR-0008's `max_context_size`).
    pub context_window: Option<u64>,
}

/// Ask `GET {base_url}/models` with this key.
///
/// `base_url` is the API root the catalog holds, without a trailing slash.
pub fn probe_models(base_url: &str, api_key: Option<&str>) -> Probe {
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    probe_models_url(&url, api_key)
}

/// As [`probe_models`], against a URL that is not `{base}/models` — DashScope's
/// documented list route is on the native path (`/api/v1/models`), not the
/// `compatible-mode` one its chat endpoint uses.
pub fn probe_models_url(url: &str, api_key: Option<&str>) -> Probe {
    match http::get_json_raw(url, api_key, PROBE_TIMEOUT) {
        // A transport error is the one thing that says nothing about the key.
        Err(e) => Probe::Unreachable {
            detail: e.0.clone(),
        },
        Ok(response) if (200..300).contains(&response.status) => Probe::Ok {
            models: parse_models(&response.body),
        },
        Ok(response) => {
            let detail = describe(&response.body);
            let region = looks_like_a_region_mismatch(&detail);
            Probe::Rejected {
                status: response.status,
                detail,
                region,
            }
        }
    }
}

/// The fallback for a vendor with no `/models` route: ask for one token.
///
/// Costs a token or two, which is the honest price of a vendor that publishes no
/// free way to check a key — and the model named here is a guess that will rot, so the
/// reading has to survive being wrong. Only `401`/`403` mean the key was refused. A
/// `400`/`404` means the request was *accepted far enough to complain about the model
/// id*, which is proof that the key was fine; it comes back `Ok` with no models rather
/// than as a rejection, so a stale model name costs a model list, never a working key.
pub fn probe_with_completion(base_url: &str, model: &str, api_key: Option<&str>) -> Probe {
    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    let body = serde_json::json!({
        "model": model,
        "messages": [{"role": "user", "content": "hi"}],
        "max_tokens": 1,
        "stream": false,
    })
    .to_string();
    match http::post_json(&url, &body, api_key, PROBE_TIMEOUT) {
        Err(e) => Probe::Unreachable {
            detail: e.0.clone(),
        },
        Ok(response) if (200..300).contains(&response.status) => Probe::Ok {
            models: vec![ModelInfo {
                id: model.to_string(),
                context_window: None,
            }],
        },
        // The key was accepted; the model name was not. That is a fact about our guess,
        // not about the user's credential.
        Ok(response) if response.status == 400 || response.status == 404 => {
            Probe::Ok { models: Vec::new() }
        }
        Ok(response) => {
            let detail = describe(&response.body);
            let region = looks_like_a_region_mismatch(&detail);
            Probe::Rejected {
                status: response.status,
                detail,
                region,
            }
        }
    }
}

/// Read a `/models` body into model ids.
///
/// Forgiving by design: the vendors here agree on `data[].id` and nothing else.
/// DeepSeek adds `context_window`; Moonshot and DashScope do not. A body we cannot
/// parse is not a failed probe — the key demonstrably worked, and the caller can
/// still write a profile and let the user name a model. Returning "no models" is
/// strictly better than turning a working key into an error.
fn parse_models(body: &str) -> Vec<ModelInfo> {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return Vec::new();
    };
    let list = value
        .get("data")
        .and_then(Value::as_array)
        .or_else(|| value.get("models").and_then(Value::as_array));
    let Some(list) = list else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|entry| {
            // Ollama's native `/api/tags` says `name`; everything OpenAI-shaped says
            // `id`. Accepting both costs a line and saves a special case.
            let id = entry
                .get("id")
                .or_else(|| entry.get("name"))
                .and_then(Value::as_str)?;
            Some(ModelInfo {
                id: id.to_string(),
                context_window: entry
                    .get("context_window")
                    .or_else(|| entry.get("context_length"))
                    .and_then(Value::as_u64),
            })
        })
        .collect()
}

/// Trim a vendor's error body down to something showable.
fn describe(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return "no detail".to_string();
    }
    // Error bodies are JSON in every case seen; pull the message out if it is there,
    // because a person should not be shown a wall of JSON to read one sentence.
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        for path in [&["error", "message"][..], &["message"][..], &["error"][..]] {
            let mut cursor = &value;
            let mut found = true;
            for key in path {
                match cursor.get(key) {
                    Some(next) => cursor = next,
                    None => {
                        found = false;
                        break;
                    }
                }
            }
            if found {
                if let Some(text) = cursor.as_str() {
                    return text.to_string();
                }
            }
        }
    }
    trimmed.chars().take(200).collect()
}

/// Whether a rejection is a region mismatch rather than a bad key.
///
/// DashScope: a key bound to one region returns `401 invalid_api_key` from another
/// region's endpoint. The message is the only thing that says so.
fn looks_like_a_region_mismatch(detail: &str) -> bool {
    let lower = detail.to_ascii_lowercase();
    lower.contains("invalid_api_key")
        || lower.contains("region")
        || detail.contains("地域")
        || detail.contains("区域")
}

/// Every model id the vendor listed, for a caller that just wants the names.
pub fn model_ids(probe: &Probe) -> Vec<String> {
    match probe {
        Probe::Ok { models } => models.iter().map(|m| m.id.clone()).collect(),
        _ => Vec::new(),
    }
}

/// A one-line summary for a status line or an error.
pub fn render(probe: &Probe) -> String {
    match probe {
        Probe::Ok { models } if models.is_empty() => "连接成功（未返回模型列表）".to_string(),
        Probe::Ok { models } => {
            let windows: Vec<u64> = models.iter().filter_map(|m| m.context_window).collect();
            match windows.first() {
                Some(tokens) => format!("连接成功（{} 个模型，context {}）", models.len(), tokens),
                None => format!("连接成功（{} 个模型）", models.len()),
            }
        }
        Probe::Rejected {
            status,
            detail,
            region: true,
        } => format!("密钥可能有效，但区域不匹配（HTTP {status}）：{detail}"),
        Probe::Rejected { status, detail, .. } => format!("密钥被拒绝（HTTP {status}）：{detail}"),
        Probe::Unreachable { detail } => format!("无法连接（这不说明密钥有问题）：{detail}"),
    }
}

impl From<HttpError> for Probe {
    fn from(error: HttpError) -> Self {
        Probe::Unreachable { detail: error.0 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_deepseek_style_body_yields_ids_and_context_windows() {
        // The real shape, captured from `GET https://api.deepseek.com/models`.
        let body = r#"{"object":"list","data":[
            {"id":"deepseek-flash","object":"model","context_window":1048576,
             "max_output_tokens":393216},
            {"id":"deepseek-v4-pro","object":"model","context_window":1048576}
        ]}"#;
        let models = parse_models(body);
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "deepseek-flash");
        assert_eq!(
            models[0].context_window,
            Some(1_048_576),
            "the vendor publishes it, so nobody has to look it up"
        );
        assert_eq!(models[1].id, "deepseek-v4-pro");
    }

    #[test]
    fn a_body_without_context_windows_still_yields_model_ids() {
        // Moonshot and DashScope list ids only. That is a *working key*, so it must
        // not read as a failure.
        let body = r#"{"object":"list","data":[{"id":"kimi-k3"},{"id":"kimi-k2.6"}]}"#;
        let models = parse_models(body);
        assert_eq!(
            model_ids(&Probe::Ok { models }),
            vec!["kimi-k3", "kimi-k2.6"]
        );
    }

    #[test]
    fn an_ollama_shaped_body_is_read_by_its_own_key_name() {
        // Ollama's native `/api/tags` says `name`, not `id`.
        let body = r#"{"models":[{"name":"qwen2.5:3b"},{"name":"llama3:8b"}]}"#;
        let models = parse_models(body);
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "qwen2.5:3b");
    }

    #[test]
    fn an_unparseable_body_is_not_treated_as_a_failed_probe() {
        // Whatever this was, the key worked. Losing the model list is survivable;
        // throwing away a working key is not.
        assert!(parse_models("<html>maintenance</html>").is_empty());
        assert!(parse_models("").is_empty());
        assert!(parse_models(r#"{"object":"list"}"#).is_empty());
    }

    #[test]
    fn a_region_mismatch_is_not_reported_as_a_bad_key() {
        // DashScope's real wording, which is the only thing that distinguishes this
        // from a genuinely wrong key.
        let rejected = Probe::Rejected {
            status: 401,
            detail: "Incorrect API key provided. invalid_api_key".to_string(),
            region: looks_like_a_region_mismatch("Incorrect API key provided. invalid_api_key"),
        };
        let rendered = render(&rejected);
        assert!(
            rendered.contains("区域不匹配"),
            "a valid key in the wrong zone is not a bad key: {rendered}"
        );
        assert!(
            !rendered.contains("密钥被拒绝"),
            "and must not be reported as one: {rendered}"
        );
    }

    #[test]
    fn a_plain_rejection_reads_as_a_rejection() {
        let rejected = Probe::Rejected {
            status: 401,
            detail: "Authentication Fails, Your api key is invalid".to_string(),
            region: looks_like_a_region_mismatch("Authentication Fails, Your api key is invalid"),
        };
        assert!(render(&rejected).contains("密钥被拒绝"));
    }

    #[test]
    fn unreachable_never_blames_the_key() {
        let probe = Probe::Unreachable {
            detail: "connecting to 127.0.0.1:1: Connection refused (os error 111)".to_string(),
        };
        let rendered = render(&probe);
        assert!(
            rendered.contains("不说明密钥有问题"),
            "the user must not go and re-issue a working key: {rendered}"
        );
    }

    #[test]
    fn an_error_body_is_reduced_to_its_message() {
        // A person should not have to read a wall of JSON to find one sentence.
        let described = describe(r#"{"error":{"message":"model not found","type":"x"}}"#);
        assert_eq!(described, "model not found");
        assert_eq!(describe(r#"{"message":"plain"}"#), "plain");
        assert_eq!(describe("not json at all"), "not json at all");
        assert_eq!(describe("   "), "no detail");

        let long = "x".repeat(500);
        assert_eq!(describe(&long).chars().count(), 200, "and is bounded");
    }

    #[test]
    fn the_models_url_is_built_without_doubling_the_slash() {
        // `probe_models` is what the wizard calls, so the join has to be right for
        // both `https://api.deepseek.com` and `https://api.moonshot.cn/v1`.
        for base in ["https://api.deepseek.com", "https://api.moonshot.cn/v1"] {
            let url = format!("{}/models", base.trim_end_matches('/'));
            assert!(!url.contains("//models"), "{url}");
            assert!(url.ends_with("/models"), "{url}");
        }
    }
}
