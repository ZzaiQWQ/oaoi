use crate::instance::{resolve_game_dir, safe_path_name, version_dir};
use crate::modcn::{contains_chinese, load_modcn};
use serde_json::Value as JsonValue;
use serde::Serialize;
use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tauri::Emitter;
use zip::ZipArchive;

const MAX_MOD_ICON_BYTES: u64 = 256 * 1024;

#[derive(Serialize, Clone)]
pub struct ModInfo {
    pub file_name: String,
    pub cn_name: String,
    pub enabled: bool,
    pub size_kb: u64,
    pub icon_url: String,
}

#[derive(Serialize, Clone)]
struct ModIconPatch {
    file_name: String,
    icon_url: String,
}

#[derive(Serialize, Clone)]
struct ModListStreamEvent {
    request_id: String,
    name: String,
    status: String,
    mods: Vec<ModInfo>,
    icons: Vec<ModIconPatch>,
    message: String,
}

/// 列出实例的所有 mod（.jar 和 .jar.disabled）
#[tauri::command]
pub async fn list_mods(game_dir: String, name: String) -> Result<Vec<ModInfo>, String> {
    tokio::task::spawn_blocking(move || list_mods_blocking(&game_dir, &name))
        .await
        .map_err(|e| format!("任务失败: {}", e))?
}

/// 流式列出 mod：先显示文件列表，再补 jar 内图标，避免页面空等。
#[tauri::command]
pub async fn stream_mods(
    app_handle: tauri::AppHandle,
    game_dir: String,
    name: String,
    request_id: String,
) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        stream_mods_blocking(&app_handle, &game_dir, &name, &request_id)
    })
    .await
    .map_err(|e| format!("任务失败: {}", e))?
}

fn list_mods_blocking(game_dir: &str, name: &str) -> Result<Vec<ModInfo>, String> {
    let (_, mods) = collect_mod_infos(game_dir, name)?;
    Ok(mods)
}

fn stream_mods_blocking(
    app_handle: &tauri::AppHandle,
    game_dir: &str,
    name: &str,
    request_id: &str,
) -> Result<(), String> {
    let (mods_dir, mods) = collect_mod_infos(game_dir, name)?;
    for chunk in mods.chunks(80) {
        emit_mod_list_stream(
            app_handle,
            request_id,
            name,
            "batch",
            chunk.to_vec(),
            Vec::new(),
            "",
        );
    }

    let mut icon_batch = Vec::new();
    for item in &mods {
        let path = mods_dir.join(&item.file_name);
        if let Some(icon_url) = read_mod_icon_url(&path) {
            icon_batch.push(ModIconPatch {
                file_name: item.file_name.clone(),
                icon_url,
            });
        }
        if icon_batch.len() >= 24 {
            emit_mod_list_stream(
                app_handle,
                request_id,
                name,
                "icon",
                Vec::new(),
                std::mem::take(&mut icon_batch),
                "",
            );
        }
    }
    if !icon_batch.is_empty() {
        emit_mod_list_stream(
            app_handle,
            request_id,
            name,
            "icon",
            Vec::new(),
            icon_batch,
            "",
        );
    }
    emit_mod_list_stream(
        app_handle,
        request_id,
        name,
        "done",
        Vec::new(),
        Vec::new(),
        "",
    );
    Ok(())
}

fn emit_mod_list_stream(
    app_handle: &tauri::AppHandle,
    request_id: &str,
    name: &str,
    status: &str,
    mods: Vec<ModInfo>,
    icons: Vec<ModIconPatch>,
    message: &str,
) {
    let _ = app_handle.emit(
        "mod-list-stream",
        ModListStreamEvent {
            request_id: request_id.to_string(),
            name: name.to_string(),
            status: status.to_string(),
            mods,
            icons,
            message: message.to_string(),
        },
    );
}

fn collect_mod_infos(game_dir: &str, name: &str) -> Result<(PathBuf, Vec<ModInfo>), String> {
    let dir = resolve_game_dir(&game_dir);
    let safe_name = safe_path_name(&name, "版本名")?;
    let mods_dir = version_dir(&dir, &safe_name).join("mods");
    if !mods_dir.exists() {
        return Ok((mods_dir, vec![]));
    }

    // 加载 modcn 索引用于匹配中文名
    let modcn_index = modcn_index();

    let mut mods = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&mods_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let fname = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let lower = fname.to_lowercase();
            if lower.ends_with(".jar") || lower.ends_with(".jar.disabled") {
                let enabled = !lower.ends_with(".disabled");
                let size_kb = entry.metadata().map(|m| m.len() / 1024).unwrap_or(0);

                // 从文件名提取模组名用于匹配中文
                let base = lower.trim_end_matches(".disabled").trim_end_matches(".jar");
                // 去掉版本号和mc版本部分
                let mod_key = base
                    .split(|c: char| c.is_ascii_digit())
                    .next()
                    .unwrap_or(base)
                    .trim_end_matches('-')
                    .trim_end_matches('_')
                    .trim_end_matches('.');
                // 也按 - 分割取第一段作为核心名
                let first_seg = base
                    .split('-')
                    .next()
                    .unwrap_or(base)
                    .split('_')
                    .next()
                    .unwrap_or(base);

                let cn_name = find_cn_name(&modcn_index, mod_key, first_seg, base);

                mods.push(ModInfo {
                    file_name: fname,
                    cn_name,
                    enabled,
                    size_kb,
                    icon_url: String::new(),
                });
            }
        }
    }
    mods.sort_by(|a, b| a.file_name.to_lowercase().cmp(&b.file_name.to_lowercase()));
    Ok((mods_dir, mods))
}

fn read_mod_icon_url(path: &Path) -> Option<String> {
    let file = File::open(path).ok()?;
    let mut archive = ZipArchive::new(file).ok()?;
    let icon_path = find_mod_icon_path(&mut archive)?;
    let icon_path = normalize_icon_path(&icon_path)?;
    let mime = icon_mime_type(&icon_path)?;
    let mut icon_file = archive.by_name(&icon_path).ok()?;
    if icon_file.size() > MAX_MOD_ICON_BYTES {
        return None;
    }
    let mut bytes = Vec::with_capacity(icon_file.size() as usize);
    icon_file.read_to_end(&mut bytes).ok()?;
    if bytes.is_empty() {
        return None;
    }
    Some(format!("data:{};base64,{}", mime, base64_encode(&bytes)))
}

fn find_mod_icon_path(archive: &mut ZipArchive<File>) -> Option<String> {
    if let Some(text) = read_zip_text(archive, "fabric.mod.json") {
        if let Ok(value) = serde_json::from_str::<JsonValue>(&text) {
            if let Some(path) = extract_icon_value(value.get("icon")) {
                return Some(path);
            }
        }
    }
    if let Some(text) = read_zip_text(archive, "quilt.mod.json") {
        if let Ok(value) = serde_json::from_str::<JsonValue>(&text) {
            if let Some(path) = extract_icon_value(value.pointer("/quilt_loader/metadata/icon"))
                .or_else(|| extract_icon_value(value.pointer("/metadata/icon")))
            {
                return Some(path);
            }
        }
    }
    if let Some(text) = read_zip_text(archive, "META-INF/mods.toml") {
        if let Ok(value) = toml::from_str::<toml::Value>(&text) {
            if let Some(path) = extract_forge_icon(&value) {
                return Some(path);
            }
        }
    }
    if let Some(text) = read_zip_text(archive, "mcmod.info") {
        if let Ok(value) = serde_json::from_str::<JsonValue>(&text) {
            if let Some(path) = extract_mcmod_icon(&value) {
                return Some(path);
            }
        }
    }
    None
}

fn read_zip_text(archive: &mut ZipArchive<File>, name: &str) -> Option<String> {
    let mut file = archive.by_name(name).ok()?;
    if file.size() > 256 * 1024 {
        return None;
    }
    let mut text = String::new();
    file.read_to_string(&mut text).ok()?;
    Some(text)
}

fn extract_icon_value(value: Option<&JsonValue>) -> Option<String> {
    match value? {
        JsonValue::String(path) => non_empty_string(path),
        JsonValue::Object(map) => {
            let mut sized = map
                .iter()
                .filter_map(|(key, value)| {
                    let size = key.parse::<u32>().ok()?;
                    let path = value.as_str()?;
                    Some((size, path.to_string()))
                })
                .collect::<Vec<_>>();
            sized.sort_by_key(|(size, _)| *size);
            sized
                .pop()
                .map(|(_, path)| path)
                .or_else(|| map.values().find_map(|value| value.as_str().map(str::to_string)))
        }
        _ => None,
    }
}

fn extract_forge_icon(value: &toml::Value) -> Option<String> {
    if let Some(path) = value.get("logoFile").and_then(|value| value.as_str()) {
        return non_empty_string(path);
    }
    value
        .get("mods")
        .and_then(|value| value.as_array())
        .and_then(|mods| {
            mods.iter().find_map(|item| {
                item.get("logoFile")
                    .and_then(|value| value.as_str())
                    .and_then(non_empty_string)
            })
        })
}

fn extract_mcmod_icon(value: &JsonValue) -> Option<String> {
    if let Some(items) = value.as_array() {
        return items.iter().find_map(extract_mcmod_icon_from_object);
    }
    if let Some(path) = extract_mcmod_icon_from_object(value) {
        return Some(path);
    }
    value
        .get("modList")
        .and_then(|value| value.as_array())
        .and_then(|items| items.iter().find_map(extract_mcmod_icon_from_object))
}

fn extract_mcmod_icon_from_object(value: &JsonValue) -> Option<String> {
    value
        .get("logoFile")
        .or_else(|| value.get("logo"))
        .and_then(|value| value.as_str())
        .and_then(non_empty_string)
}

fn non_empty_string(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn normalize_icon_path(path: &str) -> Option<String> {
    let normalized = path.trim().trim_start_matches('/').replace('\\', "/");
    if normalized.is_empty()
        || normalized.contains("..")
        || normalized.starts_with('/')
        || normalized.starts_with('\\')
    {
        None
    } else {
        Some(normalized)
    }
}

fn icon_mime_type(path: &str) -> Option<&'static str> {
    let lower = path.to_lowercase();
    if lower.ends_with(".png") {
        Some("image/png")
    } else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        Some("image/jpeg")
    } else if lower.ends_with(".gif") {
        Some("image/gif")
    } else if lower.ends_with(".webp") {
        Some("image/webp")
    } else if lower.ends_with(".svg") {
        Some("image/svg+xml")
    } else {
        None
    }
}

fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    let mut index = 0;
    while index < bytes.len() {
        let b0 = bytes[index];
        let b1 = bytes.get(index + 1).copied().unwrap_or(0);
        let b2 = bytes.get(index + 2).copied().unwrap_or(0);
        out.push(TABLE[(b0 >> 2) as usize] as char);
        out.push(TABLE[(((b0 & 0b0000_0011) << 4) | (b1 >> 4)) as usize] as char);
        if index + 1 < bytes.len() {
            out.push(TABLE[(((b1 & 0b0000_1111) << 2) | (b2 >> 6)) as usize] as char);
        } else {
            out.push('=');
        }
        if index + 2 < bytes.len() {
            out.push(TABLE[(b2 & 0b0011_1111) as usize] as char);
        } else {
            out.push('=');
        }
        index += 3;
    }
    out
}

struct ModCnIndex {
    exact: HashMap<String, String>,
    slugs: Vec<(String, String)>,
}

fn modcn_index() -> &'static ModCnIndex {
    static MODCN_INDEX: OnceLock<ModCnIndex> = OnceLock::new();
    MODCN_INDEX.get_or_init(|| build_modcn_index(load_modcn()))
}

fn build_modcn_index(entries: &[crate::modcn::ModCnEntry]) -> ModCnIndex {
    let mut exact = HashMap::new();
    let mut slugs = Vec::new();
    for entry in entries {
        if entry.cn_name.is_empty() || !contains_chinese(&entry.cn_name) {
            continue;
        }
        let en_lower = entry.en_name.to_lowercase();
        if !en_lower.is_empty() {
            let en_slug = en_lower.replace(' ', "-").replace('_', "-");
            if en_slug.len() >= 2 {
                exact
                    .entry(en_slug.clone())
                    .or_insert_with(|| entry.cn_name.clone());
                slugs.push((en_slug, entry.cn_name.clone()));
            }
        }
        let abbr_lower = entry.abbr.to_lowercase();
        if abbr_lower.len() >= 2 {
            exact
                .entry(abbr_lower)
                .or_insert_with(|| entry.cn_name.clone());
        }
    }
    ModCnIndex { exact, slugs }
}

fn find_cn_name(index: &ModCnIndex, mod_key: &str, first_seg: &str, base: &str) -> String {
    if mod_key.len() >= 2 {
        if let Some(name) = index.exact.get(mod_key) {
            return name.clone();
        }
    }
    if first_seg.len() >= 2 {
        if let Some(name) = index.exact.get(first_seg) {
            return name.clone();
        }
    }
    if mod_key.len() >= 3 {
        if let Some((_, name)) = index
            .slugs
            .iter()
            .find(|(slug, _)| base.contains(slug) || slug.contains(mod_key))
        {
            return name.clone();
        }
    }
    String::new()
}

/// 切换 mod 启用/禁用（.jar ↔ .jar.disabled）
#[tauri::command]
pub fn toggle_mod(game_dir: String, name: String, file_name: String) -> Result<bool, String> {
    let dir = resolve_game_dir(&game_dir);
    let safe_name = safe_path_name(&name, "版本名")?;
    let safe_file_name = safe_path_name(&file_name, "文件名")?;
    let mods_dir = version_dir(&dir, &safe_name).join("mods");
    let src = mods_dir.join(&safe_file_name);
    if !src.exists() {
        return Err(format!("文件不存在: {}", file_name));
    }
    let lower = safe_file_name.to_lowercase();
    let (dst_name, new_enabled) = if lower.ends_with(".jar.disabled") {
        // 启用：去掉 .disabled
        (
            safe_file_name.trim_end_matches(".disabled").to_string(),
            true,
        )
    } else if lower.ends_with(".jar") {
        // 禁用：加 .disabled
        (format!("{}.disabled", safe_file_name), false)
    } else {
        return Err("不支持的文件类型".to_string());
    };
    let dst = mods_dir.join(&dst_name);
    std::fs::rename(&src, &dst).map_err(|e| format!("重命名失败: {}", e))?;
    Ok(new_enabled)
}

/// 删除指定 mod 文件
#[tauri::command]
pub fn delete_mod(game_dir: String, name: String, file_name: String) -> Result<bool, String> {
    let dir = resolve_game_dir(&game_dir);
    let safe_name = safe_path_name(&name, "版本名")?;
    let safe_file_name = safe_path_name(&file_name, "文件名")?;
    let mods_dir = version_dir(&dir, &safe_name).join("mods");
    let target = mods_dir.join(&safe_file_name);
    if !target.exists() {
        return Err(format!("文件不存在: {}", file_name));
    }
    std::fs::remove_file(&target).map_err(|e| format!("删除失败: {}", e))?;
    Ok(true)
}
