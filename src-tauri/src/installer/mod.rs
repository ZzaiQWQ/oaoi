pub mod vanilla;
pub mod fabric;
pub mod forge;
pub mod neoforge;
pub mod quilt;

use tauri::Emitter;
use std::sync::Mutex;
use crate::instance::resolve_game_dir;

/// Forge / NeoForge 安装器全局锁 — 同一时间只能运行一个安装
pub static FORGE_LOCK: std::sync::LazyLock<Mutex<()>> = std::sync::LazyLock::new(|| Mutex::new(()));

/// 将官方 URL 替换为 BMCLAPI 国内镜像
pub fn mirror_url(url: &str, use_mirror: bool) -> String {
    if !use_mirror {
        return url.to_string();
    }
    url.replace("https://piston-meta.mojang.com", "https://bmclapi2.bangbang93.com")
       .replace("https://piston-data.mojang.com", "https://bmclapi2.bangbang93.com")
       .replace("https://launchermeta.mojang.com", "https://bmclapi2.bangbang93.com")
       .replace("https://launcher.mojang.com", "https://bmclapi2.bangbang93.com")
       .replace("https://libraries.minecraft.net", "https://bmclapi2.bangbang93.com/maven")
       .replace("https://resources.download.minecraft.net", "https://bmclapi2.bangbang93.com/assets")
       .replace("https://maven.minecraftforge.net", "https://bmclapi2.bangbang93.com/maven")
       .replace("https://files.minecraftforge.net/maven", "https://bmclapi2.bangbang93.com/maven")
       .replace("https://maven.fabricmc.net", "https://bmclapi2.bangbang93.com/maven")
       .replace("https://maven.neoforged.net/releases", "https://bmclapi2.bangbang93.com/maven")
       .replace("https://maven.quiltmc.org/repository/release", "https://bmclapi2.bangbang93.com/maven")
}

/// 下载文件，如果已存在且 sha1 匹配则跳过
pub fn download_file_if_needed(http: &reqwest::blocking::Client, url: &str, dest: &std::path::Path, expected_sha1: Option<&str>, use_mirror: bool) -> Result<bool, String> {
    if dest.exists() {
        if let Some(sha1) = expected_sha1 {
            if let Ok(data) = std::fs::read(dest) {
                let hash = sha1_smol::Sha1::from(&data).digest().to_string();
                if hash == sha1 {
                    return Ok(false);
                }
            }
        } else {
            return Ok(false);
        }
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    
    let real_url = mirror_url(url, use_mirror);
    let max_retries = 5;
    let mut last_err = String::new();
    for attempt in 0..max_retries {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_secs(1 << (attempt - 1)));
        }
        match do_download(http, &real_url, dest) {
            Ok(()) => return Ok(true),
            Err(e) => {
                last_err = e;
                eprintln!("[download] 重试 {}/{}: {} ({})", attempt + 1, max_retries, last_err, real_url);
            }
        }
    }
    
    // 选官方失败 → 自动回退镜像再试
    if !use_mirror {
        let fallback_url = mirror_url(url, true);
        if fallback_url != real_url {
            eprintln!("[download] 官方源失败，回退镜像: {}", fallback_url);
            for attempt in 0..3 {
                if attempt > 0 {
                    std::thread::sleep(std::time::Duration::from_secs(1 << (attempt - 1)));
                }
                match do_download(http, &fallback_url, dest) {
                    Ok(()) => return Ok(true),
                    Err(e) => {
                        last_err = e;
                        eprintln!("[download] 镜像重试 {}/3: {} ({})", attempt + 1, last_err, fallback_url);
                    }
                }
            }
        }
    }
    
    Err(format!("下载失败(重试后): {} ({})", last_err, real_url))
}

/// 实际执行单次下载
fn do_download(http: &reqwest::blocking::Client, url: &str, dest: &std::path::Path) -> Result<(), String> {
    let resp = http.get(url).send().map_err(|e| format!("请求失败: {}", e))?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    let bytes = resp.bytes().map_err(|e| format!("读取失败: {}", e))?;
    std::fs::write(dest, &bytes).map_err(|e| format!("写入失败: {}", e))?;
    Ok(())
}

/// 限制并发的下载执行器
pub fn parallel_download(
    http: &reqwest::blocking::Client,
    tasks: Vec<(String, std::path::PathBuf, Option<String>)>,
    done: &std::sync::Arc<std::sync::atomic::AtomicUsize>,
    max_workers: usize,
    use_mirror: bool,
) {
    for chunk in tasks.chunks(max_workers) {
        let handles: Vec<_> = chunk.iter().map(|(url, dest, sha1)| {
            let url = url.clone();
            let dest = dest.clone();
            let sha1 = sha1.clone();
            let done = done.clone();
            let h = http.clone();
            std::thread::spawn(move || {
                if let Err(e) = download_file_if_needed(&h, &url, &dest, sha1.as_deref(), use_mirror) {
                    eprintln!("[download] 失败: {} -> {}", url, e);
                }
                done.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            })
        }).collect();
        for h in handles { let _ = h.join(); }
    }
}

/// 检查 library 的 rules 是否允许当前 OS
pub fn library_allowed(rules: &Option<Vec<serde_json::Value>>) -> bool {
    let rules = match rules {
        Some(r) => r,
        None => return true,
    };
    let mut dominated_match = false;
    for rule in rules {
        let action = rule.get("action").and_then(|v| v.as_str()).unwrap_or("");
        let os_name = rule.get("os").and_then(|o| o.get("name")).and_then(|v| v.as_str());
        match (action, os_name) {
            ("allow", Some("windows")) => return true,
            ("allow", None) => dominated_match = true,
            ("disallow", Some("windows")) => return false,
            _ => {}
        }
    }
    dominated_match
}

/// 递归复制目录
pub fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    if !dst.exists() { std::fs::create_dir_all(dst)?; }
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let dest_path = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&entry.path(), &dest_path)?;
        } else {
            std::fs::copy(entry.path(), &dest_path)?;
        }
    }
    Ok(())
}

/// 用于 emit 安装进度的辅助类型
pub type EmitFn<'a> = Box<dyn Fn(&str, usize, usize, &str) + 'a>;

pub fn make_emitter<'a>(app_handle: &'a tauri::AppHandle, inst_name: &'a str) -> EmitFn<'a> {
    Box::new(move |stage: &str, current: usize, total: usize, detail: &str| {
        let _ = app_handle.emit("install-progress", serde_json::json!({
            "name": inst_name, "stage": stage, "current": current, "total": total, "detail": detail
        }));
    })
}

#[tauri::command]
pub fn create_instance(
    app_handle: tauri::AppHandle, 
    name: String, 
    mc_version: String, 
    meta_url: String, 
    game_dir: String, 
    loader_type: String, 
    loader_version: String,
    java_path: String,
    use_mirror: bool
) -> Result<String, String> {
    let name_clone = name.clone();
    std::thread::spawn(move || {
        eprintln!("[install] 开始创建实例: {} (mc={}, loader={} {}, java={})", name, mc_version, loader_type, loader_version, java_path);
        if let Err(e) = do_create_instance(&app_handle, &name, &mc_version, &meta_url, &game_dir, &loader_type, &loader_version, &java_path, use_mirror) {
            eprintln!("[install] 错误: {}", e);
            let inst_dir = std::path::PathBuf::from(&game_dir).join("instances").join(&name);
            if inst_dir.exists() {
                let _ = std::fs::remove_dir_all(&inst_dir);
                eprintln!("[install] 已清理残留目录: {}", inst_dir.display());
            }
            let _ = app_handle.emit("install-progress", serde_json::json!({
                "name": name, "stage": "error", "current": 0, "total": 0, "detail": e
            }));
        }
    });
    Ok(format!("开始创建实例: {}", name_clone))
}

fn do_create_instance(
    app_handle: &tauri::AppHandle,
    name: &str,
    mc_version: &str,
    meta_url: &str,
    game_dir_input: &str,
    loader_type: &str,
    loader_version: &str,
    java_path: &str,
    use_mirror: bool
) -> Result<String, String> {
    // 路径安全校验
    if name.is_empty()
        || name.contains("..")
        || name.contains('/')
        || name.contains('\\')
        || name.contains(':')
        || name.contains('*')
        || name.contains('?')
        || name.contains('"')
        || name.contains('<')
        || name.contains('>')
        || name.contains('|')
    {
        return Err(format!("实例名 '{}' 包含非法字符", name));
    }
    let game_dir = resolve_game_dir(game_dir_input);
    let emit = make_emitter(app_handle, name);

    let inst_dir = game_dir.join("instances").join(name);
    if inst_dir.exists() {
        return Err(format!("实例 '{}' 已存在，请换一个名称！", name));
    }
    std::fs::create_dir_all(&inst_dir).map_err(|e| e.to_string())?;
    let inst_json_path = inst_dir.join("instance.json");

    let http = reqwest::blocking::Client::builder()
        .pool_max_idle_per_host(16)
        .connect_timeout(std::time::Duration::from_secs(15))
        .timeout(std::time::Duration::from_secs(60))
        .user_agent("OAOI-Launcher/1.0")
        .build().map_err(|e| e.to_string())?;

    // 下载 vanilla 基础（client.jar + libraries + assets）
    let mut ver_json = vanilla::install_vanilla(app_handle, name, mc_version, meta_url, &game_dir, &inst_dir, &http, use_mirror)?;

    // 处理 Mod Loader
    match loader_type {
        "fabric" if !loader_version.is_empty() => {
            fabric::install_fabric(app_handle, name, mc_version, loader_version, &game_dir, &inst_dir, &http, use_mirror, &mut ver_json)?;
        }
        "forge" if !loader_version.is_empty() => {
            forge::install_forge(app_handle, name, mc_version, loader_version, &game_dir, &inst_dir, &http, java_path, use_mirror, &mut ver_json)?;
        }
        "quilt" if !loader_version.is_empty() => {
            quilt::install_quilt(app_handle, name, mc_version, loader_version, &game_dir, &inst_dir, &http, use_mirror, &mut ver_json)?;
        }
        "neoforge" if !loader_version.is_empty() => {
            neoforge::install_neoforge(app_handle, name, mc_version, loader_version, &game_dir, &inst_dir, &http, java_path, use_mirror, &mut ver_json)?;
        }
        _ => {}
    }

    // 写回最终配置到 instance.json
    std::fs::write(&inst_json_path, serde_json::to_string_pretty(&ver_json).unwrap())
        .map_err(|e| format!("保存实例配置失败: {}", e))?;

    emit("done", 1, 1, &format!("实例 '{}' 创建完成！", name));
    Ok(format!("实例 {} 创建成功", name))
}
