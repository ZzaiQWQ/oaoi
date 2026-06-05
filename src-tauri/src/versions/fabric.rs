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

        let resp = http
            .get(&url)
            .send()
            .map_err(|e| format!("获取 Fabric 版本失败: {}", e))?;
        let json: serde_json::Value = resp.json().map_err(|e| e.to_string())?;

        let arr = json.as_array().ok_or("格式错误")?;
        let mut versions = Vec::new();
        for v in arr {
            if let Some(loader) = v.get("loader") {
                if let Some(ver) = loader.get("version").and_then(|v| v.as_str()) {
                    versions.push(ver.to_string());
                }
            }
        }
        Ok(versions)
    })
    .await
    .map_err(|e| e.to_string())?
}
