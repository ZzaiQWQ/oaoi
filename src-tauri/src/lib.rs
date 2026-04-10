// ===== 模块声明 =====
mod java_detect;
mod java_download;
mod auth;
mod instance;
mod modcn;
mod mod_manager;
mod mod_search;
mod mod_download;
mod modpack_search;
mod launch;
mod versions;
mod installer;
mod modpack;

pub mod secrets {
    include!(concat!(env!("OUT_DIR"), "/secrets.rs"));
}

use tauri::Manager;
use tauri::Emitter;
use tauri::window::Color;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
  tauri::Builder::default()
    .plugin(tauri_plugin_dialog::init())
    .invoke_handler(tauri::generate_handler![
        java_detect::get_system_memory,
        java_detect::find_java,
        launch::launch_minecraft,
        auth::start_ms_login,
        auth::cancel_ms_login,
        auth::refresh_ms_login,
        installer::create_instance,
        versions::fabric::get_fabric_versions,
        versions::forge::get_forge_versions,
        versions::neoforge::get_neoforge_versions,
        versions::quilt::get_quilt_versions,
        instance::list_installed_versions,
        instance::delete_version,
        instance::open_folder,
        instance::open_url,
        instance::cancel_modpack_install,
        mod_manager::list_mods,
        mod_manager::toggle_mod,
        mod_manager::delete_mod,
        mod_manager::lookup_mod_urls,
        mod_search::search_online_mods,
        mod_download::download_online_mod,
        java_download::download_java,
        modpack::import_modpack,
        modpack_search::search_modpacks,
        modpack_search::get_modpack_versions,
        modpack_search::install_modpack_direct,
    ])
    .setup(|app| {
      let window = app.get_webview_window("main").unwrap();
      let _ = window.set_background_color(Some(Color(0, 0, 0, 0)));

      // 监听文件拖放，将路径发给前端
      let window2 = window.clone();
      window.on_window_event(move |evt| {
        if let tauri::WindowEvent::DragDrop(tauri::DragDropEvent::Drop { paths, position: _ }) = evt {
          for path in paths {
            let path_str = path.to_string_lossy().to_string();
            let lower = path_str.to_lowercase();
            if lower.ends_with(".zip") || lower.ends_with(".mrpack") {
              let _ = window2.emit("modpack-drop", serde_json::json!({ "path": path_str }));
            }
          }
        }
        if let tauri::WindowEvent::DragDrop(tauri::DragDropEvent::Enter { paths: _, position: _ }) = evt {
          let _ = window2.emit("modpack-drag-enter", ());
        }
        if let tauri::WindowEvent::DragDrop(tauri::DragDropEvent::Leave) = evt {
          let _ = window2.emit("modpack-drag-leave", ());
        }
      });

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
