#[tauri::command]
pub fn get_forge_versions(mc_version: String) -> Result<Vec<String>, String> {
    let mc1 = mc_version.clone();
    let mc2 = mc_version.clone();
    
    let bmcl_handle = std::thread::spawn(move || -> Vec<String> {
        let http = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(3))
            .build().ok();
        let Some(client) = http else { return vec![] };
        let url = format!("https://bmclapi2.bangbang93.com/forge/minecraft/{}", mc1);
        let Ok(resp) = client.get(&url).send() else { return vec![] };
        let Ok(json) = resp.json::<serde_json::Value>() else { return vec![] };
        let Some(arr) = json.as_array() else { return vec![] };
        let mut versions: Vec<String> = arr.iter()
            .filter_map(|v| v.get("version").and_then(|v| v.as_str()).map(|s| s.to_string()))
            .collect();
        versions.reverse();
        versions
    });
    
    let forge_handle = std::thread::spawn(move || -> Vec<String> {
        let http = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .user_agent("OAOI-Launcher/1.0")
            .build().ok();
        let Some(client) = http else { return vec![] };
        let url = format!("https://files.minecraftforge.net/net/minecraftforge/forge/index_{}.html", mc2);
        let Ok(resp) = client.get(&url).send() else { return vec![] };
        if !resp.status().is_success() { return vec![] }
        let Ok(html) = resp.text() else { return vec![] };
        
        let prefix = format!("forge-{}-", mc2);
        let mut versions = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for part in html.split(&prefix) {
            if let Some(end) = part.find("-installer.jar") {
                let ver = &part[..end];
                if !ver.is_empty() && !ver.contains('<') && !ver.contains('"') && seen.insert(ver.to_string()) {
                    versions.push(ver.to_string());
                }
            }
        }
        versions
    });
    
    let bmcl_versions = bmcl_handle.join().unwrap_or_default();
    if !bmcl_versions.is_empty() {
        return Ok(bmcl_versions);
    }
    
    let forge_versions = forge_handle.join().unwrap_or_default();
    Ok(forge_versions)
}
