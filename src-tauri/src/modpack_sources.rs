pub(crate) fn safe_index_name(value: &str) -> String {
    value
        .trim()
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect()
}

pub fn sha1_from_curseforge_hashes(value: &serde_json::Value) -> Option<String> {
    value.as_array().and_then(|hashes| {
        hashes.iter().find_map(|item| {
            let algo = item["algo"].as_u64().or_else(|| item["algorithm"].as_u64());
            if algo == Some(1) {
                item["value"].as_str().map(|v| v.to_lowercase())
            } else {
                None
            }
        })
    })
}
