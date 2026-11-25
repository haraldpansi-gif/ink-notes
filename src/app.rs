//! The app itself: window, event loop (winit), mouse, touch and pen input and control flow,
//! with the egui toolbar, page management, undo/redo, vector PDF export and asynchronous
//! notebook persistence (autosave). Lives in the library so that other entry points (e.g.
//! Android, Issue #42) can start it too.

// On Android the pen state is unused until pen input arrives there (Issue #44)
#![cfg_attr(
    target_os = "android",
    allow(
        unused_mut,
        unused_variables,
        unused_assignments,
        unused_imports,
        dead_code
    )
)]

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use winit::{
    application::ApplicationHandler,
    event::{ElementState, KeyEvent, TouchPhase, WindowEvent},
    event_loop::ActiveEventLoop,
    keyboard::{Key, ModifiersState},
    window::{Window, WindowId},
};

use crate::autosave::AutosaveController;
use crate::canvas::CanvasState;
use crate::disk_watch::DiskWatch;
use crate::document::Notebook;
use crate::geometry::RecognizedShape;
use crate::i18n::{self, Language};
use crate::library::Library;
use crate::osk::KeyboardRequester;
use crate::pdf_doc::PdfRenderService;
use crate::platform;
use crate::selection::SelectionState;
use crate::settings::{StylusAction, UiSettings};
use crate::storage::{self, NotebookLayout, NotebookManifest};
use crate::t;
use crate::ui::{self, ActiveTool, UiAction, UiState};
use crate::updater::UpdaterController;

mod about_to_wait;
mod actions;
mod helpers;
mod keyboard;
mod pointer;
mod redraw;

use helpers::*;

/// Kennung der App (Name der .desktop-Datei und des Symbols).
const APP_ID: &str = "eu.pansi.ink-notes";
/// After the pen leaves the screen, new touches still count as the writing hand for this long
/// (Issue #39).
pub(super) const PALM_GRACE: Duration = Duration::from_millis(700);

/// Gesamter Laufzeitzustand der App, aufgeteilt auf die Event-Handler-Submodule.
struct AppState {
    ui_settings_path: Option<PathBuf>,
    ui_settings: UiSettings,
    window: Arc<Window>,
    canvas: CanvasState,
    #[cfg(not(target_os = "android"))]
    tablet_manager: Option<octotablet::Manager>,
    keyboard_requester: Option<KeyboardRequester>,
    ime_was_active: bool,
    ime_composing: bool,
    storage_base_dir: PathBuf,
    storage_dir: PathBuf,
    notebook: Notebook,
    manifest: NotebookManifest,
    autosave: AutosaveController,
    egui_ctx: egui::Context,
    export_tx: Sender<Result<PathBuf, String>>,
    export_rx: Receiver<Result<PathBuf, String>>,
    updater: UpdaterController,
    egui_winit_state: egui_winit::State,
    ui_state: UiState,
    cloud: crate::cloud_sync::CloudSync,
    ocr: crate::handwriting_ocr::OcrController,
    selection_state: SelectionState,
    last_deleted_page: Option<(String, usize, crate::document::Page)>,
    clipboard: ClipboardBridge,
    pdf_renderer: PdfRenderService,
    notebook_lock: Option<storage::NotebookLock>,
    disk_watch: DiskWatch,
    start_time: Instant,
    cursor_screen_pos: (f32, f32),
    is_drawing: bool,
    is_erasing: bool,
    current_pressure: f32,
    modifiers: ModifiersState,
    is_mouse_drawing: bool,
    active_touches: HashMap<u64, (f32, f32)>,
    pinch_prev_distance: Option<f32>,
    ui_touch_ids: HashSet<u64>,
    touches_down: HashSet<u64>,
    pinch_prev_center: Option<(f32, f32)>,
    is_pinch_gesturing: bool,
    is_touch_drawing: bool,
    is_touch_panning: bool,
    long_press: ui::LongPress,
    touch_pan_last_pos: (f32, f32),
    touch_pan_trace: (u32, f32, Option<(Instant, Instant)>),
    touch_clock: Option<(Instant, u32)>,
    fling: crate::viewport::Fling,
    debug_fling: bool,
    is_stylus_drawing: bool,
    is_stylus_selecting: bool,
    is_stylus_in_proximity: bool,
    palm_touch_ids: HashSet<u64>,
    stylus_left_at: Option<Instant>,
    stylus_tool_id: Option<u64>,
    #[cfg(target_os = "android")]
    android_pen: crate::pen::AndroidPen,
    stylus_held: Option<(u64, StylusAction)>,
    tool_before_button: Option<ActiveTool>,
    is_stylus_spacing: bool,
    is_touch_spacing: bool,
    stylus_pan_last: Option<(f32, f32)>,
    stylus_screen_pos: (f32, f32),
    stylus_ui_capture: bool,
    egui_repaint_at: Option<Instant>,
    is_panning: bool,
    pan_last_screen_pos: (f32, f32),
    is_space_pressed: bool,
    stroke_last_motion_time: Instant,
    stroke_hold_anchor_pos: (f32, f32),
    stroke_already_snapped: bool,
    stroke_snapped_shape: Option<RecognizedShape>,
    stroke_max_radius: f32,
    last_scroll: Option<[f32; 3]>,
}

impl AppState {
    /// Durchgehende Ansicht: vor Zeichnen, Radieren oder Auswählen auf die Seite unter dem
    /// Bildschirmpunkt wechseln.
    pub(super) fn focus_page_at(&mut self, pos: (f32, f32)) {
        if let Some(index) = page_to_focus(
            &self.canvas,
            &self.notebook,
            &self.ui_state,
            &self.selection_state,
            self.egui_ctx.pixels_per_point(),
            pos,
        ) {
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

    fn sync_layout_flags(&mut self) {
        // Anordnung der Seiten gehört zum offenen Notizbuch (Issue #30)
        // Endlos ist ein Notizbuch an seiner einzigen, endlosen Seite erkennbar
        self.ui_state.endless = self.notebook.is_endless();
        self.ui_state.continuous_scroll = !self.ui_state.endless
            && self.manifest.layout_or(self.ui_settings.continuous_scroll)
                == NotebookLayout::Continuous;
        self.ui_state.endless_switch_possible = if self.ui_state.endless {
            self.notebook.can_make_paged()
        } else {
            self.notebook.can_make_endless()
        };
    }

    fn handle_suspended(&mut self) {
        let _slow_event_log = SlowEventLog::new(self.debug_fling, "Ereignis");
        finish_text_edit(
            &mut self.canvas,
            &mut self.notebook,
            &mut self.manifest,
            &mut self.autosave,
            &mut self.ui_state,
        );
        if let Err(err) = self.autosave.flush() {
            eprintln!(
                "[Speichern] Fehler beim Wechsel in den Hintergrund: {}",
                err.messages.join(", ")
            );
        }
        self.canvas.suspend();
    }

    fn handle_resumed(&mut self) {
        let _slow_event_log = SlowEventLog::new(self.debug_fling, "Ereignis");
        if let Err(err) = self.canvas.resume(self.window.clone()) {
            eprintln!("[Grafik-Fehler] {err}");
            std::process::exit(1);
        }
        self.window.request_redraw();
    }

    fn handle_window_event(&mut self, event: WindowEvent, target: &ActiveEventLoop) {
        let label = match &event {
            WindowEvent::RedrawRequested => "Zeichnen",
            WindowEvent::Touch(_) => "Finger",
            _ => "Fensterereignis",
        };
        let _slow_event_log = SlowEventLog::new(self.debug_fling, label);
        self.sync_layout_flags();
        // 1. Window-Event zuerst an egui übergeben
        if let WindowEvent::Touch(touch) = &event {
            match touch.phase {
                TouchPhase::Started => {
                    self.touches_down.insert(touch.id);
                }
                TouchPhase::Ended | TouchPhase::Cancelled => {
                    self.touches_down.remove(&touch.id);
                }
                TouchPhase::Moved => {}
            }
        }
        // Beim Antippen blendet GNOME den Mauszeiger aus und meldet CursorLeft, während der
        // Finger noch aufliegt. egui-winit verwirft dabei die Zeigerposition und meldet das
        // Abheben nicht mehr, der Tipp ginge verloren – daher erfährt egui davon nichts.
        let egui_response =
            if matches!(event, WindowEvent::CursorLeft { .. }) && !self.touches_down.is_empty() {
                egui_winit::EventResponse {
                    repaint: true,
                    consumed: false,
                }
            } else {
                // Ctrl+V in a text field: egui-winit is built without its own clipboard
                // access and would only paste what was copied inside the app, so it gets the
                // text of the system clipboard first (Issue #57)
                if let WindowEvent::KeyboardInput {
                    event:
                        KeyEvent {
                            logical_key: Key::Character(c),
                            state: ElementState::Pressed,
                            ..
                        },
                    ..
                } = &event
                {
                    if c.eq_ignore_ascii_case("v")
                        && (self.modifiers.control_key() || self.modifiers.super_key())
                        && self.egui_ctx.egui_wants_keyboard_input()
                    {
                        if let Some(text) = self.clipboard.system_text() {
                            self.egui_winit_state.set_clipboard_text(text);
                        }
                    }
                }
                self.egui_winit_state.on_window_event(&self.window, &event)
            };
        // egui-winit asks for a repaint after every RedrawRequested as well; following
        // that would redraw endlessly even when idle (high CPU load in the app and the
        // compositor). Follow-up frames egui needs come via `repaint_delay` instead.
        if egui_response.repaint && !matches!(event, WindowEvent::RedrawRequested) {
            self.window.request_redraw();
        }

        // 2. Prüfen, ob das UI Pointer-Interaktionen beansprucht (Event-Isolation).
        // Konsumiert Klicks/Scrolls innerhalb der Pill-Toolbar, der Popover oder offener Menüs.
        // Wichtig (egui 0.37 / Issue #64): `egui_wants_pointer_input()` meldet bei `begin_pass`
        // fälschlich `true` über `Order::Background`-Ebenen (z. B. Seitentrennlinien bei ≥ 2
        // Seiten oder Lineal) und blockiert dann Touchpad-Scrollen auf dem Blatt. Daher prüfen
        // wir `egui_is_using_pointer()` (aktives Ziehen an einem UI-Widget) plus unser eigenes
        // `is_screen_point_in_ui` (ignoriert `Order::Background`).
        let is_pointer_in_ui = self.egui_ctx.egui_is_using_pointer()
            || ui::is_screen_point_in_ui(
                &self.egui_ctx,
                &self.ui_state,
                self.cursor_screen_pos,
                self.window.scale_factor() as f32,
            );

        match event {
            WindowEvent::CloseRequested => {
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
            WindowEvent::DroppedFile(path) => {
                if crate::import::is_importable_path(&path) {
                    self.ui_state
                        .queued_actions
                        .push(UiAction::ImportFile(path));
                } else if crate::assets::is_supported_image_path(&path) {
                    let (sx, sy) = self.cursor_screen_pos;
                    let center = self.canvas.screen_to_page_clamped(sx, sy);
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
                } else {
                    self.ui_state.set_export_message(t!("Nur Bilder (PNG, JPEG, GIF, WebP, BMP) und PDFs können eingefügt werden").to_string());
                }
                self.window.request_redraw();
            }
            WindowEvent::Focused(true) => {
                // Coming back from another app: look for changes right away
                self.disk_watch.check_soon(Instant::now());
                self.cloud.sync_soon();
                self.window.request_redraw();
            }
            WindowEvent::Focused(false) => {
                // Bei Fokusverlust Strich abbrechen und 1€-Filter resetten
                if self.is_drawing {
                    self.is_drawing = false;
                    self.canvas.cancel_stroke();
                    self.window.request_redraw();
                }
                self.is_erasing = false;
                self.is_mouse_drawing = false;
                self.active_touches.clear();
                self.ui_touch_ids.clear();
                self.palm_touch_ids.clear();
                self.stylus_ui_capture = false;
                self.is_pinch_gesturing = false;
                self.pinch_prev_distance = None;
                self.pinch_prev_center = None;
                self.is_touch_drawing = false;
                self.is_touch_panning = false;
                self.fling.stop();
                self.is_stylus_drawing = false;
                self.is_stylus_selecting = false;
                self.is_stylus_in_proximity = false;
            }
            WindowEvent::Resized(physical_size) => {
                self.canvas.resize(physical_size);
                self.canvas.sync_page(self.notebook.current_page_mut());
                self.window.request_redraw();
            }
            WindowEvent::ScaleFactorChanged { .. } => {
                self.canvas.resize(self.window.inner_size());
                self.canvas.sync_page(self.notebook.current_page_mut());
                self.window.request_redraw();
            }
            WindowEvent::ModifiersChanged(new_mods) => {
                self.modifiers = new_mods.state();
            }
            WindowEvent::KeyboardInput { event, .. } => self.handle_keyboard_input(event, target),
            WindowEvent::MouseInput { state, button, .. } => {
                self.handle_mouse_input(state, button, is_pointer_in_ui)
            }
            WindowEvent::CursorMoved { position, .. } => self.handle_cursor_moved(position),
            WindowEvent::MouseWheel { delta, phase, .. } => {
                self.handle_mouse_wheel(delta, phase, is_pointer_in_ui)
            }
            WindowEvent::Touch(touch) => self.handle_touch(touch),
            WindowEvent::RedrawRequested => self.handle_redraw_requested(target),
            _ => {}
        }
    }
}

struct AppRunner {
    app: Option<AppState>,
    init_error: Option<Box<dyn std::error::Error>>,
}

impl ApplicationHandler for AppRunner {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(app) = &mut self.app {
            app.handle_resumed();
            return;
        }
        match AppState::new(event_loop) {
            Ok(app) => self.app = Some(app),
            Err(err) => {
                self.init_error = Some(err);
                event_loop.exit();
            }
        }
    }

    fn suspended(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(app) = &mut self.app {
            app.handle_suspended();
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(app) = &mut self.app {
            let _slow_event_log = SlowEventLog::new(app.debug_fling, "Leerlauf");
            app.sync_layout_flags();
            app.handle_about_to_wait(event_loop);
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        if let Some(app) = &mut self.app {
            if window_id == app.window.id() {
                app.handle_window_event(event, event_loop);
            }
        }
    }
}

/// Runs the app until the window is closed (the desktop entry point is `src/main.rs`).
#[cfg(not(target_os = "android"))]
pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    // Anzeigesystem wählen: Unter WSL (WSLg) bricht Wayland ab, X11 (Xwayland) läuft stabil
    run_with(platform::desktop_event_loop()?)
}

/// Runs the app on `event_loop` (desktop: [`run`], Android: `android_main` in `lib.rs`).
pub fn run_with(
    event_loop: winit::event_loop::EventLoop<()>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut runner = AppRunner {
        app: None,
        init_error: None,
    };
    platform::run_event_loop(event_loop, &mut runner)?;
    if let Some(err) = runner.init_error {
        return Err(err);
    }
    Ok(())
}

impl AppState {
    fn new(event_loop: &ActiveEventLoop) -> Result<Self, Box<dyn std::error::Error>> {
        // Gespeicherte UI-Einstellungen; zuerst, damit alle Texte in der gewählten Sprache sind
        let ui_settings_path = UiSettings::default_path();
        let ui_settings = ui_settings_path
            .as_deref()
            .map(UiSettings::load_from)
            .unwrap_or_default();
        i18n::set_language(ui_settings.language.unwrap_or_else(Language::system));

        // App-Kennung wie die .desktop-Datei (eu.pansi.ink-notes): Daran ordnen Dock und Taskleiste
        // das Fenster dem App-Symbol zu (Wayland: app_id, X11: WM_CLASS). Das Fenstersymbol selbst
        // nutzt nur X11.
        let attrs = Window::default_attributes()
            .with_title("ink-notes")
            .with_inner_size(winit::dpi::LogicalSize::new(1280.0, 850.0))
            .with_window_icon(app_icon());
        let attrs = platform::with_app_id(attrs, APP_ID);
        let window = Arc::new(event_loop.create_window(attrs)?);

        let mut canvas = match pollster::block_on(CanvasState::new(window.clone())) {
            Ok(canvas) => canvas,
            Err(err) => {
                eprintln!("[Grafik-Fehler] {err}");
                std::process::exit(1);
            }
        };

        // Android has no octotablet: the pen arrives as touch events there (Issue #44).
        // octotablet panics on Windows without the Tablet PC components (RealTimeStylus not
        // registered); the app then runs with mouse and touch events only
        #[cfg(not(target_os = "android"))]
        let tablet_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            // Windows Ink would otherwise report the mouse as a pen (Issue #48): every mouse
            // move counted as pen hover, every click as pen down next to the normal mouse events
            octotablet::builder::Builder::default()
                .emulate_tool_from_mouse(false)
                .build_shared(&window)
        }));
        #[cfg(not(target_os = "android"))]
        let tablet_manager = match tablet_result {
            Ok(Ok(mgr)) => {
                println!("[Input] Octotablet Stylus-/Tablet-Manager erfolgreich initialisiert (Wayland/X11 Tablet-Protokoll aktiv).");
                Some(mgr)
            }
            Ok(Err(err)) => {
                println!("[Input] Hinweis: Octotablet Tablet-Manager nicht aktiv ({err}). Nutze Standard-Event-Pipeline.");
                None
            }
            Err(_) => {
                println!("[Input] Hinweis: Octotablet Tablet-Manager abgestürzt. Nutze Standard-Event-Pipeline.");
                None
            }
        };

        // Bildschirmtastatur (nur Wayland): eigenes text-input-Objekt, siehe osk.rs
        // Safety: `window` lives until the end of run(), the event loop owns both
        let keyboard_requester = unsafe { KeyboardRequester::for_window(&window) };
        let ime_was_active = false;
        let ime_composing = false;

        // 1. Notizbuch aus Storage laden oder initialisieren (Session-Wiederherstellung)
        let storage_base_dir = ui_settings
            .storage_dir
            .clone()
            .unwrap_or_else(storage::default_storage_dir);
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
        let (mut notebook, manifest) = if storage_dir.join("manifest.json").exists() {
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
                    let (nb, m, dir) =
                        storage::create_new_notebook(&storage_base_dir, &initial_title)?;
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
        let mut autosave =
            AutosaveController::new(storage_dir.clone(), Duration::from_millis(1500));

        // 3. egui Context und State initialisieren
        let egui_ctx = egui::Context::default();
        ui::configure_context(&egui_ctx);

        // 4. In-App Gitea Updater initialisieren und im Hintergrund nach Releases fragen
        // Ergebnisse der PDF-Exporte aus dem Hintergrundthread
        let (export_tx, export_rx) = mpsc::channel::<Result<PathBuf, String>>();

        let mut updater = UpdaterController::new();
        // Android: updates come as APK, which the updater cannot install yet
        #[cfg(not(target_os = "android"))]
        updater.check_for_updates();

        let egui_winit_state = egui_winit::State::new(
            egui_ctx.clone(),
            egui::ViewportId::ROOT,
            &window,
            Some(window.scale_factor() as f32),
            None,
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
        ui_state.pen_presets = ui_settings.pens();
        ui_state.stylus_buttons = ui_settings.stylus_buttons.clone();
        ui_state.set_eraser_radius_mm(ui_settings.eraser_radius_mm);
        ui_state.math.set_enabled(ui_settings.math_assist);
        ui_state.touch_draw_enabled = ui_settings.touch_draw;
        ui_state.saved_colors = ui_settings.saved_colors.clone();
        ui_state.select_pen(ui_settings.active_pen);
        ui_state.highlighter_colors = ui_settings.highlighter_colors;
        ui_state.select_highlighter_color(ui_settings.active_highlighter);
        ui_state.text_colors = ui_settings.text_colors;
        ui_state.active_text_color = ui_settings.active_text_color;
        ui_state.text_color = ui_settings.text_colors[ui_settings.active_text_color];
        ui_state.current_notebook_id = manifest.id.clone();
        ui_state.document_title = manifest.title.clone();
        ui_state.available_notebooks = available_notebooks;
        ui_state.open_tabs = ui_settings.open_tabs.clone();
        ui_state.prune_tabs();
        ui_state.library = Library::load(&storage_base_dir);
        ui_state.stamps = crate::stamps::StampLibrary::load(&storage_base_dir);
        ui_state.storage_dir = storage_base_dir.clone();
        ui_state.storage_dir_custom = ui_settings.storage_dir.is_some();
        // Built-in Nextcloud sync of the storage folder (Issue #49, stage 2)
        let cloud = crate::cloud_sync::CloudSync::new(storage_base_dir.clone());
        if let Some(warning) = startup_warning {
            refresh_notebooks(&storage_base_dir, &mut ui_state);
            ui_state.set_export_message(warning);
        }
        let selection_state = SelectionState::new();
        let ocr = crate::handwriting_ocr::OcrController::new();
        // Zuletzt gelöschte Seite (Notizbuch-Kennung, Index, Seite) zum Wiederherstellen in dieser
        // Sitzung
        let last_deleted_page: Option<(String, usize, crate::document::Page)> = None;
        let clipboard = ClipboardBridge::new();
        // PDF-Seiten werden im Hintergrund gerendert und wecken danach den Event-Loop
        let pdf_renderer = {
            let window = window.clone();
            PdfRenderService::new(move || window.request_redraw())
        };

        let notebook_lock = match storage::NotebookLock::acquire(&storage_dir) {
            Ok(lock) => {
                autosave.set_read_only(false);
                warn_if_open_elsewhere(Some(&lock), &mut ui_state);
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

        // Notices changes made on other devices through a sync client (Issue #49)
        let disk_watch = DiskWatch::new(storage_dir.clone(), Instant::now());

        // Initiale Seite und Werkzeugeinstellungen mit Canvas synchronisieren
        canvas.set_paper_theme(ui_state.paper_theme, ui_state.adapt_ink_colors);
        canvas.set_pattern_color(ui_state.pattern_color);
        canvas.sync_page(notebook.current_page_mut());
        canvas.set_stroke_color(ui_state.pen_color);
        canvas.set_stroke_width_mm(ui_state.pen_stroke_width_mm);

        let start_time = Instant::now();
        let cursor_screen_pos = (0.0_f32, 0.0_f32);
        let is_drawing = false;
        let is_erasing = false;
        let current_pressure = 0.5_f32;
        let modifiers = ModifiersState::empty();

        let is_mouse_drawing = false;

        // Multi-Touch & Pinch-to-Zoom Zustand
        let active_touches: std::collections::HashMap<u64, (f32, f32)> =
            std::collections::HashMap::new();
        let pinch_prev_distance: Option<f32> = None;
        // Berührungen, die auf der UI begonnen haben, gehören bis zum Abheben egui
        let ui_touch_ids: std::collections::HashSet<u64> = std::collections::HashSet::new();
        // Alle aufliegenden Finger (auch auf der UI), unabhängig von der Gestenauswertung
        let touches_down: std::collections::HashSet<u64> = std::collections::HashSet::new();
        let pinch_prev_center: Option<(f32, f32)> = None;
        let is_pinch_gesturing = false;
        let is_touch_drawing = false;
        let is_touch_panning = false;
        // Langes Drücken mit einem Finger öffnet das Kontextmenü
        let long_press = ui::LongPress::default();
        let touch_pan_last_pos = (0.0_f32, 0.0_f32);
        // Diagnostics for the fling: moves, path length (px), and when the last move of this swipe
        // was handled and happened (compositor time)
        let touch_pan_trace: (u32, f32, Option<(Instant, Instant)>) = (0, 0.0, None);
        // Compositor time (ms) of the latest touch Started and when it was handled: maps the event
        // times of the following touch events to `Instant`s
        let touch_clock: Option<(Instant, u32)> = None;
        // Nachgleiten nach schnellem Wischen mit dem Finger (Issue #28)
        let fling = crate::viewport::Fling::default();
        // Diagnosis for Issue #28: log every finger lift and the swipe speed
        let debug_fling = std::env::var_os("INK_NOTES_DEBUG_FLING").is_some();
        if debug_fling {
            eprintln!("[Nachgleiten] Diagnose aktiv – bitte mit dem Finger wischen");
        }

        // Stylus / Digitizer Zustand (Octotablet / Wayland Tablet Protocol)
        let is_stylus_drawing = false;
        // Stift zieht mit dem Auswahlwerkzeug (Rahmen, Verschieben, Drehen, Skalieren)
        let is_stylus_selecting = false;
        let is_stylus_in_proximity = false;
        // Palm rejection (Issue #39): fingers that rest on the screen when the pen comes near, or
        // that touch it while the pen is near, are ignored until they are lifted
        let palm_touch_ids: std::collections::HashSet<u64> = std::collections::HashSet::new();
        let stylus_left_at: Option<Instant> = None;
        let stylus_tool_id: Option<u64> = None;
        #[cfg(target_os = "android")]
        let android_pen = crate::pen::AndroidPen::default();
        // Gedrückte Stifttaste (Kennung, Belegung) und das Werkzeug, das während eines Strichs mit
        // einer Werkzeug-Taste (Auswählen, Textmarker, Stift) vertreten wird
        let stylus_held: Option<(u64, StylusAction)> = None;
        let tool_before_button: Option<ActiveTool> = None;
        // Platz schaffen läuft mit dem Stift bzw. dem Finger (sonst mit der Maus)
        let is_stylus_spacing = false;
        let is_touch_spacing = false;
        // View only: the pen moves the page and remembers its last screen position (Issue #35)
        let stylus_pan_last: Option<(f32, f32)> = None;
        let stylus_screen_pos = (0.0_f32, 0.0_f32);
        // Stift hat auf der UI aufgesetzt: alle Ereignisse bis zum Abheben gehen an egui
        let stylus_ui_capture = false;
        // Von egui angeforderter Zeitpunkt für das nächste Neuzeichnen (Layout-Vermessung, Tooltips)
        let egui_repaint_at: Option<Instant> = None;

        // Viewport-Pan & Zoom-Zustand
        let is_panning = false;
        let pan_last_screen_pos = (0.0_f32, 0.0_f32);
        let is_space_pressed = false;

        // Draw-and-Hold Zustand für automatische Formerkennung
        let stroke_last_motion_time = Instant::now();
        let stroke_hold_anchor_pos = (0.0_f32, 0.0_f32);
        let stroke_already_snapped = false;
        let stroke_snapped_shape: Option<RecognizedShape> = None;

        // Radier-Parameter
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
        println!(
            "  Farbpalette (Multicolor), Strichbreite, Undo/Redo, Zoom, Navigation & PDF-Export"
        );
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
        let last_scroll: Option<[f32; 3]> = None;

        Ok(Self {
            ui_settings_path,
            ui_settings,
            window,
            canvas,
            #[cfg(not(target_os = "android"))]
            tablet_manager,
            keyboard_requester,
            ime_was_active,
            ime_composing,
            storage_base_dir,
            storage_dir,
            notebook,
            manifest,
            autosave,
            egui_ctx,
            export_tx,
            export_rx,
            updater,
            egui_winit_state,
            ui_state,
            cloud,
            ocr,
            selection_state,
            last_deleted_page,
            clipboard,
            pdf_renderer,
            notebook_lock,
            disk_watch,
            start_time,
            cursor_screen_pos,
            is_drawing,
            is_erasing,
            current_pressure,
            modifiers,
            is_mouse_drawing,
            active_touches,
            pinch_prev_distance,
            ui_touch_ids,
            touches_down,
            pinch_prev_center,
            is_pinch_gesturing,
            is_touch_drawing,
            is_touch_panning,
            long_press,
            touch_pan_last_pos,
            touch_pan_trace,
            touch_clock,
            fling,
            debug_fling,
            is_stylus_drawing,
            is_stylus_selecting,
            is_stylus_in_proximity,
            palm_touch_ids,
            stylus_left_at,
            stylus_tool_id,
            #[cfg(target_os = "android")]
            android_pen,
            stylus_held,
            tool_before_button,
            is_stylus_spacing,
            is_touch_spacing,
            stylus_pan_last,
            stylus_screen_pos,
            stylus_ui_capture,
            egui_repaint_at,
            is_panning,
            pan_last_screen_pos,
            is_space_pressed,
            stroke_last_motion_time,
            stroke_hold_anchor_pos,
            stroke_already_snapped,
            stroke_snapped_shape,
            stroke_max_radius,
            last_scroll,
        })
    }
}
