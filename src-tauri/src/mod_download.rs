use crate::instance::{resolve_game_dir, cf_api_key};

/// 下载 Mod/材质包/光影包到实例目录（异步）
#[tauri::command]
pub async fn download_online_mod(
    game_dir: String,
    name: String,
    project_id: String,
    mc_version: String,
    loader: String,
    project_type: Option<String>,
) -> Result<String, String> {
    let ptype = project_type.unwrap_or_else(|| "mod".to_string());
    let result: Result<String, String> = tokio::task::spawn_blocking(move || {
        download_online_mod_blocking(&game_dir, &name, &project_id, &mc_version, &loader, &ptype)
    }).await.map_err(|e| format!("任务失败: {}", e))?;
    result
}

fn download_online_mod_blocking(
    game_dir: &str,
    name: &str,
    project_id: &str,
    mc_version: &str,
    loader: &str,
    project_type: &str,
) -> Result<String, String> {
    let http = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) oaoi-launcher/1.0")
        .build()
        .map_err(|e| e.to_string())?;

    // 判断是 CurseForge 还是 Modrinth
    if let Some(cf_id) = project_id.strip_prefix("cf_") {
        return download_from_curseforge(&http, game_dir, name, cf_id, mc_version, loader, project_type);
    }

    // Modrinth 下载 — 带降级：先精确匹配，再逐步放宽条件
    let versions_arr = {
        let try_params = vec![
            (mc_version, loader),                     // 1. 精确: 版本+loader
            (mc_version, ""),                         // 2. 去掉 loader
        ];
        let mut found: Option<Vec<serde_json::Value>> = None;
        for (ver, ldr) in &try_params {
            let mut url = format!("https://api.modrinth.com/v2/project/{}/version", project_id);
            let mut p = vec![];
            if !ver.is_empty() { p.push(format!("game_versions=[\"{}\"]", ver)); }
            if !ldr.is_empty() && *ldr != "vanilla" && project_type == "mod" {
                p.push(format!("loaders=[\"{}\"]", ldr));
            }
            if !p.is_empty() { url = format!("{}?{}", url, p.join("&")); }
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

    let version = &versions_arr[0];
    let files = version["files"].as_array().ok_or("版本无文件")?;
    let file = files.iter()
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
    let main_result = do_download_to_dir(&http, game_dir, name, download_url, file_name, sub_dir)?;

    // 检查并下载前置依赖
    let mut dep_names: Vec<String> = Vec::new();
    if let Some(deps) = version["dependencies"].as_array() {
        let mods_dir = resolve_game_dir(game_dir).join("instances").join(name).join("mods");
        for dep in deps {
            let dep_type = dep["dependency_type"].as_str().unwrap_or("");
            if dep_type != "required" { continue; }

            let dep_project_id = match dep["project_id"].as_str() {
                Some(id) => id,
                None => continue,
            };

            eprintln!("[dep] 检查前置依赖: {}", dep_project_id);

            // 获取依赖项目信息
            let dep_version_url = if let Some(vid) = dep["version_id"].as_str() {
                format!("https://api.modrinth.com/v2/version/{}", vid)
            } else {
                let mut u = format!("https://api.modrinth.com/v2/project/{}/version", dep_project_id);
                let mut p = vec![];
                if !mc_version.is_empty() { p.push(format!("game_versions=[\"{}\"]", mc_version)); }
                if !loader.is_empty() && loader != "vanilla" { p.push(format!("loaders=[\"{}\"]", loader)); }
                if !p.is_empty() { u = format!("{}?{}", u, p.join("&")); }
                u
            };

            if let Ok(dep_resp) = http.get(&dep_version_url).send() {
                if let Ok(dep_json) = dep_resp.json::<serde_json::Value>() {
                    // 可能是单个 version 或 array
                    let dep_ver = if dep_json.is_array() {
                        dep_json.as_array().and_then(|a| a.first()).cloned()
                    } else {
                        Some(dep_json)
                    };

                    if let Some(dv) = dep_ver {
                        if let Some(dep_files) = dv["files"].as_array() {
                            let dep_file = dep_files.iter()
                                .find(|f| f["primary"].as_bool() == Some(true))
                                .or_else(|| dep_files.first());
                            if let Some(df) = dep_file {
                                let dep_url = df["url"].as_str().unwrap_or("");
                                let dep_fname = df["filename"].as_str().unwrap_or("");
                                if !dep_url.is_empty() && !dep_fname.is_empty() {
                                    // 检查是否已安装
                                    if !mods_dir.join(dep_fname).exists() {
                                        match do_download_to_dir(&http, game_dir, name, dep_url, dep_fname, "mods") {
                                            Ok(_) => {
                                                eprintln!("[dep] 已下载前置: {}", dep_fname);
                                                dep_names.push(dep_fname.to_string());
                                            }
                                            Err(e) => eprintln!("[dep] 下载前置失败: {} - {}", dep_fname, e),
                                        }
                                    } else {
                                        eprintln!("[dep] 前置已存在: {}", dep_fname);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    if dep_names.is_empty() {
        Ok(main_result)
    } else {
        Ok(format!("{} (已自动下载前置: {})", main_result, dep_names.join(", ")))
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
) -> Result<String, String> {
    let loader_type = match loader {
        "forge" => "1", "fabric" => "4", "quilt" => "5", "neoforge" => "6", _ => "0",
    };
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

    let resp = http.get(&url)
        .header("x-api-key", &cf_api_key())
        .send()
        .map_err(|e| format!("CurseForge 请求失败: {}", e))?;
    let json: serde_json::Value = resp.json().map_err(|e| format!("解析失败: {}", e))?;

    let data = json["data"].as_array().ok_or("无文件数据")?;
    if data.is_empty() {
        return Err("没有找到匹配的版本".to_string());
    }

    let file = &data[0];
    let file_name = file["fileName"].as_str().ok_or("无文件名")?;
    let file_id = file["id"].as_u64().unwrap_or(0);
    let download_url = match file["downloadUrl"].as_str() {
        Some(u) if !u.is_empty() => u.to_string(),
        _ => {
            // downloadUrl 为 null，使用 CDN 盲猜
            if file_id > 0 {
                eprintln!("[cf] downloadUrl 为空，CDN 回退: fileId={}", file_id);
                format!("https://edge.forgecdn.net/files/{}/{}/{}",
                    file_id / 1000, file_id % 1000,
                    urlencoding::encode(file_name))
            } else {
                return Err("此 Mod 不允许第三方下载，请从 CurseForge 网站手动下载".to_string());
            }
        }
    };

    let sub_dir = match project_type {
        "resourcepack" => "resourcepacks",
        "shader" => "shaderpacks",
        _ => "mods",
    };
    let main_result = do_download_to_dir(http, game_dir, name, &download_url, file_name, sub_dir)?;

    // 检查 CurseForge 前置依赖
    let mut dep_names: Vec<String> = Vec::new();
    if let Some(deps) = file["dependencies"].as_array() {
        let mods_dir = resolve_game_dir(game_dir).join("instances").join(name).join("mods");
        for dep in deps {
            let relation = dep["relationType"].as_i64().unwrap_or(0);
            if relation != 3 { continue; } // 3 = required dependency

            let dep_mod_id = dep["modId"].as_i64().unwrap_or(0);
            if dep_mod_id == 0 { continue; }

            eprintln!("[cf_dep] 检查前置依赖: modId={}", dep_mod_id);

            // 获取依赖的最新文件
            let dep_url = format!(
                "https://api.curseforge.com/v1/mods/{}/files?pageSize=1&gameVersion={}&modLoaderType={}",
                dep_mod_id, mc_version, loader_type
            );
            if let Ok(dep_resp) = http.get(&dep_url).header("x-api-key", &cf_api_key()).send() {
                if let Ok(dep_json) = dep_resp.json::<serde_json::Value>() {
                    if let Some(dep_files) = dep_json["data"].as_array() {
                        if let Some(df) = dep_files.first() {
                            let dep_fname = df["fileName"].as_str().unwrap_or("");
                            let dep_dl_url = df["downloadUrl"].as_str().unwrap_or("");
                            if !dep_fname.is_empty() && !dep_dl_url.is_empty() && !mods_dir.join(dep_fname).exists() {
                                match do_download_to_dir(http, game_dir, name, dep_dl_url, dep_fname, "mods") {
                                    Ok(_) => {
                                        eprintln!("[cf_dep] 已下载前置: {}", dep_fname);
                                        dep_names.push(dep_fname.to_string());
                                    }
                                    Err(e) => eprintln!("[cf_dep] 下载前置失败: {} - {}", dep_fname, e),
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    if dep_names.is_empty() {
        Ok(main_result)
    } else {
        Ok(format!("{} (已自动下载前置: {})", main_result, dep_names.join(", ")))
    }
}

fn do_download_to_dir(
    http: &reqwest::blocking::Client,
    game_dir: &str,
    name: &str,
    download_url: &str,
    file_name: &str,
    sub_dir: &str,
) -> Result<String, String> {
    let dir = resolve_game_dir(game_dir);
    let target_dir = dir.join("instances").join(name).join(sub_dir);
    std::fs::create_dir_all(&target_dir).ok();
    let dest = target_dir.join(file_name);

    if dest.exists() {
        return Ok(format!("已存在: {}", file_name));
    }

    let mut response = http.get(download_url).send().map_err(|e| format!("下载失败: {}", e))?;
    let mut out = std::fs::File::create(&dest).map_err(|e| format!("创建文件失败: {}", e))?;
    std::io::copy(&mut response, &mut out).map_err(|e| format!("写入失败: {}", e))?;

    Ok(file_name.to_string())
}
