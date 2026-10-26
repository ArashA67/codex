//! `codex council init`: writes the council's role files and config snippet.

use std::fs;
use std::io;
use std::path::Path;
use std::path::PathBuf;

/// Model choices for the built-in members. Delegates pick their own model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CouncilModels {
    pub kimi_model: String,
    pub deepseek_model: String,
}

impl Default for CouncilModels {
    fn default() -> Self {
        Self {
            kimi_model: "k3.1-pro".to_string(),
            deepseek_model: "deepseek-v4-flash".to_string(),
        }
    }
}

/// Instructions for the session that leads the council (GPT by default).
pub const COORDINATOR_INSTRUCTIONS: &str = r#"You lead a council of AI models working on this project together. Each member has a role:

- Planner: Claude, through the `ask_claude` tool (the user's Claude subscription).
- Coders: you (GPT) and the `council-coder` sub-agent role (Kimi).
- Reviewer: Grok, through the `ask_grok` tool (the user's SuperGrok subscription).
- Tester: the `council-tester` sub-agent role (DeepSeek).

Work in this order:
1. Plan. Call `ask_claude` in read-only mode with the user's request and the context it needs (it cannot see this conversation). Ask for a numbered plan of small, independently testable tasks. Show the plan to the user and wait for approval before changing code.
2. Build. Do tasks yourself or spawn `council-coder` agents for independent tasks, so work runs in parallel. Give every agent a precise brief.
3. Review. After each task, call `ask_grok` in read-only mode with the task, the diff and how to check it. Fix real problems; explain any finding you decline.
4. Test. Spawn a `council-tester` agent to add and run tests for the finished tasks.
5. Report. Summarize what changed, which member did what, and anything left open.

Always pass `cwd` to `ask_claude` and `ask_grok`: the project root for read-only work, or a separate git worktree when a delegate edits files.

Limits: call `provider_limits` at the start and whenever a member fails. Never send work to a member that is at its limit. Fallbacks: planning Claude -> Grok -> you; review Grok -> Claude -> a `council-coder` agent; coding Kimi -> you; testing DeepSeek -> Kimi -> you. Keep delegate calls few and substantial: one planning call and one review per task, not a stream of small questions.
"#;

fn coder_role(models: &CouncilModels) -> String {
    format!(
        r#"name = "council-coder"
description = "Council coder on Kimi. Implements one well-specified task from the council plan and reports the diff."
nickname_candidates = ["Kimi"]
model_provider = "kimi-code"
model = "{model}"
developer_instructions = """
You are a coder on a council of AI models. Implement exactly the task you were given, following the project's conventions.
Keep the change focused, run the relevant checks, and finish with a short report: what changed, how you verified it, and anything uncertain.
"""
"#,
        model = models.kimi_model
    )
}

fn tester_role(models: &CouncilModels) -> String {
    format!(
        r#"name = "council-tester"
description = "Council tester on DeepSeek. Writes and runs tests for finished council tasks."
nickname_candidates = ["DeepSeek"]
model_provider = "deepseek"
model = "{model}"
developer_instructions = """
You are the tester on a council of AI models. For each task you are given, add or extend tests that prove the behavior, run them, and report results.
Do not change production code unless a test reveals a clear bug; report such bugs instead of rewriting features.
"""
"#,
        model = models.deepseek_model
    )
}

/// File name of the council config layer, selected with `codex --profile council`.
pub const COUNCIL_PROFILE: &str = "council";

/// Contents of `$CODEX_HOME/council.config.toml`.
///
/// It is a profile layer on top of the user's config, so the council lead's
/// instructions and the delegate MCP server apply only to council sessions.
pub fn council_config_toml() -> String {
    let instructions = toml::Value::String(COORDINATOR_INSTRUCTIONS.to_string());
    format!(
        r#"# Council profile. Start with `codex council start` (same as `codex --profile council`).

# The council lead runs on your ChatGPT subscription.
model_provider = "openai"

developer_instructions = {instructions}

# Claude Code and Grok Build, on your subscriptions, exposed as tools.
[mcp_servers.council]
command = "codex"
args = ["council", "mcp"]
# Delegated tasks can take a while.
tool_timeout_sec = 1800
"#
    )
}

/// Writes the council profile unless it exists and `force` is false.
pub fn write_council_profile(codex_home: &Path, force: bool) -> io::Result<Option<PathBuf>> {
    let path = codex_home.join(format!("{COUNCIL_PROFILE}.config.toml"));
    if path.exists() && !force {
        return Ok(None);
    }
    fs::create_dir_all(codex_home)?;
    fs::write(&path, council_config_toml())?;
    Ok(Some(path))
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct InitReport {
    pub written: Vec<PathBuf>,
    pub skipped: Vec<PathBuf>,
}

/// Writes role files under `<project>/.codex/agents`, keeping existing files unless `force`.
pub fn write_role_files(
    project_root: &Path,
    models: &CouncilModels,
    force: bool,
) -> io::Result<InitReport> {
    let agents_dir = project_root.join(".codex").join("agents");
    fs::create_dir_all(&agents_dir)?;
    let mut report = InitReport::default();
    for (file_name, contents) in [
        ("council-coder.toml", coder_role(models)),
        ("council-tester.toml", tester_role(models)),
    ] {
        let path = agents_dir.join(file_name);
        if path.exists() && !force {
            report.skipped.push(path);
            continue;
        }
        fs::write(&path, contents)?;
        report.written.push(path);
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn writes_roles_once_unless_forced() {
        let dir = tempfile::tempdir().expect("tempdir");
        let models = CouncilModels::default();

        let first = write_role_files(dir.path(), &models, false).expect("init");
        assert_eq!(first.written.len(), 2);
        let coder = fs::read_to_string(dir.path().join(".codex/agents/council-coder.toml"))
            .expect("coder role");
        let parsed: toml::Value = toml::from_str(&coder).expect("valid toml");
        assert_eq!(parsed["model_provider"].as_str(), Some("kimi-code"));
        assert_eq!(parsed["model"].as_str(), Some("k3.1-pro"));

        let second = write_role_files(dir.path(), &models, false).expect("init again");
        assert_eq!((second.written.len(), second.skipped.len()), (0, 2));

        let forced = write_role_files(dir.path(), &models, true).expect("force");
        assert_eq!(forced.written.len(), 2);
    }

    #[test]
    fn council_profile_is_valid_toml_and_kept_unless_forced() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = write_council_profile(dir.path(), false)
            .expect("write")
            .expect("written");
        let parsed: toml::Value =
            toml::from_str(&fs::read_to_string(&path).expect("read")).expect("valid toml");
        assert_eq!(parsed["model_provider"].as_str(), Some("openai"));
        assert_eq!(
            parsed["developer_instructions"].as_str(),
            Some(COORDINATOR_INSTRUCTIONS)
        );
        assert_eq!(
            parsed["mcp_servers"]["council"]["tool_timeout_sec"].as_integer(),
            Some(1800)
        );

        assert_eq!(
            write_council_profile(dir.path(), false).expect("keep"),
            None
        );
        assert!(
            write_council_profile(dir.path(), true)
                .expect("force")
                .is_some()
        );
    }
}
