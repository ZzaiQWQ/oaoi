use super::detect_target_dir;
use crate::installer::download_file_if_needed_cancelable;
use crate::instance::{cf_api_key, safe_path_name};

/// 根据 fileId 构造 CurseForge CDN URL（fileId 拆分为 id/1000 和 id%1000 路径）
pub fn cf_cdn_urls(file_id: u32, file_name: &str) -> Vec<String> {
    let id1 = file_id / 1000;
    let id2 = file_id % 1000;
    let encoded_name = urlencoding::encode(file_name);
    vec![
        format!(
            "https://edge.forgecdn.net/files/{}/{}/{}",
            id1, id2, encoded_name
        ),
        format!(
            "https://mediafilez.forgecdn.net/files/{}/{}/{}",
            id1, id2, encoded_name
        ),
    ]
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
                    if dest.exists() {
                        return Ok(false);
                    }
                    // 1a) CDN + 文件名（通常比 API 返回的 downloadUrl 更快）
                    let cdn_urls = cf_cdn_urls(file_id, &fname);
                    for cdn in &cdn_urls {
                        if is_cancelled(cancel_name) {
                            return Err("用户取消下载".to_string());
                        }
                        eprintln!("[cf] CDN: {}", cdn);
                        let download_result = download_file_if_needed_cancelable(
                            http,
                            cdn,
                            &dest,
                            None,
                            false,
                            cancel_name,
                        );
                        match download_result {
                            Ok(r) => return Ok(r),
                            Err(e) => {
                                eprintln!("[cf] CDN失败: {}", e);
                                let _ = std::fs::remove_file(&dest);
                                continue;
                            }
                        }
                    }
                    // 1b) 有 downloadUrl 时作为兜底
                    if !dl_url.is_empty() {
                        eprintln!("[cf] API downloadUrl fallback: {}", fname);
                        return download_file_if_needed_cancelable(
                            http,
                            &dl_url,
                            &dest,
                            None,
                            false,
                            cancel_name,
                        );
                    }
                    // 1c) 尝试 download-url 端点
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
                                        eprintln!("[cf] download-url端点: {}", fname);
                                        return download_file_if_needed_cancelable(
                                            http,
                                            url,
                                            &dest,
                                            None,
                                            false,
                                            cancel_name,
                                        );
                                    }
                                }
                            }
                        }
                    }
                    return Err(format!(
                        "CF无可用URL: p={} f={} n={}",
                        project_id, file_id, fname
                    ));
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
        format!("https://edge.forgecdn.net/files/{}/{}", p1, p2),
        format!("https://mediafilez.forgecdn.net/files/{}/{}", p1, p2),
    ];
    for base_url in &base_urls {
        if is_cancelled(cancel_name) {
            return Err("用户取消下载".to_string());
        }
        eprintln!("[cf] CDN盲猜: {} p={} f={}", base_url, project_id, file_id);
        match http.get(base_url).send() {
            Ok(mut resp) => {
                if !resp.status().is_success() {
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
                let tmp_path = dest.with_extension("tmp");
                let written = write_response_cancelable(&mut resp, &tmp_path, cancel_name)
                    .map_err(|e| {
                        let _ = std::fs::remove_file(&tmp_path);
                        e
                    })?;
                std::fs::rename(&tmp_path, &dest).map_err(|e| format!("重命名失败: {}", e))?;
                eprintln!("[cf] CDN成功: {} ({} bytes)", filename, written);
                return Ok(true);
            }
            Err(e) => {
                eprintln!("[cf] CDN失败: {} - {}", base_url, e);
                continue;
            }
        }
    }
    Err(format!(
        "CurseForge下载失败: p={} f={}",
        project_id, file_id
    ))
}

fn is_cancelled(cancel_name: Option<&str>) -> bool {
    cancel_name.is_some_and(crate::instance::is_cancelled)
}

fn write_response_cancelable(
    resp: &mut reqwest::blocking::Response,
    dest: &std::path::Path,
    cancel_name: Option<&str>,
) -> Result<u64, String> {
    let mut file = std::fs::File::create(dest).map_err(|e| format!("创建文件失败: {}", e))?;
    let mut written = 0u64;
    let mut buf = [0u8; 128 * 1024];
    loop {
        if is_cancelled(cancel_name) {
            return Err("用户取消下载".to_string());
        }
        let read = std::io::Read::read(resp, &mut buf).map_err(|e| format!("读取失败: {}", e))?;
        if read == 0 {
            break;
        }
        std::io::Write::write_all(&mut file, &buf[..read])
            .map_err(|e| format!("写入失败: {}", e))?;
        written += read as u64;
    }
    Ok(written)
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
