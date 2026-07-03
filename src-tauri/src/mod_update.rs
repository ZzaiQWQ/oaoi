use crate::downloader::event::{DownloadEvent, DownloadOutcome};
use crate::downloader::{
    DownloadCandidate, DownloadEngineOptions, DownloadManager, DownloadRequest,
};
use crate::instance::{resolve_game_dir, safe_path_name, version_dir};
use crate::mod_download::OnlineModVersionInfo;
use crate::modpack_sources::safe_index_name;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::Emitter;

const CACHE_DIR: &str = "launcher-data";
const UPDATE_CACHE_SUBDIR: &str = "mod-update-cache";
const UPDATE_ROLLBACK_SUBDIR: &str = "mod-update-rollback";
const UPDATE_CACHE_VERSION: u32 = 7;
// 更新检查缓存保留 8 小时，避免频繁重复查远端更新。
const UPDATE_CACHE_TTL: Duration = Duration::from_secs(8 * 60 * 60);
const UPDATE_REQUEST_TIMEOUT: Duration = Duration::from_secs(6);
const UPDATE_CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const MOD_API_SHORT_TIMEOUT: Duration = Duration::from_secs(10);
const MOD_API_LONG_TIMEOUT: Duration = Duration::from_secs(30);
const UPDATE_HASH_CONCURRENCY: usize = 64;
const MOD_UPDATE_DOWNLOAD_WORKERS: usize = 64;
const CURSEFORGE_MIRROR_HOST: &str = "mod.mcimirror.top";
const CURSEFORGE_MIRROR_DNS_CACHE_TTL: Duration = Duration::from_secs(5 * 60);
const OLD_MOD_SUFFIX: &str = ".oaoi-old";

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ModUpdateInfo {
    pub file_name: String,
    pub display_name: String,
    pub enabled: bool,
    pub source: String,
    pub project_id: String,
    pub current_id: String,
    pub latest_version_id: String,
    pub latest_version_name: String,
    pub latest_file_name: String,
    pub mc_versions: String,
    pub loaders: String,
    pub date: String,
    pub file_size: u64,
    #[serde(default)]
    pub mr_url: String,
    #[serde(default)]
    pub cf_url: String,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ModSourceLinkInfo {
    pub file_name: String,
    pub mr_url: String,
    pub cf_url: String,
    #[serde(default)]
    pub modrinth_checked: bool,
    #[serde(default)]
    pub curseforge_checked: bool,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ModUpdateCacheView {
    pub name: String,
    pub mc_version: String,
    pub loader: String,
    pub updates: Vec<ModUpdateInfo>,
    pub links: Vec<ModSourceLinkInfo>,
    pub checked_at: Option<u64>,
    pub stale: bool,
    pub refreshing: bool,
    pub message: String,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ModUpdateCacheEvent {
    name: String,
    mc_version: String,
    loader: String,
    status: String,
    view: Option<ModUpdateCacheView>,
    message: String,
}

#[derive(Clone, Default)]
struct DetectedSources {
    modrinth: Option<ModrinthSource>,
    curseforge: Option<CurseForgeSource>,
}

#[derive(Clone)]
struct ModrinthSource {
    project_id: String,
    version_id: String,
}

#[derive(Clone, Copy)]
struct CurseForgeSource {
    project_id: u32,
    file_id: u32,
}

impl DetectedSources {
    fn is_empty(&self) -> bool {
        self.modrinth.is_none() && self.curseforge.is_none()
    }

    fn merge(&mut self, other: DetectedSources) {
        if other.modrinth.is_some() {
            self.modrinth = other.modrinth;
        }
        if other.curseforge.is_some() {
            self.curseforge = other.curseforge;
        }
    }

    fn legacy_identity(&self) -> (Option<String>, Option<String>, Option<String>) {
        if let Some(source) = self.modrinth.as_ref() {
            return (
                Some("modrinth".to_string()),
                Some(source.project_id.clone()),
                Some(source.version_id.clone()),
            );
        }
        if let Some(source) = self.curseforge.as_ref() {
            return (
                Some("curseforge".to_string()),
                Some(source.project_id.to_string()),
                Some(source.file_id.to_string()),
            );
        }
        (None, None, None)
    }
}

struct LocalModFile {
    file_name: String,
    rel: String,
    path: PathBuf,
    enabled: bool,
    size: u64,
    modified_ms: u64,
    changed: bool,
    sources: DetectedSources,
    sha1: Option<String>,
    sha512: Option<String>,
    fingerprint: Option<u32>,
    modrinth_checked: bool,
    curseforge_checked: bool,
    mrpack_source: Option<String>,
    mrpack_downloads: Vec<String>,
}

#[derive(Clone)]
struct HashedModFile {
    index: usize,
    sha1: String,
    fingerprint: Option<u32>,
}

struct ModrinthVersionMatch {
    project_id: String,
    version_id: String,
    downloads: Vec<String>,
}

struct CurseForgeFingerprintMatch {
    project_id: u32,
    file_id: u32,
    downloads: Vec<String>,
}

struct CurseForgeFingerprintLookupResult {
    matches: HashMap<String, CurseForgeFingerprintMatch>,
    checked_fingerprints: HashSet<u32>,
}

pub(crate) struct MrpackDownloadCacheEntry {
    pub downloads: Vec<String>,
    pub size: u64,
    pub modified_ms: u64,
    pub sha1: String,
    pub sha512: Option<String>,
    pub fingerprint: Option<u32>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModBulkUpdateResult {
    pub view: ModUpdateCacheView,
    pub updated: usize,
    pub failed: Vec<String>,
    pub old_backups: Vec<OldModBackupInfo>,
    pub rollback_records: Vec<ModUpdateRollbackRecord>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct OldModBackupInfo {
    pub file_name: String,
    pub size: u64,
    pub modified_ms: u64,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ModUpdateRollbackRecord {
    pub id: String,
    pub display_name: String,
    pub old_file_name: String,
    pub old_backup_file_name: String,
    pub new_file_name: String,
    pub new_sha1: String,
    pub updated_at_ms: u64,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModUpdateRollbackStore {
    version: u32,
    #[serde(default)]
    records: Vec<ModUpdateRollbackRecord>,
    #[serde(default)]
    update_plans: Vec<CachedUpdateDownloadPlan>,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct CachedUpdateDownloadPlan {
    key: String,
    update: ModUpdateInfo,
    downloads: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sha1: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    curseforge_file_id: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    curseforge_file_name: Option<String>,
    source_project_id: String,
    source_version_id: String,
    source: String,
    checked_at_ms: u64,
}

#[derive(Clone)]
struct UpdateLookupPlan {
    update: ModUpdateInfo,
    downloads: Vec<String>,
    sha1: Option<String>,
    curseforge_file_id: Option<u32>,
    curseforge_file_name: Option<String>,
    source_project_id: String,
    source_version_id: String,
    source: String,
}

#[derive(Clone)]
struct VersionDownloadInfo {
    info: OnlineModVersionInfo,
    downloads: Vec<String>,
    sha1: Option<String>,
}

#[derive(Clone)]
struct SourceLookupFile {
    index: usize,
    file_name: String,
    enabled: bool,
    sha1: Option<String>,
    fingerprint: Option<u32>,
    modrinth: Option<ModrinthSource>,
    curseforge: Option<CurseForgeSource>,
    modrinth_checked: bool,
    curseforge_checked: bool,
    changed: bool,
}

enum OnlineRefreshEvent {
    ModrinthLookup {
        index: usize,
        source: Option<ModrinthSource>,
        downloads: Vec<String>,
    },
    CurseForgeLookup {
        index: usize,
        source: Option<CurseForgeSource>,
        downloads: Vec<String>,
    },
    UpdatePlan(UpdateLookupPlan),
    Done,
}

#[derive(Clone)]
struct UpdateDownloadPlan {
    file_name: String,
    urls: Vec<String>,
    cache_urls: Vec<String>,
    sha1: Option<String>,
    source_project_id: String,
    source_version_id: String,
    source: String,
}

struct PendingModUpdate {
    request_id: String,
    update: ModUpdateInfo,
    plan: UpdateDownloadPlan,
    target_file_name: String,
    old_path: PathBuf,
    target_path: PathBuf,
    temp_path: PathBuf,
}

#[derive(Default, Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ModUpdateCache {
    version: u32,
    name: String,
    mc_version: String,
    loader: String,
    checked_at: u64,
    #[serde(default)]
    refreshing: bool,
    #[serde(default)]
    files: HashMap<String, CachedModFile>,
    #[serde(default)]
    links: Vec<ModSourceLinkInfo>,
    #[serde(default)]
    updates: Vec<ModUpdateInfo>,
}

#[derive(Default, Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct CachedModFile {
    file_name: String,
    rel: String,
    size: u64,
    modified_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sha1: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sha512: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    fingerprint: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    current_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    modrinth_project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    modrinth_version_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    curseforge_project_id: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    curseforge_file_id: Option<u32>,
    #[serde(default)]
    modrinth_checked: bool,
    #[serde(default)]
    curseforge_checked: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mrpack_source: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    mrpack_downloads: Vec<String>,
}

fn update_tasks() -> &'static Mutex<HashSet<String>> {
    static TASKS: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    TASKS.get_or_init(|| Mutex::new(HashSet::new()))
}

#[tauri::command]
// 前端切到“可更新”时先走这里；只读本地缓存，不在这里等待网络。
pub async fn get_mod_update_cache(
    game_dir: String,
    name: String,
    mc_version: String,
    loader: String,
) -> Result<ModUpdateCacheView, String> {
    let view = tokio::task::spawn_blocking(move || {
        read_mod_update_cache_view(&game_dir, &name, &mc_version, &loader)
    })
    .await
    .map_err(|e| format!("任务失败: {}", e))??;
    Ok(view)
}

#[tauri::command]
// 打开实例后后台预热缓存，避免用户进入 Tab 时才开始等网络。
pub async fn warm_mod_update_cache(
    app_handle: tauri::AppHandle,
    game_dir: String,
    name: String,
    mc_version: String,
    loader: String,
    force_update: Option<bool>,
) -> Result<(), String> {
    let game_root = resolve_game_dir(&game_dir);
    let safe_name = safe_path_name(&name, "版本名")?;
    let task_key = update_task_key(&game_root, &safe_name, &mc_version, &loader);
    let view = read_mod_update_cache_view(&game_dir, &name, &mc_version, &loader)?;
    if !view.stale || !begin_update_task(&task_key) {
        return Ok(());
    }

    tokio::spawn(async move {
        let event_name = name.clone();
        let event_mc_version = mc_version.clone();
        let event_loader = loader.clone();
        let stage_app_handle = app_handle.clone();
        let result = tokio::task::spawn_blocking(move || {
            refresh_mod_update_cache_blocking(
                &stage_app_handle,
                &game_dir,
                &name,
                &mc_version,
                &loader,
                force_update.unwrap_or(false),
            )
        })
        .await
        .map_err(|e| format!("任务失败: {}", e))
        .and_then(|value| value);
        finish_update_task(&task_key);
        match result {
            Ok(view) => {
                let event = ModUpdateCacheEvent {
                    name: event_name,
                    mc_version: event_mc_version,
                    loader: event_loader,
                    status: "ready".to_string(),
                    message: "Mod 更新缓存已刷新".to_string(),
                    view: Some(view),
                };
                let _ = app_handle.emit("mod-update-cache", event);
            }
            Err(err) => {
                let event = ModUpdateCacheEvent {
                    name: event_name,
                    mc_version: event_mc_version,
                    loader: event_loader,
                    status: "failed".to_string(),
                    message: err,
                    view: None,
                };
                let _ = app_handle.emit("mod-update-cache", event);
            }
        }
    });

    Ok(())
}

#[tauri::command]
// 从更新缓存里的结果执行更新；更新下载不重写检测缓存，下一次打开实例时重新对齐本地文件。
pub async fn update_mods_from_cache(
    game_dir: String,
    name: String,
    mc_version: String,
    loader: String,
    selected_file_names: Vec<String>,
) -> Result<ModBulkUpdateResult, String> {
    tokio::task::spawn_blocking(move || {
        update_mods_from_cache_blocking(&game_dir, &name, &mc_version, &loader, selected_file_names)
    })
    .await
    .map_err(|e| format!("任务失败: {}", e))?
}

#[tauri::command]
// 弹窗里展示自动更新留下的旧版 Mod，普通 Mod 列表不会扫描这些后缀。
pub async fn list_old_mod_backups(
    game_dir: String,
    name: String,
) -> Result<Vec<OldModBackupInfo>, String> {
    tokio::task::spawn_blocking(move || list_old_mod_backups_blocking(&game_dir, &name))
        .await
        .map_err(|e| format!("任务失败: {}", e))?
}

#[tauri::command]
// 只允许删除自动更新打过旧版标记的文件，防止前端参数误删正常 Mod。
pub async fn delete_old_mod_backups(
    game_dir: String,
    name: String,
    file_names: Vec<String>,
) -> Result<Vec<OldModBackupInfo>, String> {
    tokio::task::spawn_blocking(move || {
        delete_old_mod_backups_blocking(&game_dir, &name, file_names)
    })
    .await
    .map_err(|e| format!("任务失败: {}", e))?
}

#[tauri::command]
// 更新回档记录和更新检测缓存分开保存，避免更新完成后污染检测缓存。
pub async fn list_mod_update_rollbacks(
    game_dir: String,
    name: String,
) -> Result<Vec<ModUpdateRollbackRecord>, String> {
    tokio::task::spawn_blocking(move || list_mod_update_rollbacks_blocking(&game_dir, &name))
        .await
        .map_err(|e| format!("任务失败: {}", e))?
}

#[tauri::command]
// 一键回档：当前新版先打旧版后缀，再把旧备份恢复成原文件名。
pub async fn rollback_mod_updates(
    game_dir: String,
    name: String,
    record_ids: Vec<String>,
) -> Result<Vec<ModUpdateRollbackRecord>, String> {
    tokio::task::spawn_blocking(move || rollback_mod_updates_blocking(&game_dir, &name, record_ids))
        .await
        .map_err(|e| format!("任务失败: {}", e))?
}

fn refresh_mod_update_cache_blocking(
    app_handle: &tauri::AppHandle,
    game_dir: &str,
    name: &str,
    mc_version: &str,
    loader: &str,
    force_update: bool,
) -> Result<ModUpdateCacheView, String> {
    let game_root = resolve_game_dir(game_dir);
    let safe_name = safe_path_name(name, "版本名")?;
    let http = mod_api_http_client_builder()
        .build()
        .map_err(|e| e.to_string())?;

    let old_cache = load_update_cache(&game_root, &safe_name);
    let valid_old_cache = old_cache.as_ref().filter(|item| {
        item.version == UPDATE_CACHE_VERSION && cache_runtime_matches(item, mc_version, loader)
    });
    let mut files = scan_local_mod_files(&game_root, &safe_name, valid_old_cache)?;
    let has_cache = valid_old_cache.is_some();
    let local_changed = valid_old_cache
        .map(|item| local_cache_changed(item, &files))
        .unwrap_or(true);
    let update_expired = valid_old_cache.map(update_cache_expired).unwrap_or(true);
    let source_checks_pending = valid_old_cache
        .map(cache_source_checks_pending)
        .unwrap_or(true);
    let should_check_updates =
        force_update || !has_cache || update_expired || source_checks_pending;
    let force_missing_recheck = has_cache && (force_update || update_expired);
    let should_refresh_sources =
        !has_cache || local_changed || force_missing_recheck || source_checks_pending;
    let changed_only =
        has_cache && local_changed && !force_missing_recheck && !source_checks_pending;
    let checked_at_override = (!should_check_updates)
        .then(|| valid_old_cache.map(|old| old.checked_at))
        .flatten();

    let update_plans = if should_refresh_sources || should_check_updates {
        refresh_online_mod_data_parallel(
            app_handle,
            &game_root,
            &safe_name,
            name,
            &http,
            &mut files,
            changed_only,
            force_missing_recheck,
            should_refresh_sources,
            should_check_updates,
            mc_version,
            loader,
            checked_at_override,
        )?
    } else {
        Vec::new()
    };
    let mut updates = if should_check_updates {
        update_plans
            .iter()
            .map(|plan| plan.update.clone())
            .collect::<Vec<_>>()
    } else {
        inherit_cached_updates(valid_old_cache, &files)
    };
    sort_update_entries(&mut updates);
    if should_check_updates {
        save_cached_update_plans(&game_root, &safe_name, update_plans)?;
    }
    mark_files_reconciled(&mut files);
    save_update_cache_view(
        &game_root,
        &safe_name,
        name,
        mc_version,
        loader,
        &files,
        &updates,
        checked_at_override,
        false,
        "Mod 更新缓存已刷新".to_string(),
    )
}

fn save_update_cache_view(
    game_root: &Path,
    safe_name: &str,
    name: &str,
    mc_version: &str,
    loader: &str,
    files: &[LocalModFile],
    updates: &[ModUpdateInfo],
    checked_at_override: Option<u64>,
    refreshing: bool,
    message: String,
) -> Result<ModUpdateCacheView, String> {
    let mut cache = build_update_cache(name, mc_version, loader, files, updates.to_vec());
    if let Some(checked_at) = checked_at_override {
        cache.checked_at = checked_at;
    }
    let stale = cache_is_stale(&cache, mc_version, loader, files);
    save_update_cache(game_root, safe_name, &cache)?;
    Ok(cache_to_view_without_changed_filter(
        name,
        mc_version,
        loader,
        Some(cache),
        files,
        stale,
        refreshing,
        message,
    ))
}

struct ModUpdateTempDir {
    path: PathBuf,
}

impl ModUpdateTempDir {
    fn create(mods_dir: &Path) -> Result<Self, String> {
        let pid = std::process::id();
        for index in 0..1000 {
            let dir_name = if index == 0 {
                format!(".oaoi-mod-update-{}", pid)
            } else {
                format!(".oaoi-mod-update-{}-{}", pid, index)
            };
            let path = mods_dir.join(dir_name);
            match std::fs::create_dir(&path) {
                Ok(()) => return Ok(Self { path }),
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(err) => return Err(format!("创建 Mod 更新临时目录失败: {}", err)),
            }
        }
        Err("创建 Mod 更新临时目录失败: 名称冲突过多".to_string())
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ModUpdateTempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

struct ModUpdateCancelGuard {
    name: String,
}

impl ModUpdateCancelGuard {
    fn register(name: &str) -> Result<Self, String> {
        crate::instance::try_register_cancel(name)?;
        Ok(Self {
            name: name.to_string(),
        })
    }
}

impl Drop for ModUpdateCancelGuard {
    fn drop(&mut self) {
        crate::instance::unregister_cancel(&self.name);
    }
}

fn update_mods_from_cache_blocking(
    game_dir: &str,
    name: &str,
    mc_version: &str,
    loader: &str,
    selected_file_names: Vec<String>,
) -> Result<ModBulkUpdateResult, String> {
    let game_root = resolve_game_dir(game_dir);
    let safe_name = safe_path_name(name, "版本名")?;
    let inst_dir = version_dir(&game_root, &safe_name);
    let mods_dir = inst_dir.join("mods");
    std::fs::create_dir_all(&mods_dir).map_err(|e| e.to_string())?;

    let mut cache = load_update_cache(&game_root, &safe_name)
        .ok_or_else(|| "还没有可用的 Mod 更新缓存".to_string())?;
    if cache.version != UPDATE_CACHE_VERSION || !cache_runtime_matches(&cache, mc_version, loader) {
        return Err("Mod 更新缓存和当前实例不匹配，请等待后台检测完成".to_string());
    }

    let selected: HashSet<String> = selected_file_names
        .into_iter()
        .map(|name| mod_cache_key(&name))
        .collect();
    if selected.is_empty() {
        return Err("没有选择需要更新的 Mod".to_string());
    }

    let updates: Vec<ModUpdateInfo> = cache
        .updates
        .iter()
        .filter(|update| selected.contains(&mod_cache_key(&update.file_name)))
        .cloned()
        .collect();
    if updates.is_empty() {
        return Err("选择的 Mod 已不在更新缓存里".to_string());
    }
    let store = load_mod_update_rollback_store(&game_root, &safe_name);
    let plans_by_key: HashMap<String, CachedUpdateDownloadPlan> = store
        .update_plans
        .into_iter()
        .map(|plan| (plan.key.clone(), plan))
        .collect();

    let mut updated_keys = HashSet::new();
    let mut failed = Vec::new();
    let temp_dir = ModUpdateTempDir::create(&mods_dir)?;
    let mut pending = Vec::new();

    for update in updates {
        let key = mod_cache_key(&update.file_name);
        let Some(cached_plan) = plans_by_key.get(&key) else {
            failed.push(format!(
                "{}: 更新下载地址还没有写入缓存，请等待后台检测完成",
                update.file_name
            ));
            continue;
        };
        match prepare_single_mod_update(
            &mods_dir,
            temp_dir.path(),
            pending.len(),
            &update,
            cached_plan_to_download_plan(cached_plan)?,
        ) {
            Ok(item) => pending.push(item),
            Err(err) => failed.push(format!("{}: {}", update.file_name, err)),
        }
    }

    if !pending.is_empty() {
        let cancel_name = format!("mod-update-{}", safe_name);
        let _cancel_guard = ModUpdateCancelGuard::register(&cancel_name)?;
        let outcomes = download_pending_mod_updates(&cancel_name, &pending)?;
        let pending_by_id: HashMap<String, &PendingModUpdate> = pending
            .iter()
            .map(|item| (item.request_id.clone(), item))
            .collect();

        for outcome in outcomes {
            match outcome {
                DownloadOutcome::Finished(result) => {
                    let Some(item) = pending_by_id.get(&result.request_id) else {
                        continue;
                    };
                    match install_single_mod_update(&game_root, &safe_name, &mut cache, item) {
                        Ok(()) => {
                            updated_keys.insert(mod_cache_key(&item.update.file_name));
                        }
                        Err(err) => failed.push(format!("{}: {}", item.update.file_name, err)),
                    }
                }
                DownloadOutcome::Failed {
                    request_id, error, ..
                } => {
                    let file_name = pending_by_id
                        .get(&request_id)
                        .map(|item| item.update.file_name.as_str())
                        .unwrap_or(request_id.as_str());
                    failed.push(format!("{}: {}", file_name, error));
                }
            }
        }
    }

    cache
        .updates
        .retain(|update| !updated_keys.contains(&mod_cache_key(&update.file_name)));
    if !updated_keys.is_empty() {
        remove_cached_update_plans(&game_root, &safe_name, &updated_keys)?;
    }
    let files = scan_local_mod_files(&game_root, &safe_name, Some(&cache))?;
    let remaining_updates = cache.updates.clone();
    let mut next_cache = build_update_cache(name, mc_version, loader, &files, remaining_updates);
    // 更新安装不落盘重写检测缓存，下一次打开实例时让增删改检查重新接管。
    next_cache.checked_at = cache.checked_at;
    let view = cache_to_view_without_changed_filter(
        name,
        mc_version,
        loader,
        Some(next_cache),
        &files,
        false,
        false,
        "Mod 已更新".to_string(),
    );

    Ok(ModBulkUpdateResult {
        view,
        updated: updated_keys.len(),
        failed,
        old_backups: list_old_mod_backups_blocking(game_dir, name)?,
        rollback_records: list_mod_update_rollbacks_blocking(game_dir, name)?,
    })
}

fn prepare_single_mod_update(
    mods_dir: &Path,
    temp_dir: &Path,
    index: usize,
    update: &ModUpdateInfo,
    plan: UpdateDownloadPlan,
) -> Result<PendingModUpdate, String> {
    let target_file_name = target_update_file_name(update, &plan.file_name);
    let safe_target = safe_path_name(&target_file_name, "文件名")?;
    let old_file_name = safe_path_name(&update.file_name, "文件名")?;
    let old_path = mods_dir.join(&old_file_name);
    let target_path = mods_dir.join(&safe_target);
    let temp_path = temp_dir.join(format!("{}.download", safe_target));
    Ok(PendingModUpdate {
        request_id: format!("mod-update-file-{}", index),
        update: update.clone(),
        plan,
        target_file_name: safe_target,
        old_path,
        target_path,
        temp_path,
    })
}

fn mod_update_download_options() -> DownloadEngineOptions {
    let mut options = DownloadEngineOptions::default();
    options.max_global_connections = DownloadEngineOptions::default_global_connection_limit();
    options.max_active_files = MOD_UPDATE_DOWNLOAD_WORKERS;
    options.max_connections_per_file = 8;
    options.candidate_low_speed_limit = 32 * 1024;
    options
}

fn mod_update_download_candidates(urls: Vec<String>) -> Vec<String> {
    crate::modpack::download_mirror::with_mod_mirrors(urls)
}

fn build_mod_update_download_request(item: &PendingModUpdate) -> DownloadRequest {
    let candidates = mod_update_download_candidates(item.plan.urls.clone())
        .into_iter()
        .map(DownloadCandidate::new)
        .collect::<Vec<_>>();
    let mut request =
        DownloadRequest::new(item.request_id.clone(), candidates, &item.temp_path).without_resume();
    if let Some(sha1) = item
        .plan
        .sha1
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        request = request.with_expected_sha1(sha1.trim().to_string());
    }
    request
}

fn download_pending_mod_updates(
    cancel_name: &str,
    pending: &[PendingModUpdate],
) -> Result<Vec<DownloadOutcome>, String> {
    let options = mod_update_download_options();
    let pool = crate::instance::install_download_pool(cancel_name, options.max_global_connections);
    let manager = DownloadManager::with_options_and_pool(options, pool)?;
    let _manager_registration = crate::instance::register_download_manager(cancel_name, &manager);
    let requests = pending
        .iter()
        .map(build_mod_update_download_request)
        .collect::<Vec<_>>();
    Ok(manager.download_many(requests, move |event| {
        if let DownloadEvent::FileFailed {
            request_id, error, ..
        } = event
        {
            eprintln!("[mod-update] 下载失败: {} -> {}", request_id, error);
        }
    }))
}

fn install_single_mod_update(
    game_root: &Path,
    safe_name: &str,
    cache: &mut ModUpdateCache,
    item: &PendingModUpdate,
) -> Result<(), String> {
    let (sha1, sha512, fingerprint, size) =
        crate::modpack_export::hash_update_candidate(&item.temp_path)?;
    if item
        .plan
        .sha1
        .as_deref()
        .is_some_and(|expected| !expected.eq_ignore_ascii_case(&sha1))
    {
        return Err(format!(
            "sha1 不匹配: expected {}, got {}",
            item.plan.sha1.as_deref().unwrap_or(""),
            sha1
        ));
    }
    let backups =
        replace_downloaded_file(&item.temp_path, &item.target_path, &[item.old_path.clone()])?;
    if let Some((old_backup, _)) = backups
        .iter()
        .find(|(_, original)| *original == item.old_path)
        .or_else(|| {
            backups
                .iter()
                .find(|(_, original)| *original == item.target_path)
        })
    {
        let updated_at_ms = now_ms();
        save_mod_update_rollback_record(
            game_root,
            safe_name,
            ModUpdateRollbackRecord {
                id: mod_update_rollback_id(
                    &item.update.file_name,
                    &item.target_file_name,
                    updated_at_ms,
                ),
                display_name: item.update.display_name.clone(),
                old_file_name: item.update.file_name.clone(),
                old_backup_file_name: old_backup
                    .file_name()
                    .map(|value| value.to_string_lossy().to_string())
                    .unwrap_or_default(),
                new_file_name: item.target_file_name.clone(),
                new_sha1: sha1.clone(),
                updated_at_ms,
            },
        )?;
    }

    let metadata = std::fs::metadata(&item.target_path).map_err(|e| e.to_string())?;
    let modified_ms = metadata
        .modified()
        .ok()
        .map(system_time_ms)
        .unwrap_or_default();
    let old_key = mod_cache_key(&item.update.file_name);
    let old_entry = cache.files.remove(&old_key);
    let new_entry = updated_cache_file(
        &item.update,
        &item.plan,
        &item.target_file_name,
        size,
        modified_ms,
        sha1,
        sha512,
        fingerprint,
        old_entry.as_ref(),
    );
    cache
        .files
        .insert(mod_cache_key(&item.target_file_name), new_entry.clone());

    Ok(())
}

fn target_update_file_name(update: &ModUpdateInfo, latest_file_name: &str) -> String {
    let base = latest_file_name.trim().trim_end_matches(".disabled");
    if update.enabled {
        base.to_string()
    } else {
        format!("{}.disabled", base)
    }
}

fn replace_downloaded_file(
    tmp: &Path,
    dest: &Path,
    old_paths: &[PathBuf],
) -> Result<Vec<(PathBuf, PathBuf)>, String> {
    let mut backups = Vec::new();
    if dest.exists() {
        if let Err(err) = mark_old_mod_for_install(dest, &mut backups) {
            restore_marked_old_mods(backups);
            return Err(err);
        }
    }
    for old_path in old_paths {
        if old_path != dest && old_path.exists() {
            if let Err(err) = mark_old_mod_for_install(old_path, &mut backups) {
                restore_marked_old_mods(backups);
                return Err(err);
            }
        }
    }
    match std::fs::rename(tmp, dest) {
        Ok(()) => Ok(backups),
        Err(err) => {
            restore_marked_old_mods(backups);
            Err(format!("替换文件失败: {}", err))
        }
    }
}

fn mark_old_mod_for_install(
    path: &Path,
    backups: &mut Vec<(PathBuf, PathBuf)>,
) -> Result<(), String> {
    if let Some(backup) = mark_old_mod_file(path)? {
        backups.push((backup, path.to_path_buf()));
    }
    Ok(())
}

fn restore_marked_old_mods(backups: Vec<(PathBuf, PathBuf)>) {
    for (backup, original) in backups.into_iter().rev() {
        if !original.exists() {
            let _ = std::fs::rename(backup, original);
        }
    }
}

fn mark_old_mod_file(path: &Path) -> Result<Option<PathBuf>, String> {
    if !path.exists() {
        return Ok(None);
    }
    let Some(file_name) = path.file_name().and_then(|value| value.to_str()) else {
        return Err("旧版 Mod 文件名无效".to_string());
    };
    let backup = next_old_mod_backup_path(path, file_name)?;
    std::fs::rename(path, &backup).map_err(|e| format!("标记旧版 Mod 失败: {}", e))?;
    Ok(Some(backup))
}

fn next_old_mod_backup_path(path: &Path, file_name: &str) -> Result<PathBuf, String> {
    let Some(parent) = path.parent() else {
        return Err("旧版 Mod 路径无效".to_string());
    };
    for index in 0..1000 {
        let backup_name = if index == 0 {
            format!("{}{}", file_name, OLD_MOD_SUFFIX)
        } else {
            format!("{}.{}{}", file_name, index, OLD_MOD_SUFFIX)
        };
        let candidate = parent.join(backup_name);
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err("旧版 Mod 备份名称冲突过多".to_string())
}

fn list_old_mod_backups_blocking(
    game_dir: &str,
    name: &str,
) -> Result<Vec<OldModBackupInfo>, String> {
    let game_root = resolve_game_dir(game_dir);
    let safe_name = safe_path_name(name, "版本名")?;
    let mods_dir = version_dir(&game_root, &safe_name).join("mods");
    if !mods_dir.is_dir() {
        return Ok(Vec::new());
    }

    let mut backups = Vec::new();
    for entry in std::fs::read_dir(&mods_dir).map_err(|e| e.to_string())? {
        let Ok(entry) = entry else {
            continue;
        };
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(file_name) = path
            .file_name()
            .map(|value| value.to_string_lossy().to_string())
        else {
            continue;
        };
        if !is_old_mod_backup_name(&file_name) {
            continue;
        }
        let metadata = std::fs::metadata(&path).map_err(|e| e.to_string())?;
        backups.push(OldModBackupInfo {
            file_name,
            size: metadata.len(),
            modified_ms: metadata
                .modified()
                .ok()
                .map(system_time_ms)
                .unwrap_or_default(),
        });
    }
    backups.sort_by(|a, b| b.modified_ms.cmp(&a.modified_ms));
    Ok(backups)
}

fn delete_old_mod_backups_blocking(
    game_dir: &str,
    name: &str,
    file_names: Vec<String>,
) -> Result<Vec<OldModBackupInfo>, String> {
    let game_root = resolve_game_dir(game_dir);
    let safe_name = safe_path_name(name, "版本名")?;
    let mods_dir = version_dir(&game_root, &safe_name).join("mods");
    if !mods_dir.is_dir() {
        return Ok(Vec::new());
    }

    let targets = if file_names.is_empty() {
        list_old_mod_backups_blocking(game_dir, name)?
            .into_iter()
            .map(|item| item.file_name)
            .collect()
    } else {
        file_names
    };

    let mut deleted_names = HashSet::new();
    for file_name in targets {
        let safe_file_name = safe_path_name(&file_name, "文件名")?;
        if !is_old_mod_backup_name(&safe_file_name) {
            return Err(format!("拒绝删除非旧版 Mod 文件: {}", safe_file_name));
        }
        let path = mods_dir.join(&safe_file_name);
        if path.is_file() {
            std::fs::remove_file(&path).map_err(|e| format!("删除旧版 Mod 失败: {}", e))?;
        }
        deleted_names.insert(safe_file_name);
    }

    if !deleted_names.is_empty() {
        let mut store = load_mod_update_rollback_store(&game_root, &safe_name);
        // 删除旧版文件后同步剔除回档记录，避免前端显示已不存在的备份。
        store.records.retain(|record| {
            !deleted_names.contains(&record.old_backup_file_name)
                && is_old_mod_backup_name(&record.old_backup_file_name)
                && mods_dir.join(&record.old_backup_file_name).is_file()
        });
        save_mod_update_rollback_store(&game_root, &safe_name, &store)?;
    }

    list_old_mod_backups_blocking(game_dir, name)
}

fn list_mod_update_rollbacks_blocking(
    game_dir: &str,
    name: &str,
) -> Result<Vec<ModUpdateRollbackRecord>, String> {
    let game_root = resolve_game_dir(game_dir);
    let safe_name = safe_path_name(name, "版本名")?;
    let mods_dir = version_dir(&game_root, &safe_name).join("mods");
    let mut store = load_mod_update_rollback_store(&game_root, &safe_name);
    store.records.retain(|record| {
        is_old_mod_backup_name(&record.old_backup_file_name)
            && mods_dir.join(&record.old_backup_file_name).is_file()
    });
    save_mod_update_rollback_store(&game_root, &safe_name, &store)?;
    Ok(store.records)
}

fn rollback_mod_updates_blocking(
    game_dir: &str,
    name: &str,
    record_ids: Vec<String>,
) -> Result<Vec<ModUpdateRollbackRecord>, String> {
    let game_root = resolve_game_dir(game_dir);
    let safe_name = safe_path_name(name, "版本名")?;
    let mods_dir = version_dir(&game_root, &safe_name).join("mods");
    std::fs::create_dir_all(&mods_dir).map_err(|e| e.to_string())?;

    let selected: HashSet<String> = record_ids.into_iter().collect();
    let mut store = load_mod_update_rollback_store(&game_root, &safe_name);
    let mut kept = Vec::new();
    let mut errors = Vec::new();

    for record in store.records.into_iter() {
        let should_restore = selected.is_empty() || selected.contains(&record.id);
        if !should_restore {
            kept.push(record);
            continue;
        }
        match rollback_single_mod_update(&mods_dir, &record) {
            Ok(()) => {}
            Err(err) => {
                errors.push(format!("{}: {}", record.display_name, err));
                kept.push(record);
            }
        }
    }

    store.records = kept;
    save_mod_update_rollback_store(&game_root, &safe_name, &store)?;
    if !errors.is_empty() {
        return Err(errors.join("\n"));
    }
    list_mod_update_rollbacks_blocking(game_dir, name)
}

fn rollback_single_mod_update(
    mods_dir: &Path,
    record: &ModUpdateRollbackRecord,
) -> Result<(), String> {
    let old_file_name = safe_path_name(&record.old_file_name, "文件名")?;
    let old_backup_file_name = safe_path_name(&record.old_backup_file_name, "文件名")?;
    let new_file_name = safe_path_name(&record.new_file_name, "文件名")?;
    if !is_old_mod_backup_name(&old_backup_file_name) {
        return Err("旧版备份文件名无效".to_string());
    }

    let old_backup_path = mods_dir.join(old_backup_file_name);
    let old_restore_path = mods_dir.join(old_file_name);
    let new_path = mods_dir.join(new_file_name);
    if !old_backup_path.is_file() {
        return Err("旧版备份不存在".to_string());
    }

    if new_path.is_file() {
        if !record.new_sha1.is_empty() {
            let (actual_sha1, _, _, _) = crate::modpack_export::hash_update_candidate(&new_path)?;
            if !actual_sha1.eq_ignore_ascii_case(&record.new_sha1) {
                return Err("新版文件已被修改，已拒绝自动回档".to_string());
            }
        }
        mark_old_mod_file(&new_path)?;
    }
    if old_restore_path.exists() {
        mark_old_mod_file(&old_restore_path)?;
    }
    std::fs::rename(&old_backup_path, &old_restore_path)
        .map_err(|e| format!("恢复旧版 Mod 失败: {}", e))
}

fn save_mod_update_rollback_record(
    game_root: &Path,
    safe_name: &str,
    record: ModUpdateRollbackRecord,
) -> Result<(), String> {
    if record.old_backup_file_name.is_empty() {
        return Ok(());
    }
    let mut store = load_mod_update_rollback_store(game_root, safe_name);
    store
        .records
        .retain(|item| item.old_backup_file_name != record.old_backup_file_name);
    store.records.push(record);
    store
        .records
        .sort_by(|a, b| b.updated_at_ms.cmp(&a.updated_at_ms));
    save_mod_update_rollback_store(game_root, safe_name, &store)
}

fn save_cached_update_plans(
    game_root: &Path,
    safe_name: &str,
    plans: Vec<CachedUpdateDownloadPlan>,
) -> Result<(), String> {
    let mut store = load_mod_update_rollback_store(game_root, safe_name);
    store.update_plans = plans;
    save_mod_update_rollback_store(game_root, safe_name, &store)
}

fn remove_cached_update_plans(
    game_root: &Path,
    safe_name: &str,
    removed_keys: &HashSet<String>,
) -> Result<(), String> {
    let mut store = load_mod_update_rollback_store(game_root, safe_name);
    store
        .update_plans
        .retain(|plan| !removed_keys.contains(&plan.key));
    save_mod_update_rollback_store(game_root, safe_name, &store)
}

fn cached_plan_to_download_plan(
    plan: &CachedUpdateDownloadPlan,
) -> Result<UpdateDownloadPlan, String> {
    let mut urls = plan.downloads.clone();
    if let Some(file_id) = plan.curseforge_file_id {
        let file_name = plan
            .curseforge_file_name
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or(&plan.update.latest_file_name);
        for url in curseforge_mrpack_download_candidates(file_id, file_name, "") {
            push_unique_mrpack_url(&mut urls, url);
        }
    }
    if urls.is_empty() {
        return Err(format!("{} 缺少更新下载地址", plan.update.file_name));
    }
    Ok(UpdateDownloadPlan {
        file_name: plan.update.latest_file_name.clone(),
        urls,
        cache_urls: plan.downloads.clone(),
        sha1: plan.sha1.clone(),
        source_project_id: plan.source_project_id.clone(),
        source_version_id: plan.source_version_id.clone(),
        source: plan.source.clone(),
    })
}

fn load_mod_update_rollback_store(game_root: &Path, safe_name: &str) -> ModUpdateRollbackStore {
    let Some(data) = std::fs::read_to_string(mod_update_rollback_path(game_root, safe_name)).ok()
    else {
        return ModUpdateRollbackStore {
            version: UPDATE_CACHE_VERSION,
            records: Vec::new(),
            update_plans: Vec::new(),
        };
    };
    serde_json::from_str(&data).unwrap_or_else(|_| ModUpdateRollbackStore {
        version: UPDATE_CACHE_VERSION,
        records: Vec::new(),
        update_plans: Vec::new(),
    })
}

fn save_mod_update_rollback_store(
    game_root: &Path,
    safe_name: &str,
    store: &ModUpdateRollbackStore,
) -> Result<(), String> {
    let path = mod_update_rollback_path(game_root, safe_name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let data = serde_json::to_string_pretty(store).map_err(|e| e.to_string())?;
    std::fs::write(path, data).map_err(|e| e.to_string())
}

fn mod_update_rollback_path(game_root: &Path, safe_name: &str) -> PathBuf {
    game_root
        .join(CACHE_DIR)
        .join(UPDATE_ROLLBACK_SUBDIR)
        .join(format!("{}.json", safe_index_name(safe_name)))
}

fn mod_update_rollback_id(old_file_name: &str, new_file_name: &str, updated_at_ms: u64) -> String {
    format!(
        "{}|{}|{}",
        mod_cache_key(old_file_name),
        mod_cache_key(new_file_name),
        updated_at_ms
    )
}

fn is_old_mod_backup_name(file_name: &str) -> bool {
    file_name.to_ascii_lowercase().ends_with(OLD_MOD_SUFFIX)
}

fn updated_cache_file(
    update: &ModUpdateInfo,
    plan: &UpdateDownloadPlan,
    file_name: &str,
    size: u64,
    modified_ms: u64,
    sha1: String,
    sha512: String,
    fingerprint: u32,
    old: Option<&CachedModFile>,
) -> CachedModFile {
    let mut entry = old.cloned().unwrap_or_default();
    entry.file_name = file_name.to_string();
    entry.rel = format!("mods/{}", file_name);
    entry.size = size;
    entry.modified_ms = modified_ms;
    entry.sha1 = Some(sha1);
    entry.sha512 = Some(sha512);
    entry.fingerprint = Some(fingerprint);
    entry.source = Some(plan.source.clone());
    entry.project_id = Some(plan.source_project_id.clone());
    entry.current_id = Some(plan.source_version_id.clone());
    if plan.source == "modrinth" {
        entry.modrinth_project_id = Some(plan.source_project_id.clone());
        entry.modrinth_version_id = Some(plan.source_version_id.clone());
        entry.modrinth_checked = true;
        entry.mrpack_source = Some("modrinth".to_string());
        entry.mrpack_downloads = plan.cache_urls.clone();
    } else {
        entry.curseforge_project_id = plan.source_project_id.parse().ok();
        entry.curseforge_file_id = plan.source_version_id.parse().ok();
        entry.curseforge_checked = true;
        entry.mrpack_source = Some("curseforge".to_string());
        entry.mrpack_downloads.clear();
    }
    if update.source == "modrinth" {
        entry.modrinth_checked = true;
    }
    if update.source == "curseforge" {
        entry.curseforge_checked = true;
    }
    entry
}

fn read_mod_update_cache_view(
    game_dir: &str,
    name: &str,
    mc_version: &str,
    loader: &str,
) -> Result<ModUpdateCacheView, String> {
    let game_root = resolve_game_dir(game_dir);
    let safe_name = safe_path_name(name, "版本名")?;
    let cache = load_update_cache(&game_root, &safe_name);
    let files = scan_local_mod_files(&game_root, &safe_name, cache.as_ref())?;
    let stale = cache
        .as_ref()
        .map(|item| cache_is_stale(item, mc_version, loader, &files))
        .unwrap_or(true);
    let message = match (cache.as_ref(), stale) {
        (None, _) => "还没有 Mod 更新缓存".to_string(),
        (Some(_), true) => "缓存已过期，正在后台刷新".to_string(),
        (Some(_), false) => "已读取 Mod 更新缓存".to_string(),
    };
    Ok(cache_to_view(
        name,
        mc_version,
        loader,
        cache,
        &files,
        stale,
        is_update_refreshing(&update_task_key(&game_root, &safe_name, mc_version, loader)),
        message,
    ))
}

// 扫描只比较 size/mtime；变化的 jar 不复用旧来源，避免同名替换串到旧 ID。
fn scan_local_mod_files(
    game_root: &Path,
    safe_name: &str,
    cache: Option<&ModUpdateCache>,
) -> Result<Vec<LocalModFile>, String> {
    let mods_dir = version_dir(game_root, safe_name).join("mods");
    if !mods_dir.is_dir() {
        return Ok(Vec::new());
    }

    let mut files = Vec::new();
    // 这里只读文件元数据；耗时的 hash 只在缓存缺失或文件变化时计算。
    for entry in std::fs::read_dir(&mods_dir).map_err(|e| e.to_string())? {
        let Ok(entry) = entry else {
            continue;
        };
        let path = entry.path();
        if !path.is_file() || !is_mod_file(&path) {
            continue;
        }
        let Some(file_name) = path
            .file_name()
            .map(|value| value.to_string_lossy().to_string())
        else {
            continue;
        };
        let metadata = std::fs::metadata(&path).map_err(|e| e.to_string())?;
        let size = metadata.len();
        let modified_ms = metadata
            .modified()
            .ok()
            .map(system_time_ms)
            .unwrap_or_default();
        let rel = format!("mods/{}", file_name);
        let enabled = !file_name.to_ascii_lowercase().ends_with(".disabled");
        let cached_any = cached_file_for(cache, &file_name);
        let changed = cached_any
            .map(|item| item.size != size || item.modified_ms != modified_ms)
            .unwrap_or(true);
        let cached = cached_any.filter(|_| !changed);
        let mut sources = DetectedSources::default();
        if let Some(cached) = cached {
            sources.merge(detected_sources_from_cache(cached));
        }
        let modrinth_checked = !changed
            && (sources.modrinth.is_some()
                || cached.map(|item| item.modrinth_checked).unwrap_or(false));
        let curseforge_checked = !changed
            && (sources.curseforge.is_some()
                || cached.map(|item| item.curseforge_checked).unwrap_or(false));
        let sha1 = if changed {
            None
        } else {
            cached.and_then(|item| item.sha1.clone())
        };
        let sha512 = if changed {
            None
        } else {
            cached.and_then(|item| item.sha512.clone())
        };
        let fingerprint = if changed {
            None
        } else {
            cached.and_then(|item| item.fingerprint)
        };
        let mrpack_source = if changed {
            None
        } else {
            cached.and_then(|item| item.mrpack_source.clone())
        };
        let mrpack_downloads = if changed {
            Vec::new()
        } else {
            cached
                .map(|item| item.mrpack_downloads.clone())
                .unwrap_or_default()
        };
        files.push(LocalModFile {
            file_name,
            rel,
            path,
            enabled,
            size,
            modified_ms,
            changed,
            sources,
            sha1,
            sha512,
            fingerprint,
            modrinth_checked,
            curseforge_checked,
            mrpack_source,
            mrpack_downloads,
        });
    }
    Ok(files)
}

fn build_update_cache(
    name: &str,
    mc_version: &str,
    loader: &str,
    files: &[LocalModFile],
    updates: Vec<ModUpdateInfo>,
) -> ModUpdateCache {
    let mut cached_files = HashMap::new();
    for file in files {
        let (source, project_id, current_id) = file.sources.legacy_identity();
        cached_files.insert(
            mod_cache_key(&file.file_name),
            CachedModFile {
                file_name: file.file_name.clone(),
                rel: file.rel.clone(),
                size: file.size,
                modified_ms: file.modified_ms,
                sha1: file.sha1.clone(),
                sha512: file.sha512.clone(),
                fingerprint: file.fingerprint,
                source,
                project_id,
                current_id,
                modrinth_project_id: file
                    .sources
                    .modrinth
                    .as_ref()
                    .map(|source| source.project_id.clone()),
                modrinth_version_id: file
                    .sources
                    .modrinth
                    .as_ref()
                    .map(|source| source.version_id.clone()),
                curseforge_project_id: file
                    .sources
                    .curseforge
                    .as_ref()
                    .map(|source| source.project_id),
                curseforge_file_id: file
                    .sources
                    .curseforge
                    .as_ref()
                    .map(|source| source.file_id),
                modrinth_checked: file.modrinth_checked || file.sources.modrinth.is_some(),
                curseforge_checked: file.curseforge_checked || file.sources.curseforge.is_some(),
                mrpack_source: file.mrpack_source.clone(),
                mrpack_downloads: file.mrpack_downloads.clone(),
            },
        );
    }
    let links = source_links_from_files(files);
    ModUpdateCache {
        version: UPDATE_CACHE_VERSION,
        name: name.to_string(),
        mc_version: mc_version.to_string(),
        loader: loader.to_string(),
        checked_at: now_secs(),
        refreshing: false,
        files: cached_files,
        links,
        updates,
    }
}

fn mark_files_reconciled(files: &mut [LocalModFile]) {
    for file in files {
        file.changed = false;
    }
}

fn cache_to_view(
    name: &str,
    mc_version: &str,
    loader: &str,
    cache: Option<ModUpdateCache>,
    files: &[LocalModFile],
    stale: bool,
    refreshing: bool,
    message: String,
) -> ModUpdateCacheView {
    cache_to_view_with_changed_filter(
        name, mc_version, loader, cache, files, stale, refreshing, message, true,
    )
}

fn cache_to_view_without_changed_filter(
    name: &str,
    mc_version: &str,
    loader: &str,
    cache: Option<ModUpdateCache>,
    files: &[LocalModFile],
    stale: bool,
    refreshing: bool,
    message: String,
) -> ModUpdateCacheView {
    cache_to_view_with_changed_filter(
        name, mc_version, loader, cache, files, stale, refreshing, message, false,
    )
}

fn cache_to_view_with_changed_filter(
    name: &str,
    mc_version: &str,
    loader: &str,
    cache: Option<ModUpdateCache>,
    files: &[LocalModFile],
    stale: bool,
    refreshing: bool,
    message: String,
    filter_changed: bool,
) -> ModUpdateCacheView {
    let valid_cache = cache.as_ref().filter(|item| {
        item.version == UPDATE_CACHE_VERSION && cache_runtime_matches(item, mc_version, loader)
    });
    let updates = valid_cache
        .map(|item| filter_cached_updates(&item.updates, files, filter_changed))
        .unwrap_or_default();
    // 新缓存直接保存链接；老缓存没有 links 时才用文件来源补一次显示。
    let links = valid_cache
        .map(|item| {
            if item.links.is_empty() {
                source_links_from_files(files)
            } else {
                filter_cached_links(&item.links, files, filter_changed)
            }
        })
        .unwrap_or_default();
    ModUpdateCacheView {
        name: name.to_string(),
        mc_version: mc_version.to_string(),
        loader: loader.to_string(),
        checked_at: cache.map(|item| item.checked_at),
        updates,
        links,
        stale,
        refreshing,
        message,
    }
}

fn filter_cached_updates(
    updates: &[ModUpdateInfo],
    files: &[LocalModFile],
    filter_changed: bool,
) -> Vec<ModUpdateInfo> {
    let by_key: HashMap<String, &LocalModFile> = files
        .iter()
        .map(|file| (mod_cache_key(&file.file_name), file))
        .collect();
    updates
        .iter()
        .filter_map(|update| {
            let file = by_key.get(&mod_cache_key(&update.file_name))?;
            if filter_changed && file.changed {
                return None;
            }
            let mut next = update.clone();
            next.file_name = file.file_name.clone();
            next.enabled = file.enabled;
            next.display_name = display_name_from_file(&file.file_name);
            let links = source_link_for_file(file);
            next.mr_url = links.mr_url;
            next.cf_url = links.cf_url;
            Some(next)
        })
        .collect()
}

fn filter_cached_links(
    links: &[ModSourceLinkInfo],
    files: &[LocalModFile],
    filter_changed: bool,
) -> Vec<ModSourceLinkInfo> {
    let by_key: HashMap<String, &LocalModFile> = files
        .iter()
        .map(|file| (mod_cache_key(&file.file_name), file))
        .collect();
    links
        .iter()
        .filter_map(|link| {
            let file = by_key.get(&mod_cache_key(&link.file_name))?;
            if filter_changed && file.changed {
                return None;
            }
            Some(ModSourceLinkInfo {
                file_name: file.file_name.clone(),
                mr_url: link.mr_url.clone(),
                cf_url: link.cf_url.clone(),
                modrinth_checked: link.modrinth_checked,
                curseforge_checked: link.curseforge_checked,
            })
        })
        .filter(|item| !item.mr_url.is_empty() || !item.cf_url.is_empty())
        .collect()
}

fn source_links_from_files(files: &[LocalModFile]) -> Vec<ModSourceLinkInfo> {
    files
        .iter()
        .map(source_link_for_file)
        .filter(|item| !item.mr_url.is_empty() || !item.cf_url.is_empty())
        .collect()
}

fn source_link_for_file(file: &LocalModFile) -> ModSourceLinkInfo {
    ModSourceLinkInfo {
        file_name: file.file_name.clone(),
        mr_url: file
            .sources
            .modrinth
            .as_ref()
            .map(|source| format!("https://modrinth.com/mod/{}", source.project_id))
            .unwrap_or_default(),
        cf_url: file
            .sources
            .curseforge
            .as_ref()
            .map(|source| format!("https://www.curseforge.com/projects/{}", source.project_id))
            .unwrap_or_default(),
        modrinth_checked: file.modrinth_checked || file.sources.modrinth.is_some(),
        curseforge_checked: file.curseforge_checked || file.sources.curseforge.is_some(),
    }
}

fn curseforge_mrpack_download_candidates(
    file_id: u32,
    file_name: &str,
    api_download_url: &str,
) -> Vec<String> {
    let mut urls = Vec::new();
    if file_id > 0 && !file_name.is_empty() {
        let encoded_name = urlencoding::encode(file_name);
        push_unique_mrpack_url(
            &mut urls,
            format!(
                "https://edge.forgecdn.net/files/{}/{}/{}",
                file_id / 1000,
                file_id % 1000,
                encoded_name
            ),
        );
        push_unique_mrpack_url(
            &mut urls,
            format!(
                "https://mediafilez.forgecdn.net/files/{}/{}/{}",
                file_id / 1000,
                file_id % 1000,
                encoded_name
            ),
        );
        push_unique_mrpack_url(
            &mut urls,
            format!(
                "https://media.forgecdn.net/files/{}/{}/{}",
                file_id / 1000,
                file_id % 1000,
                encoded_name
            ),
        );
    }
    if !api_download_url.is_empty() {
        push_unique_mrpack_url(&mut urls, api_download_url.to_string());
    }
    urls
}

fn push_unique_mrpack_url(urls: &mut Vec<String>, url: String) {
    let normalized = url.trim();
    if normalized.is_empty() || urls.iter().any(|existing| existing == normalized) {
        return;
    }
    urls.push(normalized.to_string());
}

fn unique_strings(values: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    values
        .iter()
        .filter_map(|value| {
            let value = value.trim();
            if value.is_empty() || !seen.insert(value.to_string()) {
                return None;
            }
            Some(value.to_string())
        })
        .collect()
}

fn unique_u32(values: &[u32]) -> Vec<u32> {
    let mut seen = HashSet::new();
    values
        .iter()
        .copied()
        .filter(|value| seen.insert(*value))
        .collect()
}

fn cache_is_stale(
    cache: &ModUpdateCache,
    mc_version: &str,
    loader: &str,
    files: &[LocalModFile],
) -> bool {
    if cache.version != UPDATE_CACHE_VERSION || !cache_runtime_matches(cache, mc_version, loader) {
        return true;
    }
    if cache_source_checks_pending(cache) {
        return true;
    }
    if update_cache_expired(cache) {
        return true;
    }
    local_cache_changed(cache, files)
}

fn cache_source_checks_pending(cache: &ModUpdateCache) -> bool {
    cache.files.values().any(|file| {
        // 缓存需要记住“没查到”这个结果，否则没有平台来源的 Mod 会每次打开都重查。
        !file.modrinth_checked || !file.curseforge_checked
    })
}

fn update_cache_expired(cache: &ModUpdateCache) -> bool {
    if cache.refreshing {
        return true;
    }
    now_secs().saturating_sub(cache.checked_at) > UPDATE_CACHE_TTL.as_secs()
}

fn local_cache_changed(cache: &ModUpdateCache, files: &[LocalModFile]) -> bool {
    if cache.files.len() != files.len() {
        return true;
    }
    files.iter().any(|file| {
        let Some(cached) = cache.files.get(&mod_cache_key(&file.file_name)) else {
            return true;
        };
        cached.size != file.size || cached.modified_ms != file.modified_ms
    })
}

fn inherit_cached_updates(
    cache: Option<&ModUpdateCache>,
    files: &[LocalModFile],
) -> Vec<ModUpdateInfo> {
    let Some(cache) = cache else {
        return Vec::new();
    };
    let unchanged_keys: HashSet<String> = files
        .iter()
        .filter(|file| !file.changed)
        .map(|file| mod_cache_key(&file.file_name))
        .collect();
    let updates: Vec<ModUpdateInfo> = cache
        .updates
        .iter()
        .filter(|item| unchanged_keys.contains(&mod_cache_key(&item.file_name)))
        .cloned()
        .collect();
    filter_cached_updates(&updates, files, true)
}

fn cache_runtime_matches(cache: &ModUpdateCache, mc_version: &str, loader: &str) -> bool {
    cache.mc_version == mc_version && cache.loader == loader
}

fn load_update_cache(game_root: &Path, safe_name: &str) -> Option<ModUpdateCache> {
    let data = std::fs::read_to_string(update_cache_path(game_root, safe_name)).ok()?;
    serde_json::from_str(&data).ok()
}

fn save_update_cache(
    game_root: &Path,
    safe_name: &str,
    cache: &ModUpdateCache,
) -> Result<(), String> {
    let path = update_cache_path(game_root, safe_name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let data = serde_json::to_string_pretty(cache).map_err(|e| e.to_string())?;
    std::fs::write(path, data).map_err(|e| e.to_string())
}

pub(crate) fn load_cached_mrpack_downloads(
    game_root: &Path,
    safe_name: &str,
    mc_version: &str,
    loader: &str,
) -> HashMap<String, MrpackDownloadCacheEntry> {
    let Some(cache) = load_update_cache(game_root, safe_name) else {
        return HashMap::new();
    };
    if cache.version != UPDATE_CACHE_VERSION || !cache_runtime_matches(&cache, mc_version, loader) {
        return HashMap::new();
    }
    cache
        .files
        .into_values()
        .filter_map(|file| {
            let sha1 = file.sha1.clone()?;
            let mut downloads = file.mrpack_downloads;
            if let Some(file_id) = file.curseforge_file_id {
                let file_name = if file.file_name.is_empty() {
                    file.rel.rsplit('/').next().unwrap_or_default().to_string()
                } else {
                    file.file_name
                };
                for url in curseforge_mrpack_download_candidates(file_id, &file_name, "") {
                    push_unique_mrpack_url(&mut downloads, url);
                }
            }
            if downloads.is_empty() {
                return None;
            }
            Some((
                file.rel.clone(),
                MrpackDownloadCacheEntry {
                    size: file.size,
                    modified_ms: file.modified_ms,
                    sha1,
                    sha512: file.sha512,
                    fingerprint: file.fingerprint,
                    downloads,
                },
            ))
        })
        .collect()
}

fn update_cache_path(game_root: &Path, safe_name: &str) -> PathBuf {
    game_root
        .join(CACHE_DIR)
        .join(UPDATE_CACHE_SUBDIR)
        .join(format!("{}.json", safe_index_name(safe_name)))
}

fn cached_file_for<'a>(
    cache: Option<&'a ModUpdateCache>,
    file_name: &str,
) -> Option<&'a CachedModFile> {
    cache?.files.get(&mod_cache_key(file_name))
}

fn detected_sources_from_cache(file: &CachedModFile) -> DetectedSources {
    let mut sources = DetectedSources::default();
    if let (Some(project_id), Some(version_id)) = (
        file.modrinth_project_id.as_deref(),
        file.modrinth_version_id.as_deref(),
    ) {
        sources.modrinth = Some(ModrinthSource {
            project_id: project_id.to_string(),
            version_id: version_id.to_string(),
        });
    }
    if let (Some(project_id), Some(file_id)) = (file.curseforge_project_id, file.curseforge_file_id)
    {
        sources.curseforge = Some(CurseForgeSource {
            project_id,
            file_id,
        });
    }
    if !sources.is_empty() {
        return sources;
    }

    match (
        file.source.as_deref(),
        file.project_id.as_deref(),
        file.current_id.as_deref(),
    ) {
        (Some("modrinth"), Some(project_id), Some(version_id)) => {
            sources.modrinth = Some(ModrinthSource {
                project_id: project_id.to_string(),
                version_id: version_id.to_string(),
            });
        }
        (Some("curseforge"), Some(project_id), Some(file_id)) => {
            if let (Ok(project_id), Ok(file_id)) = (project_id.parse(), file_id.parse()) {
                sources.curseforge = Some(CurseForgeSource {
                    project_id,
                    file_id,
                });
            }
        }
        _ => {}
    }
    sources
}

fn update_task_key(game_root: &Path, safe_name: &str, mc_version: &str, loader: &str) -> String {
    format!(
        "{}|{}|{}|{}",
        game_root.display(),
        safe_name,
        mc_version,
        loader
    )
}

fn begin_update_task(key: &str) -> bool {
    update_tasks()
        .lock()
        .map(|mut tasks| tasks.insert(key.to_string()))
        .unwrap_or(false)
}

fn finish_update_task(key: &str) {
    if let Ok(mut tasks) = update_tasks().lock() {
        tasks.remove(key);
    }
}

fn is_update_refreshing(key: &str) -> bool {
    update_tasks()
        .lock()
        .map(|tasks| tasks.contains(key))
        .unwrap_or(false)
}

fn mod_cache_key(file_name: &str) -> String {
    file_name
        .strip_suffix(".disabled")
        .unwrap_or(file_name)
        .to_ascii_lowercase()
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_secs())
        .unwrap_or_default()
}

fn now_ms() -> u64 {
    system_time_ms(SystemTime::now())
}

fn system_time_ms(value: SystemTime) -> u64 {
    value
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or_default()
}

fn mod_api_http_client_builder() -> reqwest::blocking::ClientBuilder {
    reqwest::blocking::Client::builder()
        .use_rustls_tls()
        .connect_timeout(UPDATE_CONNECT_TIMEOUT)
        .timeout(UPDATE_REQUEST_TIMEOUT)
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) oaoi-launcher/1.0")
}

fn mod_api_url(url: &str) -> String {
    url.replace(
        "https://api.modrinth.com",
        "https://mod.mcimirror.top/modrinth",
    )
    .replace(
        "https://api.curseforge.com",
        "https://mod.mcimirror.top/curseforge",
    )
}

fn curseforge_fingerprint_timeout(item_count: usize) -> Duration {
    // 全量指纹查询的响应体会随 Mod 数量线性变大，按数量给镜像更宽的读取时间。
    let groups = ((item_count.max(1) as u64) + 99) / 100;
    let seconds = (10 + groups * 7).clamp(30, 180);
    Duration::from_secs(seconds)
}

fn mod_api_candidates(url: &str, item_count: usize) -> Vec<(String, Duration)> {
    let mirror = mod_api_url(url);
    if mirror == url {
        return vec![(url.to_string(), MOD_API_LONG_TIMEOUT)];
    }
    if url == "https://api.curseforge.com/v1/fingerprints/432" {
        return vec![
            (mirror, curseforge_fingerprint_timeout(item_count)),
            (url.to_string(), MOD_API_SHORT_TIMEOUT),
        ];
    }
    // 国内环境官方 API 经常连不上，先走 mcimirror 代理；官方 API 只放最后兜底。
    vec![
        (mirror.clone(), MOD_API_SHORT_TIMEOUT),
        (mirror, MOD_API_LONG_TIMEOUT),
        (url.to_string(), MOD_API_SHORT_TIMEOUT),
    ]
}

fn curseforge_mirror_ip_scores() -> &'static Mutex<HashMap<IpAddr, f64>> {
    static SCORES: OnceLock<Mutex<HashMap<IpAddr, f64>>> = OnceLock::new();
    SCORES.get_or_init(|| Mutex::new(HashMap::new()))
}

struct CurseForgeMirrorDnsCache {
    addrs: Vec<SocketAddr>,
    refreshed_at: Instant,
}

fn curseforge_mirror_dns_cache() -> &'static Mutex<Option<CurseForgeMirrorDnsCache>> {
    static CACHE: OnceLock<Mutex<Option<CurseForgeMirrorDnsCache>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

fn resolve_curseforge_mirror_addrs() -> Vec<SocketAddr> {
    if let Ok(cache) = curseforge_mirror_dns_cache().lock() {
        if let Some(cache) = cache.as_ref() {
            if cache.refreshed_at.elapsed() < CURSEFORGE_MIRROR_DNS_CACHE_TTL {
                return cache.addrs.clone();
            }
        }
    }

    let system_addrs = resolve_curseforge_mirror_system_addrs();
    let doh_addrs = resolve_curseforge_mirror_doh_addrs();
    let mut addrs = system_addrs.clone();
    addrs.extend(doh_addrs.clone());
    addrs.sort();
    addrs.dedup();
    eprintln!(
        "[mod-update][cf] 镜像 DNS 汇总: host={} system=[{}] doh=[{}] total=[{}]",
        CURSEFORGE_MIRROR_HOST,
        socket_addr_ips_summary(&system_addrs),
        socket_addr_ips_summary(&doh_addrs),
        socket_addr_ips_summary(&addrs)
    );
    if !addrs.is_empty() {
        if let Ok(mut cache) = curseforge_mirror_dns_cache().lock() {
            *cache = Some(CurseForgeMirrorDnsCache {
                addrs: addrs.clone(),
                refreshed_at: Instant::now(),
            });
        }
    }
    addrs
}

fn resolve_curseforge_mirror_system_addrs() -> Vec<SocketAddr> {
    let mut addrs = match (CURSEFORGE_MIRROR_HOST, 443).to_socket_addrs() {
        Ok(addrs) => addrs.collect::<Vec<_>>(),
        Err(err) => {
            eprintln!(
                "[mod-update][cf] 镜像系统 DNS 解析失败: host={} error={}",
                CURSEFORGE_MIRROR_HOST, err
            );
            return Vec::new();
        }
    };
    addrs.sort();
    addrs.dedup();
    addrs
}

fn resolve_curseforge_mirror_doh_addrs() -> Vec<SocketAddr> {
    let client = match reqwest::blocking::Client::builder()
        .use_rustls_tls()
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(3))
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) oaoi-launcher/1.0")
        .build()
    {
        Ok(client) => client,
        Err(err) => {
            eprintln!("[mod-update][cf] 镜像 DoH 客户端创建失败: error={}", err);
            return Vec::new();
        }
    };
    let endpoints = [
        (
            "alidns",
            format!(
                "https://dns.alidns.com/resolve?name={}&type=A",
                CURSEFORGE_MIRROR_HOST
            ),
        ),
        (
            "tencent",
            format!(
                "https://doh.pub/dns-query?name={}&type=A",
                CURSEFORGE_MIRROR_HOST
            ),
        ),
        (
            "cloudflare",
            format!(
                "https://cloudflare-dns.com/dns-query?name={}&type=A",
                CURSEFORGE_MIRROR_HOST
            ),
        ),
        (
            "google",
            format!(
                "https://dns.google/resolve?name={}&type=A",
                CURSEFORGE_MIRROR_HOST
            ),
        ),
    ];
    let mut out = Vec::new();
    for (name, url) in endpoints {
        let started = Instant::now();
        let resp = client
            .get(&url)
            .header("Accept", "application/dns-json")
            .send();
        let Ok(resp) = resp else {
            let err = resp.err().map(|err| err.to_string()).unwrap_or_default();
            eprintln!(
                "[mod-update][cf] 镜像 DoH 解析失败: provider={} elapsed={}ms error={}",
                name,
                started.elapsed().as_millis(),
                err
            );
            continue;
        };
        if !resp.status().is_success() {
            eprintln!(
                "[mod-update][cf] 镜像 DoH 状态异常: provider={} status={} elapsed={}ms",
                name,
                resp.status(),
                started.elapsed().as_millis()
            );
            continue;
        }
        let json = resp.json::<serde_json::Value>();
        let Ok(json) = json else {
            let err = json.err().map(|err| err.to_string()).unwrap_or_default();
            eprintln!(
                "[mod-update][cf] 镜像 DoH 响应解析失败: provider={} elapsed={}ms error={}",
                name,
                started.elapsed().as_millis(),
                err
            );
            continue;
        };
        let mut addrs = doh_answer_addrs(&json);
        addrs.sort();
        addrs.dedup();
        eprintln!(
            "[mod-update][cf] 镜像 DoH 解析完成: provider={} elapsed={}ms addrs=[{}]",
            name,
            started.elapsed().as_millis(),
            socket_addr_ips_summary(&addrs)
        );
        out.extend(addrs);
    }
    out.sort();
    out.dedup();
    out
}

fn doh_answer_addrs(json: &serde_json::Value) -> Vec<SocketAddr> {
    json["Answer"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|item| item["type"].as_u64() == Some(1))
        .filter_map(|item| item["data"].as_str())
        .filter_map(|value| value.parse::<IpAddr>().ok())
        .map(|ip| SocketAddr::new(ip, 443))
        .collect()
}

fn socket_addr_ips_summary(addrs: &[SocketAddr]) -> String {
    addrs
        .iter()
        .map(|addr| addr.ip().to_string())
        .collect::<Vec<_>>()
        .join(",")
}

fn select_curseforge_mirror_addr() -> Option<SocketAddr> {
    let addrs = resolve_curseforge_mirror_addrs();
    if addrs.is_empty() {
        return None;
    }
    let mut scores = curseforge_mirror_ip_scores().lock().ok()?;
    let selected = addrs
        .iter()
        .copied()
        .max_by(|left, right| {
            let left_score = *scores.get(&left.ip()).unwrap_or(&0.0);
            let right_score = *scores.get(&right.ip()).unwrap_or(&0.0);
            left_score
                .partial_cmp(&right_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .unwrap_or(addrs[0]);
    let entry = scores.entry(selected.ip()).or_insert(0.0);
    *entry -= 0.01;
    let summary = addrs
        .iter()
        .map(|addr| {
            format!(
                "{}:{:.3}",
                addr.ip(),
                scores.get(&addr.ip()).copied().unwrap_or(0.0)
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    eprintln!(
        "[mod-update][cf] 镜像 IP 选择: host={} selected={} candidates=[{}]",
        CURSEFORGE_MIRROR_HOST,
        selected.ip(),
        summary
    );
    Some(selected)
}

fn record_curseforge_mirror_addr(addr: Option<SocketAddr>, result: f64, reason: &str) {
    let Some(addr) = addr else {
        return;
    };
    let Ok(mut scores) = curseforge_mirror_ip_scores().lock() else {
        return;
    };
    let score = scores.entry(addr.ip()).or_insert(0.0);
    *score = *score * 0.5 + result * 0.5;
    eprintln!(
        "[mod-update][cf] 镜像 IP 评分: ip={} score={:.3} reason={}",
        addr.ip(),
        *score,
        reason
    );
}

fn build_curseforge_mirror_client(addr: SocketAddr) -> Result<reqwest::blocking::Client, String> {
    mod_api_http_client_builder()
        .resolve(CURSEFORGE_MIRROR_HOST, addr)
        .build()
        .map_err(|err| err.to_string())
}

fn is_curseforge_api_mirror(url: &str) -> bool {
    url.starts_with("https://mod.mcimirror.top/curseforge/")
}

fn should_log_curseforge_api(curseforge_key: bool, url: &str) -> bool {
    curseforge_key || url.contains("curseforge")
}

fn mod_api_candidate_kind(url: &str) -> &'static str {
    if is_curseforge_api_mirror(url) {
        "镜像"
    } else if url.starts_with("https://api.curseforge.com/") {
        "官方"
    } else {
        "普通"
    }
}

fn mod_api_body_items(body: &serde_json::Value) -> usize {
    ["fingerprints", "fileIds", "modIds", "hashes"]
        .iter()
        .find_map(|key| {
            body.get(*key)
                .and_then(|value| value.as_array())
                .map(|items| items.len())
        })
        .unwrap_or_default()
}

fn response_header_value(resp: &reqwest::blocking::Response, name: &str) -> String {
    resp.headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string()
}

fn response_header_summary(resp: &reqwest::blocking::Response) -> String {
    format!(
        "content-type={} content-encoding={} content-length={} transfer-encoding={} connection={}",
        response_header_value(resp, "content-type"),
        response_header_value(resp, "content-encoding"),
        response_header_value(resp, "content-length"),
        response_header_value(resp, "transfer-encoding"),
        response_header_value(resp, "connection")
    )
}

fn body_preview(body: &[u8]) -> String {
    let end = body.len().min(300);
    String::from_utf8_lossy(&body[..end])
        .replace('\r', "\\r")
        .replace('\n', "\\n")
}

fn mod_api_read_json_body(
    mut resp: reqwest::blocking::Response,
    candidate_kind: &str,
    candidate: &str,
    started: Instant,
    header_summary: &str,
    log_cf: bool,
) -> Option<serde_json::Value> {
    let expected = resp.content_length();
    let mut body = expected
        .and_then(|value| usize::try_from(value).ok())
        .map(Vec::with_capacity)
        .unwrap_or_default();
    let mut buffer = [0u8; 81920];
    loop {
        match resp.read(&mut buffer) {
            Ok(0) => break,
            Ok(size) => body.extend_from_slice(&buffer[..size]),
            Err(err) => {
                if log_cf {
                    eprintln!(
                        "[mod-update][cf] {}接口响应体下载失败: url={} expected={} downloaded={} elapsed={}ms headers=[{}] error={}",
                        candidate_kind,
                        candidate,
                        expected.map(|value| value.to_string()).unwrap_or_else(|| "-".to_string()),
                        body.len(),
                        started.elapsed().as_millis(),
                        header_summary,
                        err
                    );
                }
                return None;
            }
        }
    }
    if log_cf {
        eprintln!(
            "[mod-update][cf] {}接口响应体下载完成: url={} expected={} downloaded={} elapsed={}ms",
            candidate_kind,
            candidate,
            expected
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_string()),
            body.len(),
            started.elapsed().as_millis()
        );
    }
    match serde_json::from_slice::<serde_json::Value>(&body) {
        Ok(json) => Some(json),
        Err(err) => {
            if log_cf {
                eprintln!(
                    "[mod-update][cf] {}接口 JSON 解析失败: url={} downloaded={} elapsed={}ms preview={} error={}",
                    candidate_kind,
                    candidate,
                    body.len(),
                    started.elapsed().as_millis(),
                    body_preview(&body),
                    err
                );
            }
            None
        }
    }
}

fn mod_api_post_json_prefer_non_empty_array(
    http: &reqwest::blocking::Client,
    url: &str,
    body: &serde_json::Value,
    curseforge_key: bool,
    array_path: &[&str],
) -> Option<serde_json::Value> {
    let mut empty_result = None;
    let log_cf = should_log_curseforge_api(curseforge_key, url);
    let request_items = mod_api_body_items(body);
    let array_label = array_path.join(".");
    for (candidate, timeout) in mod_api_candidates(url, request_items) {
        let candidate_kind = mod_api_candidate_kind(&candidate);
        if log_cf {
            eprintln!(
                "[mod-update][cf] 尝试{}接口: url={} timeout={}s items={}",
                candidate_kind,
                candidate,
                timeout.as_secs(),
                request_items
            );
        }
        let mirror_addr = if is_curseforge_api_mirror(&candidate) {
            select_curseforge_mirror_addr()
        } else {
            None
        };
        let override_client = if let Some(addr) = mirror_addr {
            match build_curseforge_mirror_client(addr) {
                Ok(client) => Some(client),
                Err(err) => {
                    eprintln!(
                        "[mod-update][cf] 镜像 IP 客户端创建失败: ip={} error={}",
                        addr.ip(),
                        err
                    );
                    record_curseforge_mirror_addr(Some(addr), -1.0, "client-build-failed");
                    continue;
                }
            }
        } else {
            None
        };
        let request_http = override_client.as_ref().unwrap_or(http);
        let mut request = request_http
            .post(&candidate)
            .timeout(timeout)
            .header("Accept", "application/json")
            .json(body);
        if curseforge_key {
            request = request
                .version(reqwest::Version::HTTP_11)
                .header("Connection", "close");
        }
        if curseforge_key && !is_curseforge_api_mirror(&candidate) {
            request = request.header("x-api-key", &crate::instance::cf_api_key());
        }
        let started = Instant::now();
        let resp = match request.send() {
            Ok(resp) => resp,
            Err(err) => {
                record_curseforge_mirror_addr(mirror_addr, -1.0, "send-failed");
                if log_cf {
                    eprintln!(
                        "[mod-update][cf] {}接口请求失败: url={} selected_ip={} elapsed={}ms error={}",
                        candidate_kind,
                        candidate,
                        mirror_addr.map(|addr| addr.ip().to_string()).unwrap_or_else(|| "-".to_string()),
                        started.elapsed().as_millis(),
                        err
                    );
                }
                continue;
            }
        };
        let status = resp.status();
        let header_summary = if log_cf {
            Some(response_header_summary(&resp))
        } else {
            None
        };
        if !status.is_success() {
            record_curseforge_mirror_addr(mirror_addr, -0.7, "bad-status");
            if log_cf {
                eprintln!(
                    "[mod-update][cf] {}接口状态异常: url={} status={} selected_ip={} remote={} elapsed={}ms headers=[{}]",
                    candidate_kind,
                    candidate,
                    status,
                    mirror_addr.map(|addr| addr.ip().to_string()).unwrap_or_else(|| "-".to_string()),
                    resp.remote_addr().map(|addr| addr.to_string()).unwrap_or_else(|| "-".to_string()),
                    started.elapsed().as_millis(),
                    header_summary.as_deref().unwrap_or("")
                );
            }
            continue;
        }
        if log_cf {
            eprintln!(
                "[mod-update][cf] {}接口响应头: url={} status={} selected_ip={} remote={} elapsed={}ms headers=[{}]",
                candidate_kind,
                candidate,
                status,
                mirror_addr.map(|addr| addr.ip().to_string()).unwrap_or_else(|| "-".to_string()),
                resp.remote_addr().map(|addr| addr.to_string()).unwrap_or_else(|| "-".to_string()),
                started.elapsed().as_millis(),
                header_summary.as_deref().unwrap_or("")
            );
        }
        let Some(json) = mod_api_read_json_body(
            resp,
            candidate_kind,
            &candidate,
            started,
            header_summary.as_deref().unwrap_or(""),
            log_cf,
        ) else {
            record_curseforge_mirror_addr(mirror_addr, -1.0, "body-or-json-failed");
            continue;
        };
        record_curseforge_mirror_addr(mirror_addr, 0.5, "json-ok");
        let Some(items) = json_array_at_path(&json, array_path) else {
            if log_cf {
                eprintln!(
                    "[mod-update][cf] {}接口缺少字段: url={} path={} elapsed={}ms",
                    candidate_kind,
                    candidate,
                    array_label,
                    started.elapsed().as_millis()
                );
            }
            continue;
        };
        let result_items = items.len();
        if result_items > 0 {
            if log_cf {
                eprintln!(
                    "[mod-update][cf] {}接口命中结果: url={} path={} count={} elapsed={}ms",
                    candidate_kind,
                    candidate,
                    array_label,
                    result_items,
                    started.elapsed().as_millis()
                );
            }
            return Some(json);
        }
        if log_cf {
            eprintln!(
                "[mod-update][cf] {}接口返回空结果: url={} path={} elapsed={}ms",
                candidate_kind,
                candidate,
                array_label,
                started.elapsed().as_millis()
            );
        }
        if empty_result.is_none() {
            empty_result = Some(json);
        }
    }
    if log_cf {
        if empty_result.is_some() {
            eprintln!(
                "[mod-update][cf] 所有候选接口都返回空结果: api={} items={}",
                url, request_items
            );
        } else {
            eprintln!(
                "[mod-update][cf] 所有候选接口都失败: api={} items={}",
                url, request_items
            );
        }
    }
    empty_result
}

fn mod_api_post_json_prefer_non_empty_object(
    http: &reqwest::blocking::Client,
    url: &str,
    body: &serde_json::Value,
    curseforge_key: bool,
) -> Option<serde_json::Value> {
    let mut empty_result = None;
    let log_cf = should_log_curseforge_api(curseforge_key, url);
    let request_items = mod_api_body_items(body);
    for (candidate, timeout) in mod_api_candidates(url, request_items) {
        let candidate_kind = mod_api_candidate_kind(&candidate);
        let mirror_addr = if is_curseforge_api_mirror(&candidate) {
            select_curseforge_mirror_addr()
        } else {
            None
        };
        let override_client = if let Some(addr) = mirror_addr {
            match build_curseforge_mirror_client(addr) {
                Ok(client) => Some(client),
                Err(err) => {
                    eprintln!(
                        "[mod-update][cf] 镜像 IP 客户端创建失败: ip={} error={}",
                        addr.ip(),
                        err
                    );
                    record_curseforge_mirror_addr(Some(addr), -1.0, "client-build-failed");
                    continue;
                }
            }
        } else {
            None
        };
        let request_http = override_client.as_ref().unwrap_or(http);
        let mut request = request_http
            .post(&candidate)
            .timeout(timeout)
            .header("Accept", "application/json")
            .json(body);
        if curseforge_key {
            request = request
                .version(reqwest::Version::HTTP_11)
                .header("Connection", "close");
        }
        if curseforge_key && !is_curseforge_api_mirror(&candidate) {
            request = request.header("x-api-key", &crate::instance::cf_api_key());
        }
        let started = Instant::now();
        let resp = match request.send() {
            Ok(resp) => resp,
            Err(_) => {
                record_curseforge_mirror_addr(mirror_addr, -1.0, "send-failed");
                continue;
            }
        };
        let status = resp.status();
        let header_summary = if log_cf {
            Some(response_header_summary(&resp))
        } else {
            None
        };
        if !status.is_success() {
            record_curseforge_mirror_addr(mirror_addr, -0.7, "bad-status");
            continue;
        };
        if log_cf {
            eprintln!(
                "[mod-update][cf] {}接口响应头: url={} status={} selected_ip={} remote={} elapsed={}ms headers=[{}]",
                candidate_kind,
                candidate,
                status,
                mirror_addr.map(|addr| addr.ip().to_string()).unwrap_or_else(|| "-".to_string()),
                resp.remote_addr().map(|addr| addr.to_string()).unwrap_or_else(|| "-".to_string()),
                started.elapsed().as_millis(),
                header_summary.as_deref().unwrap_or("")
            );
        }
        let Some(json) = mod_api_read_json_body(
            resp,
            candidate_kind,
            &candidate,
            started,
            header_summary.as_deref().unwrap_or(""),
            log_cf,
        ) else {
            record_curseforge_mirror_addr(mirror_addr, -1.0, "body-or-json-failed");
            continue;
        };
        record_curseforge_mirror_addr(mirror_addr, 0.5, "json-ok");
        let Some(map) = json.as_object() else {
            continue;
        };
        if !map.is_empty() {
            return Some(json);
        }
        if empty_result.is_none() {
            empty_result = Some(json);
        }
    }
    empty_result
}

fn json_array_at_path<'a>(
    json: &'a serde_json::Value,
    path: &[&str],
) -> Option<&'a Vec<serde_json::Value>> {
    let mut current = json;
    for key in path {
        current = current.get(*key)?;
    }
    current.as_array()
}

fn fill_missing_fingerprint_parallel(files: &mut [LocalModFile], indexes: &[usize]) {
    let jobs: Vec<(usize, PathBuf)> = indexes
        .iter()
        .copied()
        .filter(|index| {
            files[*index].fingerprint.is_none()
                || files[*index].sha1.is_none()
                || files[*index].sha512.is_none()
        })
        .map(|index| (index, files[index].path.clone()))
        .collect();
    for batch in jobs.chunks(UPDATE_HASH_CONCURRENCY) {
        let mut handles = Vec::new();
        for (index, path) in batch.iter().cloned() {
            handles.push(std::thread::spawn(move || {
                let (sha1, sha512, fingerprint, _size) =
                    crate::modpack_export::hash_update_candidate(&path).ok()?;
                Some((index, sha1, sha512, fingerprint))
            }));
        }
        for handle in handles {
            let Ok(Some((index, sha1, sha512, fingerprint))) = handle.join() else {
                continue;
            };
            if files[index].sha1.is_none() {
                files[index].sha1 = Some(sha1);
            }
            if files[index].sha512.is_none() {
                files[index].sha512 = Some(sha512);
            }
            files[index].fingerprint = Some(fingerprint);
        }
    }
}

fn refresh_online_mod_data_parallel(
    app_handle: &tauri::AppHandle,
    game_root: &Path,
    safe_name: &str,
    name: &str,
    http: &reqwest::blocking::Client,
    files: &mut [LocalModFile],
    changed_only: bool,
    force_missing_recheck: bool,
    should_refresh_sources: bool,
    should_check_updates: bool,
    mc_version: &str,
    loader: &str,
    checked_at_override: Option<u64>,
) -> Result<Vec<CachedUpdateDownloadPlan>, String> {
    if should_refresh_sources || should_check_updates {
        let indexes: Vec<usize> = (0..files.len()).collect();
        fill_missing_fingerprint_parallel(files, &indexes);
    }

    let lookup_files = files
        .iter()
        .enumerate()
        .map(|(index, file)| SourceLookupFile {
            index,
            file_name: file.file_name.clone(),
            enabled: file.enabled,
            sha1: file.sha1.clone(),
            fingerprint: file.fingerprint,
            modrinth: file.sources.modrinth.clone(),
            curseforge: file.sources.curseforge,
            modrinth_checked: file.modrinth_checked,
            curseforge_checked: file.curseforge_checked,
            changed: file.changed,
        })
        .collect::<Vec<_>>();

    let (tx, rx) = std::sync::mpsc::channel();

    let mr_tx = tx.clone();
    let mr_http = http.clone();
    let mr_files = lookup_files.clone();
    let mr_mc_version = mc_version.to_string();
    let mr_loader = loader.to_string();
    let mr_handle = std::thread::spawn(move || {
        refresh_modrinth_worker(
            &mr_http,
            mr_files,
            changed_only,
            force_missing_recheck,
            should_refresh_sources,
            should_check_updates,
            &mr_mc_version,
            &mr_loader,
            mr_tx.clone(),
        );
        let _ = mr_tx.send(OnlineRefreshEvent::Done);
    });

    let cf_tx = tx.clone();
    let cf_http = http.clone();
    let cf_files = lookup_files;
    let cf_mc_version = mc_version.to_string();
    let cf_loader = loader.to_string();
    let cf_handle = std::thread::spawn(move || {
        refresh_curseforge_worker(
            &cf_http,
            cf_files,
            changed_only,
            force_missing_recheck,
            should_refresh_sources,
            should_check_updates,
            &cf_mc_version,
            &cf_loader,
            cf_tx.clone(),
        );
        let _ = cf_tx.send(OnlineRefreshEvent::Done);
    });
    drop(tx);

    let mut plans = Vec::new();
    let mut done_count = 0;
    while done_count < 2 {
        let Ok(event) = rx.recv() else {
            break;
        };
        match event {
            OnlineRefreshEvent::Done => {
                done_count += 1;
            }
            event => {
                let update_plan_changed = matches!(event, OnlineRefreshEvent::UpdatePlan(_));
                let changed = apply_online_refresh_event(files, event, &mut plans);
                if !changed {
                    continue;
                }
                dedupe_update_lookup_plans(&mut plans);
                if update_plan_changed {
                    save_cached_update_plans(
                        game_root,
                        safe_name,
                        plans
                            .iter()
                            .cloned()
                            .map(cached_update_download_plan)
                            .collect(),
                    )?;
                }
                save_and_emit_mod_update_partial(
                    app_handle,
                    game_root,
                    safe_name,
                    name,
                    mc_version,
                    loader,
                    files,
                    &plans,
                    checked_at_override,
                )?;
            }
        }
    }

    let _ = mr_handle.join();
    let _ = cf_handle.join();

    dedupe_update_lookup_plans(&mut plans);
    Ok(plans.into_iter().map(cached_update_download_plan).collect())
}

fn apply_online_refresh_event(
    files: &mut [LocalModFile],
    event: OnlineRefreshEvent,
    plans: &mut Vec<UpdateLookupPlan>,
) -> bool {
    match event {
        OnlineRefreshEvent::ModrinthLookup {
            index,
            source,
            downloads,
        } => {
            if let Some(file) = files.get_mut(index) {
                file.modrinth_checked = true;
                if let Some(source) = source {
                    file.sources.modrinth = Some(source);
                }
                if !downloads.is_empty() {
                    file.mrpack_source = Some("modrinth".to_string());
                    file.mrpack_downloads = downloads;
                }
                return true;
            }
            false
        }
        OnlineRefreshEvent::CurseForgeLookup {
            index,
            source,
            downloads,
        } => {
            if let Some(file) = files.get_mut(index) {
                file.curseforge_checked = true;
                if let Some(source) = source {
                    file.sources.curseforge = Some(source);
                }
                if file.sources.modrinth.is_none()
                    && file.mrpack_downloads.is_empty()
                    && !downloads.is_empty()
                {
                    file.mrpack_source = Some("curseforge".to_string());
                }
                return true;
            }
            false
        }
        OnlineRefreshEvent::UpdatePlan(plan) => {
            plans.push(plan);
            true
        }
        OnlineRefreshEvent::Done => false,
    }
}

fn save_and_emit_mod_update_partial(
    app_handle: &tauri::AppHandle,
    game_root: &Path,
    safe_name: &str,
    name: &str,
    mc_version: &str,
    loader: &str,
    files: &[LocalModFile],
    plans: &[UpdateLookupPlan],
    checked_at_override: Option<u64>,
) -> Result<(), String> {
    let mut updates = plans
        .iter()
        .map(|plan| plan.update.clone())
        .collect::<Vec<_>>();
    sort_update_entries(&mut updates);
    let mut cache = build_update_cache(name, mc_version, loader, files, updates);
    if let Some(checked_at) = checked_at_override {
        cache.checked_at = checked_at;
    }
    cache.refreshing = true;
    save_update_cache(game_root, safe_name, &cache)?;
    let view = cache_to_view_without_changed_filter(
        name,
        mc_version,
        loader,
        Some(cache),
        files,
        true,
        true,
        "正在合并 Mod 更新结果...".to_string(),
    );
    let event = ModUpdateCacheEvent {
        name: name.to_string(),
        mc_version: mc_version.to_string(),
        loader: loader.to_string(),
        status: "partial".to_string(),
        view: Some(view),
        message: "正在合并 Mod 更新结果...".to_string(),
    };
    let _ = app_handle.emit("mod-update-cache", event);
    Ok(())
}

fn refresh_modrinth_worker(
    http: &reqwest::blocking::Client,
    mut files: Vec<SourceLookupFile>,
    changed_only: bool,
    force_missing_recheck: bool,
    should_refresh_sources: bool,
    should_check_updates: bool,
    mc_version: &str,
    loader: &str,
    tx: std::sync::mpsc::Sender<OnlineRefreshEvent>,
) {
    if should_refresh_sources {
        let candidates = files
            .iter()
            .filter(|file| !changed_only || file.changed)
            .filter(|file| file.modrinth.is_none())
            .filter(|file| force_missing_recheck || !file.modrinth_checked)
            .filter_map(|file| file.sha1.as_ref().map(|sha1| (file.index, sha1.clone())))
            .collect::<Vec<_>>();
        let hashes = candidates
            .iter()
            .map(|(_, sha1)| sha1.clone())
            .collect::<Vec<_>>();
        if let Some(matches) = lookup_modrinth_versions_by_sha1_batch(http, &hashes) {
            for (index, sha1) in candidates {
                let mut source = None;
                let mut downloads = Vec::new();
                if let Some(version) = matches.get(&sha1) {
                    let next_source = ModrinthSource {
                        project_id: version.project_id.clone(),
                        version_id: version.version_id.clone(),
                    };
                    if let Some(file) = files.iter_mut().find(|file| file.index == index) {
                        file.modrinth = Some(next_source.clone());
                    }
                    source = Some(next_source);
                    downloads = version.downloads.clone();
                }
                if tx
                    .send(OnlineRefreshEvent::ModrinthLookup {
                        index,
                        source,
                        downloads,
                    })
                    .is_err()
                {
                    return;
                }
            }
        }
    }
    if should_check_updates {
        for plan in lookup_modrinth_update_plans(http, &files, mc_version, loader) {
            if tx.send(OnlineRefreshEvent::UpdatePlan(plan)).is_err() {
                return;
            }
        }
    }
}

fn refresh_curseforge_worker(
    http: &reqwest::blocking::Client,
    mut files: Vec<SourceLookupFile>,
    changed_only: bool,
    force_missing_recheck: bool,
    should_refresh_sources: bool,
    should_check_updates: bool,
    mc_version: &str,
    loader: &str,
    tx: std::sync::mpsc::Sender<OnlineRefreshEvent>,
) {
    if should_refresh_sources {
        let candidates = files
            .iter()
            .filter(|file| !changed_only || file.changed)
            .filter(|file| file.curseforge.is_none())
            .filter(|file| force_missing_recheck || !file.curseforge_checked)
            .filter_map(|file| {
                Some(HashedModFile {
                    index: file.index,
                    sha1: file.sha1.clone()?,
                    fingerprint: file.fingerprint,
                })
            })
            .collect::<Vec<_>>();
        if let Some(result) = lookup_curseforge_mods_by_fingerprints_fast(http, &candidates) {
            for item in candidates {
                if item
                    .fingerprint
                    .map(|fingerprint| !result.checked_fingerprints.contains(&fingerprint))
                    .unwrap_or(false)
                {
                    continue;
                }
                let mut source = None;
                let mut downloads = Vec::new();
                if let Some(matched) = result.matches.get(&item.sha1) {
                    let next_source = CurseForgeSource {
                        project_id: matched.project_id,
                        file_id: matched.file_id,
                    };
                    if let Some(file) = files.iter_mut().find(|file| file.index == item.index) {
                        file.curseforge = Some(next_source);
                    }
                    source = Some(next_source);
                    downloads = matched.downloads.clone();
                }
                if tx
                    .send(OnlineRefreshEvent::CurseForgeLookup {
                        index: item.index,
                        source,
                        downloads,
                    })
                    .is_err()
                {
                    return;
                }
            }
        } else {
            eprintln!(
                "[mod-update][cf] 指纹查询整轮失败，跳过本次 CF 来源写回: candidates={}",
                candidates.len()
            );
        }
    }
    if should_check_updates {
        for plan in lookup_curseforge_update_plans(http, &files, mc_version, loader) {
            if tx.send(OnlineRefreshEvent::UpdatePlan(plan)).is_err() {
                return;
            }
        }
    }
}

fn lookup_modrinth_update_plans(
    http: &reqwest::blocking::Client,
    files: &[SourceLookupFile],
    mc_version: &str,
    loader: &str,
) -> Vec<UpdateLookupPlan> {
    let mut by_hash: HashMap<String, Vec<&SourceLookupFile>> = HashMap::new();
    for file in files {
        if file.modrinth.is_none() {
            continue;
        }
        if let Some(sha1) = file.sha1.as_deref() {
            by_hash.entry(sha1.to_string()).or_default().push(file);
        }
    }
    let hashes = by_hash.keys().cloned().collect::<Vec<_>>();
    let latest_by_hash =
        lookup_modrinth_update_downloads_by_sha1_batch(http, &hashes, mc_version, loader);
    let mut plans = Vec::new();
    for (hash, latest) in latest_by_hash {
        let Some(matched_files) = by_hash.get(&hash) else {
            continue;
        };
        for file in matched_files {
            let Some(source) = file.modrinth.as_ref() else {
                continue;
            };
            if latest.info.version_id == source.version_id
                || latest.sha1.as_deref() == file.sha1.as_deref()
            {
                continue;
            }
            let update = update_info(
                &file.file_name,
                file.enabled,
                "modrinth",
                &source.project_id,
                &source.version_id,
                latest.info.clone(),
            );
            plans.push(UpdateLookupPlan {
                update,
                downloads: mod_update_download_candidates(latest.downloads.clone()),
                sha1: latest.sha1.clone(),
                curseforge_file_id: None,
                curseforge_file_name: None,
                source_project_id: source.project_id.clone(),
                source_version_id: latest.info.version_id.clone(),
                source: "modrinth".to_string(),
            });
        }
    }
    plans
}

fn lookup_modrinth_update_downloads_by_sha1_batch(
    http: &reqwest::blocking::Client,
    hashes: &[String],
    mc_version: &str,
    loader: &str,
) -> HashMap<String, VersionDownloadInfo> {
    let hashes = unique_strings(hashes);
    if hashes.is_empty() {
        return HashMap::new();
    }
    lookup_modrinth_update_downloads_chunk(http, hashes, mc_version, loader)
}

fn lookup_modrinth_update_downloads_chunk(
    http: &reqwest::blocking::Client,
    hashes: Vec<String>,
    mc_version: &str,
    loader: &str,
) -> HashMap<String, VersionDownloadInfo> {
    let mut out = HashMap::new();
    let mut body = serde_json::json!({
        "hashes": hashes,
        "algorithm": "sha1",
    });
    if !mc_version.trim().is_empty() {
        body["game_versions"] = serde_json::json!([mc_version]);
    }
    if !loader.trim().is_empty() && loader != "vanilla" {
        body["loaders"] = serde_json::json!([loader]);
    }
    let Some(json) = mod_api_post_json_prefer_non_empty_object(
        http,
        "https://api.modrinth.com/v2/version_files/update",
        &body,
        false,
    ) else {
        return out;
    };
    let Some(map) = json.as_object() else {
        return out;
    };
    for (hash, version) in map {
        let Some(info) = modrinth_version_to_download_info(version, loader) else {
            continue;
        };
        out.insert(hash.to_string(), info);
    }
    out
}

fn lookup_curseforge_update_plans(
    http: &reqwest::blocking::Client,
    files: &[SourceLookupFile],
    mc_version: &str,
    loader: &str,
) -> Vec<UpdateLookupPlan> {
    let mut project_ids = HashSet::new();
    for file in files {
        if let Some(source) = file.curseforge.as_ref() {
            project_ids.insert(source.project_id);
        }
    }
    if project_ids.is_empty() {
        return Vec::new();
    }
    let ids = project_ids.into_iter().collect::<Vec<_>>();
    let projects = lookup_curseforge_projects_batch(http, &ids);
    let mut update_file_ids: HashMap<u32, Vec<&SourceLookupFile>> = HashMap::new();
    for file in files {
        let Some(source) = file.curseforge.as_ref() else {
            continue;
        };
        let Some(project) = projects.get(&source.project_id) else {
            continue;
        };
        let Some(indexes) = project["latestFilesIndexes"].as_array() else {
            continue;
        };
        let latest = indexes
            .iter()
            .filter(|item| curseforge_index_matches(item, mc_version, loader))
            .filter_map(|item| {
                let id = item["fileId"]
                    .as_u64()
                    .and_then(|value| u32::try_from(value).ok())?;
                Some(id)
            })
            .max();
        let Some(latest_file_id) = latest else {
            continue;
        };
        if latest_file_id <= source.file_id {
            continue;
        }
        update_file_ids
            .entry(latest_file_id)
            .or_default()
            .push(file);
    }
    if update_file_ids.is_empty() {
        return Vec::new();
    }
    let file_ids = update_file_ids.keys().copied().collect::<Vec<_>>();
    let latest_files = lookup_curseforge_files_by_ids_batch(http, &file_ids);
    let mut plans = Vec::new();
    for (file_id, matched_files) in update_file_ids {
        let Some(latest) = latest_files.get(&file_id) else {
            continue;
        };
        for file in matched_files {
            let Some(source) = file.curseforge.as_ref() else {
                continue;
            };
            if latest.sha1.as_deref() == file.sha1.as_deref() {
                continue;
            }
            let update = update_info(
                &file.file_name,
                file.enabled,
                "curseforge",
                &format!("cf_{}", source.project_id),
                &source.file_id.to_string(),
                latest.info.clone(),
            );
            plans.push(UpdateLookupPlan {
                update,
                downloads: latest.downloads.clone(),
                sha1: latest.sha1.clone(),
                curseforge_file_id: Some(file_id),
                curseforge_file_name: Some(latest.info.file_name.clone()),
                source_project_id: source.project_id.to_string(),
                source_version_id: latest.info.version_id.clone(),
                source: "curseforge".to_string(),
            });
        }
    }
    plans
}

fn lookup_curseforge_files_by_ids_batch(
    http: &reqwest::blocking::Client,
    ids: &[u32],
) -> HashMap<u32, VersionDownloadInfo> {
    let ids = unique_u32(ids);
    if ids.is_empty() {
        return HashMap::new();
    }
    lookup_curseforge_files_chunk(http, ids)
}

fn lookup_curseforge_files_chunk(
    http: &reqwest::blocking::Client,
    file_ids: Vec<u32>,
) -> HashMap<u32, VersionDownloadInfo> {
    let mut out = HashMap::new();
    let body = serde_json::json!({ "fileIds": file_ids });
    let Some(json) = mod_api_post_json_prefer_non_empty_array(
        http,
        "https://api.curseforge.com/v1/mods/files",
        &body,
        true,
        &["data"],
    ) else {
        return out;
    };
    let Some(items) = json["data"].as_array() else {
        return out;
    };
    for item in items {
        let Some(file_id) = item["id"]
            .as_u64()
            .and_then(|value| u32::try_from(value).ok())
        else {
            continue;
        };
        let Some(info) = curseforge_file_to_download_info(item) else {
            continue;
        };
        out.insert(file_id, info);
    }
    out
}

fn merge_update_lookup_plan(
    by_file: &mut HashMap<String, UpdateLookupPlan>,
    plan: UpdateLookupPlan,
) {
    let key = mod_cache_key(&plan.update.file_name);
    match by_file.get_mut(&key) {
        Some(existing) => {
            let mut downloads = existing.downloads.clone();
            crate::modpack::download_mirror::append_unique_urls(
                &mut downloads,
                plan.downloads.clone(),
            );
            let curseforge_file_id = existing.curseforge_file_id.or(plan.curseforge_file_id);
            let curseforge_file_name = existing
                .curseforge_file_name
                .clone()
                .or_else(|| plan.curseforge_file_name.clone());
            let existing_update = existing.update.clone();
            let incoming_update = plan.update.clone();
            let mut selected = if prefer_update(&existing_update, &incoming_update) {
                existing.clone()
            } else {
                plan
            };
            selected.downloads = downloads;
            if selected.curseforge_file_id.is_none() {
                selected.curseforge_file_id = curseforge_file_id;
            }
            if selected.curseforge_file_name.is_none() {
                selected.curseforge_file_name = curseforge_file_name;
            }
            merge_update_links(&mut selected.update, &existing_update);
            merge_update_links(&mut selected.update, &incoming_update);
            *existing = selected;
        }
        None => {
            by_file.insert(key, plan);
        }
    }
}

fn merge_update_links(target: &mut ModUpdateInfo, other: &ModUpdateInfo) {
    if target.mr_url.is_empty() && !other.mr_url.is_empty() {
        target.mr_url = other.mr_url.clone();
    }
    if target.cf_url.is_empty() && !other.cf_url.is_empty() {
        target.cf_url = other.cf_url.clone();
    }
}

fn dedupe_update_lookup_plans(plans: &mut Vec<UpdateLookupPlan>) {
    let mut by_file: HashMap<String, UpdateLookupPlan> = HashMap::new();
    for plan in plans.drain(..) {
        merge_update_lookup_plan(&mut by_file, plan);
    }
    plans.extend(by_file.into_values());
    plans.sort_by(|a, b| {
        a.update
            .display_name
            .to_lowercase()
            .cmp(&b.update.display_name.to_lowercase())
    });
}

fn cached_update_download_plan(plan: UpdateLookupPlan) -> CachedUpdateDownloadPlan {
    CachedUpdateDownloadPlan {
        key: mod_cache_key(&plan.update.file_name),
        update: plan.update,
        downloads: plan.downloads,
        sha1: plan.sha1,
        curseforge_file_id: plan.curseforge_file_id,
        curseforge_file_name: plan.curseforge_file_name,
        source_project_id: plan.source_project_id,
        source_version_id: plan.source_version_id,
        source: plan.source,
        checked_at_ms: now_ms(),
    }
}

fn lookup_curseforge_projects_batch(
    http: &reqwest::blocking::Client,
    ids: &[u32],
) -> HashMap<u32, serde_json::Value> {
    let mut projects = HashMap::new();
    let ids = unique_u32(ids);
    if ids.is_empty() {
        return projects;
    }
    for (project_id, item) in lookup_curseforge_projects_chunk(http, ids) {
        projects.insert(project_id, item);
    }
    projects
}

fn lookup_curseforge_projects_chunk(
    http: &reqwest::blocking::Client,
    mod_ids: Vec<u32>,
) -> Vec<(u32, serde_json::Value)> {
    let body = serde_json::json!({ "modIds": mod_ids, "filterPcOnly": true });
    let Some(json) = mod_api_post_json_prefer_non_empty_array(
        http,
        "https://api.curseforge.com/v1/mods",
        &body,
        true,
        &["data"],
    ) else {
        return Vec::new();
    };
    let Some(items) = json["data"].as_array() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for item in items {
        if let Some(project_id) = item["id"]
            .as_u64()
            .and_then(|value| u32::try_from(value).ok())
        {
            out.push((project_id, item.clone()));
        }
    }
    out
}

fn modrinth_version_to_info(
    version: &serde_json::Value,
    preferred_loader: &str,
) -> Option<OnlineModVersionInfo> {
    let files = version["files"].as_array()?;
    let file = files
        .iter()
        .find(|item| item["primary"].as_bool() == Some(true))
        .or_else(|| files.first())?;
    let loaders = json_string_array(&version["loaders"]);
    let game_versions = json_string_array(&version["game_versions"]);
    let selected_loader = if !preferred_loader.is_empty()
        && preferred_loader != "vanilla"
        && loaders
            .iter()
            .any(|item| item.eq_ignore_ascii_case(preferred_loader))
    {
        preferred_loader.to_string()
    } else {
        loaders.first().cloned().unwrap_or_default()
    };
    Some(OnlineModVersionInfo {
        version_id: version["id"].as_str()?.to_string(),
        version_name: version["version_number"]
            .as_str()
            .unwrap_or("未命名版本")
            .to_string(),
        mc_versions: game_versions.join(", "),
        mc_version: game_versions.first().cloned().unwrap_or_default(),
        loaders: loaders.join(", "),
        loader: selected_loader,
        file_name: file["filename"].as_str().unwrap_or("").to_string(),
        file_size: file["size"].as_u64().unwrap_or(0),
        date: version["date_published"]
            .as_str()
            .map(short_date)
            .unwrap_or_default(),
        source: "modrinth".to_string(),
    })
}

fn modrinth_version_to_download_info(
    version: &serde_json::Value,
    preferred_loader: &str,
) -> Option<VersionDownloadInfo> {
    let files = version["files"].as_array()?;
    let file = files
        .iter()
        .find(|item| item["primary"].as_bool() == Some(true))
        .or_else(|| files.first())?;
    let info = modrinth_version_to_info(version, preferred_loader)?;
    let mut downloads = Vec::new();
    if let Some(url) = file["url"].as_str() {
        push_unique_mrpack_url(&mut downloads, url.to_string());
    }
    Some(VersionDownloadInfo {
        info,
        downloads,
        sha1: file["hashes"]["sha1"]
            .as_str()
            .map(|value| value.to_string()),
    })
}

fn curseforge_file_to_download_info(file: &serde_json::Value) -> Option<VersionDownloadInfo> {
    let file_id = file["id"]
        .as_u64()
        .and_then(|value| u32::try_from(value).ok())?;
    let file_name = file["fileName"]
        .as_str()
        .or_else(|| file["file_name"].as_str())
        .unwrap_or("")
        .to_string();
    if file_name.is_empty() {
        return None;
    }
    let game_versions = json_string_array(&file["gameVersions"]);
    let api_download_url = file["downloadUrl"].as_str().unwrap_or("");
    let downloads = curseforge_mrpack_download_candidates(file_id, &file_name, api_download_url);
    let sha1 = crate::modpack_sources::sha1_from_curseforge_hashes(&file["hashes"]);
    Some(VersionDownloadInfo {
        info: OnlineModVersionInfo {
            version_id: file_id.to_string(),
            version_name: file["displayName"]
                .as_str()
                .filter(|value| !value.trim().is_empty())
                .unwrap_or(&file_name)
                .to_string(),
            mc_versions: game_versions.join(", "),
            mc_version: game_versions.first().cloned().unwrap_or_default(),
            loaders: String::new(),
            loader: String::new(),
            file_name,
            file_size: file["fileLength"].as_u64().unwrap_or(0),
            date: file["fileDate"]
                .as_str()
                .map(short_date)
                .unwrap_or_default(),
            source: "curseforge".to_string(),
        },
        downloads,
        sha1,
    })
}

fn json_string_array(value: &serde_json::Value) -> Vec<String> {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str())
                .filter(|item| !item.trim().is_empty())
                .map(|item| item.trim().to_string())
                .collect()
        })
        .unwrap_or_default()
}

fn short_date(value: &str) -> String {
    value.chars().take(10).collect()
}

fn curseforge_index_matches(item: &serde_json::Value, mc_version: &str, loader: &str) -> bool {
    if !mc_version.trim().is_empty()
        && item["gameVersion"]
            .as_str()
            .is_some_and(|value| value != mc_version)
    {
        return false;
    }
    let Some(expected_loader) = curseforge_loader_type(loader) else {
        return false;
    };
    item["modLoader"].as_u64() == Some(expected_loader)
}

fn curseforge_loader_type(loader: &str) -> Option<u64> {
    match loader.to_ascii_lowercase().as_str() {
        "forge" => Some(1),
        "fabric" => Some(4),
        "quilt" => Some(5),
        "neoforge" => Some(6),
        _ => None,
    }
}

fn update_info(
    file_name: &str,
    enabled: bool,
    source: &str,
    project_id: &str,
    current_id: &str,
    latest: OnlineModVersionInfo,
) -> ModUpdateInfo {
    ModUpdateInfo {
        file_name: file_name.to_string(),
        display_name: display_name_from_file(file_name),
        enabled,
        source: source.to_string(),
        project_id: project_id.to_string(),
        current_id: current_id.to_string(),
        latest_version_id: latest.version_id,
        latest_version_name: latest.version_name,
        latest_file_name: latest.file_name,
        mc_versions: latest.mc_versions,
        loaders: latest.loaders,
        date: latest.date,
        file_size: latest.file_size,
        mr_url: if source == "modrinth" {
            format!("https://modrinth.com/mod/{}", project_id)
        } else {
            String::new()
        },
        cf_url: if source == "curseforge" {
            project_id
                .strip_prefix("cf_")
                .map(|id| format!("https://www.curseforge.com/projects/{}", id))
                .unwrap_or_default()
        } else {
            String::new()
        },
    }
}

fn dedupe_update_entries(updates: &mut Vec<ModUpdateInfo>) {
    let mut by_file: HashMap<String, ModUpdateInfo> = HashMap::new();
    for update in updates.drain(..) {
        let key = mod_cache_key(&update.file_name);
        if let Some(existing) = by_file.get_mut(&key) {
            let existing_update = existing.clone();
            let incoming_update = update.clone();
            let mut selected = if prefer_update(&existing_update, &incoming_update) {
                existing_update
            } else {
                update
            };
            merge_update_links(&mut selected, existing);
            merge_update_links(&mut selected, &incoming_update);
            *existing = selected;
        } else {
            by_file.insert(key, update);
        }
    }
    updates.extend(by_file.into_values());
}

fn sort_update_entries(updates: &mut Vec<ModUpdateInfo>) {
    dedupe_update_entries(updates);
    updates.sort_by(|a, b| {
        a.display_name
            .to_lowercase()
            .cmp(&b.display_name.to_lowercase())
    });
}

fn prefer_update(existing: &ModUpdateInfo, candidate: &ModUpdateInfo) -> bool {
    if !existing.date.is_empty() && candidate.date.is_empty() {
        return true;
    }
    if !candidate.date.is_empty() && existing.date.is_empty() {
        return false;
    }
    if !existing.date.is_empty() && !candidate.date.is_empty() && existing.date != candidate.date {
        return existing.date >= candidate.date;
    }
    existing.latest_version_id >= candidate.latest_version_id
}

fn lookup_modrinth_versions_by_sha1_batch(
    http: &reqwest::blocking::Client,
    hashes: &[String],
) -> Option<HashMap<String, ModrinthVersionMatch>> {
    let hashes = unique_strings(hashes);
    if hashes.is_empty() {
        return Some(HashMap::new());
    }
    lookup_modrinth_versions_chunk(http, hashes)
}

fn lookup_modrinth_versions_chunk(
    http: &reqwest::blocking::Client,
    hashes: Vec<String>,
) -> Option<HashMap<String, ModrinthVersionMatch>> {
    let mut out = HashMap::new();
    let body = serde_json::json!({
        "hashes": hashes,
        "algorithm": "sha1",
    });
    let Some(json) = mod_api_post_json_prefer_non_empty_object(
        http,
        "https://api.modrinth.com/v2/version_files",
        &body,
        false,
    ) else {
        return None;
    };
    let Some(map) = json.as_object() else {
        return None;
    };
    for (hash, version) in map {
        let Some(project_id) = version["project_id"].as_str() else {
            continue;
        };
        let Some(version_id) = version["id"].as_str() else {
            continue;
        };
        let mut downloads = Vec::new();
        if let Some(files) = version["files"].as_array() {
            for file in files {
                if file["hashes"]["sha1"].as_str() == Some(hash.as_str()) {
                    if let Some(url) = file["url"].as_str() {
                        push_unique_mrpack_url(&mut downloads, url.to_string());
                    }
                }
            }
        }
        out.insert(
            hash.to_string(),
            ModrinthVersionMatch {
                project_id: project_id.to_string(),
                version_id: version_id.to_string(),
                downloads,
            },
        );
    }
    Some(out)
}

fn lookup_curseforge_mods_by_fingerprints_fast(
    http: &reqwest::blocking::Client,
    candidates: &[HashedModFile],
) -> Option<CurseForgeFingerprintLookupResult> {
    let mut out = HashMap::new();
    let mut fp_to_sha = HashMap::new();
    let mut fingerprints = Vec::new();
    for item in candidates {
        let Some(fingerprint) = item.fingerprint else {
            continue;
        };
        fp_to_sha.insert(fingerprint, item.sha1.clone());
        fingerprints.push(fingerprint);
    }
    if fingerprints.is_empty() {
        return Some(CurseForgeFingerprintLookupResult {
            matches: out,
            checked_fingerprints: HashSet::new(),
        });
    }

    let mut matches_by_sha: HashMap<String, Vec<CurseForgeFingerprintMatch>> = HashMap::new();
    let fingerprints = unique_u32(&fingerprints);
    eprintln!(
        "[mod-update][cf] 开始指纹查询: candidates={} fingerprints={} mode=all",
        candidates.len(),
        fingerprints.len()
    );
    let Some(items) = lookup_curseforge_fingerprints_chunk(http, fingerprints.clone(), fp_to_sha)
    else {
        eprintln!(
            "[mod-update][cf] 指纹查询失败: fingerprints={}",
            fingerprints.len()
        );
        return None;
    };
    let checked_fingerprints = fingerprints.iter().copied().collect::<HashSet<_>>();
    for (sha1, matches) in items {
        matches_by_sha.entry(sha1).or_default().extend(matches);
    }

    for (sha1, mut matches) in matches_by_sha {
        // 指纹结果已经来自本地 mods 文件夹，避免再拉一次 /v1/mods 大响应只为确认 classId。
        if let Some(item) = matches.pop() {
            out.insert(sha1, item);
        }
    }
    eprintln!(
        "[mod-update][cf] 指纹查询完成: fingerprints={} matches={}",
        checked_fingerprints.len(),
        out.len()
    );
    Some(CurseForgeFingerprintLookupResult {
        matches: out,
        checked_fingerprints,
    })
}

fn lookup_curseforge_fingerprints_chunk(
    http: &reqwest::blocking::Client,
    fingerprints: Vec<u32>,
    fp_to_sha: HashMap<u32, String>,
) -> Option<HashMap<String, Vec<CurseForgeFingerprintMatch>>> {
    let mut out: HashMap<String, Vec<CurseForgeFingerprintMatch>> = HashMap::new();
    let body = serde_json::json!({ "fingerprints": fingerprints });
    let Some(json) = mod_api_post_json_prefer_non_empty_array(
        http,
        "https://api.curseforge.com/v1/fingerprints/432",
        &body,
        true,
        &["data", "exactMatches"],
    ) else {
        return None;
    };
    let Some(matches) = json["data"]["exactMatches"].as_array() else {
        return None;
    };
    for item in matches {
        let file = &item["file"];
        let sha1 = if let Some(sha1) =
            crate::modpack_sources::sha1_from_curseforge_hashes(&file["hashes"])
        {
            sha1
        } else {
            let Some(fingerprint) = file["fileFingerprint"]
                .as_u64()
                .and_then(|value| u32::try_from(value).ok())
            else {
                continue;
            };
            let Some(sha1) = fp_to_sha.get(&fingerprint).cloned() else {
                continue;
            };
            sha1
        };
        let project_id = file["modId"]
            .as_u64()
            .or_else(|| item["id"].as_u64())
            .and_then(|value| u32::try_from(value).ok());
        let file_id = file["id"]
            .as_u64()
            .and_then(|value| u32::try_from(value).ok());
        if let (Some(project_id), Some(file_id)) = (project_id, file_id) {
            let file_name = file["fileName"]
                .as_str()
                .or_else(|| file["file_name"].as_str())
                .unwrap_or("");
            let api_download_url = file["downloadUrl"].as_str().unwrap_or("");
            let downloads =
                curseforge_mrpack_download_candidates(file_id, file_name, api_download_url);
            out.entry(sha1)
                .or_default()
                .push(CurseForgeFingerprintMatch {
                    project_id,
                    file_id,
                    downloads,
                });
        }
    }
    Some(out)
}

fn is_mod_file(path: &PathBuf) -> bool {
    let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
        return false;
    };
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".jar") || lower.ends_with(".jar.disabled")
}

fn display_name_from_file(file_name: &str) -> String {
    file_name
        .trim_end_matches(".disabled")
        .trim_end_matches(".jar")
        .to_string()
}
