use crate::installer::fabric;
use crate::instance::{
    detect_loader, resolve_game_dir, safe_path_name, strip_launcher_private_version_fields,
    version_dir, version_json_path,
};
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};
use tauri::Emitter;

#[derive(Serialize)]
pub struct FabricUpgradeResult {
    #[serde(rename = "mcVersion")]
    pub mc_version: String,
    #[serde(rename = "oldLoaderVersion")]
    pub old_loader_version: String,
    #[serde(rename = "newLoaderVersion")]
    pub new_loader_version: String,
}

#[tauri::command]
pub async fn upgrade_fabric_loader(
    app_handle: tauri::AppHandle,
    game_dir: String,
    name: String,
    mc_version: String,
    current_loader_version: String,
    target_loader_version: String,
    use_mirror: bool,
) -> Result<FabricUpgradeResult, String> {
    tokio::task::spawn_blocking(move || {
        do_upgrade_fabric_loader(
            &app_handle,
            &game_dir,
            &name,
            &mc_version,
            &current_loader_version,
            &target_loader_version,
            use_mirror,
        )
    })
    .await
    .map_err(|e| e.to_string())?
}

fn do_upgrade_fabric_loader(
    app_handle: &tauri::AppHandle,
    game_dir_input: &str,
    name: &str,
    mc_version_input: &str,
    current_loader_version: &str,
    target_loader_version: &str,
    use_mirror: bool,
) -> Result<FabricUpgradeResult, String> {
    let safe_name = safe_path_name(name, "版本名")?;
    let target_loader_version = target_loader_version.trim();
    if target_loader_version.is_empty() {
        return Err("目标 Fabric Loader 版本不能为空".to_string());
    }

    emit_upgrade(app_handle, &safe_name, 0, 4, "准备更新 Fabric Loader...");

    let game_dir = resolve_game_dir(game_dir_input);
    let inst_dir = version_dir(&game_dir, &safe_name);
    let json_path = version_json_path(&inst_dir, &safe_name);
    let backup_dir = inst_dir.join(".oaoi_fabric_upgrade_backup");
    let backup_json = backup_dir.join(format!("{}.json", safe_name));

    let original_text =
        fs::read_to_string(&json_path).map_err(|e| format!("读取版本配置失败: {}", e))?;
    let mut ver_json: serde_json::Value =
        serde_json::from_str(&original_text).map_err(|e| format!("解析版本配置失败: {}", e))?;

    let (loader_type, detected_loader_version) = detect_loader(&ver_json, &safe_name);
    if loader_type != "fabric" {
        return Err("当前版本不是 Fabric，不能执行 Fabric Loader 更新".to_string());
    }

    let old_loader_version = if current_loader_version.trim().is_empty() {
        detected_loader_version
    } else {
        current_loader_version.trim().to_string()
    };
    if old_loader_version == target_loader_version {
        return Ok(FabricUpgradeResult {
            mc_version: resolve_mc_version(mc_version_input, &ver_json),
            old_loader_version,
            new_loader_version: target_loader_version.to_string(),
        });
    }

    fs::create_dir_all(&backup_dir).map_err(|e| format!("创建 Fabric 更新备份目录失败: {}", e))?;
    fs::copy(&json_path, &backup_json).map_err(|e| format!("备份版本配置失败: {}", e))?;

    let result = (|| {
        let mc_version = resolve_mc_version(mc_version_input, &ver_json);
        if mc_version.is_empty() || mc_version == "unknown" {
            return Err("无法识别当前 Minecraft 版本".to_string());
        }

        emit_upgrade(app_handle, &safe_name, 1, 4, "下载新版 Fabric 配置...");
        let http = reqwest::blocking::Client::builder()
            .pool_max_idle_per_host(32)
            .connect_timeout(std::time::Duration::from_secs(15))
            .timeout(std::time::Duration::from_secs(300))
            .user_agent("OAOI-Launcher/1.0")
            .build()
            .map_err(|e| format!("创建 Fabric 更新下载客户端失败: {}", e))?;

        ensure_libraries_array(&mut ver_json);
        remove_old_fabric_jvm_args(&mut ver_json);

        emit_upgrade(app_handle, &safe_name, 2, 4, "下载新版 Fabric 组件...");
        fabric::install_fabric(
            app_handle,
            &safe_name,
            &mc_version,
            target_loader_version,
            &game_dir,
            &inst_dir,
            &http,
            use_mirror,
            &mut ver_json,
            false,
        )?;

        emit_upgrade(app_handle, &safe_name, 3, 4, "写入新版 Fabric 配置...");
        strip_launcher_private_version_fields(&mut ver_json);
        write_version_json_with_restore(&json_path, &backup_json, &ver_json)?;

        Ok(FabricUpgradeResult {
            mc_version,
            old_loader_version: old_loader_version.clone(),
            new_loader_version: target_loader_version.to_string(),
        })
    })();

    match result {
        Ok(result) => {
            let _ = fs::remove_dir_all(&backup_dir);
            emit_upgrade(app_handle, &safe_name, 4, 4, "Fabric Loader 更新完成");
            Ok(result)
        }
        Err(error) => {
            let _ = restore_backup(&backup_json, &json_path);
            emit_upgrade(
                app_handle,
                &safe_name,
                0,
                0,
                &format!("Fabric Loader 更新失败: {}", error),
            );
            Err(error)
        }
    }
}

fn emit_upgrade(app_handle: &tauri::AppHandle, name: &str, current: u32, total: u32, detail: &str) {
    let _ = app_handle.emit(
        "install-progress",
        serde_json::json!({
            "name": name,
            "stage": "fabric-upgrade",
            "current": current,
            "total": total,
            "detail": detail
        }),
    );
}

fn resolve_mc_version(input: &str, json: &serde_json::Value) -> String {
    let input = input.trim();
    if !input.is_empty() {
        return input.to_string();
    }
    if let Some(version) = json.get("clientVersion").and_then(|v| v.as_str()) {
        return version.to_string();
    }
    if let Some(version) = json.get("mcVersion").and_then(|v| v.as_str()) {
        return version.to_string();
    }
    if let Some(version) = json
        .get("libraries")
        .and_then(|v| v.as_array())
        .and_then(|libs| {
            libs.iter().find_map(|lib| {
                lib.get("name")
                    .and_then(|v| v.as_str())
                    .and_then(|name| name.strip_prefix("net.fabricmc:intermediary:"))
                    .map(|version| version.to_string())
            })
        })
    {
        return version;
    }
    json.get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string()
}

fn ensure_libraries_array(json: &mut serde_json::Value) {
    if !json.get("libraries").is_some_and(|value| value.is_array()) {
        json["libraries"] = serde_json::json!([]);
    }
}

fn remove_old_fabric_jvm_args(json: &mut serde_json::Value) {
    let Some(args) = json
        .get_mut("arguments")
        .and_then(|value| value.get_mut("jvm"))
        .and_then(|value| value.as_array_mut())
    else {
        return;
    };

    // Fabric Loader 的 JVM 参数需要整组替换，否则多次更新会留下旧版本参数。
    args.retain(|arg| {
        !arg.as_str().is_some_and(|text| {
            matches!(text, "-Dfabric.side=client" | "-Dfabric.development=false")
                || text.starts_with("-Dfabric.gameVersion=")
                || text.starts_with("-Dfabric.loader.version=")
        })
    });
}

fn write_version_json_with_restore(
    json_path: &Path,
    backup_json: &Path,
    json: &serde_json::Value,
) -> Result<(), String> {
    let tmp_path = temp_json_path(json_path);
    let data =
        serde_json::to_string_pretty(json).map_err(|e| format!("序列化版本配置失败: {}", e))?;
    fs::write(&tmp_path, data).map_err(|e| format!("写入临时版本配置失败: {}", e))?;

    if let Err(error) = fs::remove_file(json_path) {
        let _ = fs::remove_file(&tmp_path);
        return Err(format!("替换旧版本配置失败: {}", error));
    }

    if let Err(error) = fs::rename(&tmp_path, json_path) {
        let _ = restore_backup(backup_json, json_path);
        let _ = fs::remove_file(&tmp_path);
        return Err(format!("写入新版版本配置失败: {}", error));
    }

    Ok(())
}

fn restore_backup(backup_json: &Path, json_path: &Path) -> Result<(), String> {
    if !backup_json.exists() {
        return Ok(());
    }
    fs::copy(backup_json, json_path)
        .map(|_| ())
        .map_err(|e| format!("恢复 Fabric 更新备份失败: {}", e))
}

fn temp_json_path(json_path: &Path) -> PathBuf {
    let file_name = json_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("version.json");
    json_path.with_file_name(format!("{}.oaoi_tmp", file_name))
}
