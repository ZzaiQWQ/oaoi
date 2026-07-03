use serde::Serialize;
use std::collections::HashSet;
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use sysinfo::System;

#[derive(Serialize, Clone)]
pub struct JavaInfo {
    pub path: String,
    pub version: String,
    pub major: u32,
}

#[tauri::command]
pub fn get_system_memory() -> u64 {
    let sys = System::new_with_specifics(
        sysinfo::RefreshKind::nothing().with_memory(sysinfo::MemoryRefreshKind::everything()),
    );
    sys.total_memory() / 1024 / 1024
}

#[tauri::command]
pub async fn find_java(game_dir: Option<String>) -> Result<Vec<JavaInfo>, String> {
    tokio::task::spawn_blocking(move || find_java_blocking(game_dir))
        .await
        .map_err(|e| format!("Java 扫描线程失败: {}", e))
}

#[tauri::command]
pub fn java_path_exists(path: String) -> bool {
    Path::new(path.trim()).is_file()
}

pub fn find_java_blocking(game_dir: Option<String>) -> Vec<JavaInfo> {
    let mut candidate_dirs = Vec::new();
    let mut seen_dirs = HashSet::new();
    let mut visited_dirs = HashSet::new();

    collect_env_java_dirs(&mut candidate_dirs, &mut seen_dirs);
    collect_registry_java_dirs(&mut candidate_dirs, &mut seen_dirs);
    collect_priority_java_dirs(
        game_dir.as_deref(),
        &mut candidate_dirs,
        &mut seen_dirs,
        &mut visited_dirs,
    );
    collect_drive_java_dirs(&mut candidate_dirs, &mut seen_dirs, &mut visited_dirs);

    let mut results = Vec::new();
    let mut checked_java = HashSet::new();
    for dir in candidate_dirs {
        let java_path = java_exe_path(&dir);
        if checked_java.insert(normalize_key(&java_path)) {
            if let Some(info) = get_java_info(&java_path) {
                results.push(info);
            }
        }
    }

    results.sort_by(|a, b| b.major.cmp(&a.major).then_with(|| a.path.cmp(&b.path)));
    results
}

fn collect_env_java_dirs(candidate_dirs: &mut Vec<PathBuf>, seen_dirs: &mut HashSet<String>) {
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            add_java_dir_if_valid(&dir, candidate_dirs, seen_dirs);
        }
    }

    if let Some(java_home) = std::env::var_os("JAVA_HOME") {
        let home = PathBuf::from(java_home);
        add_java_dir_if_valid(&home.join("bin"), candidate_dirs, seen_dirs);
        add_java_dir_if_valid(&home, candidate_dirs, seen_dirs);
    }
}

#[cfg(windows)]
fn collect_registry_java_dirs(candidate_dirs: &mut Vec<PathBuf>, seen_dirs: &mut HashSet<String>) {
    let keys = [
        "HKLM\\SOFTWARE\\JavaSoft\\Java Runtime Environment",
        "HKLM\\SOFTWARE\\JavaSoft\\JDK",
        "HKLM\\SOFTWARE\\JavaSoft\\Java Development Kit",
        "HKLM\\SOFTWARE\\WOW6432Node\\JavaSoft\\Java Runtime Environment",
        "HKLM\\SOFTWARE\\WOW6432Node\\JavaSoft\\JDK",
    ];

    for key in keys {
        let output = Command::new("reg")
            .args(["query", key, "/s", "/v", "JavaHome"])
            .creation_flags(0x08000000)
            .output();
        let Ok(output) = output else {
            continue;
        };
        let text = String::from_utf8_lossy(&output.stdout);
        for line in text.lines() {
            if let Some(home) = parse_registry_java_home(line) {
                add_java_dir_if_valid(&PathBuf::from(home).join("bin"), candidate_dirs, seen_dirs);
            }
        }
    }
}

#[cfg(not(windows))]
fn collect_registry_java_dirs(
    _candidate_dirs: &mut Vec<PathBuf>,
    _seen_dirs: &mut HashSet<String>,
) {
}

fn collect_priority_java_dirs(
    game_dir: Option<&str>,
    candidate_dirs: &mut Vec<PathBuf>,
    seen_dirs: &mut HashSet<String>,
    visited_dirs: &mut HashSet<String>,
) {
    if let Some(game_dir) = game_dir.map(str::trim).filter(|v| !v.is_empty()) {
        let game_dir = PathBuf::from(game_dir);
        search_java_folder(&game_dir, candidate_dirs, seen_dirs, visited_dirs, true);
        search_java_folder(
            &game_dir.join("runtime"),
            candidate_dirs,
            seen_dirs,
            visited_dirs,
            true,
        );
        if let Some(parent) = game_dir.parent() {
            search_java_folder(parent, candidate_dirs, seen_dirs, visited_dirs, true);
        }
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            search_java_folder(parent, candidate_dirs, seen_dirs, visited_dirs, true);
        }
    }

    if let Some(user) = user_home_dir() {
        search_java_folder(&user, candidate_dirs, seen_dirs, visited_dirs, false);
        search_java_folder(
            &user.join(".jdks"),
            candidate_dirs,
            seen_dirs,
            visited_dirs,
            true,
        );
        search_java_folder(
            &user.join(".sdkman").join("candidates").join("java"),
            candidate_dirs,
            seen_dirs,
            visited_dirs,
            true,
        );
        search_java_folder(
            &user.join(".gradle").join("jdks"),
            candidate_dirs,
            seen_dirs,
            visited_dirs,
            true,
        );
        search_java_folder(
            &user.join("AppData").join("Local").join("Programs"),
            candidate_dirs,
            seen_dirs,
            visited_dirs,
            true,
        );
    }
}

fn collect_drive_java_dirs(
    candidate_dirs: &mut Vec<PathBuf>,
    seen_dirs: &mut HashSet<String>,
    visited_dirs: &mut HashSet<String>,
) {
    for drive in local_drive_roots() {
        search_java_folder(&drive, candidate_dirs, seen_dirs, visited_dirs, false);
    }
}

fn search_java_folder(
    start: &Path,
    candidate_dirs: &mut Vec<PathBuf>,
    seen_dirs: &mut HashSet<String>,
    visited_dirs: &mut HashSet<String>,
    full_current_level: bool,
) {
    search_java_folder_inner(
        start,
        candidate_dirs,
        seen_dirs,
        visited_dirs,
        full_current_level,
    );
}

fn search_java_folder_inner(
    dir: &Path,
    candidate_dirs: &mut Vec<PathBuf>,
    seen_dirs: &mut HashSet<String>,
    visited: &mut HashSet<String>,
    full_current_level: bool,
) {
    if !visited.insert(normalize_key(dir)) || !dir.is_dir() {
        return;
    }

    add_java_dir_if_valid(dir, candidate_dirs, seen_dirs);

    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let parent_is_users = dir
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.eq_ignore_ascii_case("users"))
        .unwrap_or(false);

    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() || file_type.is_symlink() {
            continue;
        }

        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_lowercase();
        if full_current_level || parent_is_users || should_scan_java_dir(&name) {
            // 完整扫描只作用于当前层，下一层继续靠关键词控制，避免全盘无限扫。
            search_java_folder_inner(&path, candidate_dirs, seen_dirs, visited, false);
        }
    }
}

fn add_java_dir_if_valid(
    dir: &Path,
    candidate_dirs: &mut Vec<PathBuf>,
    seen_dirs: &mut HashSet<String>,
) {
    if !is_javaw_dir(dir) {
        return;
    }
    if seen_dirs.insert(normalize_key(dir)) {
        candidate_dirs.push(dir.to_path_buf());
    }
}

fn is_javaw_dir(dir: &Path) -> bool {
    javaw_exe_path(dir).exists() && java_exe_path(dir).exists()
}

fn java_exe_path(dir: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        dir.join("java.exe")
    }
    #[cfg(not(windows))]
    {
        dir.join("java")
    }
}

fn javaw_exe_path(dir: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        dir.join("javaw.exe")
    }
    #[cfg(not(windows))]
    {
        dir.join("java")
    }
}

fn should_scan_java_dir(name: &str) -> bool {
    if name == "bin" || name.parse::<f64>().is_ok() {
        return true;
    }

    const KEYWORDS: &[&str] = &[
        "java",
        "jdk",
        "jre",
        "jbr",
        "runtime",
        "env",
        "sdk",
        "candidate",
        "current",
        "software",
        "program",
        "programs",
        "users",
        "appdata",
        "local",
        "packages",
        "cache",
        "temp",
        "mc",
        "minecraft",
        ".minecraft",
        "craft",
        "game",
        "games",
        "launcher",
        "launch",
        "pcl",
        "hmcl",
        "baka",
        "zulu",
        "oracle",
        "eclipse",
        "adoptium",
        "microsoft",
        "corretto",
        "bellsoft",
        "graal",
        "hotspot",
        "forge",
        "fabric",
        "neoforge",
        "quilt",
        "mod",
        "download",
        "version",
        "versions",
        "server",
        "client",
        "世界",
        "游戏",
        "启动",
        "启动器",
        "运行",
        "环境",
        "软件",
        "整合",
        "官方",
        "原版",
        "前置",
        "服务",
        "客户",
        "新建文件夹",
        "网易",
    ];

    KEYWORDS.iter().any(|keyword| name.contains(keyword))
}

fn parse_registry_java_home(line: &str) -> Option<String> {
    let lower = line.to_ascii_lowercase();
    if !lower.contains("javahome") {
        return None;
    }
    let reg_pos = lower.find("reg_sz")?;
    let value = line[reg_pos + "reg_sz".len()..].trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

#[cfg(windows)]
fn local_drive_roots() -> Vec<PathBuf> {
    ('A'..='Z')
        .map(|drive| PathBuf::from(format!("{}:\\", drive)))
        .filter(|path| path.exists())
        .collect()
}

#[cfg(not(windows))]
fn local_drive_roots() -> Vec<PathBuf> {
    vec![PathBuf::from("/")]
}

fn user_home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

fn normalize_key(path: &Path) -> String {
    path.to_string_lossy()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_ascii_lowercase()
}

fn get_java_info(path: &Path) -> Option<JavaInfo> {
    let output = run_java_version(path, Duration::from_secs(15))?;
    let mut text = String::new();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    text.push_str(&String::from_utf8_lossy(&output.stdout));
    let version = parse_java_version(&text)?;
    let major = extract_major(&version);
    Some(JavaInfo {
        path: path.to_string_lossy().to_string(),
        version,
        major,
    })
}

fn run_java_version(path: &Path, timeout: Duration) -> Option<std::process::Output> {
    let mut command = Command::new(path);
    command
        .arg("-version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        command.creation_flags(0x08000000);
    }

    let mut child = command.spawn().ok()?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return child.wait_with_output().ok(),
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(30));
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

fn parse_java_version(output: &str) -> Option<String> {
    for line in output.lines() {
        let lower = line.to_ascii_lowercase();
        if lower.contains("version") {
            if let Some(start) = line.find('"') {
                if let Some(end) = line[start + 1..].find('"') {
                    return Some(line[start + 1..start + 1 + end].replace('_', "."));
                }
            }
        }
        if let Some(version) = lower.strip_prefix("openjdk ") {
            return version.split_whitespace().next().map(|v| v.to_string());
        }
    }
    None
}

fn extract_major(version: &str) -> u32 {
    if version.starts_with("1.") {
        return version
            .split('.')
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
    }
    version
        .split('.')
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}
