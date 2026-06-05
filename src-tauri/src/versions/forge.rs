#[tauri::command]
pub async fn get_forge_versions(mc_version: String) -> Result<Vec<String>, String> {
    tokio::task::spawn_blocking(move || -> Result<Vec<String>, String> {
        let http = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(8))
            .user_agent("OAOI-Launcher/1.0")
            .build()
            .map_err(|e| e.to_string())?;

        let mut versions = Vec::new();
        let mut seen = std::collections::HashSet::new();

        merge_versions(
            &mut versions,
            &mut seen,
            fetch_bmcl_versions(&http, &mc_version),
        );
        for url in [
            "https://bmclapi2.bangbang93.com/maven/net/minecraftforge/forge/maven-metadata.xml",
            "https://maven.minecraftforge.net/net/minecraftforge/forge/maven-metadata.xml",
        ] {
            merge_versions(
                &mut versions,
                &mut seen,
                fetch_maven_metadata_versions(&http, url, &mc_version),
            );
        }
        if versions.is_empty() {
            merge_versions(
                &mut versions,
                &mut seen,
                fetch_forge_html_versions(&http, &mc_version),
            );
        }

        Ok(versions)
    })
    .await
    .map_err(|e| e.to_string())?
}

fn fetch_bmcl_versions(http: &reqwest::blocking::Client, mc_version: &str) -> Vec<String> {
    let url = format!(
        "https://bmclapi2.bangbang93.com/forge/minecraft/{}",
        mc_version
    );
    let Ok(resp) = http.get(url).send() else {
        return vec![];
    };
    let Ok(json) = resp.json::<serde_json::Value>() else {
        return vec![];
    };
    let Some(arr) = json.as_array() else {
        return vec![];
    };

    let mut versions = arr
        .iter()
        .filter_map(|item| {
            let version = item
                .get("version")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            let branch = item
                .get("branch")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .trim();
            let version = if branch.is_empty() || version.ends_with(&format!("-{}", branch)) {
                version.to_string()
            } else {
                format!("{}-{}", version, branch)
            };
            normalize_forge_version(mc_version, &version)
        })
        .collect::<Vec<_>>();
    versions.reverse();
    versions
}

fn fetch_maven_metadata_versions(
    http: &reqwest::blocking::Client,
    url: &str,
    mc_version: &str,
) -> Vec<String> {
    let Ok(resp) = http.get(url).send() else {
        return vec![];
    };
    if !resp.status().is_success() {
        return vec![];
    }
    let Ok(xml) = resp.text() else {
        return vec![];
    };

    let mut versions = Vec::new();
    let mut rest = xml.as_str();
    while let Some(start) = rest.find("<version>") {
        rest = &rest[start + "<version>".len()..];
        let Some(end) = rest.find("</version>") else {
            break;
        };
        if let Some(version) = normalize_maven_forge_version(mc_version, &rest[..end]) {
            versions.push(version);
        }
        rest = &rest[end + "</version>".len()..];
    }
    versions.reverse();
    versions
}

fn fetch_forge_html_versions(http: &reqwest::blocking::Client, mc_version: &str) -> Vec<String> {
    let url = format!(
        "https://files.minecraftforge.net/net/minecraftforge/forge/index_{}.html",
        mc_version
    );
    let Ok(resp) = http.get(url).send() else {
        return vec![];
    };
    if !resp.status().is_success() {
        return vec![];
    }
    let Ok(html) = resp.text() else {
        return vec![];
    };

    let prefix = format!("forge-{}-", mc_version);
    html.split(&prefix)
        .filter_map(|part| {
            part.find("-installer.jar")
                .and_then(|end| normalize_forge_version(mc_version, &part[..end]))
        })
        .collect()
}

fn normalize_forge_version(mc_version: &str, value: &str) -> Option<String> {
    let mut version = value.trim();
    if version.is_empty() || version.contains('<') || version.contains('"') || version.contains('/')
    {
        return None;
    }
    if let Some(rest) = version.strip_prefix("forge-") {
        version = rest;
    }
    let full_prefix = format!("{}-", mc_version);
    if let Some(rest) = version.strip_prefix(&full_prefix) {
        return normalize_forge_loader_version(rest);
    }
    if looks_like_other_minecraft_prefix(version) {
        return None;
    }
    normalize_forge_loader_version(version)
}

fn normalize_maven_forge_version(mc_version: &str, value: &str) -> Option<String> {
    let mut version = value.trim();
    if version.is_empty() || version.contains('<') || version.contains('"') || version.contains('/')
    {
        return None;
    }
    if let Some(rest) = version.strip_prefix("forge-") {
        version = rest;
    }
    let full_prefix = format!("{}-", mc_version);
    version
        .strip_prefix(&full_prefix)
        .and_then(normalize_forge_loader_version)
}

fn normalize_forge_loader_version(version: &str) -> Option<String> {
    let version = version.trim();
    if version.is_empty() || version.contains('<') || version.contains('"') || version.contains('/')
    {
        None
    } else {
        Some(version.to_string())
    }
}

fn looks_like_other_minecraft_prefix(version: &str) -> bool {
    let Some((head, _)) = version.split_once('-') else {
        return false;
    };
    if !head.starts_with("1.") {
        return false;
    }
    let mut count = 0;
    for part in head.split('.') {
        if part.is_empty() || !part.chars().all(|ch| ch.is_ascii_digit()) {
            return false;
        }
        count += 1;
    }
    count >= 2
}

fn merge_versions(
    out: &mut Vec<String>,
    seen: &mut std::collections::HashSet<String>,
    versions: Vec<String>,
) {
    for version in versions {
        if seen.insert(version.clone()) {
            out.push(version);
        }
    }
}
