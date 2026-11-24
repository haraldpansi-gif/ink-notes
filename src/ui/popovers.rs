//! Schwebende Popover-Menüs (Stift, Marker, Radierer, Textfarbe, ☰-Menü, Seitenauswahl).

use crate::autosave::AutosaveStatus;
use crate::background::{PaperTheme, PatternColor};
use crate::document::{Notebook, PageBackground};
use crate::settings::SidebarSide;
use crate::storage::NotebookLayout;
use crate::t;

use super::overlays::{page_thumbnail, page_tile_size, page_title_list};
use super::sidebar::SidebarAnchors;
use super::widgets::{
    autosave_status_style, close_button, color_editor, dialog_header, elapsed_label,
    eraser_size_button, layout_picker, paper_theme_button, pattern_color_button, popover_frame,
    popover_placement, pressure_curve_editor, stroke_preview_button, stylus_buttons_editor,
    touch_friendly_spacing, PAGE_PICKER_COLUMNS, PAGE_TILE_WIDTH,
};
use super::{
    ActiveTool, PageExportKind, UiAction, UiState, ERASER_DIAMETER_RANGE_MM, ERASER_RADII_MM,
    HIGHLIGHTER_COLOR_PRESETS, HIGHLIGHTER_WIDTH_PRESETS, PEN_COLOR_PRESETS, PEN_WIDTH_PRESETS,
};

pub(super) fn render_popovers(
    ctx: &egui::Context,
    state: &mut UiState,
    notebook: &Notebook,
    autosave_status: AutosaveStatus,
    updater: &crate::updater::UpdaterController,
    anchors: &SidebarAnchors,
    actions: &mut Vec<UiAction>,
) {
    // 2. SCHWEBENDES POPOVER-MENÜ (FÜR STIFT ODER TEXTMARKER)
    let mut popover_rect = None;

    if state.is_pen_menu_open {
        if let Some(btn_rect) = anchors.pen_btn_rect {
            let (popover_pos, pivot) =
                popover_placement(state.sidebar_side, anchors.toolbar_rect, btn_rect);
            let pop_response = egui::Area::new(egui::Id::unique("pen_settings_popover"))
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
                                egui::RichText::new(t!(
                                    "Stift {number}",
                                    number = state.active_pen + 1
                                ))
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
                            egui::RichText::new(t!("FARBE"))
                                .size(10.5)
                                .strong()
                                .color(egui::Color32::from_gray(160)),
                        );
                        ui.add_space(3.0);
                        if color_editor(
                            ui,
                            &PEN_COLOR_PRESETS,
                            &mut state.saved_colors,
                            &mut state.pen_color,
                            &mut state.show_color_picker,
                        ) {
                            actions.push(UiAction::SetColor(state.pen_color));
                        }

                        ui.add_space(10.0);

                        // Zeile 2: Dicken-Presets mit visueller Linien-Vorschau
                        ui.label(
                            egui::RichText::new(t!("DICKEN-PRESETS"))
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
                            egui::RichText::new(t!("STUFENLOSE STÄRKE"))
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

                        ui.add_space(10.0);

                        // Zeile 4: Linienstil (durchgezogen, gestrichelt, gepunktet) & Form-Füllung
                        ui.label(
                            egui::RichText::new(t!("LINIENSTIL"))
                                .size(10.5)
                                .strong()
                                .color(egui::Color32::from_gray(160)),
                        );
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            use crate::document::LineStyle;
                            for (style, icon, tip) in [
                                (LineStyle::Solid, "━━━━", t!("Durchgezogen")),
                                (LineStyle::Dashed, "╌ ╌ ╌", t!("Gestrichelt")),
                                (LineStyle::Dotted, "⋯⋯⋯", t!("Gepunktet")),
                            ] {
                                if ui
                                    .add_sized(
                                        [92.0, 30.0],
                                        egui::Button::selectable(state.line_style == style, icon),
                                    )
                                    .on_hover_text(tip)
                                    .clicked()
                                {
                                    state.line_style = style;
                                    actions.push(UiAction::SetLineStyle(style));
                                }
                            }
                        });
                        ui.add_space(6.0);
                        if ui
                            .checkbox(&mut state.shape_fill, t!("Erkannte Formen füllen"))
                            .on_hover_text(t!(
                                "Geschlossene Formen (Kreis, Rechteck, Dreieck) halbtransparent füllen"
                            ))
                            .changed()
                        {
                            actions.push(UiAction::SetShapeFill(state.shape_fill));
                        }
                    });

                    frame.response.rect
                });

            popover_rect = Some(pop_response.inner);
        }
    } else if state.is_eraser_menu_open && state.active_tool == ActiveTool::Eraser {
        if let Some(btn_rect) = anchors.eraser_btn_rect {
            let (popover_pos, pivot) =
                popover_placement(state.sidebar_side, anchors.toolbar_rect, btn_rect);
            let pop_response = egui::Area::new(egui::Id::unique("eraser_settings_popover"))
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
                                egui::RichText::new(t!("Radierer"))
                                    .strong()
                                    .color(egui::Color32::WHITE)
                                    .size(12.5),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if close_button(ui).clicked() {
                                        state.is_eraser_menu_open = false;
                                    }
                                },
                            );
                        });

                        ui.add_space(8.0);
                        ui.label(
                            egui::RichText::new(t!("GRÖSSE"))
                                .size(10.5)
                                .strong()
                                .color(egui::Color32::from_gray(160)),
                        );
                        ui.add_space(3.0);
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing = egui::vec2(6.0, 0.0);
                            for radius in ERASER_RADII_MM {
                                let is_sel = (state.eraser_radius_mm - radius).abs() < 0.05;
                                if eraser_size_button(ui, is_sel, radius).clicked() {
                                    state.eraser_radius_mm = radius;
                                }
                            }
                        });

                        ui.add_space(10.0);
                        ui.label(
                            egui::RichText::new(t!("STUFENLOSE GRÖSSE"))
                                .size(10.5)
                                .strong()
                                .color(egui::Color32::from_gray(160)),
                        );
                        ui.add_space(2.0);
                        let mut diameter = state.eraser_radius_mm * 2.0;
                        if ui
                            .add(
                                egui::Slider::new(
                                    &mut diameter,
                                    ERASER_DIAMETER_RANGE_MM.0..=ERASER_DIAMETER_RANGE_MM.1,
                                )
                                .suffix(" mm")
                                .step_by(0.5),
                            )
                            .changed()
                        {
                            state.set_eraser_radius_mm(diameter / 2.0);
                        }
                    });

                    frame.response.rect
                });

            popover_rect = Some(pop_response.inner);
        }
    } else if state.is_highlighter_menu_open {
        if let Some(btn_rect) = anchors.high_btn_rect {
            let (popover_pos, pivot) =
                popover_placement(state.sidebar_side, anchors.toolbar_rect, btn_rect);
            let pop_response = egui::Area::new(egui::Id::unique("highlighter_settings_popover"))
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
                                egui::RichText::new(t!(
                                    "Textmarker – Farbe {number}",
                                    number = state.active_highlighter + 1
                                ))
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
                            egui::RichText::new(t!("FARBE"))
                                .size(10.5)
                                .strong()
                                .color(egui::Color32::from_gray(160)),
                        );
                        ui.add_space(3.0);
                        if color_editor(
                            ui,
                            &HIGHLIGHTER_COLOR_PRESETS,
                            &mut state.saved_colors,
                            &mut state.highlighter_color,
                            &mut state.show_color_picker,
                        ) {
                            actions.push(UiAction::SetColor(state.highlighter_color));
                        }

                        ui.add_space(10.0);

                        // Zeile 2: Dicken-Presets
                        ui.label(
                            egui::RichText::new(t!("DICKEN-PRESETS"))
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
                            egui::RichText::new(t!("STUFENLOSE STÄRKE"))
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
    } else if state.is_text_color_menu_open {
        if let Some(btn_rect) = anchors.text_color_btn_rect {
            let (popover_pos, pivot) =
                popover_placement(state.sidebar_side, anchors.toolbar_rect, btn_rect);
            let pop_response = egui::Area::new(egui::Id::unique("text_color_popover"))
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
                                egui::RichText::new(t!(
                                    "Textfarbe {number}",
                                    number = state.active_text_color + 1
                                ))
                                .strong()
                                .color(egui::Color32::WHITE)
                                .size(12.5),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if close_button(ui).clicked() {
                                        state.is_text_color_menu_open = false;
                                    }
                                },
                            );
                        });
                        ui.add_space(8.0);
                        let slot = state.active_text_color.min(state.text_colors.len() - 1);
                        let mut color = state.text_colors[slot];
                        if color_editor(
                            ui,
                            &PEN_COLOR_PRESETS,
                            &mut state.saved_colors,
                            &mut color,
                            &mut state.show_color_picker,
                        ) {
                            state.text_colors[slot] = color;
                            state.set_text_color(color);
                            actions.push(UiAction::SetColor(color));
                        }
                    });
                    frame.response.rect
                });
            popover_rect = Some(pop_response.inner);
        } else {
            // Die Farbe ist nicht mehr zu sehen (anderes Werkzeug)
            state.is_text_color_menu_open = false;
        }
    }

    // 2a. STEMPEL-POPOVER (wiederverwendbare Elemente)
    if state.is_stamps_menu_open {
        if let Some(btn_rect) = anchors.stamps_btn_rect {
            let (popover_pos, pivot) =
                popover_placement(state.sidebar_side, anchors.toolbar_rect, btn_rect);
            let mut close_after = false;
            let pop_response = egui::Area::new(egui::Id::unique("stamps_popover"))
                .fixed_pos(popover_pos)
                .pivot(pivot)
                .constrain(true)
                .order(egui::Order::Foreground)
                .show(ctx, |ui| {
                    let frame = popover_frame().show(ui, |ui| {
                        ui.set_width(310.0);
                        touch_friendly_spacing(ui);

                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(t!("Stempel"))
                                    .strong()
                                    .color(egui::Color32::WHITE)
                                    .size(13.0),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if close_button(ui).clicked() {
                                        close_after = true;
                                    }
                                },
                            );
                        });

                        ui.add_space(6.0);

                        if state.selection_aspect_lock.is_some() {
                            ui.horizontal(|ui| {
                                let edit = ui.add_sized(
                                    egui::vec2(175.0, 28.0),
                                    egui::TextEdit::singleline(&mut state.stamp_name_input)
                                        .hint_text(t!("Name (optional)")),
                                );
                                let enter = edit.lost_focus()
                                    && ui.input(|i| i.key_pressed(egui::Key::Enter));
                                if ui
                                    .button(t!("Speichern"))
                                    .on_hover_text(t!("Auswahl als Stempel speichern"))
                                    .clicked()
                                    || enter
                                {
                                    let name = std::mem::take(&mut state.stamp_name_input);
                                    let opt = (!name.trim().is_empty()).then_some(name);
                                    actions.push(UiAction::SaveSelectionAsStamp(opt));
                                }
                            });
                            ui.add_space(6.0);
                        }

                        if state.stamps.stamps.is_empty() {
                            ui.label(
                                egui::RichText::new(t!(
                                    "Wähle Objekte mit dem Lasso aus und speichere sie als Stempel."
                                ))
                                .size(12.0)
                                .color(egui::Color32::from_gray(175)),
                            );
                        } else {
                            let stamps_snapshot: Vec<_> = state.stamps.stamps.clone();
                            let paper = state.paper_theme;
                            let adapt = state.adapt_ink_colors;
                            egui::ScrollArea::vertical()
                                .id_salt("stamps_popover_scroll")
                                .max_height(320.0)
                                .show(ui, |ui| {
                                    egui::Grid::new("stamps_grid")
                                        .num_columns(3)
                                        .spacing(egui::vec2(8.0, 8.0))
                                        .show(ui, |ui| {
                                            for (idx, stamp) in stamps_snapshot.iter().enumerate() {
                                                ui.vertical(|ui| {
                                                    ui.set_width(90.0);
                                                    let (rect, resp) = ui.allocate_exact_size(
                                                        egui::vec2(90.0, 62.0),
                                                        egui::Sense::click(),
                                                    );
                                                    let shapes =
                                                        crate::stamps::stamp_thumbnail_shapes(
                                                            stamp, rect, paper, adapt,
                                                        );
                                                    ui.painter().extend(shapes);
                                                    let stroke_color = if resp.hovered() {
                                                        egui::Color32::from_rgb(120, 175, 255)
                                                    } else {
                                                        egui::Color32::from_rgba_unmultiplied(
                                                            255, 255, 255, 45,
                                                        )
                                                    };
                                                    ui.painter().rect_stroke(
                                                        rect,
                                                        egui::CornerRadius::same(4),
                                                        egui::Stroke::new(1.2, stroke_color),
                                                        egui::StrokeKind::Inside,
                                                    );
                                                    if resp
                                                        .on_hover_text(t!(
                                                            "Stempel '{name}' einfügen",
                                                            name = stamp.name
                                                        ))
                                                        .clicked()
                                                    {
                                                        actions.push(UiAction::InsertStamp(
                                                            stamp.id.clone(),
                                                        ));
                                                        close_after = true;
                                                    }

                                                    ui.horizontal(|ui| {
                                                        ui.spacing_mut().item_spacing.x = 2.0;
                                                        let short = super::tabs::truncate_tab_title(
                                                            &stamp.name,
                                                            10,
                                                        );
                                                        ui.label(
                                                            egui::RichText::new(short)
                                                                .size(11.0)
                                                                .color(egui::Color32::from_gray(
                                                                    220,
                                                                )),
                                                        )
                                                        .on_hover_text(&stamp.name);
                                                        ui.with_layout(
                                                            egui::Layout::right_to_left(
                                                                egui::Align::Center,
                                                            ),
                                                            |ui| {
                                                                if ui
                                                                    .add(
                                                                        egui::Button::new(
                                                                            egui::RichText::new(
                                                                                "🗑",
                                                                            )
                                                                            .size(11.0),
                                                                        )
                                                                        .frame(false),
                                                                    )
                                                                    .on_hover_text(t!(
                                                                        "Stempel löschen"
                                                                    ))
                                                                    .clicked()
                                                                {
                                                                    actions.push(
                                                                        UiAction::DeleteStamp(
                                                                            stamp.id.clone(),
                                                                        ),
                                                                    );
                                                                }
                                                            },
                                                        );
                                                    });
                                                });
                                                if (idx + 1) % 3 == 0 {
                                                    ui.end_row();
                                                }
                                            }
                                        });
                                });
                        }
                    });
                    frame.response.rect
                });
            popover_rect = Some(pop_response.inner);
            if close_after {
                state.is_stamps_menu_open = false;
            }
        }
    }

    // 2b. ☰-MENÜ (selten genutzte Funktionen), öffnet sich nach oben zur Blattseite hin
    if state.is_more_menu_open {
        if let Some(btn_rect) = anchors.more_btn_rect {
            let (mut pos, pivot) =
                popover_placement(state.sidebar_side, anchors.toolbar_rect, btn_rect);
            pos.y = btn_rect.max.y;
            let pivot = match pivot {
                egui::Align2::RIGHT_TOP => egui::Align2::RIGHT_BOTTOM,
                _ => egui::Align2::LEFT_BOTTOM,
            };
            let pop_response = egui::Area::new(egui::Id::unique("more_menu_popover"))
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
                                    egui::RichText::new(t!("Weitere Funktionen"))
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

                            ui.add_space(4.0);
                            if ui
                                .add_sized(
                                    [ui.available_width(), 32.0],
                                    egui::Button::new(t!("🔍 Suchen (Strg+F)")),
                                )
                                .clicked()
                            {
                                state.close_overlays();
                                state.is_search_open = true;
                                state.search_focus = true;
                                if !state.search_query.trim().is_empty() {
                                    actions.push(UiAction::RunSearch);
                                }
                            }

                            // Bei kleinen Fenstern scrollen statt abschneiden
                            egui::ScrollArea::vertical()
                                .max_height((ctx.viewport_rect().height() - 90.0).max(150.0))
                                .show(ui, |ui| {
                            section(ui, t!("DOKUMENT"));
                            if ui
                                .add_sized([ui.available_width(), 30.0], egui::Button::new(t!("📄  Als PDF exportieren (Strg+E)")))
                                .clicked()
                            {
                                actions.push(UiAction::ExportPdf);
                                state.is_more_menu_open = false;
                            }
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new(t!("Seite als")).color(egui::Color32::from_gray(190)));
                                for (label, kind, hint) in [
                                    ("PNG", PageExportKind::Png, t!("Bild mit Papier und Raster (200 dpi)")),
                                    ("PNG transp.", PageExportKind::PngTransparent, t!("Bild mit transparentem Hintergrund, nur Inhalt")),
                                    ("SVG", PageExportKind::Svg, t!("Vektorgrafik, z. B. für Inkscape oder LaTeX")),
                                ] {
                                    if ui.button(label).on_hover_text(hint).clicked() {
                                        actions.push(UiAction::ExportPage(kind));
                                        state.is_more_menu_open = false;
                                    }
                                }
                            });
                            if ui
                                .add_enabled_ui(!state.endless, |ui| {
                                    ui.add_sized([ui.available_width(), 30.0], egui::Button::new(t!("➕  Neue Seite einfügen")))
                                })
                                .inner
                                .on_hover_text(t!("Leere Seite direkt nach der aktuellen Seite"))
                                .clicked()
                            {
                                actions.push(UiAction::AddPage);
                            }
                            delete_page_buttons(ui, state, notebook, actions);
                            if ui
                                .add_sized([ui.available_width(), 30.0], egui::Button::new(t!("🖼  Bild einfügen … (Strg+I)")))
                                .on_hover_text(t!("Auch per Strg+V aus der Zwischenablage oder durch Ziehen einer Bilddatei ins Fenster"))
                                .clicked()
                            {
                                state.open_image_picker();
                            }
                            if ui
                                .add_sized([ui.available_width(), 30.0], egui::Button::new(t!("📄  Importieren (PDF, Xournal++, Rnote, Saber) …")))
                                .on_hover_text(t!("Vorlesungsfolien, Übungsblätter, Formulare oder Notizbücher aus Xournal++, Rnote und Saber als neues Notizbuch öffnen und darauf schreiben"))
                                .clicked()
                            {
                                state.open_import_picker();
                            }

                            section(ui, t!("HINTERGRUND DIESER SEITE"));
                            let current_bg = notebook.current_page().background;
                            egui::Grid::new("more_menu_backgrounds")
                                .num_columns(2)
                                .spacing([6.0, 6.0])
                                .show(ui, |ui| {
                                    for (i, (bg, label)) in [
                                        (PageBackground::Grid5mm, t!("Kariert")),
                                        (PageBackground::Lined, t!("Liniert")),
                                        (PageBackground::DotGrid, t!("Punkte")),
                                        (PageBackground::Blank, t!("Blanko")),
                                    ]
                                    .into_iter()
                                    .enumerate()
                                    {
                                        if ui
                                            .add_sized(
                                                [115.0, 28.0],
                                                egui::Button::selectable(current_bg == bg, label),
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

                            section(ui, t!("PAPIERFARBE (NUR ANZEIGE)"));
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
                                        t!("Dunkle Stiftfarben aufhellen"),
                                    ),
                                )
                                .on_hover_text(
                                    t!("Schwarz wird Weiß, Dunkelblau wird Hellblau. Gespeichert und \
                                     als PDF exportiert werden immer die Originalfarben."),
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

                            section(ui, t!("KAROS, LINIEN UND PUNKTE"));
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

                            section(ui, t!("SEITEN DIESES NOTIZBUCHS"));
                            let mut layout = if state.endless {
                                NotebookLayout::Endless
                            } else if state.continuous_scroll {
                                NotebookLayout::Continuous
                            } else {
                                NotebookLayout::Paged
                            };
                            if layout_picker(ui, &mut layout, state.endless_switch_possible, true) {
                                actions.push(UiAction::SetNotebookLayout(layout));
                            }

                            section(ui, t!("ZOOM"));
                            ui.horizontal(|ui| {
                                if ui
                                    .add_enabled(state.current_zoom > 0.25, egui::Button::new("🔍 -"))
                                    .on_hover_text(t!("Herauszoomen (Strg+-)"))
                                    .clicked()
                                {
                                    actions.push(UiAction::ZoomOut);
                                }
                                let pct = (state.current_zoom * 100.0).round() as u32;
                                if ui
                                    .button(format!("{pct} %"))
                                    .on_hover_text(t!("Zurücksetzen (Strg+0)"))
                                    .clicked()
                                {
                                    actions.push(UiAction::ResetZoom);
                                }
                                if ui
                                    .add_enabled(state.current_zoom < 5.0, egui::Button::new("🔍 +"))
                                    .on_hover_text(t!("Heranzoomen (Strg++)"))
                                    .clicked()
                                {
                                    actions.push(UiAction::ZoomIn);
                                }
                            });

                            section(ui, t!("EINGABE"));
                            if ui
                                .checkbox(
                                    &mut state.touch_draw_enabled,
                                    t!("Mit dem Finger zeichnen"),
                                )
                                .on_hover_text(
                                    t!("Aus: ein Finger verschiebt, zwei Finger zoomen (empfohlen mit Stift).\nAn: ein Finger zeichnet."),
                                )
                                .changed()
                            {
                                actions.push(UiAction::SetTouchDraw(state.touch_draw_enabled));
                            }
                            let mut math = state.math.enabled;
                            if ui
                                .checkbox(&mut math, t!("Formeln nach „=“ ausrechnen"))
                                .on_hover_text(t!("Nach einem handgeschriebenen „=“ erscheint das Ergebnis daneben, z. B. 2⁴ = 16. Antippen mit dem Stift oder Klicken fügt es als Text ein.\nGelesen werden Ziffern, + − · : /, Klammern, Hochzahlen, Brüche und x, y, a, b."))
                                .changed()
                            {
                                state.math.set_enabled(math);
                                actions.push(UiAction::SetMathAssist(math));
                            }
                            if let Some(curve) = pressure_curve_editor(ui, state) {
                                actions.push(UiAction::SavePressureCurve(curve));
                            }
                            if stylus_buttons_editor(ui, state) {
                                actions.push(UiAction::SaveStylusButtons);
                            }

                            section(ui, t!("WERKZEUGLEISTE"));
                            ui.horizontal(|ui| {
                                for (side, label) in
                                    [(SidebarSide::Left, t!("Links")), (SidebarSide::Right, t!("Rechts"))]
                                {
                                    if ui
                                        .add_sized(
                                            [115.0, 28.0],
                                            egui::Button::selectable(
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

                            // Zweisprachig, damit man die Auswahl in jeder Sprache findet
                            section(ui, "SPRACHE / LANGUAGE");
                            ui.horizontal(|ui| {
                                let choices = [
                                    (None, t!("System")),
                                    (Some(crate::i18n::Language::German), "Deutsch"),
                                    (Some(crate::i18n::Language::English), "English"),
                                ];
                                for (language, label) in choices {
                                    if ui
                                        .add_sized(
                                            [75.0, 28.0],
                                            egui::Button::selectable(
                                                state.language == language,
                                                label,
                                            ),
                                        )
                                        .on_hover_text(t!("Sprache der Oberfläche"))
                                        .clicked()
                                        && state.language != language
                                    {
                                        state.language = language;
                                        actions.push(UiAction::SetLanguage(language));
                                    }
                                }
                            });

                            section(ui, t!("SPEICHERORT"));
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(state.storage_dir.display().to_string())
                                        .size(11.5)
                                        .color(egui::Color32::from_gray(190)),
                                )
                                .truncate(),
                            )
                            .on_hover_text(state.storage_dir.display().to_string());
                            ui.horizontal(|ui| {
                                if ui
                                    .add_sized([115.0, 28.0], egui::Button::new(t!("📁  Ändern …")))
                                    .on_hover_text(t!("Z. B. einen Nextcloud- oder Google-Drive-Ordner wählen, um die Notizbücher mit anderen Geräten abzugleichen"))
                                    .clicked()
                                {
                                    state.open_storage_picker();
                                }
                                if ui
                                    .add_enabled(
                                        state.storage_dir_custom,
                                        egui::Button::new(t!("Standard")).min_size(egui::vec2(115.0, 28.0)),
                                    )
                                    .on_hover_text(t!("Zurück zum Standard-Speicherort"))
                                    .clicked()
                                {
                                    actions.push(UiAction::SetStorageDir(None));
                                }
                            });
                            cloud_section(ui, state, actions);

                            section(ui, t!("STATUS"));
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
                                    t!("🚀  Update {version} verfügbar", version = rel.tag_name)
                                }
                                crate::updater::UpdaterStatus::Downloading { percent, .. } => {
                                    t!("⏳  Update lädt … {percent} %", percent = format!("{:.0}", percent * 100.0))
                                }
                                crate::updater::UpdaterStatus::DownloadFinished { .. } => {
                                    t!("✔  Update bereit").to_string()
                                }
                                crate::updater::UpdaterStatus::Checking => {
                                    t!("🔄  Suche nach Updates …").to_string()
                                }
                                _ => t!("🔄  Updates & Versionen").to_string(),
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
        if let Some(btn_rect) = anchors.page_btn_rect {
            let (pos, pivot) =
                popover_placement(state.sidebar_side, anchors.toolbar_rect, btn_rect);
            let pop_response = egui::Area::new(egui::Id::unique("page_picker_popover"))
                .fixed_pos(pos)
                .pivot(pivot)
                .constrain(true)
                .order(egui::Order::Foreground)
                .show(ctx, |ui| {
                    popover_frame()
                        .show(ui, |ui| {
                            // Breite = Kachelraster; sonst nimmt die Kopfzeile die ganze Fensterbreite ein
                            ui.set_width(
                                PAGE_PICKER_COLUMNS as f32 * (PAGE_TILE_WIDTH + 8.0) - 8.0,
                            );
                            touch_friendly_spacing(ui);
                            let title = t!("Seiten ({count})", count = notebook.pages.len());
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
                                egui::RichText::new(t!(
                                    "TITEL VON SEITE {number}",
                                    number = notebook.current_page_index + 1
                                ))
                                .size(10.5)
                                .strong()
                                .color(egui::Color32::from_gray(160)),
                            );
                            let field = ui.add(
                                egui::TextEdit::singleline(&mut state.page_title_input)
                                    .hint_text(t!("z. B. Kapitel 2 – Integrale"))
                                    .margin(egui::vec2(8.0, 6.0))
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
                                let is_bookmarked = current.bookmarked;
                                let star_label = if is_bookmarked { "★" } else { "☆" };
                                let star_color = if is_bookmarked {
                                    egui::Color32::from_rgb(255, 200, 60)
                                } else {
                                    egui::Color32::from_gray(190)
                                };
                                if ui
                                    .add_sized(
                                        [34.0, 28.0],
                                        egui::Button::new(
                                            egui::RichText::new(star_label)
                                                .size(14.0)
                                                .color(star_color),
                                        ),
                                    )
                                    .on_hover_text(t!("Lesezeichen umschalten (Strg+D)"))
                                    .clicked()
                                {
                                    actions.push(UiAction::ToggleBookmark(current.id));
                                }
                                let can_prev = notebook.current_page_index > 0;
                                if ui
                                    .add_enabled(
                                        can_prev,
                                        egui::Button::new("◀").min_size(egui::vec2(30.0, 28.0)),
                                    )
                                    .on_hover_text(t!("Seite nach vorne schieben"))
                                    .clicked()
                                {
                                    actions.push(UiAction::MoveCurrentPage(-1));
                                }
                                let can_next =
                                    notebook.current_page_index + 1 < notebook.pages.len();
                                if ui
                                    .add_enabled(
                                        can_next,
                                        egui::Button::new("▶").min_size(egui::vec2(30.0, 28.0)),
                                    )
                                    .on_hover_text(t!("Seite nach hinten schieben"))
                                    .clicked()
                                {
                                    actions.push(UiAction::MoveCurrentPage(1));
                                }
                                if ui
                                    .add_sized([34.0, 28.0], egui::Button::new("⧉"))
                                    .on_hover_text(t!("Aktuelle Seite duplizieren"))
                                    .clicked()
                                {
                                    actions.push(UiAction::DuplicateCurrentPage);
                                }
                            });
                            ui.add_space(2.0);
                            ui.horizontal(|ui| {
                                for (list, label) in
                                    [(false, t!("🖼  Kacheln")), (true, t!("📑  Inhalt"))]
                                {
                                    if ui
                                        .add_sized(
                                            [104.0, 30.0],
                                            egui::Button::selectable(
                                                state.page_picker_list == list,
                                                label,
                                            ),
                                        )
                                        .clicked()
                                    {
                                        state.page_picker_list = list;
                                    }
                                }
                                let bm_count =
                                    notebook.pages.iter().filter(|p| p.bookmarked).count();
                                let bm_label = if bm_count > 0 {
                                    format!("★ {bm_count}")
                                } else {
                                    "★".to_string()
                                };
                                if ui
                                    .add_sized(
                                        [46.0, 30.0],
                                        egui::Button::selectable(
                                            state.page_picker_bookmarks_only,
                                            bm_label,
                                        ),
                                    )
                                    .on_hover_text(t!("Nur Lesezeichen anzeigen"))
                                    .clicked()
                                {
                                    state.page_picker_bookmarks_only =
                                        !state.page_picker_bookmarks_only;
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
                                        if let Some(idx) = page_title_list(
                                            ui,
                                            notebook,
                                            state.page_picker_bookmarks_only,
                                        ) {
                                            actions.push(UiAction::GoToPage(idx));
                                            state.show_page_jump_popup = false;
                                        }
                                    });
                            } else {
                                egui::ScrollArea::vertical()
                                    .max_height(420.0)
                                    .show(ui, |ui| {
                                        let visible_pages: Vec<(usize, &crate::document::Page)> =
                                            notebook
                                                .pages
                                                .iter()
                                                .enumerate()
                                                .filter(|(_, p)| {
                                                    !state.page_picker_bookmarks_only
                                                        || p.bookmarked
                                                })
                                                .collect();
                                        if state.page_picker_bookmarks_only
                                            && visible_pages.is_empty()
                                        {
                                            ui.label(
                                                egui::RichText::new(t!(
                                                    "Noch keine Lesezeichen gesetzt. Tippe oben auf ☆ oder drücke Strg+D."
                                                ))
                                                .color(egui::Color32::from_gray(150)),
                                            );
                                        } else {
                                            let mut wanted_pdfs = Vec::new();
                                            let tile = page_tile_size(notebook);
                                            egui::Grid::new("page_picker_grid")
                                                .spacing([8.0, 8.0])
                                                .show(ui, |ui| {
                                                    for (col_idx, (idx, page)) in
                                                        visible_pages.into_iter().enumerate()
                                                    {
                                                        let is_current =
                                                            idx == notebook.current_page_index;
                                                        if page_thumbnail(
                                                            ui,
                                                            state,
                                                            page,
                                                            idx + 1,
                                                            is_current,
                                                            tile,
                                                            &mut wanted_pdfs,
                                                        )
                                                        .clicked()
                                                        {
                                                            actions.push(UiAction::GoToPage(idx));
                                                            state.show_page_jump_popup = false;
                                                        }
                                                        if (col_idx + 1) % PAGE_PICKER_COLUMNS == 0
                                                        {
                                                            ui.end_row();
                                                        }
                                                    }
                                                });
                                            if !wanted_pdfs.is_empty() {
                                                actions.push(UiAction::RequestPdfThumbnails(
                                                    wanted_pdfs,
                                                ));
                                            }
                                        }
                                    });
                            }
                            ui.add_space(4.0);
                            if ui
                                .add_sized(
                                    [ui.available_width(), 36.0],
                                    egui::Button::new(t!("➕  Neue Seite einfügen")),
                                )
                                .on_hover_text(t!("Leere Seite direkt nach der aktuellen Seite"))
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
        || state.is_eraser_menu_open
        || state.is_highlighter_menu_open
        || state.is_text_color_menu_open
        || state.is_stamps_menu_open
        || state.is_more_menu_open
        || state.show_page_jump_popup)
        && ctx.input(|i| i.pointer.any_pressed())
    {
        if let Some(pos) = ctx.input(|i| i.pointer.press_origin()) {
            let in_toolbar = state.toolbar_rect.is_some_and(|r| r.contains(pos));
            let in_popover = state.popover_rect.is_some_and(|r| r.contains(pos));
            if !in_toolbar && !in_popover {
                state.is_pen_menu_open = false;
                state.is_eraser_menu_open = false;
                state.is_highlighter_menu_open = false;
                state.is_text_color_menu_open = false;
                state.is_stamps_menu_open = false;
                state.is_more_menu_open = false;
                state.show_page_jump_popup = false;
            }
        }
    }
}

/// „Seite löschen“ mit Rückfrage im ☰-Menü und „Gelöschte Seite wiederherstellen“.
pub(super) fn delete_page_buttons(
    ui: &mut egui::Ui,
    state: &mut UiState,
    notebook: &Notebook,
    actions: &mut Vec<UiAction>,
) {
    let number = notebook.current_page_index + 1;
    if state.confirm_delete_page {
        ui.label(
            egui::RichText::new(t!(
                "Seite {number} mit allem Inhalt löschen?",
                number = number
            ))
            .color(egui::Color32::from_rgb(255, 170, 160)),
        );
        ui.horizontal(|ui| {
            let half = (ui.available_width() - ui.spacing().item_spacing.x) / 2.0;
            if ui
                .add_sized(
                    [half, 30.0],
                    egui::Button::new(
                        egui::RichText::new(t!("Löschen")).color(egui::Color32::WHITE),
                    )
                    .fill(egui::Color32::from_rgb(170, 50, 45)),
                )
                .clicked()
            {
                actions.push(UiAction::DeleteCurrentPage);
                state.confirm_delete_page = false;
            }
            if ui
                .add_sized([half, 30.0], egui::Button::new(t!("Abbrechen")))
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
                    egui::Button::new(t!("🗑  Seite {number} löschen …", number = number)),
                )
            })
            .inner
            .on_hover_text(t!(
                "Löscht die aktuelle Seite; danach im Menü wiederherstellbar"
            ))
            .on_disabled_hover_text(t!(
                "Die einzige Seite eines Notizbuchs kann nicht gelöscht werden"
            ));
        if response.clicked() {
            state.confirm_delete_page = true;
        }
    }
    if let Some(number) = state.restorable_page {
        if ui
            .add_sized(
                [ui.available_width(), 30.0],
                egui::Button::new(t!(
                    "⟲  Gelöschte Seite {number} wiederherstellen",
                    number = number
                )),
            )
            .clicked()
        {
            actions.push(UiAction::RestoreDeletedPage);
        }
    }
}
/// Nextcloud sync in the menu (Issue #49, stage 2).
pub(super) fn cloud_section(ui: &mut egui::Ui, state: &mut UiState, actions: &mut Vec<UiAction>) {
    use crate::cloud_sync::Status;
    let small = |text: String, color: egui::Color32| {
        egui::Label::new(egui::RichText::new(text).size(11.5).color(color)).wrap()
    };
    let grey = egui::Color32::from_gray(190);
    let warn = egui::Color32::from_rgb(240, 180, 80);
    let button = |text: String| egui::Button::new(text).min_size(egui::vec2(115.0, 28.0));
    ui.add_space(4.0);
    ui.label(
        egui::RichText::new(t!("NEXTCLOUD"))
            .size(10.5)
            .strong()
            .color(egui::Color32::from_gray(160)),
    );
    let status = state.cloud.status.clone();
    if matches!(
        status,
        Status::Connecting | Status::WaitingForLogin { .. } | Status::Syncing
    ) {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(500));
    }
    match status {
        Status::Off | Status::Failed(_) if state.cloud.account.is_none() => {
            if let Status::Failed(msg) = &status {
                ui.add(small(format!("⚠ {msg}"), warn));
            }
            let field = ui.add(
                egui::TextEdit::singleline(&mut state.cloud_server_input)
                    .hint_text(t!("z. B. cloud.example.org"))
                    .margin(egui::vec2(8.0, 6.0))
                    .desired_width(f32::INFINITY),
            );
            if state.cloud_server_input.trim().starts_with("http://") {
                ui.add(small(
                    t!("⚠ Ohne https ist das App-Passwort im Netz lesbar – nur im eigenen Heimnetz verwenden").to_string(),
                    warn,
                ));
            }
            let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if ui
                .add_enabled(
                    !state.cloud_server_input.trim().is_empty(),
                    button(t!("🔗  Verbinden …").to_string()),
                )
                .on_hover_text(t!("Anmeldung im Browser; die App bekommt ein eigenes App-Passwort, das sich in Nextcloud jederzeit widerrufen lässt"))
                .clicked()
                || enter && !state.cloud_server_input.trim().is_empty()
            {
                actions.push(UiAction::CloudConnect {
                    server: state.cloud_server_input.clone(),
                    trusted: None,
                });
            }
        }
        Status::Off | Status::Connecting => {
            ui.add(small(t!("⏳  Verbinde …").to_string(), grey));
            if ui.add(button(t!("Abbrechen").to_string())).clicked() {
                actions.push(UiAction::CloudCancel);
            }
        }
        Status::Untrusted {
            server,
            fingerprint,
        } => {
            ui.add(small(
                t!("⚠ Das Zertifikat von {server} ist von keiner bekannten Stelle signiert. Nur vertrauen, wenn dieser Fingerabdruck mit dem des Servers übereinstimmt:", server = server),
                warn,
            ));
            ui.add(
                egui::Label::new(
                    egui::RichText::new(&fingerprint)
                        .monospace()
                        .size(10.0)
                        .color(grey),
                )
                .wrap(),
            );
            ui.horizontal(|ui| {
                if ui.add(button(t!("Vertrauen").to_string())).clicked() {
                    actions.push(UiAction::CloudConnect {
                        server: server.clone(),
                        trusted: Some(fingerprint.clone()),
                    });
                }
                if ui.add(button(t!("Abbrechen").to_string())).clicked() {
                    actions.push(UiAction::CloudCancel);
                }
            });
        }
        Status::WaitingForLogin { url } => {
            ui.add(small(
                t!("Bitte im Browser anmelden und den Zugriff erlauben …").to_string(),
                grey,
            ));
            ui.horizontal(|ui| {
                if ui
                    .add(button(t!("🌐  Browser öffnen").to_string()))
                    .clicked()
                {
                    actions.push(UiAction::OpenUrlInBrowser(url.clone()));
                }
                if ui.add(button(t!("Abbrechen").to_string())).clicked() {
                    actions.push(UiAction::CloudCancel);
                }
            });
        }
        Status::Idle { .. } | Status::Syncing | Status::Offline(_) | Status::Failed(_) => {
            if let Some(account) = &state.cloud.account {
                ui.add(small(format!("👤  {account}"), grey).truncate())
                    .on_hover_text(account);
            }
            let (text, color) = match &status {
                Status::Idle { last: Some(at) } => (
                    t!("✔  Synchronisiert {when}", when = elapsed_label(*at)),
                    egui::Color32::from_rgb(120, 200, 120),
                ),
                Status::Offline(msg) => (
                    t!("⚪  Offline – neuer Versuch in Kürze ({msg})", msg = msg),
                    grey,
                ),
                Status::Failed(msg) => (format!("⚠  {msg}"), warn),
                _ => (t!("🔄  Synchronisiere …").to_string(), grey),
            };
            ui.add(small(text, color));
            if state.cloud.insecure {
                ui.add(small(t!("⚠ Unverschlüsselt (http)").to_string(), warn));
            }
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        !matches!(status, Status::Syncing),
                        button(t!("🔄  Jetzt").to_string()),
                    )
                    .on_hover_text(t!("Sofort mit dem Server abgleichen"))
                    .clicked()
                {
                    actions.push(UiAction::CloudSyncNow);
                }
                if ui
                    .add(button(t!("Trennen").to_string()))
                    .on_hover_text(t!("Abgleich beenden und das App-Passwort widerrufen; die Notizbücher bleiben auf diesem Gerät und auf dem Server"))
                    .clicked()
                {
                    actions.push(UiAction::CloudDisconnect);
                }
            });
        }
    }
}
