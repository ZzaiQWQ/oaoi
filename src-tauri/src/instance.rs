use serde::Serialize;
use std::path::{Path, PathBuf};

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

static NEXT_DOWNLOAD_MANAGER_ID: AtomicU64 = AtomicU64::new(1);
const SCOPED_TASK_SEPARATOR: char = '\u{1f}';
const DELETE_TASK_PREFIX: &str = "__oaoi_delete_version__:";

pub struct DownloadManagerRegistration {
    name: String,
    id: u64,
}

/// 生成只用于进程内任务注册的游戏目录键，避免不同目录的同名任务互相冲突。
pub fn normalized_game_dir_task_key(game_dir: &str) -> String {
    let game_root = resolve_game_dir(game_dir);
    let absolute = if game_root.is_absolute() {
        game_root
    } else {
        std::env::current_dir()
            .map(|current| current.join(&game_root))
            .unwrap_or(game_root)
    };
    let mut lexical = PathBuf::new();
    for component in absolute.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                lexical.pop();
            }
            other => lexical.push(other.as_os_str()),
        }
    }
    let normalized = lexical
        .canonicalize()
        .unwrap_or(lexical)
        .to_string_lossy()
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_string();
    #[cfg(windows)]
    let normalized = normalized.to_lowercase();
    normalized
}

/// 将原有显示名称包装成跨游戏目录唯一的内部任务键。
pub fn scoped_task_key(game_dir: &str, display_name: &str) -> String {
    format!(
        "{}{}{}",
        normalized_game_dir_task_key(game_dir),
        SCOPED_TASK_SEPARATOR,
        display_name
    )
}

fn scoped_task_display_name(task_key: &str) -> &str {
    task_key
        .rsplit_once(SCOPED_TASK_SEPARATOR)
        .map(|(_, display_name)| display_name)
        .unwrap_or(task_key)
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

/// 注册一个不允许同名并发的安装任务。
pub fn try_register_cancel(name: &str) -> Result<std::sync::Arc<AtomicBool>, String> {
    let mut tasks = install_cancel().lock().unwrap();
    if tasks.contains_key(name) {
        return Err(format!("已有同名安装任务正在运行: {}", name));
    }
    if let Some((game_key, _)) = name.rsplit_once(SCOPED_TASK_SEPARATOR) {
        let delete_prefix = format!(
            "{}{}{}",
            game_key, SCOPED_TASK_SEPARATOR, DELETE_TASK_PREFIX
        );
        if tasks.keys().any(|key| key.starts_with(&delete_prefix)) {
            return Err("当前游戏目录正在删除版本，暂时不能开始新任务".to_string());
        }
    }
    let flag = std::sync::Arc::new(AtomicBool::new(false));
    tasks.insert(name.to_string(), flag.clone());
    Ok(flag)
}

fn try_register_delete_task(game_dir: &str, version_name: &str) -> Result<String, String> {
    let game_key = normalized_game_dir_task_key(game_dir);
    let game_prefix = format!("{}{}", game_key, SCOPED_TASK_SEPARATOR);
    let task_key = scoped_task_key(game_dir, &format!("{}{}", DELETE_TASK_PREFIX, version_name));
    let mut tasks = install_cancel()
        .lock()
        .map_err(|_| "安装任务状态锁已损坏".to_string())?;
    if tasks.keys().any(|key| key.starts_with(&game_prefix)) {
        return Err("当前游戏目录仍有安装、下载或更新任务，完成后才能删除版本".to_string());
    }
    tasks.insert(task_key.clone(), Arc::new(AtomicBool::new(false)));
    Ok(task_key)
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
    let _ = name;
    crate::downloader::pool::process_connection_pool(max_connections)
}

/// 注册当前安装任务里的下载器，取消时直接中断下载器任务。
pub fn register_download_manager(
    name: &str,
    manager: &DownloadManager,
) -> DownloadManagerRegistration {
    let id = NEXT_DOWNLOAD_MANAGER_ID.fetch_add(1, Ordering::Relaxed);
    // 取消标志与下载器注册使用固定锁顺序，避免取消刚好发生在两次查询之间。
    let cancel_tasks = install_cancel().lock().unwrap();
    let already_cancelled = cancel_tasks
        .get(name)
        .is_some_and(|flag| flag.load(Ordering::Relaxed));
    let mut managers = install_download_managers().lock().unwrap();
    managers
        .entry(name.to_string())
        .or_default()
        .push((id, manager.clone()));
    drop(managers);
    drop(cancel_tasks);
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
}

fn resolve_cancel_task_key(file_name: &str, game_dir: Option<&str>) -> Result<String, String> {
    if let Some(game_dir) = game_dir.filter(|value| !value.trim().is_empty()) {
        let task_key = scoped_task_key(game_dir, file_name);
        if install_cancel().lock().unwrap().contains_key(&task_key) {
            return Ok(task_key);
        }
        return Err(format!("未找到安装任务: {}", file_name));
    }

    {
        let tasks = install_cancel().lock().unwrap();
        if tasks.contains_key(file_name) {
            Ok(file_name.to_string())
        } else {
            let mut matches = tasks
                .keys()
                .filter(|key| scoped_task_display_name(key) == file_name)
                .cloned();
            let Some(first) = matches.next() else {
                return Err(format!("未找到安装任务: {}", file_name));
            };
            if matches.next().is_some() {
                return Err(format!(
                    "存在多个不同游戏目录的同名任务，必须指定游戏目录: {}",
                    file_name
                ));
            }
            Ok(first)
        }
    }
}

/// 按内部任务键取消，供同一安装流程的失败联动直接调用。
pub fn cancel_install_task(task_key: &str) -> Result<String, String> {
    let task_display_name = scoped_task_display_name(&task_key).to_string();
    let mut already_cancelled = false;
    let found = if let Some(flag) = install_cancel().lock().unwrap().get(task_key) {
        already_cancelled = flag.swap(true, Ordering::Relaxed);
        true
    } else {
        false
    };

    if !found {
        return Err(format!("未找到安装任务: {}", task_display_name));
    }

    let managers = install_download_managers()
        .lock()
        .unwrap()
        .get(task_key)
        .cloned()
        .unwrap_or_default();
    for (_, manager) in managers {
        manager.cancel_all();
    }
    crate::downloader::pool::wake_process_connection_pool();

    if already_cancelled {
        eprintln!(
            "[cancel] 任务已经处于取消状态，已再次唤醒下载器: {}",
            task_display_name
        );
    } else {
        eprintln!("[cancel] 已标记取消并中断下载器: {}", task_display_name);
    }
    Ok("cancelled".to_string())
}

/// 取消安装命令；传入 gameDir 时精确取消对应游戏目录的任务。
#[tauri::command]
pub fn cancel_modpack_install(
    file_name: String,
    game_dir: Option<String>,
) -> Result<String, String> {
    let task_key = resolve_cancel_task_key(&file_name, game_dir.as_deref())?;
    cancel_install_task(&task_key)
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

pub fn resolve_game_dir(game_dir: &str) -> PathBuf {
    let base = if !game_dir.is_empty() {
        PathBuf::from(game_dir)
    } else {
        let home = std::env::var("APPDATA").unwrap_or_else(|_| ".".to_string());
        Path::new(&home).join(".minecraft")
    };
    normalize_minecraft_dir(base)
}

pub fn normalize_minecraft_dir(path: PathBuf) -> PathBuf {
    let is_minecraft_dir = path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            #[cfg(windows)]
            {
                name.eq_ignore_ascii_case(".minecraft")
            }
            #[cfg(not(windows))]
            {
                name == ".minecraft"
            }
        });
    if is_minecraft_dir {
        path
    } else {
        path.join(".minecraft")
    }
}

pub fn versions_dir(game_dir: &Path) -> PathBuf {
    game_dir.join("versions")
}

pub fn libraries_dir(game_dir: &Path) -> PathBuf {
    game_dir.join("libraries")
}

pub fn assets_dir(game_dir: &Path) -> PathBuf {
    game_dir.join("assets")
}

pub fn version_dir(game_dir: &Path, name: &str) -> PathBuf {
    versions_dir(game_dir).join(name)
}

pub fn version_json_path(inst_dir: &Path, name: &str) -> PathBuf {
    inst_dir.join(format!("{name}.json"))
}

pub fn version_jar_path(inst_dir: &Path, name: &str) -> PathBuf {
    inst_dir.join(format!("{name}.jar"))
}

pub fn natives_dir(inst_dir: &Path, name: &str) -> PathBuf {
    inst_dir.join(format!("{name}-natives"))
}

pub struct MergedVersionJson {
    pub json: serde_json::Value,
    pub jar_version: String,
}

pub fn load_merged_version_json(
    game_dir: &Path,
    version_name: &str,
) -> Result<MergedVersionJson, String> {
    let mut loading = Vec::new();
    load_merged_version_json_inner(game_dir, version_name, &mut loading)
}

fn load_merged_version_json_inner(
    game_dir: &Path,
    version_name: &str,
    loading: &mut Vec<String>,
) -> Result<MergedVersionJson, String> {
    let safe_name = safe_path_name(version_name, "版本名")?;
    if loading.len() >= 32 {
        return Err("版本继承层级超过限制".to_string());
    }
    if loading.iter().any(|name| name == &safe_name) {
        loading.push(safe_name);
        return Err(format!("版本继承存在循环: {}", loading.join(" -> ")));
    }

    let json_path = version_json_path(&version_dir(game_dir, &safe_name), &safe_name);
    let data = std::fs::read_to_string(&json_path)
        .map_err(|e| format!("读取版本配置失败 {}: {}", json_path.display(), e))?;
    let mut child: serde_json::Value = serde_json::from_str(&data)
        .map_err(|e| format!("解析版本配置失败 {}: {}", json_path.display(), e))?;

    let explicit_jar = child
        .get("jar")
        .and_then(|value| value.as_str())
        .map(|value| safe_path_name(value, "版本 jar"))
        .transpose()?;
    let owns_client = child
        .get("downloads")
        .and_then(|value| value.get("client"))
        .is_some();
    let parent_name = child
        .get("inheritsFrom")
        .and_then(|value| value.as_str())
        .map(|value| safe_path_name(value, "父版本名"))
        .transpose()?;

    if let Some(parent_name) = parent_name {
        loading.push(safe_name.clone());
        let parent_result = load_merged_version_json_inner(game_dir, &parent_name, loading);
        loading.pop();
        let parent = parent_result?;
        let inherited_client_version = parent
            .json
            .get("clientVersion")
            .and_then(|value| value.as_str())
            .or_else(|| {
                parent
                    .json
                    .get("mcVersion")
                    .and_then(|value| value.as_str())
            })
            .unwrap_or(&parent.jar_version)
            .to_string();
        let needs_client_version = child
            .get("clientVersion")
            .and_then(|value| value.as_str())
            .is_none()
            && child
                .get("mcVersion")
                .and_then(|value| value.as_str())
                .is_none();
        let jar_version = explicit_jar.unwrap_or_else(|| {
            if owns_client {
                safe_name.clone()
            } else {
                parent.jar_version.clone()
            }
        });
        merge_version_json(&mut child, parent.json);
        if let Some(object) = child.as_object_mut() {
            object.remove("inheritsFrom");
            if needs_client_version {
                // 仅补充合并结果，外部启动器创建的原始版本 JSON 保持不变。
                object.insert(
                    "clientVersion".to_string(),
                    serde_json::Value::String(inherited_client_version),
                );
            }
        }
        Ok(MergedVersionJson {
            json: child,
            jar_version,
        })
    } else {
        Ok(MergedVersionJson {
            json: child,
            jar_version: explicit_jar.unwrap_or(safe_name),
        })
    }
}

fn merge_version_json(child: &mut serde_json::Value, mut parent: serde_json::Value) {
    let (Some(child_object), Some(parent_object)) = (child.as_object_mut(), parent.as_object_mut())
    else {
        return;
    };

    if let Some(child_libraries) = child_object
        .get("libraries")
        .and_then(|value| value.as_array())
        .cloned()
    {
        let mut libraries = parent_object
            .remove("libraries")
            .and_then(|value| value.as_array().cloned())
            .unwrap_or_default();
        libraries.extend(child_libraries);
        child_object.insert("libraries".to_string(), serde_json::Value::Array(libraries));
    }

    if let Some(child_arguments) = child_object.get("arguments").cloned() {
        if let Some(parent_arguments) = parent_object.remove("arguments") {
            child_object.insert(
                "arguments".to_string(),
                merge_version_arguments(parent_arguments, child_arguments),
            );
        }
    }

    for (key, value) in parent_object.iter() {
        child_object
            .entry(key.clone())
            .or_insert_with(|| value.clone());
    }
}

fn merge_version_arguments(
    mut parent: serde_json::Value,
    child: serde_json::Value,
) -> serde_json::Value {
    let (Some(parent_object), Some(child_object)) = (parent.as_object_mut(), child.as_object())
    else {
        return child;
    };

    for (key, child_value) in child_object {
        if matches!(key.as_str(), "game" | "jvm") {
            let mut values = parent_object
                .remove(key)
                .and_then(|value| value.as_array().cloned())
                .unwrap_or_default();
            if let Some(child_values) = child_value.as_array() {
                values.extend(child_values.iter().cloned());
                parent_object.insert(key.clone(), serde_json::Value::Array(values));
                continue;
            }
        }
        parent_object.insert(key.clone(), child_value.clone());
    }
    parent
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
    use super::{
        cancel_modpack_install, list_installed_versions, load_merged_version_json,
        normalize_minecraft_dir, scoped_task_key, set_minecraft_language, try_register_cancel,
        try_register_delete_task, unregister_cancel,
    };

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("oaoi_test_{}_{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn 任务键区分不同游戏目录并统一_minecraft_目录写法() {
        let first_root = temp_dir("task-key-a");
        let second_root = temp_dir("task-key-b");
        let first = scoped_task_key(first_root.to_string_lossy().as_ref(), "同名任务");
        let first_explicit = scoped_task_key(
            first_root.join(".minecraft").to_string_lossy().as_ref(),
            "同名任务",
        );
        let first_with_parent = scoped_task_key(
            first_root
                .join("temp")
                .join("..")
                .join(".minecraft")
                .to_string_lossy()
                .as_ref(),
            "同名任务",
        );
        let second = scoped_task_key(second_root.to_string_lossy().as_ref(), "同名任务");

        assert_eq!(first, first_explicit);
        assert_eq!(first, first_with_parent);
        assert_ne!(first, second);
        let _ = std::fs::remove_dir_all(first_root);
        let _ = std::fs::remove_dir_all(second_root);
    }

    #[cfg(windows)]
    #[test]
    fn windows_接受不同大小写的_minecraft_目录名() {
        let path = std::path::PathBuf::from(r"E:\Games\.Minecraft");
        assert_eq!(normalize_minecraft_dir(path.clone()), path);
    }

    #[test]
    fn 旧取消名称可以取消唯一的跨目录任务() {
        let root = temp_dir("legacy-cancel");
        let display_name = format!("兼容取消-{}", std::process::id());
        let task_key = scoped_task_key(root.to_string_lossy().as_ref(), &display_name);
        let flag = try_register_cancel(&task_key).unwrap();

        cancel_modpack_install(display_name, None).unwrap();

        assert!(flag.load(std::sync::atomic::Ordering::Relaxed));
        unregister_cancel(&task_key);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn 两个目录的同名任务可以并存且旧接口拒绝误取消() {
        let first_root = temp_dir("parallel-task-a");
        let second_root = temp_dir("parallel-task-b");
        let display_name = format!("跨目录任务-{}", std::process::id());
        let first_key = scoped_task_key(first_root.to_string_lossy().as_ref(), &display_name);
        let second_key = scoped_task_key(second_root.to_string_lossy().as_ref(), &display_name);
        let first = try_register_cancel(&first_key).unwrap();
        let second = try_register_cancel(&second_key).unwrap();

        let error = cancel_modpack_install(display_name, None).unwrap_err();

        assert!(error.contains("多个不同游戏目录"));
        assert!(!first.load(std::sync::atomic::Ordering::Relaxed));
        assert!(!second.load(std::sync::atomic::Ordering::Relaxed));
        unregister_cancel(&first_key);
        unregister_cancel(&second_key);
        let _ = std::fs::remove_dir_all(first_root);
        let _ = std::fs::remove_dir_all(second_root);
    }

    #[test]
    fn 指定游戏目录只取消对应的同名任务() {
        let first_root = temp_dir("exact-cancel-a");
        let second_root = temp_dir("exact-cancel-b");
        let display_name = format!("精确取消-{}", std::process::id());
        let first_key = scoped_task_key(first_root.to_string_lossy().as_ref(), &display_name);
        let second_key = scoped_task_key(second_root.to_string_lossy().as_ref(), &display_name);
        let first = try_register_cancel(&first_key).unwrap();
        let second = try_register_cancel(&second_key).unwrap();

        cancel_modpack_install(display_name, Some(first_root.to_string_lossy().to_string()))
            .unwrap();

        assert!(first.load(std::sync::atomic::Ordering::Relaxed));
        assert!(!second.load(std::sync::atomic::Ordering::Relaxed));
        unregister_cancel(&first_key);
        unregister_cancel(&second_key);
        let _ = std::fs::remove_dir_all(first_root);
        let _ = std::fs::remove_dir_all(second_root);
    }

    #[test]
    fn 删除任务会阻止同目录新安装任务() {
        let root = temp_dir("delete-task-guard");
        let game_dir = root.to_string_lossy().to_string();
        let delete_key = try_register_delete_task(&game_dir, "测试实例").unwrap();
        let install_key = scoped_task_key(&game_dir, "另一个安装任务");

        assert!(try_register_cancel(&install_key).is_err());

        unregister_cancel(&delete_key);
        assert!(try_register_cancel(&install_key).is_ok());
        unregister_cancel(&install_key);
        let _ = std::fs::remove_dir_all(root);
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

    #[test]
    fn load_merged_version_json_merges_parent_libraries_and_arguments() {
        let dir = temp_dir("merge_version");
        let versions = dir.join("versions");
        let parent_dir = versions.join("1.20.1");
        let child_dir = versions.join("fabric-test");
        std::fs::create_dir_all(&parent_dir).unwrap();
        std::fs::create_dir_all(&child_dir).unwrap();
        std::fs::write(
            parent_dir.join("1.20.1.json"),
            r#"{
                "id":"1.20.1",
                "mainClass":"net.minecraft.client.main.Main",
                "assetIndex":{"id":"5"},
                "downloads":{"client":{"url":"https://example.invalid/client.jar"}},
                "libraries":[{"name":"parent:library:1"}],
                "arguments":{"jvm":["-Dparent=true"],"game":["--parent"]}
            }"#,
        )
        .unwrap();
        std::fs::write(
            child_dir.join("fabric-test.json"),
            r#"{
                "id":"fabric-test",
                "inheritsFrom":"1.20.1",
                "mainClass":"net.fabricmc.loader.impl.launch.knot.KnotClient",
                "libraries":[{"name":"child:library:1"}],
                "arguments":{"jvm":["-Dchild=true"],"game":["--child"]}
            }"#,
        )
        .unwrap();

        let merged = load_merged_version_json(&dir, "fabric-test").unwrap();
        assert_eq!(merged.jar_version, "1.20.1");
        assert_eq!(
            merged.json["mainClass"].as_str(),
            Some("net.fabricmc.loader.impl.launch.knot.KnotClient")
        );
        assert_eq!(merged.json["libraries"].as_array().unwrap().len(), 2);
        assert_eq!(merged.json["arguments"]["jvm"].as_array().unwrap().len(), 2);
        assert_eq!(merged.json["assetIndex"]["id"].as_str(), Some("5"));
        assert_eq!(merged.json["clientVersion"].as_str(), Some("1.20.1"));
        assert!(merged.json.get("inheritsFrom").is_none());
        let child_on_disk = std::fs::read_to_string(child_dir.join("fabric-test.json")).unwrap();
        assert!(!child_on_disk.contains("clientVersion"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn nested_inheritance_keeps_parent_client_version() {
        let dir = temp_dir("nested-inheritance");
        let parent_dir = dir.join("versions").join("custom-parent");
        let child_dir = dir.join("versions").join("custom-child");
        std::fs::create_dir_all(&parent_dir).unwrap();
        std::fs::create_dir_all(&child_dir).unwrap();
        std::fs::write(
            parent_dir.join("custom-parent.json"),
            r#"{
                "id":"custom-parent",
                "clientVersion":"1.20.1",
                "downloads":{"client":{"url":"https://example.invalid/client.jar"}},
                "mainClass":"example.Parent"
            }"#,
        )
        .unwrap();
        std::fs::write(
            child_dir.join("custom-child.json"),
            r#"{
                "id":"custom-child",
                "inheritsFrom":"custom-parent",
                "mainClass":"example.Child"
            }"#,
        )
        .unwrap();

        let merged = load_merged_version_json(&dir, "custom-child").unwrap();

        assert_eq!(merged.json["clientVersion"].as_str(), Some("1.20.1"));
        assert!(merged.json.get("inheritsFrom").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 单个损坏版本不会隐藏其他正常版本() {
        let root = temp_dir("list-partial-error");
        let versions = root.join(".minecraft").join("versions");
        let good = versions.join("1.20.1");
        let bad = versions.join("broken");
        std::fs::create_dir_all(&good).unwrap();
        std::fs::create_dir_all(&bad).unwrap();
        std::fs::write(
            good.join("1.20.1.json"),
            r#"{"id":"1.20.1","mainClass":"net.minecraft.client.main.Main"}"#,
        )
        .unwrap();
        std::fs::write(bad.join("broken.json"), "{ invalid json").unwrap();

        let versions = list_installed_versions(root.to_string_lossy().to_string()).unwrap();

        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].name, "1.20.1");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn 全部版本损坏时返回诊断错误() {
        let root = temp_dir("list-all-invalid");
        let bad = root.join(".minecraft").join("versions").join("broken");
        std::fs::create_dir_all(&bad).unwrap();
        std::fs::write(bad.join("broken.json"), "{ invalid json").unwrap();

        let error = match list_installed_versions(root.to_string_lossy().to_string()) {
            Err(error) => error,
            Ok(_) => panic!("损坏版本不应返回空的成功结果"),
        };

        assert!(error.contains("未能读取任何有效版本"));
        assert!(error.contains("broken.json"));
        let _ = std::fs::remove_dir_all(&root);
    }
}

fn count_instance_mods(instance_dir: &std::path::Path) -> Result<u32, String> {
    let mods_dir = instance_dir.join("mods");
    if !mods_dir.exists() {
        return Ok(0);
    }
    let entries = std::fs::read_dir(&mods_dir)
        .map_err(|error| format!("读取 Mod 目录失败 {}: {}", mods_dir.display(), error))?;
    let mut count = 0_u32;
    for entry in entries {
        let entry = entry
            .map_err(|error| format!("读取 Mod 目录项失败 {}: {}", mods_dir.display(), error))?;
        let name = entry.file_name().to_string_lossy().to_lowercase();
        if name.ends_with(".jar") || name.ends_with(".zip") {
            count = count.saturating_add(1);
        }
    }
    Ok(count)
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

fn version_number_parts(version: &str) -> Vec<u32> {
    let mut parts = Vec::new();
    let mut current = String::new();
    for ch in version.chars() {
        if ch.is_ascii_digit() {
            current.push(ch);
        } else if ch == '.' {
            if !current.is_empty() {
                if let Ok(value) = current.parse::<u32>() {
                    parts.push(value);
                }
                current.clear();
            }
        } else {
            break;
        }
    }
    if !current.is_empty() {
        if let Ok(value) = current.parse::<u32>() {
            parts.push(value);
        }
    }
    parts
}

fn compare_mc_versions(a: &str, b: &str) -> std::cmp::Ordering {
    let a_parts = version_number_parts(a);
    let b_parts = version_number_parts(b);
    if a_parts.is_empty() || b_parts.is_empty() {
        return a_parts.is_empty().cmp(&b_parts.is_empty());
    }
    let max_len = a_parts.len().max(b_parts.len());
    for index in 0..max_len {
        let left = a_parts.get(index).copied().unwrap_or(0);
        let right = b_parts.get(index).copied().unwrap_or(0);
        match left.cmp(&right) {
            std::cmp::Ordering::Equal => {}
            ordering => return ordering,
        }
    }
    a.cmp(b)
}

fn is_version_named_instance(name: &str) -> bool {
    name.chars().next().is_some_and(|ch| ch.is_ascii_digit())
        && !version_number_parts(name).is_empty()
}

pub fn cf_api_key() -> String {
    crate::secrets::get_cf_api_key()
}

fn infer_mc_version(json: &serde_json::Value) -> String {
    if let Some(version) = json.get("clientVersion").and_then(|v| v.as_str()) {
        return version.to_string();
    }
    if let Some(version) = json.get("mcVersion").and_then(|v| v.as_str()) {
        return version.to_string();
    }
    if let Some(version) = json
        .get("libraries")
        .and_then(|v| v.as_array())
        .and_then(|libraries| {
            libraries.iter().find_map(|lib| {
                lib.get("name").and_then(|v| v.as_str()).and_then(|name| {
                    loader_version_from_library(name, "net.fabricmc:intermediary:")
                })
            })
        })
    {
        return version;
    }
    if let Some(version) = json.get("inheritsFrom").and_then(|v| v.as_str()) {
        return version.to_string();
    }
    json.get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string()
}

fn loader_version_from_library(name: &str, marker: &str) -> Option<String> {
    if !name.contains(marker) {
        return None;
    }
    name.split(':').nth(2).map(|version| version.to_string())
}

fn forge_version_from_fmlloader(name: &str) -> Option<String> {
    let version = loader_version_from_library(name, "net.minecraftforge:fmlloader:")?;
    // 新版 Forge 的 fmlloader 版本通常是 mcVersion-forgeVersion，只展示真正的 Forge 版本。
    version
        .rsplit_once('-')
        .map(|(_, forge_version)| forge_version.to_string())
        .or(Some(version))
}

fn collect_argument_values(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::String(text) => out.push(text.to_string()),
        serde_json::Value::Array(values) => {
            for item in values {
                collect_argument_values(item, out);
            }
        }
        serde_json::Value::Object(object) => {
            if let Some(value) = object.get("value") {
                collect_argument_values(value, out);
            }
        }
        _ => {}
    }
}

fn argument_value_after_flag(
    json: &serde_json::Value,
    section: &str,
    flag: &str,
) -> Option<String> {
    let mut args = Vec::new();
    collect_argument_values(json.get("arguments")?.get(section)?, &mut args);
    args.windows(2).find_map(|pair| {
        let key = pair[0].trim();
        let value = pair[1].trim();
        if key == flag && !value.is_empty() && !value.starts_with("--") {
            Some(value.to_string())
        } else {
            None
        }
    })
}

fn has_loader_metadata_type(json: &serde_json::Value, expected_type: &str) -> bool {
    json.get("loader")
        .and_then(|loader| loader.get("type"))
        .and_then(|value| value.as_str())
        .is_some_and(|loader_type| loader_type.eq_ignore_ascii_case(expected_type))
}

fn loader_metadata_version(json: &serde_json::Value, expected_type: &str) -> Option<String> {
    if !has_loader_metadata_type(json, expected_type) {
        return None;
    }
    json.get("loader")?
        .get("version")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn has_neoforge_library(json: &serde_json::Value) -> bool {
    json.get("libraries")
        .and_then(|v| v.as_array())
        .is_some_and(|libraries| {
            libraries.iter().any(|lib| {
                lib.get("name")
                    .and_then(|v| v.as_str())
                    .is_some_and(|name| name.contains("net.neoforged"))
            })
        })
}

fn has_argument_value(json: &serde_json::Value, section: &str, expected: &str) -> bool {
    let Some(arguments) = json.get("arguments").and_then(|v| v.get(section)) else {
        return false;
    };
    let mut values = Vec::new();
    collect_argument_values(arguments, &mut values);
    values.iter().any(|value| value == expected)
}

fn has_forge_runtime_marker(json: &serde_json::Value, main_class: &str) -> bool {
    if main_class.contains("cpw.mods.bootstraplauncher")
        && has_argument_value(json, "game", "forgeclient")
    {
        return true;
    }
    json.get("libraries")
        .and_then(|v| v.as_array())
        .is_some_and(|libraries| {
            libraries.iter().any(|lib| {
                lib.get("name")
                    .and_then(|v| v.as_str())
                    .is_some_and(|name| {
                        name.starts_with("net.minecraftforge:fmlloader:")
                            || name.starts_with("net.minecraftforge:fmlearlydisplay:")
                    })
            })
        })
}

pub fn detect_loader(json: &serde_json::Value, _version_name: &str) -> (String, String) {
    if let Some(version) = loader_metadata_version(json, "neoforge") {
        return ("neoforge".to_string(), version);
    }

    if let Some(version) = argument_value_after_flag(json, "game", "--fml.neoForgeVersion") {
        return ("neoforge".to_string(), version);
    }
    if let Some(version) = argument_value_after_flag(json, "game", "--fml.forgeVersion") {
        return ("forge".to_string(), version);
    }

    if let Some(libraries) = json.get("libraries").and_then(|v| v.as_array()) {
        for lib in libraries {
            let name = lib.get("name").and_then(|v| v.as_str()).unwrap_or("");
            if let Some(version) = loader_version_from_library(name, "net.fabricmc:fabric-loader:")
            {
                return ("fabric".to_string(), version);
            }
            if let Some(version) = loader_version_from_library(name, "org.quiltmc:quilt-loader:") {
                return ("quilt".to_string(), version);
            }
            if let Some(version) = loader_version_from_library(name, "net.minecraftforge:forge:") {
                return ("forge".to_string(), version);
            }
            if let Some(version) =
                loader_version_from_library(name, "net.minecraftforge:minecraftforge:")
            {
                return ("forge".to_string(), version);
            }
            if let Some(version) = forge_version_from_fmlloader(name) {
                return ("forge".to_string(), version);
            }
            if let Some(version) = loader_version_from_library(name, "net.neoforged:neoforge:") {
                return ("neoforge".to_string(), version);
            }
            if let Some(version) = loader_version_from_library(name, "net.neoforged:forge:") {
                return ("neoforge".to_string(), version);
            }
        }
    }

    let main_class = json.get("mainClass").and_then(|v| v.as_str()).unwrap_or("");
    if main_class.contains("net.neoforged") || has_neoforge_library(json) {
        // 新版 NeoForge 不一定保留 net.neoforged:neoforge 主库，需用启动参数和库前缀兜底识别。
        return ("neoforge".to_string(), String::new());
    }
    if has_loader_metadata_type(json, "neoforge") {
        return ("neoforge".to_string(), String::new());
    }

    if json
        .get("minecraftArguments")
        .and_then(|v| v.as_str())
        .is_some_and(|args| args.contains("FMLTweaker"))
    {
        return ("forge".to_string(), String::new());
    }
    if has_forge_runtime_marker(json, main_class) {
        return ("forge".to_string(), String::new());
    }

    ("vanilla".to_string(), String::new())
}

pub fn remove_empty_legacy_arguments(json: &mut serde_json::Value) {
    if json
        .get("minecraftArguments")
        .and_then(|v| v.as_str())
        .is_none()
    {
        return;
    }
    let remove_arguments = json.get("arguments").is_some_and(|arguments| {
        ["jvm", "game"].iter().all(|key| {
            arguments
                .get(key)
                .and_then(|value| value.as_array())
                .map(|values| values.is_empty())
                .unwrap_or(true)
        })
    });
    if remove_arguments {
        if let Some(object) = json.as_object_mut() {
            // 旧版 minecraftArguments 不能带空 arguments，否则会按新版 JVM 参数启动。
            object.remove("arguments");
        }
    }
}

pub fn strip_launcher_private_version_fields(json: &mut serde_json::Value) {
    if let Some(object) = json.as_object_mut() {
        object.remove("name");
        object.remove("mcVersion");
        object.remove("loader");
    }
    remove_empty_legacy_arguments(json);
}

#[tauri::command]
pub fn list_installed_versions(game_dir: String) -> Result<Vec<InstanceInfo>, String> {
    let dir = resolve_game_dir(&game_dir);
    let versions_path = versions_dir(&dir);
    if !versions_path.exists() {
        return Ok(vec![]);
    }
    let mut list = Vec::new();
    let mut diagnostics = Vec::new();
    let entries = std::fs::read_dir(&versions_path)
        .map_err(|error| format!("读取版本目录失败 {}: {}", versions_path.display(), error))?;
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                diagnostics.push(format!("读取版本目录项失败: {}", error));
                continue;
            }
        };
        let path = entry.path();
        match entry.file_type() {
            Ok(file_type) if file_type.is_dir() => {}
            Ok(_) => continue,
            Err(error) => {
                diagnostics.push(format!(
                    "读取版本目录类型失败 {}: {}",
                    path.display(),
                    error
                ));
                continue;
            }
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let json_path = version_json_path(&path, &name);
        let data = match std::fs::read_to_string(&json_path) {
            Ok(data) => data,
            Err(error) => {
                diagnostics.push(format!(
                    "读取版本配置失败 {}: {}",
                    json_path.display(),
                    error
                ));
                continue;
            }
        };
        let raw_json = match serde_json::from_str::<serde_json::Value>(&data) {
            Ok(json) => json,
            Err(error) => {
                diagnostics.push(format!(
                    "解析版本配置失败 {}: {}",
                    json_path.display(),
                    error
                ));
                continue;
            }
        };
        let json = match load_merged_version_json(&dir, &name) {
            Ok(merged) => merged.json,
            Err(error) => {
                // 原始配置仍可识别时保留实例，同时记录继承合并错误供诊断。
                diagnostics.push(format!(
                    "合并版本配置失败 {}: {}",
                    json_path.display(),
                    error
                ));
                raw_json
            }
        };
        let mc_version = infer_mc_version(&json);
        let (loader_type, loader_version) = detect_loader(&json, &name);
        let recommended_memory = json["recommendedMemory"].as_u64().map(|v| v as u32);
        let mod_count = match count_instance_mods(&path) {
            Ok(count) => count,
            Err(error) => {
                diagnostics.push(format!("统计版本 {} 的 Mod 失败: {}", name, error));
                0
            }
        };
        let estimated_memory = Some(estimate_memory_by_mod_count(mod_count));
        let pack_recommended_memory = json["packRecommendedMemory"].as_u64().map(|v| v as u32);
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
    if !diagnostics.is_empty() {
        let message = diagnostics.join("; ");
        eprintln!("[instance] 版本列表存在读取错误: {}", message);
        if list.is_empty() {
            return Err(format!("未能读取任何有效版本: {}", message));
        }
    }
    // 自定义命名的整合包先显示，版本号命名的实例再按 MC 版本号归位。
    list.sort_by(|a, b| {
        let a_version_named = is_version_named_instance(&a.name);
        let b_version_named = is_version_named_instance(&b.name);
        a_version_named
            .cmp(&b_version_named)
            .then_with(|| {
                if a_version_named && b_version_named {
                    compare_mc_versions(&a.mc_version, &b.mc_version)
                } else {
                    a.name.to_lowercase().cmp(&b.name.to_lowercase())
                }
            })
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(list)
}

#[tauri::command]
pub async fn delete_version(game_dir: String, name: String) -> Result<String, String> {
    tokio::task::spawn_blocking(move || {
        let dir = resolve_game_dir(&game_dir);
        let safe_name = safe_path_name(&name, "版本名")?;
        let mutation_lock = crate::mod_manager::mod_mutation_lock(&game_dir, &safe_name)?;
        let _mutation_guard = match mutation_lock.try_lock() {
            Ok(guard) => guard,
            Err(std::sync::TryLockError::WouldBlock) => {
                return Err(format!("版本 {} 正在执行相关文件任务，暂时不能删除", name));
            }
            Err(std::sync::TryLockError::Poisoned(_)) => {
                return Err("版本文件事务锁已损坏".to_string());
            }
        };
        if crate::launch::is_instance_running(&game_dir, &safe_name)? {
            return Err(format!("版本 {} 正在运行，关闭游戏后才能删除", name));
        }
        let delete_task_key = try_register_delete_task(&game_dir, &safe_name)?;
        let inst_path = version_dir(&dir, &safe_name);
        let result = if !inst_path.exists() {
            Err(format!("版本 {} 不存在", name))
        } else {
            std::fs::remove_dir_all(&inst_path)
                .map_err(|e| format!("删除失败: {}", e))
                .map(|_| format!("已删除版本: {}", name))
        };
        unregister_cancel(&delete_task_key);
        result
    })
    .await
    .map_err(|e| e.to_string())?
}

/// 使用系统文件管理器打开指定目录
#[tauri::command]
pub fn open_folder(game_dir: String, name: String, sub_dir: String) -> Result<String, String> {
    let dir = resolve_game_dir(&game_dir);
    let safe_name = safe_path_name(&name, "版本名")?;
    let mut target = version_dir(&dir, &safe_name);
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
