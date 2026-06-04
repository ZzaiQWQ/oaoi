pub fn with_mod_mirrors<I>(urls: I) -> Vec<String>
where
    I: IntoIterator<Item = String>,
{
    let mut official_seen = std::collections::HashSet::new();
    let mut mirror_seen = std::collections::HashSet::new();
    let mut officials = Vec::new();
    let mut mirrors = Vec::new();

    for url in urls {
        let url = url.trim().to_string();
        if url.is_empty() {
            continue;
        }
        push_unique(&mut officials, &mut official_seen, url.clone());
        if let Some(mirror) = mirror_mod_file_url(&url) {
            push_unique(&mut mirrors, &mut mirror_seen, mirror);
        }
    }

    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for url in officials {
        push_unique(&mut out, &mut seen, url);
    }
    for url in mirrors {
        push_unique(&mut out, &mut seen, url);
    }
    out
}

pub fn append_unique_urls<I>(out: &mut Vec<String>, urls: I)
where
    I: IntoIterator<Item = String>,
{
    let mut seen: std::collections::HashSet<String> = out.iter().cloned().collect();
    for url in urls {
        push_unique(out, &mut seen, url.trim().to_string());
    }
}

fn push_unique(out: &mut Vec<String>, seen: &mut std::collections::HashSet<String>, url: String) {
    if !url.is_empty() && seen.insert(url.clone()) {
        out.push(url);
    }
}

fn mirror_mod_file_url(url: &str) -> Option<String> {
    const RULES: [(&str, &str); 8] = [
        ("https://cdn.modrinth.com/", "https://mod.mcimirror.top/"),
        ("http://cdn.modrinth.com/", "https://mod.mcimirror.top/"),
        ("https://edge.forgecdn.net/", "https://mod.mcimirror.top/"),
        ("http://edge.forgecdn.net/", "https://mod.mcimirror.top/"),
        (
            "https://mediafilez.forgecdn.net/",
            "https://mod.mcimirror.top/",
        ),
        (
            "http://mediafilez.forgecdn.net/",
            "https://mod.mcimirror.top/",
        ),
        ("https://media.forgecdn.net/", "https://mod.mcimirror.top/"),
        ("http://media.forgecdn.net/", "https://mod.mcimirror.top/"),
    ];

    for (from, to) in RULES {
        if let Some(rest) = url.strip_prefix(from) {
            return Some(format!("{}{}", to, rest));
        }
    }

    None
}
