use crate::installer::{
    download_file_mirror_then_official, download_file_mirror_then_official_with_progress,
    library_rules_value_allowed, maven_name_to_path, parallel_download_mirror_then_official,
    safe_maven_path,
};
use crate::instance::{resolve_game_dir, safe_path_name};
#[cfg(windows)]
use std::os::windows::process::CommandExt;
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
    pub custom_jvm_args: Option<String>,
}

fn split_command_args(input: &str) -> Result<Vec<String>, String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut in_arg = false;

    for ch in input.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            in_arg = true;
            continue;
        }

        if ch == '\\' {
            if quote == Some('\'') {
                current.push(ch);
            } else {
                escaped = true;
            }
            in_arg = true;
            continue;
        }

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

    if escaped {
        current.push('\\');
    }
    if quote.is_some() {
        return Err("JVM 参数引号未闭合".to_string());
    }
    if in_arg {
        args.push(current);
    }

    Ok(args)
}

fn sha1_ok(path: &std::path::Path, expected_sha1: Option<&str>) -> bool {
    if !path.exists() {
        return false;
    }
    let Some(expected_sha1) = expected_sha1 else {
        return true;
    };
    match std::fs::read(path) {
        Ok(data) => sha1_smol::Sha1::from(&data)
            .digest()
            .to_string()
            .eq_ignore_ascii_case(expected_sha1),
        Err(_) => false,
    }
}

fn is_valid_sha1(value: &str) -> bool {
    value.len() == 40 && value.chars().all(|ch| ch.is_ascii_hexdigit())
}

fn repair_launch_files(
    app_handle: &tauri::AppHandle,
    version_name: &str,
    game_dir: &std::path::Path,
    ver_dir: &std::path::Path,
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
            let jar_path = ver_dir.join("client.jar");
            if !sha1_ok(&jar_path, sha1) {
                emit("client", 0, 1, "启动前修复 client.jar...");
                download_file_mirror_then_official_with_progress(
                    &http,
                    url,
                    &jar_path,
                    sha1,
                    None,
                    |done, total| {
                        let total = total.unwrap_or_else(|| done.max(1)).max(1);
                        let current = done.min(usize::MAX as u64) as usize;
                        let total = total.min(usize::MAX as u64) as usize;
                        emit("client", current, total, "启动前修复 client.jar...");
                    },
                )
                .map_err(|e| format!("启动前修复 client.jar 失败: {}", e))?;
                emit("client", 1, 1, "client.jar 修复完成");
            }
        }
    }

    let libs_dir = game_dir.join("libs");
    let mut lib_tasks: Vec<(String, std::path::PathBuf, Option<String>)> = Vec::new();
    if let Some(libs) = json.get("libraries").and_then(|v| v.as_array()) {
        for lib in libs {
            if !library_rules_value_allowed(lib.get("rules")) {
                continue;
            }
            if let Some(artifact) = lib.get("downloads").and_then(|d| d.get("artifact")) {
                if let (Some(path), Some(url)) = (
                    artifact.get("path").and_then(|v| v.as_str()),
                    artifact.get("url").and_then(|v| v.as_str()),
                ) {
                    let Ok(rel_path) = safe_maven_path(path) else {
                        continue;
                    };
                    let dest = libs_dir.join(rel_path);
                    let sha1 = artifact.get("sha1").and_then(|v| v.as_str());
                    if !sha1_ok(&dest, sha1) {
                        lib_tasks.push((url.to_string(), dest, sha1.map(|s| s.to_string())));
                    }
                    continue;
                }
            }

            if let Some(name) = lib.get("name").and_then(|v| v.as_str()) {
                let rel_path = maven_name_to_path(name);
                let Ok(rel_path_buf) = safe_maven_path(&rel_path) else {
                    continue;
                };
                let maven_url = lib
                    .get("url")
                    .and_then(|v| v.as_str())
                    .unwrap_or("https://libraries.minecraft.net/");
                let url = format!("{}/{}", maven_url.trim_end_matches('/'), rel_path);
                let dest = libs_dir.join(rel_path_buf);
                let sha1 = lib.get("sha1").and_then(|v| v.as_str());
                if !sha1_ok(&dest, sha1) {
                    lib_tasks.push((url, dest, sha1.map(|s| s.to_string())));
                }
            }
        }
    }
    if !lib_tasks.is_empty() {
        let total = lib_tasks.len();
        emit(
            "libraries",
            0,
            total,
            &format!("启动前修复 {} 个依赖库...", total),
        );
        let done = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let app_clone = app_handle.clone();
        let done_reporter = done.clone();
        let version_copy = version_name.to_string();
        let reporter = std::thread::spawn(move || loop {
            let finished = done_reporter.load(std::sync::atomic::Ordering::Relaxed);
            let _ = app_clone.emit(
                "install-progress",
                serde_json::json!({
                    "name": version_copy,
                    "stage": "libraries",
                    "current": finished,
                    "total": total,
                    "detail": format!("启动前修复依赖库 {}/{}", finished, total)
                }),
            );
            if finished >= total {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(300));
        });
        let result = parallel_download_mirror_then_official(&http, lib_tasks, &done, 32, None);
        let _ = reporter.join();
        result.map_err(|e| format!("启动前修复依赖库失败: {}", e))?;
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
            let index_path = game_dir
                .join("res")
                .join("indexes")
                .join(format!("{}.json", index_id));
            if !sha1_ok(&index_path, index_sha1) {
                emit("assetIndex", 0, 0, "启动前修复资源索引...");
                let mut index_urls = Vec::new();
                if !index_id.is_empty() && index_id != "unknown" {
                    index_urls.push(format!(
                        "https://bmclapi2.bangbang93.com/indexes/{}.json",
                        index_id
                    ));
                }
                index_urls.push(index_url.to_string());
                let mut last_index_err = String::new();
                let mut index_done = false;
                for url in index_urls {
                    match download_file_mirror_then_official(&http, &url, &index_path, index_sha1, None) {
                        Ok(_) => {
                            index_done = true;
                            break;
                        }
                        Err(e) => {
                            last_index_err = format!("{} ({})", e, url);
                            let _ = std::fs::remove_file(&index_path);
                        }
                    }
                }
                if !index_done {
                    return Err(format!("启动前修复资源索引失败: {}", last_index_err));
                }
                emit("assetIndex", 1, 1, "资源索引修复完成");
            }
            let index_content = std::fs::read_to_string(&index_path)
                .map_err(|e| format!("读取资源索引失败: {}", e))?;
            let index_json: serde_json::Value = serde_json::from_str(&index_content)
                .map_err(|e| format!("解析资源索引失败: {}", e))?;
            if let Some(objects) = index_json.get("objects").and_then(|v| v.as_object()) {
                let mut asset_tasks: Vec<(String, std::path::PathBuf, Option<String>)> = Vec::new();
                for (_name, info) in objects {
                    let hash = info.get("hash").and_then(|v| v.as_str()).unwrap_or("");
                    if !is_valid_sha1(hash) {
                        continue;
                    }
                    let prefix = &hash[..2];
                    let dest = game_dir.join("res").join("objects").join(prefix).join(hash);
                    if sha1_ok(&dest, Some(hash)) {
                        continue;
                    }
                    let url = format!(
                        "https://resources.download.minecraft.net/{}/{}",
                        prefix, hash
                    );
                    asset_tasks.push((url, dest, Some(hash.to_string())));
                }
                if !asset_tasks.is_empty() {
                    let total = asset_tasks.len();
                    emit(
                        "assets",
                        0,
                        total,
                        &format!("启动前修复 {} 个资源文件...", total),
                    );
                    let done = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
                    let app_clone = app_handle.clone();
                    let done_reporter = done.clone();
                    let version_copy = version_name.to_string();
                    let reporter = std::thread::spawn(move || loop {
                        let finished = done_reporter.load(std::sync::atomic::Ordering::Relaxed);
                        let _ = app_clone.emit(
                            "install-progress",
                            serde_json::json!({
                                "name": version_copy,
                                "stage": "assets",
                                "current": finished,
                                "total": total,
                                "detail": format!("启动前修复资源 {}/{}", finished, total)
                            }),
                        );
                        if finished >= total {
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(300));
                    });
                    let result =
                        parallel_download_mirror_then_official(&http, asset_tasks, &done, 32, None);
                    let _ = reporter.join();
                    result.map_err(|e| format!("启动前修复资源文件失败: {}", e))?;
                }
            }
        }
    }

    Ok(())
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
    let game_dir = resolve_game_dir(&options.game_dir);
    if !game_dir.exists() {
        return Err("游戏目录不存在".to_string());
    }

    // 实例目录
    let version_name = safe_path_name(&options.version_name, "版本名")?;
    let ver_dir = game_dir.join("instances").join(&version_name);
    if !ver_dir.exists() {
        return Err(format!("版本 {} 未安装", version_name));
    }

    // 读取实例 JSON
    let version_json_path = ver_dir.join("instance.json");
    let json_str = std::fs::read_to_string(&version_json_path)
        .map_err(|e| format!("读取版本配置失败: {}", e))?;
    let json: serde_json::Value =
        serde_json::from_str(&json_str).map_err(|e| format!("解析版本 JSON 失败: {}", e))?;

    repair_launch_files(&app_handle, &version_name, &game_dir, &ver_dir, &json)?;

    // 获取主类
    let main_class = json["mainClass"]
        .as_str()
        .ok_or("版本 JSON 中缺少 mainClass")?;

    // 获取 asset index
    let asset_index = json["assetIndex"]["id"].as_str().unwrap_or("legacy");

    // 构建 classpath（按 group:artifact 去重）
    let libs_dir = game_dir.join("libs");
    let mut classpath = Vec::new();
    let mut seen_keys: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

    if let Some(libs) = json["libraries"].as_array() {
        for lib in libs {
            if !library_rules_value_allowed(lib.get("rules")) {
                continue;
            }

            // 解析库路径
            let lib_path_opt = if let Some(artifact) = lib["downloads"]["artifact"]["path"].as_str()
            {
                safe_maven_path(artifact).ok().and_then(|path| {
                    let p = libs_dir.join(path);
                    if p.exists() {
                        Some(p.to_string_lossy().to_string())
                    } else {
                        None
                    }
                })
            } else if let Some(name) = lib["name"].as_str() {
                let rel_path = maven_name_to_path(name);
                safe_maven_path(&rel_path).ok().and_then(|path| {
                    let p = libs_dir.join(path);
                    if p.exists() {
                        Some(p.to_string_lossy().to_string())
                    } else {
                        None
                    }
                })
            } else {
                None
            };

            if let Some(path) = lib_path_opt {
                let dedup_key = lib["name"]
                    .as_str()
                    .and_then(|n| {
                        let parts: Vec<&str> = n.split(':').collect();
                        if parts.len() >= 4 {
                            Some(format!("{}:{}:{}", parts[0], parts[1], parts[3]))
                        } else if parts.len() >= 2 {
                            Some(format!("{}:{}", parts[0], parts[1]))
                        } else {
                            None
                        }
                    })
                    .unwrap_or_default();

                if !dedup_key.is_empty() {
                    if let Some(&idx) = seen_keys.get(&dedup_key) {
                        classpath[idx] = path;
                    } else {
                        seen_keys.insert(dedup_key, classpath.len());
                        classpath.push(path);
                    }
                } else {
                    classpath.push(path);
                }
            }
        }
    }

    // 添加版本 jar
    let version_jar = ver_dir.join("client.jar");
    if version_jar.exists() {
        classpath.push(version_jar.to_string_lossy().to_string());
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
    let natives_dir = ver_dir.join("natives");
    if !natives_dir.exists() {
        let _ = std::fs::create_dir_all(&natives_dir);
    }

    // 自动解压 natives（老版本需要 LWJGL native dll）
    let natives_empty = std::fs::read_dir(&natives_dir)
        .map(|mut d| d.next().is_none())
        .unwrap_or(true);
    if natives_empty {
        if let Some(libs) = json["libraries"].as_array() {
            for lib in libs {
                // 只处理有 natives.windows 的库
                let classifier_key = match lib["natives"]["windows"].as_str() {
                    Some(k) => k.to_string(),
                    None => continue,
                };
                // 获取 natives jar 路径
                let native_jar_path = if let Some(cl) =
                    lib["downloads"]["classifiers"][&classifier_key]["path"].as_str()
                {
                    let Ok(path) = safe_maven_path(cl) else {
                        continue;
                    };
                    libs_dir.join(path)
                } else {
                    continue;
                };
                if !native_jar_path.exists() {
                    // 尝试下载
                    if let Some(url) =
                        lib["downloads"]["classifiers"][&classifier_key]["url"].as_str()
                    {
                        if let Some(parent) = native_jar_path.parent() {
                            let _ = std::fs::create_dir_all(parent);
                        }
                        eprintln!("[launch] 下载 native: {}", url);
                        let sha1 =
                            lib["downloads"]["classifiers"][&classifier_key]["sha1"].as_str();
                        let http = reqwest::blocking::Client::builder()
                            .connect_timeout(std::time::Duration::from_secs(15))
                            .timeout(std::time::Duration::from_secs(60))
                            .user_agent("OAOI-Launcher/1.0")
                            .build()
                            .map_err(|e| format!("创建 native 下载客户端失败: {}", e))?;
                        download_file_mirror_then_official(&http, url, &native_jar_path, sha1, None)
                            .map_err(|e| format!("下载 native 失败: {} -> {}", url, e))?;
                    }
                }
                if native_jar_path.exists() {
                    // 解压 dll 文件到 natives 目录
                    if let Ok(file) = std::fs::File::open(&native_jar_path) {
                        if let Ok(mut archive) = zip::ZipArchive::new(file) {
                            for i in 0..archive.len() {
                                if let Ok(mut entry) = archive.by_index(i) {
                                    let name = entry.name().to_string();
                                    if name.ends_with(".dll")
                                        || name.ends_with(".so")
                                        || name.ends_with(".dylib")
                                    {
                                        let Some(filename) = name.rsplit('/').next() else {
                                            continue;
                                        };
                                        let Ok(filename) = safe_path_name(filename, "native文件名")
                                        else {
                                            continue;
                                        };
                                        let out_path = natives_dir.join(filename);
                                        if !out_path.exists() {
                                            if let Ok(mut out) = std::fs::File::create(&out_path) {
                                                let _ = std::io::copy(&mut entry, &mut out);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    eprintln!("[launch] 已解压 natives: {}", native_jar_path.display());
                }
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

    // 生成离线 UUID（使用 "OfflinePlayer:" + 玩家名的 SHA1 前 128 bit，与官方离线模式一致）
    let uuid = {
        let digest = sha1_smol::Sha1::from(format!("OfflinePlayer:{}", options.player_name))
            .digest()
            .bytes();
        // 取前16字节作为 UUID bytes
        let mut bytes = [0u8; 16];
        bytes.copy_from_slice(&digest[..16]);
        // 设置 version 3 (name-based) 和 variant bits
        bytes[6] = (bytes[6] & 0x0f) | 0x30;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        format!("{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            bytes[0], bytes[1], bytes[2], bytes[3],
            bytes[4], bytes[5], bytes[6], bytes[7],
            bytes[8], bytes[9], bytes[10], bytes[11],
            bytes[12], bytes[13], bytes[14], bytes[15])
    };

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

        let replace_vars = |s: &str| -> String {
            let mut r = s
                .replace("${natives_directory}", &natives_dir_str)
                .replace("${library_directory}", &libs_dir_str)
                .replace("${launcher_name}", "oaoi")
                .replace("${launcher_version}", "1.0")
                .replace("${classpath}", &classpath_str)
                .replace("${classpath_separator}", ";")
                .replace("${version_name}", &version_name)
                .replace("${primary_jar_name}", "client.jar");
            // NeoForge: ignoreList 引用 ${version_name}.jar，但我们的是 client.jar
            if r.starts_with("-DignoreList=") {
                r = r.replace(&format!("{}.jar", version_name), "client.jar");
            }
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
    let loader_type = json["loader"]["type"].as_str().unwrap_or("");
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
        let replaced = mc_args_str
            .replace("${auth_player_name}", &options.player_name)
            .replace("${version_name}", &version_name)
            .replace("${game_directory}", &ver_dir.to_string_lossy())
            .replace("${assets_root}", &game_dir.join("res").to_string_lossy())
            .replace("${assets_index_name}", asset_index)
            .replace("${auth_uuid}", options.uuid.as_deref().unwrap_or(&uuid))
            .replace(
                "${auth_access_token}",
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
            .replace("${user_properties}", "{}");
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
            game_dir.join("res").to_string_lossy().to_string(),
            "--assetIndex".to_string(),
            asset_index.to_string(),
            "--uuid".to_string(),
            options.uuid.clone().unwrap_or(uuid.clone()),
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
            for arg in game_args {
                if let Some(s) = arg.as_str() {
                    if !s.contains("${")
                        && !s.starts_with("--username")
                        && !s.starts_with("--version")
                        && !s.starts_with("--gameDir")
                        && !s.starts_with("--assetsDir")
                        && !s.starts_with("--assetIndex")
                        && !s.starts_with("--uuid")
                        && !s.starts_with("--accessToken")
                        && !s.starts_with("--userType")
                        && !s.starts_with("--versionType")
                    {
                        args.push(s.to_string());
                    }
                }
            }
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
    let _ = std::fs::create_dir_all(&mods_dir);

    // 调试日志
    eprintln!("\n[launch] ===== 启动命令 =====");
    eprintln!("[launch] Java: {}", options.java_path);
    eprintln!("[launch] MainClass: {}", main_class);
    eprintln!("[launch] Classpath entries: {}", classpath.len());
    for (i, arg) in args.iter().enumerate() {
        if i > 0 && args.get(i - 1).map(|s| s.as_str()) == Some("--accessToken") && arg != "0" {
            eprintln!("[launch] arg[{}]: *****(已隐藏)", i);
        } else if arg.len() > 200 {
            eprintln!("[launch] arg[{}]: {}... (truncated)", i, &arg[..200]);
        } else {
            eprintln!("[launch] arg[{}]: {}", i, arg);
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
    let mut child = cmd.spawn().map_err(|e| format!("启动游戏失败: {}", e))?;

    let pid = child.id();
    let cp_len = classpath.len();
    let version_for_log = version_name.to_string();
    let log_path_clone = log_path.clone();
    let ver_dir_clone = ver_dir.clone();

    // 后台线程：等待游戏进程退出，崩溃时发送事件
    std::thread::spawn(move || {
        match child.wait() {
            Ok(status) => {
                let exit_code = status.code().unwrap_or(-1);
                if exit_code != 0 {
                    // 非正常退出 → 安全读取日志尾部（最多200行），避免大日志 OOM
                    let log_content = read_tail_lines(&log_path_clone, 200);
                    let mut diagnosis = analyze_crash_log(&log_content, exit_code);

                    // 如果启动日志没匹配到有用规则，再读游戏自己的日志做二次分析
                    let mut combined_log = log_content.clone();
                    if diagnosis.contains("日志最后几行") || diagnosis.contains("日志文件为空")
                    {
                        let game_log =
                            read_tail_lines(&ver_dir_clone.join("logs").join("latest.log"), 100);
                        let fml_log = read_tail_lines(
                            &ver_dir_clone.join("logs").join("fml-client-latest.log"),
                            100,
                        );
                        let game_combined = format!("{}\n{}", game_log, fml_log);
                        let retry = analyze_crash_log(&game_combined, exit_code);
                        if !retry.contains("日志最后几行") && !retry.contains("日志文件为空")
                        {
                            diagnosis = retry;
                        }
                        combined_log = format!("{}\n{}", combined_log, game_combined);
                    }

                    // 截取最后 150 行给 AI 分析
                    let log_lines: Vec<&str> = combined_log.lines().collect();
                    let tail_start = if log_lines.len() > 150 {
                        log_lines.len() - 150
                    } else {
                        0
                    };
                    let log_tail = log_lines[tail_start..].join("\n");
                    // 也尝试读取 crash-reports
                    let crash_report = read_latest_crash_report(&ver_dir_clone);
                    let _ = app_handle.emit(
                        "game-crashed",
                        serde_json::json!({
                            "version": version_for_log,
                            "exit_code": exit_code,
                            "diagnosis": diagnosis,
                            "log_tail": log_tail,
                            "crash_report": crash_report
                        }),
                    );
                } else {
                    // 退出码 0 但可能有 Forge/Fabric Mod 加载错误（弹窗关闭后退出码仍为 0）
                    let game_log =
                        read_tail_lines(&ver_dir_clone.join("logs").join("latest.log"), 100);
                    let fml_log = read_tail_lines(
                        &ver_dir_clone.join("logs").join("fml-client-latest.log"),
                        100,
                    );
                    let combined = format!("{}\n{}", game_log, fml_log);
                    let combined_lower = combined.to_lowercase();

                    // 检测 Forge/Fabric 常见 Mod 错误
                    if combined_lower.contains("missing mods")
                        || combined_lower.contains("there were errors previously")
                        || combined_lower.contains("errors loading minecraft")
                        || combined_lower.contains("missing or unsupported mandatory dependencies")
                        || combined_lower.contains("(missing)")
                        || combined_lower.contains("incompatible mods found")
                    {
                        let diagnosis = analyze_crash_log(&combined, 0);
                        let log_lines: Vec<&str> = combined.lines().collect();
                        let tail_start = if log_lines.len() > 150 {
                            log_lines.len() - 150
                        } else {
                            0
                        };
                        let log_tail = log_lines[tail_start..].join("\n");
                        let _ = app_handle.emit(
                            "game-crashed",
                            serde_json::json!({
                                "version": version_for_log,
                                "exit_code": 0,
                                "diagnosis": diagnosis,
                                "log_tail": log_tail,
                                "crash_report": ""
                            }),
                        );
                    } else {
                        let _ = app_handle.emit(
                            "game-exited",
                            serde_json::json!({
                                "version": version_for_log,
                                "exit_code": 0
                            }),
                        );
                    }
                }
            }
            Err(e) => {
                eprintln!("[launch] 等待进程出错: {}", e);
            }
        }
    });

    // 立即返回启动成功
    Ok(format!(
        "游戏已启动 (PID: {}), 版本: {}, 库: {}/{}",
        pid, version_name, cp_len, total_libs
    ))
}

/// 读取最新的 crash-report 文件内容（如果存在且是最近 2 分钟内的）
fn read_latest_crash_report(game_dir: &std::path::Path) -> String {
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
        // 只读最近 2 分钟内的
        if time.elapsed().map_or(true, |d| d.as_secs() < 120) {
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
