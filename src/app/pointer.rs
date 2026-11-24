//! Maus-, Mausrad- und Multi-Touch-Eingaben (Zeichnen, Radieren, Auswahl, Pinch-Zoom, Pan, Fling).

use std::time::{Duration, Instant};

use glam::Vec2;
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, Touch, TouchPhase};

use crate::geometry;
use crate::platform;
use crate::selection::DragMode;
use crate::t;
use crate::ui::{self, ActiveTool};

use super::helpers::*;
use super::{AppState, PALM_GRACE};

impl AppState {
    pub(super) fn handle_mouse_input(
        &mut self,
        state: ElementState,
        button: MouseButton,
        is_pointer_in_ui: bool,
    ) {
        if self.debug_fling {
            eprintln!("[Nachgleiten] Maus: {button:?} {state:?}");
        }
        let now = self.start_time.elapsed().as_secs_f64();
        let is_eraser_action =
            self.ui_state.active_tool == ActiveTool::Eraser || button == MouseButton::Right;

        match state {
            ElementState::Pressed => {
                // Palm-Rejection: Keine Mausevents verarbeiten, wenn der Stift aktiv
                // zeichnet oder auswählt (manche Systeme melden ihn zusätzlich als Maus)
                if self.is_stylus_drawing
                    || self.is_stylus_selecting
                    || self.stylus_pan_last.is_some()
                {
                    return;
                }

                // Event-Isolation: Klicks auf die egui-Toolbar NICHT als Strich zeichnen
                if is_pointer_in_ui {
                    return;
                }

                // Clicking a math suggestion inserts its result (Issue #46)
                if button == MouseButton::Left && !self.ui_state.view_mode {
                    let (sx, sy) = self.cursor_screen_pos;
                    let scale = self.window.scale_factor() as f32;
                    if let Some(suggestion) = self
                        .ui_state
                        .math
                        .take_hit(egui::pos2(sx / scale, sy / scale))
                    {
                        insert_math_result(
                            &suggestion,
                            &mut self.canvas,
                            &mut self.notebook,
                            &mut self.manifest,
                            &mut self.autosave,
                        );
                        self.window.request_redraw();
                        return;
                    }
                }

                self.fling.stop();
                // 1. Pan via mittlere Maustaste oder Leertaste + Linksklick
                // In view-only mode the left and right buttons move the page too (Issue #35)
                if button == MouseButton::Middle
                    || (button == MouseButton::Left && self.is_space_pressed)
                    || (self.ui_state.view_mode
                        && matches!(button, MouseButton::Left | MouseButton::Right))
                {
                    self.is_panning = true;
                    self.pan_last_screen_pos = self.cursor_screen_pos;
                    return;
                }

                self.focus_page_at(self.cursor_screen_pos);
                if is_eraser_action {
                    self.is_erasing = true;
                    let (sx, sy) = self.cursor_screen_pos;
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
                } else if button == MouseButton::Left {
                    let (sx, sy) = self.cursor_screen_pos;

                    // Platz schaffen: Ansatzhöhe merken
                    if self.ui_state.active_tool == ActiveTool::Space {
                        space_press((sx, sy), &self.canvas, &self.notebook, &mut self.ui_state);
                        self.window.request_redraw();
                        return;
                    }

                    // Text-Werkzeug: Textfeld öffnen
                    if self.ui_state.active_tool == ActiveTool::Text {
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
                        return;
                    }

                    // 2. Objektauswahl & Transformation im Select-Modus
                    if self.ui_state.active_tool == ActiveTool::Select {
                        if selection_press(
                            (sx, sy),
                            self.modifiers.shift_key(),
                            &self.canvas,
                            self.notebook.current_page(),
                            self.egui_ctx.pixels_per_point(),
                            &mut self.selection_state,
                        ) {
                            self.window.request_redraw();
                        }
                        return;
                    }

                    // 3. Tap-to-Convert im Shape-Modus
                    if self.ui_state.active_tool == ActiveTool::Shape {
                        if let Some(page_pt) = self.canvas.screen_to_page(sx, sy) {
                            if let Some(stroke) =
                                self.notebook.current_page().find_stroke_at(page_pt, 40.0)
                            {
                                if let Some(shape) = geometry::detect_shape(&stroke.points) {
                                    let old_id = stroke.id;
                                    let shape_name = shape.name();
                                    let stroke_color = stroke.color;
                                    let stroke_width = self.ui_state.pen_stroke_width_mm
                                        * crate::document::UNITS_PER_MM;
                                    let new_stroke = geometry::shape_to_stroke(
                                        shape,
                                        old_id,
                                        stroke_color,
                                        stroke_width,
                                    );
                                    self.notebook
                                        .current_page_mut()
                                        .replace_stroke(old_id, new_stroke);
                                    self.canvas.sync_page(self.notebook.current_page_mut());
                                    self.autosave.mark_dirty(&self.notebook, &mut self.manifest);
                                    self.ui_state.set_export_message(t!(
                                        "{shape_name} ausgerichtet! (Undo mit Strg+Z)",
                                        shape_name = shape_name
                                    ));
                                    self.window.request_redraw();
                                    return;
                                }
                            }
                        }
                    }

                    // 4. Strich zeichnen
                    if self.ui_state.active_tool.is_drawing()
                        || self.ui_state.active_tool == ActiveTool::Shape
                    {
                        if let Some(page_pt) = self.canvas.screen_to_page(sx, sy) {
                            self.is_mouse_drawing = true;
                            self.is_drawing = true;
                            self.stroke_last_motion_time = Instant::now();
                            self.stroke_hold_anchor_pos = self.cursor_screen_pos;
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
            }
            ElementState::Released => {
                if button == MouseButton::Middle
                    || (matches!(button, MouseButton::Left | MouseButton::Right) && self.is_panning)
                {
                    self.is_panning = false;
                }
                if self.is_erasing {
                    self.is_erasing = false;
                }
                if self.ui_state.space_drag.is_some()
                    && !self.is_stylus_spacing
                    && !self.is_touch_spacing
                {
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
                if self.ui_state.active_tool == ActiveTool::Select
                    && !self.is_stylus_selecting
                    && selection_release(
                        self.modifiers.shift_key(),
                        &mut self.notebook,
                        &mut self.manifest,
                        &mut self.autosave,
                        &mut self.selection_state,
                        &mut self.canvas,
                    )
                {
                    self.window.request_redraw();
                }
                if self.is_drawing && self.is_mouse_drawing {
                    self.is_drawing = false;
                    self.is_mouse_drawing = false;
                    if !self.stroke_already_snapped {
                        let (sx, sy) = self.cursor_screen_pos;
                        let page_pt = self.canvas.screen_to_page_clamped(sx, sy);
                        let now = self.start_time.elapsed().as_secs_f64();
                        self.canvas
                            .extend_stroke(page_pt.x, page_pt.y, self.current_pressure, now);
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
            }
        }
    }

    pub(super) fn handle_cursor_moved(&mut self, position: PhysicalPosition<f64>) {
        self.cursor_screen_pos = (position.x as f32, position.y as f32);
        let (sx, sy) = self.cursor_screen_pos;

        // Canvas-Panning via Mausbewegung
        if self.is_panning {
            let dx = sx - self.pan_last_screen_pos.0;
            let dy = sy - self.pan_last_screen_pos.1;
            self.pan_last_screen_pos = (sx, sy);
            self.canvas.pan(dx, dy);
            self.window.request_redraw();
            return;
        }

        // Wenn der Stylus aktiv zeichnet, keine Mauscursor-Bewegungen verarbeiten
        if !self.is_stylus_drawing && !self.is_stylus_selecting {
            if self.ui_state.space_drag.is_some()
                && !self.is_stylus_spacing
                && !self.is_touch_spacing
            {
                space_move((sx, sy), &self.canvas, &mut self.ui_state);
                self.window.request_redraw();
            } else if self.ui_state.active_tool == ActiveTool::Select
                && self.selection_state.drag_mode != DragMode::None
            {
                if let Some(page_pt) = self.canvas.screen_to_page(sx, sy) {
                    self.selection_state.pending_drag = Some(page_pt);
                    self.window.request_redraw();
                }
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
            } else if self.is_drawing && self.is_mouse_drawing {
                // Draw-and-Hold Bewegungsmessung: Bei Bewegung > 3 px Timer zurücksetzen
                let dx = sx - self.stroke_hold_anchor_pos.0;
                let dy = sy - self.stroke_hold_anchor_pos.1;
                if (dx * dx + dy * dy).sqrt() > hold_tolerance(&self.window) {
                    self.stroke_last_motion_time = Instant::now();
                    self.stroke_hold_anchor_pos = (sx, sy);
                }

                if !self.stroke_already_snapped {
                    let page_pt = self.canvas.screen_to_page_clamped(sx, sy);
                    let now = self.start_time.elapsed().as_secs_f64();
                    self.canvas
                        .extend_stroke(page_pt.x, page_pt.y, self.current_pressure, now);
                    self.window.request_redraw();
                }
            }
        }
    }

    pub(super) fn handle_mouse_wheel(
        &mut self,
        delta: MouseScrollDelta,
        phase: TouchPhase,
        is_pointer_in_ui: bool,
    ) {
        if self.debug_fling {
            eprintln!("[Nachgleiten] Mausrad/Touchpad: {delta:?} ({phase:?}), über der Leiste: {is_pointer_in_ui}");
        }
        self.fling.stop();
        if is_pointer_in_ui {
            return;
        }
        // Durchgehende Ansicht und endlose Fläche: Mausrad scrollt (mit Umschalt
        // waagerecht), Strg + Mausrad zoomt
        if (self.ui_state.continuous_scroll || self.ui_state.endless)
            && !self.modifiers.control_key()
        {
            let (mut dx, mut dy) = match delta {
                MouseScrollDelta::LineDelta(x, y) => {
                    let line = 120.0 * self.window.scale_factor() as f32;
                    (x * line, y * line)
                }
                MouseScrollDelta::PixelDelta(pos) => (pos.x as f32, pos.y as f32),
            };
            if self.modifiers.shift_key() && dx == 0.0 {
                (dx, dy) = (dy, 0.0);
            }
            self.canvas.pan(dx, dy);
            self.window.request_redraw();
            return;
        }
        let zoom_factor = match delta {
            MouseScrollDelta::LineDelta(_x, y) => {
                if y > 0.0 {
                    1.12_f32.powf(y)
                } else if y < 0.0 {
                    (1.0 / 1.12_f32).powf(-y)
                } else {
                    1.0
                }
            }
            MouseScrollDelta::PixelDelta(pos) => {
                let y = pos.y as f32;
                if y > 0.0 {
                    1.0 + (y * 0.005).min(0.5)
                } else if y < 0.0 {
                    1.0 / (1.0 + (-y * 0.005).min(0.5))
                } else {
                    1.0
                }
            }
        };
        self.canvas.zoom_at(
            self.cursor_screen_pos.0,
            self.cursor_screen_pos.1,
            zoom_factor,
        );
        self.ui_state.current_zoom = self.canvas.camera.zoom;
        self.window.request_redraw();
    }

    pub(super) fn handle_touch(&mut self, touch: Touch) {
        let now = self.start_time.elapsed().as_secs_f64();
        // When the touch really happened (Wayland reports the time of each
        // event): the swipe speed must not depend on how late we handle it
        let handled_at = Instant::now();
        let event_time = match touch.phase {
            TouchPhase::Cancelled => None,
            phase => platform::take_touch_time(touch.id, phase),
        };
        if let (TouchPhase::Started, Some(time)) = (touch.phase, event_time) {
            self.touch_clock = Some((handled_at, time));
        }
        let touch_at = match (event_time, self.touch_clock) {
            (Some(time), Some((at, start))) => {
                let ms = time.wrapping_sub(start) as i32;
                if ms >= 0 {
                    at + Duration::from_millis(ms as u64)
                } else {
                    handled_at
                }
            }
            _ => handled_at,
        };
        let tx = touch.location.x as f32;
        let ty = touch.location.y as f32;
        let raw_pressure = match touch.force {
            Some(winit::event::Force::Normalized(f)) => Some(f as f32),
            Some(winit::event::Force::Calibrated {
                force,
                max_possible_force,
                ..
            }) => Some((force / max_possible_force).clamp(0.0, 1.0) as f32),
            None => None,
        };
        // Druckkurve nur bei echtem Druck; Finger ohne Druck zeichnen mittelstark
        let pressure = raw_pressure.map_or(0.5, |p| self.ui_state.pressure_curve.apply(p));

        let is_eraser_action = self.ui_state.active_tool == ActiveTool::Eraser;

        // Palm rejection: a touch that starts while the pen is near the screen (or
        // was just lifted away) belongs to the hand and is ignored until lifted
        let pen_near = self.is_stylus_drawing
            || self.is_stylus_in_proximity
            || self
                .stylus_left_at
                .is_some_and(|t| t.elapsed() < PALM_GRACE);
        if touch.phase == TouchPhase::Started && pen_near {
            self.palm_touch_ids.insert(touch.id);
        }
        if self.palm_touch_ids.contains(&touch.id) {
            if matches!(touch.phase, TouchPhase::Ended | TouchPhase::Cancelled) {
                self.palm_touch_ids.remove(&touch.id);
            }
            if self.debug_fling && touch.phase != TouchPhase::Moved {
                eprintln!(
                    "[Nachgleiten] Finger ignoriert ({:?}): Stift in der Nähe",
                    touch.phase
                );
            }
            return;
        }

        // Berührungen der UI (egui-winit hat sie bereits an egui weitergegeben)
        if self.ui_touch_ids.contains(&touch.id) {
            if matches!(touch.phase, TouchPhase::Ended | TouchPhase::Cancelled) {
                self.ui_touch_ids.remove(&touch.id);
            }
            self.window.request_redraw();
            return;
        }

        match touch.phase {
            TouchPhase::Started => {
                // Jede neue Berührung hält ein gleitendes Blatt an
                if self.debug_fling && self.fling.is_active() {
                    eprintln!("[Nachgleiten] Neuer Finger hält das Gleiten an");
                }
                self.fling.stop();
                // Event-Isolation für Touch-Eingaben
                let is_touch_in_ui = ui::is_screen_point_in_ui(
                    &self.egui_ctx,
                    &self.ui_state,
                    (tx, ty),
                    self.window.scale_factor() as f32,
                );
                if self.debug_fling {
                    eprintln!(
                                        "[Nachgleiten] Finger aufgesetzt bei ({tx:.0}, {ty:.0}), auf der Leiste: {is_touch_in_ui}"
                                    );
                }
                if is_touch_in_ui {
                    self.ui_touch_ids.insert(touch.id);
                    self.window.request_redraw();
                    return;
                }

                self.active_touches.insert(touch.id, (tx, ty));

                // Langes Drücken: nur beim Verschieben oder Zeichnen mit dem Finger, nicht
                // beim Radieren, Auswählen oder Text (dort hält man auch mal still)
                if self.active_touches.len() == 1
                    && !self.is_pinch_gesturing
                    && !self.ui_state.view_mode
                    && (!self.ui_state.touch_draw_enabled
                        || self.ui_state.active_tool.is_drawing()
                        || self.ui_state.active_tool == ActiveTool::Shape)
                {
                    self.long_press.start(touch.id, (tx, ty), Instant::now());
                } else {
                    self.long_press.cancel();
                }

                if self.active_touches.len() >= 2 {
                    // Multi-Touch Geste: Sobald 2+ Finger aufliegen, Inking sofort abbrechen und zoomen/pannen
                    if self.is_touch_drawing {
                        self.is_touch_drawing = false;
                        self.is_drawing = false;
                        self.canvas.cancel_stroke();
                        self.window.request_redraw();
                    }
                    self.is_touch_panning = false;
                    if self.is_touch_spacing {
                        self.is_touch_spacing = false;
                        self.ui_state.space_drag = None;
                    }
                    self.is_pinch_gesturing = true;

                    let pts: Vec<(f32, f32)> =
                        self.active_touches.values().copied().take(2).collect();
                    let p1 = pts[0];
                    let p2 = pts[1];
                    let center = ((p1.0 + p2.0) * 0.5, (p1.1 + p2.1) * 0.5);
                    let dist = ((p2.0 - p1.0).powi(2) + (p2.1 - p1.1).powi(2)).sqrt();
                    self.pinch_prev_center = Some(center);
                    self.pinch_prev_distance = Some(dist);
                } else if self.active_touches.len() == 1 && !self.is_pinch_gesturing {
                    self.cursor_screen_pos = (tx, ty);
                    self.current_pressure = pressure;

                    // Ohne Finger-Zeichnen verschiebt ein Finger das Blatt – außer bei
                    // Werkzeugen, die nicht zeichnen: Text und Platz schaffen nehmen den
                    // Finger immer, Auswählen, wenn er ein Objekt oder die Auswahl trifft
                    // (Issue #26)
                    let finger_uses_tool = match self.ui_state.active_tool {
                        _ if self.ui_state.view_mode => false,
                        ActiveTool::Text | ActiveTool::Space => true,
                        ActiveTool::Select => {
                            self.focus_page_at((tx, ty));
                            finger_grabs_selection(
                                (tx, ty),
                                &self.canvas,
                                self.notebook.current_page(),
                                self.egui_ctx.pixels_per_point(),
                                &self.selection_state,
                            )
                        }
                        _ => false,
                    };
                    if finger_uses_tool {
                        // Halten soll hier kein Kontextmenü öffnen
                        self.long_press.cancel();
                    }
                    if (!self.ui_state.touch_draw_enabled || self.ui_state.view_mode)
                        && !finger_uses_tool
                    {
                        // Standard: 1 Finger scrollt / verschiebt den Canvas (Pan)
                        self.is_touch_panning = true;
                        self.touch_pan_last_pos = (tx, ty);
                        self.touch_pan_trace = (0, 0.0, None);
                    } else {
                        // Modus Finger-Zeichnen aktiv
                        self.focus_page_at((tx, ty));
                        if is_eraser_action {
                            self.is_erasing = true;
                            if let Some(page_pt) = self.canvas.screen_to_page(tx, ty) {
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
                        } else if self.ui_state.active_tool == ActiveTool::Text {
                            if let Some(page_pt) = self.canvas.screen_to_page(tx, ty) {
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
                        } else if self.ui_state.active_tool == ActiveTool::Select {
                            if selection_press(
                                (tx, ty),
                                false,
                                &self.canvas,
                                self.notebook.current_page(),
                                self.egui_ctx.pixels_per_point(),
                                &mut self.selection_state,
                            ) {
                                self.window.request_redraw();
                            }
                        } else if self.ui_state.active_tool == ActiveTool::Space {
                            self.is_touch_spacing = space_press(
                                (tx, ty),
                                &self.canvas,
                                &self.notebook,
                                &mut self.ui_state,
                            );
                            self.window.request_redraw();
                        } else if self.ui_state.active_tool.is_drawing()
                            || self.ui_state.active_tool == ActiveTool::Shape
                        {
                            if let Some(page_pt) = self.canvas.screen_to_page(tx, ty) {
                                self.is_touch_drawing = true;
                                self.is_drawing = true;
                                self.stroke_last_motion_time = Instant::now();
                                self.stroke_hold_anchor_pos = (tx, ty);
                                self.stroke_already_snapped = false;
                                self.stroke_snapped_shape = None;
                                self.canvas
                                    .start_stroke(page_pt.x, page_pt.y, pressure, now);
                                self.window.request_redraw();
                            }
                        }
                    }
                }
            }
            TouchPhase::Moved => {
                self.active_touches.insert(touch.id, (tx, ty));
                self.long_press
                    .moved(touch.id, (tx, ty), self.window.scale_factor() as f32);

                if self.active_touches.len() >= 2 {
                    let pts: Vec<(f32, f32)> =
                        self.active_touches.values().copied().take(2).collect();
                    let p1 = pts[0];
                    let p2 = pts[1];
                    let curr_center = ((p1.0 + p2.0) * 0.5, (p1.1 + p2.1) * 0.5);
                    let curr_dist = ((p2.0 - p1.0).powi(2) + (p2.1 - p1.1).powi(2)).sqrt();

                    if let (Some(prev_dist), Some(prev_center)) =
                        (self.pinch_prev_distance, self.pinch_prev_center)
                    {
                        let dx = curr_center.0 - prev_center.0;
                        let dy = curr_center.1 - prev_center.1;
                        if dx.abs() > 0.05 || dy.abs() > 0.05 {
                            self.canvas.pan(dx, dy);
                        }
                        if prev_dist > 5.0 && curr_dist > 5.0 {
                            let zoom_factor = curr_dist / prev_dist;
                            if (zoom_factor - 1.0).abs() > 0.001 {
                                self.canvas
                                    .zoom_at(curr_center.0, curr_center.1, zoom_factor);
                                self.ui_state.current_zoom = self.canvas.camera.zoom;
                            }
                        }
                        self.window.request_redraw();
                    }
                    self.pinch_prev_center = Some(curr_center);
                    self.pinch_prev_distance = Some(curr_dist);
                } else if self.active_touches.len() == 1 && !self.is_pinch_gesturing {
                    if self.is_touch_panning {
                        let dx = tx - self.touch_pan_last_pos.0;
                        let dy = ty - self.touch_pan_last_pos.1;
                        self.touch_pan_last_pos = (tx, ty);
                        self.canvas.pan(dx, dy);
                        self.fling.track(Vec2::new(dx, dy), touch_at);
                        self.touch_pan_trace.0 += 1;
                        self.touch_pan_trace.1 += Vec2::new(dx, dy).length();
                        self.touch_pan_trace.2 = Some((handled_at, touch_at));
                        self.window.request_redraw();
                    } else if self.is_touch_drawing {
                        self.current_pressure = pressure;
                        self.cursor_screen_pos = (tx, ty);

                        let dx = tx - self.stroke_hold_anchor_pos.0;
                        let dy = ty - self.stroke_hold_anchor_pos.1;
                        if (dx * dx + dy * dy).sqrt() > hold_tolerance(&self.window) {
                            self.stroke_last_motion_time = Instant::now();
                            self.stroke_hold_anchor_pos = (tx, ty);
                        }

                        if !self.stroke_already_snapped {
                            let page_pt = self.canvas.screen_to_page_clamped(tx, ty);
                            self.canvas
                                .extend_stroke(page_pt.x, page_pt.y, pressure, now);
                            self.window.request_redraw();
                        }
                    } else if self.is_touch_spacing {
                        space_move((tx, ty), &self.canvas, &mut self.ui_state);
                        self.window.request_redraw();
                    } else if self.is_erasing {
                        self.focus_page_at((tx, ty));
                        if let Some(page_pt) = self.canvas.screen_to_page(tx, ty) {
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
                    } else if self.ui_state.active_tool == ActiveTool::Select
                        && self.selection_state.drag_mode != DragMode::None
                    {
                        if let Some(page_pt) = self.canvas.screen_to_page(tx, ty) {
                            self.selection_state.pending_drag = Some(page_pt);
                            self.window.request_redraw();
                        }
                    }
                }
            }
            TouchPhase::Ended | TouchPhase::Cancelled => {
                self.active_touches.remove(&touch.id);
                self.long_press.cancel();
                if self.debug_fling {
                    eprintln!(
                        "[Nachgleiten] Finger abgehoben ({:?}), verschiebt: {}, weitere Finger: {}",
                        touch.phase,
                        self.is_touch_panning,
                        self.active_touches.len()
                    );
                }

                if self.active_touches.is_empty() {
                    self.is_pinch_gesturing = false;
                    self.pinch_prev_distance = None;
                    self.pinch_prev_center = None;
                    if self.is_touch_panning && touch.phase == TouchPhase::Ended {
                        // Letztes Stück bis zum Abheben noch mitnehmen
                        let (dx, dy) = (
                            tx - self.touch_pan_last_pos.0,
                            ty - self.touch_pan_last_pos.1,
                        );
                        self.canvas.pan(dx, dy);
                        self.fling.track(Vec2::new(dx, dy), touch_at);
                        let velocity = self.fling.release(touch_at);
                        // The glide itself starts now
                        self.fling.restart_clock(Instant::now());
                        if self.debug_fling {
                            let (moves, path, last_move) = self.touch_pan_trace;
                            let ms = |d: Option<Duration>| d.map_or(-1, |d| d.as_millis() as i64);
                            eprintln!(
                                                "[Nachgleiten] {:.0} px/s beim Abheben, gleitet: {} ({moves} Bewegungen, {path:.0} px, letzte vor {} ms, laut System vor {} ms{})",
                                                velocity.length(),
                                                self.fling.is_active(),
                                                ms(last_move.map(|(handled, _)| handled_at.duration_since(handled))),
                                                ms(last_move.map(|(_, at)| touch_at.duration_since(at))),
                                                if event_time.is_some() { "" } else { ", keine Systemzeit" }
                                            );
                        }
                        self.window.request_redraw();
                    }
                    self.is_touch_panning = false;

                    if self.is_erasing {
                        self.is_erasing = false;
                    }

                    if self.is_touch_spacing {
                        self.is_touch_spacing = false;
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

                    if self.ui_state.active_tool == ActiveTool::Select
                        && selection_release(
                            false,
                            &mut self.notebook,
                            &mut self.manifest,
                            &mut self.autosave,
                            &mut self.selection_state,
                            &mut self.canvas,
                        )
                    {
                        self.window.request_redraw();
                    }

                    if self.is_touch_drawing {
                        self.is_touch_drawing = false;
                        self.is_drawing = false;

                        if !self.stroke_already_snapped {
                            let page_pt = self.canvas.screen_to_page_clamped(tx, ty);
                            self.canvas
                                .extend_stroke(page_pt.x, page_pt.y, pressure, now);
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
                } else if self.active_touches.len() == 1 {
                    self.pinch_prev_distance = None;
                    self.pinch_prev_center = None;
                    let rem = *self.active_touches.values().next().unwrap();
                    self.touch_pan_last_pos = rem;
                }
            }
        }
    }
}
