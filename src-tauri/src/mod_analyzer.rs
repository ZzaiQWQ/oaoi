use crate::instance::{resolve_game_dir, safe_path_name, version_dir};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::{Cursor, Read, Seek};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

const META_CACHE_DIR: &str = "launcher-data";
const META_CACHE_SUBDIR: &str = "mod-meta-cache";
const META_CACHE_VERSION: u32 = 10;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ModMetadata {
    file: String,
    mod_id: String,
    name: String,
    version: String,
    loader: String,
    #[serde(default)]
    provides: Vec<String>,
    dependencies: Vec<DependencyRequirement>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct DependencyRequirement {
    mod_id: String,
    version_req: String,
    required: bool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModMetaCache {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    files: HashMap<String, CachedModFile>,
}

impl Default for ModMetaCache {
    fn default() -> Self {
        Self {
            version: META_CACHE_VERSION,
            files: HashMap::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CachedModFile {
    path: String,
    size: u64,
    modified_time: u64,
    mods: Vec<ModMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parse_error: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalyzeResult {
    scanned_files: usize,
    parsed_mods: usize,
    issue_count: usize,
    duplicates: Vec<DuplicateIssue>,
    missing_dependencies: Vec<MissingDependencyIssue>,
    loader_mismatches: Vec<LoaderMismatchIssue>,
    warnings: Vec<AnalyzeWarning>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DuplicateIssue {
    mod_id: String,
    name: String,
    files: Vec<DuplicateFile>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DuplicateFile {
    file: String,
    version: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MissingDependencyIssue {
    file: String,
    mod_id: String,
    mod_name: String,
    dependency_id: String,
    version_req: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoaderMismatchIssue {
    file: String,
    mod_id: String,
    mod_name: String,
    mod_loader: String,
    instance_loader: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalyzeWarning {
    file: String,
    message: String,
}

fn meta_cache_lock() -> &'static Mutex<()> {
    static INSTANCE: OnceLock<Mutex<()>> = OnceLock::new();
    INSTANCE.get_or_init(|| Mutex::new(()))
}

/// 检测当前实例的 mods 文件夹，返回重复 Mod、缺前置、Loader 不匹配等结果。
#[tauri::command]
pub async fn analyze_instance_mods(
    game_dir: String,
    name: String,
    mc_version: String,
    loader: String,
) -> Result<AnalyzeResult, String> {
    tokio::task::spawn_blocking(move || {
        analyze_instance_mods_blocking(&game_dir, &name, &mc_version, &loader)
    })
    .await
    .map_err(|e| format!("检测任务失败: {}", e))?
}

/// 下载完成后后台解析刚下载的 jar，失败只写日志，不影响下载成功。
pub fn spawn_cache_downloaded_mods(
    game_dir: String,
    name: String,
    loader: String,
    rel_paths: Vec<String>,
) {
    if rel_paths.is_empty() {
        return;
    }
    std::thread::spawn(move || {
        let mut seen = HashSet::new();
        for rel_path in rel_paths {
            let normalized = normalize_rel_path(&rel_path);
            if !seen.insert(normalized.clone()) {
                continue;
            }
            if let Err(err) = cache_single_downloaded_mod(&game_dir, &name, &loader, &normalized) {
                eprintln!("[mod_analyzer] 后台解析失败 {}: {}", normalized, err);
            }
        }
    });
}

fn analyze_instance_mods_blocking(
    game_dir: &str,
    name: &str,
    _mc_version: &str,
    loader: &str,
) -> Result<AnalyzeResult, String> {
    let safe_name = safe_path_name(name, "版本名")?;
    let game_root = resolve_game_dir(game_dir);
    let mods_dir = version_dir(&game_root, &safe_name).join("mods");
    if !mods_dir.exists() {
        return Ok(empty_result());
    }
    if !mods_dir.is_dir() {
        return Err(format!("Mod 目录不是文件夹: {}", mods_dir.display()));
    }

    let _guard = meta_cache_lock()
        .lock()
        .map_err(|_| "Mod 元数据缓存锁异常".to_string())?;
    let mut cache = load_meta_cache(&game_root, &safe_name);
    let mut cache_changed = false;
    let mut scanned_files = 0usize;
    let mut mods = Vec::new();
    let mut warnings = Vec::new();
    let mut current_paths = HashSet::new();
    let mut jars = list_jar_files(&mods_dir)?;
    jars.sort();

    for path in jars {
        scanned_files += 1;
        let file_name = path
            .file_name()
            .map(|value| value.to_string_lossy().to_string())
            .unwrap_or_else(|| path.display().to_string());
        let rel_path = format!("mods/{}", file_name);
        current_paths.insert(rel_path.clone());

        match read_mod_metadata_with_cache(&path, &rel_path, loader, &mut cache, &mut cache_changed)
        {
            Ok(mut parsed) if !parsed.is_empty() => mods.append(&mut parsed),
            Ok(_) if should_ignore_unidentified_file(&path, loader) => {}
            Ok(_) => warnings.push(AnalyzeWarning {
                file: file_name,
                message: "没有识别到 Mod 元数据".to_string(),
            }),
            Err(message) => warnings.push(AnalyzeWarning {
                file: file_name,
                message,
            }),
        }
    }

    // 清理已经被删除的 jar 缓存，避免检测结果吃到旧文件。
    let before_len = cache.files.len();
    cache.files.retain(|rel, _| current_paths.contains(rel));
    if cache.files.len() != before_len {
        cache_changed = true;
    }
    if cache_changed {
        save_meta_cache(&game_root, &safe_name, &cache)?;
    }

    let duplicates = detect_duplicates(&mods);
    let missing_dependencies = detect_missing_dependencies(&mods);
    let loader_mismatches = detect_loader_mismatches(&mods, loader);
    let issue_count =
        duplicates.len() + missing_dependencies.len() + loader_mismatches.len() + warnings.len();

    Ok(AnalyzeResult {
        scanned_files,
        parsed_mods: mods.len(),
        issue_count,
        duplicates,
        missing_dependencies,
        loader_mismatches,
        warnings,
    })
}

fn empty_result() -> AnalyzeResult {
    AnalyzeResult {
        scanned_files: 0,
        parsed_mods: 0,
        issue_count: 0,
        duplicates: Vec::new(),
        missing_dependencies: Vec::new(),
        loader_mismatches: Vec::new(),
        warnings: Vec::new(),
    }
}

fn cache_single_downloaded_mod(
    game_dir: &str,
    name: &str,
    loader: &str,
    rel_path: &str,
) -> Result<(), String> {
    let safe_name = safe_path_name(name, "版本名")?;
    let rel_path = normalize_rel_path(rel_path);
    if !rel_path.starts_with("mods/") || !rel_path.to_ascii_lowercase().ends_with(".jar") {
        return Ok(());
    }

    let game_root = resolve_game_dir(game_dir);
    let file_path = version_dir(&game_root, &safe_name).join(&rel_path);
    if !file_path.is_file() {
        return Ok(());
    }

    let _guard = meta_cache_lock()
        .lock()
        .map_err(|_| "Mod 元数据缓存锁异常".to_string())?;
    let mut cache = load_meta_cache(&game_root, &safe_name);
    let mut cache_changed = false;
    let _ = read_mod_metadata_with_cache(
        &file_path,
        &rel_path,
        loader,
        &mut cache,
        &mut cache_changed,
    );
    if cache_changed {
        save_meta_cache(&game_root, &safe_name, &cache)?;
    }
    Ok(())
}

fn list_jar_files(mods_dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(mods_dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(file_name) = path
            .file_name()
            .map(|value| value.to_string_lossy().to_lowercase())
        else {
            continue;
        };
        if file_name.ends_with(".jar") {
            out.push(path);
        }
    }
    Ok(out)
}

fn should_ignore_unidentified_file(path: &Path, loader: &str) -> bool {
    let loader = normalize_loader(loader);
    if !matches!(loader.as_str(), "forge" | "neoforge") {
        return false;
    }

    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    let Ok(mut archive) = zip::ZipArchive::new(file) else {
        return false;
    };
    let has_provider = archive
        .by_name("META-INF/services/net.minecraftforge.forgespi.language.IModLanguageProvider")
        .is_ok();
    has_provider
}

fn read_mod_metadata_with_cache(
    path: &Path,
    rel_path: &str,
    loader: &str,
    cache: &mut ModMetaCache,
    cache_changed: &mut bool,
) -> Result<Vec<ModMetadata>, String> {
    let (size, modified_time) = file_stamp(path)?;
    if let Some(cached) = cache.files.get(rel_path) {
        if cached.size == size && cached.modified_time == modified_time {
            if let Some(err) = &cached.parse_error {
                return Err(err.clone());
            }
            return Ok(cached.mods.clone());
        }
    }

    // 只有新增或变动的 jar 才重新解析，避免几百个 Mod 每次都全量读。
    let parsed = parse_mod_jar(path, rel_path, loader);
    match parsed {
        Ok(mods) => {
            cache.files.insert(
                rel_path.to_string(),
                CachedModFile {
                    path: rel_path.to_string(),
                    size,
                    modified_time,
                    mods: mods.clone(),
                    parse_error: None,
                },
            );
            *cache_changed = true;
            Ok(mods)
        }
        Err(err) => {
            cache.files.insert(
                rel_path.to_string(),
                CachedModFile {
                    path: rel_path.to_string(),
                    size,
                    modified_time,
                    mods: Vec::new(),
                    parse_error: Some(err.clone()),
                },
            );
            *cache_changed = true;
            Err(err)
        }
    }
}

fn file_stamp(path: &Path) -> Result<(u64, u64), String> {
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    let modified_time = meta
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0);
    Ok((meta.len(), modified_time))
}

/// 解析顶层 jar 的 Fabric/Quilt/Forge/NeoForge 元数据；内嵌 jar 不作为独立 Mod 参与检测。
fn parse_mod_jar(path: &Path, rel_path: &str, loader: &str) -> Result<Vec<ModMetadata>, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("打开失败: {}", e))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("读取 Jar 失败: {}", e))?;
    parse_zip_archive(&mut archive, rel_path, loader)
}

fn parse_zip_archive<R: Read + Seek>(
    archive: &mut zip::ZipArchive<R>,
    rel_path: &str,
    loader: &str,
) -> Result<Vec<ModMetadata>, String> {
    let mut out = Vec::new();
    let loader = normalize_loader(loader);

    if should_parse_fabric_like(&loader) {
        parse_fabric_like_metadata(archive, rel_path, &mut out)?;
    }

    if should_parse_neoforge(&loader) {
        let mut parsed_loader_metadata = false;
        if let Some(text) = read_zip_text(archive, "META-INF/neoforge.mods.toml")? {
            out.extend(parse_forge_mods_toml(rel_path, &text, "neoforge")?);
            parsed_loader_metadata = !out.is_empty();
        } else if let Some(text) = read_zip_text(archive, "META-INF/mods.toml")? {
            out.extend(parse_forge_mods_toml(rel_path, &text, "neoforge")?);
            parsed_loader_metadata = !out.is_empty();
        }
        attach_forge_embedded_provides(archive, rel_path, &mut out);
        if !parsed_loader_metadata || has_connector_marker(&out) {
            parse_fabric_like_metadata(archive, rel_path, &mut out)?;
        }
    } else if should_parse_forge(&loader) {
        let mut parsed_loader_metadata = false;
        if let Some(text) = read_zip_text(archive, "META-INF/mods.toml")? {
            out.extend(parse_forge_mods_toml(rel_path, &text, "forge")?);
            parsed_loader_metadata = !out.is_empty();
        }
        attach_forge_embedded_provides(archive, rel_path, &mut out);
        if !parsed_loader_metadata || has_connector_marker(&out) {
            parse_fabric_like_metadata(archive, rel_path, &mut out)?;
        }
    }

    Ok(out)
}

fn normalize_loader(loader: &str) -> String {
    loader.trim().to_ascii_lowercase()
}

fn should_parse_fabric_like(loader: &str) -> bool {
    matches!(loader, "fabric" | "quilt")
}

fn should_parse_forge(loader: &str) -> bool {
    loader == "forge"
}

fn should_parse_neoforge(loader: &str) -> bool {
    loader == "neoforge"
}

fn parse_fabric_like_metadata<R: Read + Seek>(
    archive: &mut zip::ZipArchive<R>,
    rel_path: &str,
    out: &mut Vec<ModMetadata>,
) -> Result<(), String> {
    if let Some(text) = read_zip_text(archive, "fabric.mod.json")? {
        let json = parse_json_metadata(&text, "fabric.mod.json")?;
        let mut parsed = parse_fabric_value(rel_path, &json);
        // 内嵌 jar 不作为独立 Mod 检测，只记录它们提供的 modId，用来避免缺前置误报。
        let embedded_ids = collect_embedded_provided_mod_ids(archive, &json);
        if let Some(first) = parsed.first_mut() {
            append_unique_ids(&mut first.provides, embedded_ids);
        }
        out.extend(parsed);
    }
    if let Some(text) = read_zip_text(archive, "quilt.mod.json")? {
        let json = parse_json_metadata(&text, "quilt.mod.json")?;
        let mut parsed = parse_quilt_value(rel_path, &json);
        let embedded_ids = collect_embedded_provided_mod_ids(archive, &json);
        if let Some(first) = parsed.first_mut() {
            append_unique_ids(&mut first.provides, embedded_ids);
        }
        out.extend(parsed);
    }
    Ok(())
}

fn has_connector_marker(mods: &[ModMetadata]) -> bool {
    mods.iter().any(|item| {
        item.provides
            .iter()
            .any(|id| normalize_mod_id(id) == "connectormod")
    })
}

fn read_zip_text<R: Read + Seek>(
    archive: &mut zip::ZipArchive<R>,
    name: &str,
) -> Result<Option<String>, String> {
    match archive.by_name(name) {
        Ok(mut entry) => {
            let mut text = String::new();
            entry
                .read_to_string(&mut text)
                .map_err(|e| format!("读取 {} 失败: {}", name, e))?;
            Ok(Some(text))
        }
        Err(zip::result::ZipError::FileNotFound) => Ok(None),
        Err(e) => Err(format!("读取 {} 失败: {}", name, e)),
    }
}

fn parse_json_metadata(text: &str, label: &str) -> Result<serde_json::Value, String> {
    match serde_json::from_str(text) {
        Ok(value) => Ok(value),
        Err(first_err) => {
            let cleaned = strip_json_control_chars(text);
            serde_json::from_str(&cleaned)
                .map_err(|_| format!("解析 {} 失败: {}", label, first_err))
        }
    }
}

fn strip_json_control_chars(text: &str) -> String {
    text.chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect()
}

fn read_zip_bytes<R: Read + Seek>(
    archive: &mut zip::ZipArchive<R>,
    name: &str,
) -> Result<Option<Vec<u8>>, String> {
    match archive.by_name(name) {
        Ok(mut entry) => {
            let mut bytes = Vec::new();
            entry
                .read_to_end(&mut bytes)
                .map_err(|e| format!("读取 {} 失败: {}", name, e))?;
            Ok(Some(bytes))
        }
        Err(zip::result::ZipError::FileNotFound) => Ok(None),
        Err(e) => Err(format!("读取 {} 失败: {}", name, e)),
    }
}

fn parse_fabric_value(rel_path: &str, json: &serde_json::Value) -> Vec<ModMetadata> {
    let mod_id = json["id"].as_str().unwrap_or("").trim();
    if mod_id.is_empty() {
        return Vec::new();
    }

    let mut dependencies = Vec::new();
    if let Some(depends) = json["depends"].as_object() {
        for (dep_id, value) in depends {
            dependencies.push(DependencyRequirement {
                mod_id: dep_id.to_string(),
                version_req: json_dependency_value_to_string(value),
                required: true,
            });
        }
    }

    vec![ModMetadata {
        file: rel_path.to_string(),
        mod_id: mod_id.to_string(),
        name: json["name"].as_str().unwrap_or(mod_id).to_string(),
        version: json["version"].as_str().unwrap_or("").to_string(),
        loader: "fabric".to_string(),
        provides: collect_json_provides(json),
        dependencies,
    }]
}

fn parse_quilt_value(rel_path: &str, json: &serde_json::Value) -> Vec<ModMetadata> {
    let loader = &json["quilt_loader"];
    let mod_id = loader["id"].as_str().unwrap_or("").trim();
    if mod_id.is_empty() {
        return Vec::new();
    }

    let mut dependencies = Vec::new();
    collect_quilt_dependencies(&loader["depends"], &mut dependencies);

    vec![ModMetadata {
        file: rel_path.to_string(),
        mod_id: mod_id.to_string(),
        name: loader["metadata"]["name"]
            .as_str()
            .unwrap_or(mod_id)
            .to_string(),
        version: loader["version"].as_str().unwrap_or("").to_string(),
        loader: "quilt".to_string(),
        provides: collect_json_provides(loader),
        dependencies,
    }]
}

fn embedded_jar_paths(json: &serde_json::Value) -> Vec<String> {
    json["jars"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item["file"].as_str().map(|path| path.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

fn collect_embedded_provided_mod_ids<R: Read + Seek>(
    archive: &mut zip::ZipArchive<R>,
    json: &serde_json::Value,
) -> Vec<String> {
    let mut out = Vec::new();
    for nested_path in embedded_jar_paths(json) {
        let Ok(Some(bytes)) = read_zip_bytes(archive, &nested_path) else {
            continue;
        };
        let cursor = Cursor::new(bytes);
        let Ok(mut nested_archive) = zip::ZipArchive::new(cursor) else {
            continue;
        };
        append_unique_ids(&mut out, collect_declared_mod_ids(&mut nested_archive));
    }
    out
}

fn attach_forge_embedded_provides<R: Read + Seek>(
    archive: &mut zip::ZipArchive<R>,
    rel_path: &str,
    mods: &mut Vec<ModMetadata>,
) {
    let mut provided = collect_rel_path_provided_mod_ids(rel_path);
    append_unique_ids(
        &mut provided,
        collect_forge_embedded_provided_mod_ids(archive),
    );
    if provided.is_empty() {
        return;
    }

    if let Some(first) = mods.first_mut() {
        append_unique_ids(&mut first.provides, provided);
        return;
    }

    let mod_id = provided
        .first()
        .cloned()
        .unwrap_or_else(|| format_analyze_mod_file_name_fallback(rel_path));
    mods.push(ModMetadata {
        file: rel_path.to_string(),
        mod_id: mod_id.clone(),
        name: mod_id,
        version: String::new(),
        loader: "forge".to_string(),
        provides: provided,
        dependencies: Vec::new(),
    });
}

fn collect_forge_embedded_provided_mod_ids<R: Read + Seek>(
    archive: &mut zip::ZipArchive<R>,
) -> Vec<String> {
    let mut paths = collect_jarjar_metadata_paths(archive);
    append_unique_ids(&mut paths, collect_archive_embedded_jar_paths(archive));

    let mut out = Vec::new();
    for nested_path in paths {
        let Ok(Some(bytes)) = read_zip_bytes(archive, &nested_path) else {
            continue;
        };
        let cursor = Cursor::new(bytes);
        let Ok(mut nested_archive) = zip::ZipArchive::new(cursor) else {
            continue;
        };
        let before = out.len();
        append_unique_ids(&mut out, collect_declared_mod_ids(&mut nested_archive));
        if out.len() == before {
            append_unique_ids(&mut out, collect_jar_path_provided_mod_ids(&nested_path));
        }
    }
    out
}

fn collect_jarjar_metadata_paths<R: Read + Seek>(archive: &mut zip::ZipArchive<R>) -> Vec<String> {
    let Ok(Some(text)) = read_zip_text(archive, "META-INF/jarjar/metadata.json") else {
        return Vec::new();
    };
    let Ok(json) = parse_json_metadata(&text, "META-INF/jarjar/metadata.json") else {
        return Vec::new();
    };
    json["jars"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item["path"].as_str().map(|path| path.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

fn collect_archive_embedded_jar_paths<R: Read + Seek>(
    archive: &mut zip::ZipArchive<R>,
) -> Vec<String> {
    let mut out = Vec::new();
    for idx in 0..archive.len() {
        let Ok(entry) = archive.by_index(idx) else {
            continue;
        };
        let name = entry.name().replace('\\', "/");
        if !name.to_ascii_lowercase().ends_with(".jar") {
            continue;
        }
        if name.starts_with("META-INF/jarjar/") || name.starts_with("META-INF/jars/") {
            out.push(name);
        }
    }
    out
}

fn collect_rel_path_provided_mod_ids(rel_path: &str) -> Vec<String> {
    let lower = rel_path.to_ascii_lowercase();
    let mut out = Vec::new();
    if lower.contains("kotlinforforge") {
        out.push("kotlinforforge".to_string());
    }
    out
}

fn collect_jar_path_provided_mod_ids(path: &str) -> Vec<String> {
    let lower = path.to_ascii_lowercase();
    let mut out = Vec::new();
    if lower.contains("kffmod") || lower.contains("kotlinforforge") {
        out.push("kotlinforforge".to_string());
    }
    out
}

fn format_analyze_mod_file_name_fallback(rel_path: &str) -> String {
    rel_path
        .replace('\\', "/")
        .rsplit('/')
        .next()
        .unwrap_or(rel_path)
        .trim_end_matches(".jar")
        .to_string()
}

fn collect_declared_mod_ids<R: Read + Seek>(archive: &mut zip::ZipArchive<R>) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(Some(text)) = read_zip_text(archive, "fabric.mod.json") {
        if let Ok(json) = parse_json_metadata(&text, "fabric.mod.json") {
            if let Some(id) = json["id"].as_str() {
                out.push(id.to_string());
            }
            append_unique_ids(&mut out, collect_json_provides(&json));
        }
    }
    if let Ok(Some(text)) = read_zip_text(archive, "quilt.mod.json") {
        if let Ok(json) = parse_json_metadata(&text, "quilt.mod.json") {
            let loader = &json["quilt_loader"];
            if let Some(id) = loader["id"].as_str() {
                out.push(id.to_string());
            }
            append_unique_ids(&mut out, collect_json_provides(loader));
        }
    }
    if let Ok(Some(text)) = read_zip_text(archive, "META-INF/neoforge.mods.toml") {
        append_unique_ids(&mut out, collect_forge_mod_ids(&text));
    } else if let Ok(Some(text)) = read_zip_text(archive, "META-INF/mods.toml") {
        append_unique_ids(&mut out, collect_forge_mod_ids(&text));
    }
    out
}

fn collect_json_provides(json: &serde_json::Value) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(items) = json["provides"].as_array() {
        for item in items {
            if let Some(id) = item.as_str() {
                out.push(id.to_string());
            } else if let Some(id) = item["id"].as_str() {
                out.push(id.to_string());
            }
        }
    }
    out
}

fn collect_forge_mod_ids(text: &str) -> Vec<String> {
    let Ok(value) = toml::from_str::<toml::Value>(text) else {
        return Vec::new();
    };
    value
        .get("mods")
        .and_then(|item| item.as_array())
        .map(|mods| {
            mods.iter()
                .filter_map(|item| item.get("modId").and_then(|value| value.as_str()))
                .map(|id| id.to_string())
                .collect()
        })
        .unwrap_or_default()
}

fn append_unique_ids(target: &mut Vec<String>, ids: Vec<String>) {
    for id in ids {
        let id = id.trim();
        if id.is_empty() {
            continue;
        }
        if !target
            .iter()
            .any(|item| normalize_mod_id(item) == normalize_mod_id(id))
        {
            target.push(id.to_string());
        }
    }
}

fn parse_forge_mods_toml(
    rel_path: &str,
    text: &str,
    loader: &str,
) -> Result<Vec<ModMetadata>, String> {
    let value: toml::Value =
        toml::from_str(text).map_err(|e| format!("解析 mods.toml 失败: {}", e))?;
    let mut out = Vec::new();
    let Some(mods) = value.get("mods").and_then(|item| item.as_array()) else {
        return Ok(out);
    };

    let dependency_table = value
        .get("dependencies")
        .and_then(|item| item.as_table())
        .cloned()
        .unwrap_or_default();

    for item in mods {
        let mod_id = item
            .get("modId")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .trim();
        if mod_id.is_empty() {
            continue;
        }
        let mut dependencies = Vec::new();
        if let Some(dep_values) = dependency_table
            .get(mod_id)
            .and_then(|value| value.as_array())
        {
            for dep in dep_values {
                let dep_id = dep
                    .get("modId")
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .trim();
                if dep_id.is_empty() {
                    continue;
                }
                let required = dep
                    .get("mandatory")
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false);
                dependencies.push(DependencyRequirement {
                    mod_id: dep_id.to_string(),
                    version_req: dep
                        .get("versionRange")
                        .map(toml_value_to_string)
                        .unwrap_or_default(),
                    required,
                });
            }
        }

        out.push(ModMetadata {
            file: rel_path.to_string(),
            mod_id: mod_id.to_string(),
            name: item
                .get("displayName")
                .and_then(|value| value.as_str())
                .unwrap_or(mod_id)
                .to_string(),
            version: item
                .get("version")
                .map(toml_value_to_string)
                .unwrap_or_default(),
            loader: loader.to_string(),
            provides: Vec::new(),
            dependencies,
        });
    }

    Ok(out)
}

fn collect_quilt_dependencies(value: &serde_json::Value, out: &mut Vec<DependencyRequirement>) {
    if let Some(items) = value.as_array() {
        for item in items {
            if let Some(dep_id) = item.as_str() {
                out.push(DependencyRequirement {
                    mod_id: dep_id.to_string(),
                    version_req: String::new(),
                    required: true,
                });
                continue;
            }
            let dep_id = item["id"].as_str().unwrap_or("").trim();
            if dep_id.is_empty() {
                continue;
            }
            out.push(DependencyRequirement {
                mod_id: dep_id.to_string(),
                version_req: json_dependency_value_to_string(&item["versions"]),
                required: true,
            });
        }
    } else if let Some(map) = value.as_object() {
        for (dep_id, version) in map {
            out.push(DependencyRequirement {
                mod_id: dep_id.to_string(),
                version_req: json_dependency_value_to_string(version),
                required: true,
            });
        }
    }
}

/// 检测同一个 modId 是否来自多个 jar，这通常就是不同版本重复安装。
fn detect_duplicates(mods: &[ModMetadata]) -> Vec<DuplicateIssue> {
    let mut grouped: HashMap<String, Vec<&ModMetadata>> = HashMap::new();
    for item in mods {
        grouped
            .entry(normalize_mod_id(&item.mod_id))
            .or_default()
            .push(item);
    }

    let mut out = Vec::new();
    for (mod_id, items) in grouped {
        let mut files = HashSet::new();
        for item in &items {
            files.insert(item.file.clone());
        }
        if files.len() <= 1 {
            continue;
        }
        out.push(DuplicateIssue {
            mod_id,
            name: items
                .first()
                .map(|item| item.name.clone())
                .unwrap_or_default(),
            files: items
                .iter()
                .map(|item| DuplicateFile {
                    file: item.file.clone(),
                    version: item.version.clone(),
                })
                .collect(),
        });
    }
    out.sort_by(|a, b| a.mod_id.cmp(&b.mod_id));
    out
}

/// 只检查 required 前置是否存在；版本范围先展示出来，后面再做严格比较。
fn detect_missing_dependencies(mods: &[ModMetadata]) -> Vec<MissingDependencyIssue> {
    let installed = collect_provided_mod_ids(mods);
    let mut seen = HashSet::new();
    let mut out = Vec::new();

    for item in mods {
        for dep in &item.dependencies {
            if !dep.required {
                continue;
            }
            let dep_id = normalize_mod_id(&dep.mod_id);
            if dep_id.is_empty() || is_dependency_provided(&dep_id, &installed) {
                continue;
            }
            let key = format!("{}:{}:{}", item.file, item.mod_id, dep_id);
            if !seen.insert(key) {
                continue;
            }
            out.push(MissingDependencyIssue {
                file: item.file.clone(),
                mod_id: item.mod_id.clone(),
                mod_name: item.name.clone(),
                dependency_id: dep_id,
                version_req: dep.version_req.clone(),
            });
        }
    }
    out.sort_by(|a, b| a.dependency_id.cmp(&b.dependency_id));
    out
}

fn collect_provided_mod_ids(mods: &[ModMetadata]) -> HashSet<String> {
    let mut out = HashSet::new();
    for item in mods {
        out.insert(normalize_mod_id(&item.mod_id));
        for provided in &item.provides {
            out.insert(normalize_mod_id(provided));
        }
    }
    out
}

/// 先做明显的 Loader 不匹配，Quilt 允许 Fabric Mod。
fn detect_loader_mismatches(
    mods: &[ModMetadata],
    instance_loader: &str,
) -> Vec<LoaderMismatchIssue> {
    let instance_loader = instance_loader.trim().to_lowercase();
    if instance_loader.is_empty() || instance_loader == "vanilla" {
        return Vec::new();
    }
    let provided = collect_provided_mod_ids(mods);
    let connector_enabled = provided.contains("connectormod")
        || provided.contains("connector")
        || provided.contains("connector-mod");

    let mut out = Vec::new();
    for item in mods {
        if loader_is_compatible(&instance_loader, &item.loader, connector_enabled) {
            continue;
        }
        out.push(LoaderMismatchIssue {
            file: item.file.clone(),
            mod_id: item.mod_id.clone(),
            mod_name: item.name.clone(),
            mod_loader: item.loader.clone(),
            instance_loader: instance_loader.clone(),
        });
    }
    out
}

fn loader_is_compatible(instance_loader: &str, mod_loader: &str, connector_enabled: bool) -> bool {
    let mod_loader = mod_loader.trim().to_lowercase();
    match instance_loader {
        "fabric" => mod_loader == "fabric" || mod_loader == "quilt",
        "quilt" => mod_loader == "quilt" || mod_loader == "fabric",
        "forge" => {
            mod_loader == "forge"
                || (connector_enabled && matches!(mod_loader.as_str(), "fabric" | "quilt"))
        }
        "neoforge" => {
            mod_loader == "neoforge"
                || (connector_enabled && matches!(mod_loader.as_str(), "fabric" | "quilt"))
        }
        _ => true,
    }
}

fn is_builtin_dependency(mod_id: &str) -> bool {
    matches!(
        mod_id,
        "minecraft"
            | "java"
            | "fabric"
            | "fabric-api"
            | "fabric_api"
            | "fabricloader"
            | "fabric-loader"
            | "quilt_loader"
            | "quilt-loader"
            | "quilt_resource_loader"
            | "quilt-resource-loader"
            | "quilted_fabric_api"
            | "qsl"
            | "quilt_base"
            | "forge"
            | "neoforge"
            | "fml"
            | "javafml"
            | "modlauncher"
            | "mixin"
            | "mixinextras"
    )
}

fn is_dependency_provided(mod_id: &str, installed: &HashSet<String>) -> bool {
    if is_builtin_dependency(mod_id) || installed.contains(mod_id) {
        return true;
    }

    // Fabric API 顶层 jar 自带很多内部模块；这些模块不当成独立 Mod 检测。
    installed.contains("fabric-api") && is_fabric_api_module_dependency(mod_id)
}

fn is_fabric_api_module_dependency(mod_id: &str) -> bool {
    matches!(
        mod_id,
        "fabric"
            | "fabric-api-base"
            | "fabric-api-lookup-api-v1"
            | "fabric-biome-api-v1"
            | "fabric-block-api-v1"
            | "fabric-block-view-api-v2"
            | "fabric-blockrenderlayer-v1"
            | "fabric-client-tags-api-v1"
            | "fabric-command-api-v1"
            | "fabric-command-api-v2"
            | "fabric-commands-v0"
            | "fabric-content-registries-v0"
            | "fabric-convention-tags-v1"
            | "fabric-convention-tags-v2"
            | "fabric-crash-report-info-v1"
            | "fabric-data-attachment-api-v1"
            | "fabric-data-generation-api-v1"
            | "fabric-dimensions-v1"
            | "fabric-entity-events-v1"
            | "fabric-events-interaction-v0"
            | "fabric-game-rule-api-v1"
            | "fabric-gametest-api-v1"
            | "fabric-item-api-v1"
            | "fabric-item-group-api-v1"
            | "fabric-key-binding-api-v1"
            | "fabric-keybindings-v0"
            | "fabric-lifecycle-events-v1"
            | "fabric-loot-api-v2"
            | "fabric-loot-api-v3"
            | "fabric-loot-tables-v1"
            | "fabric-message-api-v1"
            | "fabric-mining-level-api-v1"
            | "fabric-model-loading-api-v1"
            | "fabric-models-v0"
            | "fabric-networking-api-v1"
            | "fabric-networking-v0"
            | "fabric-object-builder-api-v1"
            | "fabric-particles-v1"
            | "fabric-recipe-api-v1"
            | "fabric-recipe-api-v2"
            | "fabric-registry-sync-v0"
            | "fabric-renderer-api-v1"
            | "fabric-renderer-indigo"
            | "fabric-renderer-registries-v1"
            | "fabric-rendering-data-attachment-v1"
            | "fabric-rendering-fluids-v1"
            | "fabric-rendering-v0"
            | "fabric-rendering-v1"
            | "fabric-resource-conditions-api-v1"
            | "fabric-resource-loader-v0"
            | "fabric-resource-loader-v1"
            | "fabric-screen-api-v1"
            | "fabric-screen-handler-api-v1"
            | "fabric-sound-api-v1"
            | "fabric-transfer-api-v1"
            | "fabric-transitive-access-wideners-v1"
    )
}

fn load_meta_cache(game_root: &Path, instance_name: &str) -> ModMetaCache {
    let path = meta_cache_path(game_root, instance_name);
    let Ok(data) = std::fs::read_to_string(path) else {
        return ModMetaCache::default();
    };
    let cache: ModMetaCache = serde_json::from_str(&data).unwrap_or_default();
    if cache.version != META_CACHE_VERSION {
        return ModMetaCache::default();
    }
    cache
}

fn save_meta_cache(
    game_root: &Path,
    instance_name: &str,
    cache: &ModMetaCache,
) -> Result<(), String> {
    let dir = game_root.join(META_CACHE_DIR).join(META_CACHE_SUBDIR);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = meta_cache_path(game_root, instance_name);
    let data = serde_json::to_string_pretty(cache).map_err(|e| e.to_string())?;
    std::fs::write(path, data).map_err(|e| e.to_string())
}

fn meta_cache_path(game_root: &Path, instance_name: &str) -> PathBuf {
    game_root
        .join(META_CACHE_DIR)
        .join(META_CACHE_SUBDIR)
        .join(format!("{}.json", safe_index_name(instance_name)))
}

fn safe_index_name(value: &str) -> String {
    value
        .trim()
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect()
}

fn normalize_mod_id(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

fn normalize_rel_path(value: &str) -> String {
    value.replace('\\', "/")
}

fn json_dependency_value_to_string(value: &serde_json::Value) -> String {
    if let Some(text) = value.as_str() {
        return text.to_string();
    }
    if let Some(items) = value.as_array() {
        return items
            .iter()
            .filter_map(|item| item.as_str().map(|text| text.to_string()))
            .collect::<Vec<_>>()
            .join(", ");
    }
    String::new()
}

fn toml_value_to_string(value: &toml::Value) -> String {
    value
        .as_str()
        .map(|text| text.to_string())
        .unwrap_or_else(|| value.to_string().trim_matches('"').to_string())
}
