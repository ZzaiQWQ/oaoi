use super::{
    build_data_map, download_file_with_progress, get_jar_main_class,
    installer_generated_client_library, make_emitter, maven_name_to_path, merge_libraries,
    parallel_download, resolve_data_arg, run_java_process_cancelable, safe_maven_path,
    wait_for_install_file, FORGE_LOCK,
};
use crate::instance::{libraries_dir, safe_join, version_jar_path};
use tauri::Emitter;

/// 安装 NeoForge loader（解压 installer.jar，自行下载库 + 执行 processors）
pub fn install_neoforge(
    app_handle: &tauri::AppHandle,
    name: &str,
    mc_version: &str,
    loader_version: &str,
    game_dir: &std::path::Path,
    inst_dir: &std::path::Path,
    http: &reqwest::blocking::Client,
    java_path: &str,
    use_mirror: bool,
    ver_json: &mut serde_json::Value,
) -> Result<(), String> {
    install_neoforge_with_names(
        app_handle,
        name,
        name,
        mc_version,
        loader_version,
        game_dir,
        inst_dir,
        http,
        java_path,
        use_mirror,
        ver_json,
    )
}

pub fn install_neoforge_with_names(
    app_handle: &tauri::AppHandle,
    progress_name: &str,
    version_name: &str,
    mc_version: &str,
    loader_version: &str,
    game_dir: &std::path::Path,
    inst_dir: &std::path::Path,
    http: &reqwest::blocking::Client,
    java_path: &str,
    use_mirror: bool,
    ver_json: &mut serde_json::Value,
) -> Result<(), String> {
    if java_path.is_empty() {
        return Err("必须先在设置中配置 Java 路径才能安装 NeoForge".to_string());
    }

    let emit = make_emitter(app_handle, progress_name);
    emit(
        "neoforge",
        0,
        1,
        &format!("处理 NeoForge {}...", loader_version),
    );

    // 获取安装锁（和 Forge 共享）
    emit("neoforge", 0, 100, "等待其他安装器完成...");
    let _forge_guard = loop {
        if crate::instance::is_cancelled(progress_name) {
            return Err("用户取消安装".to_string());
        }
        match FORGE_LOCK.try_lock() {
            Ok(guard) => break guard,
            Err(std::sync::TryLockError::WouldBlock) => {
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            Err(std::sync::TryLockError::Poisoned(e)) => break e.into_inner(),
        }
    };

    // 1. 下载 neoforge-installer.jar
    let legacy_artifact = loader_version.starts_with(&format!("{}-", mc_version));
    let (artifact_path, artifact_name) = if legacy_artifact {
        ("net/neoforged/forge", "forge")
    } else {
        ("net/neoforged/neoforge", "neoforge")
    };
    let installer_url = if use_mirror {
        format!(
            "https://bmclapi2.bangbang93.com/maven/{}/{}/{}-{}-installer.jar",
            artifact_path, loader_version, artifact_name, loader_version
        )
    } else {
        format!(
            "https://maven.neoforged.net/releases/{}/{}/{}-{}-installer.jar",
            artifact_path, loader_version, artifact_name, loader_version
        )
    };
    let installer_path = inst_dir.join("neoforge-installer.jar");

    emit("neoforge-installer", 0, 1, "下载 NeoForge 安装器...");
    download_file_with_progress(
        http,
        &installer_url,
        &installer_path,
        None,
        use_mirror,
        Some(progress_name),
        |downloaded, total| {
            let total = total.unwrap_or_else(|| downloaded.max(1)).max(1);
            emit(
                "neoforge-installer",
                downloaded.min(usize::MAX as u64) as usize,
                total.min(usize::MAX as u64) as usize,
                "下载 NeoForge 安装器...",
            );
        },
    )
    .map_err(|e| format!("下载 NeoForge 安装器失败: {}", e))?;
    emit("neoforge-installer", 1, 1, "NeoForge 安装器下载完成");

    // 2. 解压 installer.jar
    emit("neoforge", 15, 100, "解压 NeoForge 安装器...");
    let temp_dir = inst_dir.join(".neoforge_temp");
    let _ = std::fs::remove_dir_all(&temp_dir);
    std::fs::create_dir_all(&temp_dir).map_err(|e| e.to_string())?;

    {
        let file = std::fs::File::open(&installer_path).map_err(|e| e.to_string())?;
        let mut archive = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
        for i in 0..archive.len() {
            if crate::instance::is_cancelled(progress_name) {
                return Err("用户取消安装".to_string());
            }
            if let Ok(mut entry) = archive.by_index(i) {
                let Ok(out_path) = safe_join(&temp_dir, entry.name()) else {
                    continue;
                };
                if entry.is_dir() {
                    std::fs::create_dir_all(&out_path).ok();
                } else {
                    if let Some(parent) = out_path.parent() {
                        std::fs::create_dir_all(parent).ok();
                    }
                    if let Ok(mut out_file) = std::fs::File::create(&out_path) {
                        std::io::copy(&mut entry, &mut out_file).ok();
                    }
                }
            }
        }
    }

    // 3. 读取 version.json（NeoForge 的版本信息）
    emit("neoforge", 25, 100, "解析 NeoForge 配置...");
    let version_json_path = temp_dir.join("version.json");
    if !version_json_path.exists() {
        let _ = std::fs::remove_dir_all(&temp_dir);
        return Err("NeoForge installer 中未找到 version.json".to_string());
    }
    let nf_data = std::fs::read_to_string(&version_json_path).map_err(|e| e.to_string())?;
    let parsed_nf: serde_json::Value = serde_json::from_str(&nf_data).map_err(|e| e.to_string())?;

    // 4. 读取 install_profile.json（processors 和依赖信息）
    let install_profile_path = temp_dir.join("install_profile.json");
    let install_profile: Option<serde_json::Value> = if install_profile_path.exists() {
        let data = std::fs::read_to_string(&install_profile_path).map_err(|e| e.to_string())?;
        Some(serde_json::from_str(&data).map_err(|e| e.to_string())?)
    } else {
        None
    };

    // 5. 下载 NeoForge 的所有依赖库
    emit("neoforge", 30, 100, "下载 NeoForge 依赖库...");
    let libs_dir = libraries_dir(game_dir);
    std::fs::create_dir_all(&libs_dir).ok();

    // 从 version.json 和 install_profile.json 收集所有库
    let mut all_libs: Vec<serde_json::Value> = Vec::new();
    if let Some(libs) = parsed_nf["libraries"].as_array() {
        all_libs.extend(libs.clone());
    }
    if let Some(ref profile) = install_profile {
        if let Some(libs) = profile["libraries"].as_array() {
            all_libs.extend(libs.clone());
        }
    }

    let total_libs = all_libs.len();
    let mut scanned = 0;
    let mut download_tasks: Vec<(String, std::path::PathBuf, Option<String>)> = Vec::new();
    let mut generated_libs: Vec<(String, std::path::PathBuf)> = Vec::new();

    for lib in &all_libs {
        if crate::instance::is_cancelled(progress_name) {
            return Err("用户取消安装".to_string());
        }
        scanned += 1;
        let lib_name = lib["name"].as_str().unwrap_or("");

        // 解析 Maven 坐标 → 文件路径
        let (rel_path, artifact_url, sha1) =
            if let Some(artifact) = lib["downloads"]["artifact"].as_object() {
                let path = artifact
                    .get("path")
                    .and_then(|p| p.as_str())
                    .unwrap_or("")
                    .to_string();
                let url = artifact
                    .get("url")
                    .and_then(|u| u.as_str())
                    .unwrap_or("")
                    .to_string();
                let sha1 = artifact
                    .get("sha1")
                    .and_then(|s| s.as_str())
                    .map(|s| s.to_string());
                (path, url, sha1)
            } else if !lib_name.is_empty() {
                // 没有 downloads，从 name 推导路径
                let path = maven_name_to_path(lib_name);
                let url_base = lib
                    .get("url")
                    .and_then(|u| u.as_str())
                    .unwrap_or("https://maven.neoforged.net/releases");
                let url = format!("{}/{}", url_base.trim_end_matches('/'), path);
                let sha1 = lib
                    .get("sha1")
                    .and_then(|s| s.as_str())
                    .map(|s| s.to_string());
                (path, url, sha1)
            } else {
                continue;
            };

        if rel_path.is_empty() {
            continue;
        }
        let Ok(rel_path_buf) = safe_maven_path(&rel_path) else {
            eprintln!("[neoforge] skip unsafe library path: {}", rel_path);
            continue;
        };
        let dest = libs_dir.join(&rel_path_buf);

        if dest.exists() {
            continue;
        }

        // 先检查 installer.jar 里的 maven/ 目录是否有本地副本
        // 优先从 installer 内置的 maven/ 目录复制本地库
        let local_maven = temp_dir.join("maven").join(&rel_path_buf);
        if local_maven.exists() {
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent).ok();
            }
            std::fs::copy(&local_maven, &dest).ok();
            emit(
                "neoforge",
                30 + (scanned * 20 / total_libs.max(1)),
                100,
                &format!("复制本地库 {}/{}", scanned, total_libs),
            );
            continue;
        }

        if !artifact_url.is_empty() {
            download_tasks.push((artifact_url, dest, sha1));
        } else if installer_generated_client_library(lib_name) {
            // NeoForge/legacy NeoForge 的 client 库也可能由 processor 生成。
            generated_libs.push((lib_name.to_string(), dest));
        } else {
            return Err(format!("NeoForge 库缺少下载地址: {}", lib_name));
        }
    }
    if !download_tasks.is_empty() {
        let total = download_tasks.len();
        emit(
            "neoforge-libs",
            0,
            total,
            &format!("并行下载 {} 个 NeoForge 依赖库...", total),
        );
        let done = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let app_clone = app_handle.clone();
        let done_reporter = done.clone();
        let inst_name_copy = progress_name.to_string();
        let reporter = std::thread::spawn(move || loop {
            let finished = done_reporter.load(std::sync::atomic::Ordering::Relaxed);
            let _ = app_clone.emit(
                "install-progress",
                serde_json::json!({
                    "name": inst_name_copy,
                    "stage": "neoforge-libs",
                    "current": finished,
                    "total": total,
                    "detail": format!("NeoForge 依赖库 {}/{}", finished, total)
                }),
            );
            if finished >= total {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(300));
        });
        let result = parallel_download(
            http,
            download_tasks,
            &done,
            32,
            use_mirror,
            Some(progress_name),
        );
        let _ = reporter.join();
        result.map_err(|e| format!("NeoForge 依赖库下载失败: {}", e))?;
        emit("neoforge-libs", total, total, "NeoForge 依赖库下载完成");
    }

    // 6. 执行 Processors（运行 install_profile 定义的处理器链）
    if let Some(ref profile) = install_profile {
        if let Some(processors) = profile["processors"].as_array() {
            let client_processors: Vec<&serde_json::Value> = processors
                .iter()
                .filter(|p| {
                    // 过滤掉 server-only 的 processor
                    if let Some(sides) = p["sides"].as_array() {
                        sides.iter().any(|s| s.as_str() == Some("client"))
                    } else {
                        true // 没有 sides 字段 = 双端都执行
                    }
                })
                .collect();

            let total_proc = client_processors.len();
            let client_jar = version_jar_path(inst_dir, version_name);
            if total_proc > 0 {
                emit("neoforge", 70, 100, "等待版本 jar 完成...");
                wait_for_install_file(&client_jar, "版本 jar", progress_name)?;
            }

            for (i, proc) in client_processors.iter().enumerate() {
                if crate::instance::is_cancelled(progress_name) {
                    return Err("用户取消安装".to_string());
                }
                emit(
                    "neoforge",
                    70 + (i * 25 / total_proc.max(1)),
                    100,
                    &format!("执行处理器 {}/{}...", i + 1, total_proc),
                );

                let jar_name = proc["jar"].as_str().unwrap_or("");
                if jar_name.is_empty() {
                    continue;
                }

                let jar_rel_path = maven_name_to_path(jar_name);
                let Ok(jar_rel_path) = safe_maven_path(&jar_rel_path) else {
                    eprintln!("[neoforge] skip unsafe processor jar path: {}", jar_name);
                    continue;
                };
                let jar_path = libs_dir.join(jar_rel_path);
                if !jar_path.exists() {
                    return Err(format!(
                        "NeoForge processor jar 不存在: {}",
                        jar_path.display()
                    ));
                }

                // 构建 classpath（processor jar + 所有依赖）
                let mut proc_cp = vec![jar_path.to_string_lossy().to_string()];
                if let Some(classpath) = proc["classpath"].as_array() {
                    for cp in classpath {
                        if let Some(cp_name) = cp.as_str() {
                            let cp_rel_path = maven_name_to_path(cp_name);
                            let Ok(cp_rel_path) = safe_maven_path(&cp_rel_path) else {
                                eprintln!(
                                    "[neoforge] skip unsafe processor classpath: {}",
                                    cp_name
                                );
                                continue;
                            };
                            let cp_path = libs_dir.join(cp_rel_path);
                            if cp_path.exists() {
                                proc_cp.push(cp_path.to_string_lossy().to_string());
                            }
                        }
                    }
                }

                // 构建参数，替换 data 变量
                let ver_json_path = temp_dir.join("version.json");
                let data_map = build_data_map(
                    profile,
                    &libs_dir,
                    &client_jar,
                    &ver_json_path,
                    &installer_path,
                    &temp_dir,
                    mc_version,
                );

                let mut proc_args: Vec<String> = Vec::new();
                if let Some(args) = proc["args"].as_array() {
                    for arg in args {
                        if let Some(s) = arg.as_str() {
                            proc_args.push(resolve_data_arg(s, &data_map, &libs_dir));
                        }
                    }
                }

                // 从 jar manifest 获取 Main-Class
                let main_class = get_jar_main_class(&jar_path).unwrap_or_default();
                if main_class.is_empty() {
                    return Err(format!("NeoForge processor 缺少 Main-Class: {}", jar_name));
                }

                eprintln!(
                    "[neoforge] processor: {} main={} args={:?}",
                    jar_name, main_class, proc_args
                );

                match run_java_process_cancelable(
                    java_path,
                    &proc_cp.join(";"),
                    &main_class,
                    &proc_args,
                    inst_dir,
                    progress_name,
                ) {
                    Ok(status) => {
                        if !status.success() {
                            return Err(format!(
                                "NeoForge processor 执行失败: {} ({})",
                                jar_name, status
                            ));
                        }
                    }
                    Err(e) => {
                        if e.contains("取消") {
                            return Err(e);
                        }
                        return Err(format!("NeoForge processor 执行出错: {} - {}", jar_name, e));
                    }
                }
            }
        }
    }

    for (lib_name, dest) in &generated_libs {
        if !dest.exists() {
            return Err(format!(
                "NeoForge processor 未生成库: {} ({})",
                lib_name,
                dest.display()
            ));
        }
    }

    // 7. 合并 version.json 到实例配置（mainClass, libraries, arguments）
    emit("neoforge", 95, 100, "合并 NeoForge 配置...");

    if let Some(main_class) = parsed_nf["mainClass"].as_str() {
        ver_json["mainClass"] = serde_json::Value::String(main_class.to_string());
    }

    // 合并库（按 group:artifact 去重，避免 classpath 冲突）
    if let Some(nf_libs) = parsed_nf["libraries"].as_array() {
        if let Some(existing_libs) = ver_json["libraries"].as_array_mut() {
            merge_libraries(existing_libs, nf_libs);
        }
    }

    // 合并 arguments
    let nf_args = &parsed_nf["arguments"];
    if !nf_args.is_null() {
        let mut base_jvm = vec![];
        let mut base_game = vec![];
        if let Some(existing) = ver_json["arguments"]["jvm"].as_array() {
            base_jvm.extend(existing.clone());
        }
        if let Some(existing) = ver_json["arguments"]["game"].as_array() {
            base_game.extend(existing.clone());
        }
        if let Some(f_jvm) = nf_args["jvm"].as_array() {
            base_jvm.extend(f_jvm.clone());
        }
        if let Some(f_game) = nf_args["game"].as_array() {
            base_game.extend(f_game.clone());
        }
        if !base_jvm.is_empty() || !base_game.is_empty() {
            ver_json["arguments"] = serde_json::json!({
                "jvm": base_jvm,
                "game": base_game
            });
        }
    }

    ver_json["loader"] = serde_json::json!({
        "type": "neoforge",
        "version": loader_version
    });

    // 清理
    emit("neoforge", 100, 100, "NeoForge 安装完成");
    let _ = std::fs::remove_dir_all(&temp_dir);
    let _ = std::fs::remove_file(installer_path);
    Ok(())
}
