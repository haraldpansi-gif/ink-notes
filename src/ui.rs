//! Immediate-Mode UI mit `egui` für ink-notes.
//!
//! Stellt eine moderne, schwebende Top-Toolbar im Pill-Design bereit:
//! - Milchglas-Look (Transluzenter Dark-Theme-Hintergrund mit weichem Schatten)
//! - Abgerundete Ecken (14 px Radius) und feine Kante
//! - Visuelle Farbindikatoren (3 px hohe Balken) direkt an den Werkzeug-Buttons
//! - Schwebendes Popover-Menü zur Konfiguration von Stiftfarbe, Dicken-Presets mit Linien-Vorschau und stufenlosem Slider
//! - Exaktes Hit-Testing für Event-Transparenz außerhalb der Pill und des Popovers
//! - Werkzeugauswahl: Stift (Pen), Textmarker (Highlighter), Radierer (Eraser) mit Akzent-Highlight
//! - Undo & Redo (mit dynamischer Deaktivierung)
//! - Diskrete Seitennavigation (◄, Seite X/Y mit Schnellauswahl, ►, ➕ Neue Seite)
//! - Hintergrundraster-Dropdown (Kariert, Liniert, Punktraster, Blanko)
//! - PDF-Export-Button mit Feedback-Banner
//! - Autosave-Statusanzeige (Live-Badge mit Farbindikator)

use std::time::Instant;

use crate::autosave::AutosaveStatus;
use crate::document::{Notebook, PageBackground};
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
pub const HIGHLIGHTER_WIDTH_PRESETS: [(f32, &str); 3] = [
    (4.0, "4.0 mm"),
    (6.0, "6.0 mm"),
    (8.0, "8.0 mm"),
];

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
    RenameNotebook { id: String, new_title: String },
    DeleteNotebook(String),
    RefreshNotebooks,
    // Gitea Release & Updater
    CheckForUpdates,
    DownloadUpdateAsset(crate::updater::GiteaAsset),
    InstallOrOpenUpdate(std::path::PathBuf),
    OpenUrlInBrowser(String),
    SaveUpdaterConfig { server_url: String, auth_token: Option<String> },
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
        }
    }
}

impl UiState {
    /// Radius der abgerundeten Ecken für die Toolbar-Pill.
    pub const TOOLBAR_ROUNDING: f32 = 14.0;

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

/// Stilvoller Werkzeug-Button mit 3 px hohem Farbindikator-Balken an der Unterkante
/// und Ausklapp-Indikator (▾ / ▴).
fn tool_pill_button_with_indicator(
    ui: &mut egui::Ui,
    is_active: bool,
    text: &str,
    indicator_color: [f32; 4],
    is_menu_open: bool,
    tooltip: &str,
) -> egui::Response {
    let size = egui::vec2(90.0, 30.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());

    if ui.is_rect_visible(rect) {
        let (bg, text_color, stroke) = if is_active {
            (
                egui::Color32::from_rgba_premultiplied(45, 80, 145, 230),
                egui::Color32::WHITE,
                egui::Stroke::new(1.0_f32, egui::Color32::from_rgba_premultiplied(110, 165, 255, 190)),
            )
        } else if response.hovered() {
            (
                egui::Color32::from_rgba_premultiplied(255, 255, 255, 16),
                egui::Color32::WHITE,
                egui::Stroke::new(1.0_f32, egui::Color32::from_rgba_premultiplied(255, 255, 255, 25)),
            )
        } else {
            (
                egui::Color32::from_rgba_premultiplied(255, 255, 255, 8),
                egui::Color32::from_gray(215),
                egui::Stroke::new(1.0_f32, egui::Color32::from_rgba_premultiplied(255, 255, 255, 15)),
            )
        };

        // Button-Hintergrund
        ui.painter().rect_filled(rect, egui::Rounding::same(8.0), bg);
        ui.painter().rect_stroke(rect, egui::Rounding::same(8.0), stroke);

        // Text & Caret
        let label = if is_menu_open {
            format!("{} ▴", text)
        } else {
            format!("{} ▾", text)
        };
        ui.painter().text(
            egui::pos2(rect.center().x, rect.center().y - 2.5),
            egui::Align2::CENTER_CENTER,
            label,
            egui::FontId::proportional(12.5),
            text_color,
        );

        // 3 px hoher Farbbalken an der Unterkante
        let bar_margin_x = 10.0;
        let bar_h = 3.0;
        let bar_rect = egui::Rect::from_min_max(
            egui::pos2(rect.min.x + bar_margin_x, rect.max.y - bar_h - 2.5),
            egui::pos2(rect.max.x - bar_margin_x, rect.max.y - 2.5),
        );
        let bar_color = egui::Color32::from_rgba_unmultiplied(
            (indicator_color[0] * 255.0) as u8,
            (indicator_color[1] * 255.0) as u8,
            (indicator_color[2] * 255.0) as u8,
            ((indicator_color[3] * 255.0) as u8).max(180),
        );
        ui.painter().rect_filled(bar_rect, egui::Rounding::same(1.5), bar_color);
    }

    response.on_hover_text(tooltip)
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
            egui::Stroke::new(1.0_f32, egui::Color32::from_rgba_premultiplied(110, 165, 255, 190)),
        )
    } else {
        (
            egui::Color32::from_rgba_premultiplied(255, 255, 255, 8),
            egui::Color32::from_gray(215),
            egui::Stroke::new(1.0_f32, egui::Color32::from_rgba_premultiplied(255, 255, 255, 15)),
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
                egui::Stroke::new(1.0_f32, egui::Color32::from_rgba_premultiplied(120, 175, 255, 200)),
            )
        } else if response.hovered() {
            (
                egui::Color32::from_rgba_premultiplied(255, 255, 255, 16),
                egui::Stroke::new(1.0_f32, egui::Color32::from_rgba_premultiplied(255, 255, 255, 25)),
            )
        } else {
            (
                egui::Color32::from_rgba_premultiplied(255, 255, 255, 8),
                egui::Stroke::new(1.0_f32, egui::Color32::from_rgba_premultiplied(255, 255, 255, 14)),
            )
        };

        ui.painter().rect_filled(rect, egui::Rounding::same(6.0), bg);
        ui.painter().rect_stroke(rect, egui::Rounding::same(6.0), border);

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
            if is_active { egui::Color32::WHITE } else { egui::Color32::from_gray(190) },
        );
    }

    response.on_hover_text(format!("Stärke auf {} setzen", label_text))
}

/// Kompakter Strichbreiten-Preset-Button für die Haupt-Toolbar.
fn preset_width_button(
    ui: &mut egui::Ui,
    is_active: bool,
    text: &str,
) -> egui::Response {
    let (bg, text_color, stroke) = if is_active {
        (
            egui::Color32::from_rgba_premultiplied(50, 85, 150, 230),
            egui::Color32::WHITE,
            egui::Stroke::new(1.0_f32, egui::Color32::from_rgba_premultiplied(120, 175, 255, 190)),
        )
    } else {
        (
            egui::Color32::from_rgba_premultiplied(255, 255, 255, 6),
            egui::Color32::from_gray(190),
            egui::Stroke::new(1.0_f32, egui::Color32::from_rgba_premultiplied(255, 255, 255, 12)),
        )
    };

    let button = egui::Button::new(
        egui::RichText::new(text)
            .color(text_color)
            .size(11.5),
    )
    .fill(bg)
    .stroke(stroke)
    .rounding(egui::Rounding::same(6.0))
    .min_size(egui::vec2(0.0, 24.0));

    ui.add(button)
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
            egui::Stroke::new(1.0_f32, egui::Color32::from_rgba_premultiplied(255, 255, 255, 22)),
        )
    } else {
        (
            egui::Color32::from_rgba_premultiplied(255, 255, 255, 3),
            egui::Stroke::new(1.0_f32, egui::Color32::from_rgba_premultiplied(255, 255, 255, 8)),
        )
    };

    let button = egui::Button::new(
        egui::RichText::new(text)
            .color(text_color)
            .size(12.5),
    )
    .fill(bg)
    .stroke(stroke)
    .rounding(egui::Rounding::same(7.0))
    .min_size(egui::vec2(0.0, 26.0));

    ui.add_enabled(enabled, button).on_hover_text(tooltip)
}

/// Feine, transparente Trennlinie für Gruppenabstände.
fn toolbar_divider(ui: &mut egui::Ui) {
    ui.add_space(2.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(1.0, 20.0), egui::Sense::hover());
    ui.painter().vline(
        rect.center().x,
        rect.y_range(),
        egui::Stroke::new(1.0_f32, egui::Color32::from_rgba_premultiplied(255, 255, 255, 25)),
    );
    ui.add_space(2.0);
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

        let stroke = if is_selected {
            egui::Stroke::new(2.5_f32, egui::Color32::WHITE)
        } else if response.hovered() {
            egui::Stroke::new(1.8_f32, egui::Color32::from_white_alpha(200))
        } else {
            egui::Stroke::new(1.0_f32, egui::Color32::from_white_alpha(65))
        };

        let radius = (diameter / 2.0) - 1.5;
        ui.painter().circle_filled(rect.center(), radius, c32);
        ui.painter().circle_stroke(rect.center(), radius, stroke);
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

    // 1. Schwebende Top-Toolbar (zentriert oben mit 16 px Abstand zum Fensterrand)
    let area_response = egui::Area::new(egui::Id::new("floating_toolbar_area"))
        .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 16.0))
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            let frame_response = egui::Frame::none()
                .fill(egui::Color32::from_rgba_premultiplied(24, 24, 28, 225)) // Transluzenter Milchglas-Hintergrund
                .rounding(egui::Rounding::same(UiState::TOOLBAR_ROUNDING)) // Abgerundete Pill-Ecken (14 px)
                .stroke(egui::Stroke::new(
                    1.0_f32,
                    egui::Color32::from_rgba_premultiplied(255, 255, 255, 30), // Feine Kante
                ))
                .inner_margin(egui::Margin::symmetric(14.0, 8.0))
                .shadow(egui::epaint::Shadow {
                    offset: egui::vec2(0.0, 4.0),
                    blur: 16.0,
                    spread: 0.0,
                    color: egui::Color32::from_black_alpha(100), // Weicher Schattenwurf
                })
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing = egui::vec2(6.0, 0.0);

                        // --- 0. DOKUMENTEN- & NOTIZBUCH-AUSWAHL ---
                        let doc_title_truncated = if state.document_title.chars().count() > 16 {
                            let prefix: String = state.document_title.chars().take(14).collect();
                            format!("{prefix}…")
                        } else if state.document_title.is_empty() {
                            "Notizbuch".to_string()
                        } else {
                            state.document_title.clone()
                        };
                        let doc_btn_label = format!("📚 {} ▾", doc_title_truncated);
                        let doc_resp = tool_pill_button(
                            ui,
                            state.is_document_menu_open,
                            &doc_btn_label,
                            "Dokumenten-Manager: Notizbuch wechseln, anlegen oder verwalten",
                        );
                        if doc_resp.clicked() {
                            state.is_document_menu_open = !state.is_document_menu_open;
                            state.is_pen_menu_open = false;
                            state.is_highlighter_menu_open = false;
                            if state.is_document_menu_open {
                                actions.push(UiAction::RefreshNotebooks);
                            }
                        }

                        toolbar_divider(ui);

                        // --- 1. WERKZEUGAUSWAHL MIT FARBINDIKATOR ---
                        let pen_resp = tool_pill_button_with_indicator(
                            ui,
                            state.active_tool == ActiveTool::Pen,
                            "✏ Stift",
                            state.pen_color,
                            state.is_pen_menu_open,
                            "Stift wählen / Einstellungen öffnen (Farbe & Stärke)",
                        );
                        pen_btn_rect = Some(pen_resp.rect);

                        if pen_resp.clicked() {
                            if state.active_tool != ActiveTool::Pen {
                                state.active_tool = ActiveTool::Pen;
                                state.is_pen_menu_open = true;
                                state.is_highlighter_menu_open = false;
                                actions.push(UiAction::SetTool(ActiveTool::Pen));
                                actions.push(UiAction::SetColor(state.pen_color));
                                actions.push(UiAction::SetStrokeWidthMm(state.pen_stroke_width_mm));
                            } else {
                                state.is_pen_menu_open = !state.is_pen_menu_open;
                                state.is_highlighter_menu_open = false;
                            }
                        }

                        let high_resp = tool_pill_button_with_indicator(
                            ui,
                            state.active_tool == ActiveTool::Highlighter,
                            "🖍 Marker",
                            state.highlighter_color,
                            state.is_highlighter_menu_open,
                            "Textmarker wählen / Einstellungen öffnen",
                        );
                        high_btn_rect = Some(high_resp.rect);

                        if high_resp.clicked() {
                            if state.active_tool != ActiveTool::Highlighter {
                                state.active_tool = ActiveTool::Highlighter;
                                state.is_highlighter_menu_open = true;
                                state.is_pen_menu_open = false;
                                actions.push(UiAction::SetTool(ActiveTool::Highlighter));
                                actions.push(UiAction::SetColor(state.highlighter_color));
                                actions.push(UiAction::SetStrokeWidthMm(state.highlighter_stroke_width_mm));
                            } else {
                                state.is_highlighter_menu_open = !state.is_highlighter_menu_open;
                                state.is_pen_menu_open = false;
                            }
                        }

                        let shape_resp = tool_pill_button(
                            ui,
                            state.active_tool == ActiveTool::Shape,
                            "📐 Formen",
                            "Geometrie / Auto-Snap (Tippe auf Striche zur perfekten Formerkennung)",
                        );
                        if shape_resp.clicked() {
                            state.active_tool = ActiveTool::Shape;
                            state.is_pen_menu_open = false;
                            state.is_highlighter_menu_open = false;
                            actions.push(UiAction::SetTool(ActiveTool::Shape));
                        }

                        if tool_pill_button(
                            ui,
                            state.active_tool == ActiveTool::Eraser,
                            "⌫ Radierer",
                            "Strichradierer (Taste: E oder Rechtsklick)",
                        )
                        .clicked()
                        {
                            state.active_tool = ActiveTool::Eraser;
                            state.is_pen_menu_open = false;
                            state.is_highlighter_menu_open = false;
                            actions.push(UiAction::SetTool(ActiveTool::Eraser));
                        }

                        let select_resp = tool_pill_button(
                            ui,
                            state.active_tool == ActiveTool::Select,
                            "⬚ Auswählen",
                            "Objektauswahl / Lasso (Taste: V) - Verschieben, Drehen, Skalieren, Kopieren, Löschen",
                        );
                        if select_resp.clicked() {
                            state.active_tool = ActiveTool::Select;
                            state.is_pen_menu_open = false;
                            state.is_highlighter_menu_open = false;
                            actions.push(UiAction::SetTool(ActiveTool::Select));
                        }

                        toolbar_divider(ui);

                        // --- 2. FARBPALETTE SCHNELLAUSWAHL ---
                        if state.active_tool == ActiveTool::Pen || state.active_tool == ActiveTool::Shape || state.active_tool == ActiveTool::Select {
                            for (color, label) in PEN_COLOR_PRESETS {
                                let is_sel = (state.pen_color[0] - color[0]).abs() < 0.05
                                    && (state.pen_color[1] - color[1]).abs() < 0.05
                                    && (state.pen_color[2] - color[2]).abs() < 0.05;
                                if color_circle_swatch(ui, color, is_sel, label, 20.0).clicked() {
                                    state.pen_color = color;
                                    actions.push(UiAction::SetColor(color));
                                }
                            }
                        } else if state.active_tool == ActiveTool::Highlighter {
                            for (color, label) in HIGHLIGHTER_COLOR_PRESETS {
                                let is_sel = (state.highlighter_color[0] - color[0]).abs() < 0.05
                                    && (state.highlighter_color[1] - color[1]).abs() < 0.05
                                    && (state.highlighter_color[2] - color[2]).abs() < 0.05;
                                if color_circle_swatch(ui, color, is_sel, label, 20.0).clicked() {
                                    state.highlighter_color = color;
                                    actions.push(UiAction::SetColor(color));
                                }
                            }
                        }

                        // Bei aktiver Auswahl: Direkte Schnell-Aktionen in der Toolbar
                        if state.active_tool == ActiveTool::Select {
                            toolbar_divider(ui);
                            if ui.button(egui::RichText::new("📋 Kopieren").size(12.0)).on_hover_text("Auswahl in die Zwischenablage kopieren (Strg+C)").clicked() {
                                actions.push(UiAction::CopySelected);
                            }
                            if ui.button(egui::RichText::new("📥 Einfügen").size(12.0)).on_hover_text("Aus Zwischenablage einfügen (Strg+V)").clicked() {
                                actions.push(UiAction::PasteSelected);
                            }
                            if ui.button(egui::RichText::new("🔄 90° Drehen").size(12.0)).on_hover_text("Ausgewählte Objekte um 90° im Uhrzeigersinn drehen (R)").clicked() {
                                actions.push(UiAction::RotateSelected);
                            }
                            if ui.button(egui::RichText::new("🗑 Löschen").color(egui::Color32::from_rgb(255, 110, 110)).size(12.0)).on_hover_text("Ausgewählte Objekte entfernen (Entf/Backspace)").clicked() {
                                actions.push(UiAction::DeleteSelected);
                            }
                        }

                        if state.active_tool != ActiveTool::Eraser && state.active_tool != ActiveTool::Select {
                            toolbar_divider(ui);
                        }

                        // --- 3. STRICHBREITEN-SCHNELLAUSWAHL ---
                        if state.active_tool == ActiveTool::Pen || state.active_tool == ActiveTool::Shape {
                            for &(preset, label) in &PEN_WIDTH_PRESETS {
                                let is_sel = (state.pen_stroke_width_mm - preset).abs() < 0.05;
                                if preset_width_button(ui, is_sel, label).clicked() {
                                    state.pen_stroke_width_mm = preset;
                                    actions.push(UiAction::SetStrokeWidthMm(preset));
                                }
                            }
                            toolbar_divider(ui);
                        } else if state.active_tool == ActiveTool::Highlighter {
                            for &(preset, label) in &HIGHLIGHTER_WIDTH_PRESETS {
                                let is_sel = (state.highlighter_stroke_width_mm - preset).abs() < 0.1;
                                if preset_width_button(ui, is_sel, label).clicked() {
                                    state.highlighter_stroke_width_mm = preset;
                                    actions.push(UiAction::SetStrokeWidthMm(preset));
                                }
                            }
                            toolbar_divider(ui);
                        }

                        // --- 4. UNDO & REDO ---
                        let can_undo = notebook.current_page().history.can_undo();
                        let can_redo = notebook.current_page().history.can_redo();

                        if action_pill_button(ui, can_undo, "⟲ Undo", "Rückgängig (Strg + Z)").clicked() {
                            actions.push(UiAction::Undo);
                        }

                        if action_pill_button(ui, can_redo, "⟳ Redo", "Wiederholen (Strg + Y)").clicked() {
                            actions.push(UiAction::Redo);
                        }

                        toolbar_divider(ui);

                        // --- 5. DISKRETE SEITENNAVIGATION ---
                        let can_prev = notebook.current_page_index > 0;
                        if action_pill_button(ui, can_prev, "◄", "Vorherige Seite (PageUp)").clicked() {
                            actions.push(UiAction::PreviousPage);
                        }

                        let page_badge_text = format!(
                            "Seite {} / {}",
                            notebook.current_page_index + 1,
                            notebook.pages.len()
                        );
                        if action_pill_button(ui, true, &page_badge_text, "Schnellauswahl öffnen").clicked() {
                            state.show_page_jump_popup = !state.show_page_jump_popup;
                        }

                        if action_pill_button(ui, true, "►", "Nächste Seite (PageDown / N)").clicked() {
                            actions.push(UiAction::NextPage);
                        }

                        if action_pill_button(ui, true, "➕", "Neue Seite anlegen").clicked() {
                            actions.push(UiAction::AddPage);
                        }

                        toolbar_divider(ui);

                        // --- 6. HINTERGRUNDRASTER-DROPDOWN ---
                        let current_bg = notebook.current_page().background;
                        let current_bg_text = match current_bg {
                            PageBackground::Grid5mm => "Kariert (5mm)",
                            PageBackground::Lined => "Liniert (7.5mm)",
                            PageBackground::DotGrid => "Punktraster (5mm)",
                            PageBackground::Blank => "Blanko",
                        };

                        egui::ComboBox::from_id_source("bg_dropdown_picker")
                            .selected_text(current_bg_text)
                            .show_ui(ui, |ui| {
                                if ui
                                    .selectable_label(current_bg == PageBackground::Grid5mm, "Kariert (5mm)")
                                    .clicked()
                                {
                                    actions.push(UiAction::SetBackground(PageBackground::Grid5mm));
                                }
                                if ui
                                    .selectable_label(current_bg == PageBackground::Lined, "Liniert (7.5mm)")
                                    .clicked()
                                {
                                    actions.push(UiAction::SetBackground(PageBackground::Lined));
                                }
                                if ui
                                    .selectable_label(current_bg == PageBackground::DotGrid, "Punktraster (5mm)")
                                    .clicked()
                                {
                                    actions.push(UiAction::SetBackground(PageBackground::DotGrid));
                                }
                                if ui
                                    .selectable_label(current_bg == PageBackground::Blank, "Blanko")
                                    .clicked()
                                {
                                    actions.push(UiAction::SetBackground(PageBackground::Blank));
                                }
                            });

                        toolbar_divider(ui);

                        // --- 7. PDF-EXPORT-BUTTON ---
                        if action_pill_button(
                            ui,
                            true,
                            "📄 PDF",
                            "Dokument als Vektor-PDF exportieren (Strg + E / S)",
                        )
                        .clicked()
                        {
                            actions.push(UiAction::ExportPdf);
                        }

                        toolbar_divider(ui);

                        // --- 8. ZOOM-STEUERUNG & ANZEIGE ---
                        if action_pill_button(ui, state.current_zoom > 0.25, "🔍-", "Herauszoomen (Mausrad)").clicked() {
                            actions.push(UiAction::ZoomOut);
                        }

                        let zoom_pct = (state.current_zoom * 100.0).round() as u32;
                        let zoom_badge = format!("{}%", zoom_pct);
                        if action_pill_button(ui, true, &zoom_badge, "Zoom auf 100% zentriert zurücksetzen (Strg + 0)").clicked() {
                            actions.push(UiAction::ResetZoom);
                        }

                        if action_pill_button(ui, state.current_zoom < 5.0, "🔍+", "Heranzoomen (Mausrad)").clicked() {
                            actions.push(UiAction::ZoomIn);
                        }

                        toolbar_divider(ui);

                        // --- 9. AUTOSAVE-STATUSANZEIGE ---
                        let (dot_color, label_text) = match autosave_status {
                            AutosaveStatus::Saved => (egui::Color32::from_rgb(46, 204, 113), "Gespeichert"),
                            AutosaveStatus::Saving => (egui::Color32::from_rgb(241, 196, 15), "Speichert..."),
                            AutosaveStatus::Pending => (egui::Color32::from_rgb(230, 126, 34), "Ungespeichert"),
                            AutosaveStatus::Error => (egui::Color32::from_rgb(231, 76, 60), "Fehler"),
                        };

                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing = egui::vec2(4.0, 0.0);
                            ui.colored_label(dot_color, "●");
                            ui.label(
                                egui::RichText::new(label_text)
                                    .color(dot_color)
                                    .size(11.5)
                                    .strong(),
                            );
                        });

                        toolbar_divider(ui);

                        // --- 10. GITEA-RELEASE & UPDATE-STATUS ---
                        let (upd_label, is_highlight) = match updater.status() {
                            crate::updater::UpdaterStatus::UpdateAvailable(rel) => {
                                (format!("🚀 Neu: {}", rel.tag_name), true)
                            }
                            crate::updater::UpdaterStatus::Downloading { percent, .. } => {
                                (format!("⏳ {:.0}%", percent * 100.0), true)
                            }
                            crate::updater::UpdaterStatus::DownloadFinished { .. } => {
                                ("✓ Update bereit".to_string(), true)
                            }
                            crate::updater::UpdaterStatus::Checking => {
                                ("🔄 Prüfe...".to_string(), false)
                            }
                            _ => ("🔄 Update".to_string(), false),
                        };

                        let upd_btn = if is_highlight {
                            let text = egui::RichText::new(&upd_label)
                                .color(egui::Color32::from_rgb(46, 204, 113))
                                .strong();
                            ui.add(egui::Button::new(text).frame(true))
                                .on_hover_text("Update verfügbar! Klicke hier für Release-Notes und Download.")
                        } else {
                            action_pill_button(
                                ui,
                                true,
                                &upd_label,
                                "Release- & Update-Manager (Gitea Releases prüfen & installieren)",
                            )
                        };

                        if upd_btn.clicked() {
                            state.is_update_dialog_open = !state.is_update_dialog_open;
                            state.is_pen_menu_open = false;
                            state.is_highlighter_menu_open = false;
                            state.is_document_menu_open = false;
                        }
                    });
                });

            frame_response.response.rect
        });

    // Toolbar-Rechteck für pixelgenaue Event-Isolation speichern
    state.toolbar_rect = Some(area_response.inner);

    // 2. SCHWEBENDES POPOVER-MENÜ (FÜR STIFT ODER TEXTMARKER)
    let mut popover_rect = None;

    if state.is_pen_menu_open {
        if let Some(btn_rect) = pen_btn_rect {
            let popover_pos = egui::pos2(btn_rect.min.x - 12.0, btn_rect.max.y + 8.0);
            let pop_response = egui::Area::new(egui::Id::new("pen_settings_popover"))
                .fixed_pos(popover_pos)
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
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if ui
                                        .add(egui::Button::new(egui::RichText::new("✕").size(11.0)).frame(false))
                                        .clicked()
                                    {
                                        state.is_pen_menu_open = false;
                                    }
                                });
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
                                    if color_circle_swatch(ui, color, is_sel, label, 22.0).clicked() {
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
                                    if stroke_preview_button(ui, is_sel, preset, label, state.pen_color).clicked()
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
            let popover_pos = egui::pos2(btn_rect.min.x - 12.0, btn_rect.max.y + 8.0);
            let pop_response = egui::Area::new(egui::Id::new("highlighter_settings_popover"))
                .fixed_pos(popover_pos)
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
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if ui
                                        .add(egui::Button::new(egui::RichText::new("✕").size(11.0)).frame(false))
                                        .clicked()
                                    {
                                        state.is_highlighter_menu_open = false;
                                    }
                                });
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
                                    if color_circle_swatch(ui, color, is_sel, label, 22.0).clicked() {
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
                                    let is_sel = (state.highlighter_stroke_width_mm - preset).abs() < 0.1;
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
                                    egui::Slider::new(&mut state.highlighter_stroke_width_mm, 2.0..=16.0)
                                        .suffix(" mm")
                                        .step_by(0.5),
                                )
                                .changed()
                            {
                                actions.push(UiAction::SetStrokeWidthMm(state.highlighter_stroke_width_mm));
                            }
                        });

                    frame.response.rect
                });

            popover_rect = Some(pop_response.inner);
        }
    }

    state.popover_rect = popover_rect;

    // Automatisches Schließen des Popovers bei Klick außerhalb von Toolbar und Popover
    if (state.is_pen_menu_open || state.is_highlighter_menu_open)
        && ctx.input(|i| i.pointer.any_pressed())
    {
        if let Some(pos) = ctx.input(|i| i.pointer.press_origin()) {
            let in_toolbar = state.toolbar_rect.is_some_and(|r| r.contains(pos));
            let in_popover = state.popover_rect.is_some_and(|r| r.contains(pos));
            if !in_toolbar && !in_popover {
                state.is_pen_menu_open = false;
                state.is_highlighter_menu_open = false;
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

                egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
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
                                                    if is_current {
                                                        ui.colored_label(egui::Color32::from_rgb(46, 204, 113), "● Aktiv");
                                                    }
                                                });
                                                let subtext = format!("{} Seite(n)", summary.page_count);
                                                ui.label(
                                                    egui::RichText::new(subtext)
                                                        .size(11.0)
                                                        .color(egui::Color32::from_gray(160)),
                                                );
                                            });

                                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                                if state.available_notebooks.len() > 1 {
                                                    if ui.button(egui::RichText::new("🗑").size(11.0).color(egui::Color32::from_rgb(255, 120, 120)))
                                                        .on_hover_text("Notizbuch unwiderruflich löschen")
                                                        .clicked()
                                                    {
                                                        actions.push(UiAction::DeleteNotebook(summary.id.clone()));
                                                    }
                                                }

                                                if ui.button(egui::RichText::new("✏").size(11.0))
                                                    .on_hover_text("Titel umbenennen")
                                                    .clicked()
                                                {
                                                    state.renaming_notebook_id = Some(summary.id.clone());
                                                    state.renaming_title_input = summary.title.clone();
                                                }
                                            });
                                        });
                                    });

                                if frame_resp.response.interact(egui::Sense::click()).clicked() && !is_current {
                                    actions.push(UiAction::OpenNotebook(summary.id.clone()));
                                    state.is_document_menu_open = false;
                                }
                            }
                        });
                        ui.add_space(4.0);
                    }
                });

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
                            if !release.html_url.is_empty() {
                                if ui.button(egui::RichText::new("🌐 Release auf Gitea öffnen").size(12.0)).clicked() {
                                    actions.push(UiAction::OpenUrlInBrowser(release.html_url.clone()));
                                }
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
                        .fill(egui::Color32::from_rgba_unmultiplied(20, 25, 30, alpha.min(235)))
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
                                    .color(egui::Color32::from_rgba_unmultiplied(220, 255, 230, alpha))
                                    .size(13.0)
                                    .strong(),
                            );
                        });
                });
        } else {
            state.export_message = None;
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
            },
            NotebookSummary {
                id: "nb-2".to_string(),
                title: "Physik II".to_string(),
                page_count: 5,
                modified_at: 200,
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
}
