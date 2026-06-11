#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct AccountIdentity {
    pub key: String,
    pub label: String,
    pub source: String,
}

impl AccountIdentity {
    pub fn new(key: String, label: String, source: impl Into<String>) -> Self {
        Self {
            key,
            label,
            source: source.into(),
        }
    }
}

pub fn token_identity(provider: &str, token: &str, source: &str) -> Option<AccountIdentity> {
    let token = token.trim();
    if token.is_empty() {
        return None;
    }
    let fp = stable_fingerprint(token);
    Some(AccountIdentity::new(
        format!("{provider}:token:{fp}"),
        format!("acct {}", &fp[..8]),
        source,
    ))
}

pub fn explicit_identity(provider: &str, id: &str, source: &str) -> Option<AccountIdentity> {
    let id = id.trim();
    if id.is_empty() {
        return None;
    }
    let fp = stable_fingerprint(id);
    Some(AccountIdentity::new(
        format!("{provider}:id:{fp}"),
        format!("acct {}", &fp[..8]),
        source,
    ))
}

fn stable_fingerprint(input: &str) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in input.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_identity_does_not_expose_secret() {
        let id = token_identity("claude", "secret-token-value", "test").unwrap();
        assert!(id.key.starts_with("claude:token:"));
        assert!(!id.key.contains("secret"));
        assert!(id.label.starts_with("acct "));
    }

    #[test]
    fn blank_identity_is_none() {
        assert!(token_identity("claude", "   ", "test").is_none());
        assert!(explicit_identity("codex", "", "test").is_none());
    }
}
