use serde::Serialize;

#[derive(Serialize, Clone)]
pub struct InstanceInfo {
    pub name: String,
    pub mc_version: String,
    pub loader_type: String,
    pub loader_version: String,
}

pub fn resolve_game_dir(game_dir: &str) -> std::path::PathBuf {
    if !game_dir.is_empty() {
        std::path::PathBuf::from(game_dir)
    } else {
        let home = std::env::var("APPDATA").unwrap_or_else(|_| ".".to_string());
        std::path::Path::new(&home).join(".oaoi").join("oaoi")
    }
}

#[tauri::command]
pub fn list_installed_versions(game_dir: String) -> Result<Vec<InstanceInfo>, String> {
    let dir = resolve_game_dir(&game_dir);
    let instances_path = dir.join("instances");
    if !instances_path.exists() {
        return Ok(vec![]);
    }
    let mut list = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&instances_path) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() { continue; }
            let json_path = path.join("instance.json");
            if json_path.exists() {
                if let Ok(data) = std::fs::read_to_string(&json_path) {
                    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&data) {
                        let name = entry.file_name().to_string_lossy().to_string();
                        let mc_version = json["id"].as_str().unwrap_or("unknown").to_string();
                        let loader_type = json["loader"]["type"].as_str().unwrap_or("vanilla").to_string();
                        let loader_version = json["loader"]["version"].as_str().unwrap_or("").to_string();
                        list.push(InstanceInfo { name, mc_version, loader_type, loader_version });
                    }
                }
            }
        }
    }
    Ok(list)
}

#[tauri::command]
pub fn delete_version(game_dir: String, name: String) -> Result<String, String> {
    let dir = resolve_game_dir(&game_dir);
    let inst_path = dir.join("instances").join(&name);
    if !inst_path.exists() {
        return Err(format!("实例 {} 不存在", name));
    }
    std::fs::remove_dir_all(&inst_path)
        .map_err(|e| format!("删除失败: {}", e))?;
    Ok(format!("已删除实例: {}", name))
}
