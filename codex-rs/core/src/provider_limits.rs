//! Records third-party provider limits and usage into the shared limits store,
//! which the TUI and council delegates read to show every provider's limits.

use crate::session::turn_context::TurnContext;
use codex_api::X_RATELIMIT_LIMIT_ID;
use codex_protocol::protocol::RateLimitSnapshot;
use codex_protocol::protocol::RateLimitWindow;
use codex_protocol::protocol::TokenUsage;
use codex_provider_limits::LimitGauge;
use codex_provider_limits::LimitQuality;
use codex_provider_limits::LimitsStore;
use codex_provider_limits::local_day;

/// Returns true when the snapshot came from `x-ratelimit-*` headers and was recorded here.
///
/// Such snapshots must not reach the ChatGPT account limit state.
pub(crate) fn record_header_snapshot(
    turn_context: &TurnContext,
    snapshot: &RateLimitSnapshot,
) -> bool {
    if snapshot.limit_id.as_deref() != Some(X_RATELIMIT_LIMIT_ID) {
        return false;
    }
    if turn_context.provider.info().is_openai() {
        return true;
    }
    let gauges: Vec<LimitGauge> = [("req", &snapshot.primary), ("tok", &snapshot.secondary)]
        .into_iter()
        .filter_map(|(label, window)| window.as_ref().map(|window| gauge(label, window)))
        .collect();
    update_store(turn_context, move |limits| {
        limits.quality = LimitQuality::Exact;
        limits.gauges = gauges;
        limits.rate_limited_until = None;
    });
    true
}

/// Adds a finished request's tokens to the provider's daily counters.
pub(crate) fn record_request_usage(turn_context: &TurnContext, usage: Option<&TokenUsage>) {
    if turn_context.provider.info().is_openai() {
        return;
    }
    let tokens = usage
        .map(|usage| u64::try_from(usage.total_tokens).unwrap_or_default())
        .unwrap_or_default();
    update_store(turn_context, move |limits| {
        limits.record_request(tokens, &local_day(now()));
    });
}

fn gauge(label: &str, window: &RateLimitWindow) -> LimitGauge {
    LimitGauge {
        label: label.to_string(),
        used_percent: window.used_percent,
        resets_at: window.resets_at,
    }
}

fn update_store(
    turn_context: &TurnContext,
    update: impl FnOnce(&mut codex_provider_limits::ProviderLimits) + Send + 'static,
) {
    let store = LimitsStore::for_codex_home(turn_context.config.codex_home.as_path());
    let provider_id = turn_context.config.model_provider_id.clone();
    let display_name = turn_context.provider.info().name.clone();
    tokio::task::spawn_blocking(move || {
        if let Err(err) = store.update(&provider_id, &display_name, now(), update) {
            tracing::debug!("failed to record limits for {provider_id}: {err}");
        }
    });
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}
