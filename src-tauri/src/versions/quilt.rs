#[tauri::command]
pub fn get_quilt_versions(mc_version: String) -> Result<Vec<String>, String> {
    let url = format!("https://meta.quiltmc.org/v3/versions/loader/{}", mc_version);
    let http = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .user_agent("OAOI-Launcher/1.0")
        .build().map_err(|e| e.to_string())?;
    
    let resp = http.get(&url).send().map_err(|e| format!("获取 Quilt 版本失败: {}", e))?;
    if !resp.status().is_success() {
        return Ok(vec![]);
    }
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
}
