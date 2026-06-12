use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::Aes256Gcm;
use anyhow::{anyhow, Result};
use base64::Engine;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

use super::api::{live_limits_from_usage_response, UsageResponse};
use super::types::{ClaudeLimitSource, FetchOutcome};
use crate::account::AccountIdentity;

const TOKEN_REFRESH_MARGIN_SECS: i64 = 60;

#[derive(Debug, Deserialize)]
struct LocalState {
    os_crypt: OsCrypt,
}

#[derive(Debug, Deserialize)]
struct OsCrypt {
    encrypted_key: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DesktopTokenEntry {
    token: Option<String>,
    expires_at: Option<i64>,
    subscription_type: Option<String>,
    rate_limit_tier: Option<String>,
}

#[derive(Debug, Clone)]
struct DesktopToken {
    token: String,
    client_id: String,
    org_id: String,
    scope: String,
    subscription_type: Option<String>,
    rate_limit_tier: Option<String>,
}

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

#[derive(Debug, Clone, Deserialize, Serialize)]
struct CachedProfileIdentity {
    token_key: String,
    account: AccountIdentity,
    ts: i64,
}

pub(crate) fn has_credentials() -> bool {
    read_desktop_token().is_ok()
}

pub(crate) fn active_account_identity() -> Option<AccountIdentity> {
    let token = read_desktop_token().ok()?;
    let token_identity = token_account_identity(&token);
    if let Some(token_id) = token_identity.as_ref() {
        if let Some(profile_id) = read_cached_profile_identity(token_id) {
            return Some(profile_id);
        }
    }
    token_identity
}

pub(crate) fn fetch_plan_tier() -> Result<String> {
    let token = read_desktop_token()?;
    if let Some(tier) = token
        .rate_limit_tier
        .as_deref()
        .filter(|s| !s.trim().is_empty())
    {
        return Ok(tier.to_string());
    }
    let profile = fetch_desktop_profile(&token.token)?;
    Ok(profile
        .organization
        .and_then(|org| org.rate_limit_tier)
        .unwrap_or_else(|| "unknown".to_string()))
}

pub(crate) fn desktop_fetch_live_limits() -> FetchOutcome {
    let token = match read_desktop_token() {
        Ok(token) => token,
        Err(e) => return FetchOutcome::Other(e),
    };
    let token_identity = token_account_identity(&token);
    let profile_identity = match fetch_desktop_profile(&token.token) {
        Ok(profile) => {
            let identity = profile_account_identity(&profile);
            if let (Some(token_id), Some(profile_id)) = (token_identity.as_ref(), identity.as_ref())
            {
                write_cached_profile_identity(token_id, profile_id);
            }
            identity
        }
        Err(e) => {
            eprintln!(
                "[tally] Claude Desktop profile identity failed ({e}); using Desktop token identity"
            );
            None
        }
    };

    let result = ureq::get("https://api.anthropic.com/api/oauth/usage")
        .set("Authorization", &format!("Bearer {}", token.token))
        .set("Accept", "application/json")
        .set("anthropic-version", "2023-06-01")
        .set("anthropic-beta", "oauth-2025-04-20")
        .set("User-Agent", "Tally/ClaudeDesktop")
        .timeout(std::time::Duration::from_secs(8))
        .call();
    let resp = match result {
        Ok(resp) => resp,
        Err(ureq::Error::Status(429, _)) => {
            return FetchOutcome::RateLimited(
                "Anthropic Desktop /api/oauth/usage returned 429 - backing off".to_string(),
            );
        }
        Err(e) => return FetchOutcome::Other(anyhow!("call Desktop /api/oauth/usage: {e}")),
    };
    let body: UsageResponse = match resp.into_json() {
        Ok(body) => body,
        Err(e) => return FetchOutcome::Other(anyhow!("decode Desktop usage response: {e}")),
    };

    let mut live = live_limits_from_usage_response(body, ClaudeLimitSource::Desktop);
    live.account = profile_identity.or(token_identity);
    FetchOutcome::Ok(live)
}

fn read_desktop_token() -> Result<DesktopToken> {
    let root = desktop_root().ok_or_else(|| anyhow!("Claude Desktop storage not found"))?;
    let local_state = read_json::<LocalState>(root.join("Local State"))?;
    let config = read_json::<serde_json::Value>(root.join("config.json"))?;
    let encrypted = config
        .get("oauth:tokenCache")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| anyhow!("Claude Desktop oauth:tokenCache missing"))?;
    let key = decrypt_os_crypt_key(&local_state.os_crypt.encrypted_key)?;
    let plaintext = decrypt_chromium_v10(&key, encrypted)?;
    let cache: HashMap<String, DesktopTokenEntry> = serde_json::from_slice(&plaintext)
        .map_err(|e| anyhow!("parse Claude Desktop token cache: {e}"))?;
    select_desktop_token(&cache).ok_or_else(|| anyhow!("no usable Claude Desktop OAuth token"))
}

fn desktop_root() -> Option<PathBuf> {
    if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") {
        let packages = PathBuf::from(local_app_data).join("Packages");
        if let Ok(entries) = std::fs::read_dir(packages) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
                if name.starts_with("claude_") {
                    let root = entry
                        .path()
                        .join("LocalCache")
                        .join("Roaming")
                        .join("Claude");
                    if root.join("config.json").is_file() && root.join("Local State").is_file() {
                        return Some(root);
                    }
                }
            }
        }
    }
    None
}

fn read_json<T: for<'de> Deserialize<'de>>(path: PathBuf) -> Result<T> {
    let raw =
        std::fs::read_to_string(&path).map_err(|e| anyhow!("read {}: {e}", path.display()))?;
    serde_json::from_str(&raw).map_err(|e| anyhow!("parse {}: {e}", path.display()))
}

fn select_desktop_token(cache: &HashMap<String, DesktopTokenEntry>) -> Option<DesktopToken> {
    cache
        .iter()
        .filter_map(|(key, entry)| token_candidate(key, entry))
        .max_by_key(candidate_score)
}

fn token_candidate(key: &str, entry: &DesktopTokenEntry) -> Option<DesktopToken> {
    let mut parts = key.splitn(3, ':');
    let client_id = parts.next()?.to_string();
    let org_id = parts.next()?.to_string();
    let scope = parts.next()?.to_string();
    if !scope.contains("https://api.anthropic.com") || !scope.contains("user:profile") {
        return None;
    }
    if scope.contains("user:office") {
        return None;
    }
    if is_entry_expired(entry) {
        return None;
    }
    let token = entry.token.as_deref()?.trim();
    if token.is_empty() {
        return None;
    }
    Some(DesktopToken {
        token: token.to_string(),
        client_id,
        org_id,
        scope,
        subscription_type: entry.subscription_type.clone(),
        rate_limit_tier: entry.rate_limit_tier.clone(),
    })
}

fn candidate_score(token: &DesktopToken) -> i32 {
    let mut score = 0;
    if token.scope.contains("user:profile") {
        score += 10;
    }
    // The plain profile grant is longer-lived and maps the visible Desktop
    // account; keep claude_code as a valid fallback when it is the only grant.
    if !token.scope.contains("claude_code") {
        score += 5;
    }
    if token.subscription_type.is_some() {
        score += 2;
    }
    if token.rate_limit_tier.is_some() {
        score += 2;
    }
    score
}

fn is_entry_expired(entry: &DesktopTokenEntry) -> bool {
    match entry.expires_at {
        Some(exp_ms) => {
            Utc::now().timestamp_millis() + (TOKEN_REFRESH_MARGIN_SECS * 1000) >= exp_ms
        }
        None => false,
    }
}

fn decrypt_os_crypt_key(encrypted_key: &str) -> Result<Vec<u8>> {
    let raw = base64::engine::general_purpose::STANDARD
        .decode(encrypted_key)
        .map_err(|e| anyhow!("decode Desktop os_crypt key: {e}"))?;
    let payload = raw.strip_prefix(b"DPAPI").unwrap_or(&raw);
    dpapi_unprotect(payload)
}

fn decrypt_chromium_v10(key: &[u8], encrypted_value: &str) -> Result<Vec<u8>> {
    let raw = base64::engine::general_purpose::STANDARD
        .decode(encrypted_value)
        .map_err(|e| anyhow!("decode Desktop token cache: {e}"))?;
    if raw.len() < 16 || &raw[..3] != b"v10" {
        return Err(anyhow!("unsupported Desktop token cache cipher"));
    }
    let cipher = Aes256Gcm::new_from_slice(key)
        .map_err(|_| anyhow!("invalid Desktop os_crypt key length"))?;
    let nonce = aes_gcm::Nonce::from_slice(&raw[3..15]);
    cipher
        .decrypt(nonce, &raw[15..])
        .map_err(|_| anyhow!("decrypt Desktop token cache"))
}

#[cfg(windows)]
fn dpapi_unprotect(data: &[u8]) -> Result<Vec<u8>> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{CryptUnprotectData, CRYPT_INTEGER_BLOB};

    let input = CRYPT_INTEGER_BLOB {
        cbData: data.len() as u32,
        pbData: data.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    let ok = unsafe {
        CryptUnprotectData(
            &input,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0,
            &mut output,
        )
    };
    if ok == 0 {
        return Err(anyhow!(
            "decrypt Desktop os_crypt key: {}",
            std::io::Error::last_os_error()
        ));
    }
    let bytes = unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) };
    let out = bytes.to_vec();
    unsafe {
        let _ = LocalFree(output.pbData as *mut _);
    }
    Ok(out)
}

#[cfg(not(windows))]
fn dpapi_unprotect(_data: &[u8]) -> Result<Vec<u8>> {
    Err(anyhow!(
        "Claude Desktop token cache decrypt is Windows-only"
    ))
}

fn fetch_desktop_profile(token: &str) -> Result<ProfileResponse> {
    let resp = ureq::get("https://api.anthropic.com/api/oauth/profile")
        .set("Authorization", &format!("Bearer {token}"))
        .set("anthropic-version", "2023-06-01")
        .set("anthropic-beta", "oauth-2025-04-20")
        .set("User-Agent", "Tally/ClaudeDesktop")
        .timeout(std::time::Duration::from_secs(8))
        .call()
        .map_err(|e| anyhow!("call Desktop /api/oauth/profile: {e}"))?;
    resp.into_json()
        .map_err(|e| anyhow!("decode Desktop profile response: {e}"))
}

fn token_account_identity(token: &DesktopToken) -> Option<AccountIdentity> {
    crate::account::explicit_identity(
        "claude",
        &format!(
            "desktop|client:{}|org:{}|scope:{}",
            token.client_id, token.org_id, token.scope
        ),
        "claude-desktop-token",
    )
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
    crate::account::explicit_identity("claude", &parts.join("|"), "claude-desktop-profile")
}

fn profile_identity_cache_path() -> Option<PathBuf> {
    let mut p = dirs::cache_dir()?;
    p.push("tally");
    let _ = std::fs::create_dir_all(&p);
    p.push("claude-desktop-account.json");
    Some(p)
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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn entry(token: &str, scope: &str) -> (String, DesktopTokenEntry) {
        (
            format!(
                "client-1:org-1:https://api.anthropic.com:user:inference user:file_upload user:profile {scope}"
            ),
            DesktopTokenEntry {
                token: Some(token.to_string()),
                expires_at: Some((Utc::now() + Duration::hours(1)).timestamp_millis()),
                subscription_type: Some("pro".to_string()),
                rate_limit_tier: Some("default_claude_ai".to_string()),
            },
        )
    }

    #[test]
    fn selects_desktop_profile_token_over_claude_code_grant() {
        let mut cache = HashMap::new();
        let (code_key, code_entry) = entry("code-token", "user:sessions:claude_code");
        let (profile_key, profile_entry) = entry("profile-token", "");
        cache.insert(code_key, code_entry);
        cache.insert(profile_key, profile_entry);

        let selected = select_desktop_token(&cache).unwrap();
        assert_eq!(selected.token, "profile-token");
        assert_eq!(selected.org_id, "org-1");
        assert_eq!(
            selected.rate_limit_tier.as_deref(),
            Some("default_claude_ai")
        );
    }

    #[test]
    fn skips_office_and_expired_desktop_tokens() {
        let mut cache = HashMap::new();
        cache.insert(
            "client-1:org-1:https://api.anthropic.com:user:inference user:office".to_string(),
            DesktopTokenEntry {
                token: Some("office-token".to_string()),
                expires_at: Some((Utc::now() + Duration::hours(1)).timestamp_millis()),
                subscription_type: None,
                rate_limit_tier: None,
            },
        );
        let (expired_key, mut expired_entry) = entry("expired-token", "");
        expired_entry.expires_at = Some((Utc::now() - Duration::minutes(5)).timestamp_millis());
        cache.insert(expired_key, expired_entry);

        assert!(select_desktop_token(&cache).is_none());
    }
}
