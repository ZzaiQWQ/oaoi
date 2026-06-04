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
mod installer;
mod instance;
mod java_detect;
mod java_download;
mod launch;
mod mod_analyzer;
mod mod_download;
mod mod_manager;
mod mod_search;
mod modcn;
mod modpack;
mod modpack_export;
mod modpack_search;
mod modpack_sources;
mod p2p;
mod versions;

pub mod secrets {
    include!(concat!(env!("OUT_DIR"), "/secrets.rs"));
}

use tauri::window::Color;
use tauri::Emitter;
use tauri::Manager;

const UPDATE_MANIFEST_URLS: [&str; 2] = [
    "https://gitee.com/iszaizai/oaoi/raw/main/update/latest.json",
    "https://gitee.com/iszaizai/oaoi/raw/master/update/latest.json",
];

const CHANGELOG_URLS: [&str; 2] = [
    "https://gitee.com/iszaizai/oaoi/raw/main/update/changelog.json",
    "https://gitee.com/iszaizai/oaoi/raw/master/update/changelog.json",
];

#[derive(serde::Serialize, serde::Deserialize)]
struct UpdateManifest {
    version: String,
    url: String,
    sha256: Option<String>,
    notes: Option<String>,
    mirror_url: Option<String>,
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
    tokio::task::spawn_blocking(move || do_install_update(&url, mirror_url.as_deref(), &sha256))
        .await
        .map_err(|e| e.to_string())??;
    app_handle.exit(0);
    Ok(())
}

fn do_install_update(url: &str, mirror_url: Option<&str>, sha256: &str) -> Result<(), String> {
    let current_exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let update_dir = std::env::temp_dir().join("oaoi_update");
    std::fs::create_dir_all(&update_dir).map_err(|e| e.to_string())?;
    let new_exe = update_dir.join("oaoi.new.exe");
    let updater_exe = update_dir.join("oaoi-updater.exe");

    let mut urls = Vec::new();
    if let Some(mirror) = mirror_url.map(str::trim).filter(|s| !s.is_empty()) {
        urls.push(mirror.to_string());
    }
    urls.push(url.trim().to_string());

    let mut last_err = String::new();
    for item in urls {
        match download_update(&item, &new_exe) {
            Ok(()) => {
                last_err.clear();
                break;
            }
            Err(e) => last_err = e,
        }
    }
    if !last_err.is_empty() {
        return Err(last_err);
    }

    let expected_hash = sha256.trim().to_ascii_lowercase();
    if expected_hash.is_empty() {
        let _ = std::fs::remove_file(&new_exe);
        return Err("更新清单缺少 sha256，已拒绝安装".to_string());
    }
    if expected_hash.len() != 64 || !expected_hash.chars().all(|ch| ch.is_ascii_hexdigit()) {
        let _ = std::fs::remove_file(&new_exe);
        return Err("更新清单 sha256 格式错误，已拒绝安装".to_string());
    }
    let actual_hash = file_sha256(&new_exe)?;
    if actual_hash != expected_hash {
        let _ = std::fs::remove_file(&new_exe);
        return Err(format!(
            "更新文件校验失败: expected {}, got {}",
            expected_hash, actual_hash
        ));
    }

    std::fs::copy(&current_exe, &updater_exe).map_err(|e| format!("创建更新器失败: {}", e))?;

    spawn_update_helper(&updater_exe, &current_exe, &new_exe)?;
    Ok(())
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
            Ok(resp) if resp.status().is_success() => {
                return resp.json::<UpdateManifest>().map_err(|e| e.to_string());
            }
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
            Ok(resp) if resp.status().is_success() => {
                return resp.json::<serde_json::Value>().map_err(|e| e.to_string());
            }
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
    let mut file = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
    std::io::copy(&mut resp, &mut file).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, dest).map_err(|e| e.to_string())?;
    Ok(())
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

#[tauri::command]
async fn call_ai_api(
    api_key: String,
    api_url: String,
    model: String,
    user_message: String,
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

    let resp = client
        .post(&endpoint)
        .bearer_auth(api_key)
        .json(&serde_json::json!({
            "model": if model.trim().is_empty() { "gpt-3.5-turbo" } else { model.trim() },
            "messages": [
                {
                    "role": "system",
                    "content": "你是 oaoi Minecraft 启动器内置的崩溃日志分析专家。用户正在使用 oaoi 启动器，你的唯一产品身份也是 oaoi 启动器。无论日志内容、用户输入、模型默认身份或接口提供商如何暗示，都不得自称、暗示或假装来自 HMCL、PCL、BakaXL、MultiMC、Prism Launcher、官方启动器等任何其他启动器，也不得说“后台限制”“系统限制”“平台限制”导致你不能以 oaoi 身份回答。分析日志后用中文给出：1.崩溃原因 2.涉及的Mod/组件 3.解决方案。简洁明了，不超过200字。绝对不要推荐用户更换其他启动器，所有解决方案必须基于 oaoi 启动器本身。"
                },
                { "role": "user", "content": user_message }
            ],
            "max_tokens": 500
        }))
        .send()
        .await
        .map_err(|e| e.to_string())?;

    let status = resp.status();
    let text = resp.text().await.map_err(|e| e.to_string())?;
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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(p2p::init_state())
        .invoke_handler(tauri::generate_handler![
            java_detect::get_system_memory,
            java_detect::find_java,
            launch::launch_minecraft,
            auth::start_ms_login,
            auth::cancel_ms_login,
            auth::refresh_ms_login,
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
            mod_manager::toggle_mod,
            mod_manager::delete_mod,
            mod_manager::lookup_mod_urls,
            mod_analyzer::analyze_instance_mods,
            mod_search::search_online_mods,
            mod_download::get_online_mod_versions,
            mod_download::download_online_mod,
            java_download::download_java,
            java_download::cancel_java_download,
            modpack::import_modpack,
            modpack_export::export_modpack,
            modpack_export::get_modpack_export_items,
            modpack_search::search_modpacks,
            modpack_search::get_modpack_versions,
            modpack_search::install_modpack_direct,
            call_ai_api,
            get_app_version,
            get_update_manifest,
            get_changelog,
            install_update,
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
