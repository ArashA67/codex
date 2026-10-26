//! Limit sources that live outside the Codex agent loop.

use crate::delegate::DelegateKind;
use crate::events::DelegateOutcome;
use codex_provider_limits::LimitGauge;
use codex_provider_limits::LimitQuality;
use codex_provider_limits::LimitsStore;
use codex_provider_limits::ProviderLimits;
use codex_provider_limits::local_day;
use serde::Deserialize;
use std::io;

pub const DEEPSEEK_PROVIDER_ID: &str = "deepseek";
const DEEPSEEK_BALANCE_URL: &str = "https://api.deepseek.com/user/balance";

pub fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Counts a finished delegate run and records any subscription cap it hit.
pub fn record_delegate_outcome(
    store: &LimitsStore,
    kind: DelegateKind,
    outcome: &DelegateOutcome,
    now: i64,
) -> io::Result<ProviderLimits> {
    store.update(kind.provider_id(), kind.display_name(), now, |limits| {
        limits.record_request(outcome.total_tokens(), &local_day(now));
        if let Some(until) = outcome.rate_limited_until {
            limits.rate_limited_until = Some(until);
        }
    })
}

#[derive(Debug, Deserialize)]
struct StatusLineInput {
    rate_limits: Option<StatusLineRateLimits>,
}

#[derive(Debug, Deserialize)]
struct StatusLineRateLimits {
    five_hour: Option<StatusLineWindow>,
    seven_day: Option<StatusLineWindow>,
}

#[derive(Debug, Deserialize)]
struct StatusLineWindow {
    used_percentage: Option<f64>,
    resets_at: Option<i64>,
}

/// Handles one Claude Code status-line refresh.
///
/// Claude Code pipes a JSON object to its configured status-line command, and for
/// Pro and Max plans that object carries `rate_limits.five_hour` and
/// `rate_limits.seven_day`. This records them as last-seen values and returns the
/// text Claude Code should show in its own status line.
pub fn handle_claude_status_line(
    store: &LimitsStore,
    input: &str,
    now: i64,
) -> anyhow::Result<String> {
    let parsed: StatusLineInput = serde_json::from_str(input)?;
    let gauges: Vec<LimitGauge> = parsed
        .rate_limits
        .map(|limits| {
            [("5h", limits.five_hour), ("wk", limits.seven_day)]
                .into_iter()
                .filter_map(|(label, window)| {
                    let window = window?;
                    Some(LimitGauge {
                        label: label.to_string(),
                        used_percent: window.used_percentage?,
                        resets_at: window.resets_at,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    if gauges.is_empty() {
        return Ok(String::new());
    }
    let kind = DelegateKind::ClaudeCode;
    let display = gauges
        .iter()
        .map(|gauge| {
            let left = (100.0 - gauge.used_percent).clamp(0.0, 100.0);
            format!("{}:{left:.0}%", gauge.label)
        })
        .collect::<Vec<_>>()
        .join(" ");
    store.update(
        kind.provider_id(),
        kind.display_name(),
        now,
        move |limits| {
            limits.quality = LimitQuality::LastSeen;
            limits.gauges = gauges;
            if limits.rate_limited_until.is_some_and(|until| until <= now) {
                limits.rate_limited_until = None;
            }
        },
    )?;
    Ok(format!("{} {display} left", kind.display_name()))
}

#[derive(Debug, Deserialize)]
struct DeepSeekBalance {
    #[serde(default)]
    balance_infos: Vec<DeepSeekBalanceInfo>,
}

#[derive(Debug, Deserialize)]
struct DeepSeekBalanceInfo {
    currency: String,
    total_balance: String,
}

/// Fetches the DeepSeek account balance with the user's API key and stores it.
pub async fn refresh_deepseek_balance(
    store: &LimitsStore,
    api_key: &str,
) -> anyhow::Result<ProviderLimits> {
    let response = reqwest::Client::new()
        .get(DEEPSEEK_BALANCE_URL)
        .bearer_auth(api_key)
        .send()
        .await?
        .error_for_status()?;
    let balance: DeepSeekBalance = response.json().await?;
    let formatted = format_deepseek_balance(&balance);
    let now = now();
    Ok(
        store.update(DEEPSEEK_PROVIDER_ID, "DeepSeek", now, move |limits| {
            limits.quality = LimitQuality::Exact;
            limits.balance = formatted;
        })?,
    )
}

fn format_deepseek_balance(balance: &DeepSeekBalance) -> Option<String> {
    let parts: Vec<String> = balance
        .balance_infos
        .iter()
        .map(|info| match info.currency.as_str() {
            "USD" => format!("${}", info.total_balance),
            "CNY" => format!("¥{}", info.total_balance),
            other => format!("{} {other}", info.total_balance),
        })
        .collect();
    (!parts.is_empty()).then(|| parts.join(" + "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn claude_status_line_records_last_seen_windows() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = LimitsStore::new(dir.path().to_path_buf());
        let input = r#"{"model":{"display_name":"Opus"},"rate_limits":{"five_hour":{"used_percentage":42,"resets_at":1800003600},"seven_day":{"used_percentage":19.4}}}"#;

        let line = handle_claude_status_line(&store, input, 1_800_000_000).expect("handled");
        assert_eq!(line, "Claude 5h:58% wk:81% left");

        let saved = store.load("claude-code").expect("saved");
        assert_eq!(saved.quality, LimitQuality::LastSeen);
        assert_eq!(
            saved.gauges,
            vec![
                LimitGauge {
                    label: "5h".to_string(),
                    used_percent: 42.0,
                    resets_at: Some(1_800_003_600),
                },
                LimitGauge {
                    label: "wk".to_string(),
                    used_percent: 19.4,
                    resets_at: None,
                },
            ]
        );
    }

    #[test]
    fn claude_status_line_without_limits_writes_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = LimitsStore::new(dir.path().to_path_buf());
        assert_eq!(handle_claude_status_line(&store, "{}", 0).expect("ok"), "");
        assert_eq!(store.load("claude-code"), None);
    }

    #[test]
    fn delegate_outcome_counts_tokens_and_caps() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = LimitsStore::new(dir.path().to_path_buf());
        let outcome = DelegateOutcome {
            input_tokens: 10,
            output_tokens: 5,
            rate_limited_until: Some(99),
            ..Default::default()
        };
        let saved =
            record_delegate_outcome(&store, DelegateKind::GrokBuild, &outcome, 1).expect("saved");
        assert_eq!(saved.provider_id, "grok-build");
        assert_eq!(saved.tokens_today, 15);
        assert_eq!(saved.rate_limited_until, Some(99));
    }

    #[test]
    fn formats_deepseek_balances() {
        let balance: DeepSeekBalance = serde_json::from_str(
            r#"{"is_available":true,"balance_infos":[{"currency":"USD","total_balance":"8.40","granted_balance":"0.00","topped_up_balance":"8.40"}]}"#,
        )
        .expect("parse");
        assert_eq!(format_deepseek_balance(&balance), Some("$8.40".to_string()));
    }
}
