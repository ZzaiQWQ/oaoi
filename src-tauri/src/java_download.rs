use tauri::Emitter;

/// 根据 Java 大版本号自动下载对应 JRE（Adoptium）
/// 安装到 {game_dir}/runtime/jre-{major}/
#[tauri::command]
pub fn download_java(app_handle: tauri::AppHandle, major: u32, game_dir: String) -> Result<String, String> {
    // 快速检查：已存在直接返回
    let java_exe = std::path::PathBuf::from(&game_dir).join("runtime").join(format!("jre-{}", major)).join("bin").join("java.exe");
    if java_exe.exists() {
        eprintln!("[java] Java {} 已存在: {}", major, java_exe.display());
        return Ok(java_exe.to_string_lossy().to_string());
    }

    // 后台线程下载，不阻塞 UI
    std::thread::spawn(move || {
        match do_download_java(&app_handle, major, &game_dir) {
            Ok(path) => {
                let _ = app_handle.emit("java-download-done", serde_json::json!({
                    "major": major, "success": true, "path": path
                }));
            }
            Err(e) => {
                eprintln!("[java] 下载失败: {}", e);
                let _ = app_handle.emit("java-download-done", serde_json::json!({
                    "major": major, "success": false, "error": e
                }));
            }
        }
    });
    // 立即返回，前端通过事件监听结果
    Ok("downloading".to_string())
}

fn do_download_java(app_handle: &tauri::AppHandle, major: u32, game_dir: &str) -> Result<String, String> {
    let java_base = std::path::PathBuf::from(game_dir).join("runtime");
    let java_dir = java_base.join(format!("jre-{}", major));
    let java_exe = java_dir.join("bin").join("java.exe");

    // 已存在，直接返回
    if java_exe.exists() {
        eprintln!("[java] Java {} 已存在: {}", major, java_exe.display());
        return Ok(java_exe.to_string_lossy().to_string());
    }

    eprintln!("[java] 开始下载 Java {} ...", major);
    let _ = app_handle.emit("java-download-progress", serde_json::json!({
        "major": major, "stage": "downloading", "detail": format!("正在下载 Java {} ...", major)
    }));

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
        .build().map_err(|e| format!("HTTP 客户端创建失败: {}", e))?;

    // 尝试官方源，失败后回退到清华镜像
    let bytes = {
        eprintln!("[java] 尝试官方源: {}", official_url);
        match http.get(&official_url).send() {
            Ok(resp) if resp.status().is_success() => {
                resp.bytes().map_err(|e| format!("读取下载数据失败: {}", e))?
            }
            _ => {
                eprintln!("[java] 官方源失败，尝试清华镜像...");
                let _ = app_handle.emit("java-download-progress", serde_json::json!({
                    "major": major, "stage": "downloading", "detail": format!("官方源失败，正在从镜像下载 Java {} ...", major)
                }));
                let listing = http.get(&mirror_url_base).send()
                    .map_err(|e| format!("镜像源请求失败: {}", e))?
                    .text().map_err(|e| e.to_string())?;
                let zip_name = listing.lines()
                    .filter_map(|line| {
                        if let Some(start) = line.find("href=\"") {
                            let rest = &line[start + 6..];
                            if let Some(end) = rest.find('"') {
                                let name = &rest[..end];
                                if name.ends_with(".zip") && name.contains("jre") && name.contains("x64") && name.contains("windows") {
                                    return Some(name.to_string());
                                }
                            }
                        }
                        None
                    })
                    .last()
                    .ok_or_else(|| format!("清华镜像中找不到 Java {} JRE zip", major))?;

                let download_url = format!("{}{}", mirror_url_base, zip_name);
                eprintln!("[java] 镜像下载: {}", download_url);
                let resp = http.get(&download_url).send()
                    .map_err(|e| format!("镜像下载失败: {}", e))?;
                if !resp.status().is_success() {
                    return Err(format!("镜像下载 Java {} 失败: HTTP {}", major, resp.status()));
                }
                resp.bytes().map_err(|e| format!("读取下载数据失败: {}", e))?
            }
        }
    };

    // 保存到临时文件
    std::fs::create_dir_all(&java_base).map_err(|e| e.to_string())?;
    let tmp_zip = java_base.join(format!("java{}.zip", major));
    std::fs::write(&tmp_zip, &bytes).map_err(|e| format!("保存 zip 失败: {}", e))?;
    eprintln!("[java] 下载完成 ({:.1} MB)，解压中...", bytes.len() as f64 / 1048576.0);

    let _ = app_handle.emit("java-download-progress", serde_json::json!({
        "major": major, "stage": "extracting", "detail": "正在解压..."
    }));

    // 解压 zip
    let zip_file = std::fs::File::open(&tmp_zip).map_err(|e| e.to_string())?;
    let mut archive = zip::ZipArchive::new(zip_file).map_err(|e| format!("打开 zip 失败: {}", e))?;

    let top_dir = archive.by_index(0)
        .map_err(|e| e.to_string())?
        .name().split('/').next().unwrap_or("").to_string();

    for i in 0..archive.len() {
        let mut file = archive.by_index(i).map_err(|e| e.to_string())?;
        let raw_name = file.name().to_string();
        let relative = if !top_dir.is_empty() && raw_name.starts_with(&top_dir) {
            raw_name[top_dir.len()..].trim_start_matches('/').to_string()
        } else {
            raw_name.clone()
        };
        if relative.is_empty() { continue; }

        let out_path = java_dir.join(&relative);
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
        let _ = app_handle.emit("java-download-progress", serde_json::json!({
            "major": major, "stage": "done", "detail": format!("Java {} 安装完成", major)
        }));
        Ok(java_exe.to_string_lossy().to_string())
    } else {
        Err(format!("解压后找不到 java.exe: {}", java_exe.display()))
    }
}
