use serde::{Deserialize, Serialize};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const OFFLINE_REGION_MESSAGE: &str = "当前地区暂不开放离线模式，请先登录正版账号后再启动游戏。";
const POLICY_CACHE_TTL: Duration = Duration::from_secs(15 * 24 * 60 * 60);
const POLICY_CACHE_TTL_MS: u64 = 15 * 24 * 60 * 60 * 1000;
static POLICY_CACHE: OnceLock<Mutex<Option<(Instant, OfflinePolicy)>>> = OnceLock::new();

#[derive(Clone, Deserialize, Serialize)]
pub struct OfflinePolicy {
    pub offline_allowed: bool,
    pub score: u8,
    pub region_cn: bool,
    pub language_zh_cn: bool,
    pub timezone_cn: bool,
    pub geo_name: Option<String>,
    pub locale_name: Option<String>,
    pub timezone_name: Option<String>,
    pub message: String,
}

#[derive(Deserialize, Serialize)]
struct OfflinePolicyCacheFile {
    saved_at_ms: u64,
    policy: OfflinePolicy,
}

#[tauri::command]
pub fn get_offline_policy() -> OfflinePolicy {
    cached_offline_policy()
}

pub fn ensure_launch_allowed(
    access_token: Option<&str>,
    uuid: Option<&str>,
    has_online_account: bool,
) -> Result<(), String> {
    if has_online_account || has_online_credentials(access_token, uuid) {
        return Ok(());
    }
    ensure_offline_allowed()
}

fn ensure_offline_allowed() -> Result<(), String> {
    let policy = cached_offline_policy();
    if policy.offline_allowed {
        Ok(())
    } else {
        Err(OFFLINE_REGION_MESSAGE.to_string())
    }
}

fn has_online_credentials(access_token: Option<&str>, uuid: Option<&str>) -> bool {
    let token_ok = access_token
        .map(str::trim)
        .filter(|value| !value.is_empty() && *value != "0")
        .is_some();
    let uuid_ok = uuid
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_some();
    token_ok && uuid_ok
}

fn evaluate_offline_policy() -> OfflinePolicy {
    let geo_name = platform::geo_name();
    let locale_name = platform::locale_name();
    let timezone_name = platform::timezone_name();

    let region_cn = geo_name
        .as_deref()
        .map(|value| value.eq_ignore_ascii_case("CN"))
        .unwrap_or(false);
    let language_zh_cn = locale_name.as_deref().map(is_zh_cn_locale).unwrap_or(false)
        || platform::ui_language_is_zh_cn();
    let timezone_cn = timezone_name
        .as_deref()
        .map(is_china_timezone)
        .unwrap_or(false);
    let score = [region_cn, language_zh_cn, timezone_cn]
        .into_iter()
        .filter(|value| *value)
        .count() as u8;

    OfflinePolicy {
        offline_allowed: score >= 2,
        score,
        region_cn,
        language_zh_cn,
        timezone_cn,
        geo_name,
        locale_name,
        timezone_name,
        message: OFFLINE_REGION_MESSAGE.to_string(),
    }
}

fn cached_offline_policy() -> OfflinePolicy {
    let cache = POLICY_CACHE.get_or_init(|| Mutex::new(None));
    if let Ok(mut guard) = cache.lock() {
        if let Some((created_at, policy)) = guard.as_ref() {
            if created_at.elapsed() < POLICY_CACHE_TTL {
                return policy.clone();
            }
        }
        if let Some(policy) = read_disk_cache() {
            *guard = Some((Instant::now(), policy.clone()));
            return policy;
        }
        let policy = evaluate_offline_policy();
        write_disk_cache(&policy);
        *guard = Some((Instant::now(), policy.clone()));
        return policy;
    }
    read_disk_cache().unwrap_or_else(|| {
        let policy = evaluate_offline_policy();
        write_disk_cache(&policy);
        policy
    })
}

fn read_disk_cache() -> Option<OfflinePolicy> {
    let path = cache_path()?;
    let text = std::fs::read_to_string(path).ok()?;
    let cached: OfflinePolicyCacheFile = serde_json::from_str(&text).ok()?;
    if current_unix_millis().saturating_sub(cached.saved_at_ms) > POLICY_CACHE_TTL_MS {
        return None;
    }
    Some(cached.policy)
}

fn write_disk_cache(policy: &OfflinePolicy) {
    let Some(path) = cache_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let cached = OfflinePolicyCacheFile {
        saved_at_ms: current_unix_millis(),
        policy: policy.clone(),
    };
    if let Ok(text) = serde_json::to_string(&cached) {
        let _ = std::fs::write(path, text);
    }
}

fn current_unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u64::MAX as u128) as u64)
        .unwrap_or(0)
}

fn cache_path() -> Option<std::path::PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("APPDATA")
            .or_else(|| std::env::var_os("LOCALAPPDATA"))
            .map(std::path::PathBuf::from)
            .map(|base| base.join("oaoi").join("offline-policy-cache.json"))
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .map(|base| base.join(".oaoi").join("offline-policy-cache.json"))
    }
}

fn is_zh_cn_locale(value: &str) -> bool {
    let normalized = value.trim().replace('_', "-").to_ascii_lowercase();
    normalized == "zh-cn"
        || normalized.starts_with("zh-cn-")
        || normalized == "zh-hans-cn"
        || normalized.starts_with("zh-hans-cn-")
}

fn is_china_timezone(value: &str) -> bool {
    let normalized = value.trim().to_ascii_lowercase();
    normalized == "china standard time"
        || normalized == "asia/shanghai"
        || normalized.contains("中国标准时间")
}

#[cfg(windows)]
mod platform {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct SystemTime {
        year: u16,
        month: u16,
        day_of_week: u16,
        day: u16,
        hour: u16,
        minute: u16,
        second: u16,
        milliseconds: u16,
    }

    #[repr(C)]
    struct DynamicTimeZoneInformation {
        bias: i32,
        standard_name: [u16; 32],
        standard_date: SystemTime,
        standard_bias: i32,
        daylight_name: [u16; 32],
        daylight_date: SystemTime,
        daylight_bias: i32,
        timezone_key_name: [u16; 128],
        dynamic_daylight_time_disabled: u8,
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GetUserDefaultGeoName(geo_name: *mut u16, geo_name_count: i32) -> i32;
        fn GetUserDefaultLocaleName(locale_name: *mut u16, locale_name_count: i32) -> i32;
        fn GetSystemDefaultLocaleName(locale_name: *mut u16, locale_name_count: i32) -> i32;
        fn GetUserDefaultUILanguage() -> u16;
        fn GetSystemDefaultUILanguage() -> u16;
        fn GetDynamicTimeZoneInformation(info: *mut DynamicTimeZoneInformation) -> u32;
    }

    pub fn geo_name() -> Option<String> {
        read_win_string(16, |buf, len| unsafe { GetUserDefaultGeoName(buf, len) })
    }

    pub fn locale_name() -> Option<String> {
        read_win_string(85, |buf, len| unsafe { GetUserDefaultLocaleName(buf, len) }).or_else(
            || {
                read_win_string(85, |buf, len| unsafe {
                    GetSystemDefaultLocaleName(buf, len)
                })
            },
        )
    }

    pub fn ui_language_is_zh_cn() -> bool {
        unsafe { GetUserDefaultUILanguage() == 0x0804 || GetSystemDefaultUILanguage() == 0x0804 }
    }

    pub fn timezone_name() -> Option<String> {
        let mut info: DynamicTimeZoneInformation = unsafe { std::mem::zeroed() };
        let _ = unsafe { GetDynamicTimeZoneInformation(&mut info) };
        wide_array_to_string(&info.timezone_key_name)
            .or_else(|| wide_array_to_string(&info.standard_name))
    }

    fn read_win_string<F>(capacity: usize, reader: F) -> Option<String>
    where
        F: FnOnce(*mut u16, i32) -> i32,
    {
        let mut buf = vec![0u16; capacity];
        let len = reader(buf.as_mut_ptr(), buf.len() as i32);
        if len <= 1 {
            return None;
        }
        wide_array_to_string(&buf[..len as usize])
    }

    fn wide_array_to_string(value: &[u16]) -> Option<String> {
        let len = value.iter().position(|ch| *ch == 0).unwrap_or(value.len());
        if len == 0 {
            return None;
        }
        let text = OsString::from_wide(&value[..len])
            .to_string_lossy()
            .trim()
            .to_string();
        if text.is_empty() {
            None
        } else {
            Some(text)
        }
    }
}

#[cfg(not(windows))]
mod platform {
    pub fn geo_name() -> Option<String> {
        std::env::var("LC_ALL")
            .or_else(|_| std::env::var("LANG"))
            .ok()
            .and_then(|value| value.split('.').next().map(str::to_string))
    }

    pub fn locale_name() -> Option<String> {
        std::env::var("LC_ALL")
            .or_else(|_| std::env::var("LANG"))
            .ok()
            .and_then(|value| value.split('.').next().map(|v| v.replace('_', "-")))
    }

    pub fn ui_language_is_zh_cn() -> bool {
        false
    }

    pub fn timezone_name() -> Option<String> {
        std::env::var("TZ").ok()
    }
}
