//! Senkrechte, seitlich angedockte Werkzeugleiste.

use crate::autosave::AutosaveStatus;
use crate::document::{EraserMode, Notebook};
use crate::settings::{SidebarSide, PEN_PRESET_COUNT};
use crate::t;

use super::widgets::{
    autosave_status_style, color_circle_swatch, format_mm, pen_preset_swatch, sidebar_button,
    sidebar_divider, text_style_buttons, SidebarIcon,
};
use super::{ActiveTool, UiAction, UiState};

/// Bildschirmpositionen der Leiste und ihrer Anker-Knöpfe für die Popover-Platzierung.
pub(super) struct SidebarAnchors {
    pub toolbar_rect: egui::Rect,
    pub pen_btn_rect: Option<egui::Rect>,
    pub eraser_btn_rect: Option<egui::Rect>,
    pub high_btn_rect: Option<egui::Rect>,
    pub text_color_btn_rect: Option<egui::Rect>,
    pub stamps_btn_rect: Option<egui::Rect>,
    pub more_btn_rect: Option<egui::Rect>,
    pub page_btn_rect: Option<egui::Rect>,
}

pub(super) fn render_sidebar(
    ctx: &egui::Context,
    state: &mut UiState,
    notebook: &Notebook,
    autosave_status: AutosaveStatus,
    updater: &crate::updater::UpdaterController,
    actions: &mut Vec<UiAction>,
) -> SidebarAnchors {
    let mut pen_btn_rect = None;
    let mut eraser_btn_rect = None;
    let mut high_btn_rect = None;
    let mut text_color_btn_rect = None;
    let mut stamps_btn_rect = None;
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
    let max_height =
        (ctx.viewport_rect().height() - 2.0 * UiState::SIDEBAR_MARGIN - 12.0).max(120.0);
    // Größe nach dem höchsten Fall (Auswahl mit Aktionen), damit Knöpfe beim
    // Werkzeugwechsel weder schrumpfen noch verrutschen
    let btn = UiState::sidebar_button_size(ActiveTool::Select, max_height, state.continuous_scroll);

    let area_response = egui::Area::new(egui::Id::unique("floating_toolbar_area"))
        .anchor(anchor, offset)
        .default_size(egui::vec2(btn + 12.0, max_height + 12.0))
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            // egui::Area merkt sich die Größe des letzten Frames (z. B. im Nur-Ansehen-Modus
            // nur ~200 px). Ohne Reset auf max_height klemmt die ScrollArea beim Zurückschalten
            // auf die kleine Vorframe-Höhe fest und die Werkzeugleiste bleibt dauerhaft kurz.
            ui.set_max_height(max_height + 12.0);
            egui::Frame::NONE
                .fill(egui::Color32::from_rgba_premultiplied(24, 24, 28, 225))
                .corner_radius(egui::CornerRadius::same((UiState::TOOLBAR_ROUNDING) as u8))
                .stroke(egui::Stroke::new(
                    1.0_f32,
                    egui::Color32::from_rgba_unmultiplied(255, 255, 255, 30),
                ))
                .inner_margin(egui::Margin::same(6))
                .shadow(egui::epaint::Shadow {
                    offset: [0, 4],
                    blur: 16,
                    spread: 0,
                    color: egui::Color32::from_black_alpha(100),
                })
                .show(ui, |ui| {
                    ui.set_max_height(max_height);
                    egui::ScrollArea::vertical()
                        .max_height(max_height)
                        .min_scrolled_height(max_height)
                        .scroll_bar_visibility(
                            egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded,
                        )
                        .show(ui, |ui| {
                            // Zentriertes Layout nimmt sonst die volle Fensterbreite ein
                            ui.set_width(btn);
                            ui.with_layout(egui::Layout::top_down(egui::Align::Center), |ui| {
                                ui.spacing_mut().item_spacing = egui::vec2(0.0, UiState::SIDEBAR_SPACING);

                                // --- Notizbücher ---
                                let doc_tooltip = t!(
                                    "Notizbücher verwalten (aktuell: {title})",
                                    title = if state.document_title.is_empty() {
                                        t!("Notizbuch")
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
                                        // Im Ordner des geöffneten Notizbuchs beginnen
                                        state.notebook_folder = state
                                            .library
                                            .folder_of(&state.current_notebook_id)
                                            .to_string();
                                        state.renaming_folder = None;
                                        state.confirm_delete_folder = None;
                                        state.moving = None;
                                        actions.push(UiAction::RefreshNotebooks);
                                    }
                                }

                                // --- View only (Issue #35) ---
                                if sidebar_button(
                                    ui,
                                    btn,
                                    SidebarIcon::Glyph("👁"),
                                    state.view_mode,
                                    true,
                                    None,
                                    if state.view_mode {
                                        t!("Nur ansehen – tippen, um wieder zu schreiben")
                                    } else {
                                        t!("Nur ansehen: Stift, Finger und Maus verschieben nur das Blatt, nichts wird verändert")
                                    },
                                )
                                .clicked()
                                {
                                    state.close_overlays();
                                    actions.push(UiAction::SetViewMode(!state.view_mode));
                                }

                                sidebar_divider(ui, btn);

                                if !state.view_mode {
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
                                            t!("Stift (B/U/R für Farben) – erneut tippen für Farbe & Stärke")
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
                                            t!("Textmarker – erneut tippen für Farbe & Stärke")
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
                                            t!("Formen: Striche werden zu Linien, Rechtecken, Kreisen … (S/F)"),
                                        ),
                                        (
                                            ActiveTool::Eraser,
                                            SidebarIcon::Eraser,
                                            t!("Radierer (E oder rechte Maustaste) – darunter: ganze Striche, Teilstrich, nur Textmarker und die Größe"),
                                        ),
                                        (
                                            ActiveTool::Select,
                                            SidebarIcon::Lasso,
                                            t!("Auswählen: verschieben, drehen, skalieren, kopieren (V)"),
                                        ),
                                        (
                                            ActiveTool::Space,
                                            SidebarIcon::Glyph("↕"),
                                            t!("Platz schaffen: auf das Blatt tippen und nach unten ziehen – alles \
                                             darunter rutscht mit, auch auf die folgenden Seiten; nach oben \
                                             schließt leeren Platz"),
                                        ),
                                        (
                                            ActiveTool::Text,
                                            SidebarIcon::Glyph("T"),
                                            t!("Text (T): ins Blatt tippen und schreiben, vorhandenen Text \
                                             antippen zum Ändern; Esc beendet"),
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
                                        t!("Lineal (L): Fläche ziehen = verschieben, Griff ⟳ = drehen; \
                                         Striche an einer Kante werden gerade"),
                                    )
                                    .clicked()
                                    {
                                        state.toggle_ruler();
                                    }

                                    // --- Stempel / wiederverwendbare Elemente ---
                                    let stamps_resp = sidebar_button(
                                        ui,
                                        btn,
                                        SidebarIcon::Glyph("📌"),
                                        state.is_stamps_menu_open,
                                        true,
                                        None,
                                        t!("Stempel & wiederverwendbare Elemente"),
                                    );
                                    stamps_btn_rect = Some(stamps_resp.rect);
                                    if stamps_resp.clicked() {
                                        let open = !state.is_stamps_menu_open;
                                        state.close_overlays();
                                        state.is_stamps_menu_open = open;
                                    }

                                    // --- Stifte bzw. Farben des aktiven Werkzeugs ---
                                    let pens = matches!(state.active_tool, ActiveTool::Pen | ActiveTool::Shape);
                                    // Fester Platz für die längste Palette, damit die Knöpfe darunter
                                    // beim Werkzeugwechsel nicht springen
                                    sidebar_divider(ui, btn);
                                    let swatch = UiState::sidebar_swatch_size(btn);
                                    let mut shown = 0;
                                    if pens {
                                        shown = PEN_PRESET_COUNT;
                                        for i in 0..PEN_PRESET_COUNT {
                                            let preset = state.pen_presets[i];
                                            let selected = i == state.active_pen;
                                            let tooltip = if selected {
                                                t!(
                                                    "Stift {number} ({width} mm) – erneut tippen für Farbe & Stärke; \
                                                     Änderungen merkt sich der Stift",
                                                    number = i + 1,
                                                    width = format!("{:.2}", preset.width_mm)
                                                )
                                            } else {
                                                t!("Stift {number} ({width} mm)", number = i + 1, width = format!("{:.2}", preset.width_mm))
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
                                    // Eigene Farben von Textmarker und Text: antippen wählt, erneut
                                    // tippen passt die Farbe an. Die Auswahl wird in die Farben der
                                    // Stifte umgefärbt.
                                    let tool = state.active_tool;
                                    let slots: Option<Vec<[f32; 4]>> = match tool {
                                        ActiveTool::Highlighter => Some(state.highlighter_colors.to_vec()),
                                        ActiveTool::Text => Some(state.text_colors.to_vec()),
                                        ActiveTool::Select => {
                                            Some(state.pen_presets.iter().map(|p| p.color).collect())
                                        }
                                        _ => None,
                                    };
                                    if let Some(colors) = slots {
                                        shown = colors.len();
                                        for (i, color) in colors.into_iter().enumerate() {
                                            let selected = match tool {
                                                ActiveTool::Highlighter => i == state.active_highlighter,
                                                ActiveTool::Text => {
                                                    i == state.active_text_color
                                                        && state.text_color == color
                                                }
                                                _ => false,
                                            };
                                            let tooltip = match tool {
                                                ActiveTool::Select => {
                                                    t!("Auswahl in der Farbe von Stift {number} umfärben", number = i + 1)
                                                }
                                                _ if selected => t!(
                                                    "Farbe {number} – erneut tippen, um sie anzupassen",
                                                    number = i + 1
                                                ),
                                                _ => t!("Farbe {number}", number = i + 1),
                                            };
                                            let response =
                                                color_circle_swatch(ui, color, selected, &tooltip, swatch);
                                            if selected {
                                                // Das Farb-Popover erscheint neben der gewählten Farbe
                                                match tool {
                                                    ActiveTool::Highlighter => high_btn_rect = Some(response.rect),
                                                    _ => text_color_btn_rect = Some(response.rect),
                                                }
                                            }
                                            if !response.clicked() {
                                                continue;
                                            }
                                            match tool {
                                                ActiveTool::Highlighter if selected => {
                                                    let open = !state.is_highlighter_menu_open;
                                                    state.close_overlays();
                                                    state.is_highlighter_menu_open = open;
                                                }
                                                ActiveTool::Highlighter => {
                                                    state.close_overlays();
                                                    state.select_highlighter_color(i);
                                                    actions.push(UiAction::SetColor(color));
                                                }
                                                ActiveTool::Text if selected => {
                                                    let open = !state.is_text_color_menu_open;
                                                    state.close_overlays();
                                                    state.is_text_color_menu_open = open;
                                                }
                                                ActiveTool::Text => {
                                                    state.close_overlays();
                                                    state.active_text_color = i;
                                                    state.set_text_color(color);
                                                    actions.push(UiAction::SetColor(color));
                                                }
                                                _ => actions.push(UiAction::SetColor(color)),
                                            }
                                        }
                                    }
                                    let spacing = ui.spacing().item_spacing.y;
                                    let mut used = shown as f32 * (swatch + spacing);
                                    // Eraser: its modes and size take the place of the colors (swatch-sized, so that
                                    // all four fit)
                                    if state.active_tool == ActiveTool::Eraser {
                                        for (mode, icon, indicator, tooltip) in [
                                            (
                                                EraserMode::All,
                                                SidebarIcon::Text(t!("Alle")),
                                                None,
                                                t!("Alle Striche radieren"),
                                            ),
                                            (
                                                EraserMode::Partial,
                                                SidebarIcon::Glyph("✂"),
                                                None,
                                                t!("Teilstrich radieren – nur den überstrichenen Teil entfernen (Formen ganz)"),
                                            ),
                                            (
                                                EraserMode::InkOnly,
                                                SidebarIcon::Glyph("✏"),
                                                None,
                                                t!("Nur Handschrift & Formen radieren – getippter Text darunter bleibt"),
                                            ),
                                            (
                                                EraserMode::HighlighterOnly,
                                                SidebarIcon::Glyph("🖊"),
                                                Some(state.highlighter_color),
                                                t!("Nur Textmarker radieren – Handschrift darunter bleibt"),
                                            ),
                                        ] {
                                            if sidebar_button(
                                                ui,
                                                swatch,
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
                                            used += swatch + spacing;
                                        }
                                        // Eraser size: opens a menu like the pens (Issue #38)
                                        let diameter = format_mm(state.eraser_radius_mm * 2.0);
                                        let tooltip = t!(
                                            "Radiergröße {size} mm – tippen zum Ändern",
                                            size = diameter
                                        );
                                        let dot = (state.eraser_radius_mm * 0.7).clamp(2.0, swatch * 0.45);
                                        let response = sidebar_button(
                                            ui,
                                            swatch,
                                            SidebarIcon::Dot(dot),
                                            state.is_eraser_menu_open,
                                            true,
                                            None,
                                            &tooltip,
                                        );
                                        eraser_btn_rect = Some(response.rect);
                                        if response.clicked() {
                                            let open = !state.is_eraser_menu_open;
                                            state.close_overlays();
                                            state.is_eraser_menu_open = open;
                                        }
                                        used += swatch + spacing;
                                    }
                                    // Auswahl: unter den 4 Stiftfarben ist Platz für das Schloss des
                                    // Seitenverhältnisses beim Skalieren (Bilder: gesperrt, Issue #29)
                                    if state.active_tool == ActiveTool::Select {
                                        let locked = state.selection_aspect_lock;
                                        let (icon, tooltip) = if locked == Some(true) {
                                            ("🔒", t!("Seitenverhältnis gesperrt – tippen zum Freigeben"))
                                        } else {
                                            ("🔓", t!("Seitenverhältnis frei – tippen zum Sperren"))
                                        };
                                        if sidebar_button(
                                            ui,
                                            swatch,
                                            SidebarIcon::Glyph(icon),
                                            locked == Some(true),
                                            locked.is_some(),
                                            None,
                                            tooltip,
                                        )
                                        .clicked()
                                        {
                                            actions.push(UiAction::ToggleAspectLock);
                                        }
                                        used += swatch + spacing;

                                        if matches!(
                                            state.ocr_status,
                                            crate::handwriting_ocr::OcrStatus::Ready { .. }
                                        ) {
                                            if sidebar_button(
                                                ui,
                                                swatch,
                                                SidebarIcon::Text("✍T"),
                                                false,
                                                locked.is_some(),
                                                None,
                                                t!("Ausgewählte Handschrift in Text umwandeln"),
                                            )
                                            .clicked()
                                            {
                                                actions.push(UiAction::ConvertSelectionToText);
                                            }
                                            used += swatch + spacing;
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
                                            ("📋", t!("Kopieren (Strg+C)"), UiAction::CopySelected),
                                            ("📥", t!("Einfügen (Strg+V)"), UiAction::PasteSelected),
                                            ("🔄", t!("Um 90° drehen (R)"), UiAction::RotateSelected),
                                            ("🗑", t!("Löschen (Entf)"), UiAction::DeleteSelected),
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
                                        t!("Rückgängig (Strg+Z)"),
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
                                        t!("Wiederholen (Strg+Y)"),
                                    )
                                    .clicked()
                                    {
                                        actions.push(UiAction::Redo);
                                    }

                                }
                                sidebar_divider(ui, btn);

                                // --- Seitennavigation ---
                                // In der durchgehenden Ansicht wird gescrollt; Blätterpfeile gibt
                                // es nur in der Einzelseitenansicht
                                let paging = !state.continuous_scroll && !state.endless;
                                if paging
                                    && sidebar_button(
                                        ui,
                                        btn,
                                        SidebarIcon::Glyph("⏶"),
                                        false,
                                        notebook.current_page_index > 0,
                                        None,
                                        t!("Vorherige Seite (Bild auf)"),
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
                                    !state.endless,
                                    None,
                                    t!("Seite wählen"),
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
                                        t!("Nächste Seite (Bild ab)"),
                                    )
                                    .clicked()
                                {
                                    actions.push(UiAction::NextPage);
                                }
                                if !state.view_mode
                                    && sidebar_button(
                                    ui,
                                    btn,
                                    SidebarIcon::Glyph("➕"),
                                    false,
                                    !state.endless,
                                    None,
                                    t!("Neue Seite nach dieser einfügen"),
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
                                        t!("Weitere Funktionen – Update verfügbar")
                                    } else {
                                        t!("Weitere Funktionen: PDF, Hintergrund, Zoom, Einstellungen")
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

    SidebarAnchors {
        toolbar_rect: area_response.inner,
        pen_btn_rect,
        eraser_btn_rect,
        high_btn_rect,
        text_color_btn_rect,
        stamps_btn_rect,
        more_btn_rect,
        page_btn_rect,
    }
}
