//! Purpose: Published model prices and the estimate one provider-reported usage becomes.
//!
//! # Contract
//! - **Serves:** `Kernel::record_cost`, which writes the estimate into `cost.recorded`.
//! - **Design:** `docs/design/cost-accounting.md` §3.
//! - **Upstream:** `crates/kernel/src/turn.rs` calls `estimate` after every model call.
//! - **Downstream:** `Estimate` and `Basis`, which the kernel copies onto the event.
//! - **Verify:** `cargo test -p loong-config pricing::` · `./scripts/check.sh`
//!
//! Prices are integer micro-USD per million tokens, so a session total is exact to
//! the micro-dollar and does not drift with float accumulation. A price the table
//! does not know is `Unknown`, never zero: the two are different facts, and a
//! zero for an unpriced remote model would understate a real bill.
//!
//! The table is deliberately small. Adding a vendor is one row plus a test; a
//! guessed price is worse than no price, because a wrong estimate is trusted.

use std::time::{SystemTime, UNIX_EPOCH};

/// One price list: what a million of each kind of token costs, in micro-USD.
///
/// Purpose: The three rates a vendor charges for one window (peak or off-peak).
/// Serves: `Price` and `rates_for`.
/// Design: `docs/design/cost-accounting.md` §3.
/// Up: the `DEEPSEEK_*` constants.
/// Down: `estimate`'s arithmetic.
/// Verify: `cargo test -p loong-config pricing::`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rates {
    /// Micro-USD per million cache-hit prompt tokens.
    pub cache_hit: u64,
    /// Micro-USD per million cache-miss prompt tokens.
    pub cache_miss: u64,
    /// Micro-USD per million generated tokens.
    pub output: u64,
}

/// A model's published price in both windows DeepSeek bills.
///
/// Purpose: One model's peak and off-peak rate cards.
/// Serves: `published` and `estimate`.
/// Design: `docs/design/cost-accounting.md` §3.
/// Up: the `DEEPSEEK_FLASH` and `DEEPSEEK_V4_PRO` constants.
/// Down: the window `estimate` selects.
/// Verify: `cargo test -p loong-config pricing::`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Price {
    pub peak: Rates,
    pub off_peak: Rates,
}

/// DeepSeek Flash, checked 2026-10-27: $0.006/$0.003 cache hit, $0.30/$0.15
/// cache miss, $1.20/$0.60 output per million, peak/off-peak.
const DEEPSEEK_FLASH: Price = Price {
    peak: Rates {
        cache_hit: 6_000,
        cache_miss: 300_000,
        output: 1_200_000,
    },
    off_peak: Rates {
        cache_hit: 3_000,
        cache_miss: 150_000,
        output: 600_000,
    },
};

/// DeepSeek V4 Pro, checked 2026-10-27: $0.044/$0.022 cache hit, $1.32/$0.66
/// cache miss, $3.96/$1.98 output per million, peak/off-peak.
const DEEPSEEK_V4_PRO: Price = Price {
    peak: Rates {
        cache_hit: 44_000,
        cache_miss: 1_320_000,
        output: 3_960_000,
    },
    off_peak: Rates {
        cache_hit: 22_000,
        cache_miss: 660_000,
        output: 1_980_000,
    },
};

/// How an estimate was reached, and therefore how much to trust it.
///
/// Purpose: The reason behind an estimated cost, stored on the event.
/// Serves: `Estimate` and the TUI's price label.
/// Design: `docs/design/cost-accounting.md` §2–§3.
/// Up: `estimate`.
/// Down: `Basis::as_str` and the `price_basis` field.
/// Verify: `cargo test -p loong-config pricing::`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Basis {
    /// A loopback endpoint or a catalog-local provider: no vendor bill exists.
    LocalFree,
    /// A published list price in DeepSeek's peak window.
    ListPeak,
    /// A published list price outside the peak window.
    ListOffPeak,
    /// The model or vendor is not in the table; the token counts are still real.
    Unknown,
}

impl Basis {
    /// Purpose: The wire value `cost.recorded.price_basis` carries.
    /// Serves: `Kernel::record_cost` and the TUI's label lookup.
    /// Design: `docs/design/cost-accounting.md` §2.
    /// Up: `Kernel::record_cost`.
    /// Down: the event payload; no other callee.
    /// Verify: `cargo test -p loong-config pricing::`.
    pub fn as_str(self) -> &'static str {
        match self {
            Basis::LocalFree => "local-free",
            Basis::ListPeak => "list-peak",
            Basis::ListOffPeak => "list-off-peak",
            Basis::Unknown => "unknown",
        }
    }
}

/// The answer to "what did this call cost?": micro-USD and its basis.
///
/// Purpose: One call's estimated cost with the reason it is trustworthy or not.
/// Serves: `Kernel::record_cost`.
/// Design: `docs/design/cost-accounting.md` §2–§3.
/// Up: `estimate`.
/// Down: the `usd_micros` and `price_basis` fields of `cost.recorded`.
/// Verify: `cargo test -p loong-config pricing::`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Estimate {
    pub usd_micros: u64,
    pub basis: Basis,
}

impl Estimate {
    /// Purpose: An estimate for a model the table cannot price, keeping the reason explicit.
    /// Serves: `estimate` and `Kernel::record_cost` when a model spec is missing.
    /// Design: `docs/design/cost-accounting.md` §3.
    /// Up: `estimate`.
    /// Down: no callee; it is a constructor.
    /// Verify: `cargo test -p loong-config pricing::`.
    pub fn unknown() -> Self {
        Self {
            usd_micros: 0,
            basis: Basis::Unknown,
        }
    }
}

/// Estimate one call from the provider's own counts and the time it happened.
///
/// The cache split is normalized so hit plus miss always equals `tokens_in`:
/// a provider that reports only hits has its miss derived, one that reports only
/// misses has its hit derived, and one that reports neither is treated as
/// all-miss. A local endpoint is free before any lookup.
///
/// Purpose: Turn one call's token counts into an estimated micro-USD and a basis.
/// Serves: `Kernel::record_cost`.
/// Design: `docs/design/cost-accounting.md` §3.
/// Up: the kernel after each successful model call.
/// Down: `is_local`, `published`, `is_peak` and `Estimate::unknown`.
/// Verify: `cargo test -p loong-config pricing::`.
#[allow(clippy::too_many_arguments)]
pub fn estimate(
    model: &str,
    provider: Option<&str>,
    base_url: &str,
    tokens_in: u64,
    cache_hit: Option<u64>,
    cache_miss: Option<u64>,
    tokens_out: u64,
    at: SystemTime,
) -> Estimate {
    if is_local(provider, base_url) {
        return Estimate {
            usd_micros: 0,
            basis: Basis::LocalFree,
        };
    }
    let Some(price) = published(model, provider, base_url) else {
        return Estimate::unknown();
    };
    // Keep the split honest even when a provider sends an inconsistent one.
    let hit = cache_hit
        .or_else(|| cache_miss.map(|miss| tokens_in.saturating_sub(miss)))
        .unwrap_or(0)
        .min(tokens_in);
    let miss = tokens_in.saturating_sub(hit);
    // One clock read, so the rate and the basis cannot disagree at a window edge.
    let peak = is_peak(at);
    let rates = if peak { price.peak } else { price.off_peak };
    let micros = (u128::from(hit) * u128::from(rates.cache_hit)
        + u128::from(miss) * u128::from(rates.cache_miss)
        + u128::from(tokens_out) * u128::from(rates.output)
        + 500_000)
        / 1_000_000;
    Estimate {
        usd_micros: u64::try_from(micros).unwrap_or(u64::MAX),
        basis: if peak {
            Basis::ListPeak
        } else {
            Basis::ListOffPeak
        },
    }
}

/// Whether this endpoint runs on the person's own machine.
///
/// A catalog provider marked `local` (Ollama) and any loopback host both count:
/// the computation happens where the person is, so there is no vendor to bill.
///
/// Purpose: Whether a model call is free because it ran locally.
/// Serves: `estimate`.
/// Design: `docs/design/cost-accounting.md` §3.
/// Up: `estimate`.
/// Down: `crate::catalog::provider` and `loopback_host`.
/// Verify: `cargo test -p loong-config pricing::`.
pub fn is_local(provider: Option<&str>, base_url: &str) -> bool {
    provider
        .and_then(crate::catalog::provider)
        .is_some_and(|known| known.local)
        || loopback_host(base_url).is_some()
}

/// The host part of a URL, when it names this machine.
///
/// Purpose: Recognize `localhost`, `127.0.0.1`, `0.0.0.0` and `::1` in a base URL.
/// Serves: `is_local`.
/// Design: `docs/design/cost-accounting.md` §3.
/// Up: `is_local`.
/// Down: no callee; it parses the string.
/// Verify: `cargo test -p loong-config pricing::`.
fn loopback_host(base_url: &str) -> Option<&str> {
    let rest = base_url
        .strip_prefix("http://")
        .or_else(|| base_url.strip_prefix("https://"))
        .unwrap_or(base_url);
    let authority = rest.split('/').next().unwrap_or("").rsplit('@').next()?;
    let host = if let Some(bracketed) = authority.strip_prefix('[') {
        bracketed.split(']').next()?
    } else {
        authority.split(':').next()?
    };
    match host {
        "localhost" | "0.0.0.0" | "::1" => Some(host),
        other if other.starts_with("127.") => Some(other),
        _ => None,
    }
}

/// Every model this table prices, by vendor and wire name.
///
/// Purpose: Look up a model's published price, if the vendor publishes one this build knows.
/// Serves: `estimate`.
/// Design: `docs/design/cost-accounting.md` §3.
/// Up: `estimate`.
/// Down: the two DeepSeek constants and the vendor check.
/// Verify: `cargo test -p loong-config pricing::`.
fn published(model: &str, provider: Option<&str>, base_url: &str) -> Option<&'static Price> {
    let deepseek = provider == Some("deepseek") || base_url.contains("api.deepseek.com");
    if !deepseek {
        return None;
    }
    match model {
        "deepseek-flash" | "deepseek-v4-flash" | "deepseek-v4-flash-vision-exp" => {
            Some(&DEEPSEEK_FLASH)
        }
        "deepseek-v4-pro" | "deepseek-pro" => Some(&DEEPSEEK_V4_PRO),
        _ => None,
    }
}

/// Whether a time falls in DeepSeek's peak window: 01:00–04:00 and 06:00–10:00 UTC,
/// Monday through Friday.
///
/// Chinese public holidays are not modeled, so a holiday hour inside a peak
/// window is estimated at the peak price — the conservative direction.
///
/// Purpose: Choose the peak or off-peak half of a price list for one instant.
/// Serves: `estimate`.
/// Design: `docs/design/cost-accounting.md` §3.
/// Up: `estimate`.
/// Down: `SystemTime` and the UTC clock arithmetic.
/// Verify: `cargo test -p loong-config pricing::`.
pub fn is_peak(at: SystemTime) -> bool {
    let seconds = at
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    let day = seconds / 86_400;
    // 1970-01-01 was a Thursday: (days + 4) % 7 gives 0 = Sunday.
    let weekday = (day + 4) % 7;
    let hour = (seconds % 86_400) / 3_600;
    (1..=5).contains(&weekday) && ((1..4).contains(&hour) || (6..10).contains(&hour))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Purpose: A fixed instant from a UTC epoch second.
    /// Contract: test helper for the peak-window cases.
    fn at(seconds: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(seconds)
    }

    /// Purpose: Pin DeepSeek's peak windows against a Tuesday and a Sunday.
    #[test]
    fn peak_is_a_weekday_utc_window() {
        // 2026-10-27 is a Tuesday, 2026-10-25 is a Sunday.
        assert!(is_peak(at(1_793_066_400)), "Tuesday 02:00 UTC is peak");
        assert!(!is_peak(at(1_793_097_600)), "Tuesday 10:40 UTC is off-peak");
        assert!(!is_peak(at(1_792_893_600)), "Sunday 02:00 UTC is off-peak");
    }

    /// Purpose: Pin that a local endpoint is free and needs no price row.
    #[test]
    fn a_loopback_or_catalog_local_endpoint_is_free() {
        for base in [
            "http://127.0.0.1:11434/v1",
            "http://localhost:8000/v1",
            "https://[::1]:8443/v1",
            "http://0.0.0.0:8000/v1",
        ] {
            let estimate = estimate(
                "qwen2.5:7b-instruct",
                Some("ollama"),
                base,
                1_000,
                Some(0),
                Some(1_000),
                100,
                at(1_793_066_400),
            );
            assert_eq!(estimate.basis, Basis::LocalFree, "{base}");
            assert_eq!(estimate.usd_micros, 0, "{base}");
        }
        assert!(is_local(Some("ollama"), "https://api.example.com/v1"));
        assert!(!is_local(Some("deepseek"), "https://api.deepseek.com"));
    }

    /// Purpose: Pin the Flash arithmetic at peak, at off-peak, and with an unreported cache.
    #[test]
    fn flash_is_priced_from_the_three_rates() {
        // Tuesday 02:00 UTC: 800 cached at $0.006/M, 200 miss at $0.30/M,
        // 100 out at $1.20/M = $0.0001848, rounded to 185 micro-USD.
        let peak = estimate(
            "deepseek-flash",
            Some("deepseek"),
            "https://api.deepseek.com",
            1_000,
            Some(800),
            Some(200),
            100,
            at(1_793_066_400),
        );
        assert_eq!(peak.usd_micros, 185, "{peak:?}");
        assert_eq!(peak.basis, Basis::ListPeak);

        // The same call outside the window is half price: 92.4 micro-USD rounds to 92.
        let off = estimate(
            "deepseek-flash",
            Some("deepseek"),
            "https://api.deepseek.com",
            1_000,
            Some(800),
            Some(200),
            100,
            at(1_793_097_600),
        );
        assert_eq!(off.usd_micros, 92, "{off:?}");
        assert_eq!(off.basis, Basis::ListOffPeak);

        // No cache report means every prompt token is billed as a miss.
        let unreported = estimate(
            "deepseek-flash",
            Some("deepseek"),
            "https://api.deepseek.com",
            1_000,
            None,
            None,
            100,
            at(1_793_066_400),
        );
        assert_eq!(unreported.usd_micros, 420, "{unreported:?}");
    }

    /// Purpose: Pin that an unknown vendor or model keeps its tokens but not an invented price.
    #[test]
    fn an_unpriced_remote_model_is_unknown() {
        let unknown = estimate(
            "qwen3.7-flash",
            Some("dashscope"),
            "https://dashscope.aliyuncs.com/compatible-mode/v1",
            1_000,
            Some(800),
            Some(200),
            100,
            at(1_793_066_400),
        );
        assert_eq!(unknown.basis, Basis::Unknown);
        assert_eq!(unknown.usd_micros, 0);

        let unknown_model = estimate(
            "deepseek-something-new",
            Some("deepseek"),
            "https://api.deepseek.com",
            1_000,
            Some(800),
            Some(200),
            100,
            at(1_793_066_400),
        );
        assert_eq!(unknown_model.basis, Basis::Unknown);
    }
}
