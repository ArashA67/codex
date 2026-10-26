//! Per-provider usage limits shared between Codex, council delegates, and the TUI.
//!
//! Each provider has one JSON file under `$CODEX_HOME/limits/`. Writers replace the
//! whole file atomically, so a reader never sees a partial entry. Several processes
//! may write (the agent loop, a delegate CLI wrapper, a Claude Code status-line
//! script), and the last write wins for a given provider.

mod headers;
mod store;
mod summary;

pub use headers::parse_reset_duration;
pub use headers::parse_x_ratelimit_headers;
pub use store::LimitsStore;
pub use summary::format_compact_summary;
pub use summary::local_day;

use serde::Deserialize;
use serde::Serialize;

/// How much the displayed numbers can be trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LimitQuality {
    /// Reported by the provider on the latest request.
    Exact,
    /// Reported by the provider, but captured outside this session and possibly stale.
    LastSeen,
    /// The provider exposes nothing; only locally counted usage is known.
    LocalOnly,
}

/// One quota window or bucket, such as "5h", "week" or "tokens/min".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LimitGauge {
    pub label: String,
    /// Share of the bucket already consumed, from 0 to 100.
    pub used_percent: f64,
    /// Unix seconds when the bucket refills.
    pub resets_at: Option<i64>,
}

/// Everything known about one provider's limits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderLimits {
    pub provider_id: String,
    pub display_name: String,
    pub quality: LimitQuality,
    #[serde(default)]
    pub gauges: Vec<LimitGauge>,
    /// Prepaid balance shown as-is, for example `"$8.40"`.
    #[serde(default)]
    pub balance: Option<String>,
    /// Local day (`YYYY-MM-DD`) that the counters below belong to.
    #[serde(default)]
    pub usage_day: Option<String>,
    #[serde(default)]
    pub tokens_today: u64,
    #[serde(default)]
    pub requests_today: u64,
    /// Unix seconds until which the provider asked us to stop (HTTP 429 or a CLI event).
    #[serde(default)]
    pub rate_limited_until: Option<i64>,
    /// Unix seconds of the last write.
    pub updated_at: i64,
}

impl ProviderLimits {
    pub fn new(provider_id: &str, display_name: &str, now: i64) -> Self {
        Self {
            provider_id: provider_id.to_string(),
            display_name: display_name.to_string(),
            quality: LimitQuality::LocalOnly,
            gauges: Vec::new(),
            balance: None,
            usage_day: None,
            tokens_today: 0,
            requests_today: 0,
            rate_limited_until: None,
            updated_at: now,
        }
    }

    /// Adds one finished request to today's counters, resetting them on a new day.
    pub fn record_request(&mut self, tokens: u64, today: &str) {
        if self.usage_day.as_deref() != Some(today) {
            self.usage_day = Some(today.to_string());
            self.tokens_today = 0;
            self.requests_today = 0;
        }
        self.tokens_today = self.tokens_today.saturating_add(tokens);
        self.requests_today = self.requests_today.saturating_add(1);
    }

    /// Highest consumed share across gauges, used to color and route.
    pub fn max_used_percent(&self) -> Option<f64> {
        self.gauges
            .iter()
            .map(|gauge| gauge.used_percent)
            .filter(|percent| percent.is_finite())
            .max_by(f64::total_cmp)
    }

    pub fn is_rate_limited(&self, now: i64) -> bool {
        self.rate_limited_until.is_some_and(|until| until > now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn record_request_resets_on_new_day() {
        let mut limits = ProviderLimits::new("kimi-code", "Kimi", 0);
        limits.record_request(100, "2026-10-26");
        limits.record_request(50, "2026-10-26");
        assert_eq!((limits.tokens_today, limits.requests_today), (150, 2));

        limits.record_request(7, "2026-10-27");
        assert_eq!((limits.tokens_today, limits.requests_today), (7, 1));
        assert_eq!(limits.usage_day.as_deref(), Some("2026-10-27"));
    }

    #[test]
    fn max_used_percent_ignores_non_finite_values() {
        let mut limits = ProviderLimits::new("xai", "Grok", 0);
        limits.gauges = vec![
            LimitGauge {
                label: "req".to_string(),
                used_percent: f64::NAN,
                resets_at: None,
            },
            LimitGauge {
                label: "tok".to_string(),
                used_percent: 42.0,
                resets_at: None,
            },
        ];
        assert_eq!(limits.max_used_percent(), Some(42.0));
    }
}
