//! Minimal MCP stdio server that exposes council delegates as tools.
//!
//! Codex (or any MCP client) launches `codex council mcp`, and the model can then
//! call `ask_claude`, `ask_grok` and `provider_limits` like any other tool.

use crate::delegate::DEFAULT_TIMEOUT;
use crate::delegate::DelegateKind;
use crate::delegate::DelegateMode;
use crate::delegate::DelegateRequest;
use crate::delegate::run_delegate;
use crate::limits::now;
use crate::limits::record_delegate_outcome;
use crate::limits::refresh_deepseek_balance;
use codex_provider_limits::LimitsStore;
use codex_provider_limits::format_compact_summary;
use serde::Deserialize;
use serde_json::Value;
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncWriteExt;
use tokio::io::BufReader;
use tokio::sync::mpsc;

const SERVER_NAME: &str = "codex-council";
const DEFAULT_PROTOCOL_VERSION: &str = "2025-06-18";
const DEEPSEEK_BALANCE_REFRESH: Duration = Duration::from_secs(5 * 60);

#[derive(Debug, Clone)]
pub struct McpServerOptions {
    pub store: LimitsStore,
    /// Working directory for delegates when a call does not name one.
    pub default_cwd: PathBuf,
}

/// Serves MCP over stdin/stdout until stdin closes.
pub async fn serve_stdio(options: McpServerOptions) -> anyhow::Result<()> {
    let options = Arc::new(options);
    spawn_deepseek_balance_refresh(options.store.clone());

    let (tx, mut rx) = mpsc::unbounded_channel::<Value>();
    let writer = tokio::spawn(async move {
        let mut stdout = tokio::io::stdout();
        while let Some(message) = rx.recv().await {
            let mut line = message.to_string();
            line.push('\n');
            if stdout.write_all(line.as_bytes()).await.is_err() {
                break;
            }
            let _ = stdout.flush().await;
        }
    });

    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    while let Some(line) = lines.next_line().await? {
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let options = Arc::clone(&options);
        let tx = tx.clone();
        tokio::spawn(async move {
            if let Some(response) = handle_message(&options, message).await {
                let _ = tx.send(response);
            }
        });
    }
    drop(tx);
    let _ = writer.await;
    Ok(())
}

fn spawn_deepseek_balance_refresh(store: LimitsStore) {
    let Some(api_key) = std::env::var("DEEPSEEK_API_KEY")
        .ok()
        .filter(|key| !key.trim().is_empty())
    else {
        return;
    };
    tokio::spawn(async move {
        loop {
            if let Err(err) = refresh_deepseek_balance(&store, &api_key).await {
                tracing::debug!("DeepSeek balance refresh failed: {err}");
            }
            tokio::time::sleep(DEEPSEEK_BALANCE_REFRESH).await;
        }
    });
}

/// Returns the JSON-RPC response for a request, or `None` for notifications.
pub async fn handle_message(options: &McpServerOptions, message: Value) -> Option<Value> {
    let id = message.get("id").cloned()?;
    let method = message.get("method").and_then(Value::as_str).unwrap_or("");
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    let result = match method {
        "initialize" => Ok(initialize_result(&params)),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tool_definitions() })),
        "tools/call" => Ok(call_tool(options, &params).await),
        _ => Err((-32601, format!("method not found: {method}"))),
    };
    Some(match result {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err((code, message)) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": code, "message": message },
        }),
    })
}

fn initialize_result(params: &Value) -> Value {
    let protocol_version = params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .unwrap_or(DEFAULT_PROTOCOL_VERSION);
    json!({
        "protocolVersion": protocol_version,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": SERVER_NAME, "version": env!("CARGO_PKG_VERSION") },
        "instructions": "Council delegates. ask_claude and ask_grok run the user's own Claude Code and Grok Build on their subscriptions; use them for planning, review and self-contained features. Call provider_limits before picking a delegate.",
    })
}

fn delegate_input_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "prompt": { "type": "string", "description": "The full task. The delegate cannot see this conversation, so include every detail it needs." },
            "mode": { "type": "string", "enum": ["read-only", "edit"], "description": "read-only (default) answers without changing files; edit may change files in cwd." },
            "role": { "type": "string", "description": "Role instructions, for example the council reviewer brief." },
            "cwd": { "type": "string", "description": "Absolute working directory. Use a separate git worktree for edit mode." },
            "model": { "type": "string", "description": "Optional model override for the delegate CLI." },
            "timeout_minutes": { "type": "integer", "minimum": 1, "maximum": 120 }
        },
        "required": ["prompt"],
        "additionalProperties": false
    })
}

fn tool_definitions() -> Value {
    json!([
        {
            "name": "ask_claude",
            "description": "Delegate a task to Claude through the user's Claude Code (Claude subscription). Best for planning, architecture and careful review. Returns Claude's final answer.",
            "inputSchema": delegate_input_schema(),
        },
        {
            "name": "ask_grok",
            "description": "Delegate a task to Grok through the user's Grok Build CLI (SuperGrok subscription). Good for independent review and second opinions. Returns Grok's final answer.",
            "inputSchema": delegate_input_schema(),
        },
        {
            "name": "provider_limits",
            "description": "Show usage left for every provider (GPT, Claude, Kimi, DeepSeek, Grok) so work can be routed away from providers near their caps.",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
        }
    ])
}

#[derive(Debug, Deserialize)]
struct DelegateArgs {
    prompt: String,
    #[serde(default)]
    mode: DelegateMode,
    role: Option<String>,
    cwd: Option<PathBuf>,
    model: Option<String>,
    timeout_minutes: Option<u64>,
}

async fn call_tool(options: &McpServerOptions, params: &Value) -> Value {
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
    let kind = match name {
        "ask_claude" => DelegateKind::ClaudeCode,
        "ask_grok" => DelegateKind::GrokBuild,
        "provider_limits" => {
            let summary = format_compact_summary(&options.store.load_all(), now())
                .unwrap_or_else(|| "No provider limits recorded yet.".to_string());
            return tool_text(summary, false);
        }
        other => return tool_text(format!("Unknown tool: {other}"), true),
    };
    let args: DelegateArgs = match serde_json::from_value(arguments) {
        Ok(args) => args,
        Err(err) => return tool_text(format!("Invalid arguments: {err}"), true),
    };

    if let Some(limits) = options.store.load(kind.provider_id())
        && limits.is_rate_limited(now())
    {
        return tool_text(
            format!(
                "{} is at its subscription limit until unix time {}. Route this task to another council member.",
                kind.display_name(),
                limits.rate_limited_until.unwrap_or_default()
            ),
            true,
        );
    }

    let request = DelegateRequest {
        kind,
        prompt: args.prompt,
        cwd: args.cwd.unwrap_or_else(|| options.default_cwd.clone()),
        mode: args.mode,
        role_instructions: args.role,
        model: args.model,
        timeout: args
            .timeout_minutes
            .map(|minutes| Duration::from_secs(minutes.clamp(1, 120) * 60))
            .unwrap_or(DEFAULT_TIMEOUT),
        binary: None,
    };
    match run_delegate(&request).await {
        Ok(outcome) => {
            if let Err(err) = record_delegate_outcome(&options.store, kind, &outcome, now()) {
                tracing::debug!("failed to record {} usage: {err}", kind.provider_id());
            }
            let mut text = outcome.text.clone();
            if let Some(until) = outcome.rate_limited_until {
                text.push_str(&format!(
                    "\n\n[{} hit its subscription limit; it resets at unix time {until}.]",
                    kind.display_name()
                ));
            }
            tool_text(text, outcome.is_error)
        }
        Err(err) => tool_text(format!("{err:#}"), true),
    }
}

fn tool_text(text: String, is_error: bool) -> Value {
    json!({
        "content": [{ "type": "text", "text": text }],
        "isError": is_error,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_provider_limits::ProviderLimits;
    use pretty_assertions::assert_eq;

    fn options(dir: &tempfile::TempDir) -> McpServerOptions {
        McpServerOptions {
            store: LimitsStore::new(dir.path().to_path_buf()),
            default_cwd: dir.path().to_path_buf(),
        }
    }

    #[tokio::test]
    async fn initialize_echoes_protocol_version() {
        let dir = tempfile::tempdir().expect("tempdir");
        let response = handle_message(
            &options(&dir),
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}),
        )
        .await
        .expect("response");
        assert_eq!(response["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(response["result"]["serverInfo"]["name"], SERVER_NAME);
    }

    #[tokio::test]
    async fn notifications_get_no_response() {
        let dir = tempfile::tempdir().expect("tempdir");
        let response = handle_message(
            &options(&dir),
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        )
        .await;
        assert_eq!(response, None);
    }

    #[tokio::test]
    async fn lists_three_tools() {
        let dir = tempfile::tempdir().expect("tempdir");
        let response = handle_message(
            &options(&dir),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
        )
        .await
        .expect("response");
        let names: Vec<&str> = response["result"]["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect();
        assert_eq!(names, vec!["ask_claude", "ask_grok", "provider_limits"]);
    }

    #[tokio::test]
    async fn rate_limited_delegate_is_refused_without_running() {
        let dir = tempfile::tempdir().expect("tempdir");
        let options = options(&dir);
        let mut limits = ProviderLimits::new("claude-code", "Claude", now());
        limits.rate_limited_until = Some(now() + 3600);
        options.store.save(&limits).expect("save");

        let response = handle_message(
            &options,
            json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"ask_claude","arguments":{"prompt":"plan it"}}}),
        )
        .await
        .expect("response");
        assert_eq!(response["result"]["isError"], true);
        let text = response["result"]["content"][0]["text"]
            .as_str()
            .expect("text");
        assert!(text.contains("subscription limit"), "{text}");
    }
}
