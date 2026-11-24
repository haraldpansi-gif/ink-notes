//! Wiederverwendbare UI-Bausteine, Buttons, Swatches, Rahmen und Eingabe-Editoren.

use crate::autosave::AutosaveStatus;
use crate::background::{PaperTheme, PatternColor};
use crate::settings::{PenPreset, PressureCurve, SidebarSide, MAX_SAVED_COLORS};
use crate::storage::NotebookLayout;
use crate::t;
use crate::text::{TextFont, TEXT_SIZES_PT};

use super::overlays::linear_color32;
use super::{ColorPreset, UiState};

/// Dicken-Preset-Button mit visueller Linien-Vorschau in Originalfarbe und Proportion.
pub(super) fn stroke_preview_button(
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
            .rect_filled(rect, egui::CornerRadius::same(6), bg);
        ui.painter().rect_stroke(
            rect,
            egui::CornerRadius::same(6),
            border,
            egui::StrokeKind::Middle,
        );

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

    response.on_hover_text(t!("Stärke auf {width} setzen", width = label_text))
}
/// Eraser size button of the eraser menu: a circle in proportion to the size and its diameter.
pub(super) fn eraser_size_button(
    ui: &mut egui::Ui,
    is_active: bool,
    radius_mm: f32,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(66.0, 40.0), egui::Sense::click());
    let label = format!("{} mm", format_mm(radius_mm * 2.0));
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let (bg, border) = if is_active {
            (
                egui::Color32::from_rgba_premultiplied(50, 85, 150, 220),
                egui::Color32::from_rgba_unmultiplied(120, 175, 255, 200),
            )
        } else if response.hovered() {
            (
                egui::Color32::from_rgba_unmultiplied(255, 255, 255, 16),
                egui::Color32::from_rgba_unmultiplied(255, 255, 255, 25),
            )
        } else {
            (
                egui::Color32::from_rgba_unmultiplied(255, 255, 255, 8),
                egui::Color32::from_rgba_unmultiplied(255, 255, 255, 14),
            )
        };
        painter.rect_filled(rect, egui::CornerRadius::same(6), bg);
        painter.rect_stroke(
            rect,
            egui::CornerRadius::same(6),
            egui::Stroke::new(1.0_f32, border),
            egui::StrokeKind::Middle,
        );
        let text_color = if is_active {
            egui::Color32::WHITE
        } else {
            egui::Color32::from_gray(190)
        };
        painter.circle_stroke(
            egui::pos2(rect.center().x, rect.center().y - 5.0),
            (radius_mm * 0.9).clamp(1.5, 11.0),
            egui::Stroke::new(1.5_f32, text_color),
        );
        painter.text(
            egui::pos2(rect.center().x, rect.max.y - 7.5),
            egui::Align2::CENTER_CENTER,
            &label,
            egui::FontId::proportional(10.0),
            text_color,
        );
    }
    response.on_hover_text(t!("Radiergröße {size} setzen", size = label))
}
/// Millimeters without trailing zeros: 3, 1.5, 2.25.
pub(super) fn format_mm(mm: f32) -> String {
    let text = format!("{:.2}", mm);
    text.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// Inhalt eines Symbol-Knopfs der Werkzeugleiste.
#[derive(Clone, Copy)]
pub(super) enum SidebarIcon<'a> {
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
    /// Filled circle with this radius in points (eraser size)
    Dot(f32),
}
/// Quadratischer Symbol-Knopf der Werkzeugleiste mit optionalem Farbbalken an der Unterkante.
pub(super) fn sidebar_button(
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
        painter.rect_filled(rect, egui::CornerRadius::same(8), bg);
        if is_active {
            painter.rect_stroke(
                rect,
                egui::CornerRadius::same(8),
                egui::Stroke::new(
                    1.0_f32,
                    egui::Color32::from_rgba_unmultiplied(110, 165, 255, 190),
                ),
                egui::StrokeKind::Middle,
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
            SidebarIcon::Dot(radius) => {
                painter.circle_filled(c, radius, color);
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
            painter.rect_filled(bar, egui::CornerRadius::same(2), color32_from(ind));
        }
    }

    if tooltip.is_empty() {
        response
    } else {
        response.on_hover_text(tooltip)
    }
}
/// Waagerechte Trennlinie zwischen Gruppen der Werkzeugleiste.
pub(super) fn sidebar_divider(ui: &mut egui::Ui, button_size: f32) {
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(button_size - 12.0, UiState::SIDEBAR_DIVIDER),
        egui::Sense::hover(),
    );
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
pub(super) fn color32_from(color: [f32; 4]) -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(
        (color[0].clamp(0.0, 1.0) * 255.0) as u8,
        (color[1].clamp(0.0, 1.0) * 255.0) as u8,
        (color[2].clamp(0.0, 1.0) * 255.0) as u8,
        ((color[3].clamp(0.0, 1.0) * 255.0) as u8).max(180),
    )
}
/// Position und Ankerpunkt eines Popovers neben einem Knopf der Werkzeugleiste,
/// immer zur Blattseite hin geöffnet.
pub(super) fn popover_placement(
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
pub(super) fn autosave_status_style(status: AutosaveStatus) -> (egui::Color32, &'static str) {
    match status {
        AutosaveStatus::Saved => (
            egui::Color32::from_rgb(46, 204, 113),
            t!("Alles gespeichert"),
        ),
        AutosaveStatus::Saving => (egui::Color32::from_rgb(241, 196, 15), t!("Speichert …")),
        AutosaveStatus::Pending => (
            egui::Color32::from_rgb(230, 126, 34),
            t!("Ungespeicherte Änderungen"),
        ),
        AutosaveStatus::Error => (egui::Color32::from_rgb(231, 76, 60), t!("Speicherfehler!")),
    }
}
/// Zeichnet einen runden Farbauswahl-Knopf (Color Swatch) mit anpassbarem Durchmesser.
/// Farbwahl in den Popovern: Vorgaben als Farbpunkte, dahinter 🎨 für einen frei
/// wählbaren Farbton (aufklappbar). Die Deckkraft von `color` bleibt erhalten (Textmarker
/// halbtransparent). `true`, wenn sich die Farbe geändert hat.
pub(super) fn color_editor(
    ui: &mut egui::Ui,
    presets: &[ColorPreset],
    saved: &mut Vec<[f32; 3]>,
    color: &mut [f32; 4],
    picker_open: &mut bool,
) -> bool {
    let same_rgb = |a: &[f32], b: &[f32]| {
        a[..3]
            .iter()
            .zip(&b[..3])
            .all(|(a, b)| (a - b).abs() < 0.01)
    };
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(8.0, 0.0);
        for &(preset, label) in presets {
            let is_sel = same_rgb(&preset, color);
            if color_circle_swatch(ui, preset, is_sel, crate::i18n::tr(label), 30.0).clicked() {
                *color = [preset[0], preset[1], preset[2], color[3]];
                changed = true;
            }
        }
        if ui
            .add(egui::Button::selectable(
                *picker_open,
                egui::RichText::new("🎨").size(18.0),
            ))
            .on_hover_text(t!("Eigene Farbe wählen"))
            .clicked()
        {
            *picker_open = !*picker_open;
        }
    });

    // My colors (Issue #56): kept for pen, highlighter and text alike
    ui.add_space(6.0);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(8.0, 6.0);
        let mut remove = None;
        for (i, rgb) in saved.iter().enumerate() {
            let swatch = [rgb[0], rgb[1], rgb[2], 1.0];
            let is_sel = same_rgb(rgb, color);
            if color_circle_swatch(ui, swatch, is_sel, t!("Meine Farbe"), 30.0).clicked() {
                *color = [rgb[0], rgb[1], rgb[2], color[3]];
                changed = true;
            }
            if is_sel {
                remove = Some(i);
            }
        }
        if let Some(i) = remove {
            if ui
                .button(egui::RichText::new("🗑").size(16.0))
                .on_hover_text(t!("Diese Farbe aus „Meine Farben“ entfernen"))
                .clicked()
            {
                saved.remove(i);
            }
        } else if saved.len() < MAX_SAVED_COLORS {
            let label = if saved.is_empty() {
                t!("➕ Farbe merken")
            } else {
                "➕"
            };
            if ui
                .button(label)
                .on_hover_text(t!("Die aktuelle Farbe zu „Meine Farben“ hinzufügen"))
                .clicked()
            {
                saved.push([color[0], color[1], color[2]]);
            }
        }
    });
    if *picker_open {
        ui.add_space(6.0);
        let [r, g, b] = [0, 1, 2].map(|i| (color[i].clamp(0.0, 1.0) * 255.0).round() as u8);
        let mut picked = egui::Color32::from_rgb(r, g, b);
        let picked_changed = ui
            .scope(|ui| {
                ui.spacing_mut().slider_width = 170.0;
                egui::color_picker::color_picker_color32(
                    ui,
                    &mut picked,
                    egui::color_picker::Alpha::Opaque,
                )
            })
            .inner;
        if picked_changed {
            let [r, g, b, _] = picked.to_array();
            *color = [
                r as f32 / 255.0,
                g as f32 / 255.0,
                b as f32 / 255.0,
                color[3],
            ];
            changed = true;
        }
    }
    changed
}
/// Stift der Werkzeugleiste: ein schräg liegender Stift in seiner Farbe; die Stärke steht
/// im Tooltip.
pub(super) fn pen_preset_swatch(
    ui: &mut egui::Ui,
    preset: PenPreset,
    is_selected: bool,
    tooltip: &str,
    size: f32,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::click());
    if ui.is_rect_visible(rect) {
        // Schräg liegender Stift ganz in seiner Farbe, damit die Stifte auf einen Blick zu
        // unterscheiden sind (Issue #31); die helle Kontur hält dunkle Farben sichtbar
        let [r, g, b, _] = preset.color.map(|c| (c.clamp(0.0, 1.0) * 255.0) as u8);
        let ink = egui::Color32::from_rgb(r, g, b);
        let c = rect.center();
        let dir = egui::vec2(1.0, -1.0).normalized();
        let perp = egui::vec2(dir.y, -dir.x);
        let half_len = size * 0.44;
        let half_w = size * 0.17;
        let p = |along: f32, across: f32| c + dir * along + perp * across;
        let tip_start = -half_len * 0.3;
        let lead_start = -half_len * 0.72;
        let lead_w = half_w * (lead_start + half_len) / (tip_start + half_len);
        let outline = egui::Stroke::new(1.0_f32, egui::Color32::from_gray(200));
        let painter = ui.painter();
        painter.add(egui::Shape::convex_polygon(
            vec![
                p(tip_start, -half_w),
                p(half_len, -half_w),
                p(half_len, half_w),
                p(tip_start, half_w),
            ],
            ink,
            outline,
        ));
        painter.add(egui::Shape::convex_polygon(
            vec![
                p(-half_len, 0.0),
                p(tip_start, -half_w),
                p(tip_start, half_w),
            ],
            egui::Color32::from_rgb(235, 210, 170),
            outline,
        ));
        painter.add(egui::Shape::convex_polygon(
            vec![
                p(-half_len, 0.0),
                p(lead_start, -lead_w),
                p(lead_start, lead_w),
            ],
            ink,
            egui::Stroke::NONE,
        ));
        if is_selected {
            ui.painter().rect_stroke(
                rect.shrink(0.5),
                egui::CornerRadius::same(7),
                egui::Stroke::new(2.0_f32, egui::Color32::WHITE),
                egui::StrokeKind::Middle,
            );
        } else if response.hovered() {
            ui.painter().rect_stroke(
                rect.shrink(1.0),
                egui::CornerRadius::same(7),
                egui::Stroke::new(1.0_f32, egui::Color32::from_white_alpha(150)),
                egui::StrokeKind::Middle,
            );
        }
    }
    response.on_hover_text(tooltip)
}
pub(super) fn color_circle_swatch(
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
pub(super) const PAGE_PICKER_COLUMNS: usize = 4;
/// Breite einer Kachel der Seitenauswahl; darunter kommt eine Zeile für die Seitenzahl.
pub(super) const PAGE_TILE_WIDTH: f32 = 66.0;
pub(super) const PAGE_TILE_LABEL: f32 = 18.0;
/// Knöpfe für Schriftart und -größe des Text-Werkzeugs. Änderungen gelten auch für das
/// gerade bearbeitete Textfeld.
pub(super) fn text_style_buttons(ui: &mut egui::Ui, btn: f32, state: &mut UiState) {
    let size = state.text_size_pt;
    let smaller = TEXT_SIZES_PT
        .iter()
        .rev()
        .copied()
        .find(|&s| s < size - 0.01);
    let larger = TEXT_SIZES_PT.iter().copied().find(|&s| s > size + 0.01);
    let smaller_tip = t!("Kleinere Schrift (jetzt {size} pt)", size = size);
    let larger_tip = t!("Größere Schrift (jetzt {size} pt)", size = size);
    enum Change {
        Font(TextFont),
        /// Neue Größe; `None`: keine kleinere bzw. größere mehr
        Size(Option<f32>),
    }
    let buttons = [
        (
            SidebarIcon::Text("Aa"),
            t!("Normale Schrift"),
            Change::Font(TextFont::Sans),
        ),
        (
            SidebarIcon::Text("{ }"),
            t!("Code-Schrift (feste Zeichenbreite, Tab rückt ein)"),
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
                state.set_export_message(t!("Schriftgröße {size} pt", size = new_size));
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
/// Auswahl der Seitenanordnung eines Notizbuchs. Ohne `can_cross` lässt sich nicht zwischen
/// Seiten und endloser Fläche wechseln; ohne `offer_paged` fehlen feste Seiten (beim Anlegen,
/// Issue #30 – umstellen geht im ☰-Menü). `true`, wenn sie geändert wurde.
pub(super) fn layout_picker(
    ui: &mut egui::Ui,
    layout: &mut NotebookLayout,
    can_cross: bool,
    offer_paged: bool,
) -> bool {
    let mut changed = false;
    let endless_now = *layout == NotebookLayout::Endless;
    ui.horizontal_wrapped(|ui| {
        for (option, label, tooltip) in [
            (
                NotebookLayout::Paged,
                t!("📄 Feste Seiten"),
                t!("Seite für Seite blättern"),
            ),
            (
                NotebookLayout::Continuous,
                t!("📜 Durchgehend"),
                t!("Seiten hängen untereinander, Mausrad und Finger scrollen \
                 (Strg+Mausrad zoomt); wer unten auf der letzten Seite schreibt, \
                 bekommt automatisch eine neue Seite."),
            ),
            (
                NotebookLayout::Endless,
                t!("♾ Endlos"),
                t!(
                    "Eine Fläche ohne Rand: in alle Richtungen schreiben, verschieben und zoomen. \
                 Wechsel nur bei einer einzigen Seite ohne PDF bzw. zurück, solange alles im \
                 Startausschnitt liegt."
                ),
            ),
        ] {
            if option == NotebookLayout::Paged && !offer_paged {
                continue;
            }
            let enabled = can_cross || (option == NotebookLayout::Endless) == endless_now;
            if ui
                .add_enabled(enabled, egui::Button::selectable(*layout == option, label))
                .on_hover_text(tooltip)
                .on_disabled_hover_text(tooltip)
                .clicked()
                && *layout != option
            {
                *layout = option;
                changed = true;
            }
        }
    });
    changed
}
/// Auswahlknopf für eine Papierfarbe: Farbmuster und Name.
pub(super) fn paper_theme_button(
    ui: &mut egui::Ui,
    theme: PaperTheme,
    selected: bool,
) -> egui::Response {
    let response = ui.add_sized(
        [115.0, 28.0],
        egui::Button::selectable(selected, format!("     {}", theme.label())),
    );
    let swatch = egui::Rect::from_center_size(
        egui::pos2(response.rect.min.x + 14.0, response.rect.center().y),
        egui::vec2(14.0, 14.0),
    );
    ui.painter().rect_filled(
        swatch,
        egui::CornerRadius::same(3),
        linear_color32(theme.paper_color()),
    );
    ui.painter().rect_stroke(
        swatch,
        egui::CornerRadius::same(3),
        egui::Stroke::new(1.0_f32, egui::Color32::from_gray(120)),
        egui::StrokeKind::Middle,
    );
    response
}
/// Knopf für eine Musterfarbe: Vorschau (kleines Karo auf der aktuellen Papierfarbe) und
/// Bezeichnung.
pub(super) fn pattern_color_button(
    ui: &mut egui::Ui,
    pattern: PatternColor,
    paper: PaperTheme,
    selected: bool,
) -> egui::Response {
    let response = ui.add_sized(
        [82.0, 28.0],
        egui::Button::selectable(selected, format!("      {}", pattern.label())),
    );
    let swatch = egui::Rect::from_center_size(
        egui::pos2(response.rect.min.x + 15.0, response.rect.center().y),
        egui::vec2(16.0, 16.0),
    );
    let painter = ui.painter();
    painter.rect_filled(
        swatch,
        egui::CornerRadius::same(3),
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
        egui::CornerRadius::same(3),
        egui::Stroke::new(1.0_f32, egui::Color32::from_gray(120)),
        egui::StrokeKind::Middle,
    );
    response.on_hover_text(t!("Gilt für alle Seiten, auch beim Export"))
}
/// Schieberegler für die Druckkurve mit Vorschau der Kennlinie. Änderungen gelten sofort
/// (`state.pressure_curve`); zurückgegeben wird die Kurve, sobald sie gespeichert werden soll
/// (Regler losgelassen oder per Tastatur geändert).
/// Belegung der Tasten am Stift. Tasten erscheinen, sobald sie einmal gedrückt wurden; die
/// gerade gedrückte ist markiert. `true`, wenn sich etwas geändert hat.
pub(super) fn stylus_buttons_editor(ui: &mut egui::Ui, state: &mut UiState) -> bool {
    use crate::settings::StylusAction;
    let mut changed = false;
    ui.add_space(4.0);
    ui.label(egui::RichText::new(t!("Tasten am Stift")).color(egui::Color32::from_gray(200)));
    if state.stylus_buttons.is_empty() {
        ui.label(
            egui::RichText::new(t!(
                "Drücke eine Taste am Stift (über dem Bildschirm) – sie erscheint dann hier. \
                 Bis dahin radieren alle Tasten."
            ))
            .size(11.0)
            .color(egui::Color32::from_gray(150)),
        );
        return false;
    }
    let mut remove = None;
    for i in 0..state.stylus_buttons.len() {
        ui.horizontal(|ui| {
            let pressed = state.pressed_stylus_button == Some(i);
            let label = egui::RichText::new(t!("Taste {number}", number = i + 1));
            ui.label(if pressed {
                label.strong().color(egui::Color32::from_rgb(46, 204, 113))
            } else {
                label
            })
            .on_hover_text(t!("Leuchtet grün, solange die Taste gedrückt ist"));
            let current = state.stylus_buttons[i].action;
            egui::ComboBox::from_id_salt(("stylus_button", i))
                .selected_text(current.label())
                .width(130.0)
                .show_ui(ui, |ui| {
                    for action in StylusAction::ALL {
                        if ui
                            .selectable_label(action == current, action.label())
                            .clicked()
                            && action != current
                        {
                            state.stylus_buttons[i].action = action;
                            changed = true;
                        }
                    }
                });
            if ui
                .small_button("🗙")
                .on_hover_text(t!(
                    "Vergessen (erscheint beim nächsten Drücken wieder, dann mit Radieren)"
                ))
                .clicked()
            {
                remove = Some(i);
            }
        });
    }
    if let Some(i) = remove {
        state.stylus_buttons.remove(i);
        state.pressed_stylus_button = None;
        changed = true;
    }
    changed
}
pub(super) fn pressure_curve_editor(
    ui: &mut egui::Ui,
    state: &mut UiState,
) -> Option<PressureCurve> {
    let mut save = false;
    let mut track = |response: egui::Response| {
        if response.drag_stopped() || (response.changed() && !response.dragged()) {
            save = true;
        }
    };

    ui.add_space(4.0);
    ui.label(t!("Druckkurve des Stifts"));
    let (graph, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 56.0), egui::Sense::hover());
    let painter = ui.painter_at(graph);
    painter.rect_filled(
        graph,
        egui::CornerRadius::same(4),
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
        ui.label(t!("weich"));
        ui.spacing_mut().slider_width = (ui.available_width() - 40.0).max(60.0);
        track(ui.add(
            egui::Slider::new(&mut state.pressure_curve.exponent, PressureCurve::EXPONENT_RANGE)
                .logarithmic(true)
                .show_value(false),
        ).on_hover_text(t!("Weich: schon leichter Druck ergibt kräftige Linien.\nHart: erst fester Druck ergibt kräftige Linien.")));
        ui.label(t!("hart"));
    });
    ui.horizontal(|ui| {
        ui.label(t!("Mindestdruck"));
        ui.spacing_mut().slider_width = (ui.available_width() - 60.0).max(60.0);
        track(ui.add(
            egui::Slider::new(&mut state.pressure_curve.threshold, PressureCurve::THRESHOLD_RANGE)
                .custom_formatter(|v, _| format!("{:.0} %", v * 100.0))
                .custom_parser(|s| s.trim().trim_end_matches('%').trim().parse::<f64>().ok().map(|v| v / 100.0)),
        ).on_hover_text(t!("Leichterer Druck zählt als kein Druck (dünnste Linie).\nHilft bei Stiften, die schon beim Schweben Druck melden.")));
    });
    if ui
        .add_enabled(
            state.pressure_curve != PressureCurve::default(),
            egui::Button::new(t!("Druckkurve zurücksetzen")),
        )
        .clicked()
    {
        state.pressure_curve = PressureCurve::default();
        save = true;
    }
    save.then_some(state.pressure_curve)
}
/// Rahmen der Popover neben der Werkzeugleiste.
pub(super) fn popover_frame() -> egui::Frame {
    egui::Frame::NONE
        .fill(egui::Color32::from_rgb(30, 30, 34))
        .corner_radius(egui::CornerRadius::same(12))
        .stroke(egui::Stroke::new(
            1.0_f32,
            egui::Color32::from_white_alpha(40),
        ))
        .inner_margin(egui::Margin::symmetric(14, 12))
        .shadow(egui::epaint::Shadow {
            offset: [0, 6],
            blur: 16,
            spread: 0,
            color: egui::Color32::from_black_alpha(120),
        })
}
/// Deckender, einheitlicher Rahmen für Dialogfenster (`danger` = rote Warnfenster).
pub(super) fn dialog_frame(danger: bool) -> egui::Frame {
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
    egui::Frame::NONE
        .fill(fill)
        .corner_radius(egui::CornerRadius::same(14))
        .stroke(stroke)
        .inner_margin(egui::Margin::symmetric(20, 16))
        .shadow(egui::epaint::Shadow {
            offset: [0, 8],
            blur: 24,
            spread: 0,
            color: egui::Color32::from_black_alpha(160),
        })
}
/// Dialogfenster ohne egui-Titelleiste (der Titel steht in [`dialog_header`]), zentriert über
/// dem freien Bereich neben der Werkzeugleiste. Warnfenster sitzen in der Bildschirmmitte.
pub(super) fn dialog_window(
    title: &'static str,
    state: &UiState,
    danger: bool,
) -> egui::Window<'static> {
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
        .id(egui::Id::unique(title))
        .title_bar(false)
        .collapsible(false)
        .resizable(false)
        .anchor(align, egui::vec2(shift, y))
        .frame(dialog_frame(danger))
}
/// Content width of a dialog: `wanted`, but at most what fits beside the toolbar, so that on a
/// phone the dialog is neither hidden under the toolbar nor cut off at the edge (Issue #50).
pub(super) fn dialog_width(ctx: &egui::Context, state: &UiState, wanted: f32) -> f32 {
    // Frame margins (20 each side) and a gap to the toolbar and the screen edge
    let free = ctx.viewport_rect().width()
        - state.sidebar_reserved_width()
        - 2.0 * 20.0
        - 2.0 * UiState::SIDEBAR_MARGIN;
    wanted.min(free).max(200.0)
}
/// Kopfzeile eines Dialogs mit Titel und optionalem Schließen-Knopf.
/// Gibt `true` zurück, wenn „Schließen“ getippt wurde.
pub(super) fn dialog_header(ui: &mut egui::Ui, title: &str, closable: bool) -> bool {
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
pub(super) fn close_button(ui: &mut egui::Ui) -> egui::Response {
    ui.add_sized(
        [36.0, 36.0],
        egui::Button::new(egui::RichText::new("🗙").size(16.0)).frame(false),
    )
    .on_hover_text(t!("Schließen (Esc)"))
}
/// Größere Knöpfe, Eingabefelder und Schieberegler für Finger und Stift.
pub(super) fn touch_friendly_spacing(ui: &mut egui::Ui) {
    let spacing = ui.spacing_mut();
    spacing.button_padding = egui::vec2(12.0, 8.0);
    spacing.interact_size = egui::vec2(40.0, 36.0);
    spacing.item_spacing = egui::vec2(8.0, 8.0);
    spacing.slider_width = 200.0;
}
/// "How long ago" for the last sync.
pub(super) fn elapsed_label(at: std::time::SystemTime) -> String {
    let secs = at.elapsed().map_or(0, |d| d.as_secs());
    if secs < 60 {
        t!("gerade eben").to_string()
    } else if secs < 3600 {
        t!("vor {minutes} min", minutes = secs / 60)
    } else {
        t!("vor {hours} h", hours = secs / 3600)
    }
}
