use crate::instance::{
    cf_api_key, detect_loader, resolve_game_dir, safe_join, safe_path_name, version_dir,
    version_json_path,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha512};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::Emitter;

use crate::mod_update::{load_cached_mrpack_downloads, MrpackDownloadCacheEntry};
use crate::modpack::{sanitize_name, strip_modpack_archive_suffix};
use crate::modpack_sources::safe_index_name;

const MOD_UPDATE_CACHE_DIR: &str = "launcher-data";
const MOD_UPDATE_CACHE_SUBDIR: &str = "mod-update-cache";

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportResult {
    pub path: String,
    pub format: String,
    pub total_files: usize,
    pub linked_files: usize,
    pub bundled_files: usize,
    pub warnings: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportItem {
    pub path: String,
    pub label: String,
    pub kind: String,
    pub size: u64,
    pub count: usize,
    pub default_checked: bool,
}

#[derive(Clone)]
struct FileCandidate {
    rel: String,
    size: u64,
    sha1: String,
    sha512: String,
    cf_fingerprint: u32,
}

#[derive(Clone)]
struct PackageFile {
    rel: String,
    path: PathBuf,
}

#[derive(Clone)]
struct MrResolved {
    downloads: Vec<String>,
}

#[derive(Clone)]
struct CfResolved {
    project_id: u32,
    file_id: u32,
    downloads: Vec<String>,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExportModUpdateCache {
    #[serde(default)]
    files: HashMap<String, ExportCachedModFile>,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExportCachedModFile {
    #[serde(default)]
    file_name: String,
    #[serde(default)]
    rel: String,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    modified_ms: u64,
    #[serde(default)]
    sha1: Option<String>,
    #[serde(default)]
    curseforge_project_id: Option<u32>,
    #[serde(default)]
    curseforge_file_id: Option<u32>,
}

#[tauri::command]
pub async fn export_modpack(
    app_handle: tauri::AppHandle,
    game_dir: String,
    name: String,
    format: String,
    export_name: Option<String>,
    export_version: Option<String>,
    output_dir: Option<String>,
    include_paths: Vec<String>,
) -> Result<ExportResult, String> {
    tokio::task::spawn_blocking(move || {
        export_modpack_blocking(
            &app_handle,
            &game_dir,
            &name,
            &format,
            export_name.as_deref(),
            export_version.as_deref(),
            output_dir.as_deref(),
            include_paths,
        )
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn get_modpack_export_items(
    game_dir: String,
    name: String,
) -> Result<Vec<ExportItem>, String> {
    tokio::task::spawn_blocking(move || get_modpack_export_items_blocking(&game_dir, &name))
        .await
        .map_err(|e| e.to_string())?
}

fn export_modpack_blocking(
    app_handle: &tauri::AppHandle,
    game_dir: &str,
    name: &str,
    format: &str,
    export_name: Option<&str>,
    export_version: Option<&str>,
    output_dir: Option<&str>,
    include_paths: Vec<String>,
) -> Result<ExportResult, String> {
    emit_export_progress(app_handle, name, "prepare", 0, 100, "Preparing export...");
    let safe_name = safe_path_name(name, "version name")?;
    let game_root = resolve_game_dir(game_dir);
    let inst_dir = version_dir(&game_root, &safe_name);
    if !inst_dir.is_dir() {
        return Err(format!("Instance not found: {}", safe_name));
    }

    let instance_json = read_instance_json(&inst_dir, &safe_name)?;
    let raw_pack_name = export_name
        .filter(|v| !v.trim().is_empty())
        .or_else(|| {
            instance_json["name"]
                .as_str()
                .filter(|v| !v.trim().is_empty())
        })
        .unwrap_or(&safe_name)
        .trim()
        .to_string();
    let mut pack_name = strip_modpack_archive_suffix(&raw_pack_name);
    if pack_name.trim().is_empty() {
        pack_name = strip_modpack_archive_suffix(&safe_name);
    }
    if pack_name.trim().is_empty() {
        pack_name = safe_name.clone();
    }
    let mut export_base_name = sanitize_name(&pack_name);
    export_base_name = strip_modpack_archive_suffix(&export_base_name);
    if export_base_name.trim().is_empty() {
        export_base_name = strip_modpack_archive_suffix(&safe_name);
    }
    if export_base_name.trim().is_empty() {
        export_base_name = safe_name.clone();
    }
    let pack_version = export_version
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("1.0.0")
        .to_string();
    let mc_version = instance_json["clientVersion"]
        .as_str()
        .or_else(|| instance_json["mcVersion"].as_str())
        .or_else(|| instance_json["id"].as_str())
        .unwrap_or("")
        .to_string();
    if mc_version.is_empty() {
        return Err("Instance is missing Minecraft version".to_string());
    }
    let (loader_type, loader_version) = detect_loader(&instance_json, &safe_name);

    let selected_paths = normalize_selected_paths(include_paths)?;
    if selected_paths.is_empty() {
        return Err("No files selected for export".to_string());
    }

    let export_format = normalize_format(format);
    let http = reqwest::blocking::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(15))
        .timeout(std::time::Duration::from_secs(60))
        .user_agent("oaoi-launcher/1.0")
        .build()
        .map_err(|e| e.to_string())?;

    let export_dir = output_dir
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| game_root.join("exports"));
    if export_dir.exists() && !export_dir.is_dir() {
        return Err(format!(
            "Export location is not a directory: {}",
            export_dir.display()
        ));
    }
    std::fs::create_dir_all(&export_dir).map_err(|e| e.to_string())?;

    match export_format.as_str() {
        "modrinth" | "mrpack" => {
            emit_export_progress(
                app_handle,
                name,
                "scan",
                5,
                100,
                "Scanning selected files...",
            );
            let cached_mrpack_downloads =
                load_cached_mrpack_downloads(&game_root, &safe_name, &mc_version, &loader_type);
            let package_paths = collect_package_paths(&inst_dir, &selected_paths)?;
            let (candidates, mut mr_matches) = collect_mrpack_candidates_with_cache(
                app_handle,
                name,
                package_paths,
                &cached_mrpack_downloads,
            )?;
            let cached_cf_matches =
                load_cached_curseforge_matches(&game_root, &safe_name, &candidates);
            emit_export_progress(
                app_handle,
                name,
                "cache",
                20,
                100,
                "Reading cached platform links...",
            );
            let mr_candidates: Vec<FileCandidate> = candidates
                .iter()
                .filter(|item| !mr_matches.contains_key(&item.sha1))
                .cloned()
                .collect();
            if !mr_candidates.is_empty() {
                emit_export_progress(
                    app_handle,
                    name,
                    "lookup",
                    30,
                    100,
                    "Checking Modrinth fallback files...",
                );
                mr_matches.extend(lookup_modrinth_hashes(&http, &mr_candidates));
            }
            let cf_candidates: Vec<FileCandidate> = candidates
                .iter()
                .filter(|item| {
                    !mr_matches.contains_key(&item.sha1)
                        && !cached_cf_matches.contains_key(&item.sha1)
                })
                .cloned()
                .collect();
            let mut cf_matches = cached_cf_matches;
            if !cf_candidates.is_empty() {
                emit_export_progress(
                    app_handle,
                    name,
                    "lookup",
                    45,
                    100,
                    "Checking CurseForge fallback files...",
                );
                cf_matches.extend(lookup_curseforge_fingerprints(&http, &cf_candidates));
            }
            emit_export_progress(
                app_handle,
                name,
                "write",
                70,
                100,
                "Writing modpack archive...",
            );
            export_mrpack(
                &export_dir,
                &inst_dir,
                &export_base_name,
                &pack_name,
                &pack_version,
                &mc_version,
                &loader_type,
                &loader_version,
                &candidates,
                &mr_matches,
                &cf_matches,
                &selected_paths,
            )
        }
        "curseforge" | "cf" => {
            emit_export_progress(
                app_handle,
                name,
                "scan",
                5,
                100,
                "Reading cached file sources...",
            );
            let package_files = collect_package_paths(&inst_dir, &selected_paths)?;
            let mut cf_matches = load_cached_curseforge_matches_for_packages(
                &game_root,
                &safe_name,
                &package_files,
            )?;
            let fallback_paths: Vec<PackageFile> = package_files
                .iter()
                .filter(|item| !cf_matches.contains_key(&item.rel))
                .cloned()
                .collect();
            if !fallback_paths.is_empty() {
                emit_export_progress(
                    app_handle,
                    name,
                    "lookup",
                    25,
                    100,
                    "Checking missing CurseForge IDs...",
                );
                let fallback_candidates = hash_package_paths(app_handle, name, fallback_paths)?;
                let fallback_matches = lookup_curseforge_fingerprints(&http, &fallback_candidates);
                for item in &fallback_candidates {
                    if let Some(cf) = fallback_matches.get(&item.sha1) {
                        cf_matches.insert(item.rel.clone(), cf.clone());
                    }
                }
            }
            emit_export_progress(
                app_handle,
                name,
                "write",
                70,
                100,
                "Writing modpack archive...",
            );
            export_curseforge_pack(
                &export_dir,
                &inst_dir,
                &export_base_name,
                &pack_name,
                &pack_version,
                &mc_version,
                &loader_type,
                &loader_version,
                &package_files,
                &cf_matches,
                &selected_paths,
            )
        }
        other => Err(format!("Unsupported export format: {}", other)),
    }
    .inspect(|_| emit_export_progress(app_handle, name, "done", 100, 100, "Export complete"))
    .inspect_err(|e| emit_export_progress(app_handle, name, "error", 0, 0, e))
}

fn emit_export_progress(
    app_handle: &tauri::AppHandle,
    name: &str,
    stage: &str,
    current: usize,
    total: usize,
    detail: &str,
) {
    let _ = app_handle.emit(
        "modpack-export-progress",
        serde_json::json!({
            "name": name,
            "stage": stage,
            "current": current,
            "total": total,
            "detail": detail,
        }),
    );
}

fn export_mrpack(
    export_dir: &Path,
    inst_dir: &Path,
    safe_name: &str,
    pack_name: &str,
    pack_version: &str,
    mc_version: &str,
    loader_type: &str,
    loader_version: &str,
    candidates: &[FileCandidate],
    mr_matches: &HashMap<String, MrResolved>,
    cf_matches: &HashMap<String, CfResolved>,
    selected_paths: &HashSet<String>,
) -> Result<ExportResult, String> {
    let out_path = unique_export_path(export_dir, &format!("{}.mrpack", safe_name));
    let mut linked = 0usize;
    let mut bundled = 0usize;
    let mut warnings = Vec::new();
    let mut manifest_files = Vec::new();
    let mut bundled_paths = HashSet::new();
    let mut linked_paths = HashSet::new();

    for item in candidates {
        if let Some(mr) = mr_matches.get(&item.sha1) {
            manifest_files.push(serde_json::json!({
                "path": item.rel,
                "hashes": {
                    "sha1": item.sha1,
                    "sha512": item.sha512,
                },
                "downloads": mr.downloads,
                "fileSize": item.size,
            }));
            linked += 1;
            linked_paths.insert(item.rel.clone());
        } else if let Some(cf) = cf_matches
            .get(&item.sha1)
            .filter(|cf| !cf.downloads.is_empty())
        {
            manifest_files.push(serde_json::json!({
                "path": item.rel,
                "hashes": {
                    "sha1": item.sha1,
                    "sha512": item.sha512,
                },
                "downloads": cf.downloads,
                "fileSize": item.size,
            }));
            linked += 1;
            linked_paths.insert(item.rel.clone());
        } else {
            bundled_paths.insert(item.rel.clone());
            bundled += 1;
        }
    }

    if candidates.len() > 0 && linked == 0 {
        warnings.push("No platform matches were found; all package files were bundled".to_string());
    } else if bundled > 0 {
        warnings.push(format!(
            "{} files were not found on Modrinth and were bundled in overrides",
            bundled
        ));
    }

    let mut dependencies = serde_json::Map::new();
    dependencies.insert(
        "minecraft".to_string(),
        serde_json::Value::String(mc_version.to_string()),
    );
    if loader_type != "vanilla" && !loader_version.is_empty() {
        let export_loader_version = export_loader_version(loader_type, mc_version, loader_version);
        dependencies.insert(
            mr_loader_key(loader_type).to_string(),
            serde_json::Value::String(export_loader_version),
        );
    }

    let manifest = serde_json::json!({
        "formatVersion": 1,
        "game": "minecraft",
        "versionId": pack_version,
        "name": pack_name,
        "summary": "Exported by oaoi",
        "files": manifest_files,
        "dependencies": dependencies,
    });

    let override_count = write_zip(&out_path, |zip| {
        write_json_file(zip, "modrinth.index.json", &manifest)?;
        write_standard_overrides(zip, inst_dir, &bundled_paths, selected_paths, &linked_paths)
    })?;

    Ok(ExportResult {
        path: out_path.to_string_lossy().to_string(),
        format: "modrinth".to_string(),
        total_files: linked + override_count,
        linked_files: linked,
        bundled_files: override_count,
        warnings,
    })
}

fn export_curseforge_pack(
    export_dir: &Path,
    inst_dir: &Path,
    safe_name: &str,
    pack_name: &str,
    pack_version: &str,
    mc_version: &str,
    loader_type: &str,
    loader_version: &str,
    package_files: &[PackageFile],
    cf_matches: &HashMap<String, CfResolved>,
    selected_paths: &HashSet<String>,
) -> Result<ExportResult, String> {
    let out_path = unique_export_path(export_dir, &format!("{}.zip", safe_name));
    let mut linked = 0usize;
    let mut bundled = 0usize;
    let mut warnings = Vec::new();
    let mut manifest_files = Vec::new();
    let mut bundled_paths = HashSet::new();
    let mut linked_paths = HashSet::new();
    let mut seen_cf_files = HashSet::new();

    for item in package_files {
        if let Some(cf) = cf_matches.get(&item.rel) {
            if seen_cf_files.insert(cf.file_id) {
                manifest_files.push(serde_json::json!({
                    "projectID": cf.project_id,
                    "fileID": cf.file_id,
                    "required": true,
                }));
                linked += 1;
                linked_paths.insert(item.rel.clone());
            } else {
                bundled_paths.insert(item.rel.clone());
                bundled += 1;
            }
        } else {
            bundled_paths.insert(item.rel.clone());
            bundled += 1;
        }
    }

    if package_files.len() > 0 && linked == 0 {
        warnings
            .push("No CurseForge matches were found; all package files were bundled".to_string());
    } else if bundled > 0 {
        warnings.push(format!(
            "{} files were not found on CurseForge and were bundled in overrides",
            bundled
        ));
    }

    let mut mod_loaders = Vec::new();
    if loader_type != "vanilla" && !loader_version.is_empty() {
        let export_loader_version = export_loader_version(loader_type, mc_version, loader_version);
        mod_loaders.push(serde_json::json!({
            "id": format!("{}-{}", cf_loader_key(loader_type), export_loader_version),
            "primary": true,
        }));
    }

    let manifest = serde_json::json!({
        "minecraft": {
            "version": mc_version,
            "modLoaders": mod_loaders,
        },
        "manifestType": "minecraftModpack",
        "manifestVersion": 1,
        "name": pack_name,
        "version": pack_version,
        "author": "oaoi",
        "files": manifest_files,
        "overrides": "overrides",
    });

    let override_count = write_zip(&out_path, |zip| {
        write_json_file(zip, "manifest.json", &manifest)?;
        write_standard_overrides(zip, inst_dir, &bundled_paths, selected_paths, &linked_paths)
    })?;

    Ok(ExportResult {
        path: out_path.to_string_lossy().to_string(),
        format: "curseforge".to_string(),
        total_files: linked + override_count,
        linked_files: linked,
        bundled_files: override_count,
        warnings,
    })
}

fn collect_package_paths(
    inst_dir: &Path,
    selected_paths: &HashSet<String>,
) -> Result<Vec<PackageFile>, String> {
    let mut out = Vec::new();
    for sub in ["mods", "resourcepacks", "shaderpacks"] {
        let dir = inst_dir.join(sub);
        if !dir.is_dir() {
            continue;
        }
        let whole_dir_selected = selected_paths.contains(sub);
        for entry in std::fs::read_dir(&dir).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let path = entry.path();
            if !path.is_file() || should_skip_package_file(&path) {
                continue;
            }
            let file_name = path
                .file_name()
                .and_then(|v| v.to_str())
                .ok_or_else(|| "Invalid file name".to_string())?
                .to_string();
            let safe_file = safe_path_name(&file_name, "file name")?;
            let rel = format!("{}/{}", sub, safe_file);
            if !whole_dir_selected && !selected_paths.contains(&rel) {
                continue;
            }
            out.push(PackageFile { rel, path });
        }
    }
    out.sort_by(|a, b| a.rel.to_lowercase().cmp(&b.rel.to_lowercase()));
    Ok(out)
}

fn collect_mrpack_candidates_with_cache(
    app_handle: &tauri::AppHandle,
    name: &str,
    package_paths: Vec<PackageFile>,
    cached: &HashMap<String, MrpackDownloadCacheEntry>,
) -> Result<(Vec<FileCandidate>, HashMap<String, MrResolved>), String> {
    let total = package_paths.len().max(1);
    let mut out = Vec::new();
    let mut matches = HashMap::new();
    let mut needs_hash = Vec::new();

    for (index, package) in package_paths.into_iter().enumerate() {
        let progress = 5 + ((index + 1) * 15 / total);
        emit_export_progress(
            app_handle,
            name,
            "scan",
            progress,
            100,
            &format!("Reading cached file links... {}/{}", index + 1, total),
        );
        let metadata = std::fs::metadata(&package.path).map_err(|e| e.to_string())?;
        let size = metadata.len();
        let modified_ms = metadata
            .modified()
            .ok()
            .map(system_time_ms)
            .unwrap_or_default();
        if let Some(candidate) = cached_mrpack_candidate(&package, cached, size, modified_ms)? {
            if let Some(entry) = cached.get(&package.rel) {
                matches.insert(
                    candidate.sha1.clone(),
                    MrResolved {
                        downloads: entry.downloads.clone(),
                    },
                );
            }
            out.push(candidate);
        } else {
            needs_hash.push(package);
        }
    }

    let hashed = hash_package_paths(app_handle, name, needs_hash)?;
    for item in hashed {
        if let Some(entry) = cached.get(&item.rel) {
            if entry.size == item.size
                && entry.sha1.eq_ignore_ascii_case(&item.sha1)
                && !entry.downloads.is_empty()
            {
                matches.insert(
                    item.sha1.clone(),
                    MrResolved {
                        downloads: entry.downloads.clone(),
                    },
                );
            }
        }
        out.push(item);
    }
    out.sort_by(|a, b| a.rel.to_lowercase().cmp(&b.rel.to_lowercase()));
    Ok((out, matches))
}

fn cached_mrpack_candidate(
    package: &PackageFile,
    cached: &HashMap<String, MrpackDownloadCacheEntry>,
    size: u64,
    modified_ms: u64,
) -> Result<Option<FileCandidate>, String> {
    let Some(entry) = cached.get(&package.rel) else {
        return Ok(None);
    };
    if entry.size != size || entry.modified_ms != modified_ms || entry.downloads.is_empty() {
        return Ok(None);
    }
    let sha512 = match entry.sha512.clone() {
        Some(value) => value,
        None => hash_sha512_file(&package.path)?,
    };
    Ok(Some(FileCandidate {
        rel: package.rel.clone(),
        size,
        sha1: entry.sha1.clone(),
        sha512,
        cf_fingerprint: entry.fingerprint.unwrap_or_default(),
    }))
}

fn system_time_ms(value: SystemTime) -> u64 {
    value
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or_default()
}

fn hash_sha512_file(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|e| e.to_string())?;
    let mut sha512 = Sha512::new();
    let mut buf = [0u8; 128 * 1024];
    loop {
        let read = file.read(&mut buf).map_err(|e| e.to_string())?;
        if read == 0 {
            break;
        }
        sha512.update(&buf[..read]);
    }
    Ok(format!("{:x}", sha512.finalize()))
}

fn hash_package_paths(
    app_handle: &tauri::AppHandle,
    name: &str,
    package_paths: Vec<PackageFile>,
) -> Result<Vec<FileCandidate>, String> {
    let total = package_paths.len().max(1);
    let mut out = Vec::new();
    for (index, package) in package_paths.into_iter().enumerate() {
        let rel = package.rel;
        let file_name = rel.rsplit('/').next().unwrap_or(&rel);
        let progress = 5 + ((index + 1) * 15 / total);
        emit_export_progress(
            app_handle,
            name,
            "scan",
            progress,
            100,
            &format!(
                "Scanning selected files... {}/{} ({})",
                index + 1,
                total,
                file_name
            ),
        );
        let (sha1, sha512, fingerprint, size) = hash_package_file(&package.path)?;
        out.push(FileCandidate {
            rel,
            size,
            sha1,
            sha512,
            cf_fingerprint: fingerprint,
        });
    }
    out.sort_by(|a, b| a.rel.to_lowercase().cmp(&b.rel.to_lowercase()));
    Ok(out)
}

fn load_cached_curseforge_matches(
    game_root: &Path,
    instance_name: &str,
    candidates: &[FileCandidate],
) -> HashMap<String, CfResolved> {
    let Some(cache) = load_export_mod_update_cache(game_root, instance_name) else {
        return HashMap::new();
    };
    let by_rel: HashMap<String, &FileCandidate> = candidates
        .iter()
        .map(|item| (item.rel.replace('\\', "/"), item))
        .collect();
    let mut out = HashMap::new();

    for file in cache.files.into_values() {
        let rel = file.rel.replace('\\', "/");
        let Some(candidate) = by_rel.get(&rel) else {
            continue;
        };
        let Some(source_sha1) = file.sha1.as_deref() else {
            continue;
        };
        if !source_sha1.eq_ignore_ascii_case(&candidate.sha1) {
            continue;
        }
        let (Some(project_id), Some(file_id)) =
            (file.curseforge_project_id, file.curseforge_file_id)
        else {
            continue;
        };
        let file_name = if file.file_name.is_empty() {
            candidate.rel.rsplit('/').next().unwrap_or("")
        } else {
            file.file_name.as_str()
        };
        let downloads = curseforge_download_candidates(file_id as u64, file_name, "");
        out.insert(
            candidate.sha1.clone(),
            CfResolved {
                project_id,
                file_id,
                downloads,
            },
        );
    }

    out
}

fn load_cached_curseforge_matches_for_packages(
    game_root: &Path,
    instance_name: &str,
    package_files: &[PackageFile],
) -> Result<HashMap<String, CfResolved>, String> {
    let Some(cache) = load_export_mod_update_cache(game_root, instance_name) else {
        return Ok(HashMap::new());
    };
    let by_rel: HashMap<String, &PackageFile> = package_files
        .iter()
        .map(|item| (item.rel.replace('\\', "/"), item))
        .collect();
    let mut out = HashMap::new();

    for file in cache.files.into_values() {
        let rel = file.rel.replace('\\', "/");
        let Some(package) = by_rel.get(&rel) else {
            continue;
        };
        let (Some(project_id), Some(file_id)) =
            (file.curseforge_project_id, file.curseforge_file_id)
        else {
            continue;
        };
        let metadata = std::fs::metadata(&package.path).map_err(|err| err.to_string())?;
        let modified_ms = metadata
            .modified()
            .ok()
            .map(system_time_ms)
            .unwrap_or_default();
        // CF 导出缓存命中只看本地文件指纹级元数据，避免全量重新 hash。
        if file.size != metadata.len() || file.modified_ms != modified_ms {
            continue;
        }
        let file_name = if file.file_name.is_empty() {
            package.rel.rsplit('/').next().unwrap_or("")
        } else {
            file.file_name.as_str()
        };
        let downloads = curseforge_download_candidates(file_id as u64, file_name, "");
        out.insert(
            package.rel.clone(),
            CfResolved {
                project_id,
                file_id,
                downloads,
            },
        );
    }

    Ok(out)
}

fn get_modpack_export_items_blocking(
    game_dir: &str,
    name: &str,
) -> Result<Vec<ExportItem>, String> {
    let safe_name = safe_path_name(name, "version name")?;
    let game_root = resolve_game_dir(game_dir);
    let inst_dir = version_dir(&game_root, &safe_name);
    if !inst_dir.is_dir() {
        return Err(format!("Instance not found: {}", safe_name));
    }

    let mut out = Vec::new();
    let mut dir_handles = Vec::new();
    for entry in std::fs::read_dir(&inst_dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|v| v.to_str()) else {
            continue;
        };
        if should_hide_export_root_item(name) {
            continue;
        }
        let safe = match safe_path_name(name, "export item") {
            Ok(v) => v,
            Err(_) => continue,
        };
        if path.is_dir() {
            let item_path = safe.clone();
            dir_handles.push(std::thread::spawn(
                move || -> Result<Option<ExportItem>, String> {
                    let (size, count) = dir_stats(&path)?;
                    if count == 0 {
                        return Ok(None);
                    }
                    Ok(Some(ExportItem {
                        path: item_path.clone(),
                        label: export_label(&item_path).to_string(),
                        kind: "folder".to_string(),
                        size,
                        count,
                        default_checked: default_export_checked(&item_path),
                    }))
                },
            ));
        } else if path.is_file() {
            if !should_include_export_root_file(&safe) {
                continue;
            }
            let size = path.metadata().map(|m| m.len()).unwrap_or(0);
            out.push(ExportItem {
                path: safe.clone(),
                label: export_label(&safe).to_string(),
                kind: "file".to_string(),
                size,
                count: 1,
                default_checked: default_export_checked(&safe),
            });
        }
    }
    for handle in dir_handles {
        match handle.join() {
            Ok(Ok(Some(item))) => out.push(item),
            Ok(Ok(None)) => {}
            Ok(Err(err)) => return Err(err),
            Err(_) => return Err("Export item scan worker panicked".to_string()),
        }
    }

    out.sort_by(|a, b| {
        let ka = export_sort_key(&a.path);
        let kb = export_sort_key(&b.path);
        ka.cmp(&kb)
            .then_with(|| a.path.to_lowercase().cmp(&b.path.to_lowercase()))
    });
    Ok(out)
}

fn normalize_selected_paths(paths: Vec<String>) -> Result<HashSet<String>, String> {
    let mut out = HashSet::new();
    for raw in paths {
        let trimmed = raw.trim().replace('\\', "/");
        if trimmed.is_empty() {
            continue;
        }
        let normalized = safe_relative_path(&trimmed)?;
        if should_hide_export_root_item(
            normalized
                .split('/')
                .next()
                .ok_or_else(|| "Invalid export path".to_string())?,
        ) {
            continue;
        }
        out.insert(normalized);
    }
    Ok(out)
}

fn safe_relative_path(value: &str) -> Result<String, String> {
    let mut parts = Vec::new();
    for component in Path::new(value).components() {
        match component {
            std::path::Component::Normal(part) => {
                let text = part
                    .to_str()
                    .ok_or_else(|| format!("Invalid export path: {}", value))?;
                safe_path_name(text, "export path")?;
                parts.push(text.to_string());
            }
            std::path::Component::CurDir => {}
            _ => return Err(format!("Invalid export path: {}", value)),
        }
    }
    if parts.is_empty() {
        return Err("Export path cannot be empty".to_string());
    }
    Ok(parts.join("/"))
}

fn dir_stats(dir: &Path) -> Result<(u64, usize), String> {
    let mut size = 0u64;
    let mut count = 0usize;
    for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        let name = path.file_name().and_then(|v| v.to_str()).unwrap_or("");
        if name.starts_with('.') || name.ends_with(".tmp") || name.ends_with(".download") {
            continue;
        }
        if path.is_dir() {
            let (sub_size, sub_count) = dir_stats(&path)?;
            size += sub_size;
            count += sub_count;
        } else if path.is_file() {
            size += path.metadata().map(|m| m.len()).unwrap_or(0);
            count += 1;
        }
    }
    Ok((size, count))
}

fn should_hide_export_root_item(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower.starts_with('.')
        || lower.ends_with("-natives")
        || lower.ends_with(".jar")
        || lower.ends_with(".json")
        || matches!(
            lower.as_str(),
            "launch_output.log"
                | "logs"
                | "crash-reports"
                | "screenshots"
                | "natives"
                | "libraries"
                | "assets"
                | "runtime"
                | "versions"
                | "downloads"
                | "pcl"
                | "usercache.json"
                | "usernamecache.json"
                | "launcher_profiles.json"
        )
}

fn should_include_export_root_file(name: &str) -> bool {
    let lower = name.to_lowercase();
    matches!(
        lower.as_str(),
        "options.txt"
            | "optionsof.txt"
            | "optionsshaders.txt"
            | "servers.dat"
            | "servers.dat_old"
            | "resourcepacks.txt"
    )
}

fn default_export_checked(path: &str) -> bool {
    !matches!(path.to_lowercase().as_str(), "saves" | "servers.dat_old")
}

fn export_label(path: &str) -> &str {
    match path.to_lowercase().as_str() {
        "mods" => "Mod",
        "resourcepacks" => "材质包",
        "shaderpacks" => "光影包",
        "config" => "配置文件",
        "defaultconfigs" => "默认配置",
        "kubejs" => "KubeJS 脚本",
        "patchouli_books" => "帕秋莉书籍",
        "scripts" => "脚本",
        "fancymenu" => "FancyMenu",
        "saves" => "存档",
        "options.txt" => "游戏设置",
        "optionsof.txt" => "OptiFine 设置",
        "optionsshaders.txt" => "光影设置",
        "servers.dat" => "服务器列表",
        "servers.dat_old" => "服务器列表备份",
        "resourcepacks.txt" => "材质包启用列表",
        _ => path,
    }
}

fn export_sort_key(path: &str) -> usize {
    match path.to_lowercase().as_str() {
        "mods" => 0,
        "resourcepacks" => 1,
        "shaderpacks" => 2,
        "config" => 3,
        "defaultconfigs" => 4,
        "kubejs" => 5,
        "patchouli_books" => 6,
        "scripts" => 7,
        "fancymenu" => 8,
        "options.txt" => 20,
        "optionsof.txt" => 21,
        "optionsshaders.txt" => 22,
        "servers.dat" => 23,
        "saves" => 80,
        _ => 50,
    }
}

fn should_skip_package_file(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|v| v.to_str())
        .unwrap_or("")
        .to_lowercase();
    if name.starts_with('.') || name.ends_with(".tmp") || name.ends_with(".download") {
        return true;
    }
    if name.ends_with(".disabled") {
        return true;
    }
    !(name.ends_with(".jar") || name.ends_with(".zip"))
}

fn hash_package_file(path: &Path) -> Result<(String, String, u32, u64), String> {
    let mut file = File::open(path).map_err(|e| e.to_string())?;
    let mut sha1 = sha1_smol::Sha1::new();
    let mut sha512 = Sha512::new();
    let mut normalized = Vec::new();
    let mut buf = [0u8; 128 * 1024];
    let mut size = 0u64;
    loop {
        let read = file.read(&mut buf).map_err(|e| e.to_string())?;
        if read == 0 {
            break;
        }
        size += read as u64;
        let chunk = &buf[..read];
        sha1.update(chunk);
        sha512.update(chunk);
        normalized.extend(
            chunk
                .iter()
                .copied()
                .filter(|b| !matches!(*b, 9 | 10 | 13 | 32)),
        );
    }
    let fingerprint = curseforge_murmur2(&normalized);
    Ok((
        sha1.digest().to_string(),
        format!("{:x}", sha512.finalize()),
        fingerprint,
        size,
    ))
}

pub(crate) fn hash_update_candidate(path: &Path) -> Result<(String, String, u32, u64), String> {
    hash_package_file(path)
}

fn lookup_modrinth_hashes(
    http: &reqwest::blocking::Client,
    candidates: &[FileCandidate],
) -> HashMap<String, MrResolved> {
    let mut out = HashMap::new();
    let hashes: Vec<String> = candidates.iter().map(|item| item.sha1.clone()).collect();
    for chunk in hashes.chunks(100) {
        let body = serde_json::json!({
            "hashes": chunk,
            "algorithm": "sha1",
        });
        let resp = http
            .post("https://api.modrinth.com/v2/version_files")
            .json(&body)
            .send();
        let Ok(resp) = resp else {
            continue;
        };
        if !resp.status().is_success() {
            continue;
        }
        let Ok(json) = resp.json::<serde_json::Value>() else {
            continue;
        };
        let Some(map) = json.as_object() else {
            continue;
        };
        for (hash, version) in map {
            let mut downloads = Vec::new();
            if let Some(files) = version["files"].as_array() {
                for file in files {
                    if file["hashes"]["sha1"].as_str() == Some(hash.as_str()) {
                        if let Some(url) = file["url"].as_str() {
                            push_unique_url(&mut downloads, url.to_string());
                        }
                    }
                }
            }
            if !downloads.is_empty() {
                out.insert(hash.clone(), MrResolved { downloads });
            }
        }
    }
    out
}

fn load_export_mod_update_cache(
    game_root: &Path,
    instance_name: &str,
) -> Option<ExportModUpdateCache> {
    let path = game_root
        .join(MOD_UPDATE_CACHE_DIR)
        .join(MOD_UPDATE_CACHE_SUBDIR)
        .join(format!("{}.json", safe_index_name(instance_name)));
    let data = std::fs::read_to_string(path).ok()?;
    // CF 导出只信更新缓存里带 sha1 的 ID，避免同名文件串到旧结果。
    serde_json::from_str(&data).ok()
}

fn lookup_curseforge_fingerprints(
    http: &reqwest::blocking::Client,
    candidates: &[FileCandidate],
) -> HashMap<String, CfResolved> {
    let mut fp_to_sha: HashMap<u32, String> = HashMap::new();
    let mut expected_class_by_sha: HashMap<String, u64> = HashMap::new();
    let mut fps: Vec<u32> = Vec::new();
    for item in candidates {
        fp_to_sha.insert(item.cf_fingerprint, item.sha1.clone());
        if let Some(class_id) = expected_curseforge_class_id(&item.rel) {
            expected_class_by_sha.insert(item.sha1.clone(), class_id);
        }
        fps.push(item.cf_fingerprint);
    }

    let mut matches_by_sha: HashMap<String, Vec<CfResolved>> = HashMap::new();
    for chunk in fps.chunks(1000) {
        let body = serde_json::json!({ "fingerprints": chunk });
        let resp = http
            .post("https://api.curseforge.com/v1/fingerprints/432")
            .header("x-api-key", &cf_api_key())
            .header("Accept", "application/json")
            .json(&body)
            .send();
        let Ok(resp) = resp else {
            continue;
        };
        if !resp.status().is_success() {
            continue;
        }
        let Ok(json) = resp.json::<serde_json::Value>() else {
            continue;
        };
        let Some(matches) = json["data"]["exactMatches"].as_array() else {
            continue;
        };
        for item in matches {
            let file = &item["file"];
            let sha = if let Some(sha) = sha1_from_cf_hashes(&file["hashes"]) {
                sha
            } else {
                let fingerprint = file["fileFingerprint"].as_u64();
                let Some(fingerprint) = fingerprint.and_then(|v| u32::try_from(v).ok()) else {
                    continue;
                };
                let Some(sha) = fp_to_sha.get(&fingerprint).cloned() else {
                    continue;
                };
                sha
            };
            let project_id = item["id"].as_u64().or_else(|| file["modId"].as_u64());
            let file_id = file["id"].as_u64();
            let (Some(project_id), Some(file_id)) = (project_id, file_id) else {
                continue;
            };
            let file_name = file["fileName"].as_str().unwrap_or("");
            let api_download_url = file["downloadUrl"].as_str().unwrap_or("");
            let downloads = curseforge_download_candidates(file_id, file_name, api_download_url);
            matches_by_sha.entry(sha).or_default().push(CfResolved {
                project_id: project_id as u32,
                file_id: file_id as u32,
                downloads,
            });
        }
    }

    let project_ids: HashSet<u32> = matches_by_sha
        .values()
        .flat_map(|items| items.iter().map(|item| item.project_id))
        .collect();
    let class_by_project = lookup_curseforge_project_classes(http, &project_ids);

    let mut out = HashMap::new();
    for (sha, mut matches) in matches_by_sha {
        if let Some(expected_class) = expected_class_by_sha.get(&sha) {
            if let Some(index) = matches
                .iter()
                .position(|item| class_by_project.get(&item.project_id) == Some(expected_class))
            {
                out.insert(sha, matches.remove(index));
                continue;
            }
            if matches
                .iter()
                .any(|item| class_by_project.contains_key(&item.project_id))
            {
                continue;
            }
        }

        if let Some(item) = matches.pop() {
            out.insert(sha, item);
        }
    }
    out
}

fn expected_curseforge_class_id(rel: &str) -> Option<u64> {
    if rel.starts_with("mods/") {
        Some(6)
    } else if rel.starts_with("resourcepacks/") {
        Some(12)
    } else if rel.starts_with("shaderpacks/") {
        Some(6552)
    } else {
        None
    }
}

fn lookup_curseforge_project_classes(
    http: &reqwest::blocking::Client,
    project_ids: &HashSet<u32>,
) -> HashMap<u32, u64> {
    let mut out = HashMap::new();
    let ids: Vec<u32> = project_ids.iter().copied().collect();
    for chunk in ids.chunks(50) {
        let body = serde_json::json!({ "modIds": chunk, "filterPcOnly": true });
        let resp = http
            .post("https://api.curseforge.com/v1/mods")
            .header("x-api-key", &cf_api_key())
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .json(&body)
            .send();
        let Ok(resp) = resp else {
            continue;
        };
        if !resp.status().is_success() {
            continue;
        }
        let Ok(json) = resp.json::<serde_json::Value>() else {
            continue;
        };
        let Some(items) = json["data"].as_array() else {
            continue;
        };
        for item in items {
            let Some(project_id) = item["id"].as_u64().and_then(|v| u32::try_from(v).ok()) else {
                continue;
            };
            let Some(class_id) = item["classId"].as_u64() else {
                continue;
            };
            out.insert(project_id, class_id);
        }
    }
    out
}

fn sha1_from_cf_hashes(value: &serde_json::Value) -> Option<String> {
    value.as_array()?.iter().find_map(|hash| {
        let algo = hash["algo"].as_u64().unwrap_or(0);
        let value = hash["value"].as_str()?.trim();
        if value.len() == 40
            && value.chars().all(|ch| ch.is_ascii_hexdigit())
            && (algo == 1 || algo == 0)
        {
            Some(value.to_ascii_lowercase())
        } else {
            None
        }
    })
}

fn curseforge_download_candidates(
    file_id: u64,
    file_name: &str,
    api_download_url: &str,
) -> Vec<String> {
    let mut urls = Vec::new();
    if file_id > 0 && !file_name.is_empty() {
        let encoded_name = urlencoding::encode(file_name);
        push_unique_url(
            &mut urls,
            format!(
                "https://edge.forgecdn.net/files/{}/{}/{}",
                file_id / 1000,
                file_id % 1000,
                encoded_name
            ),
        );
        push_unique_url(
            &mut urls,
            format!(
                "https://mediafilez.forgecdn.net/files/{}/{}/{}",
                file_id / 1000,
                file_id % 1000,
                encoded_name
            ),
        );
    }
    if !api_download_url.is_empty() {
        push_unique_url(&mut urls, api_download_url.to_string());
    }
    urls
}

fn push_unique_url(urls: &mut Vec<String>, url: String) {
    let normalized = url.trim();
    if normalized.is_empty() || urls.iter().any(|existing| existing == normalized) {
        return;
    }
    urls.push(normalized.to_string());
}

fn write_zip<F>(path: &Path, write: F) -> Result<usize, String>
where
    F: FnOnce(&mut zip::ZipWriter<File>) -> Result<usize, String>,
{
    let file = File::create(path).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipWriter::new(file);
    let written = write(&mut zip)?;
    zip.finish().map_err(|e| e.to_string())?;
    Ok(written)
}

fn write_json_file(
    zip: &mut zip::ZipWriter<File>,
    path: &str,
    value: &serde_json::Value,
) -> Result<(), String> {
    let data = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    write_bytes(zip, path, &data)
}

fn write_standard_overrides(
    zip: &mut zip::ZipWriter<File>,
    inst_dir: &Path,
    bundled_package_paths: &HashSet<String>,
    selected_paths: &HashSet<String>,
    linked_package_paths: &HashSet<String>,
) -> Result<usize, String> {
    let mut written = HashSet::new();
    let mut count = 0usize;

    for rel in bundled_package_paths {
        let src = safe_join(inst_dir, rel)?;
        if src.is_file() {
            if write_file_from_disk(zip, &src, &format!("overrides/{}", rel), &mut written)? {
                count += 1;
            }
        }
    }

    for rel in selected_paths {
        let src = safe_join(inst_dir, rel)?;
        if src.is_dir() {
            count += write_dir_recursive(zip, inst_dir, &src, &mut written, linked_package_paths)?;
        } else if src.is_file() {
            if linked_package_paths.contains(rel) {
                continue;
            }
            if write_file_from_disk(zip, &src, &format!("overrides/{}", rel), &mut written)? {
                count += 1;
            }
        }
    }

    Ok(count)
}

fn write_dir_recursive(
    zip: &mut zip::ZipWriter<File>,
    inst_dir: &Path,
    dir: &Path,
    written: &mut HashSet<String>,
    excluded_paths: &HashSet<String>,
) -> Result<usize, String> {
    let mut count = 0usize;
    for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        let name = path.file_name().and_then(|v| v.to_str()).unwrap_or("");
        if should_skip_override_file(name) {
            continue;
        }
        if path.is_dir() {
            count += write_dir_recursive(zip, inst_dir, &path, written, excluded_paths)?;
        } else if path.is_file() {
            let rel = path
                .strip_prefix(inst_dir)
                .map_err(|e| e.to_string())?
                .to_string_lossy()
                .replace('\\', "/");
            if excluded_paths.contains(&rel) {
                continue;
            }
            if write_file_from_disk(zip, &path, &format!("overrides/{}", rel), written)? {
                count += 1;
            }
        }
    }
    Ok(count)
}

fn write_file_from_disk(
    zip: &mut zip::ZipWriter<File>,
    src: &Path,
    zip_path: &str,
    written: &mut HashSet<String>,
) -> Result<bool, String> {
    let normalized = zip_path.replace('\\', "/");
    if !written.insert(normalized.clone()) {
        return Ok(false);
    }
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .unix_permissions(0o644);
    zip.start_file(&normalized, options)
        .map_err(|e| e.to_string())?;
    let mut input = File::open(src).map_err(|e| e.to_string())?;
    std::io::copy(&mut input, zip)
        .map(|_| true)
        .map_err(|e| e.to_string())
}

fn write_bytes(zip: &mut zip::ZipWriter<File>, path: &str, data: &[u8]) -> Result<(), String> {
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .unix_permissions(0o644);
    zip.start_file(path.replace('\\', "/"), options)
        .map_err(|e| e.to_string())?;
    zip.write_all(data).map_err(|e| e.to_string())
}

fn should_skip_override_file(name: &str) -> bool {
    let lower = name.to_lowercase();
    // 禁用的 Mod 不允许进入导出包，避免 overrides 把 .jar.disabled 带出去。
    lower.starts_with('.')
        || lower.ends_with(".tmp")
        || lower.ends_with(".download")
        || lower.ends_with(".disabled")
}

fn read_instance_json(inst_dir: &Path, name: &str) -> Result<serde_json::Value, String> {
    let path = version_json_path(inst_dir, name);
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read version json: {}", e))?;
    serde_json::from_str(&text).map_err(|e| format!("Invalid version json: {}", e))
}

fn unique_export_path(export_dir: &Path, file_name: &str) -> PathBuf {
    let path = export_dir.join(file_name);
    if !path.exists() {
        return path;
    }
    let stem = Path::new(file_name)
        .file_stem()
        .and_then(|v| v.to_str())
        .unwrap_or("modpack");
    let ext = Path::new(file_name)
        .extension()
        .and_then(|v| v.to_str())
        .unwrap_or("zip");
    for index in 2..1000 {
        let candidate = export_dir.join(format!("{}-{}.{}", stem, index, ext));
        if !candidate.exists() {
            return candidate;
        }
    }
    export_dir.join(file_name)
}

fn normalize_format(format: &str) -> String {
    format.trim().to_lowercase().replace(['.', ' '], "")
}

fn export_loader_version(loader_type: &str, mc_version: &str, loader_version: &str) -> String {
    match loader_type {
        // Forge 系坐标常带 MC 版本前缀，导出清单只写 Loader 版本。
        "forge" | "neoforge" => strip_mc_version_prefix(mc_version, loader_version).to_string(),
        _ => loader_version.to_string(),
    }
}

fn strip_mc_version_prefix<'a>(mc_version: &str, loader_version: &'a str) -> &'a str {
    loader_version
        .strip_prefix(mc_version)
        .and_then(|rest| rest.strip_prefix('-'))
        .unwrap_or(loader_version)
}

fn mr_loader_key(loader: &str) -> &str {
    match loader {
        "fabric" => "fabric-loader",
        "quilt" => "quilt-loader",
        "neoforge" => "neoforge",
        "forge" => "forge",
        _ => loader,
    }
}

fn cf_loader_key(loader: &str) -> &str {
    match loader {
        "neoforge" => "neoforge",
        "fabric" => "fabric",
        "quilt" => "quilt",
        "forge" => "forge",
        _ => loader,
    }
}

fn curseforge_murmur2(data: &[u8]) -> u32 {
    const M: u32 = 0x5bd1e995;
    const R: u32 = 24;
    let len = data.len() as u32;
    let mut h = 1u32 ^ len;
    let mut i = 0usize;

    while i + 4 <= data.len() {
        let mut k = (data[i] as u32)
            | ((data[i + 1] as u32) << 8)
            | ((data[i + 2] as u32) << 16)
            | ((data[i + 3] as u32) << 24);
        k = k.wrapping_mul(M);
        k ^= k >> R;
        k = k.wrapping_mul(M);
        h = h.wrapping_mul(M);
        h ^= k;
        i += 4;
    }

    match data.len() - i {
        3 => {
            h ^= (data[i + 2] as u32) << 16;
            h ^= (data[i + 1] as u32) << 8;
            h ^= data[i] as u32;
            h = h.wrapping_mul(M);
        }
        2 => {
            h ^= (data[i + 1] as u32) << 8;
            h ^= data[i] as u32;
            h = h.wrapping_mul(M);
        }
        1 => {
            h ^= data[i] as u32;
            h = h.wrapping_mul(M);
        }
        _ => {}
    }

    h ^= h >> 13;
    h = h.wrapping_mul(M);
    h ^= h >> 15;
    h
}
