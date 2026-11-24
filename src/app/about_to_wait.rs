//! Event::AboutToWait-Verarbeitung: Autosave, Nextcloud-Sync, DiskWatch, Stift/Octotablet,
//! Draw-and-Hold, Long-Press, Mathe-Assistent, Nachgleiten und Repaint-Steuerung.

use std::time::{Duration, Instant};

use glam::Vec2;
use winit::event_loop::{ActiveEventLoop, ControlFlow};

use crate::autosave::AutosaveStatus;
use crate::disk_watch::DiskWatch;
use crate::geometry;
use crate::pen::PenEvent;
use crate::platform;
use crate::selection::{DragMode, SelectionHandleHit};
use crate::settings::{StylusAction, StylusButton, MAX_STYLUS_BUTTONS};
use crate::t;
use crate::ui::{self, ActiveTool};

use super::helpers::*;
use super::{AppState, PALM_GRACE};

impl AppState {
    pub(super) fn handle_about_to_wait(&mut self, target: &ActiveEventLoop) {
        #[cfg(target_os = "android")]
        if crate::android_bridge::has_messages() {
            self.window.request_redraw();
        }
        let prev_status = self.autosave.status();
        let new_status = self.autosave.update();
        if new_status != prev_status {
            update_window_title(
                &self.window,
                &self.notebook,
                &self.manifest,
                self.ui_state.active_tool,
                new_status,
            );
        }

        // Built-in sync: changes from the server to the open notebook wait while it
        // has unsaved changes; a pass soon after saving
        let unsaved = new_status != AutosaveStatus::Saved
            || self.is_drawing
            || self.is_stylus_drawing
            || self.ui_state.text_edit.is_some();
        if self.cloud.update(&self.manifest.id, unsaved) {
            self.disk_watch.check_soon(Instant::now());
            refresh_notebooks(&self.storage_base_dir, &mut self.ui_state);
            self.window.request_redraw();
        }
        if prev_status != AutosaveStatus::Saved && new_status == AutosaveStatus::Saved {
            self.cloud.sync_soon();
        }

        // Take over changes from other devices (Issue #49), but only while everything
        // local is saved and nothing is being drawn or typed
        if self.disk_watch.dir != self.storage_dir {
            self.disk_watch = DiskWatch::new(self.storage_dir.clone(), Instant::now());
        }
        let now = Instant::now();
        if self.disk_watch.is_due(now) {
            let idle = new_status == AutosaveStatus::Saved
                && !self.is_drawing
                && !self.is_stylus_drawing
                && self.ui_state.text_edit.is_none();
            if !idle {
                self.disk_watch.postpone(now);
            } else {
                let outcome = self
                    .disk_watch
                    .check(&mut self.notebook, &mut self.manifest, now);
                if !outcome.is_empty() {
                    if outcome.reloaded_pages > 0
                        || outcome.structure_changed
                        || !outcome.conflict_pages.is_empty()
                    {
                        self.selection_state.clear();
                        self.canvas.sync_page(self.notebook.current_page_mut());
                        self.ui_state.document_title = self.manifest.title.clone();
                        self.ui_state.set_export_message(
                            t!("Änderungen von einem anderen Gerät übernommen").to_string(),
                        );
                    }
                    if !outcome.conflict_pages.is_empty() {
                        for page in self
                            .notebook
                            .pages
                            .iter()
                            .filter(|p| outcome.conflict_pages.contains(&p.id))
                        {
                            self.autosave.mark_page_dirty(page, &self.manifest);
                        }
                        self.autosave.mark_manifest_dirty(&self.manifest);
                        self.ui_state.set_export_message(t!(
                            "Konfliktkopien als eigene Seiten übernommen: {count}",
                            count = outcome.conflict_pages.len()
                        ));
                    }
                    if let Some(marker) = &outcome.opened_elsewhere {
                        self.ui_state.set_export_message(t!(
                                    "⚠ Gerade auf '{device}' geöffnet – Änderungen dort können deine überschreiben",
                                    device = marker.device
                                ));
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
            }
        }

        while let Ok(result) = self.export_rx.try_recv() {
            match result {
                Ok(path) => self
                    .ui_state
                    .set_export_message(t!("Gespeichert: {path}", path = path.display())),
                Err(err) => self
                    .ui_state
                    .set_export_message(t!("Export fehlgeschlagen: {err}", err = err)),
            }
            self.window.request_redraw();
        }

        let prev_upd = self.updater.status().clone();
        self.updater.update();
        if *self.updater.status() != prev_upd {
            self.window.request_redraw();
        }

        // Pen input (Issue #44): octotablet on the desktop (Wayland/X11 tablet protocols,
        // Windows Ink), on Android the pen's own touch samples
        #[cfg(not(target_os = "android"))]
        let pen_events = match self.tablet_manager.as_mut().map(|mgr| mgr.pump()) {
            Some(Ok(events)) => crate::pen::from_octotablet(events),
            _ => Vec::new(),
        };
        #[cfg(target_os = "android")]
        let pen_events = {
            let samples: Vec<crate::pen::Sample> = winit::platform::android::take_pen_samples()
                .into_iter()
                .map(Into::into)
                .collect();
            self.android_pen
                .convert(&samples, self.window.scale_factor() as f32, Instant::now())
        };
        if !pen_events.is_empty() {
            let scale = self.window.scale_factor() as f32;
            let now = self.start_time.elapsed().as_secs_f64();
            for crate::pen::PenInput {
                tool,
                is_eraser: tool_is_hardware_eraser,
                event,
            } in pen_events
            {
                let pen_arrives = !self.is_stylus_in_proximity
                    && matches!(event, PenEvent::In | PenEvent::Pose { .. } | PenEvent::Down);
                if pen_arrives {
                    // Fingers already on the screen are most likely the writing hand:
                    // undo what they started and ignore them until lifted
                    if self.debug_fling && !self.active_touches.is_empty() {
                        eprintln!(
                            "[Nachgleiten] Stift kommt: {} Finger als Handballen ignoriert",
                            self.active_touches.len()
                        );
                    }
                    self.palm_touch_ids
                        .extend(self.active_touches.drain().map(|(id, _)| id));
                    self.long_press.cancel();
                    if self.is_touch_drawing {
                        self.is_touch_drawing = false;
                        self.is_drawing = false;
                        self.canvas.cancel_stroke();
                    }
                    if self.is_touch_spacing {
                        self.is_touch_spacing = false;
                        self.ui_state.space_drag = None;
                    }
                    if self.selection_state.drag_mode != DragMode::None && !self.is_stylus_selecting
                    {
                        selection_release(
                            false,
                            &mut self.notebook,
                            &mut self.manifest,
                            &mut self.autosave,
                            &mut self.selection_state,
                            &mut self.canvas,
                        );
                    }
                    self.is_erasing = false;
                    self.is_touch_panning = false;
                    self.is_pinch_gesturing = false;
                    self.pinch_prev_distance = None;
                    self.pinch_prev_center = None;
                    self.stylus_left_at = None;
                    platform::ignore_decoration_touches_until(Some(
                        Instant::now() + Duration::from_secs(24 * 3600),
                    ));
                    self.window.request_redraw();
                }

                match event {
                    PenEvent::In => {
                        self.is_stylus_in_proximity = true;
                        self.stylus_tool_id = Some(tool);
                        // A finger that moved the ruler is done once the pen comes near:
                        // if its lift has not arrived yet, the ruler would follow the
                        // hovering pen (Issue #55). The pen grabs the ruler only on touch.
                        self.ui_state.ruler_drag = None;
                    }
                    PenEvent::Out => {
                        if self.is_stylus_in_proximity {
                            self.stylus_left_at = Some(Instant::now());
                            platform::ignore_decoration_touches_until(Some(
                                Instant::now() + PALM_GRACE,
                            ));
                        }
                        self.is_stylus_in_proximity = false;
                        if self.stylus_ui_capture {
                            self.stylus_ui_capture = false;
                            let (sx, sy) = self.stylus_screen_pos;
                            let input = self.egui_winit_state.egui_input_mut();
                            input.events.push(egui::Event::PointerButton {
                                pos: egui::pos2(sx / scale, sy / scale),
                                button: egui::PointerButton::Primary,
                                pressed: false,
                                modifiers: egui::Modifiers::default(),
                            });
                            input.events.push(egui::Event::PointerGone);
                            self.window.request_redraw();
                        }
                        if self.stylus_tool_id == Some(tool) {
                            self.stylus_tool_id = None;
                            self.stylus_pan_last = None;
                            if self.is_stylus_drawing {
                                self.is_stylus_drawing = false;
                                self.is_drawing = false;
                                commit_stroke_to_notebook(
                                    &mut self.canvas,
                                    &mut self.notebook,
                                    &mut self.manifest,
                                    &mut self.autosave,
                                    &mut self.ui_state,
                                    &self.window,
                                    self.stroke_snapped_shape.take(),
                                );
                            }
                            if self.is_erasing {
                                self.is_erasing = false;
                            }
                            if self.is_stylus_spacing {
                                self.is_stylus_spacing = false;
                                space_release(
                                    &mut self.canvas,
                                    &mut self.notebook,
                                    &mut self.manifest,
                                    &mut self.autosave,
                                    &self.storage_dir,
                                    &mut self.ui_state,
                                );
                                self.window.request_redraw();
                            }
                            if self.is_stylus_selecting {
                                self.is_stylus_selecting = false;
                                selection_release(
                                    false,
                                    &mut self.notebook,
                                    &mut self.manifest,
                                    &mut self.autosave,
                                    &mut self.selection_state,
                                    &mut self.canvas,
                                );
                                self.window.request_redraw();
                            }
                            if let Some(tool) = self.tool_before_button.take() {
                                activate_tool(tool, &mut self.ui_state, &mut self.canvas);
                            }
                            self.stylus_held = None;
                            self.ui_state.pressed_stylus_button = None;
                        }
                    }
                    PenEvent::Button { key, pressed } => {
                        // Tasten werden beim ersten Drücken gelernt (Standard: Radieren)
                        let mut index = self
                            .ui_state
                            .stylus_buttons
                            .iter()
                            .position(|b| b.id == key);
                        if index.is_none()
                            && pressed
                            && self.ui_state.stylus_buttons.len() < MAX_STYLUS_BUTTONS
                        {
                            self.ui_state.stylus_buttons.push(StylusButton {
                                id: key,
                                action: StylusAction::Erase,
                            });
                            index = Some(self.ui_state.stylus_buttons.len() - 1);
                            self.ui_settings.stylus_buttons = self.ui_state.stylus_buttons.clone();
                            save_ui_settings(
                                &self.ui_settings,
                                self.ui_settings_path.as_deref(),
                                &mut self.ui_state,
                            );
                        }
                        let action = index.map_or(StylusAction::Erase, |i| {
                            self.ui_state.stylus_buttons[i].action
                        });
                        if pressed {
                            self.stylus_held = Some((key, action));
                            self.ui_state.pressed_stylus_button = index;
                            if action == StylusAction::Erase && self.is_stylus_drawing {
                                self.is_stylus_drawing = false;
                                self.is_drawing = false;
                                self.canvas.cancel_stroke();
                                self.is_erasing = true;
                            }
                        } else if self.stylus_held.is_some_and(|(held, _)| held == key) {
                            self.stylus_held = None;
                            self.ui_state.pressed_stylus_button = None;
                            if action == StylusAction::Erase
                                && self.is_erasing
                                && !tool_is_hardware_eraser
                                && self.ui_state.active_tool != ActiveTool::Eraser
                            {
                                self.is_erasing = false;
                            }
                        }
                        self.window.request_redraw();
                    }
                    PenEvent::Pose { position, pressure } => {
                        self.is_stylus_in_proximity = true;
                        let sx = position[0] * scale;
                        let sy = position[1] * scale;
                        self.stylus_screen_pos = (sx, sy);
                        self.cursor_screen_pos = (sx, sy);

                        if let Some(p) = pressure {
                            self.current_pressure = self.ui_state.pressure_curve.apply(p);
                            self.ui_state.last_raw_pressure = Some(p);
                        }

                        // Stiftbewegung an egui weitergeben (Hover, Schieberegler ziehen),
                        // solange nicht auf dem Blatt gezeichnet oder radiert wird
                        if self.stylus_ui_capture
                            || (!self.is_stylus_drawing
                                && !self.is_erasing
                                && !self.is_stylus_selecting
                                && self.stylus_pan_last.is_none())
                        {
                            let ui_pos = egui::pos2(sx / scale, sy / scale);
                            self.egui_winit_state
                                .egui_input_mut()
                                .events
                                .push(egui::Event::PointerMoved(ui_pos));
                            if self.stylus_ui_capture
                                || ui::is_screen_point_in_ui(
                                    &self.egui_ctx,
                                    &self.ui_state,
                                    (sx, sy),
                                    scale,
                                )
                            {
                                self.window.request_redraw();
                            }
                        }
                        if self.stylus_ui_capture {
                            continue;
                        }

                        if let Some((lx, ly)) = self.stylus_pan_last {
                            let delta = Vec2::new(sx - lx, sy - ly);
                            self.stylus_pan_last = Some((sx, sy));
                            self.canvas.pan(delta.x, delta.y);
                            self.fling.track(delta, Instant::now());
                            self.window.request_redraw();
                        } else if self.is_stylus_spacing {
                            space_move((sx, sy), &self.canvas, &mut self.ui_state);
                            self.window.request_redraw();
                        } else if self.is_erasing {
                            self.focus_page_at((sx, sy));
                            if let Some(page_pt) = self.canvas.screen_to_page(sx, sy) {
                                if self.notebook.erase_at(
                                    page_pt,
                                    self.ui_state.eraser_radius(),
                                    self.stroke_max_radius,
                                    self.ui_state.eraser_mode,
                                ) {
                                    self.canvas.sync_page(self.notebook.current_page_mut());
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
                        } else if self.is_stylus_selecting {
                            if let Some(page_pt) = self.canvas.screen_to_page(sx, sy) {
                                self.selection_state.pending_drag = Some(page_pt);
                                self.window.request_redraw();
                            }
                        } else if self.is_stylus_drawing && self.is_drawing {
                            let dx = sx - self.stroke_hold_anchor_pos.0;
                            let dy = sy - self.stroke_hold_anchor_pos.1;
                            if (dx * dx + dy * dy).sqrt() > hold_tolerance(&self.window) {
                                self.stroke_last_motion_time = Instant::now();
                                self.stroke_hold_anchor_pos = (sx, sy);
                            }

                            if !self.stroke_already_snapped {
                                let page_pt = self.canvas.screen_to_page_clamped(sx, sy);
                                self.canvas.extend_stroke(
                                    page_pt.x,
                                    page_pt.y,
                                    self.current_pressure,
                                    now,
                                );
                                self.window.request_redraw();
                            }
                        }
                    }
                    PenEvent::Down => {
                        self.is_stylus_in_proximity = true;
                        self.stylus_tool_id = Some(tool);
                        let (sx, sy) = self.stylus_screen_pos;

                        // Tapping a math suggestion inserts its result (Issue #46)
                        if let Some(suggestion) = (!self.ui_state.view_mode)
                            .then(|| {
                                self.ui_state
                                    .math
                                    .take_hit(egui::pos2(sx / scale, sy / scale))
                            })
                            .flatten()
                        {
                            insert_math_result(
                                &suggestion,
                                &mut self.canvas,
                                &mut self.notebook,
                                &mut self.manifest,
                                &mut self.autosave,
                            );
                            // The rest of this tap goes nowhere
                            self.stylus_ui_capture = true;
                            self.window.request_redraw();
                            continue;
                        }

                        if ui::is_screen_point_in_ui(
                            &self.egui_ctx,
                            &self.ui_state,
                            (sx, sy),
                            scale,
                        ) {
                            // In die Eingabe-Warteschlange des NÄCHSTEN egui-Frames
                            // (egui_ctx.input_mut würde vom nächsten Frame überschrieben)
                            let ui_pos = egui::pos2(sx / scale, sy / scale);
                            let input = self.egui_winit_state.egui_input_mut();
                            input.events.push(egui::Event::PointerMoved(ui_pos));
                            input.events.push(egui::Event::PointerButton {
                                pos: ui_pos,
                                button: egui::PointerButton::Primary,
                                pressed: true,
                                modifiers: egui::Modifiers::default(),
                            });
                            self.stylus_ui_capture = true;
                            self.window.request_redraw();
                            continue;
                        }

                        if self.ui_state.view_mode {
                            // View only: the pen moves the page like a finger
                            self.fling.stop();
                            self.stylus_pan_last = Some((sx, sy));
                            continue;
                        }

                        self.focus_page_at((sx, sy));
                        // Werkzeug-Taste gedrückt: für diesen Strich dieses Werkzeug
                        let button_action = self.stylus_held.map(|(_, action)| action);
                        let button_tool = match button_action {
                            Some(StylusAction::Select) => Some(ActiveTool::Select),
                            Some(StylusAction::Highlighter) => Some(ActiveTool::Highlighter),
                            Some(StylusAction::Pen) => Some(ActiveTool::Pen),
                            _ => None,
                        };
                        // A selection made with the select button stays grabbable with
                        // the bare pen; elsewhere the pen draws and the selection ends
                        // (Issue #37)
                        let mut button_tool = button_tool;
                        if button_action.is_none()
                            && !tool_is_hardware_eraser
                            && (self.ui_state.active_tool.is_drawing()
                                || self.ui_state.active_tool == ActiveTool::Shape)
                            && !self.selection_state.is_empty()
                        {
                            let hit = self.selection_state.hit_test_handles(
                                self.notebook.current_page(),
                                &self.canvas,
                                Vec2::new(sx, sy),
                                self.egui_ctx.pixels_per_point(),
                            );
                            if matches!(hit, SelectionHandleHit::None) {
                                self.selection_state.clear();
                            } else {
                                button_tool = Some(ActiveTool::Select);
                            }
                        }
                        if let Some(tool) = button_tool
                            .filter(|&t| !tool_is_hardware_eraser && t != self.ui_state.active_tool)
                        {
                            self.tool_before_button = Some(self.ui_state.active_tool);
                            activate_tool(tool, &mut self.ui_state, &mut self.canvas);
                        }
                        let should_erase = tool_is_hardware_eraser
                            || button_action == Some(StylusAction::Erase)
                            || self.ui_state.active_tool == ActiveTool::Eraser;

                        if !should_erase && self.ui_state.active_tool == ActiveTool::Space {
                            self.is_stylus_spacing = space_press(
                                (sx, sy),
                                &self.canvas,
                                &self.notebook,
                                &mut self.ui_state,
                            );
                            self.window.request_redraw();
                        } else if !should_erase && self.ui_state.active_tool == ActiveTool::Text {
                            if let Some(page_pt) = self.canvas.screen_to_page(sx, sy) {
                                text_tool_click(
                                    page_pt,
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
                        } else if !should_erase && self.ui_state.active_tool == ActiveTool::Select {
                            if selection_press(
                                (sx, sy),
                                false,
                                &self.canvas,
                                self.notebook.current_page(),
                                self.egui_ctx.pixels_per_point(),
                                &mut self.selection_state,
                            ) {
                                self.is_stylus_selecting = true;
                                self.window.request_redraw();
                            }
                        } else if should_erase {
                            self.is_erasing = true;
                            if let Some(page_pt) = self.canvas.screen_to_page(sx, sy) {
                                if self.notebook.erase_at(
                                    page_pt,
                                    self.ui_state.eraser_radius(),
                                    self.stroke_max_radius,
                                    self.ui_state.eraser_mode,
                                ) {
                                    self.canvas.sync_page(self.notebook.current_page_mut());
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
                        } else if self.ui_state.active_tool.is_drawing()
                            || self.ui_state.active_tool == ActiveTool::Shape
                        {
                            if let Some(page_pt) = self.canvas.screen_to_page(sx, sy) {
                                self.is_stylus_drawing = true;
                                self.is_drawing = true;
                                self.stroke_last_motion_time = Instant::now();
                                self.stroke_hold_anchor_pos = (sx, sy);
                                self.stroke_already_snapped = false;
                                self.stroke_snapped_shape = None;
                                self.canvas.start_stroke(
                                    page_pt.x,
                                    page_pt.y,
                                    self.current_pressure,
                                    now,
                                );
                                self.window.request_redraw();
                            }
                        }
                    }
                    PenEvent::Up => {
                        let (sx, sy) = self.stylus_screen_pos;
                        if self.stylus_ui_capture {
                            self.stylus_ui_capture = false;
                            self.egui_winit_state.egui_input_mut().events.push(
                                egui::Event::PointerButton {
                                    pos: egui::pos2(sx / scale, sy / scale),
                                    button: egui::PointerButton::Primary,
                                    pressed: false,
                                    modifiers: egui::Modifiers::default(),
                                },
                            );
                            self.window.request_redraw();
                            continue;
                        }

                        if self.stylus_pan_last.take().is_some() {
                            self.fling.release(Instant::now());
                            self.window.request_redraw();
                        }

                        if self.is_erasing {
                            self.is_erasing = false;
                        }

                        if self.is_stylus_spacing {
                            self.is_stylus_spacing = false;
                            space_release(
                                &mut self.canvas,
                                &mut self.notebook,
                                &mut self.manifest,
                                &mut self.autosave,
                                &self.storage_dir,
                                &mut self.ui_state,
                            );
                            self.window.request_redraw();
                        }

                        if self.is_stylus_selecting {
                            self.is_stylus_selecting = false;
                            if selection_release(
                                false,
                                &mut self.notebook,
                                &mut self.manifest,
                                &mut self.autosave,
                                &mut self.selection_state,
                                &mut self.canvas,
                            ) {
                                self.window.request_redraw();
                            }
                        }
                        let restore_tool = self.tool_before_button.take();

                        if self.is_stylus_drawing {
                            self.is_stylus_drawing = false;
                            self.is_drawing = false;

                            if !self.stroke_already_snapped {
                                let page_pt = self.canvas.screen_to_page_clamped(sx, sy);
                                self.canvas.extend_stroke(
                                    page_pt.x,
                                    page_pt.y,
                                    self.current_pressure,
                                    now,
                                );
                            }

                            let snapped_shape = self.stroke_snapped_shape.take().or_else(|| {
                                if self.ui_state.active_tool == ActiveTool::Shape {
                                    geometry::detect_shape(&self.canvas.live_samples)
                                } else {
                                    None
                                }
                            });

                            commit_stroke_to_notebook(
                                &mut self.canvas,
                                &mut self.notebook,
                                &mut self.manifest,
                                &mut self.autosave,
                                &mut self.ui_state,
                                &self.window,
                                snapped_shape,
                            );
                        }
                        // Nach einem Strich mit Werkzeug-Taste wieder das gewählte Werkzeug
                        if let Some(tool) = restore_tool {
                            activate_tool(tool, &mut self.ui_state, &mut self.canvas);
                            self.window.request_redraw();
                        }
                    }
                }
            }
        }

        // Draw-and-Hold Überprüfung: Stift regungslos für > 400 ms gehalten (siehe hold_tolerance)
        if self.is_drawing && !self.stroke_already_snapped {
            let elapsed = self.stroke_last_motion_time.elapsed();
            let dx = self.cursor_screen_pos.0 - self.stroke_hold_anchor_pos.0;
            let dy = self.cursor_screen_pos.1 - self.stroke_hold_anchor_pos.1;
            let dist = (dx * dx + dy * dy).sqrt();

            if elapsed >= Duration::from_millis(400)
                && dist < hold_tolerance(&self.window)
                && self.canvas.live_samples.len() >= 4
            {
                if let Some(shape) = geometry::detect_shape(&self.canvas.live_samples) {
                    self.stroke_already_snapped = true;
                    self.stroke_snapped_shape = Some(shape.clone());
                    // Kreuzt die Gerade den vorigen Strich, zeigt die Vorschau schon das
                    // Achsenkreuz (übernommen wird es beim Absetzen)
                    // Textmarker bilden kein Achsenkreuz
                    let shape = if self.ui_state.active_tool == ActiveTool::Highlighter {
                        shape
                    } else {
                        axis_cross_with_previous(&self.notebook, &shape)
                            .map_or(shape, |(_, cross)| cross)
                    };
                    let width_units =
                        self.ui_state.current_stroke_width_mm() * crate::document::UNITS_PER_MM;
                    let (samples, vertices) =
                        geometry::shape_to_mesh(&shape, width_units, self.canvas.current_ink_color);
                    self.canvas.set_live_shape_mesh(samples, vertices);
                    self.ui_state
                        .set_export_message(t!("{shape} eingerastet!", shape = shape.name()));
                    self.window.request_redraw();
                }
            }
        }

        // Langes Drücken mit dem Finger: Kontextmenü zum Einfügen an dieser Stelle
        if let Some(pos) = self.long_press.fire(Instant::now()) {
            if self.is_touch_drawing {
                self.is_touch_drawing = false;
                self.is_drawing = false;
                self.canvas.cancel_stroke();
            }
            self.is_touch_panning = false;
            self.focus_page_at(pos);
            if let Some(page_pt) = self.canvas.screen_to_page(pos.0, pos.1) {
                let scale = self.window.scale_factor() as f32;
                self.ui_state.close_overlays();
                // Leicht versetzt, damit der Finger das Menü nicht verdeckt
                self.ui_state.context_menu = Some(ui::ContextMenu {
                    screen: egui::pos2(pos.0 / scale + 12.0, pos.1 / scale + 12.0),
                    page: page_pt,
                });
            }
            self.window.request_redraw();
        }

        // Pause after an `=` is over: compute the expression (Issue #46)
        if self.ui_state.math.update(
            self.notebook.current_page(),
            Instant::now(),
            self.is_drawing || self.is_stylus_drawing,
            crate::i18n::language() == crate::i18n::Language::German,
        ) {
            self.window.request_redraw();
        }

        // Nachgleiten nach dem Wischen
        if let Some(delta) = self.fling.step(Instant::now()) {
            self.canvas.pan(delta.x, delta.y);
            self.window.request_redraw();
        }

        // Von egui angefordertes, verzögertes Neuzeichnen (Tooltips, Animationen)
        if let Some(at) = self.egui_repaint_at {
            if Instant::now() >= at {
                self.egui_repaint_at = None;
                self.window.request_redraw();
            }
        }

        // Bei anstehenden Änderungen, aktivem Toast, Stylus in Proximity oder aktivem Zeichnen regelmäßig aufwachen
        if new_status == AutosaveStatus::Pending
                    || new_status == AutosaveStatus::Saving
                    || (self.is_drawing && !self.stroke_already_snapped)
                    || self.is_stylus_drawing
                    || self.is_stylus_in_proximity
                    || self.fling.is_active()
                    // While a finger is down: on Wayland, the lift can already sit in winit's
                    // queue (read from the socket by the graphics driver) without waking the
                    // loop, and then only arrives with the next touch, which stops the glide
                    // right away (Issue #28)
                    || !self.active_touches.is_empty()
        {
            target.set_control_flow(ControlFlow::wait_duration(Duration::from_millis(8)));
        } else if let Some(at) = [
            self.egui_repaint_at,
            self.long_press.deadline(),
            self.ui_state.math.deadline(),
            Some(self.disk_watch.deadline()),
            self.cloud.deadline(),
            // The updater thread does not wake the loop: look for its results now and then
            matches!(
                self.updater.status(),
                crate::updater::UpdaterStatus::Checking
                    | crate::updater::UpdaterStatus::Downloading { .. }
            )
            .then(|| Instant::now() + Duration::from_millis(100)),
        ]
        .into_iter()
        .flatten()
        .min()
        {
            target.set_control_flow(ControlFlow::WaitUntil(at));
        } else {
            target.set_control_flow(ControlFlow::Wait);
        }
    }
}
