//! Tastatur-Ereignisse und Kürzel (Undo/Redo, Werkzeuge, Zoom, Export, Navigation, Beenden).

use winit::event::{ElementState, KeyEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{Key, NamedKey};

use crate::document::PageBackground;
use crate::storage;
use crate::t;
use crate::ui::{self, ActiveTool};

use super::helpers::*;
use super::AppState;

impl AppState {
    pub(super) fn handle_keyboard_input(&mut self, event: KeyEvent, target: &ActiveEventLoop) {
        let KeyEvent {
            logical_key,
            state: key_state,
            ..
        } = event;
        // Android's back button closes menus and dialogs like Escape; it never
        // reaches the system, so it does not close the app (Issue #42)
        let is_back = matches!(logical_key, Key::Named(NamedKey::BrowserBack));
        let logical_key = if is_back {
            Key::Named(NamedKey::Escape)
        } else {
            logical_key
        };

        // Tastatureingaben nicht für Shortcuts nutzen, wenn ein Textfeld fokussiert ist
        if self.egui_ctx.egui_wants_keyboard_input() && !is_back {
            return;
        }

        // Offenes Textfeld ohne Fokus (z. B. nach einem Klick in die Leiste):
        // Tasten nicht als Kürzel deuten, sondern das Feld wieder fokussieren
        if let Some(edit) = &mut self.ui_state.text_edit {
            if key_state == ElementState::Pressed
                && !matches!(logical_key, Key::Named(NamedKey::Escape))
            {
                edit.focus = true;
                self.window.request_redraw();
                return;
            }
        }

        if let Key::Named(NamedKey::Space) = &logical_key {
            self.is_space_pressed = key_state == ElementState::Pressed;
            if !self.is_space_pressed && self.is_panning {
                self.is_panning = false;
            }
        }

        if key_state != ElementState::Pressed {
            return;
        }

        let ctrl = self.modifiers.control_key();
        let shift = self.modifiers.shift_key();

        if self.ui_state.view_mode && !ui::key_allowed_in_view_mode(&logical_key, ctrl) {
            return;
        }

        match &logical_key {
            Key::Named(NamedKey::Escape) => {
                let mut changed = false;
                if self.ui_state.shutdown_error.is_some() {
                    self.ui_state.shutdown_error = None;
                    changed = true;
                } else if self.ui_state.is_pen_menu_open
                    || self.ui_state.is_highlighter_menu_open
                    || self.ui_state.show_page_jump_popup
                    || self.ui_state.is_document_menu_open
                    || self.ui_state.is_update_dialog_open
                    || self.ui_state.show_updater_config
                {
                    self.ui_state.is_pen_menu_open = false;
                    self.ui_state.is_highlighter_menu_open = false;
                    self.ui_state.show_page_jump_popup = false;
                    self.ui_state.is_document_menu_open = false;
                    self.ui_state.is_update_dialog_open = false;
                    self.ui_state.show_updater_config = false;
                    changed = true;
                } else if let Some(edit) = &mut self.ui_state.text_edit {
                    edit.finish = true;
                    changed = true;
                } else if !self.selection_state.is_empty() {
                    self.selection_state.clear();
                    changed = true;
                }
                if changed {
                    self.window.request_redraw();
                }
            }

            // Undo: Strg + Z
            Key::Character(c) if c.eq_ignore_ascii_case("z") && ctrl && !shift => {
                finish_text_edit(
                    &mut self.canvas,
                    &mut self.notebook,
                    &mut self.manifest,
                    &mut self.autosave,
                    &mut self.ui_state,
                );
                if let Some(changed) = self.notebook.undo() {
                    self.selection_state.clear();
                    self.canvas.sync_page(self.notebook.current_page_mut());
                    persist_pages(
                        &changed,
                        &self.notebook,
                        &mut self.manifest,
                        &mut self.autosave,
                    );
                    update_window_title(
                        &self.window,
                        &self.notebook,
                        &self.manifest,
                        self.ui_state.active_tool,
                        self.autosave.status(),
                    );
                    self.window.request_redraw();
                }
            }

            // Redo: Strg + Y oder Strg + Umschalt + Z
            Key::Character(c)
                if (c.eq_ignore_ascii_case("y") && ctrl)
                    || (c.eq_ignore_ascii_case("z") && ctrl && shift) =>
            {
                finish_text_edit(
                    &mut self.canvas,
                    &mut self.notebook,
                    &mut self.manifest,
                    &mut self.autosave,
                    &mut self.ui_state,
                );
                if let Some(changed) = self.notebook.redo() {
                    self.selection_state.clear();
                    self.canvas.sync_page(self.notebook.current_page_mut());
                    persist_pages(
                        &changed,
                        &self.notebook,
                        &mut self.manifest,
                        &mut self.autosave,
                    );
                    update_window_title(
                        &self.window,
                        &self.notebook,
                        &self.manifest,
                        self.ui_state.active_tool,
                        self.autosave.status(),
                    );
                    self.window.request_redraw();
                }
            }

            // Kopieren: Strg + C
            Key::Character(c) if c.eq_ignore_ascii_case("c") && ctrl => {
                let count = self
                    .selection_state
                    .copy_to_clipboard(self.notebook.current_page());
                if count > 0 {
                    let as_image = self.clipboard.copy_selection(
                        self.notebook.current_page(),
                        &self.selection_state.selected_ids,
                        &self.storage_dir,
                    );
                    self.clipboard.note_internal_copy(&self.storage_dir);
                    self.ui_state
                        .set_export_message(copy_message(count, as_image));
                    self.window.request_redraw();
                }
            }

            // Einfügen: Strg + V
            Key::Character(c) if c.eq_ignore_ascii_case("v") && ctrl => {
                paste(
                    None,
                    &mut self.clipboard,
                    &self.storage_dir,
                    &mut self.canvas,
                    &mut self.notebook,
                    &mut self.manifest,
                    &mut self.autosave,
                    &mut self.selection_state,
                    &mut self.ui_state,
                );
                update_window_title(
                    &self.window,
                    &self.notebook,
                    &self.manifest,
                    self.ui_state.active_tool,
                    self.autosave.status(),
                );
                self.window.request_redraw();
            }

            // Lineal ein-/ausblenden: L
            Key::Character(c) if c.eq_ignore_ascii_case("l") && !ctrl => {
                self.ui_state.toggle_ruler();
                self.canvas.ruler = self.ui_state.ruler;
                self.window.request_redraw();
            }

            // Bild einfügen: Strg + I
            Key::Character(c) if c.eq_ignore_ascii_case("i") && ctrl => {
                self.ui_state.open_image_picker();
                self.window.request_redraw();
            }

            // Löschen: Entf oder Backspace
            Key::Named(NamedKey::Delete) | Key::Named(NamedKey::Backspace) => {
                if !self.selection_state.is_empty() {
                    let removed = self
                        .selection_state
                        .delete_selected(self.notebook.current_page_mut());
                    if !removed.is_empty() {
                        self.canvas.sync_page(self.notebook.current_page_mut());
                        self.autosave.mark_dirty(&self.notebook, &mut self.manifest);
                        self.ui_state.set_export_message(t!(
                            "{count} Objekt(e) gelöscht",
                            count = removed.len()
                        ));
                        self.window.request_redraw();
                    }
                }
            }

            // Jetzt speichern (Disk Flush): Strg + S
            Key::Character(c) if c.eq_ignore_ascii_case("s") && ctrl => {
                match self.autosave.flush() {
                    Ok(()) => {
                        self.ui_state.set_export_message(t!("Gespeichert"));
                    }
                    Err(err) => {
                        self.ui_state.set_export_message(t!(
                            "Speicherfehler: {messages}",
                            messages = err.messages.join(", ")
                        ));
                    }
                }
                update_window_title(
                    &self.window,
                    &self.notebook,
                    &self.manifest,
                    self.ui_state.active_tool,
                    self.autosave.status(),
                );
                self.window.request_redraw();
            }

            // PDF-Export: Strg + E
            Key::Character(c) if c.eq_ignore_ascii_case("e") && ctrl => {
                start_pdf_export(
                    &self.notebook,
                    &self.storage_dir,
                    &self.manifest.title,
                    self.ui_state.pattern_color,
                    self.export_tx.clone(),
                    self.window.clone(),
                );
                self.ui_state.set_export_message(t!("PDF-Export läuft…"));
                self.window.request_redraw();
            }

            // Radierer umschalten: E
            Key::Character(c) if c.eq_ignore_ascii_case("e") && !ctrl => {
                self.ui_state.active_tool = match self.ui_state.active_tool {
                    ActiveTool::Pen
                    | ActiveTool::Highlighter
                    | ActiveTool::Shape
                    | ActiveTool::Select
                    | ActiveTool::Text
                    | ActiveTool::Space => ActiveTool::Eraser,
                    ActiveTool::Eraser => ActiveTool::Pen,
                };
                self.ui_state.is_pen_menu_open = false;
                self.ui_state.is_highlighter_menu_open = false;
                update_window_title(
                    &self.window,
                    &self.notebook,
                    &self.manifest,
                    self.ui_state.active_tool,
                    self.autosave.status(),
                );
                self.window.request_redraw();
            }

            // Auswahlwerkzeug umschalten: V (ohne Strg)
            Key::Character(c) if c.eq_ignore_ascii_case("v") && !ctrl => {
                self.ui_state.active_tool = if self.ui_state.active_tool == ActiveTool::Select {
                    ActiveTool::Pen
                } else {
                    ActiveTool::Select
                };
                self.ui_state.is_pen_menu_open = false;
                self.ui_state.is_highlighter_menu_open = false;
                update_window_title(
                    &self.window,
                    &self.notebook,
                    &self.manifest,
                    self.ui_state.active_tool,
                    self.autosave.status(),
                );
                self.window.request_redraw();
            }

            // Stiftfarbe Notiz-Schwarz: B
            Key::Character(c) if c.eq_ignore_ascii_case("b") && !ctrl => {
                if !self.selection_state.is_empty() {
                    let color = [0.12, 0.12, 0.14, 1.0];
                    if self
                        .selection_state
                        .recolor_selected(self.notebook.current_page_mut(), color)
                    {
                        self.canvas.sync_page(self.notebook.current_page_mut());
                        self.autosave.mark_dirty(&self.notebook, &mut self.manifest);
                        self.ui_state
                            .set_export_message(t!("Auswahl auf Schwarz umgefärbt").to_string());
                        self.window.request_redraw();
                    }
                } else {
                    self.ui_state.active_tool = ActiveTool::Pen;
                    self.ui_state.pen_color = [0.12, 0.12, 0.14, 1.0];
                    self.canvas.set_stroke_color(self.ui_state.pen_color);
                    self.canvas
                        .set_stroke_width_mm(self.ui_state.pen_stroke_width_mm);
                    update_window_title(
                        &self.window,
                        &self.notebook,
                        &self.manifest,
                        self.ui_state.active_tool,
                        self.autosave.status(),
                    );
                    self.window.request_redraw();
                }
            }

            // Stiftfarbe Universitäts-Blau: U
            Key::Character(c) if c.eq_ignore_ascii_case("u") && !ctrl => {
                if !self.selection_state.is_empty() {
                    let color = [0.15, 0.35, 0.85, 1.0];
                    if self
                        .selection_state
                        .recolor_selected(self.notebook.current_page_mut(), color)
                    {
                        self.canvas.sync_page(self.notebook.current_page_mut());
                        self.autosave.mark_dirty(&self.notebook, &mut self.manifest);
                        self.ui_state
                            .set_export_message(t!("Auswahl auf Blau umgefärbt").to_string());
                        self.window.request_redraw();
                    }
                } else {
                    self.ui_state.active_tool = ActiveTool::Pen;
                    self.ui_state.pen_color = [0.15, 0.35, 0.85, 1.0];
                    self.canvas.set_stroke_color(self.ui_state.pen_color);
                    self.canvas
                        .set_stroke_width_mm(self.ui_state.pen_stroke_width_mm);
                    update_window_title(
                        &self.window,
                        &self.notebook,
                        &self.manifest,
                        self.ui_state.active_tool,
                        self.autosave.status(),
                    );
                    self.window.request_redraw();
                }
            }

            // Stiftfarbe Korrektur-Rot oder Drehen: R
            Key::Character(c) if c.eq_ignore_ascii_case("r") && !ctrl => {
                if !self.selection_state.is_empty() {
                    if self
                        .selection_state
                        .rotate_selected_90_deg(self.notebook.current_page_mut())
                    {
                        self.canvas.sync_page(self.notebook.current_page_mut());
                        self.autosave.mark_dirty(&self.notebook, &mut self.manifest);
                        self.ui_state
                            .set_export_message(t!("Auswahl um 90° gedreht").to_string());
                        self.window.request_redraw();
                    }
                } else {
                    self.ui_state.active_tool = ActiveTool::Pen;
                    self.ui_state.pen_color = [0.85, 0.20, 0.20, 1.0];
                    self.canvas.set_stroke_color(self.ui_state.pen_color);
                    self.canvas
                        .set_stroke_width_mm(self.ui_state.pen_stroke_width_mm);
                    update_window_title(
                        &self.window,
                        &self.notebook,
                        &self.manifest,
                        self.ui_state.active_tool,
                        self.autosave.status(),
                    );
                    self.window.request_redraw();
                }
            }

            // Hintergrund wechseln: G
            Key::Character(c) if c.eq_ignore_ascii_case("g") && !ctrl => {
                let page = self.notebook.current_page_mut();
                page.background = match page.background {
                    PageBackground::Grid5mm => PageBackground::Lined,
                    PageBackground::Lined => PageBackground::DotGrid,
                    PageBackground::DotGrid => PageBackground::Blank,
                    PageBackground::Blank => PageBackground::Grid5mm,
                };
                self.canvas.sync_page(page);
                self.autosave.mark_dirty(&self.notebook, &mut self.manifest);
                update_window_title(
                    &self.window,
                    &self.notebook,
                    &self.manifest,
                    self.ui_state.active_tool,
                    self.autosave.status(),
                );
                self.window.request_redraw();
            }

            // Nächste Seite: PageDown oder N (mit Lazy Loading)
            // View only: never append a new page at the end
            Key::Named(NamedKey::PageDown)
                if self.ui_state.view_mode
                    && self.notebook.current_page_index + 1 >= self.notebook.pages.len() => {}
            Key::Named(NamedKey::PageDown) => {
                self.selection_state.clear();
                self.notebook.next_page();
                let curr = self.notebook.current_page_index;
                let _ = storage::ensure_page_loaded(&self.storage_dir, &mut self.notebook, curr);
                self.canvas.sync_page(self.notebook.current_page_mut());
                self.manifest.current_page_index = self.notebook.current_page_index;
                self.manifest.next_page_id = self.notebook.next_page_id;
                self.manifest.sync_from_notebook(&self.notebook);
                self.autosave.mark_manifest_dirty(&self.manifest);
                self.autosave.mark_dirty(&self.notebook, &mut self.manifest);
                update_window_title(
                    &self.window,
                    &self.notebook,
                    &self.manifest,
                    self.ui_state.active_tool,
                    self.autosave.status(),
                );
                self.window.request_redraw();
            }
            Key::Character(c) if c.eq_ignore_ascii_case("n") && !ctrl => {
                self.selection_state.clear();
                self.notebook.next_page();
                let curr = self.notebook.current_page_index;
                let _ = storage::ensure_page_loaded(&self.storage_dir, &mut self.notebook, curr);
                self.canvas.sync_page(self.notebook.current_page_mut());
                self.manifest.current_page_index = self.notebook.current_page_index;
                self.manifest.next_page_id = self.notebook.next_page_id;
                self.manifest.sync_from_notebook(&self.notebook);
                self.autosave.mark_manifest_dirty(&self.manifest);
                self.autosave.mark_dirty(&self.notebook, &mut self.manifest);
                update_window_title(
                    &self.window,
                    &self.notebook,
                    &self.manifest,
                    self.ui_state.active_tool,
                    self.autosave.status(),
                );
                self.window.request_redraw();
            }

            // Vorherige Seite: PageUp
            Key::Named(NamedKey::PageUp) => {
                self.selection_state.clear();
                self.notebook.previous_page();
                let curr = self.notebook.current_page_index;
                let _ = storage::ensure_page_loaded(&self.storage_dir, &mut self.notebook, curr);
                self.canvas.sync_page(self.notebook.current_page_mut());
                self.manifest.current_page_index = self.notebook.current_page_index;
                self.manifest.sync_from_notebook(&self.notebook);
                self.autosave.mark_manifest_dirty(&self.manifest);
                self.autosave.mark_dirty(&self.notebook, &mut self.manifest);
                update_window_title(
                    &self.window,
                    &self.notebook,
                    &self.manifest,
                    self.ui_state.active_tool,
                    self.autosave.status(),
                );
                self.window.request_redraw();
            }

            // Erste Seite: Home / Pos1
            Key::Named(NamedKey::Home) => {
                self.selection_state.clear();
                self.notebook.first_page();
                let curr = self.notebook.current_page_index;
                let _ = storage::ensure_page_loaded(&self.storage_dir, &mut self.notebook, curr);
                self.canvas.sync_page(self.notebook.current_page_mut());
                self.manifest.current_page_index = self.notebook.current_page_index;
                self.manifest.sync_from_notebook(&self.notebook);
                self.autosave.mark_manifest_dirty(&self.manifest);
                self.autosave.mark_dirty(&self.notebook, &mut self.manifest);
                update_window_title(
                    &self.window,
                    &self.notebook,
                    &self.manifest,
                    self.ui_state.active_tool,
                    self.autosave.status(),
                );
                self.window.request_redraw();
            }

            // Letzte Seite: End / Ende
            Key::Named(NamedKey::End) => {
                self.selection_state.clear();
                self.notebook.last_page();
                let curr = self.notebook.current_page_index;
                let _ = storage::ensure_page_loaded(&self.storage_dir, &mut self.notebook, curr);
                self.canvas.sync_page(self.notebook.current_page_mut());
                self.manifest.current_page_index = self.notebook.current_page_index;
                self.manifest.sync_from_notebook(&self.notebook);
                self.autosave.mark_manifest_dirty(&self.manifest);
                self.autosave.mark_dirty(&self.notebook, &mut self.manifest);
                update_window_title(
                    &self.window,
                    &self.notebook,
                    &self.manifest,
                    self.ui_state.active_tool,
                    self.autosave.status(),
                );
                self.window.request_redraw();
            }

            // Zoom zurücksetzen: Strg + 0
            Key::Character(c) if c == "0" && ctrl => {
                self.canvas.reset_view();
                self.ui_state.current_zoom = self.canvas.camera.zoom;
                self.window.request_redraw();
            }

            // Zoom In: Strg + '+' oder Strg + '='
            Key::Character(c) if (c == "+" || c == "=") && ctrl => {
                self.canvas.zoom_at(
                    self.canvas.size.width as f32 * 0.5,
                    self.canvas.size.height as f32 * 0.5,
                    1.15,
                );
                self.ui_state.current_zoom = self.canvas.camera.zoom;
                self.window.request_redraw();
            }

            // Zoom Out: Strg + '-' oder Strg + '_'
            Key::Character(c) if (c == "-" || c == "_") && ctrl => {
                self.canvas.zoom_at(
                    self.canvas.size.width as f32 * 0.5,
                    self.canvas.size.height as f32 * 0.5,
                    1.0 / 1.15,
                );
                self.ui_state.current_zoom = self.canvas.camera.zoom;
                self.window.request_redraw();
            }

            // Text-Werkzeug umschalten: T
            Key::Character(c) if c.eq_ignore_ascii_case("t") && !ctrl => {
                self.ui_state.active_tool = if self.ui_state.active_tool == ActiveTool::Text {
                    ActiveTool::Pen
                } else {
                    ActiveTool::Text
                };
                self.ui_state.is_pen_menu_open = false;
                self.ui_state.is_highlighter_menu_open = false;
                update_window_title(
                    &self.window,
                    &self.notebook,
                    &self.manifest,
                    self.ui_state.active_tool,
                    self.autosave.status(),
                );
                self.window.request_redraw();
            }

            // Formen-Werkzeug umschalten: S (ohne Strg) oder F
            Key::Character(c)
                if (c.eq_ignore_ascii_case("s") || c.eq_ignore_ascii_case("f")) && !ctrl =>
            {
                self.ui_state.active_tool = if self.ui_state.active_tool == ActiveTool::Shape {
                    ActiveTool::Pen
                } else {
                    ActiveTool::Shape
                };
                update_window_title(
                    &self.window,
                    &self.notebook,
                    &self.manifest,
                    self.ui_state.active_tool,
                    self.autosave.status(),
                );
                self.window.request_redraw();
            }

            // Zwischen offenen Notizbuch-Tabs wechseln: Strg+Tab / Strg+Umschalt+Tab
            Key::Named(NamedKey::Tab) if ctrl => {
                let delta = if shift { -1 } else { 1 };
                if let Some(next_id) = self.ui_state.adjacent_tab(delta) {
                    self.ui_state
                        .queued_actions
                        .push(ui::UiAction::OpenNotebook(next_id));
                    self.window.request_redraw();
                }
            }

            // Aktuellen Notizbuch-Tab schließen: Strg+W (ab 2 offenen Tabs)
            Key::Character(c) if c.eq_ignore_ascii_case("w") && ctrl => {
                if self.ui_state.open_tabs.len() >= 2 {
                    let current = self.manifest.id.clone();
                    self.ui_state
                        .queued_actions
                        .push(ui::UiAction::CloseTab(current));
                    self.window.request_redraw();
                }
            }

            // Beenden: Strg + Q
            Key::Character(c) if c.eq_ignore_ascii_case("q") && ctrl => {
                finish_text_edit(
                    &mut self.canvas,
                    &mut self.notebook,
                    &mut self.manifest,
                    &mut self.autosave,
                    &mut self.ui_state,
                );
                println!("[Shutdown] Sichere alle ungespeicherten Daten auf Festplatte...");
                match self.autosave.shutdown() {
                    Ok(()) => {
                        target.exit();
                    }
                    Err(err) => {
                        eprintln!("[Shutdown-Fehler] Konnte nicht geordnet beenden: {err}");
                        self.ui_state.shutdown_error = Some(err);
                        self.window.request_redraw();
                    }
                }
            }

            _ => {}
        }
    }
}
