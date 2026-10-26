//! Council: several models working on one project with defined roles.
//!
//! Built-in Codex providers (GPT, Kimi, DeepSeek) run inside the agent loop. Claude
//! and Grok join as delegates: the user's own Claude Code and Grok Build CLIs, run
//! headless on the user's subscriptions and exposed to Codex through an MCP server.

pub mod delegate;
pub mod doctor;
pub mod events;
pub mod limits;
pub mod mcp;
pub mod roles;
