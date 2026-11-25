//! Frame-Rendering (RedrawRequested): Scroll-Abgleich, egui-Pass, Action-Dispatch und wgpu-Pass.

use std::time::{Duration, Instant};

use glam::Vec2;
use winit::event_loop::ActiveEventLoop;

use crate::assets::AssetStore;
use crate::canvas::RenderError;
use crate::osk;
use crate::platform;
use crate::selection::{render_selection_overlay, DragMode};
use crate::settings::SidebarSide;
use crate::storage;
use crate::ui;

use super::helpers::*;
use super::AppState;

impl AppState {
    pub(super) fn handle_redraw_requested(&mut self, target: &ActiveEventLoop) {
        flush_selection_drag(
            self.notebook.current_page_mut(),
            &mut self.canvas,
            &mut self.selection_state,
        );
        // 0. Durchgehende Ansicht: Nach dem Scrollen oder Zoomen wird die Seite in
        //    der Fenstermitte zur aktuellen (nicht während eines Strichs, einer
        //    Auswahl oder beim Ziehen des Lineals). Wer auf einer Nachbarseite
        //    schreibt, bleibt dort, bis wieder gescrollt wird (Undo betrifft sie).
        self.canvas.set_fit_box(self.notebook.fit_box());
        if self.notebook.is_endless() {
            if self
                .canvas
                .clamp_endless(self.notebook.current_page().endless_scroll_limit())
                && self.fling.is_active()
            {
                self.fling.stop();
            }
        } else if self.ui_state.continuous_scroll {
            let stack = self.notebook.page_stack();
            if self.canvas.clamp_scroll(&stack) && self.fling.is_active() {
                // Am Anfang oder Ende des Notizbuchs angekommen (only a glide stops
                // here; a finger still on the screen keeps its measured swipe)
                self.fling.stop();
            }
            // Unabhängig von der Bezugsseite: Lage bezogen auf die erste Seite
            let scroll = [
                self.canvas.camera.zoom,
                self.canvas.camera.pan_offset[0],
                self.canvas.camera.pan_offset[1] + stack.origins[0].y,
            ];
            let scrolled = self.last_scroll.is_none_or(|last: [f32; 3]| {
                last.iter().zip(scroll).any(|(a, b)| (a - b).abs() > 1e-3)
            });
            let busy = self.is_drawing
                || self.is_erasing
                || self.ui_state.space_drag.is_some()
                || !self.selection_state.is_empty()
                || self.selection_state.drag_mode != DragMode::None
                || self.ui_state.ruler_drag.is_some()
                || self.ui_state.text_edit.is_some();
            if scrolled && !busy {
                self.last_scroll = Some(scroll);
                let target = self
                    .canvas
                    .page_at_view_center(&stack)
                    .filter(|&i| i != self.notebook.current_page_index);
                if let Some(index) = target {
                    switch_to_page(
                        index,
                        true,
                        &mut self.canvas,
                        &mut self.notebook,
                        &mut self.manifest,
                        &mut self.autosave,
                        &self.storage_dir,
                        &mut self.selection_state,
                        &mut self.ui_state,
                        &self.window,
                    );
                }
            }
        }

        // 1. egui Eingabe erfassen und Frame starten
        let mut raw_input = self.egui_winit_state.take_egui_input(&self.window);
        osk::plain_ime_commits_to_text(&mut raw_input.events, &mut self.ime_composing);
        if let Some(requester) = self.keyboard_requester.as_mut() {
            raw_input.events.extend(requester.take_deletions());
        }
        // Android: text from the keyboard and files from the system dialog
        #[cfg(target_os = "android")]
        {
            let (events, actions) = crate::android_bridge::take_messages();
            raw_input.events.extend(events);
            self.ui_state.queued_actions.extend(actions);
        }
        // Keep the UI out of system bars and cutouts (Android, Issue #50)
        let safe_area = platform::safe_area_insets();
        if let Some(rect) = raw_input.screen_rect.filter(|_| safe_area != [0.0; 4]) {
            let ppp = self.egui_ctx.pixels_per_point();
            let [left, top, right, bottom] = safe_area.map(|v| v / ppp);
            raw_input.screen_rect = Some(egui::Rect::from_min_max(
                rect.min + egui::vec2(left, top),
                rect.max - egui::vec2(right, bottom),
            ));
        }
        self.egui_ctx.begin_pass(raw_input);

        // 2. Auswahl-Overlay rendern (Bounding Box, Griffe, Lasso)
        render_selection_overlay(
            &self.egui_ctx,
            &self.selection_state,
            self.notebook.current_page(),
            &self.canvas,
        );

        // 3. Toolbar & Widgets rendern
        self.updater.update();
        self.ui_state.page_view = Some(page_view(&self.canvas, self.window.scale_factor() as f32));
        self.ui_state.selection_aspect_lock = (!self.selection_state.is_empty()).then(|| {
            self.selection_state
                .keeps_aspect(self.notebook.current_page())
        });
        self.ui_state.restorable_page = self
            .last_deleted_page
            .as_ref()
            .filter(|(id, _, _)| *id == self.manifest.id)
            .map(|(_, index, _)| index + 1);
        self.ui_state.cloud = self.cloud.view();
        self.ui_state.ocr_status = self.ocr.status().clone();
        let actions = ui::render_ui(
            &self.egui_ctx,
            &mut self.ui_state,
            &self.notebook,
            self.autosave.status(),
            &self.updater,
        );
        self.canvas.ruler = self.ui_state.ruler;

        // Seite neben der Werkzeugleiste zentrieren (Leistenbreite in physischen Pixeln)
        let reserved = self.ui_state.sidebar_reserved_width() * self.window.scale_factor() as f32;
        let (inset_left, inset_right) = match self.ui_state.sidebar_side {
            SidebarSide::Left => (reserved + safe_area[0], safe_area[2]),
            SidebarSide::Right => (safe_area[0], reserved + safe_area[2]),
        };
        if self.canvas.set_ui_insets(inset_left, inset_right) {
            self.window.request_redraw();
        }

        // 4. UI-Aktionen verarbeiten
        let mut pdf_thumbnails_wanted = Vec::new();

        for action in actions {
            self.handle_ui_action(action, &mut pdf_thumbnails_wanted, target);
        }

        // Stifte und eigene Farben: geänderte Farbe oder Stärke gehört zum gewählten
        // Stift bzw. zur gewählten Farbe. Gespeichert wird erst nach dem Loslassen
        // (nicht bei jedem Schritt eines Schiebereglers oder Farbwählers).
        self.ui_state.sync_tool_colors();
        if !self.egui_ctx.egui_is_using_pointer() {
            let mut updated = self.ui_settings.clone();
            updated.set_pens(self.ui_state.pen_presets);
            updated.active_pen = self.ui_state.active_pen;
            updated.highlighter_colors = self.ui_state.highlighter_colors;
            updated.active_highlighter = self.ui_state.active_highlighter;
            updated.text_colors = self.ui_state.text_colors;
            updated.active_text_color = self.ui_state.active_text_color;
            updated.eraser_radius_mm = self.ui_state.eraser_radius_mm;
            updated.saved_colors.clone_from(&self.ui_state.saved_colors);
            if updated != self.ui_settings {
                self.ui_settings = updated;
                save_ui_settings(
                    &self.ui_settings,
                    self.ui_settings_path.as_deref(),
                    &mut self.ui_state,
                );
            }
        }

        // Textbearbeitung beenden: Esc, anderes Werkzeug oder andere Seite
        let finish_text = self.ui_state.text_edit.as_ref().is_some_and(|edit| {
            edit.finish
                || self.ui_state.active_tool != crate::ui::ActiveTool::Text
                || edit.page_id != self.notebook.current_page().id
        });
        if finish_text {
            finish_text_edit(
                &mut self.canvas,
                &mut self.notebook,
                &mut self.manifest,
                &mut self.autosave,
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

        // 4. egui Frame beenden & Primitives tessellieren
        let mut full_output = self.egui_ctx.end_pass();
        let ime_active = full_output.platform_output.ime.is_some();
        for cmd in &full_output.platform_output.commands {
            if let egui::OutputCommand::CopyText(text) = cmd {
                if !text.is_empty() {
                    self.clipboard.set_system_text(text.clone());
                }
            }
        }
        self.egui_winit_state
            .handle_platform_output(&self.window, full_output.platform_output);
        if let Some(requester) = self.keyboard_requester.as_mut() {
            let pressed = self.egui_ctx.input(|i| i.pointer.any_pressed());
            if osk::should_request(self.ime_was_active, ime_active, pressed) {
                requester.request();
            } else if self.ime_was_active && !ime_active {
                requester.release();
            }
        }
        self.ime_was_active = ime_active;

        // egui braucht Folge-Frames, z. B. um neu angeordnete Bereiche zu vermessen
        // (die Werkzeugleiste wird erst im 2.–3. Frame korrekt zentriert)
        let repaint_delay = full_output
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .map(|v| v.repaint_delay)
            .unwrap_or(Duration::MAX);
        if repaint_delay.is_zero() {
            self.egui_repaint_at = None;
            self.window.request_redraw();
        } else if repaint_delay < Duration::from_secs(10) {
            self.egui_repaint_at = Some(Instant::now() + repaint_delay);
        } else {
            self.egui_repaint_at = None;
        }

        let clipped_primitives = self
            .egui_ctx
            .tessellate(full_output.shapes, self.egui_ctx.pixels_per_point());
        let screen_descriptor = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [self.canvas.size.width, self.canvas.size.height],
            pixels_per_point: self.egui_ctx.pixels_per_point(),
        };

        // The toast only changes while it fades out (its last second); until
        // then a single wake-up at the start of the fade is enough
        if let Some((_, shown)) = &self.ui_state.export_message {
            let fade = *shown + Duration::from_secs(3);
            if Instant::now() >= fade {
                self.window.request_redraw();
            } else {
                self.egui_repaint_at = Some(self.egui_repaint_at.map_or(fade, |at| at.min(fade)));
            }
        }

        // 5. Canvas (Pass 1) und egui UI (Pass 2) in Surface-Textur rendern
        //    Durchgehende Ansicht: sichtbare Nachbarseiten laden und mitzeichnen
        if self.ui_state.continuous_scroll {
            let stack = self.notebook.page_stack();
            let visible = self.canvas.visible_neighbors(&stack);
            for &index in &visible {
                let _ = storage::ensure_page_loaded(&self.storage_dir, &mut self.notebook, index);
                self.notebook.pages[index].rebuild_cache_if_dirty();
            }
            let neighbors: Vec<(Vec2, &crate::document::Page)> = visible
                .iter()
                .map(|&index| (stack.origins[index], &self.notebook.pages[index]))
                .collect();
            self.canvas.set_neighbor_pages(&neighbors);
        } else {
            self.canvas.set_neighbor_pages(&[]);
        }
        let assets = AssetStore::new(&self.storage_dir);
        // PDF-Seiten, die nach diesem Bild nicht mehr gebraucht werden (schnelles
        // Scrollen), verwirft end_frame, bevor sie gerendert werden
        self.pdf_renderer.begin_frame();
        self.canvas
            .prepare_images(&mut |key| texture_for(key, &assets, &mut self.pdf_renderer));
        // Page picker: PDF pages rendered small, after the pages on screen (Issue #40)
        for pdf in pdf_thumbnails_wanted {
            let pixels = crate::pdf_doc::THUMBNAIL_RENDER_PIXELS;
            let key = format!("{}@{pixels}", pdf.key());
            let result = match assets.path_of(&pdf.asset) {
                Ok(path) => self
                    .pdf_renderer
                    .take(&key, path, pdf.page as usize, pixels),
                Err(err) => Some(Err(err.to_string())),
            };
            let texture = match result {
                None => continue,
                Some(Ok(levels)) => levels.first().map(|image| {
                    let size = [image.width as usize, image.height as usize];
                    ui::Thumbnail(self.egui_ctx.load_texture(
                        &key,
                        egui::ColorImage::from_rgba_premultiplied(size, &image.pixels),
                        egui::TextureOptions::LINEAR,
                    ))
                }),
                Some(Err(err)) => {
                    eprintln!("[PDF] Vorschau {key}: {err}");
                    None
                }
            };
            // Bounded memory: a few hundred small pages at most
            if self.ui_state.pdf_thumbnails.len() >= 300 {
                self.ui_state.pdf_thumbnails.clear();
            }
            self.ui_state.pdf_thumbnails.insert(pdf.key(), texture);
            self.window.request_redraw();
        }
        // Nächste PDF-Seite vorab rendern, damit Blättern sofort klappt
        if !self.canvas.is_waiting_for_images() {
            let next = self
                .notebook
                .pages
                .get(self.notebook.current_page_index + 1);
            if let Some(pdf) = next.and_then(|p| p.pdf_background.as_ref()) {
                if let Ok(path) = assets.path_of(&pdf.asset) {
                    self.pdf_renderer
                        .prefetch(&pdf.key(), path, pdf.page as usize);
                }
            }
        }
        self.pdf_renderer.end_frame();
        let render_result = self.canvas.render(
            &clipped_primitives,
            &screen_descriptor,
            &full_output.textures_delta,
        );
        // epaint 0.37 panickt im Debug-Build, wenn TexturesDelta ohne clear() gedroppt wird
        full_output.textures_delta.clear();
        match render_result {
            Ok(_) => {}
            Err(RenderError::LostOrOutdated) => {
                self.canvas.resize(self.canvas.size);
                self.canvas.sync_page(self.notebook.current_page_mut());
            }
            Err(RenderError::Timeout) => {
                eprintln!("Surface present timeout");
            }
        }
        let _ = target;
    }
}
