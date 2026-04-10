use serde::Serialize;
use crate::instance::cf_api_key;
use crate::modcn::{load_modcn, contains_chinese, search_modcn_fuzzy};

#[derive(Serialize, Clone)]
pub struct OnlineModResult {
    pub slug: String,
    pub title: String,
    pub cn_title: String,
    pub description: String,
    pub author: String,
    pub downloads: u64,
    pub icon_url: String,
    pub project_id: String,
    pub mr_url: String,
    pub cf_url: String,
}

/// 搜索在线 Mod/材质包/光影包（Modrinth + CurseForge + MCIM 中文翻译）
#[tauri::command]
pub async fn search_online_mods(
    query: String,
    mc_version: String,
    loader: String,
    project_type: Option<String>,
) -> Result<Vec<OnlineModResult>, String> {
    let ptype = project_type.unwrap_or_else(|| "mod".to_string());
    let result: Result<Vec<OnlineModResult>, String> = tokio::task::spawn_blocking(move || {
        search_online_mods_blocking(&query, &mc_version, &loader, &ptype)
    }).await.map_err(|e| format!("任务失败: {}", e))?;
    result
}

fn search_online_mods_blocking(
    query: &str,
    mc_version: &str,
    loader: &str,
    project_type: &str,
) -> Result<Vec<OnlineModResult>, String> {
    let http = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36")
        .build()
        .map_err(|e| e.to_string())?;

    // 加载 modcn 数据
    let modcn = load_modcn();

    // 中文查询 → 模糊匹配 → 拿到英文名去搜
    let fuzzy_matches = if contains_chinese(query) {
        search_modcn_fuzzy(query, modcn)
    } else { vec![] };

    // 提取英文搜索词（去重，最多5个）
    let en_queries: Vec<String> = fuzzy_matches.iter()
        .map(|(en, _, _)| en.clone())
        .take(5)
        .collect();

    eprintln!("[search] 原始查询: '{}', 英文搜索词: {:?}", query, en_queries);

    let mut all: Vec<OnlineModResult> = Vec::new();

    // 全部并发搜索
    std::thread::scope(|s| {
        // 1. 用原始查询搜 Modrinth + CurseForge
        let h_mr = s.spawn(|| {
            do_modrinth_search(&http, query, mc_version, loader, project_type)
                .or_else(|_| do_modrinth_search(&http, query, mc_version, "", project_type))
                .unwrap_or_default()
        });
        let h_cf = s.spawn(|| {
            do_curseforge_search(&http, query, mc_version, loader, project_type)
                .unwrap_or_default()
        });

        // 2. 用英文名搜 Modrinth + CurseForge
        let en_handles: Vec<_> = en_queries.iter().map(|en| {
            s.spawn(|| {
                let mr = do_modrinth_search(&http, en, mc_version, loader, project_type)
                    .or_else(|_| do_modrinth_search(&http, en, mc_version, "", project_type))
                    .unwrap_or_default();
                let cf = do_curseforge_search(&http, en, mc_version, loader, project_type)
                    .unwrap_or_default();
                (mr, cf)
            })
        }).collect();

        // === 合并去重 ===

        // 英文搜索结果优先（从词典匹配的，精确度高）
        for h in en_handles {
            if let Ok((mr_res, cf_res)) = h.join() {
                for r in mr_res {
                    let slug_lower = r.slug.to_lowercase();
                    if !all.iter().any(|e| e.slug.to_lowercase() == slug_lower) {
                        all.push(r);
                    }
                }
                for cf in cf_res {
                    let cf_slug = cf.slug.to_lowercase();
                    if let Some(existing) = all.iter_mut().find(|r| r.slug.to_lowercase() == cf_slug) {
                        if existing.cf_url.is_empty() { existing.cf_url = cf.cf_url; }
                    } else {
                        all.push(cf);
                    }
                }
            }
        }
        // 原始查询结果
        for r in h_mr.join().unwrap_or_default() {
            let slug_lower = r.slug.to_lowercase();
            if !all.iter().any(|e| e.slug.to_lowercase() == slug_lower) {
                all.push(r);
            }
        }
        for cf in h_cf.join().unwrap_or_default() {
            let cf_slug = cf.slug.to_lowercase();
            if let Some(existing) = all.iter_mut().find(|r| r.slug.to_lowercase() == cf_slug) {
                if existing.cf_url.is_empty() { existing.cf_url = cf.cf_url; }
            } else {
                all.push(cf);
            }
        }
    });

    // 用 modcn 数据填充中文名
    // 构建快速查找表（英文名小写 → 中文名）
    let mut en_to_cn: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for entry in modcn {
        if !entry.cn_name.is_empty() && contains_chinese(&entry.cn_name) && !entry.en_name.is_empty() {
            en_to_cn.entry(entry.en_name.to_lowercase()).or_insert_with(|| entry.cn_name.clone());
        }
    }
    for r in all.iter_mut() {
        if !r.cn_title.is_empty() { continue; }
        let title_lower = r.title.to_lowercase();
        let slug_lower = r.slug.to_lowercase();
        // 1. 英文名精确匹配
        if let Some(cn) = en_to_cn.get(&title_lower) {
            r.cn_title = cn.clone();
            continue;
        }
        // 2. slug 匹配（slug 转空格后匹配）
        let slug_spaced = slug_lower.replace('-', " ").replace('_', " ");
        if let Some(cn) = en_to_cn.get(&slug_spaced) {
            r.cn_title = cn.clone();
            continue;
        }
        // 3. 遍历 modcn 做包含匹配
        for entry in modcn {
            if entry.cn_name.is_empty() || !contains_chinese(&entry.cn_name) { continue; }
            let en_lower = entry.en_name.to_lowercase();
            if en_lower.is_empty() { continue; }
            if title_lower.contains(&en_lower) || en_lower.contains(&title_lower)
                || slug_spaced == en_lower
            {
                r.cn_title = entry.cn_name.clone();
                break;
            }
        }
    }

    // 材质包/光影包按下载量排序（热门优先）
    if project_type != "mod" || query.is_empty() {
        all.sort_by(|a, b| b.downloads.cmp(&a.downloads));
    }
    eprintln!("[search] 返回 {} 个结果 (type={})", all.len(), project_type);
    Ok(all)
}

/// 直接按 slug 精确查询 Modrinth 项目（不走搜索API）
#[allow(dead_code)]
fn do_modrinth_direct_lookup(
    http: &reqwest::blocking::Client,
    slug: &str,
) -> Option<OnlineModResult> {
    let url = format!("https://api.modrinth.com/v2/project/{}", urlencoding::encode(slug));
    let resp = http.get(&url)
        .timeout(std::time::Duration::from_secs(5))
        .send().ok()?;
    if !resp.status().is_success() { return None; }
    let json: serde_json::Value = resp.json().ok()?;
    let project_type = json["project_type"].as_str().unwrap_or("");
    if project_type != "mod" { return None; }
    let slug_val = json["slug"].as_str().unwrap_or("").to_string();
    let id = json["id"].as_str().unwrap_or("").to_string();
    eprintln!("[modrinth_direct] 精确命中: {} ({})", slug_val, id);
    Some(OnlineModResult {
        slug: slug_val.clone(),
        title: json["title"].as_str().unwrap_or("").to_string(),
        description: json["description"].as_str().unwrap_or("").to_string(),
        author: String::new(),
        downloads: json["downloads"].as_u64().unwrap_or(0),
        icon_url: json["icon_url"].as_str().unwrap_or("").to_string(),
        project_id: id,
        mr_url: format!("https://modrinth.com/mod/{}", slug_val),
        cf_url: String::new(),
        cn_title: String::new(),
    })
}

fn do_modrinth_search(
    http: &reqwest::blocking::Client,
    query: &str,
    mc_version: &str,
    loader: &str,
    project_type: &str,
) -> Result<Vec<OnlineModResult>, String> {
    let mr_type = match project_type {
        "resourcepack" => "resourcepack",
        "shader" => "shader",
        _ => "mod",
    };
    let mut facets = vec![format!(r#"["project_type:{}"]"#, mr_type)];
    if !mc_version.is_empty() {
        facets.push(format!(r#"["versions:{}"]"#, mc_version));
    }
    if !loader.is_empty() && loader != "vanilla" && project_type == "mod" {
        facets.push(format!(r#"["categories:{}"]"#, loader));
    }
    let facets_str = format!("[{}]", facets.join(","));
    let sort_index = if query.is_empty() { "downloads" } else { "relevance" };

    let url = format!(
        "https://api.modrinth.com/v2/search?query={}&facets={}&limit=40&index={}",
        urlencoding::encode(query),
        urlencoding::encode(&facets_str),
        sort_index,
    );

    let resp = http.get(&url).send().map_err(|e| format!("搜索请求失败: {}", e))?;
    let json: serde_json::Value = resp.json().map_err(|e| format!("解析响应失败: {}", e))?;

    let mut results = Vec::new();
    if let Some(hits) = json["hits"].as_array() {
        for hit in hits {
            let slug = hit["slug"].as_str().unwrap_or("").to_string();
            let mr_path = match project_type {
                "resourcepack" => "resourcepack",
                "shader" => "shader",
                _ => "mod",
            };
            results.push(OnlineModResult {
                mr_url: format!("https://modrinth.com/{}/{}", mr_path, slug),
                cf_url: String::new(),
                cn_title: String::new(),
                slug,
                title: hit["title"].as_str().unwrap_or("").to_string(),
                description: hit["description"].as_str().unwrap_or("").to_string(),
                author: hit["author"].as_str().unwrap_or("").to_string(),
                downloads: hit["downloads"].as_u64().unwrap_or(0),
                icon_url: hit["icon_url"].as_str().unwrap_or("").to_string(),
                project_id: hit["project_id"].as_str().unwrap_or("").to_string(),
            });
        }
    }
    Ok(results)
}

fn do_curseforge_search(
    http: &reqwest::blocking::Client,
    query: &str,
    mc_version: &str,
    loader: &str,
    project_type: &str,
) -> Result<Vec<OnlineModResult>, String> {
    let loader_type = match loader {
        "forge" => "1",
        "fabric" => "4",
        "quilt" => "5",
        "neoforge" => "6",
        _ => "0",
    };
    let class_id = match project_type {
        "resourcepack" => "12",
        "shader" => "6552",
        _ => "6",
    };
    // sortField: 2=popularity, 6=totalDownloads
    let sort_field = if query.is_empty() { "6" } else { "1" };

    let mut url = format!(
        "https://api.curseforge.com/v1/mods/search?gameId=432&classId={}&searchFilter={}&pageSize=50&sortField={}&sortOrder=desc",
        class_id,
        urlencoding::encode(query),
        sort_field,
    );
    if !mc_version.is_empty() && project_type == "mod" {
        url.push_str(&format!("&gameVersion={}", urlencoding::encode(mc_version)));
    }
    if loader_type != "0" && project_type == "mod" {
        url.push_str(&format!("&modLoaderType={}", loader_type));
    }

    let resp = http.get(&url)
        .header("x-api-key", &cf_api_key())
        .header("Accept", "application/json")
        .send()
        .map_err(|e| format!("CurseForge 请求失败: {}", e))?;

    let json: serde_json::Value = resp.json().map_err(|e| format!("CurseForge 解析失败: {}", e))?;

    let mut results = Vec::new();
    if let Some(data) = json["data"].as_array() {
        for item in data {
            let authors = item["authors"].as_array()
                .and_then(|a| a.first())
                .and_then(|a| a["name"].as_str())
                .unwrap_or("");
            let logo = item["logo"]["url"].as_str().unwrap_or("");
            let id = item["id"].as_u64().unwrap_or(0);
            let cf_url = item["links"]["websiteUrl"].as_str()
                .unwrap_or("").to_string();
            results.push(OnlineModResult {
                slug: item["slug"].as_str().unwrap_or("").to_string(),
                title: item["name"].as_str().unwrap_or("").to_string(),
                cn_title: String::new(),
                description: item["summary"].as_str().unwrap_or("").to_string(),
                author: authors.to_string(),
                downloads: item["downloadCount"].as_u64().unwrap_or(0),
                icon_url: logo.to_string(),
                project_id: format!("cf_{}", id),
                mr_url: String::new(),
                cf_url,
            });
        }
    }
    Ok(results)
}
