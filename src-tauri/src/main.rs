// A till is a kiosk. On Windows the console window that would otherwise appear
// behind the app is not a debugging aid to anyone standing at a bar, so it is
// suppressed in release builds only — `cargo run` still prints.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! The window around the library.
//!
//! Everything this file does is: find where the venue's data lives, open it,
//! apply any migrations, and hand the result to the webview. All the rules are
//! in `servepoint_lib`, which is why this is short.

use tauri::Manager;

use servepoint_lib::{
    bills, commands, corrections, db, floor, overview, reconcile, recovery, state::AppState, venue,
};

/// The database file inside the app's data directory.
const DB_FILE: &str = "servepoint.db";

/// Point the till at a different database — used when developing, so an
/// experiment never touches a real venue's trading history.
const DB_OVERRIDE: &str = "SERVEPOINT_DB";

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            let path = match std::env::var(DB_OVERRIDE) {
                Ok(custom) if !custom.trim().is_empty() => std::path::PathBuf::from(custom),
                _ => {
                    let dir = app.path().app_data_dir()?;
                    // First run on a clean machine: the directory does not
                    // exist yet, and SQLite will not create it for us.
                    std::fs::create_dir_all(&dir)?;
                    dir.join(DB_FILE)
                }
            };

            // Opening applies every migration the file has not seen. If that
            // fails the app refuses to start rather than trading against a
            // half-built schema — a till that will not open is a bad evening,
            // a till that silently loses stock movements is a bad year.
            let connection = db::open(&path)?;
            app.manage(AppState::new(connection));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::cmd_bootstrap,
            commands::cmd_sign_in,
            commands::cmd_sign_out,
            commands::cmd_complete_setup,
            commands::cmd_read_settings,
            commands::cmd_write_settings,
            commands::cmd_verify_audit,
            floor::cmd_floor_view,
            floor::cmd_inventory_view,
            floor::cmd_open_shift,
            floor::cmd_open_tab,
            floor::cmd_place_order,
            corrections::cmd_correction_order,
            corrections::cmd_preview_correction,
            corrections::cmd_correct_order,
            corrections::cmd_void_order,
            venue::cmd_setup_view,
            venue::cmd_add_staff,
            venue::cmd_set_staff_active,
            venue::cmd_add_product,
            venue::cmd_sell_product,
            venue::cmd_edit_product,
            venue::cmd_add_sale_item,
            venue::cmd_edit_sale_item,
            venue::cmd_set_recipe,
            venue::cmd_set_price,
            venue::cmd_add_opening_stock,
            bills::cmd_tab_bill,
            bills::cmd_settle_tab,
            reconcile::cmd_reconciliation_view,
            reconcile::cmd_settle_waiter,
            reconcile::cmd_begin_closing,
            reconcile::cmd_close_night,
            overview::cmd_overview_view,
            recovery::cmd_recovery_view,
            recovery::cmd_resolve_handwritten,
            recovery::cmd_resolve_non_print,
        ])
        .run(tauri::generate_context!())
        .expect("ServePoint could not start");
}
