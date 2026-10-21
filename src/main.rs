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
    raw_window_handle::{HasDisplayHandle, RawDisplayHandle},
    window::{Window, WindowBuilder},
};

use glam::Vec2;
use ink_notes::assets::{AssetStore, ImportedImage};
use ink_notes::autosave::{AutosaveController, AutosaveStatus};
use ink_notes::background::PatternColor;
use ink_notes::canvas::CanvasState;
use ink_notes::document::{Notebook, PageBackground};
use ink_notes::export::{self, export_notebook_to_pdf, ExportStrategy};
use ink_notes::geometry::{self, RecognizedShape};
use ink_notes::i18n::{self, Language};
use ink_notes::image_layer::TextureSource;
use ink_notes::library::Library;
use ink_notes::osk::{self, KeyboardRequester};
use ink_notes::pdf_doc::PdfRenderService;
use ink_notes::platform::{self, DisplayBackend};
use ink_notes::selection::{
    render_selection_overlay, scale_factors, DragMode, SelectionHandleHit, SelectionState,
};
use ink_notes::settings::{
    SidebarSide, StylusAction, StylusButton, UiSettings, MAX_STYLUS_BUTTONS,
};
use ink_notes::storage::{self, NotebookLayout, NotebookManifest};
use ink_notes::t;
use ink_notes::ui::{self, ActiveTool, UiAction, UiState};
use ink_notes::updater::UpdaterController;

/// Kennung der App (Name der .desktop-Datei und des Symbols).
const APP_ID: &str = "eu.pansi.ink-notes";

/// Fenstersymbol aus dem eingebauten App-Symbol, auf 64 × 64 Pixel verkleinert: In voller
/// Größe (256 × 256) überschreitet es die Größe einer einzelnen X11-Anfrage und fehlt dann.
fn app_icon() -> Option<winit::window::Icon> {
    const SIZE: u32 = 64;
    let image = image::load_from_memory(include_bytes!("../assets/ink-notes.png")).ok()?;
    let small = image::imageops::resize(
        &image.into_rgba8(),
        SIZE,
        SIZE,
        image::imageops::FilterType::Triangle,
    );
    winit::window::Icon::from_rgba(small.into_raw(), SIZE, SIZE).ok()
}

/// Platz schaffen: Druck auf das Blatt (physische Pixel) setzt die Ansatzhöhe auf der
/// aktuellen Seite. `false` neben dem Blatt oder auf einer PDF-Seite.
fn space_press(
    screen: (f32, f32),
    canvas: &CanvasState,
    notebook: &Notebook,
    ui_state: &mut UiState,
) -> bool {
    let Some(page_pt) = canvas.screen_to_page(screen.0, screen.1) else {
        return false;
    };
    let index = notebook.current_page_index;
    if notebook.space_run(index).is_none() {
        ui_state.set_export_message(
            t!("Auf Seiten mit importiertem PDF lässt sich kein Platz schaffen").to_string(),
        );
        return false;
    }
    ui_state.space_drag = Some(ui::SpaceDrag {
        page_index: index,
        y: page_pt.y,
        dy: 0.0,
        up_limit: notebook.space_up_limit(index, page_pt.y),
    });
    true
}

/// Platz schaffen: Verschiebung aus dem senkrechten Abstand zur Ansatzhöhe.
fn space_move(screen: (f32, f32), canvas: &CanvasState, ui_state: &mut UiState) {
    if let Some(drag) = &mut ui_state.space_drag {
        let start = canvas.page_to_screen(Vec2::new(0.0, drag.y));
        let below = canvas.page_to_screen(Vec2::new(0.0, drag.y + 1000.0));
        let px_per_unit = (below[1] - start[1]) / 1000.0;
        if px_per_unit > 0.0 {
            drag.dy = (screen.1 - start[1]) / px_per_unit;
        }
    }
}

/// Platz schaffen beim Loslassen ausführen (alle beteiligten Seiten werden dafür geladen).
fn space_release(
    canvas: &mut CanvasState,
    notebook: &mut Notebook,
    manifest: &mut NotebookManifest,
    autosave: &mut AutosaveController,
    storage_dir: &Path,
    ui_state: &mut UiState,
) {
    let Some(drag) = ui_state.space_drag.take() else {
        return;
    };
    if let Some(run) = notebook.space_run(drag.page_index) {
        for index in run {
            let _ = storage::ensure_page_loaded(storage_dir, notebook, index);
        }
    }
    let dy = drag.clamped_dy();
    if let Some(changed) = notebook.make_space(drag.page_index, drag.y, dy) {
        persist_pages(&changed, notebook, manifest, autosave);
        canvas.sync_page(notebook.current_page_mut());
        let mm = (dy / ink_notes::document::UNITS_PER_MM).abs().round();
        ui_state.set_export_message(if dy > 0.0 {
            t!("{mm} mm Platz geschaffen (Rückgängig mit Strg+Z)", mm = mm)
        } else {
            t!("{mm} mm Platz geschlossen (Rückgängig mit Strg+Z)", mm = mm)
        });
    }
}

/// Merkt geänderte Seiten (Kennungen) und das Manifest zum Speichern vor.
fn persist_pages(
    ids: &[u64],
    notebook: &Notebook,
    manifest: &mut NotebookManifest,
    autosave: &mut AutosaveController,
) {
    manifest.sync_from_notebook(notebook);
    for page in notebook.pages.iter().filter(|p| ids.contains(&p.id)) {
        autosave.mark_page_dirty(page, manifest);
    }
    autosave.mark_manifest_dirty(manifest);
}

/// Wählt `tool` und stellt Farbe und Stärke des Live-Strichs passend ein.
fn activate_tool(tool: ActiveTool, ui_state: &mut UiState, canvas: &mut CanvasState) {
    ui_state.active_tool = tool;
    match tool {
        ActiveTool::Pen | ActiveTool::Shape => {
            canvas.set_stroke_color(ui_state.pen_color);
            canvas.set_stroke_width_mm(ui_state.pen_stroke_width_mm);
        }
        ActiveTool::Highlighter => {
            canvas.set_stroke_color(ui_state.highlighter_color);
            canvas.set_stroke_width_mm(ui_state.highlighter_stroke_width_mm);
        }
        ActiveTool::Eraser | ActiveTool::Select | ActiveTool::Text | ActiveTool::Space => {}
    }
}

/// Kennung einer Stifttaste, die über Programmstarts hinweg gleich bleibt (die Tablet-
/// Schnittstelle liefert nur eine undurchsichtige, aber vergleichbare Kennung).
fn stylus_button_key(id: &octotablet::tool::ButtonID) -> u64 {
    use std::hash::{Hash, Hasher};
    // DefaultHasher::new() verwendet feste Schlüssel: gleiche Taste, gleicher Wert
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    id.hash(&mut hasher);
    hasher.finish()
}

/// Liest Notizbücher und Ordner für den Dokumenten-Manager neu ein.
fn refresh_notebooks(base_dir: &Path, ui_state: &mut UiState) {
    ui_state.available_notebooks = storage::list_notebooks(base_dir);
    ui_state.library = Library::load(base_dir);
}

/// Ändert die Ordner (`library.json`) und liest sie neu ein; Fehler erscheinen als Meldung.
/// Gibt bei Erfolg das Ergebnis von `change` zurück.
fn change_library<T>(
    base_dir: &Path,
    ui_state: &mut UiState,
    change: impl FnOnce(&mut Library) -> Result<T, String>,
) -> Option<T> {
    let mut library = Library::load(base_dir);
    let result = change(&mut library).and_then(|value| {
        library
            .save(base_dir)
            .map(|()| value)
            .map_err(|e| t!("Ordner nicht gespeichert: {e}", e = e))
    });
    ui_state.library = Library::load(base_dir);
    match result {
        Ok(value) => Some(value),
        Err(message) => {
            ui_state.set_export_message(message);
            None
        }
    }
}

/// Aktualisiert den Fenstertitel als minimales On-Screen-Display (OSD) inklusive Autosave-Status.
/// Speichert die UI-Einstellungen; ein Fehler wird gemeldet, bricht aber nichts ab.
fn save_ui_settings(settings: &UiSettings, path: Option<&std::path::Path>, ui_state: &mut UiState) {
    if let Some(path) = path {
        if let Err(err) = settings.save_to(path) {
            eprintln!(
                "[Einstellungen] Konnte '{}' nicht speichern: {err}",
                path.display()
            );
            ui_state.set_export_message(t!("Einstellung nicht gespeichert: {err}", err = err));
        }
    }
}

fn update_window_title(
    window: &Window,
    notebook: &Notebook,
    manifest: &NotebookManifest,
    tool: ActiveTool,
    status: AutosaveStatus,
) {
    let page = notebook.current_page();
    let bg_name = match page.background {
        PageBackground::Blank => t!("Blanko"),
        PageBackground::Grid5mm => t!("Kariert (5mm)"),
        PageBackground::Lined => t!("Liniert (7.5mm)"),
        PageBackground::DotGrid => t!("Punktraster (5mm)"),
    };

    let page_title = page
        .title
        .as_deref()
        .map(|t| format!(" – {t}"))
        .unwrap_or_default();
    let title = t!(
        "ink-notes | [{notebook}] {page}{page_title} | [{status}] | {tool} | Hintergrund: {background} | [Ctrl+Z] Undo [Ctrl+Y] Redo [E] Radierer [Ctrl+E/S] PDF-Export",
        notebook = manifest.title,
        page = notebook.osd_text(),
        page_title = page_title,
        status = status.display_text(),
        tool = tool.name(),
        background = bg_name
    );
    window.set_title(&title);
}

/// Exportiert das Notizbuch im Hintergrund als PDF. Noch nicht geladene Seiten werden vorher
/// von der Festplatte nachgeladen; das Ergebnis geht über `result_tx` an den Event-Loop zurück.
fn start_pdf_export(
    notebook: &Notebook,
    storage_dir: &Path,
    title: &str,
    pattern: PatternColor,
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
                let assets = AssetStore::new(&storage_dir);
                export_notebook_to_pdf(
                    &notebook_clone,
                    &export_path,
                    ExportStrategy::FilledPolygon,
                    Some(&assets),
                    pattern,
                )
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

/// Halten am Strichende (Form einrasten): So weit darf der Stift dabei zittern, in physischen
/// Pixeln. Echte Stifte zittern im Stillstand um einige Pixel; mit 3 px begann die Wartezeit
/// ständig neu, und Halten rastete nur mit der Maus ein (Issue #27).
fn hold_tolerance(window: &Window) -> f32 {
    6.0 * window.scale_factor() as f32
}

/// Achsenkreuz aus der eingerasteten Geraden `shape` und dem vorigen Strich der aktuellen
/// Seite, falls sich beide kreuzen (Issue #20); mit der Id des vorigen Strichs
fn axis_cross_with_previous(
    notebook: &Notebook,
    shape: &RecognizedShape,
) -> Option<(u64, RecognizedShape)> {
    let RecognizedShape::Line { start, end } = shape else {
        return None;
    };
    let previous = notebook.current_page().strokes.last()?;
    let line = geometry::stroke_as_line(previous)?;
    Some((
        previous.id,
        geometry::axis_cross_from_lines(line, (*start, *end))?,
    ))
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
    // Zweite Gerade über Kreuz mit dem vorigen Strich: beide werden zum Achsenkreuz; der
    // vorige Strich wird ersetzt, ein Rückgängig stellt ihn wieder her
    let stroke_width = ui_state.current_stroke_width_mm() * ink_notes::document::UNITS_PER_MM;
    if let Some(shape) = snapped_shape
        .as_ref()
        .filter(|_| ui_state.active_tool != ActiveTool::Highlighter)
    {
        if let Some((previous_id, cross)) = axis_cross_with_previous(notebook, shape) {
            canvas.cancel_stroke();
            let stroke = geometry::shape_to_stroke(
                cross,
                previous_id,
                canvas.current_ink_color,
                stroke_width,
            );
            let page = notebook.current_page_mut();
            page.replace_stroke(previous_id, stroke);
            canvas.sync_page(page);
            autosave.mark_dirty(notebook, manifest);
            window.request_redraw();
            return;
        }
    }

    let new_id = notebook.next_stroke_id();
    let stroke_opt = if let Some(shape) = snapped_shape {
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

    if let Some(mut stroke) = stroke_opt {
        stroke.tool = ui_state.active_tool.stroke_tool(stroke.shape.is_some());
        let stroke_bottom = stroke.aabb.max.y;
        let page = notebook.current_page_mut();
        page.add_stroke(stroke);
        canvas.sync_page(page);
        manifest.next_stroke_id = notebook.next_stroke_id;
        autosave.mark_dirty(notebook, manifest);
        // Durchgehende Ansicht: unten auf der letzten Seite geht es auf einer neuen weiter
        if ui_state.continuous_scroll && notebook.append_page_if_writing_at_end(stroke_bottom) {
            manifest.sync_from_notebook(notebook);
            if let Some(new_page) = notebook.pages.last() {
                autosave.mark_page_dirty(new_page, manifest);
            }
        }
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

/// Macht die Seite `index` zur aktuellen: lädt sie bei Bedarf und aktualisiert GPU-Puffer,
/// Manifest und Fenstertitel. Mit `keep_view` (durchgehende Ansicht) bleibt der Bildausschnitt
/// samt Lineal stehen, sonst erscheint die neue Seite an der Stelle der alten.
#[allow(clippy::too_many_arguments)]
fn switch_to_page(
    index: usize,
    keep_view: bool,
    canvas: &mut CanvasState,
    notebook: &mut Notebook,
    manifest: &mut NotebookManifest,
    autosave: &mut AutosaveController,
    storage_dir: &Path,
    selection_state: &mut SelectionState,
    ui_state: &mut UiState,
    window: &Window,
) {
    let current = notebook.current_page_index;
    if index >= notebook.pages.len() || index == current {
        return;
    }
    if keep_view {
        let origin = notebook.page_stack().origins[index];
        canvas.shift_reference_page(origin);
        if let Some(ruler) = &mut ui_state.ruler {
            ruler.center -= origin;
        }
        canvas.ruler = ui_state.ruler;
    }
    selection_state.clear();
    notebook.current_page_index = index;
    let _ = storage::ensure_page_loaded(storage_dir, notebook, index);
    canvas.sync_page(notebook.current_page_mut());
    manifest.sync_from_notebook(notebook);
    autosave.mark_manifest_dirty(manifest);
    update_window_title(
        window,
        notebook,
        manifest,
        ui_state.active_tool,
        autosave.status(),
    );
    window.request_redraw();
}

/// Durchgehende Ansicht: Index der Seite unter dem Bildschirmpunkt, falls dort eine andere
/// als die aktuelle Seite liegt und kein Griff der Auswahl getroffen wird. Zeichnen,
/// Radieren und Auswählen wechseln dann zuerst auf diese Seite.
fn page_to_focus(
    canvas: &CanvasState,
    notebook: &Notebook,
    ui_state: &UiState,
    selection_state: &SelectionState,
    pixels_per_point: f32,
    (sx, sy): (f32, f32),
) -> Option<usize> {
    if !ui_state.continuous_scroll {
        return None;
    }
    let index = canvas
        .page_at_screen(&notebook.page_stack(), sx, sy)
        .filter(|&i| i != notebook.current_page_index)?;
    let grabs_selection = ui_state.active_tool == ActiveTool::Select
        && !selection_state.is_empty()
        && !matches!(
            selection_state.hit_test_handles(
                notebook.current_page(),
                canvas,
                Vec2::new(sx, sy),
                pixels_per_point
            ),
            SelectionHandleHit::None
        );
    (!grabs_selection).then_some(index)
}

/// Zwischenablage: interne Objekte (Strg+C in ink-notes), Bilder aus der
/// Systemzwischenablage (Screenshots, aus anderen Programmen kopierte Bilder) und Text.
struct ClipboardBridge {
    system: Option<arboard::Clipboard>,
    /// Prüfsumme des Systembildes zum Zeitpunkt des letzten internen Kopierens: Strg+V fügt
    /// das Systembild nur ein, wenn es danach kopiert wurde (die jüngste Kopie gewinnt)
    image_hash_at_copy: Option<[u8; 32]>,
    /// Text der Systemzwischenablage zum Zeitpunkt des letzten internen Kopierens (wie oben)
    text_at_copy: Option<String>,
    /// Notizbuch, aus dem kopiert wurde (für Bilder beim Einfügen in ein anderes Notizbuch)
    source_dir: Option<PathBuf>,
}

impl ClipboardBridge {
    fn new() -> Self {
        Self {
            system: None,
            image_hash_at_copy: None,
            text_at_copy: None,
            source_dir: None,
        }
    }

    /// Bild in der Systemzwischenablage samt Prüfsumme.
    fn system_image(&mut self) -> Option<(arboard::ImageData<'static>, [u8; 32])> {
        if self.system.is_none() {
            self.system = arboard::Clipboard::new()
                .map_err(|e| eprintln!("[Zwischenablage] Nicht verfügbar: {e}"))
                .ok();
        }
        let image = self.system.as_mut()?.get_image().ok()?;
        use sha2::Digest;
        let mut hasher = sha2::Sha256::new();
        hasher.update((image.width as u64).to_le_bytes());
        hasher.update((image.height as u64).to_le_bytes());
        hasher.update(&image.bytes);
        let hash: [u8; 32] = hasher.finalize().into();
        Some((image.to_owned_img(), hash))
    }

    /// Text in der Systemzwischenablage (nicht nur Leerraum).
    fn system_text(&mut self) -> Option<String> {
        if self.system.is_none() {
            self.system = arboard::Clipboard::new().ok();
        }
        let text = self.system.as_mut()?.get_text().ok()?;
        (!text.trim().is_empty()).then_some(text)
    }

    /// Merkt sich beim internen Kopieren das aktuelle Systembild, den Text und das
    /// Quell-Notizbuch.
    fn note_internal_copy(&mut self, storage_dir: &Path) {
        self.image_hash_at_copy = self.system_image().map(|(_, hash)| hash);
        self.text_at_copy = self.system_text();
        self.source_dir = Some(storage_dir.to_path_buf());
    }

    /// Internes Kopieren: die Auswahl zusätzlich als Bild in die Systemzwischenablage legen,
    /// damit sie sich in Chat, Mail oder Office einfügen lässt. In ink-notes selbst fügt
    /// Strg+V weiterhin die Objekte (verlustfrei) ein.
    fn copy_selection(
        &mut self,
        page: &ink_notes::document::Page,
        ids: &[u64],
        storage_dir: &Path,
    ) -> bool {
        let assets = AssetStore::new(storage_dir);
        let image = match ink_notes::page_export::selection_image(page, ids, Some(&assets)) {
            Ok(Some(image)) => image,
            Ok(None) => return false,
            Err(err) => {
                eprintln!("[Zwischenablage] Auswahl nicht als Bild darstellbar: {err}");
                return false;
            }
        };
        if self.system.is_none() {
            self.system = arboard::Clipboard::new().ok();
        }
        let Some(system) = self.system.as_mut() else {
            return false;
        };
        let data = arboard::ImageData {
            width: image.width as usize,
            height: image.height as usize,
            bytes: std::borrow::Cow::Owned(image.pixels),
        };
        match system.set_image(data) {
            Ok(()) => true,
            Err(err) => {
                eprintln!("[Zwischenablage] Bild nicht gesetzt: {err}");
                false
            }
        }
    }
}

/// Fügt ein importiertes Bild um `center` (Seitenkoordinaten) auf der aktuellen Seite ein,
/// wählt es aus und schaltet auf das Auswahlwerkzeug, damit es gleich verschoben und
/// skaliert werden kann.
#[allow(clippy::too_many_arguments)]
fn insert_image_on_page(
    imported: ImportedImage,
    center: Vec2,
    canvas: &mut CanvasState,
    notebook: &mut Notebook,
    manifest: &mut NotebookManifest,
    autosave: &mut AutosaveController,
    selection_state: &mut SelectionState,
    ui_state: &mut UiState,
) {
    let id = notebook.next_stroke_id();
    let corners = ink_notes::document::place_image(
        imported.width,
        imported.height,
        center,
        notebook.current_page().size,
        notebook.current_page().reference_size(),
    );
    let page = notebook.current_page_mut();
    page.add_stroke(ink_notes::document::Stroke::new_image(
        id,
        imported.asset,
        corners,
    ));
    canvas.sync_page(page);
    manifest.next_stroke_id = notebook.next_stroke_id;
    autosave.mark_dirty(notebook, manifest);
    selection_state.select_single(id);
    ui_state.active_tool = ActiveTool::Select;
    ui_state.set_export_message(t!(
        "Bild eingefügt ({width} × {height} px)",
        width = imported.width,
        height = imported.height
    ));
}

/// Trifft ein Finger auf `screen` die Auswahl (Rahmen, Griffe) oder ein Objekt? Dann wählt bzw.
/// verschiebt er, sonst verschiebt er das Blatt.
fn finger_grabs_selection(
    screen: (f32, f32),
    canvas: &CanvasState,
    page: &ink_notes::document::Page,
    pixels_per_point: f32,
    selection_state: &SelectionState,
) -> bool {
    let on_selection = !selection_state.is_empty()
        && !matches!(
            selection_state.hit_test_handles(
                page,
                canvas,
                Vec2::new(screen.0, screen.1),
                pixels_per_point
            ),
            SelectionHandleHit::None
        );
    on_selection
        || canvas
            .screen_to_page(screen.0, screen.1)
            .is_some_and(|p| page.find_stroke_at(p, 25.0).is_some())
}

/// Auswahlwerkzeug: Druck auf `screen` (physische Pixel) greift einen Griff, die Auswahl
/// oder das Objekt darunter oder beginnt einen Auswahlrahmen. Mit `shift` wird die Auswahl
/// erweitert. `true`, wenn etwas begonnen wurde.
fn selection_press(
    screen: (f32, f32),
    shift: bool,
    canvas: &CanvasState,
    page: &ink_notes::document::Page,
    pixels_per_point: f32,
    selection_state: &mut SelectionState,
) -> bool {
    let (sx, sy) = screen;
    let hit = selection_state.hit_test_handles(page, canvas, Vec2::new(sx, sy), pixels_per_point);
    let page_pt = canvas.screen_to_page(sx, sy);
    let mode = match (hit, page_pt) {
        (SelectionHandleHit::RotateHandle { center_page }, Some(p)) => DragMode::Rotate {
            center_page,
            start_angle: (p.y - center_page.y).atan2(p.x - center_page.x),
            initial_strokes: page.get_strokes(&selection_state.selected_ids),
        },
        (
            SelectionHandleHit::CornerHandle {
                fixed_corner_page,
                start_pos_page,
                initial_box_size,
                ..
            },
            _,
        ) => {
            let initial_strokes = page.get_strokes(&selection_state.selected_ids);
            // Schloss der Auswahl (Bilder: an, Issue #29); Shift sperrt zusätzlich
            let keep_aspect = shift || selection_state.keeps_aspect(page);
            DragMode::Scale {
                fixed_corner_page,
                start_pos_page,
                initial_box_size,
                initial_strokes,
                keep_aspect,
            }
        }
        (SelectionHandleHit::Body, Some(p)) => DragMode::Move {
            start_page: p,
            initial_strokes: page.get_strokes(&selection_state.selected_ids),
        },
        (SelectionHandleHit::None, Some(p)) => match page.find_stroke_at(p, 25.0) {
            Some(stroke) => {
                if shift {
                    selection_state.toggle_select(stroke.id);
                } else if !selection_state.selected_ids.contains(&stroke.id) {
                    selection_state.select_single(stroke.id);
                }
                DragMode::Move {
                    start_page: p,
                    initial_strokes: page.get_strokes(&selection_state.selected_ids),
                }
            }
            None => {
                if !shift {
                    selection_state.clear();
                }
                DragMode::BoxSelect {
                    start_page: p,
                    current_page: p,
                }
            }
        },
        _ => return false,
    };
    selection_state.drag_mode = mode;
    selection_state.pending_drag = None;
    true
}

/// Wendet die zuletzt gemeldete Zugposition der Auswahl an (siehe [`selection_drag`]).
fn flush_selection_drag(
    page: &mut ink_notes::document::Page,
    canvas: &mut CanvasState,
    selection_state: &mut SelectionState,
) {
    if let Some(page_pt) = selection_state.pending_drag.take() {
        selection_drag(page_pt, page, canvas, selection_state);
    }
}

/// Auswahlwerkzeug: Ziehen nach `page_pt` – Rahmen aufziehen oder die Auswahl verschieben,
/// drehen bzw. skalieren (vorläufig, ohne Undo-Schritt). Bei Stiftbewegungen über
/// `pending_drag` und [`flush_selection_drag`] nur einmal je Bild, weil das bei großen
/// Auswahlen teuer ist.
fn selection_drag(
    page_pt: Vec2,
    page: &mut ink_notes::document::Page,
    canvas: &mut CanvasState,
    selection_state: &mut SelectionState,
) {
    match &mut selection_state.drag_mode {
        DragMode::BoxSelect { current_page, .. } => {
            *current_page = page_pt;
            return;
        }
        DragMode::None => return,
        DragMode::Move {
            start_page,
            initial_strokes,
        } => {
            let delta = page_pt - *start_page;
            page.transform_strokes_ephemeral(initial_strokes, |s| s.translate(delta));
        }
        DragMode::Rotate {
            center_page,
            start_angle,
            initial_strokes,
        } => {
            let angle = (page_pt.y - center_page.y).atan2(page_pt.x - center_page.x) - *start_angle;
            let center = *center_page;
            page.transform_strokes_ephemeral(initial_strokes, |s| s.rotate(center, angle));
        }
        DragMode::Scale {
            fixed_corner_page,
            start_pos_page,
            keep_aspect,
            initial_strokes,
            ..
        } => {
            let fixed = *fixed_corner_page;
            let factors = scale_factors(fixed, *start_pos_page, page_pt, *keep_aspect);
            page.transform_strokes_ephemeral(initial_strokes, |s| s.scale(fixed, factors));
        }
    }
    canvas.sync_page(page);
}

/// Auswahlwerkzeug: Loslassen – Rahmenauswahl übernehmen (mit `shift` hinzufügen) bzw.
/// Verschieben, Drehen oder Skalieren als einen Undo-Schritt abschließen. `true`, wenn ein
/// Ziehen lief.
fn selection_release(
    shift: bool,
    notebook: &mut Notebook,
    manifest: &mut NotebookManifest,
    autosave: &mut AutosaveController,
    selection_state: &mut SelectionState,
    canvas: &mut CanvasState,
) -> bool {
    // Die letzte Bewegung gehört noch zum Ziehen
    flush_selection_drag(notebook.current_page_mut(), canvas, selection_state);
    match std::mem::take(&mut selection_state.drag_mode) {
        DragMode::BoxSelect {
            start_page,
            current_page,
        } => {
            let box_min = start_page.min(current_page);
            let box_max = start_page.max(current_page);
            if (box_max - box_min).max_element() > 3.0 {
                let found_ids = notebook
                    .current_page()
                    .find_strokes_in_box(box_min, box_max);
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
        }
        DragMode::Move {
            initial_strokes, ..
        }
        | DragMode::Rotate {
            initial_strokes, ..
        }
        | DragMode::Scale {
            initial_strokes, ..
        } => {
            let final_strokes = notebook
                .current_page()
                .get_strokes(&selection_state.selected_ids);
            if !initial_strokes.is_empty()
                && !final_strokes.is_empty()
                && initial_strokes != final_strokes
            {
                notebook
                    .current_page_mut()
                    .commit_modified_strokes(initial_strokes, final_strokes);
                autosave.mark_dirty(notebook, manifest);
            }
        }
        DragMode::None => return false,
    }
    true
}

/// Fügt `content` als Textfeld (Schrift des Text-Werkzeugs) in der Mitte des sichtbaren
/// Bereichs ein, wählt es aus und schaltet auf das Auswahlwerkzeug zum Verschieben.
#[allow(clippy::too_many_arguments)]
fn insert_text_on_page(
    content: &str,
    at: Option<Vec2>,
    canvas: &mut CanvasState,
    notebook: &mut Notebook,
    manifest: &mut NotebookManifest,
    autosave: &mut AutosaveController,
    selection_state: &mut SelectionState,
    ui_state: &mut UiState,
) {
    let position = at.unwrap_or_else(|| {
        let center = visible_page_center(canvas);
        Vec2::new((center.x - 600.0).max(150.0), center.y)
    });
    let mut edit = ink_notes::text::TextEditing::new_at(
        notebook.current_page().id,
        position,
        notebook.current_page().text_right_edge(position.x),
        ui_state.text_size_pt * ink_notes::text::UNITS_PER_PT,
        ui_state.text_font,
        ui_state.text_color,
    );
    edit.text.content = content.replace("\r\n", "\n").replace('\r', "\n");
    let id = notebook.next_stroke_id();
    let Some(stroke) = edit.to_stroke(id) else {
        return;
    };
    let page = notebook.current_page_mut();
    page.add_stroke(stroke);
    canvas.sync_page(page);
    manifest.next_stroke_id = notebook.next_stroke_id;
    autosave.mark_dirty(notebook, manifest);
    selection_state.select_single(id);
    ui_state.active_tool = ActiveTool::Select;
    ui_state.set_export_message(t!("Text eingefügt – mit dem Text-Werkzeug (T) änderbar"));
}

/// Beendet eine laufende Textbearbeitung: übernimmt das Feld (neu, geändert oder – wenn
/// geleert – entfernt) als einen Undo-Schritt auf seiner Seite.
fn finish_text_edit(
    canvas: &mut CanvasState,
    notebook: &mut Notebook,
    manifest: &mut NotebookManifest,
    autosave: &mut AutosaveController,
    ui_state: &mut UiState,
) {
    let Some(edit) = ui_state.text_edit.take() else {
        return;
    };
    canvas.set_hidden_text(None);
    if let Some(index) = notebook.pages.iter().position(|p| p.id == edit.page_id) {
        if edit.is_changed() && !notebook.pages[index].is_readonly() {
            let id = match &edit.original {
                Some(original) => original.id,
                None => notebook.next_stroke_id(),
            };
            manifest.next_stroke_id = notebook.next_stroke_id;
            let page = &mut notebook.pages[index];
            match (edit.original.clone(), edit.to_stroke(id)) {
                (None, Some(stroke)) => page.add_stroke(stroke),
                (Some(before), Some(after)) => {
                    page.commit_modified_strokes(vec![before], vec![after])
                }
                (Some(before), None) => {
                    page.remove_strokes_by_ids(&[before.id]);
                }
                (None, None) => {}
            }
            manifest.sync_from_notebook(notebook);
            autosave.mark_page_dirty(&notebook.pages[index], manifest);
        }
    }
    canvas.sync_page(notebook.current_page_mut());
}

/// Text-Werkzeug: Tippen auf das Blatt beendet eine laufende Bearbeitung und öffnet das
/// Textfeld unter `page_pt` zum Bearbeiten – oder ein neues an dieser Stelle.
fn text_tool_click(
    page_pt: Vec2,
    canvas: &mut CanvasState,
    notebook: &mut Notebook,
    manifest: &mut NotebookManifest,
    autosave: &mut AutosaveController,
    ui_state: &mut UiState,
) {
    finish_text_edit(canvas, notebook, manifest, autosave, ui_state);
    let page = notebook.current_page();
    if page.is_readonly() {
        return;
    }
    let hit = page
        .strokes
        .iter()
        .rev()
        .find(|s| s.is_text() && s.distance_to_point(page_pt) <= 0.0);
    let edit = match hit {
        Some(stroke) => {
            let edit = ink_notes::text::TextEditing::of_stroke(page.id, stroke);
            if let Some(edit) = &edit {
                // Die Schriftknöpfe zeigen die Schrift des Felds und ändern sie
                ui_state.text_size_pt =
                    (edit.text.size / ink_notes::text::UNITS_PER_PT * 2.0).round() / 2.0;
                ui_state.text_font = edit.text.font;
                ui_state.text_color = edit.color;
                canvas.set_hidden_text(Some(stroke.id));
            }
            edit
        }
        None => Some(ink_notes::text::TextEditing::new_at(
            page.id,
            page_pt,
            page.text_right_edge(page_pt.x),
            ui_state.text_size_pt * ink_notes::text::UNITS_PER_PT,
            ui_state.text_font,
            ui_state.text_color,
        )),
    };
    ui_state.text_edit = edit;
    canvas.sync_page(notebook.current_page_mut());
}

/// Textur für einen Schlüssel der Bildebene: `<pdf>#<seite>` rendert der Hintergrund-Thread,
/// alles andere ist eine Bilddatei im Asset-Ordner.
fn texture_for(
    key: &str,
    assets: &AssetStore,
    pdf_renderer: &mut PdfRenderService,
) -> TextureSource {
    if !key.contains('#') {
        return match assets.load_rgba(key, ink_notes::assets::MAX_DISPLAY_DIMENSION) {
            Ok(image) => TextureSource::Ready(image),
            Err(err) => TextureSource::Failed(err.to_string()),
        };
    }
    let Some((Ok(path), page, max_pixels)) = ink_notes::pdf_doc::parse_page_key(key)
        .map(|(asset, page, px)| (assets.path_of(asset), page, px))
    else {
        return TextureSource::Failed(t!("Ungültiger PDF-Verweis '{key}'", key = key));
    };
    match pdf_renderer.take(key, path, page, max_pixels) {
        Some(Ok(levels)) => TextureSource::ReadyMips(levels),
        Some(Err(err)) => TextureSource::Failed(err),
        None => TextureSource::Pending,
    }
}

/// Abbildung Seite ↔ logische Bildschirmpunkte für die Oberfläche (Lineal).
fn page_view(canvas: &CanvasState, scale_factor: f32) -> ui::PageView {
    let origin = canvas.page_to_screen(Vec2::ZERO);
    let unit = canvas.page_to_screen(Vec2::new(1000.0, 0.0));
    ui::PageView {
        origin: egui::pos2(origin[0] / scale_factor, origin[1] / scale_factor),
        scale: (unit[0] - origin[0]) / 1000.0 / scale_factor,
    }
}

fn copy_message(count: usize, as_image: bool) -> String {
    if as_image {
        t!(
            "{count} Objekt(e) kopiert – auch als Bild für andere Programme",
            count = count
        )
    } else {
        t!("{count} Objekt(e) kopiert", count = count)
    }
}

/// Exportiert die aktuelle Seite im Hintergrund als PNG oder SVG nach ~/Documents.
fn start_page_export(
    kind: ui::PageExportKind,
    notebook: &Notebook,
    storage_dir: &Path,
    title: &str,
    pattern: PatternColor,
    result_tx: Sender<Result<PathBuf, String>>,
    window: Arc<Window>,
) {
    use ink_notes::page_export::{page_png, page_svg, ImageBackground};
    let page = notebook.current_page().clone();
    let number = notebook.current_page_index + 1;
    let storage_dir = storage_dir.to_path_buf();
    let (suffix, ext) = match kind {
        ui::PageExportKind::Png => ("", "png"),
        ui::PageExportKind::PngTransparent => ("_transparent", "png"),
        ui::PageExportKind::Svg => ("", "svg"),
    };
    let path = export::resolve_export_path(
        &t!(
            "{title}_Seite_{number}{suffix}",
            title = title,
            number = number,
            suffix = suffix
        ),
        ext,
    );
    std::thread::spawn(move || {
        let assets = AssetStore::new(&storage_dir);
        let bytes = match kind {
            ui::PageExportKind::Png => {
                page_png(&page, Some(&assets), ImageBackground::Paper, pattern)
            }
            ui::PageExportKind::PngTransparent => {
                page_png(&page, Some(&assets), ImageBackground::Transparent, pattern)
            }
            ui::PageExportKind::Svg => {
                page_svg(&page, Some(&assets), pattern).map(String::into_bytes)
            }
        };
        let result = bytes
            .map_err(|e| e.to_string())
            .and_then(|bytes| std::fs::write(&path, bytes).map_err(|e| e.to_string()))
            .map(|()| path);
        let _ = result_tx.send(result);
        window.request_redraw();
    });
}

/// Seitenmitte des sichtbaren Bereichs (Ziel für eingefügte Bilder).
fn visible_page_center(canvas: &CanvasState) -> Vec2 {
    canvas.screen_to_page_clamped(
        canvas.size.width as f32 / 2.0,
        canvas.size.height as f32 / 2.0,
    )
}

/// Strg+V bzw. „Einfügen“: ein neu kopiertes Systembild hat Vorrang, sonst werden die intern
/// kopierten Objekte eingefügt. `at`: Stelle auf der Seite (lange gedrückt → Einfügen), sonst
/// die Mitte des sichtbaren Bereichs.
#[allow(clippy::too_many_arguments)]
fn paste(
    at: Option<Vec2>,
    clipboard: &mut ClipboardBridge,
    storage_dir: &Path,
    canvas: &mut CanvasState,
    notebook: &mut Notebook,
    manifest: &mut NotebookManifest,
    autosave: &mut AutosaveController,
    selection_state: &mut SelectionState,
    ui_state: &mut UiState,
) {
    if notebook.current_page().is_readonly() {
        return;
    }
    if let Some((image, hash)) = clipboard.system_image() {
        // Die jüngste Kopie gewinnt: das Systembild nur, wenn es nach dem letzten internen
        // Kopieren in die Zwischenablage kam
        if selection_state.clipboard.is_empty() || clipboard.image_hash_at_copy != Some(hash) {
            let assets = AssetStore::new(storage_dir);
            match assets.import_rgba(
                image.width as u32,
                image.height as u32,
                image.bytes.into_owned(),
            ) {
                Ok(imported) => {
                    let center = at.unwrap_or_else(|| visible_page_center(canvas));
                    insert_image_on_page(
                        imported,
                        center,
                        canvas,
                        notebook,
                        manifest,
                        autosave,
                        selection_state,
                        ui_state,
                    );
                }
                Err(err) => {
                    ui_state.set_export_message(t!("Bild nicht eingefügt: {err}", err = err))
                }
            }
            return;
        }
    }

    // Aus anderen Programmen kopierter Text wird ein Textfeld
    if let Some(text) = clipboard.system_text() {
        if selection_state.clipboard.is_empty() || clipboard.text_at_copy.as_deref() != Some(&text)
        {
            insert_text_on_page(
                &text,
                at,
                canvas,
                notebook,
                manifest,
                autosave,
                selection_state,
                ui_state,
            );
            return;
        }
    }

    // Bilder aus einem anderen Notizbuch mitnehmen
    if let Some(source) = clipboard
        .source_dir
        .as_deref()
        .filter(|d| *d != storage_dir)
    {
        let from = AssetStore::new(source);
        let to = AssetStore::new(storage_dir);
        for asset in selection_state
            .clipboard
            .iter()
            .filter_map(|s| s.image.as_deref())
        {
            if let Err(err) = to.copy_from(&from, asset) {
                eprintln!("[Zwischenablage] Bild '{asset}' nicht übernommen: {err}");
            }
        }
    }
    let mut next_id = notebook.next_stroke_id;
    let new_ids =
        selection_state.paste_from_clipboard(notebook.current_page_mut(), at, &mut || {
            let id = next_id;
            next_id += 1;
            id
        });
    notebook.next_stroke_id = next_id;
    if !new_ids.is_empty() {
        canvas.sync_page(notebook.current_page_mut());
        manifest.next_stroke_id = notebook.next_stroke_id;
        autosave.mark_dirty(notebook, manifest);
        ui_state.set_export_message(t!("{count} Objekt(e) eingefügt", count = new_ids.len()));
    } else {
        ui_state.set_export_message(t!("Die Zwischenablage ist leer").to_string());
    }
}

/// Liest eine Bilddatei und fügt sie um `center` ein.
#[allow(clippy::too_many_arguments)]
fn insert_image_file(
    path: &Path,
    center: Vec2,
    storage_dir: &Path,
    canvas: &mut CanvasState,
    notebook: &mut Notebook,
    manifest: &mut NotebookManifest,
    autosave: &mut AutosaveController,
    selection_state: &mut SelectionState,
    ui_state: &mut UiState,
) {
    if notebook.current_page().is_readonly() {
        ui_state.set_export_message(t!("Seite ist schreibgeschützt").to_string());
        return;
    }
    let result = std::fs::read(path)
        .map_err(|e| e.to_string())
        .and_then(|bytes| {
            AssetStore::new(storage_dir)
                .import_bytes(&bytes)
                .map_err(|e| e.to_string())
        });
    match result {
        Ok(imported) => insert_image_on_page(
            imported,
            center,
            canvas,
            notebook,
            manifest,
            autosave,
            selection_state,
            ui_state,
        ),
        Err(err) => ui_state.set_export_message(t!(
            "'{name}' nicht eingefügt: {err}",
            name = path
                .file_name()
                .map(|n| n.to_string_lossy())
                .unwrap_or_default(),
            err = err
        )),
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Gespeicherte UI-Einstellungen; zuerst, damit alle Texte in der gewählten Sprache sind
    let ui_settings_path = UiSettings::default_path();
    let mut ui_settings = ui_settings_path
        .as_deref()
        .map(UiSettings::load_from)
        .unwrap_or_default();
    i18n::set_language(ui_settings.language.unwrap_or_else(Language::system));

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
    // App-Kennung wie die .desktop-Datei (eu.pansi.ink-notes): Daran ordnen Dock und Taskleiste
    // das Fenster dem App-Symbol zu (Wayland: app_id, X11: WM_CLASS). Das Fenstersymbol selbst
    // nutzt nur X11.
    let builder = WindowBuilder::new()
        .with_title("ink-notes")
        .with_inner_size(winit::dpi::LogicalSize::new(1280.0, 850.0))
        .with_window_icon(app_icon());
    let builder =
        winit::platform::wayland::WindowBuilderExtWayland::with_name(builder, APP_ID, "ink-notes");
    let builder =
        winit::platform::x11::WindowBuilderExtX11::with_name(builder, APP_ID, "ink-notes");
    let window = Arc::new(builder.build(&event_loop)?);

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

    // Bildschirmtastatur (nur Wayland): eigenes text-input-Objekt, siehe osk.rs
    let mut keyboard_requester = match window.display_handle().map(|handle| handle.as_raw()) {
        // Safety: `window` lebt bis zum Ende von main, die Event-Schleife besitzt beide
        Ok(RawDisplayHandle::Wayland(handle)) => {
            Some(unsafe { KeyboardRequester::new(handle.display.as_ptr()) })
        }
        _ => None,
    };
    let mut ime_was_active = false;
    let mut ime_composing = false;

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
        ("default".to_string(), t!("Standard-Notizbuch").to_string())
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
                startup_warning = Some(t!(
                    "Notizbuch '{title}' konnte nicht geladen werden – neues Notizbuch angelegt",
                    title = initial_title
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
        let notebook = Notebook::new();
        let mut manifest = NotebookManifest::new(&initial_id, &initial_title);
        // Ganz neu (z. B. erster Start): sofort speichern, sonst steht es bis zur ersten
        // Änderung ohne Manifest als „beschädigt“ in der Notizbuchliste. Ein vorhandener
        // Ordner ohne Manifest bleibt unangetastet.
        if !storage_dir.exists() {
            if let Err(err) = storage::save_notebook(&storage_dir, &notebook, &mut manifest) {
                eprintln!("[Storage] Neues Notizbuch nicht gespeichert: {err}");
            }
        }
        (notebook, manifest)
    };

    // 2. Asynchronen Autosave-Controller mit 1.5s Debounce starten
    let mut autosave = AutosaveController::new(storage_dir.clone(), Duration::from_millis(1500));

    // 3. egui Context und State initialisieren
    let egui_ctx = egui::Context::default();
    ui::configure_context(&egui_ctx);

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
    ui_state.sidebar_side = ui_settings.sidebar_side;
    ui_state.language = ui_settings.language;
    ui_state.paper_theme = ui_settings.paper_theme;
    ui_state.adapt_ink_colors = ui_settings.adapt_ink_colors;
    ui_state.pattern_color = ui_settings.pattern_color;
    ui_state.pressure_curve = ui_settings.pressure_curve;
    ui_state.continuous_scroll = ui_settings.continuous_scroll;
    ui_state.pen_presets = ui_settings.pen_presets;
    ui_state.stylus_buttons = ui_settings.stylus_buttons.clone();
    ui_state.select_pen(ui_settings.active_pen);
    ui_state.highlighter_colors = ui_settings.highlighter_colors;
    ui_state.select_highlighter_color(ui_settings.active_highlighter);
    ui_state.text_colors = ui_settings.text_colors;
    ui_state.active_text_color = ui_settings.active_text_color;
    ui_state.text_color = ui_settings.text_colors[ui_settings.active_text_color];
    ui_state.current_notebook_id = manifest.id.clone();
    ui_state.document_title = manifest.title.clone();
    ui_state.available_notebooks = available_notebooks;
    ui_state.library = Library::load(&storage_base_dir);
    if let Some(warning) = startup_warning {
        refresh_notebooks(&storage_base_dir, &mut ui_state);
        ui_state.set_export_message(warning);
    }
    let mut selection_state = SelectionState::new();
    // Zuletzt gelöschte Seite (Notizbuch-Kennung, Index, Seite) zum Wiederherstellen in dieser
    // Sitzung
    let mut last_deleted_page: Option<(String, usize, ink_notes::document::Page)> = None;
    let mut clipboard = ClipboardBridge::new();
    // PDF-Seiten werden im Hintergrund gerendert und wecken danach den Event-Loop
    let mut pdf_renderer = {
        let window = window.clone();
        PdfRenderService::new(move || window.request_redraw())
    };

    let mut _notebook_lock = match storage::NotebookLock::acquire(&storage_dir) {
        Ok(lock) => {
            autosave.set_read_only(false);
            Some(lock)
        }
        Err(storage::StorageError::AlreadyLocked(msg)) => {
            eprintln!("[Storage-Lock] {msg}");
            autosave.set_read_only(true);
            ui_state.set_export_message(t!("⚠ In anderer Instanz geöffnet (Schreibschutz)"));
            None
        }
        Err(e) => {
            eprintln!("[Storage-Lock] Konnte Lock nicht erwerben: {e}");
            autosave.set_read_only(true);
            None
        }
    };

    // Initiale Seite und Werkzeugeinstellungen mit Canvas synchronisieren
    canvas.set_paper_theme(ui_state.paper_theme, ui_state.adapt_ink_colors);
    canvas.set_pattern_color(ui_state.pattern_color);
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
    // Alle aufliegenden Finger (auch auf der UI), unabhängig von der Gestenauswertung
    let mut touches_down: std::collections::HashSet<u64> = std::collections::HashSet::new();
    let mut pinch_prev_center: Option<(f32, f32)> = None;
    let mut is_pinch_gesturing = false;
    let mut is_touch_drawing = false;
    let mut is_touch_panning = false;
    // Langes Drücken mit einem Finger öffnet das Kontextmenü
    let mut long_press = ui::LongPress::default();
    let mut touch_pan_last_pos = (0.0_f32, 0.0_f32);
    // Nachgleiten nach schnellem Wischen mit dem Finger (Issue #28)
    let mut fling = ink_notes::viewport::Fling::default();
    // Diagnosis for Issue #28: log every finger lift and the swipe speed
    let debug_fling = std::env::var_os("INK_NOTES_DEBUG_FLING").is_some();
    if debug_fling {
        eprintln!("[Nachgleiten] Diagnose aktiv – bitte mit dem Finger wischen");
    }

    // Stylus / Digitizer Zustand (Octotablet / Wayland Tablet Protocol)
    let mut is_stylus_drawing = false;
    // Stift zieht mit dem Auswahlwerkzeug (Rahmen, Verschieben, Drehen, Skalieren)
    let mut is_stylus_selecting = false;
    let mut is_stylus_in_proximity = false;
    let mut stylus_tool_id: Option<octotablet::tool::ID> = None;
    // Gedrückte Stifttaste (Kennung, Belegung) und das Werkzeug, das während eines Strichs mit
    // einer Werkzeug-Taste (Auswählen, Textmarker, Stift) vertreten wird
    let mut stylus_held: Option<(u64, StylusAction)> = None;
    let mut tool_before_button: Option<ActiveTool> = None;
    // Platz schaffen läuft mit dem Stift bzw. dem Finger (sonst mit der Maus)
    let mut is_stylus_spacing = false;
    let mut is_touch_spacing = false;
    // View only: the pen moves the page and remembers its last screen position (Issue #35)
    let mut stylus_pan_last: Option<(f32, f32)> = None;
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
    println!("  Stift / Textmarker / Formen (Auto-Snap) / Radierer / Auswählen (Lasso) / Text");
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
    println!("Text:");
    println!("  T              : Text-Werkzeug umschalten");
    println!("  Klick ins Blatt: Neues Textfeld (vorhandenes Textfeld: bearbeiten)");
    println!("  Esc            : Textbearbeitung beenden");
    println!("Tastatur- & Maus-Shortcuts:");
    println!("  Mausrad        : Scrollen (durchgehende Ansicht), sonst Zoom");
    println!("  Strg + Mausrad : Stufenloser Zoom zentriert auf Cursor");
    println!("  Mittlere Taste : Canvas verschieben (Pan)");
    println!("  Leertaste+Drag : Canvas verschieben (Pan)");
    println!("  Strg + 0       : Zoom zurücksetzen (100% zentriert)");
    println!("  Draw-and-Hold  : Form am Strichende >400ms halten zum automatischen Einrasten");
    println!("  Strg + Z / Y   : Transaktionales Undo / Redo für alle Aktionen");
    println!("  Strg + E       : Vektor-PDF-Export (~/Documents/<Titel>.pdf)");
    println!("  Strg + S       : Jetzt speichern (Disk Flush)");
    println!("  ESC            : Dialoge / Menüs schließen oder Selektion aufheben");
    println!("  Strg + Q       : Beenden (mit sicherem Disk-Flush)");

    // Durchgehende Ansicht: Scrollposition beim letzten Abgleich der aktuellen Seite (Zoom,
    // Pan unabhängig von der Bezugsseite)
    let mut last_scroll: Option<[f32; 3]> = None;

    // Durchgehende Ansicht: vor Zeichnen, Radieren oder Auswählen auf die Seite unter dem
    // Bildschirmpunkt wechseln
    macro_rules! focus_page_at {
        ($pos:expr) => {
            if let Some(index) = page_to_focus(
                &canvas,
                &notebook,
                &ui_state,
                &selection_state,
                egui_ctx.pixels_per_point(),
                $pos,
            ) {
                switch_to_page(
                    index,
                    true,
                    &mut canvas,
                    &mut notebook,
                    &mut manifest,
                    &mut autosave,
                    &storage_dir,
                    &mut selection_state,
                    &mut ui_state,
                    &window,
                );
            }
        };
    }

    event_loop.run(move |event, target| {
        // Anordnung der Seiten gehört zum offenen Notizbuch (Issue #30)
        // Endlos ist ein Notizbuch an seiner einzigen, endlosen Seite erkennbar
        ui_state.endless = notebook.is_endless();
        ui_state.continuous_scroll = !ui_state.endless
            && manifest.layout_or(ui_settings.continuous_scroll) == NotebookLayout::Continuous;
        ui_state.endless_switch_possible = if ui_state.endless {
            notebook.can_make_paged()
        } else {
            notebook.can_make_endless()
        };
        match event {
            Event::AboutToWait => {
                let prev_status = autosave.status();
                let new_status = autosave.update();
                if new_status != prev_status {
                    update_window_title(&window, &notebook, &manifest, ui_state.active_tool, new_status);
                }

                while let Ok(result) = export_rx.try_recv() {
                    match result {
                        Ok(path) => ui_state.set_export_message(t!("Gespeichert: {path}", path = path.display())),
                        Err(err) => ui_state.set_export_message(t!("Export fehlgeschlagen: {err}", err = err)),
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
                        // Aufsetzen erst mit der Position desselben Frames verarbeiten
                        let tablet_events = ink_notes::platform::down_after_pose(
                            tablet_events,
                            |e| matches!(e, octotablet::events::Event::Tool { event: octotablet::events::ToolEvent::Down, .. }),
                            |e| matches!(e, octotablet::events::Event::Tool { event: octotablet::events::ToolEvent::Pose(_), .. }),
                        );

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
                                            stylus_pan_last = None;
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
                                            if is_stylus_spacing {
                                                is_stylus_spacing = false;
                                                space_release(&mut canvas, &mut notebook, &mut manifest, &mut autosave, &storage_dir, &mut ui_state);
                                                window.request_redraw();
                                            }
                                            if is_stylus_selecting {
                                                is_stylus_selecting = false;
                                                selection_release(false, &mut notebook, &mut manifest, &mut autosave, &mut selection_state, &mut canvas);
                                                window.request_redraw();
                                            }
                                            if let Some(tool) = tool_before_button.take() {
                                                activate_tool(tool, &mut ui_state, &mut canvas);
                                            }
                                            stylus_held = None;
                                            ui_state.pressed_stylus_button = None;
                                        }
                                    }
                                    octotablet::events::ToolEvent::Button { button_id, pressed } => {
                                        // Tasten werden beim ersten Drücken gelernt (Standard: Radieren)
                                        let key = stylus_button_key(&button_id);
                                        let mut index = ui_state.stylus_buttons.iter().position(|b| b.id == key);
                                        if index.is_none() && pressed && ui_state.stylus_buttons.len() < MAX_STYLUS_BUTTONS {
                                            ui_state.stylus_buttons.push(StylusButton { id: key, action: StylusAction::Erase });
                                            index = Some(ui_state.stylus_buttons.len() - 1);
                                            ui_settings.stylus_buttons = ui_state.stylus_buttons.clone();
                                            save_ui_settings(&ui_settings, ui_settings_path.as_deref(), &mut ui_state);
                                        }
                                        let action = index.map_or(StylusAction::Erase, |i| ui_state.stylus_buttons[i].action);
                                        if pressed {
                                            stylus_held = Some((key, action));
                                            ui_state.pressed_stylus_button = index;
                                            if action == StylusAction::Erase && is_stylus_drawing {
                                                is_stylus_drawing = false;
                                                is_drawing = false;
                                                canvas.cancel_stroke();
                                                is_erasing = true;
                                            }
                                        } else if stylus_held.is_some_and(|(held, _)| held == key) {
                                            stylus_held = None;
                                            ui_state.pressed_stylus_button = None;
                                            if action == StylusAction::Erase
                                                && is_erasing
                                                && !tool_is_hardware_eraser
                                                && ui_state.active_tool != ActiveTool::Eraser
                                            {
                                                is_erasing = false;
                                            }
                                        }
                                        window.request_redraw();
                                    }
                                    octotablet::events::ToolEvent::Pose(pose) => {
                                        is_stylus_in_proximity = true;
                                        let sx = pose.position[0] * scale;
                                        let sy = pose.position[1] * scale;
                                        stylus_screen_pos = (sx, sy);
                                        cursor_screen_pos = (sx, sy);

                                        if let Some(p) = pose.pressure.get() {
                                            current_pressure = ui_state.pressure_curve.apply(p);
                                            ui_state.last_raw_pressure = Some(p);
                                        }

                                        // Stiftbewegung an egui weitergeben (Hover, Schieberegler ziehen),
                                        // solange nicht auf dem Blatt gezeichnet oder radiert wird
                                        if stylus_ui_capture
                                            || (!is_stylus_drawing && !is_erasing && !is_stylus_selecting && stylus_pan_last.is_none())
                                        {
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

                                        if let Some((lx, ly)) = stylus_pan_last {
                                            let delta = Vec2::new(sx - lx, sy - ly);
                                            stylus_pan_last = Some((sx, sy));
                                            canvas.pan(delta.x, delta.y);
                                            fling.track(delta, Instant::now());
                                            window.request_redraw();
                                        } else if is_stylus_spacing {
                                            space_move((sx, sy), &canvas, &mut ui_state);
                                            window.request_redraw();
                                        } else if is_erasing {
                                            focus_page_at!((sx, sy));
                                            if let Some(page_pt) = canvas.screen_to_page(sx, sy) {
                                                if notebook.erase_at(page_pt,
                                                    eraser_radius,
                                                    stroke_max_radius, ui_state.eraser_mode) {
                                                    canvas.sync_page(notebook.current_page_mut());
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
                                        } else if is_stylus_selecting {
                                            if let Some(page_pt) = canvas.screen_to_page(sx, sy) {
                                                selection_state.pending_drag = Some(page_pt);
                                                window.request_redraw();
                                            }
                                        } else if is_stylus_drawing && is_drawing {
                                            let dx = sx - stroke_hold_anchor_pos.0;
                                            let dy = sy - stroke_hold_anchor_pos.1;
                                            if (dx * dx + dy * dy).sqrt() > hold_tolerance(&window) {
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

                                        if ui_state.view_mode {
                                            // View only: the pen moves the page like a finger
                                            fling.stop();
                                            stylus_pan_last = Some((sx, sy));
                                            continue;
                                        }

                                        focus_page_at!((sx, sy));
                                        // Werkzeug-Taste gedrückt: für diesen Strich dieses Werkzeug
                                        let button_action = stylus_held.map(|(_, action)| action);
                                        let button_tool = match button_action {
                                            Some(StylusAction::Select) => Some(ActiveTool::Select),
                                            Some(StylusAction::Highlighter) => Some(ActiveTool::Highlighter),
                                            Some(StylusAction::Pen) => Some(ActiveTool::Pen),
                                            _ => None,
                                        };
                                        if let Some(tool) = button_tool
                                            .filter(|&t| !tool_is_hardware_eraser && t != ui_state.active_tool)
                                        {
                                            tool_before_button = Some(ui_state.active_tool);
                                            activate_tool(tool, &mut ui_state, &mut canvas);
                                        }
                                        let should_erase = tool_is_hardware_eraser
                                            || button_action == Some(StylusAction::Erase)
                                            || ui_state.active_tool == ActiveTool::Eraser;

                                        if !should_erase && ui_state.active_tool == ActiveTool::Space {
                                            is_stylus_spacing = space_press((sx, sy), &canvas, &notebook, &mut ui_state);
                                            window.request_redraw();
                                        } else if !should_erase && ui_state.active_tool == ActiveTool::Text {
                                            if let Some(page_pt) = canvas.screen_to_page(sx, sy) {
                                                text_tool_click(page_pt, &mut canvas, &mut notebook, &mut manifest, &mut autosave, &mut ui_state);
                                                update_window_title(&window, &notebook, &manifest, ui_state.active_tool, autosave.status());
                                                window.request_redraw();
                                            }
                                        } else if !should_erase && ui_state.active_tool == ActiveTool::Select {
                                            if selection_press((sx, sy), false, &canvas, notebook.current_page(), egui_ctx.pixels_per_point(), &mut selection_state) {
                                                is_stylus_selecting = true;
                                                window.request_redraw();
                                            }
                                        } else if should_erase {
                                            is_erasing = true;
                                            if let Some(page_pt) = canvas.screen_to_page(sx, sy) {
                                                if notebook.erase_at(page_pt,
                                                    eraser_radius,
                                                    stroke_max_radius, ui_state.eraser_mode) {
                                                    canvas.sync_page(notebook.current_page_mut());
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

                                        if stylus_pan_last.take().is_some() {
                                            fling.release(Instant::now());
                                            window.request_redraw();
                                        }

                                        if is_erasing {
                                            is_erasing = false;
                                        }

                                        if is_stylus_spacing {
                                            is_stylus_spacing = false;
                                            space_release(&mut canvas, &mut notebook, &mut manifest, &mut autosave, &storage_dir, &mut ui_state);
                                            window.request_redraw();
                                        }

                                        if is_stylus_selecting {
                                            is_stylus_selecting = false;
                                            if selection_release(false, &mut notebook, &mut manifest, &mut autosave, &mut selection_state, &mut canvas) {
                                                window.request_redraw();
                                            }
                                        }
                                        let restore_tool = tool_before_button.take();

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
                                        // Nach einem Strich mit Werkzeug-Taste wieder das gewählte Werkzeug
                                        if let Some(tool) = restore_tool {
                                            activate_tool(tool, &mut ui_state, &mut canvas);
                                            window.request_redraw();
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                }

                // Draw-and-Hold Überprüfung: Stift regungslos für > 400 ms gehalten (siehe hold_tolerance)
                if is_drawing && !stroke_already_snapped {
                    let elapsed = stroke_last_motion_time.elapsed();
                    let dx = cursor_screen_pos.0 - stroke_hold_anchor_pos.0;
                    let dy = cursor_screen_pos.1 - stroke_hold_anchor_pos.1;
                    let dist = (dx * dx + dy * dy).sqrt();

                    if elapsed >= Duration::from_millis(400) && dist < hold_tolerance(&window) && canvas.live_samples.len() >= 4 {
                        if let Some(shape) = geometry::detect_shape(&canvas.live_samples) {
                            stroke_already_snapped = true;
                            stroke_snapped_shape = Some(shape.clone());
                            // Kreuzt die Gerade den vorigen Strich, zeigt die Vorschau schon das
                            // Achsenkreuz (übernommen wird es beim Absetzen)
                            // Textmarker bilden kein Achsenkreuz
                            let shape = if ui_state.active_tool == ActiveTool::Highlighter {
                                shape
                            } else {
                                axis_cross_with_previous(&notebook, &shape)
                                    .map_or(shape, |(_, cross)| cross)
                            };
                            let width_units = ui_state.current_stroke_width_mm() * ink_notes::document::UNITS_PER_MM;
                            let (samples, vertices) = geometry::shape_to_mesh(&shape, width_units, canvas.current_ink_color);
                            canvas.set_live_shape_mesh(samples, vertices);
                            ui_state.set_export_message(t!("{shape} eingerastet!", shape = shape.name()));
                            window.request_redraw();
                        }
                    }
                }

                // Langes Drücken mit dem Finger: Kontextmenü zum Einfügen an dieser Stelle
                if let Some(pos) = long_press.fire(Instant::now()) {
                    if is_touch_drawing {
                        is_touch_drawing = false;
                        is_drawing = false;
                        canvas.cancel_stroke();
                    }
                    is_touch_panning = false;
                    focus_page_at!(pos);
                    if let Some(page_pt) = canvas.screen_to_page(pos.0, pos.1) {
                        let scale = window.scale_factor() as f32;
                        ui_state.close_overlays();
                        // Leicht versetzt, damit der Finger das Menü nicht verdeckt
                        ui_state.context_menu = Some(ui::ContextMenu {
                            screen: egui::pos2(pos.0 / scale + 12.0, pos.1 / scale + 12.0),
                            page: page_pt,
                        });
                    }
                    window.request_redraw();
                }

                // Nachgleiten nach dem Wischen
                if let Some(delta) = fling.step(Instant::now()) {
                    canvas.pan(delta.x, delta.y);
                    window.request_redraw();
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
                    || fling.is_active()
                {
                    target.set_control_flow(ControlFlow::wait_duration(Duration::from_millis(8)));
                } else if let Some(at) = [egui_repaint_at, long_press.deadline()]
                    .into_iter()
                    .flatten()
                    .min()
                {
                    target.set_control_flow(ControlFlow::WaitUntil(at));
                } else {
                    target.set_control_flow(ControlFlow::Wait);
                }
            }

            Event::WindowEvent { event, window_id } if window_id == window.id() => {
                // 1. Window-Event zuerst an egui übergeben
                if let WindowEvent::Touch(touch) = &event {
                    match touch.phase {
                        TouchPhase::Started => {
                            touches_down.insert(touch.id);
                        }
                        TouchPhase::Ended | TouchPhase::Cancelled => {
                            touches_down.remove(&touch.id);
                        }
                        TouchPhase::Moved => {}
                    }
                }
                // Beim Antippen blendet GNOME den Mauszeiger aus und meldet CursorLeft, während der
                // Finger noch aufliegt. egui-winit verwirft dabei die Zeigerposition und meldet das
                // Abheben nicht mehr, der Tipp ginge verloren – daher erfährt egui davon nichts.
                let egui_response = if matches!(event, WindowEvent::CursorLeft { .. })
                    && !touches_down.is_empty()
                {
                    egui_winit::EventResponse {
                        repaint: true,
                        consumed: false,
                    }
                } else {
                    egui_winit_state.on_window_event(&window, &event)
                };
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
                        finish_text_edit(&mut canvas, &mut notebook, &mut manifest, &mut autosave, &mut ui_state);
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

                    // Bilddatei ins Fenster gezogen: an der Ablagestelle einfügen
                    WindowEvent::DroppedFile(path) => {
                        if ink_notes::file_picker::is_pdf_path(&path) {
                            ui_state.queued_actions.push(UiAction::ImportPdf(path));
                        } else if ink_notes::assets::is_supported_image_path(&path) {
                            let (sx, sy) = cursor_screen_pos;
                            let center = canvas.screen_to_page_clamped(sx, sy);
                            insert_image_file(
                                &path, center, &storage_dir, &mut canvas, &mut notebook, &mut manifest,
                                &mut autosave, &mut selection_state, &mut ui_state,
                            );
                            update_window_title(&window, &notebook, &manifest, ui_state.active_tool, autosave.status());
                        } else {
                            ui_state.set_export_message(t!("Nur Bilder (PNG, JPEG, GIF, WebP, BMP) und PDFs können eingefügt werden").to_string());
                        }
                        window.request_redraw();
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
                        fling.stop();
                        is_stylus_drawing = false;
                        is_stylus_selecting = false;
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

                        // Offenes Textfeld ohne Fokus (z. B. nach einem Klick in die Leiste):
                        // Tasten nicht als Kürzel deuten, sondern das Feld wieder fokussieren
                        if let Some(edit) = &mut ui_state.text_edit {
                            if key_state == ElementState::Pressed
                                && !matches!(logical_key, Key::Named(NamedKey::Escape))
                            {
                                edit.focus = true;
                                window.request_redraw();
                                return;
                            }
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

                        if ui_state.view_mode && !ui::key_allowed_in_view_mode(&logical_key, ctrl) {
                            return;
                        }

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
                                } else if let Some(edit) = &mut ui_state.text_edit {
                                    edit.finish = true;
                                    changed = true;
                                } else if !selection_state.is_empty() {
                                    selection_state.clear();
                                    changed = true;
                                }
                                if changed {
                                    window.request_redraw();
                                }
                            }

                            // Undo: Strg + Z
                            Key::Character(c) if c.eq_ignore_ascii_case("z") && ctrl && !shift => {
                                finish_text_edit(&mut canvas, &mut notebook, &mut manifest, &mut autosave, &mut ui_state);
                                if let Some(changed) = notebook.undo() {
                                    selection_state.clear();
                                    canvas.sync_page(notebook.current_page_mut());
                                    persist_pages(&changed, &notebook, &mut manifest, &mut autosave);
                                    update_window_title(&window, &notebook, &manifest, ui_state.active_tool, autosave.status());
                                    window.request_redraw();
                                }
                            }

                            // Redo: Strg + Y oder Strg + Umschalt + Z
                            Key::Character(c)
                                if (c.eq_ignore_ascii_case("y") && ctrl)
                                    || (c.eq_ignore_ascii_case("z") && ctrl && shift) =>
                            {
                                finish_text_edit(&mut canvas, &mut notebook, &mut manifest, &mut autosave, &mut ui_state);
                                if let Some(changed) = notebook.redo() {
                                    selection_state.clear();
                                    canvas.sync_page(notebook.current_page_mut());
                                    persist_pages(&changed, &notebook, &mut manifest, &mut autosave);
                                    update_window_title(&window, &notebook, &manifest, ui_state.active_tool, autosave.status());
                                    window.request_redraw();
                                }
                            }

                            // Kopieren: Strg + C
                            Key::Character(c) if c.eq_ignore_ascii_case("c") && ctrl => {
                                let count = selection_state.copy_to_clipboard(notebook.current_page());
                                if count > 0 {
                                    let as_image = clipboard.copy_selection(notebook.current_page(), &selection_state.selected_ids, &storage_dir);
                                    clipboard.note_internal_copy(&storage_dir);
                                    ui_state.set_export_message(copy_message(count, as_image));
                                    window.request_redraw();
                                }
                            }

                            // Einfügen: Strg + V
                            Key::Character(c) if c.eq_ignore_ascii_case("v") && ctrl => {
                                paste(
                                    None, &mut clipboard, &storage_dir, &mut canvas, &mut notebook, &mut manifest,
                                    &mut autosave, &mut selection_state, &mut ui_state,
                                );
                                update_window_title(&window, &notebook, &manifest, ui_state.active_tool, autosave.status());
                                window.request_redraw();
                            }

                            // Lineal ein-/ausblenden: L
                            Key::Character(c) if c.eq_ignore_ascii_case("l") && !ctrl => {
                                ui_state.toggle_ruler();
                                canvas.ruler = ui_state.ruler;
                                window.request_redraw();
                            }

                            // Bild einfügen: Strg + I
                            Key::Character(c) if c.eq_ignore_ascii_case("i") && ctrl => {
                                ui_state.open_image_picker();
                                window.request_redraw();
                            }

                            // Löschen: Entf oder Backspace
                            Key::Named(NamedKey::Delete) | Key::Named(NamedKey::Backspace) => {
                                if !selection_state.is_empty() {
                                    let removed = selection_state.delete_selected(notebook.current_page_mut());
                                    if !removed.is_empty() {
                                        canvas.sync_page(notebook.current_page_mut());
                                        autosave.mark_dirty(&notebook, &mut manifest);
                                        ui_state.set_export_message(t!("{count} Objekt(e) gelöscht", count = removed.len()));
                                        window.request_redraw();
                                    }
                                }
                            }

                            // Jetzt speichern (Disk Flush): Strg + S
                            Key::Character(c) if c.eq_ignore_ascii_case("s") && ctrl => {
                                match autosave.flush() {
                                    Ok(()) => {
                                        ui_state.set_export_message(t!("Gespeichert"));
                                    }
                                    Err(err) => {
                                        ui_state.set_export_message(t!(
                                            "Speicherfehler: {messages}",
                                            messages = err.messages.join(", ")
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
                                start_pdf_export(&notebook, &storage_dir, &manifest.title, ui_state.pattern_color, export_tx.clone(), window.clone());
                                ui_state.set_export_message(t!("PDF-Export läuft…"));
                                window.request_redraw();
                            }


                            // Radierer umschalten: E
                            Key::Character(c) if c.eq_ignore_ascii_case("e") && !ctrl => {
                                ui_state.active_tool = match ui_state.active_tool {
                                    ActiveTool::Pen | ActiveTool::Highlighter | ActiveTool::Shape | ActiveTool::Select | ActiveTool::Text | ActiveTool::Space => ActiveTool::Eraser,
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
                                        ui_state.set_export_message(t!("Auswahl auf Schwarz umgefärbt").to_string());
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
                                        ui_state.set_export_message(t!("Auswahl auf Blau umgefärbt").to_string());
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
                                        ui_state.set_export_message(t!("Auswahl um 90° gedreht").to_string());
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
                            // View only: never append a new page at the end
                            Key::Named(NamedKey::PageDown)
                                if ui_state.view_mode && notebook.current_page_index + 1 >= notebook.pages.len() => {}
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

                            // Text-Werkzeug umschalten: T
                            Key::Character(c) if c.eq_ignore_ascii_case("t") && !ctrl => {
                                ui_state.active_tool = if ui_state.active_tool == ActiveTool::Text {
                                    ActiveTool::Pen
                                } else {
                                    ActiveTool::Text
                                };
                                ui_state.is_pen_menu_open = false;
                                ui_state.is_highlighter_menu_open = false;
                                update_window_title(&window, &notebook, &manifest, ui_state.active_tool, autosave.status());
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
                                finish_text_edit(&mut canvas, &mut notebook, &mut manifest, &mut autosave, &mut ui_state);
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
                                // Palm-Rejection: Keine Mausevents verarbeiten, wenn der Stift aktiv
                                // zeichnet oder auswählt (manche Systeme melden ihn zusätzlich als Maus)
                                if is_stylus_drawing || is_stylus_selecting || stylus_pan_last.is_some() {
                                    return;
                                }

                                // Event-Isolation: Klicks auf die egui-Toolbar NICHT als Strich zeichnen
                                if is_pointer_in_ui {
                                    return;
                                }

                                fling.stop();
                                // 1. Pan via mittlere Maustaste oder Leertaste + Linksklick
                                // In view-only mode the left and right buttons move the page too (Issue #35)
                                if button == MouseButton::Middle
                                    || (button == MouseButton::Left && is_space_pressed)
                                    || (ui_state.view_mode && matches!(button, MouseButton::Left | MouseButton::Right))
                                {
                                    is_panning = true;
                                    pan_last_screen_pos = cursor_screen_pos;
                                    return;
                                }

                                focus_page_at!(cursor_screen_pos);
                                if is_eraser_action {
                                    is_erasing = true;
                                    let (sx, sy) = cursor_screen_pos;
                                    if let Some(page_pt) = canvas.screen_to_page(sx, sy) {
                                        if notebook.erase_at(page_pt, eraser_radius, stroke_max_radius, ui_state.eraser_mode) {
                                            canvas.sync_page(notebook.current_page_mut());
                                            autosave.mark_dirty(&notebook, &mut manifest);
                                            update_window_title(&window, &notebook, &manifest, ui_state.active_tool, autosave.status());
                                            window.request_redraw();
                                        }
                                    }
                                } else if button == MouseButton::Left {
                                    let (sx, sy) = cursor_screen_pos;

                                    // Platz schaffen: Ansatzhöhe merken
                                    if ui_state.active_tool == ActiveTool::Space {
                                        space_press((sx, sy), &canvas, &notebook, &mut ui_state);
                                        window.request_redraw();
                                        return;
                                    }

                                    // Text-Werkzeug: Textfeld öffnen
                                    if ui_state.active_tool == ActiveTool::Text {
                                        if let Some(page_pt) = canvas.screen_to_page(sx, sy) {
                                            text_tool_click(page_pt, &mut canvas, &mut notebook, &mut manifest, &mut autosave, &mut ui_state);
                                            update_window_title(&window, &notebook, &manifest, ui_state.active_tool, autosave.status());
                                            window.request_redraw();
                                        }
                                        return;
                                    }

                                    // 2. Objektauswahl & Transformation im Select-Modus
                                    if ui_state.active_tool == ActiveTool::Select {
                                        if selection_press((sx, sy), modifiers.shift_key(), &canvas, notebook.current_page(), egui_ctx.pixels_per_point(), &mut selection_state) {
                                            window.request_redraw();
                                        }
                                        return;
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
                                                    ui_state.set_export_message(t!("{shape_name} ausgerichtet! (Undo mit Strg+Z)", shape_name = shape_name));
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
                                if button == MouseButton::Middle
                                    || (matches!(button, MouseButton::Left | MouseButton::Right) && is_panning)
                                {
                                    is_panning = false;
                                }
                                if is_erasing {
                                    is_erasing = false;
                                }
                                if ui_state.space_drag.is_some() && !is_stylus_spacing && !is_touch_spacing {
                                    space_release(&mut canvas, &mut notebook, &mut manifest, &mut autosave, &storage_dir, &mut ui_state);
                                    window.request_redraw();
                                }
                                if ui_state.active_tool == ActiveTool::Select
                                    && !is_stylus_selecting
                                    && selection_release(modifiers.shift_key(), &mut notebook, &mut manifest, &mut autosave, &mut selection_state, &mut canvas)
                                {
                                    window.request_redraw();
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
                        if !is_stylus_drawing && !is_stylus_selecting {
                            if ui_state.space_drag.is_some() && !is_stylus_spacing && !is_touch_spacing {
                                space_move((sx, sy), &canvas, &mut ui_state);
                                window.request_redraw();
                            } else if ui_state.active_tool == ActiveTool::Select && selection_state.drag_mode != DragMode::None {
                                if let Some(page_pt) = canvas.screen_to_page(sx, sy) {
                                    selection_state.pending_drag = Some(page_pt);
                                    window.request_redraw();
                                }
                            } else if is_erasing {
                                focus_page_at!((sx, sy));
                                if let Some(page_pt) = canvas.screen_to_page(sx, sy) {
                                    if notebook.erase_at(page_pt, eraser_radius, stroke_max_radius, ui_state.eraser_mode) {
                                        canvas.sync_page(notebook.current_page_mut());
                                        autosave.mark_dirty(&notebook, &mut manifest);
                                        update_window_title(&window, &notebook, &manifest, ui_state.active_tool, autosave.status());
                                        window.request_redraw();
                                    }
                                }
                            } else if is_drawing && is_mouse_drawing {
                                // Draw-and-Hold Bewegungsmessung: Bei Bewegung > 3 px Timer zurücksetzen
                                let dx = sx - stroke_hold_anchor_pos.0;
                                let dy = sy - stroke_hold_anchor_pos.1;
                                if (dx * dx + dy * dy).sqrt() > hold_tolerance(&window) {
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
                        fling.stop();
                        if is_pointer_in_ui {
                            return;
                        }
                        // Durchgehende Ansicht und endlose Fläche: Mausrad scrollt (mit Umschalt
                        // waagerecht), Strg + Mausrad zoomt
                        if (ui_state.continuous_scroll || ui_state.endless) && !modifiers.control_key() {
                            let (mut dx, mut dy) = match delta {
                                MouseScrollDelta::LineDelta(x, y) => {
                                    let line = 120.0 * window.scale_factor() as f32;
                                    (x * line, y * line)
                                }
                                MouseScrollDelta::PixelDelta(pos) => (pos.x as f32, pos.y as f32),
                            };
                            if modifiers.shift_key() && dx == 0.0 {
                                (dx, dy) = (dy, 0.0);
                            }
                            canvas.pan(dx, dy);
                            window.request_redraw();
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
                        let pressure =
                            raw_pressure.map_or(0.5, |p| ui_state.pressure_curve.apply(p));

                        let is_eraser_action = ui_state.active_tool == ActiveTool::Eraser;

                        // Hardware-Palm-Rejection: Wenn der Stylus in Proximity ist oder zeichnet, Touch ignorieren
                        if is_stylus_drawing || is_stylus_in_proximity {
                            if debug_fling && touch.phase != TouchPhase::Moved {
                                eprintln!("[Nachgleiten] Finger ignoriert ({:?}): Stift in der Nähe", touch.phase);
                            }
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
                                // Jede neue Berührung hält ein gleitendes Blatt an
                                fling.stop();
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

                                // Langes Drücken: nur beim Verschieben oder Zeichnen mit dem Finger, nicht
                                // beim Radieren, Auswählen oder Text (dort hält man auch mal still)
                                if active_touches.len() == 1
                                    && !is_pinch_gesturing
                                    && !ui_state.view_mode
                                    && (!ui_state.touch_draw_enabled
                                        || ui_state.active_tool.is_drawing()
                                        || ui_state.active_tool == ActiveTool::Shape)
                                {
                                    long_press.start(touch.id, (tx, ty), Instant::now());
                                } else {
                                    long_press.cancel();
                                }

                                if active_touches.len() >= 2 {
                                    // Multi-Touch Geste: Sobald 2+ Finger aufliegen, Inking sofort abbrechen und zoomen/pannen
                                    if is_touch_drawing {
                                        is_touch_drawing = false;
                                        is_drawing = false;
                                        canvas.cancel_stroke();
                                        window.request_redraw();
                                    }
                                    is_touch_panning = false;
                                    if is_touch_spacing {
                                        is_touch_spacing = false;
                                        ui_state.space_drag = None;
                                    }
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

                                    // Ohne Finger-Zeichnen verschiebt ein Finger das Blatt – außer bei
                                    // Werkzeugen, die nicht zeichnen: Text und Platz schaffen nehmen den
                                    // Finger immer, Auswählen, wenn er ein Objekt oder die Auswahl trifft
                                    // (Issue #26)
                                    let finger_uses_tool = match ui_state.active_tool {
                                        _ if ui_state.view_mode => false,
                                        ActiveTool::Text | ActiveTool::Space => true,
                                        ActiveTool::Select => {
                                            focus_page_at!((tx, ty));
                                            finger_grabs_selection(
                                                (tx, ty),
                                                &canvas,
                                                notebook.current_page(),
                                                egui_ctx.pixels_per_point(),
                                                &selection_state,
                                            )
                                        }
                                        _ => false,
                                    };
                                    if finger_uses_tool {
                                        // Halten soll hier kein Kontextmenü öffnen
                                        long_press.cancel();
                                    }
                                    if (!ui_state.touch_draw_enabled || ui_state.view_mode) && !finger_uses_tool {
                                        // Standard: 1 Finger scrollt / verschiebt den Canvas (Pan)
                                        is_touch_panning = true;
                                        touch_pan_last_pos = (tx, ty);
                                    } else {
                                        // Modus Finger-Zeichnen aktiv
                                        focus_page_at!((tx, ty));
                                        if is_eraser_action {
                                            is_erasing = true;
                                            if let Some(page_pt) = canvas.screen_to_page(tx, ty) {
                                                if notebook.erase_at(page_pt, eraser_radius, stroke_max_radius, ui_state.eraser_mode) {
                                                    canvas.sync_page(notebook.current_page_mut());
                                                    autosave.mark_dirty(&notebook, &mut manifest);
                                                    update_window_title(&window, &notebook, &manifest, ui_state.active_tool, autosave.status());
                                                    window.request_redraw();
                                                }
                                            }
                                        } else if ui_state.active_tool == ActiveTool::Text {
                                            if let Some(page_pt) = canvas.screen_to_page(tx, ty) {
                                                text_tool_click(page_pt, &mut canvas, &mut notebook, &mut manifest, &mut autosave, &mut ui_state);
                                                update_window_title(&window, &notebook, &manifest, ui_state.active_tool, autosave.status());
                                                window.request_redraw();
                                            }
                                        } else if ui_state.active_tool == ActiveTool::Select {
                                            if selection_press((tx, ty), false, &canvas, notebook.current_page(), egui_ctx.pixels_per_point(), &mut selection_state) {
                                                window.request_redraw();
                                            }
                                        } else if ui_state.active_tool == ActiveTool::Space {
                                            is_touch_spacing = space_press((tx, ty), &canvas, &notebook, &mut ui_state);
                                            window.request_redraw();
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
                                long_press.moved(touch.id, (tx, ty), window.scale_factor() as f32);

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
                                        fling.track(Vec2::new(dx, dy), Instant::now());
                                        window.request_redraw();
                                    } else if is_touch_drawing {
                                        current_pressure = pressure;
                                        cursor_screen_pos = (tx, ty);

                                        let dx = tx - stroke_hold_anchor_pos.0;
                                        let dy = ty - stroke_hold_anchor_pos.1;
                                        if (dx * dx + dy * dy).sqrt() > hold_tolerance(&window) {
                                            stroke_last_motion_time = Instant::now();
                                            stroke_hold_anchor_pos = (tx, ty);
                                        }

                                        if !stroke_already_snapped {
                                            let page_pt = canvas.screen_to_page_clamped(tx, ty);
                                            canvas.extend_stroke(page_pt.x, page_pt.y, pressure, now);
                                            window.request_redraw();
                                        }
                                    } else if is_touch_spacing {
                                        space_move((tx, ty), &canvas, &mut ui_state);
                                        window.request_redraw();
                                    } else if is_erasing {
                                        focus_page_at!((tx, ty));
                                        if let Some(page_pt) = canvas.screen_to_page(tx, ty) {
                                            if notebook.erase_at(page_pt, eraser_radius, stroke_max_radius, ui_state.eraser_mode) {
                                                canvas.sync_page(notebook.current_page_mut());
                                                autosave.mark_dirty(&notebook, &mut manifest);
                                                update_window_title(&window, &notebook, &manifest, ui_state.active_tool, autosave.status());
                                                window.request_redraw();
                                            }
                                        }
                                    } else if ui_state.active_tool == ActiveTool::Select && selection_state.drag_mode != DragMode::None {
                                        if let Some(page_pt) = canvas.screen_to_page(tx, ty) {
                                            selection_state.pending_drag = Some(page_pt);
                                            window.request_redraw();
                                        }
                                    }
                                }
                            }
                            TouchPhase::Ended | TouchPhase::Cancelled => {
                                active_touches.remove(&touch.id);
                                long_press.cancel();
                                if debug_fling {
                                    eprintln!(
                                        "[Nachgleiten] Finger abgehoben ({:?}), verschiebt: {}, weitere Finger: {}",
                                        touch.phase,
                                        is_touch_panning,
                                        active_touches.len()
                                    );
                                }

                                if active_touches.is_empty() {
                                    is_pinch_gesturing = false;
                                    pinch_prev_distance = None;
                                    pinch_prev_center = None;
                                    if is_touch_panning && touch.phase == TouchPhase::Ended {
                                        // Letztes Stück bis zum Abheben noch mitnehmen
                                        let (dx, dy) = (tx - touch_pan_last_pos.0, ty - touch_pan_last_pos.1);
                                        canvas.pan(dx, dy);
                                        fling.track(Vec2::new(dx, dy), Instant::now());
                                        let velocity = fling.release(Instant::now());
                                        if debug_fling {
                                            eprintln!(
                                                "[Nachgleiten] {:.0} px/s beim Abheben, gleitet: {}",
                                                velocity.length(),
                                                fling.is_active()
                                            );
                                        }
                                        window.request_redraw();
                                    }
                                    is_touch_panning = false;

                                    if is_erasing {
                                        is_erasing = false;
                                    }

                                    if is_touch_spacing {
                                        is_touch_spacing = false;
                                        space_release(&mut canvas, &mut notebook, &mut manifest, &mut autosave, &storage_dir, &mut ui_state);
                                        window.request_redraw();
                                    }

                                    if ui_state.active_tool == ActiveTool::Select
                                        && selection_release(false, &mut notebook, &mut manifest, &mut autosave, &mut selection_state, &mut canvas)
                                    {
                                        window.request_redraw();
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
                        flush_selection_drag(notebook.current_page_mut(), &mut canvas, &mut selection_state);
                        // 0. Durchgehende Ansicht: Nach dem Scrollen oder Zoomen wird die Seite in
                        //    der Fenstermitte zur aktuellen (nicht während eines Strichs, einer
                        //    Auswahl oder beim Ziehen des Lineals). Wer auf einer Nachbarseite
                        //    schreibt, bleibt dort, bis wieder gescrollt wird (Undo betrifft sie).
                        canvas.set_fit_box(notebook.fit_box());
                        if notebook.is_endless() {
                            if canvas.clamp_endless(notebook.current_page().endless_scroll_limit()) {
                                fling.stop();
                            }
                        } else if ui_state.continuous_scroll {
                            let stack = notebook.page_stack();
                            if canvas.clamp_scroll(&stack) {
                                // Am Anfang oder Ende des Notizbuchs angekommen
                                fling.stop();
                            }
                            // Unabhängig von der Bezugsseite: Lage bezogen auf die erste Seite
                            let scroll = [
                                canvas.camera.zoom,
                                canvas.camera.pan_offset[0],
                                canvas.camera.pan_offset[1] + stack.origins[0].y,
                            ];
                            let scrolled = last_scroll
                                .is_none_or(|last: [f32; 3]| last.iter().zip(scroll).any(|(a, b)| (a - b).abs() > 1e-3));
                            let busy = is_drawing
                                || is_erasing
                                || ui_state.space_drag.is_some()
                                || !selection_state.is_empty()
                                || selection_state.drag_mode != DragMode::None
                                || ui_state.ruler_drag.is_some()
                                || ui_state.text_edit.is_some();
                            if scrolled && !busy {
                                last_scroll = Some(scroll);
                                let target = canvas
                                    .page_at_view_center(&stack)
                                    .filter(|&i| i != notebook.current_page_index);
                                if let Some(index) = target {
                                    switch_to_page(
                                        index,
                                        true,
                                        &mut canvas,
                                        &mut notebook,
                                        &mut manifest,
                                        &mut autosave,
                                        &storage_dir,
                                        &mut selection_state,
                                        &mut ui_state,
                                        &window,
                                    );
                                }
                            }
                        }

                        // 1. egui Eingabe erfassen und Frame starten
                        let mut raw_input = egui_winit_state.take_egui_input(&window);
                        osk::plain_ime_commits_to_text(&mut raw_input.events, &mut ime_composing);
                        if let Some(requester) = keyboard_requester.as_mut() {
                            raw_input.events.extend(requester.take_deletions());
                        }
                        egui_ctx.begin_frame(raw_input);

                        // 2. Auswahl-Overlay rendern (Bounding Box, Griffe, Lasso)
                        render_selection_overlay(&egui_ctx, &selection_state, notebook.current_page(), &canvas);

                        // 3. Toolbar & Widgets rendern
                        updater.update();
                        ui_state.page_view = Some(page_view(&canvas, window.scale_factor() as f32));
                        ui_state.selection_aspect_lock = (!selection_state.is_empty())
                            .then(|| selection_state.keeps_aspect(notebook.current_page()));
                        ui_state.restorable_page = last_deleted_page
                            .as_ref()
                            .filter(|(id, _, _)| *id == manifest.id)
                            .map(|(_, index, _)| index + 1);
                        let actions = ui::render_ui(&egui_ctx, &mut ui_state, &notebook, autosave.status(), &updater);
                        canvas.ruler = ui_state.ruler;

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
                                    activate_tool(tool, &mut ui_state, &mut canvas);
                                    update_window_title(&window, &notebook, &manifest, tool, autosave.status());
                                    window.request_redraw();
                                }
                                UiAction::SetViewMode(on) => {
                                    ui_state.view_mode = on;
                                    if on {
                                        // Nothing may stay half-edited while only viewing
                                        finish_text_edit(&mut canvas, &mut notebook, &mut manifest, &mut autosave, &mut ui_state);
                                        selection_state.clear();
                                        ui_state.ruler = None;
                                        ui_state.ruler_drag = None;
                                        canvas.ruler = None;
                                        ui_state.set_export_message(t!("Nur ansehen – 👁 erneut tippen, um zu schreiben"));
                                    }
                                    update_window_title(&window, &notebook, &manifest, ui_state.active_tool, autosave.status());
                                    window.request_redraw();
                                }
                                UiAction::SetColor(color) => {
                                    if !selection_state.is_empty()
                                        && selection_state.recolor_selected(notebook.current_page_mut(), color) {
                                            canvas.sync_page(notebook.current_page_mut());
                                            autosave.mark_dirty(&notebook, &mut manifest);
                                            ui_state.set_export_message(t!("Auswahl umgefärbt").to_string());
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
                                        ActiveTool::Eraser | ActiveTool::Select | ActiveTool::Text | ActiveTool::Space => {}
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
                                        ActiveTool::Eraser | ActiveTool::Select | ActiveTool::Text | ActiveTool::Space => {}
                                    }
                                    window.request_redraw();
                                }
                                UiAction::DeleteSelected => {
                                    if !selection_state.is_empty() {
                                        let removed = selection_state.delete_selected(notebook.current_page_mut());
                                        if !removed.is_empty() {
                                            canvas.sync_page(notebook.current_page_mut());
                                            autosave.mark_dirty(&notebook, &mut manifest);
                                            ui_state.set_export_message(t!("{count} Objekt(e) gelöscht", count = removed.len()));
                                            window.request_redraw();
                                        }
                                    }
                                }
                                UiAction::CopySelected => {
                                    let count = selection_state.copy_to_clipboard(notebook.current_page());
                                    if count > 0 {
                                        let as_image = clipboard.copy_selection(notebook.current_page(), &selection_state.selected_ids, &storage_dir);
                                        clipboard.note_internal_copy(&storage_dir);
                                        ui_state.set_export_message(copy_message(count, as_image));
                                        window.request_redraw();
                                    }
                                }
                                UiAction::PasteSelected | UiAction::PasteAt(_) => {
                                    let at = match action {
                                        UiAction::PasteAt(at) => Some(at),
                                        _ => None,
                                    };
                                    paste(
                                        at, &mut clipboard, &storage_dir, &mut canvas, &mut notebook, &mut manifest,
                                        &mut autosave, &mut selection_state, &mut ui_state,
                                    );
                                    update_window_title(&window, &notebook, &manifest, ui_state.active_tool, autosave.status());
                                    window.request_redraw();
                                }
                                UiAction::InsertImageFile(path) => {
                                    let center = visible_page_center(&canvas);
                                    insert_image_file(
                                        &path, center, &storage_dir, &mut canvas, &mut notebook, &mut manifest,
                                        &mut autosave, &mut selection_state, &mut ui_state,
                                    );
                                    update_window_title(&window, &notebook, &manifest, ui_state.active_tool, autosave.status());
                                    window.request_redraw();
                                }
                                UiAction::ToggleAspectLock => {
                                    if !selection_state.is_empty() {
                                        selection_state.toggle_aspect_lock(notebook.current_page());
                                        window.request_redraw();
                                    }
                                }
                                UiAction::RotateSelected => {
                                    if !selection_state.is_empty()
                                        && selection_state.rotate_selected_90_deg(notebook.current_page_mut()) {
                                            canvas.sync_page(notebook.current_page_mut());
                                            autosave.mark_dirty(&notebook, &mut manifest);
                                            ui_state.set_export_message(t!("Auswahl um 90° gedreht").to_string());
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
                                    finish_text_edit(&mut canvas, &mut notebook, &mut manifest, &mut autosave, &mut ui_state);
                                    if let Some(changed) = notebook.undo() {
                                        selection_state.clear();
                                        canvas.sync_page(notebook.current_page_mut());
                                        persist_pages(&changed, &notebook, &mut manifest, &mut autosave);
                                        update_window_title(&window, &notebook, &manifest, ui_state.active_tool, autosave.status());
                                        window.request_redraw();
                                    }
                                }
                                UiAction::Redo => {
                                    finish_text_edit(&mut canvas, &mut notebook, &mut manifest, &mut autosave, &mut ui_state);
                                    if let Some(changed) = notebook.redo() {
                                        selection_state.clear();
                                        canvas.sync_page(notebook.current_page_mut());
                                        persist_pages(&changed, &notebook, &mut manifest, &mut autosave);
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
                                    manifest.sync_from_notebook(&notebook);
                                    autosave.mark_manifest_dirty(&manifest);
                                    autosave.mark_dirty(&notebook, &mut manifest);
                                    update_window_title(&window, &notebook, &manifest, ui_state.active_tool, autosave.status());
                                    window.request_redraw();
                                }
                                UiAction::ExportPage(kind) => {
                                    start_page_export(kind, &notebook, &storage_dir, &manifest.title, ui_state.pattern_color, export_tx.clone(), window.clone());
                                    ui_state.set_export_message(t!("Seite wird exportiert …").to_string());
                                    window.request_redraw();
                                }
                                UiAction::SetPageTitle { page_id, title } => {
                                    if notebook.set_page_title(page_id, &title) {
                                        if let Some(page) = notebook.pages.iter().find(|p| p.id == page_id) {
                                            manifest.sync_from_notebook(&notebook);
                                            autosave.mark_page_dirty(page, &manifest);
                                        }
                                        update_window_title(&window, &notebook, &manifest, ui_state.active_tool, autosave.status());
                                    }
                                }
                                // Die endlose Fläche bleibt eine einzige Seite
                                UiAction::AddPage if notebook.is_endless() => {}
                                UiAction::AddPage => {
                                    // Leere Seite direkt nach der aktuellen (in PDFs: nach der Folie,
                                    // auf der man gerade ist) und dorthin wechseln
                                    let bg = notebook.current_page().background;
                                    let index = notebook.insert_page_after_current(bg);
                                    manifest.sync_from_notebook(&notebook);
                                    switch_to_page(
                                        index,
                                        false,
                                        &mut canvas,
                                        &mut notebook,
                                        &mut manifest,
                                        &mut autosave,
                                        &storage_dir,
                                        &mut selection_state,
                                        &mut ui_state,
                                        &window,
                                    );
                                    ui_state.set_export_message(t!("Seite {number} eingefügt", number = index + 1));
                                }
                                UiAction::DeleteCurrentPage => {
                                    let index = notebook.current_page_index;
                                    if let Some(page) = notebook.remove_current_page() {
                                        selection_state.clear();
                                        let curr = notebook.current_page_index;
                                        let _ = storage::ensure_page_loaded(&storage_dir, &mut notebook, curr);
                                        canvas.sync_page(notebook.current_page_mut());
                                        manifest.sync_from_notebook(&notebook);
                                        autosave.mark_manifest_dirty(&manifest);
                                        last_deleted_page = Some((manifest.id.clone(), index, page));
                                        ui_state.set_export_message(t!(
                                            "Seite {number} gelöscht – im Menü ☰ wiederherstellbar",
                                            number = index + 1
                                        ));
                                        update_window_title(&window, &notebook, &manifest, ui_state.active_tool, autosave.status());
                                        window.request_redraw();
                                    }
                                }
                                UiAction::RestoreDeletedPage => {
                                    if let Some((id, index, page)) = last_deleted_page.take() {
                                        if id == manifest.id {
                                            selection_state.clear();
                                            let index = notebook.restore_page(index, page);
                                            canvas.sync_page(notebook.current_page_mut());
                                            manifest.sync_from_notebook(&notebook);
                                            // Die Seitendatei neu schreiben (sie kann inzwischen fehlen)
                                            autosave.mark_page_dirty(notebook.current_page(), &manifest);
                                            autosave.mark_manifest_dirty(&manifest);
                                            ui_state.set_export_message(t!("Seite {number} wiederhergestellt", number = index + 1));
                                            update_window_title(&window, &notebook, &manifest, ui_state.active_tool, autosave.status());
                                            window.request_redraw();
                                        } else {
                                            last_deleted_page = Some((id, index, page));
                                        }
                                    }
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
                                    start_pdf_export(&notebook, &storage_dir, &manifest.title, ui_state.pattern_color, export_tx.clone(), window.clone());
                                    ui_state.set_export_message(t!("PDF-Export läuft…"));
                                    window.request_redraw();
                                }

                                UiAction::CreateFolder { parent, name } => {
                                    if let Some(path) = change_library(&storage_base_dir, &mut ui_state, |library| {
                                        library.create_folder(&parent, &name)
                                    }) {
                                        ui_state.set_export_message(t!(
                                            "Ordner „{name}“ angelegt",
                                            name = ink_notes::library::folder_name(&path)
                                        ));
                                    }
                                    window.request_redraw();
                                }
                                UiAction::RenameFolder { path, new_name } => {
                                    if let Some(new_path) = change_library(&storage_base_dir, &mut ui_state, |library| {
                                        library.rename_folder(&path, &new_name)
                                    }) {
                                        // Der gezeigte Ordner kann darin liegen
                                        let shown = &ui_state.notebook_folder;
                                        if shown == &path || shown.starts_with(&format!("{path}/")) {
                                            ui_state.notebook_folder = format!("{new_path}{}", &shown[path.len()..]);
                                        }
                                    }
                                    window.request_redraw();
                                }
                                UiAction::DeleteFolder(path) => {
                                    if change_library(&storage_base_dir, &mut ui_state, |library| {
                                        library.delete_folder(&path);
                                        Ok(())
                                    })
                                    .is_some()
                                    {
                                        ui_state.set_export_message(t!(
                                            "Ordner „{name}“ gelöscht, sein Inhalt liegt eine Ebene höher",
                                            name = ink_notes::library::folder_name(&path)
                                        ));
                                    }
                                    window.request_redraw();
                                }
                                UiAction::MoveFolder { path, new_parent } => {
                                    if let Some(new_path) = change_library(&storage_base_dir, &mut ui_state, |library| {
                                        library.move_folder(&path, &new_parent)
                                    }) {
                                        // Der gezeigte Ordner kann darin liegen
                                        let shown = &ui_state.notebook_folder;
                                        if shown == &path || shown.starts_with(&format!("{path}/")) {
                                            ui_state.notebook_folder = format!("{new_path}{}", &shown[path.len()..]);
                                        }
                                        let target = if new_parent.is_empty() {
                                            t!("die oberste Ebene").to_string()
                                        } else {
                                            format!("„{}“", ink_notes::library::folder_name(&new_parent))
                                        };
                                        ui_state.set_export_message(t!(
                                            "Ordner „{name}“ verschoben in {target}",
                                            name = ink_notes::library::folder_name(&path),
                                            target = target
                                        ));
                                    }
                                    window.request_redraw();
                                }
                                UiAction::MoveNotebook { id, folder } => {
                                    if change_library(&storage_base_dir, &mut ui_state, |library| {
                                        library.move_notebook(&id, &folder)
                                    })
                                    .is_some()
                                    {
                                        let target = if folder.is_empty() {
                                            t!("die oberste Ebene").to_string()
                                        } else {
                                            format!("„{}“", ink_notes::library::folder_name(&folder))
                                        };
                                        ui_state.set_export_message(t!("Verschoben in {target}", target = target));
                                    }
                                    window.request_redraw();
                                }
                                UiAction::RefreshNotebooks => {
                                    refresh_notebooks(&storage_base_dir, &mut ui_state);
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
                                                        ui_state.set_export_message(t!("⚠ In anderer Instanz geöffnet (Schreibschutz)"));
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
                                                refresh_notebooks(&storage_base_dir, &mut ui_state);
                                                ui_state.is_document_menu_open = false;
                                                canvas.sync_page(notebook.current_page_mut());
                                                update_window_title(&window, &notebook, &manifest, ui_state.active_tool, autosave.status());
                                                if _notebook_lock.is_some() {
                                                    ui_state.set_export_message(t!("Dokument '{title}' geöffnet", title = manifest.title));
                                                }
                                                window.request_redraw();
                                            }
                                            Err(err) => {
                                                eprintln!("[Dokument-Fehler] Konnte Dokument '{id}' nicht laden: {err}");
                                                ui_state.set_export_message(t!("Fehler beim Öffnen: {err}", err = err));
                                            }
                                        }
                                    }
                                }
                                UiAction::ImportPdf(path) => {
                                    // Neben das gerade geöffnete Notizbuch
                                    let folder = ui_state.library.folder_of(&manifest.id).to_string();
                                    let title = path
                                        .file_stem()
                                        .map(|s| s.to_string_lossy().into_owned())
                                        .unwrap_or_else(|| "PDF".to_string());
                                    let result = std::fs::read(&path)
                                        .map_err(|e| e.to_string())
                                        .and_then(|bytes| storage::create_notebook_from_pdf(&storage_base_dir, &title, bytes));
                                    match result {
                                        Ok((new_nb, new_manifest, new_dir)) => {
                                            let id = new_manifest.id.clone();
                                            change_library(&storage_base_dir, &mut ui_state, |library| {
                                                library.move_notebook(&id, &folder)
                                            });
                                            let _ = autosave.flush();
                                            _notebook_lock = storage::NotebookLock::acquire(&new_dir).ok();
                                            autosave.set_read_only(_notebook_lock.is_none());
                                            autosave.switch_notebook_dir(new_dir.clone());
                                            notebook = new_nb;
                                            manifest = new_manifest;
                                            storage_dir = new_dir;
                                            selection_state.clear();
                                            ui_state.current_notebook_id = manifest.id.clone();
                                            ui_state.document_title = manifest.title.clone();
                                            refresh_notebooks(&storage_base_dir, &mut ui_state);
                                            canvas.reset_view();
                                            canvas.sync_page(notebook.current_page_mut());
                                            update_window_title(&window, &notebook, &manifest, ui_state.active_tool, autosave.status());
                                            ui_state.set_export_message(t!(
                                                "'{title}' importiert ({count} Seiten)",
                                                title = manifest.title,
                                                count = notebook.pages.len()
                                            ));
                                        }
                                        Err(err) => {
                                            eprintln!("[PDF-Import] '{}': {err}", path.display());
                                            ui_state.set_export_message(t!("PDF nicht importiert: {err}", err = err));
                                        }
                                    }
                                    window.request_redraw();
                                }
                                UiAction::CreateNotebook { title, folder, layout } => {
                                    let _ = autosave.flush();
                                    match storage::create_new_notebook(&storage_base_dir, &title) {
                                        Ok((mut new_nb, mut new_manifest, new_dir)) => {
                                            let id = new_manifest.id.clone();
                                            new_manifest.layout = Some(layout);
                                            if layout == NotebookLayout::Endless {
                                                new_nb.make_endless();
                                            }
                                            change_library(&storage_base_dir, &mut ui_state, |library| {
                                                library.move_notebook(&id, &folder)
                                            });
                                            _notebook_lock = storage::NotebookLock::acquire(&new_dir).ok();
                                            autosave.set_read_only(_notebook_lock.is_none());
                                            autosave.switch_notebook_dir(new_dir.clone());
                                            notebook = new_nb;
                                            manifest = new_manifest;
                                            storage_dir = new_dir;
                                            autosave.mark_dirty(&notebook, &mut manifest);
                                            canvas.reset_view();
                                            selection_state.clear();
                                            ui_state.current_notebook_id = manifest.id.clone();
                                            ui_state.document_title = manifest.title.clone();
                                            refresh_notebooks(&storage_base_dir, &mut ui_state);
                                            ui_state.is_document_menu_open = false;
                                            canvas.sync_page(notebook.current_page_mut());
                                            update_window_title(&window, &notebook, &manifest, ui_state.active_tool, autosave.status());
                                            ui_state.set_export_message(t!("Dokument '{title}' erstellt", title = manifest.title));
                                            window.request_redraw();
                                        }
                                        Err(err) => {
                                            eprintln!("[Dokument-Fehler] Konnte Dokument '{title}' nicht erstellen: {err}");
                                            ui_state.set_export_message(t!("Fehler beim Erstellen: {err}", err = err));
                                        }
                                    }
                                }
                                UiAction::RenameNotebook { id, new_title } => {
                                    let target_dir = storage_base_dir.join(&id);
                                    if id == manifest.id {
                                        match storage::rename_notebook(&target_dir, &mut manifest, &new_title) {
                                            Ok(()) => {
                                                ui_state.document_title = manifest.title.clone();
                                                refresh_notebooks(&storage_base_dir, &mut ui_state);
                                                manifest.sync_from_notebook(&notebook);
                                                autosave.mark_manifest_dirty(&manifest);
                                                update_window_title(&window, &notebook, &manifest, ui_state.active_tool, autosave.status());
                                                ui_state.set_export_message(t!("Umbenannt in '{title}'", title = new_title));
                                                window.request_redraw();
                                            }
                                            Err(err) => {
                                                eprintln!("[Dokument-Fehler] Konnte Dokument '{id}' nicht umbenennen: {err}");
                                                ui_state.set_export_message(t!("Fehler beim Umbenennen: {err}", err = err));
                                            }
                                        }
                                    } else {
                                        let manifest_path = target_dir.join("manifest.json");
                                        if let Ok(content) = std::fs::read_to_string(&manifest_path) {
                                            if let Ok(mut other_manifest) = serde_json::from_str::<NotebookManifest>(&content) {
                                                let _ = storage::rename_notebook(&target_dir, &mut other_manifest, &new_title);
                                                refresh_notebooks(&storage_base_dir, &mut ui_state);
                                                ui_state.set_export_message(t!("Umbenannt in '{title}'", title = new_title));
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
                                            refresh_notebooks(&storage_base_dir, &mut ui_state);
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
                                                            ui_state.set_export_message(t!("Gewechselt zu '{title}'", title = manifest.title));
                                                        }
                                                        Err(err) => {
                                                            eprintln!("[Dokument-Fehler] Konnte Dokument '{}' nicht laden: {err}", first.id);
                                                        }
                                                    }
                                                } else if let Ok((new_nb, new_manifest, new_dir)) = storage::create_new_notebook(&storage_base_dir, t!("Standard-Notizbuch")) {
                                                    _notebook_lock = storage::NotebookLock::acquire(&new_dir).ok();
                                                    autosave.set_read_only(_notebook_lock.is_none());
                                                    autosave.switch_notebook_dir(new_dir.clone());
                                                    notebook = new_nb;
                                                    manifest = new_manifest;
                                                    storage_dir = new_dir;
                                                    selection_state.clear();
                                                    ui_state.current_notebook_id = manifest.id.clone();
                                                    ui_state.document_title = manifest.title.clone();
                                                    refresh_notebooks(&storage_base_dir, &mut ui_state);
                                                    canvas.sync_page(notebook.current_page_mut());
                                                    update_window_title(&window, &notebook, &manifest, ui_state.active_tool, autosave.status());
                                                    ui_state.set_export_message(t!("Standard-Notizbuch erstellt").to_string());
                                                }
                                            } else {
                                                ui_state.set_export_message(t!("Dokument in Papierkorb verschoben").to_string());
                                            }
                                            window.request_redraw();
                                        }
                                        Err(err) => {
                                            eprintln!("[Dokument-Fehler] Konnte Dokument '{id}' nicht löschen: {err}");
                                            ui_state.set_export_message(t!("Fehler beim Löschen: {err}", err = err));
                                        }
                                    }
                                }
                                UiAction::UndoDeleteNotebook => {
                                    if let Some((trash_path, _title)) = ui_state.last_deleted_notebook.take() {
                                        match storage::restore_notebook_from_trash(&storage_base_dir, &trash_path) {
                                            Ok(_restored_dir) => {
                                                refresh_notebooks(&storage_base_dir, &mut ui_state);
                                                ui_state.set_export_message(t!("Notizbuch wiederhergestellt").to_string());
                                                window.request_redraw();
                                            }
                                            Err(err) => {
                                                eprintln!("[Dokument-Fehler] Konnte Notizbuch nicht wiederherstellen: {err}");
                                                ui_state.set_export_message(t!("Fehler beim Wiederherstellen: {err}", err = err));
                                            }
                                        }
                                    }
                                }
                                UiAction::RestoreNotebookBackup(id) => {
                                    let target_dir = storage_base_dir.join(&id);
                                    match storage::restore_manifest_from_backup(&target_dir) {
                                        Ok(()) => {
                                            refresh_notebooks(&storage_base_dir, &mut ui_state);
                                            ui_state.set_export_message(t!("Manifest aus Backup wiederhergestellt").to_string());
                                            window.request_redraw();
                                        }
                                        Err(err) => {
                                            eprintln!("[Dokument-Fehler] Konnte Backup für '{id}' nicht wiederherstellen: {err}");
                                            ui_state.set_export_message(t!("Wiederherstellung fehlgeschlagen: {err}", err = err));
                                        }
                                    }
                                }
                                UiAction::OpenNotebookFolder(id) => {
                                    let target_dir = storage_base_dir.join(&id);
                                    ui_state.set_export_message(t!("Ordner: {path}", path = target_dir.display()));
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
                                        ui_state.set_export_message(t!("Update-Fehler: {err}", err = err));
                                    } else {
                                        ui_state.set_export_message(t!("Update gestartet / geöffnet"));
                                    }
                                    window.request_redraw();
                                }
                                UiAction::OpenUrlInBrowser(url) => {
                                    let _ = webbrowser::open(&url);
                                }
                                UiAction::SaveUpdaterConfig { server_url, auth_token } => {
                                    updater.set_server_url(server_url);
                                    updater.set_auth_token(auth_token);
                                    ui_state.set_export_message(t!("Server-Einstellungen gespeichert"));
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
                                                se.messages.push(t!("Notfall-Export fehlgeschlagen: {err}", err = err));
                                            }
                                            window.request_redraw();
                                        }
                                    }
                                }
                                UiAction::SetSidebarSide(side) => {
                                    ui_settings.sidebar_side = side;
                                    save_ui_settings(&ui_settings, ui_settings_path.as_deref(), &mut ui_state);
                                    window.request_redraw();
                                }
                                UiAction::SetLanguage(language) => {
                                    ui_settings.language = language;
                                    i18n::set_language(language.unwrap_or_else(Language::system));
                                    save_ui_settings(&ui_settings, ui_settings_path.as_deref(), &mut ui_state);
                                    update_window_title(&window, &notebook, &manifest, ui_state.active_tool, autosave.status());
                                    window.request_redraw();
                                }
                                UiAction::SetPaperTheme { theme, adapt_ink } => {
                                    canvas.set_paper_theme(theme, adapt_ink);
                                    canvas.sync_page(notebook.current_page_mut());
                                    ui_settings.paper_theme = theme;
                                    ui_settings.adapt_ink_colors = adapt_ink;
                                    save_ui_settings(&ui_settings, ui_settings_path.as_deref(), &mut ui_state);
                                    window.request_redraw();
                                }
                                UiAction::SetPatternColor(pattern) => {
                                    canvas.set_pattern_color(pattern);
                                    ui_settings.pattern_color = pattern;
                                    save_ui_settings(&ui_settings, ui_settings_path.as_deref(), &mut ui_state);
                                    window.request_redraw();
                                }
                                UiAction::ScrollTo(y) => {
                                    fling.stop();
                                    canvas.scroll_center_to(y);
                                    window.request_redraw();
                                }
                                UiAction::SetNotebookLayout(layout) => {
                                    // Von und zur endlosen Fläche nur mit einer Seite, deren Inhalt passt
                                    let was_endless = notebook.is_endless();
                                    let to_endless = layout == NotebookLayout::Endless;
                                    let converted = match (was_endless, to_endless) {
                                        (false, true) => notebook.make_endless(),
                                        (true, false) => notebook.make_paged(),
                                        _ => true,
                                    };
                                    if !converted {
                                        ui_state.set_export_message(if to_endless {
                                            t!("Endlos geht nur bei einem Notizbuch mit einer einzigen Seite ohne PDF").to_string()
                                        } else {
                                            t!("Zurück zu Seiten geht nur, solange alles im Startausschnitt liegt").to_string()
                                        });
                                    } else {
                                        manifest.layout = Some(layout);
                                        ui_state.endless = to_endless;
                                        ui_state.continuous_scroll = layout == NotebookLayout::Continuous;
                                        if was_endless != to_endless {
                                            selection_state.clear();
                                            canvas.sync_page(notebook.current_page_mut());
                                            canvas.reset_view();
                                            autosave.mark_dirty(&notebook, &mut manifest);
                                        } else {
                                            autosave.mark_manifest_dirty(&manifest);
                                        }
                                    }
                                    window.request_redraw();
                                }
                                UiAction::SaveStylusButtons => {
                                    ui_settings.stylus_buttons = ui_state.stylus_buttons.clone();
                                    save_ui_settings(&ui_settings, ui_settings_path.as_deref(), &mut ui_state);
                                }
                                UiAction::SavePressureCurve(curve) => {
                                    ui_settings.pressure_curve = curve;
                                    save_ui_settings(&ui_settings, ui_settings_path.as_deref(), &mut ui_state);
                                }
                                UiAction::ForceExit => {
                                    println!("[Shutdown] Beenden erzwungen trotz ungespeicherter Änderungen.");
                                    target.exit();
                                }
                            }
                        }

                        // Stifte und eigene Farben: geänderte Farbe oder Stärke gehört zum gewählten
                        // Stift bzw. zur gewählten Farbe. Gespeichert wird erst nach dem Loslassen
                        // (nicht bei jedem Schritt eines Schiebereglers oder Farbwählers).
                        ui_state.sync_tool_colors();
                        if !egui_ctx.is_using_pointer() {
                            let mut updated = ui_settings.clone();
                            updated.pen_presets = ui_state.pen_presets;
                            updated.active_pen = ui_state.active_pen;
                            updated.highlighter_colors = ui_state.highlighter_colors;
                            updated.active_highlighter = ui_state.active_highlighter;
                            updated.text_colors = ui_state.text_colors;
                            updated.active_text_color = ui_state.active_text_color;
                            if updated != ui_settings {
                                ui_settings = updated;
                                save_ui_settings(&ui_settings, ui_settings_path.as_deref(), &mut ui_state);
                            }
                        }

                        // Textbearbeitung beenden: Esc, anderes Werkzeug oder andere Seite
                        let finish_text = ui_state.text_edit.as_ref().is_some_and(|edit| {
                            edit.finish
                                || ui_state.active_tool != ActiveTool::Text
                                || edit.page_id != notebook.current_page().id
                        });
                        if finish_text {
                            finish_text_edit(&mut canvas, &mut notebook, &mut manifest, &mut autosave, &mut ui_state);
                            update_window_title(&window, &notebook, &manifest, ui_state.active_tool, autosave.status());
                            window.request_redraw();
                        }

                        // 4. egui Frame beenden & Primitives tessellieren
                        let full_output = egui_ctx.end_frame();
                        let ime_active = full_output.platform_output.ime.is_some();
                        egui_winit_state.handle_platform_output(&window, full_output.platform_output);
                        if let Some(requester) = keyboard_requester.as_mut() {
                            let pressed = egui_ctx.input(|i| i.pointer.any_pressed());
                            if osk::should_request(ime_was_active, ime_active, pressed) {
                                requester.request();
                            }
                        }
                        ime_was_active = ime_active;

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
                        //    Durchgehende Ansicht: sichtbare Nachbarseiten laden und mitzeichnen
                        if ui_state.continuous_scroll {
                            let stack = notebook.page_stack();
                            let visible = canvas.visible_neighbors(&stack);
                            for &index in &visible {
                                let _ = storage::ensure_page_loaded(&storage_dir, &mut notebook, index);
                                notebook.pages[index].rebuild_cache_if_dirty();
                            }
                            let neighbors: Vec<(Vec2, &ink_notes::document::Page)> = visible
                                .iter()
                                .map(|&index| (stack.origins[index], &notebook.pages[index]))
                                .collect();
                            canvas.set_neighbor_pages(&neighbors);
                        } else {
                            canvas.set_neighbor_pages(&[]);
                        }
                        let assets = AssetStore::new(&storage_dir);
                        // PDF-Seiten, die nach diesem Bild nicht mehr gebraucht werden (schnelles
                        // Scrollen), verwirft end_frame, bevor sie gerendert werden
                        pdf_renderer.begin_frame();
                        canvas.prepare_images(&mut |key| texture_for(key, &assets, &mut pdf_renderer));
                        // Nächste PDF-Seite vorab rendern, damit Blättern sofort klappt
                        if !canvas.is_waiting_for_images() {
                            let next = notebook.pages.get(notebook.current_page_index + 1);
                            if let Some(pdf) = next.and_then(|p| p.pdf_background.as_ref()) {
                                if let Ok(path) = assets.path_of(&pdf.asset) {
                                    pdf_renderer.prefetch(&pdf.key(), path, pdf.page as usize);
                                }
                            }
                        }
                        pdf_renderer.end_frame();
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
