//! ink-notes: Latenzoptimierte Linux-Inking-App mit wgpu, winit, egui-Toolbar,
//! Seitenverwaltung, Undo/Redo, Vektor-PDF-Export und asynchroner Notizbuch-Persistenz (Autosave).
//!
//! Desktop entry point; the app itself is [`ink_notes::app`].

// No console window next to the app on Windows (debug builds keep it for the log)
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

#[cfg(not(target_os = "android"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    ink_notes::app::run()
}

/// On Android the app starts in `android_main` of the library.
#[cfg(target_os = "android")]
fn main() {}
