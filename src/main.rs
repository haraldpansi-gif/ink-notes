//! ink-notes: Latenzoptimierte Linux-Inking-App mit wgpu, winit, egui-Toolbar,
//! Seitenverwaltung, Undo/Redo, Vektor-PDF-Export und asynchroner Notizbuch-Persistenz (Autosave).

use std::sync::Arc;
use std::time::{Duration, Instant};
use winit::{
    event::{ElementState, Event, KeyEvent, MouseButton, MouseScrollDelta, TouchPhase, WindowEvent},
    event_loop::{ControlFlow, EventLoop},
    keyboard::{Key, ModifiersState, NamedKey},
    window::{Window, WindowBuilder},
};

use glam::Vec2;
use ink_notes::autosave::{AutosaveController, AutosaveStatus};
use ink_notes::canvas::CanvasState;
use ink_notes::document::{Notebook, PageBackground};
use ink_notes::export::{export_notebook_to_pdf, ExportStrategy};
use ink_notes::geometry::{self, RecognizedShape};
use ink_notes::selection::{render_selection_overlay, DragMode, SelectionHandleHit, SelectionState};
use ink_notes::storage::{self, NotebookManifest};
use ink_notes::ui::{self, ActiveTool, UiAction, UiState};

/// Aktualisiert den Fenstertitel als minimales On-Screen-Display (OSD) inklusive Autosave-Status.
fn update_window_title(
    window: &Window,
    notebook: &Notebook,
    manifest: &NotebookManifest,
    tool: ActiveTool,
    status: AutosaveStatus,
) {
    let page = notebook.current_page();
    let bg_name = match page.background {
        PageBackground::Blank => "Blanko",
        PageBackground::Grid5mm => "Kariert (5mm)",
        PageBackground::Lined => "Liniert (7.5mm)",
        PageBackground::DotGrid => "Punktraster (5mm)",
    };

    let title = format!(
        "ink-notes | [{}] {} | [{}] | {} | Hintergrund: {} | [Ctrl+Z] Undo [Ctrl+Y] Redo [E] Radierer [Ctrl+E/S] PDF-Export",
        manifest.title,
        notebook.osd_text(),
        status.display_text(),
        tool.name(),
        bg_name
    );
    window.set_title(&title);
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let event_loop = EventLoop::new()?;
    let window = Arc::new(
        WindowBuilder::new()
            .with_title("ink-notes (Latenzoptimiertes Linux-Inking)")
            .with_inner_size(winit::dpi::LogicalSize::new(1280.0, 850.0))
            .build(&event_loop)?,
    );

    let mut canvas = pollster::block_on(CanvasState::new(window.clone()))?;

    // 1. Notizbuch aus Storage laden oder initialisieren (Session-Wiederherstellung)
    let storage_base_dir = storage::default_storage_dir();
    let available_notebooks = storage::list_notebooks(&storage_base_dir);

    let (initial_id, initial_title) = if let Some(first) = available_notebooks.first() {
        (first.id.clone(), first.title.clone())
    } else {
        ("default".to_string(), "Standard-Notizbuch".to_string())
    };

    let mut storage_dir = storage_base_dir.join(&initial_id);
    let (mut notebook, mut manifest) = if storage_dir.join("manifest.json").exists() {
        println!("[Storage] Lade Notizbuch aus: {}", storage_dir.display());
        match storage::load_notebook(&storage_dir) {
            Ok((nb, m)) => (nb, m),
            Err(err) => {
                eprintln!("[Storage-Fehler] Konnte Notizbuch nicht laden ({err}). Erstelle neues Notizbuch.");
                (Notebook::new(), NotebookManifest::new(&initial_id, &initial_title))
            }
        }
    } else {
        println!("[Storage] Erstelle neues Notizbuch in: {}", storage_dir.display());
        (Notebook::new(), NotebookManifest::new(&initial_id, &initial_title))
    };

    // 2. Asynchronen Autosave-Controller mit 1.5s Debounce starten
    let mut autosave = AutosaveController::new(storage_dir.clone(), Duration::from_millis(1500));

    // 3. egui Context und State initialisieren
    let egui_ctx = egui::Context::default();
    egui_ctx.set_visuals(egui::Visuals::dark());

    let mut egui_winit_state = egui_winit::State::new(
        egui_ctx.clone(),
        egui::ViewportId::ROOT,
        &window,
        Some(window.scale_factor() as f32),
        Some(2048),
    );

    let mut ui_state = UiState::new();
    ui_state.current_notebook_id = manifest.id.clone();
    ui_state.document_title = manifest.title.clone();
    ui_state.available_notebooks = available_notebooks;
    let mut selection_state = SelectionState::new();

    // Initiale Seite und Werkzeugeinstellungen mit Canvas synchronisieren
    canvas.sync_page(notebook.current_page_mut());
    canvas.set_stroke_color(ui_state.pen_color);
    canvas.set_stroke_width_mm(ui_state.pen_stroke_width_mm);

    let start_time = Instant::now();
    let mut cursor_screen_pos = (0.0_f32, 0.0_f32);
    let mut is_drawing = false;
    let mut is_erasing = false;
    let mut current_pressure = 0.5_f32;
    let mut modifiers = ModifiersState::empty();

    let mut is_mouse_drawing = false;
    let mut active_touch_id: Option<u64> = None;

    // Viewport-Pan & Zoom-Zustand
    let mut is_panning = false;
    let mut pan_last_screen_pos = (0.0_f32, 0.0_f32);
    let mut is_space_pressed = false;

    // Draw-and-Hold Zustand für automatische Formerkennung
    let mut stroke_last_motion_time = Instant::now();
    let mut stroke_hold_anchor_pos = (0.0_f32, 0.0_f32);
    let mut stroke_already_snapped = false;
    let mut stroke_snapped_shape: Option<RecognizedShape> = None;

    // Radier-Parameter
    let eraser_radius = 30.0_f32; // 3 mm in DIN-A4-Einheiten
    let stroke_max_radius = 12.0_f32;

    update_window_title(&window, &notebook, &manifest, ui_state.active_tool, autosave.status());

    println!("=== ink-notes (Multi-Color & Objektauswahl/Transformation) bereit ===");
    println!("Toolbar:");
    println!("  Stift / Textmarker / Formen (Auto-Snap) / Radierer / Auswählen (Lasso)");
    println!("  Farbpalette (Multicolor), Strichbreite, Undo/Redo, Zoom, Navigation & PDF-Export");
    println!("Objektauswahl & Transformation:");
    println!("  V              : Auswahlwerkzeug (Lasso) umschalten");
    println!("  Lasso-Drag     : Auswahlrahmen über Striche aufziehen");
    println!("  Klick/Shift+Kl.: Einzelne Striche auswählen / Auswahl umschalten");
    println!("  Objekt ziehen  : Striche in Echtzeit verschieben");
    println!("  Griffe (Ecken) : Ausgewählte Objekte stufenlos skalieren");
    println!("  Dreh-Griff / R : Objekte stufenlos drehen oder um 90° kippen (R)");
    println!("  Palette-Klick  : Ausgewählte Objekte direkt umfärben");
    println!("  Strg + C / V   : Ausgewählte Objekte kopieren & einfügen");
    println!("  Entf/Backspace : Ausgewählte Objekte löschen (Undo mit Strg+Z)");
    println!("Tastatur- & Maus-Shortcuts:");
    println!("  Mausrad        : Stufenloser Zoom zentriert auf Cursor");
    println!("  Mittlere Taste : Canvas verschieben (Pan)");
    println!("  Leertaste+Drag : Canvas verschieben (Pan)");
    println!("  Strg + 0       : Zoom zurücksetzen (100% zentriert)");
    println!("  Draw-and-Hold  : Form am Strichende >400ms halten zum automatischen Einrasten");
    println!("  Strg + Z / Y   : Transaktionales Undo / Redo für alle Aktionen");
    println!("  Strg + E / S   : Vektor-PDF-Export (nach ./export.pdf)");
    println!("  ESC / Q        : Beenden (mit sicherem Disk-Flush)");

    event_loop.run(move |event, target| {
        match event {
            Event::AboutToWait => {
                let prev_status = autosave.status();
                let new_status = autosave.update();
                if new_status != prev_status {
                    update_window_title(&window, &notebook, &manifest, ui_state.active_tool, new_status);
                }

                // Draw-and-Hold Überprüfung: Stift regungslos für > 400 ms gehalten (< 3 px Distanz)
                if is_drawing && !stroke_already_snapped {
                    let elapsed = stroke_last_motion_time.elapsed();
                    let dx = cursor_screen_pos.0 - stroke_hold_anchor_pos.0;
                    let dy = cursor_screen_pos.1 - stroke_hold_anchor_pos.1;
                    let dist = (dx * dx + dy * dy).sqrt();

                    if elapsed >= Duration::from_millis(400) && dist < 3.0 && canvas.live_samples.len() >= 4 {
                        if let Some(shape) = geometry::detect_shape(&canvas.live_samples) {
                            stroke_already_snapped = true;
                            stroke_snapped_shape = Some(shape.clone());
                            let width_units = ui_state.pen_stroke_width_mm * ink_notes::document::UNITS_PER_MM;
                            let (samples, vertices) = geometry::shape_to_mesh(&shape, width_units, canvas.current_ink_color);
                            canvas.set_live_shape_mesh(samples, vertices);
                            ui_state.set_export_message(format!("{} eingerastet!", shape.name()));
                            window.request_redraw();
                        }
                    }
                }

                // Bei anstehenden Änderungen, aktivem Toast oder aktivem Zeichnen regelmäßig aufwachen
                if new_status == AutosaveStatus::Pending
                    || new_status == AutosaveStatus::Saving
                    || ui_state.export_message.is_some()
                    || (is_drawing && !stroke_already_snapped)
                {
                    target.set_control_flow(ControlFlow::wait_duration(Duration::from_millis(16)));
                } else {
                    target.set_control_flow(ControlFlow::Wait);
                }
            }

            Event::WindowEvent { event, window_id } if window_id == window.id() => {
                // 1. Window-Event zuerst an egui übergeben
                let egui_response = egui_winit_state.on_window_event(&window, &event);
                if egui_response.repaint {
                    window.request_redraw();
                }

                // 2. Prüfen, ob das UI Pointer-Interaktionen beansprucht (Event-Isolation)
                // Konsumiert Klicks innerhalb der abgerundeten Pill-Toolbar, des Popovers oder offener Menüs
                let is_pointer_in_ui = egui_ctx.wants_pointer_input()
                    || ui_state.is_point_in_ui(cursor_screen_pos.0, cursor_screen_pos.1)
                    || (ui_state.show_page_jump_popup && egui_ctx.is_pointer_over_area())
                    || (ui_state.is_document_menu_open && egui_ctx.is_pointer_over_area());

                match event {
                    WindowEvent::CloseRequested => {
                        println!("[Shutdown] Sichere alle ungespeicherten Daten auf Festplatte...");
                        autosave.shutdown();
                        target.exit();
                    }

                    WindowEvent::Focused(false) => {
                        // Bei Fokusverlust Strich abbrechen und 1€-Filter resetten
                        if is_drawing {
                            is_drawing = false;
                            canvas.cancel_stroke();
                            window.request_redraw();
                        }
                        is_erasing = false;
                        is_mouse_drawing = false;
                        active_touch_id = None;
                    }

                    WindowEvent::Resized(physical_size) => {
                        canvas.resize(physical_size);
                        canvas.sync_page(notebook.current_page_mut());
                        window.request_redraw();
                    }

                    WindowEvent::ScaleFactorChanged { .. } => {
                        canvas.resize(window.inner_size());
                        canvas.sync_page(notebook.current_page_mut());
                        window.request_redraw();
                    }

                    WindowEvent::ModifiersChanged(new_mods) => {
                        modifiers = new_mods.state();
                    }

                    WindowEvent::KeyboardInput {
                        event:
                            KeyEvent {
                                logical_key,
                                state: key_state,
                                ..
                            },
                        ..
                    } => {
                        // Tastatureingaben nicht für Shortcuts nutzen, wenn ein Textfeld fokussiert ist
                        if egui_ctx.wants_keyboard_input() {
                            return;
                        }

                        if let Key::Named(NamedKey::Space) = &logical_key {
                            is_space_pressed = key_state == ElementState::Pressed;
                            if !is_space_pressed && is_panning {
                                is_panning = false;
                            }
                        }

                        if key_state != ElementState::Pressed {
                            return;
                        }

                        let ctrl = modifiers.control_key();
                        let shift = modifiers.shift_key();

                        match &logical_key {
                            Key::Named(NamedKey::Escape) => {
                                println!("[Shutdown] Sichere alle ungespeicherten Daten auf Festplatte...");
                                autosave.shutdown();
                                target.exit();
                            }

                            // Undo: Strg + Z
                            Key::Character(c) if c.eq_ignore_ascii_case("z") && ctrl && !shift => {
                                if notebook.current_page_mut().undo() {
                                    selection_state.clear();
                                    canvas.sync_page(notebook.current_page_mut());
                                    autosave.mark_page_dirty(notebook.current_page(), &manifest);
                                    update_window_title(&window, &notebook, &manifest, ui_state.active_tool, autosave.status());
                                    window.request_redraw();
                                }
                            }

                            // Redo: Strg + Y oder Strg + Umschalt + Z
                            Key::Character(c)
                                if (c.eq_ignore_ascii_case("y") && ctrl)
                                    || (c.eq_ignore_ascii_case("z") && ctrl && shift) =>
                            {
                                if notebook.current_page_mut().redo() {
                                    selection_state.clear();
                                    canvas.sync_page(notebook.current_page_mut());
                                    autosave.mark_page_dirty(notebook.current_page(), &manifest);
                                    update_window_title(&window, &notebook, &manifest, ui_state.active_tool, autosave.status());
                                    window.request_redraw();
                                }
                            }

                            // Kopieren: Strg + C
                            Key::Character(c) if c.eq_ignore_ascii_case("c") && ctrl => {
                                let count = selection_state.copy_to_clipboard(notebook.current_page());
                                if count > 0 {
                                    ui_state.set_export_message(format!("{count} Objekt(e) kopiert"));
                                    window.request_redraw();
                                }
                            }

                            // Einfügen: Strg + V
                            Key::Character(c) if c.eq_ignore_ascii_case("v") && ctrl => {
                                let mut next_id = notebook.next_stroke_id;
                                let new_ids = selection_state.paste_from_clipboard(
                                    notebook.current_page_mut(),
                                    &mut || {
                                        let id = next_id;
                                        next_id += 1;
                                        id
                                    },
                                );
                                notebook.next_stroke_id = next_id;
                                if !new_ids.is_empty() {
                                    canvas.sync_page(notebook.current_page_mut());
                                    manifest.next_stroke_id = notebook.next_stroke_id;
                                    autosave.mark_page_dirty(notebook.current_page(), &manifest);
                                    ui_state.set_export_message(format!("{} Objekt(e) eingefügt", new_ids.len()));
                                    window.request_redraw();
                                }
                            }

                            // Löschen: Entf oder Backspace
                            Key::Named(NamedKey::Delete) | Key::Named(NamedKey::Backspace) => {
                                if !selection_state.is_empty() {
                                    let removed = selection_state.delete_selected(notebook.current_page_mut());
                                    if !removed.is_empty() {
                                        canvas.sync_page(notebook.current_page_mut());
                                        autosave.mark_page_dirty(notebook.current_page(), &manifest);
                                        ui_state.set_export_message(format!("{} Objekt(e) gelöscht", removed.len()));
                                        window.request_redraw();
                                    }
                                }
                            }

                            // PDF-Export: Strg + E oder Strg + S
                            Key::Character(c)
                                if (c.eq_ignore_ascii_case("e") || c.eq_ignore_ascii_case("s"))
                                    && ctrl =>
                            {
                                let notebook_clone = notebook.clone();
                                let export_path = "export.pdf".to_string();
                                let window_ref = window.clone();
                                println!(
                                    "[Export] Starte PDF-Vektorexport nach '{}' (im Hintergrundthread)...",
                                    export_path
                                );
                                ui_state.set_export_message("Exportiere PDF...");
                                std::thread::spawn(move || {
                                    let start = std::time::Instant::now();
                                    match export_notebook_to_pdf(
                                        &notebook_clone,
                                        &export_path,
                                        ExportStrategy::FilledPolygon,
                                    ) {
                                        Ok(()) => {
                                             let elapsed = start.elapsed();
                                             println!(
                                                 "[Export] PDF erfolgreich exportiert: '{}' ({:.1?} für {} Seiten)",
                                                 export_path,
                                                 elapsed,
                                                 notebook_clone.pages.len()
                                             );
                                             window_ref.request_redraw();
                                         }
                                         Err(err) => {
                                             eprintln!("[Export-Fehler] Konnte PDF nicht exportieren: {err}");
                                         }
                                     }
                                 });
                                 ui_state.set_export_message("PDF erfolgreich als export.pdf exportiert!");
                                 window.request_redraw();
                            }

                            // Radierer umschalten: E
                            Key::Character(c) if c.eq_ignore_ascii_case("e") && !ctrl => {
                                ui_state.active_tool = match ui_state.active_tool {
                                    ActiveTool::Pen | ActiveTool::Highlighter | ActiveTool::Shape | ActiveTool::Select => ActiveTool::Eraser,
                                    ActiveTool::Eraser => ActiveTool::Pen,
                                };
                                ui_state.is_pen_menu_open = false;
                                ui_state.is_highlighter_menu_open = false;
                                update_window_title(&window, &notebook, &manifest, ui_state.active_tool, autosave.status());
                                window.request_redraw();
                            }

                            // Auswahlwerkzeug umschalten: V (ohne Strg)
                            Key::Character(c) if c.eq_ignore_ascii_case("v") && !ctrl => {
                                ui_state.active_tool = if ui_state.active_tool == ActiveTool::Select {
                                    ActiveTool::Pen
                                } else {
                                    ActiveTool::Select
                                };
                                ui_state.is_pen_menu_open = false;
                                ui_state.is_highlighter_menu_open = false;
                                update_window_title(&window, &notebook, &manifest, ui_state.active_tool, autosave.status());
                                window.request_redraw();
                            }

                            // Stiftfarbe Notiz-Schwarz: B
                            Key::Character(c) if c.eq_ignore_ascii_case("b") && !ctrl => {
                                if !selection_state.is_empty() {
                                    let color = [0.12, 0.12, 0.14, 1.0];
                                    if selection_state.recolor_selected(notebook.current_page_mut(), color) {
                                        canvas.sync_page(notebook.current_page_mut());
                                        autosave.mark_page_dirty(notebook.current_page(), &manifest);
                                        ui_state.set_export_message("Auswahl auf Schwarz umgefärbt".to_string());
                                        window.request_redraw();
                                    }
                                } else {
                                    ui_state.active_tool = ActiveTool::Pen;
                                    ui_state.pen_color = [0.12, 0.12, 0.14, 1.0];
                                    canvas.set_stroke_color(ui_state.pen_color);
                                    canvas.set_stroke_width_mm(ui_state.pen_stroke_width_mm);
                                    update_window_title(&window, &notebook, &manifest, ui_state.active_tool, autosave.status());
                                    window.request_redraw();
                                }
                            }

                            // Stiftfarbe Universitäts-Blau: U
                            Key::Character(c) if c.eq_ignore_ascii_case("u") && !ctrl => {
                                if !selection_state.is_empty() {
                                    let color = [0.15, 0.35, 0.85, 1.0];
                                    if selection_state.recolor_selected(notebook.current_page_mut(), color) {
                                        canvas.sync_page(notebook.current_page_mut());
                                        autosave.mark_page_dirty(notebook.current_page(), &manifest);
                                        ui_state.set_export_message("Auswahl auf Blau umgefärbt".to_string());
                                        window.request_redraw();
                                    }
                                } else {
                                    ui_state.active_tool = ActiveTool::Pen;
                                    ui_state.pen_color = [0.15, 0.35, 0.85, 1.0];
                                    canvas.set_stroke_color(ui_state.pen_color);
                                    canvas.set_stroke_width_mm(ui_state.pen_stroke_width_mm);
                                    update_window_title(&window, &notebook, &manifest, ui_state.active_tool, autosave.status());
                                    window.request_redraw();
                                }
                            }

                            // Stiftfarbe Korrektur-Rot oder Drehen: R
                            Key::Character(c) if c.eq_ignore_ascii_case("r") && !ctrl => {
                                if !selection_state.is_empty() {
                                    if selection_state.rotate_selected_90_deg(notebook.current_page_mut()) {
                                        canvas.sync_page(notebook.current_page_mut());
                                        autosave.mark_page_dirty(notebook.current_page(), &manifest);
                                        ui_state.set_export_message("Auswahl um 90° gedreht".to_string());
                                        window.request_redraw();
                                    }
                                } else {
                                    ui_state.active_tool = ActiveTool::Pen;
                                    ui_state.pen_color = [0.85, 0.20, 0.20, 1.0];
                                    canvas.set_stroke_color(ui_state.pen_color);
                                    canvas.set_stroke_width_mm(ui_state.pen_stroke_width_mm);
                                    update_window_title(&window, &notebook, &manifest, ui_state.active_tool, autosave.status());
                                    window.request_redraw();
                                }
                            }

                            // Hintergrund wechseln: G
                            Key::Character(c) if c.eq_ignore_ascii_case("g") && !ctrl => {
                                let page = notebook.current_page_mut();
                                page.background = match page.background {
                                    PageBackground::Grid5mm => PageBackground::Lined,
                                    PageBackground::Lined => PageBackground::DotGrid,
                                    PageBackground::DotGrid => PageBackground::Blank,
                                    PageBackground::Blank => PageBackground::Grid5mm,
                                };
                                canvas.sync_page(page);
                                autosave.mark_page_dirty(notebook.current_page(), &manifest);
                                update_window_title(&window, &notebook, &manifest, ui_state.active_tool, autosave.status());
                                window.request_redraw();
                            }

                            // Nächste Seite: PageDown oder N (mit Lazy Loading)
                            Key::Named(NamedKey::PageDown) => {
                                selection_state.clear();
                                notebook.next_page();
                                let curr = notebook.current_page_index;
                                let _ = storage::ensure_page_loaded(&storage_dir, &mut notebook, curr);
                                canvas.sync_page(notebook.current_page_mut());
                                manifest.current_page_index = notebook.current_page_index;
                                manifest.next_page_id = notebook.next_page_id;
                                autosave.mark_page_dirty(notebook.current_page(), &manifest);
                                update_window_title(&window, &notebook, &manifest, ui_state.active_tool, autosave.status());
                                window.request_redraw();
                            }
                            Key::Character(c) if c.eq_ignore_ascii_case("n") && !ctrl => {
                                selection_state.clear();
                                notebook.next_page();
                                let curr = notebook.current_page_index;
                                let _ = storage::ensure_page_loaded(&storage_dir, &mut notebook, curr);
                                canvas.sync_page(notebook.current_page_mut());
                                manifest.current_page_index = notebook.current_page_index;
                                manifest.next_page_id = notebook.next_page_id;
                                autosave.mark_page_dirty(notebook.current_page(), &manifest);
                                update_window_title(&window, &notebook, &manifest, ui_state.active_tool, autosave.status());
                                window.request_redraw();
                            }

                            // Vorherige Seite: PageUp
                            Key::Named(NamedKey::PageUp) => {
                                selection_state.clear();
                                notebook.previous_page();
                                let curr = notebook.current_page_index;
                                let _ = storage::ensure_page_loaded(&storage_dir, &mut notebook, curr);
                                canvas.sync_page(notebook.current_page_mut());
                                manifest.current_page_index = notebook.current_page_index;
                                autosave.mark_page_dirty(notebook.current_page(), &manifest);
                                update_window_title(&window, &notebook, &manifest, ui_state.active_tool, autosave.status());
                                window.request_redraw();
                            }

                            // Erste Seite: Home / Pos1
                            Key::Named(NamedKey::Home) => {
                                selection_state.clear();
                                notebook.first_page();
                                let curr = notebook.current_page_index;
                                let _ = storage::ensure_page_loaded(&storage_dir, &mut notebook, curr);
                                canvas.sync_page(notebook.current_page_mut());
                                manifest.current_page_index = notebook.current_page_index;
                                autosave.mark_page_dirty(notebook.current_page(), &manifest);
                                update_window_title(&window, &notebook, &manifest, ui_state.active_tool, autosave.status());
                                window.request_redraw();
                            }

                            // Letzte Seite: End / Ende
                            Key::Named(NamedKey::End) => {
                                selection_state.clear();
                                notebook.last_page();
                                let curr = notebook.current_page_index;
                                let _ = storage::ensure_page_loaded(&storage_dir, &mut notebook, curr);
                                canvas.sync_page(notebook.current_page_mut());
                                manifest.current_page_index = notebook.current_page_index;
                                autosave.mark_page_dirty(notebook.current_page(), &manifest);
                                update_window_title(&window, &notebook, &manifest, ui_state.active_tool, autosave.status());
                                window.request_redraw();
                            }

                            // Zoom zurücksetzen: Strg + 0
                            Key::Character(c) if c == "0" && ctrl => {
                                canvas.reset_view();
                                ui_state.current_zoom = canvas.camera.zoom;
                                window.request_redraw();
                            }

                            // Zoom In: Strg + '+' oder Strg + '='
                            Key::Character(c) if (c == "+" || c == "=") && ctrl => {
                                canvas.zoom_at(canvas.size.width as f32 * 0.5, canvas.size.height as f32 * 0.5, 1.15);
                                ui_state.current_zoom = canvas.camera.zoom;
                                window.request_redraw();
                            }

                            // Zoom Out: Strg + '-' oder Strg + '_'
                            Key::Character(c) if (c == "-" || c == "_") && ctrl => {
                                canvas.zoom_at(canvas.size.width as f32 * 0.5, canvas.size.height as f32 * 0.5, 1.0 / 1.15);
                                ui_state.current_zoom = canvas.camera.zoom;
                                window.request_redraw();
                            }

                            // Formen-Werkzeug umschalten: S (ohne Strg) oder F
                            Key::Character(c) if (c.eq_ignore_ascii_case("s") || c.eq_ignore_ascii_case("f")) && !ctrl => {
                                ui_state.active_tool = if ui_state.active_tool == ActiveTool::Shape {
                                    ActiveTool::Pen
                                } else {
                                    ActiveTool::Shape
                                };
                                update_window_title(&window, &notebook, &manifest, ui_state.active_tool, autosave.status());
                                window.request_redraw();
                            }

                            // Beenden: Q
                            Key::Character(c) if c.eq_ignore_ascii_case("q") && !ctrl => {
                                println!("[Shutdown] Sichere alle ungespeicherten Daten auf Festplatte...");
                                autosave.shutdown();
                                target.exit();
                            }

                            _ => {}
                        }
                    }

                    WindowEvent::MouseInput { state, button, .. } => {
                        let now = start_time.elapsed().as_secs_f64();
                        let is_eraser_action = ui_state.active_tool == ActiveTool::Eraser
                            || button == MouseButton::Right;

                        match state {
                            ElementState::Pressed => {
                                // Palm-Rejection: Keine Mausevents verarbeiten, wenn Touch aktiv ist
                                if active_touch_id.is_some() {
                                    return;
                                }

                                // Event-Isolation: Klicks auf die egui-Toolbar NICHT als Strich zeichnen
                                if is_pointer_in_ui {
                                    return;
                                }

                                // 1. Pan via mittlere Maustaste oder Leertaste + Linksklick
                                if button == MouseButton::Middle || (button == MouseButton::Left && is_space_pressed) {
                                    is_panning = true;
                                    pan_last_screen_pos = cursor_screen_pos;
                                    return;
                                }

                                if is_eraser_action {
                                    is_erasing = true;
                                    let (sx, sy) = cursor_screen_pos;
                                    if let Some(page_pt) = canvas.screen_to_page(sx, sy) {
                                        let page = notebook.current_page_mut();
                                        let removed = page.erase_at(page_pt, eraser_radius, stroke_max_radius);
                                        if !removed.is_empty() {
                                            canvas.sync_page(page);
                                            autosave.mark_page_dirty(notebook.current_page(), &manifest);
                                            update_window_title(&window, &notebook, &manifest, ui_state.active_tool, autosave.status());
                                            window.request_redraw();
                                        }
                                    }
                                } else if button == MouseButton::Left {
                                    let (sx, sy) = cursor_screen_pos;

                                    // 2. Objektauswahl & Transformation im Select-Modus
                                    if ui_state.active_tool == ActiveTool::Select {
                                        let screen_vec = Vec2::new(sx, sy);
                                        let hit = selection_state.hit_test_handles(notebook.current_page(), &canvas, screen_vec);
                                        match hit {
                                            SelectionHandleHit::RotateHandle { center_page } => {
                                                if let Some(page_pt) = canvas.screen_to_page(sx, sy) {
                                                    let start_angle = (page_pt.y - center_page.y).atan2(page_pt.x - center_page.x);
                                                    let initial_strokes = notebook.current_page().get_strokes(&selection_state.selected_ids);
                                                    selection_state.drag_mode = DragMode::Rotate {
                                                        center_page,
                                                        start_angle,
                                                        initial_strokes,
                                                    };
                                                    window.request_redraw();
                                                    return;
                                                }
                                            }
                                            SelectionHandleHit::CornerHandle {
                                                corner_idx: _,
                                                fixed_corner_page,
                                                start_pos_page,
                                                initial_box_size,
                                            } => {
                                                let initial_strokes = notebook.current_page().get_strokes(&selection_state.selected_ids);
                                                selection_state.drag_mode = DragMode::Scale {
                                                    fixed_corner_page,
                                                    start_pos_page,
                                                    initial_box_size,
                                                    initial_strokes,
                                                };
                                                window.request_redraw();
                                                return;
                                            }
                                            SelectionHandleHit::Body => {
                                                if let Some(page_pt) = canvas.screen_to_page(sx, sy) {
                                                    let initial_strokes = notebook.current_page().get_strokes(&selection_state.selected_ids);
                                                    selection_state.drag_mode = DragMode::Move {
                                                        start_page: page_pt,
                                                        initial_strokes,
                                                    };
                                                    window.request_redraw();
                                                    return;
                                                }
                                            }
                                            SelectionHandleHit::None => {
                                                if let Some(page_pt) = canvas.screen_to_page(sx, sy) {
                                                    let shift = modifiers.shift_key();
                                                    if let Some(stroke) = notebook.current_page().find_stroke_at(page_pt, 25.0) {
                                                        let stroke_id = stroke.id;
                                                        if shift {
                                                            selection_state.toggle_select(stroke_id);
                                                        } else if !selection_state.selected_ids.contains(&stroke_id) {
                                                            selection_state.select_single(stroke_id);
                                                        }
                                                        let initial_strokes = notebook.current_page().get_strokes(&selection_state.selected_ids);
                                                        selection_state.drag_mode = DragMode::Move {
                                                            start_page: page_pt,
                                                            initial_strokes,
                                                        };
                                                        window.request_redraw();
                                                        return;
                                                    } else {
                                                        if !shift {
                                                            selection_state.clear();
                                                        }
                                                        selection_state.drag_mode = DragMode::BoxSelect {
                                                            start_page: page_pt,
                                                            current_page: page_pt,
                                                        };
                                                        window.request_redraw();
                                                        return;
                                                    }
                                                }
                                            }
                                        }
                                    }

                                    // 3. Tap-to-Convert im Shape-Modus
                                    if ui_state.active_tool == ActiveTool::Shape {
                                        if let Some(page_pt) = canvas.screen_to_page(sx, sy) {
                                            if let Some(stroke) = notebook.current_page().find_stroke_at(page_pt, 40.0) {
                                                if let Some(shape) = geometry::detect_shape(&stroke.points) {
                                                    let old_id = stroke.id;
                                                    let shape_name = shape.name();
                                                    let stroke_color = stroke.color;
                                                    let stroke_width = ui_state.pen_stroke_width_mm * ink_notes::document::UNITS_PER_MM;
                                                    let new_stroke = geometry::shape_to_stroke(shape, old_id, stroke_color, stroke_width);
                                                    notebook.current_page_mut().replace_stroke(old_id, new_stroke);
                                                    canvas.sync_page(notebook.current_page_mut());
                                                    autosave.mark_page_dirty(notebook.current_page(), &manifest);
                                                    ui_state.set_export_message(format!("{shape_name} ausgerichtet! (Undo mit Strg+Z)"));
                                                    window.request_redraw();
                                                    return;
                                                }
                                            }
                                        }
                                    }

                                    // 4. Strich zeichnen
                                    if ui_state.active_tool.is_drawing() || ui_state.active_tool == ActiveTool::Shape {
                                        if let Some(page_pt) = canvas.screen_to_page(sx, sy) {
                                            is_mouse_drawing = true;
                                            is_drawing = true;
                                            stroke_last_motion_time = Instant::now();
                                            stroke_hold_anchor_pos = cursor_screen_pos;
                                            stroke_already_snapped = false;
                                            stroke_snapped_shape = None;
                                            canvas.start_stroke(page_pt.x, page_pt.y, current_pressure, now);
                                            window.request_redraw();
                                        }
                                    }
                                }
                            }
                            ElementState::Released => {
                                if button == MouseButton::Middle || (button == MouseButton::Left && is_panning) {
                                    is_panning = false;
                                }
                                if is_erasing {
                                    is_erasing = false;
                                }
                                if ui_state.active_tool == ActiveTool::Select {
                                    let shift = modifiers.shift_key();
                                    match std::mem::take(&mut selection_state.drag_mode) {
                                        DragMode::BoxSelect { start_page, current_page } => {
                                            let box_min = Vec2::new(start_page.x.min(current_page.x), start_page.y.min(current_page.y));
                                            let box_max = Vec2::new(start_page.x.max(current_page.x), start_page.y.max(current_page.y));
                                            if (box_max.x - box_min.x).max(box_max.y - box_min.y) > 3.0 {
                                                let found_ids = notebook.current_page().find_strokes_in_box(box_min, box_max);
                                                if shift {
                                                    for id in found_ids {
                                                        if !selection_state.selected_ids.contains(&id) {
                                                            selection_state.selected_ids.push(id);
                                                        }
                                                    }
                                                } else {
                                                    selection_state.select_multiple(found_ids);
                                                }
                                            }
                                            window.request_redraw();
                                        }
                                        DragMode::Move { initial_strokes, .. }
                                        | DragMode::Rotate { initial_strokes, .. }
                                        | DragMode::Scale { initial_strokes, .. } => {
                                            let final_strokes = notebook.current_page().get_strokes(&selection_state.selected_ids);
                                            if !initial_strokes.is_empty() && !final_strokes.is_empty() && initial_strokes != final_strokes {
                                                notebook.current_page_mut().commit_modified_strokes(initial_strokes, final_strokes);
                                                autosave.mark_page_dirty(notebook.current_page(), &manifest);
                                            }
                                            window.request_redraw();
                                        }
                                        DragMode::None => {}
                                    }
                                }
                                if is_drawing && is_mouse_drawing {
                                    is_drawing = false;
                                    is_mouse_drawing = false;
                                    if !stroke_already_snapped {
                                        let (sx, sy) = cursor_screen_pos;
                                        let page_pt = canvas.screen_to_page_clamped(sx, sy);
                                        let now = start_time.elapsed().as_secs_f64();
                                        canvas.extend_stroke(page_pt.x, page_pt.y, current_pressure, now);
                                    }
                                    let new_id = notebook.next_stroke_id();

                                    // Prüfen, ob Form durch Draw-and-Hold gesnappt ist oder im Shape-Modus gezeichnet wurde
                                    let snapped_shape = stroke_snapped_shape.take().or_else(|| {
                                        if ui_state.active_tool == ActiveTool::Shape {
                                            geometry::detect_shape(&canvas.live_samples)
                                        } else {
                                            None
                                        }
                                    });

                                    let stroke_opt = if let Some(shape) = snapped_shape {
                                        let stroke_width = ui_state.pen_stroke_width_mm * ink_notes::document::UNITS_PER_MM;
                                        let color = canvas.current_ink_color;
                                        canvas.cancel_stroke();
                                        Some(geometry::shape_to_stroke(shape, new_id, color, stroke_width))
                                    } else {
                                        canvas.finish_stroke(new_id)
                                    };

                                    if let Some(stroke) = stroke_opt {
                                        let page = notebook.current_page_mut();
                                        page.add_stroke(stroke);
                                        canvas.sync_page(page);
                                        manifest.next_stroke_id = notebook.next_stroke_id;
                                        autosave.mark_page_dirty(notebook.current_page(), &manifest);
                                        update_window_title(&window, &notebook, &manifest, ui_state.active_tool, autosave.status());
                                        window.request_redraw();
                                    }
                                }
                            }
                        }
                    }

                    WindowEvent::CursorMoved { position, .. } => {
                        cursor_screen_pos = (position.x as f32, position.y as f32);
                        let (sx, sy) = cursor_screen_pos;

                        // Canvas-Panning via Mausbewegung
                        if is_panning {
                            let dx = sx - pan_last_screen_pos.0;
                            let dy = sy - pan_last_screen_pos.1;
                            pan_last_screen_pos = (sx, sy);
                            canvas.pan(dx, dy);
                            window.request_redraw();
                            return;
                        }

                        // Palm-Rejection: Keine Cursor-Events verarbeiten, wenn Touch aktiv ist
                        if active_touch_id.is_none() {
                            if ui_state.active_tool == ActiveTool::Select && selection_state.drag_mode != DragMode::None {
                                if let Some(page_pt) = canvas.screen_to_page(sx, sy) {
                                    match &mut selection_state.drag_mode {
                                        DragMode::BoxSelect { current_page, .. } => {
                                            *current_page = page_pt;
                                            window.request_redraw();
                                        }
                                        DragMode::Move { start_page, initial_strokes } => {
                                            let delta = page_pt - *start_page;
                                            let mut updated = initial_strokes.clone();
                                            for s in &mut updated {
                                                s.translate(delta);
                                            }
                                            notebook.current_page_mut().update_strokes_ephemeral(&updated);
                                            canvas.sync_page(notebook.current_page_mut());
                                            window.request_redraw();
                                        }
                                        DragMode::Rotate { center_page, start_angle, initial_strokes } => {
                                            let current_angle = (page_pt.y - center_page.y).atan2(page_pt.x - center_page.x);
                                            let delta_angle = current_angle - *start_angle;
                                            let mut updated = initial_strokes.clone();
                                            for s in &mut updated {
                                                s.rotate(*center_page, delta_angle);
                                            }
                                            notebook.current_page_mut().update_strokes_ephemeral(&updated);
                                            canvas.sync_page(notebook.current_page_mut());
                                            window.request_redraw();
                                        }
                                        DragMode::Scale { fixed_corner_page, start_pos_page, initial_box_size: _, initial_strokes } => {
                                            let orig_delta = *start_pos_page - *fixed_corner_page;
                                            let new_delta = page_pt - *fixed_corner_page;
                                            let scale_x = if orig_delta.x.abs() > 1.0 {
                                                (new_delta.x / orig_delta.x).clamp(0.05, 20.0)
                                            } else {
                                                1.0
                                            };
                                            let scale_y = if orig_delta.y.abs() > 1.0 {
                                                (new_delta.y / orig_delta.y).clamp(0.05, 20.0)
                                            } else {
                                                1.0
                                            };
                                            let factors = Vec2::new(scale_x, scale_y);
                                            let mut updated = initial_strokes.clone();
                                            for s in &mut updated {
                                                s.scale(*fixed_corner_page, factors);
                                            }
                                            notebook.current_page_mut().update_strokes_ephemeral(&updated);
                                            canvas.sync_page(notebook.current_page_mut());
                                            window.request_redraw();
                                        }
                                        DragMode::None => {}
                                    }
                                }
                            } else if is_erasing {
                                if let Some(page_pt) = canvas.screen_to_page(sx, sy) {
                                    let page = notebook.current_page_mut();
                                    let removed = page.erase_at(page_pt, eraser_radius, stroke_max_radius);
                                    if !removed.is_empty() {
                                        canvas.sync_page(page);
                                        autosave.mark_page_dirty(notebook.current_page(), &manifest);
                                        update_window_title(&window, &notebook, &manifest, ui_state.active_tool, autosave.status());
                                        window.request_redraw();
                                    }
                                }
                            } else if is_drawing && is_mouse_drawing {
                                // Draw-and-Hold Bewegungsmessung: Bei Bewegung > 3 px Timer zurücksetzen
                                let dx = sx - stroke_hold_anchor_pos.0;
                                let dy = sy - stroke_hold_anchor_pos.1;
                                if (dx * dx + dy * dy).sqrt() > 3.0 {
                                    stroke_last_motion_time = Instant::now();
                                    stroke_hold_anchor_pos = (sx, sy);
                                }

                                if !stroke_already_snapped {
                                    let page_pt = canvas.screen_to_page_clamped(sx, sy);
                                    let now = start_time.elapsed().as_secs_f64();
                                    canvas.extend_stroke(page_pt.x, page_pt.y, current_pressure, now);
                                    window.request_redraw();
                                }
                            }
                        }
                    }

                    WindowEvent::MouseWheel { delta, .. } => {
                        if is_pointer_in_ui {
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
                        canvas.zoom_at(cursor_screen_pos.0, cursor_screen_pos.1, zoom_factor);
                        ui_state.current_zoom = canvas.camera.zoom;
                        window.request_redraw();
                    }

                    WindowEvent::Touch(touch) => {
                        let now = start_time.elapsed().as_secs_f64();
                        let tx = touch.location.x as f32;
                        let ty = touch.location.y as f32;
                        let pressure = match touch.force {
                            Some(winit::event::Force::Normalized(f)) => f as f32,
                            Some(winit::event::Force::Calibrated {
                                force,
                                max_possible_force,
                                ..
                            }) => (force / max_possible_force).clamp(0.0, 1.0) as f32,
                            None => 0.5,
                        };

                        let is_eraser_action = ui_state.active_tool == ActiveTool::Eraser;

                        match touch.phase {
                            TouchPhase::Started => {
                                // Palm-Rejection
                                if is_mouse_drawing || (active_touch_id.is_some() && active_touch_id != Some(touch.id)) {
                                    return;
                                }

                                // Event-Isolation für Touch-Eingaben
                                let is_touch_in_ui = egui_ctx.wants_pointer_input()
                                    || ui_state.is_point_in_ui(tx, ty)
                                    || (ui_state.show_page_jump_popup && egui_ctx.is_pointer_over_area())
                                    || (ui_state.is_document_menu_open && egui_ctx.is_pointer_over_area());
                                if is_touch_in_ui {
                                    return;
                                }

                                active_touch_id = Some(touch.id);
                                current_pressure = pressure;
                                cursor_screen_pos = (tx, ty);

                                if is_eraser_action {
                                    is_erasing = true;
                                    if let Some(page_pt) = canvas.screen_to_page(tx, ty) {
                                        let page = notebook.current_page_mut();
                                        let removed = page.erase_at(page_pt, eraser_radius, stroke_max_radius);
                                        if !removed.is_empty() {
                                            canvas.sync_page(page);
                                            autosave.mark_page_dirty(notebook.current_page(), &manifest);
                                            update_window_title(&window, &notebook, &manifest, ui_state.active_tool, autosave.status());
                                            window.request_redraw();
                                        }
                                    }
                                } else if ui_state.active_tool == ActiveTool::Select {
                                    let screen_vec = Vec2::new(tx, ty);
                                    let hit = selection_state.hit_test_handles(notebook.current_page(), &canvas, screen_vec);
                                    match hit {
                                        SelectionHandleHit::RotateHandle { center_page } => {
                                            if let Some(page_pt) = canvas.screen_to_page(tx, ty) {
                                                let start_angle = (page_pt.y - center_page.y).atan2(page_pt.x - center_page.x);
                                                let initial_strokes = notebook.current_page().get_strokes(&selection_state.selected_ids);
                                                selection_state.drag_mode = DragMode::Rotate {
                                                    center_page,
                                                    start_angle,
                                                    initial_strokes,
                                                };
                                                window.request_redraw();
                                            }
                                        }
                                        SelectionHandleHit::CornerHandle {
                                            corner_idx: _,
                                            fixed_corner_page,
                                            start_pos_page,
                                            initial_box_size,
                                        } => {
                                            let initial_strokes = notebook.current_page().get_strokes(&selection_state.selected_ids);
                                            selection_state.drag_mode = DragMode::Scale {
                                                fixed_corner_page,
                                                start_pos_page,
                                                initial_box_size,
                                                initial_strokes,
                                            };
                                            window.request_redraw();
                                        }
                                        SelectionHandleHit::Body => {
                                            if let Some(page_pt) = canvas.screen_to_page(tx, ty) {
                                                let initial_strokes = notebook.current_page().get_strokes(&selection_state.selected_ids);
                                                selection_state.drag_mode = DragMode::Move {
                                                    start_page: page_pt,
                                                    initial_strokes,
                                                };
                                                window.request_redraw();
                                            }
                                        }
                                        SelectionHandleHit::None => {
                                            if let Some(page_pt) = canvas.screen_to_page(tx, ty) {
                                                if let Some(stroke) = notebook.current_page().find_stroke_at(page_pt, 25.0) {
                                                    let stroke_id = stroke.id;
                                                    if !selection_state.selected_ids.contains(&stroke_id) {
                                                        selection_state.select_single(stroke_id);
                                                    }
                                                    let initial_strokes = notebook.current_page().get_strokes(&selection_state.selected_ids);
                                                    selection_state.drag_mode = DragMode::Move {
                                                        start_page: page_pt,
                                                        initial_strokes,
                                                    };
                                                    window.request_redraw();
                                                } else {
                                                    selection_state.clear();
                                                    selection_state.drag_mode = DragMode::BoxSelect {
                                                        start_page: page_pt,
                                                        current_page: page_pt,
                                                    };
                                                    window.request_redraw();
                                                }
                                            }
                                        }
                                    }
                                } else if ui_state.active_tool.is_drawing() || ui_state.active_tool == ActiveTool::Shape {
                                    if let Some(page_pt) = canvas.screen_to_page(tx, ty) {
                                        is_drawing = true;
                                        stroke_last_motion_time = Instant::now();
                                        stroke_hold_anchor_pos = (tx, ty);
                                        stroke_already_snapped = false;
                                        stroke_snapped_shape = None;
                                        canvas.start_stroke(page_pt.x, page_pt.y, pressure, now);
                                        window.request_redraw();
                                    }
                                }
                            }
                            TouchPhase::Moved => {
                                if is_mouse_drawing || active_touch_id != Some(touch.id) {
                                    return;
                                }

                                current_pressure = pressure;
                                cursor_screen_pos = (tx, ty);

                                if ui_state.active_tool == ActiveTool::Select && selection_state.drag_mode != DragMode::None {
                                    if let Some(page_pt) = canvas.screen_to_page(tx, ty) {
                                        match &mut selection_state.drag_mode {
                                            DragMode::BoxSelect { current_page, .. } => {
                                                *current_page = page_pt;
                                                window.request_redraw();
                                            }
                                            DragMode::Move { start_page, initial_strokes } => {
                                                let delta = page_pt - *start_page;
                                                let mut updated = initial_strokes.clone();
                                                for s in &mut updated {
                                                    s.translate(delta);
                                                }
                                                notebook.current_page_mut().update_strokes_ephemeral(&updated);
                                                canvas.sync_page(notebook.current_page_mut());
                                                window.request_redraw();
                                            }
                                            DragMode::Rotate { center_page, start_angle, initial_strokes } => {
                                                let current_angle = (page_pt.y - center_page.y).atan2(page_pt.x - center_page.x);
                                                let delta_angle = current_angle - *start_angle;
                                                let mut updated = initial_strokes.clone();
                                                for s in &mut updated {
                                                    s.rotate(*center_page, delta_angle);
                                                }
                                                notebook.current_page_mut().update_strokes_ephemeral(&updated);
                                                canvas.sync_page(notebook.current_page_mut());
                                                window.request_redraw();
                                            }
                                            DragMode::Scale { fixed_corner_page, start_pos_page, initial_box_size: _, initial_strokes } => {
                                                let orig_delta = *start_pos_page - *fixed_corner_page;
                                                let new_delta = page_pt - *fixed_corner_page;
                                                let scale_x = if orig_delta.x.abs() > 1.0 {
                                                    (new_delta.x / orig_delta.x).clamp(0.05, 20.0)
                                                } else {
                                                    1.0
                                                };
                                                let scale_y = if orig_delta.y.abs() > 1.0 {
                                                    (new_delta.y / orig_delta.y).clamp(0.05, 20.0)
                                                } else {
                                                    1.0
                                                };
                                                let factors = Vec2::new(scale_x, scale_y);
                                                let mut updated = initial_strokes.clone();
                                                for s in &mut updated {
                                                    s.scale(*fixed_corner_page, factors);
                                                }
                                                notebook.current_page_mut().update_strokes_ephemeral(&updated);
                                                canvas.sync_page(notebook.current_page_mut());
                                                window.request_redraw();
                                            }
                                            DragMode::None => {}
                                        }
                                    }
                                } else if is_erasing {
                                    if let Some(page_pt) = canvas.screen_to_page(tx, ty) {
                                        let page = notebook.current_page_mut();
                                        let removed = page.erase_at(page_pt, eraser_radius, stroke_max_radius);
                                        if !removed.is_empty() {
                                            canvas.sync_page(page);
                                            autosave.mark_page_dirty(notebook.current_page(), &manifest);
                                            update_window_title(&window, &notebook, &manifest, ui_state.active_tool, autosave.status());
                                            window.request_redraw();
                                        }
                                    }
                                } else if is_drawing {
                                    let dx = tx - stroke_hold_anchor_pos.0;
                                    let dy = ty - stroke_hold_anchor_pos.1;
                                    if (dx * dx + dy * dy).sqrt() > 3.0 {
                                        stroke_last_motion_time = Instant::now();
                                        stroke_hold_anchor_pos = (tx, ty);
                                    }

                                    if !stroke_already_snapped {
                                        let page_pt = canvas.screen_to_page_clamped(tx, ty);
                                        canvas.extend_stroke(page_pt.x, page_pt.y, pressure, now);
                                        window.request_redraw();
                                    }
                                }
                            }
                            TouchPhase::Ended => {
                                if active_touch_id == Some(touch.id) {
                                    active_touch_id = None;
                                    if is_erasing {
                                        is_erasing = false;
                                    }
                                    if ui_state.active_tool == ActiveTool::Select {
                                        match std::mem::take(&mut selection_state.drag_mode) {
                                            DragMode::BoxSelect { start_page, current_page } => {
                                                let box_min = Vec2::new(start_page.x.min(current_page.x), start_page.y.min(current_page.y));
                                                let box_max = Vec2::new(start_page.x.max(current_page.x), start_page.y.max(current_page.y));
                                                if (box_max.x - box_min.x).max(box_max.y - box_min.y) > 3.0 {
                                                    let found_ids = notebook.current_page().find_strokes_in_box(box_min, box_max);
                                                    selection_state.select_multiple(found_ids);
                                                }
                                                window.request_redraw();
                                            }
                                            DragMode::Move { initial_strokes, .. }
                                            | DragMode::Rotate { initial_strokes, .. }
                                            | DragMode::Scale { initial_strokes, .. } => {
                                                let final_strokes = notebook.current_page().get_strokes(&selection_state.selected_ids);
                                                if !initial_strokes.is_empty() && !final_strokes.is_empty() && initial_strokes != final_strokes {
                                                    notebook.current_page_mut().commit_modified_strokes(initial_strokes, final_strokes);
                                                    autosave.mark_page_dirty(notebook.current_page(), &manifest);
                                                }
                                                window.request_redraw();
                                            }
                                            DragMode::None => {}
                                        }
                                    }
                                    if is_drawing {
                                        is_drawing = false;
                                        if !stroke_already_snapped {
                                            let page_pt = canvas.screen_to_page_clamped(tx, ty);
                                            let now = start_time.elapsed().as_secs_f64();
                                            canvas.extend_stroke(page_pt.x, page_pt.y, pressure, now);
                                        }
                                        let new_id = notebook.next_stroke_id();

                                        let snapped_shape = stroke_snapped_shape.take().or_else(|| {
                                            if ui_state.active_tool == ActiveTool::Shape {
                                                geometry::detect_shape(&canvas.live_samples)
                                            } else {
                                                None
                                            }
                                        });

                                        let stroke_opt = if let Some(shape) = snapped_shape {
                                            let stroke_width = ui_state.pen_stroke_width_mm * ink_notes::document::UNITS_PER_MM;
                                            let color = canvas.current_ink_color;
                                            canvas.cancel_stroke();
                                            Some(geometry::shape_to_stroke(shape, new_id, color, stroke_width))
                                        } else {
                                            canvas.finish_stroke(new_id)
                                        };

                                        if let Some(stroke) = stroke_opt {
                                            let page = notebook.current_page_mut();
                                            page.add_stroke(stroke);
                                            canvas.sync_page(page);
                                            manifest.next_stroke_id = notebook.next_stroke_id;
                                            autosave.mark_page_dirty(notebook.current_page(), &manifest);
                                            update_window_title(&window, &notebook, &manifest, ui_state.active_tool, autosave.status());
                                            window.request_redraw();
                                        }
                                    }
                                }
                            }
                            TouchPhase::Cancelled => {
                                if active_touch_id == Some(touch.id) {
                                    active_touch_id = None;
                                    selection_state.drag_mode = DragMode::None;
                                    if is_drawing {
                                        is_drawing = false;
                                        canvas.cancel_stroke();
                                        window.request_redraw();
                                    }
                                    is_erasing = false;
                                }
                            }
                        }
                    }

                    WindowEvent::RedrawRequested => {
                        // 1. egui Eingabe erfassen und Frame starten
                        let raw_input = egui_winit_state.take_egui_input(&window);
                        egui_ctx.begin_frame(raw_input);

                        // 2. Auswahl-Overlay rendern (Bounding Box, Griffe, Lasso)
                        render_selection_overlay(&egui_ctx, &selection_state, notebook.current_page(), &canvas);

                        // 3. Toolbar & Widgets rendern
                        let actions = ui::render_ui(&egui_ctx, &mut ui_state, &notebook, autosave.status());

                        // 4. UI-Aktionen verarbeiten
                        for action in actions {
                            match action {
                                UiAction::SetTool(tool) => {
                                    ui_state.active_tool = tool;
                                    match tool {
                                        ActiveTool::Pen => {
                                            canvas.set_stroke_color(ui_state.pen_color);
                                            canvas.set_stroke_width_mm(ui_state.pen_stroke_width_mm);
                                        }
                                        ActiveTool::Highlighter => {
                                            canvas.set_stroke_color(ui_state.highlighter_color);
                                            canvas.set_stroke_width_mm(ui_state.highlighter_stroke_width_mm);
                                        }
                                        ActiveTool::Shape => {
                                            canvas.set_stroke_color(ui_state.pen_color);
                                            canvas.set_stroke_width_mm(ui_state.pen_stroke_width_mm);
                                        }
                                        ActiveTool::Eraser | ActiveTool::Select => {}
                                    }
                                    update_window_title(&window, &notebook, &manifest, tool, autosave.status());
                                    window.request_redraw();
                                }
                                UiAction::SetColor(color) => {
                                    if !selection_state.is_empty()
                                        && selection_state.recolor_selected(notebook.current_page_mut(), color) {
                                            canvas.sync_page(notebook.current_page_mut());
                                            autosave.mark_page_dirty(notebook.current_page(), &manifest);
                                            ui_state.set_export_message("Auswahl umgefärbt".to_string());
                                        }
                                    match ui_state.active_tool {
                                        ActiveTool::Pen | ActiveTool::Shape => {
                                            ui_state.pen_color = color;
                                            canvas.set_stroke_color(color);
                                        }
                                        ActiveTool::Highlighter => {
                                            ui_state.highlighter_color = color;
                                            canvas.set_stroke_color(color);
                                        }
                                        ActiveTool::Eraser | ActiveTool::Select => {}
                                    }
                                    window.request_redraw();
                                }
                                UiAction::SetStrokeWidthMm(width) => {
                                    match ui_state.active_tool {
                                        ActiveTool::Pen | ActiveTool::Shape => {
                                            ui_state.pen_stroke_width_mm = width;
                                            canvas.set_stroke_width_mm(width);
                                        }
                                        ActiveTool::Highlighter => {
                                            ui_state.highlighter_stroke_width_mm = width;
                                            canvas.set_stroke_width_mm(width);
                                        }
                                        ActiveTool::Eraser | ActiveTool::Select => {}
                                    }
                                    window.request_redraw();
                                }
                                UiAction::DeleteSelected => {
                                    if !selection_state.is_empty() {
                                        let removed = selection_state.delete_selected(notebook.current_page_mut());
                                        if !removed.is_empty() {
                                            canvas.sync_page(notebook.current_page_mut());
                                            autosave.mark_page_dirty(notebook.current_page(), &manifest);
                                            ui_state.set_export_message(format!("{} Objekt(e) gelöscht", removed.len()));
                                            window.request_redraw();
                                        }
                                    }
                                }
                                UiAction::CopySelected => {
                                    let count = selection_state.copy_to_clipboard(notebook.current_page());
                                    if count > 0 {
                                        ui_state.set_export_message(format!("{count} Objekt(e) kopiert"));
                                        window.request_redraw();
                                    }
                                }
                                UiAction::PasteSelected => {
                                    let mut next_id = notebook.next_stroke_id;
                                    let new_ids = selection_state.paste_from_clipboard(
                                        notebook.current_page_mut(),
                                        &mut || {
                                            let id = next_id;
                                            next_id += 1;
                                            id
                                        },
                                    );
                                    notebook.next_stroke_id = next_id;
                                    if !new_ids.is_empty() {
                                        canvas.sync_page(notebook.current_page_mut());
                                        manifest.next_stroke_id = notebook.next_stroke_id;
                                        autosave.mark_page_dirty(notebook.current_page(), &manifest);
                                        ui_state.set_export_message(format!("{} Objekt(e) eingefügt", new_ids.len()));
                                        window.request_redraw();
                                    }
                                }
                                UiAction::RotateSelected => {
                                    if !selection_state.is_empty()
                                        && selection_state.rotate_selected_90_deg(notebook.current_page_mut()) {
                                            canvas.sync_page(notebook.current_page_mut());
                                            autosave.mark_page_dirty(notebook.current_page(), &manifest);
                                            ui_state.set_export_message("Auswahl um 90° gedreht".to_string());
                                            window.request_redraw();
                                        }
                                }
                                UiAction::ZoomIn => {
                                    let center_x = canvas.size.width as f32 * 0.5;
                                    let center_y = canvas.size.height as f32 * 0.5;
                                    canvas.zoom_at(center_x, center_y, 1.25);
                                    ui_state.current_zoom = canvas.camera.zoom;
                                    window.request_redraw();
                                }
                                UiAction::ZoomOut => {
                                    let center_x = canvas.size.width as f32 * 0.5;
                                    let center_y = canvas.size.height as f32 * 0.5;
                                    canvas.zoom_at(center_x, center_y, 1.0 / 1.25);
                                    ui_state.current_zoom = canvas.camera.zoom;
                                    window.request_redraw();
                                }
                                UiAction::ResetZoom => {
                                    canvas.reset_view();
                                    ui_state.current_zoom = canvas.camera.zoom;
                                    window.request_redraw();
                                }
                                UiAction::Undo => {
                                    if notebook.current_page_mut().undo() {
                                        selection_state.clear();
                                        canvas.sync_page(notebook.current_page_mut());
                                        autosave.mark_page_dirty(notebook.current_page(), &manifest);
                                        update_window_title(&window, &notebook, &manifest, ui_state.active_tool, autosave.status());
                                        window.request_redraw();
                                    }
                                }
                                UiAction::Redo => {
                                    if notebook.current_page_mut().redo() {
                                        selection_state.clear();
                                        canvas.sync_page(notebook.current_page_mut());
                                        autosave.mark_page_dirty(notebook.current_page(), &manifest);
                                        update_window_title(&window, &notebook, &manifest, ui_state.active_tool, autosave.status());
                                        window.request_redraw();
                                    }
                                }
                                UiAction::PreviousPage => {
                                    selection_state.clear();
                                    notebook.previous_page();
                                    let curr = notebook.current_page_index;
                                    let _ = storage::ensure_page_loaded(&storage_dir, &mut notebook, curr);
                                    canvas.sync_page(notebook.current_page_mut());
                                    manifest.current_page_index = notebook.current_page_index;
                                    autosave.mark_page_dirty(notebook.current_page(), &manifest);
                                    update_window_title(&window, &notebook, &manifest, ui_state.active_tool, autosave.status());
                                    window.request_redraw();
                                }
                                UiAction::NextPage => {
                                    selection_state.clear();
                                    notebook.next_page();
                                    let curr = notebook.current_page_index;
                                    let _ = storage::ensure_page_loaded(&storage_dir, &mut notebook, curr);
                                    canvas.sync_page(notebook.current_page_mut());
                                    manifest.current_page_index = notebook.current_page_index;
                                    manifest.next_page_id = notebook.next_page_id;
                                    autosave.mark_page_dirty(notebook.current_page(), &manifest);
                                    update_window_title(&window, &notebook, &manifest, ui_state.active_tool, autosave.status());
                                    window.request_redraw();
                                }
                                UiAction::AddPage => {
                                    selection_state.clear();
                                    let bg = notebook.current_page().background;
                                    notebook.add_page(bg);
                                    manifest.current_page_index = notebook.current_page_index;
                                    manifest.next_page_id = notebook.next_page_id;
                                    canvas.sync_page(notebook.current_page_mut());
                                    autosave.mark_page_dirty(notebook.current_page(), &manifest);
                                    update_window_title(&window, &notebook, &manifest, ui_state.active_tool, autosave.status());
                                    window.request_redraw();
                                }
                                UiAction::GoToPage(idx) => {
                                    if idx < notebook.pages.len() {
                                        selection_state.clear();
                                        notebook.current_page_index = idx;
                                        let _ = storage::ensure_page_loaded(&storage_dir, &mut notebook, idx);
                                        canvas.sync_page(notebook.current_page_mut());
                                        manifest.current_page_index = idx;
                                        autosave.mark_page_dirty(notebook.current_page(), &manifest);
                                        update_window_title(&window, &notebook, &manifest, ui_state.active_tool, autosave.status());
                                        window.request_redraw();
                                    }
                                }
                                UiAction::SetBackground(bg) => {
                                    let page = notebook.current_page_mut();
                                    page.background = bg;
                                    canvas.sync_page(page);
                                    autosave.mark_page_dirty(notebook.current_page(), &manifest);
                                    update_window_title(&window, &notebook, &manifest, ui_state.active_tool, autosave.status());
                                    window.request_redraw();
                                }
                                UiAction::ExportPdf => {
                                    let notebook_clone = notebook.clone();
                                    let export_path = "export.pdf".to_string();
                                    let window_ref = window.clone();
                                    println!(
                                        "[Export] Starte PDF-Vektorexport nach '{}' (im Hintergrundthread)...",
                                        export_path
                                    );
                                    std::thread::spawn(move || {
                                        let start = std::time::Instant::now();
                                        match export_notebook_to_pdf(
                                            &notebook_clone,
                                            &export_path,
                                            ExportStrategy::FilledPolygon,
                                        ) {
                                            Ok(()) => {
                                                let elapsed = start.elapsed();
                                                println!(
                                                    "[Export] PDF erfolgreich exportiert: '{}' ({:.1?} für {} Seiten)",
                                                    export_path,
                                                    elapsed,
                                                    notebook_clone.pages.len()
                                                );
                                                window_ref.request_redraw();
                                            }
                                            Err(err) => {
                                                eprintln!("[Export-Fehler] Konnte PDF nicht exportieren: {err}");
                                            }
                                        }
                                    });
                                    ui_state.set_export_message("PDF erfolgreich als export.pdf exportiert!");
                                    window.request_redraw();
                                }
                                UiAction::RefreshNotebooks => {
                                    ui_state.available_notebooks = storage::list_notebooks(&storage_base_dir);
                                    window.request_redraw();
                                }
                                UiAction::OpenNotebook(id) => {
                                    if id == manifest.id {
                                        ui_state.is_document_menu_open = false;
                                        window.request_redraw();
                                    } else {
                                        let new_notebook_dir = storage_base_dir.join(&id);
                                        match storage::load_notebook(&new_notebook_dir) {
                                            Ok((loaded_nb, loaded_manifest)) => {
                                                autosave.switch_notebook_dir(new_notebook_dir.clone());
                                                notebook = loaded_nb;
                                                manifest = loaded_manifest;
                                                storage_dir = new_notebook_dir;
                                                selection_state.clear();
                                                ui_state.current_notebook_id = manifest.id.clone();
                                                ui_state.document_title = manifest.title.clone();
                                                ui_state.available_notebooks = storage::list_notebooks(&storage_base_dir);
                                                ui_state.is_document_menu_open = false;
                                                canvas.sync_page(notebook.current_page_mut());
                                                update_window_title(&window, &notebook, &manifest, ui_state.active_tool, autosave.status());
                                                ui_state.set_export_message(format!("Dokument '{}' geöffnet", manifest.title));
                                                window.request_redraw();
                                            }
                                            Err(err) => {
                                                eprintln!("[Dokument-Fehler] Konnte Dokument '{id}' nicht laden: {err}");
                                                ui_state.set_export_message(format!("Fehler beim Öffnen: {err}"));
                                            }
                                        }
                                    }
                                }
                                UiAction::CreateNotebook(title) => {
                                    autosave.flush();
                                    match storage::create_new_notebook(&storage_base_dir, &title) {
                                        Ok((new_nb, new_manifest, new_dir)) => {
                                            autosave.switch_notebook_dir(new_dir.clone());
                                            notebook = new_nb;
                                            manifest = new_manifest;
                                            storage_dir = new_dir;
                                            selection_state.clear();
                                            ui_state.current_notebook_id = manifest.id.clone();
                                            ui_state.document_title = manifest.title.clone();
                                            ui_state.available_notebooks = storage::list_notebooks(&storage_base_dir);
                                            ui_state.is_document_menu_open = false;
                                            canvas.sync_page(notebook.current_page_mut());
                                            update_window_title(&window, &notebook, &manifest, ui_state.active_tool, autosave.status());
                                            ui_state.set_export_message(format!("Dokument '{}' erstellt", manifest.title));
                                            window.request_redraw();
                                        }
                                        Err(err) => {
                                            eprintln!("[Dokument-Fehler] Konnte Dokument '{title}' nicht erstellen: {err}");
                                            ui_state.set_export_message(format!("Fehler beim Erstellen: {err}"));
                                        }
                                    }
                                }
                                UiAction::RenameNotebook { id, new_title } => {
                                    let target_dir = storage_base_dir.join(&id);
                                    if id == manifest.id {
                                        match storage::rename_notebook(&target_dir, &mut manifest, &new_title) {
                                            Ok(()) => {
                                                ui_state.document_title = manifest.title.clone();
                                                ui_state.available_notebooks = storage::list_notebooks(&storage_base_dir);
                                                update_window_title(&window, &notebook, &manifest, ui_state.active_tool, autosave.status());
                                                ui_state.set_export_message(format!("Umbenannt in '{}'", new_title));
                                                window.request_redraw();
                                            }
                                            Err(err) => {
                                                eprintln!("[Dokument-Fehler] Konnte Dokument '{id}' nicht umbenennen: {err}");
                                                ui_state.set_export_message(format!("Fehler beim Umbenennen: {err}"));
                                            }
                                        }
                                    } else {
                                        let manifest_path = target_dir.join("manifest.json");
                                        if let Ok(content) = std::fs::read_to_string(&manifest_path) {
                                            if let Ok(mut other_manifest) = serde_json::from_str::<NotebookManifest>(&content) {
                                                let _ = storage::rename_notebook(&target_dir, &mut other_manifest, &new_title);
                                                ui_state.available_notebooks = storage::list_notebooks(&storage_base_dir);
                                                ui_state.set_export_message(format!("Umbenannt in '{}'", new_title));
                                                window.request_redraw();
                                            }
                                        }
                                    }
                                }
                                UiAction::DeleteNotebook(id) => {
                                    let target_dir = storage_base_dir.join(&id);
                                    let is_current = id == manifest.id;
                                    match storage::delete_notebook(&target_dir) {
                                        Ok(()) => {
                                            ui_state.available_notebooks = storage::list_notebooks(&storage_base_dir);
                                            if is_current {
                                                if let Some(first) = ui_state.available_notebooks.first().cloned() {
                                                    let new_notebook_dir = storage_base_dir.join(&first.id);
                                                    match storage::load_notebook(&new_notebook_dir) {
                                                        Ok((loaded_nb, loaded_manifest)) => {
                                                            autosave.switch_notebook_dir(new_notebook_dir.clone());
                                                            notebook = loaded_nb;
                                                            manifest = loaded_manifest;
                                                            storage_dir = new_notebook_dir;
                                                            selection_state.clear();
                                                            ui_state.current_notebook_id = manifest.id.clone();
                                                            ui_state.document_title = manifest.title.clone();
                                                            canvas.sync_page(notebook.current_page_mut());
                                                            update_window_title(&window, &notebook, &manifest, ui_state.active_tool, autosave.status());
                                                            ui_state.set_export_message(format!("Gewechselt zu '{}'", manifest.title));
                                                        }
                                                        Err(err) => {
                                                            eprintln!("[Dokument-Fehler] Konnte Dokument '{}' nicht laden: {err}", first.id);
                                                        }
                                                    }
                                                } else if let Ok((new_nb, new_manifest, new_dir)) = storage::create_new_notebook(&storage_base_dir, "Standard-Notizbuch") {
                                                    autosave.switch_notebook_dir(new_dir.clone());
                                                    notebook = new_nb;
                                                    manifest = new_manifest;
                                                    storage_dir = new_dir;
                                                    selection_state.clear();
                                                    ui_state.current_notebook_id = manifest.id.clone();
                                                    ui_state.document_title = manifest.title.clone();
                                                    ui_state.available_notebooks = storage::list_notebooks(&storage_base_dir);
                                                    canvas.sync_page(notebook.current_page_mut());
                                                    update_window_title(&window, &notebook, &manifest, ui_state.active_tool, autosave.status());
                                                    ui_state.set_export_message("Standard-Notizbuch erstellt".to_string());
                                                }
                                            } else {
                                                ui_state.set_export_message("Dokument gelöscht".to_string());
                                            }
                                            window.request_redraw();
                                        }
                                        Err(err) => {
                                            eprintln!("[Dokument-Fehler] Konnte Dokument '{id}' nicht löschen: {err}");
                                            ui_state.set_export_message(format!("Fehler beim Löschen: {err}"));
                                        }
                                    }
                                }
                            }
                        }

                        // 4. egui Frame beenden & Primitives tessellieren
                        let full_output = egui_ctx.end_frame();
                        egui_winit_state.handle_platform_output(&window, full_output.platform_output);

                        let clipped_primitives = egui_ctx.tessellate(full_output.shapes, egui_ctx.pixels_per_point());
                        let screen_descriptor = egui_wgpu::ScreenDescriptor {
                            size_in_pixels: [canvas.size.width, canvas.size.height],
                            pixels_per_point: egui_ctx.pixels_per_point(),
                        };

                        if ui_state.export_message.is_some() {
                            window.request_redraw();
                        }

                        // 5. Canvas (Pass 1) und egui UI (Pass 2) in Surface-Textur rendern
                        match canvas.render(&clipped_primitives, &screen_descriptor, &full_output.textures_delta) {
                            Ok(_) => {}
                            Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                                canvas.resize(canvas.size);
                                canvas.sync_page(notebook.current_page_mut());
                            }
                            Err(wgpu::SurfaceError::OutOfMemory) => {
                                eprintln!("GPU Out of Memory!");
                                target.exit();
                            }
                            Err(wgpu::SurfaceError::Timeout) => {
                                eprintln!("Surface present timeout");
                            }
                        }
                    }

                    _ => {}
                }
            }
            _ => {}
        }
    })?;

    Ok(())
}
