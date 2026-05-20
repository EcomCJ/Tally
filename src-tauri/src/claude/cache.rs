use anyhow::{anyhow, Result};
use chrono::{TimeZone, Utc};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use super::cli::fetch_cli_usage_limits;
use super::history::record_limit_sample;
use super::oauth::http_fetch_live_limits;
use super::types::{ClaudeLimitSource, ClaudeLiveLimits, FetchOutcome};
use super::web::web_fetch_live_limits;

const MIN_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(60);
const RATE_LIMIT_BACKOFF: std::time::Duration = std::time::Duration::from_secs(900);

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct DiskCache {
    fetched_at_unix: i64,
    #[serde(default)]
    source: ClaudeLimitSource,
    five_hour_percent: f64,
    five_hour_resets_at: Option<chrono::DateTime<Utc>>,
    weekly_percent: f64,
    weekly_resets_at: Option<chrono::DateTime<Utc>>,
}

struct CacheEntry {
    fetched_at: Instant,
    value: ClaudeLiveLimits,
    cooldown_until: Option<Instant>,
    last_error: Option<String>,
}

fn cache() -> &'static Mutex<Option<CacheEntry>> {
    static C: OnceLock<Mutex<Option<CacheEntry>>> = OnceLock::new();
    C.get_or_init(|| {
        let seed = read_disk_cache().map(|d| CacheEntry {
            // Mark as old enough to refresh so the first call still tries live data.
            fetched_at: Instant::now()
                .checked_sub(std::time::Duration::from_secs(3600))
                .unwrap_or_else(Instant::now),
            value: ClaudeLiveLimits {
                source: d.source,
                fetched_at: Utc
                    .timestamp_opt(d.fetched_at_unix, 0)
                    .single()
                    .unwrap_or_else(Utc::now),
                five_hour_percent: d.five_hour_percent,
                five_hour_resets_at: d.five_hour_resets_at,
                weekly_percent: d.weekly_percent,
                weekly_resets_at: d.weekly_resets_at,
                sub_quotas: Vec::new(),
                extra_usage: None,
            },
            cooldown_until: None,
            last_error: None,
        });
        Mutex::new(seed)
    })
}

pub(crate) fn disk_cache_path() -> Option<std::path::PathBuf> {
    let mut p = dirs::cache_dir()?;
    p.push("tally");
    let _ = std::fs::create_dir_all(&p);
    p.push("claude-usage-cache.json");
    Some(p)
}

fn legacy_disk_cache_paths() -> Vec<std::path::PathBuf> {
    let mut paths = Vec::new();

    if let Some(mut p) = dirs::cache_dir() {
        p.push("usage-widget");
        p.push("claude-usage-cache.json");
        paths.push(p);
    }

    if let Some(mut local) = dirs::data_local_dir() {
        local.push("usage-widget");
        local.push("claude-usage-cache.json");
        paths.push(local);
    }

    if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") {
        let mut packages = std::path::PathBuf::from(local_app_data);
        packages.push("Packages");
        if let Ok(entries) = std::fs::read_dir(packages) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.to_ascii_lowercase().contains("claude") {
                    let mut p = entry.path();
                    p.push("LocalCache");
                    p.push("Local");
                    p.push("usage-widget");
                    p.push("claude-usage-cache.json");
                    paths.push(p);
                }
            }
        }
    }

    paths
}

fn read_disk_cache() -> Option<DiskCache> {
    let mut paths = Vec::new();
    if let Some(path) = disk_cache_path() {
        paths.push(path);
    }
    paths.extend(legacy_disk_cache_paths());

    for path in paths {
        let s = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(_) => continue,
        };
        if let Ok(cache) = serde_json::from_str::<DiskCache>(&s) {
            return Some(cache);
        }
    }
    None
}

fn write_disk_cache(value: &ClaudeLiveLimits) {
    if let Some(path) = disk_cache_path() {
        let d = DiskCache {
            fetched_at_unix: value.fetched_at.timestamp(),
            source: value.source,
            five_hour_percent: value.five_hour_percent,
            five_hour_resets_at: value.five_hour_resets_at,
            weekly_percent: value.weekly_percent,
            weekly_resets_at: value.weekly_resets_at,
        };
        if let Ok(s) = serde_json::to_string(&d) {
            let _ = std::fs::write(path, s);
        }
    }
}

pub fn fetch_live_limits(refresh_ms: u64) -> Result<ClaudeLiveLimits> {
    let ttl = std::time::Duration::from_millis(refresh_ms).max(MIN_CACHE_TTL);
    let now_inst = Instant::now();
    {
        let guard = cache().lock().unwrap();
        if let Some(entry) = guard.as_ref() {
            if should_serve_cached(now_inst, entry.fetched_at, entry.cooldown_until, ttl) {
                return Ok(entry.value.clone());
            }
        }
    }
    let active_cooldown = {
        let guard = cache().lock().unwrap();
        guard
            .as_ref()
            .and_then(|entry| entry.cooldown_until)
            .filter(|until| now_inst < *until)
    };
    let should_skip_oauth = active_cooldown.is_some();
    let mut cooldown_after_success = active_cooldown;

    let outcome = if should_skip_oauth {
        fetch_cli_usage_limits()
            .map(FetchOutcome::Ok)
            .unwrap_or_else(|cli_err| match web_fetch_live_limits() {
                FetchOutcome::Ok(web) => FetchOutcome::Ok(web),
                FetchOutcome::RateLimited(msg) => FetchOutcome::RateLimited(msg),
                FetchOutcome::Other(web_err) => FetchOutcome::Other(anyhow!(
                    "CLI failed during OAuth cooldown ({cli_err}); Web failed ({web_err})"
                )),
            })
    } else {
        match http_fetch_live_limits() {
            FetchOutcome::Ok(fresh) => FetchOutcome::Ok(fresh),
            FetchOutcome::RateLimited(msg) => {
                eprintln!("[tally] {msg}; trying Claude CLI fallback");
                cooldown_after_success = Some(Instant::now() + RATE_LIMIT_BACKOFF);
                fetch_cli_usage_limits()
                    .map(FetchOutcome::Ok)
                    .unwrap_or_else(|cli_err| match web_fetch_live_limits() {
                        FetchOutcome::Ok(web) => FetchOutcome::Ok(web),
                        FetchOutcome::RateLimited(web_msg) => FetchOutcome::RateLimited(web_msg),
                        FetchOutcome::Other(web_err) => {
                            eprintln!(
                                "[tally] claude CLI fallback failed ({cli_err}); web fallback failed ({web_err})"
                            );
                            FetchOutcome::RateLimited(msg)
                        }
                    })
            }
            FetchOutcome::Other(oauth_err) => {
                eprintln!(
                    "[tally] claude OAuth usage failed ({oauth_err}); trying Claude CLI fallback"
                );
                fetch_cli_usage_limits()
                    .map(FetchOutcome::Ok)
                    .unwrap_or_else(|cli_err| match web_fetch_live_limits() {
                        FetchOutcome::Ok(web) => FetchOutcome::Ok(web),
                        FetchOutcome::RateLimited(msg) => FetchOutcome::RateLimited(msg),
                        FetchOutcome::Other(web_err) => FetchOutcome::Other(anyhow!(
                            "OAuth failed ({oauth_err}); CLI failed ({cli_err}); Web failed ({web_err})"
                        )),
                    })
            }
        }
    };

    match outcome {
        FetchOutcome::Ok(fresh) => {
            let mut guard = cache().lock().unwrap();
            *guard = Some(CacheEntry {
                fetched_at: Instant::now(),
                value: fresh.clone(),
                cooldown_until: cooldown_after_success,
                last_error: None,
            });
            drop(guard);
            write_disk_cache(&fresh);
            record_limit_sample(&fresh);
            Ok(fresh)
        }
        FetchOutcome::RateLimited(msg) => {
            eprintln!("[tally] {msg} - cooldown {}s", RATE_LIMIT_BACKOFF.as_secs());
            let mut guard = cache().lock().unwrap();
            if let Some(entry) = guard.as_mut() {
                entry.fetched_at = Instant::now();
                entry.cooldown_until = Some(Instant::now() + RATE_LIMIT_BACKOFF);
                entry.last_error = Some(msg);
                let mut cached = entry.value.clone();
                cached.source = ClaudeLimitSource::Cache;
                Ok(cached)
            } else {
                Err(anyhow!(msg))
            }
        }
        FetchOutcome::Other(e) => {
            let mut guard = cache().lock().unwrap();
            if let Some(entry) = guard.as_mut() {
                eprintln!("[tally] claude live fetch failed ({e}); using cached value");
                entry.fetched_at = Instant::now();
                entry.last_error = Some(e.to_string());
                let mut cached = entry.value.clone();
                cached.source = ClaudeLimitSource::Cache;
                Ok(cached)
            } else {
                Err(e)
            }
        }
    }
}

fn should_serve_cached(
    now: Instant,
    fetched_at: Instant,
    cooldown_until: Option<Instant>,
    ttl: std::time::Duration,
) -> bool {
    let fresh_enough = now.duration_since(fetched_at) < ttl;
    match cooldown_until {
        Some(until) if now < until => fresh_enough,
        _ => fresh_enough,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_disk_cache_without_source_defaults_to_cache() {
        let raw = r#"{
            "fetched_at_unix": 1779209509,
            "five_hour_percent": 26.0,
            "five_hour_resets_at": null,
            "weekly_percent": 21.0,
            "weekly_resets_at": null
        }"#;
        let parsed: DiskCache = serde_json::from_str(raw).unwrap();

        assert_eq!(parsed.source, ClaudeLimitSource::Cache);
    }

    #[test]
    fn oauth_cooldown_does_not_block_cli_after_ttl_expires() {
        let now = Instant::now();
        let ttl = std::time::Duration::from_secs(60);
        let fetched_at = now - std::time::Duration::from_secs(90);
        let cooldown_until = Some(now + std::time::Duration::from_secs(600));

        assert!(!should_serve_cached(now, fetched_at, cooldown_until, ttl));
    }

    #[test]
    fn oauth_cooldown_serves_cache_inside_ttl() {
        let now = Instant::now();
        let ttl = std::time::Duration::from_secs(60);
        let fetched_at = now - std::time::Duration::from_secs(30);
        let cooldown_until = Some(now + std::time::Duration::from_secs(600));

        assert!(should_serve_cached(now, fetched_at, cooldown_until, ttl));
    }

    #[test]
    fn stale_disk_cache_still_deserializes_as_last_resort() {
        let raw = r#"{
            "fetched_at_unix": 1,
            "source": "oauth",
            "five_hour_percent": 12.0,
            "five_hour_resets_at": null,
            "weekly_percent": 26.0,
            "weekly_resets_at": null
        }"#;
        let parsed: DiskCache = serde_json::from_str(raw).unwrap();

        assert_eq!(parsed.fetched_at_unix, 1);
        assert_eq!(parsed.source, ClaudeLimitSource::Oauth);
    }
}
