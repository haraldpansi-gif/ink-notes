//! ink-notes: Latenzoptimierte Linux-Inking-App mit wgpu, winit, egui-Toolbar,
//! Seitenverwaltung, Undo/Redo, Vektor-PDF-Export und asynchroner Notizbuch-Persistenz (Autosave).

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};
use winit::{
    event::{
        ElementState, Event, KeyEvent, MouseButton, MouseScrollDelta, TouchPhase, WindowEvent,
    },
    event_loop::{ControlFlow, EventLoopBuilder},
    keyboard::{Key, ModifiersState, NamedKey},
    platform::{wayland::EventLoopBuilderExtWayland, x11::EventLoopBuilderExtX11},
    window::{Window, WindowBuilder},
};

use glam::Vec2;
use ink_notes::autosave::{AutosaveController, AutosaveStatus};
use ink_notes::canvas::CanvasState;
use ink_notes::document::{Notebook, PageBackground};
use ink_notes::export::{self, export_notebook_to_pdf, ExportStrategy};
use ink_notes::geometry::{self, RecognizedShape};
use ink_notes::platform::{self, DisplayBackend};
use ink_notes::selection::{
    render_selection_overlay, DragMode, SelectionHandleHit, SelectionState,
};
use ink_notes::settings::{SidebarSide, UiSettings};
use ink_notes::storage::{self, NotebookManifest};
use ink_notes::ui::{self, ActiveTool, UiAction, UiState};
use ink_notes::updater::UpdaterController;

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

/// Exportiert das Notizbuch im Hintergrund als PDF. Noch nicht geladene Seiten werden vorher
/// von der Festplatte nachgeladen; das Ergebnis geht über `result_tx` an den Event-Loop zurück.
fn start_pdf_export(
    notebook: &Notebook,
    storage_dir: &Path,
    title: &str,
    result_tx: Sender<Result<PathBuf, String>>,
    window: Arc<Window>,
) {
    let mut notebook_clone = notebook.clone();
    let storage_dir = storage_dir.to_path_buf();
    let export_path = export::resolve_pdf_export_path(title);
    println!(
        "[Export] Starte PDF-Vektorexport nach '{}' (im Hintergrundthread)...",
        export_path.display()
    );
    std::thread::spawn(move || {
        let start = Instant::now();
        let result = storage::load_all_pages(&storage_dir, &mut notebook_clone)
            .map_err(|err| err.to_string())
            .and_then(|()| {
                export_notebook_to_pdf(&notebook_clone, &export_path, ExportStrategy::FilledPolygon)
                    .map_err(|err| err.to_string())
            });
        match &result {
            Ok(()) => println!(
                "[Export] PDF erfolgreich exportiert: '{}' ({:.1?} für {} Seiten)",
                export_path.display(),
                start.elapsed(),
                notebook_clone.pages.len()
            ),
            Err(err) => eprintln!("[Export-Fehler] Konnte PDF nicht exportieren: {err}"),
        }
        let _ = result_tx.send(result.map(|()| export_path));
        window.request_redraw();
    });
}

/// Finalisiert einen Strich (mit Spline-Tessellierung, Ecken-Erkennung & RDP oder Auto-Snap)
/// und fügt ihn in die aktuelle Seite des Notizbuchs ein.
fn commit_stroke_to_notebook(
    canvas: &mut CanvasState,
    notebook: &mut Notebook,
    manifest: &mut NotebookManifest,
    autosave: &mut AutosaveController,
    ui_state: &UiState,
    window: &Window,
    snapped_shape: Option<RecognizedShape>,
) {
    let new_id = notebook.next_stroke_id();
    let stroke_opt = if let Some(shape) = snapped_shape {
        let stroke_width = ui_state.pen_stroke_width_mm * ink_notes::document::UNITS_PER_MM;
        let color = canvas.current_ink_color;
        canvas.cancel_stroke();
        Some(geometry::shape_to_stroke(
            shape,
            new_id,
            color,
            stroke_width,
        ))
    } else {
        canvas.finish_stroke(new_id)
    };

    if let Some(stroke) = stroke_opt {
        let page = notebook.current_page_mut();
        page.add_stroke(stroke);
        canvas.sync_page(page);
        manifest.next_stroke_id = notebook.next_stroke_id;
        autosave.mark_dirty(notebook, manifest);
        update_window_title(
            window,
            notebook,
            manifest,
            ui_state.active_tool,
            autosave.status(),
        );
        window.request_redraw();
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Anzeigesystem wählen: Unter WSL (WSLg) bricht Wayland ab, X11 (Xwayland) läuft stabil
    let mut event_loop_builder = EventLoopBuilder::new();
    match platform::display_backend_from_env() {
        DisplayBackend::X11 => {
            eprintln!("[Anzeige] Verwende X11 (unter WSL Standard; übersteuerbar mit INK_NOTES_DISPLAY=wayland)");
            event_loop_builder.with_x11();
        }
        DisplayBackend::Wayland => {
            eprintln!("[Anzeige] Verwende Wayland (INK_NOTES_DISPLAY=wayland)");
            event_loop_builder.with_wayland();
        }
        DisplayBackend::Automatic => {}
    }
    let event_loop = event_loop_builder.build()?;
    let window = Arc::new(
        WindowBuilder::new()
            .with_title("ink-notes (Latenzoptimiertes Linux-Inking)")
            .with_inner_size(winit::dpi::LogicalSize::new(1280.0, 850.0))
            .build(&event_loop)?,
    );

    let mut canvas = match pollster::block_on(CanvasState::new(window.clone())) {
        Ok(canvas) => canvas,
        Err(err) => {
            eprintln!("[Grafik-Fehler] {err}");
            std::process::exit(1);
        }
    };

    let mut tablet_manager = match octotablet::builder::Builder::default().build_shared(&window) {
        Ok(mgr) => {
            println!("[Input] Octotablet Stylus-/Tablet-Manager erfolgreich initialisiert (Wayland/X11 Tablet-Protokoll aktiv).");
            Some(mgr)
        }
        Err(err) => {
            println!("[Input] Hinweis: Octotablet Tablet-Manager nicht aktiv ({err}). Nutze Standard-Event-Pipeline.");
            None
        }
    };

    // 1. Notizbuch aus Storage laden oder initialisieren (Session-Wiederherstellung)
    let storage_base_dir = storage::default_storage_dir();
    let _ = storage::cleanup_trash(
        &storage_base_dir,
        std::time::Duration::from_secs(30 * 24 * 3600),
    );
    let available_notebooks = storage::list_notebooks(&storage_base_dir);

    // Beschädigte Notizbücher nicht automatisch öffnen
    let (initial_id, initial_title) = if let Some(first) = available_notebooks
        .iter()
        .find(|s| s.status == storage::NotebookStatus::Normal)
    {
        (first.id.clone(), first.title.clone())
    } else {
        ("default".to_string(), "Standard-Notizbuch".to_string())
    };

    let mut storage_dir = storage_base_dir.join(&initial_id);
    let mut startup_warning: Option<String> = None;
    let (mut notebook, mut manifest) = if storage_dir.join("manifest.json").exists() {
        println!("[Storage] Lade Notizbuch aus: {}", storage_dir.display());
        match storage::load_notebook(&storage_dir) {
            Ok((nb, m)) => (nb, m),
            Err(err) => {
                // Den Ordner des nicht ladbaren Notizbuchs NICHT wiederverwenden, sonst würde
                // der Autosave dessen Manifest überschreiben und alle Seiten verwaist zurücklassen.
                eprintln!(
                    "[Storage-Fehler] Konnte Notizbuch nicht laden ({err}). '{}' bleibt unverändert, erstelle neues Notizbuch.",
                    storage_dir.display()
                );
                startup_warning = Some(format!(
                    "Notizbuch '{initial_title}' konnte nicht geladen werden – neues Notizbuch angelegt"
                ));
                let (nb, m, dir) = storage::create_new_notebook(&storage_base_dir, &initial_title)?;
                storage_dir = dir;
                (nb, m)
            }
        }
    } else {
        println!(
            "[Storage] Erstelle neues Notizbuch in: {}",
            storage_dir.display()
        );
        (
            Notebook::new(),
            NotebookManifest::new(&initial_id, &initial_title),
        )
    };

    // 2. Asynchronen Autosave-Controller mit 1.5s Debounce starten
    let mut autosave = AutosaveController::new(storage_dir.clone(), Duration::from_millis(1500));

    // 3. egui Context und State initialisieren
    let egui_ctx = egui::Context::default();
    egui_ctx.set_visuals(egui::Visuals::dark());

    // 4. In-App Gitea Updater initialisieren und im Hintergrund nach Releases fragen
    // Ergebnisse der PDF-Exporte aus dem Hintergrundthread
    let (export_tx, export_rx) = mpsc::channel::<Result<PathBuf, String>>();

    let mut updater = UpdaterController::new();
    updater.check_for_updates();

    let mut egui_winit_state = egui_winit::State::new(
        egui_ctx.clone(),
        egui::ViewportId::ROOT,
        &window,
        Some(window.scale_factor() as f32),
        Some(2048),
    );

    let mut ui_state = UiState::new();
    // Gespeicherte UI-Einstellungen (Seite der Werkzeugleiste)
    let ui_settings_path = UiSettings::default_path();
    let mut ui_settings = ui_settings_path
        .as_deref()
        .map(UiSettings::load_from)
        .unwrap_or_default();
    ui_state.sidebar_side = ui_settings.sidebar_side;
    ui_state.current_notebook_id = manifest.id.clone();
    ui_state.document_title = manifest.title.clone();
    ui_state.available_notebooks = available_notebooks;
    if let Some(warning) = startup_warning {
        ui_state.available_notebooks = storage::list_notebooks(&storage_base_dir);
        ui_state.set_export_message(warning);
    }
    let mut selection_state = SelectionState::new();

    let mut _notebook_lock = match storage::NotebookLock::acquire(&storage_dir) {
        Ok(lock) => {
            autosave.set_read_only(false);
            Some(lock)
        }
        Err(storage::StorageError::AlreadyLocked(msg)) => {
            eprintln!("[Storage-Lock] {msg}");
            autosave.set_read_only(true);
            ui_state.set_export_message("⚠ In anderer Instanz geöffnet (Schreibschutz)");
            None
        }
        Err(e) => {
            eprintln!("[Storage-Lock] Konnte Lock nicht erwerben: {e}");
            autosave.set_read_only(true);
            None
        }
    };

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

    // Multi-Touch & Pinch-to-Zoom Zustand
    let mut active_touches: std::collections::HashMap<u64, (f32, f32)> =
        std::collections::HashMap::new();
    let mut pinch_prev_distance: Option<f32> = None;
    // Berührungen, die auf der UI begonnen haben, gehören bis zum Abheben egui
    let mut ui_touch_ids: std::collections::HashSet<u64> = std::collections::HashSet::new();
    let mut pinch_prev_center: Option<(f32, f32)> = None;
    let mut is_pinch_gesturing = false;
    let mut is_touch_drawing = false;
    let mut is_touch_panning = false;
    let mut touch_pan_last_pos = (0.0_f32, 0.0_f32);

    // Stylus / Digitizer Zustand (Octotablet / Wayland Tablet Protocol)
    let mut is_stylus_drawing = false;
    let mut is_stylus_in_proximity = false;
    let mut stylus_tool_id: Option<octotablet::tool::ID> = None;
    let mut stylus_barrel_pressed = false;
    let mut stylus_screen_pos = (0.0_f32, 0.0_f32);
    // Stift hat auf der UI aufgesetzt: alle Ereignisse bis zum Abheben gehen an egui
    let mut stylus_ui_capture = false;
    // Von egui angeforderter Zeitpunkt für das nächste Neuzeichnen (Layout-Vermessung, Tooltips)
    let mut egui_repaint_at: Option<Instant> = None;

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

    update_window_title(
        &window,
        &notebook,
        &manifest,
        ui_state.active_tool,
        autosave.status(),
    );

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
    println!("  Strg + E       : Vektor-PDF-Export (~/Documents/<Titel>.pdf)");
    println!("  Strg + S       : Jetzt speichern (Disk Flush)");
    println!("  ESC            : Dialoge / Menüs schließen oder Selektion aufheben");
    println!("  Strg + Q       : Beenden (mit sicherem Disk-Flush)");

    event_loop.run(move |event, target| {
        match event {
            Event::AboutToWait => {
                let prev_status = autosave.status();
                let new_status = autosave.update();
                if new_status != prev_status {
                    update_window_title(&window, &notebook, &manifest, ui_state.active_tool, new_status);
                }

                while let Ok(result) = export_rx.try_recv() {
                    match result {
                        Ok(path) => ui_state.set_export_message(format!("PDF gespeichert: {}", path.display())),
                        Err(err) => ui_state.set_export_message(format!("PDF-Export fehlgeschlagen: {err}")),
                    }
                    window.request_redraw();
                }

                let prev_upd = updater.status().clone();
                updater.update();
                if *updater.status() != prev_upd {
                    window.request_redraw();
                }

                // Stylus-/Tablet-Events via Octotablet abfragen (Wayland/X11 Tablet-Protokoll für aktive Stifte wie Lazarite)
                if let Some(ref mut mgr) = tablet_manager {
                    if let Ok(tablet_events) = mgr.pump() {
                        let scale = window.scale_factor() as f32;
                        let now = start_time.elapsed().as_secs_f64();

                        for t_event in tablet_events {
                            if let octotablet::events::Event::Tool { tool, event } = t_event {
                                let tool_is_hardware_eraser =
                                    tool.tool_type == Some(octotablet::tool::Type::Eraser);

                                match event {
                                    octotablet::events::ToolEvent::In { .. } => {
                                        is_stylus_in_proximity = true;
                                        stylus_tool_id = Some(tool.id());
                                    }
                                    octotablet::events::ToolEvent::Out
                                    | octotablet::events::ToolEvent::Removed => {
                                        is_stylus_in_proximity = false;
                                        if stylus_ui_capture {
                                            stylus_ui_capture = false;
                                            let (sx, sy) = stylus_screen_pos;
                                            let input = egui_winit_state.egui_input_mut();
                                            input.events.push(egui::Event::PointerButton {
                                                pos: egui::pos2(sx / scale, sy / scale),
                                                button: egui::PointerButton::Primary,
                                                pressed: false,
                                                modifiers: egui::Modifiers::default(),
                                            });
                                            input.events.push(egui::Event::PointerGone);
                                            window.request_redraw();
                                        }
                                        if stylus_tool_id == Some(tool.id()) {
                                            stylus_tool_id = None;
                                            if is_stylus_drawing {
                                                is_stylus_drawing = false;
                                                is_drawing = false;
                                                commit_stroke_to_notebook(
                                                    &mut canvas,
                                                    &mut notebook,
                                                    &mut manifest,
                                                    &mut autosave,
                                                    &ui_state,
                                                    &window,
                                                    stroke_snapped_shape.take(),
                                                );
                                            }
                                            if is_erasing {
                                                is_erasing = false;
                                            }
                                        }
                                    }
                                    octotablet::events::ToolEvent::Button { pressed, .. } => {
                                        stylus_barrel_pressed = pressed;
                                        if pressed && is_stylus_drawing {
                                            is_stylus_drawing = false;
                                            is_drawing = false;
                                            canvas.cancel_stroke();
                                            is_erasing = true;
                                            window.request_redraw();
                                        } else if !pressed
                                            && is_erasing
                                            && !tool_is_hardware_eraser
                                            && ui_state.active_tool != ActiveTool::Eraser
                                        {
                                            is_erasing = false;
                                        }
                                    }
                                    octotablet::events::ToolEvent::Pose(pose) => {
                                        is_stylus_in_proximity = true;
                                        let sx = pose.position[0] * scale;
                                        let sy = pose.position[1] * scale;
                                        stylus_screen_pos = (sx, sy);
                                        cursor_screen_pos = (sx, sy);

                                        if let Some(p) = pose.pressure.get() {
                                            current_pressure = p;
                                        }

                                        // Stiftbewegung an egui weitergeben (Hover, Schieberegler ziehen),
                                        // solange nicht auf dem Blatt gezeichnet oder radiert wird
                                        if stylus_ui_capture || (!is_stylus_drawing && !is_erasing) {
                                            let ui_pos = egui::pos2(sx / scale, sy / scale);
                                            egui_winit_state
                                                .egui_input_mut()
                                                .events
                                                .push(egui::Event::PointerMoved(ui_pos));
                                            if stylus_ui_capture
                                                || ui::is_screen_point_in_ui(&egui_ctx, &ui_state, (sx, sy), scale)
                                            {
                                                window.request_redraw();
                                            }
                                        }
                                        if stylus_ui_capture {
                                            continue;
                                        }

                                        if is_erasing {
                                            if let Some(page_pt) = canvas.screen_to_page(sx, sy) {
                                                let page = notebook.current_page_mut();
                                                let removed = page.erase_at(
                                                    page_pt,
                                                    eraser_radius,
                                                    stroke_max_radius,
                                                );
                                                if !removed.is_empty() {
                                                    canvas.sync_page(page);
                                                    autosave.mark_dirty(&notebook, &mut manifest);
                                                    update_window_title(
                                                        &window,
                                                        &notebook,
                                                        &manifest,
                                                        ui_state.active_tool,
                                                        autosave.status(),
                                                    );
                                                    window.request_redraw();
                                                }
                                            }
                                        } else if is_stylus_drawing && is_drawing {
                                            let dx = sx - stroke_hold_anchor_pos.0;
                                            let dy = sy - stroke_hold_anchor_pos.1;
                                            if (dx * dx + dy * dy).sqrt() > 3.0 {
                                                stroke_last_motion_time = Instant::now();
                                                stroke_hold_anchor_pos = (sx, sy);
                                            }

                                            if !stroke_already_snapped {
                                                let page_pt = canvas.screen_to_page_clamped(sx, sy);
                                                canvas.extend_stroke(
                                                    page_pt.x,
                                                    page_pt.y,
                                                    current_pressure,
                                                    now,
                                                );
                                                window.request_redraw();
                                            }
                                        }
                                    }
                                    octotablet::events::ToolEvent::Down => {
                                        is_stylus_in_proximity = true;
                                        stylus_tool_id = Some(tool.id());
                                        let (sx, sy) = stylus_screen_pos;

                                        if ui::is_screen_point_in_ui(&egui_ctx, &ui_state, (sx, sy), scale) {
                                            // In die Eingabe-Warteschlange des NÄCHSTEN egui-Frames
                                            // (egui_ctx.input_mut würde vom nächsten Frame überschrieben)
                                            let ui_pos = egui::pos2(sx / scale, sy / scale);
                                            let input = egui_winit_state.egui_input_mut();
                                            input.events.push(egui::Event::PointerMoved(ui_pos));
                                            input.events.push(egui::Event::PointerButton {
                                                pos: ui_pos,
                                                button: egui::PointerButton::Primary,
                                                pressed: true,
                                                modifiers: egui::Modifiers::default(),
                                            });
                                            stylus_ui_capture = true;
                                            window.request_redraw();
                                            continue;
                                        }

                                        let should_erase = tool_is_hardware_eraser
                                            || stylus_barrel_pressed
                                            || ui_state.active_tool == ActiveTool::Eraser;

                                        if should_erase {
                                            is_erasing = true;
                                            if let Some(page_pt) = canvas.screen_to_page(sx, sy) {
                                                let page = notebook.current_page_mut();
                                                let removed = page.erase_at(
                                                    page_pt,
                                                    eraser_radius,
                                                    stroke_max_radius,
                                                );
                                                if !removed.is_empty() {
                                                    canvas.sync_page(page);
                                                    autosave.mark_dirty(&notebook, &mut manifest);
                                                    update_window_title(
                                                        &window,
                                                        &notebook,
                                                        &manifest,
                                                        ui_state.active_tool,
                                                        autosave.status(),
                                                    );
                                                    window.request_redraw();
                                                }
                                            }
                                        } else if ui_state.active_tool.is_drawing()
                                            || ui_state.active_tool == ActiveTool::Shape
                                        {
                                            if let Some(page_pt) = canvas.screen_to_page(sx, sy) {
                                                is_stylus_drawing = true;
                                                is_drawing = true;
                                                stroke_last_motion_time = Instant::now();
                                                stroke_hold_anchor_pos = (sx, sy);
                                                stroke_already_snapped = false;
                                                stroke_snapped_shape = None;
                                                canvas.start_stroke(
                                                    page_pt.x,
                                                    page_pt.y,
                                                    current_pressure,
                                                    now,
                                                );
                                                window.request_redraw();
                                            }
                                        }
                                    }
                                    octotablet::events::ToolEvent::Up => {
                                        let (sx, sy) = stylus_screen_pos;
                                        if stylus_ui_capture {
                                            stylus_ui_capture = false;
                                            egui_winit_state.egui_input_mut().events.push(
                                                egui::Event::PointerButton {
                                                    pos: egui::pos2(sx / scale, sy / scale),
                                                    button: egui::PointerButton::Primary,
                                                    pressed: false,
                                                    modifiers: egui::Modifiers::default(),
                                                },
                                            );
                                            window.request_redraw();
                                            continue;
                                        }

                                        if is_erasing {
                                            is_erasing = false;
                                        }

                                        if is_stylus_drawing {
                                            is_stylus_drawing = false;
                                            is_drawing = false;

                                            if !stroke_already_snapped {
                                                let page_pt = canvas.screen_to_page_clamped(sx, sy);
                                                canvas.extend_stroke(
                                                    page_pt.x,
                                                    page_pt.y,
                                                    current_pressure,
                                                    now,
                                                );
                                            }

                                            let snapped_shape =
                                                stroke_snapped_shape.take().or_else(|| {
                                                    if ui_state.active_tool == ActiveTool::Shape {
                                                        geometry::detect_shape(&canvas.live_samples)
                                                    } else {
                                                        None
                                                    }
                                                });

                                            commit_stroke_to_notebook(
                                                &mut canvas,
                                                &mut notebook,
                                                &mut manifest,
                                                &mut autosave,
                                                &ui_state,
                                                &window,
                                                snapped_shape,
                                            );
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
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

                // Von egui angefordertes, verzögertes Neuzeichnen (Tooltips, Animationen)
                if let Some(at) = egui_repaint_at {
                    if Instant::now() >= at {
                        egui_repaint_at = None;
                        window.request_redraw();
                    }
                }

                // Bei anstehenden Änderungen, aktivem Toast, Stylus in Proximity oder aktivem Zeichnen regelmäßig aufwachen
                if new_status == AutosaveStatus::Pending
                    || new_status == AutosaveStatus::Saving
                    || ui_state.export_message.is_some()
                    || (is_drawing && !stroke_already_snapped)
                    || is_stylus_drawing
                    || is_stylus_in_proximity
                {
                    target.set_control_flow(ControlFlow::wait_duration(Duration::from_millis(8)));
                } else if let Some(at) = egui_repaint_at {
                    target.set_control_flow(ControlFlow::WaitUntil(at));
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
                    || ui::is_screen_point_in_ui(
                        &egui_ctx,
                        &ui_state,
                        cursor_screen_pos,
                        window.scale_factor() as f32,
                    );

                match event {
                    WindowEvent::CloseRequested => {
                        println!("[Shutdown] Sichere alle ungespeicherten Daten auf Festplatte...");
                        match autosave.shutdown() {
                            Ok(()) => {
                                target.exit();
                            }
                            Err(err) => {
                                eprintln!("[Shutdown-Fehler] Konnte nicht geordnet beenden: {err}");
                                ui_state.shutdown_error = Some(err);
                                window.request_redraw();
                            }
                        }
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
                        active_touches.clear();
                        ui_touch_ids.clear();
                        stylus_ui_capture = false;
                        is_pinch_gesturing = false;
                        pinch_prev_distance = None;
                        pinch_prev_center = None;
                        is_touch_drawing = false;
                        is_touch_panning = false;
                        is_stylus_drawing = false;
                        is_stylus_in_proximity = false;
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
                                let mut changed = false;
                                if ui_state.shutdown_error.is_some() {
                                    ui_state.shutdown_error = None;
                                    changed = true;
                                } else if ui_state.is_pen_menu_open
                                    || ui_state.is_highlighter_menu_open
                                    || ui_state.show_page_jump_popup
                                    || ui_state.is_document_menu_open
                                    || ui_state.is_update_dialog_open
                                    || ui_state.show_updater_config
                                {
                                    ui_state.is_pen_menu_open = false;
                                    ui_state.is_highlighter_menu_open = false;
                                    ui_state.show_page_jump_popup = false;
                                    ui_state.is_document_menu_open = false;
                                    ui_state.is_update_dialog_open = false;
                                    ui_state.show_updater_config = false;
                                    changed = true;
                                }
 else if !selection_state.is_empty() {
                                    selection_state.clear();
                                    changed = true;
                                }
                                if changed {
                                    window.request_redraw();
                                }
                            }

                            // Undo: Strg + Z
                            Key::Character(c) if c.eq_ignore_ascii_case("z") && ctrl && !shift => {
                                if notebook.current_page_mut().undo() {
                                    selection_state.clear();
                                    canvas.sync_page(notebook.current_page_mut());
                                    autosave.mark_dirty(&notebook, &mut manifest);
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
                                    autosave.mark_dirty(&notebook, &mut manifest);
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
                                    autosave.mark_dirty(&notebook, &mut manifest);
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
                                        autosave.mark_dirty(&notebook, &mut manifest);
                                        ui_state.set_export_message(format!("{} Objekt(e) gelöscht", removed.len()));
                                        window.request_redraw();
                                    }
                                }
                            }

                            // Jetzt speichern (Disk Flush): Strg + S
                            Key::Character(c) if c.eq_ignore_ascii_case("s") && ctrl => {
                                match autosave.flush() {
                                    Ok(()) => {
                                        ui_state.set_export_message("Gespeichert");
                                    }
                                    Err(err) => {
                                        ui_state.set_export_message(format!(
                                            "Speicherfehler: {}",
                                            err.messages.join(", ")
                                        ));
                                    }
                                }
                                update_window_title(
                                    &window,
                                    &notebook,
                                    &manifest,
                                    ui_state.active_tool,
                                    autosave.status(),
                                );
                                window.request_redraw();
                            }

                            // PDF-Export: Strg + E
                            Key::Character(c) if c.eq_ignore_ascii_case("e") && ctrl => {
                                start_pdf_export(&notebook, &storage_dir, &manifest.title, export_tx.clone(), window.clone());
                                ui_state.set_export_message("PDF-Export läuft…");
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
                                        autosave.mark_dirty(&notebook, &mut manifest);
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
                                        autosave.mark_dirty(&notebook, &mut manifest);
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
                                        autosave.mark_dirty(&notebook, &mut manifest);
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
                                autosave.mark_dirty(&notebook, &mut manifest);
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
                                manifest.sync_from_notebook(&notebook);
                                autosave.mark_manifest_dirty(&manifest);
                                autosave.mark_dirty(&notebook, &mut manifest);
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
                                manifest.sync_from_notebook(&notebook);
                                autosave.mark_manifest_dirty(&manifest);
                                autosave.mark_dirty(&notebook, &mut manifest);
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
                                manifest.sync_from_notebook(&notebook);
                                autosave.mark_manifest_dirty(&manifest);
                                autosave.mark_dirty(&notebook, &mut manifest);
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
                                manifest.sync_from_notebook(&notebook);
                                autosave.mark_manifest_dirty(&manifest);
                                autosave.mark_dirty(&notebook, &mut manifest);
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
                                manifest.sync_from_notebook(&notebook);
                                autosave.mark_manifest_dirty(&manifest);
                                autosave.mark_dirty(&notebook, &mut manifest);
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

                            // Beenden: Strg + Q
                            Key::Character(c) if c.eq_ignore_ascii_case("q") && ctrl => {
                                println!("[Shutdown] Sichere alle ungespeicherten Daten auf Festplatte...");
                                match autosave.shutdown() {
                                    Ok(()) => {
                                        target.exit();
                                    }
                                    Err(err) => {
                                        eprintln!("[Shutdown-Fehler] Konnte nicht geordnet beenden: {err}");
                                        ui_state.shutdown_error = Some(err);
                                        window.request_redraw();
                                    }
                                }
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
                                // Palm-Rejection: Keine Mausevents verarbeiten, wenn Stylus aktiv zeichnet
                                if is_stylus_drawing {
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
                                            autosave.mark_dirty(&notebook, &mut manifest);
                                            update_window_title(&window, &notebook, &manifest, ui_state.active_tool, autosave.status());
                                            window.request_redraw();
                                        }
                                    }
                                } else if button == MouseButton::Left {
                                    let (sx, sy) = cursor_screen_pos;

                                    // 2. Objektauswahl & Transformation im Select-Modus
                                    if ui_state.active_tool == ActiveTool::Select {
                                        let screen_vec = Vec2::new(sx, sy);
                                        let hit = selection_state.hit_test_handles(notebook.current_page(), &canvas, screen_vec, egui_ctx.pixels_per_point());
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
                                                    autosave.mark_dirty(&notebook, &mut manifest);
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
                                                autosave.mark_dirty(&notebook, &mut manifest);
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
                                    let snapped_shape = stroke_snapped_shape.take().or_else(|| {
                                        if ui_state.active_tool == ActiveTool::Shape {
                                            geometry::detect_shape(&canvas.live_samples)
                                        } else {
                                            None
                                        }
                                    });

                                    commit_stroke_to_notebook(
                                        &mut canvas,
                                        &mut notebook,
                                        &mut manifest,
                                        &mut autosave,
                                        &ui_state,
                                        &window,
                                        snapped_shape,
                                    );
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

                        // Wenn der Stylus aktiv zeichnet, keine Mauscursor-Bewegungen verarbeiten
                        if !is_stylus_drawing {
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
                                        autosave.mark_dirty(&notebook, &mut manifest);
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

                        // Hardware-Palm-Rejection: Wenn der Stylus in Proximity ist oder zeichnet, Touch ignorieren
                        if is_stylus_drawing || is_stylus_in_proximity {
                            return;
                        }

                        // Berührungen der UI (egui-winit hat sie bereits an egui weitergegeben)
                        if ui_touch_ids.contains(&touch.id) {
                            if matches!(touch.phase, TouchPhase::Ended | TouchPhase::Cancelled) {
                                ui_touch_ids.remove(&touch.id);
                            }
                            window.request_redraw();
                            return;
                        }

                        match touch.phase {
                            TouchPhase::Started => {
                                // Event-Isolation für Touch-Eingaben
                                let is_touch_in_ui = ui::is_screen_point_in_ui(
                                    &egui_ctx,
                                    &ui_state,
                                    (tx, ty),
                                    window.scale_factor() as f32,
                                );
                                if is_touch_in_ui {
                                    ui_touch_ids.insert(touch.id);
                                    window.request_redraw();
                                    return;
                                }

                                active_touches.insert(touch.id, (tx, ty));

                                if active_touches.len() >= 2 {
                                    // Multi-Touch Geste: Sobald 2+ Finger aufliegen, Inking sofort abbrechen und zoomen/pannen
                                    if is_touch_drawing {
                                        is_touch_drawing = false;
                                        is_drawing = false;
                                        canvas.cancel_stroke();
                                        window.request_redraw();
                                    }
                                    is_touch_panning = false;
                                    is_pinch_gesturing = true;

                                    let pts: Vec<(f32, f32)> = active_touches.values().copied().take(2).collect();
                                    let p1 = pts[0];
                                    let p2 = pts[1];
                                    let center = ((p1.0 + p2.0) * 0.5, (p1.1 + p2.1) * 0.5);
                                    let dist = ((p2.0 - p1.0).powi(2) + (p2.1 - p1.1).powi(2)).sqrt();
                                    pinch_prev_center = Some(center);
                                    pinch_prev_distance = Some(dist);
                                } else if active_touches.len() == 1 && !is_pinch_gesturing {
                                    cursor_screen_pos = (tx, ty);
                                    current_pressure = pressure;

                                    if !ui_state.touch_draw_enabled {
                                        // Standard: 1 Finger scrollt / verschiebt den Canvas (Pan)
                                        is_touch_panning = true;
                                        touch_pan_last_pos = (tx, ty);
                                    } else {
                                        // Modus Finger-Zeichnen aktiv
                                        if is_eraser_action {
                                            is_erasing = true;
                                            if let Some(page_pt) = canvas.screen_to_page(tx, ty) {
                                                let page = notebook.current_page_mut();
                                                let removed = page.erase_at(page_pt, eraser_radius, stroke_max_radius);
                                                if !removed.is_empty() {
                                                    canvas.sync_page(page);
                                                    autosave.mark_dirty(&notebook, &mut manifest);
                                                    update_window_title(&window, &notebook, &manifest, ui_state.active_tool, autosave.status());
                                                    window.request_redraw();
                                                }
                                            }
                                        } else if ui_state.active_tool == ActiveTool::Select {
                                            let screen_vec = Vec2::new(tx, ty);
                                            let hit = selection_state.hit_test_handles(notebook.current_page(), &canvas, screen_vec, egui_ctx.pixels_per_point());
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
                                                is_touch_drawing = true;
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
                                }
                            }
                            TouchPhase::Moved => {
                                active_touches.insert(touch.id, (tx, ty));

                                if active_touches.len() >= 2 {
                                    let pts: Vec<(f32, f32)> = active_touches.values().copied().take(2).collect();
                                    let p1 = pts[0];
                                    let p2 = pts[1];
                                    let curr_center = ((p1.0 + p2.0) * 0.5, (p1.1 + p2.1) * 0.5);
                                    let curr_dist = ((p2.0 - p1.0).powi(2) + (p2.1 - p1.1).powi(2)).sqrt();

                                    if let (Some(prev_dist), Some(prev_center)) = (pinch_prev_distance, pinch_prev_center) {
                                        let dx = curr_center.0 - prev_center.0;
                                        let dy = curr_center.1 - prev_center.1;
                                        if dx.abs() > 0.05 || dy.abs() > 0.05 {
                                            canvas.pan(dx, dy);
                                        }
                                        if prev_dist > 5.0 && curr_dist > 5.0 {
                                            let zoom_factor = curr_dist / prev_dist;
                                            if (zoom_factor - 1.0).abs() > 0.001 {
                                                canvas.zoom_at(curr_center.0, curr_center.1, zoom_factor);
                                                ui_state.current_zoom = canvas.camera.zoom;
                                            }
                                        }
                                        window.request_redraw();
                                    }
                                    pinch_prev_center = Some(curr_center);
                                    pinch_prev_distance = Some(curr_dist);
                                } else if active_touches.len() == 1 && !is_pinch_gesturing {
                                    if is_touch_panning {
                                        let dx = tx - touch_pan_last_pos.0;
                                        let dy = ty - touch_pan_last_pos.1;
                                        touch_pan_last_pos = (tx, ty);
                                        canvas.pan(dx, dy);
                                        window.request_redraw();
                                    } else if is_touch_drawing {
                                        current_pressure = pressure;
                                        cursor_screen_pos = (tx, ty);

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
                                    } else if is_erasing {
                                        if let Some(page_pt) = canvas.screen_to_page(tx, ty) {
                                            let page = notebook.current_page_mut();
                                            let removed = page.erase_at(page_pt, eraser_radius, stroke_max_radius);
                                            if !removed.is_empty() {
                                                canvas.sync_page(page);
                                                autosave.mark_dirty(&notebook, &mut manifest);
                                                update_window_title(&window, &notebook, &manifest, ui_state.active_tool, autosave.status());
                                                window.request_redraw();
                                            }
                                        }
                                    } else if ui_state.active_tool == ActiveTool::Select && selection_state.drag_mode != DragMode::None {
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
                                    }
                                }
                            }
                            TouchPhase::Ended | TouchPhase::Cancelled => {
                                active_touches.remove(&touch.id);

                                if active_touches.is_empty() {
                                    is_pinch_gesturing = false;
                                    pinch_prev_distance = None;
                                    pinch_prev_center = None;
                                    is_touch_panning = false;

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
                                                    autosave.mark_dirty(&notebook, &mut manifest);
                                                }
                                                window.request_redraw();
                                            }
                                            DragMode::None => {}
                                        }
                                    }

                                    if is_touch_drawing {
                                        is_touch_drawing = false;
                                        is_drawing = false;

                                        if !stroke_already_snapped {
                                            let page_pt = canvas.screen_to_page_clamped(tx, ty);
                                            canvas.extend_stroke(page_pt.x, page_pt.y, pressure, now);
                                        }

                                        let snapped_shape = stroke_snapped_shape.take().or_else(|| {
                                            if ui_state.active_tool == ActiveTool::Shape {
                                                geometry::detect_shape(&canvas.live_samples)
                                            } else {
                                                None
                                            }
                                        });

                                        commit_stroke_to_notebook(
                                            &mut canvas,
                                            &mut notebook,
                                            &mut manifest,
                                            &mut autosave,
                                            &ui_state,
                                            &window,
                                            snapped_shape,
                                        );
                                    }
                                } else if active_touches.len() == 1 {
                                    pinch_prev_distance = None;
                                    pinch_prev_center = None;
                                    let rem = *active_touches.values().next().unwrap();
                                    touch_pan_last_pos = rem;
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
                        updater.update();
                        let actions = ui::render_ui(&egui_ctx, &mut ui_state, &notebook, autosave.status(), &updater);

                        // Seite neben der Werkzeugleiste zentrieren (Leistenbreite in physischen Pixeln)
                        let reserved = ui_state.sidebar_reserved_width() * window.scale_factor() as f32;
                        let (inset_left, inset_right) = match ui_state.sidebar_side {
                            SidebarSide::Left => (reserved, 0.0),
                            SidebarSide::Right => (0.0, reserved),
                        };
                        if canvas.set_ui_insets(inset_left, inset_right) {
                            window.request_redraw();
                        }

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
                                            autosave.mark_dirty(&notebook, &mut manifest);
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
                                            autosave.mark_dirty(&notebook, &mut manifest);
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
                                        autosave.mark_dirty(&notebook, &mut manifest);
                                        ui_state.set_export_message(format!("{} Objekt(e) eingefügt", new_ids.len()));
                                        window.request_redraw();
                                    }
                                }
                                UiAction::RotateSelected => {
                                    if !selection_state.is_empty()
                                        && selection_state.rotate_selected_90_deg(notebook.current_page_mut()) {
                                            canvas.sync_page(notebook.current_page_mut());
                                            autosave.mark_dirty(&notebook, &mut manifest);
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
                                        autosave.mark_dirty(&notebook, &mut manifest);
                                        update_window_title(&window, &notebook, &manifest, ui_state.active_tool, autosave.status());
                                        window.request_redraw();
                                    }
                                }
                                UiAction::Redo => {
                                    if notebook.current_page_mut().redo() {
                                        selection_state.clear();
                                        canvas.sync_page(notebook.current_page_mut());
                                        autosave.mark_dirty(&notebook, &mut manifest);
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
                                    manifest.sync_from_notebook(&notebook);
                                    autosave.mark_manifest_dirty(&manifest);
                                    autosave.mark_dirty(&notebook, &mut manifest);
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
                                    manifest.pages = notebook
                                        .pages
                                        .iter()
                                        .map(|p| storage::PageMeta {
                                            id: p.id,
                                            background: Some(p.background),
                                        })
                                        .collect();
                                    manifest.sync_from_notebook(&notebook);
                                    autosave.mark_manifest_dirty(&manifest);
                                    autosave.mark_dirty(&notebook, &mut manifest);
                                    update_window_title(&window, &notebook, &manifest, ui_state.active_tool, autosave.status());
                                    window.request_redraw();
                                }
                                UiAction::AddPage => {
                                    selection_state.clear();
                                    let bg = notebook.current_page().background;
                                    notebook.add_page(bg);
                                    manifest.current_page_index = notebook.current_page_index;
                                    manifest.next_page_id = notebook.next_page_id;
                                    manifest.pages = notebook
                                        .pages
                                        .iter()
                                        .map(|p| storage::PageMeta {
                                            id: p.id,
                                            background: Some(p.background),
                                        })
                                        .collect();
                                    canvas.sync_page(notebook.current_page_mut());
                                    manifest.sync_from_notebook(&notebook);
                                    autosave.mark_manifest_dirty(&manifest);
                                    autosave.mark_dirty(&notebook, &mut manifest);
                                    update_window_title(&window, &notebook, &manifest, ui_state.active_tool, autosave.status());
                                    window.request_redraw();
                                }
                                UiAction::LoadAllPages => {
                                    // Für die Vorschaubilder der Seitenauswahl; Fehler werden wie beim
                                    // Blättern behandelt (Ersatzseite, Warnung beim Öffnen der Seite)
                                    for idx in 0..notebook.pages.len() {
                                        if !notebook.pages[idx].is_loaded {
                                            let _ = storage::ensure_page_loaded(&storage_dir, &mut notebook, idx);
                                        }
                                    }
                                    window.request_redraw();
                                }
                                UiAction::GoToPage(idx) => {
                                    if idx < notebook.pages.len() {
                                        selection_state.clear();
                                        notebook.current_page_index = idx;
                                        let _ = storage::ensure_page_loaded(&storage_dir, &mut notebook, idx);
                                        canvas.sync_page(notebook.current_page_mut());
                                        manifest.current_page_index = idx;
                                        manifest.sync_from_notebook(&notebook);
                                        autosave.mark_manifest_dirty(&manifest);
                                        autosave.mark_dirty(&notebook, &mut manifest);
                                        update_window_title(&window, &notebook, &manifest, ui_state.active_tool, autosave.status());
                                        window.request_redraw();
                                    }
                                }
                                UiAction::SetBackground(bg) => {
                                    let page = notebook.current_page_mut();
                                    page.background = bg;
                                    canvas.sync_page(page);
                                    autosave.mark_dirty(&notebook, &mut manifest);
                                    update_window_title(&window, &notebook, &manifest, ui_state.active_tool, autosave.status());
                                    window.request_redraw();
                                }
                                UiAction::ExportPdf => {
                                    start_pdf_export(&notebook, &storage_dir, &manifest.title, export_tx.clone(), window.clone());
                                    ui_state.set_export_message("PDF-Export läuft…");
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
                                                let _ = autosave.flush();
                                                match storage::NotebookLock::acquire(&new_notebook_dir) {
                                                    Ok(lock) => {
                                                        _notebook_lock = Some(lock);
                                                        autosave.set_read_only(false);
                                                    }
                                                    Err(storage::StorageError::AlreadyLocked(msg)) => {
                                                        eprintln!("[Storage-Lock] {msg}");
                                                        _notebook_lock = None;
                                                        autosave.set_read_only(true);
                                                        ui_state.set_export_message("⚠ In anderer Instanz geöffnet (Schreibschutz)");
                                                    }
                                                    Err(e) => {
                                                        eprintln!("[Storage-Lock] Konnte Lock nicht erwerben: {e}");
                                                        _notebook_lock = None;
                                                        autosave.set_read_only(true);
                                                    }
                                                }
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
                                                if _notebook_lock.is_some() {
                                                    ui_state.set_export_message(format!("Dokument '{}' geöffnet", manifest.title));
                                                }
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
                                    let _ = autosave.flush();
                                    match storage::create_new_notebook(&storage_base_dir, &title) {
                                        Ok((new_nb, new_manifest, new_dir)) => {
                                            _notebook_lock = storage::NotebookLock::acquire(&new_dir).ok();
                                            autosave.set_read_only(_notebook_lock.is_none());
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
                                                manifest.sync_from_notebook(&notebook);
                                                autosave.mark_manifest_dirty(&manifest);
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
                                    let deleted_title = ui_state
                                        .available_notebooks
                                        .iter()
                                        .find(|s| s.id == id)
                                        .map(|s| s.title.clone())
                                        .unwrap_or_else(|| id.clone());

                                    if is_current {
                                        let _ = autosave.flush();
                                        drop(_notebook_lock.take());
                                    }

                                    match storage::delete_notebook(&target_dir, &storage_base_dir) {
                                        Ok(trash_path) => {
                                            ui_state.last_deleted_notebook = Some((trash_path, deleted_title));
                                            ui_state.available_notebooks = storage::list_notebooks(&storage_base_dir);
                                            if is_current {
                                                if let Some(first) = ui_state
                                                    .available_notebooks
                                                    .iter()
                                                    .find(|s| s.status != storage::NotebookStatus::Damaged)
                                                    .cloned()
                                                {
                                                    let new_notebook_dir = storage_base_dir.join(&first.id);
                                                    match storage::load_notebook(&new_notebook_dir) {
                                                        Ok((loaded_nb, loaded_manifest)) => {
                                                            _notebook_lock = storage::NotebookLock::acquire(&new_notebook_dir).ok();
                                                            autosave.set_read_only(_notebook_lock.is_none());
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
                                                    _notebook_lock = storage::NotebookLock::acquire(&new_dir).ok();
                                                    autosave.set_read_only(_notebook_lock.is_none());
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
                                                ui_state.set_export_message("Dokument in Papierkorb verschoben".to_string());
                                            }
                                            window.request_redraw();
                                        }
                                        Err(err) => {
                                            eprintln!("[Dokument-Fehler] Konnte Dokument '{id}' nicht löschen: {err}");
                                            ui_state.set_export_message(format!("Fehler beim Löschen: {err}"));
                                        }
                                    }
                                }
                                UiAction::UndoDeleteNotebook => {
                                    if let Some((trash_path, _title)) = ui_state.last_deleted_notebook.take() {
                                        match storage::restore_notebook_from_trash(&storage_base_dir, &trash_path) {
                                            Ok(_restored_dir) => {
                                                ui_state.available_notebooks = storage::list_notebooks(&storage_base_dir);
                                                ui_state.set_export_message("Notizbuch wiederhergestellt".to_string());
                                                window.request_redraw();
                                            }
                                            Err(err) => {
                                                eprintln!("[Dokument-Fehler] Konnte Notizbuch nicht wiederherstellen: {err}");
                                                ui_state.set_export_message(format!("Fehler beim Wiederherstellen: {err}"));
                                            }
                                        }
                                    }
                                }
                                UiAction::RestoreNotebookBackup(id) => {
                                    let target_dir = storage_base_dir.join(&id);
                                    match storage::restore_manifest_from_backup(&target_dir) {
                                        Ok(()) => {
                                            ui_state.available_notebooks = storage::list_notebooks(&storage_base_dir);
                                            ui_state.set_export_message("Manifest aus Backup wiederhergestellt".to_string());
                                            window.request_redraw();
                                        }
                                        Err(err) => {
                                            eprintln!("[Dokument-Fehler] Konnte Backup für '{id}' nicht wiederherstellen: {err}");
                                            ui_state.set_export_message(format!("Wiederherstellung fehlgeschlagen: {err}"));
                                        }
                                    }
                                }
                                UiAction::OpenNotebookFolder(id) => {
                                    let target_dir = storage_base_dir.join(&id);
                                    ui_state.set_export_message(format!("Ordner: {}", target_dir.display()));
                                    window.request_redraw();
                                }
                                UiAction::CheckForUpdates => {
                                    updater.check_for_updates();
                                    window.request_redraw();
                                }
                                UiAction::DownloadUpdateAsset(asset) => {
                                    updater.download_asset(asset);
                                    window.request_redraw();
                                }
                                UiAction::InstallOrOpenUpdate(path) => {
                                    if let Err(err) = updater.install_or_open(&path) {
                                        ui_state.set_export_message(format!("Update-Fehler: {err}"));
                                    } else {
                                        ui_state.set_export_message("Update gestartet / geöffnet");
                                    }
                                    window.request_redraw();
                                }
                                UiAction::OpenUrlInBrowser(url) => {
                                    let _ = webbrowser::open(&url);
                                }
                                UiAction::SaveUpdaterConfig { server_url, auth_token } => {
                                    updater.set_server_url(server_url);
                                    updater.set_auth_token(auth_token);
                                    ui_state.set_export_message("Server-Einstellungen gespeichert");
                                    window.request_redraw();
                                }
                                UiAction::UnlockCorruptedPage(page_id) => {
                                    notebook.load_warnings.remove(&page_id);
                                    if let Some(page) = notebook.pages.iter_mut().find(|p| p.id == page_id) {
                                        page.unlock_after_load_error();
                                    }
                                    autosave.mark_dirty(&notebook, &mut manifest);
                                    window.request_redraw();
                                }
                                UiAction::RetryShutdown => {
                                    println!("[Shutdown] Wiederhole Speichern vor Beenden...");
                                    match autosave.shutdown() {
                                        Ok(()) => {
                                            ui_state.shutdown_error = None;
                                            target.exit();
                                        }
                                        Err(err) => {
                                            ui_state.shutdown_error = Some(err);
                                            window.request_redraw();
                                        }
                                    }
                                }
                                UiAction::ExportRecoveryAndExit => {
                                    println!("[Shutdown] Starte Notfall-Export aller ungespeicherten Daten...");
                                    match autosave.export_recovery(Some(&notebook)) {
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
                                            if let Some(ref mut se) = ui_state.shutdown_error {
                                                se.messages.push(format!("Notfall-Export fehlgeschlagen: {err}"));
                                            }
                                            window.request_redraw();
                                        }
                                    }
                                }
                                UiAction::SetSidebarSide(side) => {
                                    ui_settings.sidebar_side = side;
                                    if let Some(path) = ui_settings_path.as_deref() {
                                        if let Err(err) = ui_settings.save_to(path) {
                                            eprintln!("[Einstellungen] Konnte '{}' nicht speichern: {err}", path.display());
                                            ui_state.set_export_message(format!("Einstellung nicht gespeichert: {err}"));
                                        }
                                    }
                                    window.request_redraw();
                                }
                                UiAction::ForceExit => {
                                    println!("[Shutdown] Beenden erzwungen trotz ungespeicherter Änderungen.");
                                    target.exit();
                                }
                            }
                        }

                        // 4. egui Frame beenden & Primitives tessellieren
                        let full_output = egui_ctx.end_frame();
                        egui_winit_state.handle_platform_output(&window, full_output.platform_output);

                        // egui braucht Folge-Frames, z. B. um neu angeordnete Bereiche zu vermessen
                        // (die Werkzeugleiste wird erst im 2.–3. Frame korrekt zentriert)
                        let repaint_delay = full_output
                            .viewport_output
                            .get(&egui::ViewportId::ROOT)
                            .map(|v| v.repaint_delay)
                            .unwrap_or(Duration::MAX);
                        if repaint_delay.is_zero() {
                            egui_repaint_at = None;
                            window.request_redraw();
                        } else if repaint_delay < Duration::from_secs(10) {
                            egui_repaint_at = Some(Instant::now() + repaint_delay);
                        } else {
                            egui_repaint_at = None;
                        }

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
