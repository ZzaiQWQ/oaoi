pub mod cf_download;
pub mod download_mirror;
pub mod install;

use crate::instance::{
    resolve_game_dir, safe_path_name, try_register_cancel, unregister_cancel,
    versions_dir as minecraft_versions_dir,
};
use std::io::{Read, Seek};
use tauri::Emitter;

pub(crate) const MAX_MODPACK_ARCHIVE_ENTRIES: usize = 50_000;
pub(crate) const MAX_MODPACK_ENTRY_SIZE: u64 = 1024 * 1024 * 1024;
pub(crate) const MAX_MODPACK_TOTAL_SIZE: u64 = 16 * 1024 * 1024 * 1024;
const MAX_MODPACK_MANIFEST_SIZE: u64 = 16 * 1024 * 1024;

pub(crate) fn validate_modpack_archive<R: Read + Seek>(
    archive: &mut zip::ZipArchive<R>,
) -> Result<(), String> {
    validate_modpack_archive_with_check(archive, || Ok(()))
}

fn validate_modpack_archive_with_check<R, F>(
    archive: &mut zip::ZipArchive<R>,
    mut check_cancelled: F,
) -> Result<(), String>
where
    R: Read + Seek,
    F: FnMut() -> Result<(), String>,
{
    if archive.len() > MAX_MODPACK_ARCHIVE_ENTRIES {
        return Err(format!(
            "整合包文件数量超过限制: {} > {}",
            archive.len(),
            MAX_MODPACK_ARCHIVE_ENTRIES
        ));
    }

    let mut total_size = 0u64;
    for index in 0..archive.len() {
        check_cancelled()?;
        let entry = archive
            .by_index(index)
            .map_err(|e| format!("读取整合包条目失败: {}", e))?;
        let size = entry.size();
        if size > MAX_MODPACK_ENTRY_SIZE {
            return Err(format!(
                "整合包单个文件超过限制: {} ({} bytes)",
                entry.name(),
                size
            ));
        }
        total_size = total_size
            .checked_add(size)
            .ok_or_else(|| "整合包展开大小溢出".to_string())?;
        if total_size > MAX_MODPACK_TOTAL_SIZE {
            return Err(format!("整合包总展开大小超过限制: {} bytes", total_size));
        }
    }
    Ok(())
}

fn ensure_detection_not_cancelled(cancel_name: &str) -> Result<(), String> {
    if crate::instance::is_cancelled(cancel_name) {
        Err("用户取消安装".to_string())
    } else {
        Ok(())
    }
}

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
    let version = mc_version.trim().to_ascii_lowercase();
    if let Some((year, week)) = parse_weekly_snapshot(&version) {
        return match (year, week) {
            (26.., _) | (25, 41..) => 25,
            (24.., _) if (year, week) >= (24, 14) => 21,
            (21.., _) if (year, week) >= (21, 19) => 17,
            (14.., _) => 8,
            _ => 7,
        };
    }

    let parts: Vec<&str> = version.split('.').collect();
    let major = parts
        .first()
        .and_then(|part| leading_number(part))
        .unwrap_or(1);
    let minor = parts
        .get(1)
        .and_then(|part| leading_number(part))
        .unwrap_or(0);
    let patch = parts
        .get(2)
        .and_then(|part| leading_number(part))
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
    if minor < 7 || (minor == 7 && patch < 10) {
        return 7;
    }
    8
}

fn leading_number(value: &str) -> Option<u32> {
    let digits = value
        .chars()
        .take_while(|character| character.is_ascii_digit())
        .collect::<String>();
    (!digits.is_empty()).then(|| digits.parse().ok()).flatten()
}

fn parse_weekly_snapshot(version: &str) -> Option<(u32, u32)> {
    let (year, remainder) = version.split_once('w')?;
    let week = leading_number(remainder)?;
    Some((year.parse().ok()?, week))
}

fn mrpack_file_supports_client(file: &serde_json::Value, index: usize) -> Result<bool, String> {
    let Some(client) = file.get("env").and_then(|env| env.get("client")) else {
        return Ok(true);
    };
    let value = client
        .as_str()
        .ok_or_else(|| format!("Modrinth 清单第 {} 项 env.client 格式错误", index + 1))?;
    match value {
        "required" | "optional" => Ok(true),
        "unsupported" => Ok(false),
        _ => Err(format!(
            "Modrinth 清单第 {} 项包含未知的 env.client 值: {}",
            index + 1,
            value
        )),
    }
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
fn detect_modpack(zip_path: &std::path::Path, cancel_name: &str) -> Result<ModpackMeta, String> {
    let file = std::fs::File::open(zip_path).map_err(|e| format!("打开文件失败: {}", e))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("解析zip失败: {}", e))?;
    validate_modpack_archive_with_check(&mut archive, || {
        ensure_detection_not_cancelled(cancel_name)
    })?;

    // 尝试 Modrinth (.mrpack 含 modrinth.index.json)
    if let Ok(mut entry) = archive.by_name("modrinth.index.json") {
        if entry.size() > MAX_MODPACK_MANIFEST_SIZE {
            return Err(format!(
                "Modrinth 清单超过限制: {} > {} bytes",
                entry.size(),
                MAX_MODPACK_MANIFEST_SIZE
            ));
        }
        let mut content = String::with_capacity(entry.size() as usize);
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
        let manifest_files = json["files"]
            .as_array()
            .ok_or_else(|| "Modrinth 清单缺少 files 数组".to_string())?;
        let mut files = Vec::with_capacity(manifest_files.len());
        for (index, manifest_file) in manifest_files.iter().enumerate() {
            ensure_detection_not_cancelled(cancel_name)?;
            // 启动器只安装客户端，明确标记为服务端专用的文件不能落入实例。
            if !mrpack_file_supports_client(manifest_file, index)? {
                continue;
            }
            let path = manifest_file["path"]
                .as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| format!("Modrinth 清单第 {} 项缺少有效 path", index + 1))?
                .to_string();
            let download_values = manifest_file["downloads"]
                .as_array()
                .ok_or_else(|| format!("Modrinth 清单第 {} 项缺少 downloads 数组", index + 1))?;
            let mut urls = Vec::with_capacity(download_values.len());
            for value in download_values {
                let url = value
                    .as_str()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| format!("Modrinth 清单第 {} 项包含无效下载地址", index + 1))?;
                urls.push(url.to_string());
            }
            if urls.is_empty() {
                return Err(format!("Modrinth 清单第 {} 项没有可用下载地址", index + 1));
            }
            let sha1 = match &manifest_file["hashes"]["sha1"] {
                serde_json::Value::Null => None,
                value => {
                    let hash = value
                        .as_str()
                        .ok_or_else(|| format!("Modrinth 清单第 {} 项 sha1 格式错误", index + 1))?;
                    Some(hash.to_string())
                }
            };
            files.push(MrpackFile { path, urls, sha1 });
        }
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
    ensure_detection_not_cancelled(cancel_name)?;
    let file = std::fs::File::open(zip_path).map_err(|e| e.to_string())?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
    validate_modpack_archive_with_check(&mut archive, || {
        ensure_detection_not_cancelled(cancel_name)
    })?;
    if let Ok(mut entry) = archive.by_name("manifest.json") {
        if entry.size() > MAX_MODPACK_MANIFEST_SIZE {
            return Err(format!(
                "CurseForge 清单超过限制: {} > {} bytes",
                entry.size(),
                MAX_MODPACK_MANIFEST_SIZE
            ));
        }
        let mut content = String::with_capacity(entry.size() as usize);
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
                    return Err(format!("不支持的 CurseForge Loader: {}", id));
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
        let manifest_files = json["files"]
            .as_array()
            .ok_or_else(|| "CurseForge 清单缺少 files 数组".to_string())?;
        for (index, manifest_file) in manifest_files.iter().enumerate() {
            ensure_detection_not_cancelled(cancel_name)?;
            let project_id = manifest_file["projectID"]
                .as_u64()
                .and_then(|value| u32::try_from(value).ok())
                .filter(|value| *value > 0)
                .ok_or_else(|| format!("CurseForge 清单第 {} 项缺少有效 projectID", index + 1))?;
            let file_id = manifest_file["fileID"]
                .as_u64()
                .and_then(|value| u32::try_from(value).ok())
                .filter(|value| *value > 0)
                .ok_or_else(|| format!("CurseForge 清单第 {} 项缺少有效 fileID", index + 1))?;
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
    pub urls: Vec<String>,
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
    let cancel_name = crate::instance::scoped_task_key(&game_dir, &progress_name);
    let cancel_flag = match try_register_cancel(&cancel_name) {
        Ok(flag) => flag,
        Err(error) => {
            let _ = app2.emit(
                "install-progress",
                serde_json::json!({
                    "name": &progress_name, "stage": "error", "current": 0, "total": 0, "detail": &error
                }),
            );
            return Err(error);
        }
    };
    std::thread::spawn(move || {
        let result = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            do_import_modpack_named_with_cancel(
                &app_handle,
                &zip_path,
                &game_dir,
                &java_path,
                use_mirror,
                Some(&progress_name),
                &cancel_name,
            )
        })) {
            Ok(result) => result,
            Err(payload) => Err(format!(
                "导入线程崩溃: {}",
                panic_payload_to_string(payload)
            )),
        };
        unregister_cancel(&cancel_name);
        match &result {
            Ok(message) => {
                let _ = app2.emit(
                    "install-progress",
                    serde_json::json!({
                        "name": &progress_name, "stage": "done", "current": 1, "total": 1, "detail": message
                    }),
                );
            }
            Err(e) => {
                let stage = install_failure_stage(
                    cancel_flag.load(std::sync::atomic::Ordering::Relaxed),
                    e,
                );
                let _ = app2.emit(
                    "install-progress",
                    serde_json::json!({
                        "name": &progress_name, "stage": stage, "current": 0, "total": 0, "detail": e
                    }),
                );
            }
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

fn install_failure_stage(cancel_flag_set: bool, error: &str) -> &'static str {
    let lower = error.to_ascii_lowercase();
    if cancel_flag_set
        && (error.contains("用户取消")
            || lower.contains("download cancelled")
            || lower.trim() == "cancelled")
    {
        "cancelled"
    } else {
        "error"
    }
}

fn cleanup_install_dir(inst_dir: &std::path::Path) -> Result<(), String> {
    if !inst_dir.exists() {
        return Ok(());
    }
    std::fs::remove_dir_all(inst_dir).map_err(|e| format!("清理安装目录失败: {}", e))
}

fn remove_install_marker(marker: &std::path::Path) -> Result<(), String> {
    let mut last_error = String::new();
    for _ in 0..3 {
        match std::fs::remove_file(marker) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => {
                last_error = e.to_string();
                std::thread::sleep(std::time::Duration::from_millis(120));
            }
        }
    }

    // 删除失败时，至少把“安装中”标记改成“已完成”标记。
    let done_marker = marker.with_file_name(".oaoi_install_complete");
    std::fs::rename(marker, &done_marker).map_err(|e| {
        format!(
            "移除安装标记失败: {}; 重命名完成标记也失败: {}",
            last_error, e
        )
    })
}

fn create_unique_instance_dir(
    game_dir: &std::path::Path,
    base_name: &str,
) -> Result<(String, std::path::PathBuf), String> {
    let versions_dir = minecraft_versions_dir(game_dir);
    std::fs::create_dir_all(&versions_dir).map_err(|e| e.to_string())?;

    for index in 0..1000 {
        let name = if index == 0 {
            base_name.to_string()
        } else {
            format!("{}-{}", base_name, index)
        };
        let inst_dir = versions_dir.join(&name);
        match std::fs::create_dir(&inst_dir) {
            Ok(()) => return Ok((name, inst_dir)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("创建版本目录失败: {}", e)),
        }
    }

    Err(format!("无法为 '{}' 找到可用版本名", base_name))
}

pub fn do_import_modpack_named_with_cancel(
    app: &tauri::AppHandle,
    zip_path: &str,
    game_dir_input: &str,
    java_path: &str,
    use_mirror: bool,
    display_name: Option<&str>,
    cancel_name: &str,
) -> Result<String, String> {
    let zip_file = std::path::Path::new(zip_path);
    let temp_name = display_name.unwrap_or("整合包");
    emit_progress(app, temp_name, "detecting", 0, 1, "正在识别整合包格式...");

    let meta = detect_modpack(zip_file, cancel_name)?;
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
        let cleanup_result = cleanup_install_dir(&inst_dir);
        return Err(match cleanup_result {
            Ok(()) => format!("创建安装标记失败: {}", e),
            Err(cleanup_error) => format!("创建安装标记失败: {}; {}", e, cleanup_error),
        });
    }

    // 包装安装，失败时自动清理目录
    let result = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        install::do_install_modpack_inner(
            app,
            zip_file,
            game_dir_input,
            java_path,
            use_mirror,
            &meta,
            &inst_dir,
            &game_dir,
            name,
            cancel_name,
        )
    })) {
        Ok(result) => result,
        Err(payload) => Err(format!(
            "安装线程崩溃: {}",
            panic_payload_to_string(payload)
        )),
    };
    match result {
        Err(e) => {
            let mut final_error = e;
            if install_marker_path.exists() {
                match cleanup_install_dir(&inst_dir) {
                    Ok(()) => eprintln!("[modpack] 安装失败，已清理: {}", inst_dir.display()),
                    Err(cleanup_error) => {
                        eprintln!(
                            "[modpack] 安装失败，清理失败: {} ({})",
                            inst_dir.display(),
                            cleanup_error
                        );
                        final_error = format!("{}; {}", final_error, cleanup_error);
                    }
                }
            } else if inst_dir.exists() {
                eprintln!("[modpack] 跳过清理非本次安装目录: {}", inst_dir.display());
            }
            Err(final_error)
        }
        Ok(message) => {
            if let Err(marker_error) = remove_install_marker(&install_marker_path) {
                eprintln!("[modpack] 安装成功，安装标记稍后清理: {}", marker_error);
            }
            Ok(message)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        get_required_java_major_pub, mrpack_file_supports_client,
        validate_modpack_archive_with_check,
    };
    use std::io::{Cursor, Write};

    #[test]
    fn java_version_parser_handles_suffixes_and_snapshots() {
        assert_eq!(get_required_java_major_pub("1.20.5-pre1"), 21);
        assert_eq!(get_required_java_major_pub("1.20.4"), 17);
        assert_eq!(get_required_java_major_pub("24w14a"), 21);
        assert_eq!(get_required_java_major_pub("21w19a"), 17);
        assert_eq!(get_required_java_major_pub("25w41a"), 25);
        assert_eq!(get_required_java_major_pub("1.7.10"), 8);
        assert_eq!(get_required_java_major_pub("1.7.9"), 7);
    }

    #[test]
    fn mrpack_client_environment_filters_only_unsupported_files() {
        let required = serde_json::json!({ "env": { "client": "required" } });
        let optional = serde_json::json!({ "env": { "client": "optional" } });
        let unsupported = serde_json::json!({ "env": { "client": "unsupported" } });
        let missing = serde_json::json!({});

        assert_eq!(mrpack_file_supports_client(&required, 0), Ok(true));
        assert_eq!(mrpack_file_supports_client(&optional, 0), Ok(true));
        assert_eq!(mrpack_file_supports_client(&unsupported, 0), Ok(false));
        assert_eq!(mrpack_file_supports_client(&missing, 0), Ok(true));
    }

    #[test]
    fn archive_validation_stops_when_cancelled() {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        writer.start_file("first.txt", options).unwrap();
        writer.write_all(b"first").unwrap();
        writer.start_file("second.txt", options).unwrap();
        writer.write_all(b"second").unwrap();
        let data = writer.finish().unwrap().into_inner();
        let mut archive = zip::ZipArchive::new(Cursor::new(data)).unwrap();
        let mut checks = 0;

        let error = validate_modpack_archive_with_check(&mut archive, || {
            checks += 1;
            if checks >= 2 {
                Err("cancelled".to_string())
            } else {
                Ok(())
            }
        })
        .unwrap_err();

        assert_eq!(error, "cancelled");
        assert_eq!(checks, 2);
    }
}
