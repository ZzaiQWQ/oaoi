const NEOFORGE_API: &str =
    "https://maven.neoforged.net/api/maven/versions/releases/net/neoforged/neoforge";
const LEGACY_FORGE_API: &str =
    "https://maven.neoforged.net/api/maven/versions/releases/net/neoforged/forge";

#[tauri::command]
pub async fn get_neoforge_versions(mc_version: String) -> Result<Vec<String>, String> {
    tokio::task::spawn_blocking(move || -> Result<Vec<String>, String> {
        if !super::release_version_at_least(&mc_version, 20, 1) {
            return Ok(vec![]);
        }

        let http = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .user_agent("OAOI-Launcher/1.0")
            .build()
            .map_err(|e| e.to_string())?;

        let mut matching = Vec::new();

        let legacy_prefix = format!("{}-", mc_version);
        if let Ok(legacy_versions) = fetch_versions(&http, LEGACY_FORGE_API) {
            matching.extend(
                legacy_versions
                    .into_iter()
                    .filter(|v| v.starts_with(&legacy_prefix) && !v.contains("alpha")),
            );
        }

        if let Some(prefix) = neoforge_prefix(&mc_version) {
            match fetch_versions(&http, NEOFORGE_API) {
                Ok(neoforge_versions) => {
                    matching.extend(
                        neoforge_versions
                            .into_iter()
                            .filter(|v| v.starts_with(&prefix) && !v.contains("alpha")),
                    );
                }
                Err(e) if matching.is_empty() => return Err(e),
                Err(_) => {}
            }
        }

        let stable: Vec<String> = matching
            .iter()
            .filter(|v| !v.contains("beta"))
            .cloned()
            .collect();

        let mut versions = if stable.is_empty() { matching } else { stable };
        versions.reverse();
        Ok(versions)
    })
    .await
    .map_err(|e| e.to_string())?
}

fn fetch_versions(http: &reqwest::blocking::Client, url: &str) -> Result<Vec<String>, String> {
    let resp = http
        .get(url)
        .send()
        .map_err(|e| format!("获取 NeoForge 版本失败: {}", e))?;

    if !resp.status().is_success() {
        return Err(format!("获取 NeoForge 版本失败: HTTP {}", resp.status()));
    }

    let json: serde_json::Value = resp.json().map_err(|e| e.to_string())?;
    let versions = json["versions"].as_array().ok_or("格式错误")?;

    Ok(versions
        .iter()
        .filter_map(|v| v.as_str())
        .map(|v| v.to_string())
        .collect())
}

fn neoforge_prefix(mc_version: &str) -> Option<String> {
    let parts: Vec<&str> = mc_version.split('.').collect();
    let major: u32 = parts.first().and_then(|s| s.parse().ok()).unwrap_or(1);

    if major > 1 {
        if parts.len() >= 3 {
            Some(format!("{}.{}.{}.", parts[0], parts[1], parts[2]))
        } else if parts.len() == 2 {
            Some(format!("{}.{}.", parts[0], parts[1]))
        } else {
            Some(format!("{}.", parts[0]))
        }
    } else if parts.len() >= 3 {
        Some(format!("{}.{}.", parts[1], parts[2]))
    } else if parts.len() == 2 {
        Some(format!("{}.0.", parts[1]))
    } else {
        None
    }
}
