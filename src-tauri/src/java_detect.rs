use serde::Serialize;
use sysinfo::System;
use std::path::Path;
use std::process::Command;
use std::collections::HashSet;
#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[derive(Serialize, Clone)]
pub struct JavaInfo {
    pub path: String,
    pub version: String,
    pub major: u32,
}

#[tauri::command]
pub fn get_system_memory() -> u64 {
    let sys = System::new_all();
    sys.total_memory() / 1024 / 1024
}

#[tauri::command]
pub fn find_java(game_dir: Option<String>) -> Vec<JavaInfo> {
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

    // 0. 扫描启动器自己下载的 Java (gameDir/runtime/)
    if let Some(ref gd) = game_dir {
        let runtime_base = Path::new(gd).join("runtime");
        if runtime_base.exists() {
            if let Ok(entries) = std::fs::read_dir(&runtime_base) {
                for entry in entries.flatten() {
                    if entry.path().is_dir() {
                        try_java(entry.path().join("bin").join("java.exe").to_string_lossy().to_string());
                    }
                }
            }
        }
    }

    // 1. where java (PATH)
    if let Ok(output) = Command::new("where").arg("java").creation_flags(0x08000000).output() {
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
            .creation_flags(0x08000000)
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
    let output = Command::new(path).arg("-version").creation_flags(0x08000000).output().ok()?;
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
