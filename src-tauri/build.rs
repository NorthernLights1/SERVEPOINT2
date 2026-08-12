//! Cargo build script. Cargo runs this automatically before compiling the
//! crate; nothing imports it.
//!
//! `tauri_build::build()` reads `tauri.conf.json` and bakes the window
//! definitions, the app identity and the capability set into the binary, so
//! `tauri::generate_context!()` in `src/main.rs` has something to expand.

fn main() {
    tauri_build::build();
}
