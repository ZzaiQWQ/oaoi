use super::{
    download_file_exact_once_with_stall_timeout, download_file_with_progress, library_allowed,
    make_emitter, mirror_url, parallel_download, safe_maven_path,
};
use crate::instance::safe_path_name;
use tauri::Emitter;

/// 安装 vanilla 基础（meta + client.jar + libraries + assets）
pub fn install_vanilla(
    app_handle: &tauri::AppHandle,
    name: &str,
    mc_version: &str,
    meta_url: &str,
    game_dir: &std::path::Path,
    inst_dir: &std::path::Path,
    http: &reqwest::blocking::Client,
    use_mirror: bool,
) -> Result<serde_json::Value, String> {
    let emit = make_emitter(app_handle, name);

    // 1. 下载版本 JSON
    emit("meta", 0, 1, &format!("下载 {} 元数据...", mc_version));
    let mirrored_meta = if use_mirror {
        format!(
            "https://bmclapi2.bangbang93.com/version/{}/json",
            mc_version
        )
    } else if meta_url.starts_with("https://bmclapi2.bangbang93.com/v1/packages/") {
        meta_url.replacen(
            "https://bmclapi2.bangbang93.com",
            "https://piston-meta.mojang.com",
            1,
        )
    } else {
        mirror_url(meta_url, false)
    };
    let resp = http
        .get(&mirrored_meta)
        .header(reqwest::header::ACCEPT_ENCODING, "identity")
        .send()
        .map_err(|e| format!("获取版本信息失败: {}\n(请检查网络或代理)", e))?;
    let status = resp.status();
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let body = resp
        .text()
        .map_err(|e| format!("读取版本信息失败: {} ({})", e, mirrored_meta))?;
    if !status.is_success() {
        let preview: String = body.chars().take(300).collect();
        return Err(format!(
            "获取版本信息失败: HTTP {} ({})\n{}",
            status, mirrored_meta, preview
        ));
    }
    let mut ver_json: serde_json::Value = serde_json::from_str(&body).map_err(|e| {
        let preview: String = body.chars().take(300).collect();
        format!(
            "解析版本信息失败: {}\nURL: {}\nContent-Type: {}\nBody: {}",
            e, mirrored_meta, content_type, preview
        )
    })?;
    emit("meta", 1, 1, "元数据下载完成");

    // 2. 元数据就绪后，client.jar / libraries / assets 同时下载
    let client_info = ver_json
        .get("downloads")
        .and_then(|d| d.get("client"))
        .ok_or("版本 JSON 缺少 downloads.client")?
        .clone();
    let libraries = ver_json
        .get("libraries")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let asset_index = ver_json.get("assetIndex").cloned();

    let mut handles: Vec<(&'static str, std::thread::JoinHandle<Result<(), String>>)> = Vec::new();
    handles.push((
        "client.jar",
        std::thread::spawn({
            let app_handle = app_handle.clone();
            let name = name.to_string();
            let inst_dir = inst_dir.to_path_buf();
            let http = http.clone();
            move || {
                download_client_stage(
                    &app_handle,
                    &name,
                    &inst_dir,
                    &http,
                    use_mirror,
                    &client_info,
                )
            }
        }),
    ));
    handles.push((
        "libraries",
        std::thread::spawn({
            let app_handle = app_handle.clone();
            let name = name.to_string();
            let game_dir = game_dir.to_path_buf();
            let http = http.clone();
            move || {
                download_libraries_stage(
                    &app_handle,
                    &name,
                    &game_dir,
                    &http,
                    use_mirror,
                    libraries,
                )
            }
        }),
    ));
    if let Some(asset_index) = asset_index {
        handles.push((
            "assets",
            std::thread::spawn({
                let app_handle = app_handle.clone();
                let name = name.to_string();
                let game_dir = game_dir.to_path_buf();
                let http = http.clone();
                move || {
                    download_assets_stage(
                        &app_handle,
                        &name,
                        &game_dir,
                        &http,
                        use_mirror,
                        &asset_index,
                    )
                }
            }),
        ));
    }
    join_vanilla_stages(name, handles)?;

    // 设置基础实例信息
    ver_json["name"] = serde_json::Value::String(name.to_string());
    ver_json["mcVersion"] = serde_json::Value::String(mc_version.to_string());

    if ver_json["mainClass"].is_null() {
        ver_json["mainClass"] =
            serde_json::Value::String("net.minecraft.client.main.Main".to_string());
    }
    ver_json["loader"] = serde_json::json!({
        "type": "vanilla",
        "version": ""
    });

    Ok(ver_json)
}

fn join_vanilla_stages(
    name: &str,
    mut handles: Vec<(&'static str, std::thread::JoinHandle<Result<(), String>>)>,
) -> Result<(), String> {
    let mut first_error: Option<String> = None;
    while !handles.is_empty() {
        let mut index = 0;
        let mut progressed = false;
        while index < handles.len() {
            if handles[index].1.is_finished() {
                let (stage, handle) = handles.swap_remove(index);
                progressed = true;
                match handle.join() {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => {
                        if first_error.is_none() {
                            let _ = crate::instance::cancel_modpack_install(name.to_string());
                            first_error = Some(format!("{}: {}", stage, e));
                        }
                    }
                    Err(_) => {
                        if first_error.is_none() {
                            let _ = crate::instance::cancel_modpack_install(name.to_string());
                            first_error = Some(format!("{} 线程异常退出", stage));
                        }
                    }
                }
            } else {
                index += 1;
            }
        }
        if !progressed {
            if crate::instance::is_cancelled(name) && first_error.is_none() {
                first_error = Some("用户取消安装".to_string());
            }
            std::thread::sleep(std::time::Duration::from_millis(80));
        }
    }
    if let Some(error) = first_error {
        Err(error)
    } else {
        Ok(())
    }
}

fn download_client_stage(
    app_handle: &tauri::AppHandle,
    name: &str,
    inst_dir: &std::path::Path,
    http: &reqwest::blocking::Client,
    use_mirror: bool,
    client_info: &serde_json::Value,
) -> Result<(), String> {
    let emit = make_emitter(app_handle, name);
    emit("client", 0, 1, "下载 client.jar...");
    let client_url = client_info
        .get("url")
        .and_then(|v| v.as_str())
        .ok_or("缺少 client url")?;
    let client_sha1 = client_info.get("sha1").and_then(|v| v.as_str());
    let jar_path = inst_dir.join("client.jar");
    download_file_with_progress(
        http,
        client_url,
        &jar_path,
        client_sha1,
        use_mirror,
        Some(name),
        |downloaded, total| {
            let total = total
                .unwrap_or_else(|| downloaded.max(1))
                .max(downloaded)
                .max(1);
            emit(
                "client",
                progress_usize(downloaded),
                progress_usize(total),
                "下载 client.jar...",
            );
        },
    )
    .map_err(|e| format!("下载 client.jar 失败: {}", e))?;
    emit("client", 1, 1, "client.jar 完成");
    Ok(())
}

fn download_libraries_stage(
    app_handle: &tauri::AppHandle,
    name: &str,
    game_dir: &std::path::Path,
    http: &reqwest::blocking::Client,
    use_mirror: bool,
    libs: Vec<serde_json::Value>,
) -> Result<(), String> {
    let emit = make_emitter(app_handle, name);
    let mut tasks: Vec<(String, std::path::PathBuf, Option<String>)> = Vec::new();
    for lib in libs.iter() {
        let rules = lib
            .get("rules")
            .map(|v| v.as_array().cloned().unwrap_or_default());
        if !library_allowed(&rules) {
            continue;
        }
        if let Some(artifact) = lib.get("downloads").and_then(|d| d.get("artifact")) {
            let path = artifact.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let url = artifact.get("url").and_then(|v| v.as_str()).unwrap_or("");
            let sha1 = artifact.get("sha1").and_then(|v| v.as_str());
            if !path.is_empty() && !url.is_empty() {
                let rel_path = safe_maven_path(path)?;
                let dest = game_dir.join("libs").join(rel_path);
                tasks.push((url.to_string(), dest, sha1.map(|s| s.to_string())));
            }
        }
    }
    let total = tasks.len();
    emit(
        "libraries",
        0,
        total,
        &format!("下载 {} 个依赖库...", total),
    );
    let done = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let app_clone = app_handle.clone();
    let done_reporter = done.clone();
    let inst_name_copy = name.to_string();
    let reporter =
        std::thread::spawn(move || loop {
            let finished = done_reporter.load(std::sync::atomic::Ordering::Relaxed);
            let _ = app_clone.emit("install-progress", serde_json::json!({
            "name": inst_name_copy, "stage": "libraries", "current": finished, "total": total,
            "detail": format!("依赖库 {}/{}", finished, total)
        }));
            if finished >= total {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(300));
        });
    let download_result = parallel_download(http, tasks, &done, 32, use_mirror, Some(name));
    let _ = reporter.join();
    download_result.map_err(|e| format!("libraries: {}", e))?;
    emit("libraries", total, total, "依赖库下载完成");
    Ok(())
}

fn download_assets_stage(
    app_handle: &tauri::AppHandle,
    name: &str,
    game_dir: &std::path::Path,
    _http: &reqwest::blocking::Client,
    use_mirror: bool,
    asset_index: &serde_json::Value,
) -> Result<(), String> {
    let emit = make_emitter(app_handle, name);
    let index_url = asset_index
        .get("url")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let index_id = asset_index
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let index_sha1 = asset_index.get("sha1").and_then(|v| v.as_str());

    let index_file = safe_path_name(&format!("{}.json", index_id), "资源索引")?;
    let index_path = game_dir.join("res").join("indexes").join(index_file);
    emit("assetIndex", 0, 0, "下载资源索引...");
    let mut index_urls = Vec::new();
    if use_mirror && !index_id.is_empty() && index_id != "unknown" {
        index_urls.push((
            "镜像".to_string(),
            format!("https://bmclapi2.bangbang93.com/indexes/{}.json", index_id),
            2,
            true,
        ));
    }
    if !index_url.is_empty() {
        index_urls.push((
            "官方".to_string(),
            index_url.to_string(),
            if use_mirror { 3 } else { 5 },
            false,
        ));
    }
    download_resource_file_candidates(&index_urls, &index_path, index_sha1, Some(name), None)
        .map_err(|e| format!("下载资源索引失败: {}", e))?;
    emit("assetIndex", 1, 1, "资源索引完成");

    let index_content =
        std::fs::read_to_string(&index_path).map_err(|e| format!("读取资源索引失败: {}", e))?;
    let index_json: serde_json::Value =
        serde_json::from_str(&index_content).map_err(|e| format!("解析资源索引失败: {}", e))?;
    let Some(objects) = index_json.get("objects").and_then(|v| v.as_object()) else {
        return Ok(());
    };
    let mut asset_tasks: Vec<(String, std::path::PathBuf, String)> = Vec::new();
    for (_name, info) in objects.iter() {
        let hash = info.get("hash").and_then(|v| v.as_str()).unwrap_or("");
        if !is_valid_sha1(hash) {
            continue;
        }
        let prefix = &hash[..2];
        let dest = game_dir.join("res").join("objects").join(prefix).join(hash);
        let url = format!(
            "https://resources.download.minecraft.net/{}/{}",
            prefix, hash
        );
        asset_tasks.push((url, dest, hash.to_string()));
    }
    let total = asset_tasks.len();
    emit("assets", 0, total, &format!("下载 {} 个资源...", total));

    let done = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let app_clone = app_handle.clone();
    let done_reporter = done.clone();
    let inst_name_copy = name.to_string();
    let reporter = std::thread::spawn(move || loop {
        let finished = done_reporter.load(std::sync::atomic::Ordering::Relaxed);
        let _ = app_clone.emit(
            "install-progress",
            serde_json::json!({
                "name": inst_name_copy, "stage": "assets", "current": finished, "total": total,
                "detail": format!("资源 {}/{}", finished, total)
            }),
        );
        if finished >= total {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
    });

    let asset_dl_tasks: Vec<(String, std::path::PathBuf, Option<String>)> = asset_tasks
        .into_iter()
        .map(|(url, dest, hash)| (url, dest, Some(hash)))
        .collect();
    let download_result =
        parallel_download_resources(asset_dl_tasks, &done, 64, use_mirror, Some(name));
    let _ = reporter.join();
    download_result.map_err(|e| format!("assets: {}", e))?;
    emit("assets", total, total, "资源下载完成");
    Ok(())
}

const RESOURCE_DOWNLOAD_STALL_TIMEOUT_SECS: u64 = 15;
const RESOURCE_DOWNLOAD_TOTAL_TIMEOUT_SECS: u64 = 90;
const RESOURCE_MIRROR_RETRIES: usize = 2;
const RESOURCE_OFFICIAL_RETRIES: usize = 3;
const RESOURCE_RETRY_DELAY_SECS: u64 = 15;

struct ResourceDownloadState {
    mirror_cooldown_until_ms: std::sync::atomic::AtomicU64,
}

impl ResourceDownloadState {
    fn new() -> Self {
        Self {
            mirror_cooldown_until_ms: std::sync::atomic::AtomicU64::new(0),
        }
    }

    fn is_mirror_cooling_down(&self) -> bool {
        current_unix_millis()
            < self
                .mirror_cooldown_until_ms
                .load(std::sync::atomic::Ordering::Relaxed)
    }

    fn mirror_cooldown_remaining_ms(&self) -> u64 {
        self.mirror_cooldown_until_ms
            .load(std::sync::atomic::Ordering::Relaxed)
            .saturating_sub(current_unix_millis())
    }

    fn cool_down_mirror(&self) {
        let until = current_unix_millis() + RESOURCE_RETRY_DELAY_SECS * 1000;
        let previous = self
            .mirror_cooldown_until_ms
            .fetch_max(until, std::sync::atomic::Ordering::Relaxed);
        if until > previous {
            eprintln!(
                "[assets] BMCLAPI 进入全局冷却 {} 秒，冷却期间新资源改走官方源",
                RESOURCE_RETRY_DELAY_SECS
            );
        }
    }
}

fn current_unix_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis().min(u64::MAX as u128) as u64)
        .unwrap_or(0)
}

fn is_resource_limit_error(last_err: &str) -> bool {
    let lower = last_err.to_ascii_lowercase();
    last_err.contains("429")
        || last_err.contains("超时")
        || last_err.contains("下载卡住")
        || last_err.contains("下载过慢")
        || lower.contains("timeout")
        || lower.contains("timed out")
        || lower.contains("deadline")
        || lower.contains("too slow")
}

fn resource_retry_delay(last_err: &str) -> std::time::Duration {
    if is_resource_limit_error(last_err) {
        std::time::Duration::from_secs(RESOURCE_RETRY_DELAY_SECS)
    } else {
        std::time::Duration::from_millis(500)
    }
}

fn download_resource_file_candidates(
    candidates: &[(String, String, usize, bool)],
    dest: &std::path::Path,
    expected_sha1: Option<&str>,
    cancel_name: Option<&str>,
    state: Option<&ResourceDownloadState>,
) -> Result<bool, String> {
    if candidates.is_empty() {
        return Err("没有可用下载地址".to_string());
    }

    let mut last_err = String::new();
    for (label, url, retries, is_mirror) in candidates {
        for attempt in 0..*retries {
            if cancel_name.is_some_and(crate::instance::is_cancelled) {
                return Err("用户取消下载".to_string());
            }
            if *is_mirror {
                if let Some(state) = state {
                    wait_for_resource_mirror_cooldown(state, cancel_name)?;
                }
            }
            eprintln!(
                "[assets] {}尝试 {}/{}: {}",
                label,
                attempt + 1,
                retries,
                url
            );
            match download_file_exact_once_with_stall_timeout(
                url,
                dest,
                expected_sha1,
                cancel_name,
                RESOURCE_DOWNLOAD_STALL_TIMEOUT_SECS,
                RESOURCE_DOWNLOAD_TOTAL_TIMEOUT_SECS,
            ) {
                Ok(result) => return Ok(result),
                Err(e) => {
                    last_err = format!("{}: {}", url, e);
                    let _ = std::fs::remove_file(dest);
                    eprintln!("[assets] {}失败: {}", label, last_err);
                    if *is_mirror && is_resource_limit_error(&last_err) {
                        if let Some(state) = state {
                            state.cool_down_mirror();
                        }
                        break;
                    }
                    if attempt + 1 < *retries {
                        std::thread::sleep(resource_retry_delay(&last_err));
                    }
                }
            }
        }
    }

    Err(format!("所有资源下载地址均失败: {}", last_err))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ResourceSource {
    Mirror,
    Official,
}

fn wait_for_resource_mirror_cooldown(
    state: &ResourceDownloadState,
    cancel_name: Option<&str>,
) -> Result<(), String> {
    loop {
        if cancel_name.is_some_and(crate::instance::is_cancelled) {
            return Err("用户取消下载".to_string());
        }
        let remaining = state.mirror_cooldown_remaining_ms();
        if remaining == 0 {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(remaining.min(250)));
    }
}

fn download_resource_source_once(
    label: &str,
    attempt: usize,
    retries: usize,
    url: &str,
    dest: &std::path::Path,
    expected_sha1: Option<&str>,
    cancel_name: Option<&str>,
) -> Result<bool, String> {
    eprintln!("[assets] {}尝试 {}/{}: {}", label, attempt, retries, url);
    download_file_exact_once_with_stall_timeout(
        url,
        dest,
        expected_sha1,
        cancel_name,
        RESOURCE_DOWNLOAD_STALL_TIMEOUT_SECS,
        RESOURCE_DOWNLOAD_TOTAL_TIMEOUT_SECS,
    )
}

fn download_resource_file(
    url: &str,
    dest: &std::path::Path,
    expected_sha1: Option<&str>,
    use_mirror: bool,
    cancel_name: Option<&str>,
    state: &ResourceDownloadState,
) -> Result<bool, String> {
    let official = mirror_url(url, false);
    let mirror = mirror_url(url, true);
    if !use_mirror || mirror == official {
        let candidates = vec![(
            "官方".to_string(),
            official,
            RESOURCE_MIRROR_RETRIES + RESOURCE_OFFICIAL_RETRIES,
            false,
        )];
        return download_resource_file_candidates(
            &candidates,
            dest,
            expected_sha1,
            cancel_name,
            None,
        );
    }

    let max_mirror_attempts = RESOURCE_MIRROR_RETRIES + RESOURCE_OFFICIAL_RETRIES;
    let max_official_attempts = RESOURCE_OFFICIAL_RETRIES;
    let mut mirror_attempts = 0usize;
    let mut official_attempts = 0usize;
    let mut last_err = String::new();
    let mut next_source = if state.is_mirror_cooling_down() {
        ResourceSource::Official
    } else {
        ResourceSource::Mirror
    };
    let mut previous_failed_source: Option<ResourceSource> = None;

    while mirror_attempts < max_mirror_attempts || official_attempts < max_official_attempts {
        if cancel_name.is_some_and(crate::instance::is_cancelled) {
            return Err("用户取消下载".to_string());
        }

        match next_source {
            ResourceSource::Mirror => {
                if mirror_attempts >= max_mirror_attempts {
                    next_source = ResourceSource::Official;
                    continue;
                }
                wait_for_resource_mirror_cooldown(state, cancel_name)?;
                mirror_attempts += 1;
                match download_resource_source_once(
                    "镜像",
                    mirror_attempts,
                    max_mirror_attempts,
                    &mirror,
                    dest,
                    expected_sha1,
                    cancel_name,
                ) {
                    Ok(result) => return Ok(result),
                    Err(e) => {
                        last_err = format!("{}: {}", mirror, e);
                        let came_from_official =
                            previous_failed_source == Some(ResourceSource::Official);
                        previous_failed_source = Some(ResourceSource::Mirror);
                        let _ = std::fs::remove_file(dest);
                        eprintln!("[assets] 镜像失败: {}", last_err);
                        if is_resource_limit_error(&last_err) {
                            state.cool_down_mirror();
                        }
                        next_source = if came_from_official {
                            ResourceSource::Mirror
                        } else if official_attempts < max_official_attempts {
                            ResourceSource::Official
                        } else {
                            ResourceSource::Mirror
                        };
                    }
                }
            }
            ResourceSource::Official => {
                if official_attempts >= max_official_attempts {
                    next_source = ResourceSource::Mirror;
                    continue;
                }
                official_attempts += 1;
                match download_resource_source_once(
                    "官方",
                    official_attempts,
                    max_official_attempts,
                    &official,
                    dest,
                    expected_sha1,
                    cancel_name,
                ) {
                    Ok(result) => return Ok(result),
                    Err(e) => {
                        last_err = format!("{}: {}", official, e);
                        previous_failed_source = Some(ResourceSource::Official);
                        let _ = std::fs::remove_file(dest);
                        eprintln!("[assets] 官方失败，回到镜像: {}", last_err);
                        next_source = ResourceSource::Mirror;
                    }
                }
            }
        }
    }

    Err(format!("所有资源下载地址均失败: {}", last_err))
}

fn parallel_download_resources(
    tasks: Vec<(String, std::path::PathBuf, Option<String>)>,
    done: &std::sync::Arc<std::sync::atomic::AtomicUsize>,
    max_workers: usize,
    use_mirror: bool,
    cancel_name: Option<&str>,
) -> Result<(), String> {
    if tasks.is_empty() {
        return Ok(());
    }
    let errors = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let cancel_name = cancel_name.map(|name| name.to_string());
    let tasks = std::sync::Arc::new(tasks);
    let next_task = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let state = std::sync::Arc::new(ResourceDownloadState::new());
    let worker_count = max_workers.max(1).min(tasks.len());
    let handles: Vec<_> = (0..worker_count)
        .map(|_| {
            let tasks = tasks.clone();
            let next_task = next_task.clone();
            let done = done.clone();
            let errors = errors.clone();
            let cancel_name = cancel_name.clone();
            let state = state.clone();
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
                if let Err(e) = download_resource_file(
                    url,
                    dest,
                    sha1.as_deref(),
                    use_mirror,
                    cancel_name.as_deref(),
                    &state,
                ) {
                    eprintln!("[assets] 失败: {} -> {}", url, e);
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

    for handle in handles {
        if handle.join().is_err() {
            errors
                .lock()
                .unwrap()
                .push("asset download worker panicked".to_string());
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

fn is_valid_sha1(value: &str) -> bool {
    value.len() == 40 && value.chars().all(|ch| ch.is_ascii_hexdigit())
}

fn progress_usize(value: u64) -> usize {
    value.min(usize::MAX as u64) as usize
}
