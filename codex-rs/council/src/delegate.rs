//! Runs a subscription-backed coding CLI (Claude Code or Grok Build) as a council member.
//!
//! The CLI signs in to the user's own subscription by itself. Council code only
//! launches the official binary and reads its structured output; it never reads,
//! copies or forwards the CLI's credentials.

use crate::events::DelegateOutcome;
use crate::events::parse_claude_stream;
use crate::events::parse_grok_output;
use anyhow::Context;
use serde::Deserialize;
use serde::Serialize;
use std::path::Path;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

/// Default cap for one delegated task.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DelegateKind {
    ClaudeCode,
    GrokBuild,
}

impl DelegateKind {
    pub const ALL: [DelegateKind; 2] = [DelegateKind::ClaudeCode, DelegateKind::GrokBuild];

    /// Id used for the limits store and tool names.
    pub fn provider_id(self) -> &'static str {
        match self {
            DelegateKind::ClaudeCode => "claude-code",
            DelegateKind::GrokBuild => "grok-build",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            DelegateKind::ClaudeCode => "Claude",
            DelegateKind::GrokBuild => "Grok",
        }
    }

    pub fn binary(self) -> &'static str {
        match self {
            DelegateKind::ClaudeCode => "claude",
            DelegateKind::GrokBuild => "grok",
        }
    }

    /// How the user signs the CLI in to their subscription.
    pub fn login_hint(self) -> &'static str {
        match self {
            DelegateKind::ClaudeCode => {
                "Install Claude Code, run `claude`, and sign in with your Claude Pro or Max account."
            }
            DelegateKind::GrokBuild => {
                "Install Grok Build and run `grok login` with your SuperGrok or X Premium+ account."
            }
        }
    }
}

/// Whether the delegate may change files.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DelegateMode {
    /// Read, search and answer; no edits or commands.
    #[default]
    ReadOnly,
    /// Edit files in the working directory without prompting.
    Edit,
}

#[derive(Debug, Clone)]
pub struct DelegateRequest {
    pub kind: DelegateKind,
    pub prompt: String,
    pub cwd: PathBuf,
    pub mode: DelegateMode,
    /// Role instructions appended to the CLI's own system prompt.
    pub role_instructions: Option<String>,
    pub model: Option<String>,
    pub timeout: Duration,
    /// Overrides the binary looked up on `PATH`.
    pub binary: Option<PathBuf>,
}

/// Arguments for one headless run, without the binary.
pub fn build_args(request: &DelegateRequest) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();
    match request.kind {
        DelegateKind::ClaudeCode => {
            // The prompt goes through stdin. `--bare` is deliberately not used: bare
            // mode ignores the subscription login.
            args.extend(
                ["-p", "--output-format", "stream-json", "--verbose"]
                    .into_iter()
                    .map(String::from),
            );
            match request.mode {
                DelegateMode::ReadOnly => {
                    args.extend(
                        [
                            "--permission-mode",
                            "dontAsk",
                            "--allowedTools",
                            "Read,Grep,Glob",
                        ]
                        .into_iter()
                        .map(String::from),
                    );
                }
                DelegateMode::Edit => {
                    args.extend(
                        ["--permission-mode", "acceptEdits"]
                            .into_iter()
                            .map(String::from),
                    );
                }
            }
            if let Some(role) = &request.role_instructions {
                args.push("--append-system-prompt".to_string());
                args.push(role.clone());
            }
            if let Some(model) = &request.model {
                args.push("--model".to_string());
                args.push(model.clone());
            }
        }
        DelegateKind::GrokBuild => {
            args.extend(
                ["--no-auto-update", "--output-format", "json", "--cwd"]
                    .into_iter()
                    .map(String::from),
            );
            args.push(request.cwd.display().to_string());
            if request.mode == DelegateMode::Edit {
                args.push("--always-approve".to_string());
            }
            if let Some(model) = &request.model {
                args.push("-m".to_string());
                args.push(model.clone());
            }
            args.push("-p".to_string());
            args.push(grok_prompt(request));
        }
    }
    args
}

/// Grok Build has no system-prompt flag, so role instructions lead the prompt.
fn grok_prompt(request: &DelegateRequest) -> String {
    match &request.role_instructions {
        Some(role) => format!("{role}\n\n---\n\n{}", request.prompt),
        None => request.prompt.clone(),
    }
}

pub fn resolve_binary(kind: DelegateKind, override_path: Option<&Path>) -> anyhow::Result<PathBuf> {
    match override_path {
        Some(path) => Ok(path.to_path_buf()),
        None => which::which(kind.binary()).with_context(|| {
            format!(
                "`{}` was not found on PATH. {}",
                kind.binary(),
                kind.login_hint()
            )
        }),
    }
}

/// Runs the delegate to completion and parses its output.
pub async fn run_delegate(request: &DelegateRequest) -> anyhow::Result<DelegateOutcome> {
    let binary = resolve_binary(request.kind, request.binary.as_deref())?;
    let mut command = Command::new(&binary);
    command
        .args(build_args(request))
        .current_dir(&request.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command
        .spawn()
        .with_context(|| format!("failed to start {}", binary.display()))?;

    let stdin_payload = match request.kind {
        DelegateKind::ClaudeCode => request.prompt.clone(),
        DelegateKind::GrokBuild => String::new(),
    };
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(stdin_payload.as_bytes()).await?;
        // Dropping stdin closes it so the CLI starts work.
    }

    let output = tokio::time::timeout(request.timeout, child.wait_with_output())
        .await
        .with_context(|| {
            format!(
                "{} did not finish within {} minutes",
                request.kind.display_name(),
                request.timeout.as_secs() / 60
            )
        })??;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let mut outcome = match request.kind {
        DelegateKind::ClaudeCode => parse_claude_stream(&stdout),
        DelegateKind::GrokBuild => parse_grok_output(&stdout),
    };
    if !output.status.success() {
        outcome.is_error = true;
        if outcome.text.trim().is_empty() {
            outcome.text = format!(
                "{} exited with {}: {}",
                request.kind.display_name(),
                output.status,
                stderr.trim()
            );
        }
    }
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn request(kind: DelegateKind, mode: DelegateMode) -> DelegateRequest {
        DelegateRequest {
            kind,
            prompt: "Review the diff".to_string(),
            cwd: PathBuf::from("/repo"),
            mode,
            role_instructions: Some("You are the reviewer.".to_string()),
            model: None,
            timeout: DEFAULT_TIMEOUT,
            binary: None,
        }
    }

    #[test]
    fn claude_read_only_args_never_use_bare_mode() {
        let args = build_args(&request(DelegateKind::ClaudeCode, DelegateMode::ReadOnly));
        assert_eq!(
            args,
            vec![
                "-p",
                "--output-format",
                "stream-json",
                "--verbose",
                "--permission-mode",
                "dontAsk",
                "--allowedTools",
                "Read,Grep,Glob",
                "--append-system-prompt",
                "You are the reviewer.",
            ]
        );
        assert!(!args.iter().any(|arg| arg == "--bare"));
    }

    #[test]
    fn grok_edit_args_put_role_before_prompt() {
        let args = build_args(&request(DelegateKind::GrokBuild, DelegateMode::Edit));
        assert_eq!(
            args,
            vec![
                "--no-auto-update",
                "--output-format",
                "json",
                "--cwd",
                "/repo",
                "--always-approve",
                "-p",
                "You are the reviewer.\n\n---\n\nReview the diff",
            ]
        );
    }
}
