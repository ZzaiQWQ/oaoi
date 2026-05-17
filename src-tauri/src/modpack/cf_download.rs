use super::detect_target_dir;
use crate::installer::download_file_exact_once_with_stall_timeout;
use crate::instance::{cf_api_key, safe_path_name};
use crate::modpack_sources::sha1_from_curseforge_hashes;

const CF_DOWNLOAD_ROUNDS: usize = 3;
const CF_RETRY_DELAY_SECS: u64 = 15;
const CF_STALL_TIMEOUT_SECS: u64 = 15;
const CF_TOTAL_TIMEOUT_SECS: u64 = 180;

/// 根据 fileId 构造 CurseForge CDN URL（fileId 拆分为 id/1000 和 id%1000 路径）
pub fn cf_cdn_urls(file_id: u32, file_name: &str) -> Vec<String> {
    let id1 = file_id / 1000;
    let id2 = file_id % 1000;
    let encoded_name = urlencoding::encode(file_name);
    vec![
        format!(
            "https://mediafilez.forgecdn.net/files/{}/{}/{}",
            id1, id2, encoded_name
        ),
        format!(
            "https://edge.forgecdn.net/files/{}/{}/{}",
            id1, id2, encoded_name
        ),
    ]
}

fn cf_retry_delay(last_err: &str) -> std::time::Duration {
    let lower = last_err.to_ascii_lowercase();
    if last_err.contains("429")
        || last_err.contains("超时")
        || last_err.contains("下载过慢")
        || lower.contains("timeout")
        || lower.contains("timed out")
        || lower.contains("deadline")
        || lower.contains("too slow")
    {
        std::time::Duration::from_secs(CF_RETRY_DELAY_SECS)
    } else {
        std::time::Duration::from_millis(500)
    }
}

fn download_cf_candidates(
    _http: &reqwest::blocking::Client,
    urls: &[String],
    dest: &std::path::Path,
    expected_sha1: Option<&str>,
    cancel_name: Option<&str>,
) -> Result<bool, String> {
    if urls.is_empty() {
        return Err("CF无可用URL".to_string());
    }

    let mut last_err = String::new();
    for round in 0..CF_DOWNLOAD_ROUNDS {
        for url in urls {
            if is_cancelled(cancel_name) {
                return Err("用户取消下载".to_string());
            }
            eprintln!("[cf] try {}/{}: {}", round + 1, CF_DOWNLOAD_ROUNDS, url);
            match download_file_exact_once_with_stall_timeout(
                url,
                dest,
                expected_sha1,
                cancel_name,
                CF_STALL_TIMEOUT_SECS,
                CF_TOTAL_TIMEOUT_SECS,
            ) {
                Ok(result) => return Ok(result),
                Err(e) => {
                    last_err = format!("{}: {}", url, e);
                    eprintln!("[cf] failed, rotate: {}", last_err);
                    let _ = std::fs::remove_file(dest);
                }
            }
        }

        if round + 1 < CF_DOWNLOAD_ROUNDS {
            std::thread::sleep(cf_retry_delay(&last_err));
        }
    }

    Err(format!("CurseForge下载失败: {}", last_err))
}

pub fn cf_download_mod_cancelable(
    http: &reqwest::blocking::Client,
    project_id: u32,
    file_id: u32,
    inst_dir: &std::path::Path,
    cancel_name: Option<&str>,
) -> Result<bool, String> {
    let mods_dir = inst_dir.join("mods");
    std::fs::create_dir_all(&mods_dir).map_err(|e| e.to_string())?;
    if is_cancelled(cancel_name) {
        return Err("用户取消下载".to_string());
    }

    let api_client = reqwest::blocking::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(8))
        .timeout(std::time::Duration::from_secs(30))
        .user_agent("OAOI-Launcher/1.0")
        .build()
        .map_err(|e| e.to_string())?;

    // 策略1: API 获取文件信息
    let api_url = format!(
        "https://api.curseforge.com/v1/mods/{}/files/{}",
        project_id, file_id
    );
    if is_cancelled(cancel_name) {
        return Err("用户取消下载".to_string());
    }
    if let Ok(resp) = api_client
        .get(&api_url)
        .header("x-api-key", &cf_api_key())
        .send()
    {
        if resp.status().is_success() {
            if let Ok(json) = resp.json::<serde_json::Value>() {
                let fname = json["data"]["fileName"].as_str().unwrap_or("").to_string();
                let dl_url = json["data"]["downloadUrl"]
                    .as_str()
                    .unwrap_or("")
                    .to_string();
                let expected_sha1 = sha1_from_curseforge_hashes(&json["data"]["hashes"]);
                if !fname.is_empty() {
                    let safe_fname = safe_path_name(&fname, "文件名")?;
                    // 根据类型判断目标目录
                    let (target_dir, file_type) =
                        detect_target_dir(&json["data"], &safe_fname, inst_dir);
                    std::fs::create_dir_all(&target_dir).ok();
                    if file_type != "mod" {
                        eprintln!(
                            "[cf] {} → {} ({})",
                            safe_fname,
                            target_dir.display(),
                            file_type
                        );
                    }
                    let dest = target_dir.join(&safe_fname);
                    // CDN + API URL 轮转，避免一个慢 CDN 地址内部连续重试导致长时间卡住。
                    let mut urls = cf_cdn_urls(file_id, &fname);
                    if !dl_url.is_empty() {
                        urls.push(dl_url);
                    }

                    // 尝试 download-url 端点，把结果也加入候选。
                    let dl_api = format!(
                        "https://api.curseforge.com/v1/mods/{}/files/{}/download-url",
                        project_id, file_id
                    );
                    if let Ok(resp2) = api_client
                        .get(&dl_api)
                        .header("x-api-key", &cf_api_key())
                        .send()
                    {
                        if resp2.status().is_success() {
                            if let Ok(json2) = resp2.json::<serde_json::Value>() {
                                if let Some(url) = json2["data"].as_str() {
                                    if !url.is_empty() {
                                        urls.push(url.to_string());
                                    }
                                }
                            }
                        }
                    }
                    return download_cf_candidates(
                        http,
                        &urls,
                        &dest,
                        expected_sha1.as_deref(),
                        cancel_name,
                    );
                }
            }
        } else {
            eprintln!(
                "[cf] API HTTP {}: p={} f={}",
                resp.status(),
                project_id,
                file_id
            );
        }
    }

    // 策略2: API 完全失败，CDN 盲猜
    let id_str = file_id.to_string();
    let (p1, p2) = if id_str.len() > 4 {
        let (a, b) = id_str.split_at(4);
        (a.to_string(), b.to_string())
    } else {
        ((file_id / 1000).to_string(), (file_id % 1000).to_string())
    };
    let base_urls = vec![
        format!("https://mediafilez.forgecdn.net/files/{}/{}", p1, p2),
        format!("https://edge.forgecdn.net/files/{}/{}", p1, p2),
    ];
    let mut last_err = String::new();
    for round in 0..CF_DOWNLOAD_ROUNDS {
        for base_url in &base_urls {
            if is_cancelled(cancel_name) {
                return Err("用户取消下载".to_string());
            }
            eprintln!(
                "[cf] CDN盲猜 {}/{}: {} p={} f={}",
                round + 1,
                CF_DOWNLOAD_ROUNDS,
                base_url,
                project_id,
                file_id
            );
            match http.get(base_url).send() {
                Ok(resp) => {
                    if !resp.status().is_success() {
                        last_err = format!("{} HTTP {}", base_url, resp.status());
                        continue;
                    }
                    let final_url = resp.url().to_string();
                    let filename = final_url
                        .rsplit('/')
                        .next()
                        .unwrap_or("")
                        .split('?')
                        .next()
                        .unwrap_or("");
                    let filename = percent_decode_simple(filename);
                    let filename = if filename.is_empty() || filename == p2 {
                        resp.headers()
                            .get("content-disposition")
                            .and_then(|v| v.to_str().ok())
                            .and_then(|v| {
                                v.split("filename=")
                                    .nth(1)
                                    .or_else(|| v.split("filename*=UTF-8''").nth(1))
                            })
                            .map(|s| s.trim_matches('"').to_string())
                            .unwrap_or_else(|| format!("{}-{}.jar", project_id, file_id))
                    } else {
                        filename.to_string()
                    };
                    let safe_filename = safe_path_name(&filename, "文件名")?;
                    // 根据文件名判断目标目录（盲猜模式没有 API 数据，用空 JSON）
                    let empty_json = serde_json::json!({});
                    let (target_dir, file_type) =
                        detect_target_dir(&empty_json, &safe_filename, inst_dir);
                    std::fs::create_dir_all(&target_dir).ok();
                    if file_type != "mod" {
                        eprintln!(
                            "[cf] CDN盲猜分类: {} → {} ({})",
                            safe_filename,
                            target_dir.display(),
                            file_type
                        );
                    }
                    let dest = target_dir.join(&safe_filename);
                    match download_file_exact_once_with_stall_timeout(
                        &final_url,
                        &dest,
                        None,
                        cancel_name,
                        CF_STALL_TIMEOUT_SECS,
                        CF_TOTAL_TIMEOUT_SECS,
                    ) {
                        Ok(result) => {
                            eprintln!("[cf] CDN成功: {}", filename);
                            return Ok(result);
                        }
                        Err(e) => {
                            last_err = format!("{}: {}", final_url, e);
                            eprintln!("[cf] CDN失败: {}", last_err);
                            let _ = std::fs::remove_file(&dest);
                        }
                    }
                }
                Err(e) => {
                    last_err = format!("{} - {}", base_url, e);
                    eprintln!("[cf] CDN失败: {}", last_err);
                    continue;
                }
            }
        }
        if round + 1 < CF_DOWNLOAD_ROUNDS {
            std::thread::sleep(cf_retry_delay(&last_err));
        }
    }
    Err(format!(
        "CurseForge下载失败: p={} f={} ({})",
        project_id, file_id, last_err
    ))
}

fn is_cancelled(cancel_name: Option<&str>) -> bool {
    cancel_name.is_some_and(crate::instance::is_cancelled)
}

/// 简单的 URL percent-decode
fn percent_decode_simple(s: &str) -> String {
    let mut result = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(val) =
                u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""), 16)
            {
                result.push(val);
                i += 3;
                continue;
            }
        }
        result.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(result).unwrap_or_else(|_| s.to_string())
}
