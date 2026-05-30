use crate::instance::cf_api_key;
use crate::modcn::{contains_chinese, load_modcn, search_modcn_fuzzy};
use serde::Serialize;

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

const MAX_CN_SEARCH_TERMS: usize = 12;
const MAX_PARALLEL_SEARCH_TERMS: usize = 3;
const MAX_ONLINE_RESULTS: usize = 120;
const RELAX_RESULT_TARGET: usize = 24;

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
    })
    .await
    .map_err(|e| format!("任务失败: {}", e))?;
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

    let query = query.trim();

    // 加载 modcn 数据
    let modcn = load_modcn();

    // 中文查询 -> 模糊匹配 -> 拿到英文名去搜
    let fuzzy_matches = if contains_chinese(query) {
        search_modcn_fuzzy(query, modcn)
    } else {
        vec![]
    };
    let search_terms = build_search_terms(query, &fuzzy_matches);

    eprintln!(
        "[search] 原始查询: '{}', 实际搜索词: {:?}",
        query, search_terms
    );

    let mut all: Vec<OnlineModResult> = Vec::new();
    let mut batches: Vec<(usize, Vec<OnlineModResult>, Vec<OnlineModResult>)> = Vec::new();

    // 分批并发搜索，避免中文模糊词一次性把 Modrinth/CurseForge 打爆。
    for (chunk_idx, chunk) in search_terms.chunks(MAX_PARALLEL_SEARCH_TERMS).enumerate() {
        let base_idx = chunk_idx * MAX_PARALLEL_SEARCH_TERMS;
        std::thread::scope(|s| {
            let handles: Vec<_> = chunk
                .iter()
                .enumerate()
                .map(|(offset, term)| {
                    let http = http.clone();
                    s.spawn(move || {
                        let mr = search_modrinth_with_fallbacks(
                            &http,
                            term,
                            mc_version,
                            loader,
                            project_type,
                        );
                        let cf = search_curseforge_with_fallbacks(
                            &http,
                            term,
                            mc_version,
                            loader,
                            project_type,
                        );
                        (base_idx + offset, mr, cf)
                    })
                })
                .collect();

            for h in handles {
                if let Ok(batch) = h.join() {
                    batches.push(batch);
                }
            }
        });
    }

    // 搜索词按优先级合并：原始词优先，中文词库命中的英文名继续补全。
    batches.sort_by_key(|(idx, _, _)| *idx);
    for (_, mr_res, cf_res) in batches {
        for r in mr_res {
            merge_result(&mut all, r);
        }
        for r in cf_res {
            merge_result(&mut all, r);
        }
    }

    // 用 modcn 数据填充中文名
    // 构建快速查找表（英文名小写 → 中文名）
    let mut en_to_cn: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for entry in modcn {
        if !entry.cn_name.is_empty()
            && contains_chinese(&entry.cn_name)
            && !entry.en_name.is_empty()
        {
            en_to_cn
                .entry(entry.en_name.to_lowercase())
                .or_insert_with(|| entry.cn_name.clone());
        }
    }
    for r in all.iter_mut() {
        if !r.cn_title.is_empty() {
            continue;
        }
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
            if entry.cn_name.is_empty() || !contains_chinese(&entry.cn_name) {
                continue;
            }
            let en_lower = entry.en_name.to_lowercase();
            if en_lower.is_empty() {
                continue;
            }
            if title_lower.contains(&en_lower)
                || en_lower.contains(&title_lower)
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
    if all.len() > MAX_ONLINE_RESULTS {
        all.truncate(MAX_ONLINE_RESULTS);
    }
    eprintln!("[search] 返回 {} 个结果 (type={})", all.len(), project_type);
    Ok(all)
}

fn build_search_terms(query: &str, fuzzy_matches: &[(String, String, i32)]) -> Vec<String> {
    let mut terms = Vec::new();
    push_search_term(&mut terms, query);
    for (en, _, _) in fuzzy_matches.iter().take(MAX_CN_SEARCH_TERMS) {
        push_search_term(&mut terms, en);
    }
    if terms.is_empty() {
        terms.push(String::new());
    }
    terms
}

fn push_search_term(terms: &mut Vec<String>, term: &str) {
    let trimmed = term.trim();
    if trimmed.is_empty() {
        return;
    }
    let normalized = trimmed.to_lowercase();
    if !terms.iter().any(|t| t.to_lowercase() == normalized) {
        terms.push(trimmed.to_string());
    }
}

fn should_relax_results(count: usize, query: &str) -> bool {
    count < RELAX_RESULT_TARGET || (!query.is_empty() && count < 40)
}

fn merge_result(all: &mut Vec<OnlineModResult>, incoming: OnlineModResult) {
    if incoming.slug.is_empty() && incoming.title.is_empty() {
        return;
    }

    if let Some(existing) = all.iter_mut().find(|r| is_same_result(r, &incoming)) {
        merge_result_fields(existing, incoming);
    } else {
        all.push(incoming);
    }
}

fn is_same_result(a: &OnlineModResult, b: &OnlineModResult) -> bool {
    let a_slug = a.slug.to_lowercase();
    let b_slug = b.slug.to_lowercase();
    if !a_slug.is_empty() && a_slug == b_slug {
        return true;
    }
    if !a.project_id.is_empty() && a.project_id == b.project_id {
        return true;
    }
    if !a.mr_url.is_empty() && a.mr_url == b.mr_url {
        return true;
    }
    if !a.cf_url.is_empty() && a.cf_url == b.cf_url {
        return true;
    }
    false
}

fn merge_result_fields(existing: &mut OnlineModResult, incoming: OnlineModResult) {
    if existing.cn_title.is_empty() {
        existing.cn_title = incoming.cn_title;
    }
    if existing.description.is_empty() {
        existing.description = incoming.description;
    }
    if existing.author.is_empty() {
        existing.author = incoming.author;
    }
    if existing.icon_url.is_empty() {
        existing.icon_url = incoming.icon_url;
    }
    if existing.mr_url.is_empty() {
        existing.mr_url = incoming.mr_url;
    }
    if existing.cf_url.is_empty() {
        existing.cf_url = incoming.cf_url;
    }
    if existing.downloads < incoming.downloads {
        existing.downloads = incoming.downloads;
    }

    let existing_is_cf = existing.project_id.starts_with("cf_");
    let incoming_is_mr = !incoming.project_id.is_empty() && !incoming.project_id.starts_with("cf_");
    if existing.project_id.is_empty() || (existing_is_cf && incoming_is_mr) {
        existing.project_id = incoming.project_id;
    }
}

fn search_modrinth_with_fallbacks(
    http: &reqwest::blocking::Client,
    query: &str,
    mc_version: &str,
    loader: &str,
    project_type: &str,
) -> Vec<OnlineModResult> {
    let mut results = Vec::new();
    append_results(
        &mut results,
        run_modrinth_search(http, query, mc_version, loader, project_type),
    );

    let can_relax_loader = project_type == "mod" && !loader.is_empty() && loader != "vanilla";
    if can_relax_loader && should_relax_results(results.len(), query) {
        append_results(
            &mut results,
            run_modrinth_search(http, query, mc_version, "", project_type),
        );
    }

    if project_type == "mod" && !mc_version.is_empty() && should_relax_results(results.len(), query)
    {
        append_results(
            &mut results,
            run_modrinth_search(http, query, "", "", project_type),
        );
    }

    results
}

fn search_curseforge_with_fallbacks(
    http: &reqwest::blocking::Client,
    query: &str,
    mc_version: &str,
    loader: &str,
    project_type: &str,
) -> Vec<OnlineModResult> {
    let mut results = Vec::new();
    append_results(
        &mut results,
        run_curseforge_search(http, query, mc_version, loader, project_type),
    );

    let can_relax_loader = project_type == "mod" && !loader.is_empty() && loader != "vanilla";
    if can_relax_loader && should_relax_results(results.len(), query) {
        append_results(
            &mut results,
            run_curseforge_search(http, query, mc_version, "", project_type),
        );
    }

    if project_type == "mod" && !mc_version.is_empty() && should_relax_results(results.len(), query)
    {
        append_results(
            &mut results,
            run_curseforge_search(http, query, "", "", project_type),
        );
    }

    results
}

fn append_results(all: &mut Vec<OnlineModResult>, incoming: Vec<OnlineModResult>) {
    for item in incoming {
        merge_result(all, item);
    }
}

fn run_modrinth_search(
    http: &reqwest::blocking::Client,
    query: &str,
    mc_version: &str,
    loader: &str,
    project_type: &str,
) -> Vec<OnlineModResult> {
    match do_modrinth_search(http, query, mc_version, loader, project_type) {
        Ok(results) => results,
        Err(err) => {
            eprintln!("[search] Modrinth 搜索失败: {}", err);
            Vec::new()
        }
    }
}

fn run_curseforge_search(
    http: &reqwest::blocking::Client,
    query: &str,
    mc_version: &str,
    loader: &str,
    project_type: &str,
) -> Vec<OnlineModResult> {
    match do_curseforge_search(http, query, mc_version, loader, project_type) {
        Ok(results) => results,
        Err(err) => {
            eprintln!("[search] CurseForge 搜索失败: {}", err);
            Vec::new()
        }
    }
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
    let sort_index = if query.is_empty() {
        "downloads"
    } else {
        "relevance"
    };

    let url = format!(
        "https://api.modrinth.com/v2/search?query={}&facets={}&limit=80&index={}",
        urlencoding::encode(query),
        urlencoding::encode(&facets_str),
        sort_index,
    );

    let resp = http
        .get(&url)
        .send()
        .map_err(|e| format!("搜索请求失败: {}", e))?;
    if !resp.status().is_success() {
        return Err(format!("Modrinth HTTP {}", resp.status()));
    }
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

    let resp = http
        .get(&url)
        .header("x-api-key", &cf_api_key())
        .header("Accept", "application/json")
        .send()
        .map_err(|e| format!("CurseForge 请求失败: {}", e))?;
    if !resp.status().is_success() {
        return Err(format!("CurseForge HTTP {}", resp.status()));
    }

    let json: serde_json::Value = resp
        .json()
        .map_err(|e| format!("CurseForge 解析失败: {}", e))?;

    let mut results = Vec::new();
    if let Some(data) = json["data"].as_array() {
        for item in data {
            let authors = item["authors"]
                .as_array()
                .and_then(|a| a.first())
                .and_then(|a| a["name"].as_str())
                .unwrap_or("");
            let logo = item["logo"]["url"].as_str().unwrap_or("");
            let id = item["id"].as_u64().unwrap_or(0);
            let cf_url = item["links"]["websiteUrl"]
                .as_str()
                .unwrap_or("")
                .to_string();
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
