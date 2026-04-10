use crate::installer::download_file_if_needed;
use crate::instance::cf_api_key;
use super::detect_target_dir;

/// 根据 fileId 构造 CurseForge CDN URL（fileId 拆分为 id/1000 和 id%1000 路径）
pub fn cf_cdn_urls(file_id: u32, file_name: &str) -> Vec<String> {
    let id1 = file_id / 1000;
    let id2 = file_id % 1000;
    vec![
        format!("https://edge.forgecdn.net/files/{}/{}/{}", id1, id2, file_name),
        format!("https://mediafilez.forgecdn.net/files/{}/{}/{}", id1, id2, file_name),
    ]
}

/// 单文件下载：先通过 CurseForge API 获取下载链接，失败则回退 CDN
pub fn cf_download_mod(http: &reqwest::blocking::Client, project_id: u32, file_id: u32, inst_dir: &std::path::Path) -> Result<bool, String> {
    let mods_dir = inst_dir.join("mods");
    std::fs::create_dir_all(&mods_dir).map_err(|e| e.to_string())?;

    let api_client = reqwest::blocking::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(8))
        .timeout(std::time::Duration::from_secs(30))
        .user_agent("OAOI-Launcher/1.0")
        .build().map_err(|e| e.to_string())?;

    // 策略1: API 获取文件信息
    let api_url = format!("https://api.curseforge.com/v1/mods/{}/files/{}", project_id, file_id);
    if let Ok(resp) = api_client.get(&api_url).header("x-api-key", &cf_api_key()).send() {
        if resp.status().is_success() {
            if let Ok(json) = resp.json::<serde_json::Value>() {
                let fname = json["data"]["fileName"].as_str().unwrap_or("").to_string();
                let dl_url = json["data"]["downloadUrl"].as_str().unwrap_or("").to_string();
                if !fname.is_empty() {
                    // 根据类型判断目标目录
                    let (target_dir, file_type) = detect_target_dir(&json["data"], &fname, inst_dir);
                    std::fs::create_dir_all(&target_dir).ok();
                    if file_type != "mod" {
                        eprintln!("[cf] {} → {} ({})", fname, target_dir.display(), file_type);
                    }
                    let dest = target_dir.join(&fname);
                    if dest.exists() { return Ok(false); }
                    // 1a) 有 downloadUrl
                    if !dl_url.is_empty() {
                        eprintln!("[cf] API downloadUrl: {}", fname);
                        return download_file_if_needed(http, &dl_url, &dest, None, false);
                    }
                    // 1b) 尝试 download-url 端点
                    let dl_api = format!("https://api.curseforge.com/v1/mods/{}/files/{}/download-url", project_id, file_id);
                    if let Ok(resp2) = api_client.get(&dl_api).header("x-api-key", &cf_api_key()).send() {
                        if resp2.status().is_success() {
                            if let Ok(json2) = resp2.json::<serde_json::Value>() {
                                if let Some(url) = json2["data"].as_str() {
                                    if !url.is_empty() {
                                        eprintln!("[cf] download-url端点: {}", fname);
                                        return download_file_if_needed(http, url, &dest, None, false);
                                    }
                                }
                            }
                        }
                    }
                    // 1c) CDN + 文件名
                    let cdn_urls = cf_cdn_urls(file_id, &fname);
                    for cdn in &cdn_urls {
                        eprintln!("[cf] CDN: {}", cdn);
                        match download_file_if_needed(http, cdn, &dest, None, false) {
                            Ok(r) => return Ok(r),
                            Err(e) => { eprintln!("[cf] CDN失败: {}", e); let _ = std::fs::remove_file(&dest); continue; }
                        }
                    }
                    return Err(format!("CF无可用URL: p={} f={} n={}", project_id, file_id, fname));
                }
            }
        } else {
            eprintln!("[cf] API HTTP {}: p={} f={}", resp.status(), project_id, file_id);
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
        format!("https://edge.forgecdn.net/files/{}/{}", p1, p2),
        format!("https://mediafilez.forgecdn.net/files/{}/{}", p1, p2),
    ];
    for base_url in &base_urls {
        eprintln!("[cf] CDN盲猜: {} p={} f={}", base_url, project_id, file_id);
        match http.get(base_url).send() {
            Ok(mut resp) => {
                if !resp.status().is_success() { continue; }
                let final_url = resp.url().to_string();
                let filename = final_url.rsplit('/').next().unwrap_or("").split('?').next().unwrap_or("");
                let filename = percent_decode_simple(filename);
                let filename = if filename.is_empty() || filename == p2 {
                    resp.headers().get("content-disposition")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.split("filename=").nth(1).or_else(|| v.split("filename*=UTF-8''").nth(1)))
                        .map(|s| s.trim_matches('"').to_string())
                        .unwrap_or_else(|| format!("{}-{}.jar", project_id, file_id))
                } else { filename.to_string() };
                // 根据文件名判断目标目录（盲猜模式没有 API 数据，用空 JSON）
                let empty_json = serde_json::json!({});
                let (target_dir, file_type) = detect_target_dir(&empty_json, &filename, inst_dir);
                std::fs::create_dir_all(&target_dir).ok();
                if file_type != "mod" {
                    eprintln!("[cf] CDN盲猜分类: {} → {} ({})", filename, target_dir.display(), file_type);
                }
                let dest = target_dir.join(&filename);
                let mut file = std::fs::File::create(&dest).map_err(|e| format!("创建文件失败: {}", e))?;
                let written = std::io::copy(&mut resp, &mut file).map_err(|e| format!("写入失败: {}", e))?;
                eprintln!("[cf] CDN成功: {} ({} bytes)", filename, written);
                return Ok(true);
            }
            Err(e) => { eprintln!("[cf] CDN失败: {} - {}", base_url, e); continue; }
        }
    }
    Err(format!("CurseForge下载失败: p={} f={}", project_id, file_id))
}

/// 简单的 URL percent-decode
fn percent_decode_simple(s: &str) -> String {
    let mut result = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(val) = u8::from_str_radix(
                std::str::from_utf8(&bytes[i+1..i+3]).unwrap_or(""),
                16,
            ) {
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
