use chrono::{DateTime, Utc};
use serde::Deserialize;

use super::types::{ClaudeLimitSource, ClaudeLiveLimits, ExtraUsageInfo, SubQuota};

#[derive(Debug, Deserialize)]
pub(crate) struct UsageResponse {
    five_hour: Option<UsageWindow>,
    seven_day: Option<UsageWindow>,
    seven_day_oauth_apps: Option<UsageWindow>,
    seven_day_fable: Option<UsageWindow>,
    seven_day_mythos: Option<UsageWindow>,
    seven_day_opus: Option<UsageWindow>,
    seven_day_sonnet: Option<UsageWindow>,
    fable: Option<UsageWindow>,
    mythos: Option<UsageWindow>,
    seven_day_design: Option<UsageWindow>,
    seven_day_claude_design: Option<UsageWindow>,
    claude_design: Option<UsageWindow>,
    design: Option<UsageWindow>,
    seven_day_routines: Option<UsageWindow>,
    seven_day_claude_routines: Option<UsageWindow>,
    claude_routines: Option<UsageWindow>,
    routines: Option<UsageWindow>,
    routine: Option<UsageWindow>,
    seven_day_cowork: Option<UsageWindow>,
    seven_day_omelette: Option<UsageWindow>,
    extra_usage: Option<ExtraUsage>,
}

#[derive(Debug, Deserialize, Clone)]
struct UsageWindow {
    utilization: f64,
    resets_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize, Clone)]
struct ExtraUsage {
    is_enabled: Option<bool>,
    monthly_limit: Option<f64>,
    used_credits: Option<f64>,
    utilization: Option<f64>,
    currency: Option<String>,
}

pub(crate) fn live_limits_from_usage_response(
    body: UsageResponse,
    source: ClaudeLimitSource,
) -> ClaudeLiveLimits {
    // Sub-quotas: include any that are present in the response (non-null).
    // Anthropic returns these whether you've used the feature or not.
    let mut sub_quotas = Vec::new();
    let push_q = |out: &mut Vec<SubQuota>, label: &str, w: &Option<UsageWindow>| {
        if let Some(w) = w {
            out.push(SubQuota {
                label: label.to_string(),
                utilization: w.utilization,
                resets_at: w.resets_at,
            });
        }
    };
    let fable = body
        .seven_day_fable
        .as_ref()
        .or(body.fable.as_ref())
        .or(body.seven_day_mythos.as_ref())
        .or(body.mythos.as_ref())
        .cloned();
    push_q(&mut sub_quotas, "Fable", &fable);
    push_q(&mut sub_quotas, "Opus", &body.seven_day_opus);
    push_q(&mut sub_quotas, "Sonnet", &body.seven_day_sonnet);

    let design = body
        .seven_day_design
        .as_ref()
        .or(body.seven_day_claude_design.as_ref())
        .or(body.claude_design.as_ref())
        .or(body.design.as_ref())
        .or(body.seven_day_omelette.as_ref())
        .cloned();
    let routines = body
        .seven_day_routines
        .as_ref()
        .or(body.seven_day_claude_routines.as_ref())
        .or(body.claude_routines.as_ref())
        .or(body.routines.as_ref())
        .or(body.routine.as_ref())
        .or(body.seven_day_cowork.as_ref())
        .cloned();
    push_q(&mut sub_quotas, "Claude Design", &design);
    push_q(&mut sub_quotas, "Claude Routines", &routines);

    let extra_usage = body.extra_usage.map(|e| ExtraUsageInfo {
        enabled: e.is_enabled.unwrap_or(false),
        used: e.used_credits.unwrap_or(0.0),
        limit: e.monthly_limit.unwrap_or(0.0),
        utilization: e.utilization.unwrap_or(0.0),
        currency: e.currency.unwrap_or_else(|| "USD".to_string()),
    });

    let session_window = body.five_hour.as_ref().or(body.seven_day.as_ref());
    let weekly_window = body
        .seven_day
        .as_ref()
        .or(body.seven_day_oauth_apps.as_ref());

    ClaudeLiveLimits {
        account: None,
        source,
        fetched_at: Utc::now(),
        five_hour_percent: session_window.map(|w| w.utilization).unwrap_or(0.0),
        five_hour_resets_at: session_window.and_then(|w| w.resets_at),
        weekly_percent: weekly_window.map(|w| w.utilization).unwrap_or(0.0),
        weekly_resets_at: weekly_window.and_then(|w| w.resets_at),
        sub_quotas,
        extra_usage,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_oauth_usage_payload_without_dropping_resets_or_extra_usage() {
        let body: UsageResponse = serde_json::from_str(
            r#"{
                "five_hour": {
                    "utilization": 12.0,
                    "resets_at": "2026-05-20T01:40:00Z"
                },
                "seven_day": {
                    "utilization": 26.0,
                    "resets_at": "2026-05-25T00:00:00Z"
                },
                "seven_day_fable": {
                    "utilization": 2.5,
                    "resets_at": "2026-05-25T00:00:00Z"
                },
                "seven_day_sonnet": {
                    "utilization": 4.0,
                    "resets_at": "2026-05-25T00:00:00Z"
                },
                "seven_day_claude_design": {
                    "utilization": 9.0,
                    "resets_at": null
                },
                "extra_usage": {
                    "is_enabled": true,
                    "monthly_limit": 100.0,
                    "used_credits": 17.5,
                    "utilization": 17.5,
                    "currency": "USD"
                }
            }"#,
        )
        .unwrap();

        let limits = live_limits_from_usage_response(body, ClaudeLimitSource::Oauth);

        assert_eq!(limits.source, ClaudeLimitSource::Oauth);
        assert_eq!(limits.five_hour_percent, 12.0);
        assert_eq!(limits.weekly_percent, 26.0);
        assert!(limits.five_hour_resets_at.is_some());
        assert!(limits.weekly_resets_at.is_some());
        assert!(limits.sub_quotas.iter().any(|q| q.label == "Fable"));
        assert!(limits.sub_quotas.iter().any(|q| q.label == "Sonnet"));
        assert!(limits.sub_quotas.iter().any(|q| q.label == "Claude Design"));
        let extra = limits.extra_usage.unwrap();
        assert!(extra.enabled);
        assert_eq!(extra.used, 17.5);
        assert_eq!(extra.limit, 100.0);
    }

    #[test]
    fn falls_back_to_seven_day_when_five_hour_is_missing() {
        let body: UsageResponse = serde_json::from_str(
            r#"{
                "seven_day": {
                    "utilization": 33.0,
                    "resets_at": "2026-05-25T00:00:00Z"
                }
            }"#,
        )
        .unwrap();

        let limits = live_limits_from_usage_response(body, ClaudeLimitSource::Web);

        assert_eq!(limits.source, ClaudeLimitSource::Web);
        assert_eq!(limits.five_hour_percent, 33.0);
        assert_eq!(limits.weekly_percent, 33.0);
    }
}
