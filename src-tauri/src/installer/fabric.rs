use super::{
    download_file_if_needed_cancelable, make_emitter, merge_libraries, parallel_download,
    safe_maven_path,
};
use crate::instance::safe_path_name;
use tauri::Emitter;

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
    auto_download_fabric_api: bool,
) -> Result<(), String> {
    let emit = make_emitter(app_handle, name);

    emit(
        "fabric",
        0,
        1,
        &format!("处理 Fabric Loader {}...", loader_version),
    );

    let profile_url = format!(
        "https://meta.fabricmc.net/v2/versions/loader/{}/{}/profile/json",
        mc_version, loader_version
    );
    let profile_resp = http
        .get(&profile_url)
        .send()
        .map_err(|e| format!("获取 Fabric 配置失败: {}", e))?;
    let fabric_profile: serde_json::Value = profile_resp
        .json()
        .map_err(|e| format!("解析 Fabric 配置失败: {}", e))?;

    // 下载 Fabric 库
    if let Some(fabric_libs) = fabric_profile["libraries"].as_array() {
        let mut fabric_tasks: Vec<(String, std::path::PathBuf, Option<String>)> = Vec::new();

        for lib in fabric_libs {
            let name_str = lib["name"].as_str().unwrap_or("");
            let maven_url = lib["url"].as_str().unwrap_or("https://maven.fabricmc.net/");
            let sha1 = lib["sha1"].as_str();

            if name_str.is_empty() {
                continue;
            }
            let parts: Vec<&str> = name_str.split(':').collect();
            if parts.len() < 3 {
                continue;
            }
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
            let Ok(relative_path) = safe_maven_path(&relative_path) else {
                continue;
            };
            let dest = game_dir.join("libs").join(relative_path);

            fabric_tasks.push((url, dest, sha1.map(|s| s.to_string())));
        }

        let total = fabric_tasks.len();
        emit(
            "fabric-libs",
            0,
            total,
            &format!("下载 {} 个 Fabric 组件...", total),
        );

        let done = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let app_clone = app_handle.clone();
        let done_reporter = done.clone();
        let inst_name_copy = name.to_string();
        let reporter = std::thread::spawn(move || loop {
            let finished = done_reporter.load(std::sync::atomic::Ordering::Relaxed);
            let _ = app_clone.emit(
                "install-progress",
                serde_json::json!({
                    "name": inst_name_copy,
                    "stage": "fabric-libs",
                    "current": finished,
                    "total": total,
                    "detail": format!("Fabric 组件 {}/{}", finished, total)
                }),
            );
            if finished >= total {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(300));
        });
        let download_result =
            parallel_download(http, fabric_tasks, &done, 32, use_mirror, Some(name));
        let _ = reporter.join();
        download_result.map_err(|e| format!("Fabric libraries failed: {}", e))?;
        emit("fabric-libs", total, total, "Fabric 组件下载完成");
    }

    // 合并 Fabric 配置
    if let Some(main_class) = fabric_profile["mainClass"].as_str() {
        ver_json["mainClass"] = serde_json::Value::String(main_class.to_string());
    }
    if let Some(fabric_libs) = fabric_profile["libraries"].as_array() {
        if let Some(existing_libs) = ver_json["libraries"].as_array_mut() {
            merge_libraries(existing_libs, fabric_libs);
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

    if !auto_download_fabric_api {
        return Ok(());
    }

    // 自动下载 Fabric API
    emit("fabric-api", 0, 1, "下载 Fabric API...");
    let mods_dir = inst_dir.join("mods");
    std::fs::create_dir_all(&mods_dir).ok();
    let game_versions_filter = format!("[\"{}\"]", mc_version);
    let loader_filter = urlencoding::encode("[\"fabric\"]");
    let game_version_filter = urlencoding::encode(&game_versions_filter);
    let api_url = format!(
        "https://api.modrinth.com/v2/project/P7dR8mSH/version?loaders={}&game_versions={}",
        loader_filter, game_version_filter
    );
    match http.get(&api_url).send() {
        Ok(resp) => {
            if let Ok(versions) = resp.json::<serde_json::Value>() {
                if let Some(arr) = versions.as_array() {
                    if let Some(first) = arr.first() {
                        if let Some(files) = first["files"].as_array() {
                            let file = files
                                .iter()
                                .find(|file| {
                                    file["primary"].as_bool() == Some(true)
                                        && is_fabric_api_runtime_file(file)
                                })
                                .or_else(|| {
                                    files.iter().find(|file| is_fabric_api_runtime_file(file))
                                });
                            if let Some(file) = file {
                                let dl_url = file["url"].as_str().unwrap_or("");
                                let filename =
                                    file["filename"].as_str().unwrap_or("fabric-api.jar");
                                let sha1 = file["hashes"]["sha1"].as_str();
                                if !dl_url.is_empty() {
                                    if let Ok(safe_filename) = safe_path_name(filename, "文件名")
                                    {
                                        let dest = mods_dir.join(&safe_filename);
                                        match download_file_if_needed_cancelable(
                                            http,
                                            dl_url,
                                            &dest,
                                            sha1,
                                            use_mirror,
                                            Some(name),
                                        ) {
                                            Ok(_) => {
                                                emit(
                                                    "fabric-api",
                                                    1,
                                                    1,
                                                    &format!("Fabric API {} 已下载", safe_filename),
                                                );
                                            }
                                            Err(e) => {
                                                eprintln!(
                                                    "[install] Fabric API 下载失败(非致命): {}",
                                                    e
                                                );
                                                emit(
                                                    "fabric-api",
                                                    1,
                                                    1,
                                                    "Fabric API 下载失败，可稍后手动安装",
                                                );
                                            }
                                        }
                                    }
                                }
                            }
                        } else {
                            eprintln!("[install] Fabric API 文件列表为空");
                        }
                    } else {
                        eprintln!("[install] 未找到适配 {} 的 Fabric API 版本", mc_version);
                    }
                } else {
                    eprintln!("[install] Fabric API 响应不是数组");
                }
            } else {
                eprintln!("[install] Fabric API 响应解析失败");
            }
        }
        Err(e) => eprintln!("[install] Fabric API 下载失败(非致命): {}", e),
    }

    Ok(())
}

fn is_fabric_api_runtime_file(file: &serde_json::Value) -> bool {
    let filename = file["filename"].as_str().unwrap_or("").to_lowercase();
    filename.ends_with(".jar")
        && !filename.contains("-sources")
        && !filename.contains("-dev")
        && !filename.contains("-javadoc")
}
