//! `codex council`: several models working on one project with defined roles.

use clap::Parser;
use clap::ValueEnum;
use codex_core::config::find_codex_home;
use codex_council::delegate::DEFAULT_TIMEOUT;
use codex_council::delegate::DelegateKind;
use codex_council::delegate::DelegateMode;
use codex_council::delegate::DelegateRequest;
use codex_council::delegate::run_delegate;
use codex_council::doctor;
use codex_council::limits;
use codex_council::mcp;
use codex_council::roles;
use codex_provider_limits::LimitsStore;
use codex_provider_limits::format_compact_summary;
use std::io::Read;

#[derive(Debug, Parser)]
pub struct CouncilCommand {
    #[command(subcommand)]
    subcommand: CouncilSubcommand,
}

#[derive(Debug, clap::Subcommand)]
enum CouncilSubcommand {
    /// Write the council profile and this project's council role files.
    Init(InitArgs),

    /// Start an interactive council session (same as `codex --profile council`).
    Start {
        /// Extra arguments passed to `codex`, such as an initial prompt.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// Check that every council member is installed and configured.
    Doctor,

    /// Show usage left for every provider.
    Limits {
        /// Fetch the DeepSeek balance first (needs DEEPSEEK_API_KEY).
        #[arg(long)]
        refresh: bool,
    },

    /// Send one task straight to a delegate and print its answer.
    Ask(AskArgs),

    /// Serve council delegates as MCP tools over stdio.
    Mcp,

    /// Record Claude Code's usage limits. Set as Claude Code's status-line command.
    ClaudeStatusline,
}

#[derive(Debug, Parser)]
struct InitArgs {
    /// Overwrite existing council files.
    #[arg(long)]
    force: bool,

    /// Kimi Code model for the council-coder role.
    #[arg(long)]
    kimi_model: Option<String>,

    /// DeepSeek model for the council-tester role.
    #[arg(long)]
    deepseek_model: Option<String>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Member {
    Claude,
    Grok,
}

#[derive(Debug, Parser)]
struct AskArgs {
    member: Member,

    /// The task.
    prompt: String,

    /// Allow the delegate to edit files in the current directory.
    #[arg(long)]
    edit: bool,
}

pub async fn run(command: CouncilCommand) -> anyhow::Result<()> {
    let codex_home = find_codex_home()?;
    let store = LimitsStore::for_codex_home(codex_home.as_path());
    match command.subcommand {
        CouncilSubcommand::Init(args) => {
            let mut models = roles::CouncilModels::default();
            if let Some(model) = args.kimi_model {
                models.kimi_model = model;
            }
            if let Some(model) = args.deepseek_model {
                models.deepseek_model = model;
            }
            match roles::write_council_profile(codex_home.as_path(), args.force)? {
                Some(path) => println!("Wrote {}", path.display()),
                None => println!(
                    "Kept existing {}/{}.config.toml (use --force to replace it)",
                    codex_home.display(),
                    roles::COUNCIL_PROFILE
                ),
            }
            let report = roles::write_role_files(&std::env::current_dir()?, &models, args.force)?;
            for path in report.written {
                println!("Wrote {}", path.display());
            }
            for path in report.skipped {
                println!("Kept existing {}", path.display());
            }
            println!(
                "\nNext: run `codex council doctor`, then `codex council start`.\n\
                 For live Claude limits, set Claude Code's status line command to \
                 `codex council claude-statusline`."
            );
        }
        CouncilSubcommand::Start { args } => {
            let status = std::process::Command::new(std::env::current_exe()?)
                .arg("--profile")
                .arg(roles::COUNCIL_PROFILE)
                .args(args)
                .status()?;
            std::process::exit(status.code().unwrap_or(1));
        }
        CouncilSubcommand::Doctor => {
            let results = doctor::run_checks().await;
            println!("{}", doctor::format_report(&results));
            if results.iter().any(|result| !result.ok) {
                std::process::exit(1);
            }
        }
        CouncilSubcommand::Limits { refresh } => {
            if refresh {
                match std::env::var("DEEPSEEK_API_KEY") {
                    Ok(key) if !key.trim().is_empty() => {
                        if let Err(err) = limits::refresh_deepseek_balance(&store, &key).await {
                            eprintln!("DeepSeek balance refresh failed: {err}");
                        }
                    }
                    _ => eprintln!("DEEPSEEK_API_KEY is not set; skipping the balance refresh."),
                }
            }
            println!(
                "{}",
                format_compact_summary(&store.load_all(), limits::now())
                    .unwrap_or_else(|| "No provider limits recorded yet.".to_string())
            );
        }
        CouncilSubcommand::Ask(args) => {
            let kind = match args.member {
                Member::Claude => DelegateKind::ClaudeCode,
                Member::Grok => DelegateKind::GrokBuild,
            };
            let request = DelegateRequest {
                kind,
                prompt: args.prompt,
                cwd: std::env::current_dir()?,
                mode: if args.edit {
                    DelegateMode::Edit
                } else {
                    DelegateMode::ReadOnly
                },
                role_instructions: None,
                model: None,
                timeout: DEFAULT_TIMEOUT,
                binary: None,
            };
            let outcome = run_delegate(&request).await?;
            limits::record_delegate_outcome(&store, kind, &outcome, limits::now())?;
            println!("{}", outcome.text);
            if outcome.is_error {
                std::process::exit(1);
            }
        }
        CouncilSubcommand::Mcp => {
            mcp::serve_stdio(mcp::McpServerOptions {
                store,
                default_cwd: std::env::current_dir()?,
            })
            .await?;
        }
        CouncilSubcommand::ClaudeStatusline => {
            let mut input = String::new();
            std::io::stdin().read_to_string(&mut input)?;
            // A status-line command must always print something sensible, so
            // parsing problems fall back to an empty line.
            let line = limits::handle_claude_status_line(&store, &input, limits::now())
                .unwrap_or_default();
            println!("{line}");
        }
    }
    Ok(())
}
