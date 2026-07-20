//! Tauri shell for openmso5202D.
//!
//! Thin: it owns the application state and registers the [`api`] commands the webview
//! calls. All scope logic lives in the `mso5202d` driver, reached through [`api`].

mod api;

use api::AppState;

/// Build and run the Tauri application.
pub fn run() {
    // File logging (3-day retention) for the whole session, same as the CLI tools.
    let _log = mso5202d::logging::init();

    tauri::Builder::default()
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            api::scope_status,
            api::connect_scope,
            api::prepare,
            api::capture,
        ])
        .run(tauri::generate_context!())
        .expect("error while running openmso5202D");
}
