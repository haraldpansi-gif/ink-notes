//! Schmale, schwebende Tab-Leiste für schnellen Wechsel zwischen geöffneten Notizbüchern.
//!
//! Erscheint erst ab 2 offenen Notizbüchern am oberen Rand neben der Werkzeugleiste, damit bei
//! einem einzelnen Notizbuch kein Platz auf dem Blatt verloren geht.

use crate::settings::SidebarSide;
use crate::t;

use super::sidebar::SidebarAnchors;
use super::{UiAction, UiState};

/// Maximale Zeichenzahl eines Tab-Titels vor der Kürzung mit `…`.
const MAX_TAB_TITLE_CHARS: usize = 22;

/// Kürzt einen Titel auf höchstens `max_chars` Zeichen (mit `…` am Ende).
pub(super) fn truncate_tab_title(title: &str, max_chars: usize) -> String {
    let mut chars = title.chars();
    let prefix: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{prefix}…")
    } else {
        prefix
    }
}

/// Rendert die Tab-Leiste, wenn mindestens zwei Notizbücher als Tab offen sind.
///
/// Gibt das Bildschirm-Rechteck der Leiste für das Hit-Testing zurück (`None`, wenn ausgeblendet).
pub(super) fn render_tab_bar(
    ctx: &egui::Context,
    state: &mut UiState,
    anchors: &SidebarAnchors,
    actions: &mut Vec<UiAction>,
) -> Option<egui::Rect> {
    if state.open_tabs.len() < 2 {
        return None;
    }

    let screen = ctx.viewport_rect();
    let gap = UiState::SIDEBAR_MARGIN;
    let (left, right) = match state.sidebar_side {
        SidebarSide::Left => (anchors.toolbar_rect.max.x + gap, screen.max.x - gap),
        SidebarSide::Right => (screen.min.x + gap, anchors.toolbar_rect.min.x - gap),
    };
    let max_width = (right - left).max(120.0);
    let pos = egui::pos2(left, screen.min.y + gap);

    let tabs_snapshot: Vec<(String, String, bool)> = state
        .open_tabs
        .iter()
        .map(|id| {
            let is_active = *id == state.current_notebook_id;
            let title = if is_active && !state.document_title.is_empty() {
                state.document_title.clone()
            } else {
                state
                    .available_notebooks
                    .iter()
                    .find(|n| n.id == *id)
                    .map(|n| n.title.clone())
                    .filter(|t| !t.is_empty())
                    .unwrap_or_else(|| id.clone())
            };
            (id.clone(), title, is_active)
        })
        .collect();

    let mut open_manager = false;

    let area_response = egui::Area::new(egui::Id::unique("notebook_tab_bar_area"))
        .fixed_pos(pos)
        .constrain(true)
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            egui::Frame::NONE
                .fill(egui::Color32::from_rgba_premultiplied(24, 24, 28, 225))
                .corner_radius(egui::CornerRadius::same(12))
                .stroke(egui::Stroke::new(
                    1.0_f32,
                    egui::Color32::from_rgba_unmultiplied(255, 255, 255, 30),
                ))
                .inner_margin(egui::Margin::symmetric(6, 4))
                .shadow(egui::epaint::Shadow {
                    offset: [0, 3],
                    blur: 12,
                    spread: 0,
                    color: egui::Color32::from_black_alpha(90),
                })
                .show(ui, |ui| {
                    ui.set_max_width((max_width - 12.0).max(80.0));
                    egui::ScrollArea::horizontal()
                        .id_salt("notebook_tab_bar_scroll")
                        .scroll_bar_visibility(
                            egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded,
                        )
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing = egui::vec2(4.0, 0.0);

                                for (id, full_title, is_active) in &tabs_snapshot {
                                    let label = truncate_tab_title(full_title, MAX_TAB_TITLE_CHARS);
                                    let (bg, border, text_color) = if *is_active {
                                        (
                                            egui::Color32::from_rgba_unmultiplied(
                                                64, 128, 232, 200,
                                            ),
                                            egui::Stroke::new(
                                                1.0,
                                                egui::Color32::from_rgba_unmultiplied(
                                                    150, 195, 255, 200,
                                                ),
                                            ),
                                            egui::Color32::WHITE,
                                        )
                                    } else {
                                        (
                                            egui::Color32::from_rgba_unmultiplied(
                                                255, 255, 255, 16,
                                            ),
                                            egui::Stroke::new(
                                                1.0,
                                                egui::Color32::from_rgba_unmultiplied(
                                                    255, 255, 255, 20,
                                                ),
                                            ),
                                            egui::Color32::from_gray(215),
                                        )
                                    };

                                    egui::Frame::NONE
                                        .fill(bg)
                                        .corner_radius(egui::CornerRadius::same(8))
                                        .stroke(border)
                                        .inner_margin(egui::Margin::symmetric(6, 3))
                                        .show(ui, |ui| {
                                            ui.horizontal(|ui| {
                                                ui.spacing_mut().item_spacing =
                                                    egui::vec2(4.0, 0.0);

                                                let tab_btn = ui
                                                    .add(
                                                        egui::Button::new(
                                                            egui::RichText::new(label)
                                                                .size(13.0)
                                                                .color(text_color),
                                                        )
                                                        .frame(false),
                                                    )
                                                    .on_hover_text(full_title);
                                                if tab_btn.clicked() && !*is_active {
                                                    actions
                                                        .push(UiAction::OpenNotebook(id.clone()));
                                                }
                                                if tab_btn.middle_clicked() {
                                                    actions.push(UiAction::CloseTab(id.clone()));
                                                }

                                                let close_btn = ui
                                                    .add(
                                                        egui::Button::new(
                                                            egui::RichText::new("×")
                                                                .size(13.0)
                                                                .color(if *is_active {
                                                                    egui::Color32::from_gray(235)
                                                                } else {
                                                                    egui::Color32::from_gray(165)
                                                                }),
                                                        )
                                                        .frame(false),
                                                    )
                                                    .on_hover_text(t!("Tab schließen"));
                                                if close_btn.clicked() {
                                                    actions.push(UiAction::CloseTab(id.clone()));
                                                }
                                            });
                                        });
                                }

                                let add_btn = ui
                                    .add(
                                        egui::Button::new(
                                            egui::RichText::new("+")
                                                .size(14.0)
                                                .color(egui::Color32::from_gray(220)),
                                        )
                                        .fill(egui::Color32::from_rgba_unmultiplied(
                                            255, 255, 255, 18,
                                        ))
                                        .corner_radius(egui::CornerRadius::same(8))
                                        .min_size(egui::vec2(26.0, 24.0)),
                                    )
                                    .on_hover_text(t!("Notizbücher verwalten"));
                                if add_btn.clicked() {
                                    open_manager = true;
                                }
                            });
                        });
                })
                .response
                .rect
        });

    if open_manager {
        state.is_document_menu_open = true;
        actions.push(UiAction::RefreshNotebooks);
    }

    Some(area_response.inner)
}
