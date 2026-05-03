/// modcn.txt 条目：简写|中文名|英文名
#[derive(Clone)]
pub struct ModCnEntry {
    pub abbr: String,
    pub cn_name: String,
    pub en_name: String,
}

/// 全局缓存 modcn 数据
static MODCN_CACHE: std::sync::OnceLock<Vec<ModCnEntry>> = std::sync::OnceLock::new();

/// 编译时嵌入的 gzip 压缩 modcn 数据
const MODCN_GZ: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/modcn_data.gz"));

/// 加载 modcn 数据（从嵌入的 gzip 解压），使用 OnceLock 全局缓存
pub fn load_modcn() -> &'static Vec<ModCnEntry> {
    MODCN_CACHE.get_or_init(|| {
        use flate2::read::GzDecoder;
        use std::io::Read;
        let mut decoder = GzDecoder::new(MODCN_GZ);
        let mut content = String::new();
        if decoder.read_to_string(&mut content).is_err() {
            eprintln!("[modcn] gzip 解压失败");
            return Vec::new();
        }
        let mut entries = Vec::new();
        for line in content.lines() {
            let parts: Vec<&str> = line.splitn(3, '|').collect();
            if parts.len() >= 2 {
                entries.push(ModCnEntry {
                    abbr: parts.first().unwrap_or(&"").to_string(),
                    cn_name: parts.get(1).unwrap_or(&"").to_string(),
                    en_name: parts.get(2).unwrap_or(&"").to_string(),
                });
            }
        }
        eprintln!("[modcn] 加载 {} 条模组数据 (gzip embedded)", entries.len());
        entries
    })
}

/// 判断字符串中是否包含中文字符
pub fn contains_chinese(s: &str) -> bool {
    s.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
}

/// 计算两个字符串的字符重叠率 (0-100)
fn char_overlap_score(query: &str, target: &str) -> i32 {
    if query.is_empty() || target.is_empty() {
        return 0;
    }
    let query_chars: Vec<char> = query.chars().collect();
    let matched = query_chars.iter().filter(|c| target.contains(**c)).count();
    (matched as f64 / query_chars.len() as f64 * 100.0) as i32
}

/// 用中文查询模糊匹配 modcn 数据，返回匹配到的英文名列表
pub fn search_modcn_fuzzy(query: &str, entries: &[ModCnEntry]) -> Vec<(String, String, i32)> {
    let query_lower = query.to_lowercase();
    let mut matches: Vec<(String, String, i32)> = Vec::new(); // (en_name, cn_name, score)
    let mut seen = std::collections::HashSet::new();

    for entry in entries {
        let cn_lower = entry.cn_name.to_lowercase();
        let abbr_lower = entry.abbr.to_lowercase();

        // 跳过没有中文名和简写的条目
        if cn_lower.is_empty() && abbr_lower.is_empty() {
            continue;
        }

        let score =
            // 简写精确匹配
            if !abbr_lower.is_empty() && abbr_lower == query_lower { 200 }
            // 中文名精确匹配
            else if !cn_lower.is_empty() && cn_lower == query_lower { 200 }
            // 查询包含在中文名中 或 中文名包含在查询中
            else if !cn_lower.is_empty() && (cn_lower.contains(&query_lower) || query_lower.contains(&cn_lower)) {
                if cn_lower.starts_with(&query_lower) { 180 } else { 160 }
            }
            // 模糊匹配：根据查询长度动态调整阈值
            else if !cn_lower.is_empty() {
                let overlap = char_overlap_score(&query_lower, &cn_lower);
                // 短查询要求更严格：2字→80%，3字→60%，4+字→40%
                let min_overlap = match query_lower.chars().count() {
                    0..=2 => 80,
                    3 => 60,
                    _ => 40,
                };
                if overlap >= min_overlap { 100 + overlap } else { 0 }
            }
            else { 0 };

        let en = if entry.en_name.is_empty() {
            entry.cn_name.clone()
        } else {
            entry.en_name.clone()
        };
        if score > 0 && !en.is_empty() && seen.insert(en.to_lowercase()) {
            matches.push((en, entry.cn_name.clone(), score));
        }
    }

    matches.sort_by(|a, b| b.2.cmp(&a.2));
    matches.truncate(10);
    eprintln!(
        "[fuzzy] '{}' -> {} 个匹配: {:?}",
        query,
        matches.len(),
        matches
            .iter()
            .take(5)
            .map(|(en, cn, s)| format!("{}({}) s:{}", en, cn, s))
            .collect::<Vec<_>>()
    );
    matches
}
