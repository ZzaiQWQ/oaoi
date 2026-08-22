use crate::installer::fabric;
use crate::instance::{
    detect_loader, resolve_game_dir, safe_path_name, scoped_task_key,
    strip_launcher_private_version_fields, try_register_cancel, unregister_cancel, version_dir,
    version_json_path,
};
use serde::Serialize;
use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use tauri::Emitter;

fn fabric_upgrade_locks() -> &'static Mutex<HashSet<String>> {
    static LOCKS: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    LOCKS.get_or_init(|| Mutex::new(HashSet::new()))
}

struct FabricUpgradeGuard {
    key: String,
}

impl FabricUpgradeGuard {
    fn acquire(json_path: &Path) -> Result<Self, String> {
        let key = json_path
            .to_string_lossy()
            .replace('/', "\\")
            .to_ascii_lowercase();
        let mut active = fabric_upgrade_locks()
            .lock()
            .map_err(|_| "Fabric 更新锁已损坏".to_string())?;
        if !active.insert(key.clone()) {
            return Err("这个实例正在更新 Fabric Loader，请等待当前任务完成".to_string());
        }
        Ok(Self { key })
    }
}

impl Drop for FabricUpgradeGuard {
    fn drop(&mut self) {
        if let Ok(mut active) = fabric_upgrade_locks().lock() {
            active.remove(&self.key);
        }
    }
}

struct FabricCancelRegistration {
    key: String,
}

impl FabricCancelRegistration {
    fn register(game_dir: &str, name: &str) -> Result<Self, String> {
        let key = scoped_task_key(game_dir, name);
        try_register_cancel(&key)?;
        Ok(Self { key })
    }
}

impl Drop for FabricCancelRegistration {
    fn drop(&mut self) {
        unregister_cancel(&self.key);
    }
}

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
    // 保留命令参数兼容现有前端，但更新判断不再信任该缓存值。
    drop(current_loader_version);
    let safe_name = safe_path_name(&name, "版本名")?;
    let cancel_registration = FabricCancelRegistration::register(&game_dir, &safe_name)?;
    let cancel_name = cancel_registration.key.clone();
    tokio::task::spawn_blocking(move || {
        let _cancel_registration = cancel_registration;
        do_upgrade_fabric_loader(
            &app_handle,
            &game_dir,
            &name,
            &mc_version,
            &target_loader_version,
            use_mirror,
            &cancel_name,
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
    target_loader_version: &str,
    use_mirror: bool,
    cancel_name: &str,
) -> Result<FabricUpgradeResult, String> {
    let safe_name = safe_path_name(name, "版本名")?;
    let target_loader_version = target_loader_version.trim();
    if target_loader_version.is_empty() {
        return Err("目标 Fabric Loader 版本不能为空".to_string());
    }
    ensure_upgrade_not_cancelled(cancel_name)?;

    emit_upgrade(app_handle, &safe_name, 0, 4, "准备更新 Fabric Loader...");

    let game_dir = resolve_game_dir(game_dir_input);
    let inst_dir = version_dir(&game_dir, &safe_name);
    let json_path = version_json_path(&inst_dir, &safe_name);
    let mutation_lock = crate::mod_manager::mod_mutation_lock(game_dir_input, &safe_name)?;
    let _mutation_guard = match mutation_lock.try_lock() {
        Ok(guard) => guard,
        Err(std::sync::TryLockError::WouldBlock) => {
            return Err("当前实例正在启动或执行文件任务，请稍后再更新 Fabric Loader".to_string());
        }
        Err(std::sync::TryLockError::Poisoned(_)) => {
            return Err("版本文件事务锁已损坏".to_string());
        }
    };
    let _upgrade_guard = FabricUpgradeGuard::acquire(&json_path)?;
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

    // 当前 Loader 版本只从实例 JSON 识别，前端缓存不能参与更新判断。
    let old_loader_version = detected_loader_version;
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
        ensure_upgrade_not_cancelled(cancel_name)?;
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
        remove_old_fabric_libraries(&mut ver_json);
        remove_old_fabric_jvm_args(&mut ver_json);

        emit_upgrade(app_handle, &safe_name, 2, 4, "下载新版 Fabric 组件...");
        fabric::install_fabric_with_cancel_name(
            app_handle,
            &safe_name,
            cancel_name,
            &mc_version,
            target_loader_version,
            &game_dir,
            &inst_dir,
            &http,
            use_mirror,
            &mut ver_json,
            false,
        )?;

        ensure_upgrade_not_cancelled(cancel_name)?;
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
            let restore_error = restore_backup(&backup_json, &json_path).err();
            emit_upgrade(
                app_handle,
                &safe_name,
                0,
                0,
                &format!("Fabric Loader 更新失败: {}", error),
            );
            if let Some(restore_error) = restore_error {
                Err(format!("{}；{}", error, restore_error))
            } else {
                Err(error)
            }
        }
    }
}

fn ensure_upgrade_not_cancelled(cancel_name: &str) -> Result<(), String> {
    if crate::instance::is_cancelled(cancel_name) {
        Err("用户取消 Fabric Loader 更新".to_string())
    } else {
        Ok(())
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
    let input = input.trim();
    if !input.is_empty() {
        return input.to_string();
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

fn remove_old_fabric_libraries(json: &mut serde_json::Value) {
    let Some(libraries) = json
        .get_mut("libraries")
        .and_then(|value| value.as_array_mut())
    else {
        return;
    };
    // Loader profile 中的库统一由 Fabric Maven 或 net.fabricmc 坐标提供，新 profile 会完整补回。
    libraries.retain(|library| !is_fabric_loader_library(library));
}

fn is_fabric_loader_library(library: &serde_json::Value) -> bool {
    let name = library
        .get("name")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if name.starts_with("net.fabricmc:") || name.starts_with("net.legacyfabric:") {
        return true;
    }

    let uses_fabric_maven = |value: Option<&serde_json::Value>| {
        value
            .and_then(|value| value.as_str())
            .map(|url| url.to_ascii_lowercase())
            .is_some_and(|url| {
                url.contains("maven.fabricmc.net") || url.contains("maven.legacyfabric.net")
            })
    };
    if uses_fabric_maven(library.get("url"))
        || uses_fabric_maven(library.pointer("/downloads/artifact/url"))
    {
        return true;
    }
    library
        .pointer("/downloads/classifiers")
        .and_then(|value| value.as_object())
        .is_some_and(|classifiers| {
            classifiers
                .values()
                .any(|value| uses_fabric_maven(value.get("url")))
        })
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
    let rollback_path = json_path.with_extension("json.oaoi_previous");
    let _ = fs::remove_file(&tmp_path);
    let _ = fs::remove_file(&rollback_path);
    let data =
        serde_json::to_string_pretty(json).map_err(|e| format!("序列化版本配置失败: {}", e))?;
    let mut tmp =
        fs::File::create(&tmp_path).map_err(|e| format!("写入临时版本配置失败: {}", e))?;
    tmp.write_all(data.as_bytes())
        .and_then(|_| tmp.sync_all())
        .map_err(|e| format!("写入临时版本配置失败: {}", e))?;
    drop(tmp);

    if let Err(error) = atomic_replace_file(json_path, &tmp_path, &rollback_path) {
        let restore_result = restore_backup(backup_json, json_path);
        let _ = fs::remove_file(&tmp_path);
        return match restore_result {
            Ok(()) => Err(format!("写入新版版本配置失败: {}", error)),
            Err(restore_error) => Err(format!(
                "写入新版版本配置失败: {}；{}",
                error, restore_error
            )),
        };
    }

    let _ = fs::remove_file(&rollback_path);
    Ok(())
}

fn restore_backup(backup_json: &Path, json_path: &Path) -> Result<(), String> {
    if !backup_json.exists() {
        return Ok(());
    }
    let restore_tmp = json_path.with_extension("json.oaoi_restore");
    let restore_previous = json_path.with_extension("json.oaoi_restore_previous");
    let _ = fs::remove_file(&restore_tmp);
    let _ = fs::remove_file(&restore_previous);
    fs::copy(backup_json, &restore_tmp).map_err(|e| format!("准备 Fabric 更新备份失败: {}", e))?;
    fs::OpenOptions::new()
        .write(true)
        .open(&restore_tmp)
        .and_then(|file| file.sync_all())
        .map_err(|e| format!("同步 Fabric 更新备份失败: {}", e))?;
    atomic_replace_file(json_path, &restore_tmp, &restore_previous)
        .map_err(|e| format!("恢复 Fabric 更新备份失败: {}", e))?;
    let _ = fs::remove_file(&restore_previous);
    Ok(())
}

fn temp_json_path(json_path: &Path) -> PathBuf {
    let file_name = json_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("version.json");
    json_path.with_file_name(format!("{}.oaoi_tmp", file_name))
}

#[cfg(windows)]
fn atomic_replace_file(target: &Path, replacement: &Path, backup: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;

    extern "system" {
        fn ReplaceFileW(
            replaced_file_name: *const u16,
            replacement_file_name: *const u16,
            backup_file_name: *const u16,
            replace_flags: u32,
            exclude: *mut std::ffi::c_void,
            reserved: *mut std::ffi::c_void,
        ) -> i32;
    }

    if !target.exists() {
        return fs::rename(replacement, target).map_err(|e| e.to_string());
    }

    let target_wide: Vec<u16> = target
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let replacement_wide: Vec<u16> = replacement
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let backup_wide: Vec<u16> = backup
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let result = unsafe {
        ReplaceFileW(
            target_wide.as_ptr(),
            replacement_wide.as_ptr(),
            backup_wide.as_ptr(),
            1,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if result == 0 {
        Err(std::io::Error::last_os_error().to_string())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn atomic_replace_file(target: &Path, replacement: &Path, backup: &Path) -> Result<(), String> {
    if !target.exists() {
        return fs::rename(replacement, target).map_err(|e| e.to_string());
    }
    fs::rename(target, backup).map_err(|e| e.to_string())?;
    if let Err(error) = fs::rename(replacement, target) {
        let _ = fs::rename(backup, target);
        return Err(error.to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::remove_old_fabric_libraries;

    #[test]
    fn fabric更新移除旧_loader_依赖并保留原版库() {
        let mut json = serde_json::json!({
            "libraries": [
                {"name": "com.mojang:logging:1.2.7", "url": "https://libraries.minecraft.net/"},
                {"name": "net.fabricmc:fabric-loader:0.15.0", "url": "https://maven.fabricmc.net/"},
                {"name": "org.ow2.asm:asm:9.5", "url": "https://maven.fabricmc.net/"},
                {"name": "example:custom:1.0", "downloads": {"artifact": {"url": "https://example.com/custom.jar"}}}
            ]
        });

        remove_old_fabric_libraries(&mut json);

        let names = json["libraries"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|library| library["name"].as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, ["com.mojang:logging:1.2.7", "example:custom:1.0"]);
    }
}
