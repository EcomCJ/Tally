use super::types::ClaudeLiveLimits;

pub(crate) fn record_limit_sample(value: &ClaudeLiveLimits) {
    let Some(mut root) = dirs::data_local_dir() else {
        return;
    };
    root.push("tally");
    root.push("history");
    if std::fs::create_dir_all(&root).is_err() {
        return;
    }
    let path = root.join(format!(
        "claude-limit-samples-{}.jsonl",
        value.fetched_at.format("%Y")
    ));
    let sample = serde_json::json!({
        "schema_version": 1,
        "captured_at": value.fetched_at,
        "source": value.source,
        "five_hour": {
            "used_percent": value.five_hour_percent,
            "resets_at": value.five_hour_resets_at,
        },
        "weekly": {
            "used_percent": value.weekly_percent,
            "resets_at": value.weekly_resets_at,
        },
        "sub_quotas": value.sub_quotas.iter().map(|q| serde_json::json!({
            "label": q.label,
            "used_percent": q.utilization,
            "resets_at": q.resets_at,
        })).collect::<Vec<_>>(),
        "extra_usage": value.extra_usage.as_ref().map(|e| serde_json::json!({
            "enabled": e.enabled,
            "used": e.used,
            "limit": e.limit,
            "used_percent": e.utilization,
            "currency": e.currency,
        })),
    });

    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        use std::io::Write;
        if serde_json::to_writer(&mut file, &sample).is_ok() {
            let _ = file.write_all(b"\n");
        }
    }
}
