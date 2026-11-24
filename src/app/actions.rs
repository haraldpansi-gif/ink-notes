//! Verarbeitung aller UiAction-Ereignisse aus Toolbar, Popovern und Dialogen.

use winit::event_loop::ActiveEventLoop;

use crate::document::PdfBackground;
use crate::i18n::{self, Language};
use crate::storage::{self, NotebookLayout, NotebookManifest};
use crate::t;
use crate::ui::{ActiveTool, UiAction};

use super::helpers::*;
use super::AppState;

impl AppState {
    pub(super) fn handle_ui_action(
        &mut self,
        action: UiAction,
        pdf_thumbnails_wanted: &mut Vec<PdfBackground>,
        target: &ActiveEventLoop,
    ) {
        match action {
            UiAction::SetTool(tool) => {
                activate_tool(tool, &mut self.ui_state, &mut self.canvas);
                update_window_title(
                    &self.window,
                    &self.notebook,
                    &self.manifest,
                    tool,
                    self.autosave.status(),
                );
                self.window.request_redraw();
            }
            UiAction::SetViewMode(on) => {
                self.ui_state.view_mode = on;
                if on {
                    // Nothing may stay half-edited while only viewing
                    finish_text_edit(
                        &mut self.canvas,
                        &mut self.notebook,
                        &mut self.manifest,
                        &mut self.autosave,
                        &mut self.ui_state,
                    );
                    self.selection_state.clear();
                    self.ui_state.ruler = None;
                    self.ui_state.ruler_drag = None;
                    self.canvas.ruler = None;
                    self.ui_state
                        .set_export_message(t!("Nur ansehen – 👁 erneut tippen, um zu schreiben"));
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
            UiAction::SetColor(color) => {
                if !self.selection_state.is_empty()
                    && self
                        .selection_state
                        .recolor_selected(self.notebook.current_page_mut(), color)
                {
                    self.canvas.sync_page(self.notebook.current_page_mut());
                    self.autosave.mark_dirty(&self.notebook, &mut self.manifest);
                    self.ui_state
                        .set_export_message(t!("Auswahl umgefärbt").to_string());
                }
                match self.ui_state.active_tool {
                    ActiveTool::Pen | ActiveTool::Shape => {
                        self.ui_state.pen_color = color;
                        self.canvas.set_stroke_color(color);
                    }
                    ActiveTool::Highlighter => {
                        self.ui_state.highlighter_color = color;
                        self.canvas.set_stroke_color(color);
                    }
                    ActiveTool::Eraser
                    | ActiveTool::Select
                    | ActiveTool::Text
                    | ActiveTool::Space => {}
                }
                self.window.request_redraw();
            }
            UiAction::SetStrokeWidthMm(width) => {
                match self.ui_state.active_tool {
                    ActiveTool::Pen | ActiveTool::Shape => {
                        self.ui_state.pen_stroke_width_mm = width;
                        self.canvas.set_stroke_width_mm(width);
                    }
                    ActiveTool::Highlighter => {
                        self.ui_state.highlighter_stroke_width_mm = width;
                        self.canvas.set_stroke_width_mm(width);
                    }
                    ActiveTool::Eraser
                    | ActiveTool::Select
                    | ActiveTool::Text
                    | ActiveTool::Space => {}
                }
                self.window.request_redraw();
            }
            UiAction::DeleteSelected => {
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
            UiAction::SaveSelectionAsStamp(requested_name) => {
                if !self.selection_state.is_empty() {
                    let selected = self
                        .notebook
                        .current_page()
                        .get_strokes(&self.selection_state.selected_ids);
                    match self
                        .ui_state
                        .stamps
                        .add_from_strokes(requested_name.as_deref(), &selected)
                    {
                        Ok(stamp) => {
                            let name = stamp.name.clone();
                            if let Err(err) = self.ui_state.stamps.save(&self.storage_base_dir) {
                                eprintln!("[Stempel] Konnte stamps.json nicht speichern: {err}");
                                self.ui_state.set_export_message(t!(
                                    "Stempel nicht gespeichert: {err}",
                                    err = err
                                ));
                            } else {
                                self.ui_state.set_export_message(t!(
                                    "Stempel '{name}' gespeichert",
                                    name = name
                                ));
                            }
                        }
                        Err(msg) => {
                            self.ui_state.set_export_message(msg);
                        }
                    }
                    self.window.request_redraw();
                }
            }
            UiAction::InsertStamp(id) => {
                if let Some(stamp) = self.ui_state.stamps.get(&id).cloned() {
                    let center = visible_page_center(&self.canvas);
                    let mut next_id = || self.notebook.next_stroke_id();
                    let strokes = stamp.instantiate(center, &mut next_id);
                    if !strokes.is_empty() {
                        let new_ids: Vec<u64> = strokes.iter().map(|s| s.id).collect();
                        self.notebook.current_page_mut().add_strokes(strokes);
                        self.selection_state.selected_ids = new_ids;
                        activate_tool(ActiveTool::Select, &mut self.ui_state, &mut self.canvas);
                        self.canvas.sync_page(self.notebook.current_page_mut());
                        self.autosave.mark_dirty(&self.notebook, &mut self.manifest);
                        update_window_title(
                            &self.window,
                            &self.notebook,
                            &self.manifest,
                            self.ui_state.active_tool,
                            self.autosave.status(),
                        );
                        self.ui_state.set_export_message(t!(
                            "Stempel '{name}' eingefügt",
                            name = stamp.name
                        ));
                        self.window.request_redraw();
                    }
                }
            }
            UiAction::DeleteStamp(id) => {
                if let Some(removed) = self.ui_state.stamps.delete(&id) {
                    if let Err(err) = self.ui_state.stamps.save(&self.storage_base_dir) {
                        eprintln!("[Stempel] Konnte stamps.json nicht speichern: {err}");
                    }
                    self.ui_state
                        .set_export_message(t!("Stempel '{name}' gelöscht", name = removed.name));
                    self.window.request_redraw();
                }
            }
            UiAction::CopySelected => {
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
            UiAction::PasteSelected | UiAction::PasteAt(_) => {
                let at = match action {
                    UiAction::PasteAt(at) => Some(at),
                    _ => None,
                };
                paste(
                    at,
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
            UiAction::InsertImageFile(path) => {
                let center = visible_page_center(&self.canvas);
                insert_image_file(
                    &path,
                    center,
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
            UiAction::ToggleAspectLock => {
                if !self.selection_state.is_empty() {
                    self.selection_state
                        .toggle_aspect_lock(self.notebook.current_page());
                    self.window.request_redraw();
                }
            }
            UiAction::RotateSelected => {
                if !self.selection_state.is_empty()
                    && self
                        .selection_state
                        .rotate_selected_90_deg(self.notebook.current_page_mut())
                {
                    self.canvas.sync_page(self.notebook.current_page_mut());
                    self.autosave.mark_dirty(&self.notebook, &mut self.manifest);
                    self.ui_state
                        .set_export_message(t!("Auswahl um 90° gedreht").to_string());
                    self.window.request_redraw();
                }
            }
            UiAction::ZoomIn => {
                let center_x = self.canvas.size.width as f32 * 0.5;
                let center_y = self.canvas.size.height as f32 * 0.5;
                self.canvas.zoom_at(center_x, center_y, 1.25);
                self.ui_state.current_zoom = self.canvas.camera.zoom;
                self.window.request_redraw();
            }
            UiAction::ZoomOut => {
                let center_x = self.canvas.size.width as f32 * 0.5;
                let center_y = self.canvas.size.height as f32 * 0.5;
                self.canvas.zoom_at(center_x, center_y, 1.0 / 1.25);
                self.ui_state.current_zoom = self.canvas.camera.zoom;
                self.window.request_redraw();
            }
            UiAction::ResetZoom => {
                self.canvas.reset_view();
                self.ui_state.current_zoom = self.canvas.camera.zoom;
                self.window.request_redraw();
            }
            UiAction::Undo => {
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
            UiAction::Redo => {
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
            UiAction::PreviousPage => {
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
            UiAction::NextPage => {
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
            UiAction::ExportPage(kind) => {
                start_page_export(
                    kind,
                    &self.notebook,
                    &self.storage_dir,
                    &self.manifest.title,
                    self.ui_state.pattern_color,
                    self.export_tx.clone(),
                    self.window.clone(),
                );
                self.ui_state
                    .set_export_message(t!("Seite wird exportiert …").to_string());
                self.window.request_redraw();
            }
            UiAction::SetPageTitle { page_id, title } => {
                if self.notebook.set_page_title(page_id, &title) {
                    if let Some(page) = self.notebook.pages.iter().find(|p| p.id == page_id) {
                        self.manifest.sync_from_notebook(&self.notebook);
                        self.autosave.mark_page_dirty(page, &self.manifest);
                    }
                    update_window_title(
                        &self.window,
                        &self.notebook,
                        &self.manifest,
                        self.ui_state.active_tool,
                        self.autosave.status(),
                    );
                }
            }
            // Die endlose Fläche bleibt eine einzige Seite
            UiAction::AddPage if self.notebook.is_endless() => {}
            UiAction::AddPage => {
                // Leere Seite direkt nach der aktuellen (in PDFs: nach der Folie,
                // auf der man gerade ist) und dorthin wechseln, with the
                // background chosen last (Issue #60)
                let bg = self
                    .ui_settings
                    .page_background
                    .unwrap_or(self.notebook.current_page().background);
                let index = self.notebook.insert_page_after_current(bg);
                self.manifest.sync_from_notebook(&self.notebook);
                switch_to_page(
                    index,
                    false,
                    &mut self.canvas,
                    &mut self.notebook,
                    &mut self.manifest,
                    &mut self.autosave,
                    &self.storage_dir,
                    &mut self.selection_state,
                    &mut self.ui_state,
                    &self.window,
                );
                self.ui_state
                    .set_export_message(t!("Seite {number} eingefügt", number = index + 1));
            }
            UiAction::DeleteCurrentPage => {
                let index = self.notebook.current_page_index;
                if let Some(page) = self.notebook.remove_current_page() {
                    self.selection_state.clear();
                    let curr = self.notebook.current_page_index;
                    let _ =
                        storage::ensure_page_loaded(&self.storage_dir, &mut self.notebook, curr);
                    self.canvas.sync_page(self.notebook.current_page_mut());
                    self.manifest.sync_from_notebook(&self.notebook);
                    self.autosave.mark_manifest_dirty(&self.manifest);
                    self.last_deleted_page = Some((self.manifest.id.clone(), index, page));
                    self.ui_state.set_export_message(t!(
                        "Seite {number} gelöscht – im Menü ☰ wiederherstellbar",
                        number = index + 1
                    ));
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
            UiAction::RestoreDeletedPage => {
                if let Some((id, index, page)) = self.last_deleted_page.take() {
                    if id == self.manifest.id {
                        self.selection_state.clear();
                        let index = self.notebook.restore_page(index, page);
                        self.canvas.sync_page(self.notebook.current_page_mut());
                        self.manifest.sync_from_notebook(&self.notebook);
                        // Die Seitendatei neu schreiben (sie kann inzwischen fehlen)
                        self.autosave
                            .mark_page_dirty(self.notebook.current_page(), &self.manifest);
                        self.autosave.mark_manifest_dirty(&self.manifest);
                        self.ui_state.set_export_message(t!(
                            "Seite {number} wiederhergestellt",
                            number = index + 1
                        ));
                        update_window_title(
                            &self.window,
                            &self.notebook,
                            &self.manifest,
                            self.ui_state.active_tool,
                            self.autosave.status(),
                        );
                        self.window.request_redraw();
                    } else {
                        self.last_deleted_page = Some((id, index, page));
                    }
                }
            }
            UiAction::RequestPdfThumbnails(pdfs) => {
                *pdf_thumbnails_wanted = pdfs;
            }
            UiAction::LoadAllPages => {
                // Für die Vorschaubilder der Seitenauswahl; Fehler werden wie beim
                // Blättern behandelt (Ersatzseite, Warnung beim Öffnen der Seite)
                for idx in 0..self.notebook.pages.len() {
                    if !self.notebook.pages[idx].is_loaded {
                        let _ =
                            storage::ensure_page_loaded(&self.storage_dir, &mut self.notebook, idx);
                    }
                }
                self.window.request_redraw();
            }
            UiAction::GoToPage(idx) => {
                if idx < self.notebook.pages.len() {
                    self.selection_state.clear();
                    self.notebook.current_page_index = idx;
                    let _ = storage::ensure_page_loaded(&self.storage_dir, &mut self.notebook, idx);
                    self.canvas.sync_page(self.notebook.current_page_mut());
                    self.manifest.current_page_index = idx;
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
            }
            UiAction::SetBackground(bg) => {
                if self.ui_settings.page_background != Some(bg) {
                    self.ui_settings.page_background = Some(bg);
                    save_ui_settings(
                        &self.ui_settings,
                        self.ui_settings_path.as_deref(),
                        &mut self.ui_state,
                    );
                }
                let page = self.notebook.current_page_mut();
                page.background = bg;
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
            UiAction::ExportPdf => {
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

            UiAction::CreateFolder { parent, name } => {
                if let Some(path) =
                    change_library(&self.storage_base_dir, &mut self.ui_state, |library| {
                        library.create_folder(&parent, &name)
                    })
                {
                    self.ui_state.set_export_message(t!(
                        "Ordner „{name}“ angelegt",
                        name = crate::library::folder_name(&path)
                    ));
                }
                self.window.request_redraw();
            }
            UiAction::RenameFolder { path, new_name } => {
                if let Some(new_path) =
                    change_library(&self.storage_base_dir, &mut self.ui_state, |library| {
                        library.rename_folder(&path, &new_name)
                    })
                {
                    // Der gezeigte Ordner kann darin liegen
                    let shown = &self.ui_state.notebook_folder;
                    if shown == &path || shown.starts_with(&format!("{path}/")) {
                        self.ui_state.notebook_folder =
                            format!("{new_path}{}", &shown[path.len()..]);
                    }
                }
                self.window.request_redraw();
            }
            UiAction::DeleteFolder(path) => {
                if change_library(&self.storage_base_dir, &mut self.ui_state, |library| {
                    library.delete_folder(&path);
                    Ok(())
                })
                .is_some()
                {
                    self.ui_state.set_export_message(t!(
                        "Ordner „{name}“ gelöscht, sein Inhalt liegt eine Ebene höher",
                        name = crate::library::folder_name(&path)
                    ));
                }
                self.window.request_redraw();
            }
            UiAction::MoveFolder { path, new_parent } => {
                if let Some(new_path) =
                    change_library(&self.storage_base_dir, &mut self.ui_state, |library| {
                        library.move_folder(&path, &new_parent)
                    })
                {
                    // Der gezeigte Ordner kann darin liegen
                    let shown = &self.ui_state.notebook_folder;
                    if shown == &path || shown.starts_with(&format!("{path}/")) {
                        self.ui_state.notebook_folder =
                            format!("{new_path}{}", &shown[path.len()..]);
                    }
                    let target = if new_parent.is_empty() {
                        t!("die oberste Ebene").to_string()
                    } else {
                        format!("„{}“", crate::library::folder_name(&new_parent))
                    };
                    self.ui_state.set_export_message(t!(
                        "Ordner „{name}“ verschoben in {target}",
                        name = crate::library::folder_name(&path),
                        target = target
                    ));
                }
                self.window.request_redraw();
            }
            UiAction::MoveNotebook { id, folder } => {
                if change_library(&self.storage_base_dir, &mut self.ui_state, |library| {
                    library.move_notebook(&id, &folder)
                })
                .is_some()
                {
                    let target = if folder.is_empty() {
                        t!("die oberste Ebene").to_string()
                    } else {
                        format!("„{}“", crate::library::folder_name(&folder))
                    };
                    self.ui_state
                        .set_export_message(t!("Verschoben in {target}", target = target));
                }
                self.window.request_redraw();
            }
            UiAction::RefreshNotebooks => {
                refresh_notebooks(&self.storage_base_dir, &mut self.ui_state);
                self.window.request_redraw();
            }
            UiAction::OpenNotebook(id) => {
                if id == self.manifest.id {
                    self.ui_state.is_document_menu_open = false;
                    self.window.request_redraw();
                } else if !save_before_switch(
                    &mut self.canvas,
                    &mut self.notebook,
                    &mut self.manifest,
                    &mut self.autosave,
                    &mut self.ui_state,
                ) {
                    self.window.request_redraw();
                } else {
                    let new_notebook_dir = self.storage_base_dir.join(&id);
                    match storage::load_notebook(&new_notebook_dir) {
                        Ok((loaded_nb, loaded_manifest)) => {
                            match storage::NotebookLock::acquire(&new_notebook_dir) {
                                Ok(lock) => {
                                    self.notebook_lock = Some(lock);
                                    self.autosave.set_read_only(false);
                                }
                                Err(storage::StorageError::AlreadyLocked(msg)) => {
                                    eprintln!("[Storage-Lock] {msg}");
                                    self.notebook_lock = None;
                                    self.autosave.set_read_only(true);
                                    self.ui_state.set_export_message(t!(
                                        "⚠ In anderer Instanz geöffnet (Schreibschutz)"
                                    ));
                                }
                                Err(e) => {
                                    eprintln!("[Storage-Lock] Konnte Lock nicht erwerben: {e}");
                                    self.notebook_lock = None;
                                    self.autosave.set_read_only(true);
                                }
                            }
                            self.autosave.switch_notebook_dir(new_notebook_dir.clone());
                            self.notebook = loaded_nb;
                            self.manifest = loaded_manifest;
                            self.storage_dir = new_notebook_dir;
                            self.selection_state.clear();
                            self.ui_state.current_notebook_id = self.manifest.id.clone();
                            self.ui_state.document_title = self.manifest.title.clone();
                            self.ui_state.ensure_tab_open(&self.manifest.id);
                            refresh_notebooks(&self.storage_base_dir, &mut self.ui_state);
                            self.ui_settings.open_tabs = self.ui_state.open_tabs.clone();
                            save_ui_settings(
                                &self.ui_settings,
                                self.ui_settings_path.as_deref(),
                                &mut self.ui_state,
                            );
                            self.ui_state.is_document_menu_open = false;
                            self.canvas.sync_page(self.notebook.current_page_mut());
                            update_window_title(
                                &self.window,
                                &self.notebook,
                                &self.manifest,
                                self.ui_state.active_tool,
                                self.autosave.status(),
                            );
                            if self.notebook_lock.is_some() {
                                self.ui_state.set_export_message(t!(
                                    "Dokument '{title}' geöffnet",
                                    title = self.manifest.title
                                ));
                            }
                            warn_if_open_elsewhere(self.notebook_lock.as_ref(), &mut self.ui_state);
                            self.window.request_redraw();
                        }
                        Err(err) => {
                            eprintln!(
                                "[Dokument-Fehler] Konnte Dokument '{id}' nicht laden: {err}"
                            );
                            self.ui_state
                                .set_export_message(t!("Fehler beim Öffnen: {err}", err = err));
                        }
                    }
                }
            }
            UiAction::CloseTab(id) => {
                if id != self.manifest.id {
                    self.ui_state.close_tab(&id);
                    self.ui_settings.open_tabs = self.ui_state.open_tabs.clone();
                    save_ui_settings(
                        &self.ui_settings,
                        self.ui_settings_path.as_deref(),
                        &mut self.ui_state,
                    );
                    self.window.request_redraw();
                } else if self.ui_state.open_tabs.len() > 1 {
                    let pos = self
                        .ui_state
                        .open_tabs
                        .iter()
                        .position(|t| t == &id)
                        .unwrap_or(0);
                    let next_id = if pos + 1 < self.ui_state.open_tabs.len() {
                        self.ui_state.open_tabs[pos + 1].clone()
                    } else {
                        self.ui_state.open_tabs[pos.saturating_sub(1)].clone()
                    };
                    self.handle_ui_action(
                        UiAction::OpenNotebook(next_id),
                        pdf_thumbnails_wanted,
                        target,
                    );
                    if self.manifest.id != id {
                        self.ui_state.close_tab(&id);
                        self.ui_settings.open_tabs = self.ui_state.open_tabs.clone();
                        save_ui_settings(
                            &self.ui_settings,
                            self.ui_settings_path.as_deref(),
                            &mut self.ui_state,
                        );
                        self.window.request_redraw();
                    }
                }
            }
            UiAction::ImportFile(path) => {
                if save_before_switch(
                    &mut self.canvas,
                    &mut self.notebook,
                    &mut self.manifest,
                    &mut self.autosave,
                    &mut self.ui_state,
                ) {
                    // Neben das gerade geöffnete Notizbuch
                    let folder = self
                        .ui_state
                        .library
                        .folder_of(&self.manifest.id)
                        .to_string();
                    let title = path
                        .file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "Import".to_string());
                    match crate::import::import_file(&self.storage_base_dir, &title, &path) {
                        Ok((new_nb, new_manifest, new_dir, report)) => {
                            let id = new_manifest.id.clone();
                            change_library(&self.storage_base_dir, &mut self.ui_state, |library| {
                                library.move_notebook(&id, &folder)
                            });
                            self.notebook_lock = storage::NotebookLock::acquire(&new_dir).ok();
                            self.autosave.set_read_only(self.notebook_lock.is_none());
                            self.autosave.switch_notebook_dir(new_dir.clone());
                            self.notebook = new_nb;
                            self.manifest = new_manifest;
                            self.storage_dir = new_dir;
                            self.selection_state.clear();
                            self.ui_state.current_notebook_id = self.manifest.id.clone();
                            self.ui_state.document_title = self.manifest.title.clone();
                            self.ui_state.ensure_tab_open(&self.manifest.id);
                            refresh_notebooks(&self.storage_base_dir, &mut self.ui_state);
                            self.ui_settings.open_tabs = self.ui_state.open_tabs.clone();
                            save_ui_settings(
                                &self.ui_settings,
                                self.ui_settings_path.as_deref(),
                                &mut self.ui_state,
                            );
                            self.canvas.reset_view();
                            self.canvas.sync_page(self.notebook.current_page_mut());
                            update_window_title(
                                &self.window,
                                &self.notebook,
                                &self.manifest,
                                self.ui_state.active_tool,
                                self.autosave.status(),
                            );
                            let mut message = t!(
                                "'{title}' importiert ({count} Seiten)",
                                title = self.manifest.title,
                                count = self.notebook.pages.len()
                            );
                            if let Some(pdf) = &report.missing_pdf {
                                message.push_str(&t!(
                                    ", PDF-Hintergrund nicht gefunden: {file}",
                                    file = pdf
                                ));
                            }
                            if report.skipped > 0 {
                                message.push_str(&t!(
                                    ", {count} Elemente übersprungen",
                                    count = report.skipped
                                ));
                            }
                            self.ui_state.set_export_message(message);
                        }
                        Err(err) => {
                            eprintln!("[Import] '{}': {err}", path.display());
                            self.ui_state
                                .set_export_message(t!("Nicht importiert: {err}", err = err));
                        }
                    }
                }
                self.window.request_redraw();
            }
            UiAction::CreateNotebook {
                title,
                folder,
                layout,
            } => {
                if save_before_switch(
                    &mut self.canvas,
                    &mut self.notebook,
                    &mut self.manifest,
                    &mut self.autosave,
                    &mut self.ui_state,
                ) {
                    match storage::create_new_notebook(&self.storage_base_dir, &title) {
                        Ok((mut new_nb, mut new_manifest, new_dir)) => {
                            let id = new_manifest.id.clone();
                            new_manifest.layout = Some(layout);
                            if let Some(bg) = self.ui_settings.page_background {
                                new_nb.pages[0].background = bg;
                            }
                            if layout == NotebookLayout::Endless {
                                new_nb.make_endless();
                            }
                            change_library(&self.storage_base_dir, &mut self.ui_state, |library| {
                                library.move_notebook(&id, &folder)
                            });
                            self.notebook_lock = storage::NotebookLock::acquire(&new_dir).ok();
                            self.autosave.set_read_only(self.notebook_lock.is_none());
                            self.autosave.switch_notebook_dir(new_dir.clone());
                            self.notebook = new_nb;
                            self.manifest = new_manifest;
                            self.storage_dir = new_dir;
                            self.autosave.mark_dirty(&self.notebook, &mut self.manifest);
                            self.canvas.reset_view();
                            self.selection_state.clear();
                            self.ui_state.current_notebook_id = self.manifest.id.clone();
                            self.ui_state.document_title = self.manifest.title.clone();
                            self.ui_state.ensure_tab_open(&self.manifest.id);
                            refresh_notebooks(&self.storage_base_dir, &mut self.ui_state);
                            self.ui_settings.open_tabs = self.ui_state.open_tabs.clone();
                            save_ui_settings(
                                &self.ui_settings,
                                self.ui_settings_path.as_deref(),
                                &mut self.ui_state,
                            );
                            self.ui_state.is_document_menu_open = false;
                            self.canvas.sync_page(self.notebook.current_page_mut());
                            update_window_title(
                                &self.window,
                                &self.notebook,
                                &self.manifest,
                                self.ui_state.active_tool,
                                self.autosave.status(),
                            );
                            self.ui_state.set_export_message(t!(
                                "Dokument '{title}' erstellt",
                                title = self.manifest.title
                            ));
                            self.window.request_redraw();
                        }
                        Err(err) => {
                            eprintln!("[Dokument-Fehler] Konnte Dokument '{title}' nicht erstellen: {err}");
                            self.ui_state
                                .set_export_message(t!("Fehler beim Erstellen: {err}", err = err));
                        }
                    }
                } else {
                    self.window.request_redraw();
                }
            }
            UiAction::RenameNotebook { id, new_title } => {
                let target_dir = self.storage_base_dir.join(&id);
                if id == self.manifest.id {
                    match storage::rename_notebook(&target_dir, &mut self.manifest, &new_title) {
                        Ok(()) => {
                            self.ui_state.document_title = self.manifest.title.clone();
                            refresh_notebooks(&self.storage_base_dir, &mut self.ui_state);
                            self.manifest.sync_from_notebook(&self.notebook);
                            self.autosave.mark_manifest_dirty(&self.manifest);
                            update_window_title(
                                &self.window,
                                &self.notebook,
                                &self.manifest,
                                self.ui_state.active_tool,
                                self.autosave.status(),
                            );
                            self.ui_state.set_export_message(t!(
                                "Umbenannt in '{title}'",
                                title = new_title
                            ));
                            self.window.request_redraw();
                        }
                        Err(err) => {
                            eprintln!(
                                "[Dokument-Fehler] Konnte Dokument '{id}' nicht umbenennen: {err}"
                            );
                            self.ui_state
                                .set_export_message(t!("Fehler beim Umbenennen: {err}", err = err));
                        }
                    }
                } else {
                    let manifest_path = target_dir.join("manifest.json");
                    if let Ok(content) = std::fs::read_to_string(&manifest_path) {
                        if let Ok(mut other_manifest) =
                            serde_json::from_str::<NotebookManifest>(&content)
                        {
                            let _ = storage::rename_notebook(
                                &target_dir,
                                &mut other_manifest,
                                &new_title,
                            );
                            refresh_notebooks(&self.storage_base_dir, &mut self.ui_state);
                            self.ui_state.set_export_message(t!(
                                "Umbenannt in '{title}'",
                                title = new_title
                            ));
                            self.window.request_redraw();
                        }
                    }
                }
            }
            UiAction::DeleteNotebook(id) => {
                let target_dir = self.storage_base_dir.join(&id);
                let is_current = id == self.manifest.id;
                let deleted_title = self
                    .ui_state
                    .available_notebooks
                    .iter()
                    .find(|s| s.id == id)
                    .map(|s| s.title.clone())
                    .unwrap_or_else(|| id.clone());

                if is_current {
                    finish_text_edit(
                        &mut self.canvas,
                        &mut self.notebook,
                        &mut self.manifest,
                        &mut self.autosave,
                        &mut self.ui_state,
                    );
                    if let Err(err) = self.autosave.flush() {
                        eprintln!(
                            "[Autosave] Not saved before deleting: {}",
                            err.messages.join(", ")
                        );
                    }
                    // Nothing may be written into the folder once it is in the trash
                    self.autosave.abandon_pending();
                    drop(self.notebook_lock.take());
                }

                match storage::delete_notebook(&target_dir, &self.storage_base_dir) {
                    Ok(trash_path) => {
                        self.ui_state.last_deleted_notebook = Some((trash_path, deleted_title));
                        self.ui_state.open_tabs.retain(|t| t != &id);
                        refresh_notebooks(&self.storage_base_dir, &mut self.ui_state);
                        if is_current {
                            let next_candidate = self
                                .ui_state
                                .open_tabs
                                .iter()
                                .find_map(|tab_id| {
                                    self.ui_state.available_notebooks.iter().find(|s| {
                                        s.id == *tab_id
                                            && s.status != storage::NotebookStatus::Damaged
                                    })
                                })
                                .or_else(|| {
                                    self.ui_state
                                        .available_notebooks
                                        .iter()
                                        .find(|s| s.status != storage::NotebookStatus::Damaged)
                                })
                                .cloned();
                            if let Some(first) = next_candidate {
                                let new_notebook_dir = self.storage_base_dir.join(&first.id);
                                match storage::load_notebook(&new_notebook_dir) {
                                    Ok((loaded_nb, loaded_manifest)) => {
                                        self.notebook_lock =
                                            storage::NotebookLock::acquire(&new_notebook_dir).ok();
                                        self.autosave.set_read_only(self.notebook_lock.is_none());
                                        self.autosave.switch_notebook_dir(new_notebook_dir.clone());
                                        self.notebook = loaded_nb;
                                        self.manifest = loaded_manifest;
                                        self.storage_dir = new_notebook_dir;
                                        self.selection_state.clear();
                                        self.ui_state.current_notebook_id =
                                            self.manifest.id.clone();
                                        self.ui_state.document_title = self.manifest.title.clone();
                                        self.ui_state.ensure_tab_open(&self.manifest.id);
                                        self.ui_settings.open_tabs =
                                            self.ui_state.open_tabs.clone();
                                        save_ui_settings(
                                            &self.ui_settings,
                                            self.ui_settings_path.as_deref(),
                                            &mut self.ui_state,
                                        );
                                        self.canvas.sync_page(self.notebook.current_page_mut());
                                        update_window_title(
                                            &self.window,
                                            &self.notebook,
                                            &self.manifest,
                                            self.ui_state.active_tool,
                                            self.autosave.status(),
                                        );
                                        self.ui_state.set_export_message(t!(
                                            "Gewechselt zu '{title}'",
                                            title = self.manifest.title
                                        ));
                                        warn_if_open_elsewhere(
                                            self.notebook_lock.as_ref(),
                                            &mut self.ui_state,
                                        );
                                    }
                                    Err(err) => {
                                        eprintln!("[Dokument-Fehler] Konnte Dokument '{}' nicht laden: {err}", first.id);
                                    }
                                }
                            } else if let Ok((new_nb, new_manifest, new_dir)) =
                                storage::create_new_notebook(
                                    &self.storage_base_dir,
                                    t!("Standard-Notizbuch"),
                                )
                            {
                                self.notebook_lock = storage::NotebookLock::acquire(&new_dir).ok();
                                self.autosave.set_read_only(self.notebook_lock.is_none());
                                self.autosave.switch_notebook_dir(new_dir.clone());
                                self.notebook = new_nb;
                                self.manifest = new_manifest;
                                self.storage_dir = new_dir;
                                self.selection_state.clear();
                                self.ui_state.current_notebook_id = self.manifest.id.clone();
                                self.ui_state.document_title = self.manifest.title.clone();
                                self.ui_state.ensure_tab_open(&self.manifest.id);
                                refresh_notebooks(&self.storage_base_dir, &mut self.ui_state);
                                self.ui_settings.open_tabs = self.ui_state.open_tabs.clone();
                                save_ui_settings(
                                    &self.ui_settings,
                                    self.ui_settings_path.as_deref(),
                                    &mut self.ui_state,
                                );
                                self.canvas.sync_page(self.notebook.current_page_mut());
                                update_window_title(
                                    &self.window,
                                    &self.notebook,
                                    &self.manifest,
                                    self.ui_state.active_tool,
                                    self.autosave.status(),
                                );
                                self.ui_state.set_export_message(
                                    t!("Standard-Notizbuch erstellt").to_string(),
                                );
                            }
                        } else {
                            self.ui_state.set_export_message(
                                t!("Dokument in Papierkorb verschoben").to_string(),
                            );
                        }
                        self.window.request_redraw();
                    }
                    Err(err) => {
                        eprintln!("[Dokument-Fehler] Konnte Dokument '{id}' nicht löschen: {err}");
                        self.ui_state
                            .set_export_message(t!("Fehler beim Löschen: {err}", err = err));
                    }
                }
            }
            UiAction::UndoDeleteNotebook => {
                if let Some((trash_path, _title)) = self.ui_state.last_deleted_notebook.take() {
                    match storage::restore_notebook_from_trash(&self.storage_base_dir, &trash_path)
                    {
                        Ok(_restored_dir) => {
                            refresh_notebooks(&self.storage_base_dir, &mut self.ui_state);
                            self.ui_state
                                .set_export_message(t!("Notizbuch wiederhergestellt").to_string());
                            self.window.request_redraw();
                        }
                        Err(err) => {
                            eprintln!(
                                "[Dokument-Fehler] Konnte Notizbuch nicht wiederherstellen: {err}"
                            );
                            self.ui_state.set_export_message(t!(
                                "Fehler beim Wiederherstellen: {err}",
                                err = err
                            ));
                        }
                    }
                }
            }
            UiAction::RestoreNotebookBackup(id) => {
                let target_dir = self.storage_base_dir.join(&id);
                match storage::restore_manifest_from_backup(&target_dir) {
                    Ok(()) => {
                        refresh_notebooks(&self.storage_base_dir, &mut self.ui_state);
                        self.ui_state.set_export_message(
                            t!("Manifest aus Backup wiederhergestellt").to_string(),
                        );
                        self.window.request_redraw();
                    }
                    Err(err) => {
                        eprintln!("[Dokument-Fehler] Konnte Backup für '{id}' nicht wiederherstellen: {err}");
                        self.ui_state.set_export_message(t!(
                            "Wiederherstellung fehlgeschlagen: {err}",
                            err = err
                        ));
                    }
                }
            }
            UiAction::OpenNotebookFolder(id) => {
                let target_dir = self.storage_base_dir.join(&id);
                self.ui_state
                    .set_export_message(t!("Ordner: {path}", path = target_dir.display()));
                self.window.request_redraw();
            }
            UiAction::CheckForUpdates => {
                self.updater.check_for_updates();
                self.window.request_redraw();
            }
            UiAction::DownloadUpdateAsset(asset) => {
                self.updater.download_asset(asset);
                self.window.request_redraw();
            }
            UiAction::InstallOrOpenUpdate(path) => {
                if let Err(err) = self.updater.install_or_open(&path) {
                    self.ui_state
                        .set_export_message(t!("Update-Fehler: {err}", err = err));
                } else if matches!(
                    self.updater.runtime(),
                    crate::updater::RuntimeEnvironment::WindowsExe { .. }
                ) {
                    self.ui_state
                        .set_export_message(t!("Update installiert – gilt ab dem nächsten Start"));
                } else {
                    self.ui_state
                        .set_export_message(t!("Update gestartet / geöffnet"));
                }
                self.window.request_redraw();
            }
            UiAction::CloudConnect { server, trusted } => {
                self.cloud.connect(&server, trusted);
                self.window.request_redraw();
            }
            UiAction::CloudCancel => {
                self.cloud.cancel_login();
                self.window.request_redraw();
            }
            UiAction::CloudSyncNow => {
                let _ = self.autosave.flush();
                self.cloud.sync_now();
                self.window.request_redraw();
            }
            UiAction::CloudDisconnect => {
                self.cloud.disconnect();
                self.ui_state.set_export_message(
                    t!("Nextcloud getrennt – die Notizbücher bleiben auf diesem Gerät").to_string(),
                );
                self.window.request_redraw();
            }
            UiAction::OpenUrlInBrowser(url) => {
                let _ = webbrowser::open(&url);
            }
            UiAction::SaveUpdaterConfig {
                server_url,
                auth_token,
            } => {
                self.updater.set_server_url(server_url);
                self.updater.set_auth_token(auth_token);
                self.ui_state
                    .set_export_message(t!("Server-Einstellungen gespeichert"));
                self.window.request_redraw();
            }
            UiAction::UnlockCorruptedPage(page_id) => {
                self.notebook.load_warnings.remove(&page_id);
                if let Some(page) = self.notebook.pages.iter_mut().find(|p| p.id == page_id) {
                    page.unlock_after_load_error();
                }
                self.autosave.mark_dirty(&self.notebook, &mut self.manifest);
                self.window.request_redraw();
            }
            UiAction::RetryShutdown => {
                println!("[Shutdown] Wiederhole Speichern vor Beenden...");
                match self.autosave.shutdown() {
                    Ok(()) => {
                        self.ui_state.shutdown_error = None;
                        target.exit();
                    }
                    Err(err) => {
                        self.ui_state.shutdown_error = Some(err);
                        self.window.request_redraw();
                    }
                }
            }
            UiAction::ExportRecoveryAndExit => {
                println!("[Shutdown] Starte Notfall-Export aller ungespeicherten Daten...");
                match self.autosave.export_recovery(Some(&self.notebook)) {
                    Ok(paths) => {
                        eprintln!(
                            "[Recovery] Notfall-Export erfolgreich geschrieben ({} Dateien): {:?}",
                            paths.len(),
                            paths
                        );
                        target.exit();
                    }
                    Err(err) => {
                        eprintln!("[Recovery-Fehler] Konnte Notfall-Export nicht schreiben: {err}");
                        if let Some(ref mut se) = self.ui_state.shutdown_error {
                            se.messages
                                .push(t!("Notfall-Export fehlgeschlagen: {err}", err = err));
                        }
                        self.window.request_redraw();
                    }
                }
            }
            UiAction::SetMathAssist(on) => {
                self.ui_settings.math_assist = on;
                save_ui_settings(
                    &self.ui_settings,
                    self.ui_settings_path.as_deref(),
                    &mut self.ui_state,
                );
            }
            UiAction::SetTouchDraw(on) => {
                self.ui_settings.touch_draw = on;
                save_ui_settings(
                    &self.ui_settings,
                    self.ui_settings_path.as_deref(),
                    &mut self.ui_state,
                );
            }
            UiAction::SetSidebarSide(side) => {
                self.ui_settings.sidebar_side = side;
                save_ui_settings(
                    &self.ui_settings,
                    self.ui_settings_path.as_deref(),
                    &mut self.ui_state,
                );
                self.window.request_redraw();
            }
            UiAction::SetStorageDir(chosen) => {
                let new_base = match &chosen {
                    Some(dir) => storage::storage_dir_in(dir),
                    None => storage::default_storage_dir(),
                };
                if new_base == self.storage_base_dir {
                    self.ui_state
                        .set_export_message(t!("Die Notizbücher liegen bereits dort").to_string());
                } else if save_before_switch(
                    &mut self.canvas,
                    &mut self.notebook,
                    &mut self.manifest,
                    &mut self.autosave,
                    &mut self.ui_state,
                ) {
                    match storage::copy_storage(&self.storage_base_dir, &new_base) {
                        Ok(copied) => {
                            // Release lock and open marker in the old location
                            self.notebook_lock = None;
                            self.storage_base_dir = new_base;
                            self.ui_settings.storage_dir =
                                chosen.map(|_| self.storage_base_dir.clone());
                            save_ui_settings(
                                &self.ui_settings,
                                self.ui_settings_path.as_deref(),
                                &mut self.ui_state,
                            );
                            self.ui_state.storage_dir = self.storage_base_dir.clone();
                            self.ui_state.storage_dir_custom =
                                self.ui_settings.storage_dir.is_some();
                            self.cloud.set_local_dir(self.storage_base_dir.clone());
                            refresh_notebooks(&self.storage_base_dir, &mut self.ui_state);
                            // Same notebook if it exists there, otherwise the first one
                            let open_id = self
                                .ui_state
                                .available_notebooks
                                .iter()
                                .filter(|s| s.status == storage::NotebookStatus::Normal)
                                .find(|s| s.id == self.manifest.id)
                                .or_else(|| {
                                    self.ui_state
                                        .available_notebooks
                                        .iter()
                                        .find(|s| s.status == storage::NotebookStatus::Normal)
                                })
                                .map(|s| s.id.clone());
                            let loaded = match open_id {
                                Some(id) => {
                                    let dir = self.storage_base_dir.join(&id);
                                    storage::load_notebook(&dir).map(|(nb, m)| (nb, m, dir))
                                }
                                None => storage::create_new_notebook(
                                    &self.storage_base_dir,
                                    t!("Standard-Notizbuch"),
                                ),
                            };
                            match loaded {
                                Ok((new_nb, new_manifest, new_dir)) => {
                                    self.notebook_lock =
                                        storage::NotebookLock::acquire(&new_dir).ok();
                                    self.autosave.set_read_only(self.notebook_lock.is_none());
                                    self.autosave.switch_notebook_dir(new_dir.clone());
                                    self.notebook = new_nb;
                                    self.manifest = new_manifest;
                                    self.storage_dir = new_dir;
                                    self.selection_state.clear();
                                    self.ui_state.current_notebook_id = self.manifest.id.clone();
                                    self.ui_state.document_title = self.manifest.title.clone();
                                    refresh_notebooks(&self.storage_base_dir, &mut self.ui_state);
                                    self.canvas.sync_page(self.notebook.current_page_mut());
                                    update_window_title(
                                        &self.window,
                                        &self.notebook,
                                        &self.manifest,
                                        self.ui_state.active_tool,
                                        self.autosave.status(),
                                    );
                                    self.ui_state.set_export_message(if copied > 0 {
                                        t!(
                                            "Speicherort geändert, {count} Notizbücher kopiert",
                                            count = copied
                                        )
                                    } else {
                                        t!(
                                            "Speicherort geändert: {path}",
                                            path = self.storage_base_dir.display()
                                        )
                                    });
                                    warn_if_open_elsewhere(
                                        self.notebook_lock.as_ref(),
                                        &mut self.ui_state,
                                    );
                                }
                                Err(err) => {
                                    // The old notebook stays open but is saved nowhere
                                    self.autosave.set_read_only(true);
                                    eprintln!("[Storage] Notizbuch am neuen Speicherort nicht geöffnet: {err}");
                                    self.ui_state.set_export_message(t!(
                                        "Fehler beim Öffnen: {err}",
                                        err = err
                                    ));
                                }
                            }
                        }
                        Err(err) => {
                            eprintln!(
                                "[Storage] Speicherort '{}' nicht nutzbar: {err}",
                                new_base.display()
                            );
                            self.ui_state.set_export_message(t!(
                                "Speicherort nicht nutzbar: {err}",
                                err = err
                            ));
                        }
                    }
                }
                self.window.request_redraw();
            }
            UiAction::SetLanguage(language) => {
                self.ui_settings.language = language;
                i18n::set_language(language.unwrap_or_else(Language::system));
                save_ui_settings(
                    &self.ui_settings,
                    self.ui_settings_path.as_deref(),
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
            UiAction::SetPaperTheme { theme, adapt_ink } => {
                self.canvas.set_paper_theme(theme, adapt_ink);
                self.canvas.sync_page(self.notebook.current_page_mut());
                self.ui_settings.paper_theme = theme;
                self.ui_settings.adapt_ink_colors = adapt_ink;
                save_ui_settings(
                    &self.ui_settings,
                    self.ui_settings_path.as_deref(),
                    &mut self.ui_state,
                );
                self.window.request_redraw();
            }
            UiAction::SetPatternColor(pattern) => {
                self.canvas.set_pattern_color(pattern);
                self.ui_settings.pattern_color = pattern;
                save_ui_settings(
                    &self.ui_settings,
                    self.ui_settings_path.as_deref(),
                    &mut self.ui_state,
                );
                self.window.request_redraw();
            }
            UiAction::ScrollTo(y) => {
                self.fling.stop();
                self.canvas.scroll_center_to(y);
                self.window.request_redraw();
            }
            UiAction::SetNotebookLayout(layout) => {
                // Von und zur endlosen Fläche nur mit einer Seite, deren Inhalt passt
                let was_endless = self.notebook.is_endless();
                let to_endless = layout == NotebookLayout::Endless;
                let converted = match (was_endless, to_endless) {
                    (false, true) => self.notebook.make_endless(),
                    (true, false) => self.notebook.make_paged(),
                    _ => true,
                };
                if !converted {
                    self.ui_state.set_export_message(if to_endless {
                        t!("Endlos geht nur bei einem Notizbuch mit einer einzigen Seite ohne PDF")
                            .to_string()
                    } else {
                        t!("Zurück zu Seiten geht nur, solange alles im Startausschnitt liegt")
                            .to_string()
                    });
                } else {
                    self.manifest.layout = Some(layout);
                    self.ui_state.endless = to_endless;
                    self.ui_state.continuous_scroll = layout == NotebookLayout::Continuous;
                    if was_endless != to_endless {
                        self.selection_state.clear();
                        self.canvas.sync_page(self.notebook.current_page_mut());
                        self.canvas.reset_view();
                        self.autosave.mark_dirty(&self.notebook, &mut self.manifest);
                    } else {
                        self.autosave.mark_manifest_dirty(&self.manifest);
                    }
                }
                self.window.request_redraw();
            }
            UiAction::SaveStylusButtons => {
                self.ui_settings.stylus_buttons = self.ui_state.stylus_buttons.clone();
                save_ui_settings(
                    &self.ui_settings,
                    self.ui_settings_path.as_deref(),
                    &mut self.ui_state,
                );
            }
            UiAction::SavePressureCurve(curve) => {
                self.ui_settings.pressure_curve = curve;
                save_ui_settings(
                    &self.ui_settings,
                    self.ui_settings_path.as_deref(),
                    &mut self.ui_state,
                );
            }
            UiAction::ForceExit => {
                println!("[Shutdown] Beenden erzwungen trotz ungespeicherter Änderungen.");
                target.exit();
            }
        }
    }
}
