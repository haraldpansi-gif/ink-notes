//! Vektor-Generierung für DIN-A4-Seitenhintergründe (Kariert, Liniert, Punktraster, Blanko).

use serde::{Deserialize, Serialize};

use crate::canvas::InkVertex;
use crate::document::PageBackground;

/// Farbe von Karos, Linien und Punkten (wird gespeichert und gilt auch für den Export).
/// `Light` ist die zurückhaltende Standardfarbe des jeweiligen Papiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PatternColor {
    #[default]
    Light,
    Medium,
    Strong,
    Blue,
    Green,
}

impl PatternColor {
    pub const ALL: [PatternColor; 5] = [
        PatternColor::Light,
        PatternColor::Medium,
        PatternColor::Strong,
        PatternColor::Blue,
        PatternColor::Green,
    ];

    pub fn label(self) -> &'static str {
        match self {
            PatternColor::Light => "Hell",
            PatternColor::Medium => "Mittel",
            PatternColor::Strong => "Kräftig",
            PatternColor::Blue => "Blau",
            PatternColor::Green => "Grün",
        }
    }

    /// Linearer Farbwert des Musters auf dem Papier `paper` (für den Export: weißes Papier).
    pub fn color(self, paper: PaperTheme) -> [f32; 4] {
        let light = paper.pattern_color();
        let dark = paper.is_dark();
        let scaled = |f: f32| [light[0] * f, light[1] * f, light[2] * f, 1.0];
        match (self, dark) {
            (PatternColor::Light, _) => light,
            // Helles Papier: kräftiger = dunkler
            (PatternColor::Medium, false) => scaled(0.62),
            (PatternColor::Strong, false) => scaled(0.28),
            // Dunkles Papier: kräftiger = heller
            (PatternColor::Medium, true) => [0.16, 0.16, 0.17, 1.0],
            (PatternColor::Strong, true) => [0.32, 0.32, 0.34, 1.0],
            (PatternColor::Blue, false) => [0.35, 0.55, 0.95, 1.0],
            (PatternColor::Blue, true) => [0.06, 0.14, 0.42, 1.0],
            (PatternColor::Green, false) => [0.32, 0.72, 0.38, 1.0],
            (PatternColor::Green, true) => [0.05, 0.22, 0.09, 1.0],
        }
    }
}

/// Linienstärke für Gitter- und Schreiblinien in Seiteneinheiten (1.5 = 0.15 mm).
pub const LINE_THICKNESS: f32 = 1.5;
/// Punktgröße für das Punktraster in Seiteneinheiten (4 = 0,4 mm, wie im PDF-Export).
pub const DOT_SIZE: f32 = 4.0;

/// Rasterabstand 5 mm (50 Einheiten).
pub const GRID_5MM_SPACING: f32 = 50.0;
/// Zeilenabstand für liniertes Papier (7.5 mm = 75 Einheiten).
pub const LINED_SPACING: f32 = 75.0;

/// Papierfarbe der Anzeige. Betrifft nur die Darstellung am Bildschirm; gespeicherte
/// Strichfarben und der PDF-Export bleiben unverändert (weißes Papier).
///
/// Alle Farben sind linear (die Oberfläche ist sRGB, die Umrechnung macht die GPU).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PaperTheme {
    #[default]
    White,
    /// Cremefarben (augenschonend, wie Recyclingpapier)
    Cream,
    /// Dunkelgrau (Nachtmodus)
    DarkGray,
    /// Schwarz (OLED)
    Black,
}

impl PaperTheme {
    pub const ALL: [PaperTheme; 4] = [
        PaperTheme::White,
        PaperTheme::Cream,
        PaperTheme::DarkGray,
        PaperTheme::Black,
    ];

    pub fn label(self) -> &'static str {
        match self {
            PaperTheme::White => "Weiß",
            PaperTheme::Cream => "Creme",
            PaperTheme::DarkGray => "Dunkelgrau",
            PaperTheme::Black => "Schwarz",
        }
    }

    /// Dunkles Papier: dunkle Tinte wird bei Bedarf aufgehellt.
    pub fn is_dark(self) -> bool {
        matches!(self, PaperTheme::DarkGray | PaperTheme::Black)
    }

    /// Farbe des Blatts.
    pub fn paper_color(self) -> [f32; 4] {
        match self {
            PaperTheme::White => [1.0, 1.0, 1.0, 1.0],
            // sRGB #FBF5E6
            PaperTheme::Cream => [0.965, 0.913, 0.791, 1.0],
            // sRGB #2B2B2E
            PaperTheme::DarkGray => [0.024, 0.024, 0.027, 1.0],
            PaperTheme::Black => [0.0, 0.0, 0.0, 1.0],
        }
    }

    /// Farbe von Karos, Linien und Punkten.
    pub fn pattern_color(self) -> [f32; 4] {
        match self {
            PaperTheme::White => [0.88, 0.88, 0.88, 1.0],
            PaperTheme::Cream => [0.78, 0.72, 0.60, 1.0],
            // sRGB #48484D bzw. #38383C
            PaperTheme::DarkGray => [0.064, 0.064, 0.074, 1.0],
            PaperTheme::Black => [0.040, 0.040, 0.045, 1.0],
        }
    }

    /// Schreibtisch-Hintergrund um das Blatt; bei dunklem Papier noch dunkler, damit sich
    /// das Blatt abhebt.
    pub fn desk_color(self) -> wgpu::Color {
        let (r, g, b) = match self {
            PaperTheme::White | PaperTheme::Cream => (0.16, 0.16, 0.18),
            PaperTheme::DarkGray => (0.004, 0.004, 0.005),
            PaperTheme::Black => (0.03, 0.03, 0.035),
        };
        wgpu::Color { r, g, b, a: 1.0 }
    }
}

/// Hellt dunkle Tinte für dunkles Papier auf: die HSL-Helligkeit L wird zu 1 − L gespiegelt,
/// Farbton und Sättigung bleiben erhalten. Farben mit L ≥ 0.5 (Rot, Blau, Textmarker)
/// bleiben unverändert. Muss mit `fs_ink` in `shader.wgsl` übereinstimmen.
pub fn adapt_ink_color(color: [f32; 4]) -> [f32; 4] {
    let max = color[0].max(color[1]).max(color[2]);
    let min = color[0].min(color[1]).min(color[2]);
    let lift = (1.0 - max - min).max(0.0);
    [color[0] + lift, color[1] + lift, color[2] + lift, color[3]]
}

/// Erzeugt die 6 Vertizes (2 Dreiecke) für ein Blatt der Größe `size`.
pub fn generate_paper_quad(color: [f32; 4], size: glam::Vec2) -> Vec<InkVertex> {
    let (page_width, page_height) = (size.x, size.y);
    vec![
        InkVertex::new(0.0, 0.0, color),
        InkVertex::new(page_width, 0.0, color),
        InkVertex::new(0.0, page_height, color),
        InkVertex::new(0.0, page_height, color),
        InkVertex::new(page_width, 0.0, color),
        InkVertex::new(page_width, page_height, color),
    ]
}

/// Fügt ein Quad (2 Dreiecke) zur Vertexliste hinzu.
#[inline]
fn push_quad(
    vertices: &mut Vec<InkVertex>,
    color: [f32; 4],
    x_min: f32,
    y_min: f32,
    x_max: f32,
    y_max: f32,
) {
    vertices.push(InkVertex::new(x_min, y_min, color));
    vertices.push(InkVertex::new(x_max, y_min, color));
    vertices.push(InkVertex::new(x_min, y_max, color));
    vertices.push(InkVertex::new(x_min, y_max, color));
    vertices.push(InkVertex::new(x_max, y_min, color));
    vertices.push(InkVertex::new(x_max, y_max, color));
}

/// Generiert das statische Vektor-Mesh für den gewählten Seitenhintergrund in `color` auf
/// einem Blatt der Größe `size`.
pub fn generate_background_mesh(
    background: PageBackground,
    color: [f32; 4],
    size: glam::Vec2,
) -> Vec<InkVertex> {
    let (page_width, page_height) = (size.x, size.y);
    match background {
        PageBackground::Blank => Vec::new(),
        PageBackground::Grid5mm => {
            let mut vertices = Vec::with_capacity(1200);
            let half_t = LINE_THICKNESS / 2.0;

            // Vertikale Linien alle 5 mm
            let mut x = GRID_5MM_SPACING;
            while x < page_width {
                push_quad(
                    &mut vertices,
                    color,
                    x - half_t,
                    0.0,
                    x + half_t,
                    page_height,
                );
                x += GRID_5MM_SPACING;
            }

            // Horizontale Linien alle 5 mm
            let mut y = GRID_5MM_SPACING;
            while y < page_height {
                push_quad(
                    &mut vertices,
                    color,
                    0.0,
                    y - half_t,
                    page_width,
                    y + half_t,
                );
                y += GRID_5MM_SPACING;
            }

            vertices
        }
        PageBackground::Lined => {
            let mut vertices = Vec::with_capacity(600);
            let half_t = LINE_THICKNESS / 2.0;

            // Oberer und unterer Randabstand (z. B. 15 mm)
            let margin_top = 150.0;
            let margin_bottom = 150.0;

            let mut y = margin_top;
            while y <= page_height - margin_bottom {
                push_quad(
                    &mut vertices,
                    color,
                    0.0,
                    y - half_t,
                    page_width,
                    y + half_t,
                );
                y += LINED_SPACING;
            }

            vertices
        }
        PageBackground::DotGrid => {
            let mut vertices = Vec::with_capacity(15000);
            let half_d = DOT_SIZE / 2.0;

            let mut y = GRID_5MM_SPACING;
            while y < page_height {
                let mut x = GRID_5MM_SPACING;
                while x < page_width {
                    push_quad(
                        &mut vertices,
                        color,
                        x - half_d,
                        y - half_d,
                        x + half_d,
                        y + half_d,
                    );
                    x += GRID_5MM_SPACING;
                }
                y += GRID_5MM_SPACING;
            }

            vertices
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::document::{A4_SIZE, PAGE_HEIGHT, PAGE_WIDTH};

    #[test]
    fn test_pattern_colors_get_more_visible() {
        // Luminanz (linear) als Abstand zum Papier
        let lum = |c: [f32; 4]| 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
        for paper in PaperTheme::ALL {
            let p = lum(paper.paper_color());
            let contrast = |c: PatternColor| (lum(c.color(paper)) - p).abs();
            assert_eq!(PatternColor::Light.color(paper), paper.pattern_color());
            assert!(
                contrast(PatternColor::Medium) > contrast(PatternColor::Light),
                "{paper:?}"
            );
            assert!(
                contrast(PatternColor::Strong) > contrast(PatternColor::Medium),
                "{paper:?}"
            );
            for c in [PatternColor::Blue, PatternColor::Green] {
                assert!(
                    contrast(c) > contrast(PatternColor::Light),
                    "{c:?} auf {paper:?}"
                );
            }
        }
    }

    #[test]
    fn test_paper_quad_generation() {
        let quad = generate_paper_quad(PaperTheme::White.paper_color(), A4_SIZE);
        assert_eq!(quad.len(), 6);
        assert_eq!(quad[0].position, [0.0, 0.0]);
        assert_eq!(quad[5].position, [PAGE_WIDTH, PAGE_HEIGHT]);
        let slide = generate_paper_quad(
            PaperTheme::White.paper_color(),
            glam::Vec2::new(2970.0, 1671.0),
        );
        assert_eq!(slide[5].position, [2970.0, 1671.0]);
    }

    const PATTERN: [f32; 4] = [0.5, 0.5, 0.5, 1.0];

    #[test]
    fn test_background_meshes() {
        let blank = generate_background_mesh(PageBackground::Blank, PATTERN, A4_SIZE);
        assert!(blank.is_empty());

        let grid = generate_background_mesh(PageBackground::Grid5mm, PATTERN, A4_SIZE);
        assert!(!grid.is_empty());
        assert_eq!(
            grid.len() % 6,
            0,
            "Muss ein Vielfaches von 6 Vertizes sein (Dreiecke)"
        );

        let lined = generate_background_mesh(PageBackground::Lined, PATTERN, A4_SIZE);
        assert!(!lined.is_empty());
        assert_eq!(lined.len() % 6, 0);

        let dots = generate_background_mesh(PageBackground::DotGrid, PATTERN, A4_SIZE);
        assert!(!dots.is_empty());
        assert_eq!(dots.len() % 6, 0);
    }

    #[test]
    fn test_background_mesh_uses_theme_color() {
        let dark = PaperTheme::Black.pattern_color();
        let mesh = generate_background_mesh(PageBackground::Lined, dark, A4_SIZE);
        assert!(mesh.iter().all(|v| v.color == dark));
        let quad = generate_paper_quad(PaperTheme::Cream.paper_color(), A4_SIZE);
        assert!(quad
            .iter()
            .all(|v| v.color == PaperTheme::Cream.paper_color()));
    }

    #[test]
    fn test_adapt_ink_color() {
        let close = |a: [f32; 4], b: [f32; 4]| a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-5);
        // Schwarz wird Weiß, Deckkraft bleibt
        assert!(close(
            adapt_ink_color([0.0, 0.0, 0.0, 0.7]),
            [1.0, 1.0, 1.0, 0.7]
        ));
        // Notiz-Schwarz wird hell
        let ink = adapt_ink_color([0.12, 0.12, 0.14, 1.0]);
        assert!(ink[0] > 0.85 && ink[2] > 0.85);
        // Dunkelblau wird Hellblau: Farbton bleibt (Blau bleibt der stärkste Kanal)
        let blue = adapt_ink_color([0.0, 0.0, 0.5, 1.0]);
        assert!(close(blue, [0.5, 0.5, 1.0, 1.0]));
        // Helle und gesättigte Farben bleiben unverändert (Rot, Universitäts-Blau, Textmarker)
        for c in [
            [0.85, 0.20, 0.20, 1.0],
            [0.15, 0.35, 0.85, 1.0],
            [1.0, 0.95, 0.1, 0.35],
            [1.0, 1.0, 1.0, 1.0],
        ] {
            assert!(close(adapt_ink_color(c), c), "{c:?} wurde verändert");
        }
        // Ergebnis ist nie heller als Weiß
        for c in [[0.0, 0.3, 0.1, 1.0], [0.45, 0.45, 0.48, 1.0]] {
            assert!(adapt_ink_color(c)[..3].iter().all(|&x| x <= 1.0 + 1e-6));
        }
    }

    #[test]
    fn test_paper_themes_contrast() {
        for theme in PaperTheme::ALL {
            let paper = theme.paper_color()[0];
            let pattern = theme.pattern_color()[0];
            assert!(
                (paper - pattern).abs() > 0.03,
                "{theme:?}: Muster hebt sich nicht vom Papier ab"
            );
            // Helles Papier ↔ dunkles Papier
            assert_eq!(theme.is_dark(), paper < 0.5, "{theme:?}");
        }
    }
}
