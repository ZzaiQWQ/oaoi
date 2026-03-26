use super::{download_file_if_needed, copy_dir_recursive, make_emitter, FORGE_LOCK};

/// 安装 NeoForge loader
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
    if java_path.is_empty() {
        return Err("必须先在设置中配置 Java 路径才能安装 NeoForge".to_string());
    }

    let emit = make_emitter(app_handle, name);
    emit("neoforge", 0, 1, &format!("处理 NeoForge {}...", loader_version));

    // 获取安装锁（和 Forge 共享）
    emit("neoforge", 0, 100, "等待其他安装器完成...");
    let _forge_guard = FORGE_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    // 1. 下载 neoforge-installer.jar
    let installer_url = format!(
        "https://maven.neoforged.net/releases/net/neoforged/neoforge/{0}/neoforge-{0}-installer.jar",
        loader_version
    );
    let installer_path = inst_dir.join("neoforge-installer.jar");
    
    emit("neoforge", 0, 100, "下载 NeoForge 安装器...");
    download_file_if_needed(http, &installer_url, &installer_path, None, use_mirror)
        .map_err(|e| format!("下载 NeoForge 安装器失败: {}", e))?;

    // 2. 创建临时 .minecraft 目录结构
    let temp_mc = inst_dir.join(".neoforge_temp");
    let _ = std::fs::create_dir_all(&temp_mc);
    std::fs::write(temp_mc.join("launcher_profiles.json"), r#"{"profiles":{}}"#)
        .map_err(|e| format!("创建 launcher_profiles.json 失败: {}", e))?;

    let temp_ver_dir = temp_mc.join("versions").join(mc_version);
    let _ = std::fs::create_dir_all(&temp_ver_dir);
    let existing_client_jar = inst_dir.join("client.jar");
    if existing_client_jar.exists() {
        let _ = std::fs::copy(&existing_client_jar, temp_ver_dir.join(format!("{}.jar", mc_version)));
    }

    // 3. 运行 NeoForge 安装器
    emit("neoforge", 30, 100, "运行 NeoForge 安装器 (这可能需要几分钟)...");
    #[cfg(windows)]
    use std::os::windows::process::CommandExt;
    let status = std::process::Command::new(java_path)
        .args(["-jar", installer_path.to_str().unwrap(), "--install-client", temp_mc.to_str().unwrap()])
        .current_dir(inst_dir)
        .creation_flags(0x08000000)
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status()
        .map_err(|e| format!("启动 NeoForge 安装器失败: {}", e))?;

    if !status.success() {
        let _ = std::fs::remove_dir_all(&temp_mc);
        return Err("NeoForge 安装器执行失败".to_string());
    }

    // 4. 复制库
    emit("neoforge", 70, 100, "复制 NeoForge 组件...");
    let temp_libs = temp_mc.join("libraries");
    if temp_libs.exists() {
        let global_libs = game_dir.join("libs");
        copy_dir_recursive(&temp_libs, &global_libs)
            .map_err(|e| format!("复制 NeoForge 库失败: {}", e))?;
    }

    // 5. 解析 NeoForge version.json
    emit("neoforge", 85, 100, "解析 NeoForge 配置...");
    let nf_ver_prefix = format!("neoforge-{}", loader_version);
    let temp_versions = temp_mc.join("versions");
    let mut nf_json_path = None;
    if let Ok(entries) = std::fs::read_dir(&temp_versions) {
        for entry in entries.flatten() {
            let entry_name = entry.file_name().to_string_lossy().to_string();
            if entry_name.contains("neoforge") || entry_name.starts_with(&nf_ver_prefix) {
                let json_file = entry.path().join(format!("{}.json", entry_name));
                if json_file.exists() {
                    nf_json_path = Some(json_file);
                    break;
                }
            }
        }
    }

    if let Some(json_path) = nf_json_path {
        let nf_data = std::fs::read_to_string(&json_path)
            .map_err(|e| format!("读取 NeoForge version.json 失败: {}", e))?;
        let parsed_nf: serde_json::Value = serde_json::from_str(&nf_data)
            .map_err(|e| format!("解析 NeoForge version.json 失败: {}", e))?;

        if let Some(main_class) = parsed_nf["mainClass"].as_str() {
            ver_json["mainClass"] = serde_json::Value::String(main_class.to_string());
        }

        // 合并库
        if let Some(nf_libs) = parsed_nf["libraries"].as_array() {
            if let Some(existing_libs) = ver_json["libraries"].as_array_mut() {
                for nf_lib in nf_libs {
                    existing_libs.push(nf_lib.clone());
                }
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
        emit("neoforge", 100, 100, "NeoForge 配置完成");
    } else {
        return Err("NeoForge 安装器中未找到 version.json".to_string());
    }

    // 清理
    let _ = std::fs::remove_dir_all(&temp_mc);
    let _ = std::fs::remove_file(installer_path);
    Ok(())
}
