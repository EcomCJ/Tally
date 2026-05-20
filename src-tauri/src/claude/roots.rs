use serde::Deserialize;
use std::collections::HashSet;
use walkdir::WalkDir;

/// Claude stores local app data in different roots depending on install mode.
/// The packaged Windows app keeps its Roaming profile under LocalCache, while
/// CLI installs typically use `%APPDATA%\Claude`.
fn claude_config_roots() -> Vec<std::path::PathBuf> {
    let mut roots = Vec::new();
    let mut seen = HashSet::new();

    if let Some(mut p) = dirs::config_dir() {
        p.push("Claude");
        if seen.insert(p.clone()) {
            roots.push(p);
        }
    }

    if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") {
        let packages = std::path::PathBuf::from(local_app_data).join("Packages");
        if let Ok(entries) = std::fs::read_dir(packages) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
                if !name.contains("claude") {
                    continue;
                }
                let p = entry
                    .path()
                    .join("LocalCache")
                    .join("Roaming")
                    .join("Claude");
                if seen.insert(p.clone()) {
                    roots.push(p);
                }
            }
        }
    }

    roots
}

pub(crate) fn cowork_session_roots() -> Vec<std::path::PathBuf> {
    claude_config_roots()
        .into_iter()
        .map(|root| root.join("local-agent-mode-sessions"))
        .filter(|root| root.exists())
        .collect()
}

/// Discover Cowork session IDs by walking the local-agent-mode-sessions tree
/// and extracting `cliSessionId` from each `local_*.json` index file.
pub(crate) fn discover_cowork_session_ids() -> std::collections::HashSet<String> {
    let mut set = std::collections::HashSet::new();
    for root in cowork_session_roots() {
        for entry in WalkDir::new(&root).into_iter().filter_map(|e| e.ok()) {
            let path = entry.path();
            let name = match path.file_name().and_then(|s| s.to_str()) {
                Some(n) => n,
                None => continue,
            };
            if !(name.starts_with("local_") && name.ends_with(".json")) {
                continue;
            }
            let body = match std::fs::read_to_string(path) {
                Ok(s) => s,
                Err(_) => continue,
            };
            #[derive(Deserialize)]
            struct CoworkIdx {
                #[serde(rename = "cliSessionId")]
                cli_session_id: Option<String>,
            }
            if let Ok(idx) = serde_json::from_str::<CoworkIdx>(&body) {
                if let Some(sid) = idx.cli_session_id {
                    set.insert(sid);
                }
            }
        }
    }
    set
}
