const MAX_COMPATIBILITY_PROBES: usize = 256;
const COMPATIBILITY_PROBE_BUDGET: std::time::Duration = std::time::Duration::from_secs(20);
const COMPATIBILITY_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

#[tauri::command]
pub async fn get_quilt_versions(mc_version: String) -> Result<Vec<String>, String> {
    tokio::task::spawn_blocking(move || -> Result<Vec<String>, String> {
        if !super::release_version_at_least(&mc_version, 14, 0) {
            return Ok(vec![]);
        }

        let http = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .user_agent("OAOI-Launcher/1.0")
            .build()
            .map_err(|e| e.to_string())?;
        let url = format!("https://meta.quiltmc.org/v3/versions/loader/{}", mc_version);
        let versions = fetch_quilt_versions_for_game(&http, &url)?;
        if !versions.is_empty() {
            return Ok(versions);
        }

        let global_versions = fetch_all_quilt_loaders(&http)?;
        find_compatible_quilt_loader(&http, &mc_version, global_versions)
    })
    .await
    .map_err(|e| e.to_string())?
}

fn fetch_quilt_versions_for_game(
    http: &reqwest::blocking::Client,
    url: &str,
) -> Result<Vec<String>, String> {
    let resp = http
        .get(url)
        .send()
        .map_err(|e| format!("获取 Quilt 版本失败: {}", e))?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(vec![]);
    }
    let resp = resp
        .error_for_status()
        .map_err(|e| format!("获取 Quilt 版本失败: {}", e))?;
    let json: serde_json::Value = resp
        .json()
        .map_err(|e| format!("解析 Quilt 版本失败: {}", e))?;
    parse_loader_versions(&json)
}

fn fetch_all_quilt_loaders(http: &reqwest::blocking::Client) -> Result<Vec<String>, String> {
    let resp = http
        .get("https://meta.quiltmc.org/v3/versions/loader")
        .send()
        .map_err(|e| format!("获取 Quilt Loader 列表失败: {}", e))?
        .error_for_status()
        .map_err(|e| format!("获取 Quilt Loader 列表失败: {}", e))?;
    let json: serde_json::Value = resp
        .json()
        .map_err(|e| format!("解析 Quilt Loader 列表失败: {}", e))?;
    parse_loader_versions(&json)
}

fn find_compatible_quilt_loader(
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
        let url = format!(
            "https://meta.quiltmc.org/v3/versions/loader/{}/{}/profile/json",
            mc_version, loader
        );
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
            return Ok(vec![loader]);
        }
    }
    Ok(vec![])
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
        .ok_or_else(|| "Quilt 版本响应格式错误".to_string())?;
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
