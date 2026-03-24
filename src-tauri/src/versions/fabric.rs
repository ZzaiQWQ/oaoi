#[tauri::command]
pub async fn get_fabric_versions(mc_version: String) -> Result<Vec<String>, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result = (|| -> Result<Vec<String>, String> {
            let url = format!("https://meta.fabricmc.net/v2/versions/loader/{}", mc_version);
            let http = reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build().map_err(|e| e.to_string())?;
            
            let resp = http.get(&url).send().map_err(|e| format!("获取 Fabric 版本失败: {}", e))?;
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
        })();
        let _ = tx.send(result);
    });
    rx.recv().map_err(|_| "线程通信失败".to_string())?
}
