//! Objekt-Auswahl und Manipulation (Selektion, Verschieben, Drehen, Skalieren, Kopieren, Einfügen, Umfärben).
//!
//! Stellt transaktionale Manipulationen für einzelne oder mehrere Striche und geometrische Formen bereit.

use glam::Vec2;
use std::f32::consts::FRAC_PI_2;

use crate::canvas::CanvasState;
use crate::document::{Aabb, Page, Stroke};
use crate::t;

/// Status des aktiven Ziehvorgangs (Drag-Operation) mit der Maus oder dem Stylus.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum DragMode {
    #[default]
    None,
    /// Aufziehen eines Auswahlrahmens (Marquee Box)
    BoxSelect {
        start_page: Vec2,
        current_page: Vec2,
    },
    /// Verschieben ausgewählter Objekte
    Move {
        start_page: Vec2,
        initial_strokes: Vec<Stroke>,
    },
    /// Drehen ausgewählter Objekte um das Zentrum der Auswahl
    Rotate {
        center_page: Vec2,
        start_angle: f32,
        initial_strokes: Vec<Stroke>,
    },
    /// Skalieren ausgewählter Objekte relativ zum gegenüberliegenden Eckpunkt
    Scale {
        fixed_corner_page: Vec2,
        start_pos_page: Vec2,
        initial_box_size: Vec2,
        initial_strokes: Vec<Stroke>,
    },
}

/// Ergebnis eines Hit-Tests auf Auswahl-Griffe (Handles).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SelectionHandleHit {
    None,
    /// Dreh-Griff (oberhalb der Auswahlbox)
    RotateHandle {
        center_page: Vec2,
    },
    /// Einer der 4 Eck-Skalierungsgriffe (0: oben-links, 1: oben-rechts, 2: unten-rechts, 3: unten-links)
    CornerHandle {
        corner_idx: usize,
        fixed_corner_page: Vec2,
        start_pos_page: Vec2,
        initial_box_size: Vec2,
    },
    /// Klick in den Körper / Innenbereich der Auswahlbox
    Body,
}

/// Verwaltet den Zustand der Objektauswahl, des Clipboards und der Manipulationen.
#[derive(Debug, Clone, Default)]
pub struct SelectionState {
    pub selected_ids: Vec<u64>,
    pub drag_mode: DragMode,
    pub clipboard: Vec<Stroke>,
}

impl SelectionState {
    pub fn new() -> Self {
        Self {
            selected_ids: Vec::new(),
            drag_mode: DragMode::None,
            clipboard: Vec::new(),
        }
    }

    /// Prüft, ob aktuell Objekte ausgewählt sind.
    pub fn is_empty(&self) -> bool {
        self.selected_ids.is_empty()
    }

    /// Anzahl der aktuell ausgewählten Objekte.
    pub fn count(&self) -> usize {
        self.selected_ids.len()
    }

    /// Leert die aktuelle Auswahl.
    pub fn clear(&mut self) {
        self.selected_ids.clear();
        self.drag_mode = DragMode::None;
    }

    /// Wählt ein einzelnes Objekt aus.
    pub fn select_single(&mut self, id: u64) {
        self.selected_ids = vec![id];
    }

    /// Schaltet die Auswahl eines Objekts um (z. B. bei Shift+Klick).
    pub fn toggle_select(&mut self, id: u64) {
        if let Some(pos) = self.selected_ids.iter().position(|&x| x == id) {
            self.selected_ids.remove(pos);
        } else {
            self.selected_ids.push(id);
        }
    }

    /// Setzt die Auswahl auf eine Liste von Objekt-IDs.
    pub fn select_multiple(&mut self, ids: Vec<u64>) {
        self.selected_ids = ids;
    }

    /// Berechnet die gemeinsame AABB aller ausgewählten Objekte.
    pub fn get_selection_aabb(&self, page: &Page) -> Option<Aabb> {
        let strokes = page.get_strokes(&self.selected_ids);
        Page::selection_aabb(&strokes)
    }

    /// Führt Hit-Testing auf die Auswahlgriffe (Drehen, Ecken, Körper) in Bildschirmkoordinaten
    /// (physische Pixel) durch. Abstände und Trefferradien sind in logischen Punkten festgelegt
    /// und werden mit `pixels_per_point` skaliert, damit sie zur gezeichneten Überlagerung passen.
    pub fn hit_test_handles(
        &self,
        page: &Page,
        canvas: &CanvasState,
        screen_pos: Vec2,
        pixels_per_point: f32,
    ) -> SelectionHandleHit {
        let Some(aabb) = self.get_selection_aabb(page) else {
            return SelectionHandleHit::None;
        };

        let p_min = canvas.page_to_screen(aabb.min);
        let p_max = canvas.page_to_screen(aabb.max);

        let screen_min = Vec2::new(p_min[0].min(p_max[0]), p_min[1].min(p_max[1]));
        let screen_max = Vec2::new(p_min[0].max(p_max[0]), p_min[1].max(p_max[1]));
        let screen_center = (screen_min + screen_max) * 0.5;
        let page_center = (aabb.min + aabb.max) * 0.5;

        // 1. Drehgriff testen (oberhalb des oberen Randes, wie gezeichnet)
        let ppp = pixels_per_point;
        let rot_screen = Vec2::new(screen_center.x, screen_min.y - ROTATE_HANDLE_OFFSET * ppp);
        if (screen_pos - rot_screen).length() <= HANDLE_HIT_RADIUS * ppp {
            return SelectionHandleHit::RotateHandle {
                center_page: page_center,
            };
        }

        // 2. 4 Eck-Skalierungsgriffe testen
        let corners_page = [
            aabb.min,                          // 0: oben-links
            Vec2::new(aabb.max.x, aabb.min.y), // 1: oben-rechts
            aabb.max,                          // 2: unten-rechts
            Vec2::new(aabb.min.x, aabb.max.y), // 3: unten-links
        ];
        let fixed_corners_page = [
            aabb.max,                          // gegenüber 0 -> unten-rechts
            Vec2::new(aabb.min.x, aabb.max.y), // gegenüber 1 -> unten-links
            aabb.min,                          // gegenüber 2 -> oben-links
            Vec2::new(aabb.max.x, aabb.min.y), // gegenüber 3 -> oben-rechts
        ];

        let corners_screen = [
            screen_min,
            Vec2::new(screen_max.x, screen_min.y),
            screen_max,
            Vec2::new(screen_min.x, screen_max.y),
        ];

        for i in 0..4 {
            if (screen_pos - corners_screen[i]).length() <= HANDLE_HIT_RADIUS * ppp {
                return SelectionHandleHit::CornerHandle {
                    corner_idx: i,
                    fixed_corner_page: fixed_corners_page[i],
                    start_pos_page: corners_page[i],
                    initial_box_size: aabb.max - aabb.min,
                };
            }
        }

        // 3. Körper-Test
        let margin = 6.0;
        if screen_pos.x >= screen_min.x - margin
            && screen_pos.x <= screen_max.x + margin
            && screen_pos.y >= screen_min.y - margin
            && screen_pos.y <= screen_max.y + margin
        {
            return SelectionHandleHit::Body;
        }

        SelectionHandleHit::None
    }

    /// Kopiert die aktuell ausgewählten Objekte in das Clipboard.
    pub fn copy_to_clipboard(&mut self, page: &Page) -> usize {
        let strokes = page.get_strokes(&self.selected_ids);
        let count = strokes.len();
        self.clipboard = strokes;
        count
    }

    /// Fügt Objekte aus dem Clipboard mit Versatz (+30 Einheiten) ein, vergibt neue IDs und wählt sie aus.
    pub fn paste_from_clipboard(
        &mut self,
        page: &mut Page,
        at: Option<Vec2>,
        next_id_gen: &mut impl FnMut() -> u64,
    ) -> Vec<u64> {
        if self.clipboard.is_empty() {
            return Vec::new();
        }

        // Mit `at` (lange gedrückt → Einfügen) landet die Mitte der Objekte dort, sonst
        // ca. 3 mm nach rechts unten versetzt
        let offset = match (at, Page::selection_aabb(&self.clipboard)) {
            (Some(at), Some(aabb)) => at - (aabb.min + aabb.max) / 2.0,
            _ => Vec2::new(30.0, 30.0),
        };
        let mut new_strokes = Vec::with_capacity(self.clipboard.len());
        let mut new_ids = Vec::with_capacity(self.clipboard.len());

        for stroke in &self.clipboard {
            let new_id = next_id_gen();
            let mut cloned = stroke.clone();
            cloned.id = new_id;
            cloned.translate(offset);
            new_ids.push(new_id);
            new_strokes.push(cloned);
        }

        page.add_strokes(new_strokes);
        self.selected_ids = new_ids.clone();
        new_ids
    }

    /// Löscht die ausgewählten Objekte transaktional aus der Seite.
    pub fn delete_selected(&mut self, page: &mut Page) -> Vec<Stroke> {
        if self.selected_ids.is_empty() {
            return Vec::new();
        }
        let removed = page.remove_strokes_by_ids(&self.selected_ids);
        self.selected_ids.clear();
        self.drag_mode = DragMode::None;
        removed
    }

    /// Färbt alle ausgewählten Objekte auf `new_color` um (inkl. Vertex-Buffer).
    pub fn recolor_selected(&mut self, page: &mut Page, new_color: [f32; 4]) -> bool {
        if self.selected_ids.is_empty() {
            return false;
        }
        // Bilder behalten ihre Farben
        let mut strokes: Vec<_> = page
            .get_strokes(&self.selected_ids)
            .into_iter()
            .filter(|s| !s.is_image())
            .collect();
        if strokes.is_empty() {
            return false;
        }

        for s in &mut strokes {
            s.set_color(new_color);
        }

        page.modify_strokes(strokes);
        true
    }

    /// Dreht alle ausgewählten Objekte um 90° im Uhrzeigersinn um das Zentrum ihrer AABB.
    pub fn rotate_selected_90_deg(&mut self, page: &mut Page) -> bool {
        let Some(aabb) = self.get_selection_aabb(page) else {
            return false;
        };
        let center = (aabb.min + aabb.max) * 0.5;
        let mut strokes = page.get_strokes(&self.selected_ids);
        if strokes.is_empty() {
            return false;
        }

        for s in &mut strokes {
            s.rotate(center, FRAC_PI_2);
        }

        page.modify_strokes(strokes);
        true
    }
}

/// Rendert das Auswahl-Overlay (Bounding Box, Ecken, Dreh-Griff, Marquee Box) über egui.
/// Abstand des Drehgriffs über dem Auswahlrahmen (logische Punkte).
const ROTATE_HANDLE_OFFSET: f32 = 24.0;
/// Trefferradius der Griffe (logische Punkte), groß genug für Finger.
const HANDLE_HIT_RADIUS: f32 = 18.0;

/// Wandelt Fensterpixel (physisch) in egui-Koordinaten (logische Punkte) um.
fn to_ui(screen: [f32; 2], pixels_per_point: f32) -> egui::Pos2 {
    egui::pos2(screen[0] / pixels_per_point, screen[1] / pixels_per_point)
}

pub fn render_selection_overlay(
    ctx: &egui::Context,
    selection: &SelectionState,
    page: &Page,
    canvas: &CanvasState,
) {
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("selection_overlay_layer"),
    ));
    // Fensterpixel sind physisch, egui zeichnet in logischen Punkten
    let ppp = ctx.pixels_per_point();

    // 1. Wenn aktuell ein Auswahlrahmen gezogen wird (Marquee Box)
    if let DragMode::BoxSelect {
        start_page,
        current_page,
    } = selection.drag_mode
    {
        let p1 = to_ui(canvas.page_to_screen(start_page), ppp);
        let p2 = to_ui(canvas.page_to_screen(current_page), ppp);
        let min_x = p1[0].min(p2[0]);
        let min_y = p1[1].min(p2[1]);
        let max_x = p1[0].max(p2[0]);
        let max_y = p1[1].max(p2[1]);

        let rect = egui::Rect::from_min_max(egui::pos2(min_x, min_y), egui::pos2(max_x, max_y));

        // Halbtransparente Füllung & blauer Auswahlrand
        painter.rect_filled(
            rect,
            egui::Rounding::same(4.0_f32),
            egui::Color32::from_rgba_unmultiplied(40, 120, 240, 35),
        );
        painter.rect_stroke(
            rect,
            egui::Rounding::same(4.0_f32),
            egui::Stroke::new(
                1.5_f32,
                egui::Color32::from_rgba_unmultiplied(80, 160, 255, 220),
            ),
        );
        return;
    }

    // 2. Wenn Objekte ausgewählt sind, Bounding Box und Griffe zeichnen
    let Some(aabb) = selection.get_selection_aabb(page) else {
        return;
    };

    let p_min = to_ui(canvas.page_to_screen(aabb.min), ppp);
    let p_max = to_ui(canvas.page_to_screen(aabb.max), ppp);

    let screen_min = egui::pos2(p_min[0].min(p_max[0]), p_min[1].min(p_max[1]));
    let screen_max = egui::pos2(p_min[0].max(p_max[0]), p_min[1].max(p_max[1]));
    let rect = egui::Rect::from_min_max(screen_min, screen_max);

    // Haupt-Auswahlrahmen (leuchtend blau)
    let border_color = egui::Color32::from_rgb(70, 150, 255);
    painter.rect_stroke(
        rect,
        egui::Rounding::same(3.0_f32),
        egui::Stroke::new(1.5_f32, border_color),
    );

    // Dreh-Stängel (Stalk) und Drehgriff
    let top_center = egui::pos2(rect.center().x, rect.min.y);
    let rot_handle = egui::pos2(rect.center().x, rect.min.y - ROTATE_HANDLE_OFFSET);

    painter.line_segment(
        [top_center, rot_handle],
        egui::Stroke::new(1.2_f32, border_color),
    );

    // Drehgriff-Kreis
    painter.circle_filled(rot_handle, 6.0_f32, egui::Color32::WHITE);
    painter.circle_stroke(
        rot_handle,
        6.0_f32,
        egui::Stroke::new(1.5_f32, egui::Color32::from_rgb(40, 90, 180)),
    );

    // 4 Eck-Skalierungsgriffe (kleine weiße Quadrate mit blauem Rand)
    let handle_size = 7.0_f32;
    let corners = [
        rect.min,
        egui::pos2(rect.max.x, rect.min.y),
        rect.max,
        egui::pos2(rect.min.x, rect.max.y),
    ];

    for c in corners {
        let handle_rect = egui::Rect::from_center_size(c, egui::vec2(handle_size, handle_size));
        painter.rect_filled(
            handle_rect,
            egui::Rounding::same(1.5_f32),
            egui::Color32::WHITE,
        );
        painter.rect_stroke(
            handle_rect,
            egui::Rounding::same(1.5_f32),
            egui::Stroke::new(1.2_f32, egui::Color32::from_rgb(30, 80, 170)),
        );
    }

    // Kleines Info-Badge an der Unterkante (z. B. "2 Objekte | [R] Drehen [Entf] Löschen")
    let badge_text = if selection.count() == 1 {
        t!("1 Objekt | [R] Drehen [Entf] Löschen").to_string()
    } else {
        t!(
            "{count} Objekte | [R] Drehen [Entf] Löschen",
            count = selection.count()
        )
    };
    let badge_pos = egui::pos2(rect.center().x, rect.max.y + 16.0);
    painter.text(
        badge_pos,
        egui::Align2::CENTER_CENTER,
        badge_text,
        egui::FontId::proportional(11.0),
        egui::Color32::from_gray(220),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::InkVertex;
    use crate::document::{PageBackground, StylusSample};

    fn make_test_stroke(id: u64, x: f32, y: f32, color: [f32; 4]) -> Stroke {
        let s1 = StylusSample {
            position: Vec2::new(x, y),
            pressure: 0.5,
            timestamp: 0.0,
        };
        let s2 = StylusSample {
            position: Vec2::new(x + 50.0, y + 50.0),
            pressure: 0.5,
            timestamp: 0.01,
        };
        let v1 = InkVertex::new(x, y, color);
        let v2 = InkVertex::new(x + 50.0, y + 50.0, color);
        Stroke::new(id, vec![s1, s2], vec![v1, v2], color)
    }

    #[test]
    fn test_selection_copy_paste_delete() {
        let mut page = Page::new(1, PageBackground::Blank);
        let s1 = make_test_stroke(1, 100.0, 100.0, [0.1, 0.1, 0.1, 1.0]);
        let s2 = make_test_stroke(2, 300.0, 300.0, [0.8, 0.2, 0.2, 1.0]);
        page.add_stroke(s1);
        page.add_stroke(s2);

        let mut sel = SelectionState::new();
        sel.select_single(1);
        assert_eq!(sel.count(), 1);

        // Copy
        let copied = sel.copy_to_clipboard(&page);
        assert_eq!(copied, 1);

        // Paste
        let mut next_id = 100;
        let new_ids = sel.paste_from_clipboard(&mut page, None, &mut || {
            next_id += 1;
            next_id
        });
        assert_eq!(new_ids.len(), 1);
        assert_eq!(page.strokes.len(), 3);
        assert_eq!(sel.selected_ids, new_ids);

        // An einer Stelle einfügen: die Mitte der Kopie liegt dort
        let target = Vec2::new(1000.0, 1500.0);
        let placed = sel.paste_from_clipboard(&mut page, Some(target), &mut || {
            next_id += 1;
            next_id
        });
        let aabb = sel.get_selection_aabb(&page).unwrap();
        assert_eq!(sel.selected_ids, placed);
        assert!(((aabb.min + aabb.max) / 2.0 - target).length() < 0.01);
        sel.delete_selected(&mut page);
        sel.selected_ids = new_ids.clone();

        // Delete
        let deleted = sel.delete_selected(&mut page);
        assert_eq!(deleted.len(), 1);
        assert_eq!(page.strokes.len(), 2);
        assert!(sel.is_empty());
    }

    #[test]
    fn test_selection_recolor_and_rotate() {
        let mut page = Page::new(1, PageBackground::Blank);
        let s1 = make_test_stroke(1, 200.0, 200.0, [0.1, 0.1, 0.1, 1.0]);
        page.add_stroke(s1);

        let mut sel = SelectionState::new();
        sel.select_single(1);

        // Recolor
        let new_color = [0.0, 1.0, 0.0, 1.0];
        assert!(sel.recolor_selected(&mut page, new_color));
        assert_eq!(page.strokes[0].color, new_color);
        assert_eq!(page.strokes[0].vertices[0].color, new_color);

        // Undo recolor
        assert!(page.undo());
        assert_eq!(page.strokes[0].color, [0.1, 0.1, 0.1, 1.0]);

        // Rotate 90 deg
        let orig_pos = page.strokes[0].points[0].position;
        assert!(sel.rotate_selected_90_deg(&mut page));
        let rotated_pos = page.strokes[0].points[0].position;
        assert!(
            (orig_pos.x - rotated_pos.x).abs() > 0.1 || (orig_pos.y - rotated_pos.y).abs() > 0.1
        );

        // Undo rotate
        assert!(page.undo());
        let restored_pos = page.strokes[0].points[0].position;
        assert!((orig_pos.x - restored_pos.x).abs() < 1e-4);
    }

    #[test]
    fn test_recolor_ignores_images() {
        let mut page = Page::new(1, PageBackground::Blank);
        let corners = [
            Vec2::new(0.0, 0.0),
            Vec2::new(100.0, 0.0),
            Vec2::new(100.0, 50.0),
            Vec2::new(0.0, 50.0),
        ];
        page.add_stroke(Stroke::new_image(1, "a.png".into(), corners));
        let undo_depth = page.history.undo_stack.len();

        let mut sel = SelectionState::new();
        sel.select_single(1);
        // Nur ein Bild ausgewählt: nichts umzufärben, kein Undo-Eintrag
        assert!(!sel.recolor_selected(&mut page, [1.0, 0.0, 0.0, 1.0]));
        assert_eq!(page.history.undo_stack.len(), undo_depth);

        // Bild und Strich: nur der Strich wird umgefärbt
        page.add_stroke(make_test_stroke(2, 20.0, 20.0, [0.1, 0.1, 0.1, 1.0]));
        sel.select_multiple(vec![1, 2]);
        assert!(sel.recolor_selected(&mut page, [1.0, 0.0, 0.0, 1.0]));
        assert_eq!(page.strokes[0].color, [1.0, 1.0, 1.0, 1.0]);
        assert_eq!(page.strokes[1].color, [1.0, 0.0, 0.0, 1.0]);
    }
}
