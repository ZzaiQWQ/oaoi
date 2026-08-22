use crate::downloader::event::DownloadEvent;
use crate::downloader::{
    DownloadCandidate, DownloadEngineOptions, DownloadManager, DownloadRequest,
};
use crate::instance::{safe_join, scoped_task_key, unregister_cancel};
use std::collections::HashSet;
use std::io::{Read, Seek};
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex, OnceLock};
use tauri::Emitter;

const MAX_JAVA_ARCHIVE_ENTRIES: usize = 32_768;
const MAX_JAVA_ARCHIVE_FILE_SIZE: u64 = 1024 * 1024 * 1024;
const MAX_JAVA_ARCHIVE_TOTAL_SIZE: u64 = 4 * 1024 * 1024 * 1024;

#[derive(Clone, Copy)]
struct JavaArchiveLimits {
    max_entries: usize,
    max_file_size: u64,
    max_total_size: u64,
}

const JAVA_ARCHIVE_LIMITS: JavaArchiveLimits = JavaArchiveLimits {
    max_entries: MAX_JAVA_ARCHIVE_ENTRIES,
    max_file_size: MAX_JAVA_ARCHIVE_FILE_SIZE,
    max_total_size: MAX_JAVA_ARCHIVE_TOTAL_SIZE,
};

fn java_install_locks() -> &'static (Mutex<HashSet<String>>, Condvar) {
    static LOCKS: OnceLock<(Mutex<HashSet<String>>, Condvar)> = OnceLock::new();
    LOCKS.get_or_init(|| (Mutex::new(HashSet::new()), Condvar::new()))
}

struct JavaInstallGuard {
    key: String,
}

impl Drop for JavaInstallGuard {
    fn drop(&mut self) {
        let (locks, signal) = java_install_locks();
        if let Ok(mut active) = locks.lock() {
            active.remove(&self.key);
            signal.notify_all();
        }
    }
}

fn acquire_java_install(
    java_dir: &Path,
    cancel_name: Option<&str>,
) -> Result<JavaInstallGuard, String> {
    let key = java_dir
        .to_string_lossy()
        .replace('/', "\\")
        .to_ascii_lowercase();
    let (locks, signal) = java_install_locks();
    let mut active = locks.lock().map_err(|_| "Java 安装锁已损坏".to_string())?;
    while active.contains(&key) {
        if is_cancelled(cancel_name) {
            return Err("用户取消下载".to_string());
        }
        let (next, _) = signal
            .wait_timeout(active, std::time::Duration::from_millis(200))
            .map_err(|_| "Java 安装锁已损坏".to_string())?;
        active = next;
    }
    active.insert(key.clone());
    Ok(JavaInstallGuard { key })
}

fn valid_java(path: &Path, major: u32) -> bool {
    crate::java_detect::java_path_matches_major(path, major)
}

fn java_archive_root_prefix<R: Read + Seek>(
    archive: &mut zip::ZipArchive<R>,
) -> Result<PathBuf, String> {
    let mut best: Option<PathBuf> = None;
    for index in 0..archive.len() {
        let entry = archive
            .by_index(index)
            .map_err(|e| format!("读取 Java 压缩包条目失败: {}", e))?;
        let path = entry
            .enclosed_name()
            .ok_or_else(|| format!("Java 压缩包包含非法路径: {}", entry.name()))?;
        let components = path.components().collect::<Vec<_>>();
        if components.len() < 2 {
            continue;
        }
        let bin = components[components.len() - 2]
            .as_os_str()
            .to_string_lossy();
        let executable = components[components.len() - 1]
            .as_os_str()
            .to_string_lossy();
        if !bin.eq_ignore_ascii_case("bin") || !executable.eq_ignore_ascii_case("java.exe") {
            continue;
        }
        let root = components[..components.len() - 2]
            .iter()
            .collect::<PathBuf>();
        if best
            .as_ref()
            .is_none_or(|current| root.components().count() < current.components().count())
        {
            best = Some(root);
        }
    }
    best.ok_or_else(|| "Java 压缩包中找不到 bin/java.exe".to_string())
}

fn extract_java_archive<R: Read + Seek>(
    archive: &mut zip::ZipArchive<R>,
    destination: &Path,
    cancel_name: Option<&str>,
) -> Result<(), String> {
    validate_java_archive_limits(archive, JAVA_ARCHIVE_LIMITS)?;
    let root_prefix = java_archive_root_prefix(archive)?;
    let mut extracted_total = 0u64;
    for index in 0..archive.len() {
        if is_cancelled(cancel_name) {
            return Err("用户取消下载".to_string());
        }
        let entry = archive
            .by_index(index)
            .map_err(|e| format!("读取 Java 压缩包条目失败: {}", e))?;
        let path = entry
            .enclosed_name()
            .ok_or_else(|| format!("Java 压缩包包含非法路径: {}", entry.name()))?;
        let relative = if root_prefix.as_os_str().is_empty() {
            path
        } else {
            match path.strip_prefix(&root_prefix) {
                Ok(path) => path.to_path_buf(),
                Err(_) => continue,
            }
        };
        if relative.as_os_str().is_empty() {
            continue;
        }

        let out_path = safe_join(destination, &relative.to_string_lossy())?;
        if entry.is_dir() {
            std::fs::create_dir_all(&out_path).map_err(|e| e.to_string())?;
        } else {
            if let Some(parent) = out_path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let mut out = std::fs::File::create(&out_path).map_err(|e| e.to_string())?;
            let remaining_total = JAVA_ARCHIVE_LIMITS
                .max_total_size
                .saturating_sub(extracted_total);
            let allowed = JAVA_ARCHIVE_LIMITS.max_file_size.min(remaining_total);
            let mut limited = entry.take(allowed.saturating_add(1));
            let copied = std::io::copy(&mut limited, &mut out).map_err(|e| e.to_string())?;
            if copied > allowed {
                drop(out);
                let _ = std::fs::remove_file(&out_path);
                return Err(format!(
                    "Java 压缩包展开体积超过限制: {}",
                    relative.display()
                ));
            }
            extracted_total = extracted_total
                .checked_add(copied)
                .ok_or_else(|| "Java 压缩包总展开体积溢出".to_string())?;
        }
    }
    Ok(())
}

fn validate_java_archive_limits<R: Read + Seek>(
    archive: &mut zip::ZipArchive<R>,
    limits: JavaArchiveLimits,
) -> Result<(), String> {
    if archive.len() > limits.max_entries {
        return Err(format!(
            "Java 压缩包条目过多: {}，上限 {}",
            archive.len(),
            limits.max_entries
        ));
    }

    let mut total = 0u64;
    for index in 0..archive.len() {
        let entry = archive
            .by_index(index)
            .map_err(|e| format!("读取 Java 压缩包条目失败: {}", e))?;
        if entry.is_dir() {
            continue;
        }
        let size = entry.size();
        if size > limits.max_file_size {
            return Err(format!(
                "Java 压缩包单个文件过大: {} ({} bytes)",
                entry.name(),
                size
            ));
        }
        total = total
            .checked_add(size)
            .ok_or_else(|| "Java 压缩包总展开体积溢出".to_string())?;
        if total > limits.max_total_size {
            return Err(format!(
                "Java 压缩包总展开体积过大: {} bytes，上限 {} bytes",
                total, limits.max_total_size
            ));
        }
    }
    Ok(())
}

fn java_backup_dir(java_dir: &Path, major: u32) -> PathBuf {
    java_dir.with_file_name(format!(".jre-{}-backup", major))
}

fn rename_java_dir_with_retry(source: &Path, target: &Path) -> Result<(), String> {
    let mut last_error = None;
    for _ in 0..20 {
        match std::fs::rename(source, target) {
            Ok(()) => return Ok(()),
            Err(error) => last_error = Some(error),
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    Err(last_error
        .map(|error| error.to_string())
        .unwrap_or_else(|| "未知目录替换错误".to_string()))
}

fn recover_java_backup(java_dir: &Path, major: u32) -> Result<(), String> {
    let backup_dir = java_backup_dir(java_dir, major);
    if !backup_dir.exists() {
        return Ok(());
    }
    if java_dir.exists() {
        std::fs::remove_dir_all(&backup_dir)
            .map_err(|error| format!("清理旧 Java 备份失败: {}", error))?;
        return Ok(());
    }
    rename_java_dir_with_retry(&backup_dir, java_dir)
        .map_err(|error| format!("恢复上次中断的 Java {} 失败: {}", major, error))
}

struct StagedJavaInstall {
    path: PathBuf,
    committed: bool,
}

impl StagedJavaInstall {
    fn create(java_base: &Path, major: u32) -> Result<Self, String> {
        std::fs::create_dir_all(java_base).map_err(|e| e.to_string())?;
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        for attempt in 0..100u32 {
            let path = java_base.join(format!(
                ".jre-{}-install-{}-{}-{}",
                major,
                std::process::id(),
                nonce,
                attempt
            ));
            match std::fs::create_dir(&path) {
                Ok(()) => {
                    return Ok(Self {
                        path,
                        committed: false,
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(format!("创建 Java 临时目录失败: {}", error)),
            }
        }
        Err("无法创建唯一的 Java 临时目录".to_string())
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn commit(
        mut self,
        java_dir: &Path,
        relative_exe: &Path,
        major: u32,
        cancel_name: Option<&str>,
    ) -> Result<PathBuf, String> {
        if is_cancelled(cancel_name) {
            return Err("用户取消下载".to_string());
        }
        let staged_exe = self.path.join(relative_exe);
        if !valid_java(&staged_exe, major) {
            return Err(format!("下载的 Java 主版本不是 {}", major));
        }
        if is_cancelled(cancel_name) {
            return Err("用户取消下载".to_string());
        }

        recover_java_backup(java_dir, major)?;
        if is_cancelled(cancel_name) {
            return Err("用户取消下载".to_string());
        }
        let backup_dir = java_backup_dir(java_dir, major);
        let had_existing = java_dir.exists();
        if had_existing {
            rename_java_dir_with_retry(java_dir, &backup_dir)
                .map_err(|e| format!("备份现有 Java {} 失败: {}", major, e))?;
        }

        if let Err(error) = rename_java_dir_with_retry(&self.path, java_dir) {
            if had_existing {
                return match rename_java_dir_with_retry(&backup_dir, java_dir) {
                    Ok(()) => Err(format!("替换 Java {} 失败，已恢复旧版本: {}", major, error)),
                    Err(restore_error) => Err(format!(
                        "替换 Java {} 失败: {}；旧版本位于 {}，恢复失败: {}",
                        major,
                        error,
                        backup_dir.display(),
                        restore_error
                    )),
                };
            }
            return Err(format!("安装 Java {} 失败: {}", major, error));
        }

        self.committed = true;
        if had_existing {
            if let Err(error) = std::fs::remove_dir_all(&backup_dir) {
                eprintln!(
                    "[java] 清理旧 Java 备份失败 {}: {}",
                    backup_dir.display(),
                    error
                );
            }
        }
        Ok(java_dir.join(relative_exe))
    }
}

impl Drop for StagedJavaInstall {
    fn drop(&mut self) {
        if !self.committed {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

/// 根据 Java 大版本号自动下载对应 JRE（Adoptium）
/// 安装到 {game_dir}/runtime/jre-{major}/
#[tauri::command]
pub fn download_java(
    app_handle: tauri::AppHandle,
    major: u32,
    game_dir: String,
) -> Result<String, String> {
    // 快速检查：已存在直接返回
    let java_exe = std::path::PathBuf::from(&game_dir)
        .join("runtime")
        .join(format!("jre-{}", major))
        .join("bin")
        .join("java.exe");
    if valid_java(&java_exe, major) {
        eprintln!("[java] Java {} 已存在: {}", major, java_exe.display());
        return Ok(java_exe.to_string_lossy().to_string());
    }

    if major == 7 {
        return Err(manual_java7_message());
    }

    let cancel_display_name = format!("java-{}", major);
    let cancel_name = scoped_task_key(&game_dir, &cancel_display_name);
    let cancel_flag = crate::instance::try_register_cancel(&cancel_name)?;
    // 后台线程下载，不阻塞 UI
    std::thread::spawn(move || {
        match do_download_java(&app_handle, major, &game_dir, Some(&cancel_name)) {
            Ok(path) => {
                unregister_cancel(&cancel_name);
                let _ = app_handle.emit(
                    "java-download-done",
                    serde_json::json!({
                        "major": major, "gameDir": game_dir, "success": true, "path": path
                    }),
                );
            }
            Err(e) => {
                let was_cancelled = cancel_flag.load(std::sync::atomic::Ordering::Relaxed);
                unregister_cancel(&cancel_name);
                eprintln!("[java] 下载失败: {}", e);
                let _ = app_handle.emit(
                    "java-download-done",
                    serde_json::json!({
                        "major": major, "gameDir": game_dir, "success": false, "cancelled": was_cancelled, "error": e
                    }),
                );
            }
        }
    });
    // 立即返回，前端通过事件监听结果
    Ok("downloading".to_string())
}

#[tauri::command]
pub fn cancel_java_download(major: u32, game_dir: Option<String>) -> Result<String, String> {
    let display_name = format!("java-{}", major);
    let task_key = game_dir
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(|game_dir| scoped_task_key(game_dir, &display_name))
        .unwrap_or(display_name);
    crate::instance::cancel_install_task(&task_key)
}

fn manual_java7_message() -> String {
    "这个版本需要 Java 7，请先手动安装 Java 7，或在版本设置里选择 Java 7 的 java.exe".to_string()
}

fn do_download_java(
    app_handle: &tauri::AppHandle,
    major: u32,
    game_dir: &str,
    cancel_name: Option<&str>,
) -> Result<String, String> {
    let java_base = std::path::PathBuf::from(game_dir).join("runtime");
    let java_dir = java_base.join(format!("jre-{}", major));
    let java_exe = java_dir.join("bin").join("java.exe");
    let _install_guard = acquire_java_install(&java_dir, cancel_name)?;
    recover_java_backup(&java_dir, major)?;

    // 等待其他任务结束后再次检查，避免重复下载同一个运行库。
    if valid_java(&java_exe, major) {
        eprintln!("[java] Java {} 已存在: {}", major, java_exe.display());
        return Ok(java_exe.to_string_lossy().to_string());
    }
    if is_cancelled(cancel_name) {
        return Err("用户取消下载".to_string());
    }

    eprintln!("[java] 开始下载 Java {} ...", major);
    let _ = app_handle.emit(
        "java-download-progress",
        serde_json::json!({
            "major": major, "gameDir": game_dir, "stage": "downloading", "detail": format!("正在下载 Java {} ...", major)
        }),
    );

    // Adoptium 官方 + 清华镜像源
    let official_url = format!(
        "https://api.adoptium.net/v3/binary/latest/{}/ga/windows/x64/jre/hotspot/normal/eclipse",
        major
    );
    let mirror_url_base = format!(
        "https://mirrors.tuna.tsinghua.edu.cn/Adoptium/{}/jre/x64/windows/",
        major
    );

    let staged = StagedJavaInstall::create(&java_base, major)?;
    let tmp_zip = staged.path().join("java.zip");
    let mirror_url = resolve_tuna_java_url(&mirror_url_base, major, cancel_name).ok();
    let dl_size = download_java_archive(
        mirror_url.as_deref(),
        &official_url,
        &tmp_zip,
        major,
        game_dir,
        cancel_name,
        Some(app_handle.clone()),
    )?;
    eprintln!(
        "[java] 下载完成 ({:.1} MB)，解压中...",
        dl_size as f64 / 1048576.0
    );

    let _ = app_handle.emit(
        "java-download-progress",
        serde_json::json!({
            "major": major, "gameDir": game_dir, "stage": "extracting", "detail": "正在解压..."
        }),
    );

    // 解压 zip
    let zip_file = std::fs::File::open(&tmp_zip).map_err(|e| e.to_string())?;
    let mut archive =
        zip::ZipArchive::new(zip_file).map_err(|e| format!("打开 zip 失败: {}", e))?;

    extract_java_archive(&mut archive, staged.path(), cancel_name)?;

    drop(archive);
    let _ = std::fs::remove_file(&tmp_zip);

    let relative_exe = Path::new("bin").join("java.exe");
    let staged_exe = staged.path().join(&relative_exe);
    if !staged_exe.is_file() {
        return Err(format!("解压后找不到 java.exe: {}", staged_exe.display()));
    }
    let installed_exe = staged.commit(&java_dir, &relative_exe, major, cancel_name)?;
    eprintln!(
        "[java] Java {} 安装完成: {}",
        major,
        installed_exe.display()
    );
    let _ = app_handle.emit(
        "java-download-progress",
        serde_json::json!({
            "major": major, "gameDir": game_dir, "stage": "done", "detail": format!("Java {} 安装完成", major)
        }),
    );
    Ok(installed_exe.to_string_lossy().to_string())
}

pub fn download_java_sync_cancelable(
    major: u32,
    game_dir: &str,
    cancel_name: Option<&str>,
) -> Result<String, String> {
    let java_exe = std::path::PathBuf::from(game_dir)
        .join("runtime")
        .join(format!("jre-{}", major))
        .join("bin")
        .join("java.exe");
    if valid_java(&java_exe, major) {
        return Ok(java_exe.to_string_lossy().to_string());
    }
    if is_cancelled(cancel_name) {
        return Err("用户取消下载".to_string());
    }
    if major == 7 {
        return Err(manual_java7_message());
    }

    let java_dir = std::path::PathBuf::from(game_dir)
        .join("runtime")
        .join(format!("jre-{}", major));
    let _install_guard = acquire_java_install(&java_dir, cancel_name)?;
    recover_java_backup(&java_dir, major)?;
    if valid_java(&java_exe, major) {
        return Ok(java_exe.to_string_lossy().to_string());
    }
    let java_base = java_dir
        .parent()
        .ok_or_else(|| "Java 运行库目录无效".to_string())?;
    let staged = StagedJavaInstall::create(java_base, major)?;

    let official = format!(
        "https://api.adoptium.net/v3/binary/latest/{}/ga/windows/x64/jre/hotspot/normal/eclipse",
        major
    );
    let mirror_base = format!(
        "https://mirrors.tuna.tsinghua.edu.cn/Adoptium/{}/jre/x64/windows/",
        major
    );
    let zip_path = staged.path().join("java.zip");
    let mirror_url = resolve_tuna_java_url(&mirror_base, major, cancel_name).ok();
    download_java_archive(
        mirror_url.as_deref(),
        &official,
        &zip_path,
        major,
        game_dir,
        cancel_name,
        None,
    )?;

    let file = std::fs::File::open(&zip_path).map_err(|e| e.to_string())?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;

    extract_java_archive(&mut archive, staged.path(), cancel_name)?;
    drop(archive);
    let _ = std::fs::remove_file(&zip_path);

    let found = staged.path().join("bin").join("java.exe");
    if !found.is_file() {
        return Err("解压后未找到 java.exe".to_string());
    }
    if !valid_java(&found, major) {
        return Err(format!("下载的 Java 主版本不是 {}", major));
    }
    let relative_exe = found
        .strip_prefix(staged.path())
        .map_err(|_| "Java 临时目录结构无效".to_string())?
        .to_path_buf();
    let installed_exe = staged.commit(&java_dir, &relative_exe, major, cancel_name)?;
    Ok(installed_exe.to_string_lossy().to_string())
}

fn is_cancelled(cancel_name: Option<&str>) -> bool {
    cancel_name.is_some_and(crate::instance::is_cancelled)
}

fn resolve_tuna_java_url(
    mirror_url_base: &str,
    major: u32,
    cancel_name: Option<&str>,
) -> Result<String, String> {
    let http = reqwest::blocking::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(20))
        .user_agent("OAOI-Launcher/1.0")
        .build()
        .map_err(|error| format!("Java 镜像客户端创建失败: {}", error))?;
    let mirror_url_base = mirror_url_base.to_string();
    let worker_url = mirror_url_base.clone();
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result = (|| {
            let response = http
                .get(&worker_url)
                .send()
                .map_err(|error| format!("镜像源请求失败: {}", error))?;
            if !response.status().is_success() {
                return Err(format!("镜像源 HTTP {}", response.status()));
            }
            if response
                .content_length()
                .is_some_and(|size| size > 2 * 1024 * 1024)
            {
                return Err("Java 镜像目录响应过大".to_string());
            }
            let data = response
                .bytes()
                .map_err(|error| format!("读取 Java 镜像目录失败: {}", error))?;
            if data.len() > 2 * 1024 * 1024 {
                return Err("Java 镜像目录响应过大".to_string());
            }
            let listing = std::str::from_utf8(&data)
                .map_err(|error| format!("Java 镜像目录编码无效: {}", error))?;
            parse_tuna_java_listing(listing, major)
        })();
        let _ = sender.send(result);
    });

    loop {
        if is_cancelled(cancel_name) {
            return Err("用户取消下载".to_string());
        }
        match receiver.recv_timeout(std::time::Duration::from_millis(200)) {
            Ok(result) => return result.map(|name| format!("{}{}", mirror_url_base, name)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return Err("Java 镜像目录任务异常结束".to_string());
            }
        }
    }
}

fn parse_tuna_java_listing(listing: &str, major: u32) -> Result<String, String> {
    listing
        .lines()
        .filter_map(|line| {
            let start = line.find("href=\"")?;
            let rest = &line[start + 6..];
            let end = rest.find('"')?;
            let name = &rest[..end];
            (name.ends_with(".zip")
                && name.contains("jre")
                && name.contains("x64")
                && name.contains("windows"))
            .then(|| name.to_string())
        })
        .last()
        .ok_or_else(|| format!("清华镜像中找不到 Java {} JRE zip", major))
}

fn download_java_archive(
    mirror_url: Option<&str>,
    official_url: &str,
    dest: &Path,
    major: u32,
    game_dir: &str,
    cancel_name: Option<&str>,
    app_handle: Option<tauri::AppHandle>,
) -> Result<u64, String> {
    if is_cancelled(cancel_name) {
        return Err("用户取消下载".to_string());
    }
    let mut candidates = Vec::new();
    let mut source_labels = Vec::new();
    if let Some(url) = mirror_url.filter(|url| !url.trim().is_empty()) {
        candidates.push(DownloadCandidate::labeled(url, "镜像"));
        source_labels.push("镜像".to_string());
    }
    candidates.push(DownloadCandidate::labeled(official_url, "官方"));
    source_labels.push("官方".to_string());

    let mut options = DownloadEngineOptions::default();
    options.max_active_files = 1;
    options.max_connections_per_file = 16;
    let pool_name = cancel_name.unwrap_or("java-runtime");
    let pool = crate::instance::install_download_pool(pool_name, options.max_global_connections);
    let manager = DownloadManager::with_options_and_pool(options, pool)?;
    let _manager_registration =
        cancel_name.map(|name| crate::instance::register_download_manager(name, &manager));
    let request =
        DownloadRequest::new(format!("java-runtime-{}", major), candidates, dest).without_resume();
    let event_game_dir = game_dir.to_string();
    let result = manager.download_one(request, move |event| {
        let DownloadEvent::Progress(progress) = event else {
            return;
        };
        let Some(app_handle) = app_handle.as_ref() else {
            return;
        };
        let source = source_labels
            .get(progress.candidate_index)
            .map(String::as_str)
            .unwrap_or("下载源");
        let total = progress.total.unwrap_or(0);
        let detail = if total > 0 {
            let percent = (progress.downloaded as f64 / total as f64 * 100.0).round() as u64;
            format!("{}下载 Java {} {}%", source, major, percent.min(100))
        } else {
            format!(
                "{}下载 Java {} {:.1} MB",
                source,
                major,
                progress.downloaded as f64 / 1048576.0
            )
        };
        let _ = app_handle.emit(
            "java-download-progress",
            serde_json::json!({
                "major": major,
                "gameDir": event_game_dir,
                "stage": "downloading",
                "source": source,
                "downloaded": progress.downloaded,
                "total": total,
                "detail": detail
            }),
        );
    });
    match result {
        Ok(result) => Ok(result.bytes),
        Err(_) if is_cancelled(cancel_name) => Err("用户取消下载".to_string()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        java_archive_root_prefix, java_backup_dir, parse_tuna_java_listing, recover_java_backup,
        validate_java_archive_limits, JavaArchiveLimits, StagedJavaInstall,
    };
    use std::io::{Cursor, Write};

    #[test]
    fn 清华镜像目录只选择_windows_x64_jre_zip() {
        let listing = r#"
            <a href="OpenJDK17U-jdk_x64_windows_hotspot.zip">jdk</a>
            <a href="OpenJDK17U-jre_x64_linux_hotspot.tar.gz">linux</a>
            <a href="OpenJDK17U-jre_x64_windows_hotspot.zip">windows jre</a>
        "#;

        assert_eq!(
            parse_tuna_java_listing(listing, 17).unwrap(),
            "OpenJDK17U-jre_x64_windows_hotspot.zip"
        );
    }

    #[test]
    fn java_zip根目录不依赖第一个条目() {
        let cursor = Cursor::new(Vec::new());
        let mut writer = zip::ZipWriter::new(cursor);
        let options = zip::write::SimpleFileOptions::default();
        writer.start_file("README.txt", options).unwrap();
        writer.write_all(b"metadata").unwrap();
        writer
            .start_file("jdk-21.0.1/bin/java.exe", options)
            .unwrap();
        writer.write_all(b"java").unwrap();
        let cursor = writer.finish().unwrap();
        let mut archive = zip::ZipArchive::new(cursor).unwrap();

        assert_eq!(
            java_archive_root_prefix(&mut archive).unwrap(),
            std::path::PathBuf::from("jdk-21.0.1")
        );
    }

    #[test]
    fn java_zip拒绝超出条目和展开体积限制() {
        let cursor = Cursor::new(Vec::new());
        let mut writer = zip::ZipWriter::new(cursor);
        let options = zip::write::SimpleFileOptions::default();
        writer.start_file("jdk/bin/java.exe", options).unwrap();
        writer.write_all(b"java").unwrap();
        writer.start_file("jdk/lib/modules", options).unwrap();
        writer.write_all(b"modules").unwrap();
        let cursor = writer.finish().unwrap();
        let mut archive = zip::ZipArchive::new(cursor).unwrap();

        let too_many = JavaArchiveLimits {
            max_entries: 1,
            max_file_size: 32,
            max_total_size: 64,
        };
        assert!(validate_java_archive_limits(&mut archive, too_many)
            .unwrap_err()
            .contains("条目过多"));

        let file_too_large = JavaArchiveLimits {
            max_entries: 8,
            max_file_size: 3,
            max_total_size: 64,
        };
        assert!(validate_java_archive_limits(&mut archive, file_too_large)
            .unwrap_err()
            .contains("单个文件过大"));

        let total_too_large = JavaArchiveLimits {
            max_entries: 8,
            max_file_size: 16,
            max_total_size: 8,
        };
        assert!(validate_java_archive_limits(&mut archive, total_too_large)
            .unwrap_err()
            .contains("总展开体积过大"));
    }

    #[test]
    fn staged_cleanup_keeps_existing_runtime() {
        let root = std::env::temp_dir().join(format!(
            "oaoi_java_stage_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let runtime = root.join("runtime");
        let existing = runtime.join("jre-17");
        std::fs::create_dir_all(&existing).unwrap();
        std::fs::write(existing.join("keep.txt"), b"old").unwrap();

        let staged = StagedJavaInstall::create(&runtime, 17).unwrap();
        let staged_path = staged.path().to_path_buf();
        std::fs::write(staged.path().join("partial.txt"), b"partial").unwrap();
        drop(staged);

        assert!(existing.join("keep.txt").is_file());
        assert!(!staged_path.exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn interrupted_java_swap_restores_backup() {
        let root = std::env::temp_dir().join(format!(
            "oaoi_java_recovery_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let java_dir = root.join("runtime").join("jre-17");
        let backup_dir = java_backup_dir(&java_dir, 17);
        std::fs::create_dir_all(&backup_dir).unwrap();
        std::fs::write(backup_dir.join("keep.txt"), b"old").unwrap();

        recover_java_backup(&java_dir, 17).unwrap();

        assert!(java_dir.join("keep.txt").is_file());
        assert!(!backup_dir.exists());
        let _ = std::fs::remove_dir_all(root);
    }
}
