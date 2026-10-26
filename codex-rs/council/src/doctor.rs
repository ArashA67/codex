//! `codex council doctor`: checks that every council member can run.

use crate::delegate::DelegateKind;
use crate::delegate::resolve_binary;
use std::time::Duration;
use tokio::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckResult {
    pub member: &'static str,
    pub ok: bool,
    pub detail: String,
}

/// API-key members and the variable each one reads. Values are never printed.
const KEY_MEMBERS: [(&str, &str, &str); 2] = [
    (
        "Kimi (kimi-code)",
        "KIMI_API_KEY",
        "Create a key in the Kimi Code console.",
    ),
    (
        "DeepSeek (deepseek)",
        "DEEPSEEK_API_KEY",
        "Create a key at https://platform.deepseek.com/api_keys.",
    ),
];

pub async fn run_checks() -> Vec<CheckResult> {
    let mut results = Vec::new();
    for kind in DelegateKind::ALL {
        results.push(check_delegate(kind).await);
    }
    for (member, env_key, hint) in KEY_MEMBERS {
        let present = std::env::var(env_key).is_ok_and(|value| !value.trim().is_empty());
        results.push(CheckResult {
            member,
            ok: present,
            detail: if present {
                format!("{env_key} is set")
            } else {
                format!("{env_key} is not set. {hint}")
            },
        });
    }
    results
}

async fn check_delegate(kind: DelegateKind) -> CheckResult {
    let member = match kind {
        DelegateKind::ClaudeCode => "Claude (Claude Code)",
        DelegateKind::GrokBuild => "Grok (Grok Build)",
    };
    let binary = match resolve_binary(kind, /*override_path*/ None) {
        Ok(binary) => binary,
        Err(err) => {
            return CheckResult {
                member,
                ok: false,
                detail: err.to_string(),
            };
        }
    };
    let version = tokio::time::timeout(
        Duration::from_secs(20),
        Command::new(&binary).arg("--version").output(),
    )
    .await;
    match version {
        Ok(Ok(output)) if output.status.success() => CheckResult {
            member,
            ok: true,
            detail: format!(
                "{} found ({}). Sign-in is checked on first use. {}",
                binary.display(),
                String::from_utf8_lossy(&output.stdout).trim(),
                kind.login_hint()
            ),
        },
        _ => CheckResult {
            member,
            ok: false,
            detail: format!("{} did not answer `--version`.", binary.display()),
        },
    }
}

pub fn format_report(results: &[CheckResult]) -> String {
    results
        .iter()
        .map(|result| {
            let mark = if result.ok { "ok " } else { "!! " };
            format!("{mark} {}: {}", result.member, result.detail)
        })
        .collect::<Vec<_>>()
        .join("\n")
}
