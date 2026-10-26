//! Parsers for delegate CLI output.

use serde_json::Value;

/// What a delegate run produced.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DelegateOutcome {
    /// Final answer or summary.
    pub text: String,
    pub session_id: Option<String>,
    pub is_error: bool,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Unix seconds when a subscription cap resets, if the run hit one.
    pub rate_limited_until: Option<i64>,
}

impl DelegateOutcome {
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens.saturating_add(self.output_tokens)
    }
}

/// Parses Claude Code `--output-format stream-json` output.
pub fn parse_claude_stream(stdout: &str) -> DelegateOutcome {
    let mut outcome = DelegateOutcome::default();
    let mut last_assistant_text = String::new();
    for line in stdout.lines() {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        match event.get("type").and_then(Value::as_str) {
            Some("assistant") => {
                let text = assistant_text(&event);
                if !text.is_empty() {
                    last_assistant_text = text;
                }
            }
            Some("rate_limit_event") => {
                let info = event.get("rate_limit_info");
                let status = info
                    .and_then(|info| info.get("status"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if status == "rate_limited" || status == "rejected" {
                    outcome.rate_limited_until = info
                        .and_then(|info| info.get("resetsAt"))
                        .and_then(Value::as_i64)
                        .or(outcome.rate_limited_until);
                }
            }
            Some("result") => {
                if let Some(text) = event.get("result").and_then(Value::as_str) {
                    outcome.text = text.to_string();
                }
                outcome.is_error = event
                    .get("is_error")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                read_usage(event.get("usage"), &mut outcome);
            }
            _ => {}
        }
        if outcome.session_id.is_none() {
            outcome.session_id = event
                .get("session_id")
                .and_then(Value::as_str)
                .map(str::to_string);
        }
    }
    if outcome.text.is_empty() {
        outcome.text = last_assistant_text;
    }
    outcome
}

fn assistant_text(event: &Value) -> String {
    event
        .pointer("/message/content")
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|block| block.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

/// Parses Grok Build `--output-format json` output.
///
/// The JSON shape is not documented, so this accepts the common field names and
/// falls back to the raw output.
pub fn parse_grok_output(stdout: &str) -> DelegateOutcome {
    let trimmed = stdout.trim();
    let Some(value) = last_json_object(trimmed) else {
        return DelegateOutcome {
            text: trimmed.to_string(),
            ..Default::default()
        };
    };
    let mut outcome = DelegateOutcome {
        text: ["result", "response", "text", "output", "message", "content"]
            .iter()
            .find_map(|key| value.get(*key).and_then(Value::as_str))
            .unwrap_or(trimmed)
            .to_string(),
        session_id: ["session_id", "sessionId", "session"]
            .iter()
            .find_map(|key| value.get(*key).and_then(Value::as_str))
            .map(str::to_string),
        is_error: value
            .get("is_error")
            .or_else(|| value.get("error").filter(|error| !error.is_null()))
            .is_some_and(|error| error.as_bool().unwrap_or(true)),
        ..Default::default()
    };
    read_usage(value.get("usage"), &mut outcome);
    outcome
}

fn last_json_object(output: &str) -> Option<Value> {
    if let Ok(value) = serde_json::from_str::<Value>(output) {
        return value.is_object().then_some(value);
    }
    output
        .lines()
        .rev()
        .filter_map(|line| serde_json::from_str::<Value>(line.trim()).ok())
        .find(Value::is_object)
}

fn read_usage(usage: Option<&Value>, outcome: &mut DelegateOutcome) {
    let Some(usage) = usage else {
        return;
    };
    let field = |names: &[&str]| {
        names
            .iter()
            .filter_map(|name| usage.get(*name).and_then(Value::as_u64))
            .sum::<u64>()
    };
    outcome.input_tokens = field(&[
        "input_tokens",
        "prompt_tokens",
        "cache_read_input_tokens",
        "cache_creation_input_tokens",
    ]);
    outcome.output_tokens = field(&["output_tokens", "completion_tokens"]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn parses_claude_result_usage_and_rate_limit() {
        let stdout = [
            r#"{"type":"system","subtype":"init","session_id":"s-1"}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Looking..."}]},"session_id":"s-1"}"#,
            r#"{"type":"rate_limit_event","rate_limit_info":{"status":"rate_limited","resetsAt":1800000000,"rateLimitType":"five_hour"},"session_id":"s-1"}"#,
            r#"{"type":"result","subtype":"success","is_error":false,"result":"Plan: 3 tasks","session_id":"s-1","usage":{"input_tokens":100,"cache_read_input_tokens":20,"output_tokens":30}}"#,
        ]
        .join("\n");
        assert_eq!(
            parse_claude_stream(&stdout),
            DelegateOutcome {
                text: "Plan: 3 tasks".to_string(),
                session_id: Some("s-1".to_string()),
                is_error: false,
                input_tokens: 120,
                output_tokens: 30,
                rate_limited_until: Some(1_800_000_000),
            }
        );
    }

    #[test]
    fn claude_falls_back_to_last_assistant_text() {
        let stdout = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"partial answer"}]}}"#;
        assert_eq!(parse_claude_stream(stdout).text, "partial answer");
    }

    #[test]
    fn allowed_rate_limit_status_is_not_a_limit() {
        let stdout =
            r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed","resetsAt":5}}"#;
        assert_eq!(parse_claude_stream(stdout).rate_limited_until, None);
    }

    #[test]
    fn parses_grok_json_and_raw_text() {
        let json = r#"{"result":"LGTM with nits","session_id":"g-9","usage":{"prompt_tokens":10,"completion_tokens":5}}"#;
        let outcome = parse_grok_output(json);
        assert_eq!(outcome.text, "LGTM with nits");
        assert_eq!(outcome.session_id.as_deref(), Some("g-9"));
        assert_eq!(outcome.total_tokens(), 15);
        assert!(!outcome.is_error);

        assert_eq!(parse_grok_output("plain words\n").text, "plain words");
    }
}
