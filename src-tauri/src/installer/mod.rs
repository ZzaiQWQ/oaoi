pub mod vanilla;
pub mod fabric;
pub mod forge;
pub mod neoforge;
pub mod quilt;

// ===== 公共工具函数（供 forge/neoforge 共用） =====

/// Maven 坐标转文件路径: "net.minecraftforge:forge:1.21.1-52.0.1" → "net/minecraftforge/forge/1.21.1-52.0.1/forge-1.21.1-52.0.1.jar"
pub fn maven_name_to_path(name: &str) -> String {
    let parts: Vec<&str> = name.split(':').collect();
    if parts.len() < 3 { return name.to_string(); }
    let group = parts[0].replace('.', "/");
    let artifact = parts[1];
    let version = parts[2];
    let classifier = if parts.len() >= 4 { Some(parts[3]) } else { None };
    let (version, ext) = if let Some(at_pos) = version.find('@') {
        (&version[..at_pos], &version[at_pos+1..])
    } else if let Some(c) = &classifier {
        if let Some(at_pos) = c.find('@') {
            let ext = &c[at_pos+1..];
            let cl = &c[..at_pos];
            return format!("{}/{}/{}/{}-{}-{}.{}", group, artifact, version, artifact, version, cl, ext);
        } else {
            (version, "jar")
        }
    } else {
        (version, "jar")
    };
    if let Some(cl) = classifier {
        format!("{}/{}/{}/{}-{}-{}.{}", group, artifact, version, artifact, version, cl, ext)
    } else {
        format!("{}/{}/{}/{}-{}.{}", group, artifact, version, artifact, version, ext)
    }
}

/// 构建 data 变量映射（供 Forge/NeoForge processor 使用）
pub fn build_data_map(
    profile: &serde_json::Value,
    libs_dir: &std::path::Path,
    client_jar: &std::path::Path,
    ver_json_path: &std::path::Path,
    installer_path: &std::path::Path,
    temp_dir: &std::path::Path,
    _mc_version: &str,
) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    map.insert("SIDE".to_string(), "client".to_string());
    map.insert("ROOT".to_string(), libs_dir.parent().unwrap_or(libs_dir).to_string_lossy().to_string());
    map.insert("MINECRAFT_JAR".to_string(), client_jar.to_string_lossy().to_string());
    map.insert("MINECRAFT_VERSION".to_string(), ver_json_path.to_string_lossy().to_string());
    map.insert("INSTALLER".to_string(), installer_path.to_string_lossy().to_string());
    map.insert("LIBRARY_DIR".to_string(), libs_dir.to_string_lossy().to_string());
    if let Some(data) = profile["data"].as_object() {
        for (key, val) in data {
            let v = val["client"].as_str().unwrap_or(val.as_str().unwrap_or(""));
            if v.starts_with('[') && v.ends_with(']') {
                let coord = &v[1..v.len()-1];
                let path = maven_name_to_path(coord);
                map.insert(key.clone(), libs_dir.join(path.replace('/', std::path::MAIN_SEPARATOR_STR)).to_string_lossy().to_string());
            } else if v.starts_with('/') {
                let real_path = temp_dir.join(&v[1..]);
                map.insert(key.clone(), real_path.to_string_lossy().to_string());
            } else {
                map.insert(key.clone(), v.to_string());
            }
        }
    }
    map
}

/// 解析 processor 参数中的 {DATA} 和 [maven] 引用
pub fn resolve_data_arg(
    s: &str,
    data_map: &std::collections::HashMap<String, String>,
    libs_dir: &std::path::Path,
) -> String {
    if s.starts_with('{') && s.ends_with('}') {
        let key = &s[1..s.len()-1];
        data_map.get(key).cloned().unwrap_or_else(|| s.to_string())
    } else if s.starts_with('[') && s.ends_with(']') {
        let coord = &s[1..s.len()-1];
        let path = maven_name_to_path(coord);
        libs_dir.join(path.replace('/', std::path::MAIN_SEPARATOR_STR)).to_string_lossy().to_string()
    } else {
        s.to_string()
    }
}

/// 从 jar 文件的 MANIFEST.MF 获取 Main-Class
pub fn get_jar_main_class(jar_path: &std::path::Path) -> Option<String> {
    let file = std::fs::File::open(jar_path).ok()?;
    let mut archive = zip::ZipArchive::new(file).ok()?;
    let mut entry = archive.by_name("META-INF/MANIFEST.MF").ok()?;
    let mut content = String::new();
    std::io::Read::read_to_string(&mut entry, &mut content).ok()?;
    for line in content.lines() {
        if line.starts_with("Main-Class:") {
            return Some(line.trim_start_matches("Main-Class:").trim().to_string());
        }
    }
    None
}

/// 合并库列表（按 group:artifact 去重），供所有 loader 共用
pub fn merge_libraries(existing_libs: &mut Vec<serde_json::Value>, new_libs: &[serde_json::Value]) {
    for new_lib in new_libs {
        let new_name = new_lib["name"].as_str().unwrap_or("");
        let new_parts: Vec<&str> = new_name.split(':').collect();
        let new_key = if new_parts.len() >= 4 {
            format!("{}:{}:{}", new_parts[0], new_parts[1], new_parts[3])
        } else if new_parts.len() >= 2 {
            format!("{}:{}", new_parts[0], new_parts[1])
        } else { String::new() };

        if !new_key.is_empty() {
            existing_libs.retain(|existing| {
                let name = existing["name"].as_str().unwrap_or("");
                let parts: Vec<&str> = name.split(':').collect();
                if parts.len() >= 2 {
                    let key = if parts.len() >= 4 {
                        format!("{}:{}:{}", parts[0], parts[1], parts[3])
                    } else {
                        format!("{}:{}", parts[0], parts[1])
                    };
                    key != new_key
                } else { true }
            });
        }
        existing_libs.push(new_lib.clone());
    }
}

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

/// 实际执行单次下载（流式写入，不一次性读到内存）
fn do_download(http: &reqwest::blocking::Client, url: &str, dest: &std::path::Path) -> Result<(), String> {
    let mut resp = http.get(url).send().map_err(|e| format!("请求失败: {}", e))?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    // 先写到 .tmp 文件，下载完成后再 rename，防止中断损坏
    let tmp_path = dest.with_extension("tmp");
    {
        let mut file = std::fs::File::create(&tmp_path).map_err(|e| format!("创建临时文件失败: {}", e))?;
        std::io::copy(&mut resp, &mut file).map_err(|e| {
            let _ = std::fs::remove_file(&tmp_path);
            format!("写入失败: {}", e)
        })?;
    }
    std::fs::rename(&tmp_path, dest).map_err(|e| format!("重命名失败: {}", e))?;
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
            let inst_dir = resolve_game_dir(&game_dir).join("instances").join(&name);
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

    // Forge/NeoForge 需要 Java，如果前端没传则自动查找/下载
    let effective_java: String;
    let java_to_use = if !java_path.is_empty() {
        java_path
    } else if loader_type == "forge" || loader_type == "neoforge" {
        let required_major = crate::modpack::get_required_java_major_pub(mc_version);
        let javas = crate::java_detect::find_java(Some(game_dir_input.to_string()));
        if let Some(j) = javas.iter().find(|j| j.major == required_major) {
            effective_java = j.path.clone();
            &effective_java
        } else {
            emit("java", 0, 1, &format!("自动下载 Java {}...", required_major));
            match crate::java_download::download_java_sync(required_major, game_dir_input) {
                Ok(p) => { effective_java = p; &effective_java }
                Err(e) => return Err(format!("安装 {} 需要 Java {}，自动下载失败: {}", loader_type, required_major, e))
            }
        }
    } else {
        java_path
    };

    // 处理 Mod Loader
    match loader_type {
        "fabric" if !loader_version.is_empty() => {
            fabric::install_fabric(app_handle, name, mc_version, loader_version, &game_dir, &inst_dir, &http, use_mirror, &mut ver_json)?;
        }
        "forge" if !loader_version.is_empty() => {
            forge::install_forge(app_handle, name, mc_version, loader_version, &game_dir, &inst_dir, &http, java_to_use, use_mirror, &mut ver_json)?;
        }
        "quilt" if !loader_version.is_empty() => {
            quilt::install_quilt(app_handle, name, mc_version, loader_version, &game_dir, &inst_dir, &http, use_mirror, &mut ver_json)?;
        }
        "neoforge" if !loader_version.is_empty() => {
            neoforge::install_neoforge(app_handle, name, mc_version, loader_version, &game_dir, &inst_dir, &http, java_to_use, use_mirror, &mut ver_json)?;
        }
        _ => {}
    }

    // 写回最终配置到 instance.json
    std::fs::write(&inst_json_path, serde_json::to_string_pretty(&ver_json).unwrap())
        .map_err(|e| format!("保存实例配置失败: {}", e))?;

    emit("done", 1, 1, &format!("实例 '{}' 创建完成！", name));
    Ok(format!("实例 {} 创建成功", name))
}
