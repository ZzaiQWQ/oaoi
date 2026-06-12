#[tauri::command]
pub async fn get_fabric_versions(mc_version: String) -> Result<Vec<String>, String> {
    tokio::task::spawn_blocking(move || -> Result<Vec<String>, String> {
        if !super::release_version_at_least(&mc_version, 14, 0) {
            return Ok(vec![]);
        }

        let url = format!(
            "https://meta.fabricmc.net/v2/versions/loader/{}",
            mc_version
        );
        let http = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .map_err(|e| e.to_string())?;

        let versions = fetch_fabric_versions_for_game(&http, &url).unwrap_or_default();
        if !versions.is_empty() {
            return Ok(versions);
        }

        // 新快照刚发布时，Fabric 可能没有返回按 MC 版本展开的列表；这时取官方最新 Loader。
        fetch_latest_fabric_loader(&http)
    })
    .await
    .map_err(|e| e.to_string())?
}

fn fetch_fabric_versions_for_game(
    http: &reqwest::blocking::Client,
    url: &str,
) -> Result<Vec<String>, String> {
    let resp = http
        .get(url)
        .send()
        .map_err(|e| format!("获取 Fabric 版本失败: {}", e))?;
    if !resp.status().is_success() {
        return Ok(vec![]);
    }
    let json: serde_json::Value = resp.json().map_err(|e| e.to_string())?;
    Ok(parse_loader_versions(&json))
}

fn fetch_latest_fabric_loader(http: &reqwest::blocking::Client) -> Result<Vec<String>, String> {
    let resp = http
        .get("https://meta.fabricmc.net/v2/versions/loader")
        .send()
        .map_err(|e| format!("获取 Fabric 最新 Loader 失败: {}", e))?;
    if !resp.status().is_success() {
        return Ok(vec![]);
    }
    let json: serde_json::Value = resp.json().map_err(|e| e.to_string())?;
    Ok(parse_loader_versions(&json).into_iter().take(1).collect())
}

fn parse_loader_versions(json: &serde_json::Value) -> Vec<String> {
    let Some(arr) = json.as_array() else {
        return vec![];
    };
    arr.iter()
        .filter_map(|item| {
            item.get("loader")
                .and_then(|loader| loader.get("version"))
                .or_else(|| item.get("version"))
                .and_then(|value| value.as_str())
                .map(|value| value.to_string())
        })
        .collect()
}
