use crate::instance::{cf_api_key, resolve_game_dir, safe_path_name};
use crate::modcn::{contains_chinese, load_modcn};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::OnceLock;

#[derive(Serialize, Clone)]
pub struct ModInfo {
    pub file_name: String,
    pub cn_name: String,
    pub enabled: bool,
    pub size_kb: u64,
}

/// 列出实例的所有 mod（.jar 和 .jar.disabled）
#[tauri::command]
pub async fn list_mods(game_dir: String, name: String) -> Result<Vec<ModInfo>, String> {
    tokio::task::spawn_blocking(move || list_mods_blocking(&game_dir, &name))
        .await
        .map_err(|e| format!("任务失败: {}", e))?
}

fn list_mods_blocking(game_dir: &str, name: &str) -> Result<Vec<ModInfo>, String> {
    let dir = resolve_game_dir(&game_dir);
    let safe_name = safe_path_name(&name, "版本名")?;
    let mods_dir = dir.join("instances").join(&safe_name).join("mods");
    if !mods_dir.exists() {
        return Ok(vec![]);
    }

    // 加载 modcn 索引用于匹配中文名
    let modcn_index = modcn_index();

    let mut mods = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&mods_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let fname = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let lower = fname.to_lowercase();
            if lower.ends_with(".jar") || lower.ends_with(".jar.disabled") {
                let enabled = !lower.ends_with(".disabled");
                let size_kb = entry.metadata().map(|m| m.len() / 1024).unwrap_or(0);

                // 从文件名提取模组名用于匹配中文
                let base = lower.trim_end_matches(".disabled").trim_end_matches(".jar");
                // 去掉版本号和mc版本部分
                let mod_key = base
                    .split(|c: char| c.is_ascii_digit())
                    .next()
                    .unwrap_or(base)
                    .trim_end_matches('-')
                    .trim_end_matches('_')
                    .trim_end_matches('.');
                // 也按 - 分割取第一段作为核心名
                let first_seg = base
                    .split('-')
                    .next()
                    .unwrap_or(base)
                    .split('_')
                    .next()
                    .unwrap_or(base);

                let cn_name = find_cn_name(&modcn_index, mod_key, first_seg, base);

                mods.push(ModInfo {
                    file_name: fname,
                    cn_name,
                    enabled,
                    size_kb,
                });
            }
        }
    }
    mods.sort_by(|a, b| a.file_name.to_lowercase().cmp(&b.file_name.to_lowercase()));
    Ok(mods)
}

struct ModCnIndex {
    exact: HashMap<String, String>,
    slugs: Vec<(String, String)>,
}

fn modcn_index() -> &'static ModCnIndex {
    static MODCN_INDEX: OnceLock<ModCnIndex> = OnceLock::new();
    MODCN_INDEX.get_or_init(|| build_modcn_index(load_modcn()))
}

fn build_modcn_index(entries: &[crate::modcn::ModCnEntry]) -> ModCnIndex {
    let mut exact = HashMap::new();
    let mut slugs = Vec::new();
    for entry in entries {
        if entry.cn_name.is_empty() || !contains_chinese(&entry.cn_name) {
            continue;
        }
        let en_lower = entry.en_name.to_lowercase();
        if !en_lower.is_empty() {
            let en_slug = en_lower.replace(' ', "-").replace('_', "-");
            if en_slug.len() >= 2 {
                exact
                    .entry(en_slug.clone())
                    .or_insert_with(|| entry.cn_name.clone());
                slugs.push((en_slug, entry.cn_name.clone()));
            }
        }
        let abbr_lower = entry.abbr.to_lowercase();
        if abbr_lower.len() >= 2 {
            exact
                .entry(abbr_lower)
                .or_insert_with(|| entry.cn_name.clone());
        }
    }
    ModCnIndex { exact, slugs }
}

fn find_cn_name(index: &ModCnIndex, mod_key: &str, first_seg: &str, base: &str) -> String {
    if mod_key.len() >= 2 {
        if let Some(name) = index.exact.get(mod_key) {
            return name.clone();
        }
    }
    if first_seg.len() >= 2 {
        if let Some(name) = index.exact.get(first_seg) {
            return name.clone();
        }
    }
    if mod_key.len() >= 3 {
        if let Some((_, name)) = index
            .slugs
            .iter()
            .find(|(slug, _)| base.contains(slug) || slug.contains(mod_key))
        {
            return name.clone();
        }
    }
    String::new()
}

/// 切换 mod 启用/禁用（.jar ↔ .jar.disabled）
#[tauri::command]
pub fn toggle_mod(game_dir: String, name: String, file_name: String) -> Result<bool, String> {
    let dir = resolve_game_dir(&game_dir);
    let safe_name = safe_path_name(&name, "版本名")?;
    let safe_file_name = safe_path_name(&file_name, "文件名")?;
    let mods_dir = dir.join("instances").join(&safe_name).join("mods");
    let src = mods_dir.join(&safe_file_name);
    if !src.exists() {
        return Err(format!("文件不存在: {}", file_name));
    }
    let lower = safe_file_name.to_lowercase();
    let (dst_name, new_enabled) = if lower.ends_with(".jar.disabled") {
        // 启用：去掉 .disabled
        (
            safe_file_name.trim_end_matches(".disabled").to_string(),
            true,
        )
    } else if lower.ends_with(".jar") {
        // 禁用：加 .disabled
        (format!("{}.disabled", safe_file_name), false)
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
    let safe_name = safe_path_name(&name, "版本名")?;
    let safe_file_name = safe_path_name(&file_name, "文件名")?;
    let mods_dir = dir.join("instances").join(&safe_name).join("mods");
    let target = mods_dir.join(&safe_file_name);
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
                if let Ok(resp) = http.get(&url).header("x-api-key", &cf_api_key()).send() {
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
