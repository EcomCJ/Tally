use anyhow::{anyhow, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::api::{live_limits_from_usage_response, UsageResponse};
use super::cache::disk_cache_path;
use super::types::{ClaudeLimitSource, FetchOutcome};
use crate::account::AccountIdentity;

// Anthropic's public Claude Code OAuth client. Same id baked into the
// Claude Code CLI and Claude Desktop. Used for the refresh-token grant
// against Anthropic's platform token endpoint.
const CLAUDE_OAUTH_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const CLAUDE_OAUTH_TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
// Refresh proactively when within this many seconds of expiry so the next
// HTTP call always carries a fresh token (no wasted 401 round-trip).
const TOKEN_REFRESH_MARGIN_SECS: i64 = 60;
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[derive(Debug, Deserialize)]
struct ProfileResponse {
    account: Option<ProfileAccount>,
    organization: Option<ProfileOrg>,
}

#[derive(Debug, Deserialize)]
struct ProfileAccount {
    uuid: Option<String>,
    email: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ProfileOrg {
    uuid: Option<String>,
    rate_limit_tier: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AuthStatus {
    logged_in: bool,
    email: Option<String>,
    org_id: Option<String>,
    subscription_type: Option<String>,
    auth_method: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct CredentialsFile {
    #[serde(rename = "claudeAiOauth")]
    claude_ai_oauth: OauthBlock,
    /// Capture-and-roundtrip any other top-level fields the Claude CLI may
    /// write (so refreshing doesn't strip them).
    #[serde(flatten)]
    other: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct OauthBlock {
    access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expires_at: Option<i64>,
    /// Preserves `scopes`, `subscriptionType`, `rateLimitTier`, etc.
    #[serde(flatten)]
    extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct RefreshResponse {
    access_token: Option<String>,
    refresh_token: Option<String>,
    expires_in: Option<i64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct CachedProfileIdentity {
    token_key: String,
    account: AccountIdentity,
    ts: i64,
}

/// Returns the Claude config root, honoring the `CLAUDE_HOME` env var the same
/// way Codex honors `CODEX_HOME`. Defaults to `~/.claude/`. Lets users with
/// non-standard installs (relocated home, sandboxed CI runners, etc.) point
/// Tally at the right credentials file without symlinks.
fn claude_home_dir() -> Result<PathBuf> {
    if let Some(raw) = std::env::var_os("CLAUDE_HOME") {
        let s = raw.to_string_lossy().trim().to_string();
        if !s.is_empty() {
            return Ok(PathBuf::from(s));
        }
    }
    let mut p = dirs::home_dir().ok_or_else(|| anyhow!("no home dir"))?;
    p.push(".claude");
    Ok(p)
}

fn credentials_path() -> Result<PathBuf> {
    Ok(claude_home_dir()?.join(".credentials.json"))
}

fn profile_identity_cache_path() -> Option<PathBuf> {
    let mut p = dirs::cache_dir()?;
    p.push("tally");
    let _ = std::fs::create_dir_all(&p);
    p.push("claude-account.json");
    Some(p)
}

fn read_credentials() -> Result<(PathBuf, CredentialsFile)> {
    let path = credentials_path()?;
    let file = File::open(&path).map_err(|e| anyhow!("read {}: {e}", path.display()))?;
    let creds: CredentialsFile =
        serde_json::from_reader(file).map_err(|e| anyhow!("parse credentials: {e}"))?;
    Ok((path, creds))
}

fn save_credentials(path: &Path, creds: &CredentialsFile) -> Result<()> {
    let body = serde_json::to_string_pretty(creds)?;
    std::fs::write(path, body).map_err(|e| anyhow!("write {}: {e}", path.display()))
}

fn is_token_expired(block: &OauthBlock) -> bool {
    match block.expires_at {
        Some(exp_ms) => {
            let now_ms = Utc::now().timestamp_millis();
            now_ms + (TOKEN_REFRESH_MARGIN_SECS * 1000) >= exp_ms
        }
        // No expiry info — assume valid until something 401s (legacy creds).
        None => false,
    }
}

fn post_refresh(refresh_token: &str) -> Result<RefreshResponse> {
    let result = ureq::post(CLAUDE_OAUTH_TOKEN_URL)
        .set("Content-Type", "application/json")
        .set(
            "User-Agent",
            &format!("claude-code/{}", claude_code_version()),
        )
        .timeout(std::time::Duration::from_secs(15))
        .send_json(serde_json::json!({
            "grant_type": "refresh_token",
            "refresh_token": refresh_token,
            "client_id": CLAUDE_OAUTH_CLIENT_ID,
        }));
    match result {
        Ok(resp) => resp
            .into_json::<RefreshResponse>()
            .map_err(|e| anyhow!("decode claude oauth refresh: {e}")),
        // Non-2xx: read the body and classify so the surfaced message is
        // actionable (re-login vs transient vs client mismatch) instead of an
        // opaque "status 400". The body read consumes the response.
        Err(ureq::Error::Status(code, resp)) => {
            let body = resp.into_string().unwrap_or_default();
            Err(anyhow!(
                "claude {}",
                crate::oauth_errors::refresh_error_message(code, &body, "claude")
            ))
        }
        // Transport-level failure (DNS, TLS, timeout, connection refused).
        Err(e) => Err(anyhow!("claude token refresh network error: {e}")),
    }
}

/// Returns a non-expired access token, refreshing + persisting the credentials
/// file if needed. Mirrors the rotation Claude Code CLI itself performs, so a
/// user without the CLI installed (or one whose CLI hasn't run in a while)
/// still gets auto-refresh on long-running Tally sessions.
fn ensure_fresh_access_token() -> Result<String> {
    let (path, mut creds) = read_credentials()?;
    if !is_token_expired(&creds.claude_ai_oauth) {
        return Ok(creds.claude_ai_oauth.access_token.clone());
    }
    let refresh = creds
        .claude_ai_oauth
        .refresh_token
        .clone()
        .ok_or_else(|| anyhow!("claude oauth token expired and no refresh_token to rotate"))?;
    let resp = post_refresh(&refresh)?;
    if let Some(at) = resp.access_token {
        creds.claude_ai_oauth.access_token = at;
    }
    if let Some(rt) = resp.refresh_token {
        creds.claude_ai_oauth.refresh_token = Some(rt);
    }
    if let Some(exp_in) = resp.expires_in {
        creds.claude_ai_oauth.expires_at = Some(Utc::now().timestamp_millis() + exp_in * 1000);
    }
    save_credentials(&path, &creds)?;
    Ok(creds.claude_ai_oauth.access_token)
}

pub(crate) fn read_oauth_token() -> Result<String> {
    ensure_fresh_access_token()
}

pub(crate) fn active_account_identity() -> Option<AccountIdentity> {
    let token_identity = credential_token_identity();
    if let Some(token_id) = token_identity.as_ref() {
        if let Some(profile_id) = read_cached_profile_identity(token_id) {
            return Some(profile_id);
        }
        return token_identity;
    }

    active_auth_status_identity()
}

fn credential_token_identity() -> Option<AccountIdentity> {
    let (_, creds) = read_credentials().ok()?;
    crate::account::token_identity(
        "claude",
        &creds.claude_ai_oauth.access_token,
        "claude-oauth-token",
    )
}

fn token_account_identity(token: &str) -> Option<AccountIdentity> {
    crate::account::token_identity("claude", token, "claude-oauth-token")
}

fn read_cached_profile_identity(token_identity: &AccountIdentity) -> Option<AccountIdentity> {
    let path = profile_identity_cache_path()?;
    let raw = std::fs::read_to_string(path).ok()?;
    let cached: CachedProfileIdentity = serde_json::from_str(&raw).ok()?;
    if cached.token_key == token_identity.key {
        Some(cached.account)
    } else {
        None
    }
}

fn write_cached_profile_identity(token_identity: &AccountIdentity, account: &AccountIdentity) {
    let Some(path) = profile_identity_cache_path() else {
        return;
    };
    let payload = CachedProfileIdentity {
        token_key: token_identity.key.clone(),
        account: account.clone(),
        ts: Utc::now().timestamp(),
    };
    if let Ok(body) = serde_json::to_string(&payload) {
        let _ = std::fs::write(path, body);
    }
}

fn profile_account_identity(profile: &ProfileResponse) -> Option<AccountIdentity> {
    let mut parts = Vec::new();
    if let Some(org_id) = profile
        .organization
        .as_ref()
        .and_then(|org| org.uuid.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        parts.push(format!("org:{org_id}"));
    }
    if let Some(account_id) = profile
        .account
        .as_ref()
        .and_then(|acct| acct.uuid.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        parts.push(format!("account:{account_id}"));
    }
    if let Some(email) = profile
        .account
        .as_ref()
        .and_then(|acct| acct.email.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        parts.push(format!("email:{}", email.to_ascii_lowercase()));
    }
    if parts.is_empty() {
        return None;
    }
    crate::account::explicit_identity("claude", &parts.join("|"), "claude-oauth-profile")
}

fn fetch_oauth_profile(token: &str) -> Result<ProfileResponse> {
    let resp = ureq::get("https://api.anthropic.com/api/oauth/profile")
        .set("Authorization", &format!("Bearer {token}"))
        .set("anthropic-version", "2023-06-01")
        .set("anthropic-beta", "oauth-2025-04-20")
        .timeout(std::time::Duration::from_secs(8))
        .call()
        .map_err(|e| anyhow!("call /api/oauth/profile: {e}"))?;
    resp.into_json()
        .map_err(|e| anyhow!("decode profile response: {e}"))
}

pub(super) fn active_auth_status_identity() -> Option<AccountIdentity> {
    #[cfg(windows)]
    let mut cmd = {
        let mut cmd = Command::new("cmd.exe");
        cmd.args(["/C", "claude", "auth", "status", "--json"]);
        cmd
    };

    #[cfg(not(windows))]
    let mut cmd = {
        let mut cmd = Command::new("claude");
        cmd.args(["auth", "status", "--json"]);
        cmd
    };

    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    let output = cmd.output().ok()?;
    if !output.status.success() {
        return None;
    }
    let status: AuthStatus = serde_json::from_slice(&output.stdout).ok()?;
    if !status.logged_in {
        return None;
    }
    let mut parts = Vec::new();
    if let Some(org_id) = status
        .org_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        parts.push(format!("org:{org_id}"));
    }
    if let Some(email) = status
        .email
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        parts.push(format!("email:{}", email.to_ascii_lowercase()));
    }
    if parts.is_empty() {
        if let Some(method) = status
            .auth_method
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            parts.push(format!("auth:{method}"));
        }
    }
    if parts.is_empty() {
        return None;
    }
    let raw = parts.join("|");
    let source = status
        .subscription_type
        .as_deref()
        .map(|sub| format!("claude-auth-status:{sub}"))
        .unwrap_or_else(|| "claude-auth-status".to_string());
    crate::account::explicit_identity("claude", &raw, &source)
}

/// Lightweight availability probe: does a parseable credentials file with an
/// access token exist on disk? Does NOT hit the network — refresh logic lives
/// in `read_oauth_token` / `ensure_fresh_access_token` and only runs when we
/// actually need a token to call an API. Splitting these prevents the
/// `is_available()` check from going dark whenever the refresh endpoint blips
/// (429, network glitch, etc) — in that situation we still want the Claude
/// card visible with stale-cache data, not hidden entirely.
pub(crate) fn has_credentials() -> bool {
    match read_credentials() {
        Ok((_, creds)) => !creds.claude_ai_oauth.access_token.is_empty(),
        Err(_) => false,
    }
}

/// Fetch the user's Claude subscription tier identifier from /api/oauth/profile.
/// Cached aggressively (24h) since it changes rarely.
pub fn fetch_plan_tier() -> Result<String> {
    let active_account = active_account_identity().map(|id| id.key);
    let cache_path = disk_cache_path().map(|mut p| {
        p.set_file_name("claude-plan.json");
        p
    });
    if let Some(p) = &cache_path {
        if let Ok(s) = std::fs::read_to_string(p) {
            if let Ok(entry) = serde_json::from_str::<serde_json::Value>(&s) {
                let ts = entry.get("ts").and_then(|v| v.as_i64()).unwrap_or(0);
                let age = Utc::now().timestamp() - ts;
                let cache_account = entry.get("account").and_then(|v| v.as_str());
                if age < 86_400 && cache_account == active_account.as_deref() {
                    if let Some(t) = entry.get("tier").and_then(|v| v.as_str()) {
                        return Ok(t.to_string());
                    }
                }
            }
        }
    }
    let token = read_oauth_token()?;
    let token_identity = token_account_identity(&token);
    let body = fetch_oauth_profile(&token)?;
    let profile_identity = profile_account_identity(&body);
    if let (Some(token_id), Some(profile_id)) = (token_identity.as_ref(), profile_identity.as_ref())
    {
        write_cached_profile_identity(token_id, profile_id);
    }
    let tier = body
        .organization
        .and_then(|o| o.rate_limit_tier)
        .unwrap_or_else(|| "unknown".to_string());
    if let Some(p) = cache_path {
        let payload = serde_json::json!({
            "ts": Utc::now().timestamp(),
            "tier": tier,
            "account": active_account,
        });
        let _ = std::fs::write(p, payload.to_string());
    }
    Ok(tier)
}

pub(crate) fn http_fetch_live_limits() -> FetchOutcome {
    let token = match read_oauth_token() {
        Ok(t) => t,
        Err(e) => return FetchOutcome::Other(e),
    };
    let token_identity = token_account_identity(&token);
    let profile_identity = match fetch_oauth_profile(&token) {
        Ok(profile) => {
            let identity = profile_account_identity(&profile);
            if let (Some(token_id), Some(profile_id)) = (token_identity.as_ref(), identity.as_ref())
            {
                write_cached_profile_identity(token_id, profile_id);
            }
            identity
        }
        Err(e) => {
            eprintln!("[tally] claude OAuth profile identity failed ({e}); using token identity");
            None
        }
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
        // 401 here means our (just-refreshed-if-needed) token was rejected.
        // Most likely cause: refresh_token revoked server-side. Bubble up
        // so the caller falls through to CLI parsing.
        Err(e) => return FetchOutcome::Other(anyhow!("call /api/oauth/usage: {e}")),
    };
    let body: UsageResponse = match resp.into_json() {
        Ok(b) => b,
        Err(e) => return FetchOutcome::Other(anyhow!("decode usage response: {e}")),
    };

    let mut live = live_limits_from_usage_response(body, ClaudeLimitSource::Oauth);
    live.account = profile_identity
        .or(token_identity)
        .or_else(active_auth_status_identity);
    FetchOutcome::Ok(live)
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn uses_claude_code_platform_refresh_endpoint() {
        assert_eq!(
            CLAUDE_OAUTH_TOKEN_URL,
            "https://platform.claude.com/v1/oauth/token"
        );
        assert_eq!(
            CLAUDE_OAUTH_CLIENT_ID,
            "9d1c250a-e61b-44d9-88ed-5944d1962f5e"
        );
    }

    #[test]
    fn parses_nested_claude_credentials_and_preserves_metadata() {
        let raw = json!({
            "claudeAiOauth": {
                "accessToken": "access",
                "refreshToken": "refresh",
                "expiresAt": Utc::now().timestamp_millis() + 3_600_000,
                "scopes": ["user:profile"],
                "subscriptionType": "max",
                "rateLimitTier": "default_claude_max_5x"
            },
            "mcpOAuth": {
                "someServer": {
                    "accessToken": "mcp"
                }
            }
        });

        let creds: CredentialsFile = serde_json::from_value(raw).unwrap();

        assert_eq!(creds.claude_ai_oauth.access_token, "access");
        assert_eq!(
            creds.claude_ai_oauth.refresh_token.as_deref(),
            Some("refresh")
        );
        assert_eq!(
            creds
                .claude_ai_oauth
                .extra
                .get("subscriptionType")
                .and_then(|v| v.as_str()),
            Some("max")
        );
        assert!(creds.other.contains_key("mcpOAuth"));
    }

    #[test]
    fn treats_expiry_as_epoch_millis_with_refresh_margin() {
        let fresh = OauthBlock {
            access_token: "access".to_string(),
            refresh_token: Some("refresh".to_string()),
            expires_at: Some(Utc::now().timestamp_millis() + 3_600_000),
            extra: serde_json::Map::new(),
        };
        let expired = OauthBlock {
            expires_at: Some(Utc::now().timestamp_millis() - 1),
            ..fresh.clone()
        };
        let inside_margin = OauthBlock {
            expires_at: Some(Utc::now().timestamp_millis() + 30_000),
            ..fresh.clone()
        };

        assert!(!is_token_expired(&fresh));
        assert!(is_token_expired(&expired));
        assert!(is_token_expired(&inside_margin));
    }
}
