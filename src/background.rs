//! Vektor-Generierung für DIN-A4-Seitenhintergründe (Kariert, Liniert, Punktraster, Blanko).

use serde::{Deserialize, Serialize};

use crate::canvas::InkVertex;
use crate::document::{PageBackground, PAGE_HEIGHT, PAGE_WIDTH};

/// Linienstärke für Gitter- und Schreiblinien in Seiteneinheiten (1.5 = 0.15 mm).
pub const LINE_THICKNESS: f32 = 1.5;
/// Punktgröße für das Punktraster in Seiteneinheiten (2.5 = 0.25 mm).
pub const DOT_SIZE: f32 = 2.5;

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

/// Erzeugt die 6 Vertizes (2 Dreiecke) für das DIN-A4-Papierblatt.
pub fn generate_paper_quad(color: [f32; 4]) -> Vec<InkVertex> {
    vec![
        InkVertex::new(0.0, 0.0, color),
        InkVertex::new(PAGE_WIDTH, 0.0, color),
        InkVertex::new(0.0, PAGE_HEIGHT, color),
        InkVertex::new(0.0, PAGE_HEIGHT, color),
        InkVertex::new(PAGE_WIDTH, 0.0, color),
        InkVertex::new(PAGE_WIDTH, PAGE_HEIGHT, color),
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

/// Generiert das statische Vektor-Mesh für den gewählten Seitenhintergrund in `color`.
pub fn generate_background_mesh(background: PageBackground, color: [f32; 4]) -> Vec<InkVertex> {
    match background {
        PageBackground::Blank => Vec::new(),
        PageBackground::Grid5mm => {
            let mut vertices = Vec::with_capacity(1200);
            let half_t = LINE_THICKNESS / 2.0;

            // Vertikale Linien alle 5 mm
            let mut x = GRID_5MM_SPACING;
            while x < PAGE_WIDTH {
                push_quad(
                    &mut vertices,
                    color,
                    x - half_t,
                    0.0,
                    x + half_t,
                    PAGE_HEIGHT,
                );
                x += GRID_5MM_SPACING;
            }

            // Horizontale Linien alle 5 mm
            let mut y = GRID_5MM_SPACING;
            while y < PAGE_HEIGHT {
                push_quad(
                    &mut vertices,
                    color,
                    0.0,
                    y - half_t,
                    PAGE_WIDTH,
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
            while y <= PAGE_HEIGHT - margin_bottom {
                push_quad(
                    &mut vertices,
                    color,
                    0.0,
                    y - half_t,
                    PAGE_WIDTH,
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
            while y < PAGE_HEIGHT {
                let mut x = GRID_5MM_SPACING;
                while x < PAGE_WIDTH {
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

    #[test]
    fn test_paper_quad_generation() {
        let quad = generate_paper_quad(PaperTheme::White.paper_color());
        assert_eq!(quad.len(), 6);
        assert_eq!(quad[0].position, [0.0, 0.0]);
        assert_eq!(quad[5].position, [PAGE_WIDTH, PAGE_HEIGHT]);
    }

    const PATTERN: [f32; 4] = [0.5, 0.5, 0.5, 1.0];

    #[test]
    fn test_background_meshes() {
        let blank = generate_background_mesh(PageBackground::Blank, PATTERN);
        assert!(blank.is_empty());

        let grid = generate_background_mesh(PageBackground::Grid5mm, PATTERN);
        assert!(!grid.is_empty());
        assert_eq!(
            grid.len() % 6,
            0,
            "Muss ein Vielfaches von 6 Vertizes sein (Dreiecke)"
        );

        let lined = generate_background_mesh(PageBackground::Lined, PATTERN);
        assert!(!lined.is_empty());
        assert_eq!(lined.len() % 6, 0);

        let dots = generate_background_mesh(PageBackground::DotGrid, PATTERN);
        assert!(!dots.is_empty());
        assert_eq!(dots.len() % 6, 0);
    }

    #[test]
    fn test_background_mesh_uses_theme_color() {
        let dark = PaperTheme::Black.pattern_color();
        let mesh = generate_background_mesh(PageBackground::Lined, dark);
        assert!(mesh.iter().all(|v| v.color == dark));
        let quad = generate_paper_quad(PaperTheme::Cream.paper_color());
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
