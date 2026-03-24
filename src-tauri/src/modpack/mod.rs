pub mod install;
pub mod cf_download;

use tauri::Emitter;
use crate::instance::resolve_game_dir;


/// 构建 HTTP client
pub(crate) fn build_http_client(connect_timeout_secs: u64, timeout_secs: u64, pool_size: usize) -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .pool_max_idle_per_host(pool_size)
        .connect_timeout(std::time::Duration::from_secs(connect_timeout_secs))
        .timeout(std::time::Duration::from_secs(timeout_secs))
        .user_agent("OAOI-Launcher/1.0")
        .build().map_err(|e| e.to_string())
}

/// 根据 MC 版本号判断所需 Java 大版本
pub(crate) fn get_required_java_major(mc_version: &str) -> u32 {
    get_required_java_major_pub(mc_version)
}

/// 公共接口：根据 MC 版本号判断所需 Java 大版本（供 installer 模块调用）
pub fn get_required_java_major_pub(mc_version: &str) -> u32 {
    let parts: Vec<&str> = mc_version.split('.').collect();
    let major = parts.first().and_then(|s| s.parse::<u32>().ok()).unwrap_or(1);
    let minor = parts.get(1).and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
    let patch = parts.get(2).and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
    if major >= 26 { return 25; }
    if minor >= 21 || (minor == 20 && patch >= 5) { return 21; }
    if minor >= 17 { return 17; }
    8
}

pub(crate) fn emit_progress(app: &tauri::AppHandle, name: &str, stage: &str, current: usize, total: usize, detail: &str) {
    eprintln!("[emit] name={}, stage={}, {}/{}", name, stage, current, total);
    let _ = app.emit("install-progress", serde_json::json!({
        "name": name, "stage": stage, "current": current, "total": total, "detail": detail
    }));
}

/// 识别整合包类型并返回元数据
fn detect_modpack(zip_path: &std::path::Path) -> Result<ModpackMeta, String> {
    let file = std::fs::File::open(zip_path).map_err(|e| format!("打开文件失败: {}", e))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("解析zip失败: {}", e))?;

    // 尝试 Modrinth (.mrpack 含 modrinth.index.json)
    if let Ok(mut entry) = archive.by_name("modrinth.index.json") {
        use std::io::Read;
        let mut content = String::new();
        entry.read_to_string(&mut content).map_err(|e| e.to_string())?;
        let json: serde_json::Value = serde_json::from_str(&content).map_err(|e| e.to_string())?;
        let mc_version = json["dependencies"]["minecraft"].as_str().unwrap_or("").to_string();
        let loader_type;
        let loader_version;
        if let Some(v) = json["dependencies"]["fabric-loader"].as_str() {
            loader_type = "fabric".to_string();
            loader_version = v.to_string();
        } else if let Some(v) = json["dependencies"]["quilt-loader"].as_str() {
            loader_type = "quilt".to_string();
            loader_version = v.to_string();
        } else if let Some(v) = json["dependencies"]["neoforge"].as_str() {
            loader_type = "neoforge".to_string();
            loader_version = v.to_string();
        } else if let Some(v) = json["dependencies"]["forge"].as_str() {
            loader_type = "forge".to_string();
            loader_version = v.to_string();
        } else {
            loader_type = "vanilla".to_string();
            loader_version = String::new();
        }
        let pack_name = json["name"].as_str().unwrap_or("modpack").to_string();
        let files: Vec<MrpackFile> = json["files"].as_array().unwrap_or(&vec![]).iter().filter_map(|f| {
            let path = f["path"].as_str()?.to_string();
            let url = f["downloads"].as_array()?.first()?.as_str()?.to_string();
            let sha1 = f["hashes"]["sha1"].as_str().map(|s| s.to_string());
            Some(MrpackFile { path, url, sha1 })
        }).collect();
        // 尝试读取 Modrinth 推荐内存（部分整合包会在 summary 或自定义字段中指定）
        let mr_memory = json["memory"].as_u64()
            .or_else(|| json["recommendedMemory"].as_u64())
            .or_else(|| json["recommended_memory"].as_u64())
            .map(|v| v as u32);
        return Ok(ModpackMeta {
            kind: ModpackKind::Modrinth { files },
            mc_version,
            loader_type,
            loader_version,
            name: pack_name,
            recommended_memory_mb: mr_memory,
        });
    }

    // 尝试 CurseForge (manifest.json)
    drop(archive);
    let file = std::fs::File::open(zip_path).map_err(|e| e.to_string())?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
    if let Ok(mut entry) = archive.by_name("manifest.json") {
        use std::io::Read;
        let mut content = String::new();
        entry.read_to_string(&mut content).map_err(|e| e.to_string())?;
        let json: serde_json::Value = serde_json::from_str(&content).map_err(|e| e.to_string())?;
        let mc_version = json["minecraft"]["version"].as_str().unwrap_or("").to_string();
        let loader_info = &json["minecraft"]["modLoaders"];
        let loader_type;
        let loader_version;
        if let Some(loaders) = loader_info.as_array() {
            let primary = loaders.iter().find(|l| l["primary"].as_bool().unwrap_or(false))
                .or_else(|| loaders.first());
            if let Some(l) = primary {
                let id = l["id"].as_str().unwrap_or("");
                if id.starts_with("fabric-") {
                    loader_type = "fabric".to_string();
                    loader_version = id.trim_start_matches("fabric-").to_string();
                } else if id.starts_with("quilt-") {
                    loader_type = "quilt".to_string();
                    loader_version = id.trim_start_matches("quilt-").to_string();
                } else if id.starts_with("neoforge-") {
                    loader_type = "neoforge".to_string();
                    loader_version = id.trim_start_matches("neoforge-").to_string();
                } else if id.starts_with("forge-") {
                    loader_type = "forge".to_string();
                    loader_version = id.trim_start_matches("forge-").to_string();
                } else {
                    loader_type = "vanilla".to_string();
                    loader_version = String::new();
                }
            } else {
                loader_type = "vanilla".to_string();
                loader_version = String::new();
            }
        } else {
            loader_type = "vanilla".to_string();
            loader_version = String::new();
        }
        let pack_name = json["name"].as_str().unwrap_or("modpack").to_string();
        let override_path = json["overrides"].as_str().unwrap_or("overrides").to_string();
        // 按 fileID 去重，防止 manifest 中出现重复条目
        let mut seen_file_ids = std::collections::HashSet::new();
        let mut cf_files: Vec<CfFile> = Vec::new();
        for f in json["files"].as_array().unwrap_or(&vec![]) {
            let project_id = match f["projectID"].as_u64() { Some(v) => v as u32, None => { eprintln!("[cf] 跳过缺少 projectID 的条目"); continue } };
            let file_id = match f["fileID"].as_u64() { Some(v) => v as u32, None => { eprintln!("[cf] 跳过缺少 fileID 的条目"); continue } };
            if !seen_file_ids.insert(file_id) { eprintln!("[cf] 跳过重复 fileID: {}", file_id); continue; }
            cf_files.push(CfFile { project_id, file_id });
        }
        // 尝试读取 CurseForge 推荐内存
        // 常见字段: memory, recommendedRam, minecraft.recommendedRam (单位可能是 MB 或 GB)
        let cf_memory = json["memory"].as_u64()
            .or_else(|| json["recommendedRam"].as_u64())
            .or_else(|| json["minecraft"]["recommendedRam"].as_u64())
            .or_else(|| json["minimumRam"].as_u64())
            .map(|v| {
                // 如果 ≤ 32 认为是 GB，转成 MB
                if v <= 32 { (v * 1024) as u32 } else { v as u32 }
            });
        return Ok(ModpackMeta {
            kind: ModpackKind::CurseForge { files: cf_files, override_path },
            mc_version,
            loader_type,
            loader_version,
            name: pack_name,
            recommended_memory_mb: cf_memory,
        });
    }

    Err("未识别的整合包格式（不含 manifest.json 或 modrinth.index.json）".to_string())
}

pub(crate) struct MrpackFile { pub path: String, pub url: String, pub sha1: Option<String> }
pub(crate) struct CfFile { pub project_id: u32, pub file_id: u32 }

pub(crate) enum ModpackKind {
    Modrinth { files: Vec<MrpackFile> },
    CurseForge { files: Vec<CfFile>, override_path: String },
}

pub(crate) struct ModpackMeta {
    pub kind: ModpackKind,
    pub mc_version: String,
    pub loader_type: String,
    pub loader_version: String,
    pub name: String,
    pub recommended_memory_mb: Option<u32>,
}

pub(crate) fn sanitize_name(name: &str) -> String {
    name.chars().map(|c| {
        if c.is_alphanumeric() || c == '-' || c == '_' || c == '.' { c } else { '_' }
    }).collect()
}

/// 根据 CurseForge API 返回的 modules 字段和文件名检测文件类型，决定安装目标目录
/// 注意：本函数只在 pid_class 映射查不到 classId 时才被调用，
/// 因此不再尝试从 item 中读取 classId（文件对象没有此字段）。
/// modules 中每个条目的 name 字段反映了 JAR 包内的顶层文件/目录结构：
///   - META-INF / mcmod.info → Mod
///   - pack.mcmeta → 材质包
///   - level.dat → 存档
/// 返回 (目标目录, 类型名称)
pub(crate) fn detect_target_dir(item: &serde_json::Value, fname: &str, inst_dir: &std::path::Path) -> (std::path::PathBuf, &'static str) {
    let mods_dir = inst_dir.join("mods");

    // 1. 通过 modules 字段判断（JAR 内部文件结构）
    if let Some(modules) = item["modules"].as_array() {
        if !modules.is_empty() {
            let module_names: Vec<&str> = modules.iter()
                .filter_map(|m| m["name"].as_str())
                .collect();
            // mod: 包含 META-INF 或 mcmod.info
            if module_names.contains(&"META-INF") || module_names.contains(&"mcmod.info") {
                return (mods_dir, "mod");
            }
            // 材质包: 包含 pack.mcmeta
            if module_names.contains(&"pack.mcmeta") {
                let dir = inst_dir.join("resourcepacks");
                std::fs::create_dir_all(&dir).ok();
                return (dir, "材质包");
            }
            // 存档: 包含 level.dat
            if module_names.contains(&"level.dat") {
                let dir = inst_dir.join("saves");
                std::fs::create_dir_all(&dir).ok();
                return (dir, "存档");
            }
        }
    }

    // 2. .jar 文件默认是 mod
    if fname.ends_with(".jar") {
        return (mods_dir, "mod");
    }

    // 3. 通过文件名特征判断光影/材质包/配置文件
    let fname_lower = fname.to_lowercase();
    if fname_lower.ends_with(".zip") {
        // 光影包: 通常包含 shaders 相关关键词
        if fname_lower.contains("shader") || fname_lower.contains("shaders") {
            let dir = inst_dir.join("shaderpacks");
            std::fs::create_dir_all(&dir).ok();
            return (dir, "光影");
        }
        // 配置文件包
        if fname_lower.contains("config") || fname_lower.contains("configuration") {
            let dir = inst_dir.join("config");
            std::fs::create_dir_all(&dir).ok();
            return (dir, "配置");
        }
        // 其余 .zip 默认是材质包
        let dir = inst_dir.join("resourcepacks");
        std::fs::create_dir_all(&dir).ok();
        return (dir, "材质包");
    }

    // 默认当 mod
    (mods_dir, "mod")
}

#[tauri::command]
pub async fn import_modpack(
    app_handle: tauri::AppHandle,
    zip_path: String,
    game_dir: String,
    java_path: String,
    use_mirror: bool,
) -> Result<String, String> {
    let (tx, rx) = std::sync::mpsc::channel::<Result<String, String>>();
    let app2 = app_handle.clone();
    std::thread::spawn(move || {
        let result = do_import_modpack(&app_handle, &zip_path, &game_dir, &java_path, use_mirror);
        if let Err(ref e) = result {
            let _ = app2.emit("install-progress", serde_json::json!({
                "name": "整合包", "stage": "error", "current": 0, "total": 0, "detail": e
            }));
        }
        let _ = tx.send(result);
    });
    rx.recv().map_err(|_| "线程通信失败".to_string())?
}

pub fn do_import_modpack(
    app: &tauri::AppHandle,
    zip_path: &str,
    game_dir_input: &str,
    java_path: &str,
    use_mirror: bool,
) -> Result<String, String> {
    do_import_modpack_named(app, zip_path, game_dir_input, java_path, use_mirror, None)
}

pub fn do_import_modpack_named(
    app: &tauri::AppHandle,
    zip_path: &str,
    game_dir_input: &str,
    java_path: &str,
    use_mirror: bool,
    display_name: Option<&str>,
) -> Result<String, String> {
    let zip_file = std::path::Path::new(zip_path);
    let temp_name = display_name.unwrap_or("整合包");
    emit_progress(app, temp_name, "detecting", 0, 1, "正在识别整合包格式...");

    let meta = detect_modpack(zip_file)?;
    let inst_name = sanitize_name(&meta.name);
    let game_dir = resolve_game_dir(game_dir_input);
    let inst_dir = game_dir.join("instances").join(&inst_name);

    // 使用传入的 display_name 或 inst_name
    let name = display_name.unwrap_or(&inst_name);
    emit_progress(app, name, "detecting", 1, 1, "识别完成");

    if inst_dir.exists() {
        return Err(format!("实例 '{}' 已存在", inst_name));
    }

    // 包装安装，失败时自动清理目录
    let result = install::do_install_modpack_inner(app, zip_file, game_dir_input, java_path, use_mirror, &meta, &inst_dir, &game_dir, name);
    if let Err(ref e) = result {
        if inst_dir.exists() {
            let _ = std::fs::remove_dir_all(&inst_dir);
            eprintln!("[modpack] 安装失败，已清理: {}", inst_dir.display());
        }
        emit_progress(app, name, "error", 0, 0, e);
    }
    result
}
