use serde::Serialize;
use std::process::Command;
#[cfg(windows)]
use std::os::windows::process::CommandExt;

const MS_CLIENT_ID: &str = "b6affacf-765f-41e6-87ee-6fb373cdb2b5";

#[derive(Serialize, Clone)]
pub struct McProfile {
    pub name: String,
    pub uuid: String,
    pub access_token: String,
}

#[tauri::command]
pub fn start_ms_login() -> Result<McProfile, String> {
    let handle = std::thread::spawn(move || -> Result<McProfile, String> {
        // 1. 动态分配端口
        let listener = std::net::TcpListener::bind("127.0.0.1:0")
            .map_err(|e| format!("无法绑定端口: {}", e))?;
        let port = listener.local_addr()
            .map_err(|e| format!("获取端口失败: {}", e))?.port();
        let redirect_uri = format!("http://localhost:{}", port);

        let server = tiny_http::Server::from_listener(listener, None)
            .map_err(|e| format!("无法启动服务器: {}", e))?;

        // 2. 打开浏览器登录
        let auth_url = format!(
            "https://login.microsoftonline.com/consumers/oauth2/v2.0/authorize?client_id={}&response_type=code&redirect_uri={}&response_mode=query&scope=XboxLive.signin%20XboxLive.offline_access&prompt=select_account",
            MS_CLIENT_ID, redirect_uri
        );
        Command::new("rundll32")
            .args(["url.dll,FileProtocolHandler", &auth_url])
            .creation_flags(0x08000000)
            .spawn()
            .map_err(|e| format!("无法打开浏览器: {}", e))?;

        // 3. 等待回调（5分钟超时）
        let request = server.recv_timeout(std::time::Duration::from_secs(300))
            .map_err(|e| format!("监听失败: {}", e))?
            .ok_or("登录超时")?;

        let request_url = format!("http://localhost{}", request.url());
        let parsed = url::Url::parse(&request_url)
            .map_err(|e| format!("解析URL失败: {}", e))?;
        let code = parsed.query_pairs()
            .find(|(k, _)| k == "code")
            .map(|(_, v)| v.to_string())
            .ok_or_else(|| {
                parsed.query_pairs()
                    .find(|(k, _)| k == "error_description")
                    .map(|(_, v)| format!("登录被拒绝: {}", v))
                    .unwrap_or_else(|| "未收到授权码".to_string())
            })?;

        // 返回成功页面
        let resp = tiny_http::Response::from_string(
            "<html><body style='text-align:center;font-family:sans-serif;padding:50px'>\
             <h1>✅ 登录成功！</h1><p>请返回启动器</p>\
             <script>setTimeout(()=>window.close(),2000)</script></body></html>"
        ).with_header("Content-Type: text/html; charset=utf-8".parse::<tiny_http::Header>().unwrap());
        let _ = request.respond(resp);

        // 4. 换取 Token
        let client = reqwest::blocking::Client::new();
        let token_resp = client
            .post("https://login.microsoftonline.com/consumers/oauth2/v2.0/token")
            .form(&[
                ("client_id", MS_CLIENT_ID),
                ("code", code.as_str()),
                ("redirect_uri", redirect_uri.as_str()),
                ("grant_type", "authorization_code"),
                ("scope", "XboxLive.signin XboxLive.offline_access"),
            ])
            .send()
            .map_err(|e| format!("换取Token失败: {}", e))?;
        let token_json: serde_json::Value = token_resp.json()
            .map_err(|e| format!("Token解析失败: {}", e))?;
        let ms_token = token_json["access_token"].as_str()
            .ok_or_else(|| format!("未获取到Token: {}", token_json))?;

        // 5. Xbox Live
        let xbox_resp = client.post("https://user.auth.xboxlive.com/user/authenticate")
            .header("Content-Type", "application/json")
            .json(&serde_json::json!({
                "Properties": { "AuthMethod": "RPS", "SiteName": "user.auth.xboxlive.com", "RpsTicket": format!("d={}", ms_token) },
                "RelyingParty": "http://auth.xboxlive.com", "TokenType": "JWT"
            })).send().map_err(|e| format!("Xbox失败: {}", e))?;
        let xbox_json: serde_json::Value = xbox_resp.json().map_err(|e| format!("Xbox解析失败: {}", e))?;
        let xbox_token = xbox_json["Token"].as_str().ok_or(format!("Xbox Token空: {}", xbox_json))?;
        let user_hash = xbox_json["DisplayClaims"]["xui"][0]["uhs"].as_str().ok_or(format!("UserHash空: {}", xbox_json))?;

        // 6. XSTS
        let xsts_resp = client.post("https://xsts.auth.xboxlive.com/xsts/authorize")
            .header("Content-Type", "application/json")
            .json(&serde_json::json!({
                "Properties": { "SandboxId": "RETAIL", "UserTokens": [xbox_token] },
                "RelyingParty": "rp://api.minecraftservices.com/", "TokenType": "JWT"
            })).send().map_err(|e| format!("XSTS失败: {}", e))?;
        let xsts_json: serde_json::Value = xsts_resp.json().map_err(|e| format!("XSTS解析失败: {}", e))?;
        let xsts_token = xsts_json["Token"].as_str().ok_or(format!("XSTS Token空: {}", xsts_json))?;

        // 7. Minecraft
        let mc_resp = client.post("https://api.minecraftservices.com/authentication/login_with_xbox")
            .header("Content-Type", "application/json")
            .json(&serde_json::json!({ "identityToken": format!("XBL3.0 x={};{}", user_hash, xsts_token) }))
            .send().map_err(|e| format!("MC认证失败: {}", e))?;
        let mc_status = mc_resp.status().as_u16();
        let mc_json: serde_json::Value = mc_resp.json().map_err(|e| format!("MC解析失败: {}", e))?;
        if mc_status != 200 {
            let err = mc_json.get("error").map(|e| e.to_string()).unwrap_or_default();
            let msg = mc_json.get("errorMessage").and_then(|m| m.as_str()).unwrap_or("未知错误");
            return Err(format!("MC登录失败({}): {} - {}", mc_status, err, msg));
        }
        let mc_token = mc_json["access_token"].as_str().ok_or(format!("MC Token空: {}", mc_json))?;

        // 8. 玩家档案
        let profile_resp = client.get("https://api.minecraftservices.com/minecraft/profile")
            .header("Authorization", format!("Bearer {}", mc_token))
            .send().map_err(|e| format!("档案失败: {}", e))?;
        let profile_json: serde_json::Value = profile_resp.json().map_err(|e| format!("档案解析失败: {}", e))?;
        let name = profile_json["name"].as_str().ok_or(format!("无玩家名: {}", profile_json))?;
        let uuid = profile_json["id"].as_str().ok_or(format!("无UUID: {}", profile_json))?;

        Ok(McProfile { name: name.to_string(), uuid: uuid.to_string(), access_token: mc_token.to_string() })
    });

    handle.join().map_err(|_| "登录线程崩溃".to_string())?
}
