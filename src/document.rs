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

/// Liegt `p` im konvexen Viereck `quad` (Ecken der Reihe nach, beliebiger Umlaufsinn)?
pub fn point_in_convex_quad(p: glam::Vec2, quad: &[glam::Vec2; 4]) -> bool {
    let mut sign = 0.0_f32;
    for i in 0..4 {
        let a = quad[i];
        let b = quad[(i + 1) % 4];
        let cross = (b - a).perp_dot(p - a);
        if cross.abs() < 1e-6 {
            continue;
        }
        if sign == 0.0 {
            sign = cross.signum();
        } else if cross.signum() != sign {
            return false;
        }
    }
    true
}

/// Größte Länge eines Seitentitels in Zeichen.
pub const MAX_PAGE_TITLE_CHARS: usize = 120;

/// Bereinigt einen Seitentitel: Zeilenumbrüche und doppelte Leerzeichen entfernen, auf
/// [`MAX_PAGE_TITLE_CHARS`] kürzen; leer bedeutet „kein Titel“.
pub fn normalize_page_title(title: &str) -> Option<String> {
    let cleaned: String = title
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(MAX_PAGE_TITLE_CHARS)
        .collect();
    (!cleaned.is_empty()).then_some(cleaned)
}

/// Verweis auf eine Seite eines importierten PDFs (Datei im Asset-Ordner).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PdfBackground {
    /// Name der PDF-Datei im Asset-Ordner
    pub asset: String,
    /// Seitennummer im PDF, ab 0
    pub page: u32,
    /// Angezeigte Größe der PDF-Seite in Punkt (Drehung berücksichtigt)
    pub width_pt: f32,
    pub height_pt: f32,
}

impl PdfBackground {
    /// Schlüssel für Texturen und Render-Aufträge.
    pub fn key(&self) -> String {
        format!("{}#{}", self.asset, self.page)
    }

    /// Ecken (oben links, oben rechts, unten rechts, unten links) der PDF-Seite auf dem
    /// A4-Blatt, siehe [`pdf_placement`].
    pub fn corners(&self) -> [glam::Vec2; 4] {
        pdf_placement(self.width_pt, self.height_pt)
    }
}

/// Lage einer PDF-Seite von `width_pt` × `height_pt` auf dem A4-Blatt: so groß wie möglich,
/// waagerecht zentriert und oben bündig. Querformat-Folien liegen so oben auf der Seite,
/// darunter bleibt Platz für Notizen.
pub fn pdf_placement(width_pt: f32, height_pt: f32) -> [glam::Vec2; 4] {
    let (w, h) = (width_pt.max(1.0), height_pt.max(1.0));
    let scale = (PAGE_WIDTH / w).min(PAGE_HEIGHT / h);
    let size = glam::Vec2::new(w * scale, h * scale);
    let tl = glam::Vec2::new((PAGE_WIDTH - size.x) / 2.0, 0.0);
    [
        tl,
        tl + glam::Vec2::new(size.x, 0.0),
        tl + size,
        tl + glam::Vec2::new(0.0, size.y),
    ]
}

/// Seiteneinheiten pro Bildpixel beim Einfügen (96 dpi wie am Bildschirm: 1 px ≈ 0,26 mm).
pub const IMAGE_UNITS_PER_PIXEL: f32 = UNITS_PER_MM * 25.4 / 96.0;

/// Ecken (oben links, oben rechts, unten rechts, unten links) für ein neu eingefügtes Bild
/// von `width` × `height` Pixeln um `center`: Bildschirmgröße bei 96 dpi, höchstens 80 % der
/// Seitenbreite und 60 % der Seitenhöhe, und möglichst ganz auf dem Blatt.
pub fn place_image(width: u32, height: u32, center: glam::Vec2) -> [glam::Vec2; 4] {
    let natural =
        glam::Vec2::new(width.max(1) as f32, height.max(1) as f32) * IMAGE_UNITS_PER_PIXEL;
    let fit = (PAGE_WIDTH * 0.8 / natural.x)
        .min(PAGE_HEIGHT * 0.6 / natural.y)
        .min(1.0);
    let size = natural * fit;
    let half = size / 2.0;
    let center = glam::Vec2::new(
        center.x.clamp(half.x, PAGE_WIDTH - half.x),
        center.y.clamp(half.y, PAGE_HEIGHT - half.y),
    );
    let tl = center - half;
    [
        tl,
        tl + glam::Vec2::new(size.x, 0.0),
        tl + size,
        tl + glam::Vec2::new(0.0, size.y),
    ]
}

/// Das Werkzeug, mit dem ein Strich gezeichnet wurde.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StrokeTool {
    #[default]
    Pen,
    Highlighter,
    Shape,
}

impl StrokeTool {
    /// Werkzeug eines gespeicherten Strichs. Bis v0.3.7 wurde das Werkzeug nicht erfasst und
    /// jeder Strich als `pen` gespeichert; Textmarker erkennt man dann an der
    /// Halbtransparenz (der Stift ist immer deckend), Formen an der erkannten Form.
    pub fn resolve_stored(stored: StrokeTool, color: [f32; 4], has_shape: bool) -> StrokeTool {
        match stored {
            StrokeTool::Pen if has_shape => StrokeTool::Shape,
            StrokeTool::Pen if color[3] < 0.99 => StrokeTool::Highlighter,
            other => other,
        }
    }
}

/// Welche Striche der Radierer entfernt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EraserMode {
    /// Alle getroffenen Striche
    #[default]
    All,
    /// Nur Textmarker – die Handschrift darunter bleibt erhalten
    HighlighterOnly,
    /// Nur den überstrichenen Teil eines Strichs entfernen; der Rest bleibt in Stücken
    /// erhalten. Formen werden als Ganzes radiert.
    Partial,
}

impl EraserMode {
    /// Darf ein Strich dieses Werkzeugs radiert werden?
    pub fn erases(self, tool: StrokeTool) -> bool {
        match self {
            EraserMode::All | EraserMode::Partial => true,
            EraserMode::HighlighterOnly => tool == StrokeTool::Highlighter,
        }
    }
}

/// Zeitfenster, in dem aufeinanderfolgende Teilradierungen zu einem Undo-Schritt
/// zusammengefasst werden (ein Radierzug besteht aus vielen kleinen Schnitten).
pub const PARTIAL_ERASE_MERGE_WINDOW: std::time::Duration = std::time::Duration::from_millis(500);

/// Zerlegt die Punktfolge eines Strichs an einem Kreis (Radierer): übrig bleiben die Stücke
/// außerhalb des Kreises, an den Kreisrand gekürzt. `None`, wenn der Kreis den Strich nicht
/// berührt. Stücke kürzer als `min_length` entfallen.
pub fn split_samples_at_circle(
    samples: &[StylusSample],
    center: glam::Vec2,
    radius: f32,
    min_length: f32,
) -> Option<Vec<Vec<StylusSample>>> {
    let inside = |p: glam::Vec2| (p - center).length_squared() <= radius * radius;
    let lerp = |a: &StylusSample, b: &StylusSample, t: f32| StylusSample {
        position: a.position.lerp(b.position, t),
        pressure: a.pressure + (b.pressure - a.pressure) * t,
        timestamp: a.timestamp + (b.timestamp - a.timestamp) * t as f64,
    };
    // Schnittparameter t ∈ (0, 1) einer Strecke mit dem Kreisrand, aufsteigend
    let crossings = |a: glam::Vec2, b: glam::Vec2| -> Vec<f32> {
        let d = b - a;
        let f = a - center;
        let qa = d.dot(d);
        if qa < 1e-12 {
            return Vec::new();
        }
        let qb = 2.0 * f.dot(d);
        let qc = f.dot(f) - radius * radius;
        let disc = qb * qb - 4.0 * qa * qc;
        if disc < 0.0 {
            return Vec::new();
        }
        let root = disc.sqrt();
        [(-qb - root) / (2.0 * qa), (-qb + root) / (2.0 * qa)]
            .into_iter()
            .filter(|t| *t > 0.0 && *t < 1.0)
            .collect()
    };

    match samples {
        [] => return None,
        [single] => return inside(single.position).then(Vec::new),
        _ => {}
    }
    let touched = samples.iter().any(|s| inside(s.position))
        || samples
            .windows(2)
            .any(|w| crossings(w[0].position, w[1].position).len() == 2);
    if !touched {
        return None;
    }

    let mut pieces = Vec::new();
    let mut current: Vec<StylusSample> = Vec::new();
    if !inside(samples[0].position) {
        current.push(samples[0]);
    }
    for w in samples.windows(2) {
        let (a, b) = (&w[0], &w[1]);
        let (a_in, b_in) = (inside(a.position), inside(b.position));
        let ts = crossings(a.position, b.position);
        match (a_in, b_in) {
            (false, false) => {
                if let [t_enter, t_exit] = ts[..] {
                    // Strecke läuft durch den Kreis
                    current.push(lerp(a, b, t_enter));
                    pieces.push(std::mem::take(&mut current));
                    current.push(lerp(a, b, t_exit));
                }
                current.push(*b);
            }
            (false, true) => {
                if let Some(&t) = ts.first() {
                    current.push(lerp(a, b, t));
                }
                pieces.push(std::mem::take(&mut current));
            }
            (true, false) => {
                current.clear();
                if let Some(&t) = ts.last() {
                    current.push(lerp(a, b, t));
                }
                current.push(*b);
            }
            (true, true) => {}
        }
    }
    pieces.push(current);

    let length = |p: &Vec<StylusSample>| -> f32 {
        p.windows(2)
            .map(|w| (w[1].position - w[0].position).length())
            .sum()
    };
    Some(
        pieces
            .into_iter()
            .filter(|p| p.len() >= 2 && length(p) >= min_length)
            .collect(),
    )
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
    pub tool: StrokeTool,
    pub width: f32,
    pub finalized: bool,
    /// Eingefügtes Bild (Name im Asset-Ordner, siehe `assets`). Die vier Punkte sind dann die
    /// Bildecken oben links, oben rechts, unten rechts, unten links; Verschieben, Drehen und
    /// Skalieren wirken so automatisch auch auf Bilder. Bilder liegen immer unter der Tinte.
    pub image: Option<String>,
    /// Textfeld (Text-Werkzeug); die vier Punkte sind wie bei Bildern die Ecken des Felds.
    /// Textfelder liegen wie Bilder unter der Tinte.
    pub text: Option<crate::text::TextBox>,
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
            tool: StrokeTool::Pen,
            width: 5.0,
            finalized: true,
            image: None,
            text: None,
        }
    }

    /// Erzeugt ein Bildobjekt mit den Ecken oben links, oben rechts, unten rechts, unten links.
    pub fn new_image(id: u64, asset: String, corners: [glam::Vec2; 4]) -> Self {
        let points = corners
            .iter()
            .map(|&position| StylusSample {
                position,
                pressure: 1.0,
                timestamp: 0.0,
            })
            .collect();
        let mut stroke = Self::new(id, points, Vec::new(), [1.0, 1.0, 1.0, 1.0]);
        stroke.width = 0.0;
        stroke.image = Some(asset);
        stroke
    }

    /// Erzeugt ein Textfeld mit den Ecken oben links, oben rechts, unten rechts, unten links.
    pub fn new_text(
        id: u64,
        text: crate::text::TextBox,
        corners: [glam::Vec2; 4],
        color: [f32; 4],
    ) -> Self {
        let mut stroke = Self::new_image(id, String::new(), corners);
        stroke.image = None;
        stroke.color = color;
        stroke.text = Some(text);
        stroke
    }

    /// Ist dieses Objekt ein Bild?
    pub fn is_image(&self) -> bool {
        self.image.is_some()
    }

    /// Ist dieses Objekt ein Textfeld?
    pub fn is_text(&self) -> bool {
        self.text.is_some()
    }

    /// Bild oder Textfeld (Objekt mit vier Ecken statt Tinte)?
    pub fn is_object(&self) -> bool {
        self.is_image() || self.is_text()
    }

    /// Ecken eines Bildes oder Textfelds (oben links, oben rechts, unten rechts, unten links).
    pub fn object_corners(&self) -> Option<[glam::Vec2; 4]> {
        if !self.is_object() || self.points.len() < 4 {
            return None;
        }
        Some([
            self.points[0].position,
            self.points[1].position,
            self.points[2].position,
            self.points[3].position,
        ])
    }

    /// Bildecken (oben links, oben rechts, unten rechts, unten links), falls ein Bild.
    pub fn image_corners(&self) -> Option<[glam::Vec2; 4]> {
        if self.image.is_none() || self.points.len() < 4 {
            return None;
        }
        Some([
            self.points[0].position,
            self.points[1].position,
            self.points[2].position,
            self.points[3].position,
        ])
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
        self.tool = StrokeTool::Shape;
        self
    }

    pub fn with_tool(mut self, tool: StrokeTool) -> Self {
        self.tool = tool;
        self
    }

    pub fn with_width(mut self, width: f32) -> Self {
        self.width = width;
        self
    }

    pub fn with_finalized(mut self, finalized: bool) -> Self {
        self.finalized = finalized;
        self
    }

    /// Berechnet den minimalen euklidischen Abstand eines Punktes zu diesem Strich.
    /// Bei Bildern und Textfeldern ist der Abstand innerhalb der Fläche 0.
    pub fn distance_to_point(&self, p: glam::Vec2) -> f32 {
        if let Some(corners) = self.object_corners() {
            if point_in_convex_quad(p, &corners) {
                return 0.0;
            }
            return (0..4)
                .map(|i| point_to_segment_distance(p, corners[i], corners[(i + 1) % 4]))
                .fold(f32::INFINITY, f32::min);
        }
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
    /// Bilder behalten ihre Farben.
    pub fn set_color(&mut self, new_color: [f32; 4]) {
        if self.is_image() {
            return;
        }
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

/// Persistenzstatus einer Seite zum Schutz vor unbeabsichtigtem Überschreiben bei Ladefehlern.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PersistState {
    #[default]
    Normal,
    ReadOnlyAfterLoadError,
}

/// Warnung beim Laden einer Seite (beschädigte Datei oder I/O-Fehler).
#[derive(Debug, Clone, PartialEq)]
pub struct PageLoadWarning {
    pub page_id: u64,
    pub message: String,
    pub quarantine_path: Option<std::path::PathBuf>,
}

/// Eine einzelne Notizbuchseite mit Strichen, Hintergrund und gecachten Meshes.
#[derive(Debug, Clone)]
pub struct Page {
    pub id: u64,
    pub strokes: Vec<Stroke>,
    pub background: PageBackground,
    /// Importierte PDF-Seite als Hintergrund (unter Bildern und Tinte)
    pub pdf_background: Option<PdfBackground>,
    /// Seitentitel (Lesezeichen); auch im Manifest, damit er ohne Laden der Seite bekannt ist
    pub title: Option<String>,
    pub vertex_cache: Vec<InkVertex>,
    pub stroke_ranges: Vec<Range<u32>>,
    pub is_dirty: bool,
    pub is_loaded: bool,
    pub persist: PersistState,
    pub history: HistoryStack,
}

impl Page {
    pub fn new(id: u64, background: PageBackground) -> Self {
        Self {
            id,
            strokes: Vec::new(),
            background,
            pdf_background: None,
            title: None,
            vertex_cache: Vec::new(),
            stroke_ranges: Vec::new(),
            is_dirty: false,
            is_loaded: true,
            persist: PersistState::Normal,
            history: HistoryStack::new(),
        }
    }

    /// Erzeugt einen noch nicht aus dem Speicher geladenen Seiten-Platzhalter (Lazy Loading).
    pub fn new_unloaded(id: u64, background: PageBackground) -> Self {
        Self {
            id,
            strokes: Vec::new(),
            background,
            pdf_background: None,
            title: None,
            vertex_cache: Vec::new(),
            stroke_ranges: Vec::new(),
            is_dirty: false,
            is_loaded: false,
            persist: PersistState::Normal,
            history: HistoryStack::new(),
        }
    }

    pub fn is_readonly(&self) -> bool {
        self.persist == PersistState::ReadOnlyAfterLoadError
    }

    pub fn unlock_after_load_error(&mut self) {
        self.persist = PersistState::Normal;
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
    /// Tinte hat Vorrang vor Bildern und Textfeldern darunter, damit Notizen darauf
    /// auswählbar bleiben.
    pub fn find_stroke_at(&self, pos: glam::Vec2, max_dist: f32) -> Option<&Stroke> {
        let mut best: Option<(&Stroke, (bool, f32))> = None;
        for s in &self.strokes {
            if !s.aabb.expanded(max_dist).contains_point(pos) {
                continue;
            }
            let d = s.distance_to_point(pos);
            if d <= max_dist {
                let key = (s.is_object(), d);
                if best.is_none_or(|(_, best_key)| key < best_key) {
                    best = Some((s, key));
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

    /// Radiert alle Striche an der Position `eraser_pos` mit Radius `eraser_radius`,
    /// die `mode` zulässt.
    ///
    /// Gibt die gelöschten Striche zurück (falls welche getroffen wurden).
    pub fn erase_at(
        &mut self,
        eraser_pos: glam::Vec2,
        eraser_radius: f32,
        max_stroke_radius: f32,
        mode: EraserMode,
    ) -> Vec<Stroke> {
        let mut hit_indices = Vec::new();

        for (i, stroke) in self.strokes.iter().enumerate() {
            if !stroke.is_object()
                && mode.erases(stroke.tool)
                && stroke.intersects_eraser(eraser_pos, eraser_radius, max_stroke_radius)
            {
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

    /// Teilradierer: entfernt den Teil aller Striche im Kreis um `eraser_pos`. Getroffene
    /// Striche werden durch ihre übrig gebliebenen Stücke ersetzt (neue IDs aus `next_id`),
    /// Formen als Ganzes entfernt, Bilder und Textfelder nie. Aufeinanderfolgende Schnitte
    /// eines Radierzugs bilden einen gemeinsamen Undo-Schritt. Gibt `true` zurück, wenn sich etwas geändert hat.
    pub fn erase_partial_at(
        &mut self,
        eraser_pos: glam::Vec2,
        eraser_radius: f32,
        next_id: &mut u64,
    ) -> bool {
        let mut removed = Vec::new();
        let mut added = Vec::new();
        let mut result = Vec::with_capacity(self.strokes.len());
        for stroke in std::mem::take(&mut self.strokes) {
            let reach = eraser_radius + stroke.width / 2.0;
            if stroke.is_object() || !stroke.aabb.expanded(reach).contains_point(eraser_pos) {
                result.push(stroke);
                continue;
            }
            if stroke.shape.is_some() {
                // Formen werden ganz radiert, sobald der Radierer sie berührt
                if stroke.distance_to_point(eraser_pos) <= reach {
                    removed.push(stroke);
                } else {
                    result.push(stroke);
                }
                continue;
            }
            match split_samples_at_circle(&stroke.points, eraser_pos, reach, stroke.width * 0.5) {
                None => result.push(stroke),
                Some(pieces) => {
                    for samples in pieces {
                        let vertices = crate::storage::reconstruct_stroke_vertices(
                            &samples,
                            stroke.color,
                            stroke.width,
                            stroke.tool,
                            true,
                        );
                        let mut piece = Stroke::new(*next_id, samples, vertices, stroke.color);
                        *next_id += 1;
                        piece.tool = stroke.tool;
                        piece.width = stroke.width;
                        piece.finalized = true;
                        result.push(piece.clone());
                        added.push(piece);
                    }
                    removed.push(stroke);
                }
            }
        }
        self.strokes = result;
        if removed.is_empty() {
            return false;
        }
        self.history.push_split(removed, added);
        self.is_dirty = true;
        true
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
    /// Bilder und Textfelder werden nur ausgewählt, wenn sie ganz in der Box liegen – sonst
    /// würde jede Auswahl von Notizen auf einem Bild das Bild mitnehmen.
    pub fn find_strokes_in_box(&self, box_min: glam::Vec2, box_max: glam::Vec2) -> Vec<u64> {
        let sel_box = Aabb::new(
            glam::Vec2::new(box_min.x.min(box_max.x), box_min.y.min(box_max.y)),
            glam::Vec2::new(box_min.x.max(box_max.x), box_min.y.max(box_max.y)),
        );
        self.strokes
            .iter()
            .filter(|s| {
                if s.is_object() {
                    sel_box.contains_point(s.aabb.min) && sel_box.contains_point(s.aabb.max)
                } else {
                    s.aabb.intersects(&sel_box)
                }
            })
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

/// Höhe des Bereichs am unteren Rand der letzten Seite, in dem Schreiben in der
/// durchgehenden Ansicht automatisch eine neue Seite anhängt.
pub const AUTO_APPEND_ZONE: f32 = PAGE_HEIGHT * 0.2;

/// Das Notizbuch mit diskreter Seitenverwaltung und Navigation.
#[derive(Debug, Clone)]
pub struct Notebook {
    pub pages: Vec<Page>,
    pub current_page_index: usize,
    pub next_stroke_id: u64,
    pub next_page_id: u64,
    pub load_warnings: std::collections::HashMap<u64, PageLoadWarning>,
}

impl Notebook {
    pub fn new() -> Self {
        let mut nb = Self {
            pages: Vec::new(),
            current_page_index: 0,
            next_stroke_id: 1,
            next_page_id: 1,
            load_warnings: std::collections::HashMap::new(),
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
            load_warnings: std::collections::HashMap::new(),
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

    /// Radiert auf der aktuellen Seite gemäß `mode` (ganze Striche, nur Textmarker oder
    /// Teilstriche). Gibt `true` zurück, wenn sich etwas geändert hat.
    pub fn erase_at(
        &mut self,
        eraser_pos: glam::Vec2,
        eraser_radius: f32,
        max_stroke_radius: f32,
        mode: EraserMode,
    ) -> bool {
        let page = &mut self.pages[self.current_page_index];
        if mode == EraserMode::Partial {
            page.erase_partial_at(eraser_pos, eraser_radius, &mut self.next_stroke_id)
        } else {
            !page
                .erase_at(eraser_pos, eraser_radius, max_stroke_radius, mode)
                .is_empty()
        }
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

    /// Fügt direkt nach der aktuellen Seite eine leere Seite mit dem angegebenen
    /// Hintergrund ein (die aktuelle Seite bleibt aktuell) und gibt ihren Index zurück.
    pub fn insert_page_after_current(&mut self, background: PageBackground) -> usize {
        let id = self.next_page_id;
        self.next_page_id += 1;
        let index = (self.current_page_index + 1).min(self.pages.len());
        self.pages.insert(index, Page::new(id, background));
        index
    }

    /// Durchgehende Ansicht: Reicht ein neuer Strich auf der letzten Seite bis in deren
    /// unteres Fünftel ([`AUTO_APPEND_ZONE`]), wird darunter eine leere Seite mit gleichem
    /// Hintergrund angehängt. `true`, wenn eine Seite angehängt wurde.
    pub fn append_page_if_writing_at_end(&mut self, stroke_bottom: f32) -> bool {
        if self.current_page_index + 1 != self.pages.len()
            || stroke_bottom < PAGE_HEIGHT - AUTO_APPEND_ZONE
        {
            return false;
        }
        let background = self.current_page().background;
        self.add_page(background);
        true
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

    /// Setzt den Titel der Seite `page_id` (leer = kein Titel). `true`, wenn er sich geändert
    /// hat.
    pub fn set_page_title(&mut self, page_id: u64, title: &str) -> bool {
        let title = normalize_page_title(title);
        let Some(page) = self.pages.iter_mut().find(|p| p.id == page_id) else {
            return false;
        };
        if page.title == title || !page.is_loaded || page.is_readonly() {
            return false;
        }
        page.title = title;
        true
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
        Self::calculate_with_insets(window_width, window_height, margin, 0.0, 0.0)
    }

    /// Wie [`Self::calculate`], aber ohne die von seitlicher UI (z. B. Werkzeugleiste)
    /// belegten Streifen links/rechts: Die Seite wird im verbleibenden Bereich zentriert.
    pub fn calculate_with_insets(
        window_width: f32,
        window_height: f32,
        margin: f32,
        inset_left: f32,
        inset_right: f32,
    ) -> Self {
        let inset_left = inset_left.clamp(0.0, window_width * 0.5);
        let inset_right = inset_right.clamp(0.0, window_width * 0.5);
        let avail_w = (window_width - inset_left - inset_right - 2.0 * margin).max(10.0);
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

        let page_x = inset_left + (window_width - inset_left - inset_right - page_w) / 2.0;
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
    fn test_viewport_insets_center_page_in_free_area() {
        // Breites Fenster: Seite wird durch die Höhe begrenzt und im freien Bereich zentriert
        let plain = PageViewport::calculate(1600.0, 900.0, 20.0);
        let inset = PageViewport::calculate_with_insets(1600.0, 900.0, 20.0, 120.0, 0.0);
        assert!((inset.page_width - plain.page_width).abs() < 0.01);
        let free_center = 120.0 + (1600.0 - 120.0) / 2.0;
        assert!((inset.page_x + inset.page_width / 2.0 - free_center).abs() < 0.01);

        // Hohes Fenster: Breite begrenzt, Seite wird schmaler und bleibt rechts der Leiste
        let narrow = PageViewport::calculate_with_insets(600.0, 1200.0, 20.0, 100.0, 0.0);
        assert!(narrow.page_x >= 100.0 + 20.0 - 0.01);
        assert!(narrow.page_x + narrow.page_width <= 600.0 - 20.0 + 0.01);

        // Leiste rechts
        let right = PageViewport::calculate_with_insets(600.0, 1200.0, 20.0, 0.0, 100.0);
        assert!(right.page_x + right.page_width <= 500.0 - 20.0 + 0.01);
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
        let removed = page.erase_at(glam::Vec2::new(30.0, 12.0), 10.0, 2.0, EraserMode::All);
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
    fn test_erase_highlighter_only_keeps_handwriting() {
        let mut page = Page::new(1, PageBackground::Grid5mm);
        let line = |id: u64, tool: StrokeTool| {
            let mut stroke = Stroke::new(
                id,
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
            stroke.tool = tool;
            stroke
        };
        // Handschrift, Form und Textmarker liegen übereinander
        page.add_stroke(line(1, StrokeTool::Pen));
        page.add_stroke(line(2, StrokeTool::Highlighter));
        page.add_stroke(line(3, StrokeTool::Shape));

        let at = glam::Vec2::new(30.0, 10.0);
        let removed = page.erase_at(at, 10.0, 2.0, EraserMode::HighlighterOnly);
        assert_eq!(removed.iter().map(|s| s.id).collect::<Vec<_>>(), vec![2]);
        assert_eq!(
            page.strokes.iter().map(|s| s.id).collect::<Vec<_>>(),
            vec![1, 3]
        );
        // Kein Textmarker mehr da: nichts wird entfernt, kein Undo-Eintrag
        assert!(page
            .erase_at(at, 10.0, 2.0, EraserMode::HighlighterOnly)
            .is_empty());
        assert!(page.undo());
        assert_eq!(page.strokes.len(), 3);

        // Normaler Radierer entfernt alles
        let removed = page.erase_at(at, 10.0, 2.0, EraserMode::All);
        assert_eq!(removed.len(), 3);
        assert!(page.strokes.is_empty());
    }

    #[test]
    fn test_place_image() {
        let center = glam::Vec2::new(PAGE_WIDTH / 2.0, PAGE_HEIGHT / 2.0);
        // Kleines Bild: Bildschirmgröße, zentriert
        let [tl, tr, br, bl] = place_image(200, 100, center);
        assert!(((tr - tl).length() - 200.0 * IMAGE_UNITS_PER_PIXEL).abs() < 1e-3);
        assert!(((bl - tl).length() - 100.0 * IMAGE_UNITS_PER_PIXEL).abs() < 1e-3);
        assert!(((tl + br) / 2.0 - center).length() < 1e-3);

        // Großer Screenshot: auf 80 % der Breite verkleinert, Seitenverhältnis bleibt
        let [tl, tr, br, _] = place_image(3840, 2160, center);
        let size = br - tl;
        assert!((size.x - PAGE_WIDTH * 0.8).abs() < 1e-2);
        assert!((size.x / size.y - 3840.0 / 2160.0).abs() < 1e-3);
        assert_eq!(tr.y, tl.y);

        // Am Rand eingefügt: wird aufs Blatt geschoben
        let corners = place_image(400, 400, glam::Vec2::new(-50.0, PAGE_HEIGHT + 50.0));
        for c in corners {
            assert!(c.x >= -1e-3 && c.x <= PAGE_WIDTH + 1e-3);
            assert!(c.y >= -1e-3 && c.y <= PAGE_HEIGHT + 1e-3);
        }
    }

    #[test]
    fn test_image_objects_hit_test_and_survive_erasing() {
        let mut page = Page::new(1, PageBackground::Blank);
        let corners = [
            glam::Vec2::new(100.0, 100.0),
            glam::Vec2::new(300.0, 100.0),
            glam::Vec2::new(300.0, 200.0),
            glam::Vec2::new(100.0, 200.0),
        ];
        page.add_stroke(Stroke::new_image(1, "a.png".into(), corners));
        assert_eq!(page.strokes[0].image_corners(), Some(corners));

        // Fläche des Bildes trifft, auch weit weg vom Rand
        let inside = glam::Vec2::new(200.0, 150.0);
        assert_eq!(page.find_stroke_at(inside, 5.0).map(|s| s.id), Some(1));
        assert!(page
            .find_stroke_at(glam::Vec2::new(400.0, 150.0), 5.0)
            .is_none());

        // Notiz auf dem Bild hat beim Antippen Vorrang
        let mut ink = Stroke::new(
            2,
            vec![
                StylusSample {
                    position: glam::Vec2::new(150.0, 150.0),
                    pressure: 0.5,
                    timestamp: 0.0,
                },
                StylusSample {
                    position: glam::Vec2::new(250.0, 152.0),
                    pressure: 0.5,
                    timestamp: 0.1,
                },
            ],
            Vec::new(),
            [0.0, 0.0, 0.0, 1.0],
        );
        ink.width = 5.0;
        page.add_stroke(ink);
        assert_eq!(page.find_stroke_at(inside, 5.0).map(|s| s.id), Some(2));

        // Radierer entfernt nur die Notiz, nie das Bild
        let removed = page.erase_at(inside, 30.0, 12.0, EraserMode::All);
        assert_eq!(removed.iter().map(|s| s.id).collect::<Vec<_>>(), vec![2]);
        assert!(page
            .erase_at(glam::Vec2::new(100.0, 100.0), 30.0, 12.0, EraserMode::All)
            .is_empty());

        // Auswahlrechteck: Bild nur bei vollständigem Einschluss
        assert!(page
            .find_strokes_in_box(glam::Vec2::new(150.0, 120.0), glam::Vec2::new(250.0, 180.0))
            .is_empty());
        assert_eq!(
            page.find_strokes_in_box(glam::Vec2::new(90.0, 90.0), glam::Vec2::new(310.0, 210.0)),
            vec![1]
        );

        // Umfärben ändert Bilder nicht, Drehen bewegt die Ecken mit
        let mut image = page.strokes[0].clone();
        image.set_color([1.0, 0.0, 0.0, 1.0]);
        assert_eq!(image.color, [1.0, 1.0, 1.0, 1.0]);
        image.rotate(glam::Vec2::new(200.0, 150.0), std::f32::consts::FRAC_PI_2);
        let rotated = image.image_corners().unwrap();
        assert!((rotated[0] - glam::Vec2::new(250.0, 50.0)).length() < 1e-3);
    }

    fn line_samples(points: &[(f32, f32)]) -> Vec<StylusSample> {
        points
            .iter()
            .enumerate()
            .map(|(i, &(x, y))| StylusSample {
                position: glam::Vec2::new(x, y),
                pressure: 0.5,
                timestamp: i as f64,
            })
            .collect()
    }

    #[test]
    fn test_split_samples_at_circle() {
        let c = glam::Vec2::new(50.0, 0.0);
        // Lange Strecke mit nur zwei Punkten, Kreis in der Mitte: zwei Stücke, am Rand gekürzt
        let pieces =
            split_samples_at_circle(&line_samples(&[(0.0, 0.0), (100.0, 0.0)]), c, 10.0, 1.0)
                .unwrap();
        assert_eq!(pieces.len(), 2);
        assert!((pieces[0].last().unwrap().position.x - 40.0).abs() < 1e-3);
        assert!((pieces[1][0].position.x - 60.0).abs() < 1e-3);
        // Druck und Zeit werden am Schnittpunkt interpoliert
        assert!((pieces[0].last().unwrap().timestamp - 0.4).abs() < 1e-3);

        // Viele Punkte, Kreis am Ende: ein Stück bleibt
        let dense: Vec<(f32, f32)> = (0..=10).map(|i| (i as f32 * 10.0, 0.0)).collect();
        let pieces = split_samples_at_circle(
            &line_samples(&dense),
            glam::Vec2::new(100.0, 0.0),
            15.0,
            1.0,
        )
        .unwrap();
        assert_eq!(pieces.len(), 1);
        assert!((pieces[0].last().unwrap().position.x - 85.0).abs() < 1e-3);

        // Ganz im Kreis: nichts bleibt; Kreis weit weg: unberührt
        assert_eq!(
            split_samples_at_circle(&line_samples(&[(48.0, 0.0), (52.0, 0.0)]), c, 10.0, 1.0),
            Some(Vec::new())
        );
        assert_eq!(
            split_samples_at_circle(
                &line_samples(&dense),
                glam::Vec2::new(50.0, 40.0),
                10.0,
                1.0
            ),
            None
        );
        // Winziger Rest wird verworfen
        let pieces = split_samples_at_circle(
            &line_samples(&[(0.0, 0.0), (100.0, 0.0)]),
            glam::Vec2::new(5.0, 0.0),
            4.0,
            2.0,
        )
        .unwrap();
        assert_eq!(pieces.len(), 1);
        assert!((pieces[0][0].position.x - 9.0).abs() < 1e-3);
    }

    #[test]
    fn test_partial_erase_splits_and_undoes_in_one_step() {
        let mut notebook = Notebook::new();
        let mut stroke = Stroke::new(
            1,
            line_samples(&[(100.0, 100.0), (500.0, 100.0)]),
            Vec::new(),
            [0.0, 0.0, 0.0, 1.0],
        );
        stroke.width = 5.0;
        notebook.next_stroke_id = 10;
        let page = notebook.current_page_mut();
        page.add_stroke(stroke);
        page.add_stroke(Stroke::new_image(
            2,
            "a.png".into(),
            [
                glam::Vec2::new(0.0, 0.0),
                glam::Vec2::new(900.0, 0.0),
                glam::Vec2::new(900.0, 900.0),
                glam::Vec2::new(0.0, 900.0),
            ],
        ));
        let mut shape = crate::geometry::shape_to_stroke(
            crate::geometry::RecognizedShape::Line {
                start: glam::Vec2::new(100.0, 300.0),
                end: glam::Vec2::new(500.0, 300.0),
            },
            3,
            [0.0, 0.0, 0.0, 1.0],
            5.0,
        );
        shape.recompute_aabb();
        page.add_stroke(shape);
        let undo_depth = page.history.undo_stack.len();

        // Zwei Schnitte desselben Zugs durch die Linie
        assert!(notebook.erase_at(
            glam::Vec2::new(200.0, 100.0),
            20.0,
            12.0,
            EraserMode::Partial
        ));
        assert!(notebook.erase_at(
            glam::Vec2::new(400.0, 100.0),
            20.0,
            12.0,
            EraserMode::Partial
        ));
        // Form wird als Ganzes radiert
        assert!(notebook.erase_at(
            glam::Vec2::new(300.0, 300.0),
            20.0,
            12.0,
            EraserMode::Partial
        ));
        let page = notebook.current_page();
        let ink: Vec<&Stroke> = page.strokes.iter().filter(|s| !s.is_image()).collect();
        assert_eq!(ink.len(), 3, "Linie in drei Stücke zerlegt, Form entfernt");
        assert!(ink.iter().all(|s| s.id >= 10 && !s.vertices.is_empty()));
        assert!(page.strokes.iter().any(|s| s.id == 2), "Bild bleibt");
        assert!(notebook.next_stroke_id > 10);

        // Ein einziger Undo-Schritt stellt alles wieder her
        let page = notebook.current_page_mut();
        assert_eq!(page.history.undo_stack.len(), undo_depth + 1);
        assert!(page.undo());
        let mut ids: Vec<u64> = page.strokes.iter().map(|s| s.id).collect();
        ids.sort();
        assert_eq!(ids, vec![1, 2, 3]);
        assert!(page.redo());
        assert_eq!(page.strokes.iter().filter(|s| !s.is_image()).count(), 3);

        // Danebenradieren ändert nichts
        assert!(!notebook.erase_at(
            glam::Vec2::new(800.0, 800.0),
            20.0,
            12.0,
            EraserMode::Partial
        ));
    }

    #[test]
    fn test_insert_page_after_current() {
        let mut nb = Notebook::new();
        nb.add_page(PageBackground::Lined);
        nb.add_page(PageBackground::Lined);
        let ids: Vec<u64> = nb.pages.iter().map(|p| p.id).collect();
        nb.current_page_index = 1;
        nb.pages[1].background = PageBackground::DotGrid;
        let index = nb.insert_page_after_current(PageBackground::DotGrid);
        assert_eq!(index, 2);
        assert_eq!(nb.current_page_index, 1);
        assert_eq!(nb.pages.len(), 4);
        assert_eq!(nb.pages[0].id, ids[0]);
        assert_eq!(nb.pages[1].id, ids[1]);
        assert_eq!(nb.pages[3].id, ids[2], "alte Seite 3 ist jetzt Seite 4");
        let new = &nb.pages[2];
        assert!(!ids.contains(&new.id) && new.strokes.is_empty());
        assert_eq!(new.background, PageBackground::DotGrid);
        assert!(new.pdf_background.is_none());
    }

    #[test]
    fn test_append_page_when_writing_at_bottom_of_last_page() {
        let mut nb = Notebook::new();
        nb.current_page_mut().background = PageBackground::Lined;
        // Oben auf der Seite: nichts
        assert!(!nb.append_page_if_writing_at_end(PAGE_HEIGHT * 0.5));
        assert_eq!(nb.pages.len(), 1);
        // Unten: neue Seite mit gleichem Hintergrund, die aktuelle Seite bleibt
        assert!(nb.append_page_if_writing_at_end(PAGE_HEIGHT - 10.0));
        assert_eq!(nb.pages.len(), 2);
        assert_eq!(nb.current_page_index, 0);
        assert_eq!(nb.pages[1].background, PageBackground::Lined);
        // Nicht auf der letzten Seite: nichts
        assert!(!nb.append_page_if_writing_at_end(PAGE_HEIGHT - 10.0));
        assert_eq!(nb.pages.len(), 2);
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
