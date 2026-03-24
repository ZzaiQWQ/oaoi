use tauri::Manager;
use tauri::Emitter;
use tauri::window::Color;
use sysinfo::System;
use serde::Serialize;
use std::path::Path;
use std::process::Command;
use std::collections::HashSet;

#[derive(Serialize, Clone)]
struct JavaInfo {
    path: String,
    version: String,
    major: u32,
}

#[tauri::command]
fn get_system_memory() -> u64 {
    let sys = System::new_all();
    sys.total_memory() / 1024 / 1024
}

#[tauri::command]
fn find_java() -> Vec<JavaInfo> {
    let mut results = Vec::new();
    let mut checked = HashSet::new();

    let mut try_java = |path: String| {
        let p = Path::new(&path);
        if p.exists() && checked.insert(path.clone()) {
            if let Some(info) = get_java_info(&path) {
                results.push(info);
            }
        }
    };

    // 1. where java (PATH)
    if let Ok(output) = Command::new("where").arg("java").output() {
        if let Ok(stdout) = String::from_utf8(output.stdout) {
            for line in stdout.lines() {
                let path = line.trim().to_string();
                if !path.is_empty() { try_java(path); }
            }
        }
    }

    // 2. JAVA_HOME
    if let Ok(java_home) = std::env::var("JAVA_HOME") {
        try_java(format!("{}\\bin\\java.exe", java_home));
    }

    // 3. Windows 注册表
    for reg_key in &[
        "HKLM\\SOFTWARE\\JavaSoft\\Java Runtime Environment",
        "HKLM\\SOFTWARE\\JavaSoft\\JDK",
        "HKLM\\SOFTWARE\\JavaSoft\\Java Development Kit",
        "HKLM\\SOFTWARE\\WOW6432Node\\JavaSoft\\Java Runtime Environment",
        "HKLM\\SOFTWARE\\WOW6432Node\\JavaSoft\\JDK",
    ] {
        if let Ok(out) = Command::new("reg")
            .args(["query", reg_key, "/s", "/v", "JavaHome"])
            .output()
        {
            if let Ok(text) = String::from_utf8(out.stdout) {
                for line in text.lines() {
                    if line.trim().to_lowercase().contains("javahome") {
                        if let Some(val) = line.split_whitespace().last() {
                            try_java(format!("{}\\bin\\java.exe", val));
                        }
                    }
                }
            }
        }
    }

    // 4. 扫描常见安装路径
    let known_names = [
        "Java", "java", "jdk", "jre",
        "Program Files\\Java",
        "Program Files (x86)\\Java",
        "Program Files\\Eclipse Adoptium",
        "Program Files\\Microsoft",
        "Program Files\\Zulu",
        "Program Files\\BellSoft",
        "Program Files\\Amazon Corretto",
    ];

    let drives: Vec<String> = ('A'..='Z')
        .filter(|c| Path::new(&format!("{}:\\", c)).exists())
        .map(|c| c.to_string())
        .collect();

    for drive in &drives {
        for name in &known_names {
            let base = format!("{}:\\{}", drive, name);
            let base_path = Path::new(&base);
            if !base_path.exists() { continue; }
            try_java(format!("{}\\bin\\java.exe", base));
            if let Ok(entries) = std::fs::read_dir(base_path) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if p.is_dir() {
                        try_java(p.join("bin").join("java.exe").to_string_lossy().to_string());
                        if let Ok(inner) = std::fs::read_dir(&p) {
                            for ie in inner.flatten() {
                                if ie.path().is_dir() {
                                    try_java(ie.path().join("bin").join("java.exe").to_string_lossy().to_string());
                                }
                            }
                        }
                    }
                }
            }
        }

        // 扫根目录顶层
        let root = format!("{}:\\", drive);
        if let Ok(entries) = std::fs::read_dir(&root) {
            for entry in entries.flatten() {
                let p = entry.path();
                if !p.is_dir() { continue; }
                try_java(p.join("bin").join("java.exe").to_string_lossy().to_string());
                if let Ok(inner) = std::fs::read_dir(&p) {
                    for ie in inner.flatten() {
                        if ie.path().is_dir() {
                            try_java(ie.path().join("bin").join("java.exe").to_string_lossy().to_string());
                        }
                    }
                }
            }
        }
    }

    results
}

fn get_java_info(path: &str) -> Option<JavaInfo> {
    let output = Command::new(path).arg("-version").output().ok()?;
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let version = parse_java_version(&stderr)?;
    let major = extract_major(&version);
    Some(JavaInfo { path: path.to_string(), version, major })
}

fn parse_java_version(output: &str) -> Option<String> {
    for line in output.lines() {
        if line.contains("version") {
            if let Some(start) = line.find('"') {
                if let Some(end) = line[start + 1..].find('"') {
                    return Some(line[start + 1..start + 1 + end].to_string());
                }
            }
        }
    }
    None
}

fn extract_major(version: &str) -> u32 {
    if version.starts_with("1.8") { return 8; }
    version.split('.').next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

#[tauri::command]
fn init_game_dir(base_dir: String) -> Result<String, String> {
    let mc_dir = Path::new(&base_dir).join(".minecraft");
    let dirs = ["versions", "assets", "libraries", "mods", "config", "resourcepacks", "saves"];
    for d in &dirs {
        let p = mc_dir.join(d);
        if !p.exists() {
            std::fs::create_dir_all(&p).map_err(|e| format!("创建目录失败: {}", e))?;
        }
    }
    // 创建 launcher_profiles.json（Minecraft 需要它）
    let profiles = mc_dir.join("launcher_profiles.json");
    if !profiles.exists() {
        std::fs::write(&profiles, r#"{"profiles":{}}"#)
            .map_err(|e| format!("创建配置文件失败: {}", e))?;
    }
    Ok(mc_dir.to_string_lossy().to_string())
}

#[derive(serde::Deserialize)]
struct LaunchOptions {
    java_path: String,
    game_dir: String,
    version_name: String,
    player_name: String,
    memory_mb: u32,
    server_ip: Option<String>,
    server_port: Option<u16>,
    access_token: Option<String>,
    uuid: Option<String>,
}

#[tauri::command]
fn launch_minecraft(options: LaunchOptions) -> Result<String, String> {
    let game_dir = resolve_game_dir(&options.game_dir);
    if !game_dir.exists() {
        return Err("游戏目录不存在".to_string());
    }

    // 实例目录
    let version_name = &options.version_name;
    let ver_dir = game_dir.join("instances").join(version_name);
    if !ver_dir.exists() {
        return Err(format!("实例 {} 未安装", version_name));
    }

    // 读取实例 JSON
    let version_json_path = ver_dir.join("instance.json");
    let json_str = std::fs::read_to_string(&version_json_path)
        .map_err(|e| format!("读取实例配置失败: {}", e))?;
    let json: serde_json::Value = serde_json::from_str(&json_str)
        .map_err(|e| format!("解析实例 JSON 失败: {}", e))?;

    // 获取主类
    let main_class = json["mainClass"].as_str()
        .ok_or("版本 JSON 中缺少 mainClass")?;

    // 获取 asset index
    let asset_index = json["assetIndex"]["id"].as_str().unwrap_or("legacy");

    // 构建 classpath（按 group:artifact 去重，后面的覆盖前面的，即 Fabric > Vanilla）
    let libs_dir = game_dir.join("libs");
    let mut classpath = Vec::new();
    let mut seen_keys: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

    if let Some(libs) = json["libraries"].as_array() {
        for lib in libs {
            if let Some(rules) = lib["rules"].as_array() {
                let mut allowed = false;
                for rule in rules {
                    let action = rule["action"].as_str().unwrap_or("");
                    let os_name = rule["os"]["name"].as_str();
                    match (action, os_name) {
                        ("allow", None) => allowed = true,
                        ("allow", Some("windows")) => allowed = true,
                        ("disallow", Some("windows")) => { allowed = false; break; },
                        ("disallow", None) => { allowed = false; break; },
                        _ => {}
                    }
                }
                if !allowed { continue; }
            }

            // 解析库路径
            let lib_path_opt = if let Some(artifact) = lib["downloads"]["artifact"]["path"].as_str() {
                let p = libs_dir.join(artifact.replace('/', "\\"));
                if p.exists() { Some(p.to_string_lossy().to_string()) } else { None }
            } else if let Some(name) = lib["name"].as_str() {
                let parts: Vec<&str> = name.split(':').collect();
                if parts.len() >= 3 {
                    let group_path = parts[0].replace('.', "\\");
                    let artifact_name = parts[1];
                    let version = parts[2];
                    let jar_name = if parts.len() >= 4 {
                        format!("{}-{}-{}.jar", artifact_name, version, parts[3])
                    } else {
                        format!("{}-{}.jar", artifact_name, version)
                    };
                    let p = libs_dir.join(&group_path).join(artifact_name).join(version).join(&jar_name);
                    if p.exists() { Some(p.to_string_lossy().to_string()) } else { None }
                } else { None }
            } else { None };

            if let Some(path) = lib_path_opt {
                // 取 group:artifact[:classifier] 作为去重 key（忽略版本号，保留 classifier）
                let dedup_key = lib["name"].as_str()
                    .and_then(|n| {
                        let parts: Vec<&str> = n.split(':').collect();
                        if parts.len() >= 4 {
                            // group:artifact:version:classifier -> key = group:artifact:classifier
                            Some(format!("{}:{}:{}", parts[0], parts[1], parts[3]))
                        } else if parts.len() >= 2 {
                            Some(format!("{}:{}", parts[0], parts[1]))
                        } else { None }
                    })
                    .unwrap_or_default();

                if !dedup_key.is_empty() {
                    if let Some(&idx) = seen_keys.get(&dedup_key) {
                        // 后面的版本覆盖前面的（Fabric 库排在 vanilla 后面，优先级更高）
                        classpath[idx] = path;
                    } else {
                        seen_keys.insert(dedup_key, classpath.len());
                        classpath.push(path);
                    }
                } else {
                    classpath.push(path);
                }
            }
        }
    }

    // 添加版本 jar
    let version_jar = ver_dir.join("client.jar");
    if version_jar.exists() {
        classpath.push(version_jar.to_string_lossy().to_string());
    }

    // 检查 classpath
    let total_libs = json["libraries"].as_array().map(|a| a.len()).unwrap_or(0);
    if classpath.is_empty() {
        return Err(format!(
            "未找到任何库文件！\n版本 JSON 中有 {} 个库，但 libraries 目录 ({}) 中没有对应的 jar 文件。\n请确保游戏文件完整。",
            total_libs,
            libs_dir.to_string_lossy()
        ));
    }

    // natives 目录
    let natives_dir = ver_dir.join("natives");
    if !natives_dir.exists() { let _ = std::fs::create_dir_all(&natives_dir); }

    // 根据系统语言自动设置 Minecraft 语言（仅首次）
    let options_file = ver_dir.join("options.txt");
    if !options_file.exists() {
        // 获取系统语言
        let sys_lang = Command::new("powershell")
            .args(["-NoProfile", "-c", "(Get-Culture).Name"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();
        let mc_lang = match sys_lang.to_lowercase().as_str() {
            "zh-cn" => "zh_cn",
            "zh-tw" | "zh-hk" => "zh_tw",
            "ja-jp" => "ja_jp",
            "ko-kr" => "ko_kr",
            "ru-ru" => "ru_ru",
            "de-de" => "de_de",
            "fr-fr" => "fr_fr",
            "es-es" => "es_es",
            "pt-br" => "pt_br",
            _ => "en_us",
        };
        let _ = std::fs::write(&options_file, format!("lang:{}\n", mc_lang));
    }

    // 生成离线 UUID
    let uuid = format!("{:032x}", {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        options.player_name.hash(&mut hasher);
        hasher.finish() as u128
    });

    // 构建启动参数
    let mut args: Vec<String> = vec![
        format!("-Xmx{}m", options.memory_mb),
        format!("-Xmn{}m", (options.memory_mb as f32 * 0.15) as u32),
        format!("-Djava.library.path={}", natives_dir.to_string_lossy()),
        "-Dlog4j2.formatMsgNoLookups=true".to_string(),
        "-XX:+UnlockExperimentalVMOptions".to_string(),
        "-XX:+UseG1GC".to_string(),
    ];

    // 注入 instance.json 中存储的 JVM 参数（模板变量替换 + 规则过滤）
    if let Some(jvm_args) = json["arguments"]["jvm"].as_array() {
        let libs_dir_str = libs_dir.to_string_lossy().to_string();
        let natives_dir_str = natives_dir.to_string_lossy().to_string();

        // 模板变量替换（参考 XMCL 的实现）
        let replace_vars = |s: &str| -> String {
            s.replace("${natives_directory}", &natives_dir_str)
             .replace("${library_directory}", &libs_dir_str)
             .replace("${launcher_name}", "oaoi")
             .replace("${launcher_version}", "1.0")
             .replace("${classpath}", &classpath.join(";"))
             .replace("${version_name}", version_name)
        };

        for arg in jvm_args {
            if let Some(s) = arg.as_str() {
                // 纯字符串参数：替换模板变量
                let resolved = replace_vars(s);
                // 跳过 -cp 和 ${classpath}（我们自己加）
                if resolved == "-cp" || resolved == classpath.join(";") {
                    continue;
                }
                args.push(resolved);
            } else if arg.is_object() {
                // 规则对象参数：检查 OS 规则
                let rules = arg["rules"].as_array();
                let mut allowed = false;
                if let Some(rules) = rules {
                    for rule in rules {
                        let action = rule["action"].as_str().unwrap_or("");
                        let os_name = rule["os"]["name"].as_str();
                        let os_arch = rule["os"]["arch"].as_str();
                        match action {
                            "allow" => {
                                match (os_name, os_arch) {
                                    (None, None) => allowed = true,
                                    (Some("windows"), _) => allowed = true,
                                    (None, Some("x86")) => {}, // 跳过 32 位
                                    _ => {}
                                }
                            }
                            "disallow" => {
                                if os_name == Some("windows") || os_name.is_none() {
                                    allowed = false;
                                }
                            }
                            _ => {}
                        }
                    }
                }
                if allowed {
                    // 提取值
                    if let Some(val) = arg["value"].as_str() {
                        args.push(replace_vars(val));
                    } else if let Some(vals) = arg["value"].as_array() {
                        for v in vals {
                            if let Some(s) = v.as_str() {
                                let resolved = replace_vars(s);
                                if resolved == "-cp" || resolved == classpath.join(";") {
                                    continue;
                                }
                                args.push(resolved);
                            }
                        }
                    }
                }
            }
        }
    }

    // 确保 Forge 必需的 -DlibraryDirectory（如果 JVM args 里没有就手动加）
    let loader_type = json["loader"]["type"].as_str().unwrap_or("");
    if loader_type == "forge" && !args.iter().any(|a| a.starts_with("-DlibraryDirectory")) {
        args.push(format!("-DlibraryDirectory={}", libs_dir.to_string_lossy()));
    }

    args.extend([
        "-cp".to_string(),
        classpath.join(";"),
        main_class.to_string(),
        "--username".to_string(), options.player_name.clone(),
        "--version".to_string(), version_name.clone(),
        "--gameDir".to_string(), ver_dir.to_string_lossy().to_string(),
        "--assetsDir".to_string(), game_dir.join("res").to_string_lossy().to_string(),
        "--assetIndex".to_string(), asset_index.to_string(),
        "--uuid".to_string(), options.uuid.clone().unwrap_or(uuid),
        "--accessToken".to_string(), options.access_token.clone().unwrap_or("0".to_string()),
        "--userType".to_string(), if options.access_token.is_some() { "msa".to_string() } else { "legacy".to_string() },
        "--versionType".to_string(), "release".to_string(),
    ]);

    // 注入 instance.json 中额外的 game 参数（Forge 的 --launchTarget 等）
    if let Some(game_args) = json["arguments"]["game"].as_array() {
        for arg in game_args {
            if let Some(s) = arg.as_str() {
                // 跳过含 ${} 占位符的参数（已在上面硬编码）和已手动设置的参数
                if !s.contains("${") && !s.starts_with("--username") && !s.starts_with("--version")
                    && !s.starts_with("--gameDir") && !s.starts_with("--assetsDir")
                    && !s.starts_with("--assetIndex") && !s.starts_with("--uuid")
                    && !s.starts_with("--accessToken") && !s.starts_with("--userType")
                    && !s.starts_with("--versionType") {
                    args.push(s.to_string());
                }
            }
        }
    } else if let Some(mc_args_str) = json["minecraftArguments"].as_str() {
        // 旧版 Minecraft/Forge 使用 minecraftArguments（空格分隔字符串）
        let replaced = mc_args_str
            .replace("${auth_player_name}", &options.player_name)
            .replace("${version_name}", version_name)
            .replace("${game_directory}", &ver_dir.to_string_lossy())
            .replace("${assets_root}", &game_dir.join("res").to_string_lossy())
            .replace("${assets_index_name}", asset_index)
            .replace("${auth_uuid}", options.uuid.as_deref().unwrap_or(&uuid))
            .replace("${auth_access_token}", options.access_token.as_deref().unwrap_or("0"))
            .replace("${user_type}", if options.access_token.is_some() { "msa" } else { "legacy" })
            .replace("${version_type}", "release")
            .replace("${user_properties}", "{}");
        for part in replaced.split_whitespace() {
            args.push(part.to_string());
        }
    }

    // 自动进服
    if let Some(ip) = &options.server_ip {
        if !ip.is_empty() {
            args.push("--server".to_string());
            args.push(ip.clone());
            args.push("--port".to_string());
            args.push(options.server_port.unwrap_or(25565).to_string());
        }
    }

    // 确保 mods 文件夹存在（Forge 需要）
    let mods_dir = ver_dir.join("mods");
    let _ = std::fs::create_dir_all(&mods_dir);

    // 调试日志：打印完整启动命令
    eprintln!("\n[launch] ===== 启动命令 =====");
    eprintln!("[launch] Java: {}", options.java_path);
    eprintln!("[launch] MainClass: {}", main_class);
    eprintln!("[launch] Classpath entries: {}", classpath.len());
    for (i, arg) in args.iter().enumerate() {
        // 隐藏 accessToken 防止凭证泄露
        if i > 0 && args.get(i - 1).map(|s| s.as_str()) == Some("--accessToken") && arg != "0" {
            eprintln!("[launch] arg[{}]: *****(已隐藏)", i);
        } else if arg.len() > 200 {
            eprintln!("[launch] arg[{}]: {}... (truncated)", i, &arg[..200]);
        } else {
            eprintln!("[launch] arg[{}]: {}", i, arg);
        }
    }
    eprintln!("[launch] ===== END =====\n");

    // 启动游戏
    let child = Command::new(&options.java_path)
        .args(&args)
        .current_dir(game_dir)
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .map_err(|e| format!("启动游戏失败: {}", e))?;

    Ok(format!("游戏已启动 (PID: {}), 版本: {}, 库: {}/{}", child.id(), version_name, classpath.len(), total_libs))
}

// ============ 微软正版登录（Authorization Code + Loopback） ============
const MS_CLIENT_ID: &str = "b6affacf-765f-41e6-87ee-6fb373cdb2b5";

#[derive(Serialize, Clone)]
struct McProfile {
    name: String,
    uuid: String,
    access_token: String,
}

#[tauri::command]
fn start_ms_login() -> Result<McProfile, String> {
    let handle = std::thread::spawn(move || -> Result<McProfile, String> {
        // 1. 动态分配端口（避免端口冲突）
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

// ============ 版本安装 ============

/// 将 Mojang 官方 URL 替换为 BMCLAPI 国内镜像（和 HMCL 做法一致）
fn mirror_url(url: &str) -> String {
    url.replace("https://piston-meta.mojang.com", "https://bmclapi2.bangbang93.com")
       .replace("https://piston-data.mojang.com", "https://bmclapi2.bangbang93.com")
       .replace("https://launchermeta.mojang.com", "https://bmclapi2.bangbang93.com")
       .replace("https://launcher.mojang.com", "https://bmclapi2.bangbang93.com")
       .replace("https://libraries.minecraft.net", "https://bmclapi2.bangbang93.com/maven")
       .replace("https://resources.download.minecraft.net", "https://bmclapi2.bangbang93.com/assets")
}

/// 下载文件，如果已存在且 sha1 匹配则跳过
/// 自带 3 次重试 + 指数退避，防止单次网络抖动导致整体卡住
fn download_file_if_needed(http: &reqwest::blocking::Client, url: &str, dest: &std::path::Path, expected_sha1: Option<&str>) -> Result<bool, String> {
    if dest.exists() {
        if let Some(sha1) = expected_sha1 {
            if let Ok(data) = std::fs::read(dest) {
                let hash = sha1_smol::Sha1::from(&data).digest().to_string();
                if hash == sha1 {
                    return Ok(false); // 已存在，跳过
                }
            }
        } else {
            return Ok(false);
        }
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let real_url = mirror_url(url);
    let max_retries = 3;
    let mut last_err = String::new();
    for attempt in 0..max_retries {
        if attempt > 0 {
            // 指数退避: 1s, 2s
            std::thread::sleep(std::time::Duration::from_secs(1 << (attempt - 1)));
        }
        match do_download(http, &real_url, dest) {
            Ok(()) => return Ok(true),
            Err(e) => {
                last_err = e;
                eprintln!("[download] 重试 {}/{}: {} ({})", attempt + 1, max_retries, last_err, real_url);
            }
        }
    }
    Err(format!("下载失败({}次重试后): {} ({})", max_retries, last_err, real_url))
}

/// 实际执行单次下载
fn do_download(http: &reqwest::blocking::Client, url: &str, dest: &std::path::Path) -> Result<(), String> {
    let resp = http.get(url).send().map_err(|e| format!("请求失败: {}", e))?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    let bytes = resp.bytes().map_err(|e| format!("读取失败: {}", e))?;
    std::fs::write(dest, &bytes).map_err(|e| format!("写入失败: {}", e))?;
    Ok(())
}

/// 限制并发的下载执行器 — 最多 max_workers 个线程同时下载
fn parallel_download(
    http: &reqwest::blocking::Client,
    tasks: Vec<(String, std::path::PathBuf, Option<String>)>,
    done: &std::sync::Arc<std::sync::atomic::AtomicUsize>,
    max_workers: usize,
) {
    for chunk in tasks.chunks(max_workers) {
        let handles: Vec<_> = chunk.iter().map(|(url, dest, sha1)| {
            let url = url.clone();
            let dest = dest.clone();
            let sha1 = sha1.clone();
            let done = done.clone();
            let h = http.clone();
            std::thread::spawn(move || {
                if let Err(e) = download_file_if_needed(&h, &url, &dest, sha1.as_deref()) {
                    eprintln!("[download] 失败: {} -> {}", url, e);
                }
                done.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            })
        }).collect();
        for h in handles { let _ = h.join(); }
    }
}

/// 检查 library 的 rules 是否允许当前 OS
fn library_allowed(rules: &Option<Vec<serde_json::Value>>) -> bool {
    let rules = match rules {
        Some(r) => r,
        None => return true, // 没有 rules = 所有平台
    };
    let mut dominated_match = false;
    for rule in rules {
        let action = rule.get("action").and_then(|v| v.as_str()).unwrap_or("");
        let os_name = rule.get("os").and_then(|o| o.get("name")).and_then(|v| v.as_str());
        match (action, os_name) {
            ("allow", Some("windows")) => return true,
            ("allow", None) => dominated_match = true,
            ("disallow", Some("windows")) => return false,
            _ => {}
        }
    }
    dominated_match
}

#[derive(Serialize, Clone)]
struct InstallProgress {
    stage: String,
    current: usize,
    total: usize,
    detail: String,
}

fn resolve_game_dir(game_dir: &str) -> std::path::PathBuf {
    if !game_dir.is_empty() {
        std::path::PathBuf::from(game_dir)
    } else {
        let home = std::env::var("APPDATA").unwrap_or_else(|_| ".".to_string());
        std::path::Path::new(&home).join(".oaoi").join("oaoi")
    }
}

#[derive(Serialize, Clone)]
struct InstanceInfo {
    name: String,
    mc_version: String,
    loader_type: String,
    loader_version: String,
}

#[tauri::command]
fn list_installed_versions(game_dir: String) -> Result<Vec<InstanceInfo>, String> {
    let dir = resolve_game_dir(&game_dir);
    let instances_path = dir.join("instances");
    if !instances_path.exists() {
        return Ok(vec![]);
    }
    let mut list = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&instances_path) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() && p.join("instance.json").exists() {
                if let Ok(s) = std::fs::read_to_string(p.join("instance.json")) {
                    if let Ok(j) = serde_json::from_str::<serde_json::Value>(&s) {
                        list.push(InstanceInfo {
                            name: j["name"].as_str().unwrap_or("").to_string(),
                            mc_version: j["mcVersion"].as_str().unwrap_or("").to_string(),
                            loader_type: j["loader"]["type"].as_str().unwrap_or("vanilla").to_string(),
                            loader_version: j["loader"]["version"].as_str().unwrap_or("").to_string(),
                        });
                    }
                }
            }
        }
    }
    list.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(list)
}

#[tauri::command]
fn delete_version(game_dir: String, version_id: String) -> Result<String, String> {
    let dir = resolve_game_dir(&game_dir);
    let inst_path = dir.join("instances").join(&version_id);
    if !inst_path.exists() {
        return Err(format!("实例 {} 不存在", version_id));
    }
    std::fs::remove_dir_all(&inst_path).map_err(|e| format!("删除失败: {}", e))?;
    Ok(format!("已删除实例 {}", version_id))
}

#[tauri::command]
fn get_fabric_versions(mc_version: String) -> Result<Vec<String>, String> {
    let url = format!("https://meta.fabricmc.net/v2/versions/loader/{}", mc_version);
    let http = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build().map_err(|e| e.to_string())?;
    
    let resp = http.get(&url).send().map_err(|e| format!("获取 Fabric 版本失败: {}", e))?;
    let json: serde_json::Value = resp.json().map_err(|e| e.to_string())?;
    
    let arr = json.as_array().ok_or("格式错误")?;
    let mut versions = Vec::new();
    for v in arr {
        if let Some(loader) = v.get("loader") {
            if let Some(ver) = loader.get("version").and_then(|v| v.as_str()) {
                versions.push(ver.to_string());
            }
        }
    }
    Ok(versions)
}

#[tauri::command]
fn get_forge_versions(mc_version: String) -> Result<Vec<String>, String> {
    let url = format!("https://bmclapi2.bangbang93.com/forge/minecraft/{}", mc_version);
    let http = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build().map_err(|e| e.to_string())?;
    
    let resp = http.get(&url).send().map_err(|e| format!("获取 Forge 版本失败: {}", e))?;
    let json: serde_json::Value = resp.json().map_err(|e| e.to_string())?;
    
    let arr = json.as_array().ok_or("格式错误")?;
    let mut versions = Vec::new();
    for v in arr {
        if let Some(ver) = v.get("version").and_then(|v| v.as_str()) {
            versions.push(ver.to_string());
        }
    }
    // BMCL API 返回的往往是时间倒序的，这里做个简单反转确保新版本在前
    versions.reverse();
    Ok(versions)
}

#[tauri::command]
fn create_instance(
    app_handle: tauri::AppHandle, 
    name: String, 
    mc_version: String, 
    meta_url: String, 
    game_dir: String, 
    loader_type: String, 
    loader_version: String,
    java_path: String
) -> Result<String, String> {
    let name_clone = name.clone();
    std::thread::spawn(move || {
        eprintln!("[install] 开始创建实例: {} (mc={}, loader={} {}, java={})", name, mc_version, loader_type, loader_version, java_path);
        if let Err(e) = do_create_instance(&app_handle, &name, &mc_version, &meta_url, &game_dir, &loader_type, &loader_version, &java_path) {
            eprintln!("[install] 错误: {}", e);
            let _ = app_handle.emit("install-progress", InstallProgress {
                stage: "error".to_string(), current: 0, total: 0, detail: e,
            });
        }
    });
    Ok(format!("开始创建实例: {}", name_clone))
}

fn do_create_instance(
    app_handle: &tauri::AppHandle,
    name: &str,
    mc_version: &str,
    meta_url: &str,
    game_dir_input: &str,
    loader_type: &str,
    loader_version: &str,
    java_path: &str
) -> Result<String, String> {
    // 路径安全校验：禁止 ..、斜杠、Windows 保留字符
    if name.is_empty()
        || name.contains("..")
        || name.contains('/')
        || name.contains('\\')
        || name.contains(':')
        || name.contains('*')
        || name.contains('?')
        || name.contains('"')
        || name.contains('<')
        || name.contains('>')
        || name.contains('|')
    {
        return Err(format!("实例名 '{}' 包含非法字符", name));
    }
    let game_dir = resolve_game_dir(game_dir_input);

    let emit = |stage: &str, current: usize, total: usize, detail: &str| {
        let _ = app_handle.emit("install-progress", InstallProgress {
            stage: stage.to_string(), current, total, detail: detail.to_string(),
        });
    };

    // 实例目录: {game_dir}/instances/{name}/
    let inst_dir = game_dir.join("instances").join(name);
    if inst_dir.exists() {
        return Err(format!("实例 '{}' 已存在，请换一个名称！", name));
    }
    std::fs::create_dir_all(&inst_dir).map_err(|e| e.to_string())?;
    let inst_json_path = inst_dir.join("instance.json");

    // HTTP 客户端 — 短超时 + 有限连接池，防止卡死
    let http = reqwest::blocking::Client::builder()
        .pool_max_idle_per_host(16)
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(30))
        .build().map_err(|e| e.to_string())?;

    // 1. 下载版本 JSON
    emit("meta", 0, 1, &format!("下载 {} 元数据...", mc_version));
    let resp = http.get(meta_url).send().map_err(|e| format!("获取版本信息失败: {}\n(请检查网络或代理)", e))?;
    let mut ver_json: serde_json::Value = resp.json().map_err(|e| format!("解析版本信息失败: {}", e))?;
    emit("meta", 1, 1, "元数据下载完成");

    // 2. 下载 client.jar
    emit("client", 0, 1, "下载 client.jar...");
    let client_info = ver_json.get("downloads")
        .and_then(|d| d.get("client"))
        .ok_or("版本 JSON 缺少 downloads.client")?;
    let client_url = client_info.get("url").and_then(|v| v.as_str()).ok_or("缺少 client url")?;
    let client_sha1 = client_info.get("sha1").and_then(|v| v.as_str());
    let jar_path = inst_dir.join("client.jar");
    download_file_if_needed(&http, client_url, &jar_path, client_sha1)
        .map_err(|e| format!("下载 client.jar 失败: {}", e))?;
    emit("client", 1, 1, "client.jar 完成");

    // 3. 下载 libraries（并发放到 libs 共享目录）
    let libs = ver_json.get("libraries").and_then(|v| v.as_array());
    if let Some(libs) = libs {
        let mut tasks: Vec<(String, std::path::PathBuf, Option<String>)> = Vec::new();
        for lib in libs.iter() {
            let rules = lib.get("rules").map(|v| v.as_array().cloned().unwrap_or_default());
            if !library_allowed(&rules) { continue; }
            if let Some(artifact) = lib.get("downloads").and_then(|d| d.get("artifact")) {
                let path = artifact.get("path").and_then(|v| v.as_str()).unwrap_or("");
                let url = artifact.get("url").and_then(|v| v.as_str()).unwrap_or("");
                let sha1 = artifact.get("sha1").and_then(|v| v.as_str());
                if !path.is_empty() && !url.is_empty() {
                    let dest = game_dir.join("libs").join(path);
                    tasks.push((url.to_string(), dest, sha1.map(|s| s.to_string())));
                }
            }
        }
        let total = tasks.len();
        emit("libraries", 0, total, &format!("下载 {} 个依赖库...", total));
        let done = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        // 进度报告线程
        let app_clone = app_handle.clone();
        let done_reporter = done.clone();
        let total_copy = total;
        let reporter = std::thread::spawn(move || {
            loop {
                let finished = done_reporter.load(std::sync::atomic::Ordering::Relaxed);
                let _ = app_clone.emit("install-progress", InstallProgress {
                    stage: "libraries".to_string(), current: finished, total: total_copy,
                    detail: format!("依赖库 {}/{}", finished, total_copy),
                });
                if finished >= total_copy { break; }
                std::thread::sleep(std::time::Duration::from_millis(300));
            }
        });
        // 限制并发为 8 线程
        parallel_download(&http, tasks, &done, 64);
        let _ = reporter.join();
        emit("libraries", total, total, "依赖库下载完成");
    }

    // 4. 下载 assets（共享 res/目录）
    if let Some(asset_index) = ver_json.get("assetIndex") {
        let index_url = asset_index.get("url").and_then(|v| v.as_str()).unwrap_or("");
        let index_id = asset_index.get("id").and_then(|v| v.as_str()).unwrap_or("unknown");
        let index_sha1 = asset_index.get("sha1").and_then(|v| v.as_str());

        let index_path = game_dir.join("res").join("indexes").join(format!("{}.json", index_id));
        emit("assets", 0, 1, "下载资源索引...");
        download_file_if_needed(&http, index_url, &index_path, index_sha1)?;

        if let Ok(index_content) = std::fs::read_to_string(&index_path) {
            if let Ok(index_json) = serde_json::from_str::<serde_json::Value>(&index_content) {
                if let Some(objects) = index_json.get("objects").and_then(|v| v.as_object()) {
                    let mut asset_tasks: Vec<(String, std::path::PathBuf, String)> = Vec::new();
                    for (_name, info) in objects.iter() {
                        let hash = info.get("hash").and_then(|v| v.as_str()).unwrap_or("");
                        if hash.len() < 2 { continue; }
                        let prefix = &hash[..2];
                        let dest = game_dir.join("res").join("objects").join(prefix).join(hash);
                        let url = format!("https://resources.download.minecraft.net/{}/{}", prefix, hash);
                        asset_tasks.push((url, dest, hash.to_string()));
                    }
                    let total = asset_tasks.len();
                    emit("assets", 0, total, &format!("下载 {} 个资源...", total));

                    let done = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
                    let app_clone = app_handle.clone();
                    let done_reporter = done.clone();
                    let total_copy = total;
                    
                    let reporter = std::thread::spawn(move || {
                        loop {
                            let finished = done_reporter.load(std::sync::atomic::Ordering::Relaxed);
                            let _ = app_clone.emit("install-progress", InstallProgress {
                                stage: "assets".to_string(), current: finished, total: total_copy,
                                detail: format!("资源 {}/{}", finished, total_copy),
                            });
                            if finished >= total_copy { break; }
                            std::thread::sleep(std::time::Duration::from_millis(300));
                        }
                    });

                    // 限制并发为 8 线程
                    let asset_dl_tasks: Vec<(String, std::path::PathBuf, Option<String>)> = asset_tasks
                        .into_iter()
                        .map(|(url, dest, hash)| (url, dest, Some(hash)))
                        .collect();
                    parallel_download(&http, asset_dl_tasks, &done, 64);
                    let _ = reporter.join();
                    emit("assets", total, total, "资源下载完成");
                }
            }
        }
    }

    // 设置基础实例信息
    ver_json["name"] = serde_json::Value::String(name.to_string());
    ver_json["mcVersion"] = serde_json::Value::String(mc_version.to_string());
    
    // 默认 Vanilla
    if ver_json["mainClass"].is_null() {
        ver_json["mainClass"] = serde_json::Value::String("net.minecraft.client.main.Main".to_string());
    }
    ver_json["loader"] = serde_json::json!({
        "type": "vanilla",
        "version": ""
    });

    // 5. 处理 Mod Loader
    if loader_type == "fabric" && !loader_version.is_empty() {
        emit("fabric", 0, 1, &format!("处理 Fabric Loader {}...", loader_version));

        let profile_url = format!(
            "https://meta.fabricmc.net/v2/versions/loader/{}/{}/profile/json",
            mc_version, loader_version
        );
        let profile_resp = http.get(&profile_url).send()
            .map_err(|e| format!("获取 Fabric 配置失败: {}", e))?;
        let fabric_profile: serde_json::Value = profile_resp.json()
            .map_err(|e| format!("解析 Fabric 配置失败: {}", e))?;

        // 下载 Fabric 库
        if let Some(fabric_libs) = fabric_profile["libraries"].as_array() {
            let mut fabric_tasks: Vec<(String, std::path::PathBuf, Option<String>)> = Vec::new();

            for lib in fabric_libs {
                let name = lib["name"].as_str().unwrap_or("");
                let maven_url = lib["url"].as_str().unwrap_or("https://maven.fabricmc.net/");
                let sha1 = lib["sha1"].as_str();

                if name.is_empty() { continue; }
                let parts: Vec<&str> = name.split(':').collect();
                if parts.len() < 3 { continue; }
                let group_path = parts[0].replace('.', "/");
                let artifact = parts[1];
                let version = parts[2];
                let jar_name = if parts.len() >= 4 {
                    format!("{}-{}-{}.jar", artifact, version, parts[3])
                } else {
                    format!("{}-{}.jar", artifact, version)
                };
                let relative_path = format!("{}/{}/{}/{}", group_path, artifact, version, jar_name);
                let url = format!("{}{}", maven_url.trim_end_matches('/'), format!("/{}", relative_path));
                let dest = game_dir.join("libs").join(&relative_path);

                fabric_tasks.push((url, dest, sha1.map(|s| s.to_string())));
            }

            let total = fabric_tasks.len();
            emit("fabric-libs", 0, total, &format!("下载 {} 个 Fabric 组件...", total));

            let done = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let handles: Vec<_> = fabric_tasks.into_iter().map(|(url, dest, sha1)| {
                let done = done.clone();
                let h = http.clone();
                std::thread::spawn(move || {
                    let _ = download_file_if_needed(&h, &url, &dest, sha1.as_deref());
                    done.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                })
            }).collect();

            loop {
                let finished = done.load(std::sync::atomic::Ordering::Relaxed);
                emit("fabric-libs", finished, total, &format!("Fabric 组件 {}/{}", finished, total));
                if finished >= total { break; }
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            for h in handles { let _ = h.join(); }
            emit("fabric-libs", total, total, "Fabric 组件下载完成");
        }

        // 合并 Fabric 配置
        if let Some(main_class) = fabric_profile["mainClass"].as_str() {
            ver_json["mainClass"] = serde_json::Value::String(main_class.to_string());
        }
        // 合并库：Fabric 库覆盖同 group:artifact 的 vanilla 库（和 ATLauncher/Prism 做法一致）
        if let Some(fabric_libs) = fabric_profile["libraries"].as_array() {
            if let Some(existing_libs) = ver_json["libraries"].as_array_mut() {
                for fabric_lib in fabric_libs {
                    let fabric_name = fabric_lib["name"].as_str().unwrap_or("");
                    let fabric_parts: Vec<&str> = fabric_name.split(':').collect();
                    let fabric_key = if fabric_parts.len() >= 4 {
                        format!("{}:{}:{}", fabric_parts[0], fabric_parts[1], fabric_parts[3])
                    } else if fabric_parts.len() >= 2 {
                        format!("{}:{}", fabric_parts[0], fabric_parts[1])
                    } else { String::new() };

                    // 移除同 group:artifact 的旧版本
                    if !fabric_key.is_empty() {
                        existing_libs.retain(|existing| {
                            let name = existing["name"].as_str().unwrap_or("");
                            let parts: Vec<&str> = name.split(':').collect();
                            if parts.len() >= 2 {
                                let key = if parts.len() >= 4 {
                                    format!("{}:{}:{}", parts[0], parts[1], parts[3])
                                } else {
                                    format!("{}:{}", parts[0], parts[1])
                                };
                                key != fabric_key
                            } else { true }
                        });
                    }
                    existing_libs.push(fabric_lib.clone());
                }
            }
        }
        let fabric_arguments = &fabric_profile["arguments"];
        if let Some(jvm_args) = fabric_arguments["jvm"].as_array() {
            if !jvm_args.is_empty() {
                if ver_json["arguments"].is_null() {
                    ver_json["arguments"] = serde_json::json!({"jvm": jvm_args, "game": []});
                } else if let Some(existing_jvm) = ver_json["arguments"]["jvm"].as_array_mut() {
                    for arg in jvm_args {
                        existing_jvm.push(arg.clone());
                    }
                }
            }
        }

        ver_json["loader"] = serde_json::json!({
            "type": "fabric",
            "version": loader_version
        });

        // 自动下载 Fabric API 到 mods/ 目录
        emit("fabric-api", 0, 1, "下载 Fabric API...");
        let mods_dir = inst_dir.join("mods");
        std::fs::create_dir_all(&mods_dir).ok();
        let api_url = format!(
            "https://api.modrinth.com/v2/project/P7dR8mSH/version?loaders=[\"fabric\"]&game_versions=[\"{}\"]",
            mc_version
        );
        match http.get(&api_url).send() {
            Ok(resp) => {
                if let Ok(versions) = resp.json::<serde_json::Value>() {
                    if let Some(arr) = versions.as_array() {
                        if let Some(first) = arr.first() {
                            if let Some(files) = first["files"].as_array() {
                                if let Some(file) = files.first() {
                                    let dl_url = file["url"].as_str().unwrap_or("");
                                    let filename = file["filename"].as_str().unwrap_or("fabric-api.jar");
                                    if !dl_url.is_empty() {
                                        let dest = mods_dir.join(filename);
                                        let _ = download_file_if_needed(&http, dl_url, &dest, None);
                                        emit("fabric-api", 1, 1, &format!("Fabric API {} 已下载", filename));
                                    }
                                }
                            }
                        }
                    }
                }
            },
            Err(e) => eprintln!("[install] Fabric API 下载失败(非致命): {}", e),
        }
    } else if loader_type == "forge" && !loader_version.is_empty() {
        if java_path.is_empty() {
            return Err("必须先在设置中配置 Java 路径才能安装 Forge".to_string());
        }

        emit("forge", 0, 1, &format!("处理 Forge {}...", loader_version));

        // 1. 下载 forge-installer.jar
        let forge_full_ver = format!("{}-{}", mc_version, loader_version);
        let installer_url = format!(
            "https://bmclapi2.bangbang93.com/maven/net/minecraftforge/forge/{0}/forge-{0}-installer.jar",
            forge_full_ver
        );
        let installer_path = inst_dir.join("forge-installer.jar");
        
        emit("forge", 0, 100, "下载 Forge 安装器...");
        download_file_if_needed(&http, &installer_url, &installer_path, None)
            .map_err(|e| format!("下载 Forge 安装器失败: {}", e))?;

        // 2. 创建临时 .minecraft 目录结构供安装器使用
        //    Forge 安装器需要标准 .minecraft 布局：launcher_profiles.json + versions/
        let temp_mc = inst_dir.join(".forge_temp");
        let _ = std::fs::create_dir_all(&temp_mc);
        std::fs::write(temp_mc.join("launcher_profiles.json"), r#"{"profiles":{}}"#)
            .map_err(|e| format!("创建 launcher_profiles.json 失败: {}", e))?;

        // 复制已有的 client.jar 到临时目录（避免安装器重新从 Mojang 下载）
        let temp_ver_dir = temp_mc.join("versions").join(mc_version);
        let _ = std::fs::create_dir_all(&temp_ver_dir);
        let existing_client_jar = inst_dir.join("client.jar");
        if existing_client_jar.exists() {
            let _ = std::fs::copy(&existing_client_jar, temp_ver_dir.join(format!("{}.jar", mc_version)));
            eprintln!("[forge] 已复制 client.jar 到临时目录，避免重新下载");
        }

        // 3. 运行 Forge 安装器（headless 模式）
        emit("forge", 30, 100, "运行 Forge 安装器 (这可能需要几分钟)...");
        eprintln!("[forge] Running installer: {} -jar {} --installClient {}",
            java_path, installer_path.display(), temp_mc.display());
        let status = std::process::Command::new(java_path)
            .args(["-jar", installer_path.to_str().unwrap(), "--installClient", temp_mc.to_str().unwrap()])
            .current_dir(&inst_dir)
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit())
            .status()
            .map_err(|e| format!("启动 Forge 安装器失败: {}", e))?;

        if !status.success() {
            let _ = std::fs::remove_dir_all(&temp_mc);
            return Err("Forge 安装器执行失败或被用户取消".to_string());
        }

        // 4. 从临时目录复制生成的库到我们的 libs/ 目录
        emit("forge", 70, 100, "复制 Forge 组件...");
        let temp_libs = temp_mc.join("libraries");
        if temp_libs.exists() {
            fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
                if !dst.exists() { std::fs::create_dir_all(dst)?; }
                for entry in std::fs::read_dir(src)? {
                    let entry = entry?;
                    let dest_path = dst.join(entry.file_name());
                    if entry.file_type()?.is_dir() {
                        copy_dir_recursive(&entry.path(), &dest_path)?;
                    } else {
                        std::fs::copy(entry.path(), &dest_path)?;
                    }
                }
                Ok(())
            }
            copy_dir_recursive(&temp_libs, &game_dir.join("libs"))
                .map_err(|e| format!("复制 Forge 库失败: {}", e))?;
        }

        // 5. 从安装器 JAR 中提取 version.json 合并到实例配置
        emit("forge", 85, 100, "解析 Forge 配置...");
        let installer_file = std::fs::File::open(&installer_path)
            .map_err(|e| format!("打开 Forge 安装器失败: {}", e))?;
        let mut archive = zip::ZipArchive::new(installer_file)
            .map_err(|e| format!("解析 Forge 安装器 ZIP 失败: {}", e))?;

        let forge_version_json: Option<serde_json::Value> = archive.by_name("version.json").ok()
            .and_then(|mut f| {
                let mut s = String::new();
                use std::io::Read;
                f.read_to_string(&mut s).ok()?;
                serde_json::from_str(&s).ok()
            });

        if let Some(parsed_forge) = forge_version_json {
            // 合并 mainClass
            if let Some(main_class) = parsed_forge["mainClass"].as_str() {
                ver_json["mainClass"] = serde_json::Value::String(main_class.to_string());
            }

            // 合并库（去重）
            if let Some(forge_libs) = parsed_forge["libraries"].as_array() {
                if let Some(existing_libs) = ver_json["libraries"].as_array_mut() {
                    for forge_lib in forge_libs {
                        let forge_name = forge_lib["name"].as_str().unwrap_or("");
                        let forge_parts: Vec<&str> = forge_name.split(':').collect();
                        let forge_key = if forge_parts.len() >= 4 {
                            format!("{}:{}:{}", forge_parts[0], forge_parts[1], forge_parts[3])
                        } else if forge_parts.len() >= 2 {
                            format!("{}:{}", forge_parts[0], forge_parts[1])
                        } else { String::new() };

                        if !forge_key.is_empty() {
                            existing_libs.retain(|existing| {
                                let name = existing["name"].as_str().unwrap_or("");
                                let parts: Vec<&str> = name.split(':').collect();
                                if parts.len() >= 2 {
                                    let key = if parts.len() >= 4 {
                                        format!("{}:{}:{}", parts[0], parts[1], parts[3])
                                    } else {
                                        format!("{}:{}", parts[0], parts[1])
                                    };
                                    key != forge_key
                                } else { true }
                            });
                        }
                        existing_libs.push(forge_lib.clone());
                    }
                }
            }

            // 合并 arguments
            let forge_args = &parsed_forge["arguments"];
            if !forge_args.is_null() {
                let mut base_jvm = vec![];
                let mut base_game = vec![];

                if let Some(existing) = ver_json["arguments"]["jvm"].as_array() {
                    base_jvm.extend(existing.clone());
                }
                if let Some(existing) = ver_json["arguments"]["game"].as_array() {
                    base_game.extend(existing.clone());
                }

                if let Some(f_jvm) = forge_args["jvm"].as_array() {
                    base_jvm.extend(f_jvm.clone());
                }
                if let Some(f_game) = forge_args["game"].as_array() {
                    base_game.extend(f_game.clone());
                }

                if !base_jvm.is_empty() || !base_game.is_empty() {
                    ver_json["arguments"] = serde_json::json!({
                        "jvm": base_jvm,
                        "game": base_game
                    });
                }
            } else if let Some(minecraft_args) = parsed_forge["minecraftArguments"].as_str() {
                ver_json["minecraftArguments"] = serde_json::Value::String(minecraft_args.to_string());
            }

            ver_json["loader"] = serde_json::json!({
                "type": "forge",
                "version": loader_version
            });
            emit("forge", 100, 100, "Forge 配置解析完成");
        } else {
            return Err("Forge 安装器中未找到 version.json".to_string());
        }

        // 清理
        let _ = std::fs::remove_dir_all(&temp_mc);
        let _ = std::fs::remove_file(installer_path);
    }

    // 写回最终配置到 instance.json
    std::fs::write(&inst_json_path, serde_json::to_string_pretty(&ver_json).unwrap())
        .map_err(|e| format!("保存实例配置失败: {}", e))?;

    emit("done", 1, 1, &format!("实例 '{}' 创建完成！", name));
    Ok(format!("实例 {} 创建成功", name))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
  tauri::Builder::default()
    .plugin(tauri_plugin_dialog::init())
    .invoke_handler(tauri::generate_handler![get_system_memory, find_java, init_game_dir, launch_minecraft, start_ms_login, create_instance, get_fabric_versions, get_forge_versions, list_installed_versions, delete_version])
    .setup(|app| {
      let window = app.get_webview_window("main").unwrap();
      let _ = window.set_background_color(Some(Color(0, 0, 0, 0)));
      if cfg!(debug_assertions) {
        app.handle().plugin(
          tauri_plugin_log::Builder::default()
            .level(log::LevelFilter::Info)
            .build(),
        )?;
      }
      Ok(())
    })
    .run(tauri::generate_context!())
    .expect("error while running tauri application");
}
