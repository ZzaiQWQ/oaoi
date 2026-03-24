fn main() {
  // 读取项目根 .env，注入编译环境变量供 env!() 使用
  let env_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../.env");
  if let Ok(content) = std::fs::read_to_string(&env_path) {
    for line in content.lines() {
      let line = line.trim();
      if line.is_empty() || line.starts_with('#') { continue; }
      if let Some((key, val)) = line.split_once('=') {
        println!("cargo:rustc-env={}={}", key.trim(), val.trim());
      }
    }
    println!("cargo:rerun-if-changed={}", env_path.display());
  }
  tauri_build::build()
}
