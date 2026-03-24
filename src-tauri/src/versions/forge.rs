#[tauri::command]
pub async fn get_forge_versions(mc_version: String) -> Result<Vec<String>, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result = (|| -> Result<Vec<String>, String> {
            let mc1 = mc_version.clone();
            let mc2 = mc_version.clone();
            
            // 使用 channel 竞速: 谁先返回非空结果就用谁
            let (result_tx, result_rx) = std::sync::mpsc::channel::<Vec<String>>();
            
            let tx1 = result_tx.clone();
            let bmcl_handle = std::thread::spawn(move || {
                let http = reqwest::blocking::Client::builder()
                    .timeout(std::time::Duration::from_secs(3))
                    .build().ok();
                let Some(client) = http else { let _ = tx1.send(vec![]); return; };
                let url = format!("https://bmclapi2.bangbang93.com/forge/minecraft/{}", mc1);
                let Ok(resp) = client.get(&url).send() else { let _ = tx1.send(vec![]); return; };
                let Ok(json) = resp.json::<serde_json::Value>() else { let _ = tx1.send(vec![]); return; };
                let Some(arr) = json.as_array() else { let _ = tx1.send(vec![]); return; };
                let mut versions: Vec<String> = arr.iter()
                    .filter_map(|v| v.get("version").and_then(|v| v.as_str()).map(|s| s.to_string()))
                    .collect();
                versions.reverse();
                let _ = tx1.send(versions);
            });
            
            let tx2 = result_tx.clone();
            let forge_handle = std::thread::spawn(move || {
                let http = reqwest::blocking::Client::builder()
                    .timeout(std::time::Duration::from_secs(5))
                    .user_agent("OAOI-Launcher/1.0")
                    .build().ok();
                let Some(client) = http else { let _ = tx2.send(vec![]); return; };
                let url = format!("https://files.minecraftforge.net/net/minecraftforge/forge/index_{}.html", mc2);
                let Ok(resp) = client.get(&url).send() else { let _ = tx2.send(vec![]); return; };
                if !resp.status().is_success() { let _ = tx2.send(vec![]); return; }
                let Ok(html) = resp.text() else { let _ = tx2.send(vec![]); return; };
                
                let prefix = format!("forge-{}-", mc2);
                let mut versions = Vec::new();
                let mut seen = std::collections::HashSet::new();
                for part in html.split(&prefix) {
                    if let Some(end) = part.find("-installer.jar") {
                        let ver = &part[..end];
                        if !ver.is_empty() && !ver.contains('<') && !ver.contains('"') && seen.insert(ver.to_string()) {
                            versions.push(ver.to_string());
                        }
                    }
                }
                let _ = tx2.send(versions);
            });

            drop(result_tx); // 关闭发送端，rx 在两个线程都完成后会返回 Err

            // 取第一个非空结果；如果两个都空则返回空
            let mut fallback = vec![];
            for received in result_rx {
                if !received.is_empty() {
                    // 拿到有效结果，等其他线程自然结束
                    let _ = bmcl_handle.join();
                    let _ = forge_handle.join();
                    return Ok(received);
                }
                fallback = received;
            }
            Ok(fallback)
        })();
        let _ = tx.send(result);
    });
    rx.recv().map_err(|_| "线程通信失败".to_string())?
}
