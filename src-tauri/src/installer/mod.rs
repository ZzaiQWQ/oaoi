pub mod fabric;
pub mod forge;
pub mod neoforge;
pub mod quilt;
pub mod vanilla;

// ===== 公共工具函数（供 forge/neoforge 共用） =====

const MAX_INSTALLER_ARCHIVE_ENTRIES: usize = 10_000;
const MAX_INSTALLER_ENTRY_SIZE: u64 = 512 * 1024 * 1024;
const MAX_INSTALLER_TOTAL_SIZE: u64 = 2 * 1024 * 1024 * 1024;

/// Maven 坐标转文件路径: "net.minecraftforge:forge:1.21.1-52.0.1" → "net/minecraftforge/forge/1.21.1-52.0.1/forge-1.21.1-52.0.1.jar"
pub fn maven_name_to_path(name: &str) -> String {
    let parts: Vec<&str> = name.split(':').collect();
    if parts.len() < 3 {
        return name.to_string();
    }
    let group = parts[0].replace('.', "/");
    let artifact = parts[1];
    let version = parts[2];
    let classifier = if parts.len() >= 4 {
        Some(parts[3])
    } else {
        None
    };
    let (version, ext) = if let Some(at_pos) = version.find('@') {
        (&version[..at_pos], &version[at_pos + 1..])
    } else if let Some(c) = &classifier {
        if let Some(at_pos) = c.find('@') {
            let ext = &c[at_pos + 1..];
            let cl = &c[..at_pos];
            return format!(
                "{}/{}/{}/{}-{}-{}.{}",
                group, artifact, version, artifact, version, cl, ext
            );
        } else {
            (version, "jar")
        }
    } else {
        (version, "jar")
    };
    if let Some(cl) = classifier {
        format!(
            "{}/{}/{}/{}-{}-{}.{}",
            group, artifact, version, artifact, version, cl, ext
        )
    } else {
        format!(
            "{}/{}/{}/{}-{}.{}",
            group, artifact, version, artifact, version, ext
        )
    }
}

pub fn maven_name_to_path_with_classifier(name: &str, classifier: &str) -> String {
    if name.split(':').count() >= 4 {
        return maven_name_to_path(name);
    }
    maven_name_to_path(&format!("{}:{}", name, classifier))
}

pub fn native_classifier_for_current_os(lib: &serde_json::Value) -> Option<String> {
    let classifier = lib
        .get("natives")
        .and_then(|natives| natives.get("windows"))
        .and_then(|value| value.as_str())?;
    let arch = if cfg!(target_pointer_width = "64") {
        "64"
    } else {
        "32"
    };
    Some(classifier.replace("${arch}", arch))
}

pub fn default_library_maven_base(lib_name: &str, is_native: bool) -> &'static str {
    if is_native || !lib_name.starts_with("net.minecraftforge:") {
        "https://libraries.minecraft.net"
    } else {
        "https://maven.minecraftforge.net"
    }
}

pub fn installer_generated_client_library(lib_name: &str) -> bool {
    let parts: Vec<&str> = lib_name.split(':').collect();
    if parts.len() < 4 {
        return false;
    }
    let classifier = parts[3].split('@').next().unwrap_or(parts[3]);
    if classifier != "client" {
        return false;
    }
    // 新版 Forge/NeoForge 的 client 库由 installer processor 生成，没有远端下载地址。
    matches!(
        (parts[0], parts[1]),
        ("net.minecraftforge", "forge")
            | ("net.neoforged", "forge")
            | ("net.neoforged", "neoforge")
    )
}

pub fn safe_maven_path(path: &str) -> Result<std::path::PathBuf, String> {
    let normalized = path.replace('\\', "/");
    let mut out = std::path::PathBuf::new();
    for part in normalized.split('/') {
        if part.is_empty() || part == "." || part == ".." {
            return Err(format!("非法 Maven 路径: {}", path));
        }
        let valid = part
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '+'));
        if !valid {
            return Err(format!("非法 Maven 路径: {}", path));
        }
        out.push(part);
    }
    if out.as_os_str().is_empty() {
        return Err("Maven 路径不能为空".to_string());
    }
    Ok(out)
}

/// 构建 data 变量映射（供 Forge/NeoForge processor 使用）
pub fn build_data_map(
    profile: &serde_json::Value,
    libs_dir: &std::path::Path,
    client_jar: &std::path::Path,
    installer_path: &std::path::Path,
    temp_dir: &std::path::Path,
    mc_version: &str,
) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    map.insert("SIDE".to_string(), "client".to_string());
    map.insert(
        "ROOT".to_string(),
        libs_dir
            .parent()
            .unwrap_or(libs_dir)
            .to_string_lossy()
            .to_string(),
    );
    map.insert(
        "MINECRAFT_JAR".to_string(),
        client_jar.to_string_lossy().to_string(),
    );
    map.insert("MINECRAFT_VERSION".to_string(), mc_version.to_string());
    map.insert(
        "INSTALLER".to_string(),
        installer_path.to_string_lossy().to_string(),
    );
    map.insert(
        "LIBRARY_DIR".to_string(),
        libs_dir.to_string_lossy().to_string(),
    );
    if let Some(data) = profile["data"].as_object() {
        for (key, val) in data {
            let v = val["client"].as_str().unwrap_or(val.as_str().unwrap_or(""));
            if v.starts_with('[') && v.ends_with(']') {
                let coord = &v[1..v.len() - 1];
                let path = maven_name_to_path(coord);
                if let Ok(path) = safe_maven_path(&path) {
                    map.insert(
                        key.clone(),
                        libs_dir.join(path).to_string_lossy().to_string(),
                    );
                }
            } else if v.starts_with('/') {
                if let Ok(real_path) = safe_join(temp_dir, &v[1..]) {
                    map.insert(key.clone(), real_path.to_string_lossy().to_string());
                }
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
        let key = &s[1..s.len() - 1];
        data_map.get(key).cloned().unwrap_or_else(|| s.to_string())
    } else if s.starts_with('[') && s.ends_with(']') {
        let coord = &s[1..s.len() - 1];
        let path = maven_name_to_path(coord);
        safe_maven_path(&path)
            .map(|path| libs_dir.join(path).to_string_lossy().to_string())
            .unwrap_or_else(|_| s.to_string())
    } else {
        s.to_string()
    }
}

/// 安全解压 Forge 系安装器，限制条目数量与实际展开体积。
pub fn extract_installer_archive(
    installer_path: &std::path::Path,
    temp_dir: &std::path::Path,
    loader_name: &str,
    cancel_name: &str,
) -> Result<(), String> {
    let file = std::fs::File::open(installer_path)
        .map_err(|e| format!("打开 {} 安装器失败: {}", loader_name, e))?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| format!("读取 {} 安装器失败: {}", loader_name, e))?;
    if archive.len() > MAX_INSTALLER_ARCHIVE_ENTRIES {
        return Err(format!(
            "{} 安装器文件数量超过限制: {} > {}",
            loader_name,
            archive.len(),
            MAX_INSTALLER_ARCHIVE_ENTRIES
        ));
    }

    let mut declared_total = 0u64;
    let mut actual_total = 0u64;
    for index in 0..archive.len() {
        if crate::instance::is_cancelled(cancel_name) {
            return Err("用户取消安装".to_string());
        }
        let mut entry = archive
            .by_index(index)
            .map_err(|e| format!("读取 {} 安装器条目失败: {}", loader_name, e))?;
        let entry_size = entry.size();
        if entry_size > MAX_INSTALLER_ENTRY_SIZE {
            return Err(format!(
                "{} 安装器单个文件超过限制: {} ({} bytes)",
                loader_name,
                entry.name(),
                entry_size
            ));
        }
        declared_total = declared_total
            .checked_add(entry_size)
            .ok_or_else(|| format!("{} 安装器展开大小溢出", loader_name))?;
        if declared_total > MAX_INSTALLER_TOTAL_SIZE {
            return Err(format!(
                "{} 安装器总展开大小超过限制: {} bytes",
                loader_name, declared_total
            ));
        }

        let out_path = safe_join(temp_dir, entry.name())
            .map_err(|e| format!("{} 安装器包含非法路径: {}", loader_name, e))?;
        if entry.is_dir() {
            std::fs::create_dir_all(&out_path)
                .map_err(|e| format!("创建 {} 临时目录失败: {}", loader_name, e))?;
            continue;
        }
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("创建 {} 临时目录失败: {}", loader_name, e))?;
        }
        let mut out_file = std::fs::File::create(&out_path)
            .map_err(|e| format!("创建 {} 临时文件失败: {}", loader_name, e))?;
        let copied = std::io::copy(
            &mut std::io::Read::take(&mut entry, MAX_INSTALLER_ENTRY_SIZE + 1),
            &mut out_file,
        )
        .map_err(|e| format!("解压 {} 安装器失败: {}", loader_name, e))?;
        if copied > MAX_INSTALLER_ENTRY_SIZE {
            return Err(format!(
                "{} 安装器单个文件实际展开大小超过限制: {}",
                loader_name,
                out_path.display()
            ));
        }
        actual_total = actual_total
            .checked_add(copied)
            .ok_or_else(|| format!("{} 安装器实际展开大小溢出", loader_name))?;
        if actual_total > MAX_INSTALLER_TOTAL_SIZE {
            return Err(format!(
                "{} 安装器实际总展开大小超过限制: {} bytes",
                loader_name, actual_total
            ));
        }
    }
    Ok(())
}

/// processor 成功退出后按 profile.outputs 校验每个生成文件。
pub fn verify_processor_outputs(
    processor: &serde_json::Value,
    data_map: &std::collections::HashMap<String, String>,
    libs_dir: &std::path::Path,
    work_dir: &std::path::Path,
    loader_name: &str,
) -> Result<(), String> {
    let Some(outputs) = processor.get("outputs") else {
        return Ok(());
    };
    if outputs.is_null() {
        return Ok(());
    }
    let outputs = outputs
        .as_object()
        .ok_or_else(|| format!("{} processor outputs 格式错误", loader_name))?;
    for (raw_path, expected_value) in outputs {
        let expected_raw = expected_value
            .as_str()
            .ok_or_else(|| format!("{} processor 输出 SHA1 格式错误: {}", loader_name, raw_path))?;
        let path_text = resolve_data_arg(raw_path, data_map, libs_dir);
        let expected_sha1 = resolve_data_arg(expected_raw, data_map, libs_dir);
        if expected_sha1.len() != 40 || !expected_sha1.chars().all(|ch| ch.is_ascii_hexdigit()) {
            return Err(format!(
                "{} processor 输出 SHA1 无效: {} -> {}",
                loader_name, raw_path, expected_sha1
            ));
        }
        let output_path = std::path::PathBuf::from(&path_text);
        let output_path = if output_path.is_absolute() {
            output_path
        } else {
            work_dir.join(output_path)
        };
        if !output_path.is_file() {
            return Err(format!(
                "{} processor 未生成输出文件: {}",
                loader_name,
                output_path.display()
            ));
        }
        if !file_matches_sha1(&output_path, &expected_sha1) {
            return Err(format!(
                "{} processor 输出 SHA1 不匹配: {}",
                loader_name,
                output_path.display()
            ));
        }
    }
    Ok(())
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
        } else {
            String::new()
        };

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
                } else {
                    true
                }
            });
        }
        existing_libs.push(new_lib.clone());
    }
}

pub fn empty_loader_json() -> serde_json::Value {
    serde_json::json!({
        "libraries": []
    })
}

pub fn merge_loader_install_result(base: &mut serde_json::Value, loader_json: &serde_json::Value) {
    if let Some(main_class) = loader_json.get("mainClass").and_then(|v| v.as_str()) {
        base["mainClass"] = serde_json::Value::String(main_class.to_string());
    }
    if let Some(loader) = loader_json.get("loader") {
        if !loader.is_null() {
            base["loader"] = loader.clone();
        }
    }
    if let Some(minecraft_args) = loader_json
        .get("minecraftArguments")
        .and_then(|v| v.as_str())
    {
        base["minecraftArguments"] = serde_json::Value::String(minecraft_args.to_string());
        crate::instance::remove_empty_legacy_arguments(base);
    }
    if let Some(new_libs) = loader_json.get("libraries").and_then(|v| v.as_array()) {
        if !base.get("libraries").is_some_and(|v| v.is_array()) {
            base["libraries"] = serde_json::json!([]);
        }
        if let Some(existing_libs) = base["libraries"].as_array_mut() {
            merge_libraries(existing_libs, new_libs);
        }
    }
    if let Some(loader_args) = loader_json.get("arguments") {
        if !loader_args.is_null() {
            if base["arguments"].is_null() {
                base["arguments"] = serde_json::json!({"jvm": [], "game": []});
            }
            for key in ["jvm", "game"] {
                if let Some(values) = loader_args.get(key).and_then(|v| v.as_array()) {
                    if !base["arguments"].get(key).is_some_and(|v| v.is_array()) {
                        base["arguments"][key] = serde_json::json!([]);
                    }
                    if let Some(target) = base["arguments"][key].as_array_mut() {
                        target.extend(values.iter().cloned());
                    }
                }
            }
        }
    }
}

pub fn wait_for_install_file(
    path: &std::path::Path,
    _label: &str,
    cancel_name: &str,
) -> Result<(), String> {
    loop {
        if crate::instance::is_cancelled(cancel_name) {
            return Err("用户取消安装".to_string());
        }
        if path.metadata().map(|m| m.len() > 0).unwrap_or(false) {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(120));
        if crate::instance::is_cancelled(cancel_name) {
            return Err("用户取消安装".to_string());
        }
    }
}

use crate::downloader::event::{DownloadEvent, DownloadOutcome};
use crate::downloader::{
    DownloadCandidate, DownloadEngineOptions, DownloadManager, DownloadRequest,
};
use crate::instance::{
    resolve_game_dir, safe_join, safe_path_name, strip_launcher_private_version_fields,
    version_dir, version_json_path,
};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use tauri::Emitter;

/// Forge / NeoForge 安装器全局锁 — 同一时间只能运行一个安装
pub static FORGE_LOCK: Mutex<()> = Mutex::new(());
static DOWNLOAD_TMP_COUNTER: AtomicU64 = AtomicU64::new(0);
const DOWNLOAD_TOTAL_TIMEOUT_SECS: u64 = 300;

pub(super) type LoaderDownloadTask = (String, std::path::PathBuf, Option<String>, Option<u64>);

fn loader_download_candidates(url: &str, use_mirror: bool) -> Vec<DownloadCandidate> {
    let preferred = mirror_url(url, use_mirror);
    let fallback = mirror_url(url, !use_mirror);
    let mut candidates = vec![DownloadCandidate::new(preferred.clone())];
    if fallback != preferred {
        candidates.push(DownloadCandidate::new(fallback));
    }
    candidates
}

fn loader_download_options(
    max_active_files: usize,
    max_connections_per_file: usize,
) -> DownloadEngineOptions {
    let mut options = DownloadEngineOptions::default();
    options.max_global_connections = DownloadEngineOptions::default_global_connection_limit();
    options.max_active_files = max_active_files.max(1);
    options.max_connections_per_file = max_connections_per_file.max(1);
    options
}

fn loader_download_request(
    url: &str,
    dest: &std::path::Path,
    expected_sha1: Option<&str>,
    expected_size: Option<u64>,
    use_mirror: bool,
) -> DownloadRequest {
    let request_id = DOWNLOAD_TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut request = DownloadRequest::new(
        format!("loader-download-{request_id}"),
        loader_download_candidates(url, use_mirror),
        dest,
    );
    if let Some(sha1) = expected_sha1.filter(|sha1| !sha1.trim().is_empty()) {
        request = request.with_expected_sha1(sha1.trim().to_string());
    }
    if let Some(size) = expected_size.filter(|size| *size > 0) {
        request = request.with_expected_size(size);
    }
    request
}

fn loader_download_manager(
    options: DownloadEngineOptions,
    cancel_name: Option<&str>,
) -> Result<DownloadManager, String> {
    let pool = crate::instance::install_download_pool(
        cancel_name.unwrap_or("loader-download"),
        options.max_global_connections,
    );
    DownloadManager::with_options_and_pool(options, pool)
}

fn dedupe_loader_download_tasks(tasks: Vec<LoaderDownloadTask>) -> Vec<LoaderDownloadTask> {
    let mut unique = Vec::<LoaderDownloadTask>::new();
    let mut destinations = std::collections::HashMap::<std::path::PathBuf, usize>::new();
    for (url, dest, sha1, size) in tasks {
        if let Some(index) = destinations.get(&dest).copied() {
            // Forge 配置可能在两个清单里重复声明同一库，合并校验信息后只提交一次。
            let existing = &mut unique[index];
            if existing.2.is_none() {
                existing.2 = sha1;
            }
            if existing.3.is_none() {
                existing.3 = size;
            }
            continue;
        }
        destinations.insert(dest.clone(), unique.len());
        unique.push((url, dest, sha1, size));
    }
    unique
}

fn map_loader_download_error(error: String, cancel_name: Option<&str>) -> String {
    if is_download_cancelled(cancel_name) || error.contains("cancelled") {
        "用户取消下载".to_string()
    } else {
        error
    }
}

/// 执行可取消的 JSON 请求，用户取消时会同时丢弃网络请求。
pub fn fetch_json_cancelable(
    http: &reqwest::blocking::Client,
    url: &str,
    cancel_name: &str,
    request_timeout: std::time::Duration,
) -> Result<serde_json::Value, String> {
    fetch_json_request_cancelable(
        http,
        reqwest::Method::GET,
        url,
        Vec::new(),
        None,
        cancel_name,
        request_timeout,
    )
}

fn cancellable_request_runtime() -> Result<&'static tokio::runtime::Runtime, String> {
    static RUNTIME: std::sync::OnceLock<Result<tokio::runtime::Runtime, String>> =
        std::sync::OnceLock::new();
    RUNTIME
        .get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .thread_name("oaoi-json-request")
                .enable_all()
                .build()
                .map_err(|error| format!("创建请求运行时失败: {}", error))
        })
        .as_ref()
        .map_err(Clone::clone)
}

/// 执行带请求头或 JSON 请求体的可取消请求。
pub fn fetch_json_request_cancelable(
    _http: &reqwest::blocking::Client,
    method: reqwest::Method,
    url: &str,
    headers: Vec<(String, String)>,
    body: Option<serde_json::Value>,
    cancel_name: &str,
    request_timeout: std::time::Duration,
) -> Result<serde_json::Value, String> {
    if crate::instance::is_cancelled(cancel_name) {
        return Err("用户取消安装".to_string());
    }

    let client = reqwest::Client::builder()
        .use_rustls_tls()
        .connect_timeout(request_timeout.min(std::time::Duration::from_secs(15)))
        .timeout(request_timeout)
        .user_agent("OAOI-Launcher/1.0")
        .build()
        .map_err(|error| format!("创建请求客户端失败: {}", error))?;
    let url = url.to_string();
    let cancel_name = cancel_name.to_string();
    cancellable_request_runtime()?.block_on(async move {
        let request_future = async move {
            let mut request = client.request(method, &url);
            for (name, value) in headers {
                request = request.header(name, value);
            }
            if let Some(body) = body {
                request = request.json(&body);
            }
            let response = request
                .send()
                .await
                .map_err(|error| format!("请求失败: {}", error))?
                .error_for_status()
                .map_err(|error| format!("HTTP 请求失败: {}", error))?;
            response
                .json::<serde_json::Value>()
                .await
                .map_err(|error| format!("解析响应失败: {}", error))
        };
        tokio::pin!(request_future);
        loop {
            tokio::select! {
                result = &mut request_future => return result,
                _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {
                    if crate::instance::is_cancelled(&cancel_name) {
                        return Err("用户取消安装".to_string());
                    }
                }
            }
        }
    })
}

pub fn run_java_process_cancelable(
    java_path: &str,
    classpath: &str,
    main_class: &str,
    args: &[String],
    current_dir: &std::path::Path,
    cancel_name: &str,
) -> Result<std::process::ExitStatus, String> {
    if crate::instance::is_cancelled(cancel_name) {
        return Err("用户取消安装".to_string());
    }

    let mut command = std::process::Command::new(java_path);
    command
        .arg("-cp")
        .arg(classpath)
        .arg(main_class)
        .args(args)
        .current_dir(current_dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .stdin(std::process::Stdio::null());

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }

    let mut child = command
        .spawn()
        .map_err(|e| format!("processor 启动失败: {}", e))?;

    loop {
        if crate::instance::is_cancelled(cancel_name) {
            let _ = child.kill();
            let _ = child.wait();
            return Err("用户取消安装".to_string());
        }
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(200)),
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("processor 等待失败: {}", e));
            }
        }
    }
}

/// 将官方 URL 替换为 BMCLAPI 国内镜像
pub fn mirror_url(url: &str, use_mirror: bool) -> String {
    if !use_mirror {
        return url.to_string();
    }
    url.replace(
        "https://piston-meta.mojang.com",
        "https://bmclapi2.bangbang93.com",
    )
    .replace(
        "https://piston-data.mojang.com",
        "https://bmclapi2.bangbang93.com",
    )
    .replace(
        "https://launchermeta.mojang.com",
        "https://bmclapi2.bangbang93.com",
    )
    .replace(
        "https://launcher.mojang.com",
        "https://bmclapi2.bangbang93.com",
    )
    .replace(
        "https://libraries.minecraft.net",
        "https://bmclapi2.bangbang93.com/maven",
    )
    .replace(
        "https://resources.download.minecraft.net",
        "https://bmclapi2.bangbang93.com/assets",
    )
    .replace(
        "https://maven.minecraftforge.net",
        "https://bmclapi2.bangbang93.com/maven",
    )
    .replace(
        "https://files.minecraftforge.net/maven",
        "https://bmclapi2.bangbang93.com/maven",
    )
    .replace(
        "https://maven.fabricmc.net",
        "https://bmclapi2.bangbang93.com/maven",
    )
    .replace(
        "https://maven.neoforged.net/releases",
        "https://bmclapi2.bangbang93.com/maven",
    )
    .replace(
        "https://maven.quiltmc.org/repository/release",
        "https://bmclapi2.bangbang93.com/maven",
    )
}

pub fn download_file_if_needed_cancelable(
    _http: &reqwest::blocking::Client,
    url: &str,
    dest: &std::path::Path,
    expected_sha1: Option<&str>,
    expected_size: Option<u64>,
    use_mirror: bool,
    cancel_name: Option<&str>,
) -> Result<bool, String> {
    if existing_file_ok(dest, expected_sha1, expected_size) {
        return Ok(false);
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    if is_download_cancelled(cancel_name) {
        return Err("用户取消下载".to_string());
    }

    let options = loader_download_options(1, 16);
    let manager = loader_download_manager(options, cancel_name)?;
    let _registration =
        cancel_name.map(|name| crate::instance::register_download_manager(name, &manager));
    let request = loader_download_request(url, dest, expected_sha1, expected_size, use_mirror);
    manager
        .download_one(request, |_| {})
        .map_err(|error| map_loader_download_error(error, cancel_name))?;
    Ok(true)
}

pub fn download_file_with_progress<F>(
    _http: &reqwest::blocking::Client,
    url: &str,
    dest: &std::path::Path,
    expected_sha1: Option<&str>,
    expected_size: Option<u64>,
    use_mirror: bool,
    cancel_name: Option<&str>,
    on_progress: F,
) -> Result<bool, String>
where
    F: FnMut(u64, Option<u64>) + Send + 'static,
{
    if existing_file_ok(dest, expected_sha1, expected_size) {
        let size = dest.metadata().map(|m| m.len()).unwrap_or(0);
        let mut on_progress = on_progress;
        on_progress(size, Some(size));
        return Ok(false);
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    if is_download_cancelled(cancel_name) {
        return Err("用户取消下载".to_string());
    }

    let options = loader_download_options(1, 16);
    let manager = loader_download_manager(options, cancel_name)?;
    let _registration =
        cancel_name.map(|name| crate::instance::register_download_manager(name, &manager));
    let request = loader_download_request(url, dest, expected_sha1, expected_size, use_mirror);
    let callback = std::sync::Arc::new(std::sync::Mutex::new(on_progress));
    let callback_for_event = callback.clone();
    manager
        .download_one(request, move |event| match event {
            DownloadEvent::Progress(progress) => {
                if let Ok(mut callback) = callback_for_event.lock() {
                    callback(progress.downloaded, progress.total);
                }
            }
            DownloadEvent::FileFinished(result) => {
                if let Ok(mut callback) = callback_for_event.lock() {
                    callback(result.bytes, Some(result.bytes));
                }
            }
            _ => {}
        })
        .map_err(|error| map_loader_download_error(error, cancel_name))?;
    Ok(true)
}

pub(super) fn existing_file_ok(
    dest: &std::path::Path,
    expected_sha1: Option<&str>,
    expected_size: Option<u64>,
) -> bool {
    if !dest.exists() {
        return false;
    }
    let actual_size = dest.metadata().map(|metadata| metadata.len()).ok();
    if let Some(expected_size) = expected_size.filter(|size| *size > 0) {
        if actual_size != Some(expected_size) {
            return false;
        }
    }
    match expected_sha1.filter(|sha1| !sha1.trim().is_empty()) {
        Some(sha1) => file_matches_sha1(dest, sha1),
        None => actual_size.is_some_and(|size| size > 0),
    }
}

fn file_matches_sha1(dest: &std::path::Path, expected_sha1: &str) -> bool {
    if let Ok(data) = std::fs::read(dest) {
        let hash = sha1_smol::Sha1::from(&data).digest().to_string();
        hash.eq_ignore_ascii_case(expected_sha1)
    } else {
        false
    }
}

pub(super) fn copy_local_library_if_valid(
    source: &std::path::Path,
    dest: &std::path::Path,
    expected_sha1: Option<&str>,
    expected_size: Option<u64>,
) -> Result<bool, String> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("创建依赖库目录失败 {}: {}", parent.display(), e))?;
    }
    let tmp_path = unique_temp_path(dest);
    std::fs::copy(source, &tmp_path).map_err(|e| {
        format!(
            "复制安装器内置依赖库失败 {} -> {}: {}",
            source.display(),
            tmp_path.display(),
            e
        )
    })?;
    if !existing_file_ok(&tmp_path, expected_sha1, expected_size) {
        let _ = std::fs::remove_file(&tmp_path);
        return Ok(false);
    }
    replace_downloaded_file(&tmp_path, dest)?;
    Ok(true)
}

fn is_download_cancelled(cancel_name: Option<&str>) -> bool {
    cancel_name.is_some_and(crate::instance::is_cancelled)
}

fn unique_temp_path(dest: &std::path::Path) -> std::path::PathBuf {
    let file_name = dest
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("download");
    let counter = DOWNLOAD_TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    dest.with_file_name(format!(
        ".{}.{}.{}.tmp",
        file_name,
        std::process::id(),
        counter
    ))
}

fn replace_downloaded_file(
    tmp_path: &std::path::Path,
    dest: &std::path::Path,
) -> Result<(), String> {
    crate::downloader::fs::finalize_part_file(tmp_path, dest, None, None)
        .map_err(|e| format!("替换下载文件失败: {}", e))
}

/// 限制并发的下载执行器
pub fn parallel_download(
    _http: &reqwest::blocking::Client,
    tasks: Vec<LoaderDownloadTask>,
    done: &std::sync::Arc<std::sync::atomic::AtomicUsize>,
    max_workers: usize,
    use_mirror: bool,
    cancel_name: Option<&str>,
) -> Result<(), String> {
    if tasks.is_empty() {
        return Ok(());
    }
    let requested_task_count = tasks.len();
    let tasks = dedupe_loader_download_tasks(tasks);
    if is_download_cancelled(cancel_name) {
        done.store(requested_task_count, Ordering::Relaxed);
        return Err("用户取消下载".to_string());
    }

    // Loader 依赖通常很小，保持多文件并发、单文件单连接；安装器单文件另走 16 连接配置。
    let options = loader_download_options(max_workers, 1);
    let manager = loader_download_manager(options, cancel_name).map_err(|error| {
        done.store(requested_task_count, Ordering::Relaxed);
        error
    })?;
    let _registration =
        cancel_name.map(|name| crate::instance::register_download_manager(name, &manager));
    let mut request_urls = std::collections::HashMap::new();
    let requests = tasks
        .iter()
        .map(|(url, dest, sha1, size)| {
            let request = loader_download_request(url, dest, sha1.as_deref(), *size, use_mirror);
            request_urls.insert(request.id.clone(), url.clone());
            request
        })
        .collect::<Vec<_>>();
    let completed = std::sync::Arc::new(std::sync::Mutex::new(
        std::collections::HashSet::<String>::new(),
    ));
    let completed_for_event = completed.clone();
    let done_for_event = done.clone();
    let outcomes = manager.download_many(requests, move |event| {
        let request_id = match event {
            DownloadEvent::FileFinished(result) => Some(result.request_id),
            DownloadEvent::FileFailed { request_id, .. } => Some(request_id),
            _ => None,
        };
        if let Some(request_id) = request_id {
            if completed_for_event.lock().unwrap().insert(request_id) {
                done_for_event.fetch_add(1, Ordering::Relaxed);
            }
        }
    });
    done.store(requested_task_count, Ordering::Relaxed);

    let mut errors = Vec::new();
    for outcome in outcomes {
        if let DownloadOutcome::Failed {
            request_id, error, ..
        } = outcome
        {
            let url = request_urls.get(&request_id).cloned().unwrap_or_default();
            errors.push(format!("{} -> {}", url, error));
        }
    }
    if is_download_cancelled(cancel_name) || errors.iter().any(|error| error.contains("cancelled"))
    {
        return Err("用户取消下载".to_string());
    }
    if errors.is_empty() {
        return Ok(());
    }

    let sample = errors
        .iter()
        .take(3)
        .cloned()
        .collect::<Vec<_>>()
        .join("; ");
    Err(format!("{} files failed: {}", errors.len(), sample))
}

/// 检查 library 的 rules 是否允许当前 OS
pub fn library_allowed(rules: &Option<Vec<serde_json::Value>>) -> bool {
    let rules = match rules {
        Some(r) => r,
        None => return true,
    };
    library_rules_allowed_slice(rules)
}

pub fn library_rules_value_allowed(rules: Option<&serde_json::Value>) -> bool {
    let Some(rules) = rules.and_then(|v| v.as_array()) else {
        return true;
    };
    library_rules_allowed_slice(rules)
}

fn library_rules_allowed_slice(rules: &[serde_json::Value]) -> bool {
    let has_allow = rules
        .iter()
        .any(|rule| rule.get("action").and_then(|v| v.as_str()) == Some("allow"));
    let mut allowed = !has_allow;
    for rule in rules {
        let action = rule.get("action").and_then(|v| v.as_str()).unwrap_or("");
        let os_name = rule
            .get("os")
            .and_then(|o| o.get("name"))
            .and_then(|v| v.as_str());
        if !matches!(os_name, None | Some("windows")) {
            continue;
        }
        match (action, os_name) {
            ("allow", _) => allowed = true,
            ("disallow", _) => allowed = false,
            _ => {}
        }
    }
    allowed
}

/// 用于 emit 安装进度的辅助类型
pub type EmitFn<'a> = Box<dyn Fn(&str, usize, usize, &str) + 'a>;

pub fn make_emitter<'a>(app_handle: &'a tauri::AppHandle, inst_name: &'a str) -> EmitFn<'a> {
    Box::new(
        move |stage: &str, current: usize, total: usize, detail: &str| {
            let _ = app_handle.emit("install-progress", serde_json::json!({
            "name": inst_name, "stage": stage, "current": current, "total": total, "detail": detail
        }));
        },
    )
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
    use_mirror: bool,
) -> Result<String, String> {
    let safe_name = safe_path_name(&name, "版本名")?;
    validate_loader_selection(&loader_type, &loader_version)?;
    let name_clone = safe_name.clone();
    let cancel_name = crate::instance::scoped_task_key(&game_dir, &safe_name);
    crate::instance::try_register_cancel(&cancel_name)?;
    std::thread::spawn(move || {
        eprintln!(
            "[install] 开始创建版本: {} (mc={}, loader={} {}, java={})",
            safe_name, mc_version, loader_type, loader_version, java_path
        );
        if let Err(e) = do_create_instance(
            &app_handle,
            &safe_name,
            &mc_version,
            &meta_url,
            &game_dir,
            &loader_type,
            &loader_version,
            &java_path,
            use_mirror,
            &cancel_name,
        ) {
            let user_cancelled = is_user_cancel_error(&e);
            crate::instance::unregister_cancel(&cancel_name);
            eprintln!("[install] 错误: {}", e);
            let game_root = resolve_game_dir(&game_dir);
            let inst_dir = version_dir(&game_root, &safe_name);
            let install_marker = inst_dir.join(".oaoi_installing");
            let final_json = version_json_path(&inst_dir, &safe_name);
            if install_marker.exists() && !is_complete_version_json(&final_json) {
                let _ = std::fs::remove_dir_all(&inst_dir);
                eprintln!("[install] 已清理残留目录: {}", inst_dir.display());
            } else if install_marker.exists() {
                eprintln!(
                    "[install] 安装已生成最终版本配置，保留完整实例: {}",
                    inst_dir.display()
                );
            } else if inst_dir.exists() {
                eprintln!("[install] 跳过清理非本次安装目录: {}", inst_dir.display());
            }
            let stage = if user_cancelled { "cancelled" } else { "error" };
            let _ = app_handle.emit(
                "install-progress",
                serde_json::json!({
                    "name": safe_name, "stage": stage, "current": 0, "total": 0, "detail": e
                }),
            );
        } else {
            crate::instance::unregister_cancel(&cancel_name);
        }
    });
    Ok(format!("开始创建版本: {}", name_clone))
}

fn validate_loader_selection(loader_type: &str, loader_version: &str) -> Result<(), String> {
    match loader_type {
        "vanilla" => Ok(()),
        "fabric" | "forge" | "quilt" | "neoforge" => {
            if loader_version.trim().is_empty() {
                Err(format!("{} 缺少 Loader 版本", loader_type))
            } else {
                Ok(())
            }
        }
        "" => Err("缺少 Loader 类型".to_string()),
        other => Err(format!("不支持的 Loader 类型: {}", other)),
    }
}

fn is_user_cancel_error(error: &str) -> bool {
    error.contains("用户取消")
        || error.contains("取消安装")
        || error.contains("取消下载")
        || error.contains("cancelled")
        || error.contains("download cancelled")
}

fn remove_install_marker(marker: &std::path::Path) -> Result<(), String> {
    let mut last_error = String::new();
    for _ in 0..3 {
        match std::fs::remove_file(marker) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                last_error = error.to_string();
                std::thread::sleep(std::time::Duration::from_millis(120));
            }
        }
    }

    // 删除受外部占用影响时改成完成标记，避免完整实例仍被视为安装残留。
    let completed_marker = marker.with_file_name(".oaoi_install_complete");
    std::fs::rename(marker, &completed_marker).map_err(|error| {
        format!(
            "移除安装标记失败: {}; 重命名完成标记也失败: {}",
            last_error, error
        )
    })
}

fn is_complete_version_json(path: &std::path::Path) -> bool {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|content| serde_json::from_str::<serde_json::Value>(&content).ok())
        .and_then(|json| {
            json.get("mainClass")
                .and_then(|value| value.as_str())
                .map(|value| !value.trim().is_empty())
        })
        .unwrap_or(false)
}

pub(crate) fn write_version_json_atomic(
    path: &std::path::Path,
    value: &serde_json::Value,
) -> Result<(), String> {
    use std::io::Write;

    let parent = path
        .parent()
        .ok_or_else(|| "版本配置缺少父目录".to_string())?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "版本配置文件名无效".to_string())?;
    let temporary = parent.join(format!(".{}.oaoi.tmp", file_name));
    let content = serde_json::to_vec_pretty(value)
        .map_err(|error| format!("序列化版本配置失败: {}", error))?;

    let result = (|| -> Result<(), String> {
        let mut file = std::fs::File::create(&temporary)
            .map_err(|error| format!("创建临时版本配置失败: {}", error))?;
        file.write_all(&content)
            .map_err(|error| format!("写入临时版本配置失败: {}", error))?;
        file.sync_all()
            .map_err(|error| format!("同步临时版本配置失败: {}", error))?;
        std::fs::rename(&temporary, path)
            .map_err(|error| format!("替换最终版本配置失败: {}", error))?;
        Ok(())
    })();

    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

struct LoaderInstallTask {
    handle: std::thread::JoinHandle<Result<serde_json::Value, String>>,
    aborted_downloaders: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

fn abort_vanilla_after_loader_failure(cancel_name: &str) -> bool {
    let Some(cancel_flag) = crate::instance::install_cancel_flag(cancel_name) else {
        return false;
    };
    if cancel_flag.load(Ordering::Relaxed) {
        return false;
    }

    // 内部取消保持到 Vanilla 和 Loader 都退出，确保稍后注册的下载器也会立即停止。
    crate::instance::cancel_install_task(cancel_name).is_ok()
}

fn join_loader_install(task: LoaderInstallTask) -> (Result<serde_json::Value, String>, bool) {
    let LoaderInstallTask {
        handle,
        aborted_downloaders,
    } = task;
    let result = match handle.join() {
        Ok(result) => result,
        Err(_) => Err("Loader 安装线程异常退出".to_string()),
    };
    (result, aborted_downloaders.load(Ordering::Relaxed))
}

fn clear_internal_loader_cancel(cancel_name: &str) {
    if let Some(cancel_flag) = crate::instance::install_cancel_flag(cancel_name) {
        cancel_flag.store(false, Ordering::Relaxed);
    }
}

fn spawn_loader_install(
    app_handle: &tauri::AppHandle,
    name: &str,
    mc_version: &str,
    game_dir_input: &str,
    loader_type: &str,
    loader_version: &str,
    java_path: &str,
    game_dir: &std::path::Path,
    inst_dir: &std::path::Path,
    http: &reqwest::blocking::Client,
    use_mirror: bool,
    cancel_name: &str,
) -> Option<LoaderInstallTask> {
    if loader_type == "vanilla" {
        return None;
    }
    let app_handle = app_handle.clone();
    let name = name.to_string();
    let mc_version = mc_version.to_string();
    let game_dir_input = game_dir_input.to_string();
    let loader_type = loader_type.to_string();
    let loader_version = loader_version.to_string();
    let java_path = java_path.to_string();
    let game_dir = game_dir.to_path_buf();
    let inst_dir = inst_dir.to_path_buf();
    let http = http.clone();
    let cancel_name = cancel_name.to_string();
    let aborted_downloaders = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let aborted_for_thread = aborted_downloaders.clone();

    let handle = std::thread::spawn(move || {
        let result = (|| -> Result<serde_json::Value, String> {
            let mut loader_json = empty_loader_json();
            let java_to_use = resolve_loader_java_for_install(
                &app_handle,
                &name,
                &mc_version,
                &game_dir_input,
                &loader_type,
                &java_path,
                &cancel_name,
            )?;
            match loader_type.as_str() {
                "fabric" => fabric::install_fabric_with_cancel_name(
                    &app_handle,
                    &name,
                    &cancel_name,
                    &mc_version,
                    &loader_version,
                    &game_dir,
                    &inst_dir,
                    &http,
                    use_mirror,
                    &mut loader_json,
                    true,
                )?,
                "forge" => forge::install_forge_with_task_names(
                    &app_handle,
                    &name,
                    &name,
                    &cancel_name,
                    &mc_version,
                    &loader_version,
                    &game_dir,
                    &inst_dir,
                    &http,
                    &java_to_use,
                    use_mirror,
                    &mut loader_json,
                )?,
                "quilt" => quilt::install_quilt_with_cancel_name(
                    &app_handle,
                    &name,
                    &cancel_name,
                    &mc_version,
                    &loader_version,
                    &game_dir,
                    &inst_dir,
                    &http,
                    use_mirror,
                    &mut loader_json,
                )?,
                "neoforge" => neoforge::install_neoforge_with_task_names(
                    &app_handle,
                    &name,
                    &name,
                    &cancel_name,
                    &mc_version,
                    &loader_version,
                    &game_dir,
                    &inst_dir,
                    &http,
                    &java_to_use,
                    use_mirror,
                    &mut loader_json,
                )?,
                _ => {}
            }
            Ok(loader_json)
        })();
        if let Err(error) = &result {
            if !is_user_cancel_error(error) && abort_vanilla_after_loader_failure(&cancel_name) {
                aborted_for_thread.store(true, Ordering::Relaxed);
            }
        }
        result
    });
    Some(LoaderInstallTask {
        handle,
        aborted_downloaders,
    })
}

fn resolve_loader_java_for_install(
    app_handle: &tauri::AppHandle,
    name: &str,
    mc_version: &str,
    game_dir_input: &str,
    loader_type: &str,
    java_path: &str,
    cancel_name: &str,
) -> Result<String, String> {
    if !java_path.is_empty() {
        return Ok(java_path.to_string());
    }
    if loader_type != "forge" && loader_type != "neoforge" {
        return Ok(String::new());
    }
    let emit = make_emitter(app_handle, name);
    let required_major = crate::modpack::get_required_java_major_pub(mc_version);
    let javas = crate::java_detect::find_java_blocking(Some(game_dir_input.to_string()));
    if let Some(j) = javas.iter().find(|j| j.major == required_major) {
        return Ok(j.path.clone());
    }
    if required_major == 7 {
        return Err(format!(
            "安装 {} 需要 Java 7，请先手动安装 Java 7，或在设置里选择 Java 7 的 java.exe",
            loader_type
        ));
    }
    emit(
        "java",
        0,
        1,
        &format!("自动下载 Java {}...", required_major),
    );
    crate::java_download::download_java_sync_cancelable(
        required_major,
        game_dir_input,
        Some(cancel_name),
    )
    .map_err(|e| {
        format!(
            "安装 {} 需要 Java {}，自动下载失败: {}",
            loader_type, required_major, e
        )
    })
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
    use_mirror: bool,
    cancel_name: &str,
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
        return Err(format!("版本名 '{}' 包含非法字符", name));
    }
    let game_dir = resolve_game_dir(game_dir_input);
    let emit = make_emitter(app_handle, name);
    if crate::instance::is_cancelled(cancel_name) {
        return Err("用户取消安装".to_string());
    }

    let inst_dir = version_dir(&game_dir, name);
    if inst_dir.exists() {
        return Err(format!("版本 '{}' 已存在，请换一个名称！", name));
    }
    std::fs::create_dir_all(&inst_dir).map_err(|e| e.to_string())?;
    let install_marker_path = inst_dir.join(".oaoi_installing");
    if let Err(error) = std::fs::write(
        &install_marker_path,
        format!("pid={}\nversion={}\n", std::process::id(), mc_version),
    ) {
        let cleanup_result = std::fs::remove_dir_all(&inst_dir);
        return Err(match cleanup_result {
            Ok(()) => format!("创建安装标记失败: {}", error),
            Err(cleanup_error) => format!(
                "创建安装标记失败: {}; 清理新版本目录失败: {}",
                error, cleanup_error
            ),
        });
    }
    let inst_json_path = version_json_path(&inst_dir, name);

    let http = reqwest::blocking::Client::builder()
        .pool_max_idle_per_host(64)
        .connect_timeout(std::time::Duration::from_secs(15))
        .timeout(std::time::Duration::from_secs(DOWNLOAD_TOTAL_TIMEOUT_SECS))
        .user_agent("OAOI-Launcher/1.0")
        .build()
        .map_err(|e| e.to_string())?;

    let loader_handle = spawn_loader_install(
        app_handle,
        name,
        mc_version,
        game_dir_input,
        loader_type,
        loader_version,
        java_path,
        &game_dir,
        &inst_dir,
        &http,
        use_mirror,
        cancel_name,
    );

    // 下载 vanilla 基础（版本 jar + libraries + assets），内部也会并行跑
    let vanilla_result = vanilla::install_vanilla_with_task_names(
        app_handle,
        name,
        name,
        cancel_name,
        mc_version,
        meta_url,
        &game_dir,
        &inst_dir,
        &http,
        use_mirror,
    );
    let mut ver_json = match vanilla_result {
        Ok(ver_json) => ver_json,
        Err(vanilla_error) => {
            if !crate::instance::is_cancelled(cancel_name) {
                let _ = crate::instance::cancel_install_task(cancel_name);
            }
            if let Some(task) = loader_handle {
                let (loader_result, internally_aborted) = join_loader_install(task);
                if internally_aborted {
                    clear_internal_loader_cancel(cancel_name);
                    return match loader_result {
                        Err(error) => Err(error),
                        Ok(_) => Err("Loader 失败后未返回错误信息".to_string()),
                    };
                }
            }
            return Err(vanilla_error);
        }
    };

    if let Some(task) = loader_handle {
        let (loader_result, internally_aborted) = join_loader_install(task);
        if internally_aborted {
            clear_internal_loader_cancel(cancel_name);
        }
        let loader_json = loader_result?;
        merge_loader_install_result(&mut ver_json, &loader_json);
    }
    if crate::instance::is_cancelled(cancel_name) {
        return Err("用户取消安装".to_string());
    }
    crate::instance::set_minecraft_language(&inst_dir, "zh_cn")?;

    // 写回最终版本 JSON
    strip_launcher_private_version_fields(&mut ver_json);
    write_version_json_atomic(&inst_json_path, &ver_json)
        .map_err(|error| format!("保存版本配置失败: {}", error))?;

    remove_install_marker(&install_marker_path)
        .map_err(|error| format!("安装已完成，但{}", error))?;
    emit("done", 1, 1, &format!("版本 '{}' 创建完成！", name));
    Ok(format!("版本 {} 创建成功", name))
}
