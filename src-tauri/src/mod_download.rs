use crate::instance::{cf_api_key, resolve_game_dir, safe_path_name};
use crate::modpack_sources::{save_source_entry, SourceEntry};
use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};

static MOD_DOWNLOAD_TMP_COUNTER: AtomicU64 = AtomicU64::new(0);
const MAX_DEPENDENCY_DEPTH: usize = 8;

#[derive(serde::Serialize, Clone)]
pub struct OnlineModVersionInfo {
    pub version_id: String,
    pub version_name: String,
    pub mc_versions: String,
    pub mc_version: String,
    pub loaders: String,
    pub loader: String,
    pub file_name: String,
    pub file_size: u64,
    pub date: String,
    pub source: String,
}

/// 获取在线 Mod/材质包/光影包的可下载版本列表
#[tauri::command]
pub async fn get_online_mod_versions(
    project_id: String,
    loader: String,
    project_type: Option<String>,
) -> Result<Vec<OnlineModVersionInfo>, String> {
    let ptype = project_type.unwrap_or_else(|| "mod".to_string());
    tokio::task::spawn_blocking(move || {
        get_online_mod_versions_blocking(&project_id, &loader, &ptype)
    })
    .await
    .map_err(|e| format!("任务失败: {}", e))?
}

/// 下载 Mod/材质包/光影包到实例目录（异步）
#[tauri::command]
pub async fn download_online_mod(
    game_dir: String,
    name: String,
    project_id: String,
    mc_version: String,
    loader: String,
    project_type: Option<String>,
    version_id: Option<String>,
) -> Result<String, String> {
    let ptype = project_type.unwrap_or_else(|| "mod".to_string());
    let cancel_name = online_mod_cancel_name(&name, &project_id, version_id.as_deref());
    let unregister_name = cancel_name.clone();
    crate::instance::register_cancel(&cancel_name);
    let result = tokio::task::spawn_blocking(move || {
        download_online_mod_blocking(
            &game_dir,
            &name,
            &project_id,
            &mc_version,
            &loader,
            &ptype,
            version_id.as_deref(),
            &cancel_name,
        )
    })
    .await;
    crate::instance::unregister_cancel(&unregister_name);
    result.map_err(|e| format!("任务失败: {}", e))?
}

fn get_online_mod_versions_blocking(
    project_id: &str,
    loader: &str,
    project_type: &str,
) -> Result<Vec<OnlineModVersionInfo>, String> {
    let http = reqwest::blocking::Client::builder()
        .use_rustls_tls()
        .timeout(std::time::Duration::from_secs(20))
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) oaoi-launcher/1.0")
        .build()
        .map_err(|e| e.to_string())?;

    if let Some(cf_id) = project_id.strip_prefix("cf_") {
        return get_curseforge_versions(&http, cf_id, loader, project_type);
    }
    get_modrinth_versions(&http, project_id, loader, project_type)
}

fn download_online_mod_blocking(
    game_dir: &str,
    name: &str,
    project_id: &str,
    mc_version: &str,
    loader: &str,
    project_type: &str,
    version_id: Option<&str>,
    cancel_name: &str,
) -> Result<String, String> {
    let http = reqwest::blocking::Client::builder()
        .use_rustls_tls()
        .timeout(std::time::Duration::from_secs(60))
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) oaoi-launcher/1.0")
        .build()
        .map_err(|e| e.to_string())?;

    // 判断是 CurseForge 还是 Modrinth
    if let Some(cf_id) = project_id.strip_prefix("cf_") {
        return download_from_curseforge(
            &http,
            game_dir,
            name,
            cf_id,
            mc_version,
            loader,
            project_type,
            version_id,
            cancel_name,
        );
    }

    // Modrinth 下载 — 带降级：先精确匹配，再逐步放宽条件
    let version = if let Some(vid) = version_id.filter(|v| !v.is_empty()) {
        let url = format!("https://api.modrinth.com/v2/version/{}", vid);
        let resp = http
            .get(&url)
            .send()
            .map_err(|e| format!("版本请求失败: {}", e))?;
        if !resp.status().is_success() {
            return Err(format!("版本请求失败: HTTP {}", resp.status()));
        }
        resp.json::<serde_json::Value>()
            .map_err(|e| format!("解析版本失败: {}", e))?
    } else {
        let versions_arr = {
            let try_params = vec![
                (mc_version, loader), // 1. 精确: 版本+loader
                (mc_version, ""),     // 2. 去掉 loader
            ];
            let mut found: Option<Vec<serde_json::Value>> = None;
            for (ver, ldr) in &try_params {
                let mut url = format!("https://api.modrinth.com/v2/project/{}/version", project_id);
                let mut p = vec![];
                if !ver.is_empty() {
                    p.push(format!("game_versions=[\"{}\"]", ver));
                }
                if !ldr.is_empty() && *ldr != "vanilla" && project_type == "mod" {
                    p.push(format!("loaders=[\"{}\"]", ldr));
                }
                if !p.is_empty() {
                    url = format!("{}?{}", url, p.join("&"));
                }
                if let Ok(resp) = http.get(&url).send() {
                    if let Ok(json) = resp.json::<serde_json::Value>() {
                        if let Some(arr) = json.as_array() {
                            if !arr.is_empty() {
                                found = Some(arr.clone());
                                break;
                            }
                        }
                    }
                }
            }
            found.ok_or_else(|| "没有找到匹配的版本".to_string())?
        };
        versions_arr
            .first()
            .cloned()
            .ok_or_else(|| "没有找到匹配的版本".to_string())?
    };
    let files = version["files"].as_array().ok_or("版本无文件")?;
    let file = files
        .iter()
        .find(|f| f["primary"].as_bool() == Some(true))
        .or_else(|| files.first())
        .ok_or("无下载文件")?;

    let download_url = file["url"].as_str().ok_or("无下载链接")?;
    let file_name = file["filename"].as_str().ok_or("无文件名")?;

    let sub_dir = match project_type {
        "resourcepack" => "resourcepacks",
        "shader" => "shaderpacks",
        _ => "mods",
    };
    let mut downloaded_mod_paths: Vec<String> = Vec::new();
    let main_result = do_download_to_dir(
        &http,
        game_dir,
        name,
        download_url,
        file_name,
        sub_dir,
        Some(cancel_name),
    )?;
    push_downloaded_mod_path(&mut downloaded_mod_paths, sub_dir, file_name);

    // 检查并下载前置依赖
    let mut dep_names: Vec<String> = Vec::new();
    let mut dep_errors: Vec<String> = Vec::new();
    let mut visited_deps: HashSet<String> = HashSet::new();
    if let Some(version_id) = version["id"].as_str() {
        visited_deps.insert(format!("mr-version:{}", version_id));
    }
    if let Some(deps) = version["dependencies"].as_array() {
        download_modrinth_required_dependencies(
            &http,
            game_dir,
            name,
            mc_version,
            loader,
            deps,
            &mut visited_deps,
            &mut dep_names,
            &mut dep_errors,
            &mut downloaded_mod_paths,
            0,
            cancel_name,
        )?;
    }

    if !dep_errors.is_empty() {
        return Err(format!("前置依赖下载失败: {}", dep_errors.join("; ")));
    }

    let result = if dep_names.is_empty() {
        main_result
    } else {
        format!("{} (已自动下载前置: {})", main_result, dep_names.join(", "))
    };
    crate::mod_analyzer::spawn_cache_downloaded_mods(
        game_dir.to_string(),
        name.to_string(),
        loader.to_string(),
        downloaded_mod_paths,
    );
    Ok(result)
}

fn download_modrinth_required_dependencies(
    http: &reqwest::blocking::Client,
    game_dir: &str,
    name: &str,
    mc_version: &str,
    loader: &str,
    deps: &[serde_json::Value],
    visited: &mut HashSet<String>,
    dep_names: &mut Vec<String>,
    dep_errors: &mut Vec<String>,
    downloaded_mod_paths: &mut Vec<String>,
    depth: usize,
    cancel_name: &str,
) -> Result<(), String> {
    if depth >= MAX_DEPENDENCY_DEPTH {
        dep_errors.push("前置依赖层级过深，已停止继续解析".to_string());
        return Ok(());
    }

    let safe_name = safe_path_name(name, "版本名")?;
    let mods_dir = resolve_game_dir(game_dir)
        .join("instances")
        .join(&safe_name)
        .join("mods");

    for dep in deps {
        if is_cancelled(Some(cancel_name)) {
            dep_errors.push("用户取消下载".to_string());
            break;
        }

        let dep_type = dep["dependency_type"].as_str().unwrap_or("");
        if dep_type != "required" {
            continue;
        }

        let dep_project_id = match dep["project_id"].as_str() {
            Some(id) => id,
            None => {
                dep_errors.push("Modrinth 前置缺少 project_id".to_string());
                continue;
            }
        };

        eprintln!("[dep] 检查前置依赖: {}", dep_project_id);

        match resolve_modrinth_dependency_version(http, dep, dep_project_id, mc_version, loader) {
            Ok(dep_version) => {
                if let Err(e) = download_modrinth_dependency_version(
                    http,
                    game_dir,
                    name,
                    mc_version,
                    loader,
                    dep_project_id,
                    dep_version,
                    &mods_dir,
                    visited,
                    dep_names,
                    dep_errors,
                    downloaded_mod_paths,
                    depth,
                    cancel_name,
                ) {
                    dep_errors.push(format!("{}: {}", dep_project_id, e));
                }
            }
            Err(e) => dep_errors.push(format!("{}: {}", dep_project_id, e)),
        }
    }

    Ok(())
}

fn resolve_modrinth_dependency_version(
    http: &reqwest::blocking::Client,
    dep: &serde_json::Value,
    dep_project_id: &str,
    mc_version: &str,
    loader: &str,
) -> Result<serde_json::Value, String> {
    let dep_version_url = if let Some(vid) = dep["version_id"].as_str() {
        format!("https://api.modrinth.com/v2/version/{}", vid)
    } else {
        let mut url = format!(
            "https://api.modrinth.com/v2/project/{}/version",
            dep_project_id
        );
        let mut params = Vec::new();
        if !mc_version.is_empty() {
            params.push(format!("game_versions=[\"{}\"]", mc_version));
        }
        if !loader.is_empty() && loader != "vanilla" {
            params.push(format!("loaders=[\"{}\"]", loader));
        }
        if !params.is_empty() {
            url = format!("{}?{}", url, params.join("&"));
        }
        url
    };

    let resp = http
        .get(&dep_version_url)
        .send()
        .map_err(|e| format!("前置版本请求失败: {}", e))?;
    if !resp.status().is_success() {
        return Err(format!("前置版本请求失败: HTTP {}", resp.status()));
    }
    let dep_json = resp
        .json::<serde_json::Value>()
        .map_err(|e| format!("解析前置版本失败: {}", e))?;
    if dep_json.is_array() {
        dep_json
            .as_array()
            .and_then(|versions| versions.first())
            .cloned()
            .ok_or_else(|| "没有找到匹配的前置版本".to_string())
    } else {
        Ok(dep_json)
    }
}

fn download_modrinth_dependency_version(
    http: &reqwest::blocking::Client,
    game_dir: &str,
    name: &str,
    mc_version: &str,
    loader: &str,
    dep_project_id: &str,
    dep_version: serde_json::Value,
    mods_dir: &std::path::Path,
    visited: &mut HashSet<String>,
    dep_names: &mut Vec<String>,
    dep_errors: &mut Vec<String>,
    downloaded_mod_paths: &mut Vec<String>,
    depth: usize,
    cancel_name: &str,
) -> Result<(), String> {
    let dep_key = dep_version["id"]
        .as_str()
        .map(|id| format!("mr-version:{}", id))
        .unwrap_or_else(|| format!("mr-project:{}", dep_project_id));
    if !visited.insert(dep_key) {
        return Ok(());
    }

    let dep_files = dep_version["files"]
        .as_array()
        .ok_or_else(|| "前置版本没有文件列表".to_string())?;
    let dep_file = dep_files
        .iter()
        .find(|file| file["primary"].as_bool() == Some(true))
        .or_else(|| dep_files.first())
        .ok_or_else(|| "前置版本没有可下载文件".to_string())?;
    let dep_url = dep_file["url"]
        .as_str()
        .ok_or_else(|| "前置版本没有下载链接".to_string())?;
    let dep_fname = dep_file["filename"]
        .as_str()
        .ok_or_else(|| "前置版本没有文件名".to_string())?;
    let safe_dep_fname = safe_path_name(dep_fname, "文件名")?;

    if mods_dir.join(&safe_dep_fname).exists() {
        eprintln!("[dep] 前置已存在: {}", dep_fname);
    } else {
        do_download_to_dir(
            http,
            game_dir,
            name,
            dep_url,
            dep_fname,
            "mods",
            Some(cancel_name),
        )
        .map_err(|e| format!("下载前置失败 {}: {}", dep_fname, e))?;
        eprintln!("[dep] 已下载前置: {}", dep_fname);
        dep_names.push(dep_fname.to_string());
    }
    push_downloaded_mod_path(downloaded_mod_paths, "mods", dep_fname);

    if let Some(child_deps) = dep_version["dependencies"].as_array() {
        download_modrinth_required_dependencies(
            http,
            game_dir,
            name,
            mc_version,
            loader,
            child_deps,
            visited,
            dep_names,
            dep_errors,
            downloaded_mod_paths,
            depth + 1,
            cancel_name,
        )?;
    }

    Ok(())
}

fn get_modrinth_versions(
    http: &reqwest::blocking::Client,
    project_id: &str,
    loader: &str,
    project_type: &str,
) -> Result<Vec<OnlineModVersionInfo>, String> {
    let url = format!("https://api.modrinth.com/v2/project/{}/version", project_id);
    let resp = http
        .get(&url)
        .send()
        .map_err(|e| format!("Modrinth 版本请求失败: {}", e))?;
    if !resp.status().is_success() {
        return Err(format!("Modrinth 版本请求失败: HTTP {}", resp.status()));
    }
    let json: serde_json::Value = resp.json().map_err(|e| format!("解析版本失败: {}", e))?;
    let versions = json.as_array().ok_or("版本数据格式错误")?;

    let mut out = Vec::new();
    for item in versions {
        let loaders = json_string_array(&item["loaders"]);
        if project_type == "mod" && !loader.is_empty() && loader != "vanilla" {
            if !loaders.iter().any(|l| l.eq_ignore_ascii_case(loader)) {
                continue;
            }
        }

        let files = match item["files"].as_array() {
            Some(files) => files,
            None => continue,
        };
        let file = match files
            .iter()
            .find(|f| f["primary"].as_bool() == Some(true))
            .or_else(|| files.first())
        {
            Some(file) => file,
            None => continue,
        };

        let game_versions = json_string_array(&item["game_versions"]);
        let first_mc = game_versions.first().cloned().unwrap_or_default();
        let selected_loader = if project_type == "mod"
            && !loader.is_empty()
            && loader != "vanilla"
            && loaders.iter().any(|l| l.eq_ignore_ascii_case(loader))
        {
            loader.to_string()
        } else {
            loaders
                .first()
                .cloned()
                .unwrap_or_else(|| loader.to_string())
        };
        let date = item["date_published"]
            .as_str()
            .map(short_date)
            .unwrap_or_default();

        out.push(OnlineModVersionInfo {
            version_id: item["id"].as_str().unwrap_or("").to_string(),
            version_name: item["version_number"]
                .as_str()
                .unwrap_or("未命名版本")
                .to_string(),
            mc_versions: game_versions.join(", "),
            mc_version: first_mc,
            loaders: loaders.join(", "),
            loader: selected_loader.to_lowercase(),
            file_name: file["filename"].as_str().unwrap_or("").to_string(),
            file_size: file["size"].as_u64().unwrap_or(0),
            date,
            source: "modrinth".to_string(),
        });
    }

    Ok(out)
}

fn get_curseforge_versions(
    http: &reqwest::blocking::Client,
    cf_id: &str,
    loader: &str,
    project_type: &str,
) -> Result<Vec<OnlineModVersionInfo>, String> {
    let loader_type = curseforge_loader_type(loader);
    let mut out = Vec::new();

    for index in (0..500).step_by(50) {
        let mut url = format!(
            "https://api.curseforge.com/v1/mods/{}/files?pageSize=50&index={}",
            cf_id, index
        );
        if loader_type != "0" && project_type == "mod" {
            url.push_str(&format!("&modLoaderType={}", loader_type));
        }

        let resp = http
            .get(&url)
            .header("x-api-key", &cf_api_key())
            .header("Accept", "application/json")
            .send()
            .map_err(|e| format!("CurseForge 版本请求失败: {}", e))?;
        if !resp.status().is_success() {
            return Err(format!("CurseForge 版本请求失败: HTTP {}", resp.status()));
        }
        let json: serde_json::Value = resp.json().map_err(|e| format!("解析版本失败: {}", e))?;
        let data = json["data"].as_array().ok_or("版本数据格式错误")?;
        if data.is_empty() {
            break;
        }

        for file in data {
            if let Some(info) = curseforge_file_to_version_info(file, loader) {
                out.push(info);
            }
        }
    }

    Ok(out)
}

fn curseforge_file_to_version_info(
    file: &serde_json::Value,
    preferred_loader: &str,
) -> Option<OnlineModVersionInfo> {
    let game_versions = json_string_array(&file["gameVersions"]);
    let mc_versions: Vec<String> = game_versions
        .iter()
        .filter(|v| is_minecraft_version(v))
        .cloned()
        .collect();
    let loaders: Vec<String> = game_versions
        .iter()
        .filter_map(|v| normalize_loader_name(v))
        .collect();
    let file_id = file["id"].as_u64()?.to_string();
    let file_name = file["fileName"].as_str().unwrap_or("").to_string();
    let version_name = file["displayName"]
        .as_str()
        .filter(|v| !v.is_empty())
        .unwrap_or(&file_name)
        .to_string();
    let loader = if !preferred_loader.is_empty() && preferred_loader != "vanilla" {
        preferred_loader.to_string()
    } else {
        loaders.first().cloned().unwrap_or_default()
    };

    Some(OnlineModVersionInfo {
        version_id: file_id,
        version_name,
        mc_versions: mc_versions.join(", "),
        mc_version: mc_versions.first().cloned().unwrap_or_default(),
        loaders: loaders.join(", "),
        loader,
        file_name,
        file_size: file["fileLength"].as_u64().unwrap_or(0),
        date: file["fileDate"]
            .as_str()
            .map(short_date)
            .unwrap_or_default(),
        source: "curseforge".to_string(),
    })
}

fn fetch_curseforge_file(
    http: &reqwest::blocking::Client,
    cf_id: &str,
    file_id: &str,
) -> Result<serde_json::Value, String> {
    let url = format!(
        "https://api.curseforge.com/v1/mods/{}/files/{}",
        cf_id, file_id
    );
    let resp = http
        .get(&url)
        .header("x-api-key", &cf_api_key())
        .header("Accept", "application/json")
        .send()
        .map_err(|e| format!("CurseForge 文件请求失败: {}", e))?;
    if !resp.status().is_success() {
        return Err(format!("CurseForge 文件请求失败: HTTP {}", resp.status()));
    }
    let json: serde_json::Value = resp.json().map_err(|e| format!("解析文件失败: {}", e))?;
    json["data"]
        .as_object()
        .map(|_| json["data"].clone())
        .ok_or_else(|| "CurseForge 文件数据格式错误".to_string())
}

fn json_string_array(value: &serde_json::Value) -> Vec<String> {
    value
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .filter(|v| !v.trim().is_empty())
                .map(|v| v.trim().to_string())
                .collect()
        })
        .unwrap_or_default()
}

fn short_date(value: &str) -> String {
    value.chars().take(10).collect()
}

fn is_minecraft_version(value: &str) -> bool {
    value
        .chars()
        .next()
        .map(|c| c.is_ascii_digit())
        .unwrap_or(false)
}

fn normalize_loader_name(value: &str) -> Option<String> {
    match value.to_lowercase().as_str() {
        "forge" => Some("forge".to_string()),
        "fabric" => Some("fabric".to_string()),
        "quilt" => Some("quilt".to_string()),
        "neoforge" | "neo forge" => Some("neoforge".to_string()),
        _ => None,
    }
}

fn curseforge_loader_type(loader: &str) -> &'static str {
    match loader {
        "forge" => "1",
        "fabric" => "4",
        "quilt" => "5",
        "neoforge" => "6",
        _ => "0",
    }
}

fn curseforge_cdn_urls(file_id: u64, file_name: &str) -> Vec<String> {
    if file_id == 0 || file_name.is_empty() {
        return Vec::new();
    }
    let encoded_name = urlencoding::encode(file_name);
    vec![
        format!(
            "https://edge.forgecdn.net/files/{}/{}/{}",
            file_id / 1000,
            file_id % 1000,
            encoded_name
        ),
        format!(
            "https://mediafilez.forgecdn.net/files/{}/{}/{}",
            file_id / 1000,
            file_id % 1000,
            encoded_name
        ),
    ]
}

fn curseforge_download_candidates(
    file_id: u64,
    file_name: &str,
    api_download_url: &str,
) -> Vec<String> {
    let mut urls = curseforge_cdn_urls(file_id, file_name);
    if !api_download_url.is_empty() {
        urls.push(api_download_url.to_string());
    }
    urls
}

fn do_download_to_dir_with_fallbacks(
    http: &reqwest::blocking::Client,
    game_dir: &str,
    name: &str,
    download_urls: &[String],
    file_name: &str,
    sub_dir: &str,
    cancel_name: Option<&str>,
) -> Result<String, String> {
    let mut last_err = String::new();
    for url in download_urls {
        eprintln!("[cf] try download: {}", url);
        match do_download_to_dir(http, game_dir, name, url, file_name, sub_dir, cancel_name) {
            Ok(result) => return Ok(result),
            Err(err) => {
                last_err = format!("{}: {}", url, err);
                eprintln!("[cf] download failed, try next: {}", last_err);
            }
        }
    }
    Err(if last_err.is_empty() {
        "没有可用下载地址".to_string()
    } else {
        last_err
    })
}

fn save_curseforge_download_source(
    game_dir: &str,
    name: &str,
    sub_dir: &str,
    file_name: &str,
    project_id: &str,
    file_id: u64,
) {
    let Ok(safe_name) = safe_path_name(name, "version name") else {
        return;
    };
    let Ok(safe_file_name) = safe_path_name(file_name, "file name") else {
        return;
    };
    let Ok(project_id) = project_id.parse::<u32>() else {
        return;
    };
    let Ok(file_id) = u32::try_from(file_id) else {
        return;
    };
    let game_root = resolve_game_dir(game_dir);
    let rel = format!("{}/{}", sub_dir, safe_file_name);
    if let Err(e) = save_source_entry(
        &game_root,
        &safe_name,
        SourceEntry {
            source: "curseforge".to_string(),
            path: rel,
            project_id: Some(project_id),
            file_id: Some(file_id),
            class_id: match sub_dir {
                "mods" => Some(6),
                "resourcepacks" => Some(12),
                "shaderpacks" => Some(6552),
                _ => None,
            },
            sha1: None,
            file_name: Some(safe_file_name),
        },
    ) {
        eprintln!("[cf] save source metadata failed: {}", e);
    }
}

fn download_from_curseforge(
    http: &reqwest::blocking::Client,
    game_dir: &str,
    name: &str,
    cf_id: &str,
    mc_version: &str,
    loader: &str,
    project_type: &str,
    version_id: Option<&str>,
    cancel_name: &str,
) -> Result<String, String> {
    let loader_type = curseforge_loader_type(loader);
    let file = if let Some(file_id) = version_id.filter(|v| !v.is_empty()) {
        fetch_curseforge_file(http, cf_id, file_id)?
    } else {
        let mut url = format!(
            "https://api.curseforge.com/v1/mods/{}/files?pageSize=5",
            cf_id
        );
        if !mc_version.is_empty() && project_type == "mod" {
            url.push_str(&format!("&gameVersion={}", mc_version));
        }
        if loader_type != "0" && project_type == "mod" {
            url.push_str(&format!("&modLoaderType={}", loader_type));
        }

        let resp = http
            .get(&url)
            .header("x-api-key", &cf_api_key())
            .send()
            .map_err(|e| format!("CurseForge 请求失败: {}", e))?;
        if !resp.status().is_success() {
            return Err(format!("CurseForge 请求失败: HTTP {}", resp.status()));
        }
        let json: serde_json::Value = resp.json().map_err(|e| format!("解析失败: {}", e))?;

        let data = json["data"].as_array().ok_or("无文件数据")?;
        if data.is_empty() {
            return Err("没有找到匹配的版本".to_string());
        }

        data[0].clone()
    };
    let file_name = file["fileName"].as_str().ok_or("无文件名")?;
    let file_id = file["id"].as_u64().unwrap_or(0);
    let api_download_url = file["downloadUrl"].as_str().unwrap_or("");
    let download_urls = curseforge_download_candidates(file_id, file_name, api_download_url);
    if download_urls.is_empty() {
        return Err("此 Mod 没有可用下载地址，请从 CurseForge 网站手动下载".to_string());
    }

    let sub_dir = match project_type {
        "resourcepack" => "resourcepacks",
        "shader" => "shaderpacks",
        _ => "mods",
    };
    let mut downloaded_mod_paths: Vec<String> = Vec::new();
    let main_result = do_download_to_dir_with_fallbacks(
        http,
        game_dir,
        name,
        &download_urls,
        file_name,
        sub_dir,
        Some(cancel_name),
    )?;
    push_downloaded_mod_path(&mut downloaded_mod_paths, sub_dir, file_name);
    save_curseforge_download_source(game_dir, name, sub_dir, file_name, cf_id, file_id);

    // 检查 CurseForge 前置依赖
    let mut dep_names: Vec<String> = Vec::new();
    let mut dep_errors: Vec<String> = Vec::new();
    let mut visited_deps: HashSet<String> = HashSet::new();
    if file_id != 0 {
        visited_deps.insert(format!("cf-file:{}", file_id));
    }
    if let Some(deps) = file["dependencies"].as_array() {
        download_curseforge_required_dependencies(
            http,
            game_dir,
            name,
            mc_version,
            loader_type,
            deps,
            &mut visited_deps,
            &mut dep_names,
            &mut dep_errors,
            &mut downloaded_mod_paths,
            0,
            cancel_name,
        )?;
    }

    if !dep_errors.is_empty() {
        return Err(format!("前置依赖下载失败: {}", dep_errors.join("; ")));
    }

    let result = if dep_names.is_empty() {
        main_result
    } else {
        format!("{} (已自动下载前置: {})", main_result, dep_names.join(", "))
    };
    crate::mod_analyzer::spawn_cache_downloaded_mods(
        game_dir.to_string(),
        name.to_string(),
        loader.to_string(),
        downloaded_mod_paths,
    );
    Ok(result)
}

fn download_curseforge_required_dependencies(
    http: &reqwest::blocking::Client,
    game_dir: &str,
    name: &str,
    mc_version: &str,
    loader_type: &str,
    deps: &[serde_json::Value],
    visited: &mut HashSet<String>,
    dep_names: &mut Vec<String>,
    dep_errors: &mut Vec<String>,
    downloaded_mod_paths: &mut Vec<String>,
    depth: usize,
    cancel_name: &str,
) -> Result<(), String> {
    if depth >= MAX_DEPENDENCY_DEPTH {
        dep_errors.push("前置依赖层级过深，已停止继续解析".to_string());
        return Ok(());
    }

    let safe_name = safe_path_name(name, "版本名")?;
    let mods_dir = resolve_game_dir(game_dir)
        .join("instances")
        .join(&safe_name)
        .join("mods");

    for dep in deps {
        if is_cancelled(Some(cancel_name)) {
            dep_errors.push("用户取消下载".to_string());
            break;
        }

        let relation = dep["relationType"].as_i64().unwrap_or(0);
        if relation != 3 {
            continue;
        } // 3 = required dependency

        let dep_mod_id = dep["modId"].as_u64().unwrap_or(0);
        if dep_mod_id == 0 {
            dep_errors.push("CurseForge 前置缺少 modId".to_string());
            continue;
        }

        eprintln!("[cf_dep] 检查前置依赖: modId={}", dep_mod_id);

        match resolve_curseforge_dependency_file(http, dep_mod_id, mc_version, loader_type) {
            Ok(dep_file) => {
                if let Err(e) = download_curseforge_dependency_file(
                    http,
                    game_dir,
                    name,
                    mc_version,
                    loader_type,
                    dep_mod_id,
                    dep_file,
                    &mods_dir,
                    visited,
                    dep_names,
                    dep_errors,
                    downloaded_mod_paths,
                    depth,
                    cancel_name,
                ) {
                    dep_errors.push(format!("{}: {}", dep_mod_id, e));
                }
            }
            Err(e) => dep_errors.push(format!("{}: {}", dep_mod_id, e)),
        }
    }

    Ok(())
}

fn resolve_curseforge_dependency_file(
    http: &reqwest::blocking::Client,
    dep_mod_id: u64,
    mc_version: &str,
    loader_type: &str,
) -> Result<serde_json::Value, String> {
    let dep_url = format!(
        "https://api.curseforge.com/v1/mods/{}/files?pageSize=1&gameVersion={}&modLoaderType={}",
        dep_mod_id, mc_version, loader_type
    );
    let resp = http
        .get(&dep_url)
        .header("x-api-key", &cf_api_key())
        .send()
        .map_err(|e| format!("前置文件请求失败: {}", e))?;
    if !resp.status().is_success() {
        return Err(format!("前置文件请求失败: HTTP {}", resp.status()));
    }
    let dep_json = resp
        .json::<serde_json::Value>()
        .map_err(|e| format!("解析前置文件失败: {}", e))?;
    dep_json["data"]
        .as_array()
        .and_then(|files| files.first())
        .cloned()
        .ok_or_else(|| "没有找到匹配的前置文件".to_string())
}

fn download_curseforge_dependency_file(
    http: &reqwest::blocking::Client,
    game_dir: &str,
    name: &str,
    mc_version: &str,
    loader_type: &str,
    dep_mod_id: u64,
    dep_file: serde_json::Value,
    mods_dir: &std::path::Path,
    visited: &mut HashSet<String>,
    dep_names: &mut Vec<String>,
    dep_errors: &mut Vec<String>,
    downloaded_mod_paths: &mut Vec<String>,
    depth: usize,
    cancel_name: &str,
) -> Result<(), String> {
    let dep_file_id = dep_file["id"].as_u64().unwrap_or(0);
    let dep_key = if dep_file_id != 0 {
        format!("cf-file:{}", dep_file_id)
    } else {
        format!("cf-project:{}", dep_mod_id)
    };
    if !visited.insert(dep_key) {
        return Ok(());
    }

    let dep_fname = dep_file["fileName"]
        .as_str()
        .ok_or_else(|| "前置文件没有文件名".to_string())?;
    let dep_dl_url = dep_file["downloadUrl"].as_str().unwrap_or("");
    let safe_dep_fname = safe_path_name(dep_fname, "文件名")?;
    let dep_download_urls = curseforge_download_candidates(dep_file_id, dep_fname, dep_dl_url);
    if dep_download_urls.is_empty() {
        return Err(format!("前置没有可用下载地址: {}", dep_fname));
    }

    if mods_dir.join(&safe_dep_fname).exists() {
        eprintln!("[cf_dep] 前置已存在: {}", dep_fname);
    } else {
        do_download_to_dir_with_fallbacks(
            http,
            game_dir,
            name,
            &dep_download_urls,
            dep_fname,
            "mods",
            Some(cancel_name),
        )
        .map_err(|e| format!("下载前置失败 {}: {}", dep_fname, e))?;
        save_curseforge_download_source(
            game_dir,
            name,
            "mods",
            dep_fname,
            &dep_mod_id.to_string(),
            dep_file_id,
        );
        eprintln!("[cf_dep] 已下载前置: {}", dep_fname);
        dep_names.push(dep_fname.to_string());
    }
    push_downloaded_mod_path(downloaded_mod_paths, "mods", dep_fname);

    if let Some(child_deps) = dep_file["dependencies"].as_array() {
        download_curseforge_required_dependencies(
            http,
            game_dir,
            name,
            mc_version,
            loader_type,
            child_deps,
            visited,
            dep_names,
            dep_errors,
            downloaded_mod_paths,
            depth + 1,
            cancel_name,
        )?;
    }

    Ok(())
}

fn push_downloaded_mod_path(paths: &mut Vec<String>, sub_dir: &str, file_name: &str) {
    if sub_dir != "mods" {
        return;
    }
    let Ok(safe_file_name) = safe_path_name(file_name, "文件名") else {
        return;
    };
    if !safe_file_name.to_ascii_lowercase().ends_with(".jar") {
        return;
    }
    let rel_path = format!("mods/{}", safe_file_name);
    if !paths.iter().any(|item| item == &rel_path) {
        paths.push(rel_path);
    }
}

fn do_download_to_dir(
    http: &reqwest::blocking::Client,
    game_dir: &str,
    name: &str,
    download_url: &str,
    file_name: &str,
    sub_dir: &str,
    cancel_name: Option<&str>,
) -> Result<String, String> {
    if is_cancelled(cancel_name) {
        return Err("用户取消下载".to_string());
    }
    let dir = resolve_game_dir(game_dir);
    let safe_name = safe_path_name(name, "版本名")?;
    let safe_file_name = safe_path_name(file_name, "文件名")?;
    let safe_sub_dir = match sub_dir {
        "mods" => "mods",
        "resourcepacks" => "resourcepacks",
        "shaderpacks" => "shaderpacks",
        _ => return Err(format!("非法下载目录: {}", sub_dir)),
    };
    let target_dir = dir.join("instances").join(&safe_name).join(safe_sub_dir);
    std::fs::create_dir_all(&target_dir).map_err(|e| format!("创建下载目录失败: {}", e))?;
    let dest = target_dir.join(&safe_file_name);

    if dest.exists() {
        return Ok(format!("已存在: {}", safe_file_name));
    }
    if is_cancelled(cancel_name) {
        return Err("用户取消下载".to_string());
    }

    let mut response = http
        .get(download_url)
        .send()
        .map_err(|e| format!("下载失败: {}", e))?;
    if !response.status().is_success() {
        return Err(format!("下载失败: HTTP {}", response.status()));
    }
    let tmp_counter = MOD_DOWNLOAD_TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = dest.with_file_name(format!(
        ".{}.{}.{}.download",
        safe_file_name,
        std::process::id(),
        tmp_counter
    ));
    {
        let mut out =
            std::fs::File::create(&tmp).map_err(|e| format!("创建临时文件失败: {}", e))?;
        let mut buf = [0u8; 128 * 1024];
        loop {
            if is_cancelled(cancel_name) {
                let _ = std::fs::remove_file(&tmp);
                return Err("用户取消下载".to_string());
            }
            let read = std::io::Read::read(&mut response, &mut buf).map_err(|e| {
                let _ = std::fs::remove_file(&tmp);
                format!("读取失败: {}", e)
            })?;
            if read == 0 {
                break;
            }
            std::io::Write::write_all(&mut out, &buf[..read]).map_err(|e| {
                let _ = std::fs::remove_file(&tmp);
                format!("写入失败: {}", e)
            })?;
        }
    }
    std::fs::rename(&tmp, &dest).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("保存文件失败: {}", e)
    })?;

    Ok(safe_file_name)
}

pub fn online_mod_cancel_name(name: &str, project_id: &str, version_id: Option<&str>) -> String {
    format!(
        "online-mod:{}:{}:{}",
        name,
        project_id,
        version_id.unwrap_or("")
    )
}

fn is_cancelled(cancel_name: Option<&str>) -> bool {
    cancel_name.is_some_and(crate::instance::is_cancelled)
}
