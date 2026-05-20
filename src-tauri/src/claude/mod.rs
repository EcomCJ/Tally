use anyhow::Result;

mod api;
mod cache;
mod cli;
mod history;
mod jsonl;
mod oauth;
mod roots;
mod types;
mod web;

pub use types::{ClaudeLimitSource, ClaudeLiveLimits, ClaudeStats, SubQuota};

pub fn fetch_plan_tier() -> Result<String> {
    oauth::fetch_plan_tier()
}

/// True if at least one Claude source can plausibly run.
pub fn is_available() -> bool {
    oauth::read_oauth_token().is_ok() || cli::claude_cli_available()
}

pub fn fetch_live_limits(refresh_ms: u64) -> Result<ClaudeLiveLimits> {
    cache::fetch_live_limits(refresh_ms)
}

pub fn collect(refresh_ms: u64) -> Result<ClaudeStats> {
    let mut stats = jsonl::collect_token_stats()?;

    match fetch_live_limits(refresh_ms) {
        Ok(live) => {
            stats.five_hour_percent = live.five_hour_percent;
            stats.weekly_percent = live.weekly_percent;
            stats.limit_source = Some(live.source);
            stats.limit_fetched_at = Some(live.fetched_at);
            stats.next_5h_reset = live.five_hour_resets_at;
            stats.next_weekly_reset = live.weekly_resets_at;
            stats.sub_quotas = live.sub_quotas;
            stats.extra_usage = live.extra_usage;
            stats.last_event_at.get_or_insert_with(chrono::Utc::now);
        }
        Err(e) => {
            eprintln!("[tally] claude live fetch failed: {e}");
        }
    }

    Ok(stats)
}
