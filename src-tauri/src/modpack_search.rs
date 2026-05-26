use crate::installer::download_file_with_progress;
use crate::instance::{
    cf_api_key, is_cancelled, register_cancel, safe_path_name, unregister_cancel,
};
use std::sync::atomic::Ordering;

// ===== 整合包在线搜索 =====

#[derive(serde::Serialize, Clone)]
pub struct ModpackResult {
    pub title: String,
    pub description: String,
    pub author: String,
    pub downloads: u64,
    pub icon_url: String,
    pub mr_url: String,
    pub cf_url: String,
    pub project_id: String,
    pub source: String, // "modrinth" or "curseforge"
}

#[tauri::command]
pub async fn search_modpacks(
    query: String,
    offset: Option<u32>,
) -> Result<Vec<ModpackResult>, String> {
    let offset = offset.unwrap_or(0);
    tokio::task::spawn_blocking(move || do_search_modpacks(&query, offset))
        .await
        .map_err(|e| format!("线程错误: {}", e))?
}

fn do_search_modpacks(query: &str, offset: u32) -> Result<Vec<ModpackResult>, String> {
    let http = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .user_agent("OAOI-Launcher/1.0")
        .build()
        .map_err(|e| e.to_string())?;

    // 全部并发搜索（使用 thread::scope 避免孤儿线程）
    let (mr_results, cf_results) = std::thread::scope(|s| {
        let q1 = query.to_string();
        let h1 = http.clone();
        let off1 = offset;
        let mr_handle = s.spawn(move || search_mr_modpacks(&h1, &q1, off1).unwrap_or_default());

        let q2 = query.to_string();
        let h2 = http.clone();
        let off2 = offset;
        let cf_handle = s.spawn(move || search_cf_modpacks(&h2, &q2, off2).unwrap_or_default());

        (
            mr_handle.join().unwrap_or_default(),
            cf_handle.join().unwrap_or_default(),
        )
    });

    // 合并双平台: 同名整合包合并 mr_url + cf_url
    let mut merged: Vec<ModpackResult> = Vec::new();
    let mut mr_map: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

    for item in mr_results {
        let key = item.title.to_lowercase();
        mr_map.insert(key, merged.len());
        merged.push(item);
    }

    for cf in cf_results {
        let key = cf.title.to_lowercase();
        if let Some(&idx) = mr_map.get(&key) {
            // 双平台: 合并 CF 链接到已有的 MR 条目
            merged[idx].cf_url = cf.cf_url;
            merged[idx].source = "both".to_string();
            // 取较大的下载量
            if cf.downloads > merged[idx].downloads {
                merged[idx].downloads = cf.downloads;
            }
        } else {
            merged.push(cf);
        }
    }

    merged.sort_by(|a, b| b.downloads.cmp(&a.downloads));
    Ok(merged)
}

fn search_mr_modpacks(
    http: &reqwest::blocking::Client,
    query: &str,
    offset: u32,
) -> Result<Vec<ModpackResult>, String> {
    let url = if query.is_empty() {
        format!("https://api.modrinth.com/v2/search?facets=[[\"project_type:modpack\"]]&limit=20&offset={}&index=downloads", offset)
    } else {
        format!(
            "https://api.modrinth.com/v2/search?query={}&facets=[[\"project_type:modpack\"]]&limit=20&offset={}&index=relevance",
            urlencoding::encode(query), offset
        )
    };

    let resp = http.get(&url).send().map_err(|e| e.to_string())?;
    let json: serde_json::Value = resp.json().map_err(|e| e.to_string())?;

    let mut results = Vec::new();
    if let Some(hits) = json["hits"].as_array() {
        for hit in hits {
            let slug = hit["slug"].as_str().unwrap_or("").to_string();
            results.push(ModpackResult {
                title: hit["title"].as_str().unwrap_or("").to_string(),
                description: hit["description"].as_str().unwrap_or("").to_string(),
                author: hit["author"].as_str().unwrap_or("").to_string(),
                downloads: hit["downloads"].as_u64().unwrap_or(0),
                icon_url: hit["icon_url"].as_str().unwrap_or("").to_string(),
                mr_url: format!("https://modrinth.com/modpack/{}", slug),
                cf_url: String::new(),
                project_id: hit["project_id"].as_str().unwrap_or("").to_string(),
                source: "modrinth".to_string(),
            });
        }
    }
    Ok(results)
}

fn search_cf_modpacks(
    http: &reqwest::blocking::Client,
    query: &str,
    offset: u32,
) -> Result<Vec<ModpackResult>, String> {
    let sort_field = if query.is_empty() { "6" } else { "1" };
    let url = format!(
        "https://api.curseforge.com/v1/mods/search?gameId=432&classId=4471&searchFilter={}&pageSize=20&sortField={}&sortOrder=desc&index={}",
        urlencoding::encode(query),
        sort_field,
        offset,
    );

    let resp = http
        .get(&url)
        .header("x-api-key", &cf_api_key())
        .header("Accept", "application/json")
        .send()
        .map_err(|e| e.to_string())?;

    let json: serde_json::Value = resp.json().map_err(|e| e.to_string())?;

    let mut results = Vec::new();
    if let Some(data) = json["data"].as_array() {
        for item in data {
            let authors = item["authors"]
                .as_array()
                .and_then(|a| a.first())
                .and_then(|a| a["name"].as_str())
                .unwrap_or("");
            let logo = item["logo"]["url"].as_str().unwrap_or("");
            let id = item["id"].as_u64().unwrap_or(0);
            let fallback_url = format!("https://www.curseforge.com/minecraft/modpacks/{}", id);
            let cf_url = item["links"]["websiteUrl"]
                .as_str()
                .unwrap_or(&fallback_url);
            results.push(ModpackResult {
                title: item["name"].as_str().unwrap_or("").to_string(),
                description: item["summary"].as_str().unwrap_or("").to_string(),
                author: authors.to_string(),
                downloads: item["downloadCount"].as_u64().unwrap_or(0),
                icon_url: logo.to_string(),
                mr_url: String::new(),
                cf_url: cf_url.to_string(),
                project_id: id.to_string(),
                source: "curseforge".to_string(),
            });
        }
    }
    Ok(results)
}

// ===== 整合包版本列表 + 一键安装 =====

#[derive(serde::Serialize, Clone)]
pub struct ModpackVersionInfo {
    pub version_name: String,
    pub mc_versions: String,
    pub download_url: String,
    pub download_urls: Vec<String>,
    pub file_name: String,
    pub file_size: u64,
    pub date: String,
    pub version_id: String,
}

#[tauri::command]
pub async fn get_modpack_versions(
    project_id: String,
    source: String,
) -> Result<Vec<ModpackVersionInfo>, String> {
    tokio::task::spawn_blocking(move || do_get_modpack_versions(&project_id, &source))
        .await
        .map_err(|e| e.to_string())?
}

fn do_get_modpack_versions(
    project_id: &str,
    source: &str,
) -> Result<Vec<ModpackVersionInfo>, String> {
    let http = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .user_agent("OAOI-Launcher/1.0")
        .build()
        .map_err(|e| e.to_string())?;

    match source {
        "modrinth" | "both" => get_mr_modpack_versions(&http, project_id),
        "curseforge" => get_cf_modpack_versions(&http, project_id),
        _ => get_mr_modpack_versions(&http, project_id),
    }
}

fn get_mr_modpack_versions(
    http: &reqwest::blocking::Client,
    project_id: &str,
) -> Result<Vec<ModpackVersionInfo>, String> {
    let url = format!("https://api.modrinth.com/v2/project/{}/version", project_id);
    let resp = http.get(&url).send().map_err(|e| e.to_string())?;
    let json: serde_json::Value = resp.json().map_err(|e| e.to_string())?;
    let arr = json.as_array().ok_or("格式错误")?;

    let mut results = Vec::new();
    for ver in arr.iter().take(20) {
        let game_versions = ver["game_versions"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        let files = ver["files"].as_array();
        let (dl_url, fname, fsize) = if let Some(files) = files {
            if let Some(f) = files
                .iter()
                .find(|f| f["primary"].as_bool().unwrap_or(false))
                .or(files.first())
            {
                let raw_name = f["filename"].as_str().unwrap_or("modpack.mrpack");
                (
                    f["url"].as_str().unwrap_or("").to_string(),
                    safe_path_name(raw_name, "文件名")
                        .unwrap_or_else(|_| "modpack.mrpack".to_string()),
                    f["size"].as_u64().unwrap_or(0),
                )
            } else {
                continue;
            }
        } else {
            continue;
        };

        results.push(ModpackVersionInfo {
            version_name: ver["name"]
                .as_str()
                .unwrap_or(ver["version_number"].as_str().unwrap_or(""))
                .to_string(),
            mc_versions: game_versions,
            download_url: dl_url.clone(),
            download_urls: vec![dl_url.clone()],
            file_name: fname,
            file_size: fsize,
            date: ver["date_published"]
                .as_str()
                .unwrap_or("")
                .chars()
                .take(10)
                .collect(),
            version_id: ver["id"].as_str().unwrap_or("").to_string(),
        });
    }
    Ok(results)
}

fn get_cf_modpack_versions(
    http: &reqwest::blocking::Client,
    project_id: &str,
) -> Result<Vec<ModpackVersionInfo>, String> {
    let url = format!(
        "https://api.curseforge.com/v1/mods/{}/files?pageSize=20",
        project_id
    );
    let resp = http
        .get(&url)
        .header("x-api-key", &cf_api_key())
        .header("Accept", "application/json")
        .send()
        .map_err(|e| e.to_string())?;
    let json: serde_json::Value = resp.json().map_err(|e| e.to_string())?;
    let data = json["data"].as_array().ok_or("格式错误")?;

    let mut results = Vec::new();
    for file in data.iter().take(20) {
        let game_versions = file["gameVersions"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        let raw_file_name = file["fileName"].as_str().unwrap_or("modpack.zip");
        let safe_file_name =
            safe_path_name(raw_file_name, "文件名").unwrap_or_else(|_| "modpack.zip".to_string());
        let fid = file["id"].as_u64().unwrap_or(0);
        let api_dl_url = file["downloadUrl"].as_str().unwrap_or("");
        let download_urls =
            curseforge_archive_download_candidates(fid, &safe_file_name, api_dl_url);
        let Some(dl_url) = download_urls.first().cloned() else {
            continue;
        };

        results.push(ModpackVersionInfo {
            version_name: file["displayName"].as_str().unwrap_or("").to_string(),
            mc_versions: game_versions,
            download_url: dl_url,
            download_urls,
            file_name: safe_file_name,
            file_size: file["fileLength"].as_u64().unwrap_or(0),
            date: file["fileDate"]
                .as_str()
                .unwrap_or("")
                .chars()
                .take(10)
                .collect(),
            version_id: file["id"].as_u64().unwrap_or(0).to_string(),
        });
    }
    Ok(results)
}

fn curseforge_archive_download_candidates(
    file_id: u64,
    file_name: &str,
    api_download_url: &str,
) -> Vec<String> {
    let mut urls = Vec::new();
    if file_id > 0 && !file_name.is_empty() {
        let encoded_name = urlencoding::encode(file_name);
        urls.push(format!(
            "https://edge.forgecdn.net/files/{}/{}/{}",
            file_id / 1000,
            file_id % 1000,
            encoded_name
        ));
        urls.push(format!(
            "https://mediafilez.forgecdn.net/files/{}/{}/{}",
            file_id / 1000,
            file_id % 1000,
            encoded_name
        ));
    }
    if !api_download_url.trim().is_empty() {
        urls.push(api_download_url.trim().to_string());
    }
    dedupe_urls(urls)
}

fn normalize_download_urls(
    download_url: String,
    download_urls: Option<Vec<String>>,
) -> Vec<String> {
    let mut urls = download_urls.unwrap_or_default();
    if !download_url.trim().is_empty() {
        urls.push(download_url);
    }
    dedupe_urls(urls)
}

fn dedupe_urls(urls: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut result = Vec::new();
    for url in urls {
        let url = url.trim().to_string();
        if url.is_empty() || !seen.insert(url.clone()) {
            continue;
        }
        result.push(url);
    }
    result
}

#[tauri::command]
pub fn install_modpack_direct(
    app_handle: tauri::AppHandle,
    download_url: String,
    download_urls: Option<Vec<String>>,
    file_name: String,
    game_dir: String,
    java_path: String,
    use_mirror: bool,
) -> Result<String, String> {
    let file_name = safe_path_name(&file_name, "文件名")?;
    let download_urls = normalize_download_urls(download_url, download_urls);
    let cancel_flag = register_cancel(&file_name);
    std::thread::spawn(move || {
        let result = do_install_modpack_direct(
            &app_handle,
            download_urls,
            &file_name,
            &game_dir,
            &java_path,
            use_mirror,
        );
        unregister_cancel(&file_name);
        match result {
            Ok(msg) => eprintln!("[modpack-dl] {}", msg),
            Err(e) => {
                let stage = if cancel_flag.load(Ordering::Relaxed) {
                    "cancelled"
                } else {
                    "error"
                };
                eprintln!("[modpack-dl] {}: {}", stage, e);
                let _ = tauri::Emitter::emit(
                    &app_handle,
                    "install-progress",
                    serde_json::json!({
                        "name": file_name, "stage": stage, "current": 0, "total": 0, "detail": e
                    }),
                );
            }
        }
    });
    Ok("downloading".to_string())
}

fn do_install_modpack_direct(
    app_handle: &tauri::AppHandle,
    download_urls: Vec<String>,
    file_name: &str,
    game_dir: &str,
    java_path: &str,
    use_mirror: bool,
) -> Result<String, String> {
    let _ = tauri::Emitter::emit(
        app_handle,
        "install-progress",
        serde_json::json!({
            "name": file_name, "stage": "downloading", "current": 0, "total": 1, "detail": format!("正在下载 {}...", file_name)
        }),
    );

    let http = reqwest::blocking::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(30))
        .timeout(std::time::Duration::from_secs(600))
        .user_agent("OAOI-Launcher/1.0")
        .build()
        .map_err(|e| e.to_string())?;

    // 保存到临时文件（流式写入）
    let tmp_dir = std::env::temp_dir().join("oaoi_modpack_dl");
    std::fs::create_dir_all(&tmp_dir).ok();
    let tmp_path = tmp_dir.join(file_name);
    let downloaded = download_modpack_archive_with_fallbacks(
        app_handle,
        &http,
        &download_urls,
        file_name,
        &tmp_path,
    )?;
    let total_size = std::fs::metadata(&tmp_path)
        .map(|m| m.len())
        .unwrap_or(downloaded);

    // 标记下载完成
    let _ = tauri::Emitter::emit(
        app_handle,
        "install-progress",
        serde_json::json!({
            "name": file_name, "stage": "downloading", "current": total_size, "total": total_size, "detail": "下载完成"
        }),
    );

    // 再次检查取消
    if is_cancelled(file_name) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err("用户取消安装".to_string());
    }

    eprintln!(
        "[modpack-dl] 下载完成 ({:.1} MB), 开始安装...",
        downloaded as f64 / 1048576.0
    );

    // 调用现有的 import_modpack 逻辑，传入 file_name 作为 display_name
    let result = crate::modpack::do_import_modpack_named(
        app_handle,
        &tmp_path.to_string_lossy(),
        game_dir,
        java_path,
        use_mirror,
        Some(file_name),
    );

    // 清理临时文件
    let _ = std::fs::remove_file(&tmp_path);
    let _ = std::fs::remove_dir(&tmp_dir);

    result
}

#[allow(dead_code)]
const MODPACK_PARALLEL_MIN_SIZE: u64 = 16 * 1024 * 1024;
const MODPACK_PARALLEL_WORKERS: u64 = 8;

fn download_modpack_archive(
    app_handle: &tauri::AppHandle,
    http: &reqwest::blocking::Client,
    download_url: &str,
    file_name: &str,
    tmp_path: &std::path::Path,
) -> Result<u64, String> {
    let _ = std::fs::remove_file(tmp_path);
    download_file_with_progress(
        http,
        download_url,
        tmp_path,
        None,
        false,
        Some(file_name),
        |downloaded, total| {
            emit_download_progress(app_handle, file_name, downloaded, total.unwrap_or(0));
        },
    )
    .map_err(|e| format!("下载整合包失败: {}", e))?;

    let downloaded = std::fs::metadata(tmp_path)
        .map(|m| m.len())
        .map_err(|e| format!("读取整合包文件失败: {}", e))?;
    if downloaded == 0 {
        return Err("下载整合包失败: 文件为空".to_string());
    }
    Ok(downloaded)
}

#[allow(dead_code)]
fn probe_range_size(
    http: &reqwest::blocking::Client,
    download_url: &str,
) -> Result<Option<u64>, String> {
    let resp = http
        .get(download_url)
        .header("Range", "bytes=0-0")
        .send()
        .map_err(|e| format!("探测分片下载失败: {}", e))?;

    if resp.status() == reqwest::StatusCode::PARTIAL_CONTENT {
        return Ok(resp
            .headers()
            .get("content-range")
            .and_then(|value| value.to_str().ok())
            .and_then(parse_content_range_total));
    }

    Ok(None)
}

fn parse_content_range_total(value: &str) -> Option<u64> {
    value.rsplit('/').next()?.parse::<u64>().ok()
}

fn part_path(tmp_path: &std::path::Path, index: usize) -> std::path::PathBuf {
    let file_name = tmp_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("modpack.zip");
    tmp_path.with_file_name(format!("{}.part{}", file_name, index))
}

fn remove_part_files(tmp_path: &std::path::Path, count: usize) {
    for index in 0..count {
        let _ = std::fs::remove_file(part_path(tmp_path, index));
    }
}

fn emit_download_progress(
    app_handle: &tauri::AppHandle,
    file_name: &str,
    downloaded: u64,
    total_size: u64,
) {
    if total_size > 0 {
        let pct = (downloaded as f64 / total_size as f64 * 100.0) as u64;
        let _ = tauri::Emitter::emit(
            app_handle,
            "install-progress",
            serde_json::json!({
                "name": file_name, "stage": "downloading",
                "current": downloaded, "total": total_size,
                "detail": format!("{:.1}MB / {:.1}MB ({}%)", downloaded as f64 / 1048576.0, total_size as f64 / 1048576.0, pct)
            }),
        );
    } else {
        let _ = tauri::Emitter::emit(
            app_handle,
            "install-progress",
            serde_json::json!({
                "name": file_name, "stage": "downloading",
                "current": downloaded, "total": 0,
                "detail": format!("已下载 {:.1}MB", downloaded as f64 / 1048576.0)
            }),
        );
    }
}

fn download_modpack_archive_with_fallbacks(
    app_handle: &tauri::AppHandle,
    http: &reqwest::blocking::Client,
    download_urls: &[String],
    file_name: &str,
    tmp_path: &std::path::Path,
) -> Result<u64, String> {
    let mut last_err = String::new();
    for url in download_urls {
        eprintln!("[modpack-dl] try archive: {}", url);
        match download_modpack_archive(app_handle, http, url, file_name, tmp_path) {
            Ok(downloaded) => return Ok(downloaded),
            Err(err) => {
                last_err = format!("{}: {}", url, err);
                eprintln!("[modpack-dl] archive failed, try next: {}", last_err);
                let _ = std::fs::remove_file(tmp_path);
                remove_part_files(tmp_path, MODPACK_PARALLEL_WORKERS as usize);
            }
        }
    }
    Err(if last_err.is_empty() {
        "没有可用整合包下载地址".to_string()
    } else {
        last_err
    })
}

#[allow(dead_code)]
fn download_modpack_single(
    app_handle: &tauri::AppHandle,
    http: &reqwest::blocking::Client,
    download_url: &str,
    file_name: &str,
    tmp_path: &std::path::Path,
) -> Result<u64, String> {
    let _ = tauri::Emitter::emit(
        app_handle,
        "install-progress",
        serde_json::json!({
            "name": file_name, "stage": "downloading", "current": 0, "total": 0, "detail": "单连接下载"
        }),
    );
    let resp = http
        .get(download_url)
        .send()
        .map_err(|e| format!("下载失败: {}", e))?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }

    let total_size = resp.content_length().unwrap_or(0);
    let mut out_file =
        std::fs::File::create(tmp_path).map_err(|e| format!("创建文件失败: {}", e))?;
    let mut downloaded: u64 = 0;
    let mut last_report = std::time::Instant::now();
    let mut reader = std::io::BufReader::new(resp);
    let mut buf = [0u8; 128 * 1024];

    loop {
        if is_cancelled(file_name) {
            drop(out_file);
            let _ = std::fs::remove_file(tmp_path);
            return Err("用户取消下载".to_string());
        }

        use std::io::Read;
        let n = reader
            .read(&mut buf)
            .map_err(|e| format!("读取失败: {}", e))?;
        if n == 0 {
            break;
        }
        use std::io::Write;
        out_file
            .write_all(&buf[..n])
            .map_err(|e| format!("写入失败: {}", e))?;
        downloaded += n as u64;

        if last_report.elapsed().as_millis() >= 500 {
            last_report = std::time::Instant::now();
            emit_download_progress(app_handle, file_name, downloaded, total_size);
        }
    }

    Ok(downloaded)
}

#[allow(dead_code)]
fn download_modpack_parallel(
    app_handle: &tauri::AppHandle,
    http: &reqwest::blocking::Client,
    download_url: &str,
    file_name: &str,
    tmp_path: &std::path::Path,
    total_size: u64,
) -> Result<u64, String> {
    let worker_count = MODPACK_PARALLEL_WORKERS
        .min(total_size.div_ceil(8 * 1024 * 1024))
        .max(1);
    let _ = tauri::Emitter::emit(
        app_handle,
        "install-progress",
        serde_json::json!({
            "name": file_name, "stage": "downloading", "current": 0, "total": total_size,
            "detail": format!("分片下载 {} 线程", worker_count)
        }),
    );
    let chunk_size = total_size.div_ceil(worker_count);
    let downloaded = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

    let app_for_report = app_handle.clone();
    let name_for_report = file_name.to_string();
    let downloaded_for_report = downloaded.clone();
    let done_for_report = done.clone();
    let reporter = std::thread::spawn(move || {
        while !done_for_report.load(std::sync::atomic::Ordering::Relaxed) {
            let current = downloaded_for_report.load(std::sync::atomic::Ordering::Relaxed);
            emit_download_progress(&app_for_report, &name_for_report, current, total_size);
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    });

    let mut handles = Vec::new();
    for index in 0..worker_count as usize {
        let start = index as u64 * chunk_size;
        if start >= total_size {
            break;
        }
        let end = (start + chunk_size - 1).min(total_size - 1);
        let url = download_url.to_string();
        let part = part_path(tmp_path, index);
        let client = http.clone();
        let name = file_name.to_string();
        let downloaded_for_worker = downloaded.clone();

        handles.push(std::thread::spawn(move || -> Result<(), String> {
            let mut resp = client
                .get(&url)
                .header("Range", format!("bytes={}-{}", start, end))
                .send()
                .map_err(|e| format!("分片 {} 请求失败: {}", index + 1, e))?;
            if resp.status() != reqwest::StatusCode::PARTIAL_CONTENT {
                return Err(format!("分片 {} HTTP {}", index + 1, resp.status()));
            }

            let mut out_file =
                std::fs::File::create(&part).map_err(|e| format!("创建分片失败: {}", e))?;
            let mut written: u64 = 0;
            let mut buf = [0u8; 256 * 1024];
            loop {
                if is_cancelled(&name) {
                    drop(out_file);
                    let _ = std::fs::remove_file(&part);
                    return Err("用户取消下载".to_string());
                }

                use std::io::Read;
                let n = resp
                    .read(&mut buf)
                    .map_err(|e| format!("分片 {} 读取失败: {}", index + 1, e))?;
                if n == 0 {
                    break;
                }
                use std::io::Write;
                out_file
                    .write_all(&buf[..n])
                    .map_err(|e| format!("分片 {} 写入失败: {}", index + 1, e))?;
                written += n as u64;
                downloaded_for_worker.fetch_add(n as u64, std::sync::atomic::Ordering::Relaxed);
            }

            let expected = end - start + 1;
            if written != expected {
                return Err(format!(
                    "分片 {} 大小不完整: {}/{}",
                    index + 1,
                    written,
                    expected
                ));
            }
            Ok(())
        }));
    }

    let mut first_error: Option<String> = None;
    for handle in handles {
        match handle.join() {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                if first_error.is_none() {
                    first_error = Some(e);
                }
            }
            Err(_) => {
                if first_error.is_none() {
                    first_error = Some("分片线程崩溃".to_string());
                }
            }
        }
    }
    done.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = reporter.join();

    if let Some(error) = first_error {
        remove_part_files(tmp_path, worker_count as usize);
        return Err(error);
    }

    let mut out_file =
        std::fs::File::create(tmp_path).map_err(|e| format!("创建文件失败: {}", e))?;
    for index in 0..worker_count as usize {
        let part = part_path(tmp_path, index);
        let mut part_file =
            std::fs::File::open(&part).map_err(|e| format!("读取分片失败: {}", e))?;
        std::io::copy(&mut part_file, &mut out_file).map_err(|e| format!("合并分片失败: {}", e))?;
        let _ = std::fs::remove_file(&part);
    }

    Ok(total_size)
}
