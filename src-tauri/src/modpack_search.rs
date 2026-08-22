use crate::downloader::event::{DownloadEvent, DownloadOutcome};
use crate::downloader::{
    DownloadCandidate, DownloadEngineOptions, DownloadManager, DownloadRequest,
};
use crate::instance::{
    cf_api_key, install_download_pool, is_cancelled, register_download_manager, safe_path_name,
    scoped_task_key, try_register_cancel, unregister_cancel,
};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

// ===== 整合包在线搜索 =====

const MODRINTH_MIRROR_API: &str = "https://mod.mcimirror.top/modrinth/v2";
const MODRINTH_OFFICIAL_API: &str = "https://api.modrinth.com/v2";
const CURSEFORGE_MIRROR_API: &str = "https://mod.mcimirror.top/curseforge/v1";
const CURSEFORGE_OFFICIAL_API: &str = "https://api.curseforge.com/v1";
const MODPACK_VERSION_CACHE_TTL: Duration = Duration::from_secs(10 * 60);
const MAX_MODPACK_VERSION_CACHE_ENTRIES: usize = 64;

#[derive(Clone)]
struct CachedModrinthVersions {
    fetched_at: Instant,
    versions: Vec<serde_json::Value>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct ArchiveDownloadMetadata {
    expected_size: Option<u64>,
    expected_sha1: Option<String>,
}

fn modrinth_version_cache() -> &'static Mutex<HashMap<String, CachedModrinthVersions>> {
    static CACHE: OnceLock<Mutex<HashMap<String, CachedModrinthVersions>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

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
    pub mr_project_id: String,
    pub cf_project_id: String,
    pub slug: String,
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
    let (mr_result, cf_result) = std::thread::scope(|s| {
        let q1 = query.to_string();
        let h1 = http.clone();
        let off1 = offset;
        let mr_handle = s.spawn(move || search_mr_modpacks(&h1, &q1, off1));

        let q2 = query.to_string();
        let h2 = http.clone();
        let off2 = offset;
        let cf_handle = s.spawn(move || search_cf_modpacks(&h2, &q2, off2));

        (
            mr_handle
                .join()
                .unwrap_or_else(|_| Err("Modrinth 搜索线程异常退出".to_string())),
            cf_handle
                .join()
                .unwrap_or_else(|_| Err("CurseForge 搜索线程异常退出".to_string())),
        )
    });
    let (mr_results, cf_results) = merge_platform_search_results(mr_result, cf_result)?;

    let mut merged = merge_cross_platform_results(mr_results, cf_results);
    merged.sort_by(|a, b| b.downloads.cmp(&a.downloads));
    Ok(merged)
}

fn merge_cross_platform_results(
    mr_results: Vec<ModpackResult>,
    cf_results: Vec<ModpackResult>,
) -> Vec<ModpackResult> {
    let mut merged = Vec::new();
    let mut mr_map: HashMap<String, usize> = HashMap::new();

    for item in mr_results {
        let key = cross_platform_identity(&item)
            .unwrap_or_else(|| format!("modrinth\0{}", item.mr_project_id));
        mr_map.insert(key, merged.len());
        merged.push(item);
    }

    for cf in cf_results {
        let key = cross_platform_identity(&cf)
            .unwrap_or_else(|| format!("curseforge\0{}", cf.cf_project_id));
        if let Some(&idx) = mr_map.get(&key) {
            merged[idx].cf_url = cf.cf_url;
            merged[idx].cf_project_id = cf.cf_project_id;
            merged[idx].source = "both".to_string();
            let mut icon_urls = merged[idx].icon_urls.clone();
            icon_urls.extend(cf.icon_urls);
            let icon_urls = unique_icon_urls(icon_urls);
            if merged[idx].icon_url.is_empty() {
                merged[idx].icon_url = icon_urls.first().cloned().unwrap_or_default();
            }
            merged[idx].icon_urls = icon_urls;
            if cf.downloads > merged[idx].downloads {
                merged[idx].downloads = cf.downloads;
            }
        } else {
            merged.push(cf);
        }
    }

    merged
}

fn cross_platform_identity(item: &ModpackResult) -> Option<String> {
    let slug = normalize_project_identity_part(&item.slug);
    let author = normalize_project_identity_part(&item.author);
    if slug.is_empty() || author.is_empty() {
        return None;
    }
    Some(format!("{}\0{}", slug, author))
}

fn normalize_project_identity_part(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn merge_platform_search_results(
    mr_result: Result<Vec<ModpackResult>, String>,
    cf_result: Result<Vec<ModpackResult>, String>,
) -> Result<(Vec<ModpackResult>, Vec<ModpackResult>), String> {
    match (mr_result, cf_result) {
        (Ok(mr), Ok(cf)) => Ok((mr, cf)),
        (Ok(mr), Err(error)) => {
            eprintln!("[modpack-search] CurseForge 搜索失败，保留 Modrinth 结果: {error}");
            Ok((mr, Vec::new()))
        }
        (Err(error), Ok(cf)) => {
            eprintln!("[modpack-search] Modrinth 搜索失败，保留 CurseForge 结果: {error}");
            Ok((Vec::new(), cf))
        }
        (Err(mr_error), Err(cf_error)) => Err(format!(
            "整合包搜索失败: Modrinth: {}; CurseForge: {}",
            mr_error, cf_error
        )),
    }
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
                mr_project_id: hit["project_id"].as_str().unwrap_or("").to_string(),
                cf_project_id: String::new(),
                slug,
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
                mr_project_id: String::new(),
                cf_project_id: id.to_string(),
                slug: item["slug"].as_str().unwrap_or("").to_string(),
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
    pub sha1: String,
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
    let versions = get_cached_or_fetch_modrinth_versions(http, project_id)?;

    let mut results = Vec::new();
    for ver in modrinth_version_page(&versions, offset) {
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
        let (dl_url, fname, fsize, sha1) = if let Some(files) = files {
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
                    f["hashes"]["sha1"].as_str().map(str::to_string),
                )
            } else {
                continue;
            }
        } else {
            continue;
        };
        let download_urls = dedupe_urls(vec![dl_url.clone()]);
        results.push(ModpackVersionInfo {
            version_name: ver["name"]
                .as_str()
                .unwrap_or(ver["version_number"].as_str().unwrap_or(""))
                .to_string(),
            mc_versions: game_versions,
            download_url: dl_url.clone(),
            download_urls,
            file_name: fname,
            file_size: fsize,
            sha1: sha1.unwrap_or_default(),
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

fn modrinth_version_page(versions: &[serde_json::Value], offset: u32) -> &[serde_json::Value] {
    let start = (offset as usize).min(versions.len());
    let end = start
        .saturating_add(MODPACK_VERSION_PAGE_SIZE)
        .min(versions.len());
    &versions[start..end]
}

fn get_cached_or_fetch_modrinth_versions(
    http: &reqwest::blocking::Client,
    project_id: &str,
) -> Result<Vec<serde_json::Value>, String> {
    {
        let mut cache = modrinth_version_cache()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        cache.retain(|_, entry| entry.fetched_at.elapsed() < MODPACK_VERSION_CACHE_TTL);
        if let Some(entry) = cache.get(project_id) {
            return Ok(entry.versions.clone());
        }
    }

    let encoded_project_id = urlencoding::encode(project_id);
    let mirror_url = format!(
        "{}/project/{}/version",
        MODRINTH_MIRROR_API, encoded_project_id
    );
    let official_url = format!(
        "{}/project/{}/version",
        MODRINTH_OFFICIAL_API, encoded_project_id
    );
    let versions = fetch_version_array_with_fallback(
        http,
        "Modrinth",
        vec![(mirror_url, false), (official_url, false)],
    )?;

    let mut cache = modrinth_version_cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    cache.retain(|_, entry| entry.fetched_at.elapsed() < MODPACK_VERSION_CACHE_TTL);
    if cache.len() >= MAX_MODPACK_VERSION_CACHE_ENTRIES {
        if let Some(oldest_key) = cache
            .iter()
            .min_by_key(|(_, entry)| entry.fetched_at)
            .map(|(key, _)| key.clone())
        {
            cache.remove(&oldest_key);
        }
    }
    cache.insert(
        project_id.to_string(),
        CachedModrinthVersions {
            fetched_at: Instant::now(),
            versions: versions.clone(),
        },
    );
    Ok(versions)
}

fn get_cf_modpack_versions(
    http: &reqwest::blocking::Client,
    project_id: &str,
    offset: u32,
) -> Result<Vec<ModpackVersionInfo>, String> {
    let encoded_project_id = urlencoding::encode(project_id);
    let mirror_url = format!(
        "{}/mods/{}/files?pageSize={}&index={}",
        CURSEFORGE_MIRROR_API, encoded_project_id, MODPACK_VERSION_PAGE_SIZE, offset
    );
    let official_url = format!(
        "{}/mods/{}/files?pageSize={}&index={}",
        CURSEFORGE_OFFICIAL_API, encoded_project_id, MODPACK_VERSION_PAGE_SIZE, offset
    );
    let data = fetch_version_array_with_fallback(
        http,
        "CurseForge",
        vec![(mirror_url, false), (official_url, true)],
    )?;

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
        let file_size = file["fileLength"].as_u64().unwrap_or(0);
        let sha1 = crate::modpack_sources::sha1_from_curseforge_hashes(&file["hashes"]);
        results.push(ModpackVersionInfo {
            version_name: file["displayName"].as_str().unwrap_or("").to_string(),
            mc_versions: game_versions,
            download_url: dl_url,
            download_urls,
            file_name: safe_file_name,
            file_size,
            sha1: sha1.unwrap_or_default(),
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

fn fetch_version_array_with_fallback(
    http: &reqwest::blocking::Client,
    platform: &str,
    candidates: Vec<(String, bool)>,
) -> Result<Vec<serde_json::Value>, String> {
    let mut errors = Vec::new();
    let mut successful_empty = None;
    for (url, official_curseforge) in candidates {
        match fetch_version_array(http, &url, official_curseforge) {
            Ok(values) if !values.is_empty() => return Ok(values),
            Ok(values) => {
                successful_empty = Some(values);
                errors.push(format!("{} 返回空列表", url));
            }
            Err(error) => errors.push(format!("{}: {}", url, error)),
        }
    }
    if let Some(values) = successful_empty {
        return Ok(values);
    }
    Err(format!(
        "获取 {} 整合包版本失败: {}",
        platform,
        errors.join("; ")
    ))
}

fn fetch_version_array(
    http: &reqwest::blocking::Client,
    url: &str,
    official_curseforge: bool,
) -> Result<Vec<serde_json::Value>, String> {
    let mut request = http.get(url).header("Accept", "application/json");
    if official_curseforge {
        request = request.header("x-api-key", &cf_api_key());
    }
    let response = request.send().map_err(|error| error.to_string())?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("HTTP {}", status));
    }
    let json: serde_json::Value = response.json().map_err(|error| error.to_string())?;
    if let Some(values) = json.as_array() {
        return Ok(values.clone());
    }
    json["data"]
        .as_array()
        .cloned()
        .ok_or_else(|| "响应格式错误".to_string())
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

fn archive_mirror_url(url: &str) -> Option<String> {
    const RULES: [(&str, &str); 8] = [
        ("https://cdn.modrinth.com/", "https://mod.mcimirror.top/"),
        ("http://cdn.modrinth.com/", "https://mod.mcimirror.top/"),
        ("https://edge.forgecdn.net/", "https://mod.mcimirror.top/"),
        ("http://edge.forgecdn.net/", "https://mod.mcimirror.top/"),
        (
            "https://mediafilez.forgecdn.net/",
            "https://mod.mcimirror.top/",
        ),
        (
            "http://mediafilez.forgecdn.net/",
            "https://mod.mcimirror.top/",
        ),
        ("https://media.forgecdn.net/", "https://mod.mcimirror.top/"),
        ("http://media.forgecdn.net/", "https://mod.mcimirror.top/"),
    ];
    RULES.iter().find_map(|(official, mirror)| {
        url.strip_prefix(official)
            .map(|rest| format!("{}{}", mirror, rest))
    })
}

fn is_archive_mirror_url(url: &str) -> bool {
    url.starts_with("https://mod.mcimirror.top/") || url.starts_with("http://mod.mcimirror.top/")
}

fn order_archive_download_urls(urls: Vec<String>, use_mirror: bool) -> Vec<String> {
    let originals = dedupe_urls(urls);
    let mut expanded = originals.clone();
    for url in &originals {
        if let Some(mirror) = archive_mirror_url(url) {
            expanded.push(mirror);
        }
    }

    let mut mirrors = Vec::new();
    let mut officials = Vec::new();
    for url in dedupe_urls(expanded) {
        if is_archive_mirror_url(&url) {
            mirrors.push(url);
        } else {
            officials.push(url);
        }
    }
    if use_mirror {
        mirrors.extend(officials);
        mirrors
    } else {
        officials.extend(mirrors);
        officials
    }
}

fn apply_archive_metadata(
    mut request: DownloadRequest,
    metadata: &ArchiveDownloadMetadata,
) -> DownloadRequest {
    if let Some(size) = metadata.expected_size {
        request = request.with_expected_size(size);
    }
    if let Some(sha1) = metadata.expected_sha1.as_deref() {
        request = request.with_expected_sha1(sha1.to_string());
    }
    request
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
    expected_size: Option<u64>,
    expected_sha1: Option<String>,
) -> Result<String, String> {
    let file_name = safe_path_name(&file_name, "文件名")?;
    let download_urls = order_archive_download_urls(
        normalize_download_urls(download_url, download_urls),
        use_mirror,
    );
    let archive_metadata = ArchiveDownloadMetadata {
        expected_size: expected_size.filter(|size| *size > 0),
        expected_sha1: expected_sha1
            .map(|value| value.trim().to_string())
            .filter(|value| {
                value.len() == 40 && value.chars().all(|character| character.is_ascii_hexdigit())
            }),
    };
    let cancel_name = scoped_task_key(&game_dir, &file_name);
    let cancel_flag = match try_register_cancel(&cancel_name) {
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
                &cancel_name,
                archive_metadata,
            )
        })) {
            Ok(result) => result,
            Err(payload) => Err(format!(
                "安装线程崩溃: {}",
                panic_payload_to_string(payload)
            )),
        };
        unregister_cancel(&cancel_name);
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

fn modpack_direct_temp_dir(
    game_dir: &str,
    file_name: &str,
    download_urls: &[String],
    metadata: &ArchiveDownloadMetadata,
) -> std::path::PathBuf {
    let mut stable_urls = download_urls.to_vec();
    stable_urls.sort();
    stable_urls.dedup();

    let mut digest = Sha256::new();
    digest.update(game_dir.as_bytes());
    digest.update([0]);
    digest.update(file_name.as_bytes());
    digest.update([0]);
    if let Some(sha1) = metadata.expected_sha1.as_deref() {
        digest.update(b"sha1");
        digest.update(sha1.as_bytes());
    } else {
        digest.update(b"urls");
        for url in stable_urls {
            digest.update(url.as_bytes());
            digest.update([0]);
        }
    }
    if let Some(size) = metadata.expected_size {
        digest.update(size.to_le_bytes());
    }

    std::env::temp_dir()
        .join("oaoi_modpack_dl")
        .join(format!("{:x}", digest.finalize()))
}

struct TempDirGuard {
    path: std::path::PathBuf,
}

impl TempDirGuard {
    fn new(path: std::path::PathBuf) -> Self {
        Self { path }
    }

    fn path(&self) -> &std::path::Path {
        &self.path
    }

    fn cleanup(&mut self) -> Result<(), String> {
        if !self.path.exists() {
            return Ok(());
        }
        match std::fs::remove_dir_all(&self.path) {
            Ok(()) => Ok(()),
            Err(error) => Err(format!(
                "临时目录清理失败: {} ({})",
                self.path.display(),
                error
            )),
        }
    }
}

fn do_install_modpack_direct(
    app_handle: &tauri::AppHandle,
    download_urls: Vec<String>,
    file_name: &str,
    game_dir: &str,
    java_path: &str,
    use_mirror: bool,
    cancel_name: &str,
    archive_metadata: ArchiveDownloadMetadata,
) -> Result<String, String> {
    let _ = tauri::Emitter::emit(
        app_handle,
        "install-progress",
        serde_json::json!({
            "name": file_name, "stage": "downloading", "current": 0, "total": 1, "detail": format!("正在下载 {}...", file_name)
        }),
    );

    // 保存到临时文件（流式写入）
    let mut tmp_dir = TempDirGuard::new(modpack_direct_temp_dir(
        game_dir,
        file_name,
        &download_urls,
        &archive_metadata,
    ));
    std::fs::create_dir_all(tmp_dir.path()).map_err(|e| format!("创建临时目录失败: {}", e))?;
    let tmp_path = tmp_dir.path().join(file_name);
    let downloaded = match download_modpack_archive_with_fallbacks(
        app_handle,
        &download_urls,
        file_name,
        cancel_name,
        &tmp_path,
        &archive_metadata,
    ) {
        Ok(downloaded) => downloaded,
        Err(error) => return Err(error),
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
    if is_cancelled(cancel_name) {
        return Err("用户取消安装".to_string());
    }

    eprintln!(
        "[modpack-dl] 下载完成 ({:.1} MB), 开始安装...",
        downloaded as f64 / 1048576.0
    );

    // 调用现有的 import_modpack 逻辑，传入 file_name 作为 display_name
    let result = crate::modpack::do_import_modpack_named_with_cancel(
        app_handle,
        &tmp_path.to_string_lossy(),
        game_dir,
        java_path,
        use_mirror,
        Some(file_name),
        cancel_name,
    );

    match result {
        Ok(message) => {
            if let Err(error) = tmp_dir.cleanup() {
                eprintln!("[modpack-dl] 安装成功，临时文件稍后清理: {}", error);
            }
            let _ = tauri::Emitter::emit(
                app_handle,
                "install-progress",
                serde_json::json!({
                    "name": file_name, "stage": "done", "current": 1, "total": 1, "detail": message
                }),
            );
            Ok(message)
        }
        Err(error) => Err(error),
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
    options.max_global_connections = DownloadEngineOptions::default_global_connection_limit();
    options.max_connections_per_file = options.max_global_connections;
    options.max_active_files = 1;
    options.candidate_no_progress_timeout = Duration::from_secs(10);
    options.candidate_retry_delay = Duration::from_secs(15);
    options.source_cooldown_duration = Duration::from_secs(15);
    options.read_timeout = Duration::from_secs(15);
    options
}

fn download_modpack_archive_with_fallbacks(
    app_handle: &tauri::AppHandle,
    download_urls: &[String],
    file_name: &str,
    cancel_name: &str,
    tmp_path: &std::path::Path,
    archive_metadata: &ArchiveDownloadMetadata,
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
    let options = modpack_archive_download_options();
    let max_connections = options.max_global_connections;
    let pool = install_download_pool(cancel_name, max_connections);
    let manager = DownloadManager::with_options_and_pool(options, pool)?;
    let _manager_registration = register_download_manager(cancel_name, &manager);

    let request = apply_archive_metadata(
        DownloadRequest::new("modpack-archive", candidates, tmp_path),
        archive_metadata,
    );
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
    if is_cancelled(cancel_name) {
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
        Some(DownloadOutcome::Failed { error, .. }) => Err(format!("下载整合包失败: {}", error)),
        None => Err("下载整合包失败: 没有下载结果".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_result(title: &str) -> ModpackResult {
        ModpackResult {
            title: title.to_string(),
            description: String::new(),
            author: "author".to_string(),
            downloads: 0,
            icon_url: String::new(),
            icon_urls: Vec::new(),
            mr_url: String::new(),
            cf_url: String::new(),
            project_id: title.to_string(),
            mr_project_id: title.to_string(),
            cf_project_id: String::new(),
            slug: title.to_string(),
            source: String::new(),
        }
    }

    fn platform_result(source: &str, title: &str, slug: &str, author: &str) -> ModpackResult {
        let mut result = test_result(title);
        result.source = source.to_string();
        result.slug = slug.to_string();
        result.author = author.to_string();
        if source == "curseforge" {
            result.project_id = "22".to_string();
            result.mr_project_id.clear();
            result.cf_project_id = "22".to_string();
            result.cf_url = "https://www.curseforge.com/test".to_string();
        } else {
            result.project_id = "mr-id".to_string();
            result.mr_project_id = "mr-id".to_string();
            result.cf_project_id.clear();
            result.mr_url = "https://modrinth.com/test".to_string();
        }
        result
    }

    #[test]
    fn one_successful_search_platform_keeps_results() {
        let (mr, cf) = merge_platform_search_results(
            Ok(vec![test_result("test")]),
            Err("network error".to_string()),
        )
        .unwrap();
        assert_eq!(mr.len(), 1);
        assert!(cf.is_empty());
    }

    #[test]
    fn both_failed_search_platforms_return_error() {
        let error =
            merge_platform_search_results(Err("mr error".to_string()), Err("cf error".to_string()))
                .err()
                .unwrap();
        assert!(error.contains("mr error"));
        assert!(error.contains("cf error"));
    }

    #[test]
    fn modrinth_version_page_uses_offset_without_refetching() {
        let versions = (0..45)
            .map(|index| serde_json::json!({ "id": index }))
            .collect::<Vec<_>>();
        let page = modrinth_version_page(&versions, 20);
        assert_eq!(page.len(), 20);
        assert_eq!(page[0]["id"].as_u64(), Some(20));
        assert_eq!(modrinth_version_page(&versions, 40).len(), 5);
    }

    #[test]
    fn archive_metadata_is_written_to_download_request() {
        let request = DownloadRequest::new(
            "test",
            vec![DownloadCandidate::new(
                "https://example.invalid/file.mrpack",
            )],
            "file.mrpack",
        );
        let request = apply_archive_metadata(
            request,
            &ArchiveDownloadMetadata {
                expected_size: Some(1234),
                expected_sha1: Some("0123456789012345678901234567890123456789".to_string()),
            },
        );
        assert_eq!(request.expected_size, Some(1234));
        assert_eq!(
            request.expected_sha1.as_deref(),
            Some("0123456789012345678901234567890123456789")
        );
    }

    #[test]
    fn same_title_different_project_identity_is_not_merged() {
        let merged = merge_cross_platform_results(
            vec![platform_result(
                "modrinth",
                "Same title",
                "first-pack",
                "Alice",
            )],
            vec![platform_result(
                "curseforge",
                "Same title",
                "second-pack",
                "Bob",
            )],
        );
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn matching_slug_and_author_preserves_both_project_ids() {
        let merged = merge_cross_platform_results(
            vec![platform_result("modrinth", "Pack", "same-pack", "Alice")],
            vec![platform_result("curseforge", "Pack", "same-pack", "Alice")],
        );
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].source, "both");
        assert_eq!(merged[0].mr_project_id, "mr-id");
        assert_eq!(merged[0].cf_project_id, "22");
    }

    #[test]
    fn archive_candidates_follow_selected_source_order() {
        let official = "https://cdn.modrinth.com/data/project/version/pack.mrpack";
        let mirror = "https://mod.mcimirror.top/data/project/version/pack.mrpack";
        let mirror_first = order_archive_download_urls(vec![official.to_string()], true);
        assert_eq!(mirror_first, vec![mirror.to_string(), official.to_string()]);

        let official_first = order_archive_download_urls(vec![official.to_string()], false);
        assert_eq!(
            official_first,
            vec![official.to_string(), mirror.to_string()]
        );
    }

    #[test]
    fn archive_resume_directory_is_stable_and_task_scoped() {
        let metadata = ArchiveDownloadMetadata {
            expected_size: Some(123),
            expected_sha1: Some("0123456789012345678901234567890123456789".to_string()),
        };
        let first = modpack_direct_temp_dir(
            "E:/game-a",
            "pack.mrpack",
            &["https://example.invalid/a".to_string()],
            &metadata,
        );
        let repeated = modpack_direct_temp_dir(
            "E:/game-a",
            "pack.mrpack",
            &["https://example.invalid/a".to_string()],
            &metadata,
        );
        let other_game = modpack_direct_temp_dir(
            "E:/game-b",
            "pack.mrpack",
            &["https://example.invalid/a".to_string()],
            &metadata,
        );
        assert_eq!(first, repeated);
        assert_ne!(first, other_game);
    }

    #[test]
    fn archive_download_uses_downloader_global_limit() {
        let options = modpack_archive_download_options();
        assert_eq!(
            options.max_global_connections,
            DownloadEngineOptions::default_global_connection_limit()
        );
        assert_eq!(
            options.max_connections_per_file,
            options.max_global_connections
        );
    }
}
