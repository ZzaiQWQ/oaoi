use super::{
    download_file_if_needed_cancelable, fetch_json_cancelable, make_emitter, merge_libraries,
    parallel_download, safe_maven_path, LoaderDownloadTask,
};
use crate::instance::{libraries_dir, safe_path_name};
use tauri::Emitter;

const FABRIC_META_MIRROR_API: &str = "https://bmclapi2.bangbang93.com/fabric-meta/v2";
const FABRIC_META_OFFICIAL_API: &str = "https://meta.fabricmc.net/v2";
const MODRINTH_MIRROR_API: &str = "https://mod.mcimirror.top/modrinth/v2";
const MODRINTH_OFFICIAL_API: &str = "https://api.modrinth.com/v2";

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
    install_fabric_with_cancel_name(
        app_handle,
        name,
        name,
        mc_version,
        loader_version,
        game_dir,
        inst_dir,
        http,
        use_mirror,
        ver_json,
        auto_download_fabric_api,
    )
}

pub fn install_fabric_with_cancel_name(
    app_handle: &tauri::AppHandle,
    name: &str,
    cancel_name: &str,
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

    let profile_path = format!(
        "/versions/loader/{}/{}/profile/json",
        urlencoding::encode(mc_version),
        urlencoding::encode(loader_version)
    );
    let fabric_profile =
        fetch_fabric_profile_with_fallback(http, &profile_path, use_mirror, cancel_name)
            .map_err(|e| format!("获取 Fabric 配置失败: {}", e))?;

    // 下载 Fabric 库
    if let Some(fabric_libs) = fabric_profile["libraries"].as_array() {
        let mut fabric_tasks: Vec<LoaderDownloadTask> = Vec::new();

        for lib in fabric_libs {
            let name_str = lib["name"].as_str().unwrap_or("");
            let maven_url = lib["url"].as_str().unwrap_or("https://maven.fabricmc.net/");
            let sha1 = lib["sha1"].as_str();
            let size = lib["size"].as_u64();

            if name_str.is_empty() {
                return Err("Fabric 配置中的依赖库缺少 name".to_string());
            }
            let parts: Vec<&str> = name_str.split(':').collect();
            if parts.len() < 3 {
                return Err(format!("Fabric 依赖库坐标无效: {}", name_str));
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
            let relative_path = safe_maven_path(&relative_path)
                .map_err(|e| format!("Fabric 依赖库路径无效 {}: {}", name_str, e))?;
            let dest = libraries_dir(game_dir).join(relative_path);

            fabric_tasks.push((url, dest, sha1.map(|s| s.to_string()), size));
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
            parallel_download(http, fabric_tasks, &done, 32, use_mirror, Some(cancel_name));
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
    let api_path = format!(
        "/project/P7dR8mSH/version?loaders={}&game_versions={}",
        loader_filter, game_version_filter
    );
    match fetch_fabric_api_versions_with_fallback(http, &api_path, use_mirror, cancel_name) {
        Ok(versions) => {
            if let Some(arr) = versions.as_array() {
                if let Some(first) = arr.first() {
                    if let Some(files) = first["files"].as_array() {
                        let file = files
                            .iter()
                            .find(|file| {
                                file["primary"].as_bool() == Some(true)
                                    && is_fabric_api_runtime_file(file)
                            })
                            .or_else(|| files.iter().find(|file| is_fabric_api_runtime_file(file)));
                        if let Some(file) = file {
                            let dl_url = file["url"].as_str().unwrap_or("");
                            let filename = file["filename"].as_str().unwrap_or("fabric-api.jar");
                            let sha1 = file["hashes"]["sha1"].as_str();
                            let size = file["size"].as_u64();
                            if !dl_url.is_empty() {
                                if let Ok(safe_filename) = safe_path_name(filename, "文件名") {
                                    let dest = mods_dir.join(&safe_filename);
                                    match download_file_if_needed_cancelable(
                                        http,
                                        dl_url,
                                        &dest,
                                        sha1,
                                        size,
                                        use_mirror,
                                        Some(cancel_name),
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
        }
        Err(error) if crate::instance::is_cancelled(cancel_name) => return Err(error),
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

fn validate_loader_profile(profile: &serde_json::Value, loader: &str) -> Result<(), String> {
    if !profile.is_object() {
        return Err(format!("{} 配置格式错误", loader));
    }
    if profile
        .get("mainClass")
        .and_then(|value| value.as_str())
        .map_or(true, |value| value.trim().is_empty())
    {
        return Err(format!("{} 配置缺少 mainClass", loader));
    }
    if profile
        .get("libraries")
        .and_then(|value| value.as_array())
        .is_none()
    {
        return Err(format!("{} 配置缺少 libraries", loader));
    }
    Ok(())
}

fn metadata_url_candidates(
    use_mirror: bool,
    mirror_base: &str,
    official_base: &str,
    path: &str,
) -> [String; 2] {
    let mirror_url = format!("{}{}", mirror_base.trim_end_matches('/'), path);
    let official_url = format!("{}{}", official_base.trim_end_matches('/'), path);
    if use_mirror {
        [mirror_url, official_url]
    } else {
        [official_url, mirror_url]
    }
}

fn fetch_fabric_profile_with_fallback(
    http: &reqwest::blocking::Client,
    path: &str,
    use_mirror: bool,
    cancel_name: &str,
) -> Result<serde_json::Value, String> {
    let urls = metadata_url_candidates(
        use_mirror,
        FABRIC_META_MIRROR_API,
        FABRIC_META_OFFICIAL_API,
        path,
    );
    let mut errors = Vec::new();
    for url in urls {
        match fetch_json_cancelable(http, &url, cancel_name, std::time::Duration::from_secs(30)) {
            Ok(profile) => match validate_loader_profile(&profile, "Fabric") {
                Ok(()) => return Ok(profile),
                Err(error) => errors.push(format!("{}: {}", url, error)),
            },
            Err(error) if crate::instance::is_cancelled(cancel_name) => return Err(error),
            Err(error) => errors.push(format!("{}: {}", url, error)),
        }
    }
    Err(errors.join("; "))
}

fn fetch_fabric_api_versions_with_fallback(
    http: &reqwest::blocking::Client,
    path: &str,
    use_mirror: bool,
    cancel_name: &str,
) -> Result<serde_json::Value, String> {
    let urls =
        metadata_url_candidates(use_mirror, MODRINTH_MIRROR_API, MODRINTH_OFFICIAL_API, path);
    let mut errors = Vec::new();
    let mut successful_empty = None;
    for url in urls {
        match fetch_json_cancelable(http, &url, cancel_name, std::time::Duration::from_secs(30)) {
            Ok(versions) => match versions.as_array() {
                Some(items) if !items.is_empty() => return Ok(versions),
                Some(_) => {
                    successful_empty = Some(versions);
                    errors.push(format!("{} 返回空列表", url));
                }
                None => errors.push(format!("{}: 响应不是数组", url)),
            },
            Err(error) if crate::instance::is_cancelled(cancel_name) => return Err(error),
            Err(error) => errors.push(format!("{}: {}", url, error)),
        }
    }
    successful_empty.ok_or_else(|| errors.join("; "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mirror_mode_puts_mirror_metadata_first() {
        let urls = metadata_url_candidates(
            true,
            FABRIC_META_MIRROR_API,
            FABRIC_META_OFFICIAL_API,
            "/versions/loader/1.21.1/0.16.14/profile/json",
        );
        assert!(urls[0].starts_with(FABRIC_META_MIRROR_API));
        assert!(urls[1].starts_with(FABRIC_META_OFFICIAL_API));
    }

    #[test]
    fn official_mode_puts_official_metadata_first() {
        let urls = metadata_url_candidates(
            false,
            MODRINTH_MIRROR_API,
            MODRINTH_OFFICIAL_API,
            "/project/P7dR8mSH/version",
        );
        assert!(urls[0].starts_with(MODRINTH_OFFICIAL_API));
        assert!(urls[1].starts_with(MODRINTH_MIRROR_API));
    }
}
