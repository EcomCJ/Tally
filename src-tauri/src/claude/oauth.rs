use anyhow::{anyhow, Result};
use chrono::Utc;
use serde::Deserialize;
use std::fs::File;

use super::api::{live_limits_from_usage_response, UsageResponse};
use super::cache::disk_cache_path;
use super::types::{ClaudeLimitSource, FetchOutcome};

#[derive(Debug, Deserialize)]
struct ProfileResponse {
    organization: Option<ProfileOrg>,
}

#[derive(Debug, Deserialize)]
struct ProfileOrg {
    rate_limit_tier: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Credentials {
    #[serde(rename = "claudeAiOauth")]
    claude_ai_oauth: OauthBlock,
}

#[derive(Debug, Deserialize)]
struct OauthBlock {
    #[serde(rename = "accessToken")]
    access_token: String,
}

/// Fetch the user's Claude subscription tier identifier from /api/oauth/profile.
/// Cached aggressively (24h) since it changes rarely.
pub fn fetch_plan_tier() -> Result<String> {
    let cache_path = disk_cache_path().map(|mut p| {
        p.set_file_name("claude-plan.json");
        p
    });
    if let Some(p) = &cache_path {
        if let Ok(s) = std::fs::read_to_string(p) {
            if let Ok(entry) = serde_json::from_str::<serde_json::Value>(&s) {
                let ts = entry.get("ts").and_then(|v| v.as_i64()).unwrap_or(0);
                let age = Utc::now().timestamp() - ts;
                if age < 86_400 {
                    if let Some(t) = entry.get("tier").and_then(|v| v.as_str()) {
                        return Ok(t.to_string());
                    }
                }
            }
        }
    }
    let token = read_oauth_token()?;
    let resp = ureq::get("https://api.anthropic.com/api/oauth/profile")
        .set("Authorization", &format!("Bearer {token}"))
        .set("anthropic-version", "2023-06-01")
        .set("anthropic-beta", "oauth-2025-04-20")
        .timeout(std::time::Duration::from_secs(8))
        .call()
        .map_err(|e| anyhow!("call /api/oauth/profile: {e}"))?;
    let body: ProfileResponse = resp
        .into_json()
        .map_err(|e| anyhow!("decode profile response: {e}"))?;
    let tier = body
        .organization
        .and_then(|o| o.rate_limit_tier)
        .unwrap_or_else(|| "unknown".to_string());
    if let Some(p) = cache_path {
        let payload = serde_json::json!({ "ts": Utc::now().timestamp(), "tier": tier });
        let _ = std::fs::write(p, payload.to_string());
    }
    Ok(tier)
}

pub(crate) fn read_oauth_token() -> Result<String> {
    let mut path = dirs::home_dir().ok_or_else(|| anyhow!("no home dir"))?;
    path.push(".claude");
    path.push(".credentials.json");
    let file = File::open(&path).map_err(|e| anyhow!("read {}: {e}", path.display()))?;
    let creds: Credentials =
        serde_json::from_reader(file).map_err(|e| anyhow!("parse credentials: {e}"))?;
    Ok(creds.claude_ai_oauth.access_token)
}

pub(crate) fn http_fetch_live_limits() -> FetchOutcome {
    let token = match read_oauth_token() {
        Ok(t) => t,
        Err(e) => return FetchOutcome::Other(e),
    };
    let user_agent = format!("claude-code/{}", claude_code_version());
    let result = ureq::get("https://api.anthropic.com/api/oauth/usage")
        .set("Authorization", &format!("Bearer {token}"))
        .set("Accept", "application/json")
        .set("anthropic-beta", "oauth-2025-04-20")
        .set("User-Agent", &user_agent)
        .timeout(std::time::Duration::from_secs(8))
        .call();
    let resp = match result {
        Ok(r) => r,
        Err(ureq::Error::Status(429, _)) => {
            return FetchOutcome::RateLimited(
                "Anthropic /api/oauth/usage returned 429 - backing off".to_string(),
            );
        }
        Err(e) => return FetchOutcome::Other(anyhow!("call /api/oauth/usage: {e}")),
    };
    let body: UsageResponse = match resp.into_json() {
        Ok(b) => b,
        Err(e) => return FetchOutcome::Other(anyhow!("decode usage response: {e}")),
    };

    FetchOutcome::Ok(live_limits_from_usage_response(
        body,
        ClaudeLimitSource::Oauth,
    ))
}

fn claude_code_version() -> String {
    let output = std::process::Command::new("claude")
        .arg("--version")
        .output();
    if let Ok(output) = output {
        let raw = String::from_utf8_lossy(&output.stdout);
        if let Some(first) = raw.split_whitespace().next() {
            if !first.trim().is_empty() {
                return first.trim().to_string();
            }
        }
    }
    "2.1.0".to_string()
}
