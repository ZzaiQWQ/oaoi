use super::cf_download::cf_cdn_urls;
use super::download_mirror::{append_unique_urls, with_mod_mirrors};
use super::{build_http_client, detect_target_dir, emit_progress, ModpackKind, ModpackMeta};
use crate::downloader::event::{DownloadEvent, DownloadOutcome};
use crate::downloader::{
    DownloadCandidate, DownloadEngineOptions, DownloadManager, DownloadRequest,
};
use crate::installer::{empty_loader_json, merge_loader_install_result, mirror_url};
use crate::instance::{
    cf_api_key, install_download_pool, register_download_manager, safe_join, safe_path_name,
    strip_launcher_private_version_fields, version_json_path,
};
use crate::modpack_sources::sha1_from_curseforge_hashes;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

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
) -> Result<ModpackDownloadTask, String> {
    let api_url = format!(
        "https://api.curseforge.com/v1/mods/{}/files/{}",
        project_id, file_id
    );
    let resp = client
        .get(&api_url)
        .header("x-api-key", &cf_api_key())
        .header("Accept", "application/json")
        .send()
        .map_err(|e| format!("CurseForge 文件解析失败: {}", e))?;
    if !resp.status().is_success() {
        return Err(format!(
            "CurseForge 文件解析失败: HTTP {} p={} f={}",
            resp.status(),
            project_id,
            file_id
        ));
    }

    let json: serde_json::Value = resp.json().map_err(|e| e.to_string())?;
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
    if let Ok(resp) = client
        .get(&download_url_api)
        .header("x-api-key", &cf_api_key())
        .header("Accept", "application/json")
        .send()
    {
        if resp.status().is_success() {
            if let Ok(json) = resp.json::<serde_json::Value>() {
                if let Some(url) = json["data"].as_str().filter(|url| !url.trim().is_empty()) {
                    append_unique_urls(&mut urls, vec![url.to_string()]);
                }
            }
        }
    }
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
    client: &reqwest::blocking::Client,
    project_id: u32,
    file_id: u32,
    inst_dir: &Path,
) -> Result<ModpackDownloadTask, String> {
    let id_text = file_id.to_string();
    let (first, second) = if id_text.len() > 4 {
        let (left, right) = id_text.split_at(4);
        (left.to_string(), right.to_string())
    } else {
        ((file_id / 1000).to_string(), (file_id % 1000).to_string())
    };
    let probe_urls = with_mod_mirrors(vec![
        format!("https://mediafilez.forgecdn.net/files/{}/{}", first, second),
        format!("https://edge.forgecdn.net/files/{}/{}", first, second),
    ]);
    let mut last_err = String::new();
    let mut resolved_dest: Option<PathBuf> = None;
    let mut resolved_urls = Vec::new();
    for url in probe_urls {
        match client.get(&url).send() {
            Ok(resp) if resp.status().is_success() => {
                let final_url = resp.url().to_string();
                let file_name = response_file_name(&resp, &second)
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
                last_err = format!("{} HTTP {}", url, resp.status());
            }
            Err(e) => {
                last_err = format!("{}: {}", url, e);
            }
        }
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

fn response_file_name(resp: &reqwest::blocking::Response, fallback_path: &str) -> Option<String> {
    let from_url = resp
        .url()
        .path_segments()
        .and_then(|mut parts| parts.next_back())
        .map(|name| name.split('?').next().unwrap_or(name))
        .map(percent_decode_simple)
        .filter(|name| !name.is_empty() && name != fallback_path);
    if from_url.is_some() {
        return from_url;
    }

    resp.headers()
        .get("content-disposition")
        .and_then(|value| value.to_str().ok())
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
    if crate::instance::is_cancelled(display_name) {
        return Err("用户取消安装".to_string());
    }

    // 1. 安装基础游戏
    let http = build_http_client(15, 180, 8)?;

    // 整合包安装：基础游戏（版本 jar/libs/assets）优先用镜像
    // Mod 下载保持用户原始设置（CurseForge CDN 国内直连就行）
    let mirror_manifest_url = "https://bmclapi2.bangbang93.com/mc/game/version_manifest_v2.json";
    let official_manifest_url = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";

    emit_progress(
        app,
        display_name,
        "meta",
        0,
        1,
        "获取版本清单（镜像优先）...",
    );
    let manifest_resp = http
        .get(mirror_manifest_url)
        .timeout(std::time::Duration::from_secs(8))
        .send();
    let (manifest_resp, game_mirror) = match manifest_resp {
        Ok(r) if r.status().is_success() => {
            eprintln!("[modpack] 镜像源获取清单成功");
            (Ok(r), true)
        }
        _ => {
            eprintln!("[modpack] 镜像源失败，回退到官方源...");
            emit_progress(app, display_name, "meta", 0, 1, "镜像源超时，切换官方源...");
            (http.get(official_manifest_url).send(), use_mirror)
        }
    };

    let manifest: serde_json::Value = manifest_resp
        .map_err(|e| format!("获取版本清单失败: {}", e))?
        .json()
        .map_err(|e| e.to_string())?;
    let meta_url = manifest["versions"]
        .as_array()
        .and_then(|arr| {
            arr.iter()
                .find(|v| v["id"].as_str() == Some(&meta.mc_version))
        })
        .and_then(|v| v["url"].as_str())
        .ok_or_else(|| format!("找不到 MC 版本: {}", meta.mc_version))?
        .to_string();
    let meta_url = mirror_url(&meta_url, game_mirror);

    if inst_name.is_empty() {
        return Err("整合包名称无效".to_string());
    }
    std::fs::create_dir_all(inst_dir).map_err(|e| e.to_string())?;
    let inst_json_path = version_json_path(inst_dir, &inst_name);

    if crate::instance::is_cancelled(display_name) {
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
                        let body = serde_json::json!({ "modIds": chunk, "filterPcOnly": true });
                        if let Ok(resp) = client
                            .post("https://api.curseforge.com/v1/mods")
                            .header("x-api-key", &cf_api_key())
                            .header("Content-Type", "application/json")
                            .json(&body)
                            .send()
                        {
                            if let Ok(json) = resp.json::<serde_json::Value>() {
                                if let Some(data) = json["data"].as_array() {
                                    for proj in data {
                                        let pid = proj["id"].as_u64().unwrap_or(0) as u32;
                                        let cid = proj["classId"].as_u64().unwrap_or(0);
                                        if cid > 0 {
                                            pid_class.insert(pid, cid);
                                        }
                                    }
                                }
                            }
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
                let mut unresolved: Vec<(u32, u32)> = Vec::new();

                if let Some(client) = &api_client {
                    for chunk in file_ids.chunks(500) {
                        let body = serde_json::json!({ "fileIds": chunk });
                        match client
                            .post("https://api.curseforge.com/v1/mods/files")
                            .header("x-api-key", &cf_api_key())
                            .header("Content-Type", "application/json")
                            .header("Accept", "application/json")
                            .json(&body)
                            .send()
                        {
                            Ok(resp) if resp.status().is_success() => {
                                if let Ok(json) = resp.json::<serde_json::Value>() {
                                    if let Some(data) = json["data"].as_array() {
                                        for item in data {
                                            let fid = item["id"].as_u64().unwrap_or(0) as u32;
                                            let raw_fname =
                                                item["fileName"].as_str().unwrap_or("").to_string();
                                            let dl = item["downloadUrl"]
                                                .as_str()
                                                .unwrap_or("")
                                                .to_string();
                                            let pid = file_map.get(&fid).map(|x| x.0).unwrap_or(0);
                                            let sha1 = sha1_from_curseforge_hashes(&item["hashes"]);

                                            if raw_fname.is_empty() {
                                                unresolved.push((pid, fid));
                                                continue;
                                            }
                                            let fname = match safe_path_name(&raw_fname, "文件名")
                                            {
                                                Ok(name) => name,
                                                Err(_) => {
                                                    unresolved.push((pid, fid));
                                                    continue;
                                                }
                                            };

                                            if fname.is_empty() {
                                                unresolved.push((pid, fid));
                                                continue;
                                            }

                                            // 优先用项目 classId 判断目录（最可靠）
                                            let (target_dir, file_type) = if let Some(&cid) =
                                                pid_class.get(&pid)
                                            {
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
                                            let mut urls =
                                                with_mod_mirrors(cf_cdn_urls(fid, &fname));
                                            if !dl.is_empty() {
                                                append_unique_urls(&mut urls, vec![dl]);
                                            }
                                            if urls.is_empty() {
                                                unresolved.push((pid, fid));
                                            } else {
                                                resolved.push(ModpackDownloadTask {
                                                    urls,
                                                    dest,
                                                    sha1: sha1.clone(),
                                                });
                                            }
                                        }
                                    }
                                }
                            }
                            _ => {
                                eprintln!("[cf] 批量 API 失败, 回退到逐个下载");
                                for id in chunk {
                                    if let Some(&(pid, fid)) = file_map.get(id) {
                                        unresolved.push((pid, fid));
                                    }
                                }
                            }
                        }
                    }
                } else {
                    for f in files {
                        unresolved.push((f.project_id, f.file_id));
                    }
                }

                eprintln!(
                    "[cf] 批量解析: {} 已解析, {} 需单独下载",
                    resolved.len(),
                    unresolved.len()
                );

                for (pid, fid) in unresolved {
                    let Some(client) = &api_client else {
                        return Err("CurseForge API client unavailable".to_string());
                    };
                    match resolve_curseforge_file_task(
                        client,
                        pid,
                        fid,
                        pid_class.get(&pid).copied(),
                        inst_dir,
                    ) {
                        Ok(task) => resolved.push(task),
                        Err(api_error) => {
                            eprintln!("[cf] 单文件 API 解析失败，尝试 CDN 盲猜: {}", api_error);
                            resolved.push(resolve_curseforge_blind_file_task(
                                client, pid, fid, inst_dir,
                            )?);
                        }
                    }
                }

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

    if crate::instance::is_cancelled(display_name) {
        return Err("用户取消安装".to_string());
    }

    // 基础游戏用 game_mirror（镜像优先），mod 下载用原始 use_mirror。
    // 文件任务解析成功后，再并行启动基础游戏、整合包文件和 loader。
    let vanilla_handle = {
        let app = app.clone();
        let display_name = display_name.to_string();
        let version_name = inst_name.clone();
        let mc_version = meta.mc_version.clone();
        let meta_url = meta_url.clone();
        let game_dir = game_dir.to_path_buf();
        let inst_dir = inst_dir.to_path_buf();
        let http = http.clone();
        std::thread::spawn(move || {
            vanilla::install_vanilla_with_names(
                &app,
                &display_name,
                &version_name,
                &mc_version,
                &meta_url,
                &game_dir,
                &inst_dir,
                &http,
                game_mirror,
            )
        })
    };

    let mod_download_handle = {
        let categories = categories.clone();
        let cancel_name = display_name.to_string();
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
                        let _ = crate::instance::cancel_modpack_install(display_name.clone());
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
                            Some(&display_name),
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
                                let _ =
                                    crate::instance::cancel_modpack_install(display_name.clone());
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
                "fabric" => fabric::install_fabric(
                    &app,
                    &display_name,
                    &mc_version,
                    &loader_version,
                    &game_dir,
                    &inst_dir,
                    &http,
                    game_mirror,
                    &mut loader_json,
                    false,
                )?,
                "quilt" => quilt::install_quilt(
                    &app,
                    &display_name,
                    &mc_version,
                    &loader_version,
                    &game_dir,
                    &inst_dir,
                    &http,
                    game_mirror,
                    &mut loader_json,
                )?,
                "forge" => forge::install_forge_with_names(
                    &app,
                    &display_name,
                    &version_name,
                    &mc_version,
                    &loader_version,
                    &game_dir,
                    &inst_dir,
                    &http,
                    effective_java,
                    game_mirror,
                    &mut loader_json,
                )?,
                "neoforge" => neoforge::install_neoforge_with_names(
                    &app,
                    &display_name,
                    &version_name,
                    &mc_version,
                    &loader_version,
                    &game_dir,
                    &inst_dir,
                    &http,
                    effective_java,
                    game_mirror,
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
                    let _ = crate::instance::cancel_modpack_install(display_name.to_string());
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
                    let _ = crate::instance::cancel_modpack_install(display_name.to_string());
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
                    let _ = crate::instance::cancel_modpack_install(display_name.to_string());
                }
            }
            loader_result = Some(result);
            progressed = true;
        }

        if !progressed {
            if crate::instance::is_cancelled(display_name) && first_error.is_none() {
                first_error = Some("用户取消安装".to_string());
            }
            std::thread::sleep(std::time::Duration::from_millis(80));
        }
    }

    let vanilla_result =
        vanilla_result.unwrap_or_else(|| Err("基础游戏安装未返回结果".to_string()));
    let mod_download_result = mod_download_result.unwrap_or_else(|| {
        let total_files: usize = categories.iter().map(|(_, c)| c.total).sum();
        if crate::instance::is_cancelled(display_name) {
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
    if mod_download_error
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
    if crate::instance::is_cancelled(display_name) {
        return Err("用户取消安装".to_string());
    }

    if let Err(error) = mod_download_result {
        return Err(error.message);
    }

    // 复制 overrides
    match &meta.kind {
        ModpackKind::Modrinth { .. } => {
            extract_overrides_modrinth(zip_file, inst_dir, "overrides")?;
            extract_overrides_modrinth(zip_file, inst_dir, "client-overrides")?;
        }
        ModpackKind::CurseForge { override_path, .. } => {
            extract_overrides_cf(zip_file, inst_dir, override_path)?;
        }
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
    std::fs::write(
        &inst_json_path,
        serde_json::to_string_pretty(&ver_json).unwrap(),
    )
    .map_err(|e| format!("保存版本配置失败: {}", e))?;

    Ok(format!("整合包 {} 安装成功", inst_name))
}

fn extract_overrides_modrinth(
    zip_path: &std::path::Path,
    inst_dir: &std::path::Path,
    prefix: &str,
) -> Result<(), String> {
    let file = std::fs::File::open(zip_path).map_err(|e| e.to_string())?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
    let prefix_slash = format!("{}/", prefix);
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| e.to_string())?;
        let name = entry.name().to_string();
        if name.starts_with(&prefix_slash) && !entry.is_dir() {
            let rel = &name[prefix_slash.len()..];
            if rel.is_empty() {
                continue;
            }
            let dest = safe_join(inst_dir, rel)?;
            if let Some(p) = dest.parent() {
                std::fs::create_dir_all(p).ok();
            }
            let mut out = std::fs::File::create(&dest).map_err(|e| e.to_string())?;
            std::io::copy(&mut entry, &mut out).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

fn extract_overrides_cf(
    zip_path: &std::path::Path,
    inst_dir: &std::path::Path,
    override_path: &str,
) -> Result<(), String> {
    extract_overrides_modrinth(zip_path, inst_dir, override_path)
}
