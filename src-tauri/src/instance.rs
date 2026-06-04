use serde::Serialize;

// ===== 安装取消机制 =====
use crate::downloader::pool::ConnectionPool;
use crate::downloader::DownloadManager;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

fn install_cancel() -> &'static Mutex<HashMap<String, std::sync::Arc<AtomicBool>>> {
    static INSTANCE: OnceLock<Mutex<HashMap<String, std::sync::Arc<AtomicBool>>>> = OnceLock::new();
    INSTANCE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn install_download_managers() -> &'static Mutex<HashMap<String, Vec<(u64, DownloadManager)>>> {
    static INSTANCE: OnceLock<Mutex<HashMap<String, Vec<(u64, DownloadManager)>>>> =
        OnceLock::new();
    INSTANCE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn install_connection_pools() -> &'static Mutex<HashMap<String, Arc<ConnectionPool>>> {
    static INSTANCE: OnceLock<Mutex<HashMap<String, Arc<ConnectionPool>>>> = OnceLock::new();
    INSTANCE.get_or_init(|| Mutex::new(HashMap::new()))
}

static NEXT_DOWNLOAD_MANAGER_ID: AtomicU64 = AtomicU64::new(1);

pub struct DownloadManagerRegistration {
    name: String,
    id: u64,
}

impl Drop for DownloadManagerRegistration {
    fn drop(&mut self) {
        if let Ok(mut managers) = install_download_managers().lock() {
            if let Some(items) = managers.get_mut(&self.name) {
                items.retain(|(id, _)| *id != self.id);
                if items.is_empty() {
                    managers.remove(&self.name);
                }
            }
        }
    }
}

/// 注册一个安装任务的取消标志
pub fn register_cancel(name: &str) -> std::sync::Arc<AtomicBool> {
    let flag = std::sync::Arc::new(AtomicBool::new(false));
    install_cancel()
        .lock()
        .unwrap()
        .insert(name.to_string(), flag.clone());
    flag
}

/// 注册一个不允许同名并发的安装任务。
pub fn try_register_cancel(name: &str) -> Result<std::sync::Arc<AtomicBool>, String> {
    let mut tasks = install_cancel().lock().unwrap();
    if tasks.contains_key(name) {
        return Err(format!("已有同名安装任务正在运行: {}", name));
    }
    let flag = std::sync::Arc::new(AtomicBool::new(false));
    tasks.insert(name.to_string(), flag.clone());
    Ok(flag)
}

/// 检查是否已取消
pub fn is_cancelled(name: &str) -> bool {
    install_cancel()
        .lock()
        .unwrap()
        .get(name)
        .map_or(false, |f| f.load(Ordering::Relaxed))
}

pub fn install_cancel_flag(name: &str) -> Option<Arc<AtomicBool>> {
    install_cancel().lock().unwrap().get(name).cloned()
}

/// 同一个安装任务共享连接池，避免整合包、基础库和资源各自叠加并发。
pub fn install_download_pool(name: &str, max_connections: usize) -> Arc<ConnectionPool> {
    let mut pools = install_connection_pools().lock().unwrap();
    pools
        .entry(name.to_string())
        .or_insert_with(|| Arc::new(ConnectionPool::new(max_connections)))
        .clone()
}

/// 注册当前安装任务里的下载器，取消时直接中断下载器任务。
pub fn register_download_manager(
    name: &str,
    manager: &DownloadManager,
) -> DownloadManagerRegistration {
    let id = NEXT_DOWNLOAD_MANAGER_ID.fetch_add(1, Ordering::Relaxed);
    let already_cancelled = is_cancelled(name);
    install_download_managers()
        .lock()
        .unwrap()
        .entry(name.to_string())
        .or_default()
        .push((id, manager.clone()));
    if already_cancelled {
        manager.cancel_all();
    }
    DownloadManagerRegistration {
        name: name.to_string(),
        id,
    }
}

/// 移除取消标志
pub fn unregister_cancel(name: &str) {
    install_cancel().lock().unwrap().remove(name);
    install_download_managers().lock().unwrap().remove(name);
    install_connection_pools().lock().unwrap().remove(name);
}

/// 取消安装命令
#[tauri::command]
pub fn cancel_modpack_install(file_name: String) -> Result<String, String> {
    let mut already_cancelled = false;
    let found = if let Some(flag) = install_cancel().lock().unwrap().get(&file_name) {
        already_cancelled = flag.swap(true, Ordering::Relaxed);
        true
    } else {
        false
    };

    if !found {
        return Err(format!("未找到安装任务: {}", file_name));
    }

    let managers = install_download_managers()
        .lock()
        .unwrap()
        .get(&file_name)
        .cloned()
        .unwrap_or_default();
    for (_, manager) in managers {
        manager.cancel_all();
    }
    if let Some(pool) = install_connection_pools()
        .lock()
        .unwrap()
        .get(&file_name)
        .cloned()
    {
        pool.wake_all();
    }

    if already_cancelled {
        eprintln!(
            "[cancel] 任务已经处于取消状态，已再次唤醒下载器: {}",
            file_name
        );
    } else {
        eprintln!("[cancel] 已标记取消并中断下载器: {}", file_name);
    }
    Ok("cancelled".to_string())
}

/// 在系统默认浏览器打开 URL
#[tauri::command]
pub fn open_url(url: String) -> Result<(), String> {
    let parsed = url::Url::parse(url.trim()).map_err(|_| "链接格式无效".to_string())?;
    match parsed.scheme() {
        "http" | "https" => {}
        scheme => return Err(format!("不支持的链接协议: {}", scheme)),
    }
    let url = parsed.to_string();

    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        std::process::Command::new("rundll32.exe")
            .args(["url.dll,FileProtocolHandler", &url])
            .creation_flags(0x08000000)
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
    #[serde(
        skip_serializing_if = "Option::is_none",
        rename = "packRecommendedMemory"
    )]
    pub pack_recommended_memory: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "estimatedMemory")]
    pub estimated_memory: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "memorySource")]
    pub memory_source: Option<String>,
    #[serde(rename = "modCount")]
    pub mod_count: u32,
}

pub fn resolve_game_dir(game_dir: &str) -> std::path::PathBuf {
    if !game_dir.is_empty() {
        std::path::PathBuf::from(game_dir)
    } else {
        let home = std::env::var("APPDATA").unwrap_or_else(|_| ".".to_string());
        std::path::Path::new(&home).join(".oaoi").join("oaoi")
    }
}

pub fn safe_path_name(value: &str, label: &str) -> Result<String, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(format!("{}不能为空", label));
    }
    let mut components = std::path::Path::new(trimmed).components();
    match (components.next(), components.next()) {
        (Some(std::path::Component::Normal(name)), None) => Ok(name.to_string_lossy().to_string()),
        _ => Err(format!("非法{}: {}", label, value)),
    }
}

pub fn safe_join(base: &std::path::Path, relative: &str) -> Result<std::path::PathBuf, String> {
    let mut out = std::path::PathBuf::new();
    for component in std::path::Path::new(relative).components() {
        match component {
            std::path::Component::Normal(part) => out.push(part),
            std::path::Component::CurDir => {}
            _ => return Err(format!("非法相对路径: {}", relative)),
        }
    }
    if out.as_os_str().is_empty() {
        return Err("相对路径不能为空".to_string());
    }
    Ok(base.join(out))
}

pub fn set_minecraft_language(instance_dir: &std::path::Path, lang: &str) -> Result<(), String> {
    let safe_lang = safe_path_name(lang, "语言代码")?;
    let options_path = instance_dir.join("options.txt");
    let mut lines = if options_path.exists() {
        std::fs::read_to_string(&options_path)
            .map_err(|e| format!("读取语言配置失败: {}", e))?
            .lines()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };

    let mut found = false;
    for line in &mut lines {
        if line.trim_start().starts_with("lang:") {
            *line = format!("lang:{}", safe_lang);
            found = true;
            break;
        }
    }
    if !found {
        lines.push(format!("lang:{}", safe_lang));
    }

    std::fs::write(&options_path, format!("{}\n", lines.join("\n")))
        .map_err(|e| format!("写入语言配置失败: {}", e))
}

#[cfg(test)]
mod tests {
    use super::set_minecraft_language;

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("oaoi_test_{}_{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn set_minecraft_language_only_replaces_lang_line() {
        let dir = temp_dir("replace_lang");
        let options = dir.join("options.txt");
        std::fs::write(
            &options,
            "fov:0.0\nlang:en_us\nrenderDistance:12\nkey_key.attack:key.mouse.left\n",
        )
        .unwrap();

        set_minecraft_language(&dir, "zh_cn").unwrap();

        let updated = std::fs::read_to_string(&options).unwrap();
        assert!(updated.contains("fov:0.0\n"));
        assert!(updated.contains("lang:zh_cn\n"));
        assert!(updated.contains("renderDistance:12\n"));
        assert!(updated.contains("key_key.attack:key.mouse.left\n"));
        assert!(!updated.contains("lang:en_us"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn set_minecraft_language_appends_lang_when_missing() {
        let dir = temp_dir("append_lang");
        let options = dir.join("options.txt");
        std::fs::write(&options, "fov:0.0\nrenderDistance:12\n").unwrap();

        set_minecraft_language(&dir, "zh_cn").unwrap();

        let updated = std::fs::read_to_string(&options).unwrap();
        assert!(updated.contains("fov:0.0\n"));
        assert!(updated.contains("renderDistance:12\n"));
        assert!(updated.contains("lang:zh_cn\n"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

fn count_instance_mods(instance_dir: &std::path::Path) -> u32 {
    let mods_dir = instance_dir.join("mods");
    if !mods_dir.exists() {
        return 0;
    }
    std::fs::read_dir(mods_dir)
        .map(|entries| {
            entries
                .filter(|entry| {
                    entry
                        .as_ref()
                        .ok()
                        .map(|entry| {
                            let name = entry.file_name().to_string_lossy().to_lowercase();
                            name.ends_with(".jar") || name.ends_with(".zip")
                        })
                        .unwrap_or(false)
                })
                .count() as u32
        })
        .unwrap_or(0)
}

fn estimate_memory_by_mod_count(mod_count: u32) -> u32 {
    if mod_count == 0 {
        2048
    } else if mod_count <= 50 {
        4096
    } else if mod_count <= 150 {
        6144
    } else if mod_count <= 250 {
        8192
    } else {
        10240
    }
}

pub fn cf_api_key() -> String {
    crate::secrets::get_cf_api_key()
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
            if !path.is_dir() {
                continue;
            }
            let json_path = path.join("instance.json");
            if json_path.exists() {
                if let Ok(data) = std::fs::read_to_string(&json_path) {
                    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&data) {
                        let name = entry.file_name().to_string_lossy().to_string();
                        let mc_version = json["id"].as_str().unwrap_or("unknown").to_string();
                        let loader_type = json["loader"]["type"]
                            .as_str()
                            .unwrap_or("vanilla")
                            .to_string();
                        let loader_version =
                            json["loader"]["version"].as_str().unwrap_or("").to_string();
                        let recommended_memory =
                            json["recommendedMemory"].as_u64().map(|v| v as u32);
                        let mod_count = count_instance_mods(&path);
                        let estimated_memory = Some(estimate_memory_by_mod_count(mod_count));
                        let pack_recommended_memory =
                            json["packRecommendedMemory"].as_u64().map(|v| v as u32);
                        let memory_source = if pack_recommended_memory.is_some() {
                            Some("pack".to_string())
                        } else {
                            Some("mod_count".to_string())
                        };
                        list.push(InstanceInfo {
                            name,
                            mc_version,
                            loader_type,
                            loader_version,
                            recommended_memory,
                            pack_recommended_memory,
                            estimated_memory,
                            memory_source,
                            mod_count,
                        });
                    }
                }
            }
        }
    }
    Ok(list)
}

#[tauri::command]
pub async fn delete_version(game_dir: String, name: String) -> Result<String, String> {
    tokio::task::spawn_blocking(move || {
        let dir = resolve_game_dir(&game_dir);
        let safe_name = safe_path_name(&name, "版本名")?;
        let inst_path = dir.join("instances").join(&safe_name);
        if !inst_path.exists() {
            return Err(format!("版本 {} 不存在", name));
        }
        std::fs::remove_dir_all(&inst_path).map_err(|e| format!("删除失败: {}", e))?;
        if let Err(e) = crate::modpack_sources::delete_source_index(&dir, &safe_name) {
            eprintln!("[instance] delete modpack source metadata failed: {}", e);
        }
        Ok(format!("已删除版本: {}", name))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// 使用系统文件管理器打开指定目录
#[tauri::command]
pub fn open_folder(game_dir: String, name: String, sub_dir: String) -> Result<String, String> {
    let dir = resolve_game_dir(&game_dir);
    let safe_name = safe_path_name(&name, "版本名")?;
    let mut target = dir.join("instances").join(&safe_name);
    let safe_sub = match sub_dir.as_str() {
        "" => "",
        "mods" => "mods",
        "saves" => "saves",
        "resourcepacks" => "resourcepacks",
        "shaderpacks" => "shaderpacks",
        "config" => "config",
        _ => return Err(format!("非法目录: {}", sub_dir)),
    };
    if !safe_sub.is_empty() {
        target = target.join(safe_sub);
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
