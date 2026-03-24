use serde::Serialize;
use crate::instance::{resolve_game_dir, CF_API_KEY};
use crate::modcn::{load_modcn, contains_chinese};

#[derive(Serialize, Clone)]
pub struct ModInfo {
    pub file_name: String,
    pub cn_name: String,
    pub enabled: bool,
    pub size_kb: u64,
}

/// 列出实例的所有 mod（.jar 和 .jar.disabled）
#[tauri::command]
pub fn list_mods(game_dir: String, name: String) -> Result<Vec<ModInfo>, String> {
    let dir = resolve_game_dir(&game_dir);
    let mods_dir = dir.join("instances").join(&name).join("mods");
    if !mods_dir.exists() {
        return Ok(vec![]);
    }

    // 加载 modcn 数据用于匹配中文名
    let modcn = load_modcn();

    let mut mods = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&mods_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() { continue; }
            let fname = path.file_name().unwrap_or_default().to_string_lossy().to_string();
            let lower = fname.to_lowercase();
            if lower.ends_with(".jar") || lower.ends_with(".jar.disabled") {
                let enabled = !lower.ends_with(".disabled");
                let size_kb = entry.metadata().map(|m| m.len() / 1024).unwrap_or(0);

                // 从文件名提取模组名用于匹配中文
                let base = lower.trim_end_matches(".disabled").trim_end_matches(".jar");
                // 去掉版本号和mc版本部分
                let mod_key = base.split(|c: char| c.is_ascii_digit()).next().unwrap_or(base)
                    .trim_end_matches('-').trim_end_matches('_').trim_end_matches('.');
                // 也按 - 分割取第一段作为核心名
                let first_seg = base.split('-').next().unwrap_or(base)
                    .split('_').next().unwrap_or(base);

                let cn_name = modcn.iter().find_map(|e| {
                    if e.cn_name.is_empty() || !contains_chinese(&e.cn_name) { return None; }
                    let en_lower = e.en_name.to_lowercase();
                    if en_lower.is_empty() { return None; }
                    let en_slug = en_lower.replace(' ', "-").replace('_', "-");
                    let abbr_lower = e.abbr.to_lowercase();
                    // 1. slug完全匹配核心名
                    if mod_key.len() >= 2 && (en_slug == mod_key || en_slug == first_seg) {
                        return Some(e.cn_name.clone());
                    }
                    // 2. 文件名包含英文名 或 英文名包含核心名
                    if mod_key.len() >= 3 && (base.contains(&en_slug) || en_slug.contains(mod_key)) {
                        return Some(e.cn_name.clone());
                    }
                    // 3. 缩写匹配
                    if !abbr_lower.is_empty() && abbr_lower.len() >= 2 && (mod_key == abbr_lower || first_seg == abbr_lower) {
                        return Some(e.cn_name.clone());
                    }
                    None
                }).unwrap_or_default();

                mods.push(ModInfo { file_name: fname, cn_name, enabled, size_kb });
            }
        }
    }
    mods.sort_by(|a, b| a.file_name.to_lowercase().cmp(&b.file_name.to_lowercase()));
    Ok(mods)
}

/// 切换 mod 启用/禁用（.jar ↔ .jar.disabled）
#[tauri::command]
pub fn toggle_mod(game_dir: String, name: String, file_name: String) -> Result<bool, String> {
    let dir = resolve_game_dir(&game_dir);
    let mods_dir = dir.join("instances").join(&name).join("mods");
    let src = mods_dir.join(&file_name);
    if !src.exists() {
        return Err(format!("文件不存在: {}", file_name));
    }
    let lower = file_name.to_lowercase();
    let (dst_name, new_enabled) = if lower.ends_with(".jar.disabled") {
        // 启用：去掉 .disabled
        (file_name.trim_end_matches(".disabled").to_string(), true)
    } else if lower.ends_with(".jar") {
        // 禁用：加 .disabled
        (format!("{}.disabled", file_name), false)
    } else {
        return Err("不支持的文件类型".to_string());
    };
    let dst = mods_dir.join(&dst_name);
    std::fs::rename(&src, &dst).map_err(|e| format!("重命名失败: {}", e))?;
    Ok(new_enabled)
}

/// 删除指定 mod 文件
#[tauri::command]
pub fn delete_mod(game_dir: String, name: String, file_name: String) -> Result<bool, String> {
    let dir = resolve_game_dir(&game_dir);
    let mods_dir = dir.join("instances").join(&name).join("mods");
    let target = mods_dir.join(&file_name);
    if !target.exists() {
        return Err(format!("文件不存在: {}", file_name));
    }
    std::fs::remove_file(&target).map_err(|e| format!("删除失败: {}", e))?;
    Ok(true)
}

#[derive(Serialize, Clone)]
pub struct ModUrlInfo {
    pub file_name: String,
    pub mr_url: String,
    pub cf_url: String,
}

/// 查询已安装 mod 的真实链接（Modrinth / CurseForge / MC百科）
#[tauri::command]
pub async fn lookup_mod_urls(file_names: Vec<String>) -> Result<Vec<ModUrlInfo>, String> {
    let result = tokio::task::spawn_blocking(move || {
        let http = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .user_agent("Mozilla/5.0 oaoi-launcher/1.0")
            .build()
            .map_err(|e| e.to_string())?;

        let results: Vec<ModUrlInfo> = file_names.iter().map(|fname| {
            let base = fname.to_lowercase()
                .trim_end_matches(".disabled").to_string();
            let base = base.trim_end_matches(".jar");
            // 提取 slug：去版本号和 loader 后缀
            let slug: String = base
                .split(|c: char| c.is_ascii_digit()).next().unwrap_or(base)
                .trim_end_matches('-').trim_end_matches('_')
                .to_string();

            let mut mr_url = String::new();
            let mut cf_url = String::new();

            // Modrinth: 直接用 slug 查项目
            if !slug.is_empty() {
                let url = format!("https://api.modrinth.com/v2/project/{}", slug);
                if let Ok(resp) = http.get(&url).send() {
                    if resp.status().is_success() {
                        if let Ok(json) = resp.json::<serde_json::Value>() {
                            if let Some(s) = json["slug"].as_str() {
                                mr_url = format!("https://modrinth.com/mod/{}", s);
                            }
                        }
                    }
                }
            }

            // CurseForge: 搜索 API
            if !slug.is_empty() {
                let search_name = slug.replace('-', " ");
                let url = format!(
                    "https://api.curseforge.com/v1/mods/search?gameId=432&classId=6&searchFilter={}&pageSize=1",
                    urlencoding::encode(&search_name)
                );
                if let Ok(resp) = http.get(&url).header("x-api-key", CF_API_KEY).send() {
                    if let Ok(json) = resp.json::<serde_json::Value>() {
                        if let Some(data) = json["data"].as_array() {
                            if let Some(first) = data.first() {
                                if let Some(link) = first["links"]["websiteUrl"].as_str() {
                                    cf_url = link.to_string();
                                }
                            }
                        }
                    }
                }
            }


            ModUrlInfo {
                file_name: fname.clone(),
                mr_url,
                cf_url,
            }
        }).collect();

        Ok(results)
    }).await.map_err(|e| format!("任务失败: {}", e))?;
    result
}
