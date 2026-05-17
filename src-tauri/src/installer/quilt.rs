use super::{make_emitter, merge_libraries, parallel_download, safe_maven_path};
use tauri::Emitter;

/// 安装 Quilt loader
pub fn install_quilt(
    app_handle: &tauri::AppHandle,
    name: &str,
    mc_version: &str,
    loader_version: &str,
    game_dir: &std::path::Path,
    _inst_dir: &std::path::Path,
    http: &reqwest::blocking::Client,
    use_mirror: bool,
    ver_json: &mut serde_json::Value,
) -> Result<(), String> {
    let emit = make_emitter(app_handle, name);

    emit(
        "quilt",
        0,
        1,
        &format!("处理 Quilt Loader {}...", loader_version),
    );

    let profile_url = format!(
        "https://meta.quiltmc.org/v3/versions/loader/{}/{}/profile/json",
        mc_version, loader_version
    );
    let profile_resp = http
        .get(&profile_url)
        .send()
        .map_err(|e| format!("获取 Quilt 配置失败: {}", e))?;
    let quilt_profile: serde_json::Value = profile_resp
        .json()
        .map_err(|e| format!("解析 Quilt 配置失败: {}", e))?;

    // 下载 Quilt 库
    if let Some(quilt_libs) = quilt_profile["libraries"].as_array() {
        let mut quilt_tasks: Vec<(String, std::path::PathBuf, Option<String>)> = Vec::new();

        for lib in quilt_libs {
            let name_str = lib["name"].as_str().unwrap_or("");
            let maven_url = lib["url"]
                .as_str()
                .unwrap_or("https://maven.quiltmc.org/repository/release/");
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
            let url = format!("{}{}", maven_url, relative_path);
            let Ok(relative_path) = safe_maven_path(&relative_path) else {
                continue;
            };
            let dest = game_dir.join("libs").join(relative_path);
            quilt_tasks.push((url, dest, sha1.map(|s| s.to_string())));
        }
        let total = quilt_tasks.len();
        emit("quilt", 0, total, &format!("下载 Quilt 库 0/{}", total));

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
                    "stage": "quilt",
                    "current": finished,
                    "total": total,
                    "detail": format!("Quilt 库 {}/{}", finished, total)
                }),
            );
            if finished >= total {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(300));
        });
        let download_result =
            parallel_download(http, quilt_tasks, &done, 32, use_mirror, Some(name));
        let _ = reporter.join();
        download_result.map_err(|e| format!("Quilt libraries failed: {}", e))?;
        emit("quilt", total, total, "Quilt 库下载完成");
    }

    // 合并 Quilt mainClass
    if let Some(main_class) = quilt_profile["mainClass"].as_str() {
        ver_json["mainClass"] = serde_json::Value::String(main_class.to_string());
    }
    // 合并库（按 group:artifact 去重）
    if let Some(quilt_libs) = quilt_profile["libraries"].as_array() {
        if let Some(existing_libs) = ver_json["libraries"].as_array_mut() {
            merge_libraries(existing_libs, quilt_libs);
        }
    }
    // 合并 arguments
    let quilt_arguments = &quilt_profile["arguments"];
    if !quilt_arguments.is_null() {
        if let Some(jvm_args) = quilt_arguments["jvm"].as_array() {
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
        if let Some(game_args) = quilt_arguments["game"].as_array() {
            if !game_args.is_empty() {
                if ver_json["arguments"].is_null() {
                    ver_json["arguments"] = serde_json::json!({"jvm": [], "game": game_args});
                } else if let Some(existing_game) = ver_json["arguments"]["game"].as_array_mut() {
                    for arg in game_args {
                        existing_game.push(arg.clone());
                    }
                }
            }
        }
    }

    ver_json["loader"] = serde_json::json!({
        "type": "quilt",
        "version": loader_version
    });
    emit("quilt", 1, 1, "Quilt 配置完成");
    Ok(())
}
