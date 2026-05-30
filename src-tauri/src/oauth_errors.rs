//! Shared OAuth token-refresh error classification.
//!
//! Both the Claude (`platform.claude.com`) and Codex (`auth.openai.com`)
//! refresh endpoints return OAuth 2.0-shaped error bodies. Rather than surface
//! an opaque "status 400" we parse the error code and route it to an
//! actionable message — distinguishing a refresh token that's permanently dead
//! (re-login required) from a transient failure (retry will succeed).
//!
//! Pattern mirrored from CodexBar's `CodexTokenRefresher.refreshFailureError`
//! (steipete/CodexBar), generalized to RFC 6749 standard codes so it works
//! against Anthropic's endpoint too.

/// Whether a refresh failure means the stored refresh token is permanently
/// unusable (user must re-authenticate) vs. a transient condition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshFailureKind {
    /// Refresh token expired, revoked, or already consumed — re-login required.
    NeedsRelogin,
    /// OAuth client mismatch — the app build itself may be stale.
    ClientRejected,
    /// Transient (rate limit, 5xx, network) — a later retry should recover.
    Transient,
    /// Anything else — surfaced verbatim with the status code.
    Unknown,
}

/// Extract an OAuth error code from a refresh-endpoint error body. Handles the
/// three shapes seen in the wild:
/// - `{ "error": "invalid_grant" }`              (RFC 6749 standard)
/// - `{ "error": { "code": "refresh_token_…" } }` (OpenAI-style nested)
/// - `{ "code": "…" }`                            (bare)
pub fn extract_oauth_error_code(body: &str) -> Option<String> {
    let json: serde_json::Value = serde_json::from_str(body).ok()?;
    if let Some(code) = json
        .get("error")
        .and_then(|e| e.get("code"))
        .and_then(|c| c.as_str())
    {
        return Some(code.to_string());
    }
    if let Some(err) = json.get("error").and_then(|e| e.as_str()) {
        return Some(err.to_string());
    }
    json.get("code")
        .and_then(|c| c.as_str())
        .map(str::to_string)
}

/// Classify a refresh failure by HTTP status + body. Returns the failure kind
/// (so callers can decide whether to keep retrying) — the human message is
/// built separately by [`refresh_error_message`].
pub fn classify_refresh_failure(status: u16, body: &str) -> RefreshFailureKind {
    if let Some(code) = extract_oauth_error_code(body) {
        match code.to_ascii_lowercase().as_str() {
            "invalid_grant"
            | "refresh_token_expired"
            | "refresh_token_invalidated"
            | "refresh_token_reused" => return RefreshFailureKind::NeedsRelogin,
            "invalid_client" | "unauthorized_client" => return RefreshFailureKind::ClientRejected,
            _ => {}
        }
    }
    match status {
        401 | 403 => RefreshFailureKind::NeedsRelogin,
        408 | 429 => RefreshFailureKind::Transient,
        500..=599 => RefreshFailureKind::Transient,
        _ => RefreshFailureKind::Unknown,
    }
}

/// Build an actionable, human-readable message for a refresh failure.
/// `relogin_cmd` is the CLI the user runs to re-authenticate (`claude` / `codex`).
pub fn refresh_error_message(status: u16, body: &str, relogin_cmd: &str) -> String {
    match classify_refresh_failure(status, body) {
        RefreshFailureKind::NeedsRelogin => {
            // Distinguish the "reused" sub-case since it points at a different
            // root cause (a second tool rotating the same token).
            if extract_oauth_error_code(body)
                .map(|c| c.eq_ignore_ascii_case("refresh_token_reused"))
                .unwrap_or(false)
            {
                format!(
                    "refresh token already used (another tool rotated it) — run `{relogin_cmd}` to log in again"
                )
            } else {
                format!("refresh token expired or revoked — run `{relogin_cmd}` to log in again")
            }
        }
        RefreshFailureKind::ClientRejected => {
            "OAuth client rejected — Tally may need an update".to_string()
        }
        RefreshFailureKind::Transient => {
            format!("token refresh temporarily failed (status {status}) — will retry")
        }
        RefreshFailureKind::Unknown => {
            let code = extract_oauth_error_code(body)
                .map(|c| format!(" ({c})"))
                .unwrap_or_default();
            format!("token refresh failed (status {status}){code}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- extract_oauth_error_code: every body shape ----
    #[test]
    fn extracts_standard_string_error() {
        assert_eq!(
            extract_oauth_error_code(r#"{"error":"invalid_grant"}"#).as_deref(),
            Some("invalid_grant")
        );
    }

    #[test]
    fn extracts_nested_code_error() {
        assert_eq!(
            extract_oauth_error_code(r#"{"error":{"code":"refresh_token_expired"}}"#).as_deref(),
            Some("refresh_token_expired")
        );
    }

    #[test]
    fn extracts_bare_code() {
        assert_eq!(
            extract_oauth_error_code(r#"{"code":"refresh_token_reused"}"#).as_deref(),
            Some("refresh_token_reused")
        );
    }

    #[test]
    fn malformed_json_yields_none() {
        assert_eq!(extract_oauth_error_code("not json at all"), None);
        assert_eq!(extract_oauth_error_code(""), None);
        assert_eq!(extract_oauth_error_code("{}"), None);
        // Array body (valid JSON, no error field)
        assert_eq!(extract_oauth_error_code("[1,2,3]"), None);
    }

    // ---- classify_refresh_failure: code-driven ----
    #[test]
    fn invalid_grant_needs_relogin() {
        assert_eq!(
            classify_refresh_failure(400, r#"{"error":"invalid_grant"}"#),
            RefreshFailureKind::NeedsRelogin
        );
    }

    #[test]
    fn reused_needs_relogin() {
        assert_eq!(
            classify_refresh_failure(400, r#"{"error":{"code":"refresh_token_reused"}}"#),
            RefreshFailureKind::NeedsRelogin
        );
    }

    #[test]
    fn invalid_client_is_client_rejected() {
        assert_eq!(
            classify_refresh_failure(401, r#"{"error":"invalid_client"}"#),
            RefreshFailureKind::ClientRejected
        );
    }

    // ---- classify_refresh_failure: status-driven fallbacks ----
    #[test]
    fn bare_401_without_body_needs_relogin() {
        assert_eq!(
            classify_refresh_failure(401, ""),
            RefreshFailureKind::NeedsRelogin
        );
    }

    #[test]
    fn rate_limit_is_transient() {
        assert_eq!(
            classify_refresh_failure(429, ""),
            RefreshFailureKind::Transient
        );
    }

    #[test]
    fn server_error_is_transient() {
        assert_eq!(
            classify_refresh_failure(503, ""),
            RefreshFailureKind::Transient
        );
        assert_eq!(
            classify_refresh_failure(500, "{}"),
            RefreshFailureKind::Transient
        );
    }

    #[test]
    fn unknown_status_is_unknown() {
        assert_eq!(
            classify_refresh_failure(418, ""),
            RefreshFailureKind::Unknown
        );
    }

    // Critical edge: a 429 that ALSO carries invalid_grant must be treated as
    // needs-relogin (the code wins over the status heuristic), otherwise we'd
    // retry a permanently-dead token forever.
    #[test]
    fn code_wins_over_status_heuristic() {
        assert_eq!(
            classify_refresh_failure(429, r#"{"error":"invalid_grant"}"#),
            RefreshFailureKind::NeedsRelogin
        );
    }

    // ---- refresh_error_message: actionable text ----
    #[test]
    fn relogin_message_names_the_cli() {
        let msg = refresh_error_message(400, r#"{"error":"invalid_grant"}"#, "claude");
        assert!(msg.contains("claude"), "got: {msg}");
        assert!(msg.contains("log in again"), "got: {msg}");
    }

    #[test]
    fn reused_message_is_distinct() {
        let msg = refresh_error_message(400, r#"{"error":"refresh_token_reused"}"#, "codex");
        assert!(msg.contains("another tool"), "got: {msg}");
        assert!(msg.contains("codex"), "got: {msg}");
    }

    #[test]
    fn transient_message_says_will_retry() {
        let msg = refresh_error_message(429, "", "claude");
        assert!(msg.contains("will retry"), "got: {msg}");
    }

    #[test]
    fn unknown_message_includes_status_and_code() {
        let msg = refresh_error_message(418, r#"{"error":"teapot"}"#, "claude");
        assert!(msg.contains("418"), "got: {msg}");
        assert!(msg.contains("teapot"), "got: {msg}");
    }
}
