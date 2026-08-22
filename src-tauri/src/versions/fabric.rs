const MAX_COMPATIBILITY_PROBES: usize = 256;
const COMPATIBILITY_PROBE_BUDGET: std::time::Duration = std::time::Duration::from_secs(20);
const COMPATIBILITY_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);
const FABRIC_META_MIRROR_API: &str = "https://bmclapi2.bangbang93.com/fabric-meta/v2";
const FABRIC_META_OFFICIAL_API: &str = "https://meta.fabricmc.net/v2";

#[tauri::command]
pub async fn get_fabric_versions(mc_version: String) -> Result<Vec<String>, String> {
    tokio::task::spawn_blocking(move || -> Result<Vec<String>, String> {
        if !super::release_version_at_least(&mc_version, 14, 0) {
            return Ok(vec![]);
        }

        let http = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .user_agent("OAOI-Launcher/1.0")
            .build()
            .map_err(|e| e.to_string())?;
        let versions = fetch_fabric_versions_for_game(&http, &mc_version)?;
        if !versions.is_empty() {
            return Ok(versions);
        }

        let global_versions = fetch_all_fabric_loaders(&http)?;
        find_compatible_fabric_loader(&http, &mc_version, global_versions)
    })
    .await
    .map_err(|e| e.to_string())?
}

fn fetch_fabric_versions_for_game(
    http: &reqwest::blocking::Client,
    mc_version: &str,
) -> Result<Vec<String>, String> {
    let path = format!("/versions/loader/{}", urlencoding::encode(mc_version));
    fetch_loader_versions_with_fallback(http, &path, "获取 Fabric 版本")
}

fn fetch_loader_versions_at_url(
    http: &reqwest::blocking::Client,
    url: &str,
    error_context: &str,
) -> Result<Vec<String>, String> {
    let resp = http
        .get(url)
        .send()
        .map_err(|e| format!("{}失败: {}", error_context, e))?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(vec![]);
    }
    let resp = resp
        .error_for_status()
        .map_err(|e| format!("{}失败: {}", error_context, e))?;
    let json: serde_json::Value = resp
        .json()
        .map_err(|e| format!("解析 Fabric 响应失败: {}", e))?;
    parse_loader_versions(&json)
}

fn fetch_all_fabric_loaders(http: &reqwest::blocking::Client) -> Result<Vec<String>, String> {
    fetch_loader_versions_with_fallback(http, "/versions/loader", "获取 Fabric Loader 列表")
}

fn fabric_meta_urls(path: &str) -> [String; 2] {
    let path = if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{}", path)
    };
    [
        format!("{}{}", FABRIC_META_MIRROR_API, path),
        format!("{}{}", FABRIC_META_OFFICIAL_API, path),
    ]
}

fn fetch_loader_versions_with_fallback(
    http: &reqwest::blocking::Client,
    path: &str,
    error_context: &str,
) -> Result<Vec<String>, String> {
    let mut errors = Vec::new();
    let mut received_empty = false;
    for url in fabric_meta_urls(path) {
        match fetch_loader_versions_at_url(http, &url, error_context) {
            Ok(versions) if !versions.is_empty() => return Ok(versions),
            Ok(_) => received_empty = true,
            Err(error) => errors.push(format!("{} -> {}", url, error)),
        }
    }
    if received_empty {
        Ok(vec![])
    } else {
        Err(format!("{}失败: {}", error_context, errors.join(" | ")))
    }
}

fn find_compatible_fabric_loader(
    http: &reqwest::blocking::Client,
    mc_version: &str,
    versions: Vec<String>,
) -> Result<Vec<String>, String> {
    let started = std::time::Instant::now();
    let mut probed = 0usize;
    let mut seen = std::collections::HashSet::new();
    for loader in versions {
        if !seen.insert(loader.clone()) {
            continue;
        }
        if probed >= MAX_COMPATIBILITY_PROBES || started.elapsed() >= COMPATIBILITY_PROBE_BUDGET {
            break;
        }
        probed += 1;
        let path = format!(
            "/versions/loader/{}/{}/profile/json",
            urlencoding::encode(mc_version),
            urlencoding::encode(&loader)
        );
        if fabric_profile_exists(http, &path) {
            return Ok(vec![loader]);
        }
    }
    Ok(vec![])
}

fn fabric_profile_exists(http: &reqwest::blocking::Client, path: &str) -> bool {
    for url in fabric_meta_urls(path) {
        let Ok(resp) = http.get(&url).timeout(COMPATIBILITY_REQUEST_TIMEOUT).send() else {
            continue;
        };
        if !resp.status().is_success() {
            continue;
        }
        let Ok(profile) = resp.json::<serde_json::Value>() else {
            continue;
        };
        if valid_loader_profile(&profile) {
            return true;
        }
    }
    false
}

fn valid_loader_profile(profile: &serde_json::Value) -> bool {
    profile.is_object()
        && profile
            .get("mainClass")
            .and_then(|value| value.as_str())
            .is_some_and(|value| !value.trim().is_empty())
        && profile
            .get("libraries")
            .and_then(|value| value.as_array())
            .is_some()
}

fn parse_loader_versions(json: &serde_json::Value) -> Result<Vec<String>, String> {
    let arr = json
        .as_array()
        .ok_or_else(|| "Fabric 版本响应格式错误".to_string())?;
    Ok(arr
        .iter()
        .filter_map(|item| {
            item.get("loader")
                .and_then(|loader| loader.get("version"))
                .or_else(|| item.get("version"))
                .and_then(|value| value.as_str())
                .filter(|value| !value.trim().is_empty())
                .map(|value| value.to_string())
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::{fabric_meta_urls, parse_loader_versions, valid_loader_profile};

    #[test]
    fn fabric元数据固定镜像优先官方兜底() {
        let urls = fabric_meta_urls("/versions/loader/1.21.1");
        assert_eq!(
            urls[0],
            "https://bmclapi2.bangbang93.com/fabric-meta/v2/versions/loader/1.21.1"
        );
        assert_eq!(
            urls[1],
            "https://meta.fabricmc.net/v2/versions/loader/1.21.1"
        );
    }

    #[test]
    fn fabric版本响应兼容两种官方结构() {
        let nested = serde_json::json!([{"loader": {"version": "0.16.14"}}]);
        let flat = serde_json::json!([{"version": "0.16.13"}]);
        assert_eq!(parse_loader_versions(&nested).unwrap(), ["0.16.14"]);
        assert_eq!(parse_loader_versions(&flat).unwrap(), ["0.16.13"]);
    }

    #[test]
    fn fabric_profile必须包含主类和库列表() {
        assert!(valid_loader_profile(&serde_json::json!({
            "mainClass": "net.fabricmc.loader.impl.launch.knot.KnotClient",
            "libraries": []
        })));
        assert!(!valid_loader_profile(&serde_json::json!({"libraries": []})));
    }
}
