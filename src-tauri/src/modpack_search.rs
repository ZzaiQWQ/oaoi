use std::sync::atomic::Ordering;
use crate::instance::{cf_api_key, register_cancel, is_cancelled, unregister_cancel};

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
pub async fn search_modpacks(query: String, offset: Option<u32>) -> Result<Vec<ModpackResult>, String> {
    let offset = offset.unwrap_or(0);
    tokio::task::spawn_blocking(move || {
        do_search_modpacks(&query, offset)
    }).await.map_err(|e| format!("线程错误: {}", e))?
}

fn do_search_modpacks(query: &str, offset: u32) -> Result<Vec<ModpackResult>, String> {
    let http = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .user_agent("OAOI-Launcher/1.0")
        .build().map_err(|e| e.to_string())?;

    // 全部并发搜索（使用 thread::scope 避免孤儿线程）
    let (mr_results, cf_results) = std::thread::scope(|s| {
        let q1 = query.to_string();
        let h1 = http.clone();
        let off1 = offset;
        let mr_handle = s.spawn(move || {
            search_mr_modpacks(&h1, &q1, off1).unwrap_or_default()
        });

        let q2 = query.to_string();
        let h2 = http.clone();
        let off2 = offset;
        let cf_handle = s.spawn(move || {
            search_cf_modpacks(&h2, &q2, off2).unwrap_or_default()
        });

        (mr_handle.join().unwrap_or_default(), cf_handle.join().unwrap_or_default())
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

fn search_mr_modpacks(http: &reqwest::blocking::Client, query: &str, offset: u32) -> Result<Vec<ModpackResult>, String> {
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

fn search_cf_modpacks(http: &reqwest::blocking::Client, query: &str, offset: u32) -> Result<Vec<ModpackResult>, String> {
    let sort_field = if query.is_empty() { "6" } else { "1" };
    let url = format!(
        "https://api.curseforge.com/v1/mods/search?gameId=432&classId=4471&searchFilter={}&pageSize=20&sortField={}&sortOrder=desc&index={}",
        urlencoding::encode(query),
        sort_field,
        offset,
    );

    let resp = http.get(&url)
        .header("x-api-key", &cf_api_key())
        .header("Accept", "application/json")
        .send()
        .map_err(|e| e.to_string())?;

    let json: serde_json::Value = resp.json().map_err(|e| e.to_string())?;

    let mut results = Vec::new();
    if let Some(data) = json["data"].as_array() {
        for item in data {
            let authors = item["authors"].as_array()
                .and_then(|a| a.first())
                .and_then(|a| a["name"].as_str())
                .unwrap_or("");
            let logo = item["logo"]["url"].as_str().unwrap_or("");
            let id = item["id"].as_u64().unwrap_or(0);
            let fallback_url = format!("https://www.curseforge.com/minecraft/modpacks/{}", id);
            let cf_url = item["links"]["websiteUrl"].as_str()
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
    pub file_name: String,
    pub file_size: u64,
    pub date: String,
    pub version_id: String,
}

#[tauri::command]
pub async fn get_modpack_versions(project_id: String, source: String) -> Result<Vec<ModpackVersionInfo>, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(do_get_modpack_versions(&project_id, &source));
    });
    rx.recv().map_err(|_| "线程通信失败".to_string())?
}

fn do_get_modpack_versions(project_id: &str, source: &str) -> Result<Vec<ModpackVersionInfo>, String> {
    let http = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .user_agent("OAOI-Launcher/1.0")
        .build().map_err(|e| e.to_string())?;

    match source {
        "modrinth" | "both" => get_mr_modpack_versions(&http, project_id),
        "curseforge" => get_cf_modpack_versions(&http, project_id),
        _ => get_mr_modpack_versions(&http, project_id),
    }
}

fn get_mr_modpack_versions(http: &reqwest::blocking::Client, project_id: &str) -> Result<Vec<ModpackVersionInfo>, String> {
    let url = format!("https://api.modrinth.com/v2/project/{}/version", project_id);
    let resp = http.get(&url).send().map_err(|e| e.to_string())?;
    let json: serde_json::Value = resp.json().map_err(|e| e.to_string())?;
    let arr = json.as_array().ok_or("格式错误")?;

    let mut results = Vec::new();
    for ver in arr.iter().take(20) {
        let game_versions = ver["game_versions"].as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>().join(", "))
            .unwrap_or_default();
        let files = ver["files"].as_array();
        let (dl_url, fname, fsize) = if let Some(files) = files {
            if let Some(f) = files.iter().find(|f| f["primary"].as_bool().unwrap_or(false)).or(files.first()) {
                (
                    f["url"].as_str().unwrap_or("").to_string(),
                    f["filename"].as_str().unwrap_or("modpack.mrpack").to_string(),
                    f["size"].as_u64().unwrap_or(0),
                )
            } else { continue; }
        } else { continue; };

        results.push(ModpackVersionInfo {
            version_name: ver["name"].as_str().unwrap_or(ver["version_number"].as_str().unwrap_or("")).to_string(),
            mc_versions: game_versions,
            download_url: dl_url,
            file_name: fname,
            file_size: fsize,
            date: ver["date_published"].as_str().unwrap_or("").chars().take(10).collect(),
            version_id: ver["id"].as_str().unwrap_or("").to_string(),
        });
    }
    Ok(results)
}

fn get_cf_modpack_versions(http: &reqwest::blocking::Client, project_id: &str) -> Result<Vec<ModpackVersionInfo>, String> {
    let url = format!("https://api.curseforge.com/v1/mods/{}/files?pageSize=20", project_id);
    let resp = http.get(&url)
        .header("x-api-key", &cf_api_key())
        .header("Accept", "application/json")
        .send().map_err(|e| e.to_string())?;
    let json: serde_json::Value = resp.json().map_err(|e| e.to_string())?;
    let data = json["data"].as_array().ok_or("格式错误")?;

    let mut results = Vec::new();
    for file in data.iter().take(20) {
        let game_versions = file["gameVersions"].as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>().join(", "))
            .unwrap_or_default();
        let dl_url = file["downloadUrl"].as_str().unwrap_or("").to_string();
        // CF 有时 downloadUrl 为 null，需要用 fileId 构造 CDN URL
        let dl_url = if dl_url.is_empty() {
            let fid = file["id"].as_u64().unwrap_or(0);
            if fid > 0 {
                format!("https://edge.forgecdn.net/files/{}/{}/{}", fid / 1000, fid % 1000,
                    file["fileName"].as_str().unwrap_or("file.zip"))
            } else { continue; }
        } else { dl_url };

        results.push(ModpackVersionInfo {
            version_name: file["displayName"].as_str().unwrap_or("").to_string(),
            mc_versions: game_versions,
            download_url: dl_url,
            file_name: file["fileName"].as_str().unwrap_or("modpack.zip").to_string(),
            file_size: file["fileLength"].as_u64().unwrap_or(0),
            date: file["fileDate"].as_str().unwrap_or("").chars().take(10).collect(),
            version_id: file["id"].as_u64().unwrap_or(0).to_string(),
        });
    }
    Ok(results)
}

#[tauri::command]
pub fn install_modpack_direct(
    app_handle: tauri::AppHandle,
    download_url: String,
    file_name: String,
    game_dir: String,
    java_path: String,
    use_mirror: bool,
) -> Result<String, String> {
    let cancel_flag = register_cancel(&file_name);
    std::thread::spawn(move || {
        let result = do_install_modpack_direct(&app_handle, &download_url, &file_name, &game_dir, &java_path, use_mirror);
        unregister_cancel(&file_name);
        match result {
            Ok(msg) => eprintln!("[modpack-dl] {}", msg),
            Err(e) => {
                let stage = if cancel_flag.load(Ordering::Relaxed) { "cancelled" } else { "error" };
                eprintln!("[modpack-dl] {}: {}", stage, e);
                let _ = tauri::Emitter::emit(&app_handle, "install-progress", serde_json::json!({
                    "name": file_name, "stage": stage, "current": 0, "total": 0, "detail": e
                }));
            }
        }
    });
    Ok("downloading".to_string())
}

fn do_install_modpack_direct(
    app_handle: &tauri::AppHandle,
    download_url: &str,
    file_name: &str,
    game_dir: &str,
    java_path: &str,
    use_mirror: bool,
) -> Result<String, String> {
    let _ = tauri::Emitter::emit(app_handle, "install-progress", serde_json::json!({
        "name": file_name, "stage": "downloading", "current": 0, "total": 1, "detail": format!("正在下载 {}...", file_name)
    }));

    let http = reqwest::blocking::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(30))
        .user_agent("OAOI-Launcher/1.0")
        .build().map_err(|e| e.to_string())?;

    let resp = http.get(download_url).send().map_err(|e| format!("下载失败: {}", e))?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }

    let total_size = resp.content_length().unwrap_or(0);

    // 保存到临时文件（流式写入）
    let tmp_dir = std::env::temp_dir().join("oaoi_modpack_dl");
    std::fs::create_dir_all(&tmp_dir).ok();
    let tmp_path = tmp_dir.join(file_name);
    let mut out_file = std::fs::File::create(&tmp_path).map_err(|e| format!("创建文件失败: {}", e))?;

    let mut downloaded: u64 = 0;
    let mut last_report = std::time::Instant::now();
    let mut reader = std::io::BufReader::new(resp);
    loop {
        // 检查取消
        if is_cancelled(file_name) {
            drop(out_file);
            let _ = std::fs::remove_file(&tmp_path);
            return Err("用户取消下载".to_string());
        }

        use std::io::Read;
        let mut buf = [0u8; 65536];
        let n = reader.read(&mut buf).map_err(|e| format!("读取失败: {}", e))?;
        if n == 0 { break; }
        use std::io::Write;
        out_file.write_all(&buf[..n]).map_err(|e| format!("写入失败: {}", e))?;
        downloaded += n as u64;

        // 每 500ms 报告一次进度
        if last_report.elapsed().as_millis() >= 500 {
            last_report = std::time::Instant::now();
            if total_size > 0 {
                let pct = (downloaded as f64 / total_size as f64 * 100.0) as u64;
                let _ = tauri::Emitter::emit(app_handle, "install-progress", serde_json::json!({
                    "name": file_name, "stage": "downloading",
                    "current": downloaded, "total": total_size,
                    "detail": format!("{:.1}MB / {:.1}MB ({}%)", downloaded as f64 / 1048576.0, total_size as f64 / 1048576.0, pct)
                }));
            } else {
                let _ = tauri::Emitter::emit(app_handle, "install-progress", serde_json::json!({
                    "name": file_name, "stage": "downloading",
                    "current": downloaded, "total": 0,
                    "detail": format!("已下载 {:.1}MB", downloaded as f64 / 1048576.0)
                }));
            }
        }
    }
    drop(out_file);

    // 标记下载完成
    let _ = tauri::Emitter::emit(app_handle, "install-progress", serde_json::json!({
        "name": file_name, "stage": "downloading", "current": total_size, "total": total_size, "detail": "下载完成"
    }));

    // 再次检查取消
    if is_cancelled(file_name) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err("用户取消安装".to_string());
    }

    eprintln!("[modpack-dl] 下载完成 ({:.1} MB), 开始安装...", downloaded as f64 / 1048576.0);

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
