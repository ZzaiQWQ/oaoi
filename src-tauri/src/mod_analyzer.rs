use crate::instance::{resolve_game_dir, safe_path_name, version_dir};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::{Cursor, Read, Seek};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

const META_CACHE_DIR: &str = "launcher-data";
const META_CACHE_SUBDIR: &str = "mod-meta-cache";
const META_CACHE_VERSION: u32 = 12;
const MAX_MOD_JAR_SIZE: u64 = 1024 * 1024 * 1024;
const MAX_MOD_ARCHIVE_ENTRIES: usize = 20_000;
const MAX_MOD_ARCHIVE_TOTAL_SIZE: u64 = 2 * 1024 * 1024 * 1024;
const MAX_MOD_METADATA_SIZE: u64 = 8 * 1024 * 1024;
const MAX_EMBEDDED_JAR_SIZE: u64 = 256 * 1024 * 1024;
const MAX_EMBEDDED_JAR_DEPTH: usize = 1;

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
    #[serde(default)]
    content_sha1: String,
    #[serde(default)]
    loader: String,
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
    mc_version: &str,
    loader: &str,
) -> Result<AnalyzeResult, String> {
    let mutation_lock = crate::mod_manager::mod_mutation_lock(game_dir, name)?;
    let _mutation_guard = mutation_lock
        .lock()
        .map_err(|_| "Mod 文件事务锁已损坏".to_string())?;
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
    let runtime_versions =
        detect_instance_runtime_versions(&game_root, &safe_name, mc_version, loader);
    let missing_dependencies = detect_missing_dependencies(&mods, &runtime_versions);
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
    let mutation_lock = crate::mod_manager::mod_mutation_lock(game_dir, name)?;
    let _mutation_guard = mutation_lock
        .lock()
        .map_err(|_| "Mod 文件事务锁已损坏".to_string())?;
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

pub(crate) fn collect_installed_mod_ids(mods_dir: &Path, loader: &str) -> HashSet<String> {
    let Ok(files) = list_jar_files(mods_dir) else {
        return HashSet::new();
    };
    let mut ids = HashSet::new();
    for path in files {
        let rel_path = path
            .file_name()
            .map(|value| format!("mods/{}", value.to_string_lossy()))
            .unwrap_or_default();
        let Ok(metadata) = parse_mod_jar(&path, &rel_path, loader) else {
            continue;
        };
        append_metadata_ids(&mut ids, &metadata);
    }
    ids
}

pub(crate) fn read_mod_ids_from_file(path: &Path, loader: &str) -> Result<HashSet<String>, String> {
    let rel_path = path
        .file_name()
        .map(|value| format!("mods/{}", value.to_string_lossy()))
        .unwrap_or_default();
    let metadata = parse_mod_jar(path, &rel_path, loader)?;
    let mut ids = HashSet::new();
    append_metadata_ids(&mut ids, &metadata);
    Ok(ids)
}

fn append_metadata_ids(ids: &mut HashSet<String>, metadata: &[ModMetadata]) {
    for item in metadata {
        let mod_id = normalize_mod_id(&item.mod_id);
        if !mod_id.is_empty() {
            ids.insert(mod_id);
        }
        for provided in &item.provides {
            let provided = normalize_mod_id(provided);
            if !provided.is_empty() {
                ids.insert(provided);
            }
        }
    }
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
    let content_sha1 = hash_file_sha1(path)?;
    let normalized_loader = normalize_loader(loader);
    if let Some(cached) = cache.files.get(rel_path) {
        if cached.size == size
            && cached.modified_time == modified_time
            && cached.content_sha1 == content_sha1
            && cached.loader == normalized_loader
        {
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
                    content_sha1,
                    loader: normalized_loader.clone(),
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
                    content_sha1,
                    loader: normalized_loader,
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

fn hash_file_sha1(path: &Path) -> Result<String, String> {
    let mut file = std::fs::File::open(path).map_err(|e| format!("打开 Mod 文件失败: {}", e))?;
    let mut hasher = sha1_smol::Sha1::new();
    let mut buffer = [0u8; 128 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|e| format!("读取 Mod 文件失败: {}", e))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.digest().to_string())
}

/// 解析顶层 jar 的 Fabric/Quilt/Forge/NeoForge 元数据；内嵌 jar 不作为独立 Mod 参与检测。
fn parse_mod_jar(path: &Path, rel_path: &str, loader: &str) -> Result<Vec<ModMetadata>, String> {
    let file_size = std::fs::metadata(path)
        .map_err(|e| format!("读取文件信息失败: {}", e))?
        .len();
    if file_size > MAX_MOD_JAR_SIZE {
        return Err(format!("Mod 文件超过检测限制: {} bytes", file_size));
    }
    let file = std::fs::File::open(path).map_err(|e| format!("打开失败: {}", e))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("读取 Jar 失败: {}", e))?;
    validate_mod_archive(&mut archive)?;
    parse_zip_archive(&mut archive, rel_path, loader)
}

fn validate_mod_archive<R: Read + Seek>(archive: &mut zip::ZipArchive<R>) -> Result<(), String> {
    if archive.len() > MAX_MOD_ARCHIVE_ENTRIES {
        return Err(format!(
            "Mod 压缩条目超过检测限制: {} > {}",
            archive.len(),
            MAX_MOD_ARCHIVE_ENTRIES
        ));
    }
    let mut total_size = 0u64;
    for index in 0..archive.len() {
        let entry = archive
            .by_index(index)
            .map_err(|e| format!("读取 Mod 压缩条目失败: {}", e))?;
        total_size = total_size
            .checked_add(entry.size())
            .ok_or_else(|| "Mod 展开大小溢出".to_string())?;
        if total_size > MAX_MOD_ARCHIVE_TOTAL_SIZE {
            return Err(format!("Mod 总展开大小超过检测限制: {} bytes", total_size));
        }
    }
    Ok(())
}

fn parse_zip_archive<R: Read + Seek>(
    archive: &mut zip::ZipArchive<R>,
    rel_path: &str,
    _loader: &str,
) -> Result<Vec<ModMetadata>, String> {
    let mut out = Vec::new();

    // 检测必须独立于实例 Loader，否则装错 Loader 的 Mod 会被当成“无法识别”。
    parse_fabric_like_metadata(archive, rel_path, &mut out)?;
    if let Some(text) = read_zip_text(archive, "META-INF/neoforge.mods.toml")? {
        out.extend(parse_forge_mods_toml(rel_path, &text, "neoforge")?);
    }
    if let Some(text) = read_zip_text(archive, "META-INF/mods.toml")? {
        let descriptor_loader = forge_descriptor_loader(&text);
        out.extend(parse_forge_mods_toml(rel_path, &text, descriptor_loader)?);
    }
    attach_forge_embedded_provides(archive, rel_path, &mut out);

    Ok(out)
}

fn normalize_loader(loader: &str) -> String {
    loader.trim().to_ascii_lowercase()
}

fn forge_descriptor_loader(text: &str) -> &'static str {
    let Ok(value) = toml::from_str::<toml::Value>(text) else {
        return "forge";
    };
    let has_neoforge_dependency = value
        .get("dependencies")
        .and_then(toml::Value::as_table)
        .into_iter()
        .flat_map(|table| table.values())
        .filter_map(toml::Value::as_array)
        .flatten()
        .any(|dependency| {
            dependency
                .get("modId")
                .and_then(toml::Value::as_str)
                .is_some_and(|id| id.eq_ignore_ascii_case("neoforge"))
        });
    if has_neoforge_dependency {
        "neoforge"
    } else {
        "forge"
    }
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
        let embedded_ids = collect_embedded_provided_mod_ids(archive, &json, 0);
        if let Some(first) = parsed.first_mut() {
            append_unique_ids(&mut first.provides, embedded_ids);
        }
        out.extend(parsed);
    }
    if let Some(text) = read_zip_text(archive, "quilt.mod.json")? {
        let json = parse_json_metadata(&text, "quilt.mod.json")?;
        let mut parsed = parse_quilt_value(rel_path, &json);
        let embedded_ids = collect_embedded_provided_mod_ids(archive, &json, 0);
        if let Some(first) = parsed.first_mut() {
            append_unique_ids(&mut first.provides, embedded_ids);
        }
        out.extend(parsed);
    }
    Ok(())
}

fn read_zip_text<R: Read + Seek>(
    archive: &mut zip::ZipArchive<R>,
    name: &str,
) -> Result<Option<String>, String> {
    match archive.by_name(name) {
        Ok(mut entry) => {
            if entry.size() > MAX_MOD_METADATA_SIZE {
                return Err(format!("{} 超过元数据大小限制", name));
            }
            let mut bytes = Vec::with_capacity(entry.size() as usize);
            let mut limited = (&mut entry).take(MAX_MOD_METADATA_SIZE + 1);
            limited
                .read_to_end(&mut bytes)
                .map_err(|e| format!("读取 {} 失败: {}", name, e))?;
            if bytes.len() as u64 > MAX_MOD_METADATA_SIZE {
                return Err(format!("{} 超过元数据大小限制", name));
            }
            let text =
                String::from_utf8(bytes).map_err(|e| format!("读取 {} 失败: {}", name, e))?;
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
            if entry.size() > MAX_EMBEDDED_JAR_SIZE {
                return Err(format!("{} 超过内嵌 JAR 大小限制", name));
            }
            let mut bytes = Vec::with_capacity(entry.size() as usize);
            let mut limited = (&mut entry).take(MAX_EMBEDDED_JAR_SIZE + 1);
            limited
                .read_to_end(&mut bytes)
                .map_err(|e| format!("读取 {} 失败: {}", name, e))?;
            if bytes.len() as u64 > MAX_EMBEDDED_JAR_SIZE {
                return Err(format!("{} 超过内嵌 JAR 大小限制", name));
            }
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
    depth: usize,
) -> Vec<String> {
    if depth >= MAX_EMBEDDED_JAR_DEPTH {
        return Vec::new();
    }
    let mut out = Vec::new();
    for nested_path in embedded_jar_paths(json) {
        let Ok(Some(bytes)) = read_zip_bytes(archive, &nested_path) else {
            continue;
        };
        let cursor = Cursor::new(bytes);
        let Ok(mut nested_archive) = zip::ZipArchive::new(cursor) else {
            continue;
        };
        if validate_mod_archive(&mut nested_archive).is_err() {
            continue;
        }
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
    collect_forge_embedded_provided_mod_ids_at_depth(archive, 0)
}

fn collect_forge_embedded_provided_mod_ids_at_depth<R: Read + Seek>(
    archive: &mut zip::ZipArchive<R>,
    depth: usize,
) -> Vec<String> {
    if depth >= MAX_EMBEDDED_JAR_DEPTH {
        return Vec::new();
    }
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
        if validate_mod_archive(&mut nested_archive).is_err() {
            continue;
        }
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
                let side = dep
                    .get("side")
                    .and_then(|value| value.as_str())
                    .unwrap_or("BOTH");
                if side.eq_ignore_ascii_case("SERVER") {
                    continue;
                }
                let dep_id = dep
                    .get("modId")
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .trim();
                if dep_id.is_empty() {
                    continue;
                }
                // Forge 使用 mandatory，NeoForge 新格式使用 type = "required"。
                let required = dep
                    .get("mandatory")
                    .and_then(|value| value.as_bool())
                    .unwrap_or_else(|| {
                        dep.get("type")
                            .and_then(|value| value.as_str())
                            .is_some_and(|value| value.eq_ignore_ascii_case("required"))
                    });
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

/// 检查 required 前置及其声明的版本范围，运行环境依赖使用实例版本信息校验。
fn detect_missing_dependencies(
    mods: &[ModMetadata],
    runtime_versions: &HashMap<String, Vec<String>>,
) -> Vec<MissingDependencyIssue> {
    let installed = collect_provided_mod_versions(mods);
    let mut seen = HashSet::new();
    let mut out = Vec::new();

    for item in mods {
        for dep in &item.dependencies {
            if !dep.required {
                continue;
            }
            let dep_id = normalize_mod_id(&dep.mod_id);
            if dep_id.is_empty()
                || dependency_is_satisfied(&dep_id, &dep.version_req, &installed, runtime_versions)
            {
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

fn collect_provided_mod_versions(mods: &[ModMetadata]) -> HashMap<String, Vec<String>> {
    let mut out = HashMap::new();
    for item in mods {
        push_identity_version(&mut out, &item.mod_id, &item.version);
        for provided in &item.provides {
            push_identity_version(&mut out, provided, &item.version);
        }
    }
    out
}

fn collect_provided_mod_ids(mods: &[ModMetadata]) -> HashSet<String> {
    collect_provided_mod_versions(mods).into_keys().collect()
}

fn push_identity_version(versions: &mut HashMap<String, Vec<String>>, mod_id: &str, version: &str) {
    let mod_id = normalize_mod_id(mod_id);
    if mod_id.is_empty() {
        return;
    }
    let values = versions.entry(mod_id).or_default();
    let version = version.trim();
    if !version.is_empty() && !values.iter().any(|value| value == version) {
        values.push(version.to_string());
    }
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
    let compatible_files: HashSet<String> = mods
        .iter()
        .filter(|item| loader_is_compatible(&instance_loader, &item.loader, connector_enabled))
        .map(|item| item.file.clone())
        .collect();

    let mut out = Vec::new();
    let mut reported_files = HashSet::new();
    for item in mods {
        // 同一个 jar 可能同时声明 Fabric 与 Quilt，只要有一个描述符兼容就视为可用。
        if compatible_files.contains(&item.file) || !reported_files.insert(item.file.clone()) {
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
        "fabric" => mod_loader == "fabric",
        "quilt" => mod_loader == "quilt" || mod_loader == "fabric",
        "forge" => mod_loader == "forge" || (connector_enabled && mod_loader == "fabric"),
        "neoforge" => mod_loader == "neoforge" || (connector_enabled && mod_loader == "fabric"),
        _ => true,
    }
}

fn detect_instance_runtime_versions(
    game_root: &Path,
    safe_name: &str,
    mc_version: &str,
    loader: &str,
) -> HashMap<String, Vec<String>> {
    let mut versions = HashMap::new();
    let mut detected_mc = mc_version.trim().to_string();
    let mut detected_java = None;
    let mut declared_loader_version = None;
    let mut current = safe_name.to_string();
    let mut visited = HashSet::new();

    for _ in 0..16 {
        let Ok(current_safe) = safe_path_name(&current, "继承版本名") else {
            break;
        };
        if !visited.insert(current_safe.clone()) {
            break;
        }
        let json_path =
            version_dir(game_root, &current_safe).join(format!("{}.json", current_safe));
        let Ok(data) = std::fs::read_to_string(json_path) else {
            break;
        };
        let Ok(json) = serde_json::from_str::<serde_json::Value>(&data) else {
            break;
        };

        if detected_mc.is_empty() {
            detected_mc = ["clientVersion", "mcVersion"]
                .iter()
                .find_map(|key| json[*key].as_str())
                .unwrap_or_default()
                .to_string();
        }
        if detected_java.is_none() {
            detected_java = json["javaVersion"]["majorVersion"]
                .as_u64()
                .map(|value| value.to_string());
        }
        if declared_loader_version.is_none() {
            declared_loader_version = json["loaderVersion"]
                .as_str()
                .or_else(|| json["loader_version"].as_str())
                .or_else(|| json["loader"]["version"].as_str())
                .map(str::to_string);
        }
        if let Some(libraries) = json["libraries"].as_array() {
            for library in libraries {
                if let Some(name) = library["name"].as_str() {
                    collect_loader_version_from_library(&mut versions, name, &detected_mc);
                }
            }
        }

        let Some(parent) = json["inheritsFrom"]
            .as_str()
            .filter(|value| !value.is_empty())
        else {
            break;
        };
        current = parent.to_string();
    }

    if !detected_mc.is_empty() {
        push_identity_version(&mut versions, "minecraft", &detected_mc);
    }
    let java_version = detected_java.or_else(|| infer_java_major(&detected_mc));
    if let Some(java_version) = java_version {
        push_identity_version(&mut versions, "java", &java_version);
    }
    if let Some(loader_version) = declared_loader_version {
        insert_loader_runtime_aliases(&mut versions, loader, &loader_version);
    }
    versions
}

fn collect_loader_version_from_library(
    versions: &mut HashMap<String, Vec<String>>,
    coordinate: &str,
    mc_version: &str,
) {
    let parts = coordinate.split(':').collect::<Vec<_>>();
    if parts.len() < 3 {
        return;
    }
    let group = parts[0];
    let artifact = parts[1];
    let raw_version = parts[2];
    match (group, artifact) {
        ("net.fabricmc", "fabric-loader") => {
            insert_loader_runtime_aliases(versions, "fabric", raw_version)
        }
        ("org.quiltmc", "quilt-loader") => {
            insert_loader_runtime_aliases(versions, "quilt", raw_version)
        }
        ("net.minecraftforge", "forge") => {
            let version = raw_version
                .strip_prefix(&format!("{}-", mc_version))
                .or_else(|| raw_version.rsplit_once('-').map(|(_, value)| value))
                .unwrap_or(raw_version);
            insert_loader_runtime_aliases(versions, "forge", version);
        }
        ("net.neoforged", "neoforge") => {
            insert_loader_runtime_aliases(versions, "neoforge", raw_version)
        }
        _ => {}
    }
}

fn insert_loader_runtime_aliases(
    versions: &mut HashMap<String, Vec<String>>,
    loader: &str,
    version: &str,
) {
    match normalize_loader(loader).as_str() {
        "fabric" => {
            push_identity_version(versions, "fabricloader", version);
            push_identity_version(versions, "fabric-loader", version);
        }
        "quilt" => {
            push_identity_version(versions, "quilt_loader", version);
            push_identity_version(versions, "quilt-loader", version);
        }
        "forge" => {
            for id in ["forge", "fml", "javafml"] {
                push_identity_version(versions, id, version);
            }
        }
        "neoforge" => {
            for id in ["neoforge", "fml", "javafml"] {
                push_identity_version(versions, id, version);
            }
        }
        _ => {}
    }
}

fn infer_java_major(mc_version: &str) -> Option<String> {
    let version = mc_version.trim().to_ascii_lowercase();
    if version.is_empty() {
        return None;
    }
    if !version.starts_with("1.") {
        let year = version
            .split(|ch: char| !ch.is_ascii_digit())
            .find(|part| !part.is_empty())?
            .parse::<u32>()
            .ok()?;
        return Some(if year >= 26 { "25" } else { "21" }.to_string());
    }
    let numbers = version
        .split(|ch: char| !ch.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .filter_map(|part| part.parse::<u32>().ok())
        .collect::<Vec<_>>();
    let minor = *numbers.get(1)?;
    let patch = numbers.get(2).copied().unwrap_or(0);
    let major = if minor < 7 || (minor == 7 && patch < 10) {
        7
    } else if minor < 17 {
        8
    } else if minor == 17 {
        16
    } else if minor < 20 || (minor == 20 && patch <= 4) {
        17
    } else {
        21
    };
    Some(major.to_string())
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum VersionPart {
    Number(u64),
    Text(String),
}

fn version_matches_requirement(actual: &str, requirement: &str) -> bool {
    let actual = actual.trim();
    let requirement = requirement.trim().trim_matches('"');
    if actual.is_empty()
        || actual.contains("${")
        || requirement.is_empty()
        || requirement == "*"
        || requirement.contains("${")
    {
        return true;
    }
    requirement
        .split("||")
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .any(|item| version_matches_all(actual, item))
}

fn version_matches_all(actual: &str, requirement: &str) -> bool {
    let ranges = extract_maven_ranges(requirement);
    if !ranges.is_empty() {
        return ranges
            .iter()
            .any(|range| version_matches_maven_range(actual, range));
    }
    let parts = requirement
        .split(|ch: char| ch.is_whitespace() || ch == ',')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    if parts.len() > 1 {
        return parts
            .iter()
            .all(|part| version_matches_single(actual, part));
    }
    version_matches_single(actual, requirement)
}

fn extract_maven_ranges(requirement: &str) -> Vec<String> {
    let bytes = requirement.as_bytes();
    let mut ranges = Vec::new();
    let mut index = 0usize;
    while index < bytes.len() {
        if !matches!(bytes[index], b'[' | b'(') {
            index += 1;
            continue;
        }
        let start = index;
        index += 1;
        while index < bytes.len() && !matches!(bytes[index], b']' | b')') {
            index += 1;
        }
        if index < bytes.len() {
            ranges.push(requirement[start..=index].to_string());
            index += 1;
        }
    }
    ranges
}

fn version_matches_maven_range(actual: &str, range: &str) -> bool {
    if range.len() < 2 {
        return true;
    }
    let lower_inclusive = range.starts_with('[');
    let upper_inclusive = range.ends_with(']');
    let inner = &range[1..range.len() - 1];
    let Some((lower, upper)) = inner.split_once(',') else {
        return lower_inclusive
            && upper_inclusive
            && compare_versions(actual, inner.trim()) == std::cmp::Ordering::Equal;
    };
    let lower = lower.trim();
    let upper = upper.trim();
    let lower_ok = lower.is_empty()
        || match compare_versions(actual, lower) {
            std::cmp::Ordering::Greater => true,
            std::cmp::Ordering::Equal => lower_inclusive,
            std::cmp::Ordering::Less => false,
        };
    let upper_ok = upper.is_empty()
        || match compare_versions(actual, upper) {
            std::cmp::Ordering::Less => true,
            std::cmp::Ordering::Equal => upper_inclusive,
            std::cmp::Ordering::Greater => false,
        };
    lower_ok && upper_ok
}

fn version_matches_single(actual: &str, requirement: &str) -> bool {
    let requirement = requirement.trim();
    if requirement.is_empty() || requirement == "*" {
        return true;
    }
    let has_wildcard = requirement
        .split(['.', '-', '_'])
        .any(|part| part == "*" || part.eq_ignore_ascii_case("x"));
    if has_wildcard {
        let expected = requirement
            .trim_start_matches('=')
            .split(['.', '-', '_'])
            .take_while(|part| *part != "*" && !part.eq_ignore_ascii_case("x") && !part.is_empty())
            .collect::<Vec<_>>();
        let actual = actual.split(['.', '-', '_']).collect::<Vec<_>>();
        return expected
            .iter()
            .zip(actual.iter())
            .all(|(expected, actual)| expected.eq_ignore_ascii_case(actual))
            && actual.len() >= expected.len();
    }
    for (operator, ordering) in [
        (">=", std::cmp::Ordering::Greater),
        ("<=", std::cmp::Ordering::Less),
        (">", std::cmp::Ordering::Greater),
        ("<", std::cmp::Ordering::Less),
        ("=", std::cmp::Ordering::Equal),
    ] {
        if let Some(expected) = requirement.strip_prefix(operator) {
            let compared = compare_versions(actual, expected.trim());
            return match operator {
                ">=" => matches!(
                    compared,
                    std::cmp::Ordering::Greater | std::cmp::Ordering::Equal
                ),
                "<=" => matches!(
                    compared,
                    std::cmp::Ordering::Less | std::cmp::Ordering::Equal
                ),
                _ => compared == ordering,
            };
        }
    }
    if let Some(expected) = requirement.strip_prefix('^') {
        return version_matches_caret(actual, expected.trim());
    }
    if let Some(expected) = requirement.strip_prefix('~') {
        return version_matches_tilde(actual, expected.trim());
    }
    compare_versions(actual, requirement) == std::cmp::Ordering::Equal
}

fn version_matches_caret(actual: &str, expected: &str) -> bool {
    let numbers = numeric_version_parts(expected);
    if numbers.is_empty() || compare_versions(actual, expected) == std::cmp::Ordering::Less {
        return false;
    }
    let mut upper = numbers.clone();
    let bump_index = upper.iter().position(|value| *value != 0).unwrap_or(0);
    upper[bump_index] = upper[bump_index].saturating_add(1);
    upper.truncate(bump_index + 1);
    compare_versions(actual, &join_numeric_version(&upper)) == std::cmp::Ordering::Less
}

fn version_matches_tilde(actual: &str, expected: &str) -> bool {
    let numbers = numeric_version_parts(expected);
    if numbers.is_empty() || compare_versions(actual, expected) == std::cmp::Ordering::Less {
        return false;
    }
    let bump_index = if numbers.len() > 1 { 1 } else { 0 };
    let mut upper = numbers;
    upper[bump_index] = upper[bump_index].saturating_add(1);
    upper.truncate(bump_index + 1);
    compare_versions(actual, &join_numeric_version(&upper)) == std::cmp::Ordering::Less
}

fn numeric_version_parts(version: &str) -> Vec<u64> {
    version
        .split(['.', '-', '_'])
        .take_while(|part| part.chars().all(|ch| ch.is_ascii_digit()))
        .filter_map(|part| part.parse::<u64>().ok())
        .collect()
}

fn join_numeric_version(parts: &[u64]) -> String {
    parts
        .iter()
        .map(u64::to_string)
        .collect::<Vec<_>>()
        .join(".")
}

fn compare_versions(left: &str, right: &str) -> std::cmp::Ordering {
    let left = tokenize_version(left);
    let right = tokenize_version(right);
    let length = left.len().max(right.len());
    for index in 0..length {
        let compared = match (left.get(index), right.get(index)) {
            (Some(VersionPart::Number(a)), Some(VersionPart::Number(b))) => a.cmp(b),
            (Some(VersionPart::Text(a)), Some(VersionPart::Text(b))) => a.cmp(b),
            (Some(VersionPart::Number(_)), Some(VersionPart::Text(_))) => {
                std::cmp::Ordering::Greater
            }
            (Some(VersionPart::Text(_)), Some(VersionPart::Number(_))) => std::cmp::Ordering::Less,
            (Some(VersionPart::Number(value)), None) => value.cmp(&0),
            (None, Some(VersionPart::Number(value))) => 0.cmp(value),
            (Some(VersionPart::Text(_)), None) => std::cmp::Ordering::Less,
            (None, Some(VersionPart::Text(_))) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        };
        if compared != std::cmp::Ordering::Equal {
            return compared;
        }
    }
    std::cmp::Ordering::Equal
}

fn tokenize_version(version: &str) -> Vec<VersionPart> {
    let version = version
        .trim()
        .trim_start_matches(['v', 'V'])
        .split('+')
        .next()
        .unwrap_or_default();
    let mut out = Vec::new();
    let mut current = String::new();
    let mut numeric = None;
    for ch in version.chars().chain(std::iter::once('.')) {
        let next_numeric = ch.is_ascii_digit();
        if ch.is_ascii_alphanumeric() {
            if numeric.is_some_and(|value| value != next_numeric) {
                push_version_part(&mut out, &mut current, numeric == Some(true));
            }
            numeric = Some(next_numeric);
            current.push(ch.to_ascii_lowercase());
        } else {
            push_version_part(&mut out, &mut current, numeric == Some(true));
            numeric = None;
        }
    }
    out
}

fn push_version_part(out: &mut Vec<VersionPart>, current: &mut String, numeric: bool) {
    if current.is_empty() {
        return;
    }
    if numeric {
        out.push(VersionPart::Number(current.parse().unwrap_or(u64::MAX)));
    } else {
        out.push(VersionPart::Text(std::mem::take(current)));
        return;
    }
    current.clear();
}

fn is_builtin_dependency(mod_id: &str) -> bool {
    matches!(
        mod_id,
        "minecraft"
            | "java"
            | "fabricloader"
            | "fabric-loader"
            | "quilt_loader"
            | "quilt-loader"
            | "forge"
            | "neoforge"
            | "fml"
            | "javafml"
            | "modlauncher"
            | "mixin"
            | "mixinextras"
    )
}

fn dependency_is_satisfied(
    mod_id: &str,
    version_req: &str,
    installed: &HashMap<String, Vec<String>>,
    runtime_versions: &HashMap<String, Vec<String>>,
) -> bool {
    if let Some(versions) = runtime_versions.get(mod_id) {
        return versions_satisfy_requirement(versions, version_req);
    }
    if let Some(versions) = installed.get(mod_id) {
        return versions_satisfy_requirement(versions, version_req);
    }
    if is_builtin_dependency(mod_id) {
        // 部分 Loader 内建库没有稳定可读版本，未知时不制造误报。
        return true;
    }

    // Fabric API 顶层 jar 自带很多内部模块；这些模块不当成独立 Mod 检测。
    installed.contains_key("fabric-api") && is_fabric_api_module_dependency(mod_id)
}

fn versions_satisfy_requirement(versions: &[String], requirement: &str) -> bool {
    let requirement = requirement.trim();
    if requirement.is_empty() || requirement == "*" {
        return true;
    }
    if versions.is_empty() {
        // 已识别到提供者但版本字段不可解析时，不把它误判成缺失。
        return true;
    }
    versions
        .iter()
        .any(|version| version_matches_requirement(version, requirement))
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
            .join(" || ");
    }
    String::new()
}

fn toml_value_to_string(value: &toml::Value) -> String {
    value
        .as_str()
        .map(|text| text.to_string())
        .unwrap_or_else(|| value.to_string().trim_matches('"').to_string())
}

#[cfg(test)]
mod tests {
    use super::{
        compare_versions, dependency_is_satisfied, detect_loader_mismatches,
        detect_missing_dependencies, forge_descriptor_loader, hash_file_sha1, infer_java_major,
        is_builtin_dependency, loader_is_compatible, parse_forge_mods_toml,
        version_matches_requirement, DependencyRequirement, ModMetadata,
    };
    use std::collections::HashMap;

    #[test]
    fn fabric_does_not_accept_quilt_only_mods() {
        assert!(loader_is_compatible("fabric", "fabric", false));
        assert!(!loader_is_compatible("fabric", "quilt", false));
        assert!(!loader_is_compatible("fabric", "forge", false));
        assert!(!loader_is_compatible("fabric", "neoforge", false));
        assert!(loader_is_compatible("quilt", "fabric", false));
        assert!(loader_is_compatible("quilt", "quilt", false));
        assert!(!loader_is_compatible("forge", "neoforge", false));
        assert!(!loader_is_compatible("neoforge", "forge", false));
    }

    #[test]
    fn fabric_api_is_not_a_builtin_dependency() {
        assert!(!is_builtin_dependency("fabric-api"));
        assert!(!is_builtin_dependency("fabric_api"));
        assert!(!is_builtin_dependency("quilted_fabric_api"));
        assert!(!is_builtin_dependency("qsl"));
        assert!(!is_builtin_dependency("quilt_base"));
        assert!(!is_builtin_dependency("quilt_resource_loader"));
        assert!(is_builtin_dependency("fabricloader"));
        assert!(is_builtin_dependency("quilt_loader"));
    }

    #[test]
    fn connector_only_accepts_fabric_mods() {
        assert!(loader_is_compatible("neoforge", "fabric", true));
        assert!(!loader_is_compatible("neoforge", "quilt", true));
        assert!(loader_is_compatible("forge", "fabric", true));
        assert!(!loader_is_compatible("forge", "quilt", true));
    }

    #[test]
    fn dual_descriptor_jar_uses_any_compatible_loader() {
        let metadata = |loader: &str| ModMetadata {
            file: "mods/dual.jar".to_string(),
            mod_id: format!("dual-{}", loader),
            name: "双描述符 Mod".to_string(),
            version: "1.0.0".to_string(),
            loader: loader.to_string(),
            provides: Vec::new(),
            dependencies: Vec::new(),
        };
        let mods = vec![metadata("fabric"), metadata("quilt")];
        assert!(detect_loader_mismatches(&mods, "fabric").is_empty());
        assert!(detect_loader_mismatches(&mods, "quilt").is_empty());
    }

    #[test]
    fn forge_与_neoforge_必需依赖格式都能识别() {
        let forge = parse_forge_mods_toml(
            "mods/forge.jar",
            r#"
                [[mods]]
                modId = "forge_mod"
                [[dependencies.forge_mod]]
                modId = "forge_dep"
                mandatory = true
            "#,
            "forge",
        )
        .unwrap();
        let neoforge = parse_forge_mods_toml(
            "mods/neoforge.jar",
            r#"
                [[mods]]
                modId = "neoforge_mod"
                [[dependencies.neoforge_mod]]
                modId = "neoforge_dep"
                type = "required"
            "#,
            "neoforge",
        )
        .unwrap();

        assert!(forge[0].dependencies[0].required);
        assert!(neoforge[0].dependencies[0].required);
    }

    #[test]
    fn 版本范围同时支持_fabric_和_maven_格式() {
        assert!(version_matches_requirement("1.20.1", ">=1.20 <1.21"));
        assert!(!version_matches_requirement("1.21", ">=1.20 <1.21"));
        assert!(version_matches_requirement("0.15.11", "^0.15.0"));
        assert!(!version_matches_requirement("0.16.0", "^0.15.0"));
        assert!(version_matches_requirement("47.4.0", "[47,)"));
        assert!(!version_matches_requirement("46.0.0", "[47,)"));
        assert!(version_matches_requirement("1.20.1", "[1.20.1]"));
        assert!(!version_matches_requirement("1.0.0-exp.1", "1.0.0-exp"));
        assert_eq!(
            compare_versions("1.0.0", "1.0.0-beta"),
            std::cmp::Ordering::Greater
        );
    }

    #[test]
    fn 检测普通前置和运行环境的版本范围() {
        let target = ModMetadata {
            file: "mods/target.jar".to_string(),
            mod_id: "target".to_string(),
            name: "Target".to_string(),
            version: "1.0.0".to_string(),
            loader: "fabric".to_string(),
            provides: Vec::new(),
            dependencies: vec![
                DependencyRequirement {
                    mod_id: "dependency".to_string(),
                    version_req: ">=2.0.0".to_string(),
                    required: true,
                },
                DependencyRequirement {
                    mod_id: "minecraft".to_string(),
                    version_req: ">=1.20 <1.21".to_string(),
                    required: true,
                },
                DependencyRequirement {
                    mod_id: "java".to_string(),
                    version_req: ">=17".to_string(),
                    required: true,
                },
                DependencyRequirement {
                    mod_id: "fabricloader".to_string(),
                    version_req: ">=0.15".to_string(),
                    required: true,
                },
            ],
        };
        let dependency = ModMetadata {
            file: "mods/dependency.jar".to_string(),
            mod_id: "dependency".to_string(),
            name: "Dependency".to_string(),
            version: "1.5.0".to_string(),
            loader: "fabric".to_string(),
            provides: Vec::new(),
            dependencies: Vec::new(),
        };
        let runtime = HashMap::from([
            ("minecraft".to_string(), vec!["1.20.1".to_string()]),
            ("java".to_string(), vec!["17".to_string()]),
            ("fabricloader".to_string(), vec!["0.15.11".to_string()]),
        ]);
        let issues = detect_missing_dependencies(&[target, dependency], &runtime);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].dependency_id, "dependency");
    }

    #[test]
    fn 未知内建版本不误报且已知版本严格判断() {
        let installed = HashMap::new();
        let runtime = HashMap::from([("java".to_string(), vec!["17".to_string()])]);
        assert!(dependency_is_satisfied(
            "mixin", ">=0.8", &installed, &runtime
        ));
        assert!(!dependency_is_satisfied(
            "java", ">=21", &installed, &runtime
        ));
    }

    #[test]
    fn 内容身份不只依赖文件大小() {
        let root =
            std::env::temp_dir().join(format!("oaoi-mod-analyzer-content-{}", std::process::id()));
        std::fs::write(&root, b"aaaa").unwrap();
        let first = hash_file_sha1(&root).unwrap();
        std::fs::write(&root, b"bbbb").unwrap();
        let second = hash_file_sha1(&root).unwrap();
        assert_ne!(first, second);
        std::fs::remove_file(root).unwrap();
    }

    #[test]
    fn 运行环境推断覆盖旧版和新版_java() {
        assert_eq!(infer_java_major("1.7.9").as_deref(), Some("7"));
        assert_eq!(infer_java_major("1.7.10").as_deref(), Some("8"));
        assert_eq!(infer_java_major("1.20.4").as_deref(), Some("17"));
        assert_eq!(infer_java_major("1.20.5").as_deref(), Some("21"));
        assert_eq!(infer_java_major("26.2").as_deref(), Some("25"));
    }

    #[test]
    fn mods_toml_根据内建依赖区分_neoforge() {
        assert_eq!(
            forge_descriptor_loader(
                r#"
                    [[dependencies.example]]
                    modId = "neoforge"
                    type = "required"
                "#
            ),
            "neoforge"
        );
        assert_eq!(forge_descriptor_loader("modLoader = \"javafml\""), "forge");
    }
}
