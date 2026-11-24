//! Hilfsfunktionen für Werkzeuge, Auswahl, Zwischenablage, Text, Bilder, Seitenwechsel und Export.

use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::{Duration, Instant};

use glam::Vec2;
use winit::window::Window;

use crate::assets::{AssetStore, ImportedImage};
use crate::autosave::{AutosaveController, AutosaveStatus};
use crate::background::PatternColor;
use crate::canvas::CanvasState;
use crate::document::{Notebook, PageBackground};
use crate::export::{self, export_notebook_to_pdf, ExportStrategy};
use crate::geometry::{self, RecognizedShape};
use crate::image_layer::TextureSource;
use crate::library::Library;
use crate::pdf_doc::PdfRenderService;
use crate::platform;
use crate::selection::{scale_factors, DragMode, SelectionHandleHit, SelectionState};
use crate::settings::UiSettings;
use crate::storage::{self, NotebookManifest};
use crate::t;
use crate::ui::{self, ActiveTool, UiState};

/// Fenstersymbol aus dem eingebauten App-Symbol, auf 64 × 64 Pixel verkleinert: In voller
/// Größe (256 × 256) überschreitet es die Größe einer einzelnen X11-Anfrage und fehlt dann.
pub(super) fn app_icon() -> Option<winit::window::Icon> {
    const SIZE: u32 = 64;
    let image = image::load_from_memory(include_bytes!("../../assets/ink-notes.png")).ok()?;
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
pub(super) fn space_press(
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
pub(super) fn space_move(screen: (f32, f32), canvas: &CanvasState, ui_state: &mut UiState) {
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
pub(super) fn space_release(
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
        for index in run.clone() {
            let _ = storage::ensure_page_loaded(storage_dir, notebook, index);
        }
        if notebook.pages[run].iter().any(|p| p.is_readonly()) {
            ui_state.set_export_message(
                t!("Platz schaffen nicht möglich: Eine der Seiten konnte nicht geladen werden")
                    .to_string(),
            );
            return;
        }
    }
    let dy = drag.clamped_dy();
    if let Some(changed) = notebook.make_space(drag.page_index, drag.y, dy) {
        persist_pages(&changed, notebook, manifest, autosave);
        canvas.sync_page(notebook.current_page_mut());
        let mm = (dy / crate::document::UNITS_PER_MM).abs().round();
        ui_state.set_export_message(if dy > 0.0 {
            t!("{mm} mm Platz geschaffen (Rückgängig mit Strg+Z)", mm = mm)
        } else {
            t!("{mm} mm Platz geschlossen (Rückgängig mit Strg+Z)", mm = mm)
        });
    }
}

/// Merkt geänderte Seiten (Kennungen) und das Manifest zum Speichern vor.
pub(super) fn persist_pages(
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
pub(super) fn activate_tool(tool: ActiveTool, ui_state: &mut UiState, canvas: &mut CanvasState) {
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

/// Liest Notizbücher und Ordner für den Dokumenten-Manager neu ein.
pub(super) fn refresh_notebooks(base_dir: &Path, ui_state: &mut UiState) {
    ui_state.available_notebooks = storage::list_notebooks(base_dir);
    ui_state.library = Library::load(base_dir);
    ui_state.prune_tabs();
}

/// Ändert die Ordner (`library.json`) und liest sie neu ein; Fehler erscheinen als Meldung.
/// Gibt bei Erfolg das Ergebnis von `change` zurück.
pub(super) fn change_library<T>(
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
pub(super) fn save_ui_settings(
    settings: &UiSettings,
    path: Option<&std::path::Path>,
    ui_state: &mut UiState,
) {
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

/// Warns when the notebook is still marked as open on another device (Issue #49).
pub(super) fn warn_if_open_elsewhere(lock: Option<&storage::NotebookLock>, ui_state: &mut UiState) {
    if let Some(marker) = lock.and_then(|l| l.open_elsewhere.as_ref()) {
        ui_state.set_export_message(t!(
            "⚠ Auch auf '{device}' geöffnet – Änderungen dort können deine überschreiben",
            device = marker.device
        ));
    }
}

pub(super) fn update_window_title(
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
pub(super) fn start_pdf_export(
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
        let _ = result_tx.send(result.and_then(|()| platform::publish_export(export_path)));
        window.request_redraw();
    });
}

/// Halten am Strichende (Form einrasten): So weit darf der Stift dabei zittern, in physischen
/// Pixeln. Echte Stifte zittern im Stillstand um einige Pixel; mit 3 px begann die Wartezeit
/// ständig neu, und Halten rastete nur mit der Maus ein (Issue #27).
pub(super) fn hold_tolerance(window: &Window) -> f32 {
    6.0 * window.scale_factor() as f32
}

/// Achsenkreuz aus der eingerasteten Geraden `shape` und dem vorigen Strich der aktuellen
/// Seite, falls sich beide kreuzen (Issue #20); mit der Id des vorigen Strichs
pub(super) fn axis_cross_with_previous(
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
pub(super) fn commit_stroke_to_notebook(
    canvas: &mut CanvasState,
    notebook: &mut Notebook,
    manifest: &mut NotebookManifest,
    autosave: &mut AutosaveController,
    ui_state: &mut UiState,
    window: &Window,
    snapped_shape: Option<RecognizedShape>,
) {
    // Zweite Gerade über Kreuz mit dem vorigen Strich: beide werden zum Achsenkreuz; der
    // vorige Strich wird ersetzt, ein Rückgängig stellt ihn wieder her
    let stroke_width = ui_state.current_stroke_width_mm() * crate::document::UNITS_PER_MM;
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
            ui_state.math.stroke_committed(page, Instant::now());
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
        // An `=` may have been completed (Issue #46)
        ui_state.math.stroke_committed(page, Instant::now());
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
pub(super) fn switch_to_page(
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
pub(super) fn page_to_focus(
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

/// Fling diagnostics (Issue #28): reports events whose handling blocks the app for longer than
/// a few frames, because touch events wait in the queue meanwhile.
pub(super) struct SlowEventLog {
    label: Option<&'static str>,
    start: Instant,
}

impl SlowEventLog {
    const LIMIT: Duration = Duration::from_millis(40);

    pub(super) fn new(enabled: bool, label: &'static str) -> Self {
        Self {
            label: enabled.then_some(label),
            start: Instant::now(),
        }
    }
}

impl Drop for SlowEventLog {
    fn drop(&mut self) {
        let elapsed = self.start.elapsed();
        if let Some(label) = self.label.filter(|_| elapsed > Self::LIMIT) {
            eprintln!(
                "[Nachgleiten] {label} hat {} ms gedauert",
                elapsed.as_millis()
            );
        }
    }
}

/// Zwischenablage: interne Objekte (Strg+C in ink-notes), Bilder aus der
/// Systemzwischenablage (Screenshots, aus anderen Programmen kopierte Bilder) und Text.
pub(super) struct ClipboardBridge {
    system: Option<platform::clipboard::Clipboard>,
    /// Prüfsumme des Systembildes zum Zeitpunkt des letzten internen Kopierens: Strg+V fügt
    /// das Systembild nur ein, wenn es danach kopiert wurde (die jüngste Kopie gewinnt)
    image_hash_at_copy: Option<[u8; 32]>,
    /// Text der Systemzwischenablage zum Zeitpunkt des letzten internen Kopierens (wie oben)
    text_at_copy: Option<String>,
    /// Notizbuch, aus dem kopiert wurde (für Bilder beim Einfügen in ein anderes Notizbuch)
    source_dir: Option<PathBuf>,
}

impl ClipboardBridge {
    pub(super) fn new() -> Self {
        Self {
            system: None,
            image_hash_at_copy: None,
            text_at_copy: None,
            source_dir: None,
        }
    }

    /// Bild in der Systemzwischenablage samt Prüfsumme.
    pub(super) fn system_image(
        &mut self,
    ) -> Option<(platform::clipboard::ImageData<'static>, [u8; 32])> {
        if self.system.is_none() {
            self.system = platform::clipboard::Clipboard::new()
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
    pub(super) fn system_text(&mut self) -> Option<String> {
        if self.system.is_none() {
            self.system = platform::clipboard::Clipboard::new().ok();
        }
        let text = self.system.as_mut()?.get_text().ok()?;
        (!text.trim().is_empty()).then_some(text)
    }

    /// Text copied or cut in a text field goes to the system clipboard, so it can be pasted
    /// into other apps (Issue #57).
    pub(super) fn set_system_text(&mut self, text: String) {
        if self.system.is_none() {
            self.system = platform::clipboard::Clipboard::new().ok();
        }
        if let Some(system) = self.system.as_mut() {
            if let Err(err) = system.set_text(text) {
                eprintln!("[Zwischenablage] Text nicht gesetzt: {err}");
            }
        }
    }

    /// Merkt sich beim internen Kopieren das aktuelle Systembild, den Text und das
    /// Quell-Notizbuch.
    pub(super) fn note_internal_copy(&mut self, storage_dir: &Path) {
        self.image_hash_at_copy = self.system_image().map(|(_, hash)| hash);
        self.text_at_copy = self.system_text();
        self.source_dir = Some(storage_dir.to_path_buf());
    }

    /// Internes Kopieren: die Auswahl zusätzlich als Bild in die Systemzwischenablage legen,
    /// damit sie sich in Chat, Mail oder Office einfügen lässt. In ink-notes selbst fügt
    /// Strg+V weiterhin die Objekte (verlustfrei) ein.
    pub(super) fn copy_selection(
        &mut self,
        page: &crate::document::Page,
        ids: &[u64],
        storage_dir: &Path,
    ) -> bool {
        let assets = AssetStore::new(storage_dir);
        let image = match crate::page_export::selection_image(page, ids, Some(&assets)) {
            Ok(Some(image)) => image,
            Ok(None) => return false,
            Err(err) => {
                eprintln!("[Zwischenablage] Auswahl nicht als Bild darstellbar: {err}");
                return false;
            }
        };
        if self.system.is_none() {
            self.system = platform::clipboard::Clipboard::new().ok();
        }
        let Some(system) = self.system.as_mut() else {
            return false;
        };
        let data = platform::clipboard::ImageData {
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
pub(super) fn insert_image_on_page(
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
    let corners = crate::document::place_image(
        imported.width,
        imported.height,
        center,
        notebook.current_page().size,
        notebook.current_page().reference_size(),
    );
    let page = notebook.current_page_mut();
    page.add_stroke(crate::document::Stroke::new_image(
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
pub(super) fn finger_grabs_selection(
    screen: (f32, f32),
    canvas: &CanvasState,
    page: &crate::document::Page,
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
pub(super) fn selection_press(
    screen: (f32, f32),
    shift: bool,
    canvas: &CanvasState,
    page: &crate::document::Page,
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
pub(super) fn flush_selection_drag(
    page: &mut crate::document::Page,
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
pub(super) fn selection_drag(
    page_pt: Vec2,
    page: &mut crate::document::Page,
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
pub(super) fn selection_release(
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
pub(super) fn insert_text_on_page(
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
    let mut edit = crate::text::TextEditing::new_at(
        notebook.current_page().id,
        position,
        notebook.current_page().text_right_edge(position.x),
        ui_state.text_size_pt * crate::text::UNITS_PER_PT,
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

/// Inserts the result of a math suggestion as a text box next to its `=`, in the ink color
/// and as high as the handwriting, as one undo step (Issue #46)
pub(super) fn insert_math_result(
    suggestion: &crate::math_assist::Suggestion,
    canvas: &mut CanvasState,
    notebook: &mut Notebook,
    manifest: &mut NotebookManifest,
    autosave: &mut AutosaveController,
) {
    let page = notebook.current_page();
    if page.id != suggestion.page_id || page.is_readonly() {
        return;
    }
    let font = crate::text::TextFont::Sans;
    let ascent_per_em = crate::text::TextBox::new(String::new(), 1.0, font, 1.0)
        .layout()
        .ascent;
    let (origin, size) = suggestion.text_placement(ascent_per_em);
    let mut edit = crate::text::TextEditing::new_at(
        page.id,
        origin,
        page.text_right_edge(origin.x),
        size,
        font,
        suggestion.color,
    );
    edit.origin = origin;
    edit.text.content = suggestion.result.clone();
    // As wide as the result, so that it does not wrap near the edge of the page
    edit.text.wrap_width = edit
        .text
        .layout()
        .lines
        .iter()
        .map(|line| line.width)
        .fold(0.0, f32::max)
        + size;
    let id = notebook.next_stroke_id();
    let Some(stroke) = edit.to_stroke(id) else {
        return;
    };
    let page = notebook.current_page_mut();
    page.add_stroke(stroke);
    canvas.sync_page(page);
    manifest.next_stroke_id = notebook.next_stroke_id;
    autosave.mark_dirty(notebook, manifest);
}

/// Beendet eine laufende Textbearbeitung: übernimmt das Feld (neu, geändert oder – wenn
/// geleert – entfernt) als einen Undo-Schritt auf seiner Seite.
pub(super) fn finish_text_edit(
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

/// Before another notebook takes the place of the open one: finishes a running text edit
/// (it belongs to this notebook, page ids repeat in every notebook) and saves everything.
/// Returns `false` and tells the user when saving failed; the notebook then has to stay open,
/// otherwise the unsaved changes would be lost.
pub(super) fn save_before_switch(
    canvas: &mut CanvasState,
    notebook: &mut Notebook,
    manifest: &mut NotebookManifest,
    autosave: &mut AutosaveController,
    ui_state: &mut UiState,
) -> bool {
    finish_text_edit(canvas, notebook, manifest, autosave, ui_state);
    match autosave.flush() {
        Ok(()) => true,
        Err(err) => {
            let msg = err.messages.join(", ");
            eprintln!("[Autosave] Notebook not switched, saving failed: {msg}");
            ui_state.set_export_message(t!(
                "Nicht gewechselt, Speichern fehlgeschlagen: {err}",
                err = msg
            ));
            false
        }
    }
}

/// Text-Werkzeug: Tippen auf das Blatt beendet eine laufende Bearbeitung und öffnet das
/// Textfeld unter `page_pt` zum Bearbeiten – oder ein neues an dieser Stelle.
pub(super) fn text_tool_click(
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
            let edit = crate::text::TextEditing::of_stroke(page.id, stroke);
            if let Some(edit) = &edit {
                // Die Schriftknöpfe zeigen die Schrift des Felds und ändern sie
                ui_state.text_size_pt =
                    (edit.text.size / crate::text::UNITS_PER_PT * 2.0).round() / 2.0;
                ui_state.text_font = edit.text.font;
                ui_state.text_color = edit.color;
                canvas.set_hidden_text(Some(stroke.id));
            }
            edit
        }
        None => Some(crate::text::TextEditing::new_at(
            page.id,
            page_pt,
            page.text_right_edge(page_pt.x),
            ui_state.text_size_pt * crate::text::UNITS_PER_PT,
            ui_state.text_font,
            ui_state.text_color,
        )),
    };
    ui_state.text_edit = edit;
    canvas.sync_page(notebook.current_page_mut());
}

/// Textur für einen Schlüssel der Bildebene: `<pdf>#<seite>` rendert der Hintergrund-Thread,
/// alles andere ist eine Bilddatei im Asset-Ordner.
pub(super) fn texture_for(
    key: &str,
    assets: &AssetStore,
    pdf_renderer: &mut PdfRenderService,
) -> TextureSource {
    if !key.contains('#') {
        return match assets.load_rgba(key, crate::assets::MAX_DISPLAY_DIMENSION) {
            Ok(image) => TextureSource::Ready(image),
            Err(err) => TextureSource::Failed(err.to_string()),
        };
    }
    let Some((Ok(path), page, max_pixels)) = crate::pdf_doc::parse_page_key(key)
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
pub(super) fn page_view(canvas: &CanvasState, scale_factor: f32) -> ui::PageView {
    let origin = canvas.page_to_screen(Vec2::ZERO);
    let unit = canvas.page_to_screen(Vec2::new(1000.0, 0.0));
    ui::PageView {
        origin: egui::pos2(origin[0] / scale_factor, origin[1] / scale_factor),
        scale: (unit[0] - origin[0]) / 1000.0 / scale_factor,
    }
}

pub(super) fn copy_message(count: usize, as_image: bool) -> String {
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
pub(super) fn start_page_export(
    kind: ui::PageExportKind,
    notebook: &Notebook,
    storage_dir: &Path,
    title: &str,
    pattern: PatternColor,
    result_tx: Sender<Result<PathBuf, String>>,
    window: Arc<Window>,
) {
    use crate::page_export::{page_png, page_svg, ImageBackground};
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
            .and_then(|()| platform::publish_export(path));
        let _ = result_tx.send(result);
        window.request_redraw();
    });
}

/// Seitenmitte des sichtbaren Bereichs (Ziel für eingefügte Bilder).
pub(super) fn visible_page_center(canvas: &CanvasState) -> Vec2 {
    canvas.screen_to_page_clamped(
        canvas.size.width as f32 / 2.0,
        canvas.size.height as f32 / 2.0,
    )
}

/// Strg+V bzw. „Einfügen“: ein neu kopiertes Systembild hat Vorrang, sonst werden die intern
/// kopierten Objekte eingefügt. `at`: Stelle auf der Seite (lange gedrückt → Einfügen), sonst
/// die Mitte des sichtbaren Bereichs.
#[allow(clippy::too_many_arguments)]
pub(super) fn paste(
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
pub(super) fn insert_image_file(
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
