use super::cf_download::cf_cdn_urls;
use super::download_mirror::{append_unique_urls, with_mod_mirrors};
use super::{build_http_client, detect_target_dir, emit_progress, ModpackKind, ModpackMeta};
use crate::downloader::event::{DownloadEvent, DownloadOutcome};
use crate::downloader::{
    DownloadCandidate, DownloadEngineOptions, DownloadManager, DownloadRequest,
};
use crate::installer::{
    empty_loader_json, fetch_json_cancelable, fetch_json_request_cancelable,
    merge_loader_install_result, write_version_json_atomic,
};
use crate::instance::{
    cf_api_key, install_download_pool, register_download_manager, safe_join, safe_path_name,
    strip_launcher_private_version_fields, version_json_path,
};
use crate::modpack_sources::sha1_from_curseforge_hashes;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone)]
struct ModpackDownloadTask {
    urls: Vec<String>,
    dest: std::path::PathBuf,
    sha1: Option<String>,
}

struct CategoryCounter {
    done: AtomicUsize,
    total: usize,
}

#[derive(Debug, Clone)]
struct ModpackDownloadError {
    failed_count: usize,
    total_files: usize,
    message: String,
    cancelled: bool,
}

impl ModpackDownloadError {
    fn new(failed_count: usize, total_files: usize, message: impl Into<String>) -> Self {
        Self {
            failed_count,
            total_files,
            message: message.into(),
            cancelled: false,
        }
    }

    fn cancelled(total_files: usize, message: impl Into<String>) -> Self {
        Self {
            failed_count: 0,
            total_files,
            message: message.into(),
            cancelled: true,
        }
    }
}

const MODPACK_DOWNLOAD_WORKERS: usize = 48;
const MODPACK_FILE_RETRY_DELAY_SECS: u64 = 15;
const MODPACK_FILE_STALL_TIMEOUT_SECS: u64 = 10;
const MODPACK_FILE_SLOW_SAMPLE_SECS: u64 = 10;
const MODPACK_FILE_SLOW_MIN_BYTES_PER_SEC: u64 = 32 * 1024;
const CF_API_COOLDOWN_SECS: u64 = 15;
const CF_API_RETRY_ATTEMPTS: usize = 2;
const CF_FALLBACK_CONCURRENCY: usize = 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct UnresolvedCurseForgeFile {
    project_id: u32,
    file_id: u32,
    try_individual_api: bool,
}

#[derive(Default)]
struct CurseForgeApiThrottle {
    cooldown_until: Mutex<Option<Instant>>,
}

impl CurseForgeApiThrottle {
    fn wait(&self, cancel_name: &str) -> Result<(), String> {
        loop {
            ensure_modpack_not_cancelled(cancel_name)?;
            let remaining = {
                let mut cooldown = self
                    .cooldown_until
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let now = Instant::now();
                match *cooldown {
                    Some(until) if until > now => Some(until.duration_since(now)),
                    Some(_) => {
                        *cooldown = None;
                        None
                    }
                    None => None,
                }
            };
            let Some(remaining) = remaining else {
                return Ok(());
            };
            std::thread::sleep(remaining.min(Duration::from_millis(100)));
        }
    }

    fn record_failure(&self, error: &str) {
        if !curseforge_api_requires_cooldown(error) {
            return;
        }
        let until = Instant::now() + Duration::from_secs(CF_API_COOLDOWN_SECS);
        let mut cooldown = self
            .cooldown_until
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if cooldown.is_none_or(|current| current < until) {
            *cooldown = Some(until);
        }
    }
}

fn curseforge_api_requires_cooldown(error: &str) -> bool {
    ["429", "500", "502", "503", "504", "timed out", "timeout"]
        .iter()
        .any(|marker| error.to_ascii_lowercase().contains(marker))
}

fn classify_modpack_path(dest: &Path) -> &'static str {
    let s = dest.to_string_lossy().replace('\\', "/");
    if s.contains("/mods/") {
        "mods"
    } else if s.contains("/resourcepacks/") || s.contains("/resources/") {
        "resourcepacks"
    } else if s.contains("/shaderpacks/") || s.contains("/shaders/") {
        "shaderpacks"
    } else if s.contains("/config/") {
        "config"
    } else {
        "other"
    }
}

#[derive(Clone)]
struct ModpackTaskInfo {
    category: String,
    first_url: String,
}

fn modpack_download_options() -> DownloadEngineOptions {
    let mut options = DownloadEngineOptions::default();
    options.max_global_connections = DownloadEngineOptions::default_global_connection_limit();
    options.max_connections_per_file = options.max_global_connections;
    options.max_active_files = MODPACK_DOWNLOAD_WORKERS;
    options.candidate_no_progress_timeout = Duration::from_secs(MODPACK_FILE_STALL_TIMEOUT_SECS);
    options.candidate_low_speed_limit = MODPACK_FILE_SLOW_MIN_BYTES_PER_SEC;
    options.candidate_low_speed_window = Duration::from_secs(MODPACK_FILE_SLOW_SAMPLE_SECS);
    options.candidate_retry_delay = Duration::from_secs(MODPACK_FILE_RETRY_DELAY_SECS);
    options.source_cooldown_duration = Duration::from_secs(MODPACK_FILE_RETRY_DELAY_SECS);
    options.read_timeout = Duration::from_secs(MODPACK_FILE_STALL_TIMEOUT_SECS);
    options
}

fn build_download_request(index: usize, task: &ModpackDownloadTask) -> DownloadRequest {
    let candidates = task
        .urls
        .iter()
        .cloned()
        .map(DownloadCandidate::new)
        .collect::<Vec<_>>();
    let mut request = DownloadRequest::new(format!("modpack-file-{index}"), candidates, &task.dest);
    if let Some(sha1) = task.sha1.as_deref().filter(|sha1| !sha1.trim().is_empty()) {
        request = request.with_expected_sha1(sha1.trim().to_string());
    }
    request
}

fn mark_modpack_task_done(
    request_id: &str,
    info_by_id: &HashMap<String, ModpackTaskInfo>,
    categories: &HashMap<String, CategoryCounter>,
    completed: &Mutex<HashSet<String>>,
) {
    let mut completed = completed.lock().unwrap();
    if !completed.insert(request_id.to_string()) {
        return;
    }
    drop(completed);

    if let Some(info) = info_by_id.get(request_id) {
        if let Some(counter) = categories.get(&info.category) {
            counter.done.fetch_add(1, Ordering::Relaxed);
        }
    }
}

fn is_cancel_related_error(error: &str) -> bool {
    error.contains("cancelled")
        || error.contains("download cancelled")
        || error.contains("用户取消")
}

fn ensure_modpack_not_cancelled(cancel_name: &str) -> Result<(), String> {
    if crate::instance::is_cancelled(cancel_name) {
        Err("用户取消安装".to_string())
    } else {
        Ok(())
    }
}

fn fetch_curseforge_json_controlled(
    client: &reqwest::blocking::Client,
    method: reqwest::Method,
    url: &str,
    body: Option<serde_json::Value>,
    cancel_name: &str,
    throttle: &CurseForgeApiThrottle,
    attempts: usize,
) -> Result<serde_json::Value, String> {
    let attempts = attempts.max(1);
    let mut last_error = String::new();
    for attempt in 0..attempts {
        throttle.wait(cancel_name)?;
        match fetch_json_request_cancelable(
            client,
            method.clone(),
            url,
            vec![("x-api-key".to_string(), cf_api_key())],
            body.clone(),
            cancel_name,
            Duration::from_secs(60),
        ) {
            Ok(json) => return Ok(json),
            Err(error) if is_cancel_related_error(&error) => return Err(error),
            Err(error) => {
                throttle.record_failure(&error);
                last_error = error;
                if attempt + 1 < attempts && !curseforge_api_requires_cooldown(&last_error) {
                    let retry_until = Instant::now() + Duration::from_secs(2);
                    while Instant::now() < retry_until {
                        ensure_modpack_not_cancelled(cancel_name)?;
                        std::thread::sleep(Duration::from_millis(100));
                    }
                }
            }
        }
    }
    Err(last_error)
}

struct CancelableProbeResponse {
    status: reqwest::StatusCode,
    final_url: reqwest::Url,
    content_disposition: Option<String>,
}

fn fetch_response_cancelable(
    url: &str,
    cancel_name: &str,
    timeout: Duration,
) -> Result<CancelableProbeResponse, String> {
    ensure_modpack_not_cancelled(cancel_name)?;
    let cancel = crate::instance::install_cancel_flag(cancel_name)
        .ok_or_else(|| "安装任务已结束".to_string())?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("创建探测运行时失败: {}", error))?;
    let client = reqwest::Client::builder()
        .connect_timeout(timeout.min(Duration::from_secs(10)))
        .build()
        .map_err(|error| format!("创建探测客户端失败: {}", error))?;
    let url = url.to_string();

    runtime.block_on(async move {
        let request = client.get(&url).timeout(timeout).send();
        tokio::pin!(request);
        loop {
            tokio::select! {
                result = &mut request => {
                    let response = result.map_err(|error| error.to_string())?;
                    return Ok(CancelableProbeResponse {
                        status: response.status(),
                        final_url: response.url().clone(),
                        content_disposition: response.headers()
                            .get("content-disposition")
                            .and_then(|value| value.to_str().ok())
                            .map(str::to_string),
                    });
                }
                _ = tokio::time::sleep(Duration::from_millis(50)) => {
                    if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                        return Err("用户取消安装".to_string());
                    }
                }
            }
        }
    })
}

fn abort_downloaders_after_stage_failure(cancel_name: &str) -> bool {
    let Some(cancel_flag) = crate::instance::install_cancel_flag(cancel_name) else {
        return false;
    };
    if cancel_flag.load(std::sync::atomic::Ordering::Relaxed) {
        return false;
    }

    // 标志保持到并行线程全部退出，确保稍后才注册的原版下载器也会立即停止。
    crate::instance::cancel_install_task(cancel_name).is_ok()
}

fn download_modpack_tasks_with_engine(
    tasks: Vec<ModpackDownloadTask>,
    categories: Arc<HashMap<String, CategoryCounter>>,
    cancel_name: String,
) -> Result<(), ModpackDownloadError> {
    if tasks.is_empty() {
        return Ok(());
    }

    let mut requests = Vec::with_capacity(tasks.len());
    let mut info_by_id = HashMap::new();
    for (index, task) in tasks.iter().enumerate() {
        let request = build_download_request(index, task);
        let first_url = task.urls.first().cloned().unwrap_or_default();
        info_by_id.insert(
            request.id.clone(),
            ModpackTaskInfo {
                category: classify_modpack_path(&task.dest).to_string(),
                first_url,
            },
        );
        requests.push(request);
    }

    let options = modpack_download_options();
    let pool = install_download_pool(&cancel_name, options.max_global_connections);
    let manager = DownloadManager::with_options_and_pool(options, pool)
        .map_err(|message| ModpackDownloadError::new(0, info_by_id.len(), message))?;
    let _manager_registration = register_download_manager(&cancel_name, &manager);

    let info_for_event = Arc::new(info_by_id.clone());
    let completed_for_event = Arc::new(Mutex::new(HashSet::new()));
    let categories_for_event = categories.clone();
    let completed_event = completed_for_event.clone();
    let outcomes = manager.download_many(requests, move |event| match event {
        DownloadEvent::FileFinished(result) => {
            mark_modpack_task_done(
                &result.request_id,
                &info_for_event,
                &categories_for_event,
                &completed_event,
            );
        }
        DownloadEvent::FileFailed {
            request_id, error, ..
        } => {
            if !is_cancel_related_error(&error) {
                let first_url = info_for_event
                    .get(&request_id)
                    .map(|info| info.first_url.as_str())
                    .unwrap_or("");
                eprintln!("[modpack] 文件下载失败事件: {} -> {}", first_url, error);
            }
        }
        _ => {}
    });

    let mut errors = Vec::new();
    for outcome in outcomes {
        match outcome {
            DownloadOutcome::Finished(_) => {}
            DownloadOutcome::Failed {
                request_id, error, ..
            } => {
                let first_url = info_by_id
                    .get(&request_id)
                    .map(|info| info.first_url.clone())
                    .unwrap_or_default();
                errors.push(format!("{}: {}", first_url, error));
            }
        }
    }

    let was_cancelled = crate::instance::is_cancelled(&cancel_name);
    if errors.is_empty() {
        if was_cancelled {
            return Err(ModpackDownloadError::cancelled(
                info_by_id.len(),
                "用户取消下载",
            ));
        }
        return Ok(());
    }
    if was_cancelled && errors.iter().all(|error| is_cancel_related_error(error)) {
        return Err(ModpackDownloadError::cancelled(
            info_by_id.len(),
            "用户取消下载",
        ));
    }

    let total_files = info_by_id.len();
    for error in &errors {
        eprintln!("[modpack] 失败: {}", error);
    }
    let sample = errors
        .iter()
        .take(5)
        .cloned()
        .collect::<Vec<_>>()
        .join("; ");
    let message = format!(
        "整合包文件下载失败: {}/{} 个文件失败。{}",
        errors.len(),
        total_files,
        sample
    );
    Err(ModpackDownloadError::new(
        errors.len(),
        total_files,
        message,
    ))
}

fn curseforge_target_dir(
    class_id: Option<u64>,
    item: &serde_json::Value,
    file_name: &str,
    inst_dir: &Path,
) -> (PathBuf, &'static str) {
    match class_id {
        Some(6) => (inst_dir.join("mods"), "mod"),
        Some(12) => {
            let dir = inst_dir.join("resourcepacks");
            std::fs::create_dir_all(&dir).ok();
            (dir, "材质包")
        }
        Some(6552) => {
            let dir = inst_dir.join("shaderpacks");
            std::fs::create_dir_all(&dir).ok();
            (dir, "光影")
        }
        Some(17) => {
            let dir = inst_dir.join("saves");
            std::fs::create_dir_all(&dir).ok();
            (dir, "存档")
        }
        _ => detect_target_dir(item, file_name, inst_dir),
    }
}

fn resolve_curseforge_file_task(
    client: &reqwest::blocking::Client,
    project_id: u32,
    file_id: u32,
    class_id: Option<u64>,
    inst_dir: &Path,
    cancel_name: &str,
    throttle: &CurseForgeApiThrottle,
) -> Result<ModpackDownloadTask, String> {
    ensure_modpack_not_cancelled(cancel_name)?;
    let api_url = format!(
        "https://api.curseforge.com/v1/mods/{}/files/{}",
        project_id, file_id
    );
    let json = fetch_curseforge_json_controlled(
        client,
        reqwest::Method::GET,
        &api_url,
        None,
        cancel_name,
        throttle,
        1,
    )
    .map_err(|error| {
        format!(
            "CurseForge 文件解析失败: {} p={} f={}",
            error, project_id, file_id
        )
    })?;
    let item = &json["data"];
    let raw_file_name = item["fileName"]
        .as_str()
        .filter(|name| !name.trim().is_empty())
        .ok_or_else(|| format!("CurseForge 文件缺少文件名: p={} f={}", project_id, file_id))?;
    let file_name = safe_path_name(raw_file_name, "文件名")?;
    let sha1 = sha1_from_curseforge_hashes(&item["hashes"]);
    let (target_dir, file_type) = curseforge_target_dir(class_id, item, &file_name, inst_dir);
    if file_type != "mod" {
        eprintln!(
            "[cf] {} → {} ({}) [classId={}]",
            file_name,
            target_dir.display(),
            file_type,
            class_id.unwrap_or(0)
        );
    }
    let dest = target_dir.join(&file_name);
    let mut urls = with_mod_mirrors(cf_cdn_urls(file_id, &file_name));
    if let Some(download_url) = item["downloadUrl"]
        .as_str()
        .filter(|url| !url.trim().is_empty())
    {
        append_unique_urls(&mut urls, vec![download_url.to_string()]);
    }
    let download_url_api = format!(
        "https://api.curseforge.com/v1/mods/{}/files/{}/download-url",
        project_id, file_id
    );
    ensure_modpack_not_cancelled(cancel_name)?;
    match fetch_curseforge_json_controlled(
        client,
        reqwest::Method::GET,
        &download_url_api,
        None,
        cancel_name,
        throttle,
        1,
    ) {
        Ok(json) => {
            if let Some(url) = json["data"].as_str().filter(|url| !url.trim().is_empty()) {
                append_unique_urls(&mut urls, vec![url.to_string()]);
            }
        }
        Err(error) if crate::instance::is_cancelled(cancel_name) => return Err(error),
        Err(error) => {
            eprintln!("[cf] 下载地址 API 失败，继续使用 CDN 候选: {}", error);
        }
    }
    ensure_modpack_not_cancelled(cancel_name)?;
    if urls.is_empty() {
        return Err(format!(
            "CurseForge 文件没有可用下载地址: p={} f={}",
            project_id, file_id
        ));
    }

    Ok(ModpackDownloadTask {
        urls,
        dest,
        sha1: sha1.clone(),
    })
}

fn resolve_curseforge_blind_file_task(
    project_id: u32,
    file_id: u32,
    inst_dir: &Path,
    cancel_name: &str,
) -> Result<ModpackDownloadTask, String> {
    ensure_modpack_not_cancelled(cancel_name)?;
    let (first, second) = curseforge_cdn_parts(file_id);
    let probe_urls = with_mod_mirrors(vec![
        format!("https://mediafilez.forgecdn.net/files/{}/{}", first, second),
        format!("https://edge.forgecdn.net/files/{}/{}", first, second),
    ]);
    let mut last_err = String::new();
    let mut resolved_dest: Option<PathBuf> = None;
    let mut resolved_urls = Vec::new();
    for url in probe_urls {
        ensure_modpack_not_cancelled(cancel_name)?;
        match fetch_response_cancelable(&url, cancel_name, Duration::from_secs(60)) {
            Ok(resp) if resp.status.is_success() => {
                ensure_modpack_not_cancelled(cancel_name)?;
                let final_url = resp.final_url.to_string();
                let file_name = response_file_name(
                    &resp.final_url,
                    resp.content_disposition.as_deref(),
                    &second,
                )
                .unwrap_or_else(|| format!("{}-{}.jar", project_id, file_id));
                let file_name = safe_path_name(&file_name, "文件名")?;
                let empty_json = serde_json::json!({});
                let (target_dir, file_type) = detect_target_dir(&empty_json, &file_name, inst_dir);
                if file_type != "mod" {
                    eprintln!(
                        "[cf] CDN盲猜分类: {} → {} ({})",
                        file_name,
                        target_dir.display(),
                        file_type
                    );
                }
                if resolved_dest.is_none() {
                    resolved_dest = Some(target_dir.join(file_name));
                }
                append_unique_urls(&mut resolved_urls, with_mod_mirrors(vec![final_url]));
            }
            Ok(resp) => {
                last_err = format!("{} HTTP {}", url, resp.status);
            }
            Err(e) => {
                last_err = format!("{}: {}", url, e);
            }
        }
        ensure_modpack_not_cancelled(cancel_name)?;
    }
    if let Some(dest) = resolved_dest {
        if !resolved_urls.is_empty() {
            return Ok(ModpackDownloadTask {
                urls: resolved_urls,
                dest,
                sha1: None,
            });
        }
    }
    Err(format!(
        "CurseForge CDN 盲猜解析失败: p={} f={} ({})",
        project_id, file_id, last_err
    ))
}

fn curseforge_cdn_parts(file_id: u32) -> (String, String) {
    ((file_id / 1000).to_string(), (file_id % 1000).to_string())
}

fn append_missing_curseforge_file_ids(
    requested: &[u32],
    returned: &std::collections::HashSet<u32>,
    file_map: &std::collections::HashMap<u32, (u32, u32)>,
    unresolved: &mut Vec<UnresolvedCurseForgeFile>,
) {
    for file_id in requested {
        if !returned.contains(file_id) {
            if let Some(&(project_id, file_id)) = file_map.get(file_id) {
                unresolved.push(UnresolvedCurseForgeFile {
                    project_id,
                    file_id,
                    try_individual_api: true,
                });
            }
        }
    }
}

fn push_unresolved_curseforge_file(
    unresolved: &mut Vec<UnresolvedCurseForgeFile>,
    project_id: u32,
    file_id: u32,
    try_individual_api: bool,
) {
    if let Some(existing) = unresolved
        .iter_mut()
        .find(|item| item.project_id == project_id && item.file_id == file_id)
    {
        existing.try_individual_api |= try_individual_api;
        return;
    }
    unresolved.push(UnresolvedCurseForgeFile {
        project_id,
        file_id,
        try_individual_api,
    });
}

fn resolve_unresolved_curseforge_files(
    client: Option<&reqwest::blocking::Client>,
    unresolved: &[UnresolvedCurseForgeFile],
    pid_class: &HashMap<u32, u64>,
    inst_dir: &Path,
    cancel_name: &str,
    throttle: &CurseForgeApiThrottle,
) -> Result<Vec<ModpackDownloadTask>, String> {
    let mut resolved = Vec::with_capacity(unresolved.len());
    for chunk in unresolved.chunks(CF_FALLBACK_CONCURRENCY) {
        ensure_modpack_not_cancelled(cancel_name)?;
        let results = std::thread::scope(|scope| {
            let handles = chunk
                .iter()
                .map(|entry| {
                    scope.spawn(move || {
                        if entry.try_individual_api {
                            let Some(client) = client else {
                                return resolve_curseforge_blind_file_task(
                                    entry.project_id,
                                    entry.file_id,
                                    inst_dir,
                                    cancel_name,
                                );
                            };
                            match resolve_curseforge_file_task(
                                client,
                                entry.project_id,
                                entry.file_id,
                                pid_class.get(&entry.project_id).copied(),
                                inst_dir,
                                cancel_name,
                                throttle,
                            ) {
                                Ok(task) => return Ok(task),
                                Err(error) if is_cancel_related_error(&error) => return Err(error),
                                Err(error) => {
                                    eprintln!("[cf] 单文件 API 解析失败，改用 CDN 探测: {}", error)
                                }
                            }
                        }
                        resolve_curseforge_blind_file_task(
                            entry.project_id,
                            entry.file_id,
                            inst_dir,
                            cancel_name,
                        )
                    })
                })
                .collect::<Vec<_>>();
            handles
                .into_iter()
                .map(|handle| {
                    handle
                        .join()
                        .unwrap_or_else(|_| Err("CurseForge 解析线程异常退出".to_string()))
                })
                .collect::<Vec<_>>()
        });
        for result in results {
            resolved.push(result?);
        }
    }
    Ok(resolved)
}

fn response_file_name(
    final_url: &reqwest::Url,
    content_disposition: Option<&str>,
    fallback_path: &str,
) -> Option<String> {
    let from_url = final_url
        .path_segments()
        .and_then(|mut parts| parts.next_back())
        .map(|name| name.split('?').next().unwrap_or(name))
        .map(percent_decode_simple)
        .filter(|name| !name.is_empty() && name != fallback_path);
    if from_url.is_some() {
        return from_url;
    }

    content_disposition
        .and_then(|value| {
            value
                .split("filename=")
                .nth(1)
                .or_else(|| value.split("filename*=UTF-8''").nth(1))
        })
        .map(|name| percent_decode_simple(name.trim_matches('"')))
        .filter(|name| !name.is_empty())
}

fn percent_decode_simple(value: &str) -> String {
    let mut out = Vec::new();
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let Ok(hex) = u8::from_str_radix(
                std::str::from_utf8(&bytes[index + 1..index + 3]).unwrap_or(""),
                16,
            ) {
                out.push(hex);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| value.to_string())
}

pub fn do_install_modpack_inner(
    app: &tauri::AppHandle,
    zip_file: &std::path::Path,
    game_dir_input: &str,
    java_path: &str,
    use_mirror: bool,
    meta: &ModpackMeta,
    inst_dir: &std::path::Path,
    game_dir: &std::path::Path,
    display_name: &str,
    cancel_name: &str,
) -> Result<String, String> {
    use crate::installer::fabric;
    use crate::installer::forge;
    use crate::installer::neoforge;
    use crate::installer::quilt;
    use crate::installer::vanilla;

    let inst_name = inst_dir
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.to_string())
        .ok_or_else(|| "整合包名称无效".to_string())?;
    if crate::instance::is_cancelled(cancel_name) {
        return Err("用户取消安装".to_string());
    }

    // 1. 安装基础游戏
    let http = build_http_client(15, 180, 8)?;

    // 游戏元数据与基础文件严格服从用户选择；Mod 下载继续使用自己的候选源顺序。
    let mirror_manifest_url = "https://bmclapi2.bangbang93.com/mc/game/version_manifest_v2.json";
    let official_manifest_url = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";

    emit_progress(
        app,
        display_name,
        "meta",
        0,
        1,
        if use_mirror {
            "获取版本清单（镜像优先）..."
        } else {
            "获取版本清单（官方优先）..."
        },
    );
    let manifest_candidates = if use_mirror {
        [
            (mirror_manifest_url, "镜像源"),
            (official_manifest_url, "官方源"),
        ]
    } else {
        [
            (official_manifest_url, "官方源"),
            (mirror_manifest_url, "镜像源"),
        ]
    };
    let mut manifest = None;
    let mut manifest_errors = Vec::new();
    for (index, (url, label)) in manifest_candidates.iter().enumerate() {
        match fetch_json_cancelable(&http, url, cancel_name, std::time::Duration::from_secs(30)) {
            Ok(value) => {
                eprintln!("[modpack] {}获取清单成功", label);
                manifest = Some(value);
                break;
            }
            Err(error) if error.contains("用户取消") => return Err(error),
            Err(error) => {
                manifest_errors.push(format!("{}: {}", label, error));
                if index + 1 < manifest_candidates.len() {
                    emit_progress(
                        app,
                        display_name,
                        "meta",
                        0,
                        1,
                        &format!("{}不可用，切换下一个来源...", label),
                    );
                }
            }
        }
    }
    let manifest = manifest.ok_or_else(|| {
        format!(
            "获取版本清单失败，所有来源均不可用: {}",
            manifest_errors.join(" | ")
        )
    })?;
    let meta_url = manifest["versions"]
        .as_array()
        .and_then(|arr| {
            arr.iter()
                .find(|v| v["id"].as_str() == Some(&meta.mc_version))
        })
        .and_then(|v| v["url"].as_str())
        .ok_or_else(|| format!("找不到 MC 版本: {}", meta.mc_version))?
        .to_string();
    if inst_name.is_empty() {
        return Err("整合包名称无效".to_string());
    }
    std::fs::create_dir_all(inst_dir).map_err(|e| e.to_string())?;
    let inst_json_path = version_json_path(inst_dir, &inst_name);

    if crate::instance::is_cancelled(cancel_name) {
        return Err("用户取消安装".to_string());
    }

    // ===== 先解析整合包文件任务，避免后台线程启动后解析失败留下残留 =====
    let tasks: Vec<ModpackDownloadTask>;
    {
        let mods_dir = inst_dir.join("mods");
        std::fs::create_dir_all(&mods_dir).ok();

        tasks = match &meta.kind {
            ModpackKind::Modrinth { files } => files
                .iter()
                .map(|f| {
                    let dest = safe_join(inst_dir, &f.path)?;
                    Ok(ModpackDownloadTask {
                        urls: with_mod_mirrors(f.urls.clone()),
                        dest,
                        sha1: f.sha1.clone(),
                    })
                })
                .collect::<Result<Vec<_>, String>>()?,
            ModpackKind::CurseForge { files, .. } => {
                let file_ids: Vec<u32> = files.iter().map(|f| f.file_id).collect();
                let project_ids: Vec<u32> = files.iter().map(|f| f.project_id).collect();
                let file_map: std::collections::HashMap<u32, (u32, u32)> = files
                    .iter()
                    .map(|f| (f.file_id, (f.project_id, f.file_id)))
                    .collect();

                let api_client = reqwest::blocking::Client::builder()
                    .connect_timeout(std::time::Duration::from_secs(15))
                    .timeout(std::time::Duration::from_secs(60))
                    .user_agent("OAOI-Launcher/1.0")
                    .build()
                    .ok();
                let api_throttle = Arc::new(CurseForgeApiThrottle::default());

                // 先批量获取项目 classId（用于区分 mod/材质包/光影包）
                let mut pid_class: std::collections::HashMap<u32, u64> =
                    std::collections::HashMap::new();
                if let Some(client) = &api_client {
                    let unique_pids: Vec<u32> = {
                        let mut s: std::collections::HashSet<u32> =
                            std::collections::HashSet::new();
                        project_ids
                            .iter()
                            .filter(|p| s.insert(**p))
                            .copied()
                            .collect()
                    };
                    eprintln!(
                        "[cf] 批量获取 {} 个项目 classId (POST /v1/mods)...",
                        unique_pids.len()
                    );
                    for chunk in unique_pids.chunks(50) {
                        ensure_modpack_not_cancelled(cancel_name)?;
                        let body = serde_json::json!({ "modIds": chunk, "filterPcOnly": true });
                        let response = fetch_curseforge_json_controlled(
                            client,
                            reqwest::Method::POST,
                            "https://api.curseforge.com/v1/mods",
                            Some(body),
                            cancel_name,
                            &api_throttle,
                            CF_API_RETRY_ATTEMPTS,
                        );
                        match response {
                            Ok(json) => {
                                if let Some(data) = json["data"].as_array() {
                                    for proj in data {
                                        ensure_modpack_not_cancelled(cancel_name)?;
                                        let pid = proj["id"].as_u64().unwrap_or(0) as u32;
                                        let cid = proj["classId"].as_u64().unwrap_or(0);
                                        if cid > 0 {
                                            pid_class.insert(pid, cid);
                                        }
                                    }
                                }
                            }
                            Err(error) if crate::instance::is_cancelled(cancel_name) => {
                                return Err(error);
                            }
                            Err(error) => eprintln!("[cf] 项目信息请求失败: {}", error),
                        }
                    }
                    eprintln!(
                        "[cf] 项目 classId 映射: {} 个 (6=Mod, 12=材质包, 6552=光影)",
                        pid_class.len()
                    );
                }

                eprintln!(
                    "[cf] 批量获取 {} 个文件信息 (POST /v1/mods/files)...",
                    file_ids.len()
                );

                let mut resolved: Vec<ModpackDownloadTask> = Vec::new();
                let mut unresolved: Vec<UnresolvedCurseForgeFile> = Vec::new();

                if let Some(client) = &api_client {
                    for chunk in file_ids.chunks(500) {
                        ensure_modpack_not_cancelled(cancel_name)?;
                        let body = serde_json::json!({ "fileIds": chunk });
                        let response = fetch_curseforge_json_controlled(
                            client,
                            reqwest::Method::POST,
                            "https://api.curseforge.com/v1/mods/files",
                            Some(body),
                            cancel_name,
                            &api_throttle,
                            CF_API_RETRY_ATTEMPTS,
                        );
                        match response {
                            Ok(json) => {
                                let requested_ids = chunk
                                    .iter()
                                    .copied()
                                    .collect::<std::collections::HashSet<_>>();
                                let mut returned_ids = std::collections::HashSet::new();
                                if let Some(data) = json["data"].as_array() {
                                    for item in data {
                                        ensure_modpack_not_cancelled(cancel_name)?;
                                        let fid = item["id"].as_u64().unwrap_or(0) as u32;
                                        if !requested_ids.contains(&fid)
                                            || !returned_ids.insert(fid)
                                        {
                                            continue;
                                        }
                                        let raw_fname =
                                            item["fileName"].as_str().unwrap_or("").to_string();
                                        let dl =
                                            item["downloadUrl"].as_str().unwrap_or("").to_string();
                                        let pid = file_map.get(&fid).map(|x| x.0).unwrap_or(0);
                                        let sha1 = sha1_from_curseforge_hashes(&item["hashes"]);

                                        if raw_fname.is_empty() {
                                            push_unresolved_curseforge_file(
                                                &mut unresolved,
                                                pid,
                                                fid,
                                                true,
                                            );
                                            continue;
                                        }
                                        let fname = match safe_path_name(&raw_fname, "文件名") {
                                            Ok(name) => name,
                                            Err(_) => {
                                                push_unresolved_curseforge_file(
                                                    &mut unresolved,
                                                    pid,
                                                    fid,
                                                    true,
                                                );
                                                continue;
                                            }
                                        };

                                        if fname.is_empty() {
                                            push_unresolved_curseforge_file(
                                                &mut unresolved,
                                                pid,
                                                fid,
                                                true,
                                            );
                                            continue;
                                        }

                                        // 优先用项目 classId 判断目录（最可靠）
                                        let (target_dir, file_type) =
                                            if let Some(&cid) = pid_class.get(&pid) {
                                                match cid {
                                                    6 => (inst_dir.join("mods"), "mod"),
                                                    12 => {
                                                        let d = inst_dir.join("resourcepacks");
                                                        std::fs::create_dir_all(&d).ok();
                                                        (d, "材质包")
                                                    }
                                                    6552 => {
                                                        let d = inst_dir.join("shaderpacks");
                                                        std::fs::create_dir_all(&d).ok();
                                                        (d, "光影")
                                                    }
                                                    17 => {
                                                        let d = inst_dir.join("saves");
                                                        std::fs::create_dir_all(&d).ok();
                                                        (d, "存档")
                                                    }
                                                    _ => detect_target_dir(item, &fname, inst_dir),
                                                }
                                            } else {
                                                detect_target_dir(item, &fname, inst_dir)
                                            };
                                        if file_type != "mod" {
                                            eprintln!(
                                                "[cf] {} → {} ({}) [classId={}]",
                                                fname,
                                                target_dir.display(),
                                                file_type,
                                                pid_class.get(&pid).unwrap_or(&0)
                                            );
                                        }
                                        let dest = target_dir.join(&fname);
                                        let mut urls = with_mod_mirrors(cf_cdn_urls(fid, &fname));
                                        if !dl.is_empty() {
                                            append_unique_urls(&mut urls, vec![dl]);
                                        }
                                        if urls.is_empty() {
                                            push_unresolved_curseforge_file(
                                                &mut unresolved,
                                                pid,
                                                fid,
                                                true,
                                            );
                                        } else {
                                            resolved.push(ModpackDownloadTask {
                                                urls,
                                                dest,
                                                sha1: sha1.clone(),
                                            });
                                        }
                                    }
                                }
                                // CF 偶尔会返回 200 但漏掉部分 ID，漏项必须继续走单文件候选解析。
                                append_missing_curseforge_file_ids(
                                    chunk,
                                    &returned_ids,
                                    &file_map,
                                    &mut unresolved,
                                );
                            }
                            Err(error) => {
                                if crate::instance::is_cancelled(cancel_name) {
                                    return Err(error);
                                }
                                eprintln!("[cf] 批量 API 失败, 回退到逐个下载: {}", error);
                                for id in chunk {
                                    if let Some(&(pid, fid)) = file_map.get(id) {
                                        push_unresolved_curseforge_file(
                                            &mut unresolved,
                                            pid,
                                            fid,
                                            false,
                                        );
                                    }
                                }
                            }
                        }
                    }
                } else {
                    for f in files {
                        ensure_modpack_not_cancelled(cancel_name)?;
                        push_unresolved_curseforge_file(
                            &mut unresolved,
                            f.project_id,
                            f.file_id,
                            false,
                        );
                    }
                }

                eprintln!(
                    "[cf] 批量解析: {} 已解析, {} 需单独下载",
                    resolved.len(),
                    unresolved.len()
                );

                // 批量接口触发限流时先完成冷却，再以固定并发解析剩余文件。
                api_throttle.wait(cancel_name)?;
                resolved.extend(resolve_unresolved_curseforge_files(
                    api_client.as_ref(),
                    &unresolved,
                    &pid_class,
                    inst_dir,
                    cancel_name,
                    &api_throttle,
                )?);

                resolved
            }
        };
    }

    // 按类型统计
    let mut category_totals: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for task in &tasks {
        let cat = classify_modpack_path(&task.dest).to_string();
        *category_totals.entry(cat).or_insert(0) += 1;
    }

    // 创建分类计数器
    let categories: std::sync::Arc<std::collections::HashMap<String, CategoryCounter>> =
        std::sync::Arc::new(
            category_totals
                .iter()
                .map(|(k, &v)| {
                    (
                        k.clone(),
                        CategoryCounter {
                            done: AtomicUsize::new(0),
                            total: v,
                        },
                    )
                })
                .collect(),
        );

    // 发送初始进度（立刻让前端知道所有类型和总数）
    let stage_names: std::collections::HashMap<&str, &str> = [
        ("mods", "Mod 文件"),
        ("resourcepacks", "材质包"),
        ("shaderpacks", "光影包"),
        ("config", "配置文件"),
        ("other", "其他文件"),
    ]
    .iter()
    .cloned()
    .collect();
    for (cat, counter) in categories.iter() {
        let cat_str = cat.as_str();
        let label = stage_names.get(cat_str).copied().unwrap_or(cat_str);
        eprintln!("[modpack] 分类: {} = {} 个文件", label, counter.total);
        emit_progress(
            app,
            display_name,
            cat,
            0,
            counter.total,
            &format!("{} 0/{}", label, counter.total),
        );
    }

    if crate::instance::is_cancelled(cancel_name) {
        return Err("用户取消安装".to_string());
    }

    // 基础游戏和 Loader 使用用户选择的来源顺序，Mod 文件保留自己的候选源逻辑。
    // 文件任务解析成功后，再并行启动基础游戏、整合包文件和 loader。
    let vanilla_handle = {
        let app = app.clone();
        let display_name = display_name.to_string();
        let cancel_name = cancel_name.to_string();
        let version_name = inst_name.clone();
        let mc_version = meta.mc_version.clone();
        let meta_url = meta_url.clone();
        let game_dir = game_dir.to_path_buf();
        let inst_dir = inst_dir.to_path_buf();
        let http = http.clone();
        std::thread::spawn(move || {
            vanilla::install_vanilla_with_task_names(
                &app,
                &display_name,
                &version_name,
                &cancel_name,
                &mc_version,
                &meta_url,
                &game_dir,
                &inst_dir,
                &http,
                use_mirror,
            )
        })
    };

    let mod_download_handle = {
        let categories = categories.clone();
        let cancel_name = cancel_name.to_string();
        std::thread::spawn(move || {
            download_modpack_tasks_with_engine(tasks, categories, cancel_name)
        })
    };

    // 启动进度汇报线程（每 500ms 汇报所有类型的进度）
    let progress_stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let progress_stop2 = progress_stop.clone();
    let progress_cats = categories.clone();
    let progress_app = app.clone();
    let progress_name = display_name.to_string();
    let progress_thread = std::thread::spawn(move || loop {
        if progress_stop2.load(std::sync::atomic::Ordering::Relaxed) {
            break;
        }
        let mut all_done = true;
        for (cat, counter) in progress_cats.iter() {
            let finished = counter.done.load(std::sync::atomic::Ordering::Relaxed);
            emit_progress(
                &progress_app,
                &progress_name,
                cat,
                finished,
                counter.total,
                &format!("{}/{}", finished, counter.total),
            );
            if finished < counter.total {
                all_done = false;
            }
        }
        if all_done {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    });

    // ===== 安装 loader（同时 vanilla 和 mod 文件都在后台下载中） =====
    let loader_handle = if meta.loader_version.is_empty()
        || !matches!(
            meta.loader_type.as_str(),
            "fabric" | "quilt" | "forge" | "neoforge"
        ) {
        None
    } else {
        let app = app.clone();
        let display_name = display_name.to_string();
        let cancel_name = cancel_name.to_string();
        let version_name = inst_name.clone();
        let mc_version = meta.mc_version.clone();
        let loader_type = meta.loader_type.clone();
        let loader_version = meta.loader_version.clone();
        let game_dir_input = game_dir_input.to_string();
        let java_path = java_path.to_string();
        let game_dir = game_dir.to_path_buf();
        let inst_dir = inst_dir.to_path_buf();
        let http = http.clone();
        Some(std::thread::spawn(move || {
            let resolved_java: String;
            let mut java_error: Option<String> = None;
            let effective_java = if !java_path.is_empty() {
                java_path.as_str()
            } else {
                let required_major = super::get_required_java_major(&mc_version);
                let javas = crate::java_detect::find_java_blocking(Some(game_dir_input.clone()));
                if let Some(j) = javas.iter().find(|j| j.major == required_major) {
                    resolved_java = j.path.clone();
                    &resolved_java
                } else {
                    if required_major == 7 {
                        java_error = Some(
                            "这个整合包需要 Java 7，请先手动安装 Java 7，或在设置里选择 Java 7 的 java.exe"
                                .to_string(),
                        );
                        resolved_java = String::new();
                        &resolved_java
                    } else {
                        emit_progress(
                            &app,
                            &display_name,
                            "java",
                            0,
                            1,
                            &format!("正在下载 Java {}...", required_major),
                        );
                        match crate::java_download::download_java_sync_cancelable(
                            required_major,
                            &game_dir_input,
                            Some(&cancel_name),
                        ) {
                            Ok(p) => {
                                resolved_java = p;
                                &resolved_java
                            }
                            Err(e) => {
                                java_error = Some(format!(
                                    "找不到 Java {}，自动下载失败: {}",
                                    required_major, e
                                ));
                                resolved_java = String::new();
                                &resolved_java
                            }
                        }
                    }
                }
            };

            if let Some(e) = java_error {
                return Err(e);
            }

            let mut loader_json = empty_loader_json();
            match loader_type.as_str() {
                "fabric" => fabric::install_fabric_with_cancel_name(
                    &app,
                    &display_name,
                    &cancel_name,
                    &mc_version,
                    &loader_version,
                    &game_dir,
                    &inst_dir,
                    &http,
                    use_mirror,
                    &mut loader_json,
                    false,
                )?,
                "quilt" => quilt::install_quilt_with_cancel_name(
                    &app,
                    &display_name,
                    &cancel_name,
                    &mc_version,
                    &loader_version,
                    &game_dir,
                    &inst_dir,
                    &http,
                    use_mirror,
                    &mut loader_json,
                )?,
                "forge" => forge::install_forge_with_task_names(
                    &app,
                    &display_name,
                    &version_name,
                    &cancel_name,
                    &mc_version,
                    &loader_version,
                    &game_dir,
                    &inst_dir,
                    &http,
                    effective_java,
                    use_mirror,
                    &mut loader_json,
                )?,
                "neoforge" => neoforge::install_neoforge_with_task_names(
                    &app,
                    &display_name,
                    &version_name,
                    &cancel_name,
                    &mc_version,
                    &loader_version,
                    &game_dir,
                    &inst_dir,
                    &http,
                    effective_java,
                    use_mirror,
                    &mut loader_json,
                )?,
                _ => {}
            }
            Ok(loader_json)
        }))
    };

    // 注意: 版本 JSON 的写入移到最后（推荐内存计算后一次性写入）

    // ===== 等待三条下载线完成：任意一条失败就立刻取消其它下载器 =====
    let mut vanilla_handle = Some(vanilla_handle);
    let mut mod_download_handle = Some(mod_download_handle);
    let mut loader_handle = loader_handle;
    let mut vanilla_result: Option<Result<serde_json::Value, String>> = None;
    let mut mod_download_result: Option<Result<(), ModpackDownloadError>> = None;
    let mut loader_result: Option<Result<serde_json::Value, String>> = None;
    let mut first_error: Option<String> = None;
    let mut coordinated_abort = false;

    while vanilla_handle.is_some() || mod_download_handle.is_some() || loader_handle.is_some() {
        let mut progressed = false;

        if vanilla_handle
            .as_ref()
            .is_some_and(|handle| handle.is_finished())
        {
            let handle = vanilla_handle.take().unwrap();
            let result = match handle.join() {
                Ok(result) => result,
                Err(_) => Err("基础游戏安装线程异常退出".to_string()),
            };
            if let Err(error) = &result {
                if first_error.is_none() {
                    eprintln!("[modpack] 基础游戏阶段失败，取消其它下载: {}", error);
                    first_error = Some(error.clone());
                    coordinated_abort = abort_downloaders_after_stage_failure(cancel_name);
                }
            }
            vanilla_result = Some(result);
            progressed = true;
        }

        if mod_download_handle
            .as_ref()
            .is_some_and(|handle| handle.is_finished())
        {
            let handle = mod_download_handle.take().unwrap();
            let total_files: usize = categories.iter().map(|(_, c)| c.total).sum();
            let result = match handle.join() {
                Ok(result) => result,
                Err(_) => Err(ModpackDownloadError::new(
                    0,
                    total_files,
                    "整合包文件下载线程异常退出",
                )),
            };
            if let Err(error) = &result {
                if first_error.is_none() {
                    eprintln!(
                        "[modpack] 整合包文件下载阶段失败，取消其它下载: {}",
                        error.message
                    );
                    first_error = Some(error.message.clone());
                    coordinated_abort = abort_downloaders_after_stage_failure(cancel_name);
                }
            }
            mod_download_result = Some(result);
            progressed = true;
        }

        if loader_handle
            .as_ref()
            .is_some_and(|handle| handle.is_finished())
        {
            let handle = loader_handle.take().unwrap();
            let result = match handle.join() {
                Ok(result) => result,
                Err(_) => Err("Loader 安装线程异常退出".to_string()),
            };
            if let Err(error) = &result {
                if first_error.is_none() {
                    eprintln!("[modpack] Loader 阶段失败，取消其它下载: {}", error);
                    first_error = Some(error.clone());
                    coordinated_abort = abort_downloaders_after_stage_failure(cancel_name);
                }
            }
            loader_result = Some(result);
            progressed = true;
        }

        if !progressed {
            if crate::instance::is_cancelled(cancel_name) && first_error.is_none() {
                first_error = Some("用户取消安装".to_string());
            }
            std::thread::sleep(std::time::Duration::from_millis(80));
        }
    }

    if coordinated_abort {
        if let Some(cancel_flag) = crate::instance::install_cancel_flag(cancel_name) {
            // 内部协同取消只用于收拢并行任务，外层仍应报告最先出现的真实错误。
            cancel_flag.store(false, std::sync::atomic::Ordering::Relaxed);
        }
    }

    let vanilla_result =
        vanilla_result.unwrap_or_else(|| Err("基础游戏安装未返回结果".to_string()));
    let mod_download_result = mod_download_result.unwrap_or_else(|| {
        let total_files: usize = categories.iter().map(|(_, c)| c.total).sum();
        if crate::instance::is_cancelled(cancel_name) {
            Err(ModpackDownloadError::cancelled(total_files, "用户取消下载"))
        } else {
            Err(ModpackDownloadError::new(
                0,
                total_files,
                "整合包文件下载未返回结果",
            ))
        }
    });
    // 停止进度汇报线程
    progress_stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = progress_thread.join();

    // 最终汇报每个类型
    for (cat, counter) in categories.iter() {
        let finished = counter.done.load(std::sync::atomic::Ordering::Relaxed);
        emit_progress(
            app,
            display_name,
            cat,
            finished,
            counter.total,
            &format!("{}/{}", finished, counter.total),
        );
    }

    let total_files: usize = categories.iter().map(|(_, c)| c.total).sum();
    let mod_download_error = mod_download_result.as_ref().err();
    let failed_count = mod_download_error
        .map(|error| error.failed_count)
        .unwrap_or(0);
    let log_total_files = mod_download_error
        .map(|error| error.total_files)
        .unwrap_or(total_files);
    if coordinated_abort
        && mod_download_error
            .map(|error| error.cancelled)
            .unwrap_or(false)
    {
        let ended_count: usize = categories
            .iter()
            .map(|(_, c)| c.done.load(Ordering::Relaxed))
            .sum();
        eprintln!(
            "[modpack] 阶段失败后已中止后台下载: 已结束={}, 总计={}",
            ended_count.min(log_total_files),
            log_total_files
        );
    } else if mod_download_error
        .map(|error| error.cancelled)
        .unwrap_or(false)
    {
        // 取消不是成功，日志只记录已经结束的任务数量。
        let ended_count: usize = categories
            .iter()
            .map(|(_, c)| c.done.load(Ordering::Relaxed))
            .sum();
        eprintln!(
            "[modpack] 下载已取消: 已结束={}, 总计={}",
            ended_count.min(log_total_files),
            log_total_files
        );
    } else {
        eprintln!(
            "[modpack] 下载完成: 成功={}, 失败={}, 总计={}",
            log_total_files.saturating_sub(failed_count),
            failed_count,
            log_total_files
        );
    }

    let mut ver_json = match vanilla_result {
        Ok(ver_json) => ver_json,
        Err(error) => return Err(first_error.clone().unwrap_or(error)),
    };
    if let Some(error) = first_error {
        return Err(error);
    }

    if let Some(result) = loader_result {
        let loader_json = result?;
        merge_loader_install_result(&mut ver_json, &loader_json);
    }
    if crate::instance::is_cancelled(cancel_name) {
        return Err("用户取消安装".to_string());
    }

    if let Err(error) = mod_download_result {
        return Err(error.message);
    }

    // 复制 overrides
    match &meta.kind {
        ModpackKind::Modrinth { .. } => {
            extract_overrides_modrinth(zip_file, inst_dir, "overrides", cancel_name)?;
            extract_overrides_modrinth(zip_file, inst_dir, "client-overrides", cancel_name)?;
        }
        ModpackKind::CurseForge { override_path, .. } => {
            extract_overrides_cf(zip_file, inst_dir, override_path, cancel_name)?;
        }
    }
    if crate::instance::is_cancelled(cancel_name) {
        return Err("用户取消安装".to_string());
    }
    crate::instance::set_minecraft_language(inst_dir, "zh_cn")?;

    // 自动内存：先使用整合包内部给出的值，没有再按 Mod 数量估算。
    let mods_dir = inst_dir.join("mods");
    let mod_count = if mods_dir.exists() {
        std::fs::read_dir(&mods_dir)
            .map(|d| {
                d.filter(|e| {
                    e.as_ref()
                        .ok()
                        .map(|e| {
                            let name = e.file_name().to_string_lossy().to_lowercase();
                            name.ends_with(".jar") || name.ends_with(".zip")
                        })
                        .unwrap_or(false)
                })
                .count()
            })
            .unwrap_or(0)
    } else {
        0
    };
    let estimated_mb: u32 = if mod_count == 0 {
        2048
    } else if mod_count <= 50 {
        4096
    } else if mod_count <= 150 {
        6144
    } else if mod_count <= 250 {
        8192
    } else {
        10240
    };
    let pack_memory_mb = meta.recommended_memory_mb;
    let auto_memory_mb = if let Some(pack_mem) = pack_memory_mb {
        eprintln!("[modpack] 使用整合包内部内存值: {}MB", pack_mem);
        pack_mem
    } else {
        eprintln!(
            "[modpack] 整合包未指定内存，按 Mod 数量({})估算: {}MB",
            mod_count, estimated_mb
        );
        estimated_mb
    };
    if let Some(pack_mem) = pack_memory_mb {
        ver_json["packRecommendedMemory"] = serde_json::json!(pack_mem);
        ver_json["memorySource"] = serde_json::json!("pack");
    } else {
        ver_json["packRecommendedMemory"] = serde_json::Value::Null;
        ver_json["memorySource"] = serde_json::json!("mod_count");
    }
    ver_json["estimatedMemory"] = serde_json::json!(estimated_mb);
    ver_json["recommendedMemory"] = serde_json::json!(auto_memory_mb);
    ver_json["modCount"] = serde_json::json!(mod_count);
    eprintln!(
        "[modpack] Mod 数量: {}, 最终自动内存: {}MB",
        mod_count, auto_memory_mb
    );

    // 重新写入（因为之前已写过，这里覆盖加上推荐内存）
    strip_launcher_private_version_fields(&mut ver_json);
    write_version_json_atomic(&inst_json_path, &ver_json)
        .map_err(|error| format!("保存版本配置失败: {}", error))?;

    Ok(format!("整合包 {} 安装成功", inst_name))
}

fn extract_overrides_modrinth(
    zip_path: &std::path::Path,
    inst_dir: &std::path::Path,
    prefix: &str,
    cancel_name: &str,
) -> Result<(), String> {
    let file = std::fs::File::open(zip_path).map_err(|e| e.to_string())?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
    super::validate_modpack_archive(&mut archive)?;
    let prefix_slash = format!("{}/", prefix);
    for i in 0..archive.len() {
        if crate::instance::is_cancelled(cancel_name) {
            return Err("用户取消安装".to_string());
        }
        let mut entry = archive.by_index(i).map_err(|e| e.to_string())?;
        let name = entry.name().to_string();
        if name.starts_with(&prefix_slash) && !entry.is_dir() {
            let rel = &name[prefix_slash.len()..];
            if rel.is_empty() {
                continue;
            }
            let dest = safe_join(inst_dir, rel)?;
            if let Some(p) = dest.parent() {
                std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
            }
            let mut out = std::fs::File::create(&dest).map_err(|e| e.to_string())?;
            let mut copied = 0u64;
            let mut buffer = [0u8; 128 * 1024];
            loop {
                if crate::instance::is_cancelled(cancel_name) {
                    return Err("用户取消安装".to_string());
                }
                let read = std::io::Read::read(&mut entry, &mut buffer)
                    .map_err(|e| format!("读取 overrides 失败: {}", e))?;
                if read == 0 {
                    break;
                }
                copied = copied
                    .checked_add(read as u64)
                    .ok_or_else(|| "overrides 文件大小溢出".to_string())?;
                if copied > super::MAX_MODPACK_ENTRY_SIZE {
                    return Err(format!("overrides 文件超过限制: {}", name));
                }
                std::io::Write::write_all(&mut out, &buffer[..read])
                    .map_err(|e| format!("写入 overrides 失败: {}", e))?;
            }
        }
    }
    Ok(())
}

fn extract_overrides_cf(
    zip_path: &std::path::Path,
    inst_dir: &std::path::Path,
    override_path: &str,
    cancel_name: &str,
) -> Result<(), String> {
    extract_overrides_modrinth(zip_path, inst_dir, override_path, cancel_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curseforge_cdn_parts_support_any_file_id_length() {
        assert_eq!(
            curseforge_cdn_parts(999),
            ("0".to_string(), "999".to_string())
        );
        assert_eq!(
            curseforge_cdn_parts(1_000),
            ("1".to_string(), "0".to_string())
        );
        assert_eq!(
            curseforge_cdn_parts(1_234_567),
            ("1234".to_string(), "567".to_string())
        );
        assert_eq!(
            curseforge_cdn_parts(12_345_678),
            ("12345".to_string(), "678".to_string())
        );
    }

    #[test]
    fn missing_curseforge_batch_ids_become_unresolved() {
        let requested = [10, 20, 30];
        let returned = HashSet::from([10, 30]);
        let file_map = HashMap::from([(10, (1, 10)), (20, (2, 20)), (30, (3, 30))]);
        let mut unresolved = Vec::new();

        append_missing_curseforge_file_ids(&requested, &returned, &file_map, &mut unresolved);

        assert_eq!(
            unresolved,
            vec![UnresolvedCurseForgeFile {
                project_id: 2,
                file_id: 20,
                try_individual_api: true,
            }]
        );
    }

    #[test]
    fn failed_batch_entries_skip_individual_api_and_are_deduplicated() {
        let mut unresolved = Vec::new();
        push_unresolved_curseforge_file(&mut unresolved, 1, 10, false);
        push_unresolved_curseforge_file(&mut unresolved, 1, 10, false);
        assert_eq!(unresolved.len(), 1);
        assert!(!unresolved[0].try_individual_api);

        push_unresolved_curseforge_file(&mut unresolved, 1, 10, true);
        assert!(unresolved[0].try_individual_api);
    }

    #[test]
    fn curseforge_pressure_errors_enter_cooldown() {
        assert!(curseforge_api_requires_cooldown(
            "HTTP 429 Too Many Requests"
        ));
        assert!(curseforge_api_requires_cooldown(
            "HTTP 503 Service Unavailable"
        ));
        assert!(!curseforge_api_requires_cooldown("HTTP 404 Not Found"));
        assert_eq!(CF_FALLBACK_CONCURRENCY, 4);
    }

    #[test]
    fn loader_failure_stops_current_and_late_download_managers_without_final_cancel_state() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let cancel_name = format!("loader-abort-test-{nonce}");
        crate::instance::try_register_cancel(&cancel_name).unwrap();

        let current = DownloadManager::with_options(DownloadEngineOptions::default()).unwrap();
        let _current_registration = register_download_manager(&cancel_name, &current);
        assert!(abort_downloaders_after_stage_failure(&cancel_name));
        assert!(crate::instance::is_cancelled(&cancel_name));

        let late = DownloadManager::with_options(DownloadEngineOptions::default()).unwrap();
        let _late_registration = register_download_manager(&cancel_name, &late);
        for (index, manager) in [&current, &late].into_iter().enumerate() {
            let request = DownloadRequest::new(
                format!("loader-abort-{index}"),
                vec![DownloadCandidate::new("http://127.0.0.1:9/unreachable")],
                std::env::temp_dir().join(format!("oaoi-loader-abort-{nonce}-{index}.bin")),
            );
            assert!(manager
                .download_one(request, |_| {})
                .unwrap_err()
                .contains("cancelled"));
        }

        crate::instance::install_cancel_flag(&cancel_name)
            .unwrap()
            .store(false, std::sync::atomic::Ordering::Relaxed);
        assert!(!crate::instance::is_cancelled(&cancel_name));
        crate::instance::unregister_cancel(&cancel_name);
    }
}
