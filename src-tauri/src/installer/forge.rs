use super::{download_file_if_needed, make_emitter, FORGE_LOCK, maven_name_to_path, build_data_map, resolve_data_arg, get_jar_main_class, merge_libraries};

/// 安装 Forge loader（解压 installer.jar，自行下载库 + 执行 processors）
pub fn install_forge(
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
    if java_path.is_empty() {
        return Err("必须先在设置中配置 Java 路径才能安装 Forge".to_string());
    }

    let emit = make_emitter(app_handle, name);
    emit("forge", 0, 1, &format!("处理 Forge {}...", loader_version));

    // 获取 Forge 安装锁
    emit("forge", 0, 100, "等待其他 Forge 安装完成...");
    let _forge_guard = FORGE_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    // 1. 下载 forge-installer.jar
    let forge_full_ver = format!("{}-{}", mc_version, loader_version);
    let installer_url = if use_mirror {
        format!(
            "https://bmclapi2.bangbang93.com/maven/net/minecraftforge/forge/{0}/forge-{0}-installer.jar",
            forge_full_ver
        )
    } else {
        format!(
            "https://maven.minecraftforge.net/net/minecraftforge/forge/{0}/forge-{0}-installer.jar",
            forge_full_ver
        )
    };
    let installer_path = inst_dir.join("forge-installer.jar");
    
    emit("forge", 5, 100, "下载 Forge 安装器...");
    download_file_if_needed(http, &installer_url, &installer_path, None, use_mirror)
        .map_err(|e| format!("下载 Forge 安装器失败: {}", e))?;

    // 2. 解压 installer.jar
    emit("forge", 15, 100, "解压 Forge 安装器...");
    let temp_dir = inst_dir.join(".forge_temp");
    let _ = std::fs::remove_dir_all(&temp_dir);
    std::fs::create_dir_all(&temp_dir).map_err(|e| e.to_string())?;

    {
        let file = std::fs::File::open(&installer_path).map_err(|e| e.to_string())?;
        let mut archive = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
        for i in 0..archive.len() {
            if let Ok(mut entry) = archive.by_index(i) {
                let out_path = temp_dir.join(entry.name());
                // 防止 ZipSlip 路径穿越攻击
                if !out_path.starts_with(&temp_dir) { continue; }
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

    // 3. 读取 version.json
    emit("forge", 25, 100, "解析 Forge 配置...");
    let version_json_path = temp_dir.join("version.json");
    if !version_json_path.exists() {
        let _ = std::fs::remove_dir_all(&temp_dir);
        return Err("Forge installer 中未找到 version.json".to_string());
    }
    let forge_data = std::fs::read_to_string(&version_json_path).map_err(|e| e.to_string())?;
    let parsed_forge: serde_json::Value = serde_json::from_str(&forge_data).map_err(|e| e.to_string())?;

    // 4. 读取 install_profile.json（processors 和依赖信息）
    let install_profile_path = temp_dir.join("install_profile.json");
    let install_profile: Option<serde_json::Value> = if install_profile_path.exists() {
        let data = std::fs::read_to_string(&install_profile_path).map_err(|e| e.to_string())?;
        Some(serde_json::from_str(&data).map_err(|e| e.to_string())?)
    } else {
        None
    };

    // 5. 下载 Forge 的所有依赖库
    emit("forge", 30, 100, "下载 Forge 依赖库...");
    let libs_dir = game_dir.join("libs");
    std::fs::create_dir_all(&libs_dir).ok();

    // 从 version.json 和 install_profile.json 收集所有库
    let mut all_libs: Vec<serde_json::Value> = Vec::new();
    if let Some(libs) = parsed_forge["libraries"].as_array() {
        all_libs.extend(libs.clone());
    }
    if let Some(ref profile) = install_profile {
        if let Some(libs) = profile["libraries"].as_array() {
            all_libs.extend(libs.clone());
        }
    }

    let total_libs = all_libs.len();
    let mut downloaded = 0;

    for lib in &all_libs {
        downloaded += 1;
        let lib_name = lib["name"].as_str().unwrap_or("");

        // 解析 Maven 坐标 → 文件路径
        let (rel_path, artifact_url) = if let Some(artifact) = lib["downloads"]["artifact"].as_object() {
            let path = artifact.get("path").and_then(|p| p.as_str()).unwrap_or("").to_string();
            let url = artifact.get("url").and_then(|u| u.as_str()).unwrap_or("").to_string();
            (path, url)
        } else if !lib_name.is_empty() {
            // 没有 downloads，从 name 推导路径
            let path = maven_name_to_path(lib_name);
            let url_base = lib.get("url").and_then(|u| u.as_str()).unwrap_or("https://maven.minecraftforge.net");
            let url = format!("{}/{}", url_base.trim_end_matches('/'), path);
            (path, url)
        } else {
            continue;
        };

        if rel_path.is_empty() { continue; }
        let dest = libs_dir.join(rel_path.replace('/', std::path::MAIN_SEPARATOR_STR));

        if dest.exists() { continue; }

        // 先检查 installer.jar 里的 maven/ 目录是否有本地副本
        let local_maven = temp_dir.join("maven").join(&rel_path);
        if local_maven.exists() {
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent).ok();
            }
            std::fs::copy(&local_maven, &dest).ok();
            emit("forge", 30 + (downloaded * 40 / total_libs.max(1)), 100,
                &format!("复制本地库 {}/{}", downloaded, total_libs));
            continue;
        }

        // 下载
        if !artifact_url.is_empty() {
            emit("forge", 30 + (downloaded * 40 / total_libs.max(1)), 100,
                &format!("下载库 {}/{}", downloaded, total_libs));
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent).ok();
            }
            match download_file_if_needed(http, &artifact_url, &dest, None, use_mirror) {
                Ok(_) => {},
                Err(e) => {
                    eprintln!("[forge] 库下载失败(非致命): {} - {}", lib_name, e);
                }
            }
        }
    }

    // 6. 执行 Processors（运行 install_profile 定义的处理器链）
    if let Some(ref profile) = install_profile {
        if let Some(processors) = profile["processors"].as_array() {
            let client_processors: Vec<&serde_json::Value> = processors.iter()
                .filter(|p| {
                    if let Some(sides) = p["sides"].as_array() {
                        sides.iter().any(|s| s.as_str() == Some("client"))
                    } else {
                        true
                    }
                })
                .collect();

            let total_proc = client_processors.len();

            for (i, proc) in client_processors.iter().enumerate() {
                emit("forge", 70 + (i * 25 / total_proc.max(1)), 100,
                    &format!("执行处理器 {}/{}...", i + 1, total_proc));

                let jar_name = proc["jar"].as_str().unwrap_or("");
                if jar_name.is_empty() { continue; }

                let jar_path = libs_dir.join(maven_name_to_path(jar_name).replace('/', std::path::MAIN_SEPARATOR_STR));
                if !jar_path.exists() {
                    eprintln!("[forge] processor jar 不存在: {}", jar_path.display());
                    continue;
                }

                // 构建 classpath（processor jar + 所有依赖）
                let mut proc_cp = vec![jar_path.to_string_lossy().to_string()];
                if let Some(classpath) = proc["classpath"].as_array() {
                    for cp in classpath {
                        if let Some(cp_name) = cp.as_str() {
                            let cp_path = libs_dir.join(maven_name_to_path(cp_name).replace('/', std::path::MAIN_SEPARATOR_STR));
                            if cp_path.exists() {
                                proc_cp.push(cp_path.to_string_lossy().to_string());
                            }
                        }
                    }
                }

                // 构建参数，替换 data 变量
                let client_jar = inst_dir.join("client.jar");
                let data_map = build_data_map(profile, &libs_dir, &client_jar, &version_json_path, &installer_path, &temp_dir, mc_version);

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
                    eprintln!("[forge] 无法获取 processor main class: {}", jar_name);
                    continue;
                }

                eprintln!("[forge] processor: {} main={} args={:?}", jar_name, main_class, proc_args);

                #[cfg(windows)]
                use std::os::windows::process::CommandExt;
                let output = std::process::Command::new(java_path)
                    .arg("-cp")
                    .arg(proc_cp.join(";"))
                    .arg(&main_class)
                    .args(&proc_args)
                    .current_dir(inst_dir)
                    .creation_flags(0x08000000)
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .stdin(std::process::Stdio::null())
                    .output();

                match output {
                    Ok(o) => {
                        if !o.status.success() {
                            let stderr = String::from_utf8_lossy(&o.stderr);
                            eprintln!("[forge] processor 失败: {} - {}", jar_name, stderr.chars().take(300).collect::<String>());
                        }
                    }
                    Err(e) => {
                        eprintln!("[forge] processor 执行出错: {} - {}", jar_name, e);
                    }
                }
            }
        }
    }

    // 7. 合并 version.json 到实例配置（mainClass, libraries, arguments）
    emit("forge", 95, 100, "合并 Forge 配置...");

    if let Some(main_class) = parsed_forge["mainClass"].as_str() {
        ver_json["mainClass"] = serde_json::Value::String(main_class.to_string());
    }

    // 合并库（按 group:artifact 去重）
    if let Some(forge_libs) = parsed_forge["libraries"].as_array() {
        if let Some(existing_libs) = ver_json["libraries"].as_array_mut() {
            merge_libraries(existing_libs, forge_libs);
        }
    }

    // 合并 arguments
    let forge_args = &parsed_forge["arguments"];
    if !forge_args.is_null() {
        let mut base_jvm = vec![];
        let mut base_game = vec![];

        if let Some(existing) = ver_json["arguments"]["jvm"].as_array() {
            base_jvm.extend(existing.clone());
        }
        if let Some(existing) = ver_json["arguments"]["game"].as_array() {
            base_game.extend(existing.clone());
        }

        if let Some(f_jvm) = forge_args["jvm"].as_array() {
            base_jvm.extend(f_jvm.clone());
        }
        if let Some(f_game) = forge_args["game"].as_array() {
            base_game.extend(f_game.clone());
        }

        if !base_jvm.is_empty() || !base_game.is_empty() {
            ver_json["arguments"] = serde_json::json!({
                "jvm": base_jvm,
                "game": base_game
            });
        }
    } else if let Some(minecraft_args) = parsed_forge["minecraftArguments"].as_str() {
        // Forge 老版本 (1.12.2-) 用 minecraftArguments 而不是 arguments
        ver_json["minecraftArguments"] = serde_json::Value::String(minecraft_args.to_string());
    }

    ver_json["loader"] = serde_json::json!({
        "type": "forge",
        "version": loader_version
    });

    // 清理
    emit("forge", 100, 100, "Forge 安装完成");
    let _ = std::fs::remove_dir_all(&temp_dir);
    let _ = std::fs::remove_file(installer_path);
    Ok(())
}
