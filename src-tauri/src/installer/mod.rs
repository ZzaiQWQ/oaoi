pub mod fabric;
pub mod forge;
pub mod neoforge;
pub mod quilt;
pub mod vanilla;

// ===== 公共工具函数（供 forge/neoforge 共用） =====

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
    ver_json_path: &std::path::Path,
    installer_path: &std::path::Path,
    temp_dir: &std::path::Path,
    _mc_version: &str,
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
    map.insert(
        "MINECRAFT_VERSION".to_string(),
        ver_json_path.to_string_lossy().to_string(),
    );
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
        "libraries": [],
        "arguments": {
            "jvm": [],
            "game": []
        }
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

use crate::instance::{resolve_game_dir, safe_join, safe_path_name};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use tauri::Emitter;

/// Forge / NeoForge 安装器全局锁 — 同一时间只能运行一个安装
pub static FORGE_LOCK: Mutex<()> = Mutex::new(());
static DOWNLOAD_TMP_COUNTER: AtomicU64 = AtomicU64::new(0);
const PARALLEL_DOWNLOAD_MIN_BYTES: u64 = 2 * 1024 * 1024;
const PARALLEL_DOWNLOAD_MIN_PART_BYTES: u64 = 512 * 1024;
const PARALLEL_DOWNLOAD_WORKERS: usize = 16;
const PARALLEL_DOWNLOAD_PROBE_TIMEOUT_SECS: u64 = 15;
const PARALLEL_DOWNLOAD_PART_STALL_TIMEOUT_SECS: u64 = 15;
const PARALLEL_DOWNLOAD_PART_RETRY_DELAY_SECS: u64 = 15;
const DOWNLOAD_TOTAL_TIMEOUT_SECS: u64 = 300;
const DOWNLOAD_SLOW_SAMPLE_SECS: u64 = 15;
const DOWNLOAD_SLOW_MIN_FILE_BYTES: u64 = 2 * 1024 * 1024;
const DOWNLOAD_SLOW_MIN_BYTES_PER_SEC: u64 = 32 * 1024;

fn download_retry_delay_secs(last_err: &str, attempt: usize) -> u64 {
    let lower = last_err.to_ascii_lowercase();
    if last_err.contains("429")
        || last_err.contains("range part")
        || last_err.contains("超时")
        || last_err.contains("下载过慢")
        || lower.contains("timeout")
        || lower.contains("timed out")
        || lower.contains("deadline")
        || lower.contains("too slow")
    {
        15
    } else {
        1 << (attempt - 1)
    }
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
    http: &reqwest::blocking::Client,
    url: &str,
    dest: &std::path::Path,
    expected_sha1: Option<&str>,
    use_mirror: bool,
    cancel_name: Option<&str>,
) -> Result<bool, String> {
    if existing_file_ok(dest, expected_sha1) {
        return Ok(false);
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    let real_url = mirror_url(url, use_mirror);
    let max_retries = 5;
    let mut last_err = String::new();
    for attempt in 0..max_retries {
        if attempt > 0 {
            let wait_secs = download_retry_delay_secs(&last_err, attempt);
            std::thread::sleep(std::time::Duration::from_secs(wait_secs));
        }
        if is_download_cancelled(cancel_name) {
            return Err("用户取消下载".to_string());
        }
        match do_download(http, &real_url, dest, cancel_name) {
            Ok(()) => match verify_downloaded_file(dest, expected_sha1) {
                Ok(()) => return Ok(true),
                Err(e) => {
                    last_err = e;
                    eprintln!(
                        "[download] 重试 {}/{}: {} ({})",
                        attempt + 1,
                        max_retries,
                        last_err,
                        real_url
                    );
                }
            },
            Err(e) => {
                last_err = e;
                eprintln!(
                    "[download] 重试 {}/{}: {} ({})",
                    attempt + 1,
                    max_retries,
                    last_err,
                    real_url
                );
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
                    let wait_secs = download_retry_delay_secs(&last_err, attempt);
                    std::thread::sleep(std::time::Duration::from_secs(wait_secs));
                }
                if is_download_cancelled(cancel_name) {
                    return Err("用户取消下载".to_string());
                }
                match do_download(http, &fallback_url, dest, cancel_name) {
                    Ok(()) => match verify_downloaded_file(dest, expected_sha1) {
                        Ok(()) => return Ok(true),
                        Err(e) => {
                            last_err = e;
                            eprintln!(
                                "[download] 镜像重试 {}/3: {} ({})",
                                attempt + 1,
                                last_err,
                                fallback_url
                            );
                        }
                    },
                    Err(e) => {
                        last_err = e;
                        eprintln!(
                            "[download] 镜像重试 {}/3: {} ({})",
                            attempt + 1,
                            last_err,
                            fallback_url
                        );
                    }
                }
            }
        }
    }

    Err(format!("下载失败(重试后): {} ({})", last_err, real_url))
}

pub fn download_file_exact_once_with_stall_timeout(
    url: &str,
    dest: &std::path::Path,
    expected_sha1: Option<&str>,
    cancel_name: Option<&str>,
    stall_timeout_secs: u64,
    total_timeout_secs: u64,
) -> Result<bool, String> {
    if existing_file_ok(dest, expected_sha1) {
        return Ok(false);
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    if is_download_cancelled(cancel_name) {
        return Err("用户取消下载".to_string());
    }

    let tmp_path = unique_temp_path(dest);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("创建下载运行时失败: {}", e))?;

    let result = runtime.block_on(async {
        let stall_timeout = std::time::Duration::from_secs(stall_timeout_secs);
        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(15))
            .timeout(std::time::Duration::from_secs(total_timeout_secs))
            .user_agent("OAOI-Launcher/1.0")
            .build()
            .map_err(|e| format!("创建下载客户端失败: {}", e))?;
        let send = client.get(url).send();
        let mut resp = tokio::time::timeout(stall_timeout, send)
            .await
            .map_err(|_| format!("请求超时: {} 秒无响应", stall_timeout_secs))?
            .map_err(|e| format!("请求失败: {}", e))?;
        if !resp.status().is_success() {
            return Err(format!("HTTP {}", resp.status()));
        }

        let total = resp.content_length();
        let mut file =
            std::fs::File::create(&tmp_path).map_err(|e| format!("创建临时文件失败: {}", e))?;
        let mut downloaded = 0u64;
        let check_slow_speed = total
            .map(|size| size >= DOWNLOAD_SLOW_MIN_FILE_BYTES)
            .unwrap_or(true);
        let mut speed_window_start = std::time::Instant::now();
        let mut speed_window_bytes = 0u64;
        loop {
            if is_download_cancelled(cancel_name) {
                return Err("用户取消下载".to_string());
            }
            let chunk = tokio::time::timeout(stall_timeout, resp.chunk())
                .await
                .map_err(|_| format!("下载卡住: {} 秒没有新数据", stall_timeout_secs))?
                .map_err(|e| format!("读取失败: {}", e))?;
            let Some(chunk) = chunk else {
                break;
            };
            if chunk.is_empty() {
                continue;
            }
            std::io::Write::write_all(&mut file, chunk.as_ref())
                .map_err(|e| format!("写入失败: {}", e))?;
            downloaded += chunk.len() as u64;
            if check_slow_speed {
                speed_window_bytes += chunk.len() as u64;
                let elapsed = speed_window_start.elapsed();
                if elapsed >= std::time::Duration::from_secs(DOWNLOAD_SLOW_SAMPLE_SECS) {
                    let seconds = elapsed.as_secs().max(1);
                    let bytes_per_sec = speed_window_bytes / seconds;
                    if bytes_per_sec < DOWNLOAD_SLOW_MIN_BYTES_PER_SEC {
                        return Err(format!(
                            "下载过慢: {} B/s 低于 {} B/s",
                            bytes_per_sec, DOWNLOAD_SLOW_MIN_BYTES_PER_SEC
                        ));
                    }
                    speed_window_start = std::time::Instant::now();
                    speed_window_bytes = 0;
                }
            }
        }
        if let Some(total) = total {
            if downloaded != total {
                return Err(format!("下载不完整: {} / {}", downloaded, total));
            }
        }
        Ok(())
    });

    if let Err(err) = result {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(err);
    }

    replace_downloaded_file(&tmp_path, dest)?;
    verify_downloaded_file(dest, expected_sha1)?;
    Ok(true)
}

pub fn download_file_with_progress<F>(
    http: &reqwest::blocking::Client,
    url: &str,
    dest: &std::path::Path,
    expected_sha1: Option<&str>,
    use_mirror: bool,
    cancel_name: Option<&str>,
    mut on_progress: F,
) -> Result<bool, String>
where
    F: FnMut(u64, Option<u64>),
{
    if existing_file_ok(dest, expected_sha1) {
        let size = dest.metadata().map(|m| m.len()).unwrap_or(0);
        on_progress(size, Some(size));
        return Ok(false);
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    let real_url = mirror_url(url, use_mirror);
    let max_retries = 5;
    let mut last_err = String::new();
    let mut allow_parallel = true;
    for attempt in 0..max_retries {
        if attempt > 0 {
            let wait_secs = download_retry_delay_secs(&last_err, attempt);
            std::thread::sleep(std::time::Duration::from_secs(wait_secs));
        }
        if is_download_cancelled(cancel_name) {
            return Err("用户取消下载".to_string());
        }
        let download_result = if allow_parallel {
            match do_parallel_download_with_progress(
                http,
                &real_url,
                dest,
                cancel_name,
                &mut on_progress,
            ) {
                Ok(()) => Ok(()),
                Err(ParallelDownloadError::Unsupported(reason)) => {
                    allow_parallel = false;
                    eprintln!(
                        "[download] parallel unsupported, fallback: {} ({})",
                        reason, real_url
                    );
                    do_download_with_progress(http, &real_url, dest, cancel_name, &mut on_progress)
                }
                Err(ParallelDownloadError::Failed(e)) if e.contains("用户取消") => Err(e),
                Err(ParallelDownloadError::Failed(e)) => {
                    allow_parallel = false;
                    eprintln!(
                        "[download] parallel failed after part retry, fallback streaming: {} ({})",
                        e, real_url
                    );
                    do_download_with_progress(http, &real_url, dest, cancel_name, &mut on_progress)
                }
            }
        } else {
            do_download_with_progress(http, &real_url, dest, cancel_name, &mut on_progress)
        };
        match download_result {
            Ok(()) => match verify_downloaded_file(dest, expected_sha1) {
                Ok(()) => return Ok(true),
                Err(e) => {
                    last_err = e;
                    eprintln!(
                        "[download] 重试 {}/{}: {} ({})",
                        attempt + 1,
                        max_retries,
                        last_err,
                        real_url
                    );
                }
            },
            Err(e) => {
                last_err = e;
                eprintln!(
                    "[download] 重试 {}/{}: {} ({})",
                    attempt + 1,
                    max_retries,
                    last_err,
                    real_url
                );
            }
        }
    }

    if !use_mirror {
        let fallback_url = mirror_url(url, true);
        if fallback_url != real_url {
            eprintln!("[download] 官方源失败，回退镜像: {}", fallback_url);
            let mut allow_parallel = true;
            for attempt in 0..3 {
                if attempt > 0 {
                    let wait_secs = download_retry_delay_secs(&last_err, attempt);
                    std::thread::sleep(std::time::Duration::from_secs(wait_secs));
                }
                if is_download_cancelled(cancel_name) {
                    return Err("用户取消下载".to_string());
                }
                let download_result = if allow_parallel {
                    match do_parallel_download_with_progress(
                        http,
                        &fallback_url,
                        dest,
                        cancel_name,
                        &mut on_progress,
                    ) {
                        Ok(()) => Ok(()),
                        Err(ParallelDownloadError::Unsupported(reason)) => {
                            allow_parallel = false;
                            eprintln!(
                                "[download] parallel unsupported, fallback: {} ({})",
                                reason, fallback_url
                            );
                            do_download_with_progress(
                                http,
                                &fallback_url,
                                dest,
                                cancel_name,
                                &mut on_progress,
                            )
                        }
                        Err(ParallelDownloadError::Failed(e)) if e.contains("用户取消") => {
                            Err(e)
                        }
                        Err(ParallelDownloadError::Failed(e)) => {
                            allow_parallel = false;
                            eprintln!(
                                "[download] parallel failed after part retry, fallback streaming: {} ({})",
                                e, fallback_url
                            );
                            do_download_with_progress(
                                http,
                                &fallback_url,
                                dest,
                                cancel_name,
                                &mut on_progress,
                            )
                        }
                    }
                } else {
                    do_download_with_progress(
                        http,
                        &fallback_url,
                        dest,
                        cancel_name,
                        &mut on_progress,
                    )
                };
                match download_result {
                    Ok(()) => match verify_downloaded_file(dest, expected_sha1) {
                        Ok(()) => return Ok(true),
                        Err(e) => {
                            last_err = e;
                            eprintln!(
                                "[download] 镜像重试 {}/3: {} ({})",
                                attempt + 1,
                                last_err,
                                fallback_url
                            );
                        }
                    },
                    Err(e) => {
                        last_err = e;
                        eprintln!(
                            "[download] 镜像重试 {}/3: {} ({})",
                            attempt + 1,
                            last_err,
                            fallback_url
                        );
                    }
                }
            }
        }
    }

    Err(format!("下载失败(重试后): {} ({})", last_err, real_url))
}

pub fn download_file_mirror_then_official_with_progress<F>(
    http: &reqwest::blocking::Client,
    url: &str,
    dest: &std::path::Path,
    expected_sha1: Option<&str>,
    cancel_name: Option<&str>,
    mut on_progress: F,
) -> Result<bool, String>
where
    F: FnMut(u64, Option<u64>),
{
    if existing_file_ok(dest, expected_sha1) {
        let size = dest.metadata().map(|m| m.len()).unwrap_or(0);
        on_progress(size, Some(size));
        return Ok(false);
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    let mirror = mirror_url(url, true);
    let official = mirror_url(url, false);
    let mut last_err = String::new();

    match download_exact_with_progress(
        http,
        &mirror,
        dest,
        expected_sha1,
        cancel_name,
        2,
        "镜像",
        &mut last_err,
        &mut on_progress,
    ) {
        Ok(()) => return Ok(true),
        Err(e) if e.contains("用户取消") => return Err(e),
        Err(_) => {}
    }

    if official != mirror {
        eprintln!("[download] 镜像源失败 2 次，回退官方源: {}", official);
        match download_exact_with_progress(
            http,
            &official,
            dest,
            expected_sha1,
            cancel_name,
            3,
            "官方",
            &mut last_err,
            &mut on_progress,
        ) {
            Ok(()) => return Ok(true),
            Err(e) if e.contains("用户取消") => return Err(e),
            Err(_) => {}
        }
    }

    Err(format!("下载失败(镜像2次+官方后): {}", last_err))
}

pub fn download_file_mirror_then_official(
    http: &reqwest::blocking::Client,
    url: &str,
    dest: &std::path::Path,
    expected_sha1: Option<&str>,
    cancel_name: Option<&str>,
) -> Result<bool, String> {
    if existing_file_ok(dest, expected_sha1) {
        return Ok(false);
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    let mirror = mirror_url(url, true);
    let official = mirror_url(url, false);
    let mut last_err = String::new();

    match download_exact(
        http,
        &mirror,
        dest,
        expected_sha1,
        cancel_name,
        2,
        "镜像",
        &mut last_err,
    ) {
        Ok(()) => return Ok(true),
        Err(e) if e.contains("用户取消") => return Err(e),
        Err(_) => {}
    }

    if official != mirror {
        eprintln!("[download] 镜像源失败 2 次，回退官方源: {}", official);
        match download_exact(
            http,
            &official,
            dest,
            expected_sha1,
            cancel_name,
            3,
            "官方",
            &mut last_err,
        ) {
            Ok(()) => return Ok(true),
            Err(e) if e.contains("用户取消") => return Err(e),
            Err(_) => {}
        }
    }

    Err(format!("下载失败(镜像2次+官方后): {}", last_err))
}

fn download_exact_with_progress<F>(
    http: &reqwest::blocking::Client,
    exact_url: &str,
    dest: &std::path::Path,
    expected_sha1: Option<&str>,
    cancel_name: Option<&str>,
    max_retries: usize,
    label: &str,
    last_err: &mut String,
    on_progress: &mut F,
) -> Result<(), String>
where
    F: FnMut(u64, Option<u64>),
{
    let mut allow_parallel = true;
    for attempt in 0..max_retries {
        if attempt > 0 {
            let wait_secs = download_retry_delay_secs(last_err, attempt);
            std::thread::sleep(std::time::Duration::from_secs(wait_secs));
        }
        if is_download_cancelled(cancel_name) {
            return Err("用户取消下载".to_string());
        }

        let download_result = if allow_parallel {
            match do_parallel_download_with_progress(
                http,
                exact_url,
                dest,
                cancel_name,
                on_progress,
            ) {
                Ok(()) => Ok(()),
                Err(ParallelDownloadError::Unsupported(reason)) => {
                    allow_parallel = false;
                    eprintln!(
                        "[download] parallel unsupported, fallback: {} ({})",
                        reason, exact_url
                    );
                    do_download_with_progress(http, exact_url, dest, cancel_name, on_progress)
                }
                Err(ParallelDownloadError::Failed(e)) if e.contains("用户取消") => Err(e),
                Err(ParallelDownloadError::Failed(e)) => {
                    allow_parallel = false;
                    eprintln!(
                        "[download] parallel failed after part retry, fallback streaming: {} ({})",
                        e, exact_url
                    );
                    do_download_with_progress(http, exact_url, dest, cancel_name, on_progress)
                }
            }
        } else {
            do_download_with_progress(http, exact_url, dest, cancel_name, on_progress)
        };

        match download_result {
            Ok(()) => match verify_downloaded_file(dest, expected_sha1) {
                Ok(()) => return Ok(()),
                Err(e) => *last_err = format!("{} ({})", e, exact_url),
            },
            Err(e) => *last_err = format!("{} ({})", e, exact_url),
        }

        eprintln!(
            "[download] {}重试 {}/{}: {}",
            label,
            attempt + 1,
            max_retries,
            last_err
        );
    }

    Err(last_err.clone())
}

fn download_exact(
    http: &reqwest::blocking::Client,
    exact_url: &str,
    dest: &std::path::Path,
    expected_sha1: Option<&str>,
    cancel_name: Option<&str>,
    max_retries: usize,
    label: &str,
    last_err: &mut String,
) -> Result<(), String> {
    for attempt in 0..max_retries {
        if attempt > 0 {
            let wait_secs = download_retry_delay_secs(last_err, attempt);
            std::thread::sleep(std::time::Duration::from_secs(wait_secs));
        }
        if is_download_cancelled(cancel_name) {
            return Err("用户取消下载".to_string());
        }

        match do_download(http, exact_url, dest, cancel_name) {
            Ok(()) => match verify_downloaded_file(dest, expected_sha1) {
                Ok(()) => return Ok(()),
                Err(e) => *last_err = format!("{} ({})", e, exact_url),
            },
            Err(e) => *last_err = format!("{} ({})", e, exact_url),
        }

        eprintln!(
            "[download] {}重试 {}/{}: {}",
            label,
            attempt + 1,
            max_retries,
            last_err
        );
    }

    Err(last_err.clone())
}

fn existing_file_ok(dest: &std::path::Path, expected_sha1: Option<&str>) -> bool {
    if !dest.exists() {
        return false;
    }
    match expected_sha1 {
        Some(sha1) => file_matches_sha1(dest, sha1),
        None => dest.metadata().map(|m| m.len() > 0).unwrap_or(false),
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

fn verify_downloaded_file(
    dest: &std::path::Path,
    expected_sha1: Option<&str>,
) -> Result<(), String> {
    if let Some(sha1) = expected_sha1 {
        if !file_matches_sha1(dest, sha1) {
            let _ = std::fs::remove_file(dest);
            return Err("sha1 校验失败".to_string());
        }
    }
    Ok(())
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

fn unique_temp_part_path(dest: &std::path::Path, part: usize) -> std::path::PathBuf {
    let file_name = dest
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("download");
    let counter = DOWNLOAD_TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    dest.with_file_name(format!(
        ".{}.{}.{}.part{}",
        file_name,
        std::process::id(),
        counter,
        part
    ))
}

fn replace_downloaded_file(
    tmp_path: &std::path::Path,
    dest: &std::path::Path,
) -> Result<(), String> {
    if dest.exists() {
        std::fs::remove_file(dest).map_err(|e| format!("删除旧文件失败: {}", e))?;
    }
    std::fs::rename(tmp_path, dest).map_err(|e| format!("重命名失败: {}", e))
}

/// 实际执行单次下载（流式写入，不一次性读到内存）
fn do_download(
    http: &reqwest::blocking::Client,
    url: &str,
    dest: &std::path::Path,
    cancel_name: Option<&str>,
) -> Result<(), String> {
    do_download_with_progress(http, url, dest, cancel_name, &mut |_, _| {})
}

enum ParallelDownloadError {
    Unsupported(String),
    Failed(String),
}

fn probe_range_size(
    http: &reqwest::blocking::Client,
    url: &str,
    cancel_name: Option<&str>,
) -> Result<(u64, String), ParallelDownloadError> {
    if is_download_cancelled(cancel_name) {
        return Err(ParallelDownloadError::Failed("用户取消下载".to_string()));
    }
    let resp = http
        .get(url)
        .header(reqwest::header::RANGE, "bytes=0-0")
        .timeout(std::time::Duration::from_secs(
            PARALLEL_DOWNLOAD_PROBE_TIMEOUT_SECS,
        ))
        .send()
        .map_err(|e| ParallelDownloadError::Unsupported(format!("probe failed: {}", e)))?;
    let status = resp.status();
    let resolved_url = resp.url().to_string();
    if status != reqwest::StatusCode::PARTIAL_CONTENT {
        return Err(ParallelDownloadError::Unsupported(format!(
            "range status {}",
            status
        )));
    }
    let content_range = resp
        .headers()
        .get(reqwest::header::CONTENT_RANGE)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| ParallelDownloadError::Unsupported("missing Content-Range".to_string()))?;
    let total = content_range
        .rsplit_once('/')
        .and_then(|(_, total)| total.parse::<u64>().ok())
        .ok_or_else(|| {
            ParallelDownloadError::Unsupported(format!("invalid Content-Range: {}", content_range))
        })?;
    if total < PARALLEL_DOWNLOAD_MIN_BYTES {
        return Err(ParallelDownloadError::Unsupported(format!(
            "file too small: {} bytes",
            total
        )));
    }
    Ok((total, resolved_url))
}

fn download_range_part_with_stall_timeout(
    url: &str,
    part_path: &std::path::Path,
    start: u64,
    end: u64,
    expected_len: u64,
    part_progress: &std::sync::atomic::AtomicU64,
    cancel_name: Option<&str>,
) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("创建下载运行时失败: {}", e))?;
    runtime.block_on(async {
        if cancel_name.is_some_and(crate::instance::is_cancelled) {
            return Err("用户取消下载".to_string());
        }
        if let Some(parent) = part_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(15))
            .timeout(std::time::Duration::from_secs(DOWNLOAD_TOTAL_TIMEOUT_SECS))
            .user_agent("OAOI-Launcher/1.0")
            .build()
            .map_err(|e| format!("创建下载客户端失败: {}", e))?;
        let stall_timeout =
            std::time::Duration::from_secs(PARALLEL_DOWNLOAD_PART_STALL_TIMEOUT_SECS);
        let send = client
            .get(url)
            .header(reqwest::header::RANGE, format!("bytes={}-{}", start, end))
            .send();
        let mut resp = tokio::time::timeout(stall_timeout, send)
            .await
            .map_err(|_| {
                format!(
                    "分片请求超时: {} 秒无响应",
                    PARALLEL_DOWNLOAD_PART_STALL_TIMEOUT_SECS
                )
            })?
            .map_err(|e| format!("请求失败: {}", e))?;
        if resp.status() != reqwest::StatusCode::PARTIAL_CONTENT {
            return Err(format!("range HTTP {}", resp.status()));
        }

        let mut file =
            std::fs::File::create(part_path).map_err(|e| format!("创建分片失败: {}", e))?;
        let mut downloaded = 0u64;
        loop {
            if cancel_name.is_some_and(crate::instance::is_cancelled) {
                return Err("用户取消下载".to_string());
            }
            let chunk = tokio::time::timeout(stall_timeout, resp.chunk())
                .await
                .map_err(|_| {
                    format!(
                        "分片下载卡住: {} 秒没有新数据",
                        PARALLEL_DOWNLOAD_PART_STALL_TIMEOUT_SECS
                    )
                })?
                .map_err(|e| format!("读取失败: {}", e))?;
            let Some(chunk) = chunk else {
                break;
            };
            if chunk.is_empty() {
                continue;
            }
            std::io::Write::write_all(&mut file, chunk.as_ref())
                .map_err(|e| format!("写入失败: {}", e))?;
            downloaded += chunk.len() as u64;
            part_progress.store(downloaded, Ordering::Relaxed);
        }
        if downloaded != expected_len {
            return Err(format!(
                "分片大小不匹配: got {}, expected {}",
                downloaded, expected_len
            ));
        }
        Ok(())
    })
}

fn do_parallel_download_with_progress<F>(
    http: &reqwest::blocking::Client,
    url: &str,
    dest: &std::path::Path,
    cancel_name: Option<&str>,
    on_progress: &mut F,
) -> Result<(), ParallelDownloadError>
where
    F: FnMut(u64, Option<u64>),
{
    let (total, resolved_url) = probe_range_size(http, url, cancel_name)?;
    let workers = PARALLEL_DOWNLOAD_WORKERS
        .min(
            ((total + PARALLEL_DOWNLOAD_MIN_PART_BYTES - 1) / PARALLEL_DOWNLOAD_MIN_PART_BYTES)
                as usize,
        )
        .max(2);
    let part_size = (total + workers as u64 - 1) / workers as u64;
    let progress = std::sync::Arc::new(
        (0..workers)
            .map(|_| std::sync::atomic::AtomicU64::new(0))
            .collect::<Vec<_>>(),
    );
    let errors = std::sync::Arc::new(Mutex::new(Vec::<String>::new()));
    let cancel_name = cancel_name.map(|name| name.to_string());
    let part_paths = (0..workers)
        .map(|part| unique_temp_part_path(dest, part))
        .collect::<Vec<_>>();

    on_progress(0, Some(total));
    eprintln!(
        "[download] parallel range start: {} workers, {} bytes ({})",
        workers, total, resolved_url
    );

    let handles = (0..workers)
        .map(|part| {
            let url = resolved_url.clone();
            let part_path = part_paths[part].clone();
            let progress = progress.clone();
            let errors = errors.clone();
            let cancel_name = cancel_name.clone();
            std::thread::spawn(move || {
                let start = part as u64 * part_size;
                let end = ((start + part_size).min(total)).saturating_sub(1);
                let expected_len = end.saturating_sub(start) + 1;
                let mut result = Ok(());
                for attempt in 0..2 {
                    if attempt > 0 {
                        std::thread::sleep(std::time::Duration::from_secs(
                            PARALLEL_DOWNLOAD_PART_RETRY_DELAY_SECS,
                        ));
                    }
                    let attempt_result = download_range_part_with_stall_timeout(
                        &url,
                        &part_path,
                        start,
                        end,
                        expected_len,
                        &progress[part],
                        cancel_name.as_deref(),
                    );
                    match attempt_result {
                        Ok(()) => {
                            result = Ok(());
                            break;
                        }
                        Err(e) if e.contains("用户取消") => {
                            result = Err(e);
                            break;
                        }
                        Err(e) => {
                            progress[part].store(0, Ordering::Relaxed);
                            let _ = std::fs::remove_file(&part_path);
                            eprintln!(
                                "[download] range part retry {}/2: part {} bytes {}-{}: {}",
                                attempt + 1,
                                part,
                                start,
                                end,
                                e
                            );
                            result = Err(e);
                        }
                    }
                }
                if let Err(e) = result {
                    let _ = std::fs::remove_file(&part_path);
                    errors.lock().unwrap().push(format!(
                        "range part {} bytes {}-{} failed: {}",
                        part, start, end, e
                    ));
                }
            })
        })
        .collect::<Vec<_>>();

    let mut last_emit = 0u64;
    loop {
        let downloaded = progress
            .iter()
            .map(|part| part.load(Ordering::Relaxed))
            .sum::<u64>()
            .min(total);
        if downloaded == total
            || downloaded.saturating_sub(last_emit) >= 512 * 1024
            || is_download_cancelled(cancel_name.as_deref())
        {
            on_progress(downloaded, Some(total));
            last_emit = downloaded;
        }
        if handles.iter().all(|handle| handle.is_finished()) {
            break;
        }
        if is_download_cancelled(cancel_name.as_deref()) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(120));
    }

    for handle in handles {
        if handle.join().is_err() {
            errors
                .lock()
                .unwrap()
                .push("download worker panicked".to_string());
        }
    }
    if is_download_cancelled(cancel_name.as_deref()) {
        for path in &part_paths {
            let _ = std::fs::remove_file(path);
        }
        return Err(ParallelDownloadError::Failed("用户取消下载".to_string()));
    }
    let errors = errors.lock().unwrap();
    if !errors.is_empty() {
        for path in &part_paths {
            let _ = std::fs::remove_file(path);
        }
        return Err(ParallelDownloadError::Failed(errors.join("; ")));
    }
    drop(errors);

    let tmp_path = unique_temp_path(dest);
    let merge_result = (|| -> Result<(), String> {
        let mut out =
            std::fs::File::create(&tmp_path).map_err(|e| format!("创建合并文件失败: {}", e))?;
        for path in &part_paths {
            let mut part_file =
                std::fs::File::open(path).map_err(|e| format!("读取分片失败: {}", e))?;
            std::io::copy(&mut part_file, &mut out).map_err(|e| format!("合并分片失败: {}", e))?;
        }
        replace_downloaded_file(&tmp_path, dest)?;
        Ok(())
    })();
    for path in &part_paths {
        let _ = std::fs::remove_file(path);
    }
    if let Err(e) = merge_result {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(ParallelDownloadError::Failed(e));
    }
    on_progress(total, Some(total));
    Ok(())
}

fn do_download_with_progress<F>(
    http: &reqwest::blocking::Client,
    url: &str,
    dest: &std::path::Path,
    cancel_name: Option<&str>,
    on_progress: &mut F,
) -> Result<(), String>
where
    F: FnMut(u64, Option<u64>),
{
    if is_download_cancelled(cancel_name) {
        return Err("用户取消下载".to_string());
    }
    let mut resp = http
        .get(url)
        .send()
        .map_err(|e| format!("请求失败: {}", e))?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }

    let total = resp.content_length();
    on_progress(0, total);
    let tmp_path = unique_temp_path(dest);
    let result = (|| -> Result<(), String> {
        let mut file =
            std::fs::File::create(&tmp_path).map_err(|e| format!("创建临时文件失败: {}", e))?;
        let mut buf = [0u8; 64 * 1024];
        let mut downloaded = 0u64;
        let mut last_emit_bytes = 0u64;
        let mut last_emit_at = std::time::Instant::now();

        loop {
            if is_download_cancelled(cancel_name) {
                return Err("用户取消下载".to_string());
            }
            let read =
                std::io::Read::read(&mut resp, &mut buf).map_err(|e| format!("读取失败: {}", e))?;
            if read == 0 {
                break;
            }
            std::io::Write::write_all(&mut file, &buf[..read])
                .map_err(|e| format!("写入失败: {}", e))?;
            downloaded += read as u64;

            let should_emit = downloaded == total.unwrap_or(downloaded)
                || downloaded.saturating_sub(last_emit_bytes) >= 512 * 1024
                || last_emit_at.elapsed() >= std::time::Duration::from_millis(250);
            if should_emit {
                on_progress(downloaded, total);
                last_emit_bytes = downloaded;
                last_emit_at = std::time::Instant::now();
            }
        }
        on_progress(downloaded, total);
        if let Some(total) = total {
            if downloaded != total {
                return Err(format!("下载不完整: {} / {}", downloaded, total));
            }
        }
        Ok(())
    })();

    if let Err(err) = result {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(err);
    }

    replace_downloaded_file(&tmp_path, dest)?;
    Ok(())
}

/// 限制并发的下载执行器
pub fn parallel_download(
    http: &reqwest::blocking::Client,
    tasks: Vec<(String, std::path::PathBuf, Option<String>)>,
    done: &std::sync::Arc<std::sync::atomic::AtomicUsize>,
    max_workers: usize,
    use_mirror: bool,
    cancel_name: Option<&str>,
) -> Result<(), String> {
    if tasks.is_empty() {
        return Ok(());
    }
    let errors = std::sync::Arc::new(Mutex::new(Vec::<String>::new()));
    let cancel_name = cancel_name.map(|name| name.to_string());
    let tasks = std::sync::Arc::new(tasks);
    let next_task = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let worker_count = max_workers.max(1).min(tasks.len());
    let handles: Vec<_> = (0..worker_count)
        .map(|_| {
            let tasks = tasks.clone();
            let next_task = next_task.clone();
            let done = done.clone();
            let errors = errors.clone();
            let h = http.clone();
            let cancel_name = cancel_name.clone();
            std::thread::spawn(move || loop {
                if cancel_name
                    .as_deref()
                    .is_some_and(crate::instance::is_cancelled)
                {
                    break;
                }
                let index = next_task.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let Some((url, dest, sha1)) = tasks.get(index) else {
                    break;
                };
                if let Err(e) = download_file_if_needed_cancelable(
                    &h,
                    url,
                    dest,
                    sha1.as_deref(),
                    use_mirror,
                    cancel_name.as_deref(),
                ) {
                    eprintln!("[download] 失败: {} -> {}", url, e);
                    errors.lock().unwrap().push(format!("{} -> {}", url, e));
                }
                done.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            })
        })
        .collect();

    let mut cancelled = false;
    loop {
        if cancel_name
            .as_deref()
            .is_some_and(crate::instance::is_cancelled)
        {
            cancelled = true;
            break;
        }
        if handles.iter().all(|handle| handle.is_finished()) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(80));
    }

    for h in handles {
        if h.join().is_err() {
            errors
                .lock()
                .unwrap()
                .push("download worker panicked".to_string());
        }
    }
    if cancel_name
        .as_deref()
        .is_some_and(crate::instance::is_cancelled)
        || cancelled
    {
        done.store(tasks.len(), std::sync::atomic::Ordering::Relaxed);
        return Err("用户取消下载".to_string());
    }
    let errors = errors.lock().unwrap();
    if errors.is_empty() {
        Ok(())
    } else {
        let sample = errors
            .iter()
            .take(3)
            .cloned()
            .collect::<Vec<_>>()
            .join("; ");
        Err(format!("{} files failed: {}", errors.len(), sample))
    }
}

pub fn parallel_download_mirror_then_official(
    http: &reqwest::blocking::Client,
    tasks: Vec<(String, std::path::PathBuf, Option<String>)>,
    done: &std::sync::Arc<std::sync::atomic::AtomicUsize>,
    max_workers: usize,
    cancel_name: Option<&str>,
) -> Result<(), String> {
    if tasks.is_empty() {
        return Ok(());
    }
    let errors = std::sync::Arc::new(Mutex::new(Vec::<String>::new()));
    let cancel_name = cancel_name.map(|name| name.to_string());
    let tasks = std::sync::Arc::new(tasks);
    let next_task = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let worker_count = max_workers.max(1).min(tasks.len());
    let handles: Vec<_> = (0..worker_count)
        .map(|_| {
            let tasks = tasks.clone();
            let next_task = next_task.clone();
            let done = done.clone();
            let errors = errors.clone();
            let h = http.clone();
            let cancel_name = cancel_name.clone();
            std::thread::spawn(move || loop {
                if cancel_name
                    .as_deref()
                    .is_some_and(crate::instance::is_cancelled)
                {
                    break;
                }
                let index = next_task.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let Some((url, dest, sha1)) = tasks.get(index) else {
                    break;
                };
                if let Err(e) = download_file_mirror_then_official(
                    &h,
                    url,
                    dest,
                    sha1.as_deref(),
                    cancel_name.as_deref(),
                ) {
                    eprintln!("[download] 失败: {} -> {}", url, e);
                    errors.lock().unwrap().push(format!("{} -> {}", url, e));
                }
                done.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            })
        })
        .collect();

    let mut cancelled = false;
    loop {
        if cancel_name
            .as_deref()
            .is_some_and(crate::instance::is_cancelled)
        {
            cancelled = true;
            break;
        }
        if handles.iter().all(|handle| handle.is_finished()) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(80));
    }

    for h in handles {
        if h.join().is_err() {
            errors
                .lock()
                .unwrap()
                .push("download worker panicked".to_string());
        }
    }
    if cancel_name
        .as_deref()
        .is_some_and(crate::instance::is_cancelled)
        || cancelled
    {
        done.store(tasks.len(), std::sync::atomic::Ordering::Relaxed);
        return Err("用户取消下载".to_string());
    }
    let errors = errors.lock().unwrap();
    if errors.is_empty() {
        Ok(())
    } else {
        let sample = errors
            .iter()
            .take(3)
            .cloned()
            .collect::<Vec<_>>()
            .join("; ");
        Err(format!("{} files failed: {}", errors.len(), sample))
    }
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
    let name_clone = safe_name.clone();
    let cancel_flag = crate::instance::register_cancel(&safe_name);
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
        ) {
            let was_cancelled = cancel_flag.load(std::sync::atomic::Ordering::Relaxed);
            crate::instance::unregister_cancel(&safe_name);
            eprintln!("[install] 错误: {}", e);
            let inst_dir = resolve_game_dir(&game_dir)
                .join("instances")
                .join(&safe_name);
            let install_marker = inst_dir.join(".oaoi_installing");
            if install_marker.exists() {
                let _ = std::fs::remove_dir_all(&inst_dir);
                eprintln!("[install] 已清理残留目录: {}", inst_dir.display());
            } else if inst_dir.exists() {
                eprintln!("[install] 跳过清理非本次安装目录: {}", inst_dir.display());
            }
            let stage = if was_cancelled { "cancelled" } else { "error" };
            let _ = app_handle.emit(
                "install-progress",
                serde_json::json!({
                    "name": safe_name, "stage": stage, "current": 0, "total": 0, "detail": e
                }),
            );
        } else {
            crate::instance::unregister_cancel(&safe_name);
        }
    });
    Ok(format!("开始创建版本: {}", name_clone))
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
) -> Option<std::thread::JoinHandle<Result<serde_json::Value, String>>> {
    if loader_version.is_empty()
        || !matches!(loader_type, "fabric" | "forge" | "quilt" | "neoforge")
    {
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

    Some(std::thread::spawn(move || {
        let mut loader_json = empty_loader_json();
        let java_to_use = resolve_loader_java_for_install(
            &app_handle,
            &name,
            &mc_version,
            &game_dir_input,
            &loader_type,
            &java_path,
        )?;
        match loader_type.as_str() {
            "fabric" => fabric::install_fabric(
                &app_handle,
                &name,
                &mc_version,
                &loader_version,
                &game_dir,
                &inst_dir,
                &http,
                use_mirror,
                &mut loader_json,
                true,
            )?,
            "forge" => forge::install_forge(
                &app_handle,
                &name,
                &mc_version,
                &loader_version,
                &game_dir,
                &inst_dir,
                &http,
                &java_to_use,
                use_mirror,
                &mut loader_json,
            )?,
            "quilt" => quilt::install_quilt(
                &app_handle,
                &name,
                &mc_version,
                &loader_version,
                &game_dir,
                &inst_dir,
                &http,
                use_mirror,
                &mut loader_json,
            )?,
            "neoforge" => neoforge::install_neoforge(
                &app_handle,
                &name,
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
    }))
}

fn resolve_loader_java_for_install(
    app_handle: &tauri::AppHandle,
    name: &str,
    mc_version: &str,
    game_dir_input: &str,
    loader_type: &str,
    java_path: &str,
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
    emit(
        "java",
        0,
        1,
        &format!("自动下载 Java {}...", required_major),
    );
    crate::java_download::download_java_sync_cancelable(required_major, game_dir_input, Some(name))
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
    if crate::instance::is_cancelled(name) {
        return Err("用户取消安装".to_string());
    }

    let inst_dir = game_dir.join("instances").join(name);
    if inst_dir.exists() {
        return Err(format!("版本 '{}' 已存在，请换一个名称！", name));
    }
    std::fs::create_dir_all(&inst_dir).map_err(|e| e.to_string())?;
    let install_marker_path = inst_dir.join(".oaoi_installing");
    std::fs::write(
        &install_marker_path,
        format!("pid={}\nversion={}\n", std::process::id(), mc_version),
    )
    .map_err(|e| format!("创建安装标记失败: {}", e))?;
    let inst_json_path = inst_dir.join("instance.json");

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
    );

    // 下载 vanilla 基础（client.jar + libraries + assets），内部也会并行跑
    let vanilla_result = vanilla::install_vanilla(
        app_handle, name, mc_version, meta_url, &game_dir, &inst_dir, &http, use_mirror,
    );
    let mut ver_json = match vanilla_result {
        Ok(ver_json) => ver_json,
        Err(e) => {
            let _ = crate::instance::cancel_modpack_install(name.to_string());
            if let Some(handle) = loader_handle {
                let _ = handle.join();
            }
            return Err(e);
        }
    };
    if crate::instance::is_cancelled(name) {
        if let Some(handle) = loader_handle {
            let _ = handle.join();
        }
        return Err("用户取消安装".to_string());
    }

    if let Some(handle) = loader_handle {
        let loader_json = match handle.join() {
            Ok(Ok(loader_json)) => loader_json,
            Ok(Err(e)) => return Err(e),
            Err(_) => return Err("Loader 安装线程异常退出".to_string()),
        };
        merge_loader_install_result(&mut ver_json, &loader_json);
    }
    if crate::instance::is_cancelled(name) {
        return Err("用户取消安装".to_string());
    }
    crate::instance::set_minecraft_language(&inst_dir, "zh_cn")?;

    // 写回最终配置到 instance.json
    std::fs::write(
        &inst_json_path,
        serde_json::to_string_pretty(&ver_json).unwrap(),
    )
    .map_err(|e| format!("保存版本配置失败: {}", e))?;

    emit("done", 1, 1, &format!("版本 '{}' 创建完成！", name));
    let _ = std::fs::remove_file(&install_marker_path);
    Ok(format!("版本 {} 创建成功", name))
}
