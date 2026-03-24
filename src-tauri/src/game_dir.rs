// game_dir.rs - 游戏目录初始化

#[tauri::command]
pub fn init_game_dir(base_dir: String) -> Result<String, String> {
    let game_dir = std::path::Path::new(&base_dir).join("oaoi");
    let dirs = ["instances", "libs", "res", "res/indexes", "res/objects", "runtime"];
    for d in &dirs {
        let p = game_dir.join(d);
        if !p.exists() {
            std::fs::create_dir_all(&p).map_err(|e| format!("创建目录失败: {}", e))?;
        }
    }
    Ok(game_dir.to_string_lossy().to_string())
}
