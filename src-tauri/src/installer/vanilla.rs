use super::{library_allowed, make_emitter, mirror_url, safe_maven_path};
use crate::downloader::event::{DownloadEvent, DownloadOutcome};
use crate::downloader::{
    DownloadCandidate, DownloadEngineOptions, DownloadManager, DownloadRequest,
};
use crate::instance::{install_download_pool, register_download_manager, safe_path_name};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
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
                            eprintln!("[vanilla] {} 阶段失败，取消基础游戏下载: {}", stage, e);
                            let _ = crate::instance::cancel_modpack_install(name.to_string());
                            first_error = Some(format!("{}: {}", stage, e));
                        }
                    }
                    Err(_) => {
                        if first_error.is_none() {
                            eprintln!("[vanilla] {} 阶段线程异常退出，取消基础游戏下载", stage);
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

#[derive(Clone)]
struct VanillaDownloadTask {
    candidates: Vec<String>,
    dest: PathBuf,
    sha1: Option<String>,
}

#[derive(Clone)]
struct VanillaTaskInfo {
    first_url: String,
}

fn vanilla_source_candidates(url: &str, use_mirror: bool) -> Vec<String> {
    let official = mirror_url(url, false);
    let mirror = mirror_url(url, true);
    let mut out = Vec::new();
    if use_mirror && mirror != official {
        out.push(mirror);
        out.push(official);
    } else {
        out.push(official);
        if mirror != out[0] {
            out.push(mirror);
        }
    }
    out
}

fn vanilla_download_options(
    max_global_connections: usize,
    max_connections_per_file: usize,
    max_active_files: usize,
) -> DownloadEngineOptions {
    let mut options = DownloadEngineOptions::default();
    options.max_global_connections = max_global_connections.max(1);
    options.max_connections_per_file = max_connections_per_file.max(1);
    options.max_active_files = max_active_files.max(1);
    options.candidate_no_progress_timeout = Duration::from_secs(15);
    options.candidate_retry_delay = Duration::from_secs(15);
    options.source_cooldown_duration = Duration::from_secs(15);
    options.read_timeout = Duration::from_secs(15);
    options
}

fn build_vanilla_request(stage: &str, index: usize, task: &VanillaDownloadTask) -> DownloadRequest {
    let candidates = task
        .candidates
        .iter()
        .cloned()
        .map(DownloadCandidate::new)
        .collect::<Vec<_>>();
    let mut request = DownloadRequest::new(format!("{stage}-{index}"), candidates, &task.dest);
    if let Some(sha1) = task.sha1.as_deref().filter(|sha1| !sha1.trim().is_empty()) {
        request = request.with_expected_sha1(sha1.trim().to_string());
    }
    request
}

fn mark_vanilla_task_done(
    request_id: &str,
    done: &AtomicUsize,
    completed: &Mutex<HashSet<String>>,
) -> usize {
    let mut completed = completed.lock().unwrap();
    if completed.insert(request_id.to_string()) {
        done.fetch_add(1, Ordering::Relaxed) + 1
    } else {
        done.load(Ordering::Relaxed)
    }
}

fn is_cancel_related_error(error: &str) -> bool {
    error.contains("cancelled")
        || error.contains("download cancelled")
        || error.contains("用户取消")
}

fn run_vanilla_downloads(
    app_handle: &tauri::AppHandle,
    name: &str,
    stage: &str,
    detail_label: &str,
    tasks: Vec<VanillaDownloadTask>,
    max_global_connections: usize,
    max_connections_per_file: usize,
    max_active_files: usize,
    report_bytes: bool,
) -> Result<(), String> {
    let emit = make_emitter(app_handle, name);
    let total = tasks.len();
    if total == 0 {
        emit(stage, 0, 0, detail_label);
        return Ok(());
    }

    let mut requests = Vec::with_capacity(tasks.len());
    let mut info_by_id = HashMap::new();
    for (index, task) in tasks.iter().enumerate() {
        let request = build_vanilla_request(stage, index, task);
        info_by_id.insert(
            request.id.clone(),
            VanillaTaskInfo {
                first_url: task.candidates.first().cloned().unwrap_or_default(),
            },
        );
        requests.push(request);
    }

    let options = vanilla_download_options(
        max_global_connections,
        max_connections_per_file,
        max_active_files,
    );
    let pool = install_download_pool(
        name,
        DownloadEngineOptions::default_global_connection_limit(),
    );
    let manager = DownloadManager::with_options_and_pool(options, pool)?;
    let _manager_registration = register_download_manager(name, &manager);

    let done = Arc::new(AtomicUsize::new(0));
    let completed = Arc::new(Mutex::new(HashSet::new()));
    let info_for_event = Arc::new(info_by_id.clone());
    let app_for_event = app_handle.clone();
    let name_for_event = name.to_string();
    let stage_for_event = stage.to_string();
    let detail_for_event = detail_label.to_string();
    let done_for_event = done.clone();
    let completed_for_event = completed.clone();
    let info_for_failed_event = info_for_event.clone();
    let outcomes = manager.download_many(requests, move |event| match event {
        DownloadEvent::Progress(progress) if report_bytes => {
            let total_bytes = progress
                .total
                .unwrap_or_else(|| progress.downloaded.max(1))
                .max(progress.downloaded)
                .max(1);
            let _ = app_for_event.emit(
                "install-progress",
                serde_json::json!({
                    "name": name_for_event,
                    "stage": stage_for_event,
                    "current": progress_usize(progress.downloaded),
                    "total": progress_usize(total_bytes),
                    "detail": detail_for_event
                }),
            );
        }
        DownloadEvent::FileFinished(result) => {
            let finished =
                mark_vanilla_task_done(&result.request_id, &done_for_event, &completed_for_event);
            if !report_bytes {
                let _ = app_for_event.emit(
                    "install-progress",
                    serde_json::json!({
                        "name": name_for_event,
                        "stage": stage_for_event,
                        "current": finished,
                        "total": total,
                        "detail": format!("{} {}/{}", detail_for_event, finished, total)
                    }),
                );
            }
        }
        DownloadEvent::FileFailed {
            request_id, error, ..
        } => {
            if !is_cancel_related_error(&error) {
                let first_url = info_for_failed_event
                    .get(&request_id)
                    .map(|info| info.first_url.as_str())
                    .unwrap_or("");
                eprintln!(
                    "[vanilla] {} 文件下载失败事件: {} -> {}",
                    stage_for_event, first_url, error
                );
            }
        }
        _ => {}
    });

    let mut errors = Vec::new();
    for outcome in outcomes {
        if let DownloadOutcome::Failed {
            request_id, error, ..
        } = outcome
        {
            let first_url = info_by_id
                .get(&request_id)
                .map(|info| info.first_url.clone())
                .unwrap_or_default();
            errors.push(format!("{} -> {}", first_url, error));
        }
    }
    let was_cancelled = crate::instance::is_cancelled(name);
    if errors.is_empty() {
        if was_cancelled {
            return Err("用户取消下载".to_string());
        }
        Ok(())
    } else if was_cancelled && errors.iter().all(|error| is_cancel_related_error(error)) {
        Err("用户取消下载".to_string())
    } else {
        for error in &errors {
            eprintln!("[vanilla] {} 下载失败: {}", stage, error);
        }
        let sample = errors
            .iter()
            .take(3)
            .cloned()
            .collect::<Vec<_>>()
            .join("; ");
        Err(format!("{} files failed: {}", errors.len(), sample))
    }
}

fn download_client_stage(
    app_handle: &tauri::AppHandle,
    name: &str,
    inst_dir: &std::path::Path,
    _http: &reqwest::blocking::Client,
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
    let task = VanillaDownloadTask {
        candidates: vanilla_source_candidates(client_url, use_mirror),
        dest: jar_path,
        sha1: client_sha1.map(|sha1| sha1.to_string()),
    };
    run_vanilla_downloads(
        app_handle,
        name,
        "client",
        "下载 client.jar...",
        vec![task],
        16,
        16,
        1,
        true,
    )
    .map_err(|e| format!("下载 client.jar 失败: {}", e))?;
    emit("client", 1, 1, "client.jar 完成");
    Ok(())
}

fn download_libraries_stage(
    app_handle: &tauri::AppHandle,
    name: &str,
    game_dir: &std::path::Path,
    _http: &reqwest::blocking::Client,
    use_mirror: bool,
    libs: Vec<serde_json::Value>,
) -> Result<(), String> {
    let emit = make_emitter(app_handle, name);
    let mut tasks = Vec::new();
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
                tasks.push(VanillaDownloadTask {
                    candidates: vanilla_source_candidates(url, use_mirror),
                    dest,
                    sha1: sha1.map(|s| s.to_string()),
                });
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
    run_vanilla_downloads(
        app_handle,
        name,
        "libraries",
        "依赖库",
        tasks,
        32,
        1,
        32,
        false,
    )
    .map_err(|e| format!("libraries: {}", e))?;
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
        index_urls.push(format!(
            "https://bmclapi2.bangbang93.com/indexes/{}.json",
            index_id
        ));
    }
    if !index_url.is_empty() {
        for url in vanilla_source_candidates(index_url, false) {
            if !index_urls.iter().any(|item| item == &url) {
                index_urls.push(url);
            }
        }
    }
    run_vanilla_downloads(
        app_handle,
        name,
        "assetIndex",
        "资源索引",
        vec![VanillaDownloadTask {
            candidates: index_urls,
            dest: index_path.clone(),
            sha1: index_sha1.map(|sha1| sha1.to_string()),
        }],
        1,
        1,
        1,
        false,
    )
    .map_err(|e| format!("下载资源索引失败: {}", e))?;
    emit("assetIndex", 1, 1, "资源索引完成");

    let index_content =
        std::fs::read_to_string(&index_path).map_err(|e| format!("读取资源索引失败: {}", e))?;
    let index_json: serde_json::Value =
        serde_json::from_str(&index_content).map_err(|e| format!("解析资源索引失败: {}", e))?;
    let Some(objects) = index_json.get("objects").and_then(|v| v.as_object()) else {
        return Ok(());
    };
    let mut asset_tasks = Vec::new();
    let mut seen_hashes = HashSet::new();
    for (_name, info) in objects.iter() {
        let hash = info.get("hash").and_then(|v| v.as_str()).unwrap_or("");
        if !is_valid_sha1(hash) {
            continue;
        }
        // 资源索引可能多个资源名指向同一个 hash，目标文件只需要下载一次。
        if !seen_hashes.insert(hash.to_string()) {
            continue;
        }
        let prefix = &hash[..2];
        let dest = game_dir.join("res").join("objects").join(prefix).join(hash);
        let url = format!(
            "https://resources.download.minecraft.net/{}/{}",
            prefix, hash
        );
        asset_tasks.push(VanillaDownloadTask {
            candidates: vanilla_source_candidates(&url, use_mirror),
            dest,
            sha1: Some(hash.to_string()),
        });
    }
    let total = asset_tasks.len();
    emit("assets", 0, total, &format!("下载 {} 个资源...", total));
    run_vanilla_downloads(
        app_handle,
        name,
        "assets",
        "资源",
        asset_tasks,
        32,
        1,
        32,
        false,
    )
    .map_err(|e| format!("assets: {}", e))?;
    emit("assets", total, total, "资源下载完成");
    Ok(())
}

fn is_valid_sha1(value: &str) -> bool {
    value.len() == 40 && value.chars().all(|ch| ch.is_ascii_hexdigit())
}

fn progress_usize(value: u64) -> usize {
    value.min(usize::MAX as u64) as usize
}
