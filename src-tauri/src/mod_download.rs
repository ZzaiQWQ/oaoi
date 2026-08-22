use crate::downloader::{
    DownloadCandidate, DownloadEngineOptions, DownloadManager, DownloadRequest,
};
use crate::instance::{cf_api_key, resolve_game_dir, safe_path_name, version_dir};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::atomic::AtomicU64;

#[cfg(test)]
static MOD_DOWNLOAD_TMP_COUNTER: AtomicU64 = AtomicU64::new(0);
const MAX_DEPENDENCY_DEPTH: usize = 8;

#[derive(Clone, Default)]
struct ExpectedDownloadFile {
    size: Option<u64>,
    sha1: Option<String>,
}

struct OnlineModCancelGuard {
    name: String,
}

impl OnlineModCancelGuard {
    fn register(name: &str) -> Result<Self, String> {
        crate::instance::try_register_cancel(name)?;
        Ok(Self {
            name: name.to_string(),
        })
    }
}

impl Drop for OnlineModCancelGuard {
    fn drop(&mut self) {
        crate::instance::unregister_cancel(&self.name);
    }
}

#[derive(Default)]
struct ModDownloadTransaction {
    created_files: Vec<PathBuf>,
    committed: bool,
}

impl ModDownloadTransaction {
    fn record(&mut self, path: PathBuf) {
        self.created_files.push(path);
    }

    fn commit(&mut self) {
        self.committed = true;
        self.created_files.clear();
    }
}

impl Drop for ModDownloadTransaction {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        for path in self.created_files.iter().rev() {
            if let Err(error) = std::fs::remove_file(path) {
                if error.kind() != std::io::ErrorKind::NotFound {
                    eprintln!("[mod-download] 回滚文件失败 {}: {}", path.display(), error);
                }
            }
        }
    }
}

#[derive(Default)]
struct InstalledDependencyIndex {
    mod_ids: HashSet<String>,
    project_ids: HashSet<String>,
}

impl InstalledDependencyIndex {
    fn load(game_root: &Path, safe_name: &str, mods_dir: &Path, loader: &str) -> Self {
        let mut index = Self::default();
        for mod_id in crate::mod_analyzer::collect_installed_mod_ids(mods_dir, loader) {
            index.record_mod_id(&mod_id);
        }
        index
            .project_ids
            .extend(crate::mod_update::load_cached_installed_project_ids(
                game_root, safe_name,
            ));
        index
    }

    fn record_mod_id(&mut self, mod_id: &str) {
        self.mod_ids.extend(dependency_identity_keys(mod_id));
    }

    fn record_mod_ids(&mut self, mod_ids: &HashSet<String>) {
        for mod_id in mod_ids {
            self.record_mod_id(mod_id);
        }
    }

    fn record_project(&mut self, source: &str, project_id: &str) {
        let project_id = project_id.trim().to_ascii_lowercase();
        if !project_id.is_empty() {
            self.project_ids
                .insert(format!("{}:{}", source, project_id));
        }
    }

    fn has_project(&self, source: &str, project_id: &str) -> bool {
        self.project_ids.contains(&format!(
            "{}:{}",
            source,
            project_id.trim().to_ascii_lowercase()
        ))
    }

    fn has_mod_aliases(&self, aliases: &[String]) -> bool {
        aliases.iter().any(|alias| {
            dependency_identity_keys(alias)
                .iter()
                .any(|key| self.mod_ids.contains(key))
        })
    }

    fn overlaps_mod_ids(&self, mod_ids: &HashSet<String>) -> bool {
        mod_ids.iter().any(|mod_id| {
            dependency_identity_keys(mod_id)
                .iter()
                .any(|key| self.mod_ids.contains(key))
        })
    }
}

fn dependency_identity_keys(value: &str) -> Vec<String> {
    let exact = value.trim().to_ascii_lowercase();
    if exact.is_empty() {
        return Vec::new();
    }
    let compact = exact
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .collect::<String>();
    if compact.is_empty() || compact == exact {
        vec![exact]
    } else {
        vec![exact, compact]
    }
}

fn curseforge_file_mod_aliases(file: &serde_json::Value) -> Vec<String> {
    file["modules"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|module| module["name"].as_str())
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .collect()
}

fn modrinth_project_aliases(
    http: &reqwest::blocking::Client,
    project_id: &str,
    cancel_name: &str,
) -> Vec<String> {
    if ensure_not_cancelled(cancel_name).is_err() {
        return Vec::new();
    }
    let url = format!("https://api.modrinth.com/v2/project/{}", project_id);
    let Ok(response) = http
        .get(url)
        .timeout(std::time::Duration::from_secs(10))
        .send()
    else {
        return Vec::new();
    };
    if !response.status().is_success() {
        return Vec::new();
    }
    let Ok(project) = response.json::<serde_json::Value>() else {
        return Vec::new();
    };
    [project["slug"].as_str(), project["id"].as_str()]
        .into_iter()
        .flatten()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect()
}

fn modrinth_expected_file(file: &serde_json::Value) -> ExpectedDownloadFile {
    ExpectedDownloadFile {
        size: file["size"].as_u64().filter(|size| *size > 0),
        sha1: file["hashes"]["sha1"]
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
    }
}

fn curseforge_expected_file(file: &serde_json::Value) -> ExpectedDownloadFile {
    ExpectedDownloadFile {
        size: file["fileLength"].as_u64().filter(|size| *size > 0),
        sha1: crate::modpack_sources::sha1_from_curseforge_hashes(&file["hashes"]),
    }
}

fn validate_existing_download(
    path: &std::path::Path,
    file_name: &str,
    expected: &ExpectedDownloadFile,
) -> Result<(), String> {
    validate_expected_download(path, expected)
        .map_err(|error| format!("同名文件冲突 {}: {}", file_name, error))
}

fn validate_new_download(
    path: &std::path::Path,
    file_name: &str,
    expected: &ExpectedDownloadFile,
) -> Result<(), String> {
    validate_expected_download(path, expected)
        .map_err(|error| format!("下载文件校验失败 {}: {}", file_name, error))
}

fn validate_expected_download(
    path: &std::path::Path,
    expected: &ExpectedDownloadFile,
) -> Result<(), String> {
    let mut verified = false;
    if let Some(expected_size) = expected.size {
        let actual_size = std::fs::metadata(path)
            .map_err(|e| format!("读取文件失败: {}", e))?
            .len();
        if actual_size != expected_size {
            return Err(format!(
                "大小不匹配（实际 {}，需要 {}）",
                actual_size, expected_size
            ));
        }
        verified = true;
    }
    if let Some(expected_sha1) = expected.sha1.as_deref() {
        let (actual_sha1, _, _, _) = crate::modpack_export::hash_update_candidate(path)?;
        if !actual_sha1.eq_ignore_ascii_case(expected_sha1) {
            return Err("SHA-1 不匹配".to_string());
        }
        verified = true;
    }
    if verified {
        Ok(())
    } else {
        Err("下载源没有提供可核验的哈希或大小".to_string())
    }
}

#[derive(serde::Serialize, Clone)]
pub struct OnlineModVersionInfo {
    pub version_id: String,
    pub version_name: String,
    pub mc_versions: String,
    pub mc_version: String,
    pub loaders: String,
    pub loader: String,
    pub file_name: String,
    pub file_size: u64,
    pub date: String,
    pub source: String,
}

/// 获取在线 Mod/材质包/光影包的可下载版本列表
#[tauri::command]
pub async fn get_online_mod_versions(
    project_id: String,
    loader: String,
    project_type: Option<String>,
) -> Result<Vec<OnlineModVersionInfo>, String> {
    let ptype = project_type.unwrap_or_else(|| "mod".to_string());
    tokio::task::spawn_blocking(move || {
        get_online_mod_versions_blocking(&project_id, &loader, &ptype)
    })
    .await
    .map_err(|e| format!("任务失败: {}", e))?
}

/// 下载 Mod/材质包/光影包到实例目录（异步）
#[tauri::command]
pub async fn download_online_mod(
    game_dir: String,
    name: String,
    project_id: String,
    mc_version: String,
    loader: String,
    project_type: Option<String>,
    version_id: Option<String>,
) -> Result<String, String> {
    let ptype = project_type.unwrap_or_else(|| "mod".to_string());
    let cancel_name = online_mod_cancel_key(&game_dir, &name, &project_id, version_id.as_deref());
    let _cancel_guard = OnlineModCancelGuard::register(&cancel_name)?;
    let result = tokio::task::spawn_blocking(move || {
        download_online_mod_blocking(
            &game_dir,
            &name,
            &project_id,
            &mc_version,
            &loader,
            &ptype,
            version_id.as_deref(),
            &cancel_name,
        )
    })
    .await;
    result.map_err(|e| format!("任务失败: {}", e))?
}

pub(crate) fn get_online_mod_versions_blocking(
    project_id: &str,
    loader: &str,
    project_type: &str,
) -> Result<Vec<OnlineModVersionInfo>, String> {
    let http = reqwest::blocking::Client::builder()
        .use_rustls_tls()
        .timeout(std::time::Duration::from_secs(20))
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) oaoi-launcher/1.0")
        .build()
        .map_err(|e| e.to_string())?;

    if let Some(cf_id) = project_id.strip_prefix("cf_") {
        return get_curseforge_versions(&http, cf_id, loader, project_type);
    }
    get_modrinth_versions(&http, project_id, loader, project_type)
}

fn download_online_mod_blocking(
    game_dir: &str,
    name: &str,
    project_id: &str,
    mc_version: &str,
    loader: &str,
    project_type: &str,
    version_id: Option<&str>,
    cancel_name: &str,
) -> Result<String, String> {
    let mutation_lock = crate::mod_manager::mod_mutation_lock(game_dir, name)?;
    let _mutation_guard = mutation_lock
        .lock()
        .map_err(|_| "Mod 文件事务锁已损坏".to_string())?;
    let http = reqwest::blocking::Client::builder()
        .use_rustls_tls()
        .connect_timeout(std::time::Duration::from_secs(5))
        .timeout(std::time::Duration::from_secs(60))
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) oaoi-launcher/1.0")
        .build()
        .map_err(|e| e.to_string())?;
    ensure_not_cancelled(cancel_name)?;

    // 判断是 CurseForge 还是 Modrinth
    if let Some(cf_id) = project_id.strip_prefix("cf_") {
        return download_from_curseforge(
            &http,
            game_dir,
            name,
            cf_id,
            mc_version,
            loader,
            project_type,
            version_id,
            cancel_name,
        );
    }

    // Modrinth 下载 — 带降级：先精确匹配，再逐步放宽条件
    let version = if let Some(vid) = version_id.filter(|v| !v.is_empty()) {
        let url = format!("https://api.modrinth.com/v2/version/{}", vid);
        ensure_not_cancelled(cancel_name)?;
        let resp = http
            .get(&url)
            .timeout(std::time::Duration::from_secs(15))
            .send()
            .map_err(|e| format!("版本请求失败: {}", e))?;
        ensure_not_cancelled(cancel_name)?;
        if !resp.status().is_success() {
            return Err(format!("版本请求失败: HTTP {}", resp.status()));
        }
        let version = resp
            .json::<serde_json::Value>()
            .map_err(|e| format!("解析版本失败: {}", e))?;
        ensure_not_cancelled(cancel_name)?;
        version
    } else {
        let versions_arr = {
            let try_params = vec![
                (mc_version, loader), // 1. 精确: 版本+loader
                (mc_version, ""),     // 2. 去掉 loader
            ];
            let mut found: Option<Vec<serde_json::Value>> = None;
            for (ver, ldr) in &try_params {
                ensure_not_cancelled(cancel_name)?;
                let mut url = format!("https://api.modrinth.com/v2/project/{}/version", project_id);
                let mut p = vec![];
                if !ver.is_empty() {
                    p.push(format!("game_versions=[\"{}\"]", ver));
                }
                if !ldr.is_empty() && *ldr != "vanilla" && project_type == "mod" {
                    p.push(format!("loaders=[\"{}\"]", ldr));
                }
                if !p.is_empty() {
                    url = format!("{}?{}", url, p.join("&"));
                }
                if let Ok(resp) = http
                    .get(&url)
                    .timeout(std::time::Duration::from_secs(15))
                    .send()
                {
                    ensure_not_cancelled(cancel_name)?;
                    if let Ok(json) = resp.json::<serde_json::Value>() {
                        ensure_not_cancelled(cancel_name)?;
                        if let Some(arr) = json.as_array() {
                            // 放宽服务端查询后仍在本地校验 Loader，避免拿到其他平台文件。
                            let compatible = arr
                                .iter()
                                .filter(|version| {
                                    modrinth_version_matches_loader(version, loader, project_type)
                                        && modrinth_version_matches_game(version, mc_version)
                                })
                                .cloned()
                                .collect::<Vec<_>>();
                            if !compatible.is_empty() {
                                found = Some(compatible);
                                break;
                            }
                        }
                    }
                }
            }
            found.ok_or_else(|| "没有找到匹配的版本".to_string())?
        };
        versions_arr
            .first()
            .cloned()
            .ok_or_else(|| "没有找到匹配的版本".to_string())?
    };
    if !modrinth_version_matches_loader(&version, loader, project_type) {
        return Err(format!("所选版本不支持当前 Loader: {}", loader));
    }
    if !modrinth_version_matches_game(&version, mc_version) {
        return Err(format!("所选版本不支持当前 Minecraft 版本: {}", mc_version));
    }
    let files = version["files"].as_array().ok_or("版本无文件")?;
    let file = files
        .iter()
        .find(|f| f["primary"].as_bool() == Some(true))
        .or_else(|| files.first())
        .ok_or("无下载文件")?;

    let download_url = file["url"].as_str().ok_or("无下载链接")?;
    let file_name = file["filename"].as_str().ok_or("无文件名")?;
    let expected_file = modrinth_expected_file(file);

    let sub_dir = match project_type {
        "resourcepack" => "resourcepacks",
        "shader" => "shaderpacks",
        _ => "mods",
    };
    let mut transaction = ModDownloadTransaction::default();
    let mut downloaded_mod_paths: Vec<String> = Vec::new();
    let main_result = do_download_to_dir(
        &http,
        game_dir,
        name,
        download_url,
        file_name,
        sub_dir,
        Some(cancel_name),
        &expected_file,
        &mut transaction,
    )?;
    push_downloaded_mod_path(&mut downloaded_mod_paths, sub_dir, file_name);

    // 检查并下载前置依赖
    let mut dep_names: Vec<String> = Vec::new();
    let mut dep_errors: Vec<String> = Vec::new();
    let mut visited_deps: HashSet<String> = HashSet::new();
    let game_root = resolve_game_dir(game_dir);
    let safe_name = safe_path_name(name, "版本名")?;
    let mods_dir = version_dir(&game_root, &safe_name).join("mods");
    let mut installed_dependencies =
        InstalledDependencyIndex::load(&game_root, &safe_name, &mods_dir, loader);
    installed_dependencies.record_project("mr", project_id);
    if let Some(version_id) = version["id"].as_str() {
        visited_deps.insert(format!("mr-version:{}", version_id));
    }
    if let Some(deps) = version["dependencies"].as_array() {
        download_modrinth_required_dependencies(
            &http,
            game_dir,
            name,
            mc_version,
            loader,
            deps,
            &mut visited_deps,
            &mut installed_dependencies,
            &mut dep_names,
            &mut dep_errors,
            &mut downloaded_mod_paths,
            0,
            cancel_name,
            &mut transaction,
        )?;
    }

    if !dep_errors.is_empty() {
        return Err(format!("前置依赖下载失败: {}", dep_errors.join("; ")));
    }

    let result = if dep_names.is_empty() {
        main_result
    } else {
        format!("{} (已自动下载前置: {})", main_result, dep_names.join(", "))
    };
    transaction.commit();
    crate::mod_analyzer::spawn_cache_downloaded_mods(
        game_dir.to_string(),
        name.to_string(),
        loader.to_string(),
        downloaded_mod_paths,
    );
    Ok(result)
}

fn download_modrinth_required_dependencies(
    http: &reqwest::blocking::Client,
    game_dir: &str,
    name: &str,
    mc_version: &str,
    loader: &str,
    deps: &[serde_json::Value],
    visited: &mut HashSet<String>,
    installed: &mut InstalledDependencyIndex,
    dep_names: &mut Vec<String>,
    dep_errors: &mut Vec<String>,
    downloaded_mod_paths: &mut Vec<String>,
    depth: usize,
    cancel_name: &str,
    transaction: &mut ModDownloadTransaction,
) -> Result<(), String> {
    if depth >= MAX_DEPENDENCY_DEPTH {
        dep_errors.push("前置依赖层级过深，已停止继续解析".to_string());
        return Ok(());
    }

    let safe_name = safe_path_name(name, "版本名")?;
    let game_root = resolve_game_dir(game_dir);
    let mods_dir = version_dir(&game_root, &safe_name).join("mods");

    for dep in deps {
        if is_cancelled(Some(cancel_name)) {
            dep_errors.push("用户取消下载".to_string());
            break;
        }

        let dep_type = dep["dependency_type"].as_str().unwrap_or("");
        if dep_type != "required" {
            continue;
        }

        let (dep_project_id, dep_version_id) = modrinth_dependency_ids(dep);
        if dep_project_id.is_none() && dep_version_id.is_none() {
            dep_errors.push("Modrinth 前置缺少 project_id 和 version_id".to_string());
            continue;
        }
        let dep_label = dep_project_id.or(dep_version_id).unwrap_or("未知前置");

        eprintln!("[dep] 检查前置依赖: {}", dep_label);

        match resolve_modrinth_dependency_version(
            http,
            dep,
            dep_project_id,
            mc_version,
            loader,
            cancel_name,
        ) {
            Ok(dep_version) => {
                if let Err(e) = download_modrinth_dependency_version(
                    http,
                    game_dir,
                    name,
                    mc_version,
                    loader,
                    dep_project_id,
                    dep_version,
                    &mods_dir,
                    visited,
                    installed,
                    dep_names,
                    dep_errors,
                    downloaded_mod_paths,
                    depth,
                    cancel_name,
                    transaction,
                ) {
                    dep_errors.push(format!("{}: {}", dep_label, e));
                }
            }
            Err(e) => dep_errors.push(format!("{}: {}", dep_label, e)),
        }
    }

    Ok(())
}

fn modrinth_dependency_ids(dep: &serde_json::Value) -> (Option<&str>, Option<&str>) {
    let project_id = dep["project_id"]
        .as_str()
        .map(str::trim)
        .filter(|id| !id.is_empty());
    let version_id = dep["version_id"]
        .as_str()
        .map(str::trim)
        .filter(|id| !id.is_empty());
    (project_id, version_id)
}

fn resolve_modrinth_dependency_version(
    http: &reqwest::blocking::Client,
    dep: &serde_json::Value,
    dep_project_id: Option<&str>,
    mc_version: &str,
    loader: &str,
    cancel_name: &str,
) -> Result<serde_json::Value, String> {
    let dep_version_url = modrinth_dependency_version_url(dep, dep_project_id, mc_version, loader)?;

    ensure_not_cancelled(cancel_name)?;
    let resp = http
        .get(&dep_version_url)
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .map_err(|e| format!("前置版本请求失败: {}", e))?;
    ensure_not_cancelled(cancel_name)?;
    if !resp.status().is_success() {
        return Err(format!("前置版本请求失败: HTTP {}", resp.status()));
    }
    let dep_json = resp
        .json::<serde_json::Value>()
        .map_err(|e| format!("解析前置版本失败: {}", e))?;
    ensure_not_cancelled(cancel_name)?;
    if dep_json.is_array() {
        dep_json
            .as_array()
            .and_then(|versions| versions.first())
            .cloned()
            .ok_or_else(|| "没有找到匹配的前置版本".to_string())
    } else {
        Ok(dep_json)
    }
}

fn modrinth_dependency_version_url(
    dep: &serde_json::Value,
    dep_project_id: Option<&str>,
    mc_version: &str,
    loader: &str,
) -> Result<String, String> {
    let (_, dep_version_id) = modrinth_dependency_ids(dep);
    let dep_version_url = if let Some(vid) = dep_version_id {
        format!("https://api.modrinth.com/v2/version/{}", vid)
    } else {
        let dep_project_id =
            dep_project_id.ok_or_else(|| "Modrinth 前置缺少可解析的项目 ID".to_string())?;
        let mut url = format!(
            "https://api.modrinth.com/v2/project/{}/version",
            dep_project_id
        );
        let mut params = Vec::new();
        if !mc_version.is_empty() {
            params.push(format!("game_versions=[\"{}\"]", mc_version));
        }
        if !loader.is_empty() && loader != "vanilla" {
            params.push(format!("loaders=[\"{}\"]", loader));
        }
        if !params.is_empty() {
            url = format!("{}?{}", url, params.join("&"));
        }
        url
    };
    Ok(dep_version_url)
}

fn download_modrinth_dependency_version(
    http: &reqwest::blocking::Client,
    game_dir: &str,
    name: &str,
    mc_version: &str,
    loader: &str,
    requested_project_id: Option<&str>,
    dep_version: serde_json::Value,
    mods_dir: &std::path::Path,
    visited: &mut HashSet<String>,
    installed: &mut InstalledDependencyIndex,
    dep_names: &mut Vec<String>,
    dep_errors: &mut Vec<String>,
    downloaded_mod_paths: &mut Vec<String>,
    depth: usize,
    cancel_name: &str,
    transaction: &mut ModDownloadTransaction,
) -> Result<(), String> {
    let dep_project_id = dep_version["project_id"]
        .as_str()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .or(requested_project_id)
        .unwrap_or("");
    let dep_key = dep_version["id"]
        .as_str()
        .map(|id| format!("mr-version:{}", id))
        .unwrap_or_else(|| format!("mr-project:{}", dep_project_id));
    if !visited.insert(dep_key) {
        return Ok(());
    }

    let aliases = if dep_project_id.is_empty() || installed.has_project("mr", dep_project_id) {
        Vec::new()
    } else {
        modrinth_project_aliases(http, dep_project_id, cancel_name)
    };
    let already_installed = (!dep_project_id.is_empty()
        && installed.has_project("mr", dep_project_id))
        || installed.has_mod_aliases(&aliases);

    let dep_files = dep_version["files"]
        .as_array()
        .ok_or_else(|| "前置版本没有文件列表".to_string())?;
    let dep_file = dep_files
        .iter()
        .find(|file| file["primary"].as_bool() == Some(true))
        .or_else(|| dep_files.first())
        .ok_or_else(|| "前置版本没有可下载文件".to_string())?;
    let dep_url = dep_file["url"]
        .as_str()
        .ok_or_else(|| "前置版本没有下载链接".to_string())?;
    let dep_fname = dep_file["filename"]
        .as_str()
        .ok_or_else(|| "前置版本没有文件名".to_string())?;
    let safe_dep_fname = safe_path_name(dep_fname, "文件名")?;
    let expected_file = modrinth_expected_file(dep_file);

    let dep_path = mods_dir.join(&safe_dep_fname);
    let mut downloaded = false;
    if already_installed {
        eprintln!("[dep] 前置已按项目或 Mod ID 识别: {}", dep_fname);
    } else if dep_path.exists() {
        validate_existing_download(&dep_path, dep_fname, &expected_file)?;
        eprintln!("[dep] 前置已存在且校验通过: {}", dep_fname);
    } else {
        do_download_to_dir(
            http,
            game_dir,
            name,
            dep_url,
            dep_fname,
            "mods",
            Some(cancel_name),
            &expected_file,
            transaction,
        )
        .map_err(|e| format!("下载前置失败 {}: {}", dep_fname, e))?;
        downloaded = true;
    }

    if !already_installed {
        let candidate_ids =
            crate::mod_analyzer::read_mod_ids_from_file(&dep_path, loader).unwrap_or_default();
        if downloaded && installed.overlaps_mod_ids(&candidate_ids) {
            // 文件名不同但 Mod ID 相同，保留原有版本，删除刚下载的重复文件。
            std::fs::remove_file(&dep_path)
                .map_err(|e| format!("删除重复前置失败 {}: {}", dep_fname, e))?;
            eprintln!("[dep] 已安装同 Mod ID 前置，丢弃重复文件: {}", dep_fname);
        } else {
            installed.record_mod_ids(&candidate_ids);
            push_downloaded_mod_path(downloaded_mod_paths, "mods", dep_fname);
            if downloaded {
                eprintln!("[dep] 已下载前置: {}", dep_fname);
                dep_names.push(dep_fname.to_string());
            }
        }
    }
    if !dep_project_id.is_empty() {
        installed.record_project("mr", dep_project_id);
    }

    if let Some(child_deps) = dep_version["dependencies"].as_array() {
        download_modrinth_required_dependencies(
            http,
            game_dir,
            name,
            mc_version,
            loader,
            child_deps,
            visited,
            installed,
            dep_names,
            dep_errors,
            downloaded_mod_paths,
            depth + 1,
            cancel_name,
            transaction,
        )?;
    }

    Ok(())
}

fn get_modrinth_versions(
    http: &reqwest::blocking::Client,
    project_id: &str,
    loader: &str,
    project_type: &str,
) -> Result<Vec<OnlineModVersionInfo>, String> {
    let url = format!("https://api.modrinth.com/v2/project/{}/version", project_id);
    let resp = http
        .get(&url)
        .send()
        .map_err(|e| format!("Modrinth 版本请求失败: {}", e))?;
    if !resp.status().is_success() {
        return Err(format!("Modrinth 版本请求失败: HTTP {}", resp.status()));
    }
    let json: serde_json::Value = resp.json().map_err(|e| format!("解析版本失败: {}", e))?;
    let versions = json.as_array().ok_or("版本数据格式错误")?;

    let mut out = Vec::new();
    for item in versions {
        let loaders = json_string_array(&item["loaders"]);
        if project_type == "mod" && !loader.is_empty() && loader != "vanilla" {
            if !loaders.iter().any(|l| l.eq_ignore_ascii_case(loader)) {
                continue;
            }
        }

        let files = match item["files"].as_array() {
            Some(files) => files,
            None => continue,
        };
        let file = match files
            .iter()
            .find(|f| f["primary"].as_bool() == Some(true))
            .or_else(|| files.first())
        {
            Some(file) => file,
            None => continue,
        };

        let game_versions = json_string_array(&item["game_versions"]);
        let first_mc = game_versions.first().cloned().unwrap_or_default();
        let selected_loader = if project_type == "mod"
            && !loader.is_empty()
            && loader != "vanilla"
            && loaders.iter().any(|l| l.eq_ignore_ascii_case(loader))
        {
            loader.to_string()
        } else {
            loaders
                .first()
                .cloned()
                .unwrap_or_else(|| loader.to_string())
        };
        let date = item["date_published"]
            .as_str()
            .map(short_date)
            .unwrap_or_default();

        out.push(OnlineModVersionInfo {
            version_id: item["id"].as_str().unwrap_or("").to_string(),
            version_name: item["version_number"]
                .as_str()
                .unwrap_or("未命名版本")
                .to_string(),
            mc_versions: game_versions.join(", "),
            mc_version: first_mc,
            loaders: loaders.join(", "),
            loader: selected_loader.to_lowercase(),
            file_name: file["filename"].as_str().unwrap_or("").to_string(),
            file_size: file["size"].as_u64().unwrap_or(0),
            date,
            source: "modrinth".to_string(),
        });
    }

    Ok(out)
}

fn modrinth_version_matches_loader(
    version: &serde_json::Value,
    loader: &str,
    project_type: &str,
) -> bool {
    if project_type != "mod" || loader.is_empty() || loader.eq_ignore_ascii_case("vanilla") {
        return true;
    }
    version["loaders"].as_array().is_some_and(|loaders| {
        loaders.iter().any(|item| {
            item.as_str()
                .is_some_and(|value| value.eq_ignore_ascii_case(loader))
        })
    })
}

fn modrinth_version_matches_game(version: &serde_json::Value, mc_version: &str) -> bool {
    let mc_version = mc_version.trim();
    mc_version.is_empty()
        || version["game_versions"].as_array().is_some_and(|versions| {
            versions
                .iter()
                .any(|item| item.as_str() == Some(mc_version))
        })
}

fn get_curseforge_versions(
    http: &reqwest::blocking::Client,
    cf_id: &str,
    loader: &str,
    project_type: &str,
) -> Result<Vec<OnlineModVersionInfo>, String> {
    let loader_type = curseforge_loader_type(loader);
    let mut out = Vec::new();

    for index in (0..500).step_by(50) {
        let mut url = format!(
            "https://api.curseforge.com/v1/mods/{}/files?pageSize=50&index={}",
            cf_id, index
        );
        if loader_type != "0" && project_type == "mod" {
            url.push_str(&format!("&modLoaderType={}", loader_type));
        }

        let resp = http
            .get(&url)
            .header("x-api-key", &cf_api_key())
            .header("Accept", "application/json")
            .send()
            .map_err(|e| format!("CurseForge 版本请求失败: {}", e))?;
        if !resp.status().is_success() {
            return Err(format!("CurseForge 版本请求失败: HTTP {}", resp.status()));
        }
        let json: serde_json::Value = resp.json().map_err(|e| format!("解析版本失败: {}", e))?;
        let data = json["data"].as_array().ok_or("版本数据格式错误")?;
        if data.is_empty() {
            break;
        }

        for file in data {
            if let Some(info) = curseforge_file_to_version_info(file, loader) {
                out.push(info);
            }
        }
    }

    Ok(out)
}

fn curseforge_file_to_version_info(
    file: &serde_json::Value,
    preferred_loader: &str,
) -> Option<OnlineModVersionInfo> {
    let game_versions = json_string_array(&file["gameVersions"]);
    let mc_versions: Vec<String> = game_versions
        .iter()
        .filter(|v| is_minecraft_version(v))
        .cloned()
        .collect();
    let loaders: Vec<String> = game_versions
        .iter()
        .filter_map(|v| normalize_loader_name(v))
        .collect();
    let file_id = file["id"].as_u64()?.to_string();
    let file_name = file["fileName"].as_str().unwrap_or("").to_string();
    let version_name = file["displayName"]
        .as_str()
        .filter(|v| !v.is_empty())
        .unwrap_or(&file_name)
        .to_string();
    let loader = if !preferred_loader.is_empty() && preferred_loader != "vanilla" {
        preferred_loader.to_string()
    } else {
        loaders.first().cloned().unwrap_or_default()
    };

    Some(OnlineModVersionInfo {
        version_id: file_id,
        version_name,
        mc_versions: mc_versions.join(", "),
        mc_version: mc_versions.first().cloned().unwrap_or_default(),
        loaders: loaders.join(", "),
        loader,
        file_name,
        file_size: file["fileLength"].as_u64().unwrap_or(0),
        date: file["fileDate"]
            .as_str()
            .map(short_date)
            .unwrap_or_default(),
        source: "curseforge".to_string(),
    })
}

fn fetch_curseforge_file(
    http: &reqwest::blocking::Client,
    cf_id: &str,
    file_id: &str,
    cancel_name: &str,
) -> Result<serde_json::Value, String> {
    let url = format!(
        "https://api.curseforge.com/v1/mods/{}/files/{}",
        cf_id, file_id
    );
    ensure_not_cancelled(cancel_name)?;
    let resp = http
        .get(&url)
        .timeout(std::time::Duration::from_secs(15))
        .header("x-api-key", &cf_api_key())
        .header("Accept", "application/json")
        .send()
        .map_err(|e| format!("CurseForge 文件请求失败: {}", e))?;
    ensure_not_cancelled(cancel_name)?;
    if !resp.status().is_success() {
        return Err(format!("CurseForge 文件请求失败: HTTP {}", resp.status()));
    }
    let json: serde_json::Value = resp.json().map_err(|e| format!("解析文件失败: {}", e))?;
    ensure_not_cancelled(cancel_name)?;
    json["data"]
        .as_object()
        .map(|_| json["data"].clone())
        .ok_or_else(|| "CurseForge 文件数据格式错误".to_string())
}

fn json_string_array(value: &serde_json::Value) -> Vec<String> {
    value
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .filter(|v| !v.trim().is_empty())
                .map(|v| v.trim().to_string())
                .collect()
        })
        .unwrap_or_default()
}

fn short_date(value: &str) -> String {
    value.chars().take(10).collect()
}

fn is_minecraft_version(value: &str) -> bool {
    value
        .chars()
        .next()
        .map(|c| c.is_ascii_digit())
        .unwrap_or(false)
}

fn normalize_loader_name(value: &str) -> Option<String> {
    match value.to_lowercase().as_str() {
        "forge" => Some("forge".to_string()),
        "fabric" => Some("fabric".to_string()),
        "quilt" => Some("quilt".to_string()),
        "neoforge" | "neo forge" => Some("neoforge".to_string()),
        _ => None,
    }
}

fn curseforge_loader_type(loader: &str) -> &'static str {
    match loader {
        "forge" => "1",
        "fabric" => "4",
        "quilt" => "5",
        "neoforge" => "6",
        _ => "0",
    }
}

fn curseforge_cdn_urls(file_id: u64, file_name: &str) -> Vec<String> {
    if file_id == 0 || file_name.is_empty() {
        return Vec::new();
    }
    let encoded_name = urlencoding::encode(file_name);
    vec![
        format!(
            "https://edge.forgecdn.net/files/{}/{}/{}",
            file_id / 1000,
            file_id % 1000,
            encoded_name
        ),
        format!(
            "https://mediafilez.forgecdn.net/files/{}/{}/{}",
            file_id / 1000,
            file_id % 1000,
            encoded_name
        ),
    ]
}

fn curseforge_download_candidates(
    file_id: u64,
    file_name: &str,
    api_download_url: &str,
) -> Vec<String> {
    let mut urls = Vec::new();
    for url in curseforge_cdn_urls(file_id, file_name) {
        push_unique_download_url(&mut urls, url);
    }
    if !api_download_url.is_empty() {
        push_unique_download_url(&mut urls, api_download_url.to_string());
    }
    urls
}

fn push_unique_download_url(urls: &mut Vec<String>, url: String) {
    let normalized = url.trim();
    if !normalized.is_empty() && !urls.iter().any(|existing| existing == normalized) {
        urls.push(normalized.to_string());
    }
}

fn do_download_to_dir_with_fallbacks(
    _http: &reqwest::blocking::Client,
    game_dir: &str,
    name: &str,
    download_urls: &[String],
    file_name: &str,
    sub_dir: &str,
    cancel_name: Option<&str>,
    expected_file: &ExpectedDownloadFile,
    transaction: &mut ModDownloadTransaction,
) -> Result<String, String> {
    download_to_dir_with_candidates(
        game_dir,
        name,
        download_urls,
        file_name,
        sub_dir,
        cancel_name,
        expected_file,
        transaction,
    )
}

fn download_from_curseforge(
    http: &reqwest::blocking::Client,
    game_dir: &str,
    name: &str,
    cf_id: &str,
    mc_version: &str,
    loader: &str,
    project_type: &str,
    version_id: Option<&str>,
    cancel_name: &str,
) -> Result<String, String> {
    ensure_not_cancelled(cancel_name)?;
    let loader_type = curseforge_loader_type(loader);
    let file = if let Some(file_id) = version_id.filter(|v| !v.is_empty()) {
        fetch_curseforge_file(http, cf_id, file_id, cancel_name)?
    } else {
        let mut url = format!(
            "https://api.curseforge.com/v1/mods/{}/files?pageSize=5",
            cf_id
        );
        if !mc_version.is_empty() && project_type == "mod" {
            url.push_str(&format!("&gameVersion={}", mc_version));
        }
        if loader_type != "0" && project_type == "mod" {
            url.push_str(&format!("&modLoaderType={}", loader_type));
        }

        ensure_not_cancelled(cancel_name)?;
        let resp = http
            .get(&url)
            .timeout(std::time::Duration::from_secs(15))
            .header("x-api-key", &cf_api_key())
            .send()
            .map_err(|e| format!("CurseForge 请求失败: {}", e))?;
        ensure_not_cancelled(cancel_name)?;
        if !resp.status().is_success() {
            return Err(format!("CurseForge 请求失败: HTTP {}", resp.status()));
        }
        let json: serde_json::Value = resp.json().map_err(|e| format!("解析失败: {}", e))?;
        ensure_not_cancelled(cancel_name)?;

        let data = json["data"].as_array().ok_or("无文件数据")?;
        if data.is_empty() {
            return Err("没有找到匹配的版本".to_string());
        }

        data[0].clone()
    };
    if !curseforge_file_matches_instance(&file, mc_version, loader, project_type) {
        return Err(format!(
            "所选版本不支持当前 Minecraft/Loader: {} / {}",
            mc_version, loader
        ));
    }
    let file_name = file["fileName"].as_str().ok_or("无文件名")?;
    let file_id = file["id"].as_u64().unwrap_or(0);
    let api_download_url = file["downloadUrl"].as_str().unwrap_or("");
    let expected_file = curseforge_expected_file(&file);
    let download_urls = curseforge_download_candidates(file_id, file_name, api_download_url);
    if download_urls.is_empty() {
        return Err("此 Mod 没有可用下载地址，请从 CurseForge 网站手动下载".to_string());
    }

    let sub_dir = match project_type {
        "resourcepack" => "resourcepacks",
        "shader" => "shaderpacks",
        _ => "mods",
    };
    let mut transaction = ModDownloadTransaction::default();
    let mut downloaded_mod_paths: Vec<String> = Vec::new();
    let main_result = do_download_to_dir_with_fallbacks(
        http,
        game_dir,
        name,
        &download_urls,
        file_name,
        sub_dir,
        Some(cancel_name),
        &expected_file,
        &mut transaction,
    )?;
    push_downloaded_mod_path(&mut downloaded_mod_paths, sub_dir, file_name);

    // 检查 CurseForge 前置依赖
    let mut dep_names: Vec<String> = Vec::new();
    let mut dep_errors: Vec<String> = Vec::new();
    let mut visited_deps: HashSet<String> = HashSet::new();
    let game_root = resolve_game_dir(game_dir);
    let safe_name = safe_path_name(name, "版本名")?;
    let mods_dir = version_dir(&game_root, &safe_name).join("mods");
    let mut installed_dependencies =
        InstalledDependencyIndex::load(&game_root, &safe_name, &mods_dir, loader);
    installed_dependencies.record_project("cf", cf_id);
    if file_id != 0 {
        visited_deps.insert(format!("cf-file:{}", file_id));
    }
    if let Some(deps) = file["dependencies"].as_array() {
        download_curseforge_required_dependencies(
            http,
            game_dir,
            name,
            mc_version,
            loader_type,
            loader,
            deps,
            &mut visited_deps,
            &mut installed_dependencies,
            &mut dep_names,
            &mut dep_errors,
            &mut downloaded_mod_paths,
            0,
            cancel_name,
            &mut transaction,
        )?;
    }

    if !dep_errors.is_empty() {
        return Err(format!("前置依赖下载失败: {}", dep_errors.join("; ")));
    }

    let result = if dep_names.is_empty() {
        main_result
    } else {
        format!("{} (已自动下载前置: {})", main_result, dep_names.join(", "))
    };
    transaction.commit();
    crate::mod_analyzer::spawn_cache_downloaded_mods(
        game_dir.to_string(),
        name.to_string(),
        loader.to_string(),
        downloaded_mod_paths,
    );
    Ok(result)
}

fn curseforge_file_matches_instance(
    file: &serde_json::Value,
    mc_version: &str,
    loader: &str,
    project_type: &str,
) -> bool {
    let versions = json_string_array(&file["gameVersions"]);
    let game_matches =
        mc_version.trim().is_empty() || versions.iter().any(|version| version == mc_version.trim());
    if !game_matches || project_type != "mod" || loader.is_empty() || loader == "vanilla" {
        return game_matches;
    }
    versions.iter().any(|version| {
        normalize_loader_name(version)
            .is_some_and(|normalized| normalized.eq_ignore_ascii_case(loader))
    })
}

fn download_curseforge_required_dependencies(
    http: &reqwest::blocking::Client,
    game_dir: &str,
    name: &str,
    mc_version: &str,
    loader_type: &str,
    loader: &str,
    deps: &[serde_json::Value],
    visited: &mut HashSet<String>,
    installed: &mut InstalledDependencyIndex,
    dep_names: &mut Vec<String>,
    dep_errors: &mut Vec<String>,
    downloaded_mod_paths: &mut Vec<String>,
    depth: usize,
    cancel_name: &str,
    transaction: &mut ModDownloadTransaction,
) -> Result<(), String> {
    if depth >= MAX_DEPENDENCY_DEPTH {
        dep_errors.push("前置依赖层级过深，已停止继续解析".to_string());
        return Ok(());
    }

    let safe_name = safe_path_name(name, "版本名")?;
    let game_root = resolve_game_dir(game_dir);
    let mods_dir = version_dir(&game_root, &safe_name).join("mods");

    for dep in deps {
        if is_cancelled(Some(cancel_name)) {
            dep_errors.push("用户取消下载".to_string());
            break;
        }

        let relation = dep["relationType"].as_i64().unwrap_or(0);
        if relation != 3 {
            continue;
        } // 3 = required dependency

        let dep_mod_id = dep["modId"].as_u64().unwrap_or(0);
        if dep_mod_id == 0 {
            dep_errors.push("CurseForge 前置缺少 modId".to_string());
            continue;
        }

        eprintln!("[cf_dep] 检查前置依赖: modId={}", dep_mod_id);

        match resolve_curseforge_dependency_file(
            http,
            dep_mod_id,
            mc_version,
            loader_type,
            cancel_name,
        ) {
            Ok(dep_file) => {
                if let Err(e) = download_curseforge_dependency_file(
                    http,
                    game_dir,
                    name,
                    mc_version,
                    loader_type,
                    loader,
                    dep_mod_id,
                    dep_file,
                    &mods_dir,
                    visited,
                    installed,
                    dep_names,
                    dep_errors,
                    downloaded_mod_paths,
                    depth,
                    cancel_name,
                    transaction,
                ) {
                    dep_errors.push(format!("{}: {}", dep_mod_id, e));
                }
            }
            Err(e) => dep_errors.push(format!("{}: {}", dep_mod_id, e)),
        }
    }

    Ok(())
}

fn resolve_curseforge_dependency_file(
    http: &reqwest::blocking::Client,
    dep_mod_id: u64,
    mc_version: &str,
    loader_type: &str,
    cancel_name: &str,
) -> Result<serde_json::Value, String> {
    let dep_url = format!(
        "https://api.curseforge.com/v1/mods/{}/files?pageSize=1&gameVersion={}&modLoaderType={}",
        dep_mod_id, mc_version, loader_type
    );
    ensure_not_cancelled(cancel_name)?;
    let resp = http
        .get(&dep_url)
        .timeout(std::time::Duration::from_secs(15))
        .header("x-api-key", &cf_api_key())
        .send()
        .map_err(|e| format!("前置文件请求失败: {}", e))?;
    ensure_not_cancelled(cancel_name)?;
    if !resp.status().is_success() {
        return Err(format!("前置文件请求失败: HTTP {}", resp.status()));
    }
    let dep_json = resp
        .json::<serde_json::Value>()
        .map_err(|e| format!("解析前置文件失败: {}", e))?;
    ensure_not_cancelled(cancel_name)?;
    dep_json["data"]
        .as_array()
        .and_then(|files| files.first())
        .cloned()
        .ok_or_else(|| "没有找到匹配的前置文件".to_string())
}

fn download_curseforge_dependency_file(
    http: &reqwest::blocking::Client,
    game_dir: &str,
    name: &str,
    mc_version: &str,
    loader_type: &str,
    loader: &str,
    dep_mod_id: u64,
    dep_file: serde_json::Value,
    mods_dir: &std::path::Path,
    visited: &mut HashSet<String>,
    installed: &mut InstalledDependencyIndex,
    dep_names: &mut Vec<String>,
    dep_errors: &mut Vec<String>,
    downloaded_mod_paths: &mut Vec<String>,
    depth: usize,
    cancel_name: &str,
    transaction: &mut ModDownloadTransaction,
) -> Result<(), String> {
    let dep_file_id = dep_file["id"].as_u64().unwrap_or(0);
    let dep_key = if dep_file_id != 0 {
        format!("cf-file:{}", dep_file_id)
    } else {
        format!("cf-project:{}", dep_mod_id)
    };
    if !visited.insert(dep_key) {
        return Ok(());
    }

    let dep_fname = dep_file["fileName"]
        .as_str()
        .ok_or_else(|| "前置文件没有文件名".to_string())?;
    let dep_dl_url = dep_file["downloadUrl"].as_str().unwrap_or("");
    let safe_dep_fname = safe_path_name(dep_fname, "文件名")?;
    let expected_file = curseforge_expected_file(&dep_file);
    let dep_download_urls = curseforge_download_candidates(dep_file_id, dep_fname, dep_dl_url);
    if dep_download_urls.is_empty() {
        return Err(format!("前置没有可用下载地址: {}", dep_fname));
    }

    let aliases = curseforge_file_mod_aliases(&dep_file);
    let project_id = dep_mod_id.to_string();
    let already_installed =
        installed.has_project("cf", &project_id) || installed.has_mod_aliases(&aliases);
    let dep_path = mods_dir.join(&safe_dep_fname);
    let mut downloaded = false;
    if already_installed {
        eprintln!("[cf_dep] 前置已按项目或 Mod ID 识别: {}", dep_fname);
    } else if dep_path.exists() {
        validate_existing_download(&dep_path, dep_fname, &expected_file)?;
        eprintln!("[cf_dep] 前置已存在且校验通过: {}", dep_fname);
    } else {
        do_download_to_dir_with_fallbacks(
            http,
            game_dir,
            name,
            &dep_download_urls,
            dep_fname,
            "mods",
            Some(cancel_name),
            &expected_file,
            transaction,
        )
        .map_err(|e| format!("下载前置失败 {}: {}", dep_fname, e))?;
        downloaded = true;
    }

    if !already_installed {
        let candidate_ids =
            crate::mod_analyzer::read_mod_ids_from_file(&dep_path, loader).unwrap_or_default();
        if downloaded && installed.overlaps_mod_ids(&candidate_ids) {
            // CurseForge 文件名变化时也以 JAR 内 Mod ID 为准，避免保留新旧两个版本。
            std::fs::remove_file(&dep_path)
                .map_err(|e| format!("删除重复前置失败 {}: {}", dep_fname, e))?;
            eprintln!("[cf_dep] 已安装同 Mod ID 前置，丢弃重复文件: {}", dep_fname);
        } else {
            installed.record_mod_ids(&candidate_ids);
            push_downloaded_mod_path(downloaded_mod_paths, "mods", dep_fname);
            if downloaded {
                eprintln!("[cf_dep] 已下载前置: {}", dep_fname);
                dep_names.push(dep_fname.to_string());
            }
        }
    }
    installed.record_project("cf", &project_id);

    if let Some(child_deps) = dep_file["dependencies"].as_array() {
        download_curseforge_required_dependencies(
            http,
            game_dir,
            name,
            mc_version,
            loader_type,
            loader,
            child_deps,
            visited,
            installed,
            dep_names,
            dep_errors,
            downloaded_mod_paths,
            depth + 1,
            cancel_name,
            transaction,
        )?;
    }

    Ok(())
}

fn push_downloaded_mod_path(paths: &mut Vec<String>, sub_dir: &str, file_name: &str) {
    if sub_dir != "mods" {
        return;
    }
    let Ok(safe_file_name) = safe_path_name(file_name, "文件名") else {
        return;
    };
    if !safe_file_name.to_ascii_lowercase().ends_with(".jar") {
        return;
    }
    let rel_path = format!("mods/{}", safe_file_name);
    if !paths.iter().any(|item| item == &rel_path) {
        paths.push(rel_path);
    }
}

fn do_download_to_dir(
    _http: &reqwest::blocking::Client,
    game_dir: &str,
    name: &str,
    download_url: &str,
    file_name: &str,
    sub_dir: &str,
    cancel_name: Option<&str>,
    expected_file: &ExpectedDownloadFile,
    transaction: &mut ModDownloadTransaction,
) -> Result<String, String> {
    download_to_dir_with_candidates(
        game_dir,
        name,
        &[download_url.to_string()],
        file_name,
        sub_dir,
        cancel_name,
        expected_file,
        transaction,
    )
}

fn download_to_dir_with_candidates(
    game_dir: &str,
    name: &str,
    download_urls: &[String],
    file_name: &str,
    sub_dir: &str,
    cancel_name: Option<&str>,
    expected_file: &ExpectedDownloadFile,
    transaction: &mut ModDownloadTransaction,
) -> Result<String, String> {
    if is_cancelled(cancel_name) {
        return Err("用户取消下载".to_string());
    }
    let dir = resolve_game_dir(game_dir);
    let safe_name = safe_path_name(name, "版本名")?;
    let safe_file_name = safe_path_name(file_name, "文件名")?;
    let safe_sub_dir = match sub_dir {
        "mods" => "mods",
        "resourcepacks" => "resourcepacks",
        "shaderpacks" => "shaderpacks",
        _ => return Err(format!("非法下载目录: {}", sub_dir)),
    };
    let target_dir = version_dir(&dir, &safe_name).join(safe_sub_dir);
    std::fs::create_dir_all(&target_dir).map_err(|e| format!("创建下载目录失败: {}", e))?;
    let dest = target_dir.join(&safe_file_name);

    if dest.exists() {
        validate_existing_download(&dest, &safe_file_name, expected_file)?;
        return Ok(format!("已存在且校验通过: {}", safe_file_name));
    }
    if is_cancelled(cancel_name) {
        return Err("用户取消下载".to_string());
    }

    let candidates = download_urls
        .iter()
        .map(|url| url.trim())
        .filter(|url| !url.is_empty())
        .map(DownloadCandidate::new)
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return Err("没有可用下载地址".to_string());
    }

    let mut options = DownloadEngineOptions::default();
    options.max_global_connections = DownloadEngineOptions::default_global_connection_limit();
    options.max_active_files = 1;
    options.max_connections_per_file = 16;
    let pool = crate::instance::install_download_pool(
        cancel_name.unwrap_or(&safe_file_name),
        options.max_global_connections,
    );
    let manager = DownloadManager::with_options_and_pool(options, pool)?;
    let _manager_registration =
        cancel_name.map(|task_key| crate::instance::register_download_manager(task_key, &manager));
    let request_id = format!(
        "online-resource:{}:{}:{}",
        safe_name, safe_sub_dir, safe_file_name
    );
    let mut request = DownloadRequest::new(request_id, candidates, &dest);
    if let Some(size) = expected_file.size {
        request = request.with_expected_size(size);
    }
    if let Some(sha1) = expected_file.sha1.as_deref() {
        request = request.with_expected_sha1(sha1.to_string());
    }

    if let Err(error) = manager.download_one(request, |_| {}) {
        if is_cancelled(cancel_name) {
            return Err("用户取消下载".to_string());
        }
        return Err(format!("下载失败: {}", error));
    }
    validate_new_download(&dest, &safe_file_name, expected_file)?;
    transaction.record(dest);

    Ok(safe_file_name)
}

pub fn online_mod_cancel_name(name: &str, project_id: &str, version_id: Option<&str>) -> String {
    format!(
        "online-mod:{}:{}:{}",
        name,
        project_id,
        version_id.unwrap_or("")
    )
}

pub fn online_mod_cancel_key(
    game_dir: &str,
    name: &str,
    project_id: &str,
    version_id: Option<&str>,
) -> String {
    crate::instance::scoped_task_key(
        game_dir,
        &online_mod_cancel_name(name, project_id, version_id),
    )
}

fn is_cancelled(cancel_name: Option<&str>) -> bool {
    cancel_name.is_some_and(crate::instance::is_cancelled)
}

fn ensure_not_cancelled(cancel_name: &str) -> Result<(), String> {
    if crate::instance::is_cancelled(cancel_name) {
        Err("用户取消下载".to_string())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        curseforge_file_matches_instance, curseforge_file_mod_aliases, modrinth_dependency_ids,
        modrinth_dependency_version_url, modrinth_version_matches_game,
        modrinth_version_matches_loader, online_mod_cancel_key, validate_existing_download,
        validate_new_download, ExpectedDownloadFile, InstalledDependencyIndex,
        OnlineModCancelGuard,
    };
    use std::io::Write;

    fn test_file(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "oaoi-{}-{}-{}",
            name,
            std::process::id(),
            super::MOD_DOWNLOAD_TMP_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ))
    }

    #[test]
    fn 同名下载文件必须通过元数据校验() {
        let path = test_file("existing-mod.jar");
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(b"oaoi-mod-download-test").unwrap();
        drop(file);
        let (sha1, _, _, size) = crate::modpack_export::hash_update_candidate(&path).unwrap();

        let expected = ExpectedDownloadFile {
            size: Some(size),
            sha1: Some(sha1),
        };
        assert!(validate_existing_download(&path, "existing-mod.jar", &expected).is_ok());
        assert!(validate_existing_download(
            &path,
            "existing-mod.jar",
            &ExpectedDownloadFile {
                size: Some(size + 1),
                sha1: None,
            },
        )
        .is_err());
        assert!(validate_existing_download(
            &path,
            "existing-mod.jar",
            &ExpectedDownloadFile::default(),
        )
        .is_err());
        assert!(validate_new_download(
            &path,
            "downloaded-mod.jar",
            &ExpectedDownloadFile {
                size: Some(size + 1),
                sha1: None,
            },
        )
        .is_err());

        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn 相同取消名称不能覆盖已有任务() {
        let name = format!(
            "online-mod-test-{}-{}",
            std::process::id(),
            super::MOD_DOWNLOAD_TMP_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        let first = OnlineModCancelGuard::register(&name).unwrap();
        assert!(OnlineModCancelGuard::register(&name).is_err());
        drop(first);
        assert!(OnlineModCancelGuard::register(&name).is_ok());
    }

    #[test]
    fn 不同游戏目录的同名在线_mod_任务互不冲突() {
        let first = online_mod_cancel_key("C:/games/a", "实例", "project", Some("version"));
        let second = online_mod_cancel_key("C:/games/b", "实例", "project", Some("version"));
        assert_ne!(first, second);
    }

    #[test]
    fn 放宽_modrinth_查询后仍校验_loader() {
        let fabric = serde_json::json!({ "loaders": ["fabric", "quilt"] });
        assert!(modrinth_version_matches_loader(&fabric, "fabric", "mod"));
        assert!(!modrinth_version_matches_loader(&fabric, "forge", "mod"));
        assert!(modrinth_version_matches_loader(
            &fabric,
            "forge",
            "resourcepack"
        ));
    }

    #[test]
    fn 手动选择版本仍校验游戏版本和_loader() {
        let modrinth = serde_json::json!({
            "game_versions": ["1.21.1"],
            "loaders": ["fabric"]
        });
        assert!(modrinth_version_matches_game(&modrinth, "1.21.1"));
        assert!(!modrinth_version_matches_game(&modrinth, "1.20.1"));

        let curseforge = serde_json::json!({
            "gameVersions": ["1.21.1", "Fabric"]
        });
        assert!(curseforge_file_matches_instance(
            &curseforge,
            "1.21.1",
            "fabric",
            "mod"
        ));
        assert!(!curseforge_file_matches_instance(
            &curseforge,
            "1.20.1",
            "fabric",
            "mod"
        ));
        assert!(!curseforge_file_matches_instance(
            &curseforge,
            "1.21.1",
            "forge",
            "mod"
        ));
    }

    #[test]
    fn modrinth_前置允许仅提供_version_id() {
        let dependency = serde_json::json!({
            "dependency_type": "required",
            "version_id": "version-only"
        });
        let (project_id, version_id) = modrinth_dependency_ids(&dependency);
        assert_eq!(project_id, None);
        assert_eq!(version_id, Some("version-only"));
        assert_eq!(
            modrinth_dependency_version_url(&dependency, None, "1.21.1", "fabric").unwrap(),
            "https://api.modrinth.com/v2/version/version-only"
        );
    }

    #[test]
    fn 已安装前置同时支持项目_id_和_mod_id() {
        let mut installed = InstalledDependencyIndex::default();
        installed.record_project("mr", "ProjectABC");
        installed.record_mod_id("fabric-api-base");

        assert!(installed.has_project("mr", "projectabc"));
        assert!(installed.has_mod_aliases(&["fabric_api_base".to_string()]));
        assert!(!installed.has_project("cf", "projectabc"));
    }

    #[test]
    fn curseforge_模块名可作为_mod_id_候选() {
        let file = serde_json::json!({
            "modules": [
                { "name": "fabric-api-base" },
                { "name": "fabric-resource-loader-v0" }
            ]
        });
        assert_eq!(
            curseforge_file_mod_aliases(&file),
            vec![
                "fabric-api-base".to_string(),
                "fabric-resource-loader-v0".to_string()
            ]
        );
    }
}
