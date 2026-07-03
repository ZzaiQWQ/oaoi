use crate::downloader::event::{DownloadEvent, DownloadOutcome};
use crate::downloader::{
    DownloadCandidate, DownloadEngineOptions, DownloadManager, DownloadRequest,
};
use crate::instance::{
    cf_api_key, install_download_pool, is_cancelled, register_download_manager, safe_path_name,
    try_register_cancel, unregister_cancel,
};
use std::collections::HashSet;
use std::sync::atomic::Ordering;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

// ===== 整合包在线搜索 =====

const MODRINTH_MIRROR_API: &str = "https://mod.mcimirror.top/modrinth/v2";
const MODRINTH_OFFICIAL_API: &str = "https://api.modrinth.com/v2";
const CURSEFORGE_MIRROR_API: &str = "https://mod.mcimirror.top/curseforge/v1";
const CURSEFORGE_OFFICIAL_API: &str = "https://api.curseforge.com/v1";

#[derive(serde::Serialize, Clone)]
pub struct ModpackResult {
    pub title: String,
    pub description: String,
    pub author: String,
    pub downloads: u64,
    pub icon_url: String,
    pub icon_urls: Vec<String>,
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
            let mut icon_urls = merged[idx].icon_urls.clone();
            icon_urls.extend(cf.icon_urls);
            let icon_urls = unique_icon_urls(icon_urls);
            if merged[idx].icon_url.is_empty() {
                merged[idx].icon_url = icon_urls.first().cloned().unwrap_or_default();
            }
            merged[idx].icon_urls = icon_urls;
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
    let mirror_url = build_mr_modpack_search_url(MODRINTH_MIRROR_API, query, offset);
    let official_url = build_mr_modpack_search_url(MODRINTH_OFFICIAL_API, query, offset);

    let mut last_err = None;
    for url in [mirror_url, official_url] {
        match fetch_mr_modpack_search(http, &url) {
            Ok(results) if !results.is_empty() => return Ok(results),
            Ok(results) if url.contains(MODRINTH_OFFICIAL_API) => return Ok(results),
            Ok(_) => {}
            Err(err) => last_err = Some(err),
        }
    }

    Err(last_err.unwrap_or_else(|| "Modrinth 整合包搜索无结果".to_string()))
}

fn build_mr_modpack_search_url(base: &str, query: &str, offset: u32) -> String {
    if query.is_empty() {
        format!(
            "{}/search?facets=[[\"project_type:modpack\"]]&limit=20&offset={}&index=downloads",
            base, offset
        )
    } else {
        format!(
            "{}/search?query={}&facets=[[\"project_type:modpack\"]]&limit=20&offset={}&index=relevance",
            base,
            urlencoding::encode(query),
            offset
        )
    }
}

fn fetch_mr_modpack_search(
    http: &reqwest::blocking::Client,
    url: &str,
) -> Result<Vec<ModpackResult>, String> {
    let resp = http.get(url).send().map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    let json: serde_json::Value = resp.json().map_err(|e| e.to_string())?;
    let mut results = Vec::new();
    if let Some(hits) = json["hits"].as_array() {
        for hit in hits {
            let slug = hit["slug"].as_str().unwrap_or("").to_string();
            let icon_urls =
                unique_icon_urls(vec![hit["icon_url"].as_str().unwrap_or("").to_string()]);
            results.push(ModpackResult {
                title: hit["title"].as_str().unwrap_or("").to_string(),
                description: hit["description"].as_str().unwrap_or("").to_string(),
                author: hit["author"].as_str().unwrap_or("").to_string(),
                downloads: hit["downloads"].as_u64().unwrap_or(0),
                icon_url: icon_urls.first().cloned().unwrap_or_default(),
                icon_urls,
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
    let mirror_url = build_cf_modpack_search_url(CURSEFORGE_MIRROR_API, query, offset);
    let official_url = build_cf_modpack_search_url(CURSEFORGE_OFFICIAL_API, query, offset);

    let mut last_err = None;
    for (url, official) in [(mirror_url, false), (official_url, true)] {
        match fetch_cf_modpack_search(http, &url, official) {
            Ok(results) if !results.is_empty() => return Ok(results),
            Ok(results) if official => return Ok(results),
            Ok(_) => {}
            Err(err) => last_err = Some(err),
        }
    }

    Err(last_err.unwrap_or_else(|| "CurseForge 整合包搜索无结果".to_string()))
}

fn build_cf_modpack_search_url(base: &str, query: &str, offset: u32) -> String {
    let sort_field = if query.is_empty() { "6" } else { "1" };
    format!(
        "{}/mods/search?gameId=432&classId=4471&searchFilter={}&pageSize=20&sortField={}&sortOrder=desc&index={}",
        base,
        urlencoding::encode(query),
        sort_field,
        offset,
    )
}

fn fetch_cf_modpack_search(
    http: &reqwest::blocking::Client,
    url: &str,
    official: bool,
) -> Result<Vec<ModpackResult>, String> {
    let mut req = http.get(url).header("Accept", "application/json");
    if official {
        req = req.header("x-api-key", &cf_api_key());
    }
    let resp = req.send().map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    let json: serde_json::Value = resp.json().map_err(|e| e.to_string())?;

    let mut results = Vec::new();
    if let Some(data) = json["data"].as_array() {
        for item in data {
            let authors = item["authors"]
                .as_array()
                .and_then(|a| a.first())
                .and_then(|a| a["name"].as_str())
                .unwrap_or("");
            let mut icon_urls = Vec::new();
            if let Some(url) = item["logo"]["thumbnailUrl"].as_str() {
                push_curseforge_icon_urls(&mut icon_urls, url);
            }
            if let Some(url) = item["logo"]["url"].as_str() {
                push_curseforge_icon_urls(&mut icon_urls, url);
            }
            let icon_urls = unique_icon_urls(icon_urls);
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
                icon_url: icon_urls.first().cloned().unwrap_or_default(),
                icon_urls,
                mr_url: String::new(),
                cf_url: cf_url.to_string(),
                project_id: id.to_string(),
                source: "curseforge".to_string(),
            });
        }
    }
    Ok(results)
}

fn push_curseforge_icon_urls(out: &mut Vec<String>, url: &str) {
    let Some(path) = forgecdn_path(url) else {
        out.push(url.to_string());
        return;
    };
    // CF 图片有多个 CDN 域名，前端会按顺序失败切换。
    for host in [
        "media.forgecdn.net",
        "edge.forgecdn.net",
        "mediafilez.forgecdn.net",
    ] {
        out.push(format!("https://{}{}", host, path));
    }
}

fn forgecdn_path(url: &str) -> Option<&str> {
    for host in [
        "https://media.forgecdn.net",
        "https://edge.forgecdn.net",
        "https://mediafilez.forgecdn.net",
        "http://media.forgecdn.net",
        "http://edge.forgecdn.net",
        "http://mediafilez.forgecdn.net",
    ] {
        if let Some(path) = url.strip_prefix(host) {
            return Some(path);
        }
    }
    None
}

fn unique_icon_urls(urls: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for url in urls {
        let trimmed = url.trim();
        if trimmed.is_empty() || !seen.insert(trimmed.to_string()) {
            continue;
        }
        out.push(trimmed.to_string());
    }
    out
}

// ===== 整合包版本列表 + 一键安装 =====

const MODPACK_VERSION_PAGE_SIZE: usize = 20;

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
    offset: Option<u32>,
) -> Result<Vec<ModpackVersionInfo>, String> {
    let offset = offset.unwrap_or(0);
    tokio::task::spawn_blocking(move || do_get_modpack_versions(&project_id, &source, offset))
        .await
        .map_err(|e| e.to_string())?
}

fn do_get_modpack_versions(
    project_id: &str,
    source: &str,
    offset: u32,
) -> Result<Vec<ModpackVersionInfo>, String> {
    let http = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .user_agent("OAOI-Launcher/1.0")
        .build()
        .map_err(|e| e.to_string())?;

    match source {
        "modrinth" | "both" => get_mr_modpack_versions(&http, project_id, offset),
        "curseforge" => get_cf_modpack_versions(&http, project_id, offset),
        _ => get_mr_modpack_versions(&http, project_id, offset),
    }
}

fn get_mr_modpack_versions(
    http: &reqwest::blocking::Client,
    project_id: &str,
    offset: u32,
) -> Result<Vec<ModpackVersionInfo>, String> {
    let url = format!("https://api.modrinth.com/v2/project/{}/version", project_id);
    let resp = http.get(&url).send().map_err(|e| e.to_string())?;
    let json: serde_json::Value = resp.json().map_err(|e| e.to_string())?;
    let arr = json.as_array().ok_or("格式错误")?;

    let mut results = Vec::new();
    // Modrinth 项目版本接口一次返回完整列表，这里按前端滚动页切片。
    for ver in arr
        .iter()
        .skip(offset as usize)
        .take(MODPACK_VERSION_PAGE_SIZE)
    {
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
    offset: u32,
) -> Result<Vec<ModpackVersionInfo>, String> {
    let url = format!(
        "https://api.curseforge.com/v1/mods/{}/files?pageSize={}&index={}",
        project_id, MODPACK_VERSION_PAGE_SIZE, offset
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
    for file in data.iter().take(MODPACK_VERSION_PAGE_SIZE) {
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
    let cancel_flag = match try_register_cancel(&file_name) {
        Ok(flag) => flag,
        Err(error) => {
            let _ = tauri::Emitter::emit(
                &app_handle,
                "install-progress",
                serde_json::json!({
                    "name": &file_name, "stage": "error", "current": 0, "total": 0, "detail": &error
                }),
            );
            return Err(error);
        }
    };
    std::thread::spawn(move || {
        let result = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            do_install_modpack_direct(
                &app_handle,
                download_urls,
                &file_name,
                &game_dir,
                &java_path,
                use_mirror,
            )
        })) {
            Ok(result) => result,
            Err(payload) => Err(format!(
                "安装线程崩溃: {}",
                panic_payload_to_string(payload)
            )),
        };
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

fn panic_payload_to_string(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "未知错误".to_string()
    }
}

fn modpack_direct_temp_dir(file_name: &str) -> std::path::PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    std::env::temp_dir().join("oaoi_modpack_dl").join(format!(
        "{}-{}-{}",
        std::process::id(),
        stamp,
        file_name
    ))
}

struct TempDirGuard {
    path: std::path::PathBuf,
    cleaned: bool,
}

impl TempDirGuard {
    fn new(path: std::path::PathBuf) -> Self {
        Self {
            path,
            cleaned: false,
        }
    }

    fn path(&self) -> &std::path::Path {
        &self.path
    }

    fn cleanup(&mut self) -> Result<(), String> {
        if !self.path.exists() {
            self.cleaned = true;
            return Ok(());
        }
        match std::fs::remove_dir_all(&self.path) {
            Ok(()) => {
                self.cleaned = true;
                Ok(())
            }
            Err(error) => Err(format!(
                "临时目录清理失败: {} ({})",
                self.path.display(),
                error
            )),
        }
    }
}

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        if self.cleaned || !self.path.exists() {
            return;
        }
        // panic 或提前返回时兜底清理在线整合包临时目录。
        if let Err(error) = std::fs::remove_dir_all(&self.path) {
            eprintln!(
                "[modpack-dl] 临时目录兜底清理失败: {} ({})",
                self.path.display(),
                error
            );
        }
    }
}

fn append_cleanup_error(error: String, cleanup_result: Result<(), String>) -> String {
    match cleanup_result {
        Ok(()) => error,
        Err(cleanup_error) => format!("{}; {}", error, cleanup_error),
    }
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
    let mut tmp_dir = TempDirGuard::new(modpack_direct_temp_dir(file_name));
    std::fs::create_dir_all(tmp_dir.path()).map_err(|e| format!("创建临时目录失败: {}", e))?;
    let tmp_path = tmp_dir.path().join(file_name);
    let downloaded = match download_modpack_archive_with_fallbacks(
        app_handle,
        &http,
        &download_urls,
        file_name,
        &tmp_path,
    ) {
        Ok(downloaded) => downloaded,
        Err(error) => {
            let cleanup_result = tmp_dir.cleanup();
            return Err(append_cleanup_error(error, cleanup_result));
        }
    };
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
        let cleanup_result = tmp_dir.cleanup();
        return Err(append_cleanup_error(
            "用户取消安装".to_string(),
            cleanup_result,
        ));
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

    match result {
        Ok(message) => match tmp_dir.cleanup() {
            Ok(()) => {
                let _ = tauri::Emitter::emit(
                    app_handle,
                    "install-progress",
                    serde_json::json!({
                        "name": file_name, "stage": "done", "current": 1, "total": 1, "detail": message
                    }),
                );
                Ok(message)
            }
            Err(cleanup_error) => {
                eprintln!("[modpack-dl] {}", cleanup_error);
                Err(format!("安装已完成，但{}", cleanup_error))
            }
        },
        Err(error) => {
            let cleanup_result = tmp_dir.cleanup();
            Err(append_cleanup_error(error, cleanup_result))
        }
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

fn modpack_archive_download_options() -> DownloadEngineOptions {
    let mut options = DownloadEngineOptions::default();
    options.max_global_connections = 64;
    options.max_connections_per_file = 64;
    options.max_active_files = 1;
    options.candidate_no_progress_timeout = Duration::from_secs(10);
    options.candidate_retry_delay = Duration::from_secs(15);
    options.source_cooldown_duration = Duration::from_secs(15);
    options.read_timeout = Duration::from_secs(15);
    options
}

fn download_modpack_archive_with_fallbacks(
    app_handle: &tauri::AppHandle,
    _http: &reqwest::blocking::Client,
    download_urls: &[String],
    file_name: &str,
    tmp_path: &std::path::Path,
) -> Result<u64, String> {
    let candidates = download_urls
        .iter()
        .filter(|url| !url.trim().is_empty())
        .cloned()
        .map(DownloadCandidate::new)
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return Err("没有可用整合包下载地址".to_string());
    }
    let _ = std::fs::remove_file(tmp_path);

    let options = modpack_archive_download_options();
    let pool = install_download_pool(file_name, 64);
    let manager = DownloadManager::with_options_and_pool(options, pool)?;
    let _manager_registration = register_download_manager(file_name, &manager);

    let request = DownloadRequest::new("modpack-archive", candidates, tmp_path);
    let app_for_event = app_handle.clone();
    let file_name_for_event = file_name.to_string();
    let outcomes = manager.download_many(vec![request], move |event| {
        if let DownloadEvent::Progress(progress) = event {
            emit_download_progress(
                &app_for_event,
                &file_name_for_event,
                progress.downloaded,
                progress.total.unwrap_or(0),
            );
        }
    });
    if is_cancelled(file_name) {
        let _ = std::fs::remove_file(tmp_path);
        return Err("用户取消下载".to_string());
    }

    match outcomes.into_iter().next() {
        Some(DownloadOutcome::Finished(result)) => {
            if result.bytes == 0 {
                let _ = std::fs::remove_file(tmp_path);
                return Err("下载整合包失败: 文件为空".to_string());
            }
            Ok(result.bytes)
        }
        Some(DownloadOutcome::Failed { error, .. }) => {
            let _ = std::fs::remove_file(tmp_path);
            Err(format!("下载整合包失败: {}", error))
        }
        None => {
            let _ = std::fs::remove_file(tmp_path);
            Err("下载整合包失败: 没有下载结果".to_string())
        }
    }
}
