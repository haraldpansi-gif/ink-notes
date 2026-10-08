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

/// Oberer und unterer Rand ohne Linien bei liniertem Papier (15 mm).
pub const LINED_MARGIN: f32 = 150.0;

/// Ecke des Musterrechtecks: Das Muster selbst berechnet der Fragment-Shader (`fs_pattern`)
/// für jedes Bildschirmpixel. So liegen Linien und Punkte in jedem Zoom exakt gleichmäßig,
/// sind mindestens ein Pixel breit und kantengeglättet – vorher gerasterte Haarlinien
/// fielen je nach Lage auf das Pixelraster und wirkten unregelmäßig.
#[repr(C)]
#[derive(Copy, Clone, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct PatternVertex {
    /// Seitenkoordinate
    pub position: [f32; 2],
    pub color: [f32; 4],
    /// Art (1 = Karo, 2 = Linien, 3 = Punkte), Abstand, Linienstärke, Punktgröße
    pub params: [f32; 4],
    /// Seitenbreite, Seitenhöhe, Rand oben/unten bei Linien, ungenutzt
    pub bounds: [f32; 4],
}

impl PatternVertex {
    const ATTRIBUTES: [wgpu::VertexAttribute; 4] = wgpu::vertex_attr_array![
        0 => Float32x2,
        1 => Float32x4,
        2 => Float32x4,
        3 => Float32x4
    ];

    pub fn desc() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<PatternVertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &Self::ATTRIBUTES,
        }
    }
}

/// Rechteck über das ganze Blatt der Größe `size`, auf dem der Shader das Muster
/// `background` in `color` zeichnet; leer bei blankem Papier.
pub fn generate_pattern_quad(
    background: PageBackground,
    color: [f32; 4],
    size: glam::Vec2,
) -> Vec<PatternVertex> {
    let params = match background {
        PageBackground::Blank => return Vec::new(),
        PageBackground::Grid5mm => [1.0, GRID_5MM_SPACING, LINE_THICKNESS, DOT_SIZE],
        PageBackground::Lined => [2.0, LINED_SPACING, LINE_THICKNESS, DOT_SIZE],
        PageBackground::DotGrid => [3.0, GRID_5MM_SPACING, LINE_THICKNESS, DOT_SIZE],
    };
    let bounds = [size.x, size.y, LINED_MARGIN, 0.0];
    let corner = |x: f32, y: f32| PatternVertex {
        position: [x, y],
        color,
        params,
        bounds,
    };
    vec![
        corner(0.0, 0.0),
        corner(size.x, 0.0),
        corner(0.0, size.y),
        corner(0.0, size.y),
        corner(size.x, 0.0),
        corner(size.x, size.y),
    ]
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
    fn test_pattern_quads() {
        assert!(generate_pattern_quad(PageBackground::Blank, PATTERN, A4_SIZE).is_empty());
        for (background, kind) in [
            (PageBackground::Grid5mm, 1.0),
            (PageBackground::Lined, 2.0),
            (PageBackground::DotGrid, 3.0),
        ] {
            let quad = generate_pattern_quad(background, PATTERN, A4_SIZE);
            assert_eq!(quad.len(), 6);
            assert!(quad
                .iter()
                .all(|v| v.params[0] == kind && v.color == PATTERN));
            assert_eq!(quad[5].position, [PAGE_WIDTH, PAGE_HEIGHT]);
            assert_eq!(quad[0].bounds[..2], [PAGE_WIDTH, PAGE_HEIGHT]);
        }
    }

    #[test]
    fn test_background_mesh_uses_theme_color() {
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
