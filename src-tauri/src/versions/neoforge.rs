#[tauri::command]
pub fn get_neoforge_versions(mc_version: String) -> Result<Vec<String>, String> {
    let url = "https://maven.neoforged.net/api/maven/versions/releases/net/neoforged/neoforge";
    let http = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .user_agent("OAOI-Launcher/1.0")
        .build().map_err(|e| e.to_string())?;
    
    let resp = http.get(url).send().map_err(|e| format!("获取 NeoForge 版本失败: {}", e))?;
    let json: serde_json::Value = resp.json().map_err(|e| e.to_string())?;
    
    let all_versions = json["versions"].as_array().ok_or("格式错误")?;
    
    // NeoForge 版本号映射: MC 1.20.1 → "20.1.", MC 1.21.4 → "21.4."
    let parts: Vec<&str> = mc_version.split('.').collect();
    let prefix = if parts.len() >= 3 {
        format!("{}.{}.", parts[1], parts[2])
    } else if parts.len() == 2 {
        format!("{}.", parts[1])
    } else {
        return Ok(vec![]);
    };
    
    let matching: Vec<String> = all_versions.iter()
        .filter_map(|v| v.as_str())
        .filter(|v| v.starts_with(&prefix) && !v.contains("alpha"))
        .map(|v| v.to_string())
        .collect();
    
    let stable: Vec<String> = matching.iter()
        .filter(|v| !v.contains("beta"))
        .cloned()
        .collect();
    
    let mut versions = if stable.is_empty() { matching } else { stable };
    versions.reverse();
    Ok(versions)
}
