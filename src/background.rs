//! Vektor-Generierung für DIN-A4-Seitenhintergründe (Kariert, Liniert, Punktraster, Blanko).

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

/// Hellgraue Farbe für Hintergrundmuster (#E0E0E0).
pub const BACKGROUND_PATTERN_COLOR: [f32; 4] = [0.88, 0.88, 0.88, 1.0];
/// Papierweiß für das DIN-A4-Blatt (#FFFFFF).
pub const PAPER_COLOR: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
/// Schreibtisch-Hintergrundfarbe (Letterbox-Bereich, #28282D).
pub const DESK_COLOR: wgpu::Color = wgpu::Color {
    r: 0.16,
    g: 0.16,
    b: 0.18,
    a: 1.0,
};

/// Erzeugt die 6 Vertizes (2 Dreiecke) für das weiße DIN-A4-Papierblatt.
pub fn generate_paper_quad() -> Vec<InkVertex> {
    vec![
        InkVertex::new(0.0, 0.0, PAPER_COLOR),
        InkVertex::new(PAGE_WIDTH, 0.0, PAPER_COLOR),
        InkVertex::new(0.0, PAGE_HEIGHT, PAPER_COLOR),
        InkVertex::new(0.0, PAGE_HEIGHT, PAPER_COLOR),
        InkVertex::new(PAGE_WIDTH, 0.0, PAPER_COLOR),
        InkVertex::new(PAGE_WIDTH, PAGE_HEIGHT, PAPER_COLOR),
    ]
}

/// Fügt ein Quad (2 Dreiecke) zur Vertexliste hinzu.
#[inline]
fn push_quad(
    vertices: &mut Vec<InkVertex>,
    x_min: f32,
    y_min: f32,
    x_max: f32,
    y_max: f32,
) {
    vertices.push(InkVertex::new(x_min, y_min, BACKGROUND_PATTERN_COLOR));
    vertices.push(InkVertex::new(x_max, y_min, BACKGROUND_PATTERN_COLOR));
    vertices.push(InkVertex::new(x_min, y_max, BACKGROUND_PATTERN_COLOR));
    vertices.push(InkVertex::new(x_min, y_max, BACKGROUND_PATTERN_COLOR));
    vertices.push(InkVertex::new(x_max, y_min, BACKGROUND_PATTERN_COLOR));
    vertices.push(InkVertex::new(x_max, y_max, BACKGROUND_PATTERN_COLOR));
}

/// Generiert das statische Vektor-Mesh für den gewählten Seitenhintergrund.
pub fn generate_background_mesh(background: PageBackground) -> Vec<InkVertex> {
    match background {
        PageBackground::Blank => Vec::new(),
        PageBackground::Grid5mm => {
            let mut vertices = Vec::with_capacity(1200);
            let half_t = LINE_THICKNESS / 2.0;

            // Vertikale Linien alle 5 mm
            let mut x = GRID_5MM_SPACING;
            while x < PAGE_WIDTH {
                push_quad(&mut vertices, x - half_t, 0.0, x + half_t, PAGE_HEIGHT);
                x += GRID_5MM_SPACING;
            }

            // Horizontale Linien alle 5 mm
            let mut y = GRID_5MM_SPACING;
            while y < PAGE_HEIGHT {
                push_quad(&mut vertices, 0.0, y - half_t, PAGE_WIDTH, y + half_t);
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
                push_quad(&mut vertices, 0.0, y - half_t, PAGE_WIDTH, y + half_t);
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
                    push_quad(&mut vertices, x - half_d, y - half_d, x + half_d, y + half_d);
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
        let quad = generate_paper_quad();
        assert_eq!(quad.len(), 6);
        assert_eq!(quad[0].position, [0.0, 0.0]);
        assert_eq!(quad[5].position, [PAGE_WIDTH, PAGE_HEIGHT]);
    }

    #[test]
    fn test_background_meshes() {
        let blank = generate_background_mesh(PageBackground::Blank);
        assert!(blank.is_empty());

        let grid = generate_background_mesh(PageBackground::Grid5mm);
        assert!(!grid.is_empty());
        assert_eq!(grid.len() % 6, 0, "Muss ein Vielfaches von 6 Vertizes sein (Dreiecke)");

        let lined = generate_background_mesh(PageBackground::Lined);
        assert!(!lined.is_empty());
        assert_eq!(lined.len() % 6, 0);

        let dots = generate_background_mesh(PageBackground::DotGrid);
        assert!(!dots.is_empty());
        assert_eq!(dots.len() % 6, 0);
    }
}
