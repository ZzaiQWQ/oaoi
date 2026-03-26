use super::{download_file_if_needed, copy_dir_recursive, make_emitter, FORGE_LOCK};

/// 安装 Forge loader
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
    let installer_url = format!(
        "https://bmclapi2.bangbang93.com/maven/net/minecraftforge/forge/{0}/forge-{0}-installer.jar",
        forge_full_ver
    );
    let installer_path = inst_dir.join("forge-installer.jar");
    
    emit("forge", 0, 100, "下载 Forge 安装器...");
    download_file_if_needed(http, &installer_url, &installer_path, None, use_mirror)
        .map_err(|e| format!("下载 Forge 安装器失败: {}", e))?;

    // 2. 创建临时 .minecraft 目录结构
    let temp_mc = inst_dir.join(".forge_temp");
    let _ = std::fs::create_dir_all(&temp_mc);
    std::fs::write(temp_mc.join("launcher_profiles.json"), r#"{"profiles":{}}"#)
        .map_err(|e| format!("创建 launcher_profiles.json 失败: {}", e))?;

    let temp_ver_dir = temp_mc.join("versions").join(mc_version);
    let _ = std::fs::create_dir_all(&temp_ver_dir);
    let existing_client_jar = inst_dir.join("client.jar");
    if existing_client_jar.exists() {
        let _ = std::fs::copy(&existing_client_jar, temp_ver_dir.join(format!("{}.jar", mc_version)));
        eprintln!("[forge] 已复制 client.jar 到临时目录，避免重新下载");
    }

    // 3. 运行 Forge 安装器
    emit("forge", 30, 100, "运行 Forge 安装器 (这可能需要几分钟)...");
    eprintln!("[forge] Running installer: {} -jar {} --installClient {}",
        java_path, installer_path.display(), temp_mc.display());
    #[cfg(windows)]
    use std::os::windows::process::CommandExt;
    let status = std::process::Command::new(java_path)
        .args(["-jar", installer_path.to_str().unwrap(), "--installClient", temp_mc.to_str().unwrap()])
        .current_dir(inst_dir)
        .creation_flags(0x08000000)
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status()
        .map_err(|e| format!("启动 Forge 安装器失败: {}", e))?;

    if !status.success() {
        let _ = std::fs::remove_dir_all(&temp_mc);
        return Err("Forge 安装器执行失败或被用户取消".to_string());
    }

    // 4. 复制库
    emit("forge", 70, 100, "复制 Forge 组件...");
    let temp_libs = temp_mc.join("libraries");
    if temp_libs.exists() {
        copy_dir_recursive(&temp_libs, &game_dir.join("libs"))
            .map_err(|e| format!("复制 Forge 库失败: {}", e))?;
    }

    // 5. 解析 version.json
    emit("forge", 85, 100, "解析 Forge 配置...");
    let installer_file = std::fs::File::open(&installer_path)
        .map_err(|e| format!("打开 Forge 安装器失败: {}", e))?;
    let mut archive = zip::ZipArchive::new(installer_file)
        .map_err(|e| format!("解析 Forge 安装器 ZIP 失败: {}", e))?;

    let forge_version_json: Option<serde_json::Value> = archive.by_name("version.json").ok()
        .and_then(|mut f| {
            let mut s = String::new();
            use std::io::Read;
            f.read_to_string(&mut s).ok()?;
            serde_json::from_str(&s).ok()
        });

    if let Some(parsed_forge) = forge_version_json {
        if let Some(main_class) = parsed_forge["mainClass"].as_str() {
            ver_json["mainClass"] = serde_json::Value::String(main_class.to_string());
        }

        // 合并库（去重）
        if let Some(forge_libs) = parsed_forge["libraries"].as_array() {
            if let Some(existing_libs) = ver_json["libraries"].as_array_mut() {
                for forge_lib in forge_libs {
                    let forge_name = forge_lib["name"].as_str().unwrap_or("");
                    let forge_parts: Vec<&str> = forge_name.split(':').collect();
                    let forge_key = if forge_parts.len() >= 4 {
                        format!("{}:{}:{}", forge_parts[0], forge_parts[1], forge_parts[3])
                    } else if forge_parts.len() >= 2 {
                        format!("{}:{}", forge_parts[0], forge_parts[1])
                    } else { String::new() };

                    if !forge_key.is_empty() {
                        existing_libs.retain(|existing| {
                            let name = existing["name"].as_str().unwrap_or("");
                            let parts: Vec<&str> = name.split(':').collect();
                            if parts.len() >= 2 {
                                let key = if parts.len() >= 4 {
                                    format!("{}:{}:{}", parts[0], parts[1], parts[3])
                                } else {
                                    format!("{}:{}", parts[0], parts[1])
                                };
                                key != forge_key
                            } else { true }
                        });
                    }
                    existing_libs.push(forge_lib.clone());
                }
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
            ver_json["minecraftArguments"] = serde_json::Value::String(minecraft_args.to_string());
        }

        ver_json["loader"] = serde_json::json!({
            "type": "forge",
            "version": loader_version
        });
        emit("forge", 100, 100, "Forge 配置解析完成");
    } else {
        return Err("Forge 安装器中未找到 version.json".to_string());
    }

    // 清理
    let _ = std::fs::remove_dir_all(&temp_mc);
    let _ = std::fs::remove_file(installer_path);
    Ok(())
}
