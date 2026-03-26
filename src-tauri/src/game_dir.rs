use std::path::Path;

#[tauri::command]
pub fn init_game_dir(base_dir: String) -> Result<String, String> {
    let mc_dir = Path::new(&base_dir).join(".minecraft");
    let dirs = ["versions", "assets", "libraries", "mods", "config", "resourcepacks", "saves"];
    for d in &dirs {
        let p = mc_dir.join(d);
        if !p.exists() {
            std::fs::create_dir_all(&p).map_err(|e| format!("创建目录失败: {}", e))?;
        }
    }
    // 创建 launcher_profiles.json（Minecraft 需要它）
    let profiles = mc_dir.join("launcher_profiles.json");
    if !profiles.exists() {
        std::fs::write(&profiles, r#"{"profiles":{}}"#)
            .map_err(|e| format!("创建配置文件失败: {}", e))?;
    }
    Ok(mc_dir.to_string_lossy().to_string())
}
