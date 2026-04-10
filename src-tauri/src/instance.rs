use serde::Serialize;

// ===== 安装取消机制 =====
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::collections::HashMap;

fn install_cancel() -> &'static Mutex<HashMap<String, std::sync::Arc<AtomicBool>>> {
    static INSTANCE: OnceLock<Mutex<HashMap<String, std::sync::Arc<AtomicBool>>>> = OnceLock::new();
    INSTANCE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 注册一个安装任务的取消标志
pub fn register_cancel(name: &str) -> std::sync::Arc<AtomicBool> {
    let flag = std::sync::Arc::new(AtomicBool::new(false));
    install_cancel().lock().unwrap().insert(name.to_string(), flag.clone());
    flag
}

/// 检查是否已取消
pub fn is_cancelled(name: &str) -> bool {
    install_cancel().lock().unwrap().get(name).map_or(false, |f| f.load(Ordering::Relaxed))
}

/// 移除取消标志
pub fn unregister_cancel(name: &str) {
    install_cancel().lock().unwrap().remove(name);
}

/// 取消安装命令
#[tauri::command]
pub fn cancel_modpack_install(file_name: String) -> Result<String, String> {
    if let Some(flag) = install_cancel().lock().unwrap().get(&file_name) {
        flag.store(true, Ordering::Relaxed);
        eprintln!("[cancel] 已标记取消: {}", file_name);
        Ok("cancelled".to_string())
    } else {
        Err(format!("未找到安装任务: {}", file_name))
    }
}

/// 在系统默认浏览器打开 URL
#[tauri::command]
pub fn open_url(url: String) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("cmd")
            .args(["/c", "start", "", &url])
            .spawn()
            .map_err(|e| format!("打开链接失败: {}", e))?;
    }
    #[cfg(not(target_os = "windows"))]
    {
        std::process::Command::new("xdg-open")
            .arg(&url)
            .spawn()
            .map_err(|e| format!("打开链接失败: {}", e))?;
    }
    Ok(())
}

#[derive(Serialize, Clone)]
pub struct InstanceInfo {
    pub name: String,
    pub mc_version: String,
    pub loader_type: String,
    pub loader_version: String,
    #[serde(skip_serializing_if = "Option::is_none", rename = "recommendedMemory")]
    pub recommended_memory: Option<u32>,
}

pub fn resolve_game_dir(game_dir: &str) -> std::path::PathBuf {
    if !game_dir.is_empty() {
        std::path::PathBuf::from(game_dir)
    } else {
        let home = std::env::var("APPDATA").unwrap_or_else(|_| ".".to_string());
        std::path::Path::new(&home).join(".oaoi").join("oaoi")
    }
}

pub fn cf_api_key() -> String { crate::secrets::get_cf_api_key() }

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
                        let recommended_memory = json["recommendedMemory"].as_u64().map(|v| v as u32);
                        list.push(InstanceInfo { name, mc_version, loader_type, loader_version, recommended_memory });
                    }
                }
            }
        }
    }
    Ok(list)
}

#[tauri::command]
pub async fn delete_version(game_dir: String, name: String) -> Result<String, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send((|| {
    let dir = resolve_game_dir(&game_dir);
    let inst_path = dir.join("instances").join(&name);
    if !inst_path.exists() {
        return Err(format!("实例 {} 不存在", name));
    }
    std::fs::remove_dir_all(&inst_path)
        .map_err(|e| format!("删除失败: {}", e))?;
    Ok(format!("已删除实例: {}", name))
        })());
    });
    rx.recv().map_err(|_| "线程通信失败".to_string())?
}

/// 使用系统文件管理器打开指定目录
#[tauri::command]
pub fn open_folder(game_dir: String, name: String, sub_dir: String) -> Result<String, String> {
    let dir = resolve_game_dir(&game_dir);
    let mut target = dir.join("instances").join(&name);
    if !sub_dir.is_empty() {
        target = target.join(&sub_dir);
    }
    // 自动创建不存在的目录
    if !target.exists() {
        std::fs::create_dir_all(&target).map_err(|e| format!("创建目录失败: {}", e))?;
    }
    // 规范化路径，再去掉 \\?\ 前缀
    let canonical = std::fs::canonicalize(&target).unwrap_or_else(|_| target.clone());
    let mut path_str = canonical.to_string_lossy().to_string();
    if path_str.starts_with(r"\\?\") {
        path_str = path_str[4..].to_string();
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        std::process::Command::new("explorer")
            .arg(&path_str)
            .creation_flags(0x08000000)
            .spawn()
            .map_err(|e| format!("打开目录失败: {}", e))?;
    }
    Ok(path_str)
}
