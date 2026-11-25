//! Canvas-Overlays: Lineal, Textbearbeitung, Mathe-Vorschlag, Kontextmenü, Scrollbar und Seitenvorschauen.

use crate::background::{adapt_ink_color, PaperTheme};
use crate::document::{Notebook, Page, StrokeTool, PAGE_HEIGHT, PAGE_WIDTH};
use crate::ruler::RulerHit;
use crate::settings::SidebarSide;
use crate::t;

use super::widgets::{
    color32_from, popover_frame, touch_friendly_spacing, PAGE_TILE_LABEL, PAGE_TILE_WIDTH,
};
use super::{RulerDrag, UiAction, UiState};

pub(super) fn render_canvas_overlays(
    ctx: &egui::Context,
    state: &mut UiState,
    notebook: &Notebook,
    actions: &mut Vec<UiAction>,
) {
    page_separators(ctx, state, notebook);
    page_scrollbar(ctx, state, notebook, actions);
    space_preview(ctx, state, notebook);
    ruler_overlay(ctx, state);
    text_edit_overlay(ctx, state);
    math_suggestion(ctx, state, notebook);
    context_menu(ctx, state, actions);
}

/// Size of a page picker tile: as high as the tallest page of the notebook needs, so that
/// slides (landscape PDFs) get flat tiles without an empty strip (Issue #40).
pub(super) fn page_tile_size(notebook: &Notebook) -> egui::Vec2 {
    let aspect = notebook
        .pages
        .iter()
        .map(|p| p.size.y / p.size.x.max(1.0))
        .fold(0.0_f32, f32::max);
    let aspect = if aspect > 0.0 {
        aspect.clamp(0.3, 2.0)
    } else {
        PAGE_HEIGHT / PAGE_WIDTH
    };
    egui::vec2(PAGE_TILE_WIDTH, PAGE_TILE_WIDTH * aspect + PAGE_TILE_LABEL)
}
/// Rectangle of a page of `page_size` in a thumbnail area: as large as possible, centered
/// horizontally and at the top.
pub(super) fn thumbnail_sheet(page_size: glam::Vec2, rect: egui::Rect) -> egui::Rect {
    let scale = (rect.width() / page_size.x).min(rect.height() / page_size.y);
    let size = egui::vec2(page_size.x, page_size.y) * scale;
    egui::Rect::from_min_size(
        rect.min + egui::vec2((rect.width() - size.x) / 2.0, 0.0),
        size,
    )
}
/// Höchstzahl gezeichneter Punkte pro Vorschau (dünnt volle Seiten aus).
pub(super) const THUMBNAIL_POINT_BUDGET: usize = 1500;
/// Zeichenbefehle für die verkleinerte Vorschau einer Seite in `rect` in der Papierfarbe
/// `paper`: so groß wie möglich, waagerecht zentriert und oben (Querformat-Seiten sind
/// flacher). Striche werden als Linien gezeichnet und bei vielen Punkten gleichmäßig
/// ausgedünnt; `adapt_ink` hellt dunkle Tinte wie auf dem Blatt auf. `pdf_texture` is the
/// rendered PDF page under the ink; without it the PDF page is a plain gray area.
pub fn page_thumbnail_shapes(
    page: &Page,
    rect: egui::Rect,
    paper: PaperTheme,
    adapt_ink: bool,
    pdf_texture: Option<egui::TextureId>,
) -> Vec<egui::Shape> {
    let sheet = thumbnail_sheet(page.size, rect);
    let scale = sheet.width() / page.size.x;
    let mut shapes = vec![egui::Shape::rect_filled(
        sheet,
        egui::CornerRadius::same(3),
        linear_color32(paper.paper_color()),
    )];
    let adapt = adapt_ink && paper.is_dark();
    let to_screen = |p: glam::Vec2| sheet.min + egui::vec2(p.x * scale, p.y * scale);

    // Importierte PDF-Seite: gerendert oder als helle Fläche mit Rand
    if let Some(pdf) = &page.pdf_background {
        let points: Vec<egui::Pos2> = pdf
            .corners(page.size)
            .iter()
            .map(|&c| to_screen(c))
            .collect();
        if let Some(texture) = pdf_texture {
            shapes.push(egui::Shape::image(
                texture,
                egui::Rect::from_two_pos(points[0], points[2]),
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            ));
        } else {
            shapes.push(egui::Shape::convex_polygon(
                points,
                egui::Color32::from_rgb(236, 238, 243),
                egui::Stroke::new(0.8_f32, egui::Color32::from_gray(150)),
            ));
        }
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
/// Bearbeitung eines Textfelds: egui-Textfeld an der Stelle und in der Größe des Felds auf
/// dem Blatt (ungedreht), mit derselben Schrift. Esc beendet die Bearbeitung.
/// The result offered next to a handwritten `=` (Issue #46): what was read and the result,
/// e.g. `2⁴ = 16`. Not interactive for egui, so that fingers and palms pass through to the
/// page; the pen and the mouse take it via [`crate::math_assist::MathAssist::take_hit`].
pub(super) fn math_suggestion(ctx: &egui::Context, state: &mut UiState, notebook: &Notebook) {
    state.math.chip_rect = None;
    if state.view_mode {
        return;
    }
    let (Some(view), Some(suggestion)) = (
        state.page_view,
        state.math.suggestion_for(notebook.current_page()).cloned(),
    ) else {
        return;
    };
    let size = (suggestion.symbol_height * view.scale * 0.8).clamp(14.0, 48.0);
    let response = egui::Area::new(egui::Id::unique("math_suggestion"))
        .fixed_pos(view.to_screen(suggestion.anchor))
        .pivot(egui::Align2::LEFT_CENTER)
        .order(egui::Order::Foreground)
        .interactable(false)
        .show(ctx, |ui| {
            egui::Frame::NONE
                .fill(egui::Color32::from_rgba_unmultiplied(30, 34, 44, 235))
                .stroke(egui::Stroke::new(
                    1.5_f32,
                    egui::Color32::from_rgb(70, 130, 230),
                ))
                .corner_radius(size * 0.4)
                .inner_margin(egui::vec2(size * 0.45, size * 0.2))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = size * 0.25;
                        ui.label(
                            egui::RichText::new(format!("{} =", suggestion.expression))
                                .size(size * 0.7)
                                .color(egui::Color32::from_gray(170)),
                        );
                        ui.label(
                            egui::RichText::new(&suggestion.result)
                                .size(size)
                                .strong()
                                .color(egui::Color32::WHITE),
                        );
                    });
                });
        })
        .response;
    // A little larger than drawn, as the pen does not hit exactly
    state.math.chip_rect = Some(response.rect.expand(6.0));
}
pub(super) fn text_edit_overlay(ctx: &egui::Context, state: &mut UiState) {
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
    let width = (edit.text.edit_width() * sx * view.scale * rounding).max(8.0);
    let color = color32_from(if adapt {
        adapt_ink_color(edit.color)
    } else {
        edit.color
    });
    egui::Area::new(egui::Id::unique("text_edit_area"))
        .fixed_pos(view.to_screen(edit.origin))
        .order(egui::Order::Middle)
        .show(ctx, |ui| {
            // egui::Area merkt sich die Breite des letzten Frames (bei leerem Feld nur ~3 Zeichen).
            // Ohne explizites set_max_width/set_width bliebe das Feld beim Einfügen (Strg+V)
            // schmal und würde den Text senkrecht umbrechen (Issue #62).
            ui.set_max_width(width + 4.0);
            egui::Frame::NONE
                .stroke(egui::Stroke::new(
                    1.0_f32,
                    egui::Color32::from_rgba_unmultiplied(70, 130, 230, 200),
                ))
                .show(ui, |ui| {
                    ui.set_width(width);
                    let response = ui.add(
                        egui::TextEdit::multiline(&mut edit.text.content)
                            .id(egui::Id::unique("text_edit_field"))
                            .frame(egui::Frame::NONE)
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
pub(super) fn linear_color32(color: [f32; 4]) -> egui::Color32 {
    egui::Rgba::from_rgba_unmultiplied(color[0], color[1], color[2], color[3]).into()
}
/// Kachel der Seitenauswahl: Vorschau der Seite mit Seitenzahl darunter. A visible PDF page
/// that has not been rendered small yet is added to `wanted_pdfs`.
pub(super) fn page_thumbnail(
    ui: &mut egui::Ui,
    state: &UiState,
    page: &Page,
    number: usize,
    is_current: bool,
    tile: egui::Vec2,
    wanted_pdfs: &mut Vec<crate::document::PdfBackground>,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(tile, egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let thumb =
            egui::Rect::from_min_size(rect.min, egui::vec2(tile.x, tile.y - PAGE_TILE_LABEL));
        let pdf_texture = match &page.pdf_background {
            Some(pdf) => match state.pdf_thumbnails.get(&pdf.key()) {
                Some(texture) => texture.as_ref().map(|t| t.0.id()),
                None => {
                    wanted_pdfs.push(pdf.clone());
                    None
                }
            },
            None => None,
        };
        let painter = ui.painter();
        painter.extend(page_thumbnail_shapes(
            page,
            thumb,
            state.paper_theme,
            state.adapt_ink_colors,
            pdf_texture,
        ));
        // Frame and marks follow the page itself, not the whole tile
        let sheet = thumbnail_sheet(page.size, thumb);
        if !page.is_loaded {
            painter.text(
                sheet.center(),
                egui::Align2::CENTER_CENTER,
                "…",
                egui::FontId::proportional(16.0),
                egui::Color32::from_gray(140),
            );
        }
        // Lesezeichen-Ecke für benannte Seiten
        if page.title.is_some() {
            let corner = sheet.right_top();
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
        painter.rect_stroke(
            sheet,
            egui::CornerRadius::same(3),
            border,
            egui::StrokeKind::Middle,
        );
        if page.bookmarked {
            painter.text(
                egui::pos2(sheet.max.x - 9.0, sheet.min.y + 9.0),
                egui::Align2::CENTER_CENTER,
                "★",
                egui::FontId::proportional(13.0),
                egui::Color32::from_rgb(255, 200, 60),
            );
        }
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
        Some(title) => response.on_hover_text(t!(
            "Seite {number} – {title}",
            number = number,
            title = title
        )),
        None => response.on_hover_text(t!("Seite {number}", number = number)),
    }
}
/// Inhaltsverzeichnis der Seitenübersicht: alle Seiten mit Nummer und Titel, benannte
/// Seiten hervorgehoben. Gibt die angetippte Seite zurück.
pub(super) fn page_title_list(
    ui: &mut egui::Ui,
    notebook: &Notebook,
    bookmarks_only: bool,
) -> Option<usize> {
    let mut chosen = None;
    let visible: Vec<(usize, &Page)> = notebook
        .pages
        .iter()
        .enumerate()
        .filter(|(_, p)| !bookmarks_only || p.bookmarked)
        .collect();
    if bookmarks_only && visible.is_empty() {
        ui.label(
            egui::RichText::new(t!(
                "Noch keine Lesezeichen gesetzt. Tippe oben auf ☆ oder drücke Strg+D."
            ))
            .color(egui::Color32::from_gray(150)),
        );
        return None;
    }
    if !bookmarks_only && notebook.pages.iter().all(|p| p.title.is_none()) {
        ui.label(
            egui::RichText::new(
                t!("Noch keine Seite hat einen Titel. Gib oben einen Titel für die aktuelle Seite ein."),
            )
            .color(egui::Color32::from_gray(150)),
        );
        ui.add_space(4.0);
    }
    for (idx, page) in visible {
        let is_current = idx == notebook.current_page_index;
        let indent = "  ".repeat((page.outline_level as usize).min(4));
        let star = if page.bookmarked { "★ " } else { "" };
        let text = match &page.title {
            Some(title) => egui::RichText::new(format!("{:>3}   {indent}{star}{title}", idx + 1))
                .strong()
                .color(egui::Color32::WHITE),
            None => egui::RichText::new(format!("{:>3}   {indent}{star}–", idx + 1)).color(
                if page.bookmarked {
                    egui::Color32::from_rgb(255, 210, 90)
                } else {
                    egui::Color32::from_gray(130)
                },
            ),
        };
        let response = ui.add_sized(
            [ui.available_width(), 32.0],
            egui::Button::selectable(is_current, text),
        );
        if response.clicked() {
            chosen = Some(idx);
        }
    }
    chosen
}
/// Kontextmenü nach langem Drücken auf das Blatt: Einfügen an dieser Stelle. Ein Tippen
/// daneben oder Esc schließt es.
pub(super) fn context_menu(ctx: &egui::Context, state: &mut UiState, actions: &mut Vec<UiAction>) {
    let Some(menu) = state.context_menu else {
        return;
    };
    let response = egui::Area::new(egui::Id::unique("canvas_context_menu"))
        .fixed_pos(menu.screen)
        .constrain(true)
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            popover_frame().show(ui, |ui| {
                touch_friendly_spacing(ui);
                ui.set_width(220.0);
                if ui
                    .add_sized([220.0, 36.0], egui::Button::new(t!("📋  Einfügen")))
                    .on_hover_text(t!(
                        "Bild, Text oder kopierte Objekte aus der Zwischenablage hier einfügen"
                    ))
                    .clicked()
                {
                    actions.push(UiAction::PasteAt(menu.page));
                    state.context_menu = None;
                }
                if ui
                    .add_sized([220.0, 36.0], egui::Button::new(t!("🖼  Bild aus Datei …")))
                    .clicked()
                {
                    state.open_image_picker();
                }
            })
        });
    let rect = response.response.rect;
    let pressed_outside = ctx.input(|i| {
        i.pointer.any_pressed() && i.pointer.press_origin().is_some_and(|p| !rect.contains(p))
    });
    if pressed_outside || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        state.context_menu = None;
    }
}
/// Vorschau beim Platzschaffen: Linie an der Ansatzhöhe und das Band, das eingefügt (grün)
/// bzw. geschlossen (orange) wird, mit Angabe in Millimetern.
pub(super) fn space_preview(ctx: &egui::Context, state: &UiState, notebook: &Notebook) {
    let (Some(drag), Some(view)) = (state.space_drag, state.page_view) else {
        return;
    };
    let Some(page) = notebook.pages.get(drag.page_index) else {
        return;
    };
    let dy = drag.clamped_dy();
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::unique("space_preview"),
    ));
    let left = view.to_screen(glam::Vec2::new(0.0, drag.y));
    let right = view.to_screen(glam::Vec2::new(page.size.x, drag.y));
    let moved = view.to_screen(glam::Vec2::new(page.size.x, drag.y + dy));
    let (fill, line) = if dy >= 0.0 {
        (
            egui::Color32::from_rgba_unmultiplied(46, 204, 113, 50),
            egui::Color32::from_rgb(39, 174, 96),
        )
    } else {
        (
            egui::Color32::from_rgba_unmultiplied(230, 126, 34, 60),
            egui::Color32::from_rgb(211, 84, 0),
        )
    };
    let band = egui::Rect::from_two_pos(left, moved);
    painter.rect_filled(band, 0.0, fill);
    painter.line_segment([left, right], egui::Stroke::new(2.0_f32, line));
    painter.line_segment(
        [egui::pos2(left.x, moved.y), moved],
        egui::Stroke::new(2.0_f32, line),
    );
    let mm = dy / crate::document::UNITS_PER_MM;
    painter.text(
        egui::pos2(right.x - 8.0, moved.y.max(left.y) + 4.0),
        egui::Align2::RIGHT_TOP,
        if dy >= 0.0 {
            t!("⏷ {mm} mm Platz", mm = format!("{mm:.0}"))
        } else {
            format!("⏶ {:.0} mm", -mm)
        },
        egui::FontId::proportional(14.0),
        line,
    );
}
/// Durchgehende Ansicht: dunkle Linie an jeder Grenze zweier Seiten (die Seiten liegen
/// lückenlos untereinander). Unter allen Fenstern der Oberfläche, über der Tinte.
pub(super) fn page_separators(ctx: &egui::Context, state: &UiState, notebook: &Notebook) {
    let Some(view) = state.page_view.filter(|_| state.continuous_scroll) else {
        return;
    };
    let stack = notebook.page_stack();
    let screen = ctx.viewport_rect();
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
/// Lage der Bildlaufleiste: Leiste (Trefferfläche) und Griff in logischen Punkten. Der Griff
/// zeigt, wo die Fenstermitte `center` zwischen Ober- und Unterkante des Notizbuchs liegt;
/// `visible` ist die sichtbare Höhe (Seiteneinheiten) und bestimmt seine Länge.
pub(super) fn scrollbar_layout(
    screen: egui::Rect,
    side: SidebarSide,
    reserved: f32,
    (top, bottom): (f32, f32),
    center: f32,
    visible: f32,
) -> (egui::Rect, egui::Rect) {
    const WIDTH: f32 = 28.0;
    const MARGIN: f32 = 12.0;
    // Next to the toolbar, on its inner side: the writing hand rests on the opposite
    // half of the screen and no longer touches the bar by accident (issue #58)
    let x = match side {
        SidebarSide::Left => (screen.left() + reserved).min(screen.right() - WIDTH),
        SidebarSide::Right => (screen.right() - reserved - WIDTH).max(screen.left()),
    };
    let track = egui::Rect::from_min_size(
        egui::pos2(x, screen.top() + MARGIN),
        egui::vec2(WIDTH, (screen.height() - 2.0 * MARGIN).max(1.0)),
    );
    let total = (bottom - top).max(1.0);
    let thumb_h = (visible / (total + visible) * track.height())
        .clamp(48.0_f32.min(track.height()), track.height());
    let fraction = ((center - top) / total).clamp(0.0, 1.0);
    let thumb = egui::Rect::from_min_size(
        egui::pos2(
            track.left(),
            track.top() + fraction * (track.height() - thumb_h),
        ),
        egui::vec2(WIDTH, thumb_h),
    );
    (track, thumb)
}
/// Durchgehende Ansicht: Bildlaufleiste neben der Werkzeugleiste (zur Seite hin). Den
/// Griff ziehen (oder auf die Leiste tippen) springt schnell durch lange Notizbücher und
/// PDFs (Issue #28); beim Ziehen steht die Seitenzahl daneben.
pub(super) fn page_scrollbar(
    ctx: &egui::Context,
    state: &mut UiState,
    notebook: &Notebook,
    actions: &mut Vec<UiAction>,
) {
    let Some(view) = state.page_view.filter(|_| state.continuous_scroll) else {
        state.scrollbar_grab = None;
        return;
    };
    if notebook.pages.len() < 2 {
        state.scrollbar_grab = None;
        return;
    }
    let stack = notebook.page_stack();
    let (min, max) = stack.bounds();
    let screen = ctx.viewport_rect();
    let center = view.to_page(screen.center()).y;
    let visible = screen.height() / view.scale.max(1e-6);
    let (track, thumb) = scrollbar_layout(
        screen,
        state.sidebar_side,
        state.sidebar_reserved_width(),
        (min.y, max.y),
        center,
        visible,
    );

    let dark = state.paper_theme.is_dark();
    let response = egui::Area::new(egui::Id::unique("page_scrollbar"))
        .order(egui::Order::Middle)
        .fixed_pos(track.min)
        .show(ctx, |ui| {
            let (_, response) = ui.allocate_exact_size(track.size(), egui::Sense::drag());
            let active = response.hovered() || response.dragged();
            let bar_w = if active { 10.0 } else { 6.0 };
            let bar = egui::Rect::from_center_size(
                egui::pos2(thumb.center().x, thumb.center().y),
                egui::vec2(bar_w, thumb.height()),
            );
            let alpha = if active { 200 } else { 110 };
            let color = if dark {
                egui::Color32::from_white_alpha(alpha)
            } else {
                egui::Color32::from_black_alpha(alpha)
            };
            ui.painter()
                .rect_filled(bar, egui::CornerRadius::same((bar_w / 2.0) as u8), color);
            response
        })
        .inner;

    if response.drag_started() {
        let y = response
            .interact_pointer_pos()
            .map_or(thumb.center().y, |p| p.y);
        // Auf den Griff: dort festhalten; daneben: Griff unter den Finger springen lassen
        state.scrollbar_grab = Some(if thumb.y_range().contains(y) {
            y - thumb.top()
        } else {
            thumb.height() / 2.0
        });
    }
    if !response.dragged() {
        state.scrollbar_grab = None;
        return;
    }
    let (Some(grab), Some(pos)) = (state.scrollbar_grab, response.interact_pointer_pos()) else {
        return;
    };
    let free = (track.height() - thumb.height()).max(1.0);
    let fraction = ((pos.y - grab - track.top()) / free).clamp(0.0, 1.0);
    let target = min.y + fraction * (max.y - min.y);
    if (target - center).abs() > 0.5 {
        actions.push(UiAction::ScrollTo(target));
    }
    if let Some(page) = stack.page_at(target) {
        let label_x = match state.sidebar_side {
            SidebarSide::Left => track.right() + 8.0,
            SidebarSide::Right => track.left() - 8.0,
        };
        let anchor = match state.sidebar_side {
            SidebarSide::Left => egui::Align2::LEFT_CENTER,
            SidebarSide::Right => egui::Align2::RIGHT_CENTER,
        };
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::unique("page_scrollbar_label"),
        ));
        let text = format!("{} / {}", page + 1, notebook.pages.len());
        let galley =
            painter.layout_no_wrap(text, egui::FontId::proportional(18.0), egui::Color32::WHITE);
        let rect = anchor
            .anchor_size(egui::pos2(label_x, pos.y), galley.size())
            .expand2(egui::vec2(10.0, 6.0));
        painter.rect_filled(
            rect,
            egui::CornerRadius::same(8),
            egui::Color32::from_black_alpha(190),
        );
        painter.galley(
            rect.min + egui::vec2(10.0, 6.0),
            galley,
            egui::Color32::WHITE,
        );
    }
    ctx.request_repaint();
}
/// Zeichnet das Lineal über dem Blatt und bedient Verschieben und Drehen.
pub(super) fn ruler_overlay(ctx: &egui::Context, state: &mut UiState) {
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
    let over_ui = pos.is_some_and(|p| {
        ctx.layer_id_at(p)
            .is_some_and(|layer| layer.order != egui::Order::Background)
    });
    if pressed && !over_ui {
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
        egui::Id::unique("ruler_overlay"),
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
            let galley = ctx.fonts_mut(|f| f.layout_no_wrap(cm.to_string(), font.clone(), ink));
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
    let galley = ctx.fonts_mut(|f| {
        f.layout_no_wrap(
            label,
            egui::FontId::proportional(13.0),
            egui::Color32::WHITE,
        )
    });
    let pill = egui::Rect::from_center_size(center, galley.size() + egui::vec2(14.0, 6.0));
    painter.rect_filled(
        pill,
        egui::CornerRadius::same(9),
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
