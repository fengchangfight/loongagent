//! The provider catalog: the handful of facts a person should never have to type.
//!
//! `loongagent tui --local` exists so somebody with a private API key can try the
//! agent without learning what a `base_url` is. The catalog is what makes that
//! possible: a `provider = "deepseek/deepseek-flash"` line expands, here, into the
//! endpoint, the model id, and a *reference* to the key — so the profile a user
//! writes stays four lines long.
//!
//! Two rules from ADR-0008 survive the shorthand untouched:
//!
//! * The expansion never produces a literal secret, only `env:NAME` — the same
//!   reference form [`crate::check_secret_reference`] already enforces.
//! * It is **pure**: every value below is a constant, so `resolve` stays
//!   deterministic and testable. Reading the environment would make a profile mean
//!   something different on every machine that read it.
//!
//! **What this is not:** a routing table. Which *tier* a model belongs to is the
//! user's business (array order in their profile), never the catalog's.

/// A model a vendor documents, for a host that stops enumerating its own.
///
/// Not a substitute for asking: when the endpoint names models, those win. This is what a
/// provider offers when the endpoint answers `200` and names nothing — a state that is
/// usually a vendor quirk and occasionally our own bug, which is why the caller announces
/// when it is being used rather than passing it off as the live list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CatalogModel {
    pub id: &'static str,
    pub context_window: Option<u64>,
}

/// One provider the catalog knows about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Provider {
    /// The name that appears on the left of `provider = "…/…"`, and the id the
    /// wizard stores.
    pub id: &'static str,
    /// What a person sees in the picker.
    pub label: &'static str,
    /// The API root: what goes before `/chat/completions`.
    pub base_url: &'static str,
    /// Where the wizard writes a pasted key, and what the shorthand points at.
    pub key_env: &'static str,
    /// Conventional names the same key may *already* live under, tried in order
    /// after `key_env`. A developer who has exported `DEEPSEEK_API_KEY` for some
    /// other tool should not have to paste it again.
    pub aliases: &'static [&'static str],
    /// Whether `GET {base_url}/models` is documented. `false` for Zhipu and Z.ai,
    /// which publish no such route (verified against their `openapi.json`), so
    /// validation there has to fall back to a one-token completion.
    pub lists_models: bool,
    /// For a provider with no list route: a cheap model to spend one token on when
    /// checking a key.
    ///
    /// A guess, and a guess that will rot — so the caller must read a `400`/`404` as
    /// "our model name was stale", never as "your key is bad". It is here rather than
    /// in the adapter because it is a fact about the vendor, and the catalog is where
    /// those live.
    pub probe_model: Option<&'static str>,
    /// Where to ask "which models do you serve?", when that is not `{base_url}/models`.
    ///
    /// Ollama is the case: the OpenAI shim at `/v1/models` answers (and is what chat
    /// goes through), but it reports no context window, while the native `/api/tags`
    /// reports `context_length`. That number is what the kernel compacts against, so
    /// asking the endpoint that knows it turns a warning at startup into a working cap.
    pub probe_url: Option<&'static str>,
    /// What to say when this provider *rejects* a key, when there is a known wrong door.
    ///
    /// Several of these vendors sell two products with two hosts and two key types —
    /// Kimi Code against the open platform, a GLM coding plan against a general key —
    /// and the key from one gets a plain `401` from the other. A generic "your key was
    /// rejected" sends somebody to re-issue a working credential; this says which door
    /// they are probably standing at.
    pub wrong_door: Option<&'static str>,
    /// Offered when the vendor names nothing. Empty for a vendor that enumerates.
    pub curated_models: &'static [CatalogModel],
    /// Loopback: no key, no cost, and the provider is this machine.
    pub local: bool,
}

impl Provider {
    /// The URL that answers "which models do you serve?" — the free half of a key
    /// check. `None` where the vendor publishes no such route.
    pub fn models_url(&self) -> Option<String> {
        if let Some(explicit) = self.probe_url {
            return Some(explicit.to_string());
        }
        self.lists_models
            .then(|| format!("{}/models", self.base_url.trim_end_matches('/')))
    }
}

/// Every provider the shorthand understands.
///
/// Deliberately short. Each entry is a claim that `GET {base_url}/models` behaves
/// a certain way, and a claim nobody can keep is worse than an absent entry: a
/// wrong endpoint fails at the worst possible moment, on a first run.
///
/// Ordered so that the vendors most likely to be configured come first — this is
/// also the order the wizard suggests candidates in when a key's shape is
/// ambiguous (which, as the research showed, it usually is: three of these five
/// `sk-` vendors do not publish their key format at all).
/// What Kimi Code documents, in the order its docs present them.
///
/// `k3` reaches 1M context only on the higher tiers, but the window is a property of the
/// plan rather than of the id, and this is the number the vendor publishes. Checked
/// against the live `/coding/v1/models` answer, id for id and window for window.
const KIMI_CODE_MODELS: &[CatalogModel] = &[
    CatalogModel {
        id: "k3",
        context_window: Some(1_048_576),
    },
    CatalogModel {
        id: "k3-256k",
        context_window: Some(262_144),
    },
    CatalogModel {
        id: "kimi-for-coding",
        context_window: Some(1_048_576),
    },
    CatalogModel {
        id: "kimi-for-coding-highspeed",
        context_window: Some(262_144),
    },
];

pub const PROVIDERS: &[Provider] = &[
    Provider {
        id: "deepseek",
        label: "DeepSeek",
        base_url: "https://api.deepseek.com",
        key_env: "LOONG_DEEPSEEK_KEY",
        aliases: &["DEEPSEEK_API_KEY"],
        lists_models: true,
        probe_model: None,
        probe_url: None,
        wrong_door: None,
        curated_models: &[],
        local: false,
    },
    Provider {
        id: "kimi-code",
        label: "Kimi Code（编程套餐）",
        // Probed: `/coding/v1/models`, `/coding/v1/chat/completions` and
        // `/coding/v1/messages` all answer; `/coding/models` is a 404, so the `/v1` is
        // part of the path. A Kimi Code key gets a 401 from the open platform below and
        // vice versa — two products, two hosts, two key types. The vendor's own FAQ
        // tabulates exactly this pair, which is how it was confirmed.
        //
        // `/coding/v1/models` returns all four with their `context_length`, so
        // `curated_models` is a spare rather than the source. It is kept because it is now
        // *verified* against that live answer rather than guessed from the docs.
        //
        // Probing with a *completion* instead would be worse than useless here: the docs
        // say asking for a model above your plan also answers `401`, so a paying member on
        // a lower tier would be told their key was bad.
        base_url: "https://api.kimi.com/coding/v1",
        key_env: "LOONG_KIMI_CODE_KEY",
        aliases: &["KIMI_CODE_API_KEY", "KIMI_API_KEY"],
        lists_models: true,
        probe_model: None,
        probe_url: None,
        wrong_door: Some(
            "如果这是 Moonshot 开放平台的 key（按量计费那种），请选「Moonshot 开放平台」；\
             海外账号请选「Kimi Code（编程套餐·海外）」",
        ),
        curated_models: KIMI_CODE_MODELS,
        local: false,
    },
    Provider {
        id: "kimi-code-intl",
        label: "Kimi Code（编程套餐·海外）",
        // Probed: identical to the China host — `/coding/v1/models`,
        // `/coding/v1/chat/completions` and `/coding/v1/messages` all answer 401 to a bad
        // key. (The two hosts were first compared with `GET /chat/completions`, which is
        // a 404 on both, because that route is POST-only. Worth remembering before
        // concluding a host is missing a route.)
        base_url: "https://api.kimi.ai/coding/v1",
        key_env: "LOONG_KIMI_CODE_INTL_KEY",
        aliases: &["KIMI_CODE_INTL_API_KEY"],
        lists_models: true,
        probe_model: None,
        probe_url: None,
        wrong_door: Some(
            "国内账号请选「Kimi Code（编程套餐）」—— kimi.com 和 kimi.ai 的 key 不通用",
        ),
        curated_models: KIMI_CODE_MODELS,
        local: false,
    },
    Provider {
        id: "moonshot",
        label: "Moonshot 开放平台（中国区）",
        base_url: "https://api.moonshot.cn/v1",
        key_env: "LOONG_MOONSHOT_KEY",
        aliases: &["MOONSHOT_API_KEY"],
        lists_models: true,
        probe_model: None,
        probe_url: None,
        wrong_door: Some(
            "如果这是 Kimi Code（编程套餐）的 key，请选「Kimi Code」—— 两者是不同的产品，\
             域名和 key 都不通用",
        ),
        curated_models: &[],
        local: false,
    },
    Provider {
        id: "moonshot-intl",
        label: "Moonshot Open Platform（全球区）",
        base_url: "https://api.moonshot.ai/v1",
        key_env: "LOONG_MOONSHOT_INTL_KEY",
        aliases: &["MOONSHOT_INTL_API_KEY"],
        lists_models: true,
        probe_model: None,
        probe_url: None,
        wrong_door: Some(
            "中国区的 key 和全球区的 key 不通用（报错措辞是一样的），\
             中国的账号请选「Moonshot 开放平台（中国区）」",
        ),
        curated_models: &[],
        local: false,
    },
    Provider {
        id: "dashscope",
        label: "阿里云百炼 / Qwen",
        base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1",
        key_env: "LOONG_DASHSCOPE_KEY",
        aliases: &["DASHSCOPE_API_KEY", "QWEN_API_KEY"],
        lists_models: true,
        probe_model: None,
        probe_url: None,
        wrong_door: Some("阿里云百炼的 key 绑定地域，换错区域的端点会报 401 invalid_api_key"),
        curated_models: &[],
        local: false,
    },
    Provider {
        id: "zhipu",
        label: "智谱 GLM",
        base_url: "https://open.bigmodel.cn/api/paas/v4",
        key_env: "LOONG_ZHIPU_KEY",
        aliases: &["ZHIPU_API_KEY", "GLM_API_KEY"],
        lists_models: false,
        probe_model: Some("glm-4-flash"),
        probe_url: None,
        wrong_door: Some("Z.ai Coding Plan 的 key 用另一个入口（Z.ai）"),
        curated_models: &[],
        local: false,
    },
    Provider {
        id: "zai",
        label: "Z.ai GLM",
        base_url: "https://api.z.ai/api/paas/v4",
        key_env: "LOONG_ZAI_KEY",
        aliases: &["ZAI_API_KEY"],
        lists_models: false,
        probe_model: Some("glm-4-flash"),
        probe_url: None,
        wrong_door: Some("智谱通用 API 的 key 用另一个入口（智谱 GLM）"),
        curated_models: &[],
        local: false,
    },
    Provider {
        id: "openai",
        label: "OpenAI",
        base_url: "https://api.openai.com/v1",
        key_env: "LOONG_OPENAI_KEY",
        aliases: &["OPENAI_API_KEY"],
        lists_models: true,
        probe_model: None,
        probe_url: None,
        wrong_door: None,
        curated_models: &[],
        local: false,
    },
    Provider {
        id: "ollama",
        label: "Ollama (本机)",
        base_url: "http://127.0.0.1:11434/v1",
        key_env: "LOONG_OLLAMA_KEY",
        aliases: &[],
        lists_models: true,
        probe_model: None,
        probe_url: Some("http://127.0.0.1:11434/api/tags"),
        wrong_door: None,
        curated_models: &[],
        local: true,
    },
];

/// Look one up by id.
pub fn provider(id: &str) -> Option<&'static Provider> {
    PROVIDERS.iter().find(|p| p.id == id)
}

/// What a `provider = "deepseek/deepseek-flash"` line turns into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expansion {
    pub provider: &'static Provider,
    pub base_url: String,
    pub model: String,
    /// A reference, never a secret (ADR-0008 §5) — and `None` for a provider that wants
    /// no key at all.
    pub api_key: Option<String>,
}

/// Expand `"<provider>/<model>"`.
///
/// Split on the **first** slash only: a model id may contain more (a namespaced
/// Ollama tag, a gateway path), and the provider name never does.
pub fn expand(spec: &str) -> Result<Expansion, String> {
    let spec = spec.trim();
    let (id, model) = spec.split_once('/').ok_or_else(|| {
        format!(
            "provider {spec:?} must be written `<provider>/<model>`, e.g. \
             \"deepseek/deepseek-flash\" — the providers are: {}",
            ids().join(", ")
        )
    })?;
    let id = id.trim();
    let model = model.trim();
    if model.is_empty() {
        return Err(format!(
            "provider {spec:?} names no model — write `<provider>/<model>`, e.g. \
             \"deepseek/deepseek-flash\""
        ));
    }
    let provider = provider(id).ok_or_else(|| {
        format!(
            "unknown provider {id:?} — the catalog knows: {}",
            ids().join(", ")
        )
    })?;
    Ok(Expansion {
        provider,
        base_url: provider.base_url.to_string(),
        model: model.to_string(),
        // A loopback provider gets no key reference. Naming one would make the adapter
        // send an `Authorization` header to a server that wants none, and — worse — gives
        // whatever happens to be in that variable a home it should not have. It did: a
        // DeepSeek key ended up stored as `LOONG_OLLAMA_KEY` because the field was
        // prefilled and switching provider did not clear it.
        api_key: (!provider.local).then(|| format!("env:{}", provider.key_env)),
    })
}

/// The ids, for an error message or a picker.
pub fn ids() -> Vec<&'static str> {
    PROVIDERS.iter().map(|p| p.id).collect()
}

/// Candidate providers for a pasted key, most likely first.
///
/// **A hint, not a decision.** No vendor here documents its key format, so the
/// shape of a key cannot identify its owner: `sk-` alone matches DeepSeek,
/// Moonshot, DashScope and OpenAI. All this does is choose which row the picker
/// highlights first — the user confirms, and the probe decides.
///
/// It never returns an empty list, so a caller can always fall back to "ask".
pub fn guess_from_key(key: &str) -> Vec<&'static str> {
    let key = key.trim();
    let remote: Vec<&'static str> = PROVIDERS
        .iter()
        .filter(|p| !p.local)
        .map(|p| p.id)
        .collect();

    // `sk-` + exactly 32 lowercase hex is the shape of the two keys this project has
    // actually seen (DeepSeek's, and the sample in DashScope's docs). Both vendors
    // are equally plausible, so both lead — everything else follows in catalog order.
    let hex32 = key
        .strip_prefix("sk-")
        .filter(|rest| rest.len() == 32 && rest.chars().all(|c| c.is_ascii_hexdigit()));
    if hex32.is_some() {
        let mut ordered: Vec<&'static str> = ["deepseek", "dashscope"]
            .into_iter()
            .filter(|id| remote.contains(id))
            .collect();
        for id in &remote {
            if !ordered.contains(id) {
                ordered.push(id);
            }
        }
        return ordered;
    }

    // `sk-proj-` / `sk-svcacct-` / `sk-admin-` are OpenAI's newer, self-identifying
    // prefixes, so they can lead without a probe.
    if ["sk-proj-", "sk-svcacct-", "sk-admin-"]
        .iter()
        .any(|p| key.starts_with(p))
    {
        let mut ordered: Vec<&'static str> = vec!["openai"];
        for id in &remote {
            if !ordered.contains(id) {
                ordered.push(id);
            }
        }
        return ordered;
    }

    // Anything else: no evidence, so no ordering beyond the catalog's own.
    remote
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_provider_and_a_model_expand_to_an_endpoint_and_a_reference() {
        let expanded = expand("deepseek/deepseek-flash").unwrap();
        assert_eq!(expanded.base_url, "https://api.deepseek.com");
        assert_eq!(expanded.model, "deepseek-flash");
        assert_eq!(
            expanded.api_key.as_deref(),
            Some("env:LOONG_DEEPSEEK_KEY"),
            "the shorthand names a variable, never a secret (ADR-0008 §5)"
        );
        assert_eq!(expanded.provider.id, "deepseek");
    }

    #[test]
    fn a_local_provider_expands_with_no_key_at_all() {
        // Ollama wants none, so there is nothing to name — and nothing for a secret to be
        // filed under on the way past.
        let expanded = expand("ollama/qwen2.5:3b").unwrap();
        assert_eq!(expanded.api_key, None);
        assert_eq!(expanded.provider.id, "ollama");
        assert_eq!(expanded.base_url, "http://127.0.0.1:11434/v1");
    }

    #[test]
    fn only_the_first_slash_splits_because_model_ids_may_contain_more() {
        let expanded = expand("ollama/qwen2.5:3b").unwrap();
        assert_eq!(expanded.provider.id, "ollama");
        assert_eq!(
            expanded.model, "qwen2.5:3b",
            "a tag keeps its colon and any namespace keeps its slashes"
        );

        let namespaced = expand("openai/library/some-model").unwrap();
        assert_eq!(namespaced.provider.id, "openai");
        assert_eq!(namespaced.model, "library/some-model");
    }

    #[test]
    fn a_malformed_shorthand_says_what_the_providers_are() {
        let err = expand("deepseek-flash").unwrap_err();
        assert!(err.contains("<provider>/<model>"), "{err}");
        assert!(err.contains("deepseek"), "and lists them: {err}");

        let unknown = expand("nope/some-model").unwrap_err();
        assert!(unknown.contains("unknown provider"), "{unknown}");
        assert!(unknown.contains("dashscope"), "{unknown}");

        let no_model = expand("deepseek/").unwrap_err();
        assert!(no_model.contains("names no model"), "{no_model}");
    }

    #[test]
    fn every_provider_has_a_usable_models_url_except_the_two_that_do_not() {
        for provider in PROVIDERS {
            match provider.models_url() {
                Some(url) => {
                    assert!(url.starts_with("http"), "{url}");
                    if provider.probe_url.is_none() {
                        assert!(url.ends_with("/models"), "{url}");
                        assert!(
                            !url.contains("//models"),
                            "{url} — a base_url already ending in / must not double the slash"
                        );
                    } else {
                        // An override exists to point somewhere other than `{base}/models`.
                        assert_eq!(
                            provider.id, "ollama",
                            "{} overrides its list route; say why it has to",
                            provider.id
                        );
                    }
                }
                None => assert!(
                    matches!(provider.id, "zhipu" | "zai"),
                    "{} has no list route and that is a deliberate, verified fact",
                    provider.id
                ),
            }
        }
        assert_eq!(
            provider("ollama").unwrap().models_url().as_deref(),
            Some("http://127.0.0.1:11434/api/tags"),
            "Ollama's native route, because the OpenAI shim reports no context window"
        );
        assert_eq!(
            provider("zhipu").unwrap().models_url(),
            None,
            "Zhipu publishes no /models route (verified against its openapi.json)"
        );
    }

    #[test]
    fn a_provider_with_no_list_route_names_a_model_to_spend_a_token_on() {
        // The two facts have to agree: a vendor whose key cannot be checked for free
        // must say what to check it against, and one that can must not carry a guess
        // that will rot.
        for provider in PROVIDERS {
            match provider.models_url() {
                Some(_) => assert_eq!(
                    provider.probe_model, None,
                    "{} has a free route, so it needs no guess",
                    provider.id
                ),
                None => assert!(
                    provider.probe_model.is_some(),
                    "{} publishes no /models, so checking a key there needs a model",
                    provider.id
                ),
            }
        }
    }

    #[test]
    fn kimi_code_is_its_own_door_and_not_the_open_platform() {
        // The bug: a Kimi Code key was sent to `api.moonshot.cn/v1` and came back
        // `401 Invalid Authentication`, which reads as "your key is wrong" — and it was
        // not. They are different products on different hosts. Verified by probing both:
        // a bad key gets `Invalid Authentication` from the open platform and
        // "The API Key appears to be invalid or may have expired" from Kimi Code, which
        // is how the two were told apart.
        let code = provider("kimi-code").expect("Kimi Code is a door of its own");
        assert_eq!(code.base_url, "https://api.kimi.com/coding/v1");
        assert_eq!(
            code.models_url().as_deref(),
            Some("https://api.kimi.com/coding/v1/models")
        );
        assert_ne!(
            code.base_url,
            provider("moonshot").unwrap().base_url,
            "and it is not the open platform"
        );

        // The two Moonshot regions are different keys as well, and the error does not say
        // so, so the entry has to.
        assert_eq!(
            provider("moonshot").unwrap().base_url,
            "https://api.moonshot.cn/v1"
        );
        assert_eq!(
            provider("moonshot-intl").unwrap().base_url,
            "https://api.moonshot.ai/v1"
        );
    }

    #[test]
    fn every_vendor_with_two_doors_says_which_one_a_rejected_key_belongs_to() {
        // The generic "rejected" message sends somebody to re-issue a working key. These
        // are the vendors where the other product is a real possibility.
        for id in [
            "kimi-code",
            "moonshot",
            "moonshot-intl",
            "zhipu",
            "zai",
            "dashscope",
        ] {
            let provider = provider(id).unwrap_or_else(|| panic!("{id} exists"));
            assert!(
                provider.wrong_door.is_some(),
                "{id} has a second door its users confuse it with"
            );
        }
        // And a vendor that does not gets no invented hint.
        for id in ["deepseek", "openai", "ollama"] {
            assert_eq!(provider(id).unwrap().wrong_door, None, "{id}");
        }
    }

    #[test]
    fn a_key_of_ambiguous_shape_leads_with_the_two_vendors_it_could_be() {
        let hex = guess_from_key("sk-4f06f4b88d7149ca8eeaa939d0fb08bd");
        assert_eq!(hex[0], "deepseek");
        assert_eq!(hex[1], "dashscope");
        assert_eq!(
            hex.len(),
            PROVIDERS.len() - 1,
            "every remote provider stays on the list; only the order is a guess"
        );
        assert!(
            !hex.contains(&"ollama"),
            "a loopback provider is never a guess for a pasted key"
        );
    }

    #[test]
    fn a_self_identifying_prefix_leads_without_probing_first() {
        let openai = guess_from_key("sk-proj-abc123");
        assert_eq!(openai[0], "openai");
        assert_eq!(openai.len(), PROVIDERS.len() - 1);
    }

    #[test]
    fn an_unrecognisable_key_still_returns_every_candidate() {
        let unknown = guess_from_key("hunter2");
        assert_eq!(
            unknown.len(),
            PROVIDERS.len() - 1,
            "no evidence means no ordering, not no options"
        );
        assert_eq!(unknown[0], "deepseek", "just the catalog's own order");
    }
}
