use crate::installer::{
    default_library_maven_base, download_file_if_needed_cancelable, download_file_with_progress,
    library_rules_value_allowed, maven_name_to_path, maven_name_to_path_with_classifier,
    parallel_download, safe_maven_path,
};
use crate::instance::{
    assets_dir, detect_loader, libraries_dir, load_merged_version_json,
    natives_dir as version_natives_dir, resolve_game_dir, safe_path_name, scoped_task_key,
    version_dir, version_jar_path,
};
use std::collections::{HashMap, HashSet};
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use tauri::Emitter;

#[derive(serde::Deserialize)]
pub struct LaunchOptions {
    pub java_path: String,
    pub game_dir: String,
    pub version_name: String,
    pub player_name: String,
    pub memory_mb: u32,
    pub server_ip: Option<String>,
    pub server_port: Option<u16>,
    pub access_token: Option<String>,
    pub uuid: Option<String>,
    pub online_account_access_token: Option<String>,
    pub online_account_uuid: Option<String>,
    pub custom_jvm_args: Option<String>,
    pub launch_task_id: Option<u64>,
}

#[derive(Clone)]
struct OfflineSkinProfile {
    uuid: String,
    user_properties: String,
    cached_at_unix: u64,
}

struct OfflineSkinLookup {
    cached: Option<OfflineSkinProfile>,
    receiver: Option<std::sync::mpsc::Receiver<Option<OfflineSkinProfile>>>,
}

const LAUNCH_WINDOW_WAIT_TIMEOUT: Duration = Duration::from_secs(300);
const OFFLINE_SKIN_CACHE_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);
static NATIVE_TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn running_games() -> &'static Mutex<HashMap<String, HashSet<u32>>> {
    static RUNNING: OnceLock<Mutex<HashMap<String, HashSet<u32>>>> = OnceLock::new();
    RUNNING.get_or_init(|| Mutex::new(HashMap::new()))
}

fn offline_skin_cache() -> &'static Mutex<HashMap<String, OfflineSkinProfile>> {
    static CACHE: OnceLock<Mutex<HashMap<String, OfflineSkinProfile>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

struct RunningGameRegistration {
    task_key: String,
    pid: u32,
}

impl Drop for RunningGameRegistration {
    fn drop(&mut self) {
        if let Ok(mut games) = running_games().lock() {
            if let Some(processes) = games.get_mut(&self.task_key) {
                processes.remove(&self.pid);
                if processes.is_empty() {
                    games.remove(&self.task_key);
                }
            }
        }
    }
}

fn register_running_game(
    game_dir: &str,
    version_name: &str,
    pid: u32,
) -> Result<RunningGameRegistration, String> {
    let task_key = scoped_task_key(game_dir, version_name);
    let mut games = running_games()
        .lock()
        .map_err(|_| "游戏运行状态锁已损坏".to_string())?;
    games.entry(task_key.clone()).or_default().insert(pid);
    Ok(RunningGameRegistration { task_key, pid })
}

pub fn is_instance_running(game_dir: &str, version_name: &str) -> Result<bool, String> {
    let task_key = scoped_task_key(game_dir, version_name);
    let games = running_games()
        .lock()
        .map_err(|_| "游戏运行状态锁已损坏".to_string())?;
    Ok(games
        .get(&task_key)
        .is_some_and(|processes| !processes.is_empty()))
}

#[cfg(windows)]
type Hwnd = isize;

#[cfg(windows)]
#[link(name = "user32")]
extern "system" {
    fn EnumWindows(callback: unsafe extern "system" fn(Hwnd, isize) -> i32, lparam: isize) -> i32;
    fn GetWindowThreadProcessId(hwnd: Hwnd, process_id: *mut u32) -> u32;
    fn IsWindowVisible(hwnd: Hwnd) -> i32;
}

#[cfg(windows)]
struct WindowSearch {
    pid: u32,
    found: bool,
}

#[cfg(windows)]
unsafe extern "system" fn enum_windows_for_pid(hwnd: Hwnd, lparam: isize) -> i32 {
    let search = &mut *(lparam as *mut WindowSearch);
    let mut window_pid = 0_u32;
    GetWindowThreadProcessId(hwnd, &mut window_pid);
    if window_pid == search.pid && IsWindowVisible(hwnd) != 0 {
        search.found = true;
        return 0;
    }
    1
}

#[cfg(windows)]
fn process_has_visible_window(pid: u32) -> bool {
    let mut search = WindowSearch { pid, found: false };
    unsafe {
        EnumWindows(
            enum_windows_for_pid,
            &mut search as *mut WindowSearch as isize,
        );
    }
    search.found
}

#[cfg(not(windows))]
fn process_has_visible_window(_pid: u32) -> bool {
    true
}

fn wait_for_game_window(
    child: &mut std::process::Child,
    pid: u32,
    log_path: &std::path::Path,
    ver_dir: &std::path::Path,
) -> Result<(), String> {
    let started = Instant::now();
    loop {
        if process_has_visible_window(pid) {
            return Ok(());
        }

        if let Some(status) = child
            .try_wait()
            .map_err(|e| format!("检查游戏进程失败: {}", e))?
        {
            let exit_code = status.code().unwrap_or(-1);
            let launch_log = read_tail_lines(log_path, 120);
            let game_log = read_tail_lines(&ver_dir.join("logs").join("latest.log"), 80);
            let combined = format!("{}\n{}", launch_log, game_log);
            let diagnosis = analyze_crash_log(&combined, exit_code);
            return Err(format!(
                "游戏窗口出现前进程已退出（退出码 {}）\n{}",
                exit_code, diagnosis
            ));
        }

        if started.elapsed() >= LAUNCH_WINDOW_WAIT_TIMEOUT {
            return Err("等待游戏窗口超时，游戏进程仍在后台运行".to_string());
        }

        std::thread::sleep(Duration::from_millis(500));
    }
}

fn launch_log_preview(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let preview: String = chars.by_ref().take(max_chars).collect();
    // 中文路径不能用字节下标截断，否则正式版会直接 panic。
    if chars.next().is_some() {
        format!("{}... (truncated)", preview)
    } else {
        preview
    }
}

fn is_sensitive_launch_argument_name(argument: &str) -> bool {
    let name = argument
        .split_once('=')
        .map(|(name, _)| name)
        .unwrap_or(argument)
        .to_ascii_lowercase();
    name.starts_with('-') && (name.contains("token") || name.contains("session"))
}

fn redact_launch_argument(args: &[String], index: usize, access_token: Option<&str>) -> String {
    let argument = args.get(index).map(String::as_str).unwrap_or_default();
    if index > 0
        && !args[index - 1].contains('=')
        && is_sensitive_launch_argument_name(&args[index - 1])
    {
        return "*****(已隐藏)".to_string();
    }
    if is_sensitive_launch_argument_name(argument) {
        if let Some((name, _)) = argument.split_once('=') {
            return format!("{}=*****(已隐藏)", name);
        }
    }

    let Some(token) = access_token
        .map(str::trim)
        .filter(|token| !token.is_empty() && *token != "0")
    else {
        return argument.to_string();
    };
    if argument.contains(token) {
        argument.replace(token, "*****(已隐藏)")
    } else {
        argument.to_string()
    }
}

fn fetch_offline_skin_profile(player_name: &str) -> Option<OfflineSkinProfile> {
    let name = player_name.trim();
    if !valid_offline_player_name(name) {
        return None;
    }

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .ok()?;

    let profile_url = format!(
        "https://api.mojang.com/users/profiles/minecraft/{}",
        urlencoding::encode(name)
    );
    let profile_resp = client.get(profile_url).send().ok()?;
    if !profile_resp.status().is_success() {
        return None;
    }
    let profile_json: serde_json::Value = profile_resp.json().ok()?;
    let uuid = profile_json.get("id")?.as_str()?.to_string();

    let texture_url = format!(
        "https://sessionserver.mojang.com/session/minecraft/profile/{}?unsigned=false",
        uuid
    );
    let texture_resp = client.get(texture_url).send().ok()?;
    if !texture_resp.status().is_success() {
        return Some(OfflineSkinProfile {
            uuid,
            user_properties: "{}".to_string(),
            cached_at_unix: current_unix_seconds(),
        });
    }
    let texture_json: serde_json::Value = texture_resp.json().ok()?;
    let textures = texture_json
        .get("properties")
        .and_then(|p| p.as_array())
        .and_then(|properties| {
            properties
                .iter()
                .find(|p| p.get("name").and_then(|n| n.as_str()) == Some("textures"))
        });

    let user_properties = if let Some(texture) = textures {
        let value = texture.get("value").and_then(|v| v.as_str()).unwrap_or("");
        let signature = texture
            .get("signature")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        serde_json::json!({
            "textures": [{
                "name": "textures",
                "value": value,
                "signature": signature,
            }]
        })
        .to_string()
    } else {
        "{}".to_string()
    };

    Some(OfflineSkinProfile {
        uuid,
        user_properties,
        cached_at_unix: current_unix_seconds(),
    })
}

fn valid_offline_player_name(name: &str) -> bool {
    (3..=16).contains(&name.len())
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

fn current_unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn valid_cached_offline_skin(profile: &OfflineSkinProfile) -> bool {
    profile.uuid.len() == 32
        && profile.uuid.chars().all(|ch| ch.is_ascii_hexdigit())
        && serde_json::from_str::<serde_json::Value>(&profile.user_properties)
            .is_ok_and(|value| value.is_object())
}

fn load_offline_skin_profile(player_name: &str) -> Option<OfflineSkinProfile> {
    if !valid_offline_player_name(player_name) {
        return None;
    }
    let profile = offline_skin_cache()
        .lock()
        .ok()?
        .get(&player_name.to_ascii_lowercase())
        .cloned()?;
    valid_cached_offline_skin(&profile).then_some(profile)
}

fn offline_skin_cache_is_fresh(profile: &OfflineSkinProfile) -> bool {
    if profile.user_properties == "{}" {
        return false;
    }
    let now = current_unix_seconds();
    profile.cached_at_unix <= now.saturating_add(300)
        && now.saturating_sub(profile.cached_at_unix) <= OFFLINE_SKIN_CACHE_TTL.as_secs()
}

fn save_offline_skin_profile(
    player_name: &str,
    profile: &OfflineSkinProfile,
) -> Result<(), String> {
    if !valid_offline_player_name(player_name) || !valid_cached_offline_skin(profile) {
        return Err("离线皮肤缓存内容无效".to_string());
    }
    offline_skin_cache()
        .lock()
        .map_err(|_| "离线皮肤缓存锁已损坏".to_string())?
        .insert(player_name.to_ascii_lowercase(), profile.clone());
    Ok(())
}

fn begin_offline_skin_lookup(player_name: &str) -> OfflineSkinLookup {
    let cached = load_offline_skin_profile(player_name);
    if cached.as_ref().is_some_and(offline_skin_cache_is_fresh)
        || !valid_offline_player_name(player_name)
    {
        return OfflineSkinLookup {
            cached,
            receiver: None,
        };
    }

    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    let player_name = player_name.to_string();
    std::thread::spawn(move || {
        let profile = fetch_offline_skin_profile(&player_name);
        if let Some(profile) = profile.as_ref() {
            if save_offline_skin_profile(&player_name, profile).is_ok() {
                eprintln!("[launch] offline skin cache refreshed for {}", player_name);
            }
        }
        let _ = sender.send(profile);
    });
    OfflineSkinLookup {
        cached,
        receiver: Some(receiver),
    }
}

impl OfflineSkinLookup {
    fn resolve_without_waiting(self) -> Option<OfflineSkinProfile> {
        match self.receiver.and_then(|receiver| receiver.try_recv().ok()) {
            Some(Some(profile)) => Some(profile),
            _ => self.cached,
        }
    }
}

// 旧版 minecraftArguments 会带 Windows 路径，反斜杠必须原样保留。
fn split_command_args(input: &str) -> Result<Vec<String>, String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut in_arg = false;

    for ch in input.chars() {
        if let Some(q) = quote {
            if ch == q {
                quote = None;
            } else {
                current.push(ch);
            }
            in_arg = true;
            continue;
        }

        if ch == '"' || ch == '\'' {
            quote = Some(ch);
            in_arg = true;
            continue;
        }

        if ch.is_whitespace() {
            if in_arg {
                args.push(std::mem::take(&mut current));
                in_arg = false;
            }
            continue;
        }

        current.push(ch);
        in_arg = true;
    }

    if quote.is_some() {
        return Err("JVM 参数引号未闭合".to_string());
    }
    if in_arg {
        args.push(current);
    }

    Ok(args)
}

fn is_manually_supplied_game_argument(argument: &str) -> bool {
    matches!(
        argument,
        "--username"
            | "--version"
            | "--gameDir"
            | "--assetsDir"
            | "--assetIndex"
            | "--uuid"
            | "--accessToken"
            | "--userType"
            | "--versionType"
    )
}

fn append_extra_game_arguments(
    args: &mut Vec<String>,
    game_args: &[serde_json::Value],
    user_properties: &str,
) {
    let mut index = 0;
    while index < game_args.len() {
        let Some(argument) = game_args[index].as_str() else {
            index += 1;
            continue;
        };

        if is_manually_supplied_game_argument(argument) {
            index += 1;
            if game_args
                .get(index)
                .and_then(|value| value.as_str())
                .is_some()
            {
                index += 1;
            }
            continue;
        }

        if argument.starts_with("--") {
            if let Some(value) = game_args.get(index + 1).and_then(|value| value.as_str()) {
                if value.contains("${") {
                    let resolved = value.replace("${user_properties}", user_properties);
                    if !resolved.contains("${") {
                        args.push(argument.to_string());
                        args.push(resolved);
                    }
                    // 无法解析占位值时，参数名必须与参数值一起丢弃，避免后续参数错位。
                    index += 2;
                    continue;
                }
            }
        }

        if !argument.contains("${") {
            args.push(argument.to_string());
        }
        index += 1;
    }
}

fn launch_file_ok(path: &std::path::Path, expected_size: Option<u64>) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    expected_size.is_none_or(|size| size == 0 || meta.len() == size)
}

fn sha1_like(value: &str) -> bool {
    value.len() == 40 && value.chars().all(|ch| ch.is_ascii_hexdigit())
}

fn md5_digest(input: &[u8]) -> [u8; 16] {
    const SHIFT: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    const TABLE: [u32; 64] = [
        0xd76a_a478,
        0xe8c7_b756,
        0x2420_70db,
        0xc1bd_ceee,
        0xf57c_0faf,
        0x4787_c62a,
        0xa830_4613,
        0xfd46_9501,
        0x6980_98d8,
        0x8b44_f7af,
        0xffff_5bb1,
        0x895c_d7be,
        0x6b90_1122,
        0xfd98_7193,
        0xa679_438e,
        0x49b4_0821,
        0xf61e_2562,
        0xc040_b340,
        0x265e_5a51,
        0xe9b6_c7aa,
        0xd62f_105d,
        0x0244_1453,
        0xd8a1_e681,
        0xe7d3_fbc8,
        0x21e1_cde6,
        0xc337_07d6,
        0xf4d5_0d87,
        0x455a_14ed,
        0xa9e3_e905,
        0xfcef_a3f8,
        0x676f_02d9,
        0x8d2a_4c8a,
        0xfffa_3942,
        0x8771_f681,
        0x6d9d_6122,
        0xfde5_380c,
        0xa4be_ea44,
        0x4bde_cfa9,
        0xf6bb_4b60,
        0xbebf_bc70,
        0x289b_7ec6,
        0xeaa1_27fa,
        0xd4ef_3085,
        0x0488_1d05,
        0xd9d4_d039,
        0xe6db_99e5,
        0x1fa2_7cf8,
        0xc4ac_5665,
        0xf429_2244,
        0x432a_ff97,
        0xab94_23a7,
        0xfc93_a039,
        0x655b_59c3,
        0x8f0c_cc92,
        0xffef_f47d,
        0x8584_5dd1,
        0x6fa8_7e4f,
        0xfe2c_e6e0,
        0xa301_4314,
        0x4e08_11a1,
        0xf753_7e82,
        0xbd3a_f235,
        0x2ad7_d2bb,
        0xeb86_d391,
    ];

    let mut padded = input.to_vec();
    let bit_len = (padded.len() as u64).wrapping_mul(8);
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_le_bytes());

    let mut state = [0x6745_2301_u32, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476];
    for chunk in padded.chunks_exact(64) {
        let mut words = [0_u32; 16];
        for (index, word) in words.iter_mut().enumerate() {
            let offset = index * 4;
            *word = u32::from_le_bytes([
                chunk[offset],
                chunk[offset + 1],
                chunk[offset + 2],
                chunk[offset + 3],
            ]);
        }

        let [mut a, mut b, mut c, mut d] = state;
        for index in 0..64 {
            let (value, word_index) = match index {
                0..=15 => ((b & c) | (!b & d), index),
                16..=31 => ((d & b) | (!d & c), (5 * index + 1) % 16),
                32..=47 => (b ^ c ^ d, (3 * index + 5) % 16),
                _ => (c ^ (b | !d), (7 * index) % 16),
            };
            let next = b.wrapping_add(
                a.wrapping_add(value)
                    .wrapping_add(TABLE[index])
                    .wrapping_add(words[word_index])
                    .rotate_left(SHIFT[index]),
            );
            a = d;
            d = c;
            c = b;
            b = next;
        }
        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
    }

    let mut digest = [0_u8; 16];
    for (index, word) in state.iter().enumerate() {
        digest[index * 4..index * 4 + 4].copy_from_slice(&word.to_le_bytes());
    }
    digest
}

fn offline_player_uuid(player_name: &str) -> String {
    let mut bytes = md5_digest(format!("OfflinePlayer:{}", player_name).as_bytes());
    bytes[6] = (bytes[6] & 0x0f) | 0x30;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    bytes.iter().map(|byte| format!("{:02x}", byte)).collect()
}

fn read_valid_asset_index(
    path: &std::path::Path,
    expected_sha1: Option<&str>,
) -> Result<serde_json::Value, String> {
    let data = std::fs::read(path).map_err(|e| format!("读取资源索引失败: {}", e))?;
    if let Some(expected) = expected_sha1.filter(|value| sha1_like(value)) {
        let mut hasher = sha1_smol::Sha1::new();
        hasher.update(&data);
        let actual = hasher.digest().to_string();
        if !actual.eq_ignore_ascii_case(expected) {
            return Err(format!(
                "资源索引校验失败: expected {}, got {}",
                expected, actual
            ));
        }
    }
    let json: serde_json::Value =
        serde_json::from_slice(&data).map_err(|e| format!("解析资源索引失败: {}", e))?;
    if !json.get("objects").is_some_and(|value| value.is_object()) {
        return Err("资源索引缺少 objects".to_string());
    }
    Ok(json)
}

fn repair_required_launch_files(
    app_handle: &tauri::AppHandle,
    version_name: &str,
    game_dir: &std::path::Path,
    client_jar_path: &std::path::Path,
    json: &serde_json::Value,
) -> Result<(), String> {
    let http = reqwest::blocking::Client::builder()
        .pool_max_idle_per_host(64)
        .connect_timeout(std::time::Duration::from_secs(15))
        .timeout(std::time::Duration::from_secs(60))
        .user_agent("OAOI-Launcher/1.0")
        .build()
        .map_err(|e| format!("创建启动修复下载客户端失败: {}", e))?;

    let emit = |stage: &str, current: usize, total: usize, detail: &str| {
        let _ = app_handle.emit(
            "install-progress",
            serde_json::json!({
                "name": version_name,
                "stage": stage,
                "current": current,
                "total": total,
                "detail": detail
            }),
        );
    };

    if let Some(client_info) = json.get("downloads").and_then(|d| d.get("client")) {
        if let Some(url) = client_info.get("url").and_then(|v| v.as_str()) {
            let sha1 = client_info.get("sha1").and_then(|v| v.as_str());
            let size = client_info.get("size").and_then(|v| v.as_u64());
            if !launch_file_ok(client_jar_path, size) {
                emit("client", 0, 1, "启动前修复版本 jar...");
                download_file_with_progress(&http, url, client_jar_path, sha1, size, true, None, {
                    let app_handle = app_handle.clone();
                    let version_name = version_name.to_string();
                    move |done, total| {
                        let total = total.unwrap_or_else(|| done.max(1)).max(1);
                        let current = done.min(usize::MAX as u64) as usize;
                        let total = total.min(usize::MAX as u64) as usize;
                        let _ = app_handle.emit(
                            "install-progress",
                            serde_json::json!({
                                "name": version_name,
                                "stage": "client",
                                "current": current,
                                "total": total,
                                "detail": "启动前修复版本 jar..."
                            }),
                        );
                    }
                })
                .map_err(|e| format!("启动前修复版本 jar 失败: {}", e))?;
                emit("client", 1, 1, "版本 jar 修复完成");
            }
        }
    }

    if let Some(asset_index) = json.get("assetIndex") {
        let index_url = asset_index
            .get("url")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let index_id = asset_index
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let index_sha1 = asset_index.get("sha1").and_then(|v| v.as_str());
        if !index_url.is_empty() {
            let index_path = assets_dir(game_dir)
                .join("indexes")
                .join(format!("{}.json", index_id));
            if read_valid_asset_index(&index_path, index_sha1).is_err() {
                let _ = std::fs::remove_file(&index_path);
                emit("assetIndex", 0, 0, "启动前修复资源索引...");
                download_file_if_needed_cancelable(
                    &http,
                    index_url,
                    &index_path,
                    index_sha1,
                    asset_index.get("size").and_then(|value| value.as_u64()),
                    true,
                    None,
                )
                .map_err(|error| format!("启动前修复资源索引失败: {}", error))?;
                read_valid_asset_index(&index_path, index_sha1)?;
                emit("assetIndex", 1, 1, "资源索引修复完成");
            }
        }
    }

    Ok(())
}

fn prepare_launch_libraries(
    app_handle: &tauri::AppHandle,
    version_name: &str,
    game_dir: &std::path::Path,
    json: &serde_json::Value,
) -> Result<Vec<(String, std::path::PathBuf)>, String> {
    let libs_dir = libraries_dir(game_dir);
    let mut prepared = Vec::new();
    let mut missing = Vec::new();
    let mut queued_paths = HashSet::new();

    if let Some(libs) = json.get("libraries").and_then(|value| value.as_array()) {
        for lib in libs {
            if !library_rules_value_allowed(lib.get("rules")) {
                continue;
            }
            let library_name = lib.get("name").and_then(|value| value.as_str());
            let dedup_key = library_name
                .and_then(|name| {
                    let parts = name.split(':').collect::<Vec<_>>();
                    if parts.len() >= 4 {
                        Some(format!("{}:{}:{}", parts[0], parts[1], parts[3]))
                    } else if parts.len() >= 2 {
                        Some(format!("{}:{}", parts[0], parts[1]))
                    } else {
                        None
                    }
                })
                .unwrap_or_default();

            let artifact = lib.get("downloads").and_then(|value| value.get("artifact"));
            let (relative_path, url, sha1, size) = if let Some(path) = artifact
                .and_then(|value| value.get("path"))
                .and_then(|value| value.as_str())
            {
                let url = artifact
                    .and_then(|value| value.get("url"))
                    .and_then(|value| value.as_str())
                    .map(str::to_string)
                    .or_else(|| {
                        let base = lib
                            .get("url")
                            .and_then(|value| value.as_str())
                            .unwrap_or("https://libraries.minecraft.net/");
                        Some(format!("{}/{}", base.trim_end_matches('/'), path))
                    });
                (
                    path.to_string(),
                    url,
                    artifact
                        .and_then(|value| value.get("sha1"))
                        .and_then(|value| value.as_str())
                        .map(str::to_string),
                    artifact
                        .and_then(|value| value.get("size"))
                        .and_then(|value| value.as_u64()),
                )
            } else {
                if lib
                    .get("natives")
                    .and_then(|value| value.as_object())
                    .is_some()
                {
                    continue;
                }
                let Some(name) = library_name else {
                    continue;
                };
                let path = maven_name_to_path(name);
                let base = lib
                    .get("url")
                    .and_then(|value| value.as_str())
                    .unwrap_or("https://libraries.minecraft.net/");
                (
                    path.clone(),
                    Some(format!("{}/{}", base.trim_end_matches('/'), path)),
                    lib.get("sha1")
                        .and_then(|value| value.as_str())
                        .map(str::to_string),
                    lib.get("size").and_then(|value| value.as_u64()),
                )
            };

            let relative = safe_maven_path(&relative_path)
                .map_err(|error| format!("非法依赖库路径 {}: {}", relative_path, error))?;
            let destination = libs_dir.join(relative);
            if !launch_file_ok(&destination, size) && queued_paths.insert(destination.clone()) {
                let url = url.ok_or_else(|| format!("依赖库缺少下载地址: {}", relative_path))?;
                missing.push((url, destination.clone(), sha1, size));
            }
            prepared.push((dedup_key, destination));
        }
    }

    if !missing.is_empty() {
        let total = missing.len();
        let _ = app_handle.emit(
            "install-progress",
            serde_json::json!({
                "name": version_name,
                "stage": "libraries",
                "current": 0,
                "total": total,
                "detail": format!("启动前修复 {} 个依赖库...", total)
            }),
        );
        let http = reqwest::blocking::Client::builder()
            .pool_max_idle_per_host(32)
            .connect_timeout(std::time::Duration::from_secs(15))
            .timeout(std::time::Duration::from_secs(60))
            .user_agent("OAOI-Launcher/1.0")
            .build()
            .map_err(|error| format!("创建依赖库下载客户端失败: {}", error))?;
        let done = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        parallel_download(&http, missing, &done, 32, true, None)
            .map_err(|error| format!("启动前修复依赖库失败: {}", error))?;
        let _ = app_handle.emit(
            "install-progress",
            serde_json::json!({
                "name": version_name,
                "stage": "libraries",
                "current": total,
                "total": total,
                "detail": "依赖库修复完成"
            }),
        );
    }

    if let Some((_, missing_path)) = prepared.iter().find(|(_, path)| !path.is_file()) {
        return Err(format!("缺少必要的依赖库: {}", missing_path.display()));
    }
    Ok(prepared)
}

#[derive(serde::Deserialize, serde::Serialize)]
struct NativeExtractionManifest {
    archive_size: u64,
    archive_modified: u64,
    files: Vec<NativeExtractionFile>,
}

#[derive(serde::Deserialize, serde::Serialize)]
struct NativeExtractionFile {
    name: String,
    size: u64,
}

fn native_archive_stamp(path: &std::path::Path) -> Result<(u64, u64), String> {
    let metadata = std::fs::metadata(path)
        .map_err(|error| format!("读取 native 库信息失败 {}: {}", path.display(), error))?;
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    Ok((metadata.len(), modified))
}

fn native_manifest_path(
    natives_dir: &std::path::Path,
    native_path_text: &str,
) -> std::path::PathBuf {
    let mut hasher = sha1_smol::Sha1::new();
    hasher.update(native_path_text.as_bytes());
    natives_dir.join(format!(".oaoi-native-{}.json", hasher.digest()))
}

fn native_manifest_is_ready(
    marker: &std::path::Path,
    native_jar_path: &std::path::Path,
    natives_dir: &std::path::Path,
) -> bool {
    let Ok(data) = std::fs::read(marker) else {
        return false;
    };
    let Ok(manifest) = serde_json::from_slice::<NativeExtractionManifest>(&data) else {
        return false;
    };
    let Ok((archive_size, archive_modified)) = native_archive_stamp(native_jar_path) else {
        return false;
    };
    if manifest.archive_size != archive_size || manifest.archive_modified != archive_modified {
        return false;
    }
    manifest.files.iter().all(|file| {
        safe_path_name(&file.name, "native文件名")
            .ok()
            .and_then(|name| std::fs::metadata(natives_dir.join(name)).ok())
            .is_some_and(|metadata| metadata.is_file() && metadata.len() == file.size)
    })
}

fn write_native_manifest(
    marker: &std::path::Path,
    native_jar_path: &std::path::Path,
    files: Vec<NativeExtractionFile>,
) -> Result<(), String> {
    let (archive_size, archive_modified) = native_archive_stamp(native_jar_path)?;
    let data = serde_json::to_vec(&NativeExtractionManifest {
        archive_size,
        archive_modified,
        files,
    })
    .map_err(|error| format!("生成 native 完成清单失败: {}", error))?;
    let temp = marker.with_extension(format!("tmp-{}", std::process::id()));
    std::fs::write(&temp, data).map_err(|error| format!("写入 native 完成清单失败: {}", error))?;
    let _ = std::fs::remove_file(marker);
    std::fs::rename(&temp, marker).map_err(|error| format!("提交 native 完成清单失败: {}", error))
}

#[tauri::command]
pub async fn launch_minecraft(
    app_handle: tauri::AppHandle,
    options: LaunchOptions,
) -> Result<String, String> {
    let handle = app_handle.clone();
    tokio::task::spawn_blocking(move || do_launch_minecraft(options, handle))
        .await
        .map_err(|e| format!("启动线程失败: {}", e))?
}

fn do_launch_minecraft(
    options: LaunchOptions,
    app_handle: tauri::AppHandle,
) -> Result<String, String> {
    crate::offline_policy::ensure_launch_allowed(
        options.access_token.as_deref(),
        options.uuid.as_deref(),
        options.online_account_access_token.as_deref(),
        options.online_account_uuid.as_deref(),
    )?;

    if options.memory_mb < 512 {
        return Err("游戏内存不能低于 512 MB".to_string());
    }
    let system_memory = crate::java_detect::get_system_memory();
    if system_memory >= 512 && u64::from(options.memory_mb) > system_memory {
        return Err(format!("游戏内存不能超过当前物理内存 {} MB", system_memory));
    }

    let game_dir = resolve_game_dir(&options.game_dir);
    if !game_dir.exists() {
        return Err("游戏目录不存在".to_string());
    }
    let offline_skin_lookup = if options.access_token.is_none() && options.uuid.is_none() {
        Some(begin_offline_skin_lookup(&options.player_name))
    } else {
        None
    };

    // 实例目录
    let version_name = safe_path_name(&options.version_name, "版本名")?;
    let mutation_lock = crate::mod_manager::mod_mutation_lock(&options.game_dir, &version_name)?;
    let launch_guard = mutation_lock
        .lock()
        .map_err(|_| "版本文件事务锁已损坏".to_string())?;
    let ver_dir = version_dir(&game_dir, &version_name);
    if !ver_dir.exists() {
        return Err(format!("版本 {} 未安装", version_name));
    }

    // 外部实例可能通过 inheritsFrom 继承原版配置，启动前需先完成合并。
    let merged_version = load_merged_version_json(&game_dir, &version_name)?;
    let json = merged_version.json;
    let client_jar_dir = version_dir(&game_dir, &merged_version.jar_version);
    let client_jar_path = version_jar_path(&client_jar_dir, &merged_version.jar_version);
    if let Err(err) = repair_required_launch_files(
        &app_handle,
        &version_name,
        &game_dir,
        &client_jar_path,
        &json,
    ) {
        return Err(err);
    }

    // 获取主类
    let main_class = json["mainClass"]
        .as_str()
        .ok_or("版本 JSON 中缺少 mainClass")?;

    // 获取 asset index
    let asset_index = json["assetIndex"]["id"].as_str().unwrap_or("legacy");
    let assets_root = assets_dir(&game_dir);

    // 检查依赖与构建 classpath 共用同一遍清单，避免启动前重复遍历全部库。
    let libs_dir = libraries_dir(&game_dir);
    let mut classpath = Vec::new();
    let mut seen_keys: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for (dedup_key, path) in prepare_launch_libraries(&app_handle, &version_name, &game_dir, &json)?
    {
        let path = path.to_string_lossy().to_string();
        if !dedup_key.is_empty() {
            if let Some(&index) = seen_keys.get(&dedup_key) {
                classpath[index] = path;
            } else {
                seen_keys.insert(dedup_key, classpath.len());
                classpath.push(path);
            }
        } else {
            classpath.push(path);
        }
    }

    // 添加版本 jar
    if client_jar_path.exists() {
        classpath.push(client_jar_path.to_string_lossy().to_string());
    }

    // 检查 classpath
    let total_libs = json["libraries"].as_array().map(|a| a.len()).unwrap_or(0);
    if classpath.is_empty() {
        return Err(format!(
            "未找到任何库文件！\n版本 JSON 中有 {} 个库，但 libraries 目录 ({}) 中没有对应的 jar 文件。\n请确保游戏文件完整。",
            total_libs,
            libs_dir.to_string_lossy()
        ));
    }

    // natives 目录
    let natives_dir = version_natives_dir(&ver_dir, &version_name);
    if !natives_dir.exists() {
        std::fs::create_dir_all(&natives_dir)
            .map_err(|error| format!("创建 Native 目录失败: {}", error))?;
    }

    // 每次核对 native 清单，目录非空也可能缺少其中一部分文件。
    if let Some(libs) = json["libraries"].as_array() {
        let arch = if cfg!(target_pointer_width = "64") {
            "64"
        } else {
            "32"
        };
        let http = reqwest::blocking::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(15))
            .timeout(std::time::Duration::from_secs(60))
            .user_agent("OAOI-Launcher/1.0")
            .build()
            .map_err(|e| format!("创建 native 下载客户端失败: {}", e))?;

        for lib in libs {
            if !library_rules_value_allowed(lib.get("rules")) {
                continue;
            }
            let Some(raw_classifier) = lib["natives"]["windows"].as_str() else {
                continue;
            };
            let classifier_key = raw_classifier.replace("${arch}", arch);
            let classifier_info = lib["downloads"]["classifiers"]
                .get(&classifier_key)
                .or_else(|| lib["downloads"]["classifiers"].get(raw_classifier));
            let native_path_text = classifier_info
                .and_then(|value| value.get("path"))
                .and_then(|value| value.as_str())
                .map(|path| path.replace("${arch}", arch))
                .or_else(|| {
                    let name = lib.get("name").and_then(|value| value.as_str())?;
                    Some(maven_name_to_path_with_classifier(name, &classifier_key))
                });
            let Some(native_path_text) = native_path_text else {
                continue;
            };
            let native_rel_path = safe_maven_path(&native_path_text)
                .map_err(|e| format!("非法 native 路径 {}: {}", native_path_text, e))?;
            let native_jar_path = libs_dir.join(&native_rel_path);
            let native_size = classifier_info
                .and_then(|value| value.get("size"))
                .and_then(|value| value.as_u64());
            if !launch_file_ok(&native_jar_path, native_size) {
                let native_url = classifier_info
                    .and_then(|value| value.get("url"))
                    .and_then(|value| value.as_str())
                    .map(|url| url.to_string())
                    .or_else(|| {
                        let name = lib.get("name").and_then(|value| value.as_str())?;
                        let base = lib
                            .get("url")
                            .and_then(|value| value.as_str())
                            .unwrap_or_else(|| default_library_maven_base(name, true));
                        Some(format!(
                            "{}/{}",
                            base.trim_end_matches('/'),
                            native_path_text
                        ))
                    });
                if let Some(url) = native_url {
                    if let Some(parent) = native_jar_path.parent() {
                        std::fs::create_dir_all(parent)
                            .map_err(|e| format!("创建 native 库目录失败: {}", e))?;
                    }
                    eprintln!("[launch] 下载 native: {}", url);
                    let sha1 = classifier_info
                        .and_then(|value| value.get("sha1"))
                        .and_then(|value| value.as_str());
                    download_file_if_needed_cancelable(
                        &http,
                        &url,
                        &native_jar_path,
                        sha1,
                        native_size,
                        true,
                        None,
                    )
                    .map_err(|e| format!("下载 native 失败: {} -> {}", url, e))?;
                }
            }

            let native_marker = native_manifest_path(&natives_dir, &native_path_text);
            if native_manifest_is_ready(&native_marker, &native_jar_path, &natives_dir) {
                continue;
            }

            let file = std::fs::File::open(&native_jar_path)
                .map_err(|e| format!("打开 native 库失败 {}: {}", native_jar_path.display(), e))?;
            let mut archive = zip::ZipArchive::new(file)
                .map_err(|e| format!("读取 native 库失败 {}: {}", native_jar_path.display(), e))?;
            let mut extracted_files = Vec::new();
            for index in 0..archive.len() {
                let mut entry = archive
                    .by_index(index)
                    .map_err(|e| format!("读取 native 文件失败: {}", e))?;
                let name = entry.name().to_string();
                if !name.ends_with(".dll") && !name.ends_with(".so") && !name.ends_with(".dylib") {
                    continue;
                }
                let Some(filename) = name.rsplit('/').next() else {
                    continue;
                };
                let filename = safe_path_name(filename, "native文件名")?;
                let entry_size = entry.size();
                extracted_files.push(NativeExtractionFile {
                    name: filename.clone(),
                    size: entry_size,
                });
                let out_path = natives_dir.join(&filename);
                if launch_file_ok(&out_path, Some(entry_size)) {
                    continue;
                }
                let tmp_counter = NATIVE_TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
                let tmp_path = out_path.with_file_name(format!(
                    ".{}.{}.{}.oaoi_native_tmp",
                    filename,
                    std::process::id(),
                    tmp_counter
                ));
                let mut out = std::fs::File::create(&tmp_path)
                    .map_err(|e| format!("创建 native 临时文件失败: {}", e))?;
                if let Err(error) = std::io::copy(&mut entry, &mut out) {
                    drop(out);
                    let _ = std::fs::remove_file(&tmp_path);
                    return Err(format!("解压 native 文件失败: {}", error));
                }
                if let Err(error) = std::io::Write::flush(&mut out) {
                    drop(out);
                    let _ = std::fs::remove_file(&tmp_path);
                    return Err(format!("写入 native 文件失败: {}", error));
                }
                drop(out);
                if let Err(error) = crate::downloader::fs::finalize_part_file(
                    &tmp_path,
                    &out_path,
                    Some(entry_size),
                    None,
                ) {
                    let _ = std::fs::remove_file(&tmp_path);
                    return Err(format!("写入 native 文件失败: {}", error));
                }
            }
            if let Err(error) =
                write_native_manifest(&native_marker, &native_jar_path, extracted_files)
            {
                eprintln!("[launch] {}", error);
            }
        }
    }

    // 根据系统语言自动设置 Minecraft 语言
    let options_file = ver_dir.join("options.txt");
    if !options_file.exists() {
        let sys_lang = {
            // 使用 Windows API 获取系统语言（毫秒级，比 PowerShell 快 1000 倍）
            #[cfg(windows)]
            {
                extern "system" {
                    fn GetUserDefaultLocaleName(lpLocaleName: *mut u16, cchLocaleName: i32) -> i32;
                }
                let mut buf = [0u16; 85];
                let len = unsafe { GetUserDefaultLocaleName(buf.as_mut_ptr(), 85) };
                if len > 0 {
                    String::from_utf16_lossy(&buf[..((len - 1) as usize)])
                } else {
                    String::new()
                }
            }
            #[cfg(not(windows))]
            {
                String::new()
            }
        };
        let mc_lang = match sys_lang.to_lowercase().as_str() {
            "zh-cn" => "zh_cn",
            "zh-tw" | "zh-hk" => "zh_tw",
            "ja-jp" => "ja_jp",
            "ko-kr" => "ko_kr",
            "ru-ru" => "ru_ru",
            "de-de" => "de_de",
            "fr-fr" => "fr_fr",
            "es-es" => "es_es",
            "pt-br" => "pt_br",
            _ => "en_us",
        };
        let _ = std::fs::write(&options_file, format!("lang:{}\n", mc_lang));
    }

    // 离线 UUID 使用 Java UUID.nameUUIDFromBytes 对应的 MD5 算法。
    let uuid = offline_player_uuid(&options.player_name);
    // 只接收已经完成的后台结果；网络尚未返回时立即使用缓存或默认离线身份。
    let offline_skin_profile =
        offline_skin_lookup.and_then(|lookup| lookup.resolve_without_waiting());
    let launch_uuid = options
        .uuid
        .clone()
        .or_else(|| {
            offline_skin_profile
                .as_ref()
                .map(|profile| profile.uuid.clone())
        })
        .unwrap_or_else(|| uuid.clone());
    let user_properties = offline_skin_profile
        .as_ref()
        .map(|profile| profile.user_properties.clone())
        .unwrap_or_else(|| "{}".to_string());
    if offline_skin_profile.is_some() {
        eprintln!(
            "[launch] offline skin profile resolved for {}",
            options.player_name
        );
    }

    // 构建启动参数
    let xms = std::cmp::max(512, (options.memory_mb as f32 * 0.75) as u32);
    let mut args: Vec<String> = vec![
        format!("-Xmx{}m", options.memory_mb),
        format!("-Xms{}m", xms),
        format!("-Djava.library.path={}", natives_dir.to_string_lossy()),
        "-Dlog4j2.formatMsgNoLookups=true".to_string(),
    ];

    // 注入用户自定义 JVM 参数
    if let Some(ref custom) = options.custom_jvm_args {
        let trimmed = custom.trim();
        if !trimmed.is_empty() {
            for part in split_command_args(trimmed)? {
                args.push(part.to_string());
            }
            eprintln!("[launch] 自定义 JVM 参数: {}", trimmed);
        }
    }

    // 提前计算 classpath 字符串，避免循环内 O(n²) join
    let classpath_str = classpath.join(";");

    // 注入 JVM 参数
    if let Some(jvm_args) = json["arguments"]["jvm"].as_array() {
        let libs_dir_str = libs_dir.to_string_lossy().to_string();
        let natives_dir_str = natives_dir.to_string_lossy().to_string();
        let primary_jar_name = format!("{}.jar", version_name);

        let replace_vars = |s: &str| -> String {
            let mut r = s
                .replace("${natives_directory}", &natives_dir_str)
                .replace("${library_directory}", &libs_dir_str)
                .replace("${launcher_name}", "oaoi")
                .replace("${launcher_version}", "1.0")
                .replace("${classpath}", &classpath_str)
                .replace("${classpath_separator}", ";")
                .replace("${version_name}", &version_name)
                .replace("${primary_jar_name}", &primary_jar_name);
            // Windows: 检测任意盘符路径，将正斜杠统一为反斜杠
            let has_drive_letter =
                r.len() >= 2 && r.as_bytes()[1] == b':' && r.as_bytes()[0].is_ascii_alphabetic();
            let has_embedded_drive = r.contains(":\\") || r.contains(":/");
            if has_drive_letter || has_embedded_drive {
                r = r.replace('/', "\\");
            }
            r
        };

        for arg in jvm_args {
            if let Some(s) = arg.as_str() {
                let resolved = replace_vars(s);
                if resolved == "-cp" || resolved == classpath_str {
                    continue;
                }
                args.push(resolved);
            } else if arg.is_object() {
                let rules = arg["rules"].as_array();
                let mut allowed = false;
                if let Some(rules) = rules {
                    for rule in rules {
                        let action = rule["action"].as_str().unwrap_or("");
                        let os_name = rule["os"]["name"].as_str();
                        let os_arch = rule["os"]["arch"].as_str();
                        match action {
                            "allow" => match (os_name, os_arch) {
                                (None, None) => allowed = true,
                                (Some("windows"), _) => allowed = true,
                                (None, Some("x86")) => {}
                                _ => {}
                            },
                            "disallow" => {
                                if os_name == Some("windows") || os_name.is_none() {
                                    allowed = false;
                                }
                            }
                            _ => {}
                        }
                    }
                }
                if allowed {
                    if let Some(val) = arg["value"].as_str() {
                        args.push(replace_vars(val));
                    } else if let Some(vals) = arg["value"].as_array() {
                        for v in vals {
                            if let Some(s) = v.as_str() {
                                let resolved = replace_vars(s);
                                if resolved == "-cp" || resolved == classpath_str {
                                    continue;
                                }
                                args.push(resolved);
                            }
                        }
                    }
                }
            }
        }
    }

    // Forge / NeoForge 必需的 -DlibraryDirectory
    let (loader_type, _) = detect_loader(&json, &version_name);
    if (loader_type == "forge" || loader_type == "neoforge")
        && !args.iter().any(|a| a.starts_with("-DlibraryDirectory"))
    {
        args.push(format!("-DlibraryDirectory={}", libs_dir.to_string_lossy()));
    }

    // 构建游戏参数
    // 先检查是否为老版本格式（1.12.2及以下使用 minecraftArguments）
    let has_legacy_args = json["minecraftArguments"].as_str().is_some();

    if has_legacy_args {
        // 老版本: 只用 minecraftArguments，不手动追加基础参数（避免重复 --gameDir）
        args.extend([
            "-cp".to_string(),
            classpath_str.clone(),
            main_class.to_string(),
        ]);
        let mc_args_str = json["minecraftArguments"].as_str().unwrap();
        let legacy_assets = assets_root.join("virtual").join("legacy");
        let replaced = mc_args_str
            .replace("${auth_player_name}", &options.player_name)
            .replace("${version_name}", &version_name)
            .replace("${game_directory}", &ver_dir.to_string_lossy())
            .replace("${assets_root}", &assets_root.to_string_lossy())
            .replace("${game_assets}", &legacy_assets.to_string_lossy())
            .replace("${assets_index_name}", asset_index)
            .replace("${auth_uuid}", &launch_uuid)
            .replace(
                "${auth_access_token}",
                options.access_token.as_deref().unwrap_or("0"),
            )
            .replace(
                "${auth_session}",
                options.access_token.as_deref().unwrap_or("0"),
            )
            .replace(
                "${access_token}",
                options.access_token.as_deref().unwrap_or("0"),
            )
            .replace(
                "${user_type}",
                if options.access_token.is_some() {
                    "msa"
                } else {
                    "legacy"
                },
            )
            .replace("${version_type}", "release")
            .replace("${user_properties}", &user_properties);
        for part in split_command_args(&replaced)? {
            args.push(part.to_string());
        }
    } else {
        // 新版本: 手动构建基础参数 + arguments.game
        args.extend([
            "-cp".to_string(),
            classpath_str.clone(),
            main_class.to_string(),
            "--username".to_string(),
            options.player_name.clone(),
            "--version".to_string(),
            version_name.clone(),
            "--gameDir".to_string(),
            ver_dir.to_string_lossy().to_string(),
            "--assetsDir".to_string(),
            assets_root.to_string_lossy().to_string(),
            "--assetIndex".to_string(),
            asset_index.to_string(),
            "--uuid".to_string(),
            launch_uuid.clone(),
            "--accessToken".to_string(),
            options.access_token.clone().unwrap_or("0".to_string()),
            "--userType".to_string(),
            if options.access_token.is_some() {
                "msa".to_string()
            } else {
                "legacy".to_string()
            },
            "--versionType".to_string(),
            "release".to_string(),
        ]);

        // 注入 game 参数
        if let Some(game_args) = json["arguments"]["game"].as_array() {
            append_extra_game_arguments(&mut args, game_args, &user_properties);
        }
        let mut i = 0;
        while i < args.len() {
            if args[i] == "--userProperties" {
                let needs_value = args
                    .get(i + 1)
                    .map(|next| next.starts_with("--"))
                    .unwrap_or(true);
                if needs_value {
                    args.insert(i + 1, user_properties.clone());
                    i += 1;
                }
            }
            i += 1;
        }
    }

    // 自动进服
    if let Some(ip) = &options.server_ip {
        if !ip.is_empty() {
            args.push("--server".to_string());
            args.push(ip.clone());
            args.push("--port".to_string());
            args.push(options.server_port.unwrap_or(25565).to_string());
        }
    }

    // 确保 mods 文件夹存在
    let mods_dir = ver_dir.join("mods");
    std::fs::create_dir_all(&mods_dir).map_err(|error| format!("创建 Mod 目录失败: {}", error))?;

    // 调试日志
    eprintln!("\n[launch] ===== 启动命令 =====");
    eprintln!("[launch] Java: {}", options.java_path);
    eprintln!("[launch] MainClass: {}", main_class);
    eprintln!("[launch] Classpath entries: {}", classpath.len());
    for (i, _) in args.iter().enumerate() {
        let redacted = redact_launch_argument(&args, i, options.access_token.as_deref());
        if redacted.len() > 200 {
            eprintln!(
                "[launch] arg[{}]: {}",
                i,
                launch_log_preview(&redacted, 200)
            );
        } else {
            eprintln!("[launch] arg[{}]: {}", i, redacted);
        }
    }
    eprintln!("[launch] ===== END =====\n");

    // 启动游戏（使用 java.exe + CREATE_NO_WINDOW：无黑窗，JVM 错误写入日志而非弹对话框）
    let launch_exe = options.java_path.clone();

    // 创建日志文件
    let log_path = ver_dir.join("launch_output.log");
    let log_file = std::fs::File::create(&log_path).ok();
    let stderr_file = log_file.as_ref().and_then(|f| f.try_clone().ok());

    let mut cmd = std::process::Command::new(&launch_exe);
    cmd.args(&args)
        .current_dir(&ver_dir)
        .stdout(
            log_file
                .map(|f| std::process::Stdio::from(f))
                .unwrap_or(std::process::Stdio::null()),
        )
        .stderr(
            stderr_file
                .map(|f| std::process::Stdio::from(f))
                .unwrap_or(std::process::Stdio::null()),
        )
        .stdin(std::process::Stdio::null());
    #[cfg(windows)]
    {
        cmd.creation_flags(0x08000000);
    } // CREATE_NO_WINDOW
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(err) => return Err(format!("启动游戏失败: {}", err)),
    };

    let pid = child.id();
    let running_registration = match register_running_game(&options.game_dir, &version_name, pid) {
        Ok(registration) => registration,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
    };
    // 进程已经登记，后续删除由运行状态阻止，不再占用文件事务锁。
    drop(launch_guard);
    let launch_task_id = options.launch_task_id;
    let _ = app_handle.emit(
        "launch-window-waiting",
        serde_json::json!({
            "version": version_name,
            "pid": pid,
            "launch_task_id": launch_task_id,
            "timeout_seconds": LAUNCH_WINDOW_WAIT_TIMEOUT.as_secs()
        }),
    );
    if let Err(err) = wait_for_game_window(&mut child, pid, &log_path, &ver_dir) {
        let still_running = child
            .try_wait()
            .map(|status| status.is_none())
            .unwrap_or(true);
        if still_running {
            std::thread::spawn(move || {
                let _ = child.wait();
                drop(running_registration);
            });
        }
        return Err(err);
    }
    let _ = app_handle.emit(
        "launch-window-ready",
        serde_json::json!({
            "version": version_name,
            "pid": pid,
            "launch_task_id": launch_task_id
        }),
    );

    let cp_len = classpath.len();
    let version_for_log = version_name.to_string();
    let log_path_clone = log_path.clone();
    let ver_dir_clone = ver_dir.clone();
    let launch_started_at = std::time::SystemTime::now();

    // 后台线程：等待游戏进程退出，崩溃时发送事件
    std::thread::spawn(move || {
        let _running_registration = running_registration;
        match child.wait() {
            Ok(status) => {
                let exit_code = status.code().unwrap_or(-1);
                let launch_log = read_tail_lines(&log_path_clone, 200);
                let game_log = read_tail_lines(&ver_dir_clone.join("logs").join("latest.log"), 100);
                let fml_log = read_tail_lines(
                    &ver_dir_clone.join("logs").join("fml-client-latest.log"),
                    100,
                );
                let combined_log = format!("{}\n{}\n{}", launch_log, game_log, fml_log);
                let crash_report =
                    read_latest_crash_report_since(&ver_dir_clone, launch_started_at);

                // 窗口已出现后的退出只认明确崩溃证据，避免玩家点 X 被旧日志误判。
                if !crash_report.is_empty() || has_runtime_crash_marker(&combined_log) {
                    let diagnosis = analyze_crash_log(&combined_log, exit_code);
                    let log_lines: Vec<&str> = combined_log.lines().collect();
                    let tail_start = log_lines.len().saturating_sub(150);
                    let log_tail = log_lines[tail_start..].join("\n");
                    let _ = app_handle.emit(
                        "game-crashed",
                        serde_json::json!({
                            "version": version_for_log,
                            "launch_task_id": launch_task_id,
                            "exit_code": exit_code,
                            "diagnosis": diagnosis,
                            "log_tail": log_tail,
                            "crash_report": crash_report
                        }),
                    );
                } else {
                    let _ = app_handle.emit(
                        "game-exited",
                        serde_json::json!({
                            "version": version_for_log,
                            "launch_task_id": launch_task_id,
                            "exit_code": exit_code
                        }),
                    );
                }
            }
            Err(e) => {
                eprintln!("[launch] 等待进程出错: {}", e);
            }
        }
    });

    // 窗口已出现后再返回启动成功。
    Ok(format!(
        "游戏已启动 (PID: {}), 版本: {}, 库: {}/{}",
        pid, version_name, cp_len, total_libs
    ))
}

/// 读取本次启动后生成的 crash-report，避免拿旧报告误判。
fn read_latest_crash_report_since(
    game_dir: &std::path::Path,
    since: std::time::SystemTime,
) -> String {
    let crash_dir = game_dir.join("crash-reports");
    if !crash_dir.exists() {
        return String::new();
    }
    let mut newest: Option<(std::time::SystemTime, std::path::PathBuf)> = None;
    if let Ok(entries) = std::fs::read_dir(&crash_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().map(|e| e == "txt").unwrap_or(false) {
                if let Ok(meta) = path.metadata() {
                    if let Ok(modified) = meta.modified() {
                        if newest.as_ref().map_or(true, |(t, _)| modified > *t) {
                            newest = Some((modified, path));
                        }
                    }
                }
            }
        }
    }
    if let Some((time, path)) = newest {
        let is_current_launch =
            time >= since || since.duration_since(time).is_ok_and(|d| d.as_secs() <= 2);
        if is_current_launch {
            if let Ok(content) = std::fs::read_to_string(&path) {
                let lines: Vec<&str> = content.lines().collect();
                let start = if lines.len() > 100 {
                    lines.len() - 100
                } else {
                    0
                };
                return lines[start..].join("\n");
            }
        }
    }
    String::new()
}

fn has_runtime_crash_marker(log: &str) -> bool {
    let log_lower = log.to_lowercase();
    [
        "crash report saved to",
        "this crash report has been saved to:",
        "could not save crash report to",
        "/error]: unable to launch",
        "an exception was thrown, the game will display an error screen and halt.",
        "reported exception",
        "exception in server tick loop",
        "exception ticking world",
        "exception ticking entity",
        "exception ticking block entity",
        "a fatal error has been detected by the java runtime environment",
        "exception_access_violation",
        "sigsegv",
        "---- minecraft crash report ----",
    ]
    .iter()
    .any(|marker| log_lower.contains(marker))
}

/// 安全地只读取文件末尾最多 max_lines 行（最大读取 1MB），避免大日志 OOM
fn read_tail_lines(path: &std::path::Path, max_lines: usize) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut file) = std::fs::File::open(path) else {
        return String::new();
    };
    let Ok(metadata) = file.metadata() else {
        return String::new();
    };
    let file_size = metadata.len();
    // 最多读取 1MB
    let read_size = std::cmp::min(file_size, 1024 * 1024) as usize;
    if read_size == 0 {
        return String::new();
    }
    let offset = file_size - read_size as u64;
    if file.seek(SeekFrom::Start(offset)).is_err() {
        return String::new();
    }
    let mut buf = vec![0u8; read_size];
    let Ok(n) = file.read(&mut buf) else {
        return String::new();
    };
    buf.truncate(n);
    let content = String::from_utf8_lossy(&buf);
    let lines: Vec<&str> = content.lines().collect();
    let start = if lines.len() > max_lines {
        lines.len() - max_lines
    } else {
        0
    };
    // 如果从文件中间开始读取，第一行可能是不完整的，跳过它
    let start = if offset > 0 && start == 0 && !lines.is_empty() {
        1
    } else {
        start
    };
    lines[start..].join("\n")
}

/// 分析崩溃日志，返回人话错误提示
fn analyze_crash_log(log: &str, exit_code: i32) -> String {
    let log_lower = log.to_lowercase();

    // 按优先级匹配常见错误模式
    let patterns: Vec<(&str, &str)> = vec![
        // Mod/Forge 加载错误
        ("missing mods", "❌ 缺少前置 Mod！\n有 Mod 需要的前置依赖未安装。\n请检查游戏日志确认缺少哪些 Mod，然后安装对应的前置 Mod。"),
        ("there were errors previously", "❌ Forge Mod 加载出错！\n有 Mod 缺少依赖或版本不匹配，游戏无法启动。\n请检查 Mod 列表和前置依赖是否完整。"),
        ("errors loading minecraft", "❌ Mod 加载失败！\n有 Mod 缺少依赖或版本不匹配。\n请检查 Mod 的前置依赖是否已安装，以及 Forge 版本是否满足要求。"),
        ("missing or unsupported mandatory dependencies", "❌ 缺少必要的 Mod 依赖！\n请根据提示安装缺失的前置 Mod。"),
        ("incompatible mods found", "❌ 发现不兼容的 Mod！\nMod 之间存在版本冲突或缺少依赖。\n请根据弹窗提示安装/更新对应的 Mod。"),
        // Java 版本问题（优先级高的放前面）
        ("sun-misc-unsafe-memory-access", "❌ Java 版本过低！\n参数 --sun-misc-unsafe-memory-access 需要 Java 25 才支持。\nMinecraft 26.1+ 需要 Java 25，请在设置中选择 Java 25 的路径。"),
        ("unrecognized option", "❌ Java 版本过低，无法识别启动参数！\nMinecraft 26.1+ 需要 Java 25，请在设置中选择正确的 Java 版本。"),
        ("could not create the java virtual machine", "❌ 无法创建 Java 虚拟机！\nJava 版本与游戏不匹配。\nMinecraft 26.1+ 需要 Java 25，1.21-26.0 需要 Java 21，1.17-1.20 需要 Java 17，1.16 及以下需要 Java 8。"),
        ("urlclassloader", "❌ Java 版本不兼容！\n该游戏版本需要 Java 8，但当前使用的是 Java 9 或更高版本。\nURLClassLoader 在 Java 9+ 中已被移除。\n解决方案：请在设置中选择 Java 8（1.8）路径。"),
        ("has been compiled by a more recent version", "❌ Java 版本过低！\n请升级 Java 或使用自动选择模式。"),
        ("unsupportedclassversionerror", "❌ Java 版本不对！\n该游戏版本需要更高版本的 Java。\n请在设置中切换为合适的 Java 版本。"),
        ("java.lang.classcastexception", "❌ 类型转换异常！\n可能是 Java 版本不匹配或 Mod 冲突。\n如果是 1.12.2 等老版本，请使用 Java 8。"),
        ("java.lang.unsupportedoperationexception", "❌ Java 版本不兼容，请尝试其他 Java 版本。"),
        // 内存不足
        ("outofmemoryerror", "❌ 内存不足！\n请在设置中增加内存分配（建议至少 4096MB）。"),
        ("could not reserve enough space", "❌ 无法分配足够内存！\n当前设置的内存超过系统可用内存，请降低内存分配。"),
        ("gc overhead limit exceeded", "❌ 垃圾回收占用过多！\n请增加内存或减少 Mod 数量。"),
        // 重复参数（1.12.2 老问题）
        ("found multiple arguments for option", "❌ 启动参数重复！\n请检查自定义 JVM 参数是否与默认参数冲突。"),
        // 缺少类/Mod
        ("classnotfoundexception", "❌ 缺少必要的类文件！\n可能原因：Mod 缺少前置依赖，或游戏文件不完整。\n建议：重新安装此版本，或检查 Mod 依赖。"),
        ("nosuchfielderror", "❌ Mod 版本不兼容！\n某个 Mod 与当前游戏版本不匹配。"),
        ("nosuchmethoderror", "❌ Mod 版本冲突！\n某个 Mod 与当前游戏/Forge/Fabric 版本不兼容。\n请检查 Mod 的版本要求。"),
        // 库文件问题
        ("could not find or load main class", "❌ 找不到主类！\n游戏核心文件可能损坏，请尝试重新安装此版本。"),
        ("error: missing", "❌ 缺少必要的库文件！\n请重新安装此版本以修复文件。"),
        // Forge/Fabric 特定
        ("mixin apply failed", "❌ Mixin 注入失败！\n某个 Mod 的 Mixin 与当前版本不兼容。\n请逐个排查最近安装的 Mod。"),
        ("fml.common.loader", "⚠️ Forge 加载出错。\n请检查 Forge 版本是否与游戏版本匹配。"),
        // natives 问题
        ("no lwjgl", "❌ 缺少 LWJGL 本地库！\n请重新安装此版本。"),
        ("unsatisfiedlinkerror", "❌ 本地库加载失败！\n可能是 natives 文件缺失或损坏。\n请删除版本的 natives 文件夹后重试。"),
        // 显卡/OpenGL 问题
        ("pixel format not accelerated", "❌ 显卡不支持 OpenGL！\n请更新显卡驱动或检查是否使用了核显。\n笔记本用户请确保游戏使用独立显卡运行。"),
        ("opengl", "⚠️ OpenGL 相关错误！\n请更新显卡驱动，或尝试降低游戏画质设置。"),
        ("gl error", "⚠️ 显卡渲染出错！\n请更新显卡驱动。"),
        // 着色器
        ("shader", "⚠️ 着色器加载失败！\n当前光影可能与游戏版本不兼容。\n请删除或更换光影包后重试。"),
        // 堆栈溢出
        ("stackoverflowerror", "❌ 堆栈溢出！\n可能是 Mod 之间循环引用或递归过深。\n请排查最近安装的 Mod。"),
        // Mod 重复
        ("duplicate", "⚠️ 检测到重复的 Mod！\n请检查 mods 文件夹是否有同一个 Mod 的多个版本。"),
        // 权限问题
        ("access is denied", "❌ 文件访问被拒绝！\n请以管理员身份运行，或检查游戏目录权限。"),
        ("permission denied", "❌ 权限不足！\n请检查游戏文件夹的权限设置。"),
        // Fabric 特定
        ("fabric.mod.json", "❌ Fabric Mod 配置无效！\n某个 Mod 的 fabric.mod.json 文件损坏或格式错误。"),
        ("requires fabric", "❌ Mod 需要 Fabric 加载器！\n请确认已安装 Fabric Loader。"),
        ("requires quilt", "❌ Mod 需要 Quilt 加载器！\n请安装 Quilt Loader 后重试。"),
        // 世界损坏
        ("corrupt", "⚠️ 文件可能已损坏！\n游戏文件或存档可能损坏。\n请尝试恢复备份或重新安装。"),
        // 端口占用
        ("address already in use", "❌ 端口被占用！\n可能有其他 Minecraft 版本正在运行。\n请关闭后重试。"),
        // Java 进程崩溃（JVM crash）
        ("exception_access_violation", "❌ Java 进程崩溃（严重错误）！\n可能是显卡驱动或 Java 版本问题。\n请更新显卡驱动和 Java 版本。"),
        ("sigsegv", "❌ Java 进程崩溃（段错误）！\n请更新 Java 版本和显卡驱动。"),
    ];

    for (pattern, msg) in &patterns {
        if log_lower.contains(pattern) {
            return msg.to_string();
        }
    }

    // 未匹配到已知模式，显示日志最后几行
    let last_lines: Vec<&str> = log
        .lines()
        .rev()
        .filter(|l| !l.trim().is_empty())
        .take(8)
        .collect();

    if last_lines.is_empty() {
        format!(
            "❌ 游戏崩溃，但日志文件为空。\n退出码: {}\n请检查 Java 路径是否正确。",
            exit_code
        )
    } else {
        let mut result = String::from("❌ 游戏崩溃，以下是日志最后几行：\n\n");
        for line in last_lines.iter().rev() {
            result.push_str(line);
            result.push('\n');
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::{
        append_extra_game_arguments, is_instance_running, load_offline_skin_profile, md5_digest,
        offline_player_uuid, register_running_game, save_offline_skin_profile, OfflineSkinLookup,
        OfflineSkinProfile,
    };

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{:02x}", byte)).collect()
    }

    #[test]
    fn md5_matches_standard_vectors() {
        assert_eq!(hex(&md5_digest(b"")), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(hex(&md5_digest(b"abc")), "900150983cd24fb0d6963f7d28e17f72");
    }

    #[test]
    fn offline_uuid_matches_minecraft_algorithm() {
        assert_eq!(
            offline_player_uuid("Notch"),
            "b50ad385829d3141a2167e7d7539ba7f"
        );
    }

    #[test]
    fn 离线皮肤只使用进程内缓存且接收结果不等待() {
        let player_name = format!("Skin{}", std::process::id());
        let profile = OfflineSkinProfile {
            uuid: "0123456789abcdef0123456789abcdef".to_string(),
            user_properties: "{}".to_string(),
            cached_at_unix: 1,
        };
        save_offline_skin_profile(&player_name, &profile).unwrap();
        assert_eq!(
            load_offline_skin_profile(&player_name).unwrap().uuid,
            profile.uuid
        );

        let (_sender, receiver) = std::sync::mpsc::sync_channel(1);
        let lookup = OfflineSkinLookup {
            cached: Some(profile.clone()),
            receiver: Some(receiver),
        };
        assert_eq!(lookup.resolve_without_waiting().unwrap().uuid, profile.uuid);
    }

    #[test]
    fn 游戏运行登记会在守卫释放后清除() {
        let game_dir = std::env::temp_dir().join("oaoi-running-game-test");
        let game_dir = game_dir.to_string_lossy();
        let registration = register_running_game(&game_dir, "测试版本", 12345).unwrap();

        assert!(is_instance_running(&game_dir, "测试版本").unwrap());
        drop(registration);
        assert!(!is_instance_running(&game_dir, "测试版本").unwrap());
    }

    #[test]
    fn 缺失占位值时同时移除参数名() {
        let game_args = serde_json::json!([
            "--username",
            "${auth_player_name}",
            "--clientId",
            "${clientid}",
            "--xuid",
            "${auth_xuid}",
            "--userProperties",
            "${user_properties}",
            "--demo"
        ]);
        let mut args = Vec::new();
        append_extra_game_arguments(&mut args, game_args.as_array().unwrap(), "{}");

        assert_eq!(args, ["--userProperties", "{}", "--demo"]);
    }
}
