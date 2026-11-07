//! Purpose: The session's running bill: turn `cost.recorded` events into the numbers `/cost` shows.
//!
//! # Contract
//! - **Serves:** `App::spend`, the `/cost` command and the fixed usage row; the worker and the replay seed fill it.
//! - **Design:** `docs/design/cost-accounting.md` §4.
//! - **Upstream:** `crates/tui-app/src/surface.rs` seeds it from `Client::costs`; `worker.rs` sends live `FromWorker::Cost`.
//! - **Downstream:** `loong_tui`'s formatting helpers, and `App::drain`'s `Cost` arm.
//! - **Verify:** `cargo test -p loong-tui-app spend::` · `./scripts/check.sh`
//!
//! The sum is built only from events. The sender's ledger estimate is a different
//! number (what the next request will carry), and rendering it as a bill would be
//! wrong by however much the provider's tokenizer disagrees with ours.

use loong_schema::generated::event;
use loong_tui::{format_tokens, format_usd, price_basis_label};
use serde_json::Value;
use std::collections::BTreeMap;

/// One model call, as `cost.recorded` recorded it.
///
/// Purpose: A parsed cost event, tolerating fields an older log did not write.
/// Serves: `Spend::add` and `Spend::from_events`.
/// Design: `docs/design/cost-accounting.md` §2 and §4.
/// Up: `CostEntry::from_event`, called by the worker and the replay seed.
/// Down: the `Spend` and `ModelSpend` totals.
/// Verify: `cargo test -p loong-tui-app spend::`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CostEntry {
    pub model: String,
    pub profile: String,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub cache_hit_tokens: u64,
    pub cache_checked_tokens: u64,
    pub cache_reported: bool,
    pub usd_micros: u64,
    pub price_basis: String,
}

impl CostEntry {
    /// Purpose: Parse one event, defaulting the fields an older log did not write.
    /// Serves: the worker's live path and the surface's replay seed.
    /// Design: `docs/design/cost-accounting.md` §4.
    /// Up: `crates/tui-app/src/{worker,surface}.rs` and the spend tests.
    /// Down: `event::COST_RECORDED` and the payload's fields.
    /// Verify: `cargo test -p loong-tui-app spend::`.
    pub fn from_event(event: &Value) -> Option<Self> {
        if event.get("kind").and_then(Value::as_str) != Some(event::COST_RECORDED) {
            return None;
        }
        let data = event.get("data")?;
        let text = |field: &str| data.get(field).and_then(Value::as_str).unwrap_or("");
        let count = |field: &str| data.get(field).and_then(Value::as_u64).unwrap_or(0);
        let profile = text("profile").to_string();
        let reported = data
            .get("cache_reported")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let model = match text("model") {
            "" => profile.clone(),
            name => name.to_string(),
        };
        Some(Self {
            model,
            profile,
            tokens_in: count("tokens_in"),
            tokens_out: count("tokens_out"),
            cache_hit_tokens: count("cache_hit_tokens"),
            // A call that did not report its cache cannot contribute to a hit rate.
            cache_checked_tokens: if reported { count("tokens_in") } else { 0 },
            cache_reported: reported,
            usd_micros: count("usd_micros"),
            price_basis: match text("price_basis") {
                "" => "unknown".to_string(),
                basis => basis.to_string(),
            },
        })
    }
}

/// One model's share of the session: calls, tokens, cache and estimated money.
///
/// Purpose: The per-model rows `/cost` prints under the session total.
/// Serves: `Spend::models`.
/// Design: `docs/design/cost-accounting.md` §4.
/// Up: `Spend::add`.
/// Down: `Spend::summary`.
/// Verify: `cargo test -p loong-tui-app spend::`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelSpend {
    pub calls: u64,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub cache_hit: u64,
    pub cache_checked_in: u64,
    pub usd_micros: u64,
    /// How many calls used each price basis, so a mixed peak/off-peak model says so.
    pub bases: BTreeMap<String, u64>,
}

impl ModelSpend {
    /// Purpose: Add one call to this model's totals.
    /// Serves: `Spend::add`.
    /// Design: `docs/design/cost-accounting.md` §4.
    /// Up: `Spend::add`.
    /// Down: the fields it increments.
    /// Verify: `cargo test -p loong-tui-app spend::`.
    fn add(&mut self, entry: &CostEntry) {
        self.calls += 1;
        self.tokens_in += entry.tokens_in;
        self.tokens_out += entry.tokens_out;
        self.cache_hit += entry.cache_hit_tokens;
        self.cache_checked_in += entry.cache_checked_tokens;
        self.usd_micros += entry.usd_micros;
        *self.bases.entry(entry.price_basis.clone()).or_insert(0) += 1;
    }

    /// Purpose: The price sentence for this model: free, unknown, or an estimate with its basis.
    /// Serves: `Spend::summary`.
    /// Design: `docs/design/cost-accounting.md` §4.
    /// Up: `Spend::summary`.
    /// Down: `price_basis_label` and `format_usd`.
    /// Verify: `cargo test -p loong-tui-app spend::`.
    fn price(&self) -> String {
        let only_free = self.bases.keys().all(|basis| basis == "local-free");
        if only_free {
            return "免费（本地）".to_string();
        }
        let only_unknown = self
            .bases
            .keys()
            .all(|basis| basis == "unknown" || basis == "local-free");
        let price = if self.usd_micros > 0 {
            format!("估算 {}", format_usd(self.usd_micros))
        } else if only_unknown {
            "价格未知".to_string()
        } else {
            format_usd(self.usd_micros)
        };
        let labels: Vec<&str> = self
            .bases
            .keys()
            .map(|basis| price_basis_label(basis))
            .collect();
        format!("{price}（{}）", labels.join("/"))
    }
}

/// Every model call in the session so far, summed from `cost.recorded`.
///
/// Purpose: The session total `/cost` prints and the cache-hit rate it reports.
/// Serves: `App::spend`, `App::surface_command` and `App::drain`.
/// Design: `docs/design/cost-accounting.md` §4.
/// Up: `App::new`, `App::seed_spend` and `App::drain`'s `Cost` arm.
/// Down: `CostEntry`, `ModelSpend` and `loong_tui`'s formatting helpers.
/// Verify: `cargo test -p loong-tui-app spend::`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Spend {
    pub calls: u64,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub cache_hit: u64,
    pub cache_checked_in: u64,
    pub usd_micros: u64,
    /// The input size of the most recent call: the provider's own count of the prompt it
    /// was just sent, and therefore how full the context was on that call.
    pub last_request_tokens: u64,
    /// Calls whose model had no published price; the total is a lower bound.
    pub unpriced_calls: u64,
    pub models: BTreeMap<String, ModelSpend>,
}

impl Spend {
    /// Purpose: Add one recorded call to the session and its model's totals.
    /// Serves: the worker's live path and `Spend::from_events`.
    /// Design: `docs/design/cost-accounting.md` §4.
    /// Up: `App::drain`, `App::seed_spend` and the tests.
    /// Down: the `Spend` fields and `ModelSpend::add`.
    /// Verify: `cargo test -p loong-tui-app spend::`.
    pub fn add(&mut self, entry: CostEntry) {
        self.calls += 1;
        self.tokens_in += entry.tokens_in;
        self.tokens_out += entry.tokens_out;
        self.cache_hit += entry.cache_hit_tokens;
        self.cache_checked_in += entry.cache_checked_tokens;
        self.usd_micros += entry.usd_micros;
        self.last_request_tokens = entry.tokens_in;
        if entry.price_basis == "unknown" {
            self.unpriced_calls += 1;
        }
        self.models
            .entry(entry.model.clone())
            .or_default()
            .add(&entry);
    }

    /// Purpose: Seed a resumed session's total from the `cost.recorded` events replay returned.
    /// Serves: `crates/tui-app/src/surface.rs`.
    /// Design: `docs/design/cost-accounting.md` §4.
    /// Up: `surface::run`.
    /// Down: `CostEntry::from_event` and `Spend::add`.
    /// Verify: `cargo test -p loong-tui-app spend::`.
    pub fn from_events(events: &[Value]) -> Self {
        let mut spend = Self::default();
        for entry in events.iter().filter_map(CostEntry::from_event) {
            spend.add(entry);
        }
        spend
    }

    /// Purpose: The cache hit rate over calls that reported cache use, if any did.
    /// Serves: `Spend::summary`.
    /// Design: `docs/design/cost-accounting.md` §4.
    /// Up: `Spend::summary`.
    /// Down: `cache_hit` and `cache_checked_in`.
    /// Verify: `cargo test -p loong-tui-app spend::`.
    pub fn cache_rate(&self) -> Option<f64> {
        (self.cache_checked_in > 0)
            .then(|| self.cache_hit as f64 * 100.0 / self.cache_checked_in as f64)
    }

    /// Purpose: The fixed usage row's text: tokens in/out, the session's call count, the cache rate and the context share, or nothing before the first call.
    ///
    /// Serves: `App::draw`, so the session's usage is visible without typing `/cost`.
    /// Design: `docs/design/cost-accounting.md` §4; `docs/superpowers/specs/2026-10-28-token-efficiency-design.md` §4.4; `docs/superpowers/specs/2026-11-03-context-visibility-and-compaction-thrash-design.md` §4.1.
    /// Up: `crates/tui-app/src/lib.rs`'s `App::draw`, with the effective budget from the last `ContextExplain`.
    /// Down: `Spend::cache_rate`, `format_tokens` and `last_request_tokens`.
    /// Verify: `cargo test -p loong-tui-app spend::tests::the_usage_line_reports_the_context_share`.
    pub fn usage_line(&self, budget: Option<u64>) -> Option<String> {
        if self.calls == 0 {
            return None;
        }
        let mut line = format!(
            "入 {} · 出 {} · 调用 {}",
            format_tokens(self.tokens_in),
            format_tokens(self.tokens_out),
            self.calls
        );
        if let Some(rate) = self.cache_rate() {
            line.push_str(&format!(" · 缓存 {rate:.0}%"));
        }
        // The last request's input is the provider's own measure of how full the window
        // was. Without a budget there is no honest denominator, so show tokens rather
        // than invent a percentage.
        if self.last_request_tokens > 0 {
            match budget.filter(|budget| *budget > 0) {
                Some(budget) => {
                    let percent = self.last_request_tokens as f64 * 100.0 / budget as f64;
                    line.push_str(&format!(" · 上下文 {percent:.0}%"));
                }
                None => line.push_str(&format!(
                    " · 上下文 {}",
                    format_tokens(self.last_request_tokens)
                )),
            }
        }
        Some(line)
    }

    /// Purpose: The lines `/cost` appends to the transcript: the total first, then one row per model.
    /// Serves: `App::surface_command`'s `/cost` arm.
    /// Design: `docs/design/cost-accounting.md` §4.
    /// Up: `App::surface_command`.
    /// Down: `format_tokens`, `format_usd`, `Spend::cache_rate` and `ModelSpend::price`.
    /// Verify: `cargo test -p loong-tui-app spend::`.
    pub fn summary(&self) -> Vec<String> {
        if self.calls == 0 {
            return vec![
                "· 本会话还没有模型调用：令牌数来自每次回复的 usage，不是发送端的估算".to_string(),
            ];
        }
        let mut total = format!(
            "· 本会话 {} 次模型调用 · 入 {} · 出 {}",
            self.calls,
            format_tokens(self.tokens_in),
            format_tokens(self.tokens_out)
        );
        match self.cache_rate() {
            Some(rate) => total.push_str(&format!(
                " · 缓存命中 {}（{rate:.1}%）",
                format_tokens(self.cache_hit)
            )),
            None => total.push_str(" · 缓存未报告"),
        }
        let all_local = self
            .models
            .values()
            .all(|model| model.bases.keys().all(|basis| basis == "local-free"));
        if all_local {
            total.push_str(" · 免费（本地）");
        } else if self.unpriced_calls == self.calls {
            total.push_str(" · 价格未知");
        } else {
            total.push_str(&format!(" · 估算合计 {}", format_usd(self.usd_micros)));
        }
        if self.unpriced_calls > 0 {
            total.push_str(&format!(
                "（其中 {} 次价格未知，是下限）",
                self.unpriced_calls
            ));
        }
        let mut lines = vec![total];
        for (model, spend) in &self.models {
            let mut row = format!(
                "·   {model}：{} 次 · 入 {} / 出 {}",
                spend.calls,
                format_tokens(spend.tokens_in),
                format_tokens(spend.tokens_out)
            );
            if spend.cache_checked_in > 0 {
                row.push_str(&format!(" · 缓存 {}", format_tokens(spend.cache_hit)));
            } else {
                row.push_str(" · 缓存未报告");
            }
            row.push_str(&format!(" · {}", spend.price()));
            lines.push(row);
        }
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Purpose: A `cost.recorded` event with every field the new schema writes.
    /// Contract: test fixture for the spend tests.
    fn cost_event(model: &str, basis: &str, hit: Option<u64>, usd: u64) -> Value {
        json!({
            "kind": event::COST_RECORDED,
            "data": {
                "profile": "local", "model": model, "tokens_in": 1_000, "tokens_out": 100,
                "cache_hit_tokens": hit.unwrap_or(0), "cache_miss_tokens": 1_000 - hit.unwrap_or(0),
                "cache_reported": hit.is_some(), "usd_micros": usd, "price_basis": basis,
            },
        })
    }

    /// Purpose: Pin that the usage row reports tokens and the cache rate, and nothing before the first call.
    #[test]
    fn the_usage_line_reports_tokens_and_the_cache_rate() {
        let mut spend = Spend::default();
        assert!(spend.usage_line(None).is_none(), "no call, no row");
        spend.add(CostEntry::from_event(&cost_event("k3", "unknown", Some(700), 0)).unwrap());
        let line = spend.usage_line(None).expect("a row after one call");
        assert!(line.contains("入"), "{line}");
        assert!(line.contains("缓存 70%"), "{line}");
        assert!(line.contains("调用 1"), "{line}");
    }

    /// Purpose: Pin that the usage row's call count grows with the session's calls.
    #[test]
    fn the_usage_line_counts_the_sessions_calls() {
        let mut spend = Spend::default();
        spend.add(CostEntry::from_event(&cost_event("k3", "unknown", Some(700), 0)).unwrap());
        spend.add(CostEntry::from_event(&cost_event("k3", "unknown", Some(200), 0)).unwrap());
        let line = spend.usage_line(None).expect("a row after two calls");
        assert!(line.contains("调用 2"), "{line}");
    }

    /// Purpose: The usage row reports the last request's share of the budget, and plain tokens without one.
    #[test]
    fn the_usage_line_reports_the_context_share() {
        let mut spend = Spend::default();
        spend.add(CostEntry::from_event(&cost_event("k3", "unknown", Some(700), 0)).unwrap());
        let line = spend
            .usage_line(Some(100_000))
            .expect("a row after one call");
        assert!(line.contains("上下文 1%"), "{line}");

        let line = spend.usage_line(None).expect("a row after one call");
        assert!(line.ends_with("上下文 1,000"), "{line}");
        assert!(!line.contains("上下文 1%"), "no made-up percentage: {line}");

        let line = spend.usage_line(Some(0)).expect("a row after one call");
        assert!(line.contains("上下文 1,000"), "{line}");
    }

    /// Purpose: Pin that a total sums calls, counts cache only where it was reported, and keeps models apart.
    #[test]
    fn a_session_total_keeps_tokens_cache_and_money_apart() {
        let mut spend = Spend::default();
        spend.add(
            CostEntry::from_event(&cost_event("deepseek-flash", "list-peak", Some(800), 185))
                .unwrap(),
        );
        spend.add(
            CostEntry::from_event(&cost_event("deepseek-flash", "list-off-peak", None, 150))
                .unwrap(),
        );
        spend.add(
            CostEntry::from_event(&cost_event("qwen2.5:7b-instruct", "local-free", None, 0))
                .unwrap(),
        );

        assert_eq!(spend.calls, 3);
        assert_eq!(spend.tokens_in, 3_000);
        assert_eq!(spend.tokens_out, 300);
        assert_eq!(spend.cache_hit, 800);
        assert_eq!(
            spend.cache_checked_in, 1_000,
            "only the call that reported cache is in the denominator"
        );
        assert_eq!(spend.cache_rate(), Some(80.0));
        assert_eq!(spend.usd_micros, 335);
        assert_eq!(spend.unpriced_calls, 0);

        let summary = spend.summary().join("\n");
        assert!(summary.contains("3 次模型调用"), "{summary}");
        assert!(summary.contains("缓存命中 800（80.0%）"), "{summary}");
        assert!(summary.contains("估算合计 $0.000335"), "{summary}");
        assert!(summary.contains("deepseek-flash"), "{summary}");
        assert!(summary.contains("峰时"), "{summary}");
        assert!(summary.contains("谷时"), "{summary}");
        assert!(summary.contains("免费（本地）"), "{summary}");
    }

    /// Purpose: Pin that a pre-pricing event keeps its tokens and is reported as unpriced, never as free.
    #[test]
    fn an_old_cost_event_defaults_to_unknown() {
        let old = json!({
            "kind": event::COST_RECORDED,
            "data": {"profile": "default", "tokens_in": 40, "tokens_out": 4, "usd_micros": 0},
        });
        let mut spend = Spend::default();
        spend.add(CostEntry::from_event(&old).unwrap());
        assert_eq!(spend.tokens_in, 40);
        assert_eq!(spend.tokens_out, 4);
        assert_eq!(spend.unpriced_calls, 1);
        assert_eq!(spend.cache_rate(), None, "an old event reported no cache");
        let summary = spend.summary().join("\n");
        assert!(summary.contains("default"), "{summary}");
        assert!(summary.contains("价格未知"), "{summary}");
        assert!(!summary.contains("免费（本地）"), "{summary}");
    }

    /// Purpose: Pin that a session run only on the local model says free in the total, not $0.000000.
    #[test]
    fn a_local_only_session_says_free() {
        let mut spend = Spend::default();
        spend.add(
            CostEntry::from_event(&cost_event("qwen2.5:7b-instruct", "local-free", None, 0))
                .unwrap(),
        );
        let summary = spend.summary().join("\n");
        assert!(summary.contains("免费（本地）"), "{summary}");
        assert!(!summary.contains("估算合计"), "{summary}");
    }

    /// Purpose: Pin that an empty session says where the numbers would come from.
    #[test]
    fn an_empty_session_explains_itself() {
        let summary = Spend::default().summary();
        assert_eq!(summary.len(), 1);
        assert!(summary[0].contains("usage"), "{summary:?}");
    }
}
