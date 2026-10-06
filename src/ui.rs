//! Immediate-Mode UI mit `egui` für ink-notes.
//!
//! Stellt eine schmale, senkrechte Werkzeugleiste bereit, die seitlich angedockt ist
//! (links oder rechts, umschaltbar):
//! - Symbol-Knöpfe mit 40 px Kantenlänge (stift- und fingertauglich), Tooltips mit Kürzeln
//! - Werkzeuge, Farben des aktiven Werkzeugs, Auswahl-Aktionen, Undo/Redo, Seitennavigation
//! - Selten genutzte Funktionen (PDF, Hintergrund, Zoom, Touch-Modus, Leistenseite,
//!   Updates, Speicherstatus) im ☰-Menü
//! - Popover (Stift/Marker/Menü) öffnen sich zur Blattseite hin
//! - Hit-Testing für Event-Transparenz außerhalb der Leiste und der Popover

use std::time::Instant;

use crate::autosave::AutosaveStatus;
use crate::document::{Notebook, PageBackground};
use crate::settings::SidebarSide;
use crate::storage::NotebookSummary;

/// Aktives Werkzeug für das Zeichnen oder Bearbeiten.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveTool {
    Pen,
    Highlighter,
    Eraser,
    Shape,
    Select,
}

impl ActiveTool {
    pub fn name(&self) -> &'static str {
        match self {
            ActiveTool::Pen => "Stift (Pen)",
            ActiveTool::Highlighter => "Textmarker (Highlighter)",
            ActiveTool::Eraser => "Radierer (Eraser)",
            ActiveTool::Shape => "Formen (Shape)",
            ActiveTool::Select => "Auswahl / Lasso (Select)",
        }
    }

    pub fn is_drawing(&self) -> bool {
        matches!(self, ActiveTool::Pen | ActiveTool::Highlighter)
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
    GoToPage(usize),
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
    /// Fehler beim Flushen vor dem Beenden
    pub shutdown_error: Option<crate::autosave::AutosaveError>,
    /// Fensterseite der Werkzeugleiste
    pub sidebar_side: SidebarSide,
    /// Öffnungszustand des ☰-Menüs
    pub is_more_menu_open: bool,
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
            shutdown_error: None,
            sidebar_side: SidebarSide::Left,
            is_more_menu_open: false,
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

    /// Länge der längsten Farbpalette; so viel Platz hält die Leiste immer frei.
    pub const MAX_PALETTE_LEN: usize = 5;

    /// Durchmesser der Farbtupfer passend zur Knopfgröße.
    pub fn sidebar_swatch_size(button_size: f32) -> f32 {
        button_size - 10.0
    }

    /// Größte Knopfgröße (40 px, schrittweise bis 28 px), bei der die Leiste mit allen Elementen
    /// des aktiven Werkzeugs in `max_height` passt.
    pub fn sidebar_button_size(tool: ActiveTool, max_height: f32) -> f32 {
        let select = tool == ActiveTool::Select;
        // Notizbücher, 5 Werkzeuge, Undo/Redo, 3× Seiten, Menü (+ 4 Auswahl-Aktionen)
        let buttons = 12 + if select { 4 } else { 0 };
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

    /// Gibt die aktuelle Farbe des gewählten Werkzeugs zurück.
    pub fn current_color(&self) -> [f32; 4] {
        match self.active_tool {
            ActiveTool::Pen | ActiveTool::Shape | ActiveTool::Select => self.pen_color,
            ActiveTool::Highlighter => self.highlighter_color,
            ActiveTool::Eraser => [0.0, 0.0, 0.0, 0.0],
        }
    }

    /// Gibt die aktuelle Strichbreite in mm zurück.
    pub fn current_stroke_width_mm(&self) -> f32 {
        match self.active_tool {
            ActiveTool::Pen | ActiveTool::Shape | ActiveTool::Select => self.pen_stroke_width_mm,
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

/// Stilvoller einfacher Werkzeug-Button (für den Radierer).
fn tool_pill_button(
    ui: &mut egui::Ui,
    is_active: bool,
    text: &str,
    tooltip: &str,
) -> egui::Response {
    let (bg, text_color, stroke) = if is_active {
        (
            egui::Color32::from_rgba_premultiplied(45, 80, 145, 230),
            egui::Color32::WHITE,
            egui::Stroke::new(
                1.0_f32,
                egui::Color32::from_rgba_premultiplied(110, 165, 255, 190),
            ),
        )
    } else {
        (
            egui::Color32::from_rgba_premultiplied(255, 255, 255, 8),
            egui::Color32::from_gray(215),
            egui::Stroke::new(
                1.0_f32,
                egui::Color32::from_rgba_premultiplied(255, 255, 255, 15),
            ),
        )
    };

    let button = egui::Button::new(
        egui::RichText::new(text)
            .color(text_color)
            .size(13.0)
            .strong(),
    )
    .fill(bg)
    .stroke(stroke)
    .rounding(egui::Rounding::same(8.0))
    .min_size(egui::vec2(0.0, 28.0));

    ui.add(button).on_hover_text(tooltip)
}

/// Dicken-Preset-Button mit visueller Linien-Vorschau in Originalfarbe und Proportion.
fn stroke_preview_button(
    ui: &mut egui::Ui,
    is_active: bool,
    stroke_width_mm: f32,
    label_text: &str,
    color: [f32; 4],
) -> egui::Response {
    let size = egui::vec2(66.0, 34.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());

    if ui.is_rect_visible(rect) {
        let (bg, border) = if is_active {
            (
                egui::Color32::from_rgba_premultiplied(50, 85, 150, 220),
                egui::Stroke::new(
                    1.0_f32,
                    egui::Color32::from_rgba_premultiplied(120, 175, 255, 200),
                ),
            )
        } else if response.hovered() {
            (
                egui::Color32::from_rgba_premultiplied(255, 255, 255, 16),
                egui::Stroke::new(
                    1.0_f32,
                    egui::Color32::from_rgba_premultiplied(255, 255, 255, 25),
                ),
            )
        } else {
            (
                egui::Color32::from_rgba_premultiplied(255, 255, 255, 8),
                egui::Stroke::new(
                    1.0_f32,
                    egui::Color32::from_rgba_premultiplied(255, 255, 255, 14),
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

/// Abgerundeter Aktions-Button (Undo, Redo, Navigation, PDF).
fn action_pill_button(
    ui: &mut egui::Ui,
    enabled: bool,
    text: &str,
    tooltip: &str,
) -> egui::Response {
    let text_color = if enabled {
        egui::Color32::from_gray(230)
    } else {
        egui::Color32::from_gray(105)
    };

    let (bg, stroke) = if enabled {
        (
            egui::Color32::from_rgba_premultiplied(255, 255, 255, 10),
            egui::Stroke::new(
                1.0_f32,
                egui::Color32::from_rgba_premultiplied(255, 255, 255, 22),
            ),
        )
    } else {
        (
            egui::Color32::from_rgba_premultiplied(255, 255, 255, 3),
            egui::Stroke::new(
                1.0_f32,
                egui::Color32::from_rgba_premultiplied(255, 255, 255, 8),
            ),
        )
    };

    let button = egui::Button::new(egui::RichText::new(text).color(text_color).size(12.5))
        .fill(bg)
        .stroke(stroke)
        .rounding(egui::Rounding::same(7.0))
        .min_size(egui::vec2(0.0, 26.0));

    ui.add_enabled(enabled, button).on_hover_text(tooltip)
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
            egui::Color32::from_rgba_premultiplied(255, 255, 255, 25),
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
    let mut actions = Vec::new();

    let mut pen_btn_rect = None;
    let mut high_btn_rect = None;
    let mut more_btn_rect = None;

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
    let btn = UiState::sidebar_button_size(ActiveTool::Select, max_height);

    let area_response = egui::Area::new(egui::Id::new("floating_toolbar_area"))
        .anchor(anchor, offset)
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            egui::Frame::none()
                .fill(egui::Color32::from_rgba_premultiplied(24, 24, 28, 225))
                .rounding(egui::Rounding::same(UiState::TOOLBAR_ROUNDING))
                .stroke(egui::Stroke::new(
                    1.0_f32,
                    egui::Color32::from_rgba_premultiplied(255, 255, 255, 30),
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
                                    state.is_document_menu_open = !state.is_document_menu_open;
                                    state.is_pen_menu_open = false;
                                    state.is_highlighter_menu_open = false;
                                    state.is_more_menu_open = false;
                                    if state.is_document_menu_open {
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
                                        state.is_pen_menu_open = !state.is_pen_menu_open;
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
                                        state.is_highlighter_menu_open =
                                            !state.is_highlighter_menu_open;
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
                                        "Radierer (E oder rechte Maustaste)",
                                    ),
                                    (
                                        ActiveTool::Select,
                                        SidebarIcon::Lasso,
                                        "Auswählen: verschieben, drehen, skalieren, kopieren (V)",
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

                                // --- Farben des aktiven Werkzeugs ---
                                let palette: Option<(&[ColorPreset], [f32; 4])> =
                                    match state.active_tool {
                                        ActiveTool::Pen | ActiveTool::Shape | ActiveTool::Select => {
                                            Some((&PEN_COLOR_PRESETS[..], state.pen_color))
                                        }
                                        ActiveTool::Highlighter => Some((
                                            &HIGHLIGHTER_COLOR_PRESETS[..],
                                            state.highlighter_color,
                                        )),
                                        ActiveTool::Eraser => None,
                                    };
                                // Fester Platz für die längste Palette, damit die Knöpfe darunter
                                // beim Werkzeugwechsel nicht springen
                                sidebar_divider(ui, btn);
                                let swatch = UiState::sidebar_swatch_size(btn);
                                let shown = palette.map_or(0, |(presets, _)| presets.len());
                                if let Some((presets, current)) = palette {
                                    for &(color, label) in presets {
                                        let is_sel = (current[0] - color[0]).abs() < 0.05
                                            && (current[1] - color[1]).abs() < 0.05
                                            && (current[2] - color[2]).abs() < 0.05;
                                        if color_circle_swatch(ui, color, is_sel, label, swatch)
                                            .clicked()
                                        {
                                            if state.active_tool == ActiveTool::Highlighter {
                                                state.highlighter_color = color;
                                            } else {
                                                state.pen_color = color;
                                            }
                                            actions.push(UiAction::SetColor(color));
                                        }
                                    }
                                }
                                let missing = UiState::MAX_PALETTE_LEN.saturating_sub(shown);
                                if missing > 0 {
                                    let spacing = ui.spacing().item_spacing.y;
                                    ui.add_space(missing as f32 * (swatch + spacing));
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
                                if sidebar_button(
                                    ui,
                                    btn,
                                    SidebarIcon::Glyph("⏶"),
                                    false,
                                    notebook.current_page_index > 0,
                                    None,
                                    "Vorherige Seite (Bild↑)",
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
                                if sidebar_button(
                                    ui,
                                    btn,
                                    SidebarIcon::Text(&page_text),
                                    state.show_page_jump_popup,
                                    true,
                                    None,
                                    "Seite wählen",
                                )
                                .clicked()
                                {
                                    state.show_page_jump_popup = !state.show_page_jump_popup;
                                }
                                let is_last = notebook.current_page_index + 1 >= notebook.pages.len();
                                if sidebar_button(
                                    ui,
                                    btn,
                                    SidebarIcon::Glyph("⏷"),
                                    false,
                                    true,
                                    None,
                                    if is_last {
                                        "Neue Seite anhängen (Bild↓ / N)"
                                    } else {
                                        "Nächste Seite (Bild↓ / N)"
                                    },
                                )
                                .clicked()
                                {
                                    actions.push(UiAction::NextPage);
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
                                    state.is_more_menu_open = !state.is_more_menu_open;
                                    state.is_pen_menu_open = false;
                                    state.is_highlighter_menu_open = false;
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
                    let frame = egui::Frame::none()
                        .fill(egui::Color32::from_rgba_premultiplied(24, 24, 28, 235)) // Milchglas-Look
                        .rounding(egui::Rounding::same(12.0))
                        .stroke(egui::Stroke::new(
                            1.0_f32,
                            egui::Color32::from_rgba_premultiplied(255, 255, 255, 30),
                        ))
                        .inner_margin(egui::Margin::symmetric(14.0, 12.0))
                        .shadow(egui::epaint::Shadow {
                            offset: egui::vec2(0.0, 6.0),
                            blur: 16.0,
                            spread: 0.0,
                            color: egui::Color32::from_black_alpha(120),
                        })
                        .show(ui, |ui| {
                            ui.set_width(295.0);

                            // Titelzeile mit Schließen-Button
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new("Stift-Konfiguration")
                                        .strong()
                                        .color(egui::Color32::WHITE)
                                        .size(12.5),
                                );
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if ui
                                            .add(
                                                egui::Button::new(
                                                    egui::RichText::new("🗙").size(12.0),
                                                )
                                                .frame(false),
                                            )
                                            .clicked()
                                        {
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
                                    if color_circle_swatch(ui, color, is_sel, label, 22.0).clicked()
                                    {
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
                                    if stroke_preview_button(
                                        ui,
                                        is_sel,
                                        preset,
                                        label,
                                        state.pen_color,
                                    )
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
                    let frame = egui::Frame::none()
                        .fill(egui::Color32::from_rgba_premultiplied(24, 24, 28, 235))
                        .rounding(egui::Rounding::same(12.0))
                        .stroke(egui::Stroke::new(
                            1.0_f32,
                            egui::Color32::from_rgba_premultiplied(255, 255, 255, 30),
                        ))
                        .inner_margin(egui::Margin::symmetric(14.0, 12.0))
                        .shadow(egui::epaint::Shadow {
                            offset: egui::vec2(0.0, 6.0),
                            blur: 16.0,
                            spread: 0.0,
                            color: egui::Color32::from_black_alpha(120),
                        })
                        .show(ui, |ui| {
                            ui.set_width(280.0);

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
                                        if ui
                                            .add(
                                                egui::Button::new(
                                                    egui::RichText::new("🗙").size(12.0),
                                                )
                                                .frame(false),
                                            )
                                            .clicked()
                                        {
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
                                    let is_sel = (state.highlighter_color[0] - color[0]).abs()
                                        < 0.05
                                        && (state.highlighter_color[1] - color[1]).abs() < 0.05
                                        && (state.highlighter_color[2] - color[2]).abs() < 0.05;
                                    if color_circle_swatch(ui, color, is_sel, label, 22.0).clicked()
                                    {
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
                    egui::Frame::none()
                        .fill(egui::Color32::from_rgba_premultiplied(24, 24, 28, 235))
                        .rounding(egui::Rounding::same(12.0))
                        .stroke(egui::Stroke::new(
                            1.0_f32,
                            egui::Color32::from_rgba_premultiplied(255, 255, 255, 30),
                        ))
                        .inner_margin(egui::Margin::symmetric(14.0, 12.0))
                        .shadow(egui::epaint::Shadow {
                            offset: egui::vec2(0.0, 6.0),
                            blur: 16.0,
                            spread: 0.0,
                            color: egui::Color32::from_black_alpha(120),
                        })
                        .show(ui, |ui| {
                            ui.set_width(250.0);
                            ui.spacing_mut().button_padding = egui::vec2(10.0, 7.0);
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
                                        if ui
                                            .add(egui::Button::new("🗙").frame(false))
                                            .on_hover_text("Schließen")
                                            .clicked()
                                        {
                                            state.is_more_menu_open = false;
                                        }
                                    },
                                );
                            });

                            section(ui, "DOKUMENT");
                            if ui
                                .add_sized([ui.available_width(), 30.0], egui::Button::new("📄  Als PDF exportieren (Strg+E)"))
                                .clicked()
                            {
                                actions.push(UiAction::ExportPdf);
                                state.is_more_menu_open = false;
                            }
                            if ui
                                .add_sized([ui.available_width(), 30.0], egui::Button::new("➕  Neue Seite anhängen"))
                                .clicked()
                            {
                                actions.push(UiAction::AddPage);
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
                                state.is_update_dialog_open = true;
                                state.is_more_menu_open = false;
                                state.is_document_menu_open = false;
                            }
                        })
                        .response
                        .rect
                });
            popover_rect = Some(pop_response.inner);
        }
    }

    state.popover_rect = popover_rect;

    // Automatisches Schließen der Popover bei Klick außerhalb von Leiste und Popover
    if (state.is_pen_menu_open || state.is_highlighter_menu_open || state.is_more_menu_open)
        && ctx.input(|i| i.pointer.any_pressed())
    {
        if let Some(pos) = ctx.input(|i| i.pointer.press_origin()) {
            let in_toolbar = state.toolbar_rect.is_some_and(|r| r.contains(pos));
            let in_popover = state.popover_rect.is_some_and(|r| r.contains(pos));
            if !in_toolbar && !in_popover {
                state.is_pen_menu_open = false;
                state.is_highlighter_menu_open = false;
                state.is_more_menu_open = false;
            }
        }
    }

    // 3. Schnellauswahl-Fenster für Seiten (im gleichen Milchglas-Look)
    if state.show_page_jump_popup {
        egui::Window::new("Seiten-Schnellauswahl")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 70.0))
            .frame(
                egui::Frame::none()
                    .fill(egui::Color32::from_rgba_premultiplied(24, 24, 28, 235))
                    .rounding(egui::Rounding::same(12.0))
                    .stroke(egui::Stroke::new(
                        1.0_f32,
                        egui::Color32::from_rgba_premultiplied(255, 255, 255, 30),
                    ))
                    .inner_margin(egui::Margin::symmetric(14.0, 12.0))
                    .shadow(egui::epaint::Shadow {
                        offset: egui::vec2(0.0, 6.0),
                        blur: 16.0,
                        spread: 0.0,
                        color: egui::Color32::from_black_alpha(120),
                    }),
            )
            .show(ctx, |ui| {
                ui.label(
                    egui::RichText::new(format!("Notizbuch: {} Seite(n)", notebook.pages.len()))
                        .strong()
                        .color(egui::Color32::WHITE),
                );
                ui.separator();

                egui::ScrollArea::vertical()
                    .max_height(220.0)
                    .show(ui, |ui| {
                        for idx in 0..notebook.pages.len() {
                            let is_current = idx == notebook.current_page_index;
                            let text = format!("Seite {}", idx + 1);
                            if tool_pill_button(ui, is_current, &text, "").clicked() {
                                actions.push(UiAction::GoToPage(idx));
                                state.show_page_jump_popup = false;
                            }
                        }
                    });

                ui.separator();
                if action_pill_button(ui, true, "Schließen", "Fenster schließen").clicked() {
                    state.show_page_jump_popup = false;
                }
            });
    }

    // 3b. Dokumenten-Manager (Notizbücher wechseln, anlegen, umbenennen, löschen)
    if state.is_document_menu_open {
        let doc_win = egui::Window::new("Dokumenten-Manager")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 70.0))
            .frame(
                egui::Frame::none()
                    .fill(egui::Color32::from_rgba_premultiplied(24, 24, 28, 242))
                    .rounding(egui::Rounding::same(14.0))
                    .stroke(egui::Stroke::new(
                        1.0_f32,
                        egui::Color32::from_rgba_premultiplied(255, 255, 255, 30),
                    ))
                    .inner_margin(egui::Margin::symmetric(18.0, 16.0))
                    .shadow(egui::epaint::Shadow {
                        offset: egui::vec2(0.0, 8.0),
                        blur: 20.0,
                        spread: 0.0,
                        color: egui::Color32::from_black_alpha(140),
                    }),
            )
            .show(ctx, |ui| {
                ui.set_width(380.0);

                // Kopfzeile
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("📚 Notizbücher verwalten")
                            .strong()
                            .size(15.0)
                            .color(egui::Color32::WHITE),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add(egui::Button::new(egui::RichText::new("✕").size(12.0)).frame(false))
                            .clicked()
                        {
                            state.is_document_menu_open = false;
                        }
                    });
                });

                ui.add_space(8.0);
                ui.separator();
                ui.add_space(8.0);

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
                            .hint_text("Titel des Notizbuchs...")
                            .desired_width(260.0),
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
                                        .desired_width(230.0),
                                );
                                if ui.button("✓").clicked()
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
                                if ui.button("✕").clicked() {
                                    state.renaming_notebook_id = None;
                                }
                            } else {
                                let (bg, border) = if is_current {
                                    (
                                        egui::Color32::from_rgba_premultiplied(45, 80, 145, 200),
                                        egui::Stroke::new(1.0_f32, egui::Color32::from_rgba_premultiplied(110, 165, 255, 180)),
                                    )
                                } else {
                                    (
                                        egui::Color32::from_rgba_premultiplied(255, 255, 255, 8),
                                        egui::Stroke::new(1.0_f32, egui::Color32::from_rgba_premultiplied(255, 255, 255, 16)),
                                    )
                                };

                                let frame_resp = egui::Frame::none()
                                    .fill(bg)
                                    .stroke(border)
                                    .rounding(egui::Rounding::same(8.0))
                                    .inner_margin(egui::Margin::symmetric(10.0, 8.0))
                                    .show(ui, |ui| {
                                        ui.horizontal(|ui| {
                                            ui.vertical(|ui| {
                                                ui.horizontal(|ui| {
                                                    ui.label(
                                                        egui::RichText::new(&summary.title)
                                                            .strong()
                                                            .size(13.0)
                                                            .color(egui::Color32::WHITE),
                                                    );
                                                    if summary.status == crate::storage::NotebookStatus::Damaged {
                                                        ui.colored_label(egui::Color32::from_rgb(255, 100, 100), "⚠️ Beschädigt");
                                                    } else if is_current {
                                                        ui.colored_label(egui::Color32::from_rgb(46, 204, 113), "● Aktiv");
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

                                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                                if summary.status == crate::storage::NotebookStatus::Damaged {
                                                    if ui
                                                        .button(egui::RichText::new("🔄 Backup").size(11.0).color(egui::Color32::from_rgb(100, 200, 255)))
                                                        .on_hover_text("Aus manifest.json.bak wiederherstellen")
                                                        .clicked()
                                                    {
                                                        actions.push(UiAction::RestoreNotebookBackup(summary.id.clone()));
                                                    }
                                                    if ui
                                                        .button(egui::RichText::new("📁 Ordner").size(11.0))
                                                        .on_hover_text("Pfad in Statusleiste anzeigen")
                                                        .clicked()
                                                    {
                                                        actions.push(UiAction::OpenNotebookFolder(summary.id.clone()));
                                                    }
                                                } else {
                                                    if state.available_notebooks.len() > 1
                                                        && ui
                                                            .button(egui::RichText::new("🗑").size(11.0).color(egui::Color32::from_rgb(255, 120, 120)))
                                                            .on_hover_text("Notizbuch in Papierkorb verschieben")
                                                            .clicked()
                                                    {
                                                        actions.push(UiAction::DeleteNotebook(summary.id.clone()));
                                                    }

                                                    if ui.button(egui::RichText::new("✏").size(11.0))
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
                                    && frame_resp.response.interact(egui::Sense::click()).clicked()
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
                            .button(egui::RichText::new("↩ Rückgängig").size(11.0).color(egui::Color32::from_rgb(100, 200, 255)))
                            .clicked()
                        {
                            actions.push(UiAction::UndoDeleteNotebook);
                        }
                    });
                }

                ui.add_space(8.0);
                ui.separator();
                ui.add_space(4.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if action_pill_button(ui, true, "Schließen", "Dokumenten-Manager schließen").clicked() {
                        state.is_document_menu_open = false;
                    }
                });
            });
        state.document_modal_rect = doc_win.map(|w| w.response.rect);
    } else {
        state.document_modal_rect = None;
    }

    // 3c. Release- & Update-Center (Gitea Releases)
    if state.is_update_dialog_open {
        let win = egui::Window::new("Release- & Update-Center")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 70.0))
            .frame(
                egui::Frame::none()
                    .fill(egui::Color32::from_rgba_premultiplied(24, 24, 28, 245))
                    .rounding(egui::Rounding::same(14.0))
                    .stroke(egui::Stroke::new(
                        1.0_f32,
                        egui::Color32::from_rgba_premultiplied(255, 255, 255, 30),
                    ))
                    .inner_margin(egui::Margin::symmetric(20.0, 16.0))
                    .shadow(egui::epaint::Shadow {
                        offset: egui::vec2(0.0, 8.0),
                        blur: 24.0,
                        spread: 0.0,
                        color: egui::Color32::from_black_alpha(150),
                    }),
            )
            .show(ctx, |ui| {
                ui.set_width(450.0);

                // Kopfzeile mit Schließen-Button
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("🚀 Gitea Release- & Update-Center")
                            .strong()
                            .size(16.0)
                            .color(egui::Color32::WHITE),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add(egui::Button::new(egui::RichText::new("✕").size(13.0)).frame(false))
                            .clicked()
                        {
                            state.is_update_dialog_open = false;
                        }
                    });
                });

                ui.add_space(8.0);
                ui.separator();
                ui.add_space(8.0);

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
                            ui.colored_label(egui::Color32::from_rgb(46, 204, 113), "✓");
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
                                ui.colored_label(egui::Color32::from_rgb(46, 204, 113), "●");
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
                                ui.colored_label(egui::Color32::from_rgb(46, 204, 113), "✓");
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

                ui.add_space(6.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if action_pill_button(ui, true, "Schließen", "Fenster schließen").clicked() {
                        state.is_update_dialog_open = false;
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
                                egui::RichText::new(format!("✓ {}", msg))
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
        egui::Window::new("⚠️ Datenintegritäts-Warnung")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .frame(
                egui::Frame::none()
                    .fill(egui::Color32::from_rgba_premultiplied(35, 20, 20, 245))
                    .rounding(egui::Rounding::same(12.0))
                    .stroke(egui::Stroke::new(
                        1.5_f32,
                        egui::Color32::from_rgb(235, 87, 87),
                    ))
                    .inner_margin(egui::Margin::symmetric(24.0, 20.0))
                    .shadow(egui::epaint::Shadow {
                        offset: egui::vec2(0.0, 8.0),
                        blur: 24.0,
                        spread: 0.0,
                        color: egui::Color32::from_black_alpha(180),
                    }),
            )
            .show(ctx, |ui| {
                ui.set_width(420.0);
                ui.horizontal(|ui| {
                    ui.colored_label(egui::Color32::from_rgb(255, 80, 80), "⚠️");
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
        egui::Window::new("⚠️ Ungespeicherte Änderungen")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .frame(
                egui::Frame::none()
                    .fill(egui::Color32::from_rgba_premultiplied(35, 20, 20, 250))
                    .rounding(egui::Rounding::same(12.0))
                    .stroke(egui::Stroke::new(
                        1.5_f32,
                        egui::Color32::from_rgb(235, 87, 87),
                    ))
                    .inner_margin(egui::Margin::symmetric(24.0, 20.0))
                    .shadow(egui::epaint::Shadow {
                        offset: egui::vec2(0.0, 8.0),
                        blur: 24.0,
                        spread: 0.0,
                        color: egui::Color32::from_black_alpha(200),
                    }),
            )
            .show(ctx, |ui| {
                ui.set_width(460.0);
                ui.horizontal(|ui| {
                    ui.colored_label(egui::Color32::from_rgb(255, 80, 80), "⚠️");
                    ui.label(
                        egui::RichText::new("Fehler beim Beenden")
                            .color(egui::Color32::from_rgb(255, 100, 100))
                            .size(16.0)
                            .strong(),
                    );
                });
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new(
                        "Ungespeicherte Änderungen konnten nicht gesichert werden:",
                    )
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
            UiState::sidebar_button_size(ActiveTool::Select, 1000.0),
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
}
