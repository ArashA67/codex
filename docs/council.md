# Council: several models on one project

The council runs GPT, Claude, Kimi, DeepSeek and Grok together on the same codebase, each with a role, and shows every provider's usage limits in the status line.

| Role | Member | How it connects | Billing |
| --- | --- | --- | --- |
| Lead and coder | GPT | Built in, "Sign in with ChatGPT" | ChatGPT subscription |
| Planner | Claude | Delegate: your installed Claude Code, run headless | Claude Pro or Max subscription |
| Coder | Kimi | Built-in `kimi-code` provider (Responses API) | Kimi Code membership |
| Reviewer | Grok | Delegate: your installed Grok Build CLI, run headless | SuperGrok or X Premium+ subscription |
| Tester | DeepSeek | Built-in `deepseek` provider (Responses API) | DeepSeek API key, prepaid |

Claude and Grok don't allow other tools to use their subscription logins, so the council never handles those credentials. It starts the official `claude` and `grok` programs, which sign in by themselves, and reads their JSON output.

## Setup

1. Sign in to each member:
   - `codex login` with your ChatGPT account.
   - Install Claude Code, run `claude` once, and sign in with your Claude account.
   - Install Grok Build and run `grok login`.
   - Export `KIMI_API_KEY` (from the Kimi Code console) and `DEEPSEEK_API_KEY`.
2. In your project, run `codex council init`. It writes:
   - `$CODEX_HOME/council.config.toml`, a profile with the lead's instructions and the delegate MCP server. Ordinary `codex` sessions don't load it.
   - `.codex/agents/council-coder.toml` (Kimi) and `.codex/agents/council-tester.toml` (DeepSeek).
3. Run `codex council doctor` to check every member.
4. Run `codex council start` (the same as `codex --profile council`).

Mainland China Kimi accounts use a different endpoint. Override the preset in `config.toml`:

```toml
[model_providers.kimi-code]
name = "Kimi Code"
base_url = "https://api.kimi.com/coding/v1"
env_key = "KIMI_API_KEY"
wire_api = "responses"
```

## How a council session works

1. **Plan.** The lead asks Claude (`ask_claude`, read-only) for a numbered plan and shows it to you for approval.
2. **Build.** The lead codes some tasks itself and spawns `council-coder` agents on Kimi for independent tasks.
3. **Review.** Each finished task goes to Grok (`ask_grok`, read-only) for review.
4. **Test.** A `council-tester` agent on DeepSeek writes and runs tests.
5. **Report.** The lead summarizes what changed and which member did what.

Before routing work, the lead calls `provider_limits` and skips any member at its cap, using the fallbacks in its instructions. Edit the role files or `council.config.toml` to change who does what.

You can also call a delegate directly: `codex council ask claude "Review src/lib.rs for races"`. Add `--edit` to let it change files.

## Limits display

The `all-limits` status-line item is on by default. It reads like this:

```
GPT 5h:58% wk:81% │ Claude 5h:40% wk:77% (12m ago) │ DeepSeek $8.40 │ Kimi 1.2M tok today │ Grok 3 runs today
```

Percentages are what's left. What each provider can show:

| Provider | Source | Quality |
| --- | --- | --- |
| GPT | ChatGPT usage windows that Codex already reads | Exact |
| Claude | Claude Code's status-line `rate_limits` (5-hour and 7-day), plus the reset time when a headless run hits the cap | Last seen; shows its age |
| Kimi | `x-ratelimit-*` headers when the server sends them, otherwise tokens counted locally | Exact or local |
| DeepSeek | Account balance from the DeepSeek API, refreshed every 5 minutes while a council session runs, plus local token counts | Exact balance |
| Grok | Local run and token counts, plus the cap when a run hits it | Local |

To feed Claude's live numbers, set Claude Code's status line to the council bridge in `~/.claude/settings.json`:

```json
{
  "statusLine": { "type": "command", "command": "codex council claude-statusline" }
}
```

Each time Claude Code refreshes its status line, the bridge saves the 5-hour and 7-day usage for the council and prints them in Claude Code's own status line.

`codex council limits` prints the same summary in a terminal; `--refresh` fetches the DeepSeek balance first.

Limits are stored as one JSON file per provider in `$CODEX_HOME/limits/`.

## Notes and limits of this version

- Kimi Code's model id defaults to `k3.1-pro`; change it with `codex council init --kimi-model <id> --force` or in `.codex/agents/council-coder.toml`.
- DeepSeek's Responses API serves `deepseek-v4-flash`. Other DeepSeek models may need its Anthropic-format endpoint, which Codex doesn't speak.
- Grok Build's JSON output isn't documented. The council accepts common field names and falls back to the raw text.
- Delegated runs use your subscriptions' normal allowances. Keep delegate calls few and substantial; the lead's instructions ask for one planning call and one review per task.
