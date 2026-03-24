#[tauri::command]
pub async fn get_neoforge_versions(mc_version: String) -> Result<Vec<String>, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result = (|| -> Result<Vec<String>, String> {
            let url = "https://maven.neoforged.net/api/maven/versions/releases/net/neoforged/neoforge";
            let http = reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .user_agent("OAOI-Launcher/1.0")
                .build().map_err(|e| e.to_string())?;
            
            let resp = http.get(url).send().map_err(|e| format!("获取 NeoForge 版本失败: {}", e))?;
            let json: serde_json::Value = resp.json().map_err(|e| e.to_string())?;
            
            let all_versions = json["versions"].as_array().ok_or("格式错误")?;
            
            let parts: Vec<&str> = mc_version.split('.').collect();
            let major: u32 = parts.first().and_then(|s| s.parse().ok()).unwrap_or(1);
            
            // MC 26+ 用四段版本号: MC 26.1.1 → NeoForge "26.1.1.x"
            // MC 1.x.y 用三段: MC 1.21.1 → NeoForge "21.1.x"
            let prefix = if major > 1 {
                // MC 26.1.1 → "26.1.1."
                if parts.len() >= 3 {
                    format!("{}.{}.{}.", parts[0], parts[1], parts[2])
                } else if parts.len() == 2 {
                    format!("{}.{}.", parts[0], parts[1])
                } else {
                    format!("{}.", parts[0])
                }
            } else {
                // MC 1.21.1 → "21.1."
                if parts.len() >= 3 {
                    format!("{}.{}.", parts[1], parts[2])
                } else if parts.len() == 2 {
                    format!("{}.", parts[1])
                } else {
                    return Ok(vec![]);
                }
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
        })();
        let _ = tx.send(result);
    });
    rx.recv().map_err(|_| "线程通信失败".to_string())?
}
