use crate::instance::safe_join;
use tauri::Emitter;

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
    if java_exe.exists() {
        eprintln!("[java] Java {} 已存在: {}", major, java_exe.display());
        return Ok(java_exe.to_string_lossy().to_string());
    }

    // 后台线程下载，不阻塞 UI
    std::thread::spawn(
        move || match do_download_java(&app_handle, major, &game_dir) {
            Ok(path) => {
                let _ = app_handle.emit(
                    "java-download-done",
                    serde_json::json!({
                        "major": major, "success": true, "path": path
                    }),
                );
            }
            Err(e) => {
                eprintln!("[java] 下载失败: {}", e);
                let _ = app_handle.emit(
                    "java-download-done",
                    serde_json::json!({
                        "major": major, "success": false, "error": e
                    }),
                );
            }
        },
    );
    // 立即返回，前端通过事件监听结果
    Ok("downloading".to_string())
}

fn do_download_java(
    app_handle: &tauri::AppHandle,
    major: u32,
    game_dir: &str,
) -> Result<String, String> {
    let java_base = std::path::PathBuf::from(game_dir).join("runtime");
    let java_dir = java_base.join(format!("jre-{}", major));
    let java_exe = java_dir.join("bin").join("java.exe");

    // 已存在，直接返回
    if java_exe.exists() {
        eprintln!("[java] Java {} 已存在: {}", major, java_exe.display());
        return Ok(java_exe.to_string_lossy().to_string());
    }

    eprintln!("[java] 开始下载 Java {} ...", major);
    let _ = app_handle.emit(
        "java-download-progress",
        serde_json::json!({
            "major": major, "stage": "downloading", "detail": format!("正在下载 Java {} ...", major)
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

    let http = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .connect_timeout(std::time::Duration::from_secs(15))
        .user_agent("OAOI-Launcher/1.0")
        .build()
        .map_err(|e| format!("HTTP 客户端创建失败: {}", e))?;

    // 优先使用清华镜像，国内更稳定；失败后回退官方源。
    // 流式下载到临时文件（JRE zip ~200MB，不能读到内存）
    std::fs::create_dir_all(&java_base).map_err(|e| e.to_string())?;
    let tmp_zip = java_base.join(format!("java{}.zip", major));

    /// 流式下载：边读边写磁盘
    fn stream_download(
        http: &reqwest::blocking::Client,
        url: &str,
        dest: &std::path::Path,
        app_handle: &tauri::AppHandle,
        major: u32,
        source_label: &str,
    ) -> Result<u64, String> {
        let mut resp = http
            .get(url)
            .send()
            .map_err(|e| format!("请求失败: {}", e))?;
        if !resp.status().is_success() {
            return Err(format!("HTTP {}", resp.status()));
        }
        let mut file = std::fs::File::create(dest).map_err(|e| format!("创建文件失败: {}", e))?;
        let total = resp.content_length().unwrap_or(0);
        let mut written = 0u64;
        let mut buf = [0u8; 64 * 1024];
        let mut last_emit = std::time::Instant::now();

        loop {
            let n =
                std::io::Read::read(&mut resp, &mut buf).map_err(|e| format!("读取失败: {}", e))?;
            if n == 0 {
                break;
            }
            std::io::Write::write_all(&mut file, &buf[..n])
                .map_err(|e| format!("写入失败: {}", e))?;
            written += n as u64;

            if last_emit.elapsed() >= std::time::Duration::from_millis(500) {
                let percent = if total > 0 {
                    (written as f64 / total as f64 * 100.0).round() as u64
                } else {
                    0
                };
                let detail = if total > 0 {
                    format!("{}下载 Java {} {}%", source_label, major, percent.min(100))
                } else {
                    format!(
                        "{}下载 Java {} {:.1} MB",
                        source_label,
                        major,
                        written as f64 / 1048576.0
                    )
                };
                let _ = app_handle.emit(
                    "java-download-progress",
                    serde_json::json!({
                        "major": major,
                        "stage": "downloading",
                        "source": source_label,
                        "downloaded": written,
                        "total": total,
                        "detail": detail
                    }),
                );
                last_emit = std::time::Instant::now();
            }
        }
        Ok(written)
    }

    fn find_tuna_java_zip(
        http: &reqwest::blocking::Client,
        mirror_url_base: &str,
        major: u32,
    ) -> Result<String, String> {
        let listing = http
            .get(mirror_url_base)
            .send()
            .map_err(|e| format!("镜像源请求失败: {}", e))?
            .text()
            .map_err(|e| e.to_string())?;
        let zip_name = listing
            .lines()
            .filter_map(|line| {
                if let Some(start) = line.find("href=\"") {
                    let rest = &line[start + 6..];
                    if let Some(end) = rest.find('"') {
                        let name = &rest[..end];
                        if name.ends_with(".zip")
                            && name.contains("jre")
                            && name.contains("x64")
                            && name.contains("windows")
                        {
                            return Some(name.to_string());
                        }
                    }
                }
                None
            })
            .last()
            .ok_or_else(|| format!("清华镜像中找不到 Java {} JRE zip", major))?;
        Ok(format!("{}{}", mirror_url_base, zip_name))
    }

    let dl_size = match find_tuna_java_zip(&http, &mirror_url_base, major).and_then(
        |download_url| {
            eprintln!("[java] 尝试清华镜像: {}", download_url);
            let _ = app_handle.emit(
                "java-download-progress",
                serde_json::json!({
                    "major": major,
                    "stage": "downloading",
                    "detail": format!("正在从清华镜像下载 Java {} ...", major)
                }),
            );
            stream_download(&http, &download_url, &tmp_zip, app_handle, major, "镜像")
        },
    ) {
        Ok(size) => size,
        Err(mirror_err) => {
            eprintln!("[java] 清华镜像失败: {}，尝试官方源...", mirror_err);
            let _ = app_handle.emit("java-download-progress", serde_json::json!({
                "major": major, "stage": "downloading", "detail": format!("镜像失败，正在从官方源下载 Java {} ...", major)
            }));
            eprintln!("[java] 尝试官方源: {}", official_url);
            stream_download(&http, &official_url, &tmp_zip, app_handle, major, "官方").map_err(
                |official_err| format!("镜像源失败: {}; 官方源失败: {}", mirror_err, official_err),
            )?
        }
    };
    eprintln!(
        "[java] 下载完成 ({:.1} MB)，解压中...",
        dl_size as f64 / 1048576.0
    );

    let _ = app_handle.emit(
        "java-download-progress",
        serde_json::json!({
            "major": major, "stage": "extracting", "detail": "正在解压..."
        }),
    );

    // 解压 zip
    let zip_file = std::fs::File::open(&tmp_zip).map_err(|e| e.to_string())?;
    let mut archive =
        zip::ZipArchive::new(zip_file).map_err(|e| format!("打开 zip 失败: {}", e))?;

    let top_dir = archive
        .by_index(0)
        .map_err(|e| e.to_string())?
        .name()
        .split('/')
        .next()
        .unwrap_or("")
        .to_string();

    for i in 0..archive.len() {
        let mut file = archive.by_index(i).map_err(|e| e.to_string())?;
        let raw_name = file.name().to_string();
        let relative = if !top_dir.is_empty() && raw_name.starts_with(&top_dir) {
            raw_name[top_dir.len()..]
                .trim_start_matches('/')
                .to_string()
        } else {
            raw_name.clone()
        };
        if relative.is_empty() {
            continue;
        }

        let out_path = safe_join(&java_dir, &relative)?;
        if file.is_dir() {
            let _ = std::fs::create_dir_all(&out_path);
        } else {
            if let Some(parent) = out_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let mut out = std::fs::File::create(&out_path).map_err(|e| e.to_string())?;
            std::io::copy(&mut file, &mut out).map_err(|e| e.to_string())?;
        }
    }

    // 清理 zip
    let _ = std::fs::remove_file(&tmp_zip);

    if java_exe.exists() {
        eprintln!("[java] Java {} 安装完成: {}", major, java_exe.display());
        let _ = app_handle.emit(
            "java-download-progress",
            serde_json::json!({
                "major": major, "stage": "done", "detail": format!("Java {} 安装完成", major)
            }),
        );
        Ok(java_exe.to_string_lossy().to_string())
    } else {
        Err(format!("解压后找不到 java.exe: {}", java_exe.display()))
    }
}

/// 同步下载 Java，供 modpack 安装等无 AppHandle 的场景调用
pub fn download_java_sync(major: u32, game_dir: &str) -> Result<String, String> {
    let java_exe = std::path::PathBuf::from(game_dir)
        .join("runtime")
        .join(format!("jre-{}", major))
        .join("bin")
        .join("java.exe");
    if java_exe.exists() {
        return Ok(java_exe.to_string_lossy().to_string());
    }
    let java_dir = std::path::PathBuf::from(game_dir)
        .join("runtime")
        .join(format!("jre-{}", major));
    std::fs::create_dir_all(&java_dir).map_err(|e| e.to_string())?;

    let official = format!(
        "https://api.adoptium.net/v3/binary/latest/{}/ga/windows/x64/jre/hotspot/normal/eclipse",
        major
    );
    let mirror_base = format!(
        "https://mirrors.tuna.tsinghua.edu.cn/Adoptium/{}/jre/x64/windows/",
        major
    );
    let http = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .user_agent("OAOI-Launcher/1.0")
        .build()
        .map_err(|e| e.to_string())?;

    let zip_path = java_dir.join("java.zip");
    let mut downloaded = false;
    // 流式下载辅助函数
    fn stream_to_file(http: &reqwest::blocking::Client, url: &str, dest: &std::path::Path) -> bool {
        match http.get(url).send() {
            Ok(mut resp) if resp.status().is_success() => match std::fs::File::create(dest) {
                Ok(mut file) => std::io::copy(&mut resp, &mut file).is_ok(),
                Err(_) => false,
            },
            _ => false,
        }
    }
    // 1. 优先解析清华镜像目录页找到真实文件名
    if let Ok(resp) = http.get(&mirror_base).send() {
        if let Ok(listing) = resp.text() {
            let zip_name = listing
                .lines()
                .filter_map(|line| {
                    if let Some(start) = line.find("href=\"") {
                        let rest = &line[start + 6..];
                        if let Some(end) = rest.find('"') {
                            let name = &rest[..end];
                            if name.ends_with(".zip")
                                && name.contains("jre")
                                && name.contains("x64")
                                && name.contains("windows")
                            {
                                return Some(name.to_string());
                            }
                        }
                    }
                    None
                })
                .last();
            if let Some(zip_name) = zip_name {
                let download_url = format!("{}{}", mirror_base, zip_name);
                eprintln!("[java-sync] 镜像下载: {}", download_url);
                if stream_to_file(&http, &download_url, &zip_path) {
                    downloaded = true;
                }
            }
        }
    }
    // 2. 镜像失败 → 官方源回退
    if !downloaded {
        eprintln!("[java-sync] 镜像源失败，尝试官方源...");
        if stream_to_file(&http, &official, &zip_path) {
            downloaded = true;
        }
    }
    if !downloaded {
        return Err(format!("无法下载 Java {}", major));
    }

    let file = std::fs::File::open(&zip_path).map_err(|e| e.to_string())?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;

    // 获取顶级目录名，解压时去掉这层（与异步版本对齐）
    let top_dir = archive
        .by_index(0)
        .map_err(|e| e.to_string())?
        .name()
        .split('/')
        .next()
        .unwrap_or("")
        .to_string();

    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| e.to_string())?;
        let raw_name = entry.name().to_string();
        let relative = if !top_dir.is_empty() && raw_name.starts_with(&top_dir) {
            raw_name[top_dir.len()..]
                .trim_start_matches('/')
                .to_string()
        } else {
            raw_name.clone()
        };
        if relative.is_empty() {
            continue;
        }

        let out = safe_join(&java_dir, &relative)?;
        if entry.is_dir() {
            std::fs::create_dir_all(&out).ok();
        } else {
            if let Some(p) = out.parent() {
                std::fs::create_dir_all(p).ok();
            }
            let mut f = std::fs::File::create(&out).map_err(|e| e.to_string())?;
            std::io::copy(&mut entry, &mut f).map_err(|e| e.to_string())?;
        }
    }
    let _ = std::fs::remove_file(&zip_path);

    fn find_exe(dir: &std::path::Path) -> Option<std::path::PathBuf> {
        for e in std::fs::read_dir(dir).ok()?.flatten() {
            let p = e.path();
            if p.is_dir() {
                if let Some(r) = find_exe(&p) {
                    return Some(r);
                }
            } else if p.file_name().map(|n| n == "java.exe").unwrap_or(false) {
                return Some(p);
            }
        }
        None
    }
    find_exe(&java_dir)
        .map(|p| p.to_string_lossy().to_string())
        .ok_or_else(|| "解压后未找到 java.exe".to_string())
}
