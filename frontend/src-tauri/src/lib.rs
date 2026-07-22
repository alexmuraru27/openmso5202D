//! Tauri shell for openmso5202D.
//!
//! Thin: it owns the application state and registers the [`api`] commands the webview
//! calls. All scope logic lives in the `mso5202d` driver, reached through [`api`].

pub mod api;

use api::AppState;

/// Build and run the Tauri application.
pub fn run() {
    // File logging (3-day retention) for the whole session, same as the CLI tools.
    let _log = mso5202d::logging::init();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            api::scope_status,
            api::connect_scope,
            api::prepare,
            api::capture,
            api::list_card_files,
            api::download_card_files,
            api::load_csvs,
            api::redecode,
            api::clear_card_files,
        ])
        .run(tauri::generate_context!())
        .expect("error while running openmso5202D");
}
