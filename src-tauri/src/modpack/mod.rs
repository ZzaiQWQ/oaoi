pub mod cf_download;
pub mod install;

use crate::instance::{register_cancel, resolve_game_dir, safe_path_name, unregister_cancel};
use tauri::Emitter;

/// 构建 HTTP client
pub(crate) fn build_http_client(
    connect_timeout_secs: u64,
    timeout_secs: u64,
    pool_size: usize,
) -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .pool_max_idle_per_host(pool_size)
        .connect_timeout(std::time::Duration::from_secs(connect_timeout_secs))
        .timeout(std::time::Duration::from_secs(timeout_secs))
        .user_agent("OAOI-Launcher/1.0")
        .build()
        .map_err(|e| e.to_string())
}

/// 根据 MC 版本号判断所需 Java 大版本
pub(crate) fn get_required_java_major(mc_version: &str) -> u32 {
    get_required_java_major_pub(mc_version)
}

/// 公共接口：根据 MC 版本号判断所需 Java 大版本（供 installer 模块调用）
pub fn get_required_java_major_pub(mc_version: &str) -> u32 {
    let parts: Vec<&str> = mc_version.split('.').collect();
    let major = parts
        .first()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(1);
    let minor = parts
        .get(1)
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0);
    let patch = parts
        .get(2)
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0);
    if major >= 26 {
        return 25;
    }
    if minor >= 21 || (minor == 20 && patch >= 5) {
        return 21;
    }
    if minor >= 17 {
        return 17;
    }
    8
}

pub(crate) fn emit_progress(
    app: &tauri::AppHandle,
    name: &str,
    stage: &str,
    current: usize,
    total: usize,
    detail: &str,
) {
    eprintln!(
        "[emit] name={}, stage={}, {}/{}",
        name, stage, current, total
    );
    let _ = app.emit(
        "install-progress",
        serde_json::json!({
            "name": name, "stage": stage, "current": current, "total": total, "detail": detail
        }),
    );
}

/// 识别整合包类型并返回元数据
fn detect_modpack(zip_path: &std::path::Path) -> Result<ModpackMeta, String> {
    let file = std::fs::File::open(zip_path).map_err(|e| format!("打开文件失败: {}", e))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("解析zip失败: {}", e))?;

    // 尝试 Modrinth (.mrpack 含 modrinth.index.json)
    if let Ok(mut entry) = archive.by_name("modrinth.index.json") {
        use std::io::Read;
        let mut content = String::new();
        entry
            .read_to_string(&mut content)
            .map_err(|e| e.to_string())?;
        let json: serde_json::Value = serde_json::from_str(&content).map_err(|e| e.to_string())?;
        let mc_version = json["dependencies"]["minecraft"]
            .as_str()
            .unwrap_or("")
            .to_string();
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
        let pack_name = clean_modpack_name(json["name"].as_str().unwrap_or("modpack"));
        let files: Vec<MrpackFile> = json["files"]
            .as_array()
            .unwrap_or(&vec![])
            .iter()
            .filter_map(|f| {
                let path = f["path"].as_str()?.to_string();
                let url = f["downloads"].as_array()?.first()?.as_str()?.to_string();
                let sha1 = f["hashes"]["sha1"].as_str().map(|s| s.to_string());
                Some(MrpackFile { path, url, sha1 })
            })
            .collect();
        // 尝试读取 Modrinth 推荐内存（部分整合包会在 summary 或自定义字段中指定）
        let mr_memory = json["memory"]
            .as_u64()
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
        entry
            .read_to_string(&mut content)
            .map_err(|e| e.to_string())?;
        let json: serde_json::Value = serde_json::from_str(&content).map_err(|e| e.to_string())?;
        let mc_version = json["minecraft"]["version"]
            .as_str()
            .unwrap_or("")
            .to_string();
        let loader_info = &json["minecraft"]["modLoaders"];
        let loader_type;
        let loader_version;
        if let Some(loaders) = loader_info.as_array() {
            let primary = loaders
                .iter()
                .find(|l| l["primary"].as_bool().unwrap_or(false))
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
        let pack_name = clean_modpack_name(json["name"].as_str().unwrap_or("modpack"));
        let override_path = json["overrides"]
            .as_str()
            .unwrap_or("overrides")
            .to_string();
        // 按 fileID 去重，防止 manifest 中出现重复条目
        let mut seen_file_ids = std::collections::HashSet::new();
        let mut cf_files: Vec<CfFile> = Vec::new();
        for f in json["files"].as_array().unwrap_or(&vec![]) {
            let project_id = match f["projectID"].as_u64() {
                Some(v) => v as u32,
                None => {
                    eprintln!("[cf] 跳过缺少 projectID 的条目");
                    continue;
                }
            };
            let file_id = match f["fileID"].as_u64() {
                Some(v) => v as u32,
                None => {
                    eprintln!("[cf] 跳过缺少 fileID 的条目");
                    continue;
                }
            };
            if !seen_file_ids.insert(file_id) {
                eprintln!("[cf] 跳过重复 fileID: {}", file_id);
                continue;
            }
            cf_files.push(CfFile {
                project_id,
                file_id,
            });
        }
        // 尝试读取 CurseForge 推荐内存
        // 常见字段: memory, recommendedRam, minecraft.recommendedRam (单位可能是 MB 或 GB)
        let cf_memory = json["memory"]
            .as_u64()
            .or_else(|| json["recommendedRam"].as_u64())
            .or_else(|| json["minecraft"]["recommendedRam"].as_u64())
            .or_else(|| json["minimumRam"].as_u64())
            .map(|v| {
                // 如果 ≤ 32 认为是 GB，转成 MB
                if v <= 32 {
                    (v * 1024) as u32
                } else {
                    v as u32
                }
            });
        return Ok(ModpackMeta {
            kind: ModpackKind::CurseForge {
                files: cf_files,
                override_path,
            },
            mc_version,
            loader_type,
            loader_version,
            name: pack_name,
            recommended_memory_mb: cf_memory,
        });
    }

    Err("未识别的整合包格式（不含 manifest.json 或 modrinth.index.json）".to_string())
}

pub(crate) struct MrpackFile {
    pub path: String,
    pub url: String,
    pub sha1: Option<String>,
}
pub(crate) struct CfFile {
    pub project_id: u32,
    pub file_id: u32,
}

pub(crate) enum ModpackKind {
    Modrinth {
        files: Vec<MrpackFile>,
    },
    CurseForge {
        files: Vec<CfFile>,
        override_path: String,
    },
}

pub(crate) struct ModpackMeta {
    pub kind: ModpackKind,
    pub mc_version: String,
    pub loader_type: String,
    pub loader_version: String,
    pub name: String,
    pub recommended_memory_mb: Option<u32>,
}

pub(crate) fn strip_modpack_archive_suffix(name: &str) -> String {
    let mut cleaned = name.trim();
    loop {
        let lower = cleaned.to_ascii_lowercase();
        let next = if lower.ends_with(".mrpack") {
            Some(cleaned[..cleaned.len() - ".mrpack".len()].trim_end())
        } else if lower.ends_with(".zip") {
            Some(cleaned[..cleaned.len() - ".zip".len()].trim_end())
        } else {
            None
        };
        match next {
            Some(value) if !value.trim().is_empty() => cleaned = value,
            _ => break,
        }
    }
    cleaned.to_string()
}

fn clean_modpack_name(name: &str) -> String {
    let cleaned = strip_modpack_archive_suffix(name);
    if cleaned.trim().is_empty() {
        "modpack".to_string()
    } else {
        cleaned
    }
}

pub(crate) fn sanitize_name(name: &str) -> String {
    let cleaned = strip_modpack_archive_suffix(name);
    let source = if cleaned.trim().is_empty() {
        name.trim()
    } else {
        cleaned.trim()
    };
    source
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// 根据 CurseForge API 返回的 modules 字段和文件名检测文件类型，决定安装目标目录
/// 注意：本函数只在 pid_class 映射查不到 classId 时才被调用，
/// 因此不再尝试从 item 中读取 classId（文件对象没有此字段）。
/// modules 中每个条目的 name 字段反映了 JAR 包内的顶层文件/目录结构：
///   - META-INF / mcmod.info → Mod
///   - pack.mcmeta → 材质包
///   - level.dat → 存档
/// 返回 (目标目录, 类型名称)
pub(crate) fn detect_target_dir(
    item: &serde_json::Value,
    fname: &str,
    inst_dir: &std::path::Path,
) -> (std::path::PathBuf, &'static str) {
    let mods_dir = inst_dir.join("mods");

    // 1. 通过 modules 字段判断（JAR 内部文件结构）
    if let Some(modules) = item["modules"].as_array() {
        if !modules.is_empty() {
            let module_names: Vec<&str> =
                modules.iter().filter_map(|m| m["name"].as_str()).collect();
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
    display_name: Option<String>,
) -> Result<String, String> {
    let app2 = app_handle.clone();
    let progress_name = display_name
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| {
            std::path::Path::new(&zip_path)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("整合包")
                .to_string()
        });
    let cancel_flag = register_cancel(&progress_name);
    std::thread::spawn(move || {
        let result = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            do_import_modpack_named(
                &app_handle,
                &zip_path,
                &game_dir,
                &java_path,
                use_mirror,
                Some(&progress_name),
            )
        })) {
            Ok(result) => result,
            Err(payload) => Err(format!(
                "导入线程崩溃: {}",
                panic_payload_to_string(payload)
            )),
        };
        unregister_cancel(&progress_name);
        if let Err(ref e) = result {
            let stage = if cancel_flag.load(std::sync::atomic::Ordering::Relaxed) {
                "cancelled"
            } else {
                "error"
            };
            let _ = app2.emit(
                "install-progress",
                serde_json::json!({
                    "name": &progress_name, "stage": stage, "current": 0, "total": 0, "detail": e
                }),
            );
        }
    });
    Ok("importing".to_string())
}

fn panic_payload_to_string(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "未知错误".to_string()
    }
}

fn create_unique_instance_dir(
    game_dir: &std::path::Path,
    base_name: &str,
) -> Result<(String, std::path::PathBuf), String> {
    let instances_dir = game_dir.join("instances");
    std::fs::create_dir_all(&instances_dir).map_err(|e| e.to_string())?;

    for index in 0..1000 {
        let name = if index == 0 {
            base_name.to_string()
        } else {
            format!("{}-{}", base_name, index)
        };
        let inst_dir = instances_dir.join(&name);
        match std::fs::create_dir(&inst_dir) {
            Ok(()) => return Ok((name, inst_dir)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("创建版本目录失败: {}", e)),
        }
    }

    Err(format!("无法为 '{}' 找到可用版本名", base_name))
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
    let base_inst_name = safe_path_name(&sanitize_name(&meta.name), "版本名")?;
    let game_dir = resolve_game_dir(game_dir_input);
    let (inst_name, inst_dir) = create_unique_instance_dir(&game_dir, &base_inst_name)?;

    // 使用传入的 display_name 或 inst_name
    let name = display_name.unwrap_or(&inst_name);
    emit_progress(app, name, "detecting", 1, 1, "识别完成");
    let install_marker_path = inst_dir.join(".oaoi_installing");
    if let Err(e) = std::fs::write(
        &install_marker_path,
        format!("pid={}\nmodpack={}\n", std::process::id(), inst_name),
    ) {
        let _ = std::fs::remove_dir_all(&inst_dir);
        return Err(format!("创建安装标记失败: {}", e));
    }

    // 包装安装，失败时自动清理目录
    let result = install::do_install_modpack_inner(
        app,
        zip_file,
        game_dir_input,
        java_path,
        use_mirror,
        &meta,
        &inst_dir,
        &game_dir,
        name,
    );
    if let Err(ref e) = result {
        if install_marker_path.exists() {
            let _ = std::fs::remove_dir_all(&inst_dir);
            eprintln!("[modpack] 安装失败，已清理: {}", inst_dir.display());
        } else if inst_dir.exists() {
            eprintln!("[modpack] 跳过清理非本次安装目录: {}", inst_dir.display());
        }
        let stage = if crate::instance::is_cancelled(name) {
            "cancelled"
        } else {
            "error"
        };
        emit_progress(app, name, stage, 0, 0, e);
    } else {
        let _ = std::fs::remove_file(&install_marker_path);
    }
    result
}
