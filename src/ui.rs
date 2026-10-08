//! Immediate-Mode UI mit `egui` für ink-notes.
//!
//! Stellt eine schmale, senkrechte Werkzeugleiste bereit, die seitlich angedockt ist
//! (links oder rechts, umschaltbar):
//! - Symbol-Knöpfe mit 40 px Kantenlänge (stift- und fingertauglich), Tooltips mit Kürzeln
//! - Werkzeuge, Farben des aktiven Werkzeugs, Auswahl-Aktionen, Undo/Redo, Seitennavigation
//! - Selten genutzte Funktionen (PDF, Hintergrund, Papierfarbe, Zoom, Touch-Modus,
//!   Druckkurve, Leistenseite, Updates, Speicherstatus) im ☰-Menü
//! - Popover (Stift/Marker/Menü) öffnen sich zur Blattseite hin
//! - Hit-Testing für Event-Transparenz außerhalb der Leiste und der Popover

use std::time::Instant;

use crate::autosave::AutosaveStatus;
use crate::background::{adapt_ink_color, PaperTheme, PatternColor};
use crate::document::{
    EraserMode, Notebook, Page, PageBackground, StrokeTool, PAGE_HEIGHT, PAGE_WIDTH,
};
use crate::file_picker::{FilePicker, PickerKind};
use crate::ruler::{Ruler, RulerHit};
use crate::settings::{
    PenPreset, PressureCurve, SidebarSide, DEFAULT_PEN_PRESETS, PEN_PRESET_COUNT,
};
use crate::storage::NotebookSummary;
use crate::text::{TextEditing, TextFont, TEXT_SIZES_PT};

/// Aktives Werkzeug für das Zeichnen oder Bearbeiten.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveTool {
    Pen,
    Highlighter,
    Eraser,
    Shape,
    Select,
    /// Textfelder schreiben und bearbeiten
    Text,
}

impl ActiveTool {
    pub fn name(&self) -> &'static str {
        match self {
            ActiveTool::Pen => "Stift (Pen)",
            ActiveTool::Highlighter => "Textmarker (Highlighter)",
            ActiveTool::Eraser => "Radierer (Eraser)",
            ActiveTool::Shape => "Formen (Shape)",
            ActiveTool::Select => "Auswahl / Lasso (Select)",
            ActiveTool::Text => "Text (T)",
        }
    }

    pub fn is_drawing(&self) -> bool {
        matches!(self, ActiveTool::Pen | ActiveTool::Highlighter)
    }

    /// Werkzeug, das ein mit diesem Werkzeug gezeichneter Strich erhält.
    pub fn stroke_tool(&self, has_shape: bool) -> StrokeTool {
        match self {
            _ if has_shape => StrokeTool::Shape,
            ActiveTool::Highlighter => StrokeTool::Highlighter,
            _ => StrokeTool::Pen,
        }
    }
}

/// Farbvorgabe: RGBA-Farbe und Bezeichnung.
pub type ColorPreset = ([f32; 4], &'static str);

/// Schnellauswahlfarben für den normalen Stift laut Meilenstein 8:
/// - Notiz-Schwarz: [0.12, 0.12, 0.14, 1.0]
/// - Universitäts-Blau: [0.15, 0.35, 0.85, 1.0]
/// - Korrektur-Rot: [0.85, 0.20, 0.20, 1.0]
/// - Tafel-Grün: [0.18, 0.65, 0.35, 1.0]
/// - Graphit-Grau: [0.45, 0.45, 0.48, 1.0]
pub const PEN_COLOR_PRESETS: [([f32; 4], &str); 5] = [
    ([0.12, 0.12, 0.14, 1.0], "Notiz-Schwarz"),
    ([0.15, 0.35, 0.85, 1.0], "Universitäts-Blau"),
    ([0.85, 0.20, 0.20, 1.0], "Korrektur-Rot"),
    ([0.18, 0.65, 0.35, 1.0], "Tafel-Grün"),
    ([0.45, 0.45, 0.48, 1.0], "Graphit-Grau"),
];

/// Schnellauswahlfarben für den Textmarker (halbtransparent, Alpha 0.35):
/// - Neon-Gelb, Hellgrün, Pastell-Cyan, Rosa
pub const HIGHLIGHTER_COLOR_PRESETS: [([f32; 4], &str); 4] = [
    ([1.0, 0.95, 0.1, 0.35], "Neon-Gelb"),
    ([0.2, 0.9, 0.3, 0.35], "Hellgrün"),
    ([0.25, 0.7, 1.0, 0.35], "Pastell-Cyan"),
    ([1.0, 0.3, 0.65, 0.35], "Rosa"),
];

/// Dicken-Presets für feines Inking und präzise Notizen:
/// - Extra-Fein: 0.3 mm (für kleine Formeln / Indizes)
/// - Fein: 0.5 mm (Standard-Fließtext)
/// - Normal: 0.8 mm (Hervorhebungen)
/// - Fett: 1.2 mm (Überschriften)
pub const PEN_WIDTH_PRESETS: [(f32, &str); 4] = [
    (0.3, "0.3 mm"),
    (0.5, "0.5 mm"),
    (0.8, "0.8 mm"),
    (1.2, "1.2 mm"),
];

/// Dicken-Presets für den Textmarker:
pub const HIGHLIGHTER_WIDTH_PRESETS: [(f32, &str); 3] =
    [(4.0, "4.0 mm"), (6.0, "6.0 mm"), (8.0, "8.0 mm")];

/// Abbildung zwischen Seitenkoordinaten und logischen Bildschirmpunkten (egui), vom
/// Hauptprogramm je Bild aus Ansicht und Zoom gesetzt.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageView {
    /// Bildschirmposition des Seitenursprungs (oben links)
    pub origin: egui::Pos2,
    /// Bildschirmpunkte je Seiteneinheit
    pub scale: f32,
}

impl PageView {
    pub fn to_screen(&self, p: glam::Vec2) -> egui::Pos2 {
        self.origin + egui::vec2(p.x, p.y) * self.scale
    }

    pub fn to_page(&self, p: egui::Pos2) -> glam::Vec2 {
        let d = (p - self.origin) / self.scale.max(1e-6);
        glam::Vec2::new(d.x, d.y)
    }
}

/// Laufende Bedienung des Lineals.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RulerDrag {
    /// Verschieben; Abstand vom Griffpunkt zum Mittelpunkt
    Move {
        grab_offset: glam::Vec2,
    },
    Rotate,
}

/// Format für den Export der aktuellen Seite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageExportKind {
    Png,
    PngTransparent,
    Svg,
}

/// Aktionen, die von der UI an die Anwendungslogik signalisiert werden.
#[derive(Debug, Clone, PartialEq)]
pub enum UiAction {
    SetTool(ActiveTool),
    SetColor([f32; 4]),
    SetStrokeWidthMm(f32),
    Undo,
    Redo,
    PreviousPage,
    NextPage,
    AddPage,
    /// Aktuelle Seite löschen (nach Rückfrage in der Oberfläche)
    DeleteCurrentPage,
    /// Zuletzt gelöschte Seite wiederherstellen
    RestoreDeletedPage,
    GoToPage(usize),
    /// Alle noch nicht geladenen Seiten laden (für die Vorschaubilder der Seitenauswahl)
    LoadAllPages,
    SetBackground(PageBackground),
    ExportPdf,
    ZoomIn,
    ZoomOut,
    ResetZoom,
    DeleteSelected,
    CopySelected,
    PasteSelected,
    RotateSelected,
    // Dokumenten- & Notizbuch-Verwaltung
    OpenNotebook(String),
    CreateNotebook(String),
    RenameNotebook {
        id: String,
        new_title: String,
    },
    DeleteNotebook(String),
    UndoDeleteNotebook,
    RestoreNotebookBackup(String),
    OpenNotebookFolder(String),
    RefreshNotebooks,
    // Gitea Release & Updater
    CheckForUpdates,
    DownloadUpdateAsset(crate::updater::GiteaAsset),
    InstallOrOpenUpdate(std::path::PathBuf),
    OpenUrlInBrowser(String),
    SaveUpdaterConfig {
        server_url: String,
        auth_token: Option<String>,
    },
    // Datenintegrität / Quarantäne
    UnlockCorruptedPage(u64),
    /// Werkzeugleiste an die andere Fensterseite verlegen (wird gespeichert)
    SetSidebarSide(SidebarSide),
    /// Papierfarbe der Anzeige und Aufhellen dunkler Tinte (wird gespeichert)
    SetPaperTheme {
        theme: PaperTheme,
        adapt_ink: bool,
    },
    /// Druckkurve speichern (gilt während des Ziehens bereits über `UiState::pressure_curve`)
    SavePressureCurve(PressureCurve),
    /// Seiten durchgehend untereinander statt einzeln anzeigen (wird gespeichert)
    SetContinuousScroll(bool),
    /// Farbe von Karos, Linien und Punkten (wird gespeichert, gilt auch für den Export)
    SetPatternColor(PatternColor),
    /// Bilddatei auf der aktuellen Seite einfügen
    InsertImageFile(std::path::PathBuf),
    /// PDF als neues Notizbuch importieren
    ImportPdf(std::path::PathBuf),
    /// Aktuelle Seite als Bild oder SVG exportieren
    ExportPage(PageExportKind),
    /// Titel einer Seite setzen (leer = entfernen)
    SetPageTitle {
        page_id: u64,
        title: String,
    },
    // Beenden-Sicherheit bei Fehlschlag
    RetryShutdown,
    ExportRecoveryAndExit,
    ForceExit,
}

/// Zustand der grafischen Benutzeroberfläche.
#[derive(Debug, Clone)]
pub struct UiState {
    pub active_tool: ActiveTool,
    pub pen_color: [f32; 4],
    pub highlighter_color: [f32; 4],
    pub pen_stroke_width_mm: f32,
    pub highlighter_stroke_width_mm: f32,
    pub export_message: Option<(String, Instant)>,
    pub show_page_jump_popup: bool,
    /// Aktueller Zoomfaktor zur Anzeige in der Toolbar
    pub current_zoom: f32,
    /// Bildschirm-Rechteck der schwebenden Toolbar für pixelgenaues Hit-Testing
    pub toolbar_rect: Option<egui::Rect>,
    /// Öffnungszustand des Einstellungs-Popovers für den Stift
    pub is_pen_menu_open: bool,
    /// Öffnungszustand des Einstellungs-Popovers für den Textmarker
    pub is_highlighter_menu_open: bool,
    /// Bildschirm-Rechteck des geöffneten Einstellungs-Popovers
    pub popover_rect: Option<egui::Rect>,
    /// Dokumenten-Manager Zustand
    pub is_document_menu_open: bool,
    pub current_notebook_id: String,
    pub document_title: String,
    pub available_notebooks: Vec<NotebookSummary>,
    pub new_notebook_title_input: String,
    pub renaming_notebook_id: Option<String>,
    pub renaming_title_input: String,
    pub document_modal_rect: Option<egui::Rect>,
    /// Gitea Release- & Update-Manager
    pub is_update_dialog_open: bool,
    pub updater_server_url_input: String,
    pub updater_token_input: String,
    pub show_updater_config: bool,
    pub update_modal_rect: Option<egui::Rect>,
    /// Steuert, ob Touch-Eingaben mit einem Finger zeichnen oder das Dokument verschieben (Default: false = reine Gesten-Navigation & Zoom)
    pub touch_draw_enabled: bool,
    /// Zuletzt gelöschtes Notizbuch für Rückgängig während der Sitzung (Papierkorb-Pfad, Titel)
    pub last_deleted_notebook: Option<(std::path::PathBuf, String)>,
    /// Stifte der Werkzeugleiste (Farbe und Stärke) und der gewählte davon; `pen_color` und
    /// `pen_stroke_width_mm` gehören immer zum gewählten Stift
    pub pen_presets: [PenPreset; PEN_PRESET_COUNT],
    pub active_pen: usize,
    /// Rückfrage „Seite löschen?“ im ☰-Menü ist offen
    pub confirm_delete_page: bool,
    /// Seitenzahl der zuletzt gelöschten Seite, solange sie wiederhergestellt werden kann
    pub restorable_page: Option<usize>,
    /// Fehler beim Flushen vor dem Beenden
    pub shutdown_error: Option<crate::autosave::AutosaveError>,
    /// Fensterseite der Werkzeugleiste
    pub sidebar_side: SidebarSide,
    /// Öffnungszustand des ☰-Menüs
    pub is_more_menu_open: bool,
    /// Welche Striche der Radierer entfernt (gilt auch für Stiftrückseite und Seitentaste)
    pub eraser_mode: EraserMode,
    /// Papierfarbe der Anzeige
    pub paper_theme: PaperTheme,
    /// Dunkle Tinte auf dunklem Papier aufhellen
    pub adapt_ink_colors: bool,
    /// Farbe von Karos, Linien und Punkten
    pub pattern_color: PatternColor,
    /// Seiten hängen untereinander und werden durchgehend gescrollt
    pub continuous_scroll: bool,
    /// Text-Werkzeug: Farbe, Schriftgröße (Punkt) und Schriftart neuer Textfelder
    pub text_color: [f32; 4],
    pub text_size_pt: f32,
    pub text_font: TextFont,
    /// Laufende Bearbeitung eines Textfelds
    pub text_edit: Option<TextEditing>,
    /// Druckkurve des Stifts
    pub pressure_curve: PressureCurve,
    /// Zuletzt gemessener Rohdruck des Stifts (Anzeige in der Druckkurve)
    pub last_raw_pressure: Option<f32>,
    /// Geöffnete Dateiauswahl („Bild einfügen“ oder „PDF importieren“)
    pub file_picker: Option<FilePicker>,
    /// Eingeblendetes Lineal
    pub ruler: Option<Ruler>,
    pub ruler_drag: Option<RulerDrag>,
    /// Aktuelle Abbildung Seite ↔ Bildschirm (für Lineal und Treffertests)
    pub page_view: Option<PageView>,
    /// Fenstergröße in logischen Punkten (vom letzten Bild)
    pub screen_size: egui::Vec2,
    /// Eingabefeld „Titel dieser Seite“ in der Seitenübersicht und die Seite, zu der es gehört
    pub page_title_input: String,
    pub page_title_for: Option<u64>,
    /// Seitenübersicht als Liste mit Titeln (Inhaltsverzeichnis) statt Kacheln
    pub page_picker_list: bool,
    /// Aktionen, die außerhalb der Oberfläche ausgelöst wurden (z. B. ins Fenster gezogene
    /// PDF-Datei); `render_ui` gibt sie beim nächsten Bild mit aus
    pub queued_actions: Vec<UiAction>,
    /// Zuletzt in der Dateiauswahl benutzter Ordner (während der Sitzung)
    pub last_picker_dir: Option<std::path::PathBuf>,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            active_tool: ActiveTool::Pen,
            pen_color: PEN_COLOR_PRESETS[0].0,
            highlighter_color: HIGHLIGHTER_COLOR_PRESETS[0].0,
            pen_stroke_width_mm: 0.5, // Standard: 0.5 mm (feine Handschrift)
            highlighter_stroke_width_mm: 6.0,
            export_message: None,
            show_page_jump_popup: false,
            current_zoom: 1.0,
            toolbar_rect: None,
            is_pen_menu_open: false,
            is_highlighter_menu_open: false,
            popover_rect: None,
            is_document_menu_open: false,
            current_notebook_id: "default".to_string(),
            document_title: "Standard-Notizbuch".to_string(),
            available_notebooks: Vec::new(),
            new_notebook_title_input: String::new(),
            renaming_notebook_id: None,
            renaming_title_input: String::new(),
            document_modal_rect: None,
            is_update_dialog_open: false,
            updater_server_url_input: "https://gitea.pansi.eu".to_string(),
            updater_token_input: String::new(),
            show_updater_config: false,
            update_modal_rect: None,
            touch_draw_enabled: false,
            last_deleted_notebook: None,
            pen_presets: DEFAULT_PEN_PRESETS,
            active_pen: 0,
            confirm_delete_page: false,
            restorable_page: None,
            shutdown_error: None,
            sidebar_side: SidebarSide::Left,
            is_more_menu_open: false,
            eraser_mode: EraserMode::All,
            paper_theme: PaperTheme::White,
            adapt_ink_colors: true,
            pattern_color: PatternColor::Light,
            continuous_scroll: true,
            text_color: PEN_COLOR_PRESETS[0].0,
            text_size_pt: crate::text::DEFAULT_TEXT_SIZE_PT,
            text_font: TextFont::Sans,
            text_edit: None,
            pressure_curve: PressureCurve::default(),
            last_raw_pressure: None,
            file_picker: None,
            queued_actions: Vec::new(),
            ruler: None,
            ruler_drag: None,
            page_view: None,
            screen_size: egui::Vec2::ZERO,
            page_title_input: String::new(),
            page_title_for: None,
            page_picker_list: false,
            last_picker_dir: None,
        }
    }
}

impl UiState {
    /// Radius der abgerundeten Ecken der Werkzeugleiste.
    pub const TOOLBAR_ROUNDING: f32 = 14.0;
    /// Kantenlänge der Symbol-Knöpfe (stift- und fingertauglich).
    pub const SIDEBAR_BUTTON: f32 = 40.0;
    /// Abstand der Werkzeugleiste zum Fensterrand.
    pub const SIDEBAR_MARGIN: f32 = 10.0;

    /// Schließt alle Popover und Dialoge. Wird vor dem Öffnen eines neuen aufgerufen, damit
    /// sich nie mehrere überlagern.
    pub fn close_overlays(&mut self) {
        self.confirm_delete_page = false;
        self.is_pen_menu_open = false;
        self.is_highlighter_menu_open = false;
        self.is_more_menu_open = false;
        self.show_page_jump_popup = false;
        self.is_document_menu_open = false;
        self.is_update_dialog_open = false;
        self.file_picker = None;
    }

    /// Blendet das Lineal ein (waagerecht in der Mitte des sichtbaren Blattbereichs) oder aus.
    pub fn toggle_ruler(&mut self) {
        self.ruler_drag = None;
        self.ruler = match self.ruler {
            Some(_) => None,
            None => {
                let center = self
                    .page_view
                    .map(|v| {
                        let screen = self.visible_canvas_center();
                        v.to_page(screen)
                    })
                    .unwrap_or(glam::Vec2::new(
                        crate::document::PAGE_WIDTH / 2.0,
                        crate::document::PAGE_HEIGHT / 2.0,
                    ));
                Some(Ruler::new(center))
            }
        };
    }

    /// Mitte des Bereichs neben der Werkzeugleiste (logische Punkte).
    fn visible_canvas_center(&self) -> egui::Pos2 {
        (self.screen_size / 2.0).to_pos2()
            + egui::vec2(
                match self.sidebar_side {
                    SidebarSide::Left => self.sidebar_reserved_width() / 2.0,
                    SidebarSide::Right => -self.sidebar_reserved_width() / 2.0,
                },
                0.0,
            )
    }

    /// Öffnet die Dateiauswahl „Bild einfügen“ im zuletzt benutzten Ordner.
    pub fn open_image_picker(&mut self) {
        self.open_file_picker(PickerKind::Image);
    }

    /// Öffnet die Dateiauswahl „PDF importieren“ im zuletzt benutzten Ordner.
    pub fn open_pdf_picker(&mut self) {
        self.open_file_picker(PickerKind::Pdf);
    }

    fn open_file_picker(&mut self, kind: PickerKind) {
        let dir = self.last_picker_dir.clone();
        self.close_overlays();
        self.file_picker = Some(FilePicker::open(kind, dir.as_deref()));
    }

    /// Länge der längsten Farbpalette; so viel Platz hält die Leiste immer frei.
    pub const MAX_PALETTE_LEN: usize = 5;

    /// Durchmesser der Farbtupfer passend zur Knopfgröße.
    pub fn sidebar_swatch_size(button_size: f32) -> f32 {
        button_size - 10.0
    }

    /// Größte Knopfgröße (40 px, schrittweise bis 28 px), bei der die Leiste mit allen Elementen
    /// des aktiven Werkzeugs in `max_height` passt. `continuous`: durchgehende Ansicht (ohne
    /// Blätterpfeile).
    pub fn sidebar_button_size(tool: ActiveTool, max_height: f32, continuous: bool) -> f32 {
        let select = matches!(tool, ActiveTool::Select | ActiveTool::Text);
        // Notizbücher, 6 Werkzeuge, Undo/Redo, Seitenzahl und neue Seite (einzeln blättern:
        // dazu zwei Pfeile), Menü (+ 4 Auswahl-Aktionen bzw. Schriftknöpfe)
        let page_buttons = if continuous { 2 } else { 4 };
        let buttons = 10 + page_buttons + if select { 4 } else { 0 };
        let dividers = 5 + usize::from(select);
        let spacing = 3.0;
        let needed = |b: f32| {
            buttons as f32 * (b + spacing)
                + Self::MAX_PALETTE_LEN as f32 * (Self::sidebar_swatch_size(b) + spacing)
                + dividers as f32 * (7.0 + spacing)
                + 12.0
        };
        [Self::SIDEBAR_BUTTON, 36.0, 32.0, 30.0, 28.0]
            .into_iter()
            .find(|&b| needed(b) <= max_height)
            .unwrap_or(28.0)
    }

    /// Von der Werkzeugleiste belegte Breite (logische Punkte, inkl. Randabstand),
    /// damit die Seite daneben zentriert werden kann.
    pub fn sidebar_reserved_width(&self) -> f32 {
        self.toolbar_rect
            .map(|r| r.width() + 2.0 * Self::SIDEBAR_MARGIN)
            .unwrap_or(0.0)
    }

    pub fn new() -> Self {
        Self::default()
    }

    /// Wählt Stift `index`: Farbe und Stärke kommen aus seiner Vorlage.
    pub fn select_pen(&mut self, index: usize) {
        self.active_pen = index.min(PEN_PRESET_COUNT - 1);
        let preset = self.pen_presets[self.active_pen];
        self.pen_color = preset.color;
        self.pen_stroke_width_mm = preset.width_mm;
    }

    /// Übernimmt geänderte Farbe oder Stärke des Stifts in die Vorlage des gewählten Stifts;
    /// `true`, wenn sie sich geändert hat (dann speichern).
    pub fn sync_pen_preset(&mut self) -> bool {
        let current = PenPreset {
            color: self.pen_color,
            width_mm: self.pen_stroke_width_mm,
        };
        let preset = &mut self.pen_presets[self.active_pen.min(PEN_PRESET_COUNT - 1)];
        if *preset == current {
            return false;
        }
        *preset = current;
        true
    }

    /// Gibt die aktuelle Farbe des gewählten Werkzeugs zurück.
    pub fn current_color(&self) -> [f32; 4] {
        match self.active_tool {
            ActiveTool::Pen | ActiveTool::Shape | ActiveTool::Select => self.pen_color,
            ActiveTool::Highlighter => self.highlighter_color,
            ActiveTool::Text => self.text_color,
            ActiveTool::Eraser => [0.0, 0.0, 0.0, 0.0],
        }
    }

    /// Gibt die aktuelle Strichbreite in mm zurück.
    pub fn current_stroke_width_mm(&self) -> f32 {
        match self.active_tool {
            ActiveTool::Pen | ActiveTool::Shape | ActiveTool::Select | ActiveTool::Text => {
                self.pen_stroke_width_mm
            }
            ActiveTool::Highlighter => self.highlighter_stroke_width_mm,
            ActiveTool::Eraser => 3.0,
        }
    }

    /// Zeigt eine temporäre Erfolgs- oder Statusmeldung im UI an.
    pub fn set_export_message(&mut self, msg: impl Into<String>) {
        self.export_message = Some((msg.into(), Instant::now()));
    }

    /// Exaktes Hit-Testing: Prüft, ob ein Bildschirmpunkt (px, py) innerhalb der
    /// abgerundeten Pill-Toolbar liegt (unter Berücksichtigung der 4 Eckradien).
    pub fn is_point_inside_toolbar(&self, px: f32, py: f32) -> bool {
        let Some(rect) = self.toolbar_rect else {
            return false;
        };

        if px < rect.min.x || px > rect.max.x || py < rect.min.y || py > rect.max.y {
            return false;
        }

        let r = Self::TOOLBAR_ROUNDING;

        // 1. Oben-links
        if px < rect.min.x + r && py < rect.min.y + r {
            let dx = px - (rect.min.x + r);
            let dy = py - (rect.min.y + r);
            return dx * dx + dy * dy <= r * r;
        }
        // 2. Oben-rechts
        if px > rect.max.x - r && py < rect.min.y + r {
            let dx = px - (rect.max.x - r);
            let dy = py - (rect.min.y + r);
            return dx * dx + dy * dy <= r * r;
        }
        // 3. Unten-links
        if px < rect.min.x + r && py > rect.max.y - r {
            let dx = px - (rect.min.x + r);
            let dy = py - (rect.max.y - r);
            return dx * dx + dy * dy <= r * r;
        }
        // 4. Unten-rechts
        if px > rect.max.x - r && py > rect.max.y - r {
            let dx = px - (rect.max.x - r);
            let dy = py - (rect.max.y - r);
            return dx * dx + dy * dy <= r * r;
        }

        true
    }

    /// Prüft, ob ein Bildschirmpunkt (px, py) innerhalb der UI liegt:
    /// - Entweder innerhalb der abgerundeten Pill-Toolbar
    /// - ODER innerhalb des geöffneten Einstellungs-Popovers
    /// - ODER innerhalb geöffneter Modalfenster (Dokumenten-Manager, Update-Center)
    pub fn is_point_in_ui(&self, px: f32, py: f32) -> bool {
        if self.is_point_inside_toolbar(px, py) {
            return true;
        }
        // Lineal: Fläche und Dreh-Griff bedient die Oberfläche, die Kanten bleiben zum Zeichnen
        if self.ruler_drag.is_some() {
            return true;
        }
        if let (Some(ruler), Some(view)) = (self.ruler, self.page_view) {
            if ruler.hit(view.to_page(egui::pos2(px, py))).is_some() {
                return true;
            }
        }
        if let Some(pop_rect) = self.popover_rect {
            if pop_rect.contains(egui::pos2(px, py)) {
                return true;
            }
        }
        if self.is_document_menu_open {
            if let Some(doc_rect) = self.document_modal_rect {
                if doc_rect.contains(egui::pos2(px, py)) {
                    return true;
                }
            }
        }
        if self.is_update_dialog_open {
            if let Some(upd_rect) = self.update_modal_rect {
                if upd_rect.contains(egui::pos2(px, py)) {
                    return true;
                }
            }
        }
        false
    }
}

/// Dicken-Preset-Button mit visueller Linien-Vorschau in Originalfarbe und Proportion.
fn stroke_preview_button(
    ui: &mut egui::Ui,
    is_active: bool,
    stroke_width_mm: f32,
    label_text: &str,
    color: [f32; 4],
) -> egui::Response {
    let size = egui::vec2(66.0, 40.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());

    if ui.is_rect_visible(rect) {
        let (bg, border) = if is_active {
            (
                egui::Color32::from_rgba_premultiplied(50, 85, 150, 220),
                egui::Stroke::new(
                    1.0_f32,
                    egui::Color32::from_rgba_unmultiplied(120, 175, 255, 200),
                ),
            )
        } else if response.hovered() {
            (
                egui::Color32::from_rgba_unmultiplied(255, 255, 255, 16),
                egui::Stroke::new(
                    1.0_f32,
                    egui::Color32::from_rgba_unmultiplied(255, 255, 255, 25),
                ),
            )
        } else {
            (
                egui::Color32::from_rgba_unmultiplied(255, 255, 255, 8),
                egui::Stroke::new(
                    1.0_f32,
                    egui::Color32::from_rgba_unmultiplied(255, 255, 255, 14),
                ),
            )
        };

        ui.painter()
            .rect_filled(rect, egui::Rounding::same(6.0), bg);
        ui.painter()
            .rect_stroke(rect, egui::Rounding::same(6.0), border);

        // Horizontale Vorschau-Linie zeichnen
        let line_thickness = (stroke_width_mm * 2.5).clamp(1.0, 9.0);
        let start_x = rect.min.x + 8.0;
        let end_x = rect.max.x - 8.0;
        let y = rect.center().y - 4.5;

        let line_color = egui::Color32::from_rgba_unmultiplied(
            (color[0] * 255.0) as u8,
            (color[1] * 255.0) as u8,
            (color[2] * 255.0) as u8,
            ((color[3] * 255.0) as u8).max(180),
        );
        ui.painter().line_segment(
            [egui::pos2(start_x, y), egui::pos2(end_x, y)],
            egui::Stroke::new(line_thickness, line_color),
        );

        // Text unterhalb der Linie (z. B. "2.0 mm")
        let text_pos = egui::pos2(rect.center().x, rect.max.y - 7.5);
        ui.painter().text(
            text_pos,
            egui::Align2::CENTER_CENTER,
            label_text,
            egui::FontId::proportional(10.0),
            if is_active {
                egui::Color32::WHITE
            } else {
                egui::Color32::from_gray(190)
            },
        );
    }

    response.on_hover_text(format!("Stärke auf {} setzen", label_text))
}

/// Inhalt eines Symbol-Knopfs der Werkzeugleiste.
#[derive(Clone, Copy)]
enum SidebarIcon<'a> {
    /// Zeichen aus den eingebauten egui-Schriften
    Glyph(&'a str),
    /// Kurzer Text (z. B. Seitenzahl) in kleinerer Schrift
    Text(&'a str),
    /// Selbst gezeichneter Radiergummi (kein passendes Zeichen in den egui-Schriften)
    Eraser,
    /// Selbst gezeichnetes gestricheltes Auswahlrechteck
    Lasso,
    /// Selbst gezeichnetes Lineal mit Skala
    Ruler,
}

/// Quadratischer Symbol-Knopf der Werkzeugleiste mit optionalem Farbbalken an der Unterkante.
fn sidebar_button(
    ui: &mut egui::Ui,
    size: f32,
    icon: SidebarIcon,
    is_active: bool,
    enabled: bool,
    indicator: Option<[f32; 4]>,
    tooltip: &str,
) -> egui::Response {
    let size = egui::vec2(size, size);
    let sense = if enabled {
        egui::Sense::click()
    } else {
        egui::Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(size, sense);

    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let bg = if is_active {
            egui::Color32::from_rgba_unmultiplied(45, 80, 145, 230)
        } else if enabled && response.hovered() {
            egui::Color32::from_white_alpha(18)
        } else {
            egui::Color32::TRANSPARENT
        };
        painter.rect_filled(rect, egui::Rounding::same(8.0), bg);
        if is_active {
            painter.rect_stroke(
                rect,
                egui::Rounding::same(8.0),
                egui::Stroke::new(
                    1.0_f32,
                    egui::Color32::from_rgba_unmultiplied(110, 165, 255, 190),
                ),
            );
        }

        let color = if !enabled {
            egui::Color32::from_gray(95)
        } else if is_active {
            egui::Color32::WHITE
        } else {
            egui::Color32::from_gray(220)
        };
        let c = rect.center();
        match icon {
            SidebarIcon::Glyph(g) => {
                painter.text(
                    c,
                    egui::Align2::CENTER_CENTER,
                    g,
                    egui::FontId::proportional(19.0),
                    color,
                );
            }
            SidebarIcon::Text(t) => {
                painter.text(
                    c,
                    egui::Align2::CENTER_CENTER,
                    t,
                    egui::FontId::proportional(12.0),
                    color,
                );
            }
            SidebarIcon::Eraser => {
                // Schräg liegender Radiergummi: vordere Hälfte hell, hintere Hälfte farbig
                let dir = egui::vec2(1.0, -1.0).normalized();
                let perp = egui::vec2(dir.y, -dir.x) * -1.0;
                let (half_len, half_w) = (10.0, 5.5);
                let p = |along: f32, across: f32| c + dir * along + perp * across;
                let body = vec![
                    p(-half_len, -half_w),
                    p(0.0, -half_w),
                    p(0.0, half_w),
                    p(-half_len, half_w),
                ];
                let tip = vec![
                    p(0.0, -half_w),
                    p(half_len, -half_w),
                    p(half_len, half_w),
                    p(0.0, half_w),
                ];
                let stroke = egui::Stroke::new(1.5_f32, color);
                painter.add(egui::Shape::convex_polygon(
                    tip,
                    egui::Color32::from_rgb(232, 120, 140).gamma_multiply(if enabled {
                        1.0
                    } else {
                        0.4
                    }),
                    stroke,
                ));
                painter.add(egui::Shape::convex_polygon(
                    body,
                    egui::Color32::TRANSPARENT,
                    stroke,
                ));
            }
            SidebarIcon::Ruler => {
                let dir = egui::vec2(1.0, -1.0).normalized();
                let perp = egui::vec2(dir.y, -dir.x);
                let (half_len, half_w) = (12.0, 4.5);
                let p = |along: f32, across: f32| c + dir * along + perp * across;
                painter.add(egui::Shape::convex_polygon(
                    vec![
                        p(-half_len, -half_w),
                        p(half_len, -half_w),
                        p(half_len, half_w),
                        p(-half_len, half_w),
                    ],
                    egui::Color32::TRANSPARENT,
                    egui::Stroke::new(1.5_f32, color),
                ));
                for i in 1..6 {
                    let along = -half_len + i as f32 * 4.0;
                    let tick = if i % 2 == 0 { 4.0 } else { 2.5 };
                    painter.line_segment(
                        [p(along, -half_w), p(along, -half_w + tick)],
                        egui::Stroke::new(1.2_f32, color),
                    );
                }
            }
            SidebarIcon::Lasso => {
                let r = egui::Rect::from_center_size(c, egui::vec2(20.0, 16.0));
                let pts = [
                    r.left_top(),
                    r.right_top(),
                    r.right_bottom(),
                    r.left_bottom(),
                    r.left_top(),
                ];
                painter.extend(egui::Shape::dashed_line(
                    &pts,
                    egui::Stroke::new(1.6_f32, color),
                    3.5,
                    2.5,
                ));
            }
        }

        if let Some(ind) = indicator {
            let bar = egui::Rect::from_min_max(
                egui::pos2(rect.min.x + 9.0, rect.max.y - 5.0),
                egui::pos2(rect.max.x - 9.0, rect.max.y - 2.0),
            );
            painter.rect_filled(bar, egui::Rounding::same(1.5), color32_from(ind));
        }
    }

    if tooltip.is_empty() {
        response
    } else {
        response.on_hover_text(tooltip)
    }
}

/// Waagerechte Trennlinie zwischen Gruppen der Werkzeugleiste.
fn sidebar_divider(ui: &mut egui::Ui, button_size: f32) {
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(button_size - 12.0, 7.0), egui::Sense::hover());
    ui.painter().hline(
        rect.x_range(),
        rect.center().y,
        egui::Stroke::new(
            1.0_f32,
            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 25),
        ),
    );
}

/// Wandelt eine RGBA-Farbe (0..1) in eine gut sichtbare egui-Farbe um.
fn color32_from(color: [f32; 4]) -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(
        (color[0].clamp(0.0, 1.0) * 255.0) as u8,
        (color[1].clamp(0.0, 1.0) * 255.0) as u8,
        (color[2].clamp(0.0, 1.0) * 255.0) as u8,
        ((color[3].clamp(0.0, 1.0) * 255.0) as u8).max(180),
    )
}

/// Position und Ankerpunkt eines Popovers neben einem Knopf der Werkzeugleiste,
/// immer zur Blattseite hin geöffnet.
fn popover_placement(
    side: SidebarSide,
    toolbar: egui::Rect,
    button: egui::Rect,
) -> (egui::Pos2, egui::Align2) {
    match side {
        SidebarSide::Left => (
            egui::pos2(toolbar.max.x + 8.0, button.min.y),
            egui::Align2::LEFT_TOP,
        ),
        SidebarSide::Right => (
            egui::pos2(toolbar.min.x - 8.0, button.min.y),
            egui::Align2::RIGHT_TOP,
        ),
    }
}

/// Farbe und Beschriftung der Speicherstatus-Anzeige.
fn autosave_status_style(status: AutosaveStatus) -> (egui::Color32, &'static str) {
    match status {
        AutosaveStatus::Saved => (egui::Color32::from_rgb(46, 204, 113), "Alles gespeichert"),
        AutosaveStatus::Saving => (egui::Color32::from_rgb(241, 196, 15), "Speichert …"),
        AutosaveStatus::Pending => (
            egui::Color32::from_rgb(230, 126, 34),
            "Ungespeicherte Änderungen",
        ),
        AutosaveStatus::Error => (egui::Color32::from_rgb(231, 76, 60), "Speicherfehler!"),
    }
}

/// Zeichnet einen runden Farbauswahl-Knopf (Color Swatch) mit anpassbarem Durchmesser.
/// Stift der Werkzeugleiste: ein Strich in Farbe und (angedeuteter) Stärke auf einem
/// Stück Papier, damit auch schwarze Tinte auf der dunklen Leiste sichtbar ist.
fn pen_preset_swatch(
    ui: &mut egui::Ui,
    preset: PenPreset,
    is_selected: bool,
    tooltip: &str,
    size: f32,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let paper = rect.shrink(2.0);
        ui.painter().rect_filled(
            paper,
            egui::Rounding::same(5.0),
            egui::Color32::from_gray(245),
        );
        let [r, g, b, _] = preset.color.map(|c| (c.clamp(0.0, 1.0) * 255.0) as u8);
        let width = (1.0 + preset.width_mm * 2.5).clamp(1.5, paper.height() * 0.35);
        let inset = 3.0 + width / 2.0;
        ui.painter().line_segment(
            [
                egui::pos2(paper.left() + inset, paper.bottom() - inset),
                egui::pos2(paper.right() - inset, paper.top() + inset),
            ],
            egui::Stroke::new(width, egui::Color32::from_rgb(r, g, b)),
        );
        if is_selected {
            ui.painter().rect_stroke(
                rect.shrink(0.5),
                egui::Rounding::same(7.0),
                egui::Stroke::new(2.0_f32, egui::Color32::WHITE),
            );
        } else if response.hovered() {
            ui.painter().rect_stroke(
                rect.shrink(1.0),
                egui::Rounding::same(7.0),
                egui::Stroke::new(1.0_f32, egui::Color32::from_white_alpha(150)),
            );
        }
    }
    response.on_hover_text(tooltip)
}

fn color_circle_swatch(
    ui: &mut egui::Ui,
    color: [f32; 4],
    is_selected: bool,
    tooltip: &str,
    diameter: f32,
) -> egui::Response {
    let size = egui::vec2(diameter, diameter);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());

    if ui.is_rect_visible(rect) {
        let r = (color[0].clamp(0.0, 1.0) * 255.0) as u8;
        let g = (color[1].clamp(0.0, 1.0) * 255.0) as u8;
        let b = (color[2].clamp(0.0, 1.0) * 255.0) as u8;
        let a = ((color[3].clamp(0.0, 1.0) * 255.0) as u8).max(180);
        let c32 = egui::Color32::from_rgba_unmultiplied(r, g, b, a);

        // Füllung bleibt immer sichtbar; die Auswahl ist ein Ring mit Abstand außen herum,
        // dunkle Farben bekommen eine helle Kontur gegen den dunklen Hintergrund
        let outer = (diameter / 2.0) - 1.0;
        let fill_radius = outer - 3.0;
        let outline = if response.hovered() {
            egui::Stroke::new(1.5_f32, egui::Color32::from_white_alpha(200))
        } else {
            egui::Stroke::new(1.0_f32, egui::Color32::from_white_alpha(110))
        };
        ui.painter().circle_filled(rect.center(), fill_radius, c32);
        ui.painter()
            .circle_stroke(rect.center(), fill_radius, outline);
        if is_selected {
            ui.painter().circle_stroke(
                rect.center(),
                outer,
                egui::Stroke::new(2.0_f32, egui::Color32::WHITE),
            );
        }
    }

    response.on_hover_text(tooltip)
}

/// Spalten der Seitenauswahl.
const PAGE_PICKER_COLUMNS: usize = 4;
/// Größe einer Kachel der Seitenauswahl: A4-Vorschau plus Zeile für die Seitenzahl.
const PAGE_TILE: egui::Vec2 = egui::vec2(66.0, 66.0 * PAGE_HEIGHT / PAGE_WIDTH + 18.0);
/// Höchstzahl gezeichneter Punkte pro Vorschau (dünnt volle Seiten aus).
const THUMBNAIL_POINT_BUDGET: usize = 1500;

/// Zeichenbefehle für die verkleinerte Vorschau einer Seite in `rect` in der Papierfarbe
/// `paper`: so groß wie möglich, waagerecht zentriert und oben (Querformat-Seiten sind
/// flacher). Striche werden als Linien gezeichnet und bei vielen Punkten gleichmäßig
/// ausgedünnt; `adapt_ink` hellt dunkle Tinte wie auf dem Blatt auf.
pub fn page_thumbnail_shapes(
    page: &Page,
    rect: egui::Rect,
    paper: PaperTheme,
    adapt_ink: bool,
) -> Vec<egui::Shape> {
    let scale = (rect.width() / page.size.x).min(rect.height() / page.size.y);
    let size = egui::vec2(page.size.x, page.size.y) * scale;
    let sheet = egui::Rect::from_min_size(
        rect.min + egui::vec2((rect.width() - size.x) / 2.0, 0.0),
        size,
    );
    let mut shapes = vec![egui::Shape::rect_filled(
        sheet,
        egui::Rounding::same(3.0),
        linear_color32(paper.paper_color()),
    )];
    let adapt = adapt_ink && paper.is_dark();
    let to_screen = |p: glam::Vec2| sheet.min + egui::vec2(p.x * scale, p.y * scale);

    // Importierte PDF-Seite als helle Fläche mit Rand
    if let Some(pdf) = &page.pdf_background {
        let points: Vec<egui::Pos2> = pdf
            .corners(page.size)
            .iter()
            .map(|&c| to_screen(c))
            .collect();
        shapes.push(egui::Shape::convex_polygon(
            points,
            egui::Color32::from_rgb(236, 238, 243),
            egui::Stroke::new(0.8_f32, egui::Color32::from_gray(150)),
        ));
    }

    // Bilder als Flächen unter der Tinte
    for corners in page.strokes.iter().filter_map(|s| s.image_corners()) {
        shapes.push(egui::Shape::convex_polygon(
            corners.iter().map(|&c| to_screen(c)).collect(),
            egui::Color32::from_rgb(168, 182, 200),
            egui::Stroke::NONE,
        ));
    }

    let total: usize = page.strokes.iter().map(|s| s.points.len()).sum();
    let step = total.div_ceil(THUMBNAIL_POINT_BUDGET).max(1);

    // Textfelder: je Zeile ein Strich in Textfarbe auf Höhe der Kleinbuchstaben
    for stroke in &page.strokes {
        let (Some(text), Some(corners)) = (&stroke.text, stroke.object_corners()) else {
            continue;
        };
        let ink = if adapt {
            adapt_ink_color(stroke.color)
        } else {
            stroke.color
        };
        let color = color32_from(ink).gamma_multiply(0.7);
        let layout = text.layout();
        let (origin, [right, down]) = crate::text::frame_axes(&corners, text.box_size());
        let width = (text.size * 0.45 * down.length() * scale).clamp(0.6, 6.0);
        for (i, line) in layout.lines.iter().enumerate() {
            if line.width <= 0.0 {
                continue;
            }
            let start = origin + down * (layout.baseline(i) - text.size * 0.25);
            shapes.push(egui::Shape::line_segment(
                [to_screen(start), to_screen(start + right * line.width)],
                egui::Stroke::new(width, color),
            ));
        }
    }

    for stroke in page.strokes.iter().filter(|s| !s.is_object()) {
        let ink = if adapt {
            adapt_ink_color(stroke.color)
        } else {
            stroke.color
        };
        let mut color = color32_from(ink);
        if stroke.tool == StrokeTool::Highlighter {
            color = color.gamma_multiply(0.45);
        }
        let width = (stroke.width * scale).clamp(0.6, 4.0);
        let last = stroke.points.len().saturating_sub(1);
        let points: Vec<egui::Pos2> = stroke
            .points
            .iter()
            .enumerate()
            .filter(|(i, _)| i % step == 0 || *i == last)
            .map(|(_, s)| to_screen(s.position))
            .collect();
        match points.len() {
            0 => {}
            1 => shapes.push(egui::Shape::circle_filled(points[0], width / 2.0, color)),
            _ => shapes.push(egui::Shape::line(points, egui::Stroke::new(width, color))),
        }
    }
    shapes
}

/// Knöpfe für Schriftart und -größe des Text-Werkzeugs. Änderungen gelten auch für das
/// gerade bearbeitete Textfeld.
fn text_style_buttons(ui: &mut egui::Ui, btn: f32, state: &mut UiState) {
    let size = state.text_size_pt;
    let smaller = TEXT_SIZES_PT
        .iter()
        .rev()
        .copied()
        .find(|&s| s < size - 0.01);
    let larger = TEXT_SIZES_PT.iter().copied().find(|&s| s > size + 0.01);
    let smaller_tip = format!("Kleinere Schrift (jetzt {size} pt)");
    let larger_tip = format!("Größere Schrift (jetzt {size} pt)");
    enum Change {
        Font(TextFont),
        /// Neue Größe; `None`: keine kleinere bzw. größere mehr
        Size(Option<f32>),
    }
    let buttons = [
        (
            SidebarIcon::Text("Aa"),
            "Normale Schrift",
            Change::Font(TextFont::Sans),
        ),
        (
            SidebarIcon::Text("{ }"),
            "Code-Schrift (feste Zeichenbreite, Tab rückt ein)",
            Change::Font(TextFont::Mono),
        ),
        (SidebarIcon::Text("A−"), &smaller_tip, Change::Size(smaller)),
        (SidebarIcon::Text("A+"), &larger_tip, Change::Size(larger)),
    ];
    for (icon, tooltip, change) in buttons {
        let (active, enabled) = match change {
            Change::Font(font) => (state.text_font == font, true),
            Change::Size(new_size) => (false, new_size.is_some()),
        };
        if !sidebar_button(ui, btn, icon, active, enabled, None, tooltip).clicked() {
            continue;
        }
        match change {
            Change::Font(font) => state.text_font = font,
            Change::Size(Some(new_size)) => {
                state.text_size_pt = new_size;
                state.set_export_message(format!("Schriftgröße {new_size} pt"));
            }
            Change::Size(None) => {}
        }
        if let Some(edit) = &mut state.text_edit {
            edit.text.font = state.text_font;
            edit.text.size = state.text_size_pt * crate::text::UNITS_PER_PT;
            edit.focus = true;
        }
    }
}

/// Bearbeitung eines Textfelds: egui-Textfeld an der Stelle und in der Größe des Felds auf
/// dem Blatt (ungedreht), mit derselben Schrift. Esc beendet die Bearbeitung.
fn text_edit_overlay(ctx: &egui::Context, state: &mut UiState) {
    let adapt = state.adapt_ink_colors && state.paper_theme.is_dark();
    let (Some(view), Some(edit)) = (state.page_view, state.text_edit.as_mut()) else {
        return;
    };
    let [sx, sy] = edit.axes.map(|a| a.length());
    let font_size = (edit.text.size * sy * view.scale).clamp(2.0, 400.0);
    // egui rundet die Glyphenhöhe auf ganze Pixel: die Umbruchbreite im selben Verhältnis
    // anpassen, damit die Zeilen an denselben Stellen umbrechen wie auf dem Blatt
    let height_px = font_size * ctx.pixels_per_point() * edit.text.font.height_per_em();
    let rounding = height_px.round().max(1.0) / height_px;
    let width = (edit.text.wrap_width.max(edit.text.size) * sx * view.scale * rounding).max(8.0);
    let color = linear_color32(if adapt {
        adapt_ink_color(edit.color)
    } else {
        edit.color
    });
    egui::Area::new(egui::Id::new("text_edit_area"))
        .fixed_pos(view.to_screen(edit.origin))
        .order(egui::Order::Middle)
        .show(ctx, |ui| {
            egui::Frame::none()
                .stroke(egui::Stroke::new(
                    1.0_f32,
                    egui::Color32::from_rgba_unmultiplied(70, 130, 230, 200),
                ))
                .show(ui, |ui| {
                    let response = ui.add(
                        egui::TextEdit::multiline(&mut edit.text.content)
                            .id(egui::Id::new("text_edit_field"))
                            .frame(false)
                            .margin(egui::vec2(0.0, 0.0))
                            .font(egui::FontId::new(font_size, edit.text.font.egui_family()))
                            .text_color(color)
                            .desired_width(width)
                            .desired_rows(1)
                            .lock_focus(true),
                    );
                    if edit.focus {
                        response.request_focus();
                        edit.focus = false;
                    }
                    // egui 0.27 zählt Enter in einem fokussierten Feld als Klick und fordert
                    // den Fokus neu an – dabei geht die Tab-Sperre verloren und Tab würde
                    // danach den Fokus wegnehmen statt einzurücken
                    ui.memory_mut(|m| {
                        m.set_focus_lock_filter(
                            response.id,
                            egui::EventFilter {
                                tab: true,
                                horizontal_arrows: true,
                                vertical_arrows: true,
                                escape: false,
                            },
                        )
                    });
                });
        });
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        edit.finish = true;
    }
}

/// Linearer Farbwert (wie auf der GPU) als egui-Farbe (sRGB).
fn linear_color32(color: [f32; 4]) -> egui::Color32 {
    egui::Rgba::from_rgba_unmultiplied(color[0], color[1], color[2], color[3]).into()
}

/// Kachel der Seitenauswahl: Vorschau der Seite mit Seitenzahl darunter.
fn page_thumbnail(
    ui: &mut egui::Ui,
    state: &UiState,
    page: &Page,
    number: usize,
    is_current: bool,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(PAGE_TILE, egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let thumb = egui::Rect::from_min_size(
            rect.min,
            egui::vec2(PAGE_TILE.x, PAGE_TILE.x * PAGE_HEIGHT / PAGE_WIDTH),
        );
        let painter = ui.painter();
        painter.extend(page_thumbnail_shapes(
            page,
            thumb,
            state.paper_theme,
            state.adapt_ink_colors,
        ));
        if !page.is_loaded {
            painter.text(
                thumb.center(),
                egui::Align2::CENTER_CENTER,
                "…",
                egui::FontId::proportional(16.0),
                egui::Color32::from_gray(140),
            );
        }
        // Lesezeichen-Ecke für benannte Seiten
        if page.title.is_some() {
            let corner = thumb.right_top();
            painter.add(egui::Shape::convex_polygon(
                vec![
                    corner + egui::vec2(-14.0, 0.0),
                    corner,
                    corner + egui::vec2(0.0, 14.0),
                ],
                egui::Color32::from_rgb(90, 150, 255),
                egui::Stroke::NONE,
            ));
        }
        let border = if is_current {
            egui::Stroke::new(2.5_f32, egui::Color32::from_rgb(90, 150, 255))
        } else if response.hovered() {
            egui::Stroke::new(1.5_f32, egui::Color32::from_white_alpha(200))
        } else {
            egui::Stroke::new(1.0_f32, egui::Color32::from_white_alpha(60))
        };
        painter.rect_stroke(thumb, egui::Rounding::same(3.0), border);
        painter.text(
            egui::pos2(rect.center().x, rect.max.y - 8.0),
            egui::Align2::CENTER_CENTER,
            number.to_string(),
            egui::FontId::proportional(13.0),
            if is_current {
                egui::Color32::WHITE
            } else {
                egui::Color32::from_gray(180)
            },
        );
    }
    match &page.title {
        Some(title) => response.on_hover_text(format!("Seite {number} – {title}")),
        None => response.on_hover_text(format!("Seite {number}")),
    }
}

/// Inhaltsverzeichnis der Seitenübersicht: alle Seiten mit Nummer und Titel, benannte
/// Seiten hervorgehoben. Gibt die angetippte Seite zurück.
fn page_title_list(ui: &mut egui::Ui, notebook: &Notebook) -> Option<usize> {
    let mut chosen = None;
    if notebook.pages.iter().all(|p| p.title.is_none()) {
        ui.label(
            egui::RichText::new(
                "Noch keine Seite hat einen Titel. Gib oben einen Titel für die aktuelle Seite ein.",
            )
            .color(egui::Color32::from_gray(150)),
        );
        ui.add_space(4.0);
    }
    for (idx, page) in notebook.pages.iter().enumerate() {
        let is_current = idx == notebook.current_page_index;
        let text = match &page.title {
            Some(title) => egui::RichText::new(format!("{:>3}   {title}", idx + 1))
                .strong()
                .color(egui::Color32::WHITE),
            None => egui::RichText::new(format!("{:>3}   –", idx + 1))
                .color(egui::Color32::from_gray(130)),
        };
        let response = ui.add_sized(
            [ui.available_width(), 32.0],
            egui::SelectableLabel::new(is_current, text),
        );
        if response.clicked() {
            chosen = Some(idx);
        }
    }
    chosen
}

/// Auswahlknopf für eine Papierfarbe: Farbmuster und Name.
fn paper_theme_button(ui: &mut egui::Ui, theme: PaperTheme, selected: bool) -> egui::Response {
    let response = ui.add_sized(
        [115.0, 28.0],
        egui::SelectableLabel::new(selected, format!("     {}", theme.label())),
    );
    let swatch = egui::Rect::from_center_size(
        egui::pos2(response.rect.min.x + 14.0, response.rect.center().y),
        egui::vec2(14.0, 14.0),
    );
    ui.painter().rect_filled(
        swatch,
        egui::Rounding::same(3.0),
        linear_color32(theme.paper_color()),
    );
    ui.painter().rect_stroke(
        swatch,
        egui::Rounding::same(3.0),
        egui::Stroke::new(1.0_f32, egui::Color32::from_gray(120)),
    );
    response
}

/// Knopf für eine Musterfarbe: Vorschau (kleines Karo auf der aktuellen Papierfarbe) und
/// Bezeichnung.
fn pattern_color_button(
    ui: &mut egui::Ui,
    pattern: PatternColor,
    paper: PaperTheme,
    selected: bool,
) -> egui::Response {
    let response = ui.add_sized(
        [82.0, 28.0],
        egui::SelectableLabel::new(selected, format!("      {}", pattern.label())),
    );
    let swatch = egui::Rect::from_center_size(
        egui::pos2(response.rect.min.x + 15.0, response.rect.center().y),
        egui::vec2(16.0, 16.0),
    );
    let painter = ui.painter();
    painter.rect_filled(
        swatch,
        egui::Rounding::same(3.0),
        linear_color32(paper.paper_color()),
    );
    let line = egui::Stroke::new(1.5_f32, linear_color32(pattern.color(paper)));
    for t in [0.33, 0.67] {
        let x = swatch.min.x + swatch.width() * t;
        let y = swatch.min.y + swatch.height() * t;
        painter.line_segment(
            [egui::pos2(x, swatch.min.y), egui::pos2(x, swatch.max.y)],
            line,
        );
        painter.line_segment(
            [egui::pos2(swatch.min.x, y), egui::pos2(swatch.max.x, y)],
            line,
        );
    }
    painter.rect_stroke(
        swatch,
        egui::Rounding::same(3.0),
        egui::Stroke::new(1.0_f32, egui::Color32::from_gray(120)),
    );
    response.on_hover_text("Gilt für alle Seiten, auch beim Export")
}

/// Schieberegler für die Druckkurve mit Vorschau der Kennlinie. Änderungen gelten sofort
/// (`state.pressure_curve`); zurückgegeben wird die Kurve, sobald sie gespeichert werden soll
/// (Regler losgelassen oder per Tastatur geändert).
fn pressure_curve_editor(ui: &mut egui::Ui, state: &mut UiState) -> Option<PressureCurve> {
    let mut save = false;
    let mut track = |response: egui::Response| {
        if response.drag_stopped() || (response.changed() && !response.dragged()) {
            save = true;
        }
    };

    ui.add_space(4.0);
    ui.label("Druckkurve des Stifts");
    let (graph, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 56.0), egui::Sense::hover());
    let painter = ui.painter_at(graph);
    painter.rect_filled(
        graph,
        egui::Rounding::same(4.0),
        egui::Color32::from_black_alpha(90),
    );
    let to_screen = |raw: f32, out: f32| {
        egui::pos2(
            graph.min.x + 6.0 + raw * (graph.width() - 12.0),
            graph.max.y - 6.0 - out * (graph.height() - 12.0),
        )
    };
    painter.line_segment(
        [to_screen(0.0, 0.0), to_screen(1.0, 1.0)],
        egui::Stroke::new(1.0_f32, egui::Color32::from_white_alpha(40)),
    );
    let curve = state.pressure_curve;
    let points: Vec<egui::Pos2> = (0..=48)
        .map(|i| {
            let raw = i as f32 / 48.0;
            to_screen(raw, curve.apply(raw))
        })
        .collect();
    painter.add(egui::Shape::line(
        points,
        egui::Stroke::new(2.0_f32, egui::Color32::from_rgb(110, 165, 255)),
    ));
    if let Some(raw) = state.last_raw_pressure {
        let raw = raw.clamp(0.0, 1.0);
        painter.circle_filled(
            to_screen(raw, curve.apply(raw)),
            4.0,
            egui::Color32::from_rgb(255, 200, 80),
        );
    }

    ui.horizontal(|ui| {
        ui.label("weich");
        ui.spacing_mut().slider_width = (ui.available_width() - 40.0).max(60.0);
        track(ui.add(
            egui::Slider::new(&mut state.pressure_curve.exponent, PressureCurve::EXPONENT_RANGE)
                .logarithmic(true)
                .show_value(false),
        ).on_hover_text("Weich: schon leichter Druck ergibt kräftige Linien.\nHart: erst fester Druck ergibt kräftige Linien."));
        ui.label("hart");
    });
    ui.horizontal(|ui| {
        ui.label("Mindestdruck");
        ui.spacing_mut().slider_width = (ui.available_width() - 60.0).max(60.0);
        track(ui.add(
            egui::Slider::new(&mut state.pressure_curve.threshold, PressureCurve::THRESHOLD_RANGE)
                .custom_formatter(|v, _| format!("{:.0} %", v * 100.0))
                .custom_parser(|s| s.trim().trim_end_matches('%').trim().parse::<f64>().ok().map(|v| v / 100.0)),
        ).on_hover_text("Leichterer Druck zählt als kein Druck (dünnste Linie).\nHilft bei Stiften, die schon beim Schweben Druck melden."));
    });
    if ui
        .add_enabled(
            state.pressure_curve != PressureCurve::default(),
            egui::Button::new("Druckkurve zurücksetzen"),
        )
        .clicked()
    {
        state.pressure_curve = PressureCurve::default();
        save = true;
    }
    save.then_some(state.pressure_curve)
}

/// „Seite löschen“ mit Rückfrage im ☰-Menü und „Gelöschte Seite wiederherstellen“.
fn delete_page_buttons(
    ui: &mut egui::Ui,
    state: &mut UiState,
    notebook: &Notebook,
    actions: &mut Vec<UiAction>,
) {
    let number = notebook.current_page_index + 1;
    if state.confirm_delete_page {
        ui.label(
            egui::RichText::new(format!("Seite {number} mit allem Inhalt löschen?"))
                .color(egui::Color32::from_rgb(255, 170, 160)),
        );
        ui.horizontal(|ui| {
            let half = (ui.available_width() - ui.spacing().item_spacing.x) / 2.0;
            if ui
                .add_sized(
                    [half, 30.0],
                    egui::Button::new(egui::RichText::new("Löschen").color(egui::Color32::WHITE))
                        .fill(egui::Color32::from_rgb(170, 50, 45)),
                )
                .clicked()
            {
                actions.push(UiAction::DeleteCurrentPage);
                state.confirm_delete_page = false;
            }
            if ui
                .add_sized([half, 30.0], egui::Button::new("Abbrechen"))
                .clicked()
            {
                state.confirm_delete_page = false;
            }
        });
    } else {
        let only_page = notebook.pages.len() < 2;
        let response = ui
            .add_enabled_ui(!only_page, |ui| {
                ui.add_sized(
                    [ui.available_width(), 30.0],
                    egui::Button::new(format!("🗑  Seite {number} löschen …")),
                )
            })
            .inner
            .on_hover_text("Löscht die aktuelle Seite; danach im Menü wiederherstellbar")
            .on_disabled_hover_text(
                "Die einzige Seite eines Notizbuchs kann nicht gelöscht werden",
            );
        if response.clicked() {
            state.confirm_delete_page = true;
        }
    }
    if let Some(number) = state.restorable_page {
        if ui
            .add_sized(
                [ui.available_width(), 30.0],
                egui::Button::new(format!("⟲  Gelöschte Seite {number} wiederherstellen")),
            )
            .clicked()
        {
            actions.push(UiAction::RestoreDeletedPage);
        }
    }
}

/// Durchgehende Ansicht: dunkle Linie an jeder Grenze zweier Seiten (die Seiten liegen
/// lückenlos untereinander). Unter allen Fenstern der Oberfläche, über der Tinte.
fn page_separators(ctx: &egui::Context, state: &UiState, notebook: &Notebook) {
    let Some(view) = state.page_view.filter(|_| state.continuous_scroll) else {
        return;
    };
    let stack = notebook.page_stack();
    let screen = ctx.screen_rect();
    let color = if state.paper_theme.is_dark() {
        egui::Color32::from_white_alpha(140)
    } else {
        egui::Color32::from_black_alpha(200)
    };
    let painter = ctx.layer_painter(egui::LayerId::background());
    for i in 1..stack.len() {
        let y = stack.origins[i].y;
        let left = stack.origins[i - 1].x.min(stack.origins[i].x);
        let right = (stack.origins[i - 1].x + stack.sizes[i - 1].x)
            .max(stack.origins[i].x + stack.sizes[i].x);
        let a = view.to_screen(glam::Vec2::new(left, y));
        let b = view.to_screen(glam::Vec2::new(right, y));
        if a.y < screen.top() - 1.0 || a.y > screen.bottom() + 1.0 {
            continue;
        }
        painter.line_segment([a, b], egui::Stroke::new(1.5_f32, color));
    }
}

/// Zeichnet das Lineal über dem Blatt und bedient Verschieben und Drehen.
fn ruler_overlay(ctx: &egui::Context, state: &mut UiState) {
    let (Some(mut ruler), Some(view)) = (state.ruler, state.page_view) else {
        state.ruler_drag = None;
        return;
    };

    // Bedienung: Druck auf Fläche oder Griff beginnt das Ziehen (nicht über Leiste/Popovern)
    let (pressed, down, pos) = ctx.input(|i| {
        (
            i.pointer.primary_pressed(),
            i.pointer.primary_down(),
            i.pointer.interact_pos(),
        )
    });
    if pressed && !ctx.is_pointer_over_area() {
        if let Some(pos) = pos {
            let page = view.to_page(pos);
            state.ruler_drag = match ruler.hit(page) {
                Some(RulerHit::Body) => Some(RulerDrag::Move {
                    grab_offset: ruler.center - page,
                }),
                Some(RulerHit::RotateHandle) => Some(RulerDrag::Rotate),
                None => None,
            };
        }
    }
    match (state.ruler_drag, down, pos) {
        (Some(RulerDrag::Move { grab_offset }), true, Some(pos)) => {
            ruler.center = view.to_page(pos) + grab_offset;
        }
        (Some(RulerDrag::Rotate), true, Some(pos)) => ruler.rotate_toward(view.to_page(pos)),
        (_, false, _) => state.ruler_drag = None,
        _ => {}
    }
    state.ruler = Some(ruler);

    // Zeichnen (unter Leiste und Dialogen)
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Background,
        egui::Id::new("ruler_overlay"),
    ));
    let corners: Vec<egui::Pos2> = ruler.corners().iter().map(|&c| view.to_screen(c)).collect();
    painter.add(egui::Shape::convex_polygon(
        corners.clone(),
        egui::Color32::from_rgba_unmultiplied(214, 222, 235, 165),
        egui::Stroke::new(
            1.0_f32,
            egui::Color32::from_rgba_unmultiplied(60, 70, 90, 200),
        ),
    ));

    let ink = egui::Color32::from_rgb(45, 52, 66);
    let axis = ruler.axis();
    let normal = ruler.normal();
    let mm = crate::document::UNITS_PER_MM;
    let px_per_mm = view.scale * mm;
    let half_len_mm = (crate::ruler::RULER_LENGTH / mm / 2.0) as i32;
    let half_w = crate::ruler::RULER_WIDTH / 2.0;
    // Skala an beiden Längskanten, von der Mitte aus: Millimeter nur, wenn sie nicht verschwimmen
    for side in [-1.0_f32, 1.0] {
        let edge = ruler.center + normal * side * half_w;
        for i in -half_len_mm..=half_len_mm {
            let tick_mm = if i % 10 == 0 {
                4.0
            } else if i % 5 == 0 {
                2.5
            } else if px_per_mm >= 3.0 {
                1.5
            } else {
                continue;
            };
            let base = edge + axis * (i as f32 * mm);
            let tip = base - normal * side * tick_mm * mm;
            painter.line_segment(
                [view.to_screen(base), view.to_screen(tip)],
                egui::Stroke::new(1.0_f32, ink),
            );
        }
    }
    // Zentimeter-Zahlen an der oberen Kante, von der linken Ecke an gezählt
    if px_per_mm >= 1.2 {
        let font = egui::FontId::proportional((px_per_mm * 2.6).clamp(8.0, 13.0));
        let edge = ruler.center - normal * half_w;
        for cm in 0..=(half_len_mm * 2 / 10) {
            let along = -crate::ruler::RULER_LENGTH / 2.0 + cm as f32 * 10.0 * mm;
            let anchor = view.to_screen(edge + axis * along + normal * 5.0 * mm);
            let galley = ctx.fonts(|f| f.layout_no_wrap(cm.to_string(), font.clone(), ink));
            let offset = egui::vec2(galley.size().x / 2.0, 0.0);
            let offset = egui::vec2(
                offset.x * axis.x - offset.y * axis.y,
                offset.x * axis.y + offset.y * axis.x,
            );
            let mut text = egui::epaint::TextShape::new(anchor - offset, galley, ink);
            text.angle = ruler.angle;
            painter.add(text);
        }
    }

    // Winkelanzeige und Dreh-Griff
    let center = view.to_screen(ruler.center);
    let label = format!("{}°", ruler.display_degrees());
    let galley = ctx.fonts(|f| {
        f.layout_no_wrap(
            label,
            egui::FontId::proportional(13.0),
            egui::Color32::WHITE,
        )
    });
    let pill = egui::Rect::from_center_size(center, galley.size() + egui::vec2(14.0, 6.0));
    painter.rect_filled(
        pill,
        egui::Rounding::same(9.0),
        egui::Color32::from_rgba_unmultiplied(40, 70, 130, 220),
    );
    painter.galley(
        pill.center() - galley.size() / 2.0,
        galley,
        egui::Color32::WHITE,
    );

    let handle = view.to_screen(ruler.handle_center());
    let radius = (crate::ruler::HANDLE_RADIUS * view.scale).max(10.0);
    let active = state.ruler_drag == Some(RulerDrag::Rotate);
    painter.circle(
        handle,
        radius,
        if active {
            egui::Color32::from_rgb(70, 120, 210)
        } else {
            egui::Color32::from_rgba_unmultiplied(40, 70, 130, 200)
        },
        egui::Stroke::new(1.0_f32, egui::Color32::WHITE),
    );
    painter.text(
        handle,
        egui::Align2::CENTER_CENTER,
        "⟳",
        egui::FontId::proportional((radius * 1.1).clamp(10.0, 20.0)),
        egui::Color32::WHITE,
    );
}

/// Dialog „Bild einfügen“ bzw. „PDF importieren“: Schnellzugriffe, Ordnerpfad und Liste aus
/// Ordnern und passenden Dateien.
fn file_picker_dialog(ctx: &egui::Context, state: &mut UiState, actions: &mut Vec<UiAction>) {
    let mut close = false;
    let mut navigate: Option<std::path::PathBuf> = None;
    let mut go_up = false;
    let mut chosen: Option<std::path::PathBuf> = None;

    let Some(picker) = state.file_picker.as_ref() else {
        return;
    };
    let picker_kind = picker.kind;
    let (title, header, file_icon, empty_hint, tip) = match picker.kind {
        PickerKind::Image => (
            "Bild einfügen",
            "🖼 Bild einfügen",
            "🖼",
            "Keine Ordner oder Bilder (PNG, JPEG, GIF, WebP, BMP)",
            "Tipp: Screenshots mit Strg+V einfügen oder Bilddateien ins Fenster ziehen.",
        ),
        PickerKind::Pdf => (
            "PDF importieren",
            "📄 PDF importieren",
            "📄",
            "Keine Ordner oder PDF-Dateien",
            "Das PDF wird als neues Notizbuch angelegt, eine Seite je PDF-Seite. \
             Beim Export liegen deine Notizen über den Originalseiten.",
        ),
    };
    dialog_window(title, state, false).show(ctx, |ui| {
        ui.set_width(460.0);
        touch_friendly_spacing(ui);
        if dialog_header(ui, header, true) {
            close = true;
        }

        ui.horizontal_wrapped(|ui| {
            for (label, path) in crate::file_picker::shortcuts() {
                if ui.selectable_label(picker.dir == path, label).clicked() {
                    navigate = Some(path);
                }
            }
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui
                .add_enabled(picker.dir.parent().is_some(), egui::Button::new("⏶"))
                .on_hover_text("Übergeordneter Ordner")
                .clicked()
            {
                go_up = true;
            }
            ui.add(
                egui::Label::new(
                    egui::RichText::new(picker.dir.display().to_string())
                        .color(egui::Color32::from_gray(190)),
                )
                .truncate(true),
            );
        });
        if let Some(err) = &picker.error {
            ui.colored_label(egui::Color32::from_rgb(255, 120, 120), err);
        }
        ui.add_space(4.0);

        egui::ScrollArea::vertical()
            .max_height((ctx.screen_rect().height() - 330.0).clamp(160.0, 420.0))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                if picker.entries.is_empty() {
                    ui.label(egui::RichText::new(empty_hint).color(egui::Color32::from_gray(150)));
                }
                for entry in &picker.entries {
                    let label = if entry.is_dir {
                        format!("📁  {}", entry.name)
                    } else {
                        format!("{file_icon}  {}", entry.name)
                    };
                    let button = egui::Button::new(label)
                        .frame(false)
                        .min_size(egui::vec2(ui.available_width(), 34.0));
                    let response = ui.add(button);
                    if response.clicked() {
                        if entry.is_dir {
                            navigate = Some(entry.path.clone());
                        } else {
                            chosen = Some(entry.path.clone());
                        }
                    }
                }
            });

        ui.add_space(6.0);
        ui.label(
            egui::RichText::new(tip)
                .size(11.5)
                .color(egui::Color32::from_gray(150)),
        );
    });

    if let Some(picker) = state.file_picker.as_mut() {
        if go_up {
            picker.go_up();
        }
        if let Some(dir) = navigate {
            picker.navigate(&dir);
        }
    }
    if let Some(path) = chosen {
        state.last_picker_dir = state.file_picker.as_ref().map(|p| p.dir.clone());
        actions.push(match picker_kind {
            PickerKind::Image => UiAction::InsertImageFile(path),
            PickerKind::Pdf => UiAction::ImportPdf(path),
        });
        close = true;
    }
    if close {
        state.file_picker = None;
    }
}

/// Rahmen der Popover neben der Werkzeugleiste.
fn popover_frame() -> egui::Frame {
    egui::Frame::none()
        .fill(egui::Color32::from_rgb(30, 30, 34))
        .rounding(egui::Rounding::same(12.0))
        .stroke(egui::Stroke::new(
            1.0_f32,
            egui::Color32::from_white_alpha(40),
        ))
        .inner_margin(egui::Margin::symmetric(14.0, 12.0))
        .shadow(egui::epaint::Shadow {
            offset: egui::vec2(0.0, 6.0),
            blur: 16.0,
            spread: 0.0,
            color: egui::Color32::from_black_alpha(120),
        })
}

/// Deckender, einheitlicher Rahmen für Dialogfenster (`danger` = rote Warnfenster).
fn dialog_frame(danger: bool) -> egui::Frame {
    let (fill, stroke) = if danger {
        (
            egui::Color32::from_rgb(44, 24, 26),
            egui::Stroke::new(1.5_f32, egui::Color32::from_rgb(235, 87, 87)),
        )
    } else {
        (
            egui::Color32::from_rgb(30, 30, 34),
            egui::Stroke::new(1.0_f32, egui::Color32::from_white_alpha(40)),
        )
    };
    egui::Frame::none()
        .fill(fill)
        .rounding(egui::Rounding::same(14.0))
        .stroke(stroke)
        .inner_margin(egui::Margin::symmetric(20.0, 16.0))
        .shadow(egui::epaint::Shadow {
            offset: egui::vec2(0.0, 8.0),
            blur: 24.0,
            spread: 0.0,
            color: egui::Color32::from_black_alpha(160),
        })
}

/// Dialogfenster ohne egui-Titelleiste (der Titel steht in [`dialog_header`]), zentriert über
/// dem freien Bereich neben der Werkzeugleiste. Warnfenster sitzen in der Bildschirmmitte.
fn dialog_window(title: &'static str, state: &UiState, danger: bool) -> egui::Window<'static> {
    let shift = match state.sidebar_side {
        SidebarSide::Left => state.sidebar_reserved_width() / 2.0,
        SidebarSide::Right => -state.sidebar_reserved_width() / 2.0,
    };
    let (align, y) = if danger {
        (egui::Align2::CENTER_CENTER, 0.0)
    } else {
        (egui::Align2::CENTER_TOP, 60.0)
    };
    egui::Window::new(title)
        .title_bar(false)
        .collapsible(false)
        .resizable(false)
        .anchor(align, egui::vec2(shift, y))
        .frame(dialog_frame(danger))
}

/// Kopfzeile eines Dialogs mit Titel und optionalem Schließen-Knopf.
/// Gibt `true` zurück, wenn „Schließen“ getippt wurde.
fn dialog_header(ui: &mut egui::Ui, title: &str, closable: bool) -> bool {
    let mut close = false;
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(title)
                .strong()
                .size(16.0)
                .color(egui::Color32::WHITE),
        );
        if closable {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                close = close_button(ui).clicked();
            });
        }
    });
    ui.add_space(4.0);
    ui.separator();
    ui.add_space(6.0);
    close
}

/// Schließen-Knopf mit fingertauglicher Trefferfläche.
fn close_button(ui: &mut egui::Ui) -> egui::Response {
    ui.add_sized(
        [36.0, 36.0],
        egui::Button::new(egui::RichText::new("🗙").size(16.0)).frame(false),
    )
    .on_hover_text("Schließen (Esc)")
}

/// Größere Knöpfe, Eingabefelder und Schieberegler für Finger und Stift.
fn touch_friendly_spacing(ui: &mut egui::Ui) {
    let spacing = ui.spacing_mut();
    spacing.button_padding = egui::vec2(12.0, 8.0);
    spacing.interact_size = egui::vec2(40.0, 36.0);
    spacing.item_spacing = egui::vec2(8.0, 8.0);
    spacing.slider_width = 200.0;
}

/// Prüft, ob ein Bildschirmpunkt (physische Pixel) auf einem UI-Element liegt.
///
/// egui arbeitet in logischen Punkten; Touch- und Stift-Positionen kommen in physischen
/// Pixeln. `layer_id_at` erfasst die egui-Bereiche des letzten Frames (Leiste, Popover,
/// Fenster), auch wenn egui den Zeiger selbst noch nicht kennt (Stift, erster Touch).
/// Ebenen der Ordnung `Background` decken den ganzen Bildschirm ab und gehören zum
/// Zeichenblatt, nicht zur Oberfläche.
pub fn is_screen_point_in_ui(
    ctx: &egui::Context,
    state: &UiState,
    screen_pos: (f32, f32),
    scale: f32,
) -> bool {
    let pos = egui::pos2(screen_pos.0 / scale, screen_pos.1 / scale);
    state.is_point_in_ui(pos.x, pos.y)
        || ctx
            .layer_id_at(pos)
            .is_some_and(|layer| layer.order != egui::Order::Background)
}

/// Rendert die schwebende Toolbar und alle zugehörigen UI-Komponenten mit `egui`.
///
/// Gibt eine Liste von `UiAction`-Events zurück, die vom Hauptprogramm verarbeitet werden.
pub fn render_ui(
    ctx: &egui::Context,
    state: &mut UiState,
    notebook: &Notebook,
    autosave_status: AutosaveStatus,
    updater: &crate::updater::UpdaterController,
) -> Vec<UiAction> {
    let mut actions = std::mem::take(&mut state.queued_actions);
    state.screen_size = ctx.screen_rect().size();
    page_separators(ctx, state, notebook);
    ruler_overlay(ctx, state);
    text_edit_overlay(ctx, state);

    let mut pen_btn_rect = None;
    let mut high_btn_rect = None;
    let mut more_btn_rect = None;
    let mut page_btn_rect = None;

    // 1. Senkrechte Werkzeugleiste, seitlich oben angedockt. Bewusst nicht vertikal
    //    zentriert: sonst würden alle Knöpfe springen, sobald sich die Höhe ändert.
    let (anchor, offset) = match state.sidebar_side {
        SidebarSide::Left => (
            egui::Align2::LEFT_TOP,
            egui::vec2(UiState::SIDEBAR_MARGIN, UiState::SIDEBAR_MARGIN),
        ),
        SidebarSide::Right => (
            egui::Align2::RIGHT_TOP,
            egui::vec2(-UiState::SIDEBAR_MARGIN, UiState::SIDEBAR_MARGIN),
        ),
    };
    // Knopfgröße an die Fensterhöhe anpassen (40 px, schrittweise bis 28 px); reicht auch das nicht,
    // wird die Leiste scrollbar statt abgeschnitten
    let max_height = (ctx.screen_rect().height() - 2.0 * UiState::SIDEBAR_MARGIN - 12.0).max(120.0);
    // Größe nach dem höchsten Fall (Auswahl mit Aktionen), damit Knöpfe beim
    // Werkzeugwechsel weder schrumpfen noch verrutschen
    let btn = UiState::sidebar_button_size(ActiveTool::Select, max_height, state.continuous_scroll);

    let area_response = egui::Area::new(egui::Id::new("floating_toolbar_area"))
        .anchor(anchor, offset)
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            egui::Frame::none()
                .fill(egui::Color32::from_rgba_premultiplied(24, 24, 28, 225))
                .rounding(egui::Rounding::same(UiState::TOOLBAR_ROUNDING))
                .stroke(egui::Stroke::new(
                    1.0_f32,
                    egui::Color32::from_rgba_unmultiplied(255, 255, 255, 30),
                ))
                .inner_margin(egui::Margin::same(6.0))
                .shadow(egui::epaint::Shadow {
                    offset: egui::vec2(0.0, 4.0),
                    blur: 16.0,
                    spread: 0.0,
                    color: egui::Color32::from_black_alpha(100),
                })
                .show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .max_height(max_height)
                        .scroll_bar_visibility(
                            egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded,
                        )
                        .show(ui, |ui| {
                            // Zentriertes Layout nimmt sonst die volle Fensterbreite ein
                            ui.set_width(btn);
                            ui.with_layout(egui::Layout::top_down(egui::Align::Center), |ui| {
                                ui.spacing_mut().item_spacing = egui::vec2(0.0, 3.0);

                                // --- Notizbücher ---
                                let doc_tooltip = format!(
                                    "Notizbücher verwalten (aktuell: {})",
                                    if state.document_title.is_empty() {
                                        "Notizbuch"
                                    } else {
                                        state.document_title.as_str()
                                    }
                                );
                                if sidebar_button(
                                    ui,
                                    btn,
                                    SidebarIcon::Glyph("📚"),
                                    state.is_document_menu_open,
                                    true,
                                    None,
                                    &doc_tooltip,
                                )
                                .clicked()
                                {
                                    let open = !state.is_document_menu_open;
                                    state.close_overlays();
                                    state.is_document_menu_open = open;
                                    if open {
                                        actions.push(UiAction::RefreshNotebooks);
                                    }
                                }

                                sidebar_divider(ui, btn);

                                // --- Werkzeuge ---
                                let pen_resp = sidebar_button(
                                    ui,
                                    btn,
                                    SidebarIcon::Glyph("✏"),
                                    state.active_tool == ActiveTool::Pen,
                                    true,
                                    Some(state.pen_color),
                                    if state.is_pen_menu_open {
                                        ""
                                    } else {
                                        "Stift (B/U/R für Farben) – erneut tippen für Farbe & Stärke"
                                    },
                                );
                                pen_btn_rect = Some(pen_resp.rect);
                                if pen_resp.clicked() {
                                    state.is_highlighter_menu_open = false;
                                    state.is_more_menu_open = false;
                                    if state.active_tool != ActiveTool::Pen {
                                        state.active_tool = ActiveTool::Pen;
                                        state.is_pen_menu_open = false;
                                        actions.push(UiAction::SetTool(ActiveTool::Pen));
                                        actions.push(UiAction::SetColor(state.pen_color));
                                        actions.push(UiAction::SetStrokeWidthMm(
                                            state.pen_stroke_width_mm,
                                        ));
                                    } else {
                                        let open = !state.is_pen_menu_open;
                                        state.close_overlays();
                                        state.is_pen_menu_open = open;
                                    }
                                }

                                let high_resp = sidebar_button(
                                    ui,
                                    btn,
                                    SidebarIcon::Glyph("🖊"),
                                    state.active_tool == ActiveTool::Highlighter,
                                    true,
                                    Some(state.highlighter_color),
                                    if state.is_highlighter_menu_open {
                                        ""
                                    } else {
                                        "Textmarker – erneut tippen für Farbe & Stärke"
                                    },
                                );
                                high_btn_rect = Some(high_resp.rect);
                                if high_resp.clicked() {
                                    state.is_pen_menu_open = false;
                                    state.is_more_menu_open = false;
                                    if state.active_tool != ActiveTool::Highlighter {
                                        state.active_tool = ActiveTool::Highlighter;
                                        state.is_highlighter_menu_open = false;
                                        actions.push(UiAction::SetTool(ActiveTool::Highlighter));
                                        actions.push(UiAction::SetColor(state.highlighter_color));
                                        actions.push(UiAction::SetStrokeWidthMm(
                                            state.highlighter_stroke_width_mm,
                                        ));
                                    } else {
                                        let open = !state.is_highlighter_menu_open;
                                        state.close_overlays();
                                        state.is_highlighter_menu_open = open;
                                    }
                                }

                                for (tool, icon, tooltip) in [
                                    (
                                        ActiveTool::Shape,
                                        SidebarIcon::Glyph("📐"),
                                        "Formen: Striche werden zu Linien, Rechtecken, Kreisen … (S/F)",
                                    ),
                                    (
                                        ActiveTool::Eraser,
                                        SidebarIcon::Eraser,
                                        "Radierer (E oder rechte Maustaste) – darunter: ganze Striche, Teilstrich oder nur Textmarker",
                                    ),
                                    (
                                        ActiveTool::Select,
                                        SidebarIcon::Lasso,
                                        "Auswählen: verschieben, drehen, skalieren, kopieren (V)",
                                    ),
                                    (
                                        ActiveTool::Text,
                                        SidebarIcon::Glyph("T"),
                                        "Text (T): ins Blatt tippen und schreiben, vorhandenen Text \
                                         antippen zum Ändern; Esc beendet",
                                    ),
                                ] {
                                    if sidebar_button(
                                        ui,
                                        btn,
                                        icon,
                                        state.active_tool == tool,
                                        true,
                                        None,
                                        tooltip,
                                    )
                                    .clicked()
                                    {
                                        state.active_tool = tool;
                                        state.is_pen_menu_open = false;
                                        state.is_highlighter_menu_open = false;
                                        state.is_more_menu_open = false;
                                        actions.push(UiAction::SetTool(tool));
                                    }
                                }

                                // --- Lineal (ein-/ausblenden) ---
                                if sidebar_button(
                                    ui,
                                    btn,
                                    SidebarIcon::Ruler,
                                    state.ruler.is_some(),
                                    true,
                                    None,
                                    "Lineal (L): Fläche ziehen = verschieben, Griff ⟳ = drehen; \
                                     Striche an einer Kante werden gerade",
                                )
                                .clicked()
                                {
                                    state.toggle_ruler();
                                }

                                // --- Stifte bzw. Farben des aktiven Werkzeugs ---
                                let pens = matches!(state.active_tool, ActiveTool::Pen | ActiveTool::Shape);
                                let palette: Option<(&[ColorPreset], [f32; 4])> =
                                    match state.active_tool {
                                        ActiveTool::Pen | ActiveTool::Shape => None,
                                        ActiveTool::Select => {
                                            Some((&PEN_COLOR_PRESETS[..], state.pen_color))
                                        }
                                        ActiveTool::Highlighter => Some((
                                            &HIGHLIGHTER_COLOR_PRESETS[..],
                                            state.highlighter_color,
                                        )),
                                        ActiveTool::Text => {
                                            Some((&PEN_COLOR_PRESETS[..], state.text_color))
                                        }
                                        ActiveTool::Eraser => None,
                                    };
                                // Fester Platz für die längste Palette, damit die Knöpfe darunter
                                // beim Werkzeugwechsel nicht springen
                                sidebar_divider(ui, btn);
                                let swatch = UiState::sidebar_swatch_size(btn);
                                let mut shown = palette.map_or(0, |(presets, _)| presets.len());
                                if pens {
                                    shown = PEN_PRESET_COUNT;
                                    for i in 0..PEN_PRESET_COUNT {
                                        let preset = state.pen_presets[i];
                                        let selected = i == state.active_pen;
                                        let tooltip = if selected {
                                            format!(
                                                "Stift {} ({:.2} mm) – erneut tippen für Farbe & Stärke; \
                                                 Änderungen merkt sich der Stift",
                                                i + 1,
                                                preset.width_mm
                                            )
                                        } else {
                                            format!("Stift {} ({:.2} mm)", i + 1, preset.width_mm)
                                        };
                                        let response =
                                            pen_preset_swatch(ui, preset, selected, &tooltip, swatch);
                                        if selected {
                                            // Die Einstellungen erscheinen neben dem gewählten Stift
                                            pen_btn_rect = Some(response.rect);
                                        }
                                        if response.clicked() {
                                            state.is_highlighter_menu_open = false;
                                            state.is_more_menu_open = false;
                                            if selected {
                                                let open = !state.is_pen_menu_open;
                                                state.close_overlays();
                                                state.is_pen_menu_open = open;
                                            } else {
                                                state.is_pen_menu_open = false;
                                                state.select_pen(i);
                                                actions.push(UiAction::SetColor(state.pen_color));
                                                actions.push(UiAction::SetStrokeWidthMm(
                                                    state.pen_stroke_width_mm,
                                                ));
                                            }
                                        }
                                    }
                                }
                                if let Some((presets, current)) = palette {
                                    for &(color, label) in presets {
                                        let is_sel = (current[0] - color[0]).abs() < 0.05
                                            && (current[1] - color[1]).abs() < 0.05
                                            && (current[2] - color[2]).abs() < 0.05;
                                        if color_circle_swatch(ui, color, is_sel, label, swatch)
                                            .clicked()
                                        {
                                            match state.active_tool {
                                                ActiveTool::Highlighter => {
                                                    state.highlighter_color = color
                                                }
                                                ActiveTool::Text => {
                                                    state.text_color = color;
                                                    if let Some(edit) = &mut state.text_edit {
                                                        edit.color = color;
                                                        edit.focus = true;
                                                    }
                                                }
                                                // Auswahl umfärben, der Stift bleibt
                                                _ => {}
                                            }
                                            actions.push(UiAction::SetColor(color));
                                        }
                                    }
                                }
                                let spacing = ui.spacing().item_spacing.y;
                                let mut used = shown as f32 * (swatch + spacing);
                                // Radierer: statt Farben die Radier-Modi im selben Platz
                                if state.active_tool == ActiveTool::Eraser {
                                    for (mode, icon, indicator, tooltip) in [
                                        (
                                            EraserMode::All,
                                            SidebarIcon::Text("Alle"),
                                            None,
                                            "Alle Striche radieren",
                                        ),
                                        (
                                            EraserMode::Partial,
                                            SidebarIcon::Glyph("✂"),
                                            None,
                                            "Teilstrich radieren – nur den überstrichenen Teil entfernen (Formen ganz)",
                                        ),
                                        (
                                            EraserMode::HighlighterOnly,
                                            SidebarIcon::Glyph("🖊"),
                                            Some(state.highlighter_color),
                                            "Nur Textmarker radieren – Handschrift darunter bleibt",
                                        ),
                                    ] {
                                        if sidebar_button(
                                            ui,
                                            btn,
                                            icon,
                                            state.eraser_mode == mode,
                                            true,
                                            indicator,
                                            tooltip,
                                        )
                                        .clicked()
                                        {
                                            state.eraser_mode = mode;
                                        }
                                        used += btn + spacing;
                                    }
                                }
                                let reserved =
                                    UiState::MAX_PALETTE_LEN as f32 * (swatch + spacing);
                                if reserved > used {
                                    ui.add_space(reserved - used);
                                }

                                // --- Aktionen für die Auswahl ---
                                if state.active_tool == ActiveTool::Select {
                                    sidebar_divider(ui, btn);
                                    for (icon, tooltip, action) in [
                                        ("📋", "Kopieren (Strg+C)", UiAction::CopySelected),
                                        ("📥", "Einfügen (Strg+V)", UiAction::PasteSelected),
                                        ("🔄", "Um 90° drehen (R)", UiAction::RotateSelected),
                                        ("🗑", "Löschen (Entf)", UiAction::DeleteSelected),
                                    ] {
                                        if sidebar_button(
                                            ui,
                                            btn,
                                            SidebarIcon::Glyph(icon),
                                            false,
                                            true,
                                            None,
                                            tooltip,
                                        )
                                        .clicked()
                                        {
                                            actions.push(action);
                                        }
                                    }
                                }

                                // --- Schrift des Text-Werkzeugs ---
                                if state.active_tool == ActiveTool::Text {
                                    sidebar_divider(ui, btn);
                                    text_style_buttons(ui, btn, state);
                                }

                                sidebar_divider(ui, btn);

                                // --- Undo & Redo ---
                                let history = &notebook.current_page().history;
                                if sidebar_button(
                                    ui,
                                    btn,
                                    SidebarIcon::Glyph("⟲"),
                                    false,
                                    history.can_undo(),
                                    None,
                                    "Rückgängig (Strg+Z)",
                                )
                                .clicked()
                                {
                                    actions.push(UiAction::Undo);
                                }
                                if sidebar_button(
                                    ui,
                                    btn,
                                    SidebarIcon::Glyph("⟳"),
                                    false,
                                    history.can_redo(),
                                    None,
                                    "Wiederholen (Strg+Y)",
                                )
                                .clicked()
                                {
                                    actions.push(UiAction::Redo);
                                }

                                sidebar_divider(ui, btn);

                                // --- Seitennavigation ---
                                // In der durchgehenden Ansicht wird gescrollt; Blätterpfeile gibt
                                // es nur in der Einzelseitenansicht
                                let paging = !state.continuous_scroll;
                                if paging
                                    && sidebar_button(
                                        ui,
                                        btn,
                                        SidebarIcon::Glyph("⏶"),
                                        false,
                                        notebook.current_page_index > 0,
                                        None,
                                        "Vorherige Seite (Bild auf)",
                                    )
                                    .clicked()
                                {
                                    actions.push(UiAction::PreviousPage);
                                }
                                let page_text = format!(
                                    "{}/{}",
                                    notebook.current_page_index + 1,
                                    notebook.pages.len()
                                );
                                let page_resp = sidebar_button(
                                    ui,
                                    btn,
                                    SidebarIcon::Text(&page_text),
                                    state.show_page_jump_popup,
                                    true,
                                    None,
                                    "Seite wählen",
                                );
                                page_btn_rect = Some(page_resp.rect);
                                if page_resp.clicked() {
                                    let open = !state.show_page_jump_popup;
                                    state.close_overlays();
                                    state.show_page_jump_popup = open;
                                }
                                if paging
                                    && sidebar_button(
                                        ui,
                                        btn,
                                        SidebarIcon::Glyph("⏷"),
                                        false,
                                        notebook.current_page_index + 1 < notebook.pages.len(),
                                        None,
                                        "Nächste Seite (Bild ab)",
                                    )
                                    .clicked()
                                {
                                    actions.push(UiAction::NextPage);
                                }
                                if sidebar_button(
                                    ui,
                                    btn,
                                    SidebarIcon::Glyph("➕"),
                                    false,
                                    true,
                                    None,
                                    "Neue Seite nach dieser einfügen",
                                )
                                .clicked()
                                {
                                    state.close_overlays();
                                    actions.push(UiAction::AddPage);
                                }

                                sidebar_divider(ui, btn);

                                // --- ☰-Menü mit Status-Punkt (Speichern) und Update-Hinweis ---
                                let update_available = matches!(
                                    updater.status(),
                                    crate::updater::UpdaterStatus::UpdateAvailable(_)
                                        | crate::updater::UpdaterStatus::DownloadFinished { .. }
                                );
                                let more_resp = sidebar_button(
                                    ui,
                                    btn,
                                    SidebarIcon::Glyph("☰"),
                                    state.is_more_menu_open,
                                    true,
                                    None,
                                    if update_available {
                                        "Weitere Funktionen – Update verfügbar"
                                    } else {
                                        "Weitere Funktionen: PDF, Hintergrund, Zoom, Einstellungen"
                                    },
                                );
                                more_btn_rect = Some(more_resp.rect);
                                let (dot_color, _) = autosave_status_style(autosave_status);
                                ui.painter().circle_filled(
                                    more_resp.rect.right_top() + egui::vec2(-7.0, 7.0),
                                    3.5,
                                    dot_color,
                                );
                                if update_available {
                                    ui.painter().circle_filled(
                                        more_resp.rect.left_top() + egui::vec2(7.0, 7.0),
                                        3.5,
                                        egui::Color32::from_rgb(52, 152, 219),
                                    );
                                }
                                if more_resp.clicked() {
                                    let open = !state.is_more_menu_open;
                                    state.close_overlays();
                                    state.is_more_menu_open = open;
                                }
                            });
                        });
                })
                .response
                .rect
        });

    // Leisten-Rechteck für pixelgenaue Event-Isolation speichern
    state.toolbar_rect = Some(area_response.inner);

    // 2. SCHWEBENDES POPOVER-MENÜ (FÜR STIFT ODER TEXTMARKER)
    let mut popover_rect = None;

    if state.is_pen_menu_open {
        if let Some(btn_rect) = pen_btn_rect {
            let (popover_pos, pivot) =
                popover_placement(state.sidebar_side, area_response.inner, btn_rect);
            let pop_response = egui::Area::new(egui::Id::new("pen_settings_popover"))
                .fixed_pos(popover_pos)
                .pivot(pivot)
                .constrain(true)
                .order(egui::Order::Foreground)
                .show(ctx, |ui| {
                    let frame = popover_frame().show(ui, |ui| {
                        ui.set_width(320.0);
                        touch_friendly_spacing(ui);

                        // Titelzeile mit Schließen-Button
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(format!("Stift {}", state.active_pen + 1))
                                    .strong()
                                    .color(egui::Color32::WHITE)
                                    .size(12.5),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if close_button(ui).clicked() {
                                        state.is_pen_menu_open = false;
                                    }
                                },
                            );
                        });

                        ui.add_space(8.0);

                        // Zeile 1: Farbauswahl (Color Swatches)
                        ui.label(
                            egui::RichText::new("FARBE")
                                .size(10.5)
                                .strong()
                                .color(egui::Color32::from_gray(160)),
                        );
                        ui.add_space(3.0);
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing = egui::vec2(8.0, 0.0);
                            for (color, label) in PEN_COLOR_PRESETS {
                                let is_sel = (state.pen_color[0] - color[0]).abs() < 0.05
                                    && (state.pen_color[1] - color[1]).abs() < 0.05
                                    && (state.pen_color[2] - color[2]).abs() < 0.05;
                                if color_circle_swatch(ui, color, is_sel, label, 30.0).clicked() {
                                    state.pen_color = color;
                                    actions.push(UiAction::SetColor(color));
                                }
                            }

                            // RGBA-Colorpicker
                            let mut egui_color = egui::Rgba::from_rgba_premultiplied(
                                state.pen_color[0],
                                state.pen_color[1],
                                state.pen_color[2],
                                state.pen_color[3],
                            );
                            if egui::color_picker::color_edit_button_rgba(
                                ui,
                                &mut egui_color,
                                egui::color_picker::Alpha::Opaque,
                            )
                            .changed()
                            {
                                state.pen_color = egui_color.to_array();
                                actions.push(UiAction::SetColor(state.pen_color));
                            }
                        });

                        ui.add_space(10.0);

                        // Zeile 2: Dicken-Presets mit visueller Linien-Vorschau
                        ui.label(
                            egui::RichText::new("DICKEN-PRESETS")
                                .size(10.5)
                                .strong()
                                .color(egui::Color32::from_gray(160)),
                        );
                        ui.add_space(3.0);
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing = egui::vec2(6.0, 0.0);
                            for (preset, label) in PEN_WIDTH_PRESETS {
                                let is_sel = (state.pen_stroke_width_mm - preset).abs() < 0.05;
                                if stroke_preview_button(ui, is_sel, preset, label, state.pen_color)
                                    .clicked()
                                {
                                    state.pen_stroke_width_mm = preset;
                                    actions.push(UiAction::SetStrokeWidthMm(preset));
                                }
                            }
                        });

                        ui.add_space(10.0);

                        // Zeile 3: Feinjustierung (Slider)
                        ui.label(
                            egui::RichText::new("STUFENLOSE STÄRKE")
                                .size(10.5)
                                .strong()
                                .color(egui::Color32::from_gray(160)),
                        );
                        ui.add_space(2.0);
                        if ui
                            .add(
                                egui::Slider::new(&mut state.pen_stroke_width_mm, 0.15..=5.0)
                                    .suffix(" mm")
                                    .step_by(0.05),
                            )
                            .changed()
                        {
                            actions.push(UiAction::SetStrokeWidthMm(state.pen_stroke_width_mm));
                        }
                    });

                    frame.response.rect
                });

            popover_rect = Some(pop_response.inner);
        }
    } else if state.is_highlighter_menu_open {
        if let Some(btn_rect) = high_btn_rect {
            let (popover_pos, pivot) =
                popover_placement(state.sidebar_side, area_response.inner, btn_rect);
            let pop_response = egui::Area::new(egui::Id::new("highlighter_settings_popover"))
                .fixed_pos(popover_pos)
                .pivot(pivot)
                .constrain(true)
                .order(egui::Order::Foreground)
                .show(ctx, |ui| {
                    let frame = popover_frame().show(ui, |ui| {
                        ui.set_width(320.0);
                        touch_friendly_spacing(ui);

                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new("Textmarker-Konfiguration")
                                    .strong()
                                    .color(egui::Color32::WHITE)
                                    .size(12.5),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if close_button(ui).clicked() {
                                        state.is_highlighter_menu_open = false;
                                    }
                                },
                            );
                        });

                        ui.add_space(8.0);

                        // Zeile 1: Farbauswahl
                        ui.label(
                            egui::RichText::new("MARKER-FARBE")
                                .size(10.5)
                                .strong()
                                .color(egui::Color32::from_gray(160)),
                        );
                        ui.add_space(3.0);
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing = egui::vec2(8.0, 0.0);
                            for (color, label) in HIGHLIGHTER_COLOR_PRESETS {
                                let is_sel = (state.highlighter_color[0] - color[0]).abs() < 0.05
                                    && (state.highlighter_color[1] - color[1]).abs() < 0.05
                                    && (state.highlighter_color[2] - color[2]).abs() < 0.05;
                                if color_circle_swatch(ui, color, is_sel, label, 30.0).clicked() {
                                    state.highlighter_color = color;
                                    actions.push(UiAction::SetColor(color));
                                }
                            }

                            let mut egui_color = egui::Rgba::from_rgba_premultiplied(
                                state.highlighter_color[0],
                                state.highlighter_color[1],
                                state.highlighter_color[2],
                                state.highlighter_color[3],
                            );
                            if egui::color_picker::color_edit_button_rgba(
                                ui,
                                &mut egui_color,
                                egui::color_picker::Alpha::OnlyBlend,
                            )
                            .changed()
                            {
                                state.highlighter_color = egui_color.to_array();
                                actions.push(UiAction::SetColor(state.highlighter_color));
                            }
                        });

                        ui.add_space(10.0);

                        // Zeile 2: Dicken-Presets
                        ui.label(
                            egui::RichText::new("DICKEN-PRESETS")
                                .size(10.5)
                                .strong()
                                .color(egui::Color32::from_gray(160)),
                        );
                        ui.add_space(3.0);
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing = egui::vec2(6.0, 0.0);
                            for (preset, label) in HIGHLIGHTER_WIDTH_PRESETS {
                                let is_sel =
                                    (state.highlighter_stroke_width_mm - preset).abs() < 0.1;
                                if stroke_preview_button(
                                    ui,
                                    is_sel,
                                    preset,
                                    label,
                                    state.highlighter_color,
                                )
                                .clicked()
                                {
                                    state.highlighter_stroke_width_mm = preset;
                                    actions.push(UiAction::SetStrokeWidthMm(preset));
                                }
                            }
                        });

                        ui.add_space(10.0);

                        // Zeile 3: Slider
                        ui.label(
                            egui::RichText::new("STUFENLOSE STÄRKE")
                                .size(10.5)
                                .strong()
                                .color(egui::Color32::from_gray(160)),
                        );
                        ui.add_space(2.0);
                        if ui
                            .add(
                                egui::Slider::new(
                                    &mut state.highlighter_stroke_width_mm,
                                    2.0..=16.0,
                                )
                                .suffix(" mm")
                                .step_by(0.5),
                            )
                            .changed()
                        {
                            actions.push(UiAction::SetStrokeWidthMm(
                                state.highlighter_stroke_width_mm,
                            ));
                        }
                    });

                    frame.response.rect
                });

            popover_rect = Some(pop_response.inner);
        }
    }

    // 2b. ☰-MENÜ (selten genutzte Funktionen), öffnet sich nach oben zur Blattseite hin
    if state.is_more_menu_open {
        if let Some(btn_rect) = more_btn_rect {
            let (mut pos, pivot) =
                popover_placement(state.sidebar_side, area_response.inner, btn_rect);
            pos.y = btn_rect.max.y;
            let pivot = match pivot {
                egui::Align2::RIGHT_TOP => egui::Align2::RIGHT_BOTTOM,
                _ => egui::Align2::LEFT_BOTTOM,
            };
            let pop_response = egui::Area::new(egui::Id::new("more_menu_popover"))
                .fixed_pos(pos)
                .pivot(pivot)
                .constrain(true)
                .order(egui::Order::Foreground)
                .show(ctx, |ui| {
                    popover_frame()
                        .show(ui, |ui| {
                            ui.set_width(260.0);
                            touch_friendly_spacing(ui);
                            let section = |ui: &mut egui::Ui, title: &str| {
                                ui.add_space(8.0);
                                ui.label(
                                    egui::RichText::new(title)
                                        .size(10.5)
                                        .strong()
                                        .color(egui::Color32::from_gray(160)),
                                );
                                ui.add_space(2.0);
                            };

                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new("Weitere Funktionen")
                                        .strong()
                                        .color(egui::Color32::WHITE)
                                        .size(12.5),
                                );
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if close_button(ui).clicked()
                                        {
                                            state.is_more_menu_open = false;
                                        }
                                    },
                                );
                            });

                            // Bei kleinen Fenstern scrollen statt abschneiden
                            egui::ScrollArea::vertical()
                                .max_height((ctx.screen_rect().height() - 90.0).max(150.0))
                                .show(ui, |ui| {
                            section(ui, "DOKUMENT");
                            if ui
                                .add_sized([ui.available_width(), 30.0], egui::Button::new("📄  Als PDF exportieren (Strg+E)"))
                                .clicked()
                            {
                                actions.push(UiAction::ExportPdf);
                                state.is_more_menu_open = false;
                            }
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new("Seite als").color(egui::Color32::from_gray(190)));
                                for (label, kind, hint) in [
                                    ("PNG", PageExportKind::Png, "Bild mit Papier und Raster (200 dpi)"),
                                    ("PNG transp.", PageExportKind::PngTransparent, "Bild mit transparentem Hintergrund, nur Inhalt"),
                                    ("SVG", PageExportKind::Svg, "Vektorgrafik, z. B. für Inkscape oder LaTeX"),
                                ] {
                                    if ui.button(label).on_hover_text(hint).clicked() {
                                        actions.push(UiAction::ExportPage(kind));
                                        state.is_more_menu_open = false;
                                    }
                                }
                            });
                            if ui
                                .add_sized([ui.available_width(), 30.0], egui::Button::new("➕  Neue Seite einfügen"))
                                .on_hover_text("Leere Seite direkt nach der aktuellen Seite")
                                .clicked()
                            {
                                actions.push(UiAction::AddPage);
                            }
                            delete_page_buttons(ui, state, notebook, &mut actions);
                            if ui
                                .add_sized([ui.available_width(), 30.0], egui::Button::new("🖼  Bild einfügen … (Strg+I)"))
                                .on_hover_text("Auch per Strg+V aus der Zwischenablage oder durch Ziehen einer Bilddatei ins Fenster")
                                .clicked()
                            {
                                state.open_image_picker();
                            }
                            if ui
                                .add_sized([ui.available_width(), 30.0], egui::Button::new("📄  PDF importieren …"))
                                .on_hover_text("Vorlesungsfolien, Übungsblätter oder Formulare als neues Notizbuch öffnen und darauf schreiben")
                                .clicked()
                            {
                                state.open_pdf_picker();
                            }

                            section(ui, "HINTERGRUND DIESER SEITE");
                            let current_bg = notebook.current_page().background;
                            egui::Grid::new("more_menu_backgrounds")
                                .num_columns(2)
                                .spacing([6.0, 6.0])
                                .show(ui, |ui| {
                                    for (i, (bg, label)) in [
                                        (PageBackground::Grid5mm, "Kariert"),
                                        (PageBackground::Lined, "Liniert"),
                                        (PageBackground::DotGrid, "Punkte"),
                                        (PageBackground::Blank, "Blanko"),
                                    ]
                                    .into_iter()
                                    .enumerate()
                                    {
                                        if ui
                                            .add_sized(
                                                [115.0, 28.0],
                                                egui::SelectableLabel::new(current_bg == bg, label),
                                            )
                                            .clicked()
                                        {
                                            actions.push(UiAction::SetBackground(bg));
                                        }
                                        if i % 2 == 1 {
                                            ui.end_row();
                                        }
                                    }
                                });

                            section(ui, "PAPIERFARBE (NUR ANZEIGE)");
                            let mut paper_changed = false;
                            egui::Grid::new("more_menu_paper")
                                .num_columns(2)
                                .spacing([6.0, 6.0])
                                .show(ui, |ui| {
                                    for (i, theme) in PaperTheme::ALL.into_iter().enumerate() {
                                        if paper_theme_button(ui, theme, state.paper_theme == theme)
                                            .clicked()
                                            && state.paper_theme != theme
                                        {
                                            state.paper_theme = theme;
                                            paper_changed = true;
                                        }
                                        if i % 2 == 1 {
                                            ui.end_row();
                                        }
                                    }
                                });
                            if ui
                                .add_enabled(
                                    state.paper_theme.is_dark(),
                                    egui::Checkbox::new(
                                        &mut state.adapt_ink_colors,
                                        "Dunkle Stiftfarben aufhellen",
                                    ),
                                )
                                .on_hover_text(
                                    "Schwarz wird Weiß, Dunkelblau wird Hellblau. Gespeichert und \
                                     als PDF exportiert werden immer die Originalfarben.",
                                )
                                .changed()
                            {
                                paper_changed = true;
                            }
                            if paper_changed {
                                actions.push(UiAction::SetPaperTheme {
                                    theme: state.paper_theme,
                                    adapt_ink: state.adapt_ink_colors,
                                });
                            }

                            section(ui, "KAROS, LINIEN UND PUNKTE");
                            ui.horizontal_wrapped(|ui| {
                                ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
                                for pattern in PatternColor::ALL {
                                    if pattern_color_button(
                                        ui,
                                        pattern,
                                        state.paper_theme,
                                        state.pattern_color == pattern,
                                    )
                                    .clicked()
                                        && state.pattern_color != pattern
                                    {
                                        state.pattern_color = pattern;
                                        actions.push(UiAction::SetPatternColor(pattern));
                                    }
                                }
                            });

                            section(ui, "ANSICHT");
                            if ui
                                .checkbox(&mut state.continuous_scroll, "Seiten durchgehend scrollen")
                                .on_hover_text(
                                    "An: Seiten hängen untereinander, Mausrad und Finger scrollen \
                                     (Strg+Mausrad zoomt); wer unten auf der letzten Seite schreibt, \
                                     bekommt automatisch eine neue Seite.\nAus: Seite für Seite blättern.",
                                )
                                .changed()
                            {
                                actions.push(UiAction::SetContinuousScroll(state.continuous_scroll));
                            }

                            section(ui, "ZOOM");
                            ui.horizontal(|ui| {
                                if ui
                                    .add_enabled(state.current_zoom > 0.25, egui::Button::new("🔍 -"))
                                    .on_hover_text("Herauszoomen (Strg+-)")
                                    .clicked()
                                {
                                    actions.push(UiAction::ZoomOut);
                                }
                                let pct = (state.current_zoom * 100.0).round() as u32;
                                if ui
                                    .button(format!("{pct} %"))
                                    .on_hover_text("Zurücksetzen (Strg+0)")
                                    .clicked()
                                {
                                    actions.push(UiAction::ResetZoom);
                                }
                                if ui
                                    .add_enabled(state.current_zoom < 5.0, egui::Button::new("🔍 +"))
                                    .on_hover_text("Heranzoomen (Strg++)")
                                    .clicked()
                                {
                                    actions.push(UiAction::ZoomIn);
                                }
                            });

                            section(ui, "EINGABE");
                            ui.checkbox(
                                &mut state.touch_draw_enabled,
                                "Mit dem Finger zeichnen",
                            )
                            .on_hover_text(
                                "Aus: ein Finger verschiebt, zwei Finger zoomen (empfohlen mit Stift).\nAn: ein Finger zeichnet.",
                            );
                            if let Some(curve) = pressure_curve_editor(ui, state) {
                                actions.push(UiAction::SavePressureCurve(curve));
                            }

                            section(ui, "WERKZEUGLEISTE");
                            ui.horizontal(|ui| {
                                for (side, label) in
                                    [(SidebarSide::Left, "Links"), (SidebarSide::Right, "Rechts")]
                                {
                                    if ui
                                        .add_sized(
                                            [115.0, 28.0],
                                            egui::SelectableLabel::new(
                                                state.sidebar_side == side,
                                                label,
                                            ),
                                        )
                                        .clicked()
                                        && state.sidebar_side != side
                                    {
                                        state.sidebar_side = side;
                                        actions.push(UiAction::SetSidebarSide(side));
                                    }
                                }
                            });

                            section(ui, "STATUS");
                            let (dot_color, status_text) = autosave_status_style(autosave_status);
                            ui.horizontal(|ui| {
                                let (dot_rect, _) = ui.allocate_exact_size(
                                    egui::vec2(10.0, 10.0),
                                    egui::Sense::hover(),
                                );
                                ui.painter().circle_filled(dot_rect.center(), 4.0, dot_color);
                                ui.label(egui::RichText::new(status_text).color(dot_color));
                            });
                            let update_label = match updater.status() {
                                crate::updater::UpdaterStatus::UpdateAvailable(rel) => {
                                    format!("🚀  Update {} verfügbar", rel.tag_name)
                                }
                                crate::updater::UpdaterStatus::Downloading { percent, .. } => {
                                    format!("⏳  Update lädt … {:.0} %", percent * 100.0)
                                }
                                crate::updater::UpdaterStatus::DownloadFinished { .. } => {
                                    "✔  Update bereit".to_string()
                                }
                                crate::updater::UpdaterStatus::Checking => {
                                    "🔄  Suche nach Updates …".to_string()
                                }
                                _ => "🔄  Updates & Versionen".to_string(),
                            };
                            if ui
                                .add_sized([ui.available_width(), 30.0], egui::Button::new(update_label))
                                .clicked()
                            {
                                state.close_overlays();
                                state.is_update_dialog_open = true;
                            }
                                });
                        })
                        .response
                        .rect
                });
            popover_rect = Some(pop_response.inner);
        }
    }

    // 2c. SEITENAUSWAHL als Kacheln neben dem Seitenknopf
    if state.show_page_jump_popup {
        if let Some(btn_rect) = page_btn_rect {
            let (pos, pivot) = popover_placement(state.sidebar_side, area_response.inner, btn_rect);
            let pop_response = egui::Area::new(egui::Id::new("page_picker_popover"))
                .fixed_pos(pos)
                .pivot(pivot)
                .constrain(true)
                .order(egui::Order::Foreground)
                .show(ctx, |ui| {
                    popover_frame()
                        .show(ui, |ui| {
                            // Breite = Kachelraster; sonst nimmt die Kopfzeile die ganze Fensterbreite ein
                            ui.set_width(PAGE_PICKER_COLUMNS as f32 * (PAGE_TILE.x + 8.0) - 8.0);
                            touch_friendly_spacing(ui);
                            let title = format!("Seiten ({})", notebook.pages.len());
                            if dialog_header(ui, &title, true) {
                                state.show_page_jump_popup = false;
                            }
                            // Titel der aktuellen Seite
                            let current = notebook.current_page();
                            if state.page_title_for != Some(current.id) {
                                state.page_title_for = Some(current.id);
                                state.page_title_input = current.title.clone().unwrap_or_default();
                            }
                            ui.label(
                                egui::RichText::new(format!(
                                    "TITEL VON SEITE {}",
                                    notebook.current_page_index + 1
                                ))
                                .size(10.5)
                                .strong()
                                .color(egui::Color32::from_gray(160)),
                            );
                            let field = ui.add(
                                egui::TextEdit::singleline(&mut state.page_title_input)
                                    .hint_text("z. B. Kapitel 2 – Integrale")
                                    .margin(egui::vec2(8.0, 8.0))
                                    .desired_width(f32::INFINITY),
                            );
                            // Bei jeder Änderung übernehmen: schließt sich die Übersicht, geht nichts verloren
                            if field.changed() {
                                actions.push(UiAction::SetPageTitle {
                                    page_id: current.id,
                                    title: state.page_title_input.clone(),
                                });
                            }
                            ui.add_space(2.0);
                            ui.horizontal(|ui| {
                                for (list, label) in [(false, "🖼  Kacheln"), (true, "📑  Inhalt")]
                                {
                                    if ui
                                        .add_sized(
                                            [140.0, 30.0],
                                            egui::SelectableLabel::new(
                                                state.page_picker_list == list,
                                                label,
                                            ),
                                        )
                                        .clicked()
                                    {
                                        state.page_picker_list = list;
                                    }
                                }
                            });
                            ui.add_space(2.0);
                            // Noch nicht geladene Seiten für die Vorschau nachladen lassen
                            if !state.page_picker_list
                                && notebook.pages.iter().any(|p| !p.is_loaded)
                            {
                                actions.push(UiAction::LoadAllPages);
                            }
                            if state.page_picker_list {
                                egui::ScrollArea::vertical()
                                    .max_height(420.0)
                                    .show(ui, |ui| {
                                        ui.set_width(ui.available_width());
                                        if let Some(idx) = page_title_list(ui, notebook) {
                                            actions.push(UiAction::GoToPage(idx));
                                            state.show_page_jump_popup = false;
                                        }
                                    });
                            } else {
                                egui::ScrollArea::vertical()
                                    .max_height(420.0)
                                    .show(ui, |ui| {
                                        egui::Grid::new("page_picker_grid")
                                            .spacing([8.0, 8.0])
                                            .show(ui, |ui| {
                                                for (idx, page) in notebook.pages.iter().enumerate()
                                                {
                                                    let is_current =
                                                        idx == notebook.current_page_index;
                                                    if page_thumbnail(
                                                        ui,
                                                        state,
                                                        page,
                                                        idx + 1,
                                                        is_current,
                                                    )
                                                    .clicked()
                                                    {
                                                        actions.push(UiAction::GoToPage(idx));
                                                        state.show_page_jump_popup = false;
                                                    }
                                                    if (idx + 1) % PAGE_PICKER_COLUMNS == 0 {
                                                        ui.end_row();
                                                    }
                                                }
                                            });
                                    });
                            }
                            ui.add_space(4.0);
                            if ui
                                .add_sized(
                                    [ui.available_width(), 36.0],
                                    egui::Button::new("➕  Neue Seite einfügen"),
                                )
                                .on_hover_text("Leere Seite direkt nach der aktuellen Seite")
                                .clicked()
                            {
                                actions.push(UiAction::AddPage);
                                state.show_page_jump_popup = false;
                            }
                        })
                        .response
                        .rect
                });
            popover_rect = Some(pop_response.inner);
        }
    }

    state.popover_rect = popover_rect;
    if !state.is_more_menu_open {
        state.confirm_delete_page = false;
    }

    // Automatisches Schließen der Popover bei Klick außerhalb von Leiste und Popover
    if (state.is_pen_menu_open
        || state.is_highlighter_menu_open
        || state.is_more_menu_open
        || state.show_page_jump_popup)
        && ctx.input(|i| i.pointer.any_pressed())
    {
        if let Some(pos) = ctx.input(|i| i.pointer.press_origin()) {
            let in_toolbar = state.toolbar_rect.is_some_and(|r| r.contains(pos));
            let in_popover = state.popover_rect.is_some_and(|r| r.contains(pos));
            if !in_toolbar && !in_popover {
                state.is_pen_menu_open = false;
                state.is_highlighter_menu_open = false;
                state.is_more_menu_open = false;
                state.show_page_jump_popup = false;
            }
        }
    }

    // 3b. Dokumenten-Manager (Notizbücher wechseln, anlegen, umbenennen, löschen)
    if state.is_document_menu_open {
        let doc_win = dialog_window("Dokumenten-Manager", state, false)
            .show(ctx, |ui| {
                ui.set_width(420.0);
                touch_friendly_spacing(ui);
                if dialog_header(ui, "📚 Notizbücher", true) {
                    state.is_document_menu_open = false;
                }

                // Neues Dokument anlegen
                ui.label(
                    egui::RichText::new("NEUES NOTIZBUCH ERSTELLEN")
                        .size(10.5)
                        .strong()
                        .color(egui::Color32::from_gray(160)),
                );
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let text_edit = ui.add(
                        egui::TextEdit::singleline(&mut state.new_notebook_title_input)
                            .hint_text("Titel des neuen Notizbuchs …")
                            .margin(egui::vec2(8.0, 8.0))
                            .desired_width(270.0),
                    );
                    let create_btn = ui.button(egui::RichText::new("➕ Erstellen").strong());
                    let submit = create_btn.clicked()
                        || (text_edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));

                    if submit {
                        let title = state.new_notebook_title_input.trim();
                        if !title.is_empty() {
                            actions.push(UiAction::CreateNotebook(title.to_string()));
                            state.new_notebook_title_input.clear();
                        }
                    }
                });

                ui.add_space(12.0);
                ui.separator();
                ui.add_space(8.0);

                // Liste vorhandener Dokumente
                ui.label(
                    egui::RichText::new("VORHANDENE DOKUMENTE")
                        .size(10.5)
                        .strong()
                        .color(egui::Color32::from_gray(160)),
                );
                ui.add_space(4.0);

                egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                    for summary in &state.available_notebooks {
                        let is_current = summary.id == state.current_notebook_id;
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing = egui::vec2(6.0, 0.0);

                            if state.renaming_notebook_id.as_deref() == Some(&summary.id) {
                                let edit = ui.add(
                                    egui::TextEdit::singleline(&mut state.renaming_title_input)
                                        .margin(egui::vec2(8.0, 8.0))
                                        .desired_width(260.0),
                                );
                                if ui.button("✔").clicked()
                                    || (edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                                {
                                    let new_t = state.renaming_title_input.trim();
                                    if !new_t.is_empty() {
                                        actions.push(UiAction::RenameNotebook {
                                            id: summary.id.clone(),
                                            new_title: new_t.to_string(),
                                        });
                                    }
                                    state.renaming_notebook_id = None;
                                }
                                if ui.button("🗙").clicked() {
                                    state.renaming_notebook_id = None;
                                }
                            } else {
                                let (bg, border) = if is_current {
                                    (
                                        egui::Color32::from_rgba_premultiplied(45, 80, 145, 200),
                                        egui::Stroke::new(1.0_f32, egui::Color32::from_rgba_unmultiplied(110, 165, 255, 180)),
                                    )
                                } else {
                                    (
                                        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 8),
                                        egui::Stroke::new(1.0_f32, egui::Color32::from_rgba_unmultiplied(255, 255, 255, 16)),
                                    )
                                };

                                // Nur der Titelbereich öffnet das Notizbuch: Eine Klickfläche über der
                                // ganzen Zeile würde (als zuletzt angelegtes Element) die Klicks auf
                                // Umbenennen und Löschen abfangen
                                let mut open_clicked = false;
                                egui::Frame::none()
                                    .fill(bg)
                                    .stroke(border)
                                    .rounding(egui::Rounding::same(8.0))
                                    .inner_margin(egui::Margin::symmetric(10.0, 8.0))
                                    .show(ui, |ui| {
                                        ui.horizontal(|ui| {
                                            let title_area = ui.vertical(|ui| {
                                                ui.horizontal(|ui| {
                                                    ui.label(
                                                        egui::RichText::new(&summary.title)
                                                            .strong()
                                                            .size(13.0)
                                                            .color(egui::Color32::WHITE),
                                                    );
                                                    if summary.status == crate::storage::NotebookStatus::Damaged {
                                                        ui.colored_label(egui::Color32::from_rgb(255, 100, 100), "⚠ Beschädigt");
                                                    } else if is_current {
                                                        ui.colored_label(egui::Color32::from_rgb(46, 204, 113), "⚫ Aktiv");
                                                    }
                                                });
                                                let subtext = if let Some(ref err) = summary.error {
                                                    format!("Fehler: {err}")
                                                } else {
                                                    format!("{} Seite(n)", summary.page_count)
                                                };
                                                ui.label(
                                                    egui::RichText::new(subtext)
                                                        .size(11.0)
                                                        .color(egui::Color32::from_gray(160)),
                                                );
                                            });
                                            open_clicked = title_area
                                                .response
                                                .interact(egui::Sense::click())
                                                .on_hover_cursor(egui::CursorIcon::PointingHand)
                                                .clicked();

                                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                                if summary.status == crate::storage::NotebookStatus::Damaged {
                                                    if ui
                                                        .button(egui::RichText::new("🔄 Backup").size(13.0).color(egui::Color32::from_rgb(100, 200, 255)))
                                                        .on_hover_text("Aus manifest.json.bak wiederherstellen")
                                                        .clicked()
                                                    {
                                                        actions.push(UiAction::RestoreNotebookBackup(summary.id.clone()));
                                                    }
                                                    if ui
                                                        .button(egui::RichText::new("📁 Ordner").size(13.0))
                                                        .on_hover_text("Pfad in Statusleiste anzeigen")
                                                        .clicked()
                                                    {
                                                        actions.push(UiAction::OpenNotebookFolder(summary.id.clone()));
                                                    }
                                                } else {
                                                    if state.available_notebooks.len() > 1
                                                        && ui
                                                            .button(egui::RichText::new("🗑").size(16.0).color(egui::Color32::from_rgb(255, 120, 120)))
                                                            .on_hover_text("Notizbuch in Papierkorb verschieben")
                                                            .clicked()
                                                    {
                                                        actions.push(UiAction::DeleteNotebook(summary.id.clone()));
                                                    }

                                                    if ui.button(egui::RichText::new("✏").size(16.0))
                                                        .on_hover_text("Titel umbenennen")
                                                        .clicked()
                                                    {
                                                        state.renaming_notebook_id = Some(summary.id.clone());
                                                        state.renaming_title_input = summary.title.clone();
                                                    }
                                                }
                                            });
                                        });
                                    });

                                if summary.status != crate::storage::NotebookStatus::Damaged
                                    && open_clicked
                                    && !is_current
                                {
                                    actions.push(UiAction::OpenNotebook(summary.id.clone()));
                                    state.is_document_menu_open = false;
                                }
                            }
                        });
                        ui.add_space(4.0);
                    }
                });

                if let Some((_, ref title)) = state.last_deleted_notebook {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(format!("'{title}' gelöscht"))
                                .size(11.0)
                                .color(egui::Color32::from_rgb(255, 180, 100)),
                        );
                        if ui
                            .button(egui::RichText::new("↩ Rückgängig").size(13.0).color(egui::Color32::from_rgb(100, 200, 255)))
                            .clicked()
                        {
                            actions.push(UiAction::UndoDeleteNotebook);
                        }
                    });
                }
            });
        state.document_modal_rect = doc_win.map(|w| w.response.rect);
    } else {
        state.document_modal_rect = None;
    }

    // 3c. Dateiauswahl „Bild einfügen“
    if state.file_picker.is_some() {
        file_picker_dialog(ctx, state, &mut actions);
    }

    // 3c. Release- & Update-Center (Gitea Releases)
    if state.is_update_dialog_open {
        let win = dialog_window("Release- & Update-Center", state, false)
            .show(ctx, |ui| {
                ui.set_width(460.0);
                touch_friendly_spacing(ui);
                if dialog_header(ui, "🚀 Updates & Versionen", true) {
                    state.is_update_dialog_open = false;
                }

                // Info über lokale Installation
                let current_ver = env!("CARGO_PKG_VERSION");
                let runtime_name = updater.runtime().display_name();
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Aktuelle Version:").color(egui::Color32::from_gray(180)));
                    ui.label(egui::RichText::new(format!("v{}", current_ver)).strong().color(egui::Color32::WHITE));
                    ui.label(egui::RichText::new(format!("({})", runtime_name)).color(egui::Color32::from_gray(150)));
                });

                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Server:").color(egui::Color32::from_gray(180)));
                    ui.label(egui::RichText::new(format!("{}/{}/{}", updater.config().server_url, updater.config().repo_owner, updater.config().repo_name)).color(egui::Color32::from_gray(210)));
                });

                ui.add_space(10.0);

                // Update-Prüfung auslösen
                let is_busy = matches!(updater.status(), crate::updater::UpdaterStatus::Checking | crate::updater::UpdaterStatus::Downloading { .. });
                ui.horizontal(|ui| {
                    if ui.add_enabled(!is_busy, egui::Button::new(egui::RichText::new("🔄 Nach Updates suchen").strong())).clicked() {
                        actions.push(UiAction::CheckForUpdates);
                    }
                });

                ui.add_space(10.0);
                ui.separator();
                ui.add_space(10.0);

                // Statusbereich
                match updater.status() {
                    crate::updater::UpdaterStatus::Idle => {
                        ui.label(
                            egui::RichText::new("Klicke auf 'Nach Updates suchen', um die neuesten Releases vom Gitea-Server abzufragen.")
                                .color(egui::Color32::from_gray(170)),
                        );
                    }
                    crate::updater::UpdaterStatus::Checking => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(egui::RichText::new("Prüfe Gitea-Releases...").color(egui::Color32::WHITE));
                        });
                    }
                    crate::updater::UpdaterStatus::UpToDate { current_version } => {
                        ui.horizontal(|ui| {
                            ui.colored_label(egui::Color32::from_rgb(46, 204, 113), "✔");
                            ui.label(
                                egui::RichText::new(format!("ink-notes ist auf dem neuesten Stand (v{}).", current_version))
                                    .color(egui::Color32::from_rgb(46, 204, 113))
                                    .strong(),
                            );
                        });
                    }
                    crate::updater::UpdaterStatus::Error(err) => {
                        ui.vertical(|ui| {
                            ui.horizontal(|ui| {
                                ui.colored_label(egui::Color32::from_rgb(231, 76, 60), "⚠");
                                ui.label(
                                    egui::RichText::new("Abfrage fehlgeschlagen:")
                                        .color(egui::Color32::from_rgb(231, 76, 60))
                                        .strong(),
                                );
                            });
                            ui.add_space(4.0);
                            ui.label(
                                egui::RichText::new(err)
                                    .size(11.5)
                                    .color(egui::Color32::from_gray(190)),
                            );
                            ui.add_space(4.0);
                            ui.label(
                                egui::RichText::new("Hinweis: Falls das Repository privat ist, unten ein Personal Access Token eintragen.")
                                    .size(11.0)
                                    .italics()
                                    .color(egui::Color32::from_gray(140)),
                            );
                        });
                    }
                    crate::updater::UpdaterStatus::UpdateAvailable(release) => {
                        ui.vertical(|ui| {
                            ui.horizontal(|ui| {
                                ui.colored_label(egui::Color32::from_rgb(46, 204, 113), "⚫");
                                ui.label(
                                    egui::RichText::new(format!("Neue Version verfügbar: {}", release.tag_name))
                                        .strong()
                                        .size(14.0)
                                        .color(egui::Color32::WHITE),
                                );
                            });

                            if !release.name.is_empty() && release.name != release.tag_name {
                                ui.label(egui::RichText::new(&release.name).italics().color(egui::Color32::from_gray(180)));
                            }

                            if !release.body.is_empty() {
                                ui.add_space(6.0);
                                ui.label(egui::RichText::new("Änderungen:").strong().size(11.5).color(egui::Color32::from_gray(170)));
                                egui::ScrollArea::vertical().max_height(100.0).show(ui, |ui| {
                                    ui.label(egui::RichText::new(&release.body).size(11.5).color(egui::Color32::from_gray(210)));
                                });
                            }

                            ui.add_space(10.0);
                            ui.label(egui::RichText::new("VERFÜGBARE ARTEFAKTE:").size(10.5).strong().color(egui::Color32::from_gray(150)));
                            ui.add_space(4.0);

                            for asset in &release.assets {
                                let is_deb = asset.name.ends_with(".deb");
                                let is_appimage = asset.name.ends_with(".AppImage");
                                let is_recommended = match updater.runtime() {
                                    crate::updater::RuntimeEnvironment::AppImage { .. } => is_appimage,
                                    crate::updater::RuntimeEnvironment::DebPackage { .. } => is_deb,
                                    _ => false,
                                };

                                let size_mb = (asset.size as f64) / (1024.0 * 1024.0);
                                let btn_text = if size_mb > 0.05 {
                                    format!("📥 {} ({:.1} MB)", asset.name, size_mb)
                                } else {
                                    format!("📥 {}", asset.name)
                                };

                                ui.horizontal(|ui| {
                                    let btn = if is_recommended {
                                        ui.button(egui::RichText::new(format!("⭐ {}", btn_text)).strong().color(egui::Color32::from_rgb(110, 195, 255)))
                                    } else {
                                        ui.button(egui::RichText::new(&btn_text))
                                    };

                                    if btn.clicked() {
                                        actions.push(UiAction::DownloadUpdateAsset(asset.clone()));
                                    }

                                    if is_recommended {
                                        ui.label(egui::RichText::new("(Empfohlen für dieses System)").size(10.5).color(egui::Color32::from_rgb(46, 204, 113)));
                                    }
                                });
                            }

                            ui.add_space(8.0);
                            if !release.html_url.is_empty()
                                && ui.button(egui::RichText::new("🌐 Release auf Gitea öffnen").size(12.0)).clicked()
                            {
                                actions.push(UiAction::OpenUrlInBrowser(release.html_url.clone()));
                            }
                        });
                    }
                    crate::updater::UpdaterStatus::Downloading { asset_name, percent } => {
                        ui.vertical(|ui| {
                            ui.label(egui::RichText::new(format!("Lade '{}' herunter...", asset_name)).strong());
                            ui.add_space(6.0);
                            ui.add(egui::ProgressBar::new(*percent).show_percentage());
                        });
                    }
                    crate::updater::UpdaterStatus::DownloadFinished { file_path, asset_name } => {
                        ui.vertical(|ui| {
                            ui.horizontal(|ui| {
                                ui.colored_label(egui::Color32::from_rgb(46, 204, 113), "✔");
                                ui.label(
                                    egui::RichText::new(format!("'{}' erfolgreich heruntergeladen!", asset_name))
                                        .strong()
                                        .color(egui::Color32::from_rgb(46, 204, 113)),
                                );
                            });
                            ui.add_space(4.0);
                            ui.label(
                                egui::RichText::new(format!("Gespeichert in: {}", file_path.display()))
                                    .size(11.0)
                                    .color(egui::Color32::from_gray(170)),
                            );
                            ui.add_space(8.0);

                            let action_label = match updater.runtime() {
                                crate::updater::RuntimeEnvironment::AppImage { .. } => "🚀 AppImage aktualisieren & ausführen",
                                crate::updater::RuntimeEnvironment::DebPackage { .. } => "📦 Paket mit Software-Center öffnen / installieren",
                                crate::updater::RuntimeEnvironment::PortableOrDev { .. } => "📂 Im Dateimanager anzeigen",
                            };

                            ui.horizontal(|ui| {
                                if ui.button(egui::RichText::new(action_label).strong()).clicked() {
                                    actions.push(UiAction::InstallOrOpenUpdate(file_path.clone()));
                                }
                                if ui.button("📂 Ordner öffnen").clicked() {
                                    if let Some(parent) = file_path.parent() {
                                        actions.push(UiAction::OpenUrlInBrowser(parent.to_string_lossy().to_string()));
                                    }
                                }
                            });
                        });
                    }
                }

                ui.add_space(12.0);
                ui.separator();
                ui.add_space(4.0);

                // Erweiterte Konfiguration (Collapsible)
                egui::CollapsingHeader::new(egui::RichText::new("⚙ Erweiterte Server-Einstellungen").size(11.5).color(egui::Color32::from_gray(160)))
                    .default_open(false)
                    .show(ui, |ui| {
                        ui.add_space(4.0);
                        ui.label(egui::RichText::new("Gitea Server-URL:").size(11.0));
                        ui.add(egui::TextEdit::singleline(&mut state.updater_server_url_input).desired_width(380.0));

                        ui.add_space(4.0);
                        ui.label(egui::RichText::new("Personal Access Token (optional für private Repos):").size(11.0));
                        ui.add(egui::TextEdit::singleline(&mut state.updater_token_input).password(true).desired_width(380.0));

                        ui.add_space(6.0);
                        if ui.button(egui::RichText::new("💾 Einstellungen speichern").size(11.5)).clicked() {
                            let token = if state.updater_token_input.trim().is_empty() {
                                None
                            } else {
                                Some(state.updater_token_input.trim().to_string())
                            };
                            actions.push(UiAction::SaveUpdaterConfig {
                                server_url: state.updater_server_url_input.trim().to_string(),
                                auth_token: token,
                            });
                        }
                    });
            });
        state.update_modal_rect = win.map(|w| w.response.rect);
    } else {
        state.update_modal_rect = None;
    }

    // 4. Feedback-Toast für PDF-Export oder wichtige Systemmeldungen
    if let Some((msg, timestamp)) = &state.export_message {
        let elapsed = timestamp.elapsed().as_secs_f32();
        if elapsed < 4.0 {
            let alpha = if elapsed > 3.0 {
                ((4.0 - elapsed) * 255.0) as u8
            } else {
                255
            };

            egui::Area::new(egui::Id::new("export_toast_banner"))
                .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -35.0))
                .order(egui::Order::Tooltip)
                .show(ctx, |ui| {
                    egui::Frame::none()
                        .fill(egui::Color32::from_rgba_unmultiplied(
                            20,
                            25,
                            30,
                            alpha.min(235),
                        ))
                        .rounding(egui::Rounding::same(10.0))
                        .stroke(egui::Stroke::new(
                            1.0_f32,
                            egui::Color32::from_rgba_unmultiplied(46, 204, 113, alpha),
                        ))
                        .inner_margin(egui::Margin::symmetric(20.0, 10.0))
                        .shadow(egui::epaint::Shadow {
                            offset: egui::vec2(0.0, 4.0),
                            blur: 16.0,
                            spread: 0.0,
                            color: egui::Color32::from_black_alpha(alpha.min(130)),
                        })
                        .show(ui, |ui| {
                            ui.label(
                                egui::RichText::new(format!("✔ {}", msg))
                                    .color(egui::Color32::from_rgba_unmultiplied(
                                        220, 255, 230, alpha,
                                    ))
                                    .size(13.0)
                                    .strong(),
                            );
                        });
                });
        } else {
            state.export_message = None;
        }
    }

    // 5. Nicht-wegklickbare Warnung bei Ladefehlern / Quarantäne (Block 1)
    if let Some(warning) = notebook.load_warnings.get(&notebook.current_page().id) {
        dialog_window("⚠ Datenintegritäts-Warnung", state, true)
            .show(ctx, |ui| {
                ui.set_width(420.0);
                touch_friendly_spacing(ui);
                ui.horizontal(|ui| {
                    ui.colored_label(egui::Color32::from_rgb(255, 80, 80), "⚠");
                    ui.label(
                        egui::RichText::new(format!(
                            "Seite {} konnte nicht fehlerfrei geladen werden!",
                            warning.page_id
                        ))
                        .strong()
                        .size(15.0)
                        .color(egui::Color32::WHITE),
                    );
                });
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new(format!("Fehlermeldung: {}", warning.message))
                        .size(12.0)
                        .color(egui::Color32::from_gray(200)),
                );
                ui.add_space(8.0);
                if let Some(ref q_path) = warning.quarantine_path {
                    ui.label(
                        egui::RichText::new("Die beschädigte Originaldatei wurde in Quarantäne gesichert:")
                            .size(12.0)
                            .color(egui::Color32::from_gray(220)),
                    );
                    ui.add_space(4.0);
                    ui.monospace(
                        egui::RichText::new(q_path.display().to_string())
                            .size(11.0)
                            .color(egui::Color32::from_rgb(255, 215, 0)),
                    );
                    ui.add_space(10.0);
                    ui.label(
                        egui::RichText::new(
                            "Um Datenverlust zu verhindern, ist diese Seite schreibgeschützt.\n\
                             Wenn Sie diese Seite bearbeiten möchten, können Sie den Schreibschutz aufheben.",
                        )
                        .size(11.5)
                        .color(egui::Color32::from_gray(180)),
                    );
                    ui.add_space(12.0);
                    if ui
                        .button(
                            egui::RichText::new("Als leere Seite bearbeiten (Schreibschutz aufheben)")
                                .color(egui::Color32::WHITE)
                                .size(12.5),
                        )
                        .clicked()
                    {
                        actions.push(UiAction::UnlockCorruptedPage(warning.page_id));
                    }
                } else {
                    ui.label(
                        egui::RichText::new(
                            "Zugriffsfehler (I/O). Die Datei existiert, kann aber nicht gelesen werden.\n\
                             Die Seite bleibt schreibgeschützt, um ein Überschreiben zu verhindern.",
                        )
                        .size(12.0)
                        .color(egui::Color32::from_gray(200)),
                    );
                }
            });
    }

    // Notfall-/Sicherheits-Dialog bei Fehlschlag des Speicherns vor dem Beenden
    if let Some(err) = &state.shutdown_error {
        let mut dismiss = false;
        dialog_window("⚠ Ungespeicherte Änderungen", state, true).show(ctx, |ui| {
            ui.set_width(460.0);
            touch_friendly_spacing(ui);
            ui.horizontal(|ui| {
                ui.colored_label(egui::Color32::from_rgb(255, 80, 80), "⚠");
                ui.label(
                    egui::RichText::new("Fehler beim Beenden")
                        .color(egui::Color32::from_rgb(255, 100, 100))
                        .size(16.0)
                        .strong(),
                );
            });
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new("Ungespeicherte Änderungen konnten nicht gesichert werden:")
                    .size(13.0)
                    .color(egui::Color32::WHITE),
            );
            ui.add_space(6.0);
            egui::Frame::none()
                .fill(egui::Color32::from_black_alpha(100))
                .rounding(egui::Rounding::same(6.0))
                .inner_margin(egui::Margin::same(8.0))
                .show(ui, |ui| {
                    for msg in &err.messages {
                        ui.label(
                            egui::RichText::new(msg)
                                .size(11.5)
                                .color(egui::Color32::from_rgb(255, 180, 180)),
                        );
                    }
                });
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new("Trotzdem beenden?")
                    .size(13.0)
                    .color(egui::Color32::WHITE),
            );
            ui.add_space(12.0);
            ui.horizontal_wrapped(|ui| {
                if ui.button("Abbrechen").clicked() {
                    dismiss = true;
                }
                if ui.button("Wiederholen").clicked() {
                    actions.push(UiAction::RetryShutdown);
                }
                if ui.button("Notfall-Export").clicked() {
                    actions.push(UiAction::ExportRecoveryAndExit);
                }
                if ui
                    .button(
                        egui::RichText::new("Trotzdem beenden")
                            .color(egui::Color32::from_rgb(255, 100, 100)),
                    )
                    .clicked()
                {
                    actions.push(UiAction::ForceExit);
                }
            });
        });
        if dismiss {
            state.shutdown_error = None;
        }
    }

    actions
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ui_state_defaults() {
        let state = UiState::default();
        assert_eq!(state.active_tool, ActiveTool::Pen);
        assert_eq!(state.pen_stroke_width_mm, 0.5);
        assert_eq!(state.highlighter_stroke_width_mm, 6.0);
        assert_eq!(state.current_stroke_width_mm(), 0.5);
        assert!(!state.show_page_jump_popup);
        assert!(!state.is_pen_menu_open);
        assert!(!state.is_highlighter_menu_open);
        assert!(!state.is_document_menu_open);
        assert_eq!(state.current_notebook_id, "default");
        assert_eq!(state.document_title, "Standard-Notizbuch");
        assert!(state.export_message.is_none());
        assert!(state.toolbar_rect.is_none());
        assert!(state.popover_rect.is_none());
    }

    #[test]
    fn test_active_tool_properties() {
        assert!(ActiveTool::Pen.is_drawing());
        assert!(ActiveTool::Highlighter.is_drawing());
        assert!(!ActiveTool::Eraser.is_drawing());
        assert!(!ActiveTool::Select.is_drawing());
        assert_eq!(ActiveTool::Pen.name(), "Stift (Pen)");
        assert_eq!(ActiveTool::Eraser.name(), "Radierer (Eraser)");
        assert_eq!(ActiveTool::Select.name(), "Auswahl / Lasso (Select)");
    }

    #[test]
    fn test_render_ui_generates_shapes_and_rect() {
        let ctx = egui::Context::default();
        let mut state = UiState::new();
        let notebook = Notebook::new();
        let updater = crate::updater::UpdaterController::new();

        ctx.begin_frame(egui::RawInput::default());
        let actions = render_ui(&ctx, &mut state, &notebook, AutosaveStatus::Saved, &updater);
        let output = ctx.end_frame();

        assert!(actions.is_empty());
        assert!(!output.shapes.is_empty());
        assert!(state.toolbar_rect.is_some());
    }

    #[test]
    fn test_pill_hit_testing() {
        let mut state = UiState::new();
        state.toolbar_rect = Some(egui::Rect::from_min_max(
            egui::pos2(200.0, 16.0),
            egui::pos2(800.0, 60.0),
        ));

        // 1. Punkt im Zentrum der Pill -> getroffen
        assert!(state.is_point_inside_toolbar(500.0, 38.0));

        // 2. Punkt oberhalb der Pill (in der 16 px Lücke zum Fensterrand) -> frei
        assert!(!state.is_point_inside_toolbar(500.0, 8.0));

        // 3. Punkt weit außerhalb links/rechts -> frei
        assert!(!state.is_point_inside_toolbar(50.0, 38.0));
        assert!(!state.is_point_inside_toolbar(950.0, 38.0));

        // 4. Punkt unterhalb der Pill -> frei
        assert!(!state.is_point_inside_toolbar(500.0, 100.0));

        // 5. Eckpunkt ganz außen in der Bounding Box (außerhalb des 14 px Kreises) -> frei!
        assert!(!state.is_point_inside_toolbar(201.0, 17.0));
    }

    #[test]
    fn test_popover_hit_testing() {
        let mut state = UiState::new();
        state.toolbar_rect = Some(egui::Rect::from_min_max(
            egui::pos2(200.0, 16.0),
            egui::pos2(800.0, 60.0),
        ));
        state.popover_rect = Some(egui::Rect::from_min_max(
            egui::pos2(200.0, 68.0),
            egui::pos2(480.0, 240.0),
        ));

        // Klick in Toolbar -> UI
        assert!(state.is_point_in_ui(500.0, 38.0));
        // Klick in Popover -> UI
        assert!(state.is_point_in_ui(300.0, 120.0));
        // Klick außerhalb -> frei für Canvas
        assert!(!state.is_point_in_ui(600.0, 150.0));
    }

    #[test]
    fn test_pen_color_presets() {
        assert_eq!(PEN_COLOR_PRESETS.len(), 5);
        assert_eq!(PEN_COLOR_PRESETS[0].1, "Notiz-Schwarz");
        assert_eq!(PEN_COLOR_PRESETS[1].1, "Universitäts-Blau");
        assert_eq!(PEN_COLOR_PRESETS[2].1, "Korrektur-Rot");
        assert_eq!(PEN_COLOR_PRESETS[3].1, "Tafel-Grün");
        assert_eq!(PEN_COLOR_PRESETS[4].1, "Graphit-Grau");
    }

    #[test]
    fn test_pen_width_presets() {
        assert_eq!(PEN_WIDTH_PRESETS.len(), 4);
        assert_eq!(PEN_WIDTH_PRESETS[0].0, 0.3);
        assert_eq!(PEN_WIDTH_PRESETS[1].0, 0.5);
        assert_eq!(PEN_WIDTH_PRESETS[2].0, 0.8);
        assert_eq!(PEN_WIDTH_PRESETS[3].0, 1.2);
    }

    #[test]
    fn test_document_manager_ui_actions() {
        let mut state = UiState::new();
        state.is_document_menu_open = true;
        state.available_notebooks = vec![
            NotebookSummary {
                id: "nb-1".to_string(),
                title: "Mathematik I".to_string(),
                page_count: 3,
                modified_at: 100,
                status: crate::storage::NotebookStatus::Normal,
                error: None,
            },
            NotebookSummary {
                id: "nb-2".to_string(),
                title: "Physik II".to_string(),
                page_count: 5,
                modified_at: 200,
                status: crate::storage::NotebookStatus::Normal,
                error: None,
            },
        ];
        state.current_notebook_id = "nb-1".to_string();
        state.document_title = "Mathematik I".to_string();

        let notebook = Notebook::new();
        let updater = crate::updater::UpdaterController::new();
        let ctx = egui::Context::default();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            let _ = super::render_ui(ctx, &mut state, &notebook, AutosaveStatus::Saved, &updater);
        });

        assert!(state.is_document_menu_open);
        assert_eq!(state.available_notebooks.len(), 2);
    }

    #[test]
    fn test_updater_modal_ui_actions() {
        let mut state = UiState::new();
        state.is_update_dialog_open = true;

        let notebook = Notebook::new();
        let updater = crate::updater::UpdaterController::new();
        let ctx = egui::Context::default();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            let _ = super::render_ui(ctx, &mut state, &notebook, AutosaveStatus::Saved, &updater);
        });

        assert!(state.is_update_dialog_open);
        assert!(state.update_modal_rect.is_some());
        if let Some(rect) = state.update_modal_rect {
            assert!(state.is_point_in_ui(rect.center().x, rect.center().y));
        }
    }

    #[test]
    fn test_touch_draw_mode_toggle() {
        let mut state = UiState::new();
        assert!(!state.touch_draw_enabled); // Default: false = Gesten-Navigation & Zoom

        state.touch_draw_enabled = true;
        assert!(state.touch_draw_enabled);
    }

    /// Rendert mehrere Frames bei fester Fenstergröße. Angedockte Areas brauchen bis zu 6 Frames,
    /// bis egui ihre Größe vermessen hat und sie korrekt positioniert.
    fn render_frames(state: &mut UiState, notebook: &Notebook, frames: usize) -> egui::Context {
        render_frames_sized(state, notebook, frames, egui::vec2(1280.0, 850.0))
    }

    fn render_frames_sized(
        state: &mut UiState,
        notebook: &Notebook,
        frames: usize,
        screen: egui::Vec2,
    ) -> egui::Context {
        let ctx = egui::Context::default();
        let updater = crate::updater::UpdaterController::new();
        for _ in 0..frames {
            ctx.begin_frame(egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, screen)),
                ..Default::default()
            });
            let _ = render_ui(&ctx, state, notebook, AutosaveStatus::Saved, &updater);
            let _ = ctx.end_frame();
        }
        ctx
    }

    #[test]
    fn test_text_edit_overlay_takes_typing_and_escape() {
        let notebook = Notebook::new();
        let mut state = UiState::new();
        state.active_tool = ActiveTool::Text;
        state.page_view = Some(PageView {
            origin: egui::pos2(300.0, 50.0),
            scale: 0.3,
        });
        state.text_edit = Some(TextEditing::new_at(
            notebook.current_page().id,
            glam::Vec2::new(200.0, 300.0),
            PAGE_WIDTH,
            42.0,
            TextFont::Sans,
            [0.0, 0.0, 0.0, 1.0],
        ));
        let ctx = egui::Context::default();
        let updater = crate::updater::UpdaterController::new();
        let frame = |state: &mut UiState, events: Vec<egui::Event>| {
            ctx.begin_frame(egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1280.0, 850.0),
                )),
                events,
                ..Default::default()
            });
            let _ = render_ui(&ctx, state, &notebook, AutosaveStatus::Saved, &updater);
            let _ = ctx.end_frame();
        };
        frame(&mut state, Vec::new());
        assert!(ctx.wants_keyboard_input(), "Textfeld hat den Fokus");
        frame(&mut state, vec![egui::Event::Text("Hallo".into())]);
        assert_eq!(state.text_edit.as_ref().unwrap().text.content, "Hallo");
        // Zeilenumbruch und Tab (Einrücken) bleiben im Textfeld
        frame(
            &mut state,
            vec![egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::default(),
            }],
        );
        frame(
            &mut state,
            vec![egui::Event::Key {
                key: egui::Key::Tab,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::default(),
            }],
        );
        frame(&mut state, vec![egui::Event::Text("x".into())]);
        assert_eq!(state.text_edit.as_ref().unwrap().text.content, "Hallo\n\tx");

        frame(
            &mut state,
            vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::default(),
            }],
        );
        let edit = state.text_edit.as_ref().unwrap();
        assert!(edit.finish);
        assert_eq!(edit.text.content, "Hallo\n\tx");
    }

    #[test]
    fn test_sidebar_is_vertical_and_docked_to_chosen_side() {
        let notebook = Notebook::new();

        let mut left = UiState::new();
        render_frames(&mut left, &notebook, 10);
        let r = left.toolbar_rect.unwrap();
        assert!(
            r.height() > r.width() * 4.0,
            "Leiste muss senkrecht sein: {r:?}"
        );
        assert!(r.width() <= 60.0, "Leiste muss schmal sein: {r:?}");
        assert!(
            (r.min.x - UiState::SIDEBAR_MARGIN).abs() < 1.0,
            "links angedockt: {r:?}"
        );
        assert!(
            (r.min.y - UiState::SIDEBAR_MARGIN).abs() < 1.0,
            "oben angedockt: {r:?}"
        );
        assert!(
            r.max.y <= 850.0 && r.min.y >= 0.0,
            "passt ins Fenster: {r:?}"
        );
        assert!((left.sidebar_reserved_width() - (r.width() + 20.0)).abs() < 0.01);

        let mut right = UiState::new();
        right.sidebar_side = SidebarSide::Right;
        render_frames(&mut right, &notebook, 10);
        let r = right.toolbar_rect.unwrap();
        assert!(
            (r.max.x - (1280.0 - UiState::SIDEBAR_MARGIN)).abs() < 1.0,
            "rechts angedockt: {r:?}"
        );
    }

    #[test]
    fn test_popovers_open_towards_the_page() {
        let notebook = Notebook::new();
        for side in [SidebarSide::Left, SidebarSide::Right] {
            for open in ["pen", "more"] {
                let mut state = UiState::new();
                state.sidebar_side = side;
                match open {
                    "pen" => state.is_pen_menu_open = true,
                    _ => state.is_more_menu_open = true,
                }
                render_frames(&mut state, &notebook, 10);
                let bar = state.toolbar_rect.unwrap();
                let pop = state.popover_rect.expect("Popover muss offen sein");
                match side {
                    SidebarSide::Left => {
                        assert!(pop.min.x > bar.max.x, "{open}: {pop:?} / {bar:?}")
                    }
                    SidebarSide::Right => {
                        assert!(pop.max.x < bar.min.x, "{open}: {pop:?} / {bar:?}")
                    }
                }
                assert!(
                    pop.min.y >= 0.0 && pop.max.y <= 850.0,
                    "{open} im Fenster: {pop:?}"
                );
                assert!(state.is_point_in_ui(pop.center().x, pop.center().y));
            }
        }
    }

    #[test]
    fn test_notebook_rename_delete_and_open_are_clickable() {
        let notebook = Notebook::new();
        let fresh = || {
            let mut state = UiState::new();
            state.is_document_menu_open = true;
            state.available_notebooks = ["default", "zweites"]
                .iter()
                .map(|id| NotebookSummary {
                    id: id.to_string(),
                    title: format!("Notizbuch {id}"),
                    page_count: 1,
                    modified_at: 0,
                    status: Default::default(),
                    error: None,
                })
                .collect();
            state
        };
        let mut probe = fresh();
        render_frames(&mut probe, &notebook, 10);
        let modal = probe.document_modal_rect.expect("Notizbuch-Dialog offen");

        let updater = crate::updater::UpdaterController::new();
        let (mut deleted, mut renamed, mut opened) = (false, false, false);
        let mut y = modal.min.y + 4.0;
        while y < modal.max.y && !(deleted && renamed && opened) {
            let mut x = modal.min.x + 4.0;
            while x < modal.max.x {
                let mut state = fresh();
                let ctx = render_frames(&mut state, &notebook, 3);
                let pos = egui::pos2(x, y);
                let mut actions = Vec::new();
                for pressed in [true, false] {
                    ctx.begin_frame(egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(1280.0, 850.0),
                        )),
                        events: vec![
                            egui::Event::PointerMoved(pos),
                            egui::Event::PointerButton {
                                pos,
                                button: egui::PointerButton::Primary,
                                pressed,
                                modifiers: egui::Modifiers::default(),
                            },
                        ],
                        ..Default::default()
                    });
                    actions.extend(render_ui(
                        &ctx,
                        &mut state,
                        &notebook,
                        AutosaveStatus::Saved,
                        &updater,
                    ));
                    let _ = ctx.end_frame();
                }
                deleted |= actions
                    .iter()
                    .any(|a| matches!(a, UiAction::DeleteNotebook(_)));
                opened |= actions
                    .iter()
                    .any(|a| matches!(a, UiAction::OpenNotebook(_)));
                renamed |= state.renaming_notebook_id.is_some();
                x += 8.0;
            }
            y += 6.0;
        }
        assert!(deleted, "Löschen-Knopf reagiert nicht");
        assert!(renamed, "Umbenennen-Knopf reagiert nicht");
        assert!(opened, "Titel öffnet das Notizbuch nicht");
    }

    #[test]
    fn test_sidebar_buttons_react_to_raw_pointer_events() {
        // Gleicher Weg wie die Stift-Eingabe: PointerMoved + PointerButton in die RawInput
        let notebook = Notebook::new();
        let mut probe = UiState::new();
        render_frames(&mut probe, &notebook, 10);
        let bar = probe.toolbar_rect.unwrap();

        let updater = crate::updater::UpdaterController::new();
        let mut found = Vec::new();
        let mut y = bar.min.y + 4.0;
        while y < bar.max.y {
            let mut state = UiState::new();
            let ctx = render_frames(&mut state, &notebook, 10);
            let pos = egui::pos2(bar.center().x, y);
            let screen = Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1280.0, 850.0),
            ));
            let mut actions = Vec::new();
            for (pressed, with_move) in [(true, true), (false, false)] {
                let mut events = Vec::new();
                if with_move {
                    events.push(egui::Event::PointerMoved(pos));
                }
                events.push(egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::default(),
                });
                ctx.begin_frame(egui::RawInput {
                    screen_rect: screen,
                    events,
                    ..Default::default()
                });
                actions.extend(render_ui(
                    &ctx,
                    &mut state,
                    &notebook,
                    AutosaveStatus::Saved,
                    &updater,
                ));
                let _ = ctx.end_frame();
            }
            for a in actions {
                if let UiAction::SetTool(t) = a {
                    if !found.contains(&t) {
                        found.push(t);
                    }
                }
            }
            y += 6.0;
        }
        for tool in [
            ActiveTool::Highlighter,
            ActiveTool::Shape,
            ActiveTool::Eraser,
            ActiveTool::Select,
            ActiveTool::Text,
        ] {
            assert!(
                found.contains(&tool),
                "{tool:?} nicht per Klick erreichbar, gefunden: {found:?}"
            );
        }
    }

    #[test]
    fn test_sidebar_fits_window_height_for_every_tool() {
        let notebook = Notebook::new();
        for height in [850.0, 768.0] {
            for tool in [
                ActiveTool::Pen,
                ActiveTool::Highlighter,
                ActiveTool::Shape,
                ActiveTool::Eraser,
                ActiveTool::Select,
                ActiveTool::Text,
            ] {
                let mut state = UiState::new();
                state.active_tool = tool;
                render_frames_sized(&mut state, &notebook, 10, egui::vec2(1280.0, height));
                let r = state.toolbar_rect.unwrap();
                assert!(
                    r.min.y >= 0.0 && r.max.y <= height,
                    "{tool:?} bei {height} px Höhe: {r:?}"
                );
                // Ohne Scrollen: Würde gescrollt, wäre die Leiste genau Maximalhöhe + Rahmen hoch
                let max_inner = height - 2.0 * UiState::SIDEBAR_MARGIN - 12.0;
                assert!(
                    r.height() < max_inner + 12.0 - 0.5,
                    "{tool:?} bei {height} px Höhe muss ohne Scrollen passen: {r:?}"
                );
            }
        }
        // Große Fenster behalten die volle Knopfgröße
        assert_eq!(
            UiState::sidebar_button_size(ActiveTool::Select, 1000.0, true),
            40.0
        );
    }

    #[test]
    fn test_sidebar_buttons_stay_in_place_when_switching_tools() {
        // Gleiche Lage und Höhe = Undo/Redo, Seiten und Menü bleiben beim Werkzeugwechsel stehen
        let notebook = Notebook::new();
        let mut rects = Vec::new();
        for tool in [
            ActiveTool::Pen,
            ActiveTool::Highlighter,
            ActiveTool::Shape,
            ActiveTool::Eraser,
        ] {
            let mut state = UiState::new();
            state.active_tool = tool;
            render_frames(&mut state, &notebook, 10);
            rects.push((tool, state.toolbar_rect.unwrap()));
        }
        let first = rects[0].1;
        for (tool, r) in &rects {
            assert!(
                (r.min.y - first.min.y).abs() < 0.5 && (r.height() - first.height()).abs() < 0.5,
                "{tool:?}: {r:?} statt {first:?}"
            );
        }

        // Auswahl: gleiche Knopfgröße (Breite) und Oberkante, nur unten kommen Aktionen dazu.
        // Sonst verrutschen die Werkzeugknöpfe und das Blatt verschiebt sich.
        let mut state = UiState::new();
        state.active_tool = ActiveTool::Select;
        render_frames(&mut state, &notebook, 10);
        let r = state.toolbar_rect.unwrap();
        assert!(
            (r.width() - first.width()).abs() < 0.5,
            "Auswahl: {r:?} statt {first:?}"
        );
        assert!(
            (r.min.y - first.min.y).abs() < 0.5,
            "Auswahl: {r:?} statt {first:?}"
        );
        assert!(
            r.height() > first.height(),
            "Auswahl-Aktionen fehlen: {r:?}"
        );
    }

    /// Quelltexte, deren String-Literale in der Oberfläche erscheinen können.
    const UI_SOURCES: [(&str, &str); 4] = [
        ("ui.rs", include_str!("ui.rs")),
        ("selection.rs", include_str!("selection.rs")),
        ("main.rs", include_str!("main.rs")),
        ("file_picker.rs", include_str!("file_picker.rs")),
    ];

    /// Liefert alle Zeichen aus String-Literalen eines Quelltexts (ohne Zeilenkommentare).
    fn string_literal_chars(src: &str) -> Vec<char> {
        let mut out = Vec::new();
        let mut chars = src.chars().peekable();
        let (mut in_str, mut in_comment) = (false, false);
        while let Some(c) = chars.next() {
            if in_comment {
                in_comment = c != '\n';
            } else if in_str {
                match c {
                    '\\' => {
                        chars.next();
                    }
                    '"' => in_str = false,
                    _ => out.push(c),
                }
            } else if c == '/' && chars.peek() == Some(&'/') {
                in_comment = true;
            } else if c == '"' {
                in_str = true;
            }
        }
        out
    }

    #[test]
    fn test_ruler_drag_moves_and_rotates() {
        let ctx = egui::Context::default();
        let notebook = Notebook::new();
        let updater = crate::updater::UpdaterController::new();
        let mut state = UiState::new();
        // 1 Seiteneinheit = 0,5 Punkte, Seite beginnt bei (100, 0)
        let view = PageView {
            origin: egui::pos2(100.0, 0.0),
            scale: 0.5,
        };
        state.page_view = Some(view);
        state.ruler = Some(Ruler::new(glam::Vec2::new(1000.0, 1000.0)));

        let frame = |state: &mut UiState, events: Vec<egui::Event>| {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1600.0, 1000.0),
                )),
                events,
                ..Default::default()
            };
            ctx.begin_frame(input);
            let _ = render_ui(&ctx, state, &notebook, AutosaveStatus::Saved, &updater);
            let _ = ctx.end_frame();
        };
        let press = |pos: egui::Pos2, pressed: bool| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        };

        // Fläche und Griff gehören zur Oberfläche, die Kanten zum Zeichnen
        let center = view.to_screen(glam::Vec2::new(1000.0, 1000.0));
        assert!(state.is_point_in_ui(center.x, center.y));
        let edge = view.to_screen(glam::Vec2::new(
            1000.0,
            1000.0 - crate::ruler::RULER_WIDTH / 2.0,
        ));
        assert!(!state.is_point_in_ui(edge.x, edge.y));

        // Verschieben: Fläche greifen und um 50 Punkte (= 100 Einheiten) nach unten ziehen
        frame(&mut state, vec![egui::Event::PointerMoved(center)]);
        frame(&mut state, vec![press(center, true)]);
        let target = center + egui::vec2(0.0, 50.0);
        frame(&mut state, vec![egui::Event::PointerMoved(target)]);
        assert!(state.ruler_drag.is_some());
        frame(&mut state, vec![press(target, false)]);
        let moved = state.ruler.unwrap();
        assert!((moved.center - glam::Vec2::new(1000.0, 1100.0)).length() < 0.5);
        assert!(state.ruler_drag.is_none());

        // Drehen: Griff greifen und nach unten rechts ziehen → 45° (eingerastet)
        let handle = view.to_screen(moved.handle_center());
        frame(&mut state, vec![egui::Event::PointerMoved(handle)]);
        frame(&mut state, vec![press(handle, true)]);
        let diag = view.to_screen(moved.center + glam::Vec2::new(700.0, 690.0));
        frame(&mut state, vec![egui::Event::PointerMoved(diag)]);
        frame(&mut state, vec![press(diag, false)]);
        assert_eq!(state.ruler.unwrap().display_degrees(), 45);

        // Ausblenden
        state.toggle_ruler();
        assert!(state.ruler.is_none());
    }

    #[test]
    fn test_stroke_tool_follows_active_tool() {
        assert_eq!(ActiveTool::Pen.stroke_tool(false), StrokeTool::Pen);
        assert_eq!(
            ActiveTool::Highlighter.stroke_tool(false),
            StrokeTool::Highlighter
        );
        assert_eq!(ActiveTool::Shape.stroke_tool(false), StrokeTool::Pen);
        // Eingerastete Form (Formwerkzeug oder Halten am Strichende)
        assert_eq!(ActiveTool::Shape.stroke_tool(true), StrokeTool::Shape);
        assert_eq!(ActiveTool::Pen.stroke_tool(true), StrokeTool::Shape);
    }

    #[test]
    fn test_all_ui_texts_are_renderable() {
        // Zeichen ohne Glyphe in den eingebauten egui-Schriften erscheinen als leere Kästchen
        let ctx = egui::Context::default();
        ctx.begin_frame(egui::RawInput::default());
        let font = egui::FontId::proportional(14.0);
        let mut missing = std::collections::BTreeSet::new();
        for (name, src) in UI_SOURCES {
            for c in string_literal_chars(src) {
                if !c.is_ascii() && !ctx.fonts(|f| f.has_glyphs(&font, &c.to_string())) {
                    missing.insert(format!("{name}: {c} (U+{:04X})", c as u32));
                }
            }
        }
        let _ = ctx.end_frame();
        assert!(
            missing.is_empty(),
            "Nicht darstellbare Zeichen: {missing:#?}"
        );
    }

    #[test]
    fn test_no_invalid_premultiplied_colors() {
        // Bei vormultiplizierten Farben darf kein Farbkanal größer als Alpha sein,
        // sonst wird z. B. (255, 255, 255, 8) als fast deckendes Weiß gezeichnet
        let mut invalid = Vec::new();
        for (name, src) in UI_SOURCES {
            for part in src.split("from_rgba_premultiplied(").skip(1) {
                let args: Vec<u32> = part
                    .split(')')
                    .next()
                    .unwrap_or_default()
                    .split(',')
                    .filter_map(|a| a.trim().parse().ok())
                    .collect();
                if args.len() == 4 && args[..3].iter().any(|&v| v > args[3]) {
                    invalid.push(format!("{name}: {args:?}"));
                }
            }
        }
        assert!(
            invalid.is_empty(),
            "Ungültige vormultiplizierte Farben: {invalid:#?}"
        );
    }

    #[test]
    fn test_canvas_points_are_not_ui() {
        // Regression v0.3.0: eine bildschirmfüllende Background-Ebene wurde als UI gewertet,
        // dadurch ließ sich auf dem Blatt nicht mehr zeichnen
        let notebook = Notebook::new();
        for scale in [1.0_f32, 2.0] {
            let mut state = UiState::new();
            let ctx = render_frames(&mut state, &notebook, 10);
            let bar = state.toolbar_rect.unwrap();
            let phys = |p: egui::Pos2| (p.x * scale, p.y * scale);

            for canvas_pt in [egui::pos2(600.0, 400.0), egui::pos2(1200.0, 800.0)] {
                assert!(
                    !is_screen_point_in_ui(&ctx, &state, phys(canvas_pt), scale),
                    "Blattpunkt {canvas_pt:?} darf keine UI sein (Skalierung {scale})"
                );
            }
            assert!(
                is_screen_point_in_ui(&ctx, &state, phys(bar.center()), scale),
                "Leiste muss UI sein (Skalierung {scale})"
            );
        }
    }

    /// Simuliert einen Klick (Drücken und Loslassen) über rohe Eingabe-Ereignisse.
    fn click_at(
        ctx: &egui::Context,
        state: &mut UiState,
        notebook: &Notebook,
        pos: egui::Pos2,
    ) -> Vec<UiAction> {
        let updater = crate::updater::UpdaterController::new();
        let screen = Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1280.0, 850.0),
        ));
        let mut actions = Vec::new();
        for pressed in [true, false] {
            let mut events = vec![egui::Event::PointerMoved(pos)];
            events.push(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::default(),
            });
            ctx.begin_frame(egui::RawInput {
                screen_rect: screen,
                events,
                ..Default::default()
            });
            actions.extend(render_ui(
                ctx,
                state,
                notebook,
                AutosaveStatus::Saved,
                &updater,
            ));
            let _ = ctx.end_frame();
        }
        actions
    }

    #[test]
    fn test_only_one_dialog_or_popover_is_open() {
        let notebook = Notebook::new();
        let mut state = UiState::new();
        state.show_page_jump_popup = true;
        let ctx = render_frames(&mut state, &notebook, 10);
        let bar = state.toolbar_rect.unwrap();
        // Erster Knopf der Leiste (Notizbücher): Rahmenrand 6 px + halbe Knopfhöhe (≥ 28 px)
        let books = egui::pos2(bar.center().x, bar.min.y + 6.0 + 14.0);
        let actions = click_at(&ctx, &mut state, &notebook, books);
        assert!(
            state.is_document_menu_open,
            "Dokumenten-Manager muss offen sein"
        );
        assert!(
            !state.show_page_jump_popup,
            "Seitenauswahl muss geschlossen sein"
        );
        assert!(actions
            .iter()
            .any(|a| matches!(a, UiAction::RefreshNotebooks)));

        state.close_overlays();
        assert!(!state.is_document_menu_open && !state.show_page_jump_popup);
    }

    #[test]
    fn test_page_picker_jumps_to_every_page() {
        let mut notebook = Notebook::new();
        for _ in 0..5 {
            notebook.add_page(PageBackground::Grid5mm);
        }
        let mut probe = UiState::new();
        probe.show_page_jump_popup = true;
        render_frames(&mut probe, &notebook, 10);
        let bar = probe.toolbar_rect.unwrap();
        let pop = probe.popover_rect.expect("Seitenauswahl muss offen sein");
        assert!(pop.min.x > bar.max.x, "öffnet zur Blattseite hin: {pop:?}");
        assert!(
            pop.width() < 320.0,
            "so breit wie das Kachelraster: {pop:?}"
        );

        let mut reached = std::collections::BTreeSet::new();
        let mut y = pop.min.y + 4.0;
        while y < pop.max.y {
            let mut x = pop.min.x + 4.0;
            while x < pop.max.x {
                let mut state = UiState::new();
                state.show_page_jump_popup = true;
                let ctx = render_frames(&mut state, &notebook, 10);
                for a in click_at(&ctx, &mut state, &notebook, egui::pos2(x, y)) {
                    if let UiAction::GoToPage(idx) = a {
                        reached.insert(idx);
                        assert!(!state.show_page_jump_popup, "schließt nach der Auswahl");
                    }
                }
                x += 14.0;
            }
            y += 14.0;
        }
        assert_eq!(reached, (0..6).collect(), "alle Seiten erreichbar");
    }

    /// Seite mit `strokes` Strichen à `points` Punkten (diagonal über die Seite).
    fn page_with_strokes(strokes: usize, points: usize) -> Page {
        let mut page = Page::new(1, PageBackground::Grid5mm);
        for i in 0..strokes {
            let samples: Vec<crate::document::StylusSample> = (0..points)
                .map(|j| crate::document::StylusSample {
                    position: glam::Vec2::new(
                        100.0 + j as f32 * 1800.0 / points as f32,
                        100.0 + i as f32 * 20.0,
                    ),
                    pressure: 0.5,
                    timestamp: j as f64 * 0.01,
                })
                .collect();
            page.strokes.push(crate::document::Stroke::new(
                i as u64 + 1,
                samples,
                Vec::new(),
                [0.1, 0.1, 0.1, 1.0],
            ));
        }
        page
    }

    fn shape_points(shapes: &[egui::Shape]) -> Vec<egui::Pos2> {
        shapes
            .iter()
            .flat_map(|s| match s {
                egui::Shape::Path(p) => p.points.clone(),
                egui::Shape::Circle(c) => vec![c.center],
                _ => Vec::new(),
            })
            .collect()
    }

    #[test]
    fn test_page_thumbnail_shapes() {
        let rect = egui::Rect::from_min_size(egui::pos2(50.0, 80.0), egui::vec2(66.0, 93.0));

        // Leere Seite: nur das weiße Blatt
        let empty = Page::new(1, PageBackground::Blank);
        assert_eq!(
            page_thumbnail_shapes(&empty, rect, PaperTheme::White, true).len(),
            1
        );

        // Striche liegen innerhalb der Kachel
        let page = page_with_strokes(3, 20);
        let shapes = page_thumbnail_shapes(&page, rect, PaperTheme::White, true);
        assert_eq!(shapes.len(), 1 + 3);
        let pts = shape_points(&shapes);
        assert!(!pts.is_empty());
        assert!(
            pts.iter().all(|p| rect.contains(*p)),
            "Punkte außerhalb der Kachel"
        );

        // Sehr volle Seite wird auf das Punktbudget ausgedünnt (+ Endpunkte je Strich)
        let full = page_with_strokes(100, 1000);
        let pts = shape_points(&page_thumbnail_shapes(&full, rect, PaperTheme::White, true));
        assert!(
            pts.len() <= THUMBNAIL_POINT_BUDGET + 2 * 100,
            "{} Punkte statt höchstens {}",
            pts.len(),
            THUMBNAIL_POINT_BUDGET + 200
        );

        // Bild: gefüllte Fläche statt Umrisslinie, unter der Tinte
        let mut with_image = page_with_strokes(1, 5);
        with_image.strokes.insert(
            0,
            crate::document::Stroke::new_image(
                99,
                "a.png".into(),
                [
                    glam::Vec2::new(100.0, 100.0),
                    glam::Vec2::new(900.0, 100.0),
                    glam::Vec2::new(900.0, 600.0),
                    glam::Vec2::new(100.0, 600.0),
                ],
            ),
        );
        let shapes = page_thumbnail_shapes(&with_image, rect, PaperTheme::White, true);
        assert_eq!(shapes.len(), 3);
        assert!(
            matches!(shapes[1], egui::Shape::Path(ref p) if p.closed && p.fill != egui::Color32::TRANSPARENT)
        );
        assert!(matches!(shapes[2], egui::Shape::Path(ref p) if !p.closed));
    }

    #[test]
    fn test_page_picker_requests_loading_of_unloaded_pages() {
        let updater = crate::updater::UpdaterController::new();
        for (unloaded, expected) in [(false, false), (true, true)] {
            let mut notebook = Notebook::new();
            notebook.add_page(PageBackground::Grid5mm);
            if unloaded {
                notebook.pages[1] =
                    Page::new_unloaded(notebook.pages[1].id, PageBackground::Grid5mm);
            }
            let mut state = UiState::new();
            state.show_page_jump_popup = true;
            let ctx = render_frames(&mut state, &notebook, 3);
            ctx.begin_frame(egui::RawInput::default());
            let actions = render_ui(&ctx, &mut state, &notebook, AutosaveStatus::Saved, &updater);
            let _ = ctx.end_frame();
            let requested = actions.iter().any(|a| matches!(a, UiAction::LoadAllPages));
            assert_eq!(requested, expected, "ungeladene Seite: {unloaded}");
        }
    }
}
