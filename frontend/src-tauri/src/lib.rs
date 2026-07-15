//! Tauri shell for openmso5202D.
//!
//! For now this is only the window shell — it does **not** talk to the scope. The bridge
//! to the `mso5202d` USB driver will be added later behind a dedicated backend-API layer,
//! so the frontend never calls USB operations directly. Until that layer exists there are
//! no `#[tauri::command]`s and no backend dependency here.

/// Build and run the Tauri application.
pub fn run() {
    tauri::Builder::default()
        .run(tauri::generate_context!())
        .expect("error while running openmso5202D");
}
