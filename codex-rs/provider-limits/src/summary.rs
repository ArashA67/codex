use crate::LimitQuality;
use crate::ProviderLimits;
use chrono::DateTime;
use chrono::Local;

/// Entries older than this are marked with their age.
const STALE_AFTER_SECS: i64 = 120;
/// Last-seen values come from outside this session, so their age shows sooner.
const LAST_SEEN_AGE_AFTER_SECS: i64 = 60;

/// One line such as `Claude 5h:58% wk:81% (12m ago) │ DeepSeek $8.40 │ Kimi 1.2M tok`.
///
/// Gauge percentages are what is left, matching Codex's own limit display.
pub fn format_compact_summary(entries: &[ProviderLimits], now: i64) -> Option<String> {
    let parts: Vec<String> = entries
        .iter()
        .filter_map(|limits| format_entry(limits, now))
        .collect();
    (!parts.is_empty()).then(|| parts.join(" │ "))
}

fn format_entry(limits: &ProviderLimits, now: i64) -> Option<String> {
    let mut fields = Vec::new();
    if limits.is_rate_limited(now) {
        let until = limits.rate_limited_until.unwrap_or(now);
        fields.push(format!("limited until {}", format_clock(until)));
    } else {
        for gauge in &limits.gauges {
            let left = (100.0 - gauge.used_percent).clamp(0.0, 100.0);
            fields.push(format!("{}:{left:.0}%", gauge.label));
        }
    }
    if let Some(balance) = &limits.balance {
        fields.push(balance.clone());
    }
    let counted_today = limits.usage_day.as_deref() == Some(&local_day(now));
    if fields.is_empty() && counted_today {
        if limits.tokens_today > 0 {
            fields.push(format!("{} tok today", format_count(limits.tokens_today)));
        } else if limits.requests_today > 0 {
            let runs = limits.requests_today;
            let noun = if runs == 1 { "run" } else { "runs" };
            fields.push(format!("{runs} {noun} today"));
        }
    }
    if fields.is_empty() {
        return None;
    }
    let age = now - limits.updated_at;
    let stale_after = match limits.quality {
        LimitQuality::LastSeen => LAST_SEEN_AGE_AFTER_SECS,
        LimitQuality::Exact | LimitQuality::LocalOnly => STALE_AFTER_SECS,
    };
    if age >= stale_after {
        fields.push(format!("({} ago)", format_age(age)));
    }
    Some(format!("{} {}", limits.display_name, fields.join(" ")))
}

/// Local calendar day for `now`, as stored in [`ProviderLimits::usage_day`].
pub fn local_day(now: i64) -> String {
    local_time(now).format("%Y-%m-%d").to_string()
}

fn local_time(unix: i64) -> DateTime<Local> {
    DateTime::from_timestamp(unix, 0)
        .unwrap_or_default()
        .with_timezone(&Local)
}

fn format_clock(unix: i64) -> String {
    local_time(unix).format("%H:%M").to_string()
}

fn format_age(seconds: i64) -> String {
    let seconds = seconds.max(0);
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3600 => format!("{}m", seconds / 60),
        3600..86_400 => format!("{}h", seconds / 3600),
        _ => format!("{}d", seconds / 86_400),
    }
}

fn format_count(count: u64) -> String {
    match count {
        0..1_000 => count.to_string(),
        1_000..1_000_000 => format!("{:.1}k", count as f64 / 1_000.0),
        _ => format!("{:.1}M", count as f64 / 1_000_000.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LimitGauge;
    use pretty_assertions::assert_eq;

    const NOW: i64 = 1_800_000_000;

    fn entry(provider_id: &str, name: &str) -> ProviderLimits {
        ProviderLimits::new(provider_id, name, NOW)
    }

    #[test]
    fn formats_each_kind_of_entry() {
        let mut claude = entry("claude-code", "Claude");
        claude.quality = LimitQuality::LastSeen;
        claude.updated_at = NOW - 720;
        claude.gauges = vec![
            LimitGauge {
                label: "5h".to_string(),
                used_percent: 42.0,
                resets_at: None,
            },
            LimitGauge {
                label: "wk".to_string(),
                used_percent: 19.0,
                resets_at: None,
            },
        ];

        let mut deepseek = entry("deepseek", "DeepSeek");
        deepseek.quality = LimitQuality::Exact;
        deepseek.balance = Some("$8.40".to_string());

        let mut kimi = entry("kimi-code", "Kimi");
        kimi.record_request(1_234_567, &local_day(NOW));

        let mut grok = entry("grok-build", "Grok");
        grok.record_request(0, &local_day(NOW));
        grok.record_request(0, &local_day(NOW));

        let silent = entry("xai", "xAI");

        assert_eq!(
            format_compact_summary(&[claude, deepseek, kimi, grok, silent], NOW),
            Some(
                "Claude 5h:58% wk:81% (12m ago) │ DeepSeek $8.40 │ Kimi 1.2M tok today │ Grok 2 runs today"
                    .to_string()
            )
        );
    }

    #[test]
    fn rate_limited_entry_replaces_gauges() {
        let mut grok = entry("xai", "Grok");
        grok.gauges = vec![LimitGauge {
            label: "req".to_string(),
            used_percent: 100.0,
            resets_at: None,
        }];
        grok.rate_limited_until = Some(NOW + 600);
        let summary = format_compact_summary(&[grok], NOW).expect("summary");
        assert!(summary.starts_with("Grok limited until "), "{summary}");
    }

    #[test]
    fn empty_input_has_no_summary() {
        assert_eq!(format_compact_summary(&[], NOW), None);
    }
}
