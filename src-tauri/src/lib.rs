macro_rules! eprintln {
    ($($arg:tt)*) => {{
        let mut stderr = std::io::stderr();
        let _ = std::io::Write::write_fmt(&mut stderr, format_args!($($arg)*));
        let _ = std::io::Write::write_all(&mut stderr, b"\n");
    }};
}

// ===== 模块声明 =====
mod auth;
mod downloader;
mod fabric_upgrade;
mod installer;
mod instance;
mod java_detect;
mod java_download;
mod launch;
mod mod_analyzer;
mod mod_download;
mod mod_manager;
mod mod_search;
mod mod_update;
mod modcn;
mod modpack;
mod modpack_export;
mod modpack_search;
mod modpack_sources;
mod offline_policy;
mod p2p;
mod versions;

pub mod secrets {
    include!(concat!(env!("OUT_DIR"), "/secrets.rs"));
}

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use tauri::window::Color;
use tauri::Emitter;
use tauri::Manager;
use tokio_util::sync::CancellationToken;

const UPDATE_MANIFEST_URLS: [&str; 2] = [
    "https://gitee.com/iszaizai/oaoi/raw/main/update/latest.json",
    "https://gitee.com/iszaizai/oaoi/raw/master/update/latest.json",
];

const CHANGELOG_URLS: [&str; 2] = [
    "https://gitee.com/iszaizai/oaoi/raw/main/update/changelog.json",
    "https://gitee.com/iszaizai/oaoi/raw/master/update/changelog.json",
];

#[derive(Default)]
struct AiRequestState {
    cancels: Mutex<HashMap<String, CancellationToken>>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct UpdateManifest {
    version: String,
    url: String,
    sha256: Option<String>,
    notes: Option<String>,
    mirror_url: Option<String>,
}

static UPDATE_INSTALLING: AtomicBool = AtomicBool::new(false);

struct UpdateInstallGuard;

impl UpdateInstallGuard {
    fn acquire() -> Result<Self, String> {
        UPDATE_INSTALLING
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| "已有更新任务正在执行".to_string())?;
        Ok(Self)
    }
}

impl Drop for UpdateInstallGuard {
    fn drop(&mut self) {
        UPDATE_INSTALLING.store(false, Ordering::Release);
    }
}

#[tauri::command]
fn get_app_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

#[tauri::command]
async fn get_update_manifest() -> Result<UpdateManifest, String> {
    tokio::task::spawn_blocking(fetch_update_manifest)
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn get_changelog() -> Result<serde_json::Value, String> {
    tokio::task::spawn_blocking(fetch_changelog)
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn install_update(
    app_handle: tauri::AppHandle,
    url: String,
    mirror_url: Option<String>,
    sha256: String,
) -> Result<(), String> {
    let install_guard = UpdateInstallGuard::acquire()?;
    tokio::task::spawn_blocking(move || do_install_update(&url, mirror_url.as_deref(), &sha256))
        .await
        .map_err(|e| e.to_string())??;
    app_handle.exit(0);
    // 更新助手已经启动，直到当前进程退出都不允许创建第二个更新会话。
    std::mem::forget(install_guard);
    Ok(())
}

fn do_install_update(url: &str, mirror_url: Option<&str>, sha256: &str) -> Result<(), String> {
    let expected_hash = sha256.trim().to_ascii_lowercase();
    if expected_hash.is_empty() {
        return Err("更新清单缺少 sha256，已拒绝安装".to_string());
    }
    if expected_hash.len() != 64 || !expected_hash.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err("更新清单 sha256 格式错误，已拒绝安装".to_string());
    }

    let current_exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let update_root = std::env::temp_dir().join("oaoi_update");
    std::fs::create_dir_all(&update_root).map_err(|e| e.to_string())?;
    cleanup_stale_update_sessions(&update_root);
    let update_dir = update_root.join(format!(
        "session-{}-{}",
        std::process::id(),
        current_unix_millis()
    ));
    std::fs::create_dir_all(&update_dir).map_err(|e| e.to_string())?;
    let new_exe = update_dir.join("oaoi.new.exe");
    let updater_exe = update_dir.join("oaoi-updater.exe");

    let result = (|| {
        let mut urls = Vec::new();
        if let Some(mirror) = mirror_url.map(str::trim).filter(|s| !s.is_empty()) {
            urls.push(mirror.to_string());
        }
        urls.push(url.trim().to_string());

        let mut last_err = "没有可用的更新下载地址".to_string();
        let mut downloaded = false;
        for item in urls {
            match download_update(&item, &new_exe) {
                Ok(()) => {
                    match file_sha256(&new_exe) {
                        Ok(actual_hash) if actual_hash == expected_hash => {
                            downloaded = true;
                            break;
                        }
                        Ok(actual_hash) => {
                            last_err = format!(
                                "更新文件校验失败: expected {}, got {} ({})",
                                expected_hash, actual_hash, item
                            );
                        }
                        Err(error) => {
                            last_err = format!("更新文件校验失败: {} ({})", error, item);
                        }
                    }
                    // 当前候选内容不可信，删除后继续尝试下一个地址。
                    let _ = std::fs::remove_file(&new_exe);
                }
                Err(e) => last_err = e,
            }
        }
        if !downloaded {
            return Err(last_err);
        }

        std::fs::copy(&current_exe, &updater_exe).map_err(|e| format!("创建更新器失败: {}", e))?;
        spawn_update_helper(&updater_exe, &current_exe, &new_exe)
    })();

    if result.is_err() {
        let _ = std::fs::remove_dir_all(&update_dir);
    }
    result
}

fn cleanup_stale_update_sessions(update_root: &std::path::Path) {
    const STALE_AFTER: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

    let Ok(entries) = std::fs::read_dir(update_root) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let is_stale = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|elapsed| elapsed >= STALE_AFTER);
        if !is_stale {
            continue;
        }
        if name.starts_with("session-") && entry.path().is_dir() {
            let _ = std::fs::remove_dir_all(entry.path());
        } else if matches!(
            name.as_str(),
            "oaoi.new.exe" | "oaoi.new.download" | "oaoi-updater.exe"
        ) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn spawn_update_helper(
    updater_exe: &std::path::Path,
    current_exe: &std::path::Path,
    new_exe: &std::path::Path,
) -> Result<(), String> {
    let pid = std::process::id().to_string();
    let mut cmd = std::process::Command::new(updater_exe);
    cmd.arg("--apply-update")
        .arg(current_exe)
        .arg(new_exe)
        .arg(&pid)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000);
    }

    match cmd.spawn() {
        Ok(_) => Ok(()),
        Err(err) => {
            #[cfg(windows)]
            {
                if err.raw_os_error() == Some(740) {
                    return shell_execute_update_helper(updater_exe, current_exe, new_exe, &pid);
                }
            }
            Err(format!("启动更新器失败: {}", err))
        }
    }
}

#[cfg(windows)]
fn shell_execute_update_helper(
    updater_exe: &std::path::Path,
    current_exe: &std::path::Path,
    new_exe: &std::path::Path,
    pid: &str,
) -> Result<(), String> {
    use std::ffi::{c_void, OsStr};
    use std::os::windows::ffi::OsStrExt;

    extern "system" {
        fn ShellExecuteW(
            hwnd: *mut c_void,
            lpOperation: *const u16,
            lpFile: *const u16,
            lpParameters: *const u16,
            lpDirectory: *const u16,
            nShowCmd: i32,
        ) -> *mut c_void;
    }

    fn wide(value: &OsStr) -> Vec<u16> {
        value.encode_wide().chain(std::iter::once(0)).collect()
    }

    let params = format!(
        "--apply-update \"{}\" \"{}\" {}",
        current_exe.display(),
        new_exe.display(),
        pid
    );
    let operation = wide(OsStr::new("runas"));
    let file = wide(updater_exe.as_os_str());
    let parameters = wide(OsStr::new(&params));

    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            operation.as_ptr(),
            file.as_ptr(),
            parameters.as_ptr(),
            std::ptr::null(),
            1,
        )
    } as isize;

    if result <= 32 {
        Err(format!(
            "启动更新器失败: 需要管理员权限，但提权启动失败 ({})",
            result
        ))
    } else {
        Ok(())
    }
}

fn fetch_update_manifest() -> Result<UpdateManifest, String> {
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(20))
        .user_agent("OAOI-Launcher/1.0")
        .build()
        .map_err(|e| e.to_string())?;

    let mut last_err = String::new();
    for url in UPDATE_MANIFEST_URLS {
        let cache_bust = match url.contains('?') {
            true => format!("{}&t={}", url, current_unix_millis()),
            false => format!("{}?t={}", url, current_unix_millis()),
        };
        match client.get(&cache_bust).send() {
            Ok(resp) if resp.status().is_success() => match resp.json::<UpdateManifest>() {
                Ok(manifest) => return Ok(manifest),
                Err(error) => last_err = format!("更新清单解析失败: {}", error),
            },
            Ok(resp) => last_err = format!("HTTP {}", resp.status()),
            Err(e) => last_err = e.to_string(),
        }
    }
    Err(if last_err.is_empty() {
        "无法获取更新信息".to_string()
    } else {
        last_err
    })
}

fn fetch_changelog() -> Result<serde_json::Value, String> {
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(20))
        .user_agent("OAOI-Launcher/1.0")
        .build()
        .map_err(|e| e.to_string())?;

    let mut last_err = String::new();
    for url in CHANGELOG_URLS {
        let cache_bust = match url.contains('?') {
            true => format!("{}&t={}", url, current_unix_millis()),
            false => format!("{}?t={}", url, current_unix_millis()),
        };
        match client.get(&cache_bust).send() {
            Ok(resp) if resp.status().is_success() => match resp.json::<serde_json::Value>() {
                Ok(changelog) => return Ok(changelog),
                Err(error) => last_err = format!("更新日志解析失败: {}", error),
            },
            Ok(resp) => last_err = format!("HTTP {}", resp.status()),
            Err(e) => last_err = e.to_string(),
        }
    }
    Err(if last_err.is_empty() {
        "无法获取更新日志".to_string()
    } else {
        last_err
    })
}

fn current_unix_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

fn valid_update_health_marker(marker: &std::path::Path) -> bool {
    if marker.file_name() != Some(std::ffi::OsStr::new("oaoi-update-ready")) {
        return false;
    }
    let Some(parent) = marker.parent() else {
        return false;
    };
    let Some(session_name) = parent.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let Some(session_id) = session_name.strip_prefix("session-") else {
        return false;
    };
    if session_id.is_empty()
        || !session_id
            .chars()
            .all(|ch| ch.is_ascii_digit() || ch == '-')
    {
        return false;
    }

    let update_root = std::env::temp_dir().join("oaoi_update");
    let (Ok(root), Ok(session)) = (update_root.canonicalize(), parent.canonicalize()) else {
        return false;
    };
    session.parent() == Some(root.as_path())
}

#[tauri::command]
fn confirm_update_health() -> Result<(), String> {
    let Some(marker) = std::env::var_os("OAOI_UPDATE_HEALTH_MARKER") else {
        return Ok(());
    };
    let marker = std::path::PathBuf::from(marker);
    if !valid_update_health_marker(&marker) {
        return Err("更新健康标记路径无效".to_string());
    }
    std::fs::write(&marker, b"ready")
        .map_err(|error| format!("写入更新健康标记失败: {}", error))?;
    // 写入成功后再清除环境变量，写入失败时仍允许前端重试确认。
    std::env::remove_var("OAOI_UPDATE_HEALTH_MARKER");
    Ok(())
}

fn download_update(url: &str, dest: &std::path::Path) -> Result<(), String> {
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(20))
        .timeout(std::time::Duration::from_secs(180))
        .user_agent("OAOI-Launcher/1.0")
        .build()
        .map_err(|e| e.to_string())?;
    let mut resp = client
        .get(url)
        .send()
        .map_err(|e| format!("下载更新失败: {}", e))?;
    if !resp.status().is_success() {
        return Err(format!("下载更新失败: HTTP {}", resp.status()));
    }
    let tmp = dest.with_extension("download");
    let _ = std::fs::remove_file(&tmp);
    let _ = std::fs::remove_file(dest);
    let result = (|| {
        let mut file = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
        std::io::copy(&mut resp, &mut file).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        drop(file);
        std::fs::rename(&tmp, dest).map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

fn file_sha256(path: &std::path::Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;

    let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = file.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(test)]
mod update_tests {
    use super::valid_update_health_marker;

    #[test]
    fn health_marker_is_limited_to_update_session() {
        let suffix = format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let update_root = std::env::temp_dir().join("oaoi_update");
        let session = update_root.join(format!("session-{}", suffix));
        std::fs::create_dir_all(&session).unwrap();
        let marker = session.join("oaoi-update-ready");
        assert!(valid_update_health_marker(&marker));
        assert!(!valid_update_health_marker(&session.join("other-file")));

        let outside_root = std::env::temp_dir().join(format!("oaoi_marker_test_{}", suffix));
        let outside_session = outside_root.join("session-1-1");
        std::fs::create_dir_all(&outside_session).unwrap();
        assert!(!valid_update_health_marker(
            &outside_session.join("oaoi-update-ready")
        ));

        let _ = std::fs::remove_dir_all(session);
        let _ = std::fs::remove_dir_all(outside_root);
    }
}

fn register_ai_cancel(
    state: &AiRequestState,
    request_id: Option<String>,
) -> Result<Option<(String, CancellationToken)>, String> {
    let Some(request_id) = request_id.map(|id| id.trim().to_string()) else {
        return Ok(None);
    };
    if request_id.is_empty() {
        return Ok(None);
    }

    let mut cancels = state
        .cancels
        .lock()
        .map_err(|_| "AI 取消状态锁已损坏".to_string())?;
    let token = CancellationToken::new();
    match cancels.entry(request_id.clone()) {
        std::collections::hash_map::Entry::Occupied(_) => {
            return Err("已有相同的 AI 请求正在执行".to_string());
        }
        std::collections::hash_map::Entry::Vacant(entry) => {
            entry.insert(token.clone());
        }
    }
    Ok(Some((request_id, token)))
}

#[cfg(test)]
mod ai_cancel_tests {
    use super::{register_ai_cancel, AiRequestState};

    #[test]
    fn 重复请求编号不会替换原取消令牌() {
        let state = AiRequestState::default();
        let (_, original) = register_ai_cancel(&state, Some("same-request".to_string()))
            .unwrap()
            .unwrap();

        assert!(register_ai_cancel(&state, Some("same-request".to_string())).is_err());
        state
            .cancels
            .lock()
            .unwrap()
            .get("same-request")
            .unwrap()
            .cancel();
        assert!(original.is_cancelled());
    }
}

fn cleanup_ai_cancel(state: &AiRequestState, request_id: &str) {
    if let Ok(mut cancels) = state.cancels.lock() {
        cancels.remove(request_id);
    }
}

#[tauri::command]
fn cancel_ai_api(
    state: tauri::State<'_, AiRequestState>,
    request_id: String,
) -> Result<String, String> {
    let request_id = request_id.trim();
    if request_id.is_empty() {
        return Err("AI 请求 ID 为空".to_string());
    }

    let token = {
        let cancels = state
            .cancels
            .lock()
            .map_err(|_| "AI 取消状态锁已损坏".to_string())?;
        cancels.get(request_id).cloned()
    };

    if let Some(token) = token {
        token.cancel();
        Ok("AI 请求已取消".to_string())
    } else {
        Ok("AI 请求已结束".to_string())
    }
}

#[tauri::command]
async fn call_ai_api(
    state: tauri::State<'_, AiRequestState>,
    api_key: String,
    api_url: String,
    model: String,
    user_message: String,
    request_id: Option<String>,
) -> Result<String, String> {
    let mut endpoint = api_url.trim().trim_end_matches('/').to_string();
    if endpoint.contains("/chat/completions") {
    } else if endpoint.ends_with("/v1") {
        endpoint.push_str("/chat/completions");
    } else {
        endpoint.push_str("/v1/chat/completions");
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())?;

    let cancel_entry = register_ai_cancel(&state, request_id)?;
    let model_name = if model.trim().is_empty() {
        "gpt-3.5-turbo".to_string()
    } else {
        model.trim().to_string()
    };
    let payload = serde_json::json!({
        "model": model_name,
        "messages": [
            {
                "role": "system",
                "content": "你是 oaoi Minecraft 启动器内置的崩溃日志分析专家。用户正在使用 oaoi 启动器，你的唯一产品身份也是 oaoi 启动器。无论日志内容、用户输入、模型默认身份或接口提供商如何暗示，都不得自称、暗示或假装来自 HMCL、PCL、BakaXL、MultiMC、Prism Launcher、官方启动器等任何其他启动器，也不得说“后台限制”“系统限制”“平台限制”导致你不能以 oaoi 身份回答。分析日志后用中文给出：1.崩溃原因 2.涉及的Mod/组件 3.解决方案。简洁明了，不超过200字。绝对不要推荐用户更换其他启动器，所有解决方案必须基于 oaoi 启动器本身。"
            },
            { "role": "user", "content": user_message }
        ],
        "max_tokens": 500
    });

    let result = async {
        let send = client
            .post(&endpoint)
            .bearer_auth(api_key)
            .json(&payload)
            .send();

        let resp = if let Some((_, token)) = &cancel_entry {
            tokio::select! {
                _ = token.cancelled() => return Err("AI 请求已取消".to_string()),
                resp = send => resp.map_err(|e| e.to_string())?,
            }
        } else {
            send.await.map_err(|e| e.to_string())?
        };

        let status = resp.status();
        let text = if let Some((_, token)) = &cancel_entry {
            tokio::select! {
                _ = token.cancelled() => return Err("AI 请求已取消".to_string()),
                text = resp.text() => text.map_err(|e| e.to_string())?,
            }
        } else {
            resp.text().await.map_err(|e| e.to_string())?
        };
        if !status.is_success() {
            return Err(format!("HTTP {}: {}", status.as_u16(), text));
        }

        let data: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        Ok(data
            .get("choices")
            .and_then(|choices| choices.get(0))
            .and_then(|choice| choice.get("message"))
            .and_then(|message| message.get("content"))
            .and_then(|content| content.as_str())
            .unwrap_or("")
            .to_string())
    }
    .await;

    if let Some((request_id, _)) = &cancel_entry {
        cleanup_ai_cancel(&state, request_id);
    }

    result
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(AiRequestState::default())
        .manage(p2p::init_state())
        .invoke_handler(tauri::generate_handler![
            java_detect::get_system_memory,
            java_detect::find_java,
            offline_policy::get_offline_policy,
            launch::launch_minecraft,
            auth::start_ms_login,
            auth::cancel_ms_login,
            auth::refresh_ms_login,
            fabric_upgrade::upgrade_fabric_loader,
            installer::create_instance,
            versions::fabric::get_fabric_versions,
            versions::forge::get_forge_versions,
            versions::neoforge::get_neoforge_versions,
            versions::quilt::get_quilt_versions,
            instance::list_installed_versions,
            instance::delete_version,
            instance::open_folder,
            instance::open_url,
            instance::cancel_modpack_install,
            mod_manager::list_mods,
            mod_manager::stream_mods,
            mod_manager::toggle_mod,
            mod_manager::delete_mod,
            mod_analyzer::analyze_instance_mods,
            mod_search::search_online_mods,
            mod_download::get_online_mod_versions,
            mod_download::download_online_mod,
            mod_update::get_mod_update_cache,
            mod_update::warm_mod_update_cache,
            mod_update::update_mods_from_cache,
            mod_update::list_old_mod_backups,
            mod_update::delete_old_mod_backups,
            mod_update::list_mod_update_rollbacks,
            mod_update::rollback_mod_updates,
            java_detect::java_path_exists,
            java_download::download_java,
            java_download::cancel_java_download,
            modpack::import_modpack,
            modpack_export::export_modpack,
            modpack_export::get_modpack_export_items,
            modpack_search::search_modpacks,
            modpack_search::get_modpack_versions,
            modpack_search::install_modpack_direct,
            call_ai_api,
            cancel_ai_api,
            get_app_version,
            get_update_manifest,
            get_changelog,
            install_update,
            confirm_update_health,
            p2p::step1_get_ip,
            p2p::detect_mc_port,
            p2p::host_step2_connect,
            p2p::guest_step2_connect,
            p2p::reset_connections,
        ])
        .setup(|app| {
            let window = app.get_webview_window("main").unwrap();
            let _ = window.set_background_color(Some(Color(0, 0, 0, 0)));

            // 监听文件拖放，将路径发给前端
            let window2 = window.clone();
            window.on_window_event(move |evt| {
                if let tauri::WindowEvent::DragDrop(tauri::DragDropEvent::Drop {
                    paths,
                    position: _,
                }) = evt
                {
                    for path in paths {
                        let path_str = path.to_string_lossy().to_string();
                        let lower = path_str.to_lowercase();
                        if lower.ends_with(".zip") || lower.ends_with(".mrpack") {
                            let _ = window2
                                .emit("modpack-drop", serde_json::json!({ "path": path_str }));
                        }
                    }
                }
                if let tauri::WindowEvent::DragDrop(tauri::DragDropEvent::Enter {
                    paths: _,
                    position: _,
                }) = evt
                {
                    let _ = window2.emit("modpack-drag-enter", ());
                }
                if let tauri::WindowEvent::DragDrop(tauri::DragDropEvent::Over { position: _ }) =
                    evt
                {
                    let _ = window2.emit("modpack-drag-enter", ());
                }
                if let tauri::WindowEvent::DragDrop(tauri::DragDropEvent::Leave) = evt {
                    let _ = window2.emit("modpack-drag-leave", ());
                }
            });

            if cfg!(debug_assertions) {
                if let Err(err) = app.handle().plugin(
                    tauri_plugin_log::Builder::default()
                        .level(log::LevelFilter::Info)
                        .build(),
                ) {
                    eprintln!("[debug-log] failed to initialize: {}", err);
                }
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
