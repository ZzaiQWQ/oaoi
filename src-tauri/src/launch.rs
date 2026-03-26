use crate::instance::resolve_game_dir;
#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[derive(serde::Deserialize)]
pub struct LaunchOptions {
    pub java_path: String,
    pub game_dir: String,
    pub version_name: String,
    pub player_name: String,
    pub memory_mb: u32,
    pub server_ip: Option<String>,
    pub server_port: Option<u16>,
    pub access_token: Option<String>,
    pub uuid: Option<String>,
}

#[tauri::command]
pub fn launch_minecraft(options: LaunchOptions) -> Result<String, String> {
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

    // 构建 classpath（按 group:artifact 去重）
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
                let dedup_key = lib["name"].as_str()
                    .and_then(|n| {
                        let parts: Vec<&str> = n.split(':').collect();
                        if parts.len() >= 4 {
                            Some(format!("{}:{}:{}", parts[0], parts[1], parts[3]))
                        } else if parts.len() >= 2 {
                            Some(format!("{}:{}", parts[0], parts[1]))
                        } else { None }
                    })
                    .unwrap_or_default();

                if !dedup_key.is_empty() {
                    if let Some(&idx) = seen_keys.get(&dedup_key) {
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

    // 自动解压 natives（老版本需要 LWJGL native dll）
    let natives_empty = std::fs::read_dir(&natives_dir).map(|mut d| d.next().is_none()).unwrap_or(true);
    if natives_empty {
        if let Some(libs) = json["libraries"].as_array() {
            for lib in libs {
                // 只处理有 natives.windows 的库
                let classifier_key = match lib["natives"]["windows"].as_str() {
                    Some(k) => k.to_string(),
                    None => continue,
                };
                // 获取 natives jar 路径
                let native_jar_path = if let Some(cl) = lib["downloads"]["classifiers"][&classifier_key]["path"].as_str() {
                    libs_dir.join(cl.replace('/', "\\"))
                } else {
                    continue;
                };
                if !native_jar_path.exists() {
                    // 尝试下载
                    if let Some(url) = lib["downloads"]["classifiers"][&classifier_key]["url"].as_str() {
                        if let Some(parent) = native_jar_path.parent() {
                            let _ = std::fs::create_dir_all(parent);
                        }
                        eprintln!("[launch] 下载 native: {}", url);
                        if let Ok(resp) = reqwest::blocking::get(url) {
                            if let Ok(bytes) = resp.bytes() {
                                let _ = std::fs::write(&native_jar_path, &bytes);
                            }
                        }
                    }
                }
                if native_jar_path.exists() {
                    // 解压 dll 文件到 natives 目录
                    if let Ok(file) = std::fs::File::open(&native_jar_path) {
                        if let Ok(mut archive) = zip::ZipArchive::new(file) {
                            for i in 0..archive.len() {
                                if let Ok(mut entry) = archive.by_index(i) {
                                    let name = entry.name().to_string();
                                    if name.ends_with(".dll") || name.ends_with(".so") || name.ends_with(".dylib") {
                                        let filename = name.rsplit('/').next().unwrap_or(&name);
                                        let out_path = natives_dir.join(filename);
                                        if !out_path.exists() {
                                            if let Ok(mut out) = std::fs::File::create(&out_path) {
                                                let _ = std::io::copy(&mut entry, &mut out);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    eprintln!("[launch] 已解压 natives: {}", native_jar_path.display());
                }
            }
        }
    }

    // 根据系统语言自动设置 Minecraft 语言
    let options_file = ver_dir.join("options.txt");
    if !options_file.exists() {
        let sys_lang = std::process::Command::new("powershell")
            .args(["-NoProfile", "-c", "(Get-Culture).Name"])
            .creation_flags(0x08000000)
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

    // 注入 JVM 参数
    if let Some(jvm_args) = json["arguments"]["jvm"].as_array() {
        let libs_dir_str = libs_dir.to_string_lossy().to_string();
        let natives_dir_str = natives_dir.to_string_lossy().to_string();

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
                let resolved = replace_vars(s);
                if resolved == "-cp" || resolved == classpath.join(";") { continue; }
                args.push(resolved);
            } else if arg.is_object() {
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
                                    (None, Some("x86")) => {},
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
                    if let Some(val) = arg["value"].as_str() {
                        args.push(replace_vars(val));
                    } else if let Some(vals) = arg["value"].as_array() {
                        for v in vals {
                            if let Some(s) = v.as_str() {
                                let resolved = replace_vars(s);
                                if resolved == "-cp" || resolved == classpath.join(";") { continue; }
                                args.push(resolved);
                            }
                        }
                    }
                }
            }
        }
    }

    // Forge 必需的 -DlibraryDirectory
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
        "--uuid".to_string(), options.uuid.clone().unwrap_or(uuid.clone()),
        "--accessToken".to_string(), options.access_token.clone().unwrap_or("0".to_string()),
        "--userType".to_string(), if options.access_token.is_some() { "msa".to_string() } else { "legacy".to_string() },
        "--versionType".to_string(), "release".to_string(),
    ]);

    // 注入 game 参数
    if let Some(game_args) = json["arguments"]["game"].as_array() {
        for arg in game_args {
            if let Some(s) = arg.as_str() {
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

    // 确保 mods 文件夹存在
    let mods_dir = ver_dir.join("mods");
    let _ = std::fs::create_dir_all(&mods_dir);

    // 调试日志
    eprintln!("\n[launch] ===== 启动命令 =====");
    eprintln!("[launch] Java: {}", options.java_path);
    eprintln!("[launch] MainClass: {}", main_class);
    eprintln!("[launch] Classpath entries: {}", classpath.len());
    for (i, arg) in args.iter().enumerate() {
        if i > 0 && args.get(i - 1).map(|s| s.as_str()) == Some("--accessToken") && arg != "0" {
            eprintln!("[launch] arg[{}]: *****(已隐藏)", i);
        } else if arg.len() > 200 {
            eprintln!("[launch] arg[{}]: {}... (truncated)", i, &arg[..200]);
        } else {
            eprintln!("[launch] arg[{}]: {}", i, arg);
        }
    }
    eprintln!("[launch] ===== END =====\n");

    // 启动游戏（用 javaw.exe 代替 java.exe，无控制台窗口）
    let java_path = std::path::Path::new(&options.java_path);
    let javaw_path = java_path.with_file_name("javaw.exe");
    let launch_exe = if javaw_path.exists() {
        javaw_path.to_string_lossy().to_string()
    } else {
        options.java_path.clone()
    };
    let child = std::process::Command::new(&launch_exe)
        .args(&args)
        .current_dir(&ver_dir)
        .spawn()
        .map_err(|e| format!("启动游戏失败: {}", e))?;

    Ok(format!("游戏已启动 (PID: {}), 版本: {}, 库: {}/{}", child.id(), version_name, classpath.len(), total_libs))
}
