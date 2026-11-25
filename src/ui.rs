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
use crate::background::{PaperTheme, PatternColor};
use crate::document::{EraserMode, Notebook, PageBackground, StrokeTool};
use crate::file_picker::{FilePicker, PickerKind};
use crate::ruler::Ruler;
use crate::settings::{
    PenPreset, PressureCurve, SidebarSide, DEFAULT_HIGHLIGHTER_COLORS, DEFAULT_PEN_PRESETS,
    DEFAULT_TEXT_COLORS, PEN_PRESET_COUNT,
};
use crate::storage::{NotebookLayout, NotebookSummary};
use crate::text::{TextEditing, TextFont};
use crate::{t, tn};

mod dialogs;
mod overlays;
mod popovers;
mod sidebar;
mod tabs;
mod widgets;

pub use overlays::page_thumbnail_shapes;
#[cfg(test)]
use overlays::{page_tile_size, scrollbar_layout, thumbnail_sheet, THUMBNAIL_POINT_BUDGET};
#[cfg(test)]
use widgets::{format_mm, PAGE_TILE_LABEL};

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
    /// Platz schaffen: Inhalt ab einer Höhe nach unten oder oben schieben
    Space,
}

impl ActiveTool {
    pub fn name(&self) -> &'static str {
        match self {
            ActiveTool::Pen => t!("Stift (Pen)"),
            ActiveTool::Highlighter => t!("Textmarker (Highlighter)"),
            ActiveTool::Eraser => t!("Radierer (Eraser)"),
            ActiveTool::Shape => t!("Formen (Shape)"),
            ActiveTool::Select => t!("Auswahl / Lasso (Select)"),
            ActiveTool::Text => t!("Text (T)"),
            ActiveTool::Space => t!("Platz schaffen"),
        }
    }

    pub fn is_drawing(&self) -> bool {
        matches!(self, ActiveTool::Pen | ActiveTool::Highlighter)
    }

    /// Werkzeug, das ein mit diesem Werkzeug gezeichneter Strich erhält.
    pub fn stroke_tool(&self, has_shape: bool) -> StrokeTool {
        match self {
            // Ein eingerasteter Textmarker bleibt Textmarker (durchscheinend, eigener Radierer)
            ActiveTool::Highlighter => StrokeTool::Highlighter,
            _ if has_shape => StrokeTool::Shape,
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
    ([0.12, 0.12, 0.14, 1.0], tn!("Notiz-Schwarz")),
    ([0.15, 0.35, 0.85, 1.0], tn!("Universitäts-Blau")),
    ([0.85, 0.20, 0.20, 1.0], tn!("Korrektur-Rot")),
    ([0.18, 0.65, 0.35, 1.0], tn!("Tafel-Grün")),
    ([0.45, 0.45, 0.48, 1.0], tn!("Graphit-Grau")),
];

/// Schnellauswahlfarben für den Textmarker (halbtransparent, Alpha 0.35):
/// - Neon-Gelb, Hellgrün, Pastell-Cyan, Rosa
pub const HIGHLIGHTER_COLOR_PRESETS: [([f32; 4], &str); 4] = [
    ([1.0, 0.95, 0.1, 0.35], tn!("Neon-Gelb")),
    ([0.2, 0.9, 0.3, 0.35], tn!("Hellgrün")),
    ([0.25, 0.7, 1.0, 0.35], tn!("Pastell-Cyan")),
    ([1.0, 0.3, 0.65, 0.35], tn!("Rosa")),
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

/// Eraser size presets as radius in mm, small to large (Issue #38).
pub const ERASER_RADII_MM: [f32; 4] = [1.5, 3.0, 6.0, 12.0];
/// Range of the eraser size slider (diameter in mm).
pub const ERASER_DIAMETER_RANGE_MM: (f32, f32) = (2.0, 30.0);

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

/// Erkennt langes Drücken mit einem Finger: still liegen (höchstens [`Self::SLOP`] logische
/// Punkte Bewegung) für [`Self::DURATION`]. Positionen in physischen Pixeln.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct LongPress {
    pending: Option<(u64, Instant, (f32, f32))>,
}

impl LongPress {
    pub const DURATION: std::time::Duration = std::time::Duration::from_millis(500);
    pub const SLOP: f32 = 10.0;

    /// Finger `id` hat bei `pos` aufgesetzt.
    pub fn start(&mut self, id: u64, pos: (f32, f32), now: Instant) {
        self.pending = Some((id, now, pos));
    }

    /// Bewegung: zu weit (oder ein anderer Finger) bricht ab.
    pub fn moved(&mut self, id: u64, pos: (f32, f32), scale: f32) {
        if let Some((pending, _, start)) = self.pending {
            if pending != id || (pos.0 - start.0).hypot(pos.1 - start.1) > Self::SLOP * scale {
                self.pending = None;
            }
        }
    }

    /// Finger abgehoben, zweiter Finger oder sonstiger Abbruch.
    pub fn cancel(&mut self) {
        self.pending = None;
    }

    /// Wann ausgelöst wird (zum Aufwecken der Ereignisschleife).
    pub fn deadline(&self) -> Option<Instant> {
        self.pending.map(|(_, started, _)| started + Self::DURATION)
    }

    /// Ist die Zeit um, wird einmal die Stelle des Drückens geliefert.
    pub fn fire(&mut self, now: Instant) -> Option<(f32, f32)> {
        let (_, started, pos) = self.pending?;
        (now >= started + Self::DURATION).then(|| {
            self.pending = None;
            pos
        })
    }
}

/// Was im Dokumenten-Manager gerade verschoben wird.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MoveSubject {
    Notebook(String),
    /// Pfad des Ordners
    Folder(String),
}

/// Laufendes Platzschaffen (für die Vorschau): Seite, Höhe auf der Seite, bisherige
/// Verschiebung und wie weit es nach oben höchstens geht (Seiteneinheiten).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpaceDrag {
    pub page_index: usize,
    pub y: f32,
    pub dy: f32,
    pub up_limit: f32,
}

impl SpaceDrag {
    /// Verschiebung nach oben begrenzt auf den leeren Platz.
    pub fn clamped_dy(&self) -> f32 {
        self.dy.max(-self.up_limit)
    }
}

/// Kontextmenü nach langem Drücken auf das Blatt.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ContextMenu {
    /// Bildschirmposition (logische Punkte) und Stelle auf der aktuellen Seite
    pub screen: egui::Pos2,
    pub page: glam::Vec2,
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

/// Aktiver Tab im ☰-Menü (hält das Popover kompakt und übersichtlich).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum MoreMenuTab {
    #[default]
    Page,
    Paper,
    Settings,
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
    /// Switch the view-only mode on or off (Issue #35)
    SetViewMode(bool),
    SetColor([f32; 4]),
    SetStrokeWidthMm(f32),
    /// Linienstil für Stift, Lineal und Formen umschalten
    SetLineStyle(crate::document::LineStyle),
    /// Füllart für geschlossene erkannte Formen (Keine / Vollfläche / Schraffur / Kreuzschraffur)
    SetShapeFill(crate::document::FillStyle),
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
    /// Lesezeichen für die Seite mit dieser ID umschalten
    ToggleBookmark(u64),
    /// Aktuelle Seite um `delta` (-1 / +1) in der Reihenfolge verschieben
    MoveCurrentPage(isize),
    /// Aktuelle Seite direkt dahinter duplizieren
    DuplicateCurrentPage,
    /// Volltext-Suche ausführen
    RunSearch,
    /// Zu einem Suchtreffer springen (Notizbuch + Seitenindex)
    JumpToSearchHit {
        notebook_id: String,
        page_index: usize,
    },
    /// Alle noch nicht geladenen Seiten laden (für die Vorschaubilder der Seitenauswahl)
    LoadAllPages,
    /// Render these PDF pages small for the page picker (see `UiState::pdf_thumbnails`)
    RequestPdfThumbnails(Vec<crate::document::PdfBackground>),
    SetBackground(PageBackground),
    ExportPdf,
    ZoomIn,
    ZoomOut,
    ResetZoom,
    DeleteSelected,
    CopySelected,
    PasteSelected,
    /// Zwischenablage an dieser Stelle der aktuellen Seite einfügen (Kontextmenü)
    PasteAt(glam::Vec2),
    RotateSelected,
    /// Seitenverhältnis beim Skalieren der Auswahl sperren bzw. freigeben
    ToggleAspectLock,
    /// Aktuelle Auswahl als wiederverwendbaren Stempel speichern (optionaler Name)
    SaveSelectionAsStamp(Option<String>),
    /// Gespeicherten Stempel per ID in der Blattmitte einfügen
    InsertStamp(String),
    /// Gespeicherten Stempel per ID löschen
    DeleteStamp(String),
    /// Offline-Handschrifterkennung: Sprachpaket herunterladen / entfernen / Notizbuch indexieren
    OcrDownloadModel,
    OcrRemoveModel,
    OcrIndexNotebook,
    /// Ausgewählte Handschrift-Striche in ein bearbeitbares Textfeld umwandeln
    ConvertSelectionToText,
    // Dokumenten- & Notizbuch-Verwaltung
    OpenNotebook(String),
    /// Einen geöffneten Notizbuch-Tab schließen (wechselt bei aktivem Tab zum Nachbar-Tab)
    CloseTab(String),
    /// Neues Notizbuch im Ordner `folder` anlegen und öffnen
    CreateNotebook {
        title: String,
        folder: String,
        layout: NotebookLayout,
    },
    /// Ordnerverwaltung (siehe `library.rs`)
    CreateFolder {
        parent: String,
        name: String,
    },
    RenameFolder {
        path: String,
        new_name: String,
    },
    /// Ordner löschen; Inhalt rückt eine Ebene nach oben
    DeleteFolder(String),
    MoveNotebook {
        id: String,
        folder: String,
    },
    /// Ordner samt Inhalt nach `new_parent` verschieben
    MoveFolder {
        path: String,
        new_parent: String,
    },
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
    /// Compute handwritten math after `=` on or off (Issue #46)
    SetMathAssist(bool),
    /// One finger draws or moves the page (saved, Issue #50)
    SetTouchDraw(bool),
    /// Sprache der Oberfläche; `None` = Sprache des Systems
    SetLanguage(Option<crate::i18n::Language>),
    /// Papierfarbe der Anzeige und Aufhellen dunkler Tinte (wird gespeichert)
    SetPaperTheme {
        theme: PaperTheme,
        adapt_ink: bool,
    },
    /// Belegung der Stifttasten speichern (gilt sofort über `UiState::stylus_buttons`)
    SaveStylusButtons,
    /// Druckkurve speichern (gilt während des Ziehens bereits über `UiState::pressure_curve`)
    SavePressureCurve(PressureCurve),
    /// Anordnung der Seiten des offenen Notizbuchs (wird im Manifest gespeichert)
    SetNotebookLayout(NotebookLayout),
    /// Durchgehende Ansicht: Fenstermitte auf diese Höhe (Koordinaten der aktuellen Seite)
    /// scrollen – Bildlaufleiste, Issue #28
    ScrollTo(f32),
    /// Farbe von Karos, Linien und Punkten (wird gespeichert, gilt auch für den Export)
    SetPatternColor(PatternColor),
    /// Bilddatei auf der aktuellen Seite einfügen
    InsertImageFile(std::path::PathBuf),
    /// PDF als neues Notizbuch importieren
    ImportFile(std::path::PathBuf),
    /// Store the notebooks in this folder, `None` for the default location (Issue #49)
    SetStorageDir(Option<std::path::PathBuf>),
    /// Sign in to a Nextcloud server through the browser; with the fingerprint of a
    /// certificate the user trusts (Issue #49, stage 2)
    CloudConnect {
        server: String,
        trusted: Option<String>,
    },
    CloudCancel,
    CloudSyncNow,
    CloudDisconnect,
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

/// Texture of a page picker thumbnail (egui's handle has no `Debug`).
#[derive(Clone)]
pub struct Thumbnail(pub egui::TextureHandle);

impl std::fmt::Debug for Thumbnail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Thumbnail({:?})", self.0.id())
    }
}

/// Zustand der grafischen Benutzeroberfläche.
#[derive(Debug, Clone)]
pub struct UiState {
    pub active_tool: ActiveTool,
    pub pen_color: [f32; 4],
    pub highlighter_color: [f32; 4],
    pub pen_stroke_width_mm: f32,
    pub highlighter_stroke_width_mm: f32,
    /// Linienstil für Stift, Lineal und Formen (durchgezogen / gestrichelt / gepunktet)
    pub line_style: crate::document::LineStyle,
    /// Füllart für geschlossene erkannte Formen
    pub shape_fill: crate::document::FillStyle,
    pub export_message: Option<(String, Instant)>,
    pub show_page_jump_popup: bool,
    /// Aktueller Zoomfaktor zur Anzeige in der Toolbar
    pub current_zoom: f32,
    /// Bildschirm-Rechteck der schwebenden Toolbar für pixelgenaues Hit-Testing
    pub toolbar_rect: Option<egui::Rect>,
    /// Geöffnete Notizbuch-Tabs (Notizbuch-IDs) und Bildschirm-Rechteck der Tab-Leiste
    pub open_tabs: Vec<String>,
    pub tab_bar_rect: Option<egui::Rect>,
    /// Öffnungszustand des Einstellungs-Popovers für den Stift
    pub is_pen_menu_open: bool,
    /// Eraser size menu next to the size button (Issue #38)
    pub is_eraser_menu_open: bool,
    /// Öffnungszustand des Einstellungs-Popovers für den Textmarker
    pub is_highlighter_menu_open: bool,
    /// Öffnungszustand des Stempel-Popovers (wiederverwendbare Elemente)
    pub is_stamps_menu_open: bool,
    /// Gespeicherte Stempel und Namensfeld im Stempel-Popover
    pub stamps: crate::stamps::StampLibrary,
    pub stamp_name_input: String,
    /// Bildschirm-Rechteck des geöffneten Einstellungs-Popovers
    pub popover_rect: Option<egui::Rect>,
    /// Dokumenten-Manager Zustand
    pub is_document_menu_open: bool,
    pub current_notebook_id: String,
    pub document_title: String,
    pub available_notebooks: Vec<NotebookSummary>,
    pub new_notebook_title_input: String,
    /// Anordnung für das nächste neue Notizbuch
    pub new_notebook_layout: NotebookLayout,
    pub renaming_notebook_id: Option<String>,
    /// Ordner der Notizbücher und der gerade angezeigte Ordner im Dokumenten-Manager
    pub library: crate::library::Library,
    pub notebook_folder: String,
    /// Ordner, der gerade umbenannt wird, bzw. dessen Löschen bestätigt werden soll
    pub renaming_folder: Option<String>,
    pub confirm_delete_folder: Option<String>,
    /// Notizbuch oder Ordner, für das gerade ein Zielordner gewählt wird
    pub moving: Option<MoveSubject>,
    pub renaming_title_input: String,
    pub document_modal_rect: Option<egui::Rect>,
    /// Volltext-Suche (Strg+F)
    pub is_search_open: bool,
    pub search_focus: bool,
    pub search_query: String,
    pub search_scope: crate::search::SearchScope,
    pub search_hits: Vec<crate::search::SearchHit>,
    pub search_modal_rect: Option<egui::Rect>,
    /// Gitea Release- & Update-Manager
    pub is_update_dialog_open: bool,
    pub updater_server_url_input: String,
    pub updater_token_input: String,
    pub show_updater_config: bool,
    pub update_modal_rect: Option<egui::Rect>,
    /// Steuert, ob Touch-Eingaben mit einem Finger zeichnen oder das Dokument verschieben (Default: false = reine Gesten-Navigation & Zoom)
    pub touch_draw_enabled: bool,
    /// Computes handwritten math after an `=` (Issue #46)
    pub math: crate::math_assist::MathAssist,
    /// Zuletzt gelöschtes Notizbuch für Rückgängig während der Sitzung (Papierkorb-Pfad, Titel)
    pub last_deleted_notebook: Option<(std::path::PathBuf, String)>,
    /// Stifte der Werkzeugleiste (Farbe und Stärke) und der gewählte davon; `pen_color` und
    /// `pen_stroke_width_mm` gehören immer zum gewählten Stift
    pub pen_presets: [PenPreset; PEN_PRESET_COUNT],
    pub active_pen: usize,
    /// Eigene Farben des Textmarkers in der Werkzeugleiste und die gewählte;
    /// `highlighter_color` gehört immer zur gewählten
    pub highlighter_colors: [[f32; 4]; 4],
    pub active_highlighter: usize,
    /// Eigene Farben des Text-Werkzeugs und die zuletzt angetippte (ein geöffnetes Textfeld
    /// kann eine andere Farbe haben)
    pub text_colors: [[f32; 4]; 5],
    pub active_text_color: usize,
    /// Popover „Textfarbe“ ist offen
    pub is_text_color_menu_open: bool,
    /// Farbwähler in den Farb-Popovern aufgeklappt
    pub show_color_picker: bool,
    /// „Meine Farben“ in the color popovers (Issue #56)
    pub saved_colors: Vec<[f32; 3]>,
    /// Kontextmenü nach langem Drücken (Einfügen an dieser Stelle)
    pub context_menu: Option<ContextMenu>,
    /// Laufendes Platzschaffen (Vorschau)
    pub space_drag: Option<SpaceDrag>,
    /// Tasten am Stift und ihre Belegung; die gerade gedrückte (Index) wird hervorgehoben
    pub stylus_buttons: Vec<crate::settings::StylusButton>,
    pub pressed_stylus_button: Option<usize>,
    /// Rückfrage „Seite löschen?“ im ☰-Menü ist offen
    pub confirm_delete_page: bool,
    /// Seitenzahl der zuletzt gelöschten Seite, solange sie wiederhergestellt werden kann
    pub restorable_page: Option<usize>,
    /// Seitenverhältnis-Schloss der Auswahl (`None`: nichts ausgewählt)
    pub selection_aspect_lock: Option<bool>,
    /// Fehler beim Flushen vor dem Beenden
    pub shutdown_error: Option<crate::autosave::AutosaveError>,
    /// Fensterseite der Werkzeugleiste
    pub sidebar_side: SidebarSide,
    /// Gewählte Sprache der Oberfläche; `None` = Sprache des Systems
    pub language: Option<crate::i18n::Language>,
    /// Öffnungszustand und aktiver Tab des ☰-Menüs
    pub is_more_menu_open: bool,
    pub more_menu_tab: MoreMenuTab,
    /// Welche Striche der Radierer entfernt (gilt auch für Stiftrückseite und Seitentaste)
    pub eraser_mode: EraserMode,
    /// Eraser radius in mm, one of [`ERASER_RADII_MM`] (also for the pen's eraser end)
    pub eraser_radius_mm: f32,
    /// Small renders of PDF pages for the page picker by [`crate::document::PdfBackground::key`];
    /// `None` if the page could not be rendered (Issue #40)
    pub pdf_thumbnails: std::collections::HashMap<String, Option<Thumbnail>>,
    /// Papierfarbe der Anzeige
    pub paper_theme: PaperTheme,
    /// Dunkle Tinte auf dunklem Papier aufhellen
    pub adapt_ink_colors: bool,
    /// Farbe von Karos, Linien und Punkten
    pub pattern_color: PatternColor,
    /// Seiten hängen untereinander und werden durchgehend gescrollt
    pub continuous_scroll: bool,
    /// Offenes Notizbuch ist eine endlose Fläche (eine Seite ohne Rand, Issue #30)
    pub endless: bool,
    /// View only: pen, finger and mouse just move the page, nothing gets changed (Issue #35)
    pub view_mode: bool,
    /// Wechsel zur bzw. von der endlosen Fläche ist gerade möglich (eine Seite ohne PDF bzw.
    /// Inhalt im Startausschnitt)
    pub endless_switch_possible: bool,
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
    /// Bildlaufleiste wird gezogen: Abstand des Fingers von der Oberkante des Griffs
    pub scrollbar_grab: Option<f32>,
    /// Aktuelle Abbildung Seite ↔ Bildschirm (für Lineal und Treffertests)
    pub page_view: Option<PageView>,
    /// Usable part of the window in logical points (from the last frame; on Android without the
    /// system bars)
    pub screen_rect: egui::Rect,
    /// Eingabefeld „Titel dieser Seite“ in der Seitenübersicht und die Seite, zu der es gehört
    pub page_title_input: String,
    pub page_title_for: Option<u64>,
    /// Seitenübersicht als Liste mit Titeln (Inhaltsverzeichnis) statt Kacheln
    pub page_picker_list: bool,
    /// Seitenübersicht auf Lesezeichen-Seiten filtern
    pub page_picker_bookmarks_only: bool,
    /// Aktionen, die außerhalb der Oberfläche ausgelöst wurden (z. B. ins Fenster gezogene
    /// PDF-Datei); `render_ui` gibt sie beim nächsten Bild mit aus
    pub queued_actions: Vec<UiAction>,
    /// Zuletzt in der Dateiauswahl benutzter Ordner (während der Sitzung)
    pub last_picker_dir: Option<std::path::PathBuf>,
    /// Folder holding the notebooks and whether the user picked it (Issue #49)
    pub storage_dir: std::path::PathBuf,
    pub storage_dir_custom: bool,
    /// Nextcloud sync as the app last saw it, and the server address being typed
    pub cloud: crate::cloud_sync::View,
    pub cloud_server_input: String,
    /// Status der Offline-Handschrifterkennung (ONNX-Sprachpaket)
    pub ocr_status: crate::handwriting_ocr::OcrStatus,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            active_tool: ActiveTool::Pen,
            pen_color: PEN_COLOR_PRESETS[0].0,
            highlighter_color: HIGHLIGHTER_COLOR_PRESETS[0].0,
            pen_stroke_width_mm: 0.5, // Standard: 0.5 mm (feine Handschrift)
            highlighter_stroke_width_mm: 6.0,
            line_style: crate::document::LineStyle::Solid,
            shape_fill: crate::document::FillStyle::None,
            export_message: None,
            show_page_jump_popup: false,
            current_zoom: 1.0,
            toolbar_rect: None,
            open_tabs: vec!["default".to_string()],
            tab_bar_rect: None,
            is_pen_menu_open: false,
            is_eraser_menu_open: false,
            is_highlighter_menu_open: false,
            is_stamps_menu_open: false,
            stamps: crate::stamps::StampLibrary::default(),
            stamp_name_input: String::new(),
            popover_rect: None,
            is_document_menu_open: false,
            current_notebook_id: "default".to_string(),
            document_title: t!("Standard-Notizbuch").to_string(),
            available_notebooks: Vec::new(),
            new_notebook_title_input: String::new(),
            new_notebook_layout: NotebookLayout::Continuous,
            renaming_notebook_id: None,
            library: crate::library::Library::default(),
            notebook_folder: String::new(),
            renaming_folder: None,
            confirm_delete_folder: None,
            moving: None,
            renaming_title_input: String::new(),
            document_modal_rect: None,
            is_search_open: false,
            search_focus: false,
            search_query: String::new(),
            search_scope: crate::search::SearchScope::CurrentNotebook,
            search_hits: Vec::new(),
            search_modal_rect: None,
            is_update_dialog_open: false,
            updater_server_url_input: "https://gitea.pansi.eu".to_string(),
            updater_token_input: String::new(),
            show_updater_config: false,
            update_modal_rect: None,
            touch_draw_enabled: false,
            math: crate::math_assist::MathAssist::default(),
            last_deleted_notebook: None,
            pen_presets: DEFAULT_PEN_PRESETS,
            active_pen: 0,
            highlighter_colors: DEFAULT_HIGHLIGHTER_COLORS,
            active_highlighter: 0,
            text_colors: DEFAULT_TEXT_COLORS,
            active_text_color: 0,
            is_text_color_menu_open: false,
            show_color_picker: false,
            saved_colors: Vec::new(),
            context_menu: None,
            space_drag: None,
            stylus_buttons: Vec::new(),
            pressed_stylus_button: None,
            confirm_delete_page: false,
            restorable_page: None,
            selection_aspect_lock: None,
            shutdown_error: None,
            sidebar_side: SidebarSide::Left,
            language: None,
            is_more_menu_open: false,
            more_menu_tab: MoreMenuTab::Page,
            eraser_mode: EraserMode::All,
            eraser_radius_mm: ERASER_RADII_MM[1],
            pdf_thumbnails: std::collections::HashMap::new(),
            paper_theme: PaperTheme::White,
            adapt_ink_colors: true,
            pattern_color: PatternColor::Light,
            continuous_scroll: true,
            endless: false,
            view_mode: false,
            endless_switch_possible: true,
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
            scrollbar_grab: None,
            page_view: None,
            screen_rect: egui::Rect::ZERO,
            page_title_input: String::new(),
            page_title_for: None,
            page_picker_list: false,
            page_picker_bookmarks_only: false,
            last_picker_dir: None,
            storage_dir: std::path::PathBuf::new(),
            storage_dir_custom: false,
            cloud: crate::cloud_sync::View::default(),
            cloud_server_input: String::new(),
            ocr_status: crate::handwriting_ocr::OcrStatus::NotInstalled,
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

    /// Sets the eraser radius within the slider range (e.g. from the settings file).
    pub fn set_eraser_radius_mm(&mut self, mm: f32) {
        let (min, max) = ERASER_DIAMETER_RANGE_MM;
        self.eraser_radius_mm = if mm.is_finite() {
            mm.clamp(min / 2.0, max / 2.0)
        } else {
            ERASER_RADII_MM[1]
        };
    }

    /// Eraser radius in page units.
    pub fn eraser_radius(&self) -> f32 {
        self.eraser_radius_mm * crate::document::UNITS_PER_MM
    }

    /// Schließt alle Popover und Dialoge. Wird vor dem Öffnen eines neuen aufgerufen, damit
    /// sich nie mehrere überlagern.
    pub fn close_overlays(&mut self) {
        self.confirm_delete_page = false;
        self.context_menu = None;
        self.is_text_color_menu_open = false;
        self.is_pen_menu_open = false;
        self.is_eraser_menu_open = false;
        self.is_highlighter_menu_open = false;
        self.is_stamps_menu_open = false;
        self.is_more_menu_open = false;
        self.show_page_jump_popup = false;
        self.is_document_menu_open = false;
        self.is_search_open = false;
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
        self.screen_rect.center()
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

    /// Opens the file picker "Import" (PDF, Xournal++, Rnote, Saber) in the last used folder.
    pub fn open_import_picker(&mut self) {
        self.open_file_picker(PickerKind::Import);
    }

    /// Opens the folder picker for the storage location in the home folder.
    pub fn open_storage_picker(&mut self) {
        self.close_overlays();
        self.file_picker = Some(FilePicker::open(PickerKind::Folder, None));
    }

    fn open_file_picker(&mut self, kind: PickerKind) {
        let dir = self.last_picker_dir.clone();
        self.close_overlays();
        // Android: the app can't look into other apps' folders; the system dialog can
        #[cfg(target_os = "android")]
        if crate::android_bridge::pick_file(kind) {
            return;
        }
        self.file_picker = Some(FilePicker::open(kind, dir.as_deref()));
    }

    /// Länge der längsten Farbpalette; so viel Platz hält die Leiste immer frei.
    pub const MAX_PALETTE_LEN: usize = PEN_PRESET_COUNT;
    /// Vertical gap between toolbar buttons and the height of a divider line.
    pub const SIDEBAR_SPACING: f32 = 2.0;
    pub const SIDEBAR_DIVIDER: f32 = 3.0;

    /// Durchmesser der Farbtupfer passend zur Knopfgröße.
    pub fn sidebar_swatch_size(button_size: f32) -> f32 {
        button_size - 10.0
    }

    /// Größte Knopfgröße (40 px, schrittweise bis 26 px), bei der die Leiste mit allen Elementen
    /// des aktiven Werkzeugs in `max_height` passt. `continuous`: durchgehende Ansicht (ohne
    /// Blätterpfeile).
    pub fn sidebar_button_size(tool: ActiveTool, max_height: f32, continuous: bool) -> f32 {
        let select = matches!(tool, ActiveTool::Select | ActiveTool::Text);
        // Notizbücher, Nur ansehen, 7 Werkzeuge, Lineal, Stempel, Undo/Redo, Seitenzahl und neue
        // Seite (einzeln blättern: dazu zwei Pfeile), Menü (+ 4 Auswahl-Aktionen bzw. Schriftknöpfe)
        let page_buttons = if continuous { 2 } else { 4 };
        let buttons = 13 + page_buttons + if select { 4 } else { 0 };
        let dividers = 5 + usize::from(select);
        let spacing = Self::SIDEBAR_SPACING;
        let needed = |b: f32| {
            buttons as f32 * (b + spacing)
                + Self::MAX_PALETTE_LEN as f32 * (Self::sidebar_swatch_size(b) + spacing)
                + dividers as f32 * (Self::SIDEBAR_DIVIDER + spacing)
                // frame, margins and the spacing egui adds (measured with the selection tool)
                + 40.0
        };
        [Self::SIDEBAR_BUTTON, 36.0, 32.0, 30.0, 28.0, 26.0, 24.0]
            .into_iter()
            .find(|&b| needed(b) <= max_height)
            .unwrap_or(24.0)
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

    /// Wählt die eigene Textmarker-Farbe `index`.
    pub fn select_highlighter_color(&mut self, index: usize) {
        self.active_highlighter = index.min(self.highlighter_colors.len() - 1);
        self.highlighter_color = self.highlighter_colors[self.active_highlighter];
    }

    /// Setzt die Textfarbe (auch für ein gerade bearbeitetes Textfeld).
    pub fn set_text_color(&mut self, color: [f32; 4]) {
        self.text_color = color;
        if let Some(edit) = &mut self.text_edit {
            edit.color = color;
            edit.focus = true;
        }
    }

    /// Übernimmt geänderte Farbe oder Stärke des Stifts in die Vorlage des gewählten Stifts
    /// und die Textmarker-Farbe in die gewählte eigene Farbe; `true`, wenn sich etwas
    /// geändert hat (dann speichern).
    pub fn sync_tool_colors(&mut self) -> bool {
        let index = self
            .active_highlighter
            .min(self.highlighter_colors.len() - 1);
        let highlighter_changed = self.highlighter_colors[index] != self.highlighter_color;
        self.highlighter_colors[index] = self.highlighter_color;
        self.sync_pen_preset() | highlighter_changed
    }

    /// Übernimmt geänderte Farbe oder Stärke des Stifts in die Vorlage des gewählten Stifts;
    /// `true`, wenn sie sich geändert hat (dann speichern).
    fn sync_pen_preset(&mut self) -> bool {
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
            ActiveTool::Eraser | ActiveTool::Space => [0.0, 0.0, 0.0, 0.0],
        }
    }

    /// Gibt die aktuelle Strichbreite in mm zurück.
    pub fn current_stroke_width_mm(&self) -> f32 {
        match self.active_tool {
            ActiveTool::Pen | ActiveTool::Shape | ActiveTool::Select | ActiveTool::Text => {
                self.pen_stroke_width_mm
            }
            ActiveTool::Highlighter => self.highlighter_stroke_width_mm,
            ActiveTool::Eraser | ActiveTool::Space => 3.0,
        }
    }

    /// Zeigt eine temporäre Erfolgs- oder Statusmeldung im UI an.
    pub fn set_export_message(&mut self, msg: impl Into<String>) {
        self.export_message = Some((msg.into(), Instant::now()));
    }

    /// Stellt sicher, dass das Notizbuch `id` als Tab geöffnet ist (bis höchstens `MAX_OPEN_TABS`).
    pub fn ensure_tab_open(&mut self, id: &str) {
        if id.is_empty() || self.open_tabs.iter().any(|t| t == id) {
            return;
        }
        while self.open_tabs.len() >= crate::settings::MAX_OPEN_TABS {
            if let Some(pos) = self
                .open_tabs
                .iter()
                .position(|t| t != &self.current_notebook_id)
            {
                self.open_tabs.remove(pos);
            } else {
                break;
            }
        }
        self.open_tabs.push(id.to_string());
    }

    /// Schließt den Tab `id`, sofern mehr als ein Tab offen ist.
    ///
    /// Gibt bei Schließen des aktiven Tabs die ID des Nachbar-Tabs zurück, zu dem gewechselt
    /// werden soll (`None`, wenn ein anderer Tab geschlossen wurde oder nur noch ein Tab offen war).
    pub fn close_tab(&mut self, id: &str) -> Option<String> {
        if self.open_tabs.len() <= 1 {
            return None;
        }
        let pos = self.open_tabs.iter().position(|t| t == id)?;
        self.open_tabs.remove(pos);
        if id == self.current_notebook_id {
            let next_idx = pos.min(self.open_tabs.len().saturating_sub(1));
            self.open_tabs.get(next_idx).cloned()
        } else {
            None
        }
    }

    /// Entfernt doppelte oder nicht mehr vorhandene Notizbuch-Tabs und stellt sicher, dass das
    /// aktuelle Notizbuch enthalten ist.
    pub fn prune_tabs(&mut self) {
        let mut seen = std::collections::HashSet::new();
        let has_list = !self.available_notebooks.is_empty();
        self.open_tabs.retain(|id| {
            if id.is_empty() || !seen.insert(id.clone()) {
                return false;
            }
            if id == &self.current_notebook_id {
                return true;
            }
            !has_list || self.available_notebooks.iter().any(|n| n.id == *id)
        });
        let current = self.current_notebook_id.clone();
        self.ensure_tab_open(&current);
    }

    /// Liefert den nächsten (`delta = 1`) oder vorherigen (`delta = -1`) Tab relativ zum aktuellen.
    pub fn adjacent_tab(&self, delta: isize) -> Option<String> {
        if self.open_tabs.len() < 2 {
            return None;
        }
        let pos = self
            .open_tabs
            .iter()
            .position(|t| t == &self.current_notebook_id)
            .unwrap_or(0) as isize;
        let len = self.open_tabs.len() as isize;
        let next = (pos + delta).rem_euclid(len) as usize;
        let id = &self.open_tabs[next];
        (id != &self.current_notebook_id).then(|| id.clone())
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
        // Offenes Kontextmenü: ein Tippen daneben schließt es nur
        if self.context_menu.is_some() || self.is_point_inside_toolbar(px, py) {
            return true;
        }
        if let Some(tab_rect) = self.tab_bar_rect {
            if tab_rect.contains(egui::pos2(px, py)) {
                return true;
            }
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
        if self.is_search_open {
            if let Some(search_rect) = self.search_modal_rect {
                if search_rect.contains(egui::pos2(px, py)) {
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
/// Richtet den egui-Kontext ein. Strg+Plus/Minus/0 zoomen das Blatt; egui darf damit nicht
/// zusätzlich die Oberfläche vergrößern, sonst passen Textfeld, Auswahl und Lineal nicht mehr
/// zum Blatt (Issue #34).
pub fn configure_context(ctx: &egui::Context) {
    ctx.set_visuals(egui::Visuals::dark());
    ctx.options_mut(|o| o.zoom_with_keyboard = false);
}
/// Keys that still work in view-only mode: moving through the notebook, zoom, saving,
/// exporting and quitting. Everything that would change the notebook is ignored (Issue #35).
pub fn key_allowed_in_view_mode(key: &winit::keyboard::Key, ctrl: bool) -> bool {
    use winit::keyboard::{Key, NamedKey};
    match key {
        Key::Named(named) => {
            matches!(
                named,
                NamedKey::Escape
                    | NamedKey::Space
                    | NamedKey::PageUp
                    | NamedKey::PageDown
                    | NamedKey::Home
                    | NamedKey::End
            ) || (ctrl && named == &NamedKey::Tab)
        }
        Key::Character(c) => {
            ctrl && ["0", "+", "=", "-", "_", "s", "e", "q", "f"]
                .iter()
                .any(|k| c.eq_ignore_ascii_case(k))
        }
        _ => false,
    }
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
    state.screen_rect = ctx.viewport_rect();
    overlays::render_canvas_overlays(ctx, state, notebook, &mut actions);

    let anchors =
        sidebar::render_sidebar(ctx, state, notebook, autosave_status, updater, &mut actions);
    state.toolbar_rect = Some(anchors.toolbar_rect);
    state.tab_bar_rect = tabs::render_tab_bar(ctx, state, &anchors, &mut actions);

    popovers::render_popovers(
        ctx,
        state,
        notebook,
        autosave_status,
        updater,
        &anchors,
        &mut actions,
    );
    dialogs::render_dialogs(ctx, state, notebook, updater, &mut actions);

    actions
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{Page, PAGE_HEIGHT, PAGE_WIDTH};

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

        ctx.begin_pass(egui::RawInput::default());
        let actions = render_ui(&ctx, &mut state, &notebook, AutosaveStatus::Saved, &updater);
        let mut output = ctx.end_pass();
        output.textures_delta.clear();

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
    fn test_notebook_dialog_fits_beside_the_toolbar_on_a_phone() {
        // Phone in portrait (Issue #50): the dialog was wider than the screen
        let mut state = UiState::new();
        state.is_document_menu_open = true;
        state.available_notebooks = vec![NotebookSummary {
            id: "nb-1".to_string(),
            title: "Default notebook".to_string(),
            page_count: 1,
            modified_at: 100,
            status: crate::storage::NotebookStatus::Normal,
            error: None,
        }];
        state.current_notebook_id = "nb-1".to_string();
        let screen = egui::vec2(412.0, 900.0);
        let ctx = render_frames_sized(&mut state, &Notebook::new(), 8, screen);
        let dialog = ctx
            .memory(|m| m.area_rect(egui::Id::unique(t!("Dokumenten-Manager"))))
            .unwrap();
        let toolbar = state.toolbar_rect.unwrap();
        assert!(dialog.left() >= toolbar.right(), "{dialog:?} {toolbar:?}");
        assert!(dialog.right() <= screen.x, "{dialog:?}");
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
        ctx.run_pass(egui::RawInput::default(), |ctx| {
            let _ = super::render_ui(ctx, &mut state, &notebook, AutosaveStatus::Saved, &updater);
        })
        .textures_delta
        .clear();

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
        ctx.run_pass(egui::RawInput::default(), |ctx| {
            let _ = super::render_ui(ctx, &mut state, &notebook, AutosaveStatus::Saved, &updater);
        })
        .textures_delta
        .clear();

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
            ctx.begin_pass(egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, screen)),
                ..Default::default()
            });
            let _ = render_ui(&ctx, state, notebook, AutosaveStatus::Saved, &updater);
            ctx.end_pass().textures_delta.clear();
        }
        ctx
    }

    #[test]
    fn test_page_scrollbar_jumps_through_the_notebook() {
        // Layout: thumb at the top at the start, at the bottom at the end, beside the
        // toolbar and never on the opposite edge where the writing hand rests (issue #58)
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 850.0));
        let (track, top) = scrollbar_layout(
            screen,
            SidebarSide::Left,
            64.0,
            (0.0, 80_000.0),
            0.0,
            1000.0,
        );
        assert_eq!(track.left(), 64.0);
        assert_eq!(top.top(), track.top());
        assert!(top.height() >= 48.0);
        let (_, end) = scrollbar_layout(
            screen,
            SidebarSide::Left,
            64.0,
            (0.0, 80_000.0),
            80_000.0,
            1000.0,
        );
        assert_eq!(end.bottom(), track.bottom());
        let (right, _) = scrollbar_layout(screen, SidebarSide::Right, 64.0, (0.0, 1.0), 0.0, 1.0);
        assert_eq!(right.right(), screen.right() - 64.0);
        assert!(right.left() > screen.center().x);
        // Without a toolbar the bar sits on the toolbar's edge; it stays on screen even
        // when the toolbar is wider than the window
        let (edge, _) = scrollbar_layout(screen, SidebarSide::Right, 0.0, (0.0, 1.0), 0.0, 1.0);
        assert_eq!(edge.right(), screen.right());
        let (narrow, _) = scrollbar_layout(screen, SidebarSide::Left, 5000.0, (0.0, 1.0), 0.0, 1.0);
        assert_eq!(narrow.right(), screen.right());

        // Notizbuch mit 80 Seiten, durchgehend, Ansicht auf Seite 1
        let mut notebook = Notebook::new();
        for _ in 1..80 {
            notebook.add_page(PageBackground::Blank);
        }
        let mut state = UiState::new();
        state.continuous_scroll = true;
        state.page_view = Some(PageView {
            origin: egui::pos2(300.0, 0.0),
            scale: 0.25,
        });
        let ctx = render_frames(&mut state, &notebook, 2);
        let updater = crate::updater::UpdaterController::new();
        let x = state.sidebar_reserved_width() + 14.0;
        assert!(x > 14.0, "the toolbar has been laid out");
        let mut actions = Vec::new();
        // Griff greifen und bis ans untere Ende ziehen
        for (pos, pressed) in [
            (egui::pos2(x, 30.0), Some(true)),
            (egui::pos2(x, 400.0), None),
            (egui::pos2(x, 845.0), None),
            (egui::pos2(x, 845.0), Some(false)),
        ] {
            let mut events = vec![egui::Event::PointerMoved(pos)];
            if let Some(pressed) = pressed {
                events.push(egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::default(),
                });
            }
            ctx.begin_pass(egui::RawInput {
                screen_rect: Some(screen),
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
            ctx.end_pass().textures_delta.clear();
        }
        let targets: Vec<f32> = actions
            .iter()
            .filter_map(|a| match a {
                UiAction::ScrollTo(y) => Some(*y),
                _ => None,
            })
            .collect();
        let (_, max) = notebook.page_stack().bounds();
        assert!(targets.len() >= 2, "{actions:?}");
        assert!(targets.windows(2).all(|w| w[1] >= w[0]));
        assert!((targets.last().unwrap() - max.y).abs() < 1.0, "{targets:?}");
        assert!(state.scrollbar_grab.is_none());

        // Feste Seiten: keine Leiste, Tippen an den Rand scrollt nicht
        state.continuous_scroll = false;
        let ctx = render_frames(&mut state, &notebook, 2);
        let actions = click_at(&ctx, &mut state, &notebook, egui::pos2(x, 400.0));
        assert!(!actions.iter().any(|a| matches!(a, UiAction::ScrollTo(_))));
    }

    #[test]
    fn test_ctrl_plus_does_not_zoom_the_ui() {
        let ctx = egui::Context::default();
        configure_context(&ctx);
        let ctrl = egui::Modifiers {
            ctrl: true,
            command: true,
            ..Default::default()
        };
        for _ in 0..2 {
            ctx.begin_pass(egui::RawInput {
                events: vec![egui::Event::Key {
                    key: egui::Key::Plus,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: ctrl,
                }],
                ..Default::default()
            });
            ctx.end_pass().textures_delta.clear();
        }
        assert_eq!(ctx.zoom_factor(), 1.0);
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
            ctx.begin_pass(egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1280.0, 850.0),
                )),
                events,
                ..Default::default()
            });
            let _ = render_ui(&ctx, state, &notebook, AutosaveStatus::Saved, &updater);
            ctx.end_pass().textures_delta.clear();
        };
        frame(&mut state, Vec::new());
        assert!(ctx.egui_wants_keyboard_input(), "Textfeld hat den Fokus");
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
                    ctx.begin_pass(egui::RawInput {
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
                    ctx.end_pass().textures_delta.clear();
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
                ctx.begin_pass(egui::RawInput {
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
                ctx.end_pass().textures_delta.clear();
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

    /// Taps down the middle of the toolbar and collects every action it produces.
    fn toolbar_scan(view_mode: bool, notebook: &Notebook) -> Vec<UiAction> {
        let mut probe = UiState::new();
        probe.view_mode = view_mode;
        render_frames(&mut probe, notebook, 10);
        let bar = probe.toolbar_rect.unwrap();
        let updater = crate::updater::UpdaterController::new();
        let screen = Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1280.0, 850.0),
        ));
        let mut actions = Vec::new();
        let mut y = bar.min.y + 4.0;
        while y < bar.max.y {
            let mut state = UiState::new();
            state.view_mode = view_mode;
            let ctx = render_frames(&mut state, notebook, 10);
            let pos = egui::pos2(bar.center().x, y);
            for pressed in [true, false] {
                let mut events = vec![egui::Event::PointerMoved(pos)];
                events.push(egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::default(),
                });
                ctx.begin_pass(egui::RawInput {
                    screen_rect: screen,
                    events,
                    ..Default::default()
                });
                actions.extend(render_ui(
                    &ctx,
                    &mut state,
                    notebook,
                    AutosaveStatus::Saved,
                    &updater,
                ));
                ctx.end_pass().textures_delta.clear();
            }
            y += 6.0;
        }
        actions
    }

    #[test]
    fn test_view_mode_hides_editing_buttons() {
        let notebook = Notebook::new();
        let editing = toolbar_scan(false, &notebook);
        assert!(
            editing.contains(&UiAction::SetViewMode(true)),
            "{editing:?}"
        );
        assert!(editing.iter().any(|a| matches!(a, UiAction::SetTool(_))));
        assert!(editing.contains(&UiAction::AddPage));

        let viewing = toolbar_scan(true, &notebook);
        assert!(
            viewing.contains(&UiAction::SetViewMode(false)),
            "{viewing:?}"
        );
        assert!(
            !viewing.iter().any(|a| matches!(
                a,
                UiAction::SetTool(_) | UiAction::AddPage | UiAction::Undo | UiAction::Redo
            )),
            "{viewing:?}"
        );
    }

    #[test]
    fn test_toolbar_regrows_after_leaving_view_mode() {
        let ctx = egui::Context::default();
        let mut state = UiState::new();
        let notebook = Notebook::new();
        let updater = crate::updater::UpdaterController::new();
        let screen = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1280.0, 900.0));

        let step = |state: &mut UiState| -> f32 {
            let mut input = egui::RawInput {
                screen_rect: Some(screen),
                ..Default::default()
            };
            input.time = Some(0.0);
            ctx.begin_pass(input);
            let _ = render_ui(&ctx, state, &notebook, AutosaveStatus::Saved, &updater);
            let mut output = ctx.end_pass();
            output.textures_delta.clear();
            state.toolbar_rect.expect("toolbar rect").height()
        };

        // 1. Schreibmodus: volle Höhe
        state.view_mode = false;
        let _ = step(&mut state);
        let height_before = step(&mut state);

        // 2. View-Modus: Leiste schrumpft
        state.view_mode = true;
        let _ = step(&mut state);
        let height_view = step(&mut state);
        assert!(
            height_view < height_before * 0.6,
            "im View-Modus sollte die Leiste deutlich kürzer sein ({height_view} vs {height_before})"
        );

        // 3. Zurück in den Schreibmodus: Leiste muss wieder die volle Höhe erreichen
        state.view_mode = false;
        let _ = step(&mut state);
        let height_after = step(&mut state);
        assert!(
            (height_after - height_before).abs() < 2.0,
            "Werkzeugleiste blieb nach View-Modus kurz: vorher {height_before}, danach {height_after}"
        );
    }

    #[test]
    fn test_eraser_size_stays_in_the_slider_range() {
        let mut state = UiState::new();
        assert_eq!(state.eraser_radius(), 30.0);
        // Values from the settings file stay within the slider range
        state.set_eraser_radius_mm(7.0);
        assert_eq!(state.eraser_radius_mm, 7.0);
        state.set_eraser_radius_mm(100.0);
        assert_eq!(state.eraser_radius_mm, ERASER_DIAMETER_RANGE_MM.1 / 2.0);
        state.set_eraser_radius_mm(0.0);
        assert_eq!(state.eraser_radius_mm, ERASER_DIAMETER_RANGE_MM.0 / 2.0);
        state.set_eraser_radius_mm(f32::NAN);
        assert_eq!(state.eraser_radius_mm, 3.0);
        assert_eq!(format_mm(3.0), "3");
        assert_eq!(format_mm(2.5), "2.5");
    }

    #[test]
    fn test_view_mode_keeps_only_harmless_keys() {
        use winit::keyboard::{Key, NamedKey, SmolStr};
        let ch = |c: &str| Key::Character(SmolStr::new(c));
        for (key, ctrl) in [
            (Key::Named(NamedKey::PageDown), false),
            (Key::Named(NamedKey::Escape), false),
            (ch("+"), true),
            (ch("0"), true),
            (ch("S"), true),
        ] {
            assert!(key_allowed_in_view_mode(&key, ctrl), "{key:?}");
        }
        for (key, ctrl) in [
            (Key::Named(NamedKey::Delete), false),
            (Key::Named(NamedKey::Backspace), false),
            (ch("z"), true),
            (ch("v"), true),
            (ch("e"), false),
            (ch("g"), false),
            (ch("n"), false),
            (ch("l"), false),
        ] {
            assert!(!key_allowed_in_view_mode(&key, ctrl), "{key:?}");
        }
    }

    #[test]
    fn test_aspect_lock_button_only_with_selection() {
        // Den Knopf in der Leiste des Auswahlwerkzeugs suchen: ohne Auswahl gesperrt
        let notebook = Notebook::new();
        let toggles = |lock: Option<bool>| {
            let mut probe = UiState::new();
            probe.active_tool = ActiveTool::Select;
            render_frames(&mut probe, &notebook, 10);
            let bar = probe.toolbar_rect.unwrap();
            let mut found = 0;
            let mut y = bar.min.y + 4.0;
            while y < bar.max.y {
                let mut state = UiState::new();
                state.active_tool = ActiveTool::Select;
                state.selection_aspect_lock = lock;
                let ctx = render_frames(&mut state, &notebook, 10);
                let actions = click_at(&ctx, &mut state, &notebook, egui::pos2(bar.center().x, y));
                if actions.contains(&UiAction::ToggleAspectLock) {
                    found += 1;
                }
                y += 4.0;
            }
            found
        };
        assert_eq!(toggles(None), 0);
        assert!(toggles(Some(true)) > 0);
        assert!(toggles(Some(false)) > 0);
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
            UiState::sidebar_button_size(ActiveTool::Select, 1100.0, true),
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
        ("app.rs", include_str!("app.rs")),
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
            ctx.begin_pass(input);
            let _ = render_ui(&ctx, state, &notebook, AutosaveStatus::Saved, &updater);
            ctx.end_pass().textures_delta.clear();
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
    fn test_custom_colors_remember_changes() {
        let mut state = UiState::new();
        assert!(!state.sync_tool_colors(), "Ausgangszustand ist gespeichert");

        // Stift 2 wählen und umfärben: der Stift merkt sich die Farbe
        state.select_pen(1);
        state.pen_color = [0.5, 0.1, 0.6, 1.0];
        assert!(state.sync_tool_colors());
        assert_eq!(state.pen_presets[1].color, [0.5, 0.1, 0.6, 1.0]);
        state.select_pen(0);
        state.select_pen(1);
        assert_eq!(state.pen_color, [0.5, 0.1, 0.6, 1.0]);

        // Textmarker-Farbe 3 anpassen
        state.select_highlighter_color(2);
        state.highlighter_color = [0.9, 0.5, 0.1, 0.35];
        assert!(state.sync_tool_colors());
        assert_eq!(state.highlighter_colors[2], [0.9, 0.5, 0.1, 0.35]);
        assert_eq!(state.highlighter_colors[0], DEFAULT_HIGHLIGHTER_COLORS[0]);
        assert!(!state.sync_tool_colors());

        // Textfarbe gilt auch für das gerade bearbeitete Feld
        state.text_edit = Some(TextEditing::new_at(
            1,
            glam::Vec2::new(100.0, 100.0),
            PAGE_WIDTH,
            50.0,
            TextFont::Sans,
            [0.0, 0.0, 0.0, 1.0],
        ));
        state.set_text_color([0.2, 0.6, 0.3, 1.0]);
        assert_eq!(
            state.text_edit.as_ref().unwrap().color,
            [0.2, 0.6, 0.3, 1.0]
        );
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
        // Eingerasteter Textmarker (Issue #32)
        assert_eq!(
            ActiveTool::Highlighter.stroke_tool(true),
            StrokeTool::Highlighter
        );
    }

    #[test]
    fn test_all_ui_texts_are_renderable() {
        // Zeichen ohne Glyphe in den eingebauten egui-Schriften erscheinen als leere Kästchen
        let ctx = egui::Context::default();
        ctx.begin_pass(egui::RawInput::default());
        let font = egui::FontId::proportional(14.0);
        let mut missing = std::collections::BTreeSet::new();
        for (name, src) in UI_SOURCES {
            for c in string_literal_chars(src) {
                if !c.is_ascii() && !ctx.fonts_mut(|f| f.has_glyphs(&font, &c.to_string())) {
                    missing.insert(format!("{name}: {c} (U+{:04X})", c as u32));
                }
            }
        }
        ctx.end_pass().textures_delta.clear();
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
            ctx.begin_pass(egui::RawInput {
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
            ctx.end_pass().textures_delta.clear();
        }
        actions
    }

    #[test]
    fn test_stylus_buttons_in_menu() {
        use crate::settings::{StylusAction, StylusButton};
        let notebook = Notebook::new();
        let mut state = UiState::new();
        state.is_more_menu_open = true;
        state.more_menu_tab = MoreMenuTab::Settings;
        // Ohne gelernte Tasten: nur der Hinweis, keine Aktion
        let ctx = egui::Context::default();
        let updater = crate::updater::UpdaterController::new();
        let mut first = ctx.run_pass(egui::RawInput::default(), |ctx| {
            let _ = render_ui(ctx, &mut state, &notebook, AutosaveStatus::Saved, &updater);
        });
        first.textures_delta.clear();
        assert!(!first.shapes.is_empty());

        // Mit zwei Tasten: beide Zeilen, die gedrückte hervorgehoben, ohne Fehler
        state.stylus_buttons = vec![
            StylusButton {
                id: 1,
                action: StylusAction::Erase,
            },
            StylusButton {
                id: 2,
                action: StylusAction::Select,
            },
        ];
        state.pressed_stylus_button = Some(1);
        let mut output = ctx.run_pass(egui::RawInput::default(), |ctx| {
            let actions = render_ui(ctx, &mut state, &notebook, AutosaveStatus::Saved, &updater);
            assert!(!actions.contains(&UiAction::SaveStylusButtons));
        });
        output.textures_delta.clear();
        assert!(!output.shapes.is_empty());
        assert_eq!(state.stylus_buttons.len(), 2);
    }

    #[test]
    fn test_more_menu_tabs_render_cleanly() {
        let notebook = Notebook::new();
        for tab in [MoreMenuTab::Page, MoreMenuTab::Paper, MoreMenuTab::Settings] {
            let mut state = UiState::new();
            state.is_more_menu_open = true;
            state.more_menu_tab = tab;
            render_frames(&mut state, &notebook, 5);
            let pop = state.popover_rect.expect("☰-Menü muss offen sein");
            assert!(
                pop.min.y >= 0.0 && pop.max.y <= 850.0,
                "Tab {tab:?} sprengt das Fenster: {pop:?}"
            );
            assert!(pop.width() >= 280.0 && pop.width() <= 340.0, "{pop:?}");
        }
    }

    #[test]
    fn test_multi_page_background_layer_does_not_block_canvas_scroll() {
        // Regression für Issue #64: Bei ≥ 2 Seiten zeichnet `page_separators` auf Order::Background.
        // In egui 0.37 meldet `egui_wants_pointer_input()` dann über dem gesamten Blatt `true`,
        // weil `root_ui_available_rect` bei `begin_pass` fehlt. `egui_is_using_pointer()` und
        // `is_screen_point_in_ui()` dürfen das Blatt nicht blockieren.
        let ctx = egui::Context::default();
        let mut state = UiState::new();
        state.continuous_scroll = true;
        state.page_view = Some(PageView {
            origin: egui::pos2(100.0, 40.0),
            scale: 0.25,
        });
        let mut notebook = Notebook::new();
        notebook.add_page(PageBackground::Grid5mm);
        let updater = crate::updater::UpdaterController::new();
        let screen = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1280.0, 900.0));
        let canvas_pt = egui::pos2(600.0, 400.0);

        for _ in 0..2 {
            let input = egui::RawInput {
                screen_rect: Some(screen),
                events: vec![egui::Event::PointerMoved(canvas_pt)],
                ..Default::default()
            };
            ctx.begin_pass(input);
            let _ = render_ui(&ctx, &mut state, &notebook, AutosaveStatus::Saved, &updater);
            let mut out = ctx.end_pass();
            out.textures_delta.clear();
        }

        assert!(
            !ctx.egui_is_using_pointer(),
            "egui darf beim Schweben über dem Blatt nicht als aktiv gelten"
        );
        assert!(
            !is_screen_point_in_ui(&ctx, &state, (canvas_pt.x, canvas_pt.y), 1.0),
            "Blattpunkt darf bei mehrseitigem Notizbuch nicht als UI gelten"
        );
    }

    #[test]
    fn test_text_edit_overlay_grows_width_when_pasting() {
        // Regression für Issue #62: Ein neu geöffnetes (leeres) Textfeld startet schmal (~3 Zeichen).
        // Nach dem Einfügen eines längeren Textes muss die egui::Area sofort mitwachsen.
        let ctx = egui::Context::default();
        let mut state = UiState::new();
        state.page_view = Some(PageView {
            origin: egui::pos2(100.0, 40.0),
            scale: 0.5,
        });
        let notebook = Notebook::new();
        let updater = crate::updater::UpdaterController::new();
        let screen = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1280.0, 900.0));

        state.text_edit = Some(TextEditing::new_at(
            notebook.current_page().id,
            glam::Vec2::new(200.0, 200.0),
            crate::document::PAGE_WIDTH,
            14.0 * crate::text::UNITS_PER_PT,
            TextFont::Sans,
            [0.0, 0.0, 0.0, 1.0],
        ));

        let step = |state: &mut UiState| -> f32 {
            let input = egui::RawInput {
                screen_rect: Some(screen),
                ..Default::default()
            };
            ctx.begin_pass(input);
            let _ = render_ui(&ctx, state, &notebook, AutosaveStatus::Saved, &updater);
            let mut out = ctx.end_pass();
            out.textures_delta.clear();
            ctx.memory(|m| {
                m.area_rect(egui::Id::unique("text_edit_area"))
                    .expect("text_edit_area rect")
                    .width()
            })
        };

        let width_empty = step(&mut state);
        if let Some(edit) = state.text_edit.as_mut() {
            edit.text.content =
                "Dies ist ein längerer eingefügter Satz, der nicht schmal umbrechen darf."
                    .to_string();
        }
        let _ = step(&mut state);
        let width_pasted = step(&mut state);
        assert!(
            width_pasted > width_empty * 3.0,
            "Textfeld blieb nach Einfügen schmal: leer {width_empty}, danach {width_pasted}"
        );
    }

    #[test]
    fn test_long_press_detection() {
        let t0 = Instant::now();
        let ms = |n| t0 + std::time::Duration::from_millis(n);
        let mut press = LongPress::default();
        assert_eq!(press.fire(ms(1000)), None);

        // Stillhalten (kleines Zittern erlaubt): nach 500 ms genau einmal
        press.start(1, (100.0, 100.0), t0);
        press.moved(1, (106.0, 103.0), 1.0);
        assert_eq!(press.deadline(), Some(ms(500)));
        assert_eq!(press.fire(ms(499)), None);
        assert_eq!(press.fire(ms(500)), Some((100.0, 100.0)));
        assert_eq!(press.fire(ms(900)), None);

        // Verschieben bricht ab; bei 2× Skalierung zählt der Abstand in logischen Punkten
        press.start(1, (100.0, 100.0), t0);
        press.moved(1, (115.0, 100.0), 2.0);
        assert!(press.deadline().is_some());
        press.moved(1, (125.0, 100.0), 2.0);
        assert_eq!(press.fire(ms(600)), None);

        // Abheben oder ein anderer Finger brechen ab
        press.start(1, (100.0, 100.0), t0);
        press.cancel();
        assert_eq!(press.fire(ms(600)), None);
        press.start(1, (100.0, 100.0), t0);
        press.moved(2, (100.0, 100.0), 1.0);
        assert_eq!(press.fire(ms(600)), None);
    }

    #[test]
    fn test_context_menu_pastes_at_pressed_point() {
        let notebook = Notebook::new();
        let mut state = UiState::new();
        let at = glam::Vec2::new(800.0, 1200.0);
        state.context_menu = Some(ContextMenu {
            screen: egui::pos2(600.0, 400.0),
            page: at,
        });
        let ctx = render_frames(&mut state, &notebook, 3);
        // Solange es offen ist, gehört jeder Punkt der Oberfläche (Tippen daneben schließt)
        assert!(state.is_point_in_ui(1000.0, 700.0));

        // „Einfügen“ ist der erste Eintrag
        let actions = click_at(&ctx, &mut state, &notebook, egui::pos2(700.0, 425.0));
        assert!(actions.contains(&UiAction::PasteAt(at)), "{actions:?}");
        assert!(state.context_menu.is_none());

        // Tippen daneben schließt das Menü ohne Aktion
        state.context_menu = Some(ContextMenu {
            screen: egui::pos2(600.0, 400.0),
            page: at,
        });
        let ctx = render_frames(&mut state, &notebook, 3);
        let actions = click_at(&ctx, &mut state, &notebook, egui::pos2(1000.0, 700.0));
        assert!(state.context_menu.is_none());
        assert!(!actions.iter().any(|a| matches!(a, UiAction::PasteAt(_))));
        assert!(!state.is_point_in_ui(1000.0, 700.0));
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
            page_thumbnail_shapes(&empty, rect, PaperTheme::White, true, None).len(),
            1
        );

        // Striche liegen innerhalb der Kachel
        let page = page_with_strokes(3, 20);
        let shapes = page_thumbnail_shapes(&page, rect, PaperTheme::White, true, None);
        assert_eq!(shapes.len(), 1 + 3);
        let pts = shape_points(&shapes);
        assert!(!pts.is_empty());
        assert!(
            pts.iter().all(|p| rect.contains(*p)),
            "Punkte außerhalb der Kachel"
        );

        // Sehr volle Seite wird auf das Punktbudget ausgedünnt (+ Endpunkte je Strich)
        let full = page_with_strokes(100, 1000);
        let pts = shape_points(&page_thumbnail_shapes(
            &full,
            rect,
            PaperTheme::White,
            true,
            None,
        ));
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
        let shapes = page_thumbnail_shapes(&with_image, rect, PaperTheme::White, true, None);
        assert_eq!(shapes.len(), 3);
        assert!(
            matches!(shapes[1], egui::Shape::Path(ref p) if p.closed && p.fill != egui::Color32::TRANSPARENT)
        );
        assert!(matches!(shapes[2], egui::Shape::Path(ref p) if !p.closed));

        // PDF page: the rendered page fills the PDF area, otherwise a gray area (Issue #40)
        let mut with_pdf = Page::new(2, PageBackground::Blank);
        with_pdf.pdf_background = Some(crate::document::PdfBackground {
            asset: "a.pdf".into(),
            page: 0,
            width_pt: 595.0,
            height_pt: 842.0,
        });
        let gray = page_thumbnail_shapes(&with_pdf, rect, PaperTheme::White, true, None);
        assert!(matches!(gray[1], egui::Shape::Path(ref p) if p.closed));
        let texture = egui::TextureId::User(7);
        let shapes = page_thumbnail_shapes(&with_pdf, rect, PaperTheme::White, true, Some(texture));
        assert!(matches!(shapes[1], egui::Shape::Mesh(ref m) if m.texture_id == texture));
        assert!(rect.contains_rect(shapes[1].visual_bounding_rect()));
    }

    #[test]
    fn test_page_tiles_follow_the_page_shape() {
        // Slides only: flat tiles without an empty strip below the page (Issue #40)
        let mut notebook = Notebook::new();
        notebook.pages[0].size = glam::Vec2::new(2970.0, 1671.0);
        let flat = page_tile_size(&notebook);
        assert!((flat.y - PAGE_TILE_LABEL - flat.x * 1671.0 / 2970.0).abs() < 0.01);
        let thumb =
            egui::Rect::from_min_size(egui::Pos2::ZERO, flat - egui::vec2(0.0, PAGE_TILE_LABEL));
        let sheet = thumbnail_sheet(notebook.pages[0].size, thumb);
        assert!((sheet.height() - thumb.height()).abs() < 0.01);

        // A portrait page among them: tiles as high as A4, slides at the top
        let index = notebook.add_page(PageBackground::Blank);
        notebook.pages[index].size = crate::document::A4_SIZE;
        let tall = page_tile_size(&notebook);
        assert!((tall.y - PAGE_TILE_LABEL - tall.x * PAGE_HEIGHT / PAGE_WIDTH).abs() < 0.01);
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
            ctx.begin_pass(egui::RawInput::default());
            let actions = render_ui(&ctx, &mut state, &notebook, AutosaveStatus::Saved, &updater);
            ctx.end_pass().textures_delta.clear();
            let requested = actions.iter().any(|a| matches!(a, UiAction::LoadAllPages));
            assert_eq!(requested, expected, "ungeladene Seite: {unloaded}");
        }
    }

    #[test]
    fn test_notebook_tabs_open_close_prune_and_cycle() {
        let mut state = UiState::new();
        assert_eq!(state.open_tabs, vec!["default".to_string()]);
        // Einzelner Tab wird nicht geschlossen
        assert_eq!(state.close_tab("default"), None);
        assert_eq!(state.open_tabs.len(), 1);

        state.ensure_tab_open("nb_a");
        state.ensure_tab_open("nb_b");
        state.ensure_tab_open("nb_a"); // kein Duplikat
        assert_eq!(state.open_tabs, vec!["default", "nb_a", "nb_b"]);

        assert_eq!(state.adjacent_tab(1).as_deref(), Some("nb_a"));
        assert_eq!(state.adjacent_tab(-1).as_deref(), Some("nb_b"));

        // Nicht-aktiven Tab schließen liefert keinen Wechsel
        assert_eq!(state.close_tab("nb_a"), None);
        assert_eq!(state.open_tabs, vec!["default", "nb_b"]);

        // Aktiven Tab schließen liefert Nachbar-Tab
        assert_eq!(state.close_tab("default").as_deref(), Some("nb_b"));
        assert_eq!(state.open_tabs, vec!["nb_b"]);

        // Maximalanzahl MAX_OPEN_TABS einhalten
        state.current_notebook_id = "nb_b".to_string();
        for i in 0..12 {
            state.ensure_tab_open(&format!("nb_{i}"));
        }
        assert_eq!(state.open_tabs.len(), crate::settings::MAX_OPEN_TABS);
        assert!(state.open_tabs.contains(&"nb_b".to_string()));
    }

    #[test]
    fn test_tab_bar_only_shown_with_two_or_more_tabs() {
        let notebook = Notebook::new();
        let mut state = UiState::new();
        let _ = render_frames(&mut state, &notebook, 2);
        assert!(state.tab_bar_rect.is_none());

        state.ensure_tab_open("second");
        let _ = render_frames(&mut state, &notebook, 2);
        let rect = state
            .tab_bar_rect
            .expect("Tab-Leiste muss ab 2 offenen Tabs sichtbar sein");
        assert!(state.is_point_in_ui(rect.center().x, rect.center().y));
    }
}
