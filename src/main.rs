//! ink-notes: Latenzoptimierte Linux-Inking-App mit wgpu, winit, egui-Toolbar,
//! Seitenverwaltung, Undo/Redo, Vektor-PDF-Export und asynchroner Notizbuch-Persistenz (Autosave).
//!
//! Desktop entry point; the app itself is [`ink_notes::app`].

fn main() -> Result<(), Box<dyn std::error::Error>> {
    ink_notes::app::run()
}
