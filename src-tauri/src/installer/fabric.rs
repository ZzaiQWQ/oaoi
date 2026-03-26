use super::{download_file_if_needed, make_emitter};

/// 安装 Fabric loader
pub fn install_fabric(
    app_handle: &tauri::AppHandle,
    name: &str,
    mc_version: &str,
    loader_version: &str,
    game_dir: &std::path::Path,
    inst_dir: &std::path::Path,
    http: &reqwest::blocking::Client,
    use_mirror: bool,
    ver_json: &mut serde_json::Value,
) -> Result<(), String> {
    let emit = make_emitter(app_handle, name);

    emit("fabric", 0, 1, &format!("处理 Fabric Loader {}...", loader_version));

    let profile_url = format!(
        "https://meta.fabricmc.net/v2/versions/loader/{}/{}/profile/json",
        mc_version, loader_version
    );
    let profile_resp = http.get(&profile_url).send()
        .map_err(|e| format!("获取 Fabric 配置失败: {}", e))?;
    let fabric_profile: serde_json::Value = profile_resp.json()
        .map_err(|e| format!("解析 Fabric 配置失败: {}", e))?;

    // 下载 Fabric 库
    if let Some(fabric_libs) = fabric_profile["libraries"].as_array() {
        let mut fabric_tasks: Vec<(String, std::path::PathBuf, Option<String>)> = Vec::new();

        for lib in fabric_libs {
            let name_str = lib["name"].as_str().unwrap_or("");
            let maven_url = lib["url"].as_str().unwrap_or("https://maven.fabricmc.net/");
            let sha1 = lib["sha1"].as_str();

            if name_str.is_empty() { continue; }
            let parts: Vec<&str> = name_str.split(':').collect();
            if parts.len() < 3 { continue; }
            let group_path = parts[0].replace('.', "/");
            let artifact = parts[1];
            let version = parts[2];
            let jar_name = if parts.len() >= 4 {
                format!("{}-{}-{}.jar", artifact, version, parts[3])
            } else {
                format!("{}-{}.jar", artifact, version)
            };
            let relative_path = format!("{}/{}/{}/{}", group_path, artifact, version, jar_name);
            let url = format!("{}/{}", maven_url.trim_end_matches('/'), relative_path);
            let dest = game_dir.join("libs").join(&relative_path);

            fabric_tasks.push((url, dest, sha1.map(|s| s.to_string())));
        }

        let total = fabric_tasks.len();
        emit("fabric-libs", 0, total, &format!("下载 {} 个 Fabric 组件...", total));

        let done = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let handles: Vec<_> = fabric_tasks.into_iter().map(|(url, dest, sha1)| {
            let done = done.clone();
            let h = http.clone();
            std::thread::spawn(move || {
                let _ = download_file_if_needed(&h, &url, &dest, sha1.as_deref(), use_mirror);
                done.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            })
        }).collect();

        loop {
            let finished = done.load(std::sync::atomic::Ordering::Relaxed);
            emit("fabric-libs", finished, total, &format!("Fabric 组件 {}/{}", finished, total));
            if finished >= total { break; }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        for h in handles { let _ = h.join(); }
        emit("fabric-libs", total, total, "Fabric 组件下载完成");
    }

    // 合并 Fabric 配置
    if let Some(main_class) = fabric_profile["mainClass"].as_str() {
        ver_json["mainClass"] = serde_json::Value::String(main_class.to_string());
    }
    if let Some(fabric_libs) = fabric_profile["libraries"].as_array() {
        if let Some(existing_libs) = ver_json["libraries"].as_array_mut() {
            for fabric_lib in fabric_libs {
                let fabric_name = fabric_lib["name"].as_str().unwrap_or("");
                let fabric_parts: Vec<&str> = fabric_name.split(':').collect();
                let fabric_key = if fabric_parts.len() >= 4 {
                    format!("{}:{}:{}", fabric_parts[0], fabric_parts[1], fabric_parts[3])
                } else if fabric_parts.len() >= 2 {
                    format!("{}:{}", fabric_parts[0], fabric_parts[1])
                } else { String::new() };

                if !fabric_key.is_empty() {
                    existing_libs.retain(|existing| {
                        let name = existing["name"].as_str().unwrap_or("");
                        let parts: Vec<&str> = name.split(':').collect();
                        if parts.len() >= 2 {
                            let key = if parts.len() >= 4 {
                                format!("{}:{}:{}", parts[0], parts[1], parts[3])
                            } else {
                                format!("{}:{}", parts[0], parts[1])
                            };
                            key != fabric_key
                        } else { true }
                    });
                }
                existing_libs.push(fabric_lib.clone());
            }
        }
    }
    let fabric_arguments = &fabric_profile["arguments"];
    if let Some(jvm_args) = fabric_arguments["jvm"].as_array() {
        if !jvm_args.is_empty() {
            if ver_json["arguments"].is_null() {
                ver_json["arguments"] = serde_json::json!({"jvm": jvm_args, "game": []});
            } else if let Some(existing_jvm) = ver_json["arguments"]["jvm"].as_array_mut() {
                for arg in jvm_args {
                    existing_jvm.push(arg.clone());
                }
            }
        }
    }

    ver_json["loader"] = serde_json::json!({
        "type": "fabric",
        "version": loader_version
    });

    // 自动下载 Fabric API
    emit("fabric-api", 0, 1, "下载 Fabric API...");
    let mods_dir = inst_dir.join("mods");
    std::fs::create_dir_all(&mods_dir).ok();
    let api_url = format!(
        "https://api.modrinth.com/v2/project/P7dR8mSH/version?loaders=[\"fabric\"]&game_versions=[\"{}\"]",
        mc_version
    );
    match http.get(&api_url).send() {
        Ok(resp) => {
            if let Ok(versions) = resp.json::<serde_json::Value>() {
                if let Some(arr) = versions.as_array() {
                    if let Some(first) = arr.first() {
                        if let Some(files) = first["files"].as_array() {
                            if let Some(file) = files.first() {
                                let dl_url = file["url"].as_str().unwrap_or("");
                                let filename = file["filename"].as_str().unwrap_or("fabric-api.jar");
                                if !dl_url.is_empty() {
                                    let dest = mods_dir.join(filename);
                                    let _ = download_file_if_needed(http, dl_url, &dest, None, use_mirror);
                                    emit("fabric-api", 1, 1, &format!("Fabric API {} 已下载", filename));
                                }
                            }
                        }
                    }
                }
            }
        },
        Err(e) => eprintln!("[install] Fabric API 下载失败(非致命): {}", e),
    }

    Ok(())
}
