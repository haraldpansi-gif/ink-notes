//! Notizbuch- und Seitenverwaltung mit DIN-A4-Koordinatensystem und Vektor-Stroke-Modell.

use std::ops::Range;

use crate::canvas::InkVertex;
use crate::history::{HistoryAction, HistoryStack};
pub use crate::spline::{finalize_stroke, finalize_stroke_with_config};

/// Standardmaße DIN A4 in Millimetern.
pub const A4_WIDTH_MM: f32 = 210.0;
pub const A4_HEIGHT_MM: f32 = 297.0;
/// Seitenverhältnis DIN A4 (1 / sqrt(2) ≈ 0.7071).
pub const A4_ASPECT_RATIO: f32 = A4_WIDTH_MM / A4_HEIGHT_MM;

/// Virtuelle Seitenauflösung in internen Einheiten (10 Einheiten = 1 mm).
pub const PAGE_WIDTH: f32 = 2100.0;
pub const PAGE_HEIGHT: f32 = 2970.0;
pub const UNITS_PER_MM: f32 = 10.0;

/// Ein gefilterter Stylus-Abtastpunkt in DIN-A4-Seitenkoordinaten.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StylusSample {
    pub position: glam::Vec2,
    pub pressure: f32,
    pub timestamp: f64,
}

/// 2D Axis-Aligned Bounding Box (AABB) für schnellen Kollisionsausschluss.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Aabb {
    pub min: glam::Vec2,
    pub max: glam::Vec2,
}

impl Aabb {
    pub fn new(min: glam::Vec2, max: glam::Vec2) -> Self {
        Self { min, max }
    }

    pub fn from_point(p: glam::Vec2) -> Self {
        Self { min: p, max: p }
    }

    pub fn from_points(points: &[glam::Vec2]) -> Self {
        if points.is_empty() {
            return Self::new(glam::Vec2::ZERO, glam::Vec2::ZERO);
        }
        let mut min = points[0];
        let mut max = points[0];
        for &p in &points[1..] {
            min = min.min(p);
            max = max.max(p);
        }
        Self { min, max }
    }

    pub fn expand_by_point(&mut self, p: glam::Vec2) {
        self.min = self.min.min(p);
        self.max = self.max.max(p);
    }

    /// Vergrößert die Bounding Box allseitig um einen Radius `r`.
    pub fn expanded(&self, radius: f32) -> Self {
        let r = glam::Vec2::splat(radius);
        Self {
            min: self.min - r,
            max: self.max + r,
        }
    }

    /// Prüft, ob ein Punkt innerhalb der Bounding Box liegt.
    pub fn contains_point(&self, p: glam::Vec2) -> bool {
        p.x >= self.min.x && p.x <= self.max.x && p.y >= self.min.y && p.y <= self.max.y
    }

    /// Prüft, ob zwei Bounding Boxes überlappen.
    pub fn intersects(&self, other: &Aabb) -> bool {
        self.min.x <= other.max.x
            && self.max.x >= other.min.x
            && self.min.y <= other.max.y
            && self.max.y >= other.min.y
    }

    /// Kombiniert diese Bounding Box mit einer anderen zu einer minimalen Hüllbox.
    pub fn union(&self, other: &Aabb) -> Self {
        Self {
            min: self.min.min(other.min),
            max: self.max.max(other.max),
        }
    }
}

/// Berechnet den minimalen Abstand eines Punktes `p` zu einer Strecke zwischen `a` und `b`.
pub fn point_to_segment_distance(p: glam::Vec2, a: glam::Vec2, b: glam::Vec2) -> f32 {
    let ab = b - a;
    let len_sq = ab.length_squared();
    if len_sq < 1e-6 {
        return (p - a).length();
    }
    // Orthogonale Projektion t ∈ [0, 1]
    let t = ((p - a).dot(ab) / len_sq).clamp(0.0, 1.0);
    let projection = a + t * ab;
    (p - projection).length()
}

/// Ein einzelner Vektor-Strich auf einer Seite.
#[derive(Debug, Clone, PartialEq)]
pub struct Stroke {
    pub id: u64,
    pub points: Vec<StylusSample>,
    pub vertices: Vec<InkVertex>,
    pub aabb: Aabb,
    pub color: [f32; 4],
    pub shape: Option<crate::geometry::RecognizedShape>,
}

impl Stroke {
    pub fn new(
        id: u64,
        points: Vec<StylusSample>,
        vertices: Vec<InkVertex>,
        color: [f32; 4],
    ) -> Self {
        let aabb = if !points.is_empty() {
            let pts: Vec<glam::Vec2> = points.iter().map(|s| s.position).collect();
            Aabb::from_points(&pts)
        } else if !vertices.is_empty() {
            let pts: Vec<glam::Vec2> = vertices
                .iter()
                .map(|v| glam::Vec2::new(v.position[0], v.position[1]))
                .collect();
            Aabb::from_points(&pts)
        } else {
            Aabb::new(glam::Vec2::ZERO, glam::Vec2::ZERO)
        };

        Self {
            id,
            points,
            vertices,
            aabb,
            color,
            shape: None,
        }
    }

    /// Finalisiert rohe Stylus-Samples mit akausalem Post-Processing (Zwei-Stufen-Modell).
    pub fn finalize(
        id: u64,
        raw_samples: &[StylusSample],
        color: [f32; 4],
        config: &crate::spline::SplineConfig,
    ) -> Self {
        crate::spline::finalize_stroke_with_config(id, raw_samples, color, config)
    }

    /// Verknüpft diesen Strich mit einer mathematisch erkannten Form.
    pub fn with_shape(mut self, shape: crate::geometry::RecognizedShape) -> Self {
        self.shape = Some(shape);
        self
    }

    /// Berechnet den minimalen euklidischen Abstand eines Punktes zu diesem Strich.
    pub fn distance_to_point(&self, p: glam::Vec2) -> f32 {
        if self.points.is_empty() {
            return f32::INFINITY;
        }
        if self.points.len() == 1 {
            return (p - self.points[0].position).length();
        }
        let mut min_dist = f32::INFINITY;
        for window in self.points.windows(2) {
            let d = point_to_segment_distance(p, window[0].position, window[1].position);
            if d < min_dist {
                min_dist = d;
            }
        }
        min_dist
    }

    /// Prüft, ob dieser Strich von einem Radierer an Position `eraser_pos` mit Radius `eraser_radius` getroffen wird.
    pub fn intersects_eraser(
        &self,
        eraser_pos: glam::Vec2,
        eraser_radius: f32,
        max_stroke_radius: f32,
    ) -> bool {
        // 1. Vorabtest: Schneidet der Radierer die vergrößerte AABB?
        let total_margin = eraser_radius + max_stroke_radius;
        if !self.aabb.expanded(total_margin).contains_point(eraser_pos) {
            return false;
        }

        // 2. Punkt-zu-Strecke-Distanztest entlang aller Segmente
        if self.points.len() < 2 {
            if let Some(first) = self.points.first() {
                let dist = (eraser_pos - first.position).length();
                return dist <= total_margin;
            }
            return false;
        }

        for i in 0..self.points.len() - 1 {
            let p1 = self.points[i].position;
            let p2 = self.points[i + 1].position;
            let dist = point_to_segment_distance(eraser_pos, p1, p2);
            if dist <= total_margin {
                return true;
            }
        }

        false
    }

    /// Berechnet die Bounding Box aus den aktuellen Punkten/Vertizes neu.
    pub fn recompute_aabb(&mut self) {
        self.aabb = if !self.points.is_empty() {
            let pts: Vec<glam::Vec2> = self.points.iter().map(|s| s.position).collect();
            Aabb::from_points(&pts)
        } else if !self.vertices.is_empty() {
            let pts: Vec<glam::Vec2> = self
                .vertices
                .iter()
                .map(|v| glam::Vec2::new(v.position[0], v.position[1]))
                .collect();
            Aabb::from_points(&pts)
        } else {
            Aabb::new(glam::Vec2::ZERO, glam::Vec2::ZERO)
        };
    }

    /// Verschiebt den Strich um `delta` (DIN-A4-Einheiten).
    pub fn translate(&mut self, delta: glam::Vec2) {
        for pt in &mut self.points {
            pt.position += delta;
        }
        for v in &mut self.vertices {
            v.position[0] += delta.x;
            v.position[1] += delta.y;
        }
        if let Some(ref mut shape) = self.shape {
            shape.translate(delta);
        }
        self.recompute_aabb();
    }

    /// Dreht den Strich um `origin` mit dem Winkel `angle_rad`.
    pub fn rotate(&mut self, origin: glam::Vec2, angle_rad: f32) {
        let (sin, cos) = angle_rad.sin_cos();
        let rot = |pos: glam::Vec2| -> glam::Vec2 {
            let diff = pos - origin;
            origin + glam::Vec2::new(diff.x * cos - diff.y * sin, diff.x * sin + diff.y * cos)
        };

        for pt in &mut self.points {
            pt.position = rot(pt.position);
        }
        for v in &mut self.vertices {
            let rotated = rot(glam::Vec2::new(v.position[0], v.position[1]));
            v.position = [rotated.x, rotated.y];
        }
        if let Some(ref mut shape) = self.shape {
            shape.rotate(origin, angle_rad);
        }
        self.recompute_aabb();
    }

    /// Skaliert den Strich bezüglich `origin` um die Faktoren `factors`.
    pub fn scale(&mut self, origin: glam::Vec2, factors: glam::Vec2) {
        let sc = |pos: glam::Vec2| -> glam::Vec2 { origin + (pos - origin) * factors };

        for pt in &mut self.points {
            pt.position = sc(pt.position);
        }
        for v in &mut self.vertices {
            let scaled = sc(glam::Vec2::new(v.position[0], v.position[1]));
            v.position = [scaled.x, scaled.y];
        }
        if let Some(ref mut shape) = self.shape {
            shape.scale(origin, factors);
        }
        self.recompute_aabb();
    }

    /// Weist diesem Strich eine neue Farbe zu und aktualisiert alle Vertex-Farben.
    pub fn set_color(&mut self, new_color: [f32; 4]) {
        self.color = new_color;
        for v in &mut self.vertices {
            v.color = new_color;
        }
    }
}

/// Hintergrundtyp für eine DIN-A4-Seite.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PageBackground {
    /// Komplett blanko (Weiß)
    Blank,
    /// 5 mm Karomuster
    Grid5mm,
    /// Linierte Seite mit 7 mm Zeilenabstand
    Lined,
    /// Punktraster im 5 mm Abstand
    DotGrid,
}

/// Eine einzelne Notizbuchseite mit Strichen, Hintergrund und gecachten Meshes.
#[derive(Debug, Clone)]
pub struct Page {
    pub id: u64,
    pub strokes: Vec<Stroke>,
    pub background: PageBackground,
    pub vertex_cache: Vec<InkVertex>,
    pub stroke_ranges: Vec<Range<u32>>,
    pub is_dirty: bool,
    pub is_loaded: bool,
    pub history: HistoryStack,
}

impl Page {
    pub fn new(id: u64, background: PageBackground) -> Self {
        Self {
            id,
            strokes: Vec::new(),
            background,
            vertex_cache: Vec::new(),
            stroke_ranges: Vec::new(),
            is_dirty: false,
            is_loaded: true,
            history: HistoryStack::new(),
        }
    }

    /// Erzeugt einen noch nicht aus dem Speicher geladenen Seiten-Platzhalter (Lazy Loading).
    pub fn new_unloaded(id: u64, background: PageBackground) -> Self {
        Self {
            id,
            strokes: Vec::new(),
            background,
            vertex_cache: Vec::new(),
            stroke_ranges: Vec::new(),
            is_dirty: false,
            is_loaded: false,
            history: HistoryStack::new(),
        }
    }

    /// Fügt einen neuen Strich hinzu und registriert ihn im History-Stack.
    pub fn add_stroke(&mut self, stroke: Stroke) {
        self.history.push(HistoryAction::AddStroke(stroke.clone()));
        self.strokes.push(stroke);
        self.is_dirty = true;
    }

    /// Ersetzt einen vorhandenen Strich (z. B. durch eine erkannte geometrische Form)
    /// und hinterlegt die Aktion für Undo/Redo im History-Stack.
    pub fn replace_stroke(&mut self, old_id: u64, new_stroke: Stroke) -> bool {
        if let Some(pos) = self.strokes.iter().position(|s| s.id == old_id) {
            let old = self.strokes[pos].clone();
            self.history.push(HistoryAction::ReplaceStroke {
                old,
                new: new_stroke.clone(),
            });
            self.strokes[pos] = new_stroke;
            self.is_dirty = true;
            true
        } else {
            false
        }
    }

    /// Findet den nächsten Strich an Position `pos` innerhalb eines maximalen Suchradius.
    pub fn find_stroke_at(&self, pos: glam::Vec2, max_dist: f32) -> Option<&Stroke> {
        let mut best: Option<(&Stroke, f32)> = None;
        for s in &self.strokes {
            if !s.aabb.expanded(max_dist).contains_point(pos) {
                continue;
            }
            let d = s.distance_to_point(pos);
            if d <= max_dist {
                if let Some((_, best_d)) = best {
                    if d < best_d {
                        best = Some((s, d));
                    }
                } else {
                    best = Some((s, d));
                }
            }
        }
        best.map(|(s, _)| s)
    }

    /// Macht die letzte Aktion rückgängig (Undo).
    pub fn undo(&mut self) -> bool {
        if self.history.undo(&mut self.strokes) {
            self.is_dirty = true;
            true
        } else {
            false
        }
    }

    /// Stellt die letzte rückgängig gemachte Aktion wieder her (Redo).
    pub fn redo(&mut self) -> bool {
        if self.history.redo(&mut self.strokes) {
            self.is_dirty = true;
            true
        } else {
            false
        }
    }

    /// Radiert alle Striche an der Position `eraser_pos` mit Radius `eraser_radius`.
    ///
    /// Gibt die gelöschten Striche zurück (falls welche getroffen wurden).
    pub fn erase_at(
        &mut self,
        eraser_pos: glam::Vec2,
        eraser_radius: f32,
        max_stroke_radius: f32,
    ) -> Vec<Stroke> {
        let mut hit_indices = Vec::new();

        for (i, stroke) in self.strokes.iter().enumerate() {
            if stroke.intersects_eraser(eraser_pos, eraser_radius, max_stroke_radius) {
                hit_indices.push(i);
            }
        }

        if hit_indices.is_empty() {
            return Vec::new();
        }

        // Rückwärts entfernen, um Indexverschiebungen zu vermeiden
        let mut removed = Vec::with_capacity(hit_indices.len());
        for &idx in hit_indices.iter().rev() {
            removed.push(self.strokes.remove(idx));
        }

        removed.reverse();
        self.history
            .push(HistoryAction::RemoveStrokes(removed.clone()));
        self.is_dirty = true;

        removed
    }

    /// Baut den gecachten Vertex-Puffer der Seite neu auf, wenn `is_dirty` gesetzt ist.
    pub fn rebuild_cache_if_dirty(&mut self) {
        if !self.is_dirty {
            return;
        }

        self.vertex_cache.clear();
        self.stroke_ranges.clear();

        for stroke in &self.strokes {
            let start = self.vertex_cache.len() as u32;
            self.vertex_cache.extend_from_slice(&stroke.vertices);
            let end = self.vertex_cache.len() as u32;
            self.stroke_ranges.push(start..end);
        }

        self.is_dirty = false;
    }

    /// Berechnet die kombinierte AABB einer Menge von Strichen.
    pub fn selection_aabb(strokes: &[Stroke]) -> Option<Aabb> {
        strokes.first().map(|first| {
            let mut combined = first.aabb;
            for s in strokes.iter().skip(1) {
                combined = combined.union(&s.aabb);
            }
            combined
        })
    }

    /// Findet die IDs aller Striche, die die gegebene Auswahlbox schneiden oder darin liegen.
    pub fn find_strokes_in_box(&self, box_min: glam::Vec2, box_max: glam::Vec2) -> Vec<u64> {
        let sel_box = Aabb::new(
            glam::Vec2::new(box_min.x.min(box_max.x), box_min.y.min(box_max.y)),
            glam::Vec2::new(box_min.x.max(box_max.x), box_min.y.max(box_max.y)),
        );
        self.strokes
            .iter()
            .filter(|s| s.aabb.intersects(&sel_box))
            .map(|s| s.id)
            .collect()
    }

    /// Liefert Klone der Striche mit den angegebenen IDs.
    pub fn get_strokes(&self, ids: &[u64]) -> Vec<Stroke> {
        self.strokes
            .iter()
            .filter(|s| ids.contains(&s.id))
            .cloned()
            .collect()
    }

    /// Aktualisiert eine Menge geänderter Striche transaktional im History-Stack.
    pub fn modify_strokes(&mut self, modified: Vec<Stroke>) {
        if modified.is_empty() {
            return;
        }
        let ids: Vec<u64> = modified.iter().map(|s| s.id).collect();
        let before: Vec<Stroke> = self
            .strokes
            .iter()
            .filter(|s| ids.contains(&s.id))
            .cloned()
            .collect();

        for m in modified.iter() {
            if let Some(pos) = self.strokes.iter().position(|s| s.id == m.id) {
                self.strokes[pos] = m.clone();
            }
        }

        self.history.push(HistoryAction::ModifyStrokes {
            before,
            after: modified,
        });
        self.is_dirty = true;
    }

    /// Aktualisiert Striche flüchtig (z. B. während des Ziehens / Dragging), OHNE die History zu verändern.
    pub fn update_strokes_ephemeral(&mut self, modified: &[Stroke]) {
        for m in modified {
            if let Some(pos) = self.strokes.iter().position(|s| s.id == m.id) {
                self.strokes[pos] = m.clone();
            }
        }
        self.is_dirty = true;
    }

    /// Schließt eine Transformationsoperation transaktional mit vorherigem und finalem Zustand ab.
    pub fn commit_modified_strokes(&mut self, before: Vec<Stroke>, after: Vec<Stroke>) {
        if before.is_empty() || after.is_empty() {
            return;
        }
        for m in &after {
            if let Some(pos) = self.strokes.iter().position(|s| s.id == m.id) {
                self.strokes[pos] = m.clone();
            }
        }
        self.history
            .push(HistoryAction::ModifyStrokes { before, after });
        self.is_dirty = true;
    }

    /// Entfernt Striche anhand ihrer IDs und hinterlegt die Aktion im History-Stack.
    pub fn remove_strokes_by_ids(&mut self, ids: &[u64]) -> Vec<Stroke> {
        let mut removed = Vec::new();
        self.strokes.retain(|s| {
            if ids.contains(&s.id) {
                removed.push(s.clone());
                false
            } else {
                true
            }
        });

        if !removed.is_empty() {
            self.history
                .push(HistoryAction::RemoveStrokes(removed.clone()));
            self.is_dirty = true;
        }
        removed
    }

    /// Fügt mehrere Striche hinzu und hinterlegt die Aktion transaktional im History-Stack.
    pub fn add_strokes(&mut self, new_strokes: Vec<Stroke>) {
        if new_strokes.is_empty() {
            return;
        }
        self.history
            .push(HistoryAction::AddStrokes(new_strokes.clone()));
        self.strokes.extend(new_strokes);
        self.is_dirty = true;
    }
}

/// Das Notizbuch mit diskreter Seitenverwaltung und Navigation.
#[derive(Debug, Clone)]
pub struct Notebook {
    pub pages: Vec<Page>,
    pub current_page_index: usize,
    pub next_stroke_id: u64,
    pub next_page_id: u64,
}

impl Notebook {
    pub fn new() -> Self {
        let mut nb = Self {
            pages: Vec::new(),
            current_page_index: 0,
            next_stroke_id: 1,
            next_page_id: 1,
        };
        // Erste Standardseite anlegen (5mm Grid)
        nb.add_page(PageBackground::Grid5mm);
        nb
    }

    /// Erzeugt ein Notizbuch aus vorhandenen Seiten und Zählern (für Session-Wiederherstellung).
    pub fn from_pages(
        pages: Vec<Page>,
        current_page_index: usize,
        next_stroke_id: u64,
        next_page_id: u64,
    ) -> Self {
        let mut nb = Self {
            pages,
            current_page_index,
            next_stroke_id,
            next_page_id,
        };
        if nb.pages.is_empty() {
            nb.add_page(PageBackground::Grid5mm);
        }
        if nb.current_page_index >= nb.pages.len() {
            nb.current_page_index = 0;
        }
        nb
    }

    /// Gibt die aktuell aktive Seite zurück.
    pub fn current_page(&self) -> &Page {
        &self.pages[self.current_page_index]
    }

    /// Gibt die aktuell aktive Seite als veränderliche Referenz zurück.
    pub fn current_page_mut(&mut self) -> &mut Page {
        &mut self.pages[self.current_page_index]
    }

    /// Erzeugt eine eindeutige neue Stroke-ID.
    pub fn next_stroke_id(&mut self) -> u64 {
        let id = self.next_stroke_id;
        self.next_stroke_id += 1;
        id
    }

    /// Fügt eine neue Seite mit dem angegebenen Hintergrund hinzu.
    pub fn add_page(&mut self, background: PageBackground) -> usize {
        let id = self.next_page_id;
        self.next_page_id += 1;
        self.pages.push(Page::new(id, background));
        self.pages.len() - 1
    }

    /// Wechselt zur nächsten Seite (PageDown / N). Hängt automatisch eine neue Seite an, falls am Ende.
    pub fn next_page(&mut self) {
        if self.current_page_index + 1 < self.pages.len() {
            self.current_page_index += 1;
        } else {
            // Neue leere Seite mit gleichem Hintergrund anhängen
            let current_bg = self.current_page().background;
            let new_idx = self.add_page(current_bg);
            self.current_page_index = new_idx;
        }
    }

    /// Wechselt zur vorherigen Seite (PageUp / P).
    pub fn previous_page(&mut self) {
        if self.current_page_index > 0 {
            self.current_page_index -= 1;
        }
    }

    /// Springt zur ersten Seite (Pos1 / Home).
    pub fn first_page(&mut self) {
        self.current_page_index = 0;
    }

    /// Springt zur letzten Seite (Ende / End).
    pub fn last_page(&mut self) {
        if !self.pages.is_empty() {
            self.current_page_index = self.pages.len() - 1;
        }
    }

    /// Text für das On-Screen-Display (OSD), z. B. "Seite 3 / 12".
    pub fn osd_text(&self) -> String {
        format!(
            "Seite {} / {}",
            self.current_page_index + 1,
            self.pages.len()
        )
    }
}

impl Default for Notebook {
    fn default() -> Self {
        Self::new()
    }
}

/// Berechnet die DIN-A4-Darstellung im Fenster mit Letterboxing/Pillarboxing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageViewport {
    pub window_width: f32,
    pub window_height: f32,
    pub page_x: f32,
    pub page_y: f32,
    pub page_width: f32,
    pub page_height: f32,
}

impl PageViewport {
    /// Berechnet die zentrierte DIN-A4-Projektionsfläche unter Beibehaltung des 210x297-Seitenverhältnisses.
    pub fn calculate(window_width: f32, window_height: f32, margin: f32) -> Self {
        let avail_w = (window_width - 2.0 * margin).max(10.0);
        let avail_h = (window_height - 2.0 * margin).max(10.0);

        let window_ratio = avail_w / avail_h;
        let page_ratio = A4_ASPECT_RATIO;

        let (page_w, page_h) = if window_ratio > page_ratio {
            // Fenster ist breiter als DIN A4 -> Pillarboxing (Balken links/rechts)
            let h = avail_h;
            let w = h * page_ratio;
            (w, h)
        } else {
            // Fenster ist höher als DIN A4 -> Letterboxing (Balken oben/unten)
            let w = avail_w;
            let h = w / page_ratio;
            (w, h)
        };

        let page_x = (window_width - page_w) / 2.0;
        let page_y = (window_height - page_h) / 2.0;

        Self {
            window_width,
            window_height,
            page_x,
            page_y,
            page_width: page_w,
            page_height: page_h,
        }
    }

    /// Transformiert einen Fenster-Pixelpunkt in DIN-A4-Seitenkoordinaten [0.0, PAGE_WIDTH] x [0.0, PAGE_HEIGHT].
    ///
    /// Liefert `None`, falls der Punkt außerhalb der DIN-A4-Papierfläche liegt.
    pub fn screen_to_page(&self, screen_x: f32, screen_y: f32) -> Option<glam::Vec2> {
        let u = (screen_x - self.page_x) / self.page_width;
        let v = (screen_y - self.page_y) / self.page_height;

        if (0.0..=1.0).contains(&u) && (0.0..=1.0).contains(&v) {
            Some(glam::Vec2::new(u * PAGE_WIDTH, v * PAGE_HEIGHT))
        } else {
            None
        }
    }

    /// Transformiert einen Fenster-Pixelpunkt und klemmt ihn auf die DIN-A4-Papierfläche.
    pub fn screen_to_page_clamped(&self, screen_x: f32, screen_y: f32) -> glam::Vec2 {
        let u = ((screen_x - self.page_x) / self.page_width).clamp(0.0, 1.0);
        let v = ((screen_y - self.page_y) / self.page_height).clamp(0.0, 1.0);
        glam::Vec2::new(u * PAGE_WIDTH, v * PAGE_HEIGHT)
    }

    /// Berechnet die Shader-Transformationsparameter (Scale, Offset),
    /// um DIN-A4-Seitenkoordinaten direkt im Vertex-Shader in den NDC-Bereich [-1.0, 1.0] abzubilden.
    pub fn ndc_transform(&self) -> ([f32; 2], [f32; 2]) {
        let scale_x = (2.0 * self.page_width) / (self.window_width * PAGE_WIDTH);
        let scale_y = (-2.0 * self.page_height) / (self.window_height * PAGE_HEIGHT);
        let offset_x = (2.0 * self.page_x) / self.window_width - 1.0;
        let offset_y = 1.0 - (2.0 * self.page_y) / self.window_height;
        ([scale_x, scale_y], [offset_x, offset_y])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_point_to_segment_distance() {
        let a = glam::Vec2::new(0.0, 0.0);
        let b = glam::Vec2::new(10.0, 0.0);

        // Punkt liegt genau senkrecht über der Streckenmitte (5, 5)
        let p_mid = glam::Vec2::new(5.0, 5.0);
        assert!((point_to_segment_distance(p_mid, a, b) - 5.0).abs() < 1e-5);

        // Punkt liegt links von a (-5, 0) -> Abstand zu a
        let p_left = glam::Vec2::new(-5.0, 0.0);
        assert!((point_to_segment_distance(p_left, a, b) - 5.0).abs() < 1e-5);

        // Punkt liegt rechts von b (15, 0) -> Abstand zu b
        let p_right = glam::Vec2::new(15.0, 0.0);
        assert!((point_to_segment_distance(p_right, a, b) - 5.0).abs() < 1e-5);

        // Punkt liegt direkt auf der Strecke
        let p_on = glam::Vec2::new(3.0, 0.0);
        assert!(point_to_segment_distance(p_on, a, b) < 1e-5);
    }

    #[test]
    fn test_stroke_eraser_hit() {
        let points = vec![
            StylusSample {
                position: glam::Vec2::new(100.0, 100.0),
                pressure: 0.5,
                timestamp: 0.0,
            },
            StylusSample {
                position: glam::Vec2::new(200.0, 100.0),
                pressure: 0.5,
                timestamp: 0.01,
            },
        ];

        let stroke = Stroke::new(1, points, Vec::new(), [0.0, 0.0, 0.0, 1.0]);

        let eraser_radius = 10.0;
        let stroke_radius = 2.0;

        // Treffer: Radierer bei (150, 105) (Abstand 5 <= 12)
        assert!(stroke.intersects_eraser(
            glam::Vec2::new(150.0, 105.0),
            eraser_radius,
            stroke_radius
        ));

        // Kein Treffer: Radierer bei (150, 150) (Abstand 50 > 12)
        assert!(!stroke.intersects_eraser(
            glam::Vec2::new(150.0, 150.0),
            eraser_radius,
            stroke_radius
        ));

        // Kein Treffer: Weit außerhalb der AABB
        assert!(!stroke.intersects_eraser(
            glam::Vec2::new(500.0, 500.0),
            eraser_radius,
            stroke_radius
        ));
    }

    #[test]
    fn test_notebook_navigation() {
        let mut nb = Notebook::new();
        assert_eq!(nb.pages.len(), 1);
        assert_eq!(nb.current_page_index, 0);
        assert_eq!(nb.osd_text(), "Seite 1 / 1");

        // Weiterblättern am Ende erzeugt automatisch neue Seite
        nb.next_page();
        assert_eq!(nb.pages.len(), 2);
        assert_eq!(nb.current_page_index, 1);
        assert_eq!(nb.osd_text(), "Seite 2 / 2");

        // Zurückblättern
        nb.previous_page();
        assert_eq!(nb.current_page_index, 0);

        // Zurückblättern auf erster Seite bleibt auf Seite 0
        nb.previous_page();
        assert_eq!(nb.current_page_index, 0);

        // Ende und Home
        nb.last_page();
        assert_eq!(nb.current_page_index, 1);
        nb.first_page();
        assert_eq!(nb.current_page_index, 0);
    }

    #[test]
    fn test_page_viewport_projection() {
        let vp = PageViewport::calculate(1600.0, 900.0, 20.0);
        assert!(vp.page_width > 0.0);
        assert!(vp.page_height > 0.0);

        // Aspekt DIN A4 muss exakt erhalten bleiben
        let ratio = vp.page_width / vp.page_height;
        assert!((ratio - A4_ASPECT_RATIO).abs() < 1e-4);

        // Transformation Mitte des Papiers
        let center_screen_x = vp.page_x + vp.page_width / 2.0;
        let center_screen_y = vp.page_y + vp.page_height / 2.0;
        let page_pt = vp.screen_to_page(center_screen_x, center_screen_y).unwrap();
        assert!((page_pt.x - PAGE_WIDTH / 2.0).abs() < 1.0);
        assert!((page_pt.y - PAGE_HEIGHT / 2.0).abs() < 1.0);

        // NDC-Transformation der Mitte muss (0.0, 0.0) ergeben
        let (scale, offset) = vp.ndc_transform();
        let ndc_x = (PAGE_WIDTH / 2.0) * scale[0] + offset[0];
        let ndc_y = (PAGE_HEIGHT / 2.0) * scale[1] + offset[1];
        assert!(ndc_x.abs() < 1e-4);
        assert!(ndc_y.abs() < 1e-4);
    }

    #[test]
    fn test_page_erase_and_undo_redo() {
        let mut page = Page::new(1, PageBackground::Grid5mm);

        let s1 = Stroke::new(
            1,
            vec![
                StylusSample {
                    position: glam::Vec2::new(10.0, 10.0),
                    pressure: 0.5,
                    timestamp: 0.0,
                },
                StylusSample {
                    position: glam::Vec2::new(50.0, 10.0),
                    pressure: 0.5,
                    timestamp: 0.01,
                },
            ],
            vec![
                InkVertex::new(10.0, 10.0, [0.0, 0.0, 0.0, 1.0]),
                InkVertex::new(50.0, 10.0, [0.0, 0.0, 0.0, 1.0]),
            ],
            [0.0, 0.0, 0.0, 1.0],
        );

        let s2 = Stroke::new(
            2,
            vec![
                StylusSample {
                    position: glam::Vec2::new(10.0, 100.0),
                    pressure: 0.5,
                    timestamp: 0.02,
                },
                StylusSample {
                    position: glam::Vec2::new(50.0, 100.0),
                    pressure: 0.5,
                    timestamp: 0.03,
                },
            ],
            vec![
                InkVertex::new(10.0, 100.0, [0.0, 0.0, 0.0, 1.0]),
                InkVertex::new(50.0, 100.0, [0.0, 0.0, 0.0, 1.0]),
            ],
            [0.0, 0.0, 0.0, 1.0],
        );

        page.add_stroke(s1);
        page.add_stroke(s2);
        assert_eq!(page.strokes.len(), 2);

        // Radieren bei (30, 12): Trifft nur s1
        let removed = page.erase_at(glam::Vec2::new(30.0, 12.0), 10.0, 2.0);
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].id, 1);
        assert_eq!(page.strokes.len(), 1);
        assert_eq!(page.strokes[0].id, 2);

        // Undo: s1 wird wiederhergestellt
        assert!(page.undo());
        assert_eq!(page.strokes.len(), 2);

        // Redo: s1 wird wieder gelöscht
        assert!(page.redo());
        assert_eq!(page.strokes.len(), 1);
        assert_eq!(page.strokes[0].id, 2);
    }

    #[test]
    fn test_page_isolation() {
        let mut nb = Notebook::new();

        let s1 = Stroke::new(
            100,
            vec![StylusSample {
                position: glam::Vec2::new(50.0, 50.0),
                pressure: 0.5,
                timestamp: 0.0,
            }],
            Vec::new(),
            [0.0, 0.0, 0.0, 1.0],
        );

        nb.current_page_mut().add_stroke(s1);
        assert_eq!(nb.current_page().strokes.len(), 1);

        // Neue Seite anlegen
        nb.next_page();
        assert_eq!(nb.current_page_index, 1);
        // Neue Seite muss leer sein
        assert_eq!(nb.current_page().strokes.len(), 0);

        // Zurück zu Seite 0: Strich muss unverändert da sein
        nb.previous_page();
        assert_eq!(nb.current_page_index, 0);
        assert_eq!(nb.current_page().strokes.len(), 1);
        assert_eq!(nb.current_page().strokes[0].id, 100);
    }

    #[test]
    fn test_page_ephemeral_and_commit_manipulations() {
        let mut page = Page::new(1, PageBackground::Blank);
        let s = Stroke::new(
            10,
            vec![StylusSample {
                position: glam::Vec2::new(10.0, 10.0),
                pressure: 0.5,
                timestamp: 0.0,
            }],
            vec![InkVertex::new(10.0, 10.0, [1.0, 0.0, 0.0, 1.0])],
            [1.0, 0.0, 0.0, 1.0],
        );
        page.add_stroke(s.clone());

        // Ephemeral modification (simulating in-progress mouse drag)
        let mut moved = s.clone();
        moved.translate(glam::Vec2::new(20.0, 30.0));
        page.update_strokes_ephemeral(&[moved.clone()]);
        assert_eq!(
            page.strokes[0].points[0].position,
            glam::Vec2::new(30.0, 40.0)
        );
        // History should not have been updated yet
        assert!(!page.history.can_redo());

        // Transactional commit (mouse release)
        let before = vec![s.clone()];
        let after = vec![moved.clone()];
        page.commit_modified_strokes(before, after);

        // Verify Undo restores original position
        assert!(page.undo());
        assert_eq!(
            page.strokes[0].points[0].position,
            glam::Vec2::new(10.0, 10.0)
        );

        // Verify Redo applies modified position
        assert!(page.redo());
        assert_eq!(
            page.strokes[0].points[0].position,
            glam::Vec2::new(30.0, 40.0)
        );
    }
}
