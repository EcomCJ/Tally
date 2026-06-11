use anyhow::anyhow;
use serde::Deserialize;

use super::api::{live_limits_from_usage_response, UsageResponse};
use super::types::{ClaudeLimitSource, FetchOutcome};

#[derive(Debug, Deserialize)]
struct WebOrganization {
    #[serde(default, alias = "uuid", alias = "organization_uuid")]
    id: Option<String>,
}

pub(crate) fn web_fetch_live_limits() -> FetchOutcome {
    let cookie = match read_web_cookie_header() {
        Some(cookie) => cookie,
        None => return FetchOutcome::Other(anyhow!("no Claude web sessionKey configured")),
    };

    let orgs = match ureq::get("https://claude.ai/api/organizations")
        .set("Cookie", &cookie)
        .set("Accept", "application/json")
        .timeout(std::time::Duration::from_secs(8))
        .call()
    {
        Ok(resp) => resp,
        Err(e) => return FetchOutcome::Other(anyhow!("call claude.ai organizations: {e}")),
    };
    let orgs: Vec<WebOrganization> = match orgs.into_json() {
        Ok(orgs) => orgs,
        Err(e) => return FetchOutcome::Other(anyhow!("decode Claude web organizations: {e}")),
    };
    let Some(org_id) = orgs.into_iter().find_map(|org| org.id) else {
        return FetchOutcome::Other(anyhow!("Claude web organizations response had no org id"));
    };

    let url = format!("https://claude.ai/api/organizations/{org_id}/usage");
    let usage = match ureq::get(&url)
        .set("Cookie", &cookie)
        .set("Accept", "application/json")
        .timeout(std::time::Duration::from_secs(8))
        .call()
    {
        Ok(resp) => resp,
        Err(e) => return FetchOutcome::Other(anyhow!("call Claude web usage: {e}")),
    };
    let body: UsageResponse = match usage.into_json() {
        Ok(body) => body,
        Err(e) => return FetchOutcome::Other(anyhow!("decode Claude web usage: {e}")),
    };

    let mut live = live_limits_from_usage_response(body, ClaudeLimitSource::Web);
    live.account =
        crate::account::explicit_identity("claude", &format!("org:{org_id}"), "claude-web-org");
    FetchOutcome::Ok(live)
}

fn read_web_cookie_header() -> Option<String> {
    let raw = std::env::var("TALLY_CLAUDE_COOKIE")
        .ok()
        .or_else(|| std::env::var("TALLY_CLAUDE_SESSION_KEY").ok())?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.to_ascii_lowercase().contains("sessionkey=") {
        return Some(trimmed.to_string());
    }
    if trimmed.starts_with("sk-ant-") {
        return Some(format!("sessionKey={trimmed}"));
    }
    None
}
