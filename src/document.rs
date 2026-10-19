//! Notizbuch- und Seitenverwaltung mit DIN-A4-Koordinatensystem und Vektor-Stroke-Modell.

use std::collections::{HashMap, HashSet};
use std::ops::Range;

use crate::canvas::InkVertex;
use crate::history::{HistoryAction, HistoryStack};
pub use crate::spline::{finalize_stroke, finalize_stroke_with_config};
use crate::t;

/// Standardmaße DIN A4 in Millimetern.
pub const A4_WIDTH_MM: f32 = 210.0;
pub const A4_HEIGHT_MM: f32 = 297.0;
/// Seitenverhältnis DIN A4 (1 / sqrt(2) ≈ 0.7071).
pub const A4_ASPECT_RATIO: f32 = A4_WIDTH_MM / A4_HEIGHT_MM;

/// Virtuelle Seitenauflösung in internen Einheiten (10 Einheiten = 1 mm).
pub const PAGE_WIDTH: f32 = 2100.0;
pub const PAGE_HEIGHT: f32 = 2970.0;
/// Größe einer DIN-A4-Hochformatseite (Standard) in Seiteneinheiten.
pub const A4_SIZE: glam::Vec2 = glam::Vec2::new(PAGE_WIDTH, PAGE_HEIGHT);
pub const UNITS_PER_MM: f32 = 10.0;

/// Größe der Seite eines endlosen Notizbuchs (10 × 10 m, Issue #30): praktisch unbegrenzt,
/// aber mit festen, positiven Koordinaten – Ansicht, Speichern und Rückgängig funktionieren
/// so wie bei jeder anderen Seite.
pub const ENDLESS_PAGE_SIZE: glam::Vec2 = glam::Vec2::new(100_000.0, 100_000.0);
/// Linke obere Ecke des Startausschnitts (ein DIN-A4-Rahmen in der Mitte der endlosen Seite).
pub const ENDLESS_HOME: glam::Vec2 = glam::Vec2::new(
    (ENDLESS_PAGE_SIZE.x - PAGE_WIDTH) / 2.0,
    (ENDLESS_PAGE_SIZE.y - PAGE_HEIGHT) / 2.0,
);
/// Rand um den Inhalt beim Export einer endlosen Seite (10 mm).
const ENDLESS_EXPORT_MARGIN: f32 = 100.0;
/// Raster, auf das der Exportausschnitt einer endlosen Seite fällt: Vielfaches der Abstände von
/// Karos/Punkten (5 mm) und Linien (7 mm), damit das Muster wie am Bildschirm liegt.
const ENDLESS_EXPORT_SNAP: f32 = 350.0;

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

    /// Ecken (oben links, oben rechts, unten rechts, unten links) der PDF-Seite auf einem
    /// Blatt der Größe `page_size`, siehe [`pdf_placement`].
    pub fn corners(&self, page_size: glam::Vec2) -> [glam::Vec2; 4] {
        pdf_placement(self.width_pt, self.height_pt, page_size)
    }
}

/// Seitengröße für eine importierte PDF-Seite von `width_pt` × `height_pt`: Hochformat wird
/// DIN A4 hoch (wie bisher), Querformat (z. B. Folien) eine Seite mit der Breite von A4 quer
/// (297 mm) und dem Seitenverhältnis der PDF-Seite – ohne leeren Streifen darunter.
pub fn pdf_page_size(width_pt: f32, height_pt: f32) -> glam::Vec2 {
    let (w, h) = (width_pt.max(1.0), height_pt.max(1.0));
    if w <= h {
        return A4_SIZE;
    }
    // Auf ganze Zehntelmillimeter runden, damit gleich große Folien gleich große Seiten geben
    glam::Vec2::new(PAGE_HEIGHT, (PAGE_HEIGHT * h / w).round().max(1.0))
}

/// Lage einer PDF-Seite von `width_pt` × `height_pt` auf einem Blatt der Größe `page_size`:
/// so groß wie möglich, waagerecht zentriert und oben bündig.
pub fn pdf_placement(width_pt: f32, height_pt: f32, page_size: glam::Vec2) -> [glam::Vec2; 4] {
    let (w, h) = (width_pt.max(1.0), height_pt.max(1.0));
    let scale = (page_size.x / w).min(page_size.y / h);
    let size = glam::Vec2::new(w * scale, h * scale);
    let tl = glam::Vec2::new((page_size.x - size.x) / 2.0, 0.0);
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
/// Breite und 60 % der Höhe von `reference` (meist die Seitengröße, auf der endlosen Seite
/// DIN A4), und möglichst ganz auf dem Blatt der Größe `page_size`.
pub fn place_image(
    width: u32,
    height: u32,
    center: glam::Vec2,
    page_size: glam::Vec2,
    reference: glam::Vec2,
) -> [glam::Vec2; 4] {
    let natural =
        glam::Vec2::new(width.max(1) as f32, height.max(1) as f32) * IMAGE_UNITS_PER_PIXEL;
    let fit = (reference.x * 0.8 / natural.x)
        .min(reference.y * 0.6 / natural.y)
        .min(1.0);
    let size = natural * fit;
    let half = size / 2.0;
    let center = glam::Vec2::new(
        center.x.clamp(half.x, page_size.x - half.x),
        center.y.clamp(half.y, page_size.y - half.y),
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
        // Ohne Zwischenliste: läuft beim Ziehen einer Auswahl für jeden Strich und jede Bewegung
        let bounds = |mut pts: Box<dyn Iterator<Item = glam::Vec2> + '_>| {
            pts.next().map_or(Aabb::new(glam::Vec2::ZERO, glam::Vec2::ZERO), |first| {
                pts.fold(Aabb::new(first, first), |mut aabb, p| {
                    aabb.expand_by_point(p);
                    aabb
                })
            })
        };
        self.aabb = if !self.points.is_empty() {
            bounds(Box::new(self.points.iter().map(|s| s.position)))
        } else {
            bounds(Box::new(
                self.vertices
                    .iter()
                    .map(|v| glam::Vec2::new(v.position[0], v.position[1])),
            ))
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
    /// Seitengröße in Seiteneinheiten (Standard DIN A4 hoch, Querformat für PDF-Folien); auch im
    /// Manifest, damit sie ohne Laden der Seite bekannt ist
    pub size: glam::Vec2,
    /// Importierte PDF-Seite als Hintergrund (unter Bildern und Tinte)
    pub pdf_background: Option<PdfBackground>,
    /// Seitentitel (Lesezeichen); auch im Manifest, damit er ohne Laden der Seite bekannt ist
    pub title: Option<String>,
    pub vertex_cache: Vec<InkVertex>,
    pub stroke_ranges: Vec<Range<u32>>,
    pub is_dirty: bool,
    /// Zählt die Neuaufbauten des Vertex-Caches (erkennt geänderte Nachbarseiten)
    pub cache_generation: u64,
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
            size: A4_SIZE,
            pdf_background: None,
            title: None,
            vertex_cache: Vec::new(),
            stroke_ranges: Vec::new(),
            is_dirty: false,
            is_loaded: true,
            persist: PersistState::Normal,
            history: HistoryStack::new(),
            cache_generation: 0,
        }
    }

    /// Erzeugt einen noch nicht aus dem Speicher geladenen Seiten-Platzhalter (Lazy Loading).
    pub fn new_unloaded(id: u64, background: PageBackground) -> Self {
        Self {
            id,
            strokes: Vec::new(),
            background,
            size: A4_SIZE,
            pdf_background: None,
            title: None,
            vertex_cache: Vec::new(),
            stroke_ranges: Vec::new(),
            is_dirty: false,
            is_loaded: false,
            persist: PersistState::Normal,
            history: HistoryStack::new(),
            cache_generation: 0,
        }
    }

    /// Dieselbe Seite mit der Größe `size`.
    pub fn with_size(mut self, size: glam::Vec2) -> Self {
        self.size = size;
        self
    }

    /// Seite eines endlosen Notizbuchs (siehe [`ENDLESS_PAGE_SIZE`]).
    pub fn is_endless(&self) -> bool {
        self.size == ENDLESS_PAGE_SIZE
    }

    /// Bezugsgröße für Einfügen und Textumbruch: DIN A4 auf der endlosen Seite, sonst die Seite.
    pub fn reference_size(&self) -> glam::Vec2 {
        if self.is_endless() {
            A4_SIZE
        } else {
            self.size
        }
    }

    /// Rechter Rand für ein neues Textfeld bei `x`: auf der endlosen Seite eine A4-Breite
    /// weiter, sonst der Seitenrand.
    pub fn text_right_edge(&self, x: f32) -> f32 {
        if self.is_endless() {
            x + PAGE_WIDTH
        } else {
            self.size.x
        }
    }

    /// Kopie der Seite als Ausschnitt ab `origin` mit Größe `size`: Alle Objekte werden um
    /// `-origin` verschoben (für Export und Bilder einer Auswahl).
    pub fn cropped(&self, origin: glam::Vec2, size: glam::Vec2) -> Page {
        let mut page = self.clone();
        for stroke in &mut page.strokes {
            stroke.translate(-origin);
        }
        page.size = size;
        page.is_dirty = true;
        page
    }

    /// Exportausschnitt einer endlosen Seite: der Inhalt mit 10 mm Rand (leer: der
    /// Startausschnitt). Die linke obere Ecke liegt auf dem Musterraster.
    pub fn endless_export_bounds(&self) -> (glam::Vec2, glam::Vec2) {
        let Some(content) = Self::selection_aabb(&self.strokes) else {
            return (ENDLESS_HOME, A4_SIZE);
        };
        let max_width = self.strokes.iter().map(|s| s.width).fold(0.0, f32::max);
        let content = content.expanded(max_width / 2.0 + ENDLESS_EXPORT_MARGIN);
        let origin = (content.min / ENDLESS_EXPORT_SNAP).floor() * ENDLESS_EXPORT_SNAP;
        let origin = origin.max(glam::Vec2::ZERO);
        let end = content.max.min(self.size);
        (origin, (end - origin).max(glam::Vec2::splat(100.0)))
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
        self.cache_generation += 1;
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
        let ids: HashSet<u64> = ids.iter().copied().collect();
        self.strokes
            .iter()
            .filter(|s| ids.contains(&s.id))
            .cloned()
            .collect()
    }

    /// Ersetzt die Striche mit den IDs aus `modified` an ihrer Stelle. Über eine Tabelle
    /// statt je Strich die ganze Seite zu durchsuchen: Beim Ziehen einer großen Auswahl
    /// läuft das bei jeder Stiftbewegung.
    fn replace_strokes(&mut self, modified: &[Stroke]) {
        let index: HashMap<u64, usize> = self
            .strokes
            .iter()
            .enumerate()
            .map(|(i, s)| (s.id, i))
            .collect();
        for m in modified {
            if let Some(&pos) = index.get(&m.id) {
                self.strokes[pos] = m.clone();
            }
        }
    }

    /// Aktualisiert eine Menge geänderter Striche transaktional im History-Stack.
    pub fn modify_strokes(&mut self, modified: Vec<Stroke>) {
        if modified.is_empty() {
            return;
        }
        let ids: Vec<u64> = modified.iter().map(|s| s.id).collect();
        let before = self.get_strokes(&ids);
        self.replace_strokes(&modified);

        self.history.push(HistoryAction::ModifyStrokes {
            before,
            after: modified,
        });
        self.is_dirty = true;
    }

    /// Aktualisiert Striche flüchtig (z. B. während des Ziehens / Dragging), OHNE die History zu verändern.
    pub fn update_strokes_ephemeral(&mut self, modified: &[Stroke]) {
        self.replace_strokes(modified);
        self.is_dirty = true;
    }

    /// Wie [`Self::update_strokes_ephemeral`] für eine Transformation der Ausgangsstriche
    /// `originals`: setzt die Striche auf der Seite auf ihren Ausgangszustand zurück und wendet
    /// `transform` an. Verwendet die Puffer der Seitenstriche weiter, statt bei jeder
    /// Stiftbewegung die ganze Auswahl zweimal zu kopieren.
    pub fn transform_strokes_ephemeral(
        &mut self,
        originals: &[Stroke],
        transform: impl Fn(&mut Stroke),
    ) {
        let index: HashMap<u64, usize> = self
            .strokes
            .iter()
            .enumerate()
            .map(|(i, s)| (s.id, i))
            .collect();
        for original in originals {
            if let Some(&pos) = index.get(&original.id) {
                let stroke = &mut self.strokes[pos];
                // Nur was Verschieben, Drehen und Skalieren verändern
                stroke.points.clone_from(&original.points);
                stroke.vertices.clone_from(&original.vertices);
                stroke.shape.clone_from(&original.shape);
                transform(stroke);
            }
        }
        self.is_dirty = true;
    }

    /// Schließt eine Transformationsoperation transaktional mit vorherigem und finalem Zustand ab.
    pub fn commit_modified_strokes(&mut self, before: Vec<Stroke>, after: Vec<Stroke>) {
        if before.is_empty() || after.is_empty() {
            return;
        }
        self.replace_strokes(&after);
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

/// Anteil der Seitenhöhe am unteren Rand der letzten Seite, in dem Schreiben in der
/// durchgehenden Ansicht automatisch eine neue Seite anhängt.
pub const AUTO_APPEND_FRACTION: f32 = 0.2;

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

    /// Rahmen, in den jede Seite des Notizbuchs passt (größte Breite und Höhe).
    pub fn fit_box(&self) -> glam::Vec2 {
        self.pages
            .iter()
            .map(|p| p.size)
            .fold(glam::Vec2::ZERO, glam::Vec2::max)
            .max(glam::Vec2::ONE)
    }

    /// Durchgehende Ansicht: Lage aller Seiten relativ zur aktuellen.
    pub fn page_stack(&self) -> crate::viewport::PageStack {
        crate::viewport::PageStack::new(
            self.pages.iter().map(|p| p.size).collect(),
            self.current_page_index,
        )
    }

    /// Endloses Notizbuch: eine einzige, praktisch unbegrenzte Seite (Issue #30).
    pub fn is_endless(&self) -> bool {
        self.pages.len() == 1 && self.pages[0].is_endless()
    }

    /// Macht aus einem Notizbuch mit einer einzigen Seite (ohne importiertes PDF) ein
    /// endloses: Die Seite wird zur endlosen Fläche, ihr Inhalt landet im Startausschnitt.
    /// Der Verlauf der Seite wird geleert (er kennt nur die alten Koordinaten).
    pub fn make_endless(&mut self) -> bool {
        if !self.can_make_endless() {
            return false;
        }
        let page = &mut self.pages[0];
        for stroke in &mut page.strokes {
            stroke.translate(ENDLESS_HOME);
        }
        page.size = ENDLESS_PAGE_SIZE;
        page.history.clear();
        page.is_dirty = true;
        true
    }

    /// Siehe [`Self::make_endless`].
    pub fn can_make_endless(&self) -> bool {
        matches!(self.pages.as_slice(), [page] if page.is_loaded
            && !page.is_endless()
            && page.pdf_background.is_none())
    }

    /// Siehe [`Self::make_paged`].
    pub fn can_make_paged(&self) -> bool {
        if !self.is_endless() || !self.pages[0].is_loaded {
            return false;
        }
        Page::selection_aabb(&self.pages[0].strokes).is_none_or(|content| {
            content.min.cmpge(ENDLESS_HOME).all() && content.max.cmple(ENDLESS_HOME + A4_SIZE).all()
        })
    }

    /// Macht aus einem endlosen Notizbuch wieder eine DIN-A4-Seite – nur, wenn der Inhalt im
    /// Startausschnitt liegt (sonst ginge er über den Seitenrand). Leert den Verlauf der Seite.
    pub fn make_paged(&mut self) -> bool {
        if !self.can_make_paged() {
            return false;
        }
        let page = &mut self.pages[0];
        for stroke in &mut page.strokes {
            stroke.translate(-ENDLESS_HOME);
        }
        page.size = A4_SIZE;
        page.history.clear();
        page.is_dirty = true;
        true
    }

    /// Hängt eine neue Seite mit dem angegebenen Hintergrund und der Größe der aktuellen
    /// Seite an.
    pub fn add_page(&mut self, background: PageBackground) -> usize {
        let id = self.next_page_id;
        self.next_page_id += 1;
        let size = self
            .pages
            .get(self.current_page_index)
            .map_or(A4_SIZE, |p| p.size);
        self.pages.push(Page::new(id, background).with_size(size));
        self.pages.len() - 1
    }

    /// Fügt direkt nach der aktuellen Seite eine leere Seite mit dem angegebenen
    /// Hintergrund und der Größe der aktuellen Seite ein (die aktuelle Seite bleibt aktuell)
    /// und gibt ihren Index zurück.
    pub fn insert_page_after_current(&mut self, background: PageBackground) -> usize {
        let id = self.next_page_id;
        self.next_page_id += 1;
        let index = (self.current_page_index + 1).min(self.pages.len());
        let size = self
            .pages
            .get(self.current_page_index)
            .map_or(A4_SIZE, |p| p.size);
        self.pages
            .insert(index, Page::new(id, background).with_size(size));
        index
    }

    /// Entfernt die aktuelle Seite und gibt sie zurück; danach ist die folgende Seite aktuell
    /// (bzw. die vorige, wenn es die letzte war). Die einzige Seite eines Notizbuchs bleibt
    /// stehen (`None`).
    pub fn remove_current_page(&mut self) -> Option<Page> {
        if self.pages.len() < 2 {
            return None;
        }
        let page = self.pages.remove(self.current_page_index);
        self.load_warnings.remove(&page.id);
        self.current_page_index = self.current_page_index.min(self.pages.len() - 1);
        Some(page)
    }

    /// Setzt eine mit [`Self::remove_current_page`] entfernte Seite wieder an `index` ein
    /// (höchstens ans Ende) und macht sie zur aktuellen Seite; gibt ihren Index zurück.
    pub fn restore_page(&mut self, index: usize, page: Page) -> usize {
        let index = index.min(self.pages.len());
        self.next_page_id = self.next_page_id.max(page.id + 1);
        self.pages.insert(index, page);
        self.current_page_index = index;
        index
    }

    /// Durchgehende Ansicht: Reicht ein neuer Strich auf der letzten Seite bis in deren
    /// unteres Fünftel ([`AUTO_APPEND_FRACTION`]), wird darunter eine leere Seite mit gleichem
    /// Hintergrund angehängt. `true`, wenn eine Seite angehängt wurde.
    pub fn append_page_if_writing_at_end(&mut self, stroke_bottom: f32) -> bool {
        if self.current_page_index + 1 != self.pages.len()
            || stroke_bottom < self.current_page().size.y * (1.0 - AUTO_APPEND_FRACTION)
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
        t!(
            "Seite {number} / {count}",
            number = self.current_page_index + 1,
            count = self.pages.len()
        )
    }
}

impl Default for Notebook {
    fn default() -> Self {
        Self::new()
    }
}

/// Abstand zum oberen Rand, mit dem ein Objekt auf der nächsten Seite landet, wenn es beim
/// Platzschaffen über das Seitenende rutschen würde (10 mm).
pub const SPACE_PAGE_BREAK_MARGIN: f32 = 10.0 * UNITS_PER_MM;

/// Ein beim Platzschaffen verschobener Strich: vorher und nachher, jeweils mit Seite.
#[derive(Debug, Clone)]
pub struct StrokeMove {
    pub from_page: u64,
    pub before: Stroke,
    pub to_page: u64,
    pub after: Stroke,
}

/// Für das Platzschaffen neu eingefügte Seite.
#[derive(Debug, Clone)]
pub struct AddedPage {
    pub id: u64,
    /// Seite, hinter der sie liegt
    pub after_page: u64,
    pub size: glam::Vec2,
    pub background: PageBackground,
}

/// Ein Platzschaffen-Schritt (für Rückgängig und Wiederholen).
#[derive(Debug, Clone)]
pub struct SpaceShift {
    pub moves: Vec<StrokeMove>,
    pub added_pages: Vec<AddedPage>,
}

impl Notebook {
    /// Seiten, die beim Platzschaffen auf Seite `page_index` mitrutschen: sie selbst und die
    /// folgenden bis vor die nächste Seite mit importiertem PDF (deren Notizen gehören zur
    /// PDF-Seite). `None` auf einer PDF-Seite.
    pub fn space_run(&self, page_index: usize) -> Option<std::ops::Range<usize>> {
        let page = self.pages.get(page_index)?;
        if page.pdf_background.is_some() {
            return None;
        }
        let end = (page_index + 1..self.pages.len())
            .find(|&i| self.pages[i].pdf_background.is_some())
            .unwrap_or(self.pages.len());
        Some(page_index..end)
    }

    /// Wie weit sich der Inhalt unter `y` auf Seite `page_index` höchstens nach oben schieben
    /// lässt: bis an das unterste Objekt darüber (bzw. den oberen Seitenrand).
    pub fn space_up_limit(&self, page_index: usize, y: f32) -> f32 {
        let Some(page) = self.pages.get(page_index) else {
            return 0.0;
        };
        let bottom = page
            .strokes
            .iter()
            .filter(|s| s.aabb.min.y < y)
            .map(|s| s.aabb.max.y)
            .fold(0.0_f32, f32::max);
        (y - bottom).max(0.0)
    }

    /// Platz schaffen: Alles, was auf Seite `page_index` unterhalb von `y` beginnt, und der
    /// ganze Inhalt der folgenden Seiten (siehe [`Self::space_run`]) rutscht um `dy` nach unten
    /// (negativ: nach oben, höchstens bis [`Self::space_up_limit`]). Die Seiten gelten dabei
    /// als durchgehende Spalte: Was über ein Seitenende hinausragen würde, beginnt auf der
    /// nächsten Seite (mit [`SPACE_PAGE_BREAK_MARGIN`] Abstand), alles danach rutscht
    /// entsprechend weiter; fehlende Seiten werden angehängt. Der Schritt landet im Verlauf
    /// der Seite und gibt die Kennungen der geänderten Seiten zurück (`None`: nichts zu tun).
    pub fn make_space(&mut self, page_index: usize, y: f32, dy: f32) -> Option<Vec<u64>> {
        let run = self.space_run(page_index)?;
        if self.pages[run.clone()].iter().any(|p| !p.is_loaded) {
            return None;
        }
        let dy = if dy < 0.0 {
            dy.max(-self.space_up_limit(page_index, y))
        } else {
            dy
        };
        if dy.abs() < 1.0 {
            return None;
        }
        // Endlose Seite: keine weiteren Seiten anlegen, nichts über den Rand schieben
        let page = &self.pages[page_index];
        if page.is_endless()
            && page
                .strokes
                .iter()
                .any(|s| s.aabb.min.y >= y && s.aabb.max.y + dy > page.size.y)
        {
            return None;
        }

        // Seiten als durchgehende Spalte: Lage jeder Seite (auch noch anzulegender)
        let last_size = self.pages[run.end - 1].size;
        let mut tops = Vec::new();
        let mut heights = Vec::new();
        let mut top = 0.0;
        for page in &self.pages[run.clone()] {
            tops.push(top);
            heights.push(page.size.y);
            top += page.size.y;
        }

        // Verschoben wird alles unterhalb von y, in der Reihenfolge von oben nach unten
        let mut movable: Vec<(usize, f32, Stroke)> = Vec::new();
        for (k, page) in self.pages[run.clone()].iter().enumerate() {
            for stroke in &page.strokes {
                if k > 0 || stroke.aabb.min.y >= y {
                    movable.push((k, tops[k] + stroke.aabb.min.y, stroke.clone()));
                }
            }
        }
        if movable.is_empty() {
            return None;
        }
        movable.sort_by(|a, b| a.1.total_cmp(&b.1));

        let mut extra = 0.0_f32;
        let mut targets = Vec::with_capacity(movable.len());
        for (_, global_top, stroke) in &movable {
            let height = stroke.aabb.max.y - stroke.aabb.min.y;
            let mut new_top = global_top + dy + extra;
            let mut target = 0;
            for _ in 0..2 {
                // Seite, auf der das Objekt beginnt (bei Bedarf weitere Seiten)
                target = 0;
                while target + 1 < tops.len() && tops[target + 1] <= new_top {
                    target += 1;
                }
                while tops[target] + heights[target] <= new_top {
                    tops.push(tops[target] + heights[target]);
                    heights.push(last_size.y);
                    target += 1;
                }
                let page_end = tops[target] + heights[target];
                let fits_on_a_page = height <= heights[target] - 2.0 * SPACE_PAGE_BREAK_MARGIN;
                if new_top + height <= page_end || !fits_on_a_page {
                    break;
                }
                // Ragt über das Seitenende: auf die nächste Seite, alles Folgende rutscht mit
                let margin = if dy > 0.0 {
                    SPACE_PAGE_BREAK_MARGIN
                } else {
                    0.0
                };
                let bump = page_end + margin - new_top;
                extra += bump;
                new_top += bump;
            }
            targets.push((target, new_top));
        }

        // Fehlende Seiten hinter dem Abschnitt einfügen
        let needed = targets.iter().map(|(t, _)| t + 1).max().unwrap_or(0);
        let mut added_pages = Vec::new();
        let background = self.pages[run.end - 1].background;
        for k in run.len()..needed {
            let after_page = self.pages[run.start + k - 1].id;
            let id = self.next_page_id;
            self.next_page_id += 1;
            self.pages.insert(
                run.start + k,
                Page::new(id, background).with_size(last_size),
            );
            added_pages.push(AddedPage {
                id,
                after_page,
                size: last_size,
                background,
            });
        }

        // Umbuchen
        let mut moves = Vec::with_capacity(movable.len());
        for ((k, _, before), (target, new_top)) in movable.into_iter().zip(targets) {
            let mut after = before.clone();
            after.translate(glam::Vec2::new(
                0.0,
                new_top - tops[target] - before.aabb.min.y,
            ));
            moves.push(StrokeMove {
                from_page: self.pages[run.start + k].id,
                before,
                to_page: self.pages[run.start + target].id,
                after,
            });
        }
        let shift = SpaceShift { moves, added_pages };
        let changed = self.apply_space_moves(&shift, false);
        self.pages[page_index]
            .history
            .push(HistoryAction::SpaceShift(Box::new(shift)));
        Some(changed)
    }

    /// Bucht die Striche eines Platzschaffen-Schritts um (`undo`: zurück) und gibt die
    /// geänderten Seiten zurück. Später geänderte oder gelöschte Striche bleiben unberührt.
    fn apply_space_moves(&mut self, shift: &SpaceShift, undo: bool) -> Vec<u64> {
        let mut changed = Vec::new();
        let index_of = |pages: &[Page], id: u64| pages.iter().position(|p| p.id == id);
        for m in &shift.moves {
            let (from, old, to, new) = if undo {
                (m.to_page, &m.after, m.from_page, &m.before)
            } else {
                (m.from_page, &m.before, m.to_page, &m.after)
            };
            let (Some(from_i), Some(to_i)) =
                (index_of(&self.pages, from), index_of(&self.pages, to))
            else {
                continue;
            };
            let Some(pos) = self.pages[from_i]
                .strokes
                .iter()
                .position(|s| s.id == old.id)
            else {
                continue;
            };
            self.pages[from_i].strokes.remove(pos);
            self.pages[to_i].strokes.push(new.clone());
            for i in [from_i, to_i] {
                self.pages[i].is_dirty = true;
                if !changed.contains(&self.pages[i].id) {
                    changed.push(self.pages[i].id);
                }
            }
        }
        changed
    }

    /// Macht die letzte Aktion der aktuellen Seite rückgängig; ein Platzschaffen-Schritt
    /// betrifft dabei alle beteiligten Seiten. Gibt die geänderten Seiten zurück.
    pub fn undo(&mut self) -> Option<Vec<u64>> {
        let index = self.current_page_index;
        if let Some(HistoryAction::SpaceShift(_)) = self.pages[index].history.undo_stack.last() {
            let Some(HistoryAction::SpaceShift(shift)) = self.pages[index].history.undo_stack.pop()
            else {
                unreachable!()
            };
            let mut changed = self.apply_space_moves(&shift, true);
            // Dafür angelegte Seiten wieder entfernen, sofern sie leer geblieben sind
            for added in shift.added_pages.iter().rev() {
                if let Some(i) = self.pages.iter().position(|p| p.id == added.id) {
                    if self.pages[i].strokes.is_empty() && self.pages.len() > 1 {
                        self.pages.remove(i);
                        if self.current_page_index >= i && self.current_page_index > 0 {
                            self.current_page_index -= 1;
                        }
                        changed.retain(|&id| id != added.id);
                    }
                }
            }
            // Der Schritt gehört weiter zur aktuellen Seite (sie selbst wird nie entfernt)
            let owner = self.current_page_index;
            self.pages[owner]
                .history
                .redo_stack
                .push(HistoryAction::SpaceShift(shift));
            return Some(changed);
        }
        let page = &mut self.pages[index];
        page.undo().then(|| vec![page.id])
    }

    /// Stellt die zuletzt rückgängig gemachte Aktion der aktuellen Seite wieder her.
    pub fn redo(&mut self) -> Option<Vec<u64>> {
        let index = self.current_page_index;
        if let Some(HistoryAction::SpaceShift(_)) = self.pages[index].history.redo_stack.last() {
            let Some(HistoryAction::SpaceShift(shift)) = self.pages[index].history.redo_stack.pop()
            else {
                unreachable!()
            };
            // Entfernte Seiten wieder einfügen
            for added in &shift.added_pages {
                if self.pages.iter().any(|p| p.id == added.id) {
                    continue;
                }
                let at = self
                    .pages
                    .iter()
                    .position(|p| p.id == added.after_page)
                    .map_or(self.pages.len(), |i| i + 1);
                self.pages.insert(
                    at,
                    Page::new(added.id, added.background).with_size(added.size),
                );
                if at <= self.current_page_index {
                    self.current_page_index += 1;
                }
            }
            let changed = self.apply_space_moves(&shift, false);
            let owner = self.current_page_index;
            self.pages[owner]
                .history
                .undo_stack
                .push(HistoryAction::SpaceShift(shift));
            return Some(changed);
        }
        let page = &mut self.pages[index];
        page.redo().then(|| vec![page.id])
    }
}

/// Lage der aktuellen Seite im Fenster bei Zoom 100 % ohne Verschiebung: Ein Rahmen der
/// Größe `fit_box` (alle Seiten des Notizbuchs passen hinein) wird ins Fenster eingepasst
/// (Letterboxing/Pillarboxing); die aktuelle Seite liegt darin waagerecht zentriert und oben.
/// So bleibt der Maßstab beim Wechsel zwischen verschieden großen Seiten gleich.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageViewport {
    pub window_width: f32,
    pub window_height: f32,
    /// Bildschirmposition der linken oberen Ecke der aktuellen Seite
    pub page_x: f32,
    pub page_y: f32,
    /// Größe der aktuellen Seite auf dem Bildschirm (Pixel)
    pub page_width: f32,
    pub page_height: f32,
    /// Bildschirmpixel je Seiteneinheit
    pub scale: f32,
    /// Größe der aktuellen Seite in Seiteneinheiten
    pub page_size: glam::Vec2,
}

impl PageViewport {
    /// Zentrierte DIN-A4-Seite im Fenster.
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
        Self::calculate_for(
            window_width,
            window_height,
            margin,
            (inset_left, inset_right),
            A4_SIZE,
            A4_SIZE,
        )
    }

    /// Wie [`Self::calculate_with_insets`] für eine Seite der Größe `page_size` in einem
    /// Notizbuch, dessen Seiten in `fit_box` passen.
    pub fn calculate_for(
        window_width: f32,
        window_height: f32,
        margin: f32,
        (inset_left, inset_right): (f32, f32),
        fit_box: glam::Vec2,
        page_size: glam::Vec2,
    ) -> Self {
        let inset_left = inset_left.clamp(0.0, window_width * 0.5);
        let inset_right = inset_right.clamp(0.0, window_width * 0.5);
        let avail_w = (window_width - inset_left - inset_right - 2.0 * margin).max(10.0);
        let avail_h = (window_height - 2.0 * margin).max(10.0);
        let fit_box = fit_box.max(page_size).max(glam::Vec2::ONE);

        let scale = (avail_w / fit_box.x).min(avail_h / fit_box.y);
        let box_x =
            inset_left + (window_width - inset_left - inset_right - fit_box.x * scale) / 2.0;
        let box_y = (window_height - fit_box.y * scale) / 2.0;

        Self {
            window_width,
            window_height,
            page_x: box_x + (fit_box.x - page_size.x) / 2.0 * scale,
            page_y: box_y,
            page_width: page_size.x * scale,
            page_height: page_size.y * scale,
            scale,
            page_size,
        }
    }

    /// Ansicht der endlosen Seite: Maßstab und Lage wie bei einer DIN-A4-Seite, die am
    /// Startausschnitt ([`ENDLESS_HOME`]) der endlosen Seite liegt.
    pub fn calculate_endless(
        window_width: f32,
        window_height: f32,
        margin: f32,
        insets: (f32, f32),
    ) -> Self {
        let a4 = Self::calculate_for(
            window_width,
            window_height,
            margin,
            insets,
            A4_SIZE,
            A4_SIZE,
        );
        Self {
            page_x: a4.page_x - ENDLESS_HOME.x * a4.scale,
            page_y: a4.page_y - ENDLESS_HOME.y * a4.scale,
            page_width: ENDLESS_PAGE_SIZE.x * a4.scale,
            page_height: ENDLESS_PAGE_SIZE.y * a4.scale,
            page_size: ENDLESS_PAGE_SIZE,
            ..a4
        }
    }

    /// Transformiert einen Fenster-Pixelpunkt in Seitenkoordinaten.
    ///
    /// Liefert `None`, falls der Punkt außerhalb der Seite liegt.
    pub fn screen_to_page(&self, screen_x: f32, screen_y: f32) -> Option<glam::Vec2> {
        let p = glam::Vec2::new(screen_x - self.page_x, screen_y - self.page_y) / self.scale;
        ((0.0..=self.page_size.x).contains(&p.x) && (0.0..=self.page_size.y).contains(&p.y))
            .then_some(p)
    }

    /// Transformiert einen Fenster-Pixelpunkt und klemmt ihn auf die Seite.
    pub fn screen_to_page_clamped(&self, screen_x: f32, screen_y: f32) -> glam::Vec2 {
        let p = glam::Vec2::new(screen_x - self.page_x, screen_y - self.page_y) / self.scale;
        p.clamp(glam::Vec2::ZERO, self.page_size)
    }

    /// Berechnet die Shader-Transformationsparameter (Scale, Offset),
    /// um Seitenkoordinaten direkt im Vertex-Shader in den NDC-Bereich [-1.0, 1.0] abzubilden.
    pub fn ndc_transform(&self) -> ([f32; 2], [f32; 2]) {
        let scale_x = (2.0 * self.scale) / self.window_width;
        let scale_y = (-2.0 * self.scale) / self.window_height;
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
    fn test_pdf_page_size_follows_orientation() {
        // Hochformat (A4, Letter) bleibt A4 hoch
        assert_eq!(pdf_page_size(595.0, 842.0), A4_SIZE);
        assert_eq!(pdf_page_size(612.0, 792.0), A4_SIZE);
        // 16:9-Folie: 297 mm breit, Höhe nach Seitenverhältnis
        let slide = pdf_page_size(960.0, 540.0);
        assert_eq!(slide, glam::Vec2::new(2970.0, 1671.0));
        // Die Folie füllt ihre Seite ganz aus
        let [tl, _, br, _] = pdf_placement(960.0, 540.0, slide);
        assert!(
            tl.length() < 1e-3 && (br - slide).length() < 1.0,
            "{tl} {br}"
        );
    }

    #[test]
    fn test_viewport_scale_is_shared_by_pages_of_different_size() {
        let slide = glam::Vec2::new(2970.0, 1671.0);
        // Nur Folien: die Folie wird ins Fenster eingepasst
        let only_slides =
            PageViewport::calculate_for(1600.0, 900.0, 20.0, (0.0, 0.0), slide, slide);
        // 1560 × 860 frei: die Höhe begrenzt
        assert!(
            (only_slides.page_height - 860.0).abs() < 0.5,
            "{only_slides:?}"
        );
        assert!(
            (only_slides.page_width / only_slides.page_height - slide.x / slide.y).abs() < 1e-3
        );
        // Gemischt: gleicher Maßstab für beide Seiten, schmale Seite waagerecht zentriert
        let fit = A4_SIZE.max(slide);
        let a4 = PageViewport::calculate_for(1600.0, 900.0, 20.0, (0.0, 0.0), fit, A4_SIZE);
        let wide = PageViewport::calculate_for(1600.0, 900.0, 20.0, (0.0, 0.0), fit, slide);
        assert_eq!(a4.scale, wide.scale);
        let center = |v: &PageViewport| v.page_x + v.page_width / 2.0;
        assert!((center(&a4) - center(&wide)).abs() < 1e-3);
        assert_eq!(a4.page_y, wide.page_y);
        // Bildschirm → Seite berücksichtigt die Seitengröße
        let p = wide
            .screen_to_page(wide.page_x + wide.page_width - 1.0, wide.page_y + 1.0)
            .unwrap();
        assert!(p.x > 2900.0);
        assert!(wide
            .screen_to_page(wide.page_x + 5.0, wide.page_y + wide.page_height + 5.0)
            .is_none());
    }

    #[test]
    fn test_new_pages_take_size_of_current_page() {
        let slide = glam::Vec2::new(2970.0, 1671.0);
        let mut nb = Notebook::new();
        nb.pages[0].size = slide;
        nb.add_page(PageBackground::Blank);
        nb.insert_page_after_current(PageBackground::Blank);
        nb.next_page();
        nb.current_page_index = nb.pages.len() - 1;
        nb.next_page();
        assert!(nb.pages.iter().all(|p| p.size == slide));
        assert_eq!(nb.fit_box(), slide);
        // Auto-Anhängen bezieht sich auf die Höhe der Folie
        nb.current_page_index = nb.pages.len() - 1;
        assert!(!nb.append_page_if_writing_at_end(slide.y * 0.7));
        assert!(nb.append_page_if_writing_at_end(slide.y * 0.9));
    }

    #[test]
    fn test_place_image() {
        let center = glam::Vec2::new(PAGE_WIDTH / 2.0, PAGE_HEIGHT / 2.0);
        // Kleines Bild: Bildschirmgröße, zentriert
        let [tl, tr, br, bl] = place_image(200, 100, center, A4_SIZE, A4_SIZE);
        assert!(((tr - tl).length() - 200.0 * IMAGE_UNITS_PER_PIXEL).abs() < 1e-3);
        assert!(((bl - tl).length() - 100.0 * IMAGE_UNITS_PER_PIXEL).abs() < 1e-3);
        assert!(((tl + br) / 2.0 - center).length() < 1e-3);

        // Großer Screenshot: auf 80 % der Breite verkleinert, Seitenverhältnis bleibt
        let [tl, tr, br, _] = place_image(3840, 2160, center, A4_SIZE, A4_SIZE);
        let size = br - tl;
        assert!((size.x - PAGE_WIDTH * 0.8).abs() < 1e-2);
        assert!((size.x / size.y - 3840.0 / 2160.0).abs() < 1e-3);
        assert_eq!(tr.y, tl.y);

        // Am Rand eingefügt: wird aufs Blatt geschoben
        let corners = place_image(
            400,
            400,
            glam::Vec2::new(-50.0, PAGE_HEIGHT + 50.0),
            A4_SIZE,
            A4_SIZE,
        );
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

    /// Senkrechter Strich bei x = 300 von `y` bis `y + h`.
    fn block(id: u64, y: f32, h: f32) -> Stroke {
        let samples = [y, y + h]
            .map(|y| StylusSample {
                position: glam::Vec2::new(300.0, y),
                pressure: 0.5,
                timestamp: 0.0,
            })
            .to_vec();
        Stroke::new(id, samples, Vec::new(), [0.0, 0.0, 0.0, 1.0])
    }

    fn top_of(nb: &Notebook, page: usize, id: u64) -> Option<f32> {
        nb.pages[page]
            .strokes
            .iter()
            .find(|s| s.id == id)
            .map(|s| s.aabb.min.y)
    }

    #[test]
    fn test_make_space_moves_content_below_and_following_pages() {
        let mut nb = Notebook::new();
        nb.add_page(PageBackground::Lined);
        let a4 = A4_SIZE.y;
        nb.pages[0].strokes = vec![block(1, 200.0, 100.0), block(2, 1000.0, 100.0)];
        nb.pages[1].strokes = vec![block(3, 500.0, 100.0)];

        // 30 mm Platz bei y = 800 auf Seite 1
        let changed = nb.make_space(0, 800.0, 300.0).unwrap();
        assert_eq!(top_of(&nb, 0, 1), Some(200.0), "darüber bleibt stehen");
        assert_eq!(top_of(&nb, 0, 2), Some(1300.0));
        assert_eq!(
            top_of(&nb, 1, 3),
            Some(800.0),
            "die nächste Seite rutscht mit"
        );
        assert_eq!(changed.len(), 2);
        assert_eq!(nb.pages.len(), 2);

        // Rückgängig und wiederholen über beide Seiten
        nb.current_page_index = 0;
        assert!(nb.undo().is_some());
        assert_eq!(top_of(&nb, 0, 2), Some(1000.0));
        assert_eq!(top_of(&nb, 1, 3), Some(500.0));
        assert!(nb.redo().is_some());
        assert_eq!(top_of(&nb, 0, 2), Some(1300.0));
        assert_eq!(top_of(&nb, 1, 3), Some(800.0));

        // Über das Seitenende: auf die nächste Seite mit Abstand, das Folgende rutscht weiter
        let mut nb = Notebook::new();
        nb.pages[0].strokes = vec![block(1, a4 - 400.0, 200.0), block(2, a4 - 150.0, 100.0)];
        nb.make_space(0, a4 - 500.0, 300.0).unwrap();
        assert_eq!(nb.pages.len(), 2, "eine Seite wurde angehängt");
        assert_eq!(nb.pages[1].background, nb.pages[0].background);
        assert!(nb.pages[0].strokes.is_empty());
        let first = top_of(&nb, 1, 1).unwrap();
        assert!((first - SPACE_PAGE_BREAK_MARGIN).abs() < 1e-3, "{first}");
        // Abstand zwischen beiden bleibt (50 Einheiten unter dem Ende des ersten)
        assert!((top_of(&nb, 1, 2).unwrap() - (first + 250.0)).abs() < 1e-3);
        // Rückgängig entfernt die angehängte leere Seite
        assert!(nb.undo().is_some());
        assert_eq!(nb.pages.len(), 1);
        assert_eq!(top_of(&nb, 0, 2), Some(a4 - 150.0));
        assert!(nb.redo().is_some());
        assert_eq!(nb.pages.len(), 2);
    }

    #[test]
    fn test_make_space_upwards_only_closes_empty_space() {
        let mut nb = Notebook::new();
        nb.pages[0].strokes = vec![block(1, 200.0, 100.0), block(2, 1000.0, 100.0)];
        // Nach oben höchstens bis an das Objekt darüber (Unterkante 300)
        assert_eq!(nb.space_up_limit(0, 900.0), 600.0);
        nb.make_space(0, 900.0, -2000.0).unwrap();
        assert_eq!(top_of(&nb, 0, 2), Some(400.0));
        assert_eq!(top_of(&nb, 0, 1), Some(200.0));
        // Kein Platz mehr: nichts zu tun
        assert!(nb.make_space(0, 300.5, -50.0).is_none());
    }

    #[test]
    fn test_make_space_stops_before_pdf_pages() {
        let mut nb = Notebook::new();
        nb.add_page(PageBackground::Blank);
        nb.add_page(PageBackground::Blank);
        nb.pages[1].pdf_background = Some(PdfBackground {
            asset: "x.pdf".to_string(),
            page: 0,
            width_pt: 595.0,
            height_pt: 842.0,
        });
        nb.pages[0].strokes = vec![block(1, 1000.0, 100.0)];
        nb.pages[2].strokes = vec![block(2, 500.0, 100.0)];
        assert!(nb.space_run(1).is_none(), "nicht auf PDF-Seiten");
        assert_eq!(nb.space_run(0), Some(0..1));
        nb.make_space(0, 900.0, 300.0).unwrap();
        assert_eq!(top_of(&nb, 0, 1), Some(1300.0));
        assert_eq!(
            top_of(&nb, 2, 2),
            Some(500.0),
            "hinter der PDF-Seite bleibt alles"
        );
    }

    #[test]
    fn test_remove_and_restore_current_page() {
        let mut nb = Notebook::new();
        // Die einzige Seite bleibt
        assert!(nb.remove_current_page().is_none());
        nb.add_page(PageBackground::Lined);
        nb.add_page(PageBackground::DotGrid);
        let ids: Vec<u64> = nb.pages.iter().map(|p| p.id).collect();

        // Mittlere Seite: danach ist die folgende aktuell
        nb.current_page_index = 1;
        let removed = nb.remove_current_page().unwrap();
        assert_eq!(removed.id, ids[1]);
        assert_eq!(
            nb.pages.iter().map(|p| p.id).collect::<Vec<_>>(),
            [ids[0], ids[2]]
        );
        assert_eq!(nb.current_page_index, 1);

        // Wiederherstellen an der alten Stelle
        assert_eq!(nb.restore_page(1, removed), 1);
        assert_eq!(nb.pages.iter().map(|p| p.id).collect::<Vec<_>>(), ids);
        assert_eq!(nb.current_page_index, 1);

        // Letzte Seite: danach ist die vorige aktuell
        nb.current_page_index = 2;
        let last = nb.remove_current_page().unwrap();
        assert_eq!(last.id, ids[2]);
        assert_eq!(nb.current_page_index, 1);
        // Neue Seiten bekommen keine schon vergebene Kennung
        assert!(nb.next_page_id > ids[2]);
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

        // Ziehen über die Ausgangsstriche: jede Bewegung gilt ab dem Ausgangszustand
        for delta in [glam::Vec2::new(5.0, 0.0), glam::Vec2::new(20.0, 30.0)] {
            page.transform_strokes_ephemeral(std::slice::from_ref(&s), |st| st.translate(delta));
        }
        assert_eq!(page.strokes[0], moved);
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

    fn line(id: u64, from: glam::Vec2, to: glam::Vec2) -> Stroke {
        let sample = |position, timestamp| StylusSample {
            position,
            pressure: 0.5,
            timestamp,
        };
        Stroke::new(
            id,
            vec![sample(from, 0.0), sample(to, 0.01)],
            Vec::new(),
            [0.0, 0.0, 0.0, 1.0],
        )
    }

    #[test]
    fn test_endless_notebook_round_trip() {
        let mut nb = Notebook::new();
        nb.pages[0].add_stroke(line(
            1,
            glam::Vec2::new(100.0, 100.0),
            glam::Vec2::new(500.0, 300.0),
        ));
        assert!(!nb.is_endless());
        assert!(nb.make_endless());
        assert!(nb.is_endless());
        // Inhalt liegt im Startausschnitt, der Verlauf ist leer
        let p = nb.pages[0].strokes[0].points[0].position;
        assert!((p - (ENDLESS_HOME + glam::Vec2::new(100.0, 100.0))).length() < 1e-3);
        assert!(!nb.pages[0].history.can_undo());
        assert!(!nb.make_endless(), "schon endlos");

        // Zurück zu Seiten: Inhalt wieder an der alten Stelle
        assert!(nb.make_paged());
        assert_eq!(nb.pages[0].size, A4_SIZE);
        let p = nb.pages[0].strokes[0].points[0].position;
        assert!((p - glam::Vec2::new(100.0, 100.0)).length() < 1e-2);

        // Inhalt außerhalb des Startausschnitts: kein Zurück
        assert!(nb.make_endless());
        nb.pages[0].add_stroke(line(2, ENDLESS_HOME - 500.0, ENDLESS_HOME - 400.0));
        assert!(!nb.can_make_paged());
        assert!(!nb.make_paged());
        assert!(nb.is_endless());

        // Mehrere Seiten oder PDF-Seite: nicht endlos
        let mut two = Notebook::new();
        two.add_page(PageBackground::Blank);
        assert!(!two.make_endless());
    }

    #[test]
    fn test_endless_viewport_shows_home_like_a4() {
        let a4 = PageViewport::calculate_for(1600.0, 900.0, 20.0, (0.0, 0.0), A4_SIZE, A4_SIZE);
        let endless = PageViewport::calculate_endless(1600.0, 900.0, 20.0, (0.0, 0.0));
        assert_eq!(endless.page_size, ENDLESS_PAGE_SIZE);
        assert!((endless.scale - a4.scale).abs() < 1e-6);
        // Die Ecke des Startausschnitts liegt dort, wo sonst die A4-Seite beginnt
        let home = endless
            .screen_to_page(a4.page_x + 1.0, a4.page_y + 1.0)
            .unwrap();
        assert!((home - ENDLESS_HOME - glam::Vec2::splat(1.0 / a4.scale)).length() < 0.05);
        // Weit neben dem Startausschnitt lässt sich weiter schreiben
        assert!(endless.screen_to_page(-5000.0, -5000.0).is_some());
    }

    #[test]
    fn test_endless_export_bounds_follow_content() {
        let mut page = Page::new(1, PageBackground::Grid5mm).with_size(ENDLESS_PAGE_SIZE);
        // Leer: der Startausschnitt
        assert_eq!(page.endless_export_bounds(), (ENDLESS_HOME, A4_SIZE));

        let from = glam::Vec2::new(30_123.0, 40_456.0);
        let to = glam::Vec2::new(36_000.0, 41_000.0);
        page.add_stroke(line(1, from, to));
        let (origin, size) = page.endless_export_bounds();
        // Ecke auf dem Musterraster, Inhalt mit Rand vollständig enthalten
        assert_eq!(origin % ENDLESS_EXPORT_SNAP, glam::Vec2::ZERO);
        assert!(origin.cmple(from - ENDLESS_EXPORT_MARGIN).all());
        assert!((origin + size).cmpge(to + ENDLESS_EXPORT_MARGIN).all());
        assert!(size.x < 7_000.0 && size.y < 2_000.0, "{size:?}");

        let cropped = page.cropped(origin, size);
        assert_eq!(cropped.size, size);
        assert!((cropped.strokes[0].points[0].position - (from - origin)).length() < 1e-2);
    }
}
