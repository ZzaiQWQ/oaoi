use crate::installer::{download_file_if_needed, mirror_url};
use super::{ModpackMeta, ModpackKind, build_http_client, emit_progress, sanitize_name, detect_target_dir};
use super::cf_download::{cf_cdn_urls, cf_download_mod};
use crate::instance::cf_api_key;

pub fn do_install_modpack_inner(
    app: &tauri::AppHandle,
    zip_file: &std::path::Path,
    game_dir_input: &str,
    java_path: &str,
    use_mirror: bool,
    meta: &ModpackMeta,
    inst_dir: &std::path::Path,
    game_dir: &std::path::Path,
    display_name: &str,
) -> Result<String, String> {
    use crate::installer::vanilla;
    use crate::installer::fabric;
    use crate::installer::quilt;
    use crate::installer::neoforge;
    use crate::installer::forge;
    use crate::installer::make_emitter;

    let inst_name = sanitize_name(&meta.name);

    // 1. 安装基础游戏
    let http = build_http_client(15, 600, 8)?;

    // 整合包安装：基础游戏（client.jar/libs/assets）优先用镜像
    // Mod 下载保持用户原始设置（CurseForge CDN 国内直连就行）
    let mirror_manifest_url = "https://bmclapi2.bangbang93.com/mc/game/version_manifest_v2.json";
    let official_manifest_url = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";

    emit_progress(app, display_name, "meta", 0, 1, "获取版本清单（镜像优先）...");
    let manifest_resp = http.get(mirror_manifest_url)
        .timeout(std::time::Duration::from_secs(8))
        .send();
    let (manifest_resp, game_mirror) = match manifest_resp {
        Ok(r) if r.status().is_success() => {
            eprintln!("[modpack] 镜像源获取清单成功");
            (Ok(r), true)
        }
        _ => {
            eprintln!("[modpack] 镜像源失败，回退到官方源...");
            emit_progress(app, display_name, "meta", 0, 1, "镜像源超时，切换官方源...");
            (http.get(official_manifest_url).send(), use_mirror)
        }
    };

    let manifest: serde_json::Value = manifest_resp
        .map_err(|e| format!("获取版本清单失败: {}", e))?.json()
        .map_err(|e| e.to_string())?;
    let meta_url = manifest["versions"].as_array()
        .and_then(|arr| arr.iter().find(|v| v["id"].as_str() == Some(&meta.mc_version)))
        .and_then(|v| v["url"].as_str())
        .ok_or_else(|| format!("找不到 MC 版本: {}", meta.mc_version))?
        .to_string();
    let meta_url = mirror_url(&meta_url, game_mirror);

    if inst_name.is_empty() { return Err("整合包名称无效".to_string()); }
    std::fs::create_dir_all(inst_dir).map_err(|e| e.to_string())?;
    let inst_json_path = inst_dir.join("instance.json");

    let emit = make_emitter(app, display_name);

    // 基础游戏用 game_mirror（镜像优先），mod 下载用原始 use_mirror
    let mut ver_json = vanilla::install_vanilla(app, display_name, &meta.mc_version, &meta_url, game_dir, inst_dir, &http, game_mirror)?;

    // ===== 按类型分类下载文件（与 loader 安装并行） =====
    // 分类：mods / resourcepacks / shaderpacks / config / other
    fn classify_path(dest: &std::path::Path) -> &'static str {
        let s = dest.to_string_lossy().replace('\\', "/");
        if s.contains("/mods/") { "mods" }
        else if s.contains("/resourcepacks/") || s.contains("/resources/") { "resourcepacks" }
        else if s.contains("/shaderpacks/") || s.contains("/shaders/") { "shaderpacks" }
        else if s.contains("/config/") { "config" }
        else { "other" }
    }

    // 分类计数器
    struct CategoryCounter {
        done: std::sync::atomic::AtomicUsize,
        total: usize,
    }

    let mod_errors = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let all_handles: Vec<std::thread::JoinHandle<()>>;

    // 收集需要下载的文件
    let tasks: Vec<(String, std::path::PathBuf, Option<String>)>;
    {
        let mods_dir = inst_dir.join("mods");
        std::fs::create_dir_all(&mods_dir).ok();

        tasks = match &meta.kind {
            ModpackKind::Modrinth { files } => {
                files.iter().map(|f| {
                    let dest = inst_dir.join(&f.path);
                    (f.url.clone(), dest, f.sha1.clone())
                }).collect()
            }
            ModpackKind::CurseForge { files, .. } => {
                let file_ids: Vec<u32> = files.iter().map(|f| f.file_id).collect();
                let project_ids: Vec<u32> = files.iter().map(|f| f.project_id).collect();
                let file_map: std::collections::HashMap<u32, (u32, u32)> = files.iter()
                    .map(|f| (f.file_id, (f.project_id, f.file_id))).collect();

                let api_client = reqwest::blocking::Client::builder()
                    .connect_timeout(std::time::Duration::from_secs(15))
                    .timeout(std::time::Duration::from_secs(60))
                    .user_agent("OAOI-Launcher/1.0")
                    .build().ok();

                // 先批量获取项目 classId（用于区分 mod/材质包/光影包）
                let mut pid_class: std::collections::HashMap<u32, u64> = std::collections::HashMap::new();
                if let Some(client) = &api_client {
                    let unique_pids: Vec<u32> = {
                        let mut s: std::collections::HashSet<u32> = std::collections::HashSet::new();
                        project_ids.iter().filter(|p| s.insert(**p)).copied().collect()
                    };
                    eprintln!("[cf] 批量获取 {} 个项目 classId (POST /v1/mods)...", unique_pids.len());
                    for chunk in unique_pids.chunks(50) {
                        let body = serde_json::json!({ "modIds": chunk, "filterPcOnly": true });
                        if let Ok(resp) = client.post("https://api.curseforge.com/v1/mods")
                            .header("x-api-key", &cf_api_key())
                            .header("Content-Type", "application/json")
                            .json(&body).send()
                        {
                            if let Ok(json) = resp.json::<serde_json::Value>() {
                                if let Some(data) = json["data"].as_array() {
                                    for proj in data {
                                        let pid = proj["id"].as_u64().unwrap_or(0) as u32;
                                        let cid = proj["classId"].as_u64().unwrap_or(0);
                                        if cid > 0 { pid_class.insert(pid, cid); }
                                    }
                                }
                            }
                        }
                    }
                    eprintln!("[cf] 项目 classId 映射: {} 个 (6=Mod, 12=材质包, 6552=光影)", pid_class.len());
                }

                eprintln!("[cf] 批量获取 {} 个文件信息 (POST /v1/mods/files)...", file_ids.len());

                let mut resolved: Vec<(String, std::path::PathBuf, Option<String>)> = Vec::new();
                let mut unresolved: Vec<(u32, u32)> = Vec::new();

                if let Some(client) = &api_client {
                    for chunk in file_ids.chunks(500) {
                        let body = serde_json::json!({ "fileIds": chunk });
                        match client.post("https://api.curseforge.com/v1/mods/files")
                            .header("x-api-key", &cf_api_key())
                            .header("Content-Type", "application/json")
                            .header("Accept", "application/json")
                            .json(&body).send()
                        {
                            Ok(resp) if resp.status().is_success() => {
                                if let Ok(json) = resp.json::<serde_json::Value>() {
                                    if let Some(data) = json["data"].as_array() {
                                        for item in data {
                                            let fid = item["id"].as_u64().unwrap_or(0) as u32;
                                            let fname = item["fileName"].as_str().unwrap_or("").to_string();
                                            let dl = item["downloadUrl"].as_str().unwrap_or("").to_string();
                                            let pid = file_map.get(&fid).map(|x| x.0).unwrap_or(0);

                                            if fname.is_empty() { unresolved.push((pid, fid)); continue; }

                                            // 优先用项目 classId 判断目录（最可靠）
                                            let (target_dir, file_type) = if let Some(&cid) = pid_class.get(&pid) {
                                                match cid {
                                                    6 => (inst_dir.join("mods"), "mod"),
                                                    12 => { let d = inst_dir.join("resourcepacks"); std::fs::create_dir_all(&d).ok(); (d, "材质包") }
                                                    6552 => { let d = inst_dir.join("shaderpacks"); std::fs::create_dir_all(&d).ok(); (d, "光影") }
                                                    17 => { let d = inst_dir.join("saves"); std::fs::create_dir_all(&d).ok(); (d, "存档") }
                                                    _ => detect_target_dir(item, &fname, inst_dir)
                                                }
                                            } else {
                                                detect_target_dir(item, &fname, inst_dir)
                                            };
                                            if file_type != "mod" {
                                                eprintln!("[cf] {} → {} ({}) [classId={}]", fname, target_dir.display(), file_type, pid_class.get(&pid).unwrap_or(&0));
                                            }
                                            let dest = target_dir.join(&fname);

                                            if !dl.is_empty() {
                                                resolved.push((dl, dest, None));
                                            } else {
                                                let cdns = cf_cdn_urls(fid, &fname);
                                                if let Some(url) = cdns.first() {
                                                    resolved.push((url.clone(), dest, None));
                                                } else {
                                                    unresolved.push((pid, fid));
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            _ => {
                                eprintln!("[cf] 批量 API 失败, 回退到逐个下载");
                                for id in chunk {
                                    if let Some(&(pid, fid)) = file_map.get(id) {
                                        unresolved.push((pid, fid));
                                    }
                                }
                            }
                        }
                    }
                } else {
                    for f in files { unresolved.push((f.project_id, f.file_id)); }
                }

                eprintln!("[cf] 批量解析: {} 已解析, {} 需单独下载", resolved.len(), unresolved.len());

                for (pid, fid) in unresolved {
                    let marker = format!("CF:{}:{}", pid, fid);
                    let dest = mods_dir.join("_cf_placeholder_");
                    resolved.push((marker, dest, None));
                }

                resolved
            }
        };
    }

    // 按类型统计
    let mut category_totals: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for (_, dest, _) in &tasks {
        let cat = classify_path(dest).to_string();
        *category_totals.entry(cat).or_insert(0) += 1;
    }

    // 创建分类计数器
    let categories: std::sync::Arc<std::collections::HashMap<String, CategoryCounter>> =
        std::sync::Arc::new(category_totals.iter().map(|(k, &v)| {
            (k.clone(), CategoryCounter { done: std::sync::atomic::AtomicUsize::new(0), total: v })
        }).collect());

    // 发送初始进度（立刻让前端知道所有类型和总数）
    let stage_names: std::collections::HashMap<&str, &str> = [
        ("mods", "Mod 文件"), ("resourcepacks", "材质包"), ("shaderpacks", "光影包"),
        ("config", "配置文件"), ("other", "其他文件"),
    ].iter().cloned().collect();
    for (cat, counter) in categories.iter() {
        let cat_str = cat.as_str();
        let label = stage_names.get(cat_str).copied().unwrap_or(cat_str);
        eprintln!("[modpack] 分类: {} = {} 个文件", label, counter.total);
        emit_progress(app, display_name, cat, 0, counter.total,
            &format!("{} 0/{}", label, counter.total));
    }

    // 启动并行下载
    let (sem_tx, sem_rx) = std::sync::mpsc::sync_channel::<()>(26);
    for _ in 0..26 { sem_tx.send(()).ok(); }
    let sem_rx = std::sync::Arc::new(std::sync::Mutex::new(sem_rx));
    let sem_tx = std::sync::Arc::new(sem_tx);
    let mod_http = build_http_client(30, 300, 32)?;

    all_handles = tasks.into_iter().map(|(url, dest, sha1)| {
        let cats = categories.clone();
        let errors = mod_errors.clone();
        let h = mod_http.clone();
        let sem_r = sem_rx.clone();
        let sem_s = sem_tx.clone();
        let category = classify_path(&dest).to_string();
        std::thread::spawn(move || {
            let _ = sem_r.lock().unwrap().recv();
            let result = if url.starts_with("CF:") {
                let parts: Vec<&str> = url.split(':').collect();
                let project_id: u32 = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
                let file_id: u32 = parts.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
                let inst_dir_fallback = dest.parent().and_then(|p| p.parent()).unwrap_or(std::path::Path::new("."));
                cf_download_mod(&h, project_id, file_id, inst_dir_fallback)
            } else {
                if let Some(parent) = dest.parent() {
                    std::fs::create_dir_all(parent).ok();
                }
                download_file_if_needed(&h, &url, &dest, sha1.as_deref(), false)
            };
            if let Err(e) = result {
                errors.lock().unwrap().push(format!("{}: {}", url, e));
            }
            if let Some(counter) = cats.get(&category) {
                counter.done.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            let _ = sem_s.send(());
        })
    }).collect();

    // 启动进度汇报线程（每 500ms 汇报所有类型的进度）
    let progress_stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let progress_stop2 = progress_stop.clone();
    let progress_cats = categories.clone();
    let progress_app = app.clone();
    let progress_name = display_name.to_string();
    let progress_thread = std::thread::spawn(move || {
        loop {
            if progress_stop2.load(std::sync::atomic::Ordering::Relaxed) { break; }
            let mut all_done = true;
            for (cat, counter) in progress_cats.iter() {
                let finished = counter.done.load(std::sync::atomic::Ordering::Relaxed);
                emit_progress(&progress_app, &progress_name, cat, finished, counter.total,
                    &format!("{}/{}", finished, counter.total));
                if finished < counter.total { all_done = false; }
            }
            if all_done { break; }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    });

    // ===== 安装 loader（同时 mod 在后台下载中） =====

    // 自动解析 java 路径（当前端传入为空时从本地查找）
    let resolved_java: String;
    let effective_java = if !java_path.is_empty() {
        java_path
    } else {
        let required_major = super::get_required_java_major(&meta.mc_version);
        let javas = crate::java_detect::find_java(Some(game_dir_input.to_string()));
        if let Some(j) = javas.iter().find(|j| j.major == required_major) {
            resolved_java = j.path.clone();
            &resolved_java
        } else {
            emit_progress(app, display_name, "java", 0, 1, &format!("正在下载 Java {}...", required_major));
            match crate::java_download::download_java_sync(required_major, game_dir_input) {
                Ok(p) => { resolved_java = p; &resolved_java }
                Err(e) => return Err(format!("找不到 Java {}，自动下载失败: {}", required_major, e))
            }
        }
    };

    // 安装 loader（失败时先记录错误，等下载线程结束后再返回）
    let loader_result: Result<(), String> = match meta.loader_type.as_str() {
        "fabric" if !meta.loader_version.is_empty() => {
            fabric::install_fabric(app, display_name, &meta.mc_version, &meta.loader_version, game_dir, inst_dir, &http, game_mirror, &mut ver_json)
        }
        "quilt" if !meta.loader_version.is_empty() => {
            quilt::install_quilt(app, display_name, &meta.mc_version, &meta.loader_version, game_dir, inst_dir, &http, game_mirror, &mut ver_json)
        }
        "forge" if !meta.loader_version.is_empty() => {
            forge::install_forge(app, display_name, &meta.mc_version, &meta.loader_version, game_dir, inst_dir, &http, effective_java, game_mirror, &mut ver_json)
        }
        "neoforge" if !meta.loader_version.is_empty() => {
            neoforge::install_neoforge(app, display_name, &meta.mc_version, &meta.loader_version, game_dir, inst_dir, &http, effective_java, game_mirror, &mut ver_json)
        }
        _ => Ok(())
    };


    // 注意: instance.json 的写入移到最后（推荐内存计算后一次性写入）

    // ===== 等待所有文件下载完成 =====
    for h in all_handles { let _ = h.join(); }
    // 停止进度汇报线程
    progress_stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = progress_thread.join();

    // loader 安装失败则在线程清理后返回错误
    loader_result?;

    // 最终汇报每个类型
    for (cat, counter) in categories.iter() {
        let finished = counter.done.load(std::sync::atomic::Ordering::Relaxed);
        emit_progress(app, display_name, cat, finished, counter.total,
            &format!("{}/{}", finished, counter.total));
    }

    let errs = mod_errors.lock().unwrap();
    let total_files: usize = categories.iter().map(|(_, c)| c.total).sum();
    let success_count = total_files - errs.len();
    eprintln!("[modpack] 下载完成: 成功={}, 失败={}, 总计={}", success_count, errs.len(), total_files);
    if !errs.is_empty() {
        for e in errs.iter() {
            eprintln!("[modpack] 失败: {}", e);
        }
    }

    // 复制 overrides
    match &meta.kind {
        ModpackKind::Modrinth { .. } => {
            extract_overrides_modrinth(zip_file, inst_dir, "overrides")?;
            extract_overrides_modrinth(zip_file, inst_dir, "client-overrides")?;
        }
        ModpackKind::CurseForge { override_path, .. } => {
            extract_overrides_cf(zip_file, inst_dir, override_path)?;
        }
    }

    // 根据 mod 数量自动推荐内存
    let mods_dir = inst_dir.join("mods");
    let mod_count = if mods_dir.exists() {
        std::fs::read_dir(&mods_dir).map(|d| d.filter(|e| {
            e.as_ref().ok().map(|e| {
                let name = e.file_name().to_string_lossy().to_lowercase();
                name.ends_with(".jar") || name.ends_with(".zip")
            }).unwrap_or(false)
        }).count()).unwrap_or(0)
    } else { 0 };
    // 优先使用整合包自带的推荐内存，找不到再按 mod 数量估算
    let recommended_mb: u32 = if let Some(pack_mem) = meta.recommended_memory_mb {
        eprintln!("[modpack] 使用整合包自带推荐内存: {}MB", pack_mem);
        pack_mem
    } else {
        let estimated = if mod_count == 0 { 2048 }
            else if mod_count <= 50 { 4096 }
            else if mod_count <= 150 { 6144 }
            else if mod_count <= 250 { 8192 }
            else { 10240 };
        eprintln!("[modpack] 整合包未指定推荐内存，按 Mod 数量({})估算: {}MB", mod_count, estimated);
        estimated
    };
    ver_json["recommendedMemory"] = serde_json::json!(recommended_mb);
    eprintln!("[modpack] Mod 数量: {}, 最终推荐内存: {}MB", mod_count, recommended_mb);

    // 重新写入（因为之前已写过，这里覆盖加上推荐内存）
    std::fs::write(&inst_json_path, serde_json::to_string_pretty(&ver_json).unwrap())
        .map_err(|e| format!("保存实例配置失败: {}", e))?;

    emit("done", 1, 1, &format!("整合包 '{}' 安装完成！", meta.name));
    Ok(format!("整合包 {} 安装成功", inst_name))
}

fn extract_overrides_modrinth(zip_path: &std::path::Path, inst_dir: &std::path::Path, prefix: &str) -> Result<(), String> {
    let file = std::fs::File::open(zip_path).map_err(|e| e.to_string())?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
    let prefix_slash = format!("{}/", prefix);
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| e.to_string())?;
        let name = entry.name().to_string();
        if name.starts_with(&prefix_slash) && !entry.is_dir() {
            let rel = &name[prefix_slash.len()..];
            if rel.is_empty() { continue; }
            let dest = inst_dir.join(rel);
            if let Some(p) = dest.parent() { std::fs::create_dir_all(p).ok(); }
            let mut out = std::fs::File::create(&dest).map_err(|e| e.to_string())?;
            std::io::copy(&mut entry, &mut out).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

fn extract_overrides_cf(zip_path: &std::path::Path, inst_dir: &std::path::Path, override_path: &str) -> Result<(), String> {
    extract_overrides_modrinth(zip_path, inst_dir, override_path)
}
