//! Parsers for the `x-ratelimit-*` header family used by OpenAI-compatible APIs
//! (xAI, Kimi and others).

use crate::LimitGauge;
use http::HeaderMap;
use std::time::Duration;

const BUCKETS: [(&str, &str); 2] = [("requests", "req"), ("tokens", "tok")];

/// Reads `x-ratelimit-{limit,remaining,reset}-{requests,tokens}` into gauges.
///
/// A bucket is skipped unless both its limit and remaining counts are present.
pub fn parse_x_ratelimit_headers(headers: &HeaderMap, now: i64) -> Vec<LimitGauge> {
    BUCKETS
        .iter()
        .filter_map(|(bucket, label)| {
            let limit = header_f64(headers, &format!("x-ratelimit-limit-{bucket}"))?;
            let remaining = header_f64(headers, &format!("x-ratelimit-remaining-{bucket}"))?;
            if limit <= 0.0 {
                return None;
            }
            let used_percent = ((limit - remaining) / limit * 100.0).clamp(0.0, 100.0);
            let resets_at = header_str(headers, &format!("x-ratelimit-reset-{bucket}"))
                .and_then(parse_reset_duration)
                .map(|reset| now + reset.as_secs() as i64);
            Some(LimitGauge {
                label: (*label).to_string(),
                used_percent,
                resets_at,
            })
        })
        .collect()
}

/// Parses reset values such as `"20ms"`, `"1.5s"`, `"6m0s"`, `"1h2m3s"` or bare seconds.
pub fn parse_reset_duration(raw: &str) -> Option<Duration> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    if let Ok(seconds) = raw.parse::<f64>() {
        return (seconds.is_finite() && seconds >= 0.0).then(|| Duration::from_secs_f64(seconds));
    }

    let mut total = 0.0;
    let mut number = String::new();
    let mut chars = raw.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch.is_ascii_digit() || ch == '.' {
            number.push(ch);
            continue;
        }
        let value: f64 = number.parse().ok()?;
        number.clear();
        let unit_seconds = match ch {
            'h' => 3600.0,
            'm' if chars.peek() == Some(&'s') => {
                chars.next();
                0.001
            }
            'm' => 60.0,
            's' => 1.0,
            _ => return None,
        };
        total += value * unit_seconds;
    }
    if !number.is_empty() {
        return None;
    }
    Some(Duration::from_secs_f64(total))
}

fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name)?.to_str().ok()
}

fn header_f64(headers: &HeaderMap, name: &str) -> Option<f64> {
    header_str(headers, name)?
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite())
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::HeaderValue;
    use pretty_assertions::assert_eq;

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(*name, HeaderValue::from_static(value));
        }
        map
    }

    #[test]
    fn parses_both_buckets() {
        let map = headers(&[
            ("x-ratelimit-limit-requests", "100"),
            ("x-ratelimit-remaining-requests", "75"),
            ("x-ratelimit-reset-requests", "30s"),
            ("x-ratelimit-limit-tokens", "1000"),
            ("x-ratelimit-remaining-tokens", "900"),
            ("x-ratelimit-reset-tokens", "6m0s"),
        ]);
        assert_eq!(
            parse_x_ratelimit_headers(&map, 1_000),
            vec![
                LimitGauge {
                    label: "req".to_string(),
                    used_percent: 25.0,
                    resets_at: Some(1_030),
                },
                LimitGauge {
                    label: "tok".to_string(),
                    used_percent: 10.0,
                    resets_at: Some(1_360),
                },
            ]
        );
    }

    #[test]
    fn skips_incomplete_buckets() {
        let map = headers(&[("x-ratelimit-limit-requests", "100")]);
        assert_eq!(parse_x_ratelimit_headers(&map, 0), Vec::new());
    }

    #[test]
    fn parses_reset_formats() {
        assert_eq!(
            parse_reset_duration("20ms"),
            Some(Duration::from_millis(20))
        );
        assert_eq!(
            parse_reset_duration("1.5s"),
            Some(Duration::from_millis(1500))
        );
        assert_eq!(
            parse_reset_duration("1h2m3s"),
            Some(Duration::from_secs(3723))
        );
        assert_eq!(parse_reset_duration("42"), Some(Duration::from_secs(42)));
        assert_eq!(parse_reset_duration("5x"), None);
        assert_eq!(parse_reset_duration("12m3"), None);
    }
}
