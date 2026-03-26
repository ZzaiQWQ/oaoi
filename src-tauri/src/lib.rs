// ===== 模块声明 =====
mod java_detect;
mod java_download;
mod game_dir;
mod auth;
mod instance;
mod launch;
mod versions;
mod installer;

use tauri::Manager;
use tauri::window::Color;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
  tauri::Builder::default()
    .plugin(tauri_plugin_dialog::init())
    .invoke_handler(tauri::generate_handler![
        java_detect::get_system_memory,
        java_detect::find_java,
        game_dir::init_game_dir,
        launch::launch_minecraft,
        auth::start_ms_login,
        installer::create_instance,
        versions::fabric::get_fabric_versions,
        versions::forge::get_forge_versions,
        versions::neoforge::get_neoforge_versions,
        versions::quilt::get_quilt_versions,
        instance::list_installed_versions,
        instance::delete_version,
        java_download::download_java,
    ])
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
