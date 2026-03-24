use tauri::Emitter;
use super::{mirror_url, download_file_if_needed, parallel_download, library_allowed, make_emitter};

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
    let mirrored_meta = mirror_url(meta_url, use_mirror);
    let resp = http.get(&mirrored_meta).send().map_err(|e| format!("获取版本信息失败: {}\n(请检查网络或代理)", e))?;
    let mut ver_json: serde_json::Value = resp.json().map_err(|e| format!("解析版本信息失败: {}", e))?;
    emit("meta", 1, 1, "元数据下载完成");

    // 2. 下载 client.jar
    emit("client", 0, 1, "下载 client.jar...");
    let client_info = ver_json.get("downloads")
        .and_then(|d| d.get("client"))
        .ok_or("版本 JSON 缺少 downloads.client")?;
    let client_url = client_info.get("url").and_then(|v| v.as_str()).ok_or("缺少 client url")?;
    let client_sha1 = client_info.get("sha1").and_then(|v| v.as_str());
    let jar_path = inst_dir.join("client.jar");
    download_file_if_needed(http, client_url, &jar_path, client_sha1, use_mirror)
        .map_err(|e| format!("下载 client.jar 失败: {}", e))?;
    emit("client", 1, 1, "client.jar 完成");

    // 3. 下载 libraries
    let libs = ver_json.get("libraries").and_then(|v| v.as_array());
    if let Some(libs) = libs {
        let mut tasks: Vec<(String, std::path::PathBuf, Option<String>)> = Vec::new();
        for lib in libs.iter() {
            let rules = lib.get("rules").map(|v| v.as_array().cloned().unwrap_or_default());
            if !library_allowed(&rules) { continue; }
            if let Some(artifact) = lib.get("downloads").and_then(|d| d.get("artifact")) {
                let path = artifact.get("path").and_then(|v| v.as_str()).unwrap_or("");
                let url = artifact.get("url").and_then(|v| v.as_str()).unwrap_or("");
                let sha1 = artifact.get("sha1").and_then(|v| v.as_str());
                if !path.is_empty() && !url.is_empty() {
                    let dest = game_dir.join("libs").join(path);
                    tasks.push((url.to_string(), dest, sha1.map(|s| s.to_string())));
                }
            }
        }
        let total = tasks.len();
        emit("libraries", 0, total, &format!("下载 {} 个依赖库...", total));
        let done = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let app_clone = app_handle.clone();
        let done_reporter = done.clone();
        let total_copy = total;
        let inst_name_copy = name.to_string();
        let reporter = std::thread::spawn(move || {
            loop {
                let finished = done_reporter.load(std::sync::atomic::Ordering::Relaxed);
                let _ = app_clone.emit("install-progress", serde_json::json!({
                    "name": inst_name_copy, "stage": "libraries", "current": finished, "total": total_copy,
                    "detail": format!("依赖库 {}/{}", finished, total_copy)
                }));
                if finished >= total_copy { break; }
                std::thread::sleep(std::time::Duration::from_millis(300));
            }
        });
        parallel_download(http, tasks, &done, 32, use_mirror);
        let _ = reporter.join();
        emit("libraries", total, total, "依赖库下载完成");
    }

    // 4. 下载 assets
    if let Some(asset_index) = ver_json.get("assetIndex") {
        let index_url = asset_index.get("url").and_then(|v| v.as_str()).unwrap_or("");
        let index_id = asset_index.get("id").and_then(|v| v.as_str()).unwrap_or("unknown");
        let index_sha1 = asset_index.get("sha1").and_then(|v| v.as_str());

        let index_path = game_dir.join("res").join("indexes").join(format!("{}.json", index_id));
        emit("assets", 0, 1, "下载资源索引...");
        download_file_if_needed(http, index_url, &index_path, index_sha1, use_mirror)?;

        if let Ok(index_content) = std::fs::read_to_string(&index_path) {
            if let Ok(index_json) = serde_json::from_str::<serde_json::Value>(&index_content) {
                if let Some(objects) = index_json.get("objects").and_then(|v| v.as_object()) {
                    let mut asset_tasks: Vec<(String, std::path::PathBuf, String)> = Vec::new();
                    for (_name, info) in objects.iter() {
                        let hash = info.get("hash").and_then(|v| v.as_str()).unwrap_or("");
                        if hash.len() < 2 { continue; }
                        let prefix = &hash[..2];
                        let dest = game_dir.join("res").join("objects").join(prefix).join(hash);
                        let url = format!("https://resources.download.minecraft.net/{}/{}", prefix, hash);
                        asset_tasks.push((url, dest, hash.to_string()));
                    }
                    let total = asset_tasks.len();
                    emit("assets", 0, total, &format!("下载 {} 个资源...", total));

                    let done = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
                    let app_clone = app_handle.clone();
                    let done_reporter = done.clone();
                    let total_copy = total;
                    let inst_name_copy = name.to_string();
                    let reporter = std::thread::spawn(move || {
                        loop {
                            let finished = done_reporter.load(std::sync::atomic::Ordering::Relaxed);
                            let _ = app_clone.emit("install-progress", serde_json::json!({
                                "name": inst_name_copy, "stage": "assets", "current": finished, "total": total_copy,
                                "detail": format!("资源 {}/{}", finished, total_copy)
                            }));
                            if finished >= total_copy { break; }
                            std::thread::sleep(std::time::Duration::from_millis(300));
                        }
                    });

                    let asset_dl_tasks: Vec<(String, std::path::PathBuf, Option<String>)> = asset_tasks
                        .into_iter()
                        .map(|(url, dest, hash)| (url, dest, Some(hash)))
                        .collect();
                    parallel_download(http, asset_dl_tasks, &done, 32, use_mirror);
                    let _ = reporter.join();
                    emit("assets", total, total, "资源下载完成");
                }
            }
        }
    }

    // 设置基础实例信息
    ver_json["name"] = serde_json::Value::String(name.to_string());
    ver_json["mcVersion"] = serde_json::Value::String(mc_version.to_string());
    
    if ver_json["mainClass"].is_null() {
        ver_json["mainClass"] = serde_json::Value::String("net.minecraft.client.main.Main".to_string());
    }
    ver_json["loader"] = serde_json::json!({
        "type": "vanilla",
        "version": ""
    });

    Ok(ver_json)
}
